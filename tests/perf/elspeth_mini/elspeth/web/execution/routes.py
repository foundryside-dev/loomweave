"""REST endpoints and WebSocket for pipeline execution.

POST /api/sessions/{session_id}/validate — dry-run validation
POST /api/sessions/{session_id}/execute — start background run
GET  /api/runs/{run_id}                 — run status
POST /api/runs/{run_id}/cancel          — cancel run
GET  /api/runs/{run_id}/results         — run results (terminal only)
WS   /ws/runs/{run_id}                  — live progress stream

All endpoints require authentication. Session-scoped endpoints verify
session ownership. Run-scoped endpoints verify run ownership via the
run's parent session.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any, cast
from uuid import UUID

import structlog
from fastapi import APIRouter, Body, Depends, HTTPException, Query, Request, WebSocket, WebSocketDisconnect
from fastapi.responses import FileResponse
from pydantic import ValidationError

from elspeth.web.async_workers import run_sync_in_worker
from elspeth.web.auth.middleware import get_current_user
from elspeth.web.auth.models import AuthenticationError, UserIdentity
from elspeth.web.auth.protocol import AuthProvider
from elspeth.web.blobs.protocol import BlobNotFoundError
from elspeth.web.composer.protocol import ComposerService, ComposerServiceError
from elspeth.web.config import WebSettings
from elspeth.web.execution.accounting import load_run_accounting_for_settings
from elspeth.web.execution.diagnostics import load_run_diagnostics_for_settings
from elspeth.web.execution.errors import BlobSourcePathMismatchError, ExecuteRequestValidationError, SemanticContractViolationError
from elspeth.web.execution.fanout_guard import FANOUT_GUARD_ERROR_TYPE, ExecutionFanoutGuardRequired
from elspeth.web.execution.outputs import (
    RunOutputsAuditUnavailableError,
    load_run_outputs_for_settings,
    path_or_uri_to_filesystem_path,
)
from elspeth.web.execution.preview import build_artifact_preview
from elspeth.web.execution.progress import ProgressBroadcaster
from elspeth.web.execution.protocol import ExecutionService, StateAccessError
from elspeth.web.execution.schemas import (
    RUN_STATUS_NON_TERMINAL_VALUES,
    RUN_STATUS_TERMINAL_VALUES,
    CancelledData,
    CompletedData,
    ExecuteRequest,
    FailedData,
    RunDiagnosticsEvaluationResponse,
    RunDiagnosticsResponse,
    RunDiagnosticsWorkingView,
    RunEvent,
    RunEventType,
    RunOutputArtifactPreview,
    RunOutputsResponse,
    RunResultsResponse,
    RunStatusResponse,
    ValidationResult,
)
from elspeth.web.paths import allowed_sink_directories
from elspeth.web.sessions.ownership import verify_session_ownership
from elspeth.web.sessions.protocol import (
    OPERATOR_COMPLETION_RUN_STATUS_VALUES,
    RunRecord,
    SessionServiceProtocol,
    TerminalSessionRunStatus,
)

slog = structlog.get_logger()


# ── Dependency providers (using app.state, matching existing pattern) ──


async def _get_execution_service(request: Request) -> ExecutionService:
    return cast(ExecutionService, request.app.state.execution_service)


async def _get_session_service(request: Request) -> SessionServiceProtocol:
    return cast(SessionServiceProtocol, request.app.state.session_service)


# ── Ownership verification helpers ────────────────────────────────────
#
# Session-ownership verification lives in ``web/sessions/ownership.py`` as
# ``verify_session_ownership`` so ``execution/routes.py`` and
# ``audit_readiness/routes.py`` share a single IDOR-safe implementation.
# Run-ownership verification remains here — only execution/ runs care.


async def _verify_run_ownership(run_id: UUID, user: UserIdentity, request: Request) -> None:
    """Verify the run exists and belongs to the current user's session.

    Looks up the run's parent session and checks ownership.
    Returns 404 (not 403) to avoid leaking run existence (IDOR).
    """
    session_service: SessionServiceProtocol = request.app.state.session_service
    settings: WebSettings = request.app.state.settings
    try:
        run = await session_service.get_run(run_id)
    except ValueError:
        raise HTTPException(status_code=404, detail="Run not found") from None

    try:
        session = await session_service.get_session(run.session_id)
    except ValueError:
        raise HTTPException(status_code=404, detail="Run not found") from None

    if session.user_id != user.user_id or session.auth_provider_type != settings.auth_provider:
        raise HTTPException(status_code=404, detail="Run not found")


def _run_not_found_http() -> HTTPException:
    """Canonical IDOR-safe not-found response for run-scoped routes."""
    return HTTPException(status_code=404, detail="Run not found")


class _RunStatusNotFoundError(Exception):
    """Run disappeared between ownership verification and status projection."""


class _RunStatusIntegrityError(Exception):
    """Run status accounting projection failed internal integrity validation."""


@dataclass(frozen=True, slots=True)
class _LoadedRunStatus:
    """Run status projection paired with the exact session-row snapshot used to build it."""

    response: RunStatusResponse
    record: RunRecord


def _run_integrity_http(exc: ValidationError | _RunStatusIntegrityError) -> HTTPException:
    detail: dict[str, Any] = {
        "code": "run_integrity_error",
        "message": "Run status failed internal accounting validation.",
    }
    if isinstance(exc, ValidationError):
        detail["validation_errors"] = exc.errors(include_url=False, include_context=False, include_input=False)
    else:
        detail["error"] = str(exc)
    return HTTPException(status_code=500, detail=detail)


async def _load_run_status_snapshot_with_accounting(
    run_id: UUID,
    *,
    app: Any,
    service: ExecutionService,
) -> _LoadedRunStatus:
    """Load run status with Landscape-derived accounting when a run has audit data."""
    session_service: SessionServiceProtocol = app.state.session_service
    try:
        run_record = await session_service.get_run(run_id)
    except ValueError as exc:
        raise _RunStatusNotFoundError from exc

    accounting = None
    if run_record.landscape_run_id and run_record.status in OPERATOR_COMPLETION_RUN_STATUS_VALUES:
        try:
            accounting_by_run_id = await run_sync_in_worker(
                load_run_accounting_for_settings,
                app.state.settings,
                (run_record.landscape_run_id,),
            )
        except ValueError as exc:
            raise _RunStatusIntegrityError(str(exc)) from exc
        if run_record.landscape_run_id in accounting_by_run_id:
            accounting = accounting_by_run_id[run_record.landscape_run_id]

    try:
        status = await service.get_status(run_id, accounting=accounting, run_record=run_record)
    except ValidationError:
        raise
    except ValueError as exc:
        raise _RunStatusNotFoundError from exc
    return _LoadedRunStatus(response=status, record=run_record)


async def _load_run_status_with_accounting(
    run_id: UUID,
    *,
    app: Any,
    service: ExecutionService,
) -> RunStatusResponse:
    """Load run status with accounting, returning only the public status response."""
    loaded = await _load_run_status_snapshot_with_accounting(run_id, app=app, service=service)
    return loaded.response


def _build_terminal_run_event(current: RunStatusResponse, *, cancelled_run_record: RunRecord | None = None) -> RunEvent:
    """Synthesize a terminal RunEvent from authoritative run status.

    ``current`` comes from our session database and is therefore Tier 1.
    Impossible terminal states must raise rather than degrade into
    partial client-visible payloads.

    Phase 2.2 (elspeth-0de989c56d): the operator-completion subset
    (``completed``, ``completed_with_failures``, ``empty``) all map to the
    SSE ``event_type="completed"`` envelope; the operator-completion status
    travels in the ``CompletedData.status`` discriminator so the frontend
    can render the widened taxonomy without re-deriving from row counts.
    """
    completion_status = current.status
    if completion_status == "completed" or completion_status == "completed_with_failures" or completion_status == "empty":
        if current.landscape_run_id is None:
            raise RuntimeError(f"Completed run {current.run_id} has no landscape_run_id — Tier 1 anomaly (audit trail incomplete)")
        if current.accounting is None:
            raise RuntimeError(f"Completed run {current.run_id} has no accounting — Tier 1 anomaly (audit trail incomplete)")
        try:
            payload: CompletedData | FailedData | CancelledData = CompletedData(
                status=completion_status,
                accounting=current.accounting,
                landscape_run_id=current.landscape_run_id,
            )
        except ValidationError as exc:
            raise RuntimeError(
                f"Completed run {current.run_id} failed CompletedData validation — Tier 1 anomaly (audit trail inconsistent): {exc}"
            ) from exc
        event_type: RunEventType = "completed"
    elif current.status == "failed":
        if current.error is None:
            raise RuntimeError(f"Failed run {current.run_id} has no error message — Tier 1 anomaly (error column NULL on terminal failure)")
        payload = FailedData(
            detail=current.error,
            node_id=None,
        )
        event_type = "failed"
    elif current.status == "cancelled":
        if current.accounting is not None:
            payload = CancelledData(
                source_rows_processed=current.accounting.source.rows_processed,
                tokens_succeeded=current.accounting.tokens.succeeded,
                tokens_failed=current.accounting.tokens.failed,
                tokens_quarantined=current.accounting.routing.quarantined,
                tokens_routed_success=current.accounting.routing.routed_success,
                tokens_routed_failure=current.accounting.routing.routed_failure,
            )
        else:
            if cancelled_run_record is None:
                raise RuntimeError(
                    f"Cancelled run {current.run_id} has no accounting and no RunRecord counters — "
                    f"Tier 1 anomaly (cancellation replay cannot be reconstructed)"
                )
            if str(cancelled_run_record.id) != current.run_id:
                raise RuntimeError(
                    f"Cancelled replay RunRecord mismatch: status run {current.run_id} received counters for run {cancelled_run_record.id}"
                )
            if cancelled_run_record.status != "cancelled":
                raise RuntimeError(f"Cancelled replay RunRecord status mismatch for run {current.run_id}: {cancelled_run_record.status!r}")
            payload = CancelledData(
                source_rows_processed=cancelled_run_record.rows_processed,
                tokens_succeeded=cancelled_run_record.rows_succeeded,
                tokens_failed=cancelled_run_record.rows_failed,
                tokens_quarantined=cancelled_run_record.rows_quarantined,
                tokens_routed_success=cancelled_run_record.rows_routed_success,
                tokens_routed_failure=cancelled_run_record.rows_routed_failure,
            )
        event_type = "cancelled"
    else:
        raise RuntimeError(f"_build_terminal_run_event called for non-terminal status {current.status!r}")

    timestamp = current.finished_at or current.started_at
    if timestamp is None:
        raise RuntimeError(f"Terminal run {current.run_id} has no timestamps — Tier 1 anomaly (both finished_at and started_at are NULL)")
    return RunEvent(
        run_id=current.run_id,
        timestamp=timestamp,
        event_type=event_type,
        data=payload,
    )


def _counted(label: str, count: int) -> str:
    """Return a small English count phrase."""
    if count == 1:
        return f"1 {label}"
    return f"{count} {label}s"


def _summarize_counts(prefix: str, counts: dict[str, int]) -> str | None:
    """Render snapshot counts without implying hidden progress."""
    if not counts:
        return None
    details = ", ".join(f"{name}={count}" for name, count in sorted(counts.items()))
    return f"{prefix} include {details}."


def _diagnostics_evidence(diagnostics: RunDiagnosticsResponse) -> list[str]:
    """Build plain-English evidence from the visible diagnostics snapshot."""
    evidence: list[str] = []
    if diagnostics.cancel_requested:
        evidence.append("Cancellation has been requested; active work is draining toward a terminal cancelled status.")
    token_count = diagnostics.summary.token_count
    if token_count > 0:
        evidence.append(f"{_counted('token', token_count)} {'is' if token_count == 1 else 'are'} visible in the runtime trace.")
        if diagnostics.summary.preview_truncated:
            evidence.append(f"The preview is limited to the first {_counted('token', diagnostics.summary.preview_limit)}.")

    state_summary = _summarize_counts("Node states", diagnostics.summary.state_counts)
    if state_summary is not None:
        evidence.append(state_summary)

    operation_summary = _summarize_counts("Operation records", diagnostics.summary.operation_counts)
    if operation_summary is not None:
        evidence.append(operation_summary)

    for artifact in diagnostics.artifacts[:3]:
        evidence.append(f"Saved output is visible at {artifact.path_or_uri}.")
    if len(diagnostics.artifacts) > 3:
        additional_artifacts = len(diagnostics.artifacts) - 3
        evidence.append(
            f"{_counted('additional saved output', additional_artifacts)} {'is' if additional_artifacts == 1 else 'are'} visible."
        )

    if diagnostics.summary.latest_activity_at is not None:
        evidence.append(f"Latest recorded activity is {diagnostics.summary.latest_activity_at.isoformat()}.")

    if not evidence:
        evidence.append("No tokens, operations, or saved outputs are visible yet.")
    return evidence


def _fallback_diagnostics_working_view(
    explanation: str,
    diagnostics: RunDiagnosticsResponse,
) -> RunDiagnosticsWorkingView:
    """Synthesize a working view when the LLM returns plain text."""
    has_runtime_records = bool(
        diagnostics.summary.token_count or diagnostics.summary.state_counts or diagnostics.summary.operation_counts or diagnostics.artifacts
    )
    if diagnostics.artifacts:
        headline = "The run has produced saved output"
    elif diagnostics.cancel_requested:
        headline = "Cancellation requested"
    elif has_runtime_records:
        headline = "Runtime records are updating"
    else:
        headline = "No runtime records are visible yet"

    if explanation.strip():
        meaning = explanation.strip()
    elif diagnostics.cancel_requested:
        meaning = "The server has received the cancel request and is waiting for active work to stop."
    elif has_runtime_records:
        meaning = "The run has visible runtime records, so the server is doing work beyond showing the spinner."
    else:
        meaning = "The run may still be setting up; no bounded runtime records are visible in Landscape yet."

    next_steps: list[str] = []
    if diagnostics.artifacts:
        next_steps.append("Check the saved output path when the run completes.")
    if diagnostics.run_status in RUN_STATUS_NON_TERMINAL_VALUES:
        next_steps.append("Refresh diagnostics if the visible evidence does not change soon.")

    return RunDiagnosticsWorkingView(
        headline=headline,
        evidence=_diagnostics_evidence(diagnostics),
        meaning=meaning,
        next_steps=next_steps,
    )


def _strip_json_code_fence(text: str) -> str:
    """Accept fenced JSON defensively while the prompt still forbids it."""
    lines = text.strip().splitlines()
    if len(lines) >= 3 and lines[0].strip().startswith("```") and lines[-1].strip() == "```":
        return "\n".join(lines[1:-1]).strip()
    return text.strip()


def _parse_run_diagnostics_working_view(
    explanation: str,
    diagnostics: RunDiagnosticsResponse,
) -> tuple[str, RunDiagnosticsWorkingView]:
    """Parse the composer JSON response, falling back to visible evidence."""
    stripped = explanation.strip()
    try:
        working_view = RunDiagnosticsWorkingView.model_validate_json(_strip_json_code_fence(stripped))
    except ValidationError:
        return stripped, _fallback_diagnostics_working_view(stripped, diagnostics)
    return working_view.meaning, working_view


# ── Router ─────────────────────────────────────────────────────────────


def create_execution_router() -> APIRouter:
    """Create the execution router with REST + WebSocket endpoints."""
    router = APIRouter(tags=["execution"])

    # ── Session-scoped endpoints (validate, execute) ──────────────────

    @router.post(
        "/api/sessions/{session_id}/validate",
        response_model=ValidationResult,
    )
    async def validate_session_pipeline(
        session_id: UUID,
        request: Request,
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> ValidationResult:
        """Dry-run validation using real engine code paths."""
        await verify_session_ownership(session_id, user, request)
        result = await service.validate(session_id, user_id=user.user_id)
        return result

    @router.post(
        "/api/sessions/{session_id}/execute",
        status_code=202,
    )
    async def execute_pipeline(
        session_id: UUID,
        request: Request,
        state_id: UUID | None = None,
        execute_request: ExecuteRequest | None = Body(default=None),  # noqa: B008
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> dict[str, str]:
        """Start a background pipeline run. Returns run_id immediately.

        RunAlreadyActiveError propagates to the app-level exception handler
        (Seam Contract D) which returns the canonical 409 envelope:
        {"detail": str(exc), "error_type": "run_already_active"}.
        """
        await verify_session_ownership(session_id, user, request)
        settings: WebSettings = request.app.state.settings
        fanout_ack_token = execute_request.fanout_ack_token if execute_request is not None else None
        try:
            run_id = await service.execute(
                session_id,
                state_id,
                user_id=user.user_id,
                auth_provider_type=settings.auth_provider,
                fanout_ack_token=fanout_ack_token,
            )
        except StateAccessError:
            # IDOR contract: the "state does not exist" and
            # "state belongs to another session" branches in the
            # service MUST surface here as byte-identical 404
            # responses.  Distinguishable ``detail`` strings would
            # let an authenticated attacker probe arbitrary UUIDs
            # against their own /execute and learn which ones exist
            # in OTHER users' sessions — the same oracle commit
            # e73a921a closed on ``send_message``.  If a future
            # refactor needs diagnostic precision, route it through
            # server-side audit/telemetry, never through the HTTP
            # response body.
            raise HTTPException(status_code=404, detail="State not found") from None
        except BlobNotFoundError:
            # IDOR contract (mirrors StateAccessError above): the
            # nonexistent-blob and cross-session-blob branches MUST
            # surface here as byte-identical 404 responses.  Before
            # this handler existed, nonexistent-blob propagated as a
            # 500 while cross-session-blob returned a 404 — the IDOR
            # status itself was a side channel.
            raise HTTPException(status_code=404, detail="Blob not found") from None
        except BlobSourcePathMismatchError as exc:
            # Tier 1 audit-integrity violation: composer-stored source
            # path diverges from the canonical blob storage_path.  The
            # exception carries both paths for operator triage; we log
            # them server-side but redact them from the HTTP response
            # because the path discloses internal storage layout to any
            # caller (including the LLM agent driving the composer in
            # an MCP context).  See elspeth-07089fbaa3.
            # Per CLAUDE.md logging policy, slog is permitted for
            # audit-system failures.  Tier 1 corruption of
            # composition_states.source.options.path qualifies: the
            # audit row exists but its content is structurally invalid,
            # so neither audit (Landscape — not yet open for this run)
            # nor operational telemetry can capture the divergence.
            # slog is the only channel the operator can use to
            # correlate the redacted HTTP body with the actual paths.
            slog.error(
                "blob_source_path_mismatch",
                blob_id=exc.blob_id,
                session_id=exc.session_id,
                stored_path=exc.stored_path,
                canonical_path=exc.canonical_path,
                issue="elspeth-07089fbaa3",
            )
            raise HTTPException(
                status_code=500,
                detail={
                    "kind": "blob_source_path_mismatch",
                    "issue": "elspeth-07089fbaa3",
                    "message": (
                        "Composer-stored blob source path is not "
                        "structurally valid for the bound blob.  This "
                        "indicates a bug in composer persistence; the "
                        "operator must investigate the captured "
                        "composition state."
                    ),
                },
            ) from exc
        except ExecutionFanoutGuardRequired as exc:
            raise HTTPException(
                status_code=428,
                detail={
                    "error_type": FANOUT_GUARD_ERROR_TYPE,
                    "detail": str(exc),
                    "fanout_guard": exc.guard.to_dict(),
                },
            ) from exc
        except SemanticContractViolationError as exc:
            # Structured 422 with the same payload shape /validate
            # surfaces. Status 422 (Unprocessable Entity) — the
            # request was syntactically valid but the composition
            # fails plugin-declared semantic contracts. The
            # bare-ValueError branch below maps to 404 because most
            # other ValueErrors at this site are state-not-found
            # cases that echo the caller's own input; semantic
            # violations are NOT state-not-found and need their own
            # status. SemanticContractViolationError IS a
            # ValueError, so this handler MUST sit above the bare
            # ``except ValueError`` (the catch-order discipline hook
            # enforces that).
            raise HTTPException(
                status_code=422,
                detail={
                    "kind": "semantic_contract_violation",
                    "errors": [
                        {
                            "component": entry.component,
                            "message": entry.message,
                            "severity": entry.severity,
                        }
                        for entry in exc.entries
                    ],
                    "semantic_contracts": [
                        {
                            "from_id": contract.from_id,
                            "to_id": contract.to_id,
                            "consumer_plugin": contract.consumer_plugin,
                            "producer_plugin": contract.producer_plugin,
                            "producer_field": contract.producer_field,
                            "consumer_field": contract.consumer_field,
                            "outcome": contract.outcome.value,
                            "requirement_code": contract.requirement.requirement_code,
                        }
                        for contract in exc.contracts
                    ],
                },
            ) from exc
        except ExecuteRequestValidationError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from None
        except ValueError as exc:
            # Remaining ValueError sources are non-IDOR: the user's
            # OWN session having no composition state (when state_id
            # is None). Caller-authored request validation failures
            # (path allowlist, malformed blob_ref) raise
            # ExecuteRequestValidationError above and return 400.
            raise HTTPException(status_code=404, detail=str(exc)) from None
        return {"run_id": str(run_id)}

    # ── Run-scoped endpoints (status, cancel, results) ────────────────

    @router.get(
        "/api/runs/{run_id}",
        response_model=RunStatusResponse,
    )
    async def get_run_status(
        run_id: UUID,
        request: Request,
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> RunStatusResponse:
        """Return current run status."""
        await _verify_run_ownership(run_id, user, request)
        try:
            status = await _load_run_status_with_accounting(run_id, app=request.app, service=service)
        except _RunStatusNotFoundError:
            raise _run_not_found_http() from None
        except (ValidationError, _RunStatusIntegrityError) as exc:
            raise _run_integrity_http(exc) from exc
        if status.status in RUN_STATUS_TERMINAL_VALUES and status.landscape_run_id is not None and status.discard_summary is None:
            from elspeth.web.execution.discard_summary import load_discard_summaries_for_settings

            discard_summaries = await run_sync_in_worker(
                load_discard_summaries_for_settings,
                request.app.state.settings,
                (status.landscape_run_id,),
            )
            if status.landscape_run_id in discard_summaries:
                status = status.model_copy(update={"discard_summary": discard_summaries[status.landscape_run_id]})
        return status

    @router.get(
        "/api/runs/{run_id}/diagnostics",
        response_model=RunDiagnosticsResponse,
    )
    async def get_run_diagnostics(
        run_id: UUID,
        request: Request,
        limit: int = Query(50, ge=1, le=100),
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> RunDiagnosticsResponse:
        """Return a bounded Landscape diagnostics snapshot for a run."""
        await _verify_run_ownership(run_id, user, request)
        try:
            status = await _load_run_status_with_accounting(run_id, app=request.app, service=service)
        except _RunStatusNotFoundError:
            raise _run_not_found_http() from None
        except (ValidationError, _RunStatusIntegrityError) as exc:
            raise _run_integrity_http(exc) from exc

        landscape_run_id = status.landscape_run_id or status.run_id
        return await run_sync_in_worker(
            load_run_diagnostics_for_settings,
            request.app.state.settings,
            run_id=status.run_id,
            landscape_run_id=landscape_run_id,
            run_status=status.status,
            cancel_requested=status.cancel_requested,
            limit=limit,
        )

    @router.post(
        "/api/runs/{run_id}/diagnostics/evaluate",
        response_model=RunDiagnosticsEvaluationResponse,
    )
    async def evaluate_run_diagnostics(
        run_id: UUID,
        request: Request,
        limit: int = Query(50, ge=1, le=100),
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> RunDiagnosticsEvaluationResponse:
        """Ask the configured LLM to explain the current diagnostics snapshot."""
        await _verify_run_ownership(run_id, user, request)
        try:
            status = await _load_run_status_with_accounting(run_id, app=request.app, service=service)
        except _RunStatusNotFoundError:
            raise _run_not_found_http() from None
        except (ValidationError, _RunStatusIntegrityError) as exc:
            raise _run_integrity_http(exc) from exc

        landscape_run_id = status.landscape_run_id or status.run_id
        diagnostics = await run_sync_in_worker(
            load_run_diagnostics_for_settings,
            request.app.state.settings,
            run_id=status.run_id,
            landscape_run_id=landscape_run_id,
            run_status=status.status,
            cancel_requested=status.cancel_requested,
            limit=limit,
        )

        composer: ComposerService = request.app.state.composer_service
        try:
            explanation = await composer.explain_run_diagnostics(diagnostics.model_dump(mode="json"))
        except ComposerServiceError as exc:
            raise HTTPException(
                status_code=502,
                detail={"error_type": "run_diagnostics_explanation_failed", "detail": str(exc)},
            ) from exc

        explanation, working_view = _parse_run_diagnostics_working_view(explanation, diagnostics)
        return RunDiagnosticsEvaluationResponse(
            run_id=status.run_id,
            generated_at=datetime.now(UTC),
            explanation=explanation,
            working_view=working_view,
        )

    @router.post("/api/runs/{run_id}/cancel")
    async def cancel_run(
        run_id: UUID,
        request: Request,
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> dict[str, str | bool]:
        """Cancel a run. Idempotent on terminal runs."""
        await _verify_run_ownership(run_id, user, request)
        try:
            await service.cancel(run_id)
            status = await _load_run_status_with_accounting(run_id, app=request.app, service=service)
        except _RunStatusNotFoundError:
            raise _run_not_found_http() from None
        except (ValidationError, _RunStatusIntegrityError) as exc:
            raise _run_integrity_http(exc) from exc
        return {"status": status.status, "cancel_requested": status.cancel_requested}

    @router.get(
        "/api/runs/{run_id}/results",
        response_model=RunResultsResponse,
    )
    async def get_run_results(
        run_id: UUID,
        request: Request,
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> RunResultsResponse:
        """Return final run results. 409 if run is not terminal."""
        await _verify_run_ownership(run_id, user, request)
        try:
            status = await _load_run_status_with_accounting(run_id, app=request.app, service=service)
        except _RunStatusNotFoundError:
            raise _run_not_found_http() from None
        except (ValidationError, _RunStatusIntegrityError) as exc:
            raise _run_integrity_http(exc) from exc
        if status.status in RUN_STATUS_NON_TERMINAL_VALUES:
            raise HTTPException(
                status_code=409,
                detail=f"Run is still {status.status}",
            )
        if status.landscape_run_id is not None and status.discard_summary is None:
            from elspeth.web.execution.discard_summary import load_discard_summaries_for_settings

            discard_summaries = await run_sync_in_worker(
                load_discard_summaries_for_settings,
                request.app.state.settings,
                (status.landscape_run_id,),
            )
            if status.landscape_run_id in discard_summaries:
                status = status.model_copy(update={"discard_summary": discard_summaries[status.landscape_run_id]})
        # mypy can't narrow a Literal through frozenset membership — the
        # cast is safe because RUN_STATUS_NON_TERMINAL_VALUES is the exact
        # complement of RunResultsResponse's Literal values, enforced by a
        # module-load assertion in schemas.py.
        # Phase 2.2 (elspeth-0de989c56d): cast to the canonical 5-value
        # TerminalSessionRunStatus, not a hardcoded 3-value tuple — the
        # latter would mislead readers into thinking the API still uses the
        # narrow taxonomy after the widening.
        terminal_status = cast(TerminalSessionRunStatus, status.status)
        return RunResultsResponse(
            run_id=status.run_id,
            status=terminal_status,
            accounting=status.accounting,
            landscape_run_id=status.landscape_run_id,
            error=status.error,
            discard_summary=status.discard_summary,
        )

    # ── WebSocket Endpoint ─────────────────────────────────────────────

    @router.websocket("/ws/runs/{run_id}")
    async def websocket_run_progress(
        websocket: WebSocket,
        run_id: str,
        token: str | None = None,
    ) -> None:
        """Stream RunEvent JSON payloads for a specific run.

        AC #12: Authentication via ?token=<jwt> query parameter.
        Close code 4001 on auth failure — client MUST NOT auto-reconnect
        on 4001 (token must be refreshed or user must re-authenticate).
        """
        broadcaster: ProgressBroadcaster = websocket.app.state.broadcaster
        auth_provider: AuthProvider = websocket.app.state.auth_provider
        service: ExecutionService = websocket.app.state.execution_service

        # Auth: validate JWT from query parameter
        if token is None:
            await websocket.close(code=4001, reason="Missing authentication token")
            return
        try:
            user = await auth_provider.authenticate(token)
        except AuthenticationError:
            await websocket.close(code=4001, reason="Invalid authentication token")
            return

        await websocket.accept()

        # IDOR protection: verify authenticated user owns this run's session
        try:
            run_ownership = await service.verify_run_ownership(user, run_id)
            if not run_ownership:
                await websocket.close(code=4004, reason="Run not found")
                return
        except ValueError:
            await websocket.close(code=4004, reason="Run not found")
            return

        # Subscribe BEFORE checking terminal status to close the race
        # window where a run finishes between get_status() and subscribe().
        # If subscribed first, any terminal event broadcast during the check
        # lands in the queue and won't be lost.
        queue = broadcaster.subscribe(run_id)
        try:
            # Seed: if the run already reached a terminal state before the
            # client connected (short runs, page refresh), send the terminal
            # status immediately and close.
            try:
                current_snapshot = await _load_run_status_snapshot_with_accounting(UUID(run_id), app=websocket.app, service=service)
            except _RunStatusNotFoundError:
                await websocket.close(code=4004, reason="Run not found")
                return
            except (ValidationError, _RunStatusIntegrityError):
                await websocket.close(code=1011, reason="Run status failed internal accounting validation")
                return
            current = current_snapshot.response
            if current.status in RUN_STATUS_TERMINAL_VALUES:
                event = _build_terminal_run_event(current, cancelled_run_record=current_snapshot.record)
                await websocket.send_json(event.model_dump(mode="json"))
                await websocket.close(code=1000)
                return
            while True:
                try:
                    event = await asyncio.wait_for(queue.get(), timeout=60.0)
                except TimeoutError:
                    # Idle timeout — a terminal broadcast may have been missed.
                    # Re-check authoritative run status instead of sending an
                    # ad-hoc payload outside the RunEvent contract.
                    try:
                        current_snapshot = await _load_run_status_snapshot_with_accounting(UUID(run_id), app=websocket.app, service=service)
                    except _RunStatusNotFoundError:
                        await websocket.close(code=4004, reason="Run not found")
                        break
                    except (ValidationError, _RunStatusIntegrityError):
                        await websocket.close(code=1011, reason="Run status failed internal accounting validation")
                        break
                    current = current_snapshot.response
                    if current.status in RUN_STATUS_TERMINAL_VALUES:
                        terminal_event = _build_terminal_run_event(current, cancelled_run_record=current_snapshot.record)
                        await websocket.send_json(terminal_event.model_dump(mode="json"))
                        await websocket.close(code=1000)
                        break
                    continue
                await websocket.send_json(event.model_dump(mode="json"))
                # "error" events are non-terminal (per-row exceptions).
                # "completed", "cancelled", and "failed" are terminal.
                if event.event_type in ("completed", "cancelled", "failed"):
                    await websocket.close(code=1000)
                    break
        except WebSocketDisconnect:
            pass  # Client disconnected — fall through to finally
        except (ConnectionError, OSError) as exc:
            slog.error(
                "websocket_handler_error",
                run_id=run_id,
                error=str(exc),
            )
            try:
                await websocket.close(code=1011, reason="Internal server error")
            except (WebSocketDisconnect, ConnectionError, OSError) as close_err:
                slog.error("websocket_close_failed", run_id=run_id, error=str(close_err))
        finally:
            broadcaster.unsubscribe(run_id, queue)

    # NOTE on placement: the run-outputs endpoints sit AFTER
    # websocket_run_progress in this file rather than next to
    # get_run_diagnostics (their conceptual sibling). Reason: the
    # tier-model allowlist (config/cicd/enforce_tier_model/web.yaml)
    # uses AST-path-based fingerprints which include the function's
    # body-level index. Inserting siblings BEFORE websocket_run_progress
    # shifts that index and invalidates the existing allowlist entries.
    # Appending here keeps existing fingerprints stable.

    @router.get(
        "/api/runs/{run_id}/outputs",
        response_model=RunOutputsResponse,
    )
    async def get_run_outputs(
        run_id: UUID,
        request: Request,
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> RunOutputsResponse:
        """Return the FULL manifest of sink-write artefacts for a run.

        Distinct from ``GET /api/runs/{run_id}/diagnostics``, whose
        ``artifacts`` field is capped at 20 for operator-UI pacing. This
        endpoint is the audit-evidence retrieval surface — every artefact
        the run wrote, with ``content_hash`` and ``exists_now``.
        """
        await _verify_run_ownership(run_id, user, request)
        try:
            status = await _load_run_status_with_accounting(run_id, app=request.app, service=service)
        except _RunStatusNotFoundError:
            raise _run_not_found_http() from None
        except (ValidationError, _RunStatusIntegrityError) as exc:
            raise _run_integrity_http(exc) from exc

        landscape_run_id = status.landscape_run_id or status.run_id
        try:
            return await run_sync_in_worker(
                load_run_outputs_for_settings,
                request.app.state.settings,
                run_id=status.run_id,
                landscape_run_id=landscape_run_id,
            )
        except RunOutputsAuditUnavailableError as exc:
            raise HTTPException(
                status_code=503,
                detail={
                    "error_type": "run_outputs_audit_unavailable",
                    "landscape_run_id": exc.landscape_run_id,
                    "audit_location": exc.audit_location,
                },
            ) from exc

    @router.get("/api/runs/{run_id}/outputs/{artifact_id}/content")
    async def get_run_output_content(
        run_id: UUID,
        artifact_id: str,
        request: Request,
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> Any:
        """Stream the bytes of one artefact written by a run.

        Path-allowlist guard: refuses any artefact whose ``path_or_uri``
        resolves outside ``allowed_sink_directories(data_dir)`` (the
        canonical ``data_dir/{outputs,blobs}`` set). This is
        defence-in-depth — the path was already allowlisted at write
        time, but the audit row is read-mutable in principle and the
        read-side guard MUST NOT trust it.

        Returns:
        * 200 with file bytes when path is in-allowlist and exists.
        * 403 when path is outside allowlist.
        * 404 when artefact is not in the run's manifest.
        * 410 when path was in-allowlist but file no longer exists.
        """
        await _verify_run_ownership(run_id, user, request)
        try:
            status = await _load_run_status_with_accounting(run_id, app=request.app, service=service)
        except _RunStatusNotFoundError:
            raise _run_not_found_http() from None
        except (ValidationError, _RunStatusIntegrityError) as exc:
            raise _run_integrity_http(exc) from exc

        landscape_run_id = status.landscape_run_id or status.run_id
        try:
            manifest = await run_sync_in_worker(
                load_run_outputs_for_settings,
                request.app.state.settings,
                run_id=status.run_id,
                landscape_run_id=landscape_run_id,
            )
        except RunOutputsAuditUnavailableError as exc:
            raise HTTPException(
                status_code=503,
                detail={
                    "error_type": "run_outputs_audit_unavailable",
                    "landscape_run_id": exc.landscape_run_id,
                    "audit_location": exc.audit_location,
                },
            ) from exc
        artifact = next(
            (a for a in manifest.artifacts if a.artifact_id == artifact_id),
            None,
        )
        if artifact is None:
            raise HTTPException(
                status_code=404,
                detail={"error_type": "artifact_not_found", "artifact_id": artifact_id},
            )

        fs_path = path_or_uri_to_filesystem_path(artifact.path_or_uri)
        if fs_path is None:
            # Object-store URI (azure://, dataverse://) — content streaming
            # is not implemented for these. Audit-evidence retrieval for
            # remote sinks goes through their own retrieval API.
            raise HTTPException(
                status_code=415,
                detail={
                    "error_type": "object_store_artifact_not_streamable",
                    "path_or_uri": artifact.path_or_uri,
                },
            )

        resolved = fs_path.resolve()
        data_dir = request.app.state.settings.data_dir
        allowed = allowed_sink_directories(data_dir)
        if not any(resolved.is_relative_to(base) for base in allowed):
            raise HTTPException(
                status_code=403,
                detail={
                    "error_type": "output_path_outside_allowlist",
                    "path_or_uri": artifact.path_or_uri,
                },
            )

        if not resolved.exists():
            raise HTTPException(
                status_code=410,
                detail={
                    "error_type": "artifact_purged_or_moved",
                    "path_or_uri": artifact.path_or_uri,
                },
            )

        return FileResponse(resolved, filename=resolved.name)

    @router.get(
        "/api/runs/{run_id}/outputs/{artifact_id}/preview",
        response_model=RunOutputArtifactPreview,
    )
    async def get_run_output_preview(
        run_id: UUID,
        artifact_id: str,
        request: Request,
        user: UserIdentity = Depends(get_current_user),  # noqa: B008
        service: ExecutionService = Depends(_get_execution_service),  # noqa: B008
    ) -> RunOutputArtifactPreview:
        """Return a bounded head-of-file preview of one sink-write artefact.

        Companion to ``/content``: where ``/content`` streams the full
        file, ``/preview`` reads at most 256 KiB or 100 rows so the
        operator UI can render an inline preview without a full
        download. Same path-allowlist guard, same ownership check —
        the only behavioural difference is bounded read.

        Returns:
        * 200 with ``RunOutputArtifactPreview`` on success.
        * 403 when path is outside allowlist.
        * 404 when artefact is not in the run's manifest.
        * 410 when path was in-allowlist but file no longer exists
          (frontend treats this as the "no longer available on disk"
          state, mirroring the manifest's ``exists_now=False``).
        * 415 when the artefact is non-file (object-store URI).
        """
        await _verify_run_ownership(run_id, user, request)
        try:
            status = await _load_run_status_with_accounting(run_id, app=request.app, service=service)
        except _RunStatusNotFoundError:
            raise _run_not_found_http() from None
        except (ValidationError, _RunStatusIntegrityError) as exc:
            raise _run_integrity_http(exc) from exc

        landscape_run_id = status.landscape_run_id or status.run_id
        try:
            manifest = await run_sync_in_worker(
                load_run_outputs_for_settings,
                request.app.state.settings,
                run_id=status.run_id,
                landscape_run_id=landscape_run_id,
            )
        except RunOutputsAuditUnavailableError as exc:
            raise HTTPException(
                status_code=503,
                detail={
                    "error_type": "run_outputs_audit_unavailable",
                    "landscape_run_id": exc.landscape_run_id,
                    "audit_location": exc.audit_location,
                },
            ) from exc
        artifact = next(
            (a for a in manifest.artifacts if a.artifact_id == artifact_id),
            None,
        )
        if artifact is None:
            raise HTTPException(
                status_code=404,
                detail={"error_type": "artifact_not_found", "artifact_id": artifact_id},
            )

        fs_path = path_or_uri_to_filesystem_path(artifact.path_or_uri)
        if fs_path is None:
            raise HTTPException(
                status_code=415,
                detail={
                    "error_type": "object_store_artifact_not_previewable",
                    "path_or_uri": artifact.path_or_uri,
                },
            )

        resolved = fs_path.resolve()
        data_dir = request.app.state.settings.data_dir
        allowed = allowed_sink_directories(data_dir)
        if not any(resolved.is_relative_to(base) for base in allowed):
            raise HTTPException(
                status_code=403,
                detail={
                    "error_type": "output_path_outside_allowlist",
                    "path_or_uri": artifact.path_or_uri,
                },
            )

        if not resolved.exists():
            # Manifest/preview race: file existed at manifest-load time
            # but is gone now (purged, retention, manual delete). Match
            # the /content endpoint's vocabulary — frontend handles either.
            raise HTTPException(
                status_code=410,
                detail={
                    "error_type": "artifact_purged_or_moved",
                    "path_or_uri": artifact.path_or_uri,
                },
            )

        return await run_sync_in_worker(
            build_artifact_preview,
            resolved,
            artifact_id=artifact_id,
        )

    return router
