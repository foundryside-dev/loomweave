"""Core Orchestrator class for pipeline execution.

Coordinates:
- Run initialization
- Source loading
- Row processing
- Sink writing
- Run completion
- Post-run audit export (when configured)

The Orchestrator is the main entry point for running ELSPETH pipelines.
It delegates to focused helper modules for:
- Validation: Route and sink validation (validation.py)
- Export: Landscape export functionality (export.py)
- Aggregation: Timeout and flush handling (aggregation.py)
"""

from __future__ import annotations

import hashlib
import json
import signal
import threading
import time
from collections.abc import Callable, Iterator, Mapping, Sequence
from contextlib import contextmanager, nullcontext
from dataclasses import replace
from datetime import UTC, datetime
from itertools import chain
from types import SimpleNamespace
from typing import TYPE_CHECKING, Any

import structlog

if TYPE_CHECKING:
    from elspeth.contracts.aggregation_checkpoint import AggregationCheckpointState
    from elspeth.contracts.coalesce_checkpoint import CoalesceCheckpointState
    from elspeth.contracts.events import TelemetryEvent
    from elspeth.contracts.payload_store import PayloadStore
    from elspeth.core.events import EventBusProtocol
    from elspeth.telemetry import TelemetryManager

import elspeth.contracts.errors as contract_errors
import elspeth.engine.executors.declaration_contract_bootstrap  # noqa: F401
from elspeth import __version__ as ENGINE_VERSION
from elspeth.contracts import (
    ExportStatus,
    NodeType,
    PendingOutcome,
    PipelineRow,
    RouteDestination,
    RunStatus,
    SchemaContract,
    SecretResolutionInput,
    SinkProtocol,
    SourceProtocol,
    SourceRow,
    TokenInfo,
    TransformProtocol,
)
from elspeth.contracts.cli import ProgressEvent
from elspeth.contracts.config import RuntimeRetryConfig
from elspeth.contracts.declaration_contracts import (
    EXPECTED_CONTRACT_SITES,
    contract_sites,
    declaration_registry_is_frozen,
    freeze_declaration_registry,
    registered_declaration_contracts,
)
from elspeth.contracts.enums import NodeStateStatus, RoutingMode, TerminalOutcome, TerminalPath
from elspeth.contracts.errors import (
    ExecutionError,
    FrameworkBugError,
    GracefulShutdownError,
    OrchestrationInvariantError,
    SourceQuarantineReason,
    TelemetryExporterError,
)
from elspeth.contracts.events import (
    FieldResolutionApplied,
    PhaseAction,
    PhaseChanged,
    PhaseCompleted,
    PhaseError,
    PhaseStarted,
    PipelinePhase,
    RowCreated,
    RunCompletionStatus,
    RunFinished,
    RunStarted,
    RunSummary,
)
from elspeth.contracts.hashing import repr_hash
from elspeth.contracts.plugin_context import PluginContext
from elspeth.contracts.run_result import derive_terminal_run_status
from elspeth.contracts.runtime_val_manifest import build_runtime_val_manifest
from elspeth.contracts.schema_contract_factory import create_contract_from_config
from elspeth.contracts.tier_registry import freeze_tier_registry
from elspeth.contracts.types import (
    AggregationName,
    BranchName,
    CoalesceName,
    GateName,
    NodeID,
    SinkName,
)
from elspeth.core.canonical import canonical_json, sanitize_for_canonical, stable_hash
from elspeth.core.config import AggregationSettings
from elspeth.core.dag import ExecutionGraph
from elspeth.core.landscape import LandscapeDB
from elspeth.core.landscape.factory import RecorderFactory
from elspeth.core.operations import track_operation
from elspeth.engine._best_effort import best_effort
from elspeth.engine.executors.sink import DiversionCounts

# Import module functions from orchestrator submodules
from elspeth.engine.orchestrator.aggregation import (
    check_aggregation_timeouts,
    flush_remaining_aggregation_buffers,
    handle_incomplete_batches,
    rebind_checkpoint_batch_ids,
)
from elspeth.engine.orchestrator.export import (
    export_landscape,
    reconstruct_schema_from_json,
)
from elspeth.engine.orchestrator.outcomes import (
    accumulate_row_outcomes,
    flush_coalesce_pending,
    handle_coalesce_timeouts,
    reconcile_sink_write_diversions,
)
from elspeth.engine.orchestrator.types import (
    AggNodeEntry,
    ExecutionCounters,
    GraphArtifacts,
    LoopContext,
    LoopResult,
    PendingTokenMap,
    PipelineConfig,
    ResumeState,
    RouteValidationError,
    RowPlugin,
    RunContext,
    RunResult,
    _CheckpointFactory,
)
from elspeth.engine.orchestrator.validation import (
    validate_route_destinations,
    validate_sink_failsink_destinations,
    validate_source_quarantine_destination,
    validate_transform_error_sinks,
)
from elspeth.engine.processor import DAGTraversalContext, RowProcessor, make_step_resolver
from elspeth.engine.retry import RetryManager
from elspeth.engine.spans import SpanFactory

if TYPE_CHECKING:
    from elspeth.contracts import ResumePoint
    from elspeth.contracts.config.runtime import RuntimeCheckpointConfig, RuntimeConcurrencyConfig
    from elspeth.core.checkpoint import CheckpointManager
    from elspeth.core.config import ElspethSettings, GateSettings
    from elspeth.core.dependency_config import PreflightResult
    from elspeth.core.rate_limit import RateLimitRegistry
    from elspeth.engine.clock import Clock
    from elspeth.engine.coalesce_executor import CoalesceExecutor

slog = structlog.get_logger(__name__)


class _RunFailedWithPartialResultError(Exception):
    """Internal wrapper used to move partial counters to outer failure handlers."""

    def __init__(self, original_error: Exception, partial_result: RunResult) -> None:
        super().__init__(str(original_error))
        self.original_error = original_error
        self.partial_result = partial_result
        self.original_traceback = original_error.__traceback__


def prepare_for_run() -> None:
    """Assert framework invariants and freeze both registries at bootstrap.

    This is the canonical bootstrap entry point (ADR-010 §Decision 3). It must
    be called AFTER all plugin modules have been imported (so module-level
    side-effects like ``register_declaration_contract`` have fired) and BEFORE
    any DAG node begins execution.

    The normal import chain guarantees this ordering in production:
    - ``orchestrator/core.py`` imports
      ``engine.executors.declaration_contract_bootstrap`` at module level.
    - ``declaration_contract_bootstrap.py`` imports every production contract
      module with a module-level ``register_declaration_contract(...)`` call.
    - Each imported contract module registers its contract as a module-level
      side-effect.

    If the declaration registry is empty at this point, the import chain is
    broken — this is an import-order bug, not a runtime configuration error.
    Crashing here prevents the framework from running silently without any
    runtime VAL checks active (the exact failure mode ADR-010 was designed to
    prevent).

    Raises:
        RuntimeError: every registered ``(contract_name, dispatch_site)``
            pair does not exactly equal the pairs in
            ``EXPECTED_CONTRACT_SITES`` (N1 per-site manifest extension,
            ADR-010 §Semantics amendment 2026-04-20). The message names
            every missing and every extra contract or site so the failure
            is self-diagnosing. Indicates one of: an import-order bug
            (contract module not imported), manifest drift (contract
            registered without a manifest entry), or site drift
            (contract's ``@implements_dispatch_site`` markers disagree
            with the manifest).
    """
    # Short-circuit if the registry is already frozen — bootstrap already ran.
    # Idempotency is required because Orchestrator.run() can be called multiple
    # times in a single process (e.g. test suites). The manifest-equality
    # assertion only needs to fire ONCE, on the first call; subsequent calls
    # trust that the previous freeze was performed after a successful
    # assertion.
    #
    # The ``_clear_registry_for_tests()`` helper resets ``_FROZEN = False``, so
    # test isolation that clears and repopulates the registry will still trigger
    # the assertion on the next call.
    if declaration_registry_is_frozen():
        return

    # ADR-010 §Decision 3 manifest gate, extended by §H2 landing scope N1:
    # Assert SET EQUALITY between every registered (contract_name,
    # dispatch_site) pair and every pair in ``EXPECTED_CONTRACT_SITES``
    # BEFORE freezing. The original C2 closure checked contract-name
    # equality; N1 tightens this to per-(name, site) equality so a
    # contract registering for the wrong site (or silently no-opping at a
    # site it claims to cover) is detected at bootstrap, not masked until
    # first row.
    #
    # Every plugin behaviour recorded as "compliant" (no violation raised)
    # must be evidence of every applicable contract's method having been
    # invoked — under audit-complete semantics (ADR-010 §Semantics) absence of violation
    # means "checked and passed," which is only true if the dispatcher
    # actually dispatched to the contract for its claimed sites. The N1
    # manifest closes the (name, site) drift vector the C2 set-of-names
    # manifest missed.
    contracts = registered_declaration_contracts()
    registered_sites: dict[str, frozenset[str]] = {c.name: frozenset(contract_sites(c)) for c in contracts}
    expected_sites: dict[str, frozenset[str]] = {name: frozenset(sites) for name, sites in EXPECTED_CONTRACT_SITES.items()}
    if registered_sites != expected_sites:
        # Compose a self-diagnosing message naming every drifted (name, site)
        # pair. Five mutually exclusive drift classes are surfaced:
        #   * name missing (manifest claims, nothing registered)
        #   * name extra (registered, manifest absent)
        #   * per-name: sites missing (contract registered with fewer sites
        #     than manifest declares)
        #   * per-name: sites extra (contract registered with more sites
        #     than manifest declares)
        #   * per-name: site-set mismatch (disjoint)
        expected_names = frozenset(expected_sites.keys())
        registered_names = frozenset(registered_sites.keys())
        missing_names = expected_names - registered_names
        extra_names = registered_names - expected_names
        site_drift_lines: list[str] = []
        for name in sorted(expected_names & registered_names):
            expected_for_name = expected_sites[name]
            registered_for_name = registered_sites[name]
            if expected_for_name == registered_for_name:
                continue
            missing_sites = expected_for_name - registered_for_name
            extra_sites = registered_for_name - expected_for_name
            site_drift_lines.append(
                f"  {name!r}: expected_sites={sorted(expected_for_name)!r}, "
                f"registered_sites={sorted(registered_for_name)!r}, "
                f"missing={sorted(missing_sites)!r}, extra={sorted(extra_sites)!r}"
            )
        raise RuntimeError(
            "Declaration contract registry mismatch at orchestrator bootstrap "
            "(ADR-010 §Decision 3 manifest gate + §H2 landing scope N1).\n"
            f"  Expected (manifest):  {sorted((n, sorted(s)) for n, s in expected_sites.items())!r}\n"
            f"  Registered:           {sorted((n, sorted(s)) for n, s in registered_sites.items())!r}\n"
            f"  Missing names (not registered but in manifest): {sorted(missing_names)!r}\n"
            f"  Extra names  (registered but not in manifest): {sorted(extra_names)!r}\n"
            "  Per-name site drift:\n" + ("\n".join(site_drift_lines) if site_drift_lines else "    (none)") + "\n"
            "\n"
            "If a name is missing: the contract's module-level "
            "register_declaration_contract(...) call did not fire. Check for "
            "a conditional import that skipped it, or an import-order bug "
            "where the module was not imported before prepare_for_run().\n"
            "If a name is extra: a contract was registered without being "
            "added to EXPECTED_CONTRACT_SITES. Update the manifest in the "
            "same commit as the registration.\n"
            "If per-name sites drift: the contract's "
            "@implements_dispatch_site(...) markers disagree with "
            "EXPECTED_CONTRACT_SITES. Either fix the markers or update the "
            "manifest (and run scripts/cicd/enforce_contract_manifest.py to "
            "confirm MC3a/b/c are clean).\n"
            "\n"
            "A silent runtime VAL disable is exactly the failure mode ADR-010 "
            "was designed to prevent — extended to per-site coverage under "
            "the §Semantics amendment (2026-04-20)."
        )
    freeze_declaration_registry()
    freeze_tier_registry()


class Orchestrator:
    """Orchestrates full pipeline runs.

    Manages the complete lifecycle:
    1. Begin run in Landscape
    2. Register all nodes (and set node_id on each plugin instance)
    3. Load rows from source
    4. Process rows through transforms
    5. Write to sinks
    6. Complete run

    The Orchestrator sets node_id on each plugin instance AFTER registering
    it with Landscape. This is part of the plugin protocol contract - all
    plugins define node_id: str | None and the orchestrator populates it.
    """

    def __init__(
        self,
        db: LandscapeDB,
        *,
        event_bus: EventBusProtocol | None = None,
        canonical_version: str = "sha256-rfc8785-v1",
        checkpoint_manager: CheckpointManager | None = None,
        checkpoint_config: RuntimeCheckpointConfig | None = None,
        clock: Clock | None = None,
        rate_limit_registry: RateLimitRegistry | None = None,
        concurrency_config: RuntimeConcurrencyConfig | None = None,
        telemetry_manager: TelemetryManager | None = None,
        coalesce_completed_keys_limit: int = 10000,
    ) -> None:
        from elspeth.core.events import NullEventBus
        from elspeth.engine.clock import DEFAULT_CLOCK

        self._db = db
        self._events = event_bus if event_bus is not None else NullEventBus()
        self._canonical_version = canonical_version
        self._span_factory = SpanFactory()
        self._checkpoint_manager = checkpoint_manager
        self._checkpoint_config = checkpoint_config
        self._clock = clock if clock is not None else DEFAULT_CLOCK
        self._rate_limit_registry = rate_limit_registry
        self._concurrency_config = concurrency_config
        self._coalesce_completed_keys_limit = coalesce_completed_keys_limit
        self._sequence_number = 0  # Monotonic counter for checkpoint ordering
        self._current_graph: ExecutionGraph | None = None  # Set during execution for checkpointing
        self._telemetry = telemetry_manager  # Optional, disabled by default

    def _reset_checkpoint_sequence(self) -> None:
        """Reset checkpoint ordering for a fresh run."""
        self._sequence_number = 0

    def _rebase_checkpoint_sequence(self, sequence_number: int) -> None:
        """Continue checkpoint ordering from a previously persisted checkpoint."""
        self._sequence_number = sequence_number

    def _emit_telemetry(self, event: TelemetryEvent) -> None:
        """Emit telemetry event if manager is configured.

        Telemetry is emitted AFTER Landscape recording succeeds. Landscape is
        the legal record; telemetry is operational visibility.

        Args:
            event: The telemetry event to emit
        """
        if self._telemetry is not None:
            self._telemetry.handle_event(event)

    def _flush_telemetry(self) -> None:
        """Flush telemetry events if manager is configured.

        Ensures queued telemetry is exported before returning control to caller.
        """
        if self._telemetry is not None:
            self._telemetry.flush()

    def _emit_phase_error(
        self,
        phase: PipelinePhase,
        error: BaseException,
        target: str | None = None,
    ) -> None:
        """Best-effort PhaseError emission that never masks the original exception.

        Called from except blocks before re-raise. If PhaseError construction
        or EventBus.emit() fails (e.g., handler bug), the original exception
        must take precedence — observable telemetry is secondary to preserving
        the actual error.
        """
        with best_effort(
            "PhaseError emission",
            phase=phase.value,
            original_error=type(error).__name__,
            target=target,
        ):
            self._events.emit(PhaseError(phase=phase, error=error, target=target))

    def _safe_flush_telemetry(self) -> None:
        """Flush telemetry in a finally block, preserving any pending exception.

        If _flush_telemetry() raises TelemetryExporterError (fail_on_total=True),
        only re-raises when no other exception is pending — telemetry failures
        must not mask run errors.
        """
        import sys

        logger = slog
        pending_exc = sys.exc_info()[0]

        try:
            self._flush_telemetry()
        except TelemetryExporterError as e:
            logger.warning(
                "Telemetry flush failed - will raise after cleanup if no other exception pending",
                exporter=e.exporter_name,
                error=e.message,
            )
            if pending_exc is None:
                raise

    def _derive_resume_terminal_status_from_audit(
        self, factory: RecorderFactory, run_id: str
    ) -> tuple[RunStatus, int, int, int, int, int, int]:
        """Phase 2.2 (elspeth-0de989c56d) — recover the truthful terminal
        status of a run from the Landscape audit DB when the resume found
        no unprocessed rows.

        The "all-rows-already-processed" resume branch fires when a prior
        run was interrupted *after* all source rows had reached terminal
        states.  At that point the resume's local counters are 0 (nothing
        was reprocessed), but the audit DB carries the truth in
        ``token_outcomes``.  Pre-Phase-2.2 the engine wrote
        ``RunStatus.COMPLETED`` here unconditionally, masking runs that
        actually failed.  This helper aggregates ``token_outcomes`` and
        applies the presence-indicator predicate so the persisted status
        reflects what really happened.

        Returns:
            ``(terminal_status, rows_processed, rows_succeeded, rows_failed,
            rows_routed_success, rows_routed_failure, rows_quarantined)`` —
            the second through seventh elements feed the local RunResult so
            its row counts match the chosen status (otherwise the
            biconditional in
            :class:`elspeth.contracts.run_result.RunResult` would crash).
        """
        outcomes = factory.query.get_all_token_outcomes_for_run(run_id)
        rows_succeeded = 0
        rows_failed = 0
        rows_quarantined = 0
        rows_coalesce_failed = 0
        rows_processed = 0
        rows_routed_success = 0
        rows_routed_failure = 0
        for outcome_record in outcomes:
            if not outcome_record.completed:
                continue
            pair = (outcome_record.outcome, outcome_record.path)
            match pair:
                case (
                    (TerminalOutcome.SUCCESS, TerminalPath.DEFAULT_FLOW)
                    | (TerminalOutcome.SUCCESS, TerminalPath.COALESCED)
                    | (TerminalOutcome.SUCCESS, TerminalPath.FILTER_DROPPED)
                    | (TerminalOutcome.SUCCESS, TerminalPath.GATE_DISCARDED)
                ):
                    rows_succeeded += 1
                    rows_processed += 1
                case (TerminalOutcome.SUCCESS, TerminalPath.GATE_ROUTED):
                    rows_routed_success += 1
                    rows_succeeded += 1
                    rows_processed += 1
                case (TerminalOutcome.FAILURE, TerminalPath.ON_ERROR_ROUTED):
                    rows_failed += 1
                    rows_routed_failure += 1
                    rows_processed += 1
                case (TerminalOutcome.FAILURE, TerminalPath.UNROUTED):
                    rows_failed += 1
                    rows_processed += 1
                case (TerminalOutcome.FAILURE, TerminalPath.QUARANTINED_AT_SOURCE):
                    rows_quarantined += 1
                    rows_failed += 1
                    rows_processed += 1
                case (TerminalOutcome.FAILURE, TerminalPath.SINK_DISCARDED):
                    rows_failed += 1
                    rows_processed += 1
                case (TerminalOutcome.TRANSIENT, TerminalPath.SINK_FALLBACK_TO_FAILSINK):
                    rows_processed += 1
                case (
                    (TerminalOutcome.TRANSIENT, TerminalPath.FORK_PARENT)
                    | (TerminalOutcome.TRANSIENT, TerminalPath.EXPAND_PARENT)
                    | (TerminalOutcome.TRANSIENT, TerminalPath.BATCH_CONSUMED)
                ):
                    # Parent-token / aggregation accounting outcomes do not
                    # contribute to the success/failure tally — the child
                    # tokens (FORKED/EXPANDED) and the batch-result token
                    # (CONSUMED_IN_BATCH) carry the row-level counters via
                    # their own terminal outcomes.
                    pass
                case _:
                    raise AssertionError(
                        f"Unhandled (outcome, path) pair in resume aggregation: {pair!r}. "
                        "Add a case here; see ADR-019 mapping table and the live accumulator."
                    )
        terminal_status = derive_terminal_run_status(
            rows_processed=rows_processed,
            rows_succeeded=rows_succeeded,
            rows_failed=rows_failed,
            rows_routed_success=rows_routed_success,
            rows_routed_failure=rows_routed_failure,
            rows_quarantined=rows_quarantined,
            rows_coalesce_failed=rows_coalesce_failed,
        )
        return (
            terminal_status,
            rows_processed,
            rows_succeeded,
            rows_failed,
            rows_routed_success,
            rows_routed_failure,
            rows_quarantined,
        )

    def _cli_completion_for(self, status: RunStatus) -> tuple[RunCompletionStatus, int]:
        """Phase 2.2 (elspeth-0de989c56d) — map terminal RunStatus to the
        CLI ``RunCompletionStatus`` + exit code.

        ``COMPLETED`` and ``EMPTY`` both map to ``COMPLETED`` / exit 0:
        a run that ingested zero rows is operationally a clean exit at the
        CLI surface (the Web layer carries the structural distinction).
        ``COMPLETED_WITH_FAILURES`` reuses the existing ``PARTIAL``
        ceremony — same exit-code-1 semantics as the post-run export-failure
        path that already emits ``PARTIAL``.
        """
        match status:
            case RunStatus.COMPLETED | RunStatus.EMPTY:
                return RunCompletionStatus.COMPLETED, 0
            case RunStatus.COMPLETED_WITH_FAILURES:
                return RunCompletionStatus.PARTIAL, 1
            case RunStatus.FAILED:
                return RunCompletionStatus.FAILED, 2
            case RunStatus.INTERRUPTED:
                return RunCompletionStatus.INTERRUPTED, 3
            case _:
                raise OrchestrationInvariantError(
                    f"Cannot map RunStatus {status!r} to RunCompletionStatus — "
                    f"this is a terminal-status-only mapping; RUNNING and other "
                    f"non-terminal values must not reach this site."
                )

    def _emit_interrupted_ceremony(
        self,
        run_id: str,
        factory: RecorderFactory,
        shutdown_exc: GracefulShutdownError,
        start_time: float,
    ) -> None:
        """Emit telemetry and EventBus events for a gracefully interrupted run.

        Shared between run() and resume() — the interrupted ceremony is identical
        in both paths: finalize as INTERRUPTED, emit RunFinished, emit RunSummary.
        """

        total_duration = time.perf_counter() - start_time
        factory.run_lifecycle.finalize_run(run_id, status=RunStatus.INTERRUPTED)

        self._emit_telemetry(
            RunFinished(
                timestamp=datetime.now(UTC),
                run_id=run_id,
                status=RunStatus.INTERRUPTED,
                row_count=shutdown_exc.rows_processed,
                duration_ms=total_duration * 1000,
            )
        )

        self._events.emit(
            RunSummary(
                run_id=run_id,
                status=RunCompletionStatus.INTERRUPTED,
                total_rows=shutdown_exc.rows_processed,
                succeeded=shutdown_exc.rows_succeeded,
                failed=shutdown_exc.rows_failed,
                quarantined=shutdown_exc.rows_quarantined,
                duration_seconds=total_duration,
                exit_code=3,
                routed_success=shutdown_exc.rows_routed_success,
                routed_failure=shutdown_exc.rows_routed_failure,
                routed_destinations=tuple(shutdown_exc.routed_destinations.items()),
            )
        )

    def _emit_failed_ceremony(
        self,
        run_id: str,
        factory: RecorderFactory,
        start_time: float,
        result: RunResult | None = None,
    ) -> None:
        """Emit telemetry and EventBus events for a failed run.

        Finalizes the run as FAILED, emits RunFinished telemetry and RunSummary
        with the best available metrics. Shared between run() (when
        run_completed=False) and resume().
        """

        failed_result = result or RunResult(
            run_id=run_id,
            status=RunStatus.FAILED,
            rows_processed=0,
            rows_succeeded=0,
            rows_failed=0,
            rows_routed_success=0,
            rows_routed_failure=0,
            rows_quarantined=0,
            rows_forked=0,
            rows_coalesced=0,
            rows_coalesce_failed=0,
            rows_expanded=0,
            rows_buffered=0,
            rows_diverted=0,
            routed_destinations={},
        )
        total_duration = time.perf_counter() - start_time
        factory.run_lifecycle.finalize_run(run_id, status=RunStatus.FAILED)

        self._emit_telemetry(
            RunFinished(
                timestamp=datetime.now(UTC),
                run_id=run_id,
                status=RunStatus.FAILED,
                row_count=failed_result.rows_processed,
                duration_ms=total_duration * 1000,
            )
        )

        self._events.emit(
            RunSummary(
                run_id=run_id,
                status=RunCompletionStatus.FAILED,
                total_rows=failed_result.rows_processed,
                succeeded=failed_result.rows_succeeded,
                failed=failed_result.rows_failed,
                quarantined=failed_result.rows_quarantined,
                duration_seconds=total_duration,
                exit_code=2,  # exit_code: 0=success, 1=partial, 2=total failure
                routed_success=failed_result.rows_routed_success,
                routed_failure=failed_result.rows_routed_failure,
                routed_destinations=tuple(failed_result.routed_destinations.items()),
            )
        )

    def _maybe_checkpoint(
        self,
        run_id: str,
        token_id: str,
        node_id: str,
        aggregation_state: AggregationCheckpointState | None = None,
        coalesce_state: CoalesceCheckpointState | None = None,
    ) -> None:
        """Create checkpoint if configured.

        Called after a token has been durably written to its terminal sink.
        The checkpoint represents a durable progress marker - recovery can
        safely skip any row whose token has a checkpoint with a sink node_id.

        IMPORTANT: Checkpoints are created AFTER sink writes, not during
        the main processing loop. This ensures the checkpoint represents
        actual durable output, not just processing completion.

        Args:
            run_id: Current run ID
            token_id: Token that was just written to sink
            node_id: Sink node that received the token
            aggregation_state: Typed aggregation checkpoint state for crash recovery
            coalesce_state: Typed pending coalesce state for crash recovery
        """
        if not self._checkpoint_config or not self._checkpoint_config.enabled:
            return
        if self._checkpoint_manager is None:
            return
        if self._current_graph is None:
            # Should never happen - graph is set during execution
            raise OrchestrationInvariantError("Cannot create checkpoint: execution graph not available")

        self._sequence_number += 1

        # RuntimeCheckpointConfig.frequency is an int:
        # - 1 = every_row
        # - 0 = aggregation_only
        # - N = every N rows
        frequency = self._checkpoint_config.frequency
        should_checkpoint = False
        if frequency == 0:
            # aggregation_only: checkpoint unconditionally. In the post-sink
            # architecture (elspeth-rapid-xtmo), _maybe_checkpoint is only
            # called from checkpoint_after_sink — i.e., after sink durability.
            # Aggregation already reduces cardinality (many rows → fewer
            # aggregated results), so the I/O reduction is inherent.
            should_checkpoint = True
        elif frequency == 1:
            should_checkpoint = True  # every_row
        elif frequency > 1:
            should_checkpoint = (self._sequence_number % frequency) == 0  # every_n

        if should_checkpoint:
            self._checkpoint_manager.create_checkpoint(
                run_id=run_id,
                token_id=token_id,
                node_id=node_id,
                sequence_number=self._sequence_number,
                graph=self._current_graph,
                aggregation_state=aggregation_state,
                coalesce_state=coalesce_state,
            )

    def _make_checkpoint_after_sink_factory(
        self,
        run_id: str,
        processor: RowProcessor,
    ) -> _CheckpointFactory:
        """Create a per-sink checkpoint callback factory.

        Returns a factory that, given a sink_node_id, produces a callback
        invoked after each token is durably written to that sink.  Used by
        both the normal execution path and the resume path.
        """

        def factory(sink_node_id: str) -> Callable[[TokenInfo], None]:
            def callback(token: TokenInfo) -> None:
                agg_state = processor.get_aggregation_checkpoint_state()
                coalesce_state = processor.get_coalesce_checkpoint_state()
                self._maybe_checkpoint(
                    run_id=run_id,
                    token_id=token.token_id,
                    node_id=sink_node_id,
                    aggregation_state=agg_state,
                    coalesce_state=coalesce_state if coalesce_state is not None and coalesce_state.has_resumable_state else None,
                )

            return callback

        return factory

    def _checkpoint_interrupted_progress(
        self,
        run_id: str,
        loop_ctx: LoopContext,
        sink_id_map: Mapping[SinkName, NodeID],
        source_id: NodeID,
    ) -> None:
        """Persist a resumable checkpoint for graceful shutdown.

        Shutdown is an explicit operator action, so it creates a recovery
        checkpoint even if normal checkpoint frequency would skip this row.
        This preserves resumability for runs that stop before any sink-token
        checkpoint has been emitted, especially buffered aggregation/coalesce
        pipelines that intentionally skip end-of-source flushes on shutdown.
        """
        if not self._checkpoint_config or not self._checkpoint_config.enabled:
            return
        if self._checkpoint_manager is None:
            return
        if self._current_graph is None:
            raise OrchestrationInvariantError("Cannot create shutdown checkpoint: execution graph not available")

        aggregation_state = loop_ctx.processor.get_aggregation_checkpoint_state()
        raw_coalesce = loop_ctx.processor.get_coalesce_checkpoint_state()
        # Persist coalesce state when it has pending barriers or completed keys
        # needed for late-arrival detection on resume
        coalesce_state = raw_coalesce if raw_coalesce is not None and raw_coalesce.has_resumable_state else None

        token_id: str | None = None
        node_id: str | None = None
        checkpoint_agg_state: AggregationCheckpointState | None = None

        if aggregation_state.nodes:
            agg_node_id, agg_node_state = next(iter(aggregation_state.nodes.items()))
            token_id = agg_node_state.tokens[-1].token_id
            node_id = agg_node_id
            checkpoint_agg_state = aggregation_state
        elif coalesce_state is not None and coalesce_state.pending:
            pending_entry = coalesce_state.pending[-1]
            node_id = str(loop_ctx.coalesce_node_map[CoalesceName(pending_entry.coalesce_name)])
            if pending_entry.branches:
                last_branch = list(pending_entry.branches.values())[-1]
                token_id = last_branch.token_id
        else:
            for sink_name, token_outcome_pairs in loop_ctx.pending_tokens.items():
                if not token_outcome_pairs:
                    continue
                token_id = token_outcome_pairs[-1][0].token_id
                node_id = str(sink_id_map[SinkName(sink_name)])
                break

        if token_id is None and loop_ctx.last_token_id is not None:
            token_id = loop_ctx.last_token_id
            if node_id is None:
                node_id = str(source_id)

        if token_id is None or node_id is None:
            slog.warning(
                "shutdown_checkpoint_skipped",
                run_id=run_id,
                reason="no_token_or_node_id_available",
                has_aggregation_nodes=bool(aggregation_state.nodes),
                has_coalesce_pending=coalesce_state is not None,
                has_pending_sink_tokens=any(bool(pairs) for pairs in loop_ctx.pending_tokens.values()),
                last_token_id=loop_ctx.last_token_id,
                resolved_token_id=token_id,
                resolved_node_id=node_id,
            )
            return

        self._sequence_number += 1
        self._checkpoint_manager.create_checkpoint(
            run_id=run_id,
            token_id=token_id,
            node_id=node_id,
            sequence_number=self._sequence_number,
            graph=self._current_graph,
            aggregation_state=checkpoint_agg_state,
            coalesce_state=coalesce_state,
        )

    def _delete_checkpoints(self, run_id: str) -> None:
        """Delete all checkpoints for a run after successful completion.

        Args:
            run_id: Run to clean up checkpoints for
        """
        if self._checkpoint_manager is not None:
            self._checkpoint_manager.delete_checkpoints(run_id)

    def _write_pending_to_sinks(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
        ctx: PluginContext,
        counters: ExecutionCounters,
        pending_tokens: PendingTokenMap,
        sink_id_map: dict[SinkName, NodeID],
        edge_map: Mapping[tuple[NodeID, str], str],
        sink_step: int,
        *,
        on_token_written_factory: Callable[[str], Callable[[TokenInfo], None]] | None = None,
    ) -> DiversionCounts:
        """Write pending tokens to sinks using SinkExecutor.

        Extracted from _execute_run() and _process_resumed_rows() to eliminate
        duplication of the sink write orchestration pattern.

        Args:
            factory: RecorderFactory for audit trail
            run_id: Current run ID
            config: Pipeline configuration
            ctx: Plugin context
            pending_tokens: Dict of sink_name -> list of (token, pending_outcome) pairs
            sink_id_map: Maps SinkName -> NodeID for checkpoint callbacks
            sink_step: Audit step index for sink writes (from processor.resolve_sink_step())
            on_token_written_factory: Optional factory that creates per-sink checkpoint
                callbacks. Takes sink_node_id, returns callback(TokenInfo) -> None.
                When None (resume path), no checkpoint callbacks are used.
        """
        from itertools import groupby

        from elspeth.engine.executors.sink import DiversionCounts, SinkExecutor

        sink_executor = SinkExecutor(factory.execution, factory.data_flow, self._span_factory, run_id)
        step = sink_step
        total_diversions = DiversionCounts()

        for sink_name, token_outcome_pairs in pending_tokens.items():
            if not token_outcome_pairs:
                continue
            if sink_name not in config.sinks:
                raise OrchestrationInvariantError(
                    f"Sink '{sink_name}' in pending_tokens not found in config.sinks. "
                    f"Available: {sorted(config.sinks.keys())}. "
                    f"This indicates a token routing bug."
                )
            sink = config.sinks[sink_name]
            sink_node_id = sink_id_map[SinkName(sink_name)]

            # Resolve failsink reference (if configured and not 'discard')

            failsink: SinkProtocol | None = None
            failsink_config_name: str | None = None
            failsink_edge_id: str | None = None
            on_write_failure = sink._on_write_failure
            if on_write_failure is not None and on_write_failure != "discard":
                if on_write_failure not in config.sinks:
                    raise OrchestrationInvariantError(
                        f"Sink '{sink_name}' on_write_failure references '{on_write_failure}' "
                        f"which passed validation but is not in config.sinks at runtime. "
                        f"Available: {sorted(config.sinks.keys())}."
                    )
                failsink = config.sinks[on_write_failure]
                failsink_config_name = on_write_failure
                failsink_edge_key = (sink_node_id, "__failsink__")
                try:
                    failsink_edge_id = edge_map[failsink_edge_key]
                except KeyError as exc:
                    raise OrchestrationInvariantError(
                        f"Sink '{sink_name}' on_write_failure='{on_write_failure}' "
                        f"but no __failsink__ DIVERT edge exists in DAG for node '{sink_node_id}'. "
                        f"This is a DAG construction bug — on_write_failure should have "
                        f"created a DIVERT edge in from_plugin_instances()."
                    ) from exc

            # Group tokens by pending_outcome for separate write() calls
            # (sink_executor.write() takes a single PendingOutcome for all tokens in a batch)
            # PendingOutcome carries error_hash for QUARANTINED tokens
            def pending_sort_key(pair: tuple[TokenInfo, PendingOutcome | None]) -> tuple[bool, str, str, str]:
                pending = pair[1]
                if pending is None:
                    return (True, "", "", "")  # None sorts first
                outcome_value = pending.outcome.value if pending.outcome is not None else ""
                return (False, outcome_value, pending.path.value, pending.error_hash or "")

            sorted_pairs = sorted(token_outcome_pairs, key=pending_sort_key)

            # Build on_token_written callback (or None for resume)
            on_token_written: Callable[[TokenInfo], None] | None = None
            if on_token_written_factory is not None:
                on_token_written = on_token_written_factory(sink_node_id)

            for _group_key, group in groupby(sorted_pairs, key=pending_sort_key):
                group_pairs = list(group)
                pending_outcome = group_pairs[0][1]
                group_tokens = [token for token, _pending in group_pairs]
                _, diversion_counts = sink_executor.write(
                    sink=sink,
                    tokens=group_tokens,
                    ctx=ctx,
                    step_in_pipeline=step,
                    sink_name=sink_name,
                    pending_outcome=pending_outcome,
                    failsink=failsink,
                    failsink_name=failsink_config_name,
                    failsink_edge_id=failsink_edge_id,
                    on_token_written=on_token_written,
                )
                reconcile_sink_write_diversions(
                    counters=counters,
                    sink_name=sink_name,
                    pending_outcome=pending_outcome,
                    diversion_count=diversion_counts.total,
                )
                total_diversions = DiversionCounts(
                    failsink_mode=total_diversions.failsink_mode + diversion_counts.failsink_mode,
                    discard_mode=total_diversions.discard_mode + diversion_counts.discard_mode,
                )

        return total_diversions

    def _cleanup_plugins(
        self,
        config: PipelineConfig,
        ctx: PluginContext,
        *,
        include_source: bool = True,
    ) -> None:
        """Clean up all plugins in the finally block.

        Implements the lifecycle teardown contract:
        1. on_complete(ctx) on all plugins (transforms, sinks, optionally source)
        2. close() on all plugins (source, transforms, sinks)

        on_complete() is called even on pipeline error -- it signals "processing
        is done" (success or failure), not "processing succeeded". close() is
        pure resource teardown and always follows on_complete().

        Each call is individually try/excepted so one plugin's failure does not
        prevent other plugins from cleaning up. All errors are collected and
        raised together after all cleanup completes.

        Extracted from _execute_run() and _process_resumed_rows() to eliminate
        duplication of the finally-block cleanup pattern.

        Args:
            config: Pipeline configuration
            ctx: Plugin context
            include_source: If True (default), calls on_complete() and close()
                on the source. Set to False for resume path where source wasn't opened.

        Raises:
            RuntimeError: If any plugin cleanup hook fails. Chained from the
                pending exception if one exists.
        """
        import sys

        logger = slog
        pending_exc = sys.exc_info()[1]
        cleanup_errors: list[str] = []

        def record_cleanup_error(hook: str, plugin_name: str, error: Exception) -> None:
            # FrameworkBugError and AuditIntegrityError indicate system-level
            # corruption or bugs — Tier 1 violations that must crash immediately.
            # These must NOT be downgraded to cleanup warnings.
            if isinstance(error, contract_errors.TIER_1_ERRORS):
                raise

            logger.warning(
                "Plugin cleanup hook failed",
                hook=hook,
                plugin=plugin_name,
                error=str(error),
                error_type=type(error).__name__,
                exc_info=error,
            )
            cleanup_errors.append(f"{hook}({plugin_name}): {type(error).__name__}: {error}")

        def run_hook(hook_label: str, plugin_name: str, fn: Callable[[], None]) -> None:
            # Single broad-catch surface for plugin cleanup. Per the policy
            # documented above, plugin cleanup MUST attempt every hook even when
            # one fails — broad catch is required by contract. Tier-1 errors are
            # re-raised inside record_cleanup_error; everything else is collected
            # and folded into the RuntimeError raised after all hooks finish.
            try:
                fn()
            except Exception as exc:
                record_cleanup_error(hook_label, plugin_name, exc)

        # Call on_complete for all plugins (even on error).
        # Base classes provide no-op implementations, so no hasattr needed.
        # functools.partial preserves the bound-method type for mypy and avoids
        # the loop-variable closure trap that lambdas would otherwise need
        # default-argument workarounds for.
        from functools import partial

        for transform in config.transforms:
            run_hook("transform.on_complete", transform.name, partial(transform.on_complete, ctx))
        for sink in config.sinks.values():
            run_hook("sink.on_complete", sink.name, partial(sink.on_complete, ctx))
        if include_source:
            run_hook("source.on_complete", config.source.name, partial(config.source.on_complete, ctx))

        # Close source (if included) and all sinks
        if include_source:
            run_hook("source.close", config.source.name, config.source.close)

        # Close all transforms (release resources - file handles, connections, etc.)
        for transform in config.transforms:
            run_hook("transform.close", transform.name, transform.close)

        # Close all sinks
        for sink in config.sinks.values():
            run_hook("sink.close", sink.name, sink.close)

        if cleanup_errors:
            error_summary = "; ".join(cleanup_errors)
            if pending_exc is not None:
                raise RuntimeError(f"Plugin cleanup failed: {error_summary}") from pending_exc
            raise RuntimeError(f"Plugin cleanup failed: {error_summary}")

    def _assign_plugin_node_ids(
        self,
        source: SourceProtocol,
        transforms: Sequence[RowPlugin],
        sinks: Mapping[str, SinkProtocol],
        source_id: NodeID,
        transform_id_map: Mapping[int, NodeID],
        sink_id_map: Mapping[SinkName, NodeID],
    ) -> None:
        """Explicitly assign node_id to all plugins with validation.

        This is part of the plugin protocol contract - all plugins define
        node_id: str | None and the orchestrator populates it.

        Args:
            source: Source plugin instance
            transforms: List of transform plugins
            sinks: Dict of sink_name -> sink plugin
            source_id: Node ID for source
            transform_id_map: Maps transform sequence -> node_id
            sink_id_map: Maps sink_name -> node_id

        Raises:
            ValueError: If transform/sink not in ID map
        """
        # Set node_id on source
        source.node_id = source_id

        # Set node_id on transforms
        # Note: Aggregation transforms already have node_id set by CLI (mapped from
        # aggregation_id_map), so only assign for transforms without node_id.
        for seq, transform in enumerate(transforms):
            if transform.node_id is not None:
                # Already has node_id (e.g., aggregation transform) - skip
                continue
            if seq not in transform_id_map:
                raise OrchestrationInvariantError(
                    f"Transform at sequence {seq} not found in graph. Graph has mappings for sequences: {list(transform_id_map.keys())}"
                )
            transform.node_id = transform_id_map[seq]

        # Set node_id on sinks
        # Note: Sinks not in graph are skipped (e.g., export sinks used post-run)
        for sink_name, sink in sinks.items():
            sink_name_typed = SinkName(sink_name)
            if sink_name_typed not in sink_id_map:
                # Sink not in execution graph - skip silently
                # This happens for post-run sinks (e.g., landscape.export.sink)
                continue
            sink.node_id = sink_id_map[sink_name_typed]

    def _build_dag_traversal_context(
        self,
        graph: ExecutionGraph,
        config: PipelineConfig,
        config_gate_id_map: dict[GateName, NodeID],
    ) -> DAGTraversalContext:
        """Build traversal context for RowProcessor from graph + pipeline config."""
        node_step_map = graph.build_step_map()
        node_to_plugin: dict[NodeID, RowPlugin | GateSettings] = {}

        for transform in config.transforms:
            node_id_raw = transform.node_id
            if node_id_raw is None:
                raise OrchestrationInvariantError(f"Transform '{transform.name}' missing node_id for traversal context")
            node_to_plugin[NodeID(node_id_raw)] = transform

        for gate in config.gates:
            gate_node_id = config_gate_id_map[GateName(gate.name)]
            node_to_plugin[gate_node_id] = gate

        node_to_next: dict[NodeID, NodeID | None] = {}
        source_id = graph.get_source()
        node_to_next[source_id] = graph.get_next_node(source_id)
        for node_id in graph.get_pipeline_node_sequence():
            node_to_next[node_id] = graph.get_next_node(node_id)
        for coalesce_node_id in graph.get_coalesce_id_map().values():
            node_to_next[coalesce_node_id] = graph.get_next_node(coalesce_node_id)

        return DAGTraversalContext(
            node_step_map=node_step_map,
            node_to_plugin=node_to_plugin,
            first_transform_node_id=graph.get_first_transform_node(),
            node_to_next=node_to_next,
            coalesce_node_map=graph.get_coalesce_id_map(),
            branch_first_node=graph.get_branch_first_nodes(),
        )

    def _build_processor(
        self,
        *,
        graph: ExecutionGraph,
        config: PipelineConfig,
        settings: ElspethSettings | None,
        factory: RecorderFactory,
        run_id: str,
        source_id: NodeID,
        edge_map: dict[tuple[NodeID, str], str],
        route_resolution_map: dict[tuple[NodeID, str], RouteDestination] | None,
        config_gate_id_map: dict[GateName, NodeID],
        coalesce_id_map: dict[CoalesceName, NodeID],
        payload_store: PayloadStore,
        restored_aggregation_state: Mapping[NodeID, AggregationCheckpointState] | None = None,
        restored_coalesce_state: CoalesceCheckpointState | None = None,
    ) -> tuple[RowProcessor, dict[CoalesceName, NodeID], CoalesceExecutor | None]:
        """Build a RowProcessor with all supporting infrastructure.

        Constructs the retry manager, coalesce executor, traversal context,
        and coalesce routing maps, then assembles a RowProcessor. Used by
        both the main run path and the resume path.

        Returns:
            Tuple of (processor, coalesce_node_map, coalesce_executor).
        """
        from elspeth.engine.coalesce_executor import CoalesceExecutor
        from elspeth.engine.tokens import TokenManager

        retry_manager: RetryManager | None = None
        if settings is not None:
            retry_manager = RetryManager(RuntimeRetryConfig.from_settings(settings.retry))

        # Derive coalesce routing from graph topology unconditionally.
        # If the graph has coalesce nodes, the processor needs branch_to_coalesce
        # regardless of whether settings is available.
        branch_to_coalesce: dict[BranchName, CoalesceName] = graph.get_branch_to_coalesce_map()
        coalesce_node_map: dict[CoalesceName, NodeID] = graph.get_coalesce_id_map()

        # Build traversal context BEFORE CoalesceExecutor/TokenManager so that
        # node_step_map is available for the step_resolver closure they require.
        traversal = self._build_dag_traversal_context(graph, config, config_gate_id_map)

        # Build step_resolver from shared factory (single source of truth).
        # Same factory is used by RowProcessor internally for its executors.
        step_resolver = make_step_resolver(traversal.node_step_map, source_id)

        coalesce_executor: CoalesceExecutor | None = None

        if coalesce_node_map:
            # Graph has coalesce nodes — settings.coalesce is required for
            # CoalesceExecutor registration (merge policy, timeout, etc.)
            if settings is None or not settings.coalesce:
                raise OrchestrationInvariantError(
                    "Graph contains coalesce nodes but settings.coalesce is missing. "
                    "Coalesce settings are required when the pipeline has fork/join patterns."
                )

            # payload_store intentionally omitted: CoalesceExecutor's TokenManager only
            # calls coalesce_tokens(), which does not persist payloads (payloads are
            # recorded by the RowProcessor's TokenManager during initial token creation).
            token_manager = TokenManager(factory.data_flow, step_resolver=step_resolver)
            coalesce_executor = CoalesceExecutor(
                execution=factory.execution,
                span_factory=self._span_factory,
                token_manager=token_manager,
                run_id=run_id,
                step_resolver=step_resolver,
                clock=self._clock,
                max_completed_keys=self._coalesce_completed_keys_limit,
                data_flow=factory.data_flow,
            )

            for coalesce_settings_entry in settings.coalesce:
                coalesce_node_id = coalesce_id_map[CoalesceName(coalesce_settings_entry.name)]
                # Extract guaranteed fields from branch schemas for lost-branch audit trail.
                # Returns dict[branch_name, SchemaConfig]; we extract guaranteed fields.
                branch_schema_configs = graph.get_coalesce_branch_schemas(CoalesceName(coalesce_settings_entry.name))
                branch_schemas: dict[str, tuple[str, ...]] | None = None
                if branch_schema_configs:
                    branch_schemas = {
                        branch_name: tuple(sorted(schema.get_effective_guaranteed_fields()))
                        for branch_name, schema in branch_schema_configs.items()
                    }

                # Retrieve pre-computed output schema from DAG builder (P2 fix).
                # This ensures runtime contracts match build-time schema computation,
                # preserving nullable semantics from the P1 fix.
                coalesce_node_info = graph.get_node_info(coalesce_node_id)
                if coalesce_node_info.output_schema_config is None:
                    raise FrameworkBugError(
                        f"Coalesce node '{coalesce_node_id}' has no output_schema_config. "
                        f"The DAG builder must populate output_schema_config for all coalesce "
                        f"nodes via _assign_schema(). This indicates a builder bug."
                    )
                output_schema = create_contract_from_config(coalesce_node_info.output_schema_config)

                coalesce_executor.register_coalesce(
                    coalesce_settings_entry,
                    coalesce_node_id,
                    branch_schemas=branch_schemas,
                    output_schema=output_schema,
                )
            if restored_coalesce_state is not None:
                coalesce_executor.restore_from_checkpoint(restored_coalesce_state)

        # Derive coalesce on_success from graph's terminal sink map (graph-authoritative),
        # falling back to settings for non-terminal coalesce nodes.
        terminal_sink_map = graph.get_terminal_sink_map()
        coalesce_on_success_map: dict[CoalesceName, str] = {}
        for cname, cnode_id in coalesce_node_map.items():
            if cnode_id in terminal_sink_map:
                coalesce_on_success_map[cname] = terminal_sink_map[cnode_id]
            elif settings is not None and settings.coalesce:
                for coalesce_settings_entry in settings.coalesce:
                    if CoalesceName(coalesce_settings_entry.name) == cname and coalesce_settings_entry.on_success is not None:
                        coalesce_on_success_map[cname] = coalesce_settings_entry.on_success

        branch_to_sink = graph.get_branch_to_sink_map()
        typed_aggregation_settings: dict[NodeID, AggregationSettings] = {NodeID(k): v for k, v in config.aggregation_settings.items()}

        processor = RowProcessor(
            execution=factory.execution,
            data_flow=factory.data_flow,
            span_factory=self._span_factory,
            run_id=run_id,
            source_node_id=source_id,
            source_on_success=config.source.on_success,
            source_plugin=config.source,
            edge_map=edge_map,
            route_resolution_map=route_resolution_map,
            traversal=traversal,
            aggregation_settings=typed_aggregation_settings,
            retry_manager=retry_manager,
            coalesce_executor=coalesce_executor,
            branch_to_coalesce=branch_to_coalesce,
            branch_to_sink=branch_to_sink,
            sink_names=frozenset(config.sinks),
            coalesce_on_success_map=coalesce_on_success_map,
            restored_aggregation_state=restored_aggregation_state,
            payload_store=payload_store,
            clock=self._clock,
            max_workers=self._concurrency_config.max_workers if self._concurrency_config else None,
            telemetry_manager=self._telemetry,
        )

        return processor, coalesce_node_map, coalesce_executor

    @contextmanager
    def _shutdown_handler_context(self) -> Iterator[threading.Event]:
        """Install SIGINT/SIGTERM handlers that set a shutdown event.

        On first signal: sets the event, restores default SIGINT handler
        (so second Ctrl-C force-kills via KeyboardInterrupt).

        When called from a non-main thread (e.g., programmatic/embedded usage),
        signal registration is skipped — Python raises ValueError if
        signal.signal() is called outside the main thread.  The returned
        Event still works; it just won't be triggered by OS signals.

        Yields the Event for the processing loop to check.
        Restores original handlers in finally block (main thread only).
        """
        shutdown_event = threading.Event()

        # signal.signal() can only be called from the main thread.
        # In embedded/programmatic usage the orchestrator may run on a
        # worker thread — fall back to a plain event without handlers.
        if threading.current_thread() is not threading.main_thread():
            yield shutdown_event
            return

        original_sigint = signal.getsignal(signal.SIGINT)
        original_sigterm = signal.getsignal(signal.SIGTERM)

        def _handler(signum: int, frame: Any) -> None:
            shutdown_event.set()
            # Restore default SIGINT so second Ctrl-C force-kills
            signal.signal(signal.SIGINT, signal.default_int_handler)

        signal.signal(signal.SIGINT, _handler)
        signal.signal(signal.SIGTERM, _handler)
        try:
            yield shutdown_event
        finally:
            signal.signal(signal.SIGINT, original_sigint)
            signal.signal(signal.SIGTERM, original_sigterm)

    def _initialize_database_phase(
        self,
        config: PipelineConfig,
        payload_store: PayloadStore,
        secret_resolutions: list[SecretResolutionInput] | None,
        *,
        run_id: str | None = None,
        initiated_by_user_id: str | None = None,
        auth_provider_type: str | None = None,
    ) -> tuple[RecorderFactory, Any]:
        """Execute the DATABASE phase: create factory, begin run, record secrets.

        Args:
            config: Pipeline configuration.
            payload_store: PayloadStore for audit compliance.
            secret_resolutions: Optional secret resolution records.
            run_id: Optional caller-supplied run ID for audit correlation.
            initiated_by_user_id: Optional authenticated web user that initiated the run.
            auth_provider_type: Optional auth provider namespace for the initiating user.

        Returns:
            Tuple of (factory, run) where run has run_id and config_hash attributes.

        Raises:
            Exception: Re-raises any database connection or initialization failure.
        """

        phase_start = time.perf_counter()
        try:
            self._events.emit(PhaseStarted(phase=PipelinePhase.DATABASE, action=PhaseAction.CONNECTING))

            # Serialize source schema for resume type restoration
            # This enables proper type coercion (datetime/Decimal) when resuming from JSON payloads
            # SourceProtocol requires output_schema - all sources have schemas (even dynamic ones)
            source_schema_json = json.dumps(config.source.output_schema.model_json_schema())

            # Get source schema contract for resume PipelineRow wrapping
            # This enables proper contract propagation when resuming from stored payloads
            source_contract = config.source.get_schema_contract()

            factory = RecorderFactory(self._db, payload_store=payload_store)
            run = factory.run_lifecycle.begin_run(
                config=config.config,
                canonical_version=self._canonical_version,
                source_schema_json=source_schema_json,
                schema_contract=source_contract,
                run_id=run_id,
                initiated_by_user_id=initiated_by_user_id,
                auth_provider_type=auth_provider_type,
            )

            # Record secret resolutions in audit trail (deferred from pre-run loading)
            # Resolutions already contain pre-computed fingerprints (no plaintext values)
            if secret_resolutions:
                factory.run_lifecycle.record_secret_resolutions(
                    run_id=run.run_id,
                    resolutions=secret_resolutions,
                )

            # Emit telemetry AFTER Landscape succeeds - Landscape is the legal record
            self._emit_telemetry(
                RunStarted(
                    timestamp=datetime.now(UTC),
                    run_id=run.run_id,
                    config_hash=run.config_hash,
                    source_plugin=config.source.name,
                )
            )

            self._events.emit(PhaseCompleted(phase=PipelinePhase.DATABASE, duration_seconds=time.perf_counter() - phase_start))
        except Exception as e:
            self._emit_phase_error(PipelinePhase.DATABASE, e)
            raise  # CRITICAL: Always re-raise - database connection failure is fatal

        return factory, run

    def _execute_export_phase(
        self,
        factory: RecorderFactory,
        run_id: str,
        settings: ElspethSettings,
        sink_factory: Callable[[str], SinkProtocol],
    ) -> None:
        """Execute the EXPORT phase: export Landscape data to configured sink.

        Args:
            factory: RecorderFactory for status tracking.
            run_id: Run identifier.
            settings: Full settings (export config accessed from settings.landscape.export).
            sink_factory: Creates a fresh sink instance by name for export.

        Raises:
            Exception: Re-raises any export failure (run is still "completed" in Landscape).
        """

        export_config = settings.landscape.export
        factory.run_lifecycle.set_export_status(
            run_id,
            status=ExportStatus.PENDING,
            export_format=export_config.format,
            export_sink=export_config.sink,
        )

        phase_start = time.perf_counter()
        try:
            self._events.emit(PhaseStarted(phase=PipelinePhase.EXPORT, action=PhaseAction.EXPORTING, target=export_config.sink))

            # Emit telemetry PhaseChanged for EXPORT
            self._emit_telemetry(
                PhaseChanged(
                    timestamp=datetime.now(UTC),
                    run_id=run_id,
                    phase=PipelinePhase.EXPORT,
                    action=PhaseAction.EXPORTING,
                )
            )

            export_landscape(self._db, run_id, settings, sink_factory)

            factory.run_lifecycle.set_export_status(run_id, status=ExportStatus.COMPLETED)
            self._events.emit(PhaseCompleted(phase=PipelinePhase.EXPORT, duration_seconds=time.perf_counter() - phase_start))
        except Exception as export_error:
            self._emit_phase_error(PipelinePhase.EXPORT, export_error, target=export_config.sink)
            with best_effort(
                "Export status FAILED recording",
                run_id=run_id,
                original_error=type(export_error).__name__,
            ):
                factory.run_lifecycle.set_export_status(
                    run_id,
                    status=ExportStatus.FAILED,
                    error=str(export_error),
                )
            # Re-raise so caller knows export failed
            # (run is still "completed" in Landscape)
            raise

    def run(
        self,
        config: PipelineConfig,
        graph: ExecutionGraph | None = None,
        settings: ElspethSettings | None = None,
        *,
        payload_store: PayloadStore,
        secret_resolutions: list[SecretResolutionInput] | None = None,
        preflight_results: PreflightResult | None = None,
        shutdown_event: threading.Event | None = None,
        sink_factory: Callable[[str], SinkProtocol] | None = None,
        run_id: str | None = None,
        initiated_by_user_id: str | None = None,
        auth_provider_type: str | None = None,
    ) -> RunResult:
        """Execute a pipeline run.

        Args:
            config: Pipeline configuration with plugins
            graph: Pre-validated execution graph (required)
            settings: Full settings (for post-run hooks like export)
            payload_store: PayloadStore for persisting source row payloads.
            secret_resolutions: Optional secret resolution records from
                load_secrets_from_config(). Recorded in audit trail after run creation.
            preflight_results: Optional pre-flight results (dependency runs and
                commencement gates) from bootstrap_and_run(). Recorded in audit
                trail after run creation.
            shutdown_event: Optional pre-created shutdown event for testing.
                Skips signal handler installation when provided.
            sink_factory: Creates a fresh sink instance by name. Required when
                landscape export is enabled (the pipeline's sinks are already
                closed by the time export runs).
            run_id: Optional caller-supplied Landscape run ID. When omitted,
                Landscape generates a run ID.
            initiated_by_user_id: Optional authenticated web user that initiated the run.
            auth_provider_type: Optional auth provider namespace for the initiating user.

        Raises:
            OrchestrationInvariantError: If graph or payload_store is not provided
        """
        if graph is None:
            raise OrchestrationInvariantError("ExecutionGraph is required. Build with ExecutionGraph.from_plugin_instances()")
        if payload_store is None:
            raise OrchestrationInvariantError("PayloadStore is required for audit compliance.")

        # ADR-010 §Decision 3: assert registry non-empty and freeze both
        # registries before any row is processed. prepare_for_run() is
        # idempotent when the registry is already frozen (short-circuits on
        # repeat calls from multi-run test suites). The non-empty assertion
        # only fires on the first call per process lifetime; subsequent calls
        # trust the earlier freeze was performed after a passing assertion.
        prepare_for_run()

        # Schema validation now happens in ExecutionGraph.validate() during graph construction
        self._reset_checkpoint_sequence()

        # DATABASE phase - create factory and begin run
        factory, run = self._initialize_database_phase(
            config,
            payload_store,
            secret_resolutions,
            run_id=run_id,
            initiated_by_user_id=initiated_by_user_id,
            auth_provider_type=auth_provider_type,
        )

        # Record pre-flight results (deferred from bootstrap_and_run)
        if preflight_results is not None:
            factory.run_lifecycle.record_preflight_results(
                run_id=run.run_id,
                preflight=preflight_results,
            )

        run_completed = False
        run_start_time = time.perf_counter()
        try:
            # When shutdown_event is provided (testing), skip signal handler
            # installation and use the caller's event directly.
            shutdown_ctx = nullcontext(shutdown_event) if shutdown_event is not None else self._shutdown_handler_context()
            with self._span_factory.run_span(run.run_id), shutdown_ctx as active_event:
                result = self._execute_run(
                    factory,
                    run.run_id,
                    config,
                    graph,
                    settings,
                    payload_store=payload_store,
                    shutdown_event=active_event,
                )

            # Phase 2.2 (elspeth-0de989c56d): pick the new four-value
            # terminal status from the row-count shape so an operator
            # reading /api/runs/{rid} can distinguish "ran cleanly" from
            # "ran but no row succeeded" without opening output files.
            terminal_status = derive_terminal_run_status(
                rows_processed=result.rows_processed,
                rows_succeeded=result.rows_succeeded,
                rows_failed=result.rows_failed,
                rows_routed_success=result.rows_routed_success,
                rows_routed_failure=result.rows_routed_failure,
                rows_quarantined=result.rows_quarantined,
                rows_coalesce_failed=result.rows_coalesce_failed,
            )

            # Complete run with reproducibility grade computation
            factory.run_lifecycle.finalize_run(run.run_id, status=terminal_status)
            result = replace(result, status=terminal_status)
            run_completed = True

            # Emit telemetry AFTER Landscape finalize succeeds
            run_duration_ms = (time.perf_counter() - run_start_time) * 1000
            self._emit_telemetry(
                RunFinished(
                    timestamp=datetime.now(UTC),
                    run_id=run.run_id,
                    status=terminal_status,
                    row_count=result.rows_processed,
                    duration_ms=run_duration_ms,
                )
            )

            # Delete checkpoints on successful completion
            # (checkpoints are for recovery, not needed after success)
            self._delete_checkpoints(run.run_id)

            # EXPORT phase - post-run landscape export (if enabled)
            if settings is not None and settings.landscape.export.enabled:
                if sink_factory is None:
                    raise ValueError(
                        "Export is enabled but no sink_factory was provided to orchestrator.run(). "
                        "The caller must supply a sink_factory so the export phase can create "
                        "a fresh sink instance (the pipeline's sinks are already closed)."
                    )
                self._execute_export_phase(factory, run.run_id, settings, sink_factory)

            # Emit RunSummary event with final metrics.  Map the new
            # terminal status onto the CLI exit-code taxonomy via
            # ``_cli_completion_for`` so the operator-facing CLI summary
            # remains coherent with /api/runs/{rid}.
            cli_status, exit_code = self._cli_completion_for(terminal_status)
            total_duration = time.perf_counter() - run_start_time
            self._events.emit(
                RunSummary(
                    run_id=run.run_id,
                    status=cli_status,
                    total_rows=result.rows_processed,
                    succeeded=result.rows_succeeded,
                    failed=result.rows_failed,
                    quarantined=result.rows_quarantined,
                    duration_seconds=total_duration,
                    exit_code=exit_code,
                    routed_success=result.rows_routed_success,
                    routed_failure=result.rows_routed_failure,
                    routed_destinations=tuple(result.routed_destinations.items()),
                )
            )

            return result

        except GracefulShutdownError as shutdown_exc:
            with best_effort("Interrupted ceremony on graceful shutdown", run_id=run.run_id):
                self._emit_interrupted_ceremony(run.run_id, factory, shutdown_exc, run_start_time)
            raise  # Propagate to CLI
        except _RunFailedWithPartialResultError as failed_exc:
            with best_effort(
                "Failed/partial-result ceremony on run failure",
                run_id=run.run_id,
                run_completed=run_completed,
            ):
                if run_completed:
                    # Export failed after successful run — emit PARTIAL status.
                    # RunFinished was already emitted before the export attempt,
                    # so only emit the EventBus RunSummary here.
                    total_duration = time.perf_counter() - run_start_time
                    self._events.emit(
                        RunSummary(
                            run_id=run.run_id,
                            status=RunCompletionStatus.PARTIAL,
                            total_rows=result.rows_processed,
                            succeeded=result.rows_succeeded,
                            failed=result.rows_failed,
                            quarantined=result.rows_quarantined,
                            duration_seconds=total_duration,
                            exit_code=1,
                            routed_success=result.rows_routed_success,
                            routed_failure=result.rows_routed_failure,
                            routed_destinations=tuple(result.routed_destinations.items()),
                        )
                    )
                else:
                    self._emit_failed_ceremony(
                        run.run_id,
                        factory,
                        run_start_time,
                        failed_exc.partial_result,
                    )
            raise failed_exc.original_error.with_traceback(failed_exc.original_traceback) from None
        except Exception:
            # Outer broad-except: any unhandled exception type is a run failure
            # requiring a RunSummary. The inner ceremony is best-effort and must
            # not mask the original; the outer catch re-raises after.
            with best_effort(
                "Generic failure ceremony on run failure",
                run_id=run.run_id,
                run_completed=run_completed,
            ):
                if run_completed:
                    # Export failed after successful run — emit PARTIAL status.
                    # RunFinished was already emitted before the export attempt,
                    # so only emit the EventBus RunSummary here.
                    total_duration = time.perf_counter() - run_start_time
                    self._events.emit(
                        RunSummary(
                            run_id=run.run_id,
                            status=RunCompletionStatus.PARTIAL,
                            total_rows=result.rows_processed,
                            succeeded=result.rows_succeeded,
                            failed=result.rows_failed,
                            quarantined=result.rows_quarantined,
                            duration_seconds=total_duration,
                            exit_code=1,
                            routed_success=result.rows_routed_success,
                            routed_failure=result.rows_routed_failure,
                            routed_destinations=tuple(result.routed_destinations.items()),
                        )
                    )
                else:
                    self._emit_failed_ceremony(run.run_id, factory, run_start_time)
            raise  # CRITICAL: Always re-raise - observability doesn't suppress errors
        finally:
            self._safe_flush_telemetry()

    def _register_nodes_with_landscape(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
        graph: ExecutionGraph,
        execution_order: list[str],
        node_to_plugin: dict[NodeID, Any],
        source_id: NodeID,
        config_gate_node_ids: set[NodeID],
        coalesce_node_ids: set[NodeID],
    ) -> None:
        """Register each node in the execution graph with Landscape.

        Iterates the topological execution order, resolves plugin metadata
        (version, determinism), schema config, and output contract for each node,
        then calls factory.data_flow.register_node().

        Args:
            factory: RecorderFactory for audit trail.
            run_id: Run identifier.
            config: Pipeline configuration (for source contract).
            graph: Execution graph (for node info lookup).
            execution_order: Topological ordering of node IDs.
            node_to_plugin: Mapping from node ID to plugin instance.
            source_id: Source node ID (for output contract).
            config_gate_node_ids: Set of config gate node IDs (structural, no plugin).
            coalesce_node_ids: Set of coalesce node IDs (structural, no plugin).
        """
        from elspeth.contracts import Determinism

        for node_id in execution_order:
            node_info = graph.get_node_info(node_id)

            # Config gates and coalesce nodes are structural (no plugin instances)
            # Aggregations have plugin instances in node_to_plugin (transforms with metadata)
            if node_id in config_gate_node_ids:
                # Config gates are deterministic (expression evaluation is deterministic)
                # Use engine version to track which version of ExpressionParser was used
                plugin_version = f"engine:{ENGINE_VERSION}"
                determinism = Determinism.DETERMINISTIC
                source_file_hash = None  # Engine-internal nodes have no source file
            elif node_id in coalesce_node_ids:
                # Coalesce nodes merge tokens from parallel paths - deterministic operation
                # Use engine version to track which version of the coalesce logic was used
                plugin_version = f"engine:{ENGINE_VERSION}"
                determinism = Determinism.DETERMINISTIC
                source_file_hash = None  # Engine-internal nodes have no source file
            else:
                # Direct access - if node_id is in execution_order (from graph.topological_order()),
                # it MUST be in node_to_plugin (built from the same graph's source, transforms, sinks).
                # A KeyError here indicates a bug in graph construction or node_to_plugin building.
                plugin = node_to_plugin[NodeID(node_id)]

                # Extract plugin metadata - all protocols define these attributes,
                # all base classes provide defaults. Direct access is safe.
                plugin_version = plugin.plugin_version
                determinism = plugin.determinism
                source_file_hash = plugin.source_file_hash

            # Schema config is always available via output_schema_config —
            # populated at construction time for all node types.
            schema_config = node_info.output_schema_config
            if schema_config is None:
                raise FrameworkBugError(
                    f"Node '{node_id}' has no output_schema_config. "
                    "All nodes in execution order must have schema config "
                    "populated by the builder."
                )

            # Get output_contract for source nodes
            # Sources have get_schema_contract() method that returns their output contract
            output_contract = None
            if node_id == source_id:
                output_contract = config.source.get_schema_contract()

            factory.data_flow.register_node(
                run_id=run_id,
                node_id=node_id,
                plugin_name=node_info.plugin_name,
                node_type=NodeType(node_info.node_type),  # Already lowercase
                plugin_version=plugin_version,
                config=node_info.config,
                determinism=determinism,
                schema_config=schema_config,
                output_contract=output_contract,
                source_file_hash=source_file_hash,
            )

    def _register_graph_nodes_and_edges(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
        graph: ExecutionGraph,
    ) -> GraphArtifacts:
        """Register all graph nodes and edges in Landscape. Returns artifacts for subsequent phases.

        Performs the GRAPH phase:
        1. Build node_to_plugin mapping from config
        2. Register each node with Landscape (metadata, determinism, schema)
        3. Register edges and build edge_map
        4. Validate route destinations, error sinks, quarantine destinations

        Args:
            factory: RecorderFactory for audit trail
            run_id: Run identifier
            config: Pipeline configuration
            graph: Execution graph

        Returns:
            GraphArtifacts with edge_map, source_id, and all ID mappings
        """

        # Get execution order from graph
        execution_order = graph.topological_order()

        # Build node_id -> plugin instance mapping for metadata extraction
        # Source: single plugin from config.source
        source_id = graph.get_source()
        transform_id_map: dict[int, NodeID] = graph.get_transform_id_map()
        sink_id_map: dict[SinkName, NodeID] = graph.get_sink_id_map()
        config_gate_id_map: dict[GateName, NodeID] = graph.get_config_gate_id_map()
        aggregation_id_map: dict[AggregationName, NodeID] = graph.get_aggregation_id_map()

        # Build node ID sets for special node types
        config_gate_node_ids: set[NodeID] = set(config_gate_id_map.values())
        aggregation_node_ids: set[NodeID] = set(aggregation_id_map.values())

        # Map plugin instances to their node IDs for metadata extraction
        # Config gates and coalesce nodes don't have plugin instances (they're structural)
        # Aggregation transforms DO have instances - they're in config.transforms with node_id set
        node_to_plugin: dict[NodeID, Any] = {source_id: config.source}
        for seq, transform in enumerate(config.transforms):
            if seq in transform_id_map:
                # Regular transform - mapped by sequence number
                node_to_plugin[transform_id_map[seq]] = transform
            elif transform.node_id is not None and NodeID(transform.node_id) in aggregation_node_ids:
                # Aggregation transform - has node_id set by CLI, not in transform_id_map
                node_to_plugin[NodeID(transform.node_id)] = transform
        for sink_name, sink in config.sinks.items():
            if SinkName(sink_name) in sink_id_map:
                node_to_plugin[sink_id_map[SinkName(sink_name)]] = sink
        coalesce_id_map: dict[CoalesceName, NodeID] = graph.get_coalesce_id_map()
        coalesce_node_ids: set[NodeID] = set(coalesce_id_map.values())

        # GRAPH phase - register nodes and edges in Landscape
        phase_start = time.perf_counter()
        try:
            self._events.emit(PhaseStarted(phase=PipelinePhase.GRAPH, action=PhaseAction.BUILDING))

            # Emit telemetry PhaseChanged - we now have run_id from begin_run
            self._emit_telemetry(
                PhaseChanged(
                    timestamp=datetime.now(UTC),
                    run_id=run_id,
                    phase=PipelinePhase.GRAPH,
                    action=PhaseAction.BUILDING,
                )
            )

            # Register nodes with Landscape using graph's node IDs and actual plugin metadata
            self._register_nodes_with_landscape(
                factory,
                run_id,
                config,
                graph,
                execution_order,
                node_to_plugin,
                source_id,
                config_gate_node_ids,
                coalesce_node_ids,
            )

            # Register edges from graph - key by (from_node, label) for lookup
            # Gates return route labels, so edge_map is keyed by label
            edge_map: dict[tuple[NodeID, str], str] = {}

            for edge_info in graph.get_edges():
                edge = factory.data_flow.register_edge(
                    run_id=run_id,
                    from_node_id=edge_info.from_node,
                    to_node_id=edge_info.to_node,
                    label=edge_info.label,
                    mode=edge_info.mode,
                )
                # Key by edge label - gates return route labels, transforms use "continue"
                edge_map[(NodeID(edge_info.from_node), edge_info.label)] = edge.edge_id

            # Get route resolution map - maps (gate_node, label) -> "continue" | sink_name
            route_resolution_map = graph.get_route_resolution_map()

            # NOTE — value-source compliance is enforced at the entry-point
            # boundary, NOT here. The walker
            # (``engine/orchestrator/preflight.validate_value_source_compliance``)
            # runs inside ``cli_helpers.instantiate_plugins_from_config`` and
            # the composer/web-execution validate paths
            # (``web/execution/validation.validate_pipeline``,
            # ``web/execution/service._run_pipeline``). Every legitimate caller
            # that builds a ``PipelineConfig`` passes through one of those
            # surfaces, so by the time we reach ``Orchestrator.run`` the bundle
            # has already been gated. If you add a new entry point that
            # constructs a ``PipelineConfig`` directly (test harness,
            # programmatic API, etc.), call ``validate_value_source_compliance``
            # at that boundary too — the orchestrator does NOT re-validate
            # value-source declarations per run, and a bypassing entry point
            # would silently skip the check otherwise.
            #
            # Validate all route destinations BEFORE processing any rows
            # This catches config errors early instead of after partial processing
            # Note: config gates also add to route_resolution_map, validated the same way
            # Call module function directly (no wrapper method)
            validate_route_destinations(
                route_resolution_map=route_resolution_map,
                available_sinks=set(config.sinks.keys()),
                transform_id_map=transform_id_map,
                transforms=config.transforms,
                config_gate_id_map=config_gate_id_map,
                config_gates=config.gates,
            )

            # Validate transform error sink destinations
            # Call module function directly (no wrapper method)
            validate_transform_error_sinks(
                transforms=config.transforms,
                available_sinks=set(config.sinks.keys()),
            )

            # Validate source quarantine destination
            # Call module function directly (no wrapper method)
            validate_source_quarantine_destination(
                source=config.source,
                available_sinks=set(config.sinks.keys()),
            )

            # Validate sink failsink destinations

            sink_validation_stubs = {name: SimpleNamespace(on_write_failure=sink._on_write_failure) for name, sink in config.sinks.items()}
            sink_plugins = {name: sink.name for name, sink in config.sinks.items()}
            validate_sink_failsink_destinations(
                sink_configs=sink_validation_stubs,
                available_sinks=set(config.sinks.keys()),
                sink_plugins=sink_plugins,
            )

            self._events.emit(PhaseCompleted(phase=PipelinePhase.GRAPH, duration_seconds=time.perf_counter() - phase_start))
        except Exception as e:
            self._emit_phase_error(PipelinePhase.GRAPH, e)
            raise  # CRITICAL: Always re-raise - graph validation failure is fatal

        return GraphArtifacts(
            edge_map=edge_map,
            source_id=source_id,
            sink_id_map=sink_id_map,
            transform_id_map=transform_id_map,
            config_gate_id_map=config_gate_id_map,
            coalesce_id_map=coalesce_id_map,
        )

    def _initialize_run_context(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
        graph: ExecutionGraph,
        settings: ElspethSettings | None,
        artifacts: GraphArtifacts,
        payload_store: PayloadStore,
        *,
        include_source_on_start: bool = True,
        restored_aggregation_state: Mapping[str, AggregationCheckpointState] | None = None,
        restored_coalesce_state: CoalesceCheckpointState | None = None,
        shutdown_event: threading.Event | None = None,
    ) -> RunContext:
        """Initialize run context: assign node IDs, create PluginContext, call on_start, build processor.

        Args:
            include_source_on_start: If True, call source.on_start(). False for resume
                (source was fully consumed in original run).
            restored_aggregation_state: Map of node_id -> state for resume path.
            restored_coalesce_state: Pending coalesce state for resume path.

        Returns:
            RunContext with ctx, processor, coalesce_executor, coalesce_node_map,
            and agg_transform_lookup.
        """
        source_id = artifacts.source_id
        sink_id_map = dict(artifacts.sink_id_map)
        transform_id_map = dict(artifacts.transform_id_map)
        config_gate_id_map = dict(artifacts.config_gate_id_map)
        coalesce_id_map = dict(artifacts.coalesce_id_map)
        edge_map = dict(artifacts.edge_map)
        route_resolution_map = graph.get_route_resolution_map()

        # Assign node_ids to all plugins
        self._assign_plugin_node_ids(
            source=config.source,
            transforms=config.transforms,
            sinks=config.sinks,
            source_id=source_id,
            transform_id_map=transform_id_map,
            sink_id_map=sink_id_map,
        )

        # Create context with the PluginAuditWriter
        ctx = PluginContext(
            run_id=run_id,
            config=config.config,
            landscape=factory.plugin_audit_writer(),
            payload_store=factory.payload_store,
            rate_limit_registry=self._rate_limit_registry,
            concurrency_config=self._concurrency_config,
            telemetry_emit=self._emit_telemetry,
            shutdown_event=shutdown_event,
        )

        # Set node_id on context for source validation error attribution
        # This must be set BEFORE source.load() so that any validation errors
        # (e.g., malformed CSV rows) can be attributed to the source node
        ctx.node_id = source_id

        try:
            if include_source_on_start:
                config.source.on_start(ctx)
            for transform in config.transforms:
                transform.on_start(ctx)
            for sink in config.sinks.values():
                sink.on_start(ctx)

            processor, coalesce_node_map, coalesce_executor = self._build_processor(
                graph=graph,
                config=config,
                settings=settings,
                factory=factory,
                run_id=run_id,
                source_id=source_id,
                edge_map=edge_map,
                route_resolution_map=route_resolution_map,
                config_gate_id_map=config_gate_id_map,
                coalesce_id_map=coalesce_id_map,
                payload_store=payload_store,
                restored_aggregation_state={NodeID(k): v for k, v in restored_aggregation_state.items()}
                if restored_aggregation_state
                else None,
                restored_coalesce_state=restored_coalesce_state,
            )
        except Exception:
            self._cleanup_plugins(config, ctx, include_source=include_source_on_start)
            raise

        # Pre-compute aggregation transform lookup for O(1) access per timeout check
        agg_transform_lookup: dict[str, AggNodeEntry] = {}
        if config.aggregation_settings:
            for t in config.transforms:
                if (
                    isinstance(t, TransformProtocol)
                    and t.is_batch_aware
                    and t.node_id is not None
                    and t.node_id in config.aggregation_settings
                ):
                    agg_transform_lookup[t.node_id] = AggNodeEntry(transform=t, node_id=NodeID(t.node_id))

        return RunContext(
            ctx=ctx,
            processor=processor,
            coalesce_executor=coalesce_executor,
            coalesce_node_map=coalesce_node_map,
            agg_transform_lookup=agg_transform_lookup,
        )

    def _run_transform_runtime_preflights(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
        ctx: PluginContext,
    ) -> None:
        """Run transform-declared external readiness checks before source load."""
        for transform in config.transforms:
            if not transform.requires_runtime_preflight:
                continue
            if transform.node_id is None:
                raise OrchestrationInvariantError(
                    f"Transform {transform.name!r} requires runtime preflight before its node_id was assigned"
                )

            previous_node_id = ctx.node_id
            ctx.node_id = transform.node_id
            try:
                with track_operation(
                    recorder=factory.execution,
                    run_id=run_id,
                    node_id=transform.node_id,
                    operation_type="runtime_preflight",
                    ctx=ctx,
                    input_data={"transform_plugin": transform.name},
                ):
                    transform.runtime_preflight(ctx)
            finally:
                ctx.node_id = previous_node_id

    def _setup_resume_context(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
        graph: ExecutionGraph,
    ) -> GraphArtifacts:
        """Resume-path equivalent of _register_graph_nodes_and_edges().

        Loads node ID maps and edge_map from database records instead of
        registering new ones. The graph is the same as the original run,
        but nodes/edges already exist in Landscape.

        Returns:
            GraphArtifacts populated from existing Landscape records.
        """
        # Get explicit node ID mappings from graph
        source_id = graph.get_source()
        sink_id_map = graph.get_sink_id_map()
        transform_id_map = graph.get_transform_id_map()
        config_gate_id_map = graph.get_config_gate_id_map()
        coalesce_id_map = graph.get_coalesce_id_map()

        # Build edge_map from database (load real edge IDs registered in original run)
        # CRITICAL: Must use real edge_ids for FK integrity when recording routing events
        # Convert keys from (str, str) to (NodeID, str) to match RowProcessor's type
        raw_edge_map = factory.data_flow.get_edge_map(run_id)
        edge_map: dict[tuple[NodeID, str], str] = {(NodeID(k[0]), k[1]): v for k, v in raw_edge_map.items()}

        # Get route resolution map for validation
        route_resolution_map = graph.get_route_resolution_map()

        # Validate route destinations (config may have changed since original run)
        # This catches config errors early instead of after partial processing
        # Call module function directly (no wrapper method)
        validate_route_destinations(
            route_resolution_map=route_resolution_map,
            available_sinks=set(config.sinks.keys()),
            transform_id_map=transform_id_map,
            transforms=config.transforms,
            config_gate_id_map=config_gate_id_map,
            config_gates=config.gates,
        )

        # Validate transform error sink destinations
        # Call module function directly (no wrapper method)
        validate_transform_error_sinks(
            transforms=config.transforms,
            available_sinks=set(config.sinks.keys()),
        )

        # Validate source quarantine destination
        # Call module function directly (no wrapper method)
        validate_source_quarantine_destination(
            source=config.source,
            available_sinks=set(config.sinks.keys()),
        )

        # Validate sink failsink destinations
        sink_validation_stubs = {name: SimpleNamespace(on_write_failure=sink._on_write_failure) for name, sink in config.sinks.items()}
        sink_plugins = {name: sink.name for name, sink in config.sinks.items()}
        validate_sink_failsink_destinations(
            sink_configs=sink_validation_stubs,
            available_sinks=set(config.sinks.keys()),
            sink_plugins=sink_plugins,
        )

        return GraphArtifacts(
            edge_map=edge_map,
            source_id=source_id,
            sink_id_map=sink_id_map,
            transform_id_map=transform_id_map,
            config_gate_id_map=config_gate_id_map,
            coalesce_id_map=coalesce_id_map,
        )

    def _flush_and_write_sinks(
        self,
        factory: RecorderFactory,
        run_id: str,
        loop_ctx: LoopContext,
        sink_id_map: Mapping[SinkName, NodeID],
        edge_map: Mapping[tuple[NodeID, str], str],
        interrupted_by_shutdown: bool,
        *,
        on_token_written_factory: _CheckpointFactory | None = None,
        shutdown_checkpoint_source_id: NodeID | None = None,
    ) -> None:
        """Write all pending tokens to sinks and handle post-loop bookkeeping.

        IMPORTANT: Aggregation flush and coalesce flush are NOT in this method.
        They stay inside the processing loop because they must execute inside
        the track_operation(source_load) context to preserve audit attribution.

        Handles:
        1. Write pending tokens to sinks (each sink has its own track_operation)
        2. Raise GracefulShutdownError if interrupted
        """
        counters = loop_ctx.counters

        diversion_counts = self._write_pending_to_sinks(
            factory=factory,
            run_id=run_id,
            config=loop_ctx.config,
            ctx=loop_ctx.ctx,
            counters=loop_ctx.counters,
            pending_tokens=loop_ctx.pending_tokens,
            sink_id_map=dict(sink_id_map),
            edge_map=edge_map,
            sink_step=loop_ctx.processor.resolve_sink_step(),
            on_token_written_factory=on_token_written_factory,
        )
        # ADR-019: failsink-mode diversions are TRANSIENT structural evidence;
        # discard-mode diversions are FAILURE predicate inputs as well.
        loop_ctx.counters.rows_diverted += diversion_counts.total
        loop_ctx.counters.rows_failed += diversion_counts.discard_mode

        # If shutdown interrupted the loop, raise after all pending work is flushed.
        # At this point: sink writes are done, and any buffered aggregation/coalesce
        # state that we intentionally preserved can be checkpointed for resume.
        if interrupted_by_shutdown:
            if shutdown_checkpoint_source_id is not None:
                self._checkpoint_interrupted_progress(
                    run_id=run_id,
                    loop_ctx=loop_ctx,
                    sink_id_map=sink_id_map,
                    source_id=shutdown_checkpoint_source_id,
                )
            raise GracefulShutdownError(
                rows_processed=counters.rows_processed,
                run_id=run_id,
                rows_succeeded=counters.rows_succeeded,
                rows_failed=counters.rows_failed,
                rows_quarantined=counters.rows_quarantined,
                rows_routed_success=counters.rows_routed_success,
                rows_routed_failure=counters.rows_routed_failure,
                routed_destinations=dict(counters.routed_destinations),
            )

    def _handle_quarantine_row(
        self,
        factory: RecorderFactory,
        run_id: str,
        source_id: NodeID,
        source_item: SourceRow,
        row_index: int,
        edge_map: Mapping[tuple[NodeID, str], str],
        loop_ctx: LoopContext,
    ) -> None:
        """Handle a quarantined source row: route directly to configured sink.

        Accesses loop_ctx.processor for token creation and loop_ctx.counters
        for incrementing quarantine count. Appends to loop_ctx.pending_tokens.

        This method performs the complete quarantine workflow:
        1. Validate quarantine destination exists
        2. Sanitize data for canonical JSON
        3. Create quarantine token
        4. Record source node_state (FAILED)
        5. Record DIVERT routing_event
        6. Emit telemetry
        7. Compute error_hash
        8. Append to pending_tokens with PendingOutcome
        """

        config = loop_ctx.config
        counters = loop_ctx.counters
        processor = loop_ctx.processor
        pending_tokens = loop_ctx.pending_tokens

        # Route quarantined row to configured sink
        # Per CLAUDE.md: plugin bugs must crash, no silent drops
        quarantine_sink = source_item.quarantine_destination

        # Validate destination exists - crash on plugin bug
        if not quarantine_sink:
            raise RouteValidationError(
                f"Source '{config.source.name}' yielded quarantined row "
                f"(row_index={row_index}) with missing quarantine_destination. "
                f"This is a plugin bug: quarantined rows MUST specify a destination. "
                f"Use SourceRow.quarantined(row, error, destination) factory method."
            )
        if quarantine_sink not in config.sinks:
            raise RouteValidationError(
                f"Source '{config.source.name}' yielded quarantined row "
                f"(row_index={row_index}) with invalid quarantine_destination='{quarantine_sink}'. "
                f"No sink named '{quarantine_sink}' exists. "
                f"Available sinks: {sorted(config.sinks.keys())}. "
                f"This is a plugin bug: quarantine_destination must match "
                f"source._on_validation_failure='{config.source._on_validation_failure}'."
            )

        # Destination validated. Source quarantine is a FAILURE lifecycle with
        # a quarantine reporting subset, so bump both counters.
        counters.rows_quarantined += 1
        counters.rows_failed += 1
        validation_error_id = loop_ctx.ctx.pop_pending_quarantine_validation_error_id(source_item.row)
        # Sanitize quarantine data at Tier-3 boundary: replace non-finite
        # floats (NaN, Infinity) with None so downstream canonical JSON
        # and stable_hash operations succeed. The quarantine_error records
        # what was originally wrong with the data.
        # SourceRow is frozen — create a new instance with sanitized row data.
        source_item = replace(source_item, row=sanitize_for_canonical(source_item.row))

        # Create a token for the quarantined row using specialized method
        # (quarantine rows don't have contracts - they failed validation)
        quarantine_token = processor.token_manager.create_quarantine_token(
            run_id=run_id,
            source_node_id=source_id,
            row_index=row_index,
            source_row=source_item,
            validation_error_id=validation_error_id,
        )

        # Record source node_state (step_index=0) for quarantine audit lineage.
        # Status is FAILED because the source validation rejected this row.
        quarantine_data = source_item.row if isinstance(source_item.row, dict) else {"_raw": source_item.row}
        quarantine_error_msg = source_item.quarantine_error or "unknown_validation_error"
        source_state = factory.execution.begin_node_state(
            token_id=quarantine_token.token_id,
            node_id=source_id,
            run_id=run_id,
            step_index=0,
            input_data=quarantine_data,
            quarantined=True,
        )
        factory.execution.complete_node_state(
            state_id=source_state.state_id,
            status=NodeStateStatus.FAILED,
            duration_ms=0,
            error=ExecutionError(
                exception=quarantine_error_msg,
                exception_type="ValidationError",
            ),
        )

        # Record DIVERT routing_event for the quarantine edge.
        # The __quarantine__ edge MUST exist — DAG creates it in
        # the source quarantine edge block of from_plugin_instances().
        quarantine_edge_key = (source_id, "__quarantine__")
        try:
            quarantine_edge_id = edge_map[quarantine_edge_key]
        except KeyError as exc:
            raise OrchestrationInvariantError(
                f"Quarantine row reached orchestrator but no __quarantine__ "
                f"DIVERT edge exists in DAG for source '{source_id}'. "
                f"This is a DAG construction bug — "
                f"on_validation_failure should have created a DIVERT edge "
                f"in from_plugin_instances()."
            ) from exc
        factory.execution.record_routing_event(
            state_id=source_state.state_id,
            edge_id=quarantine_edge_id,
            mode=RoutingMode.DIVERT,
            reason=SourceQuarantineReason(
                quarantine_error=quarantine_error_msg,
            ),
        )

        # Emit RowCreated telemetry AFTER Landscape recording succeeds
        # Quarantined rows are Tier-3 data that may contain non-canonical
        # values (NaN, Infinity). Use stable_hash when possible, fall back
        # to repr_hash for non-canonical data.
        try:
            quarantine_content_hash = stable_hash(source_item.row)
        except (ValueError, TypeError) as e:
            slog.debug(
                "stable_hash_fallback_to_repr_hash",
                error_type=type(e).__name__,
                error=str(e),
            )
            quarantine_content_hash = repr_hash(source_item.row)
        self._emit_telemetry(
            RowCreated(
                timestamp=datetime.now(UTC),
                run_id=run_id,
                row_id=quarantine_token.row_id,
                token_id=quarantine_token.token_id,
                content_hash=quarantine_content_hash,
            )
        )

        # Compute error_hash for QUARANTINED outcome audit trail
        # Per CLAUDE.md: every row must reach exactly one terminal state
        # Do NOT record outcome here — record after sink durability in SinkExecutor.write()
        quarantine_error_hash = hashlib.sha256(quarantine_error_msg.encode()).hexdigest()[:16]

        # Pass PendingOutcome with error_hash - outcome recorded after sink durability
        pending_tokens[quarantine_sink].append(
            (
                quarantine_token,
                PendingOutcome(
                    outcome=TerminalOutcome.FAILURE,
                    path=TerminalPath.QUARANTINED_AT_SOURCE,
                    error_hash=quarantine_error_hash,
                ),
            )
        )

    def _record_field_resolution(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
    ) -> bool:
        """Record source field resolution mapping if available.

        Called once per run — on first iteration (after generator body executes)
        or post-loop for empty sources (header-only files where the loop never
        executes but the source computed field resolution).

        Returns:
            True if field resolution was recorded, False otherwise.
        """
        field_resolution = config.source.get_field_resolution()
        if field_resolution is None:
            return False

        resolution_mapping, normalization_version = field_resolution
        factory.run_lifecycle.record_source_field_resolution(
            run_id=run_id,
            resolution_mapping=resolution_mapping,
            normalization_version=normalization_version,
        )
        # Emit telemetry AFTER Landscape succeeds
        self._emit_telemetry(
            FieldResolutionApplied(
                timestamp=datetime.now(UTC),
                run_id=run_id,
                source_plugin=config.source.name,
                field_count=len(resolution_mapping),
                normalization_version=normalization_version,
                resolution_mapping=resolution_mapping,
            )
        )
        return True

    def _record_schema_contract(
        self,
        factory: RecorderFactory,
        run_id: str,
        source_id: NodeID,
        config: PipelineConfig,
        ctx: PluginContext,
    ) -> bool:
        """Record source schema contract if available.

        Called once per run — on the first VALID row (quarantined rows don't
        trigger contract population) or post-loop for runs with no valid rows
        (empty input or all-quarantined).

        Returns:
            True if schema contract was recorded, False otherwise.
        """
        schema_contract = config.source.get_schema_contract()
        if schema_contract is None:
            return False

        # Update run-level contract
        factory.run_lifecycle.update_run_contract(run_id, schema_contract)
        # Update source node's output_contract (was NULL at registration)
        factory.data_flow.update_node_output_contract(run_id, source_id, schema_contract)
        # Make contract available to transforms via context
        ctx.contract = schema_contract
        return True

    def _restore_source_iteration_context(
        self,
        ctx: PluginContext,
        *,
        source_id: NodeID,
        source_operation_id: str,
    ) -> None:
        """Restore source-scoped context before source generator code resumes.

        Source plugins run partly in `load(ctx)` setup and partly on each
        generator `next()` call. Transform execution mutates the shared
        PluginContext with transform-scoped node/state identity, so we must
        restore the source identity before the next generator step or any
        source-side validation/error recording will be misattributed.
        """
        ctx.node_id = source_id
        ctx.operation_id = source_operation_id

    _PROGRESS_ROW_INTERVAL = 100
    _PROGRESS_TIME_INTERVAL = 5.0  # seconds

    def _maybe_emit_progress(
        self,
        counters: ExecutionCounters,
        start_time: float,
        last_progress_time: float,
    ) -> float:
        """Emit a ProgressEvent if row count or time threshold is met.

        Hybrid timing: emit on first row, every 100 rows, or every 5 seconds.
        Used in both quarantine and valid-row paths.

        Returns:
            Updated last_progress_time (unchanged if no emission).
        """
        progress_interval = self._PROGRESS_ROW_INTERVAL
        progress_time_interval = self._PROGRESS_TIME_INTERVAL
        current_time = time.perf_counter()
        time_since_last_progress = current_time - last_progress_time
        should_emit = (
            counters.rows_processed == 1  # First row - immediate feedback
            or counters.rows_processed % progress_interval == 0  # Every N rows
            or time_since_last_progress >= progress_time_interval  # Every M seconds
        )
        if should_emit:
            elapsed = current_time - start_time
            self._events.emit(
                ProgressEvent(
                    rows_processed=counters.rows_processed,
                    # elspeth-5069612f3c — rows_routed split. Each terminal
                    # bucket is emitted on its own field so downstream
                    # consumers (web ProgressData → SSE → frontend, CLI
                    # progress formatter) can mirror the terminal-state
                    # taxonomy. The pre-split fold (rows_succeeded +=
                    # rows_routed_success) silently conflated MOVE-routed
                    # rows into rows_succeeded and dropped DIVERT entirely,
                    # leaving the in-flight signal incompatible with the
                    # terminal Pydantic schemas (CompletedData, etc.).
                    rows_succeeded=counters.rows_succeeded,
                    rows_failed=counters.rows_failed,
                    rows_quarantined=counters.rows_quarantined,
                    rows_routed_success=counters.rows_routed_success,
                    rows_routed_failure=counters.rows_routed_failure,
                    elapsed_seconds=elapsed,
                )
            )
            return current_time
        return last_progress_time

    def _finalize_source_iteration(
        self,
        loop_ctx: LoopContext,
        factory: RecorderFactory,
        run_id: str,
        source_id: NodeID,
        source_operation_id: str,
        field_resolution_recorded: bool,
        schema_contract_recorded: bool,
        *,
        interrupted_by_shutdown: bool,
    ) -> None:
        """Post-loop work after source iteration completes or is interrupted.

        Restores operation_id, optionally flushes end-of-source aggregation and
        coalesce state, and records deferred field resolution / schema contract.

        On graceful shutdown we intentionally skip end-of-source flushes. A
        shutdown stops after the current row; it must not synthesize
        END_OF_SOURCE aggregation outputs or force pending coalesces to resolve.
        """
        config = loop_ctx.config
        ctx = loop_ctx.ctx
        processor = loop_ctx.processor
        counters = loop_ctx.counters
        pending_tokens = loop_ctx.pending_tokens
        coalesce_executor = loop_ctx.coalesce_executor
        coalesce_node_map = dict(loop_ctx.coalesce_node_map)

        # CRITICAL: Restore source-scoped identity before post-loop flushes.
        # On normal loop exit, the restore at end-of-iteration ensures
        # node_id == source_id and operation_id == source_operation_id.
        # On shutdown break, that restore is SKIPPED — both fields still
        # hold transform-scoped values. Aggregation and coalesce flushes
        # can trigger transforms that make external calls — those must be
        # attributed to source_load, not orphaned or misattributed.
        # Idempotent on normal exit; essential on shutdown-break path.
        self._restore_source_iteration_context(
            ctx,
            source_id=source_id,
            source_operation_id=source_operation_id,
        )

        if not interrupted_by_shutdown:
            # CRITICAL: Flush remaining aggregation buffers only at true end-of-source.
            # A graceful shutdown is resumable and must preserve buffered state
            # instead of forcing an END_OF_SOURCE flush.
            if config.aggregation_settings:
                # NOTE: Aggregation-flushed tokens are NOT checkpointed here.
                # They go into pending_tokens and are checkpointed only after
                # SinkExecutor.write() achieves sink durability, via the
                # checkpoint_after_sink callback.
                flush_result = flush_remaining_aggregation_buffers(
                    config=config,
                    processor=processor,
                    ctx=ctx,
                    pending_tokens=pending_tokens,
                )
                counters.accumulate_flush_result(flush_result)

                # TERMINAL GUARANTEE: After end-of-source flush, all aggregation
                # buffers must be empty. Any remaining tokens would be silently
                # lost — never reaching a terminal state in the audit trail.
                for agg_node_id_str in config.aggregation_settings:
                    remaining = processor.get_aggregation_buffer_count(NodeID(agg_node_id_str))
                    if remaining > 0:
                        raise OrchestrationInvariantError(
                            f"Aggregation buffer for node '{agg_node_id_str}' still has "
                            f"{remaining} tokens after end-of-source flush. "
                            f"These tokens would never reach a terminal state."
                        )

            # Flush pending coalesce operations only when the source is actually exhausted.
            if coalesce_executor is not None:
                flush_coalesce_pending(
                    coalesce_executor=coalesce_executor,
                    coalesce_node_map=coalesce_node_map,
                    processor=processor,
                    ctx=ctx,
                    counters=counters,
                    pending_tokens=pending_tokens,
                )

        # Record field resolution for empty sources (header-only files).
        # For sources with rows, this was recorded inside the loop on first iteration.
        if not field_resolution_recorded:
            self._record_field_resolution(factory, run_id, config)

        # Record schema contract for runs with no valid source rows.
        # In-loop recording happens on first VALID row. For all-invalid
        # or empty inputs, that branch never executes.
        if not schema_contract_recorded:
            self._record_schema_contract(factory, run_id, source_id, config, ctx)

    def _load_source_with_events(
        self,
        config: PipelineConfig,
        run_id: str,
        ctx: PluginContext,
    ) -> Iterator[SourceRow]:
        """Execute SOURCE phase: emit lifecycle events, load source, handle errors.

        SOURCE phase is complete when this method returns. Errors during load()
        (file not found, auth failure) are emitted as PhaseError before re-raising.
        """

        phase_start = time.perf_counter()
        self._events.emit(PhaseStarted(phase=PipelinePhase.SOURCE, action=PhaseAction.INITIALIZING, target=config.source.name))
        self._emit_telemetry(
            PhaseChanged(
                timestamp=datetime.now(UTC),
                run_id=run_id,
                phase=PipelinePhase.SOURCE,
                action=PhaseAction.INITIALIZING,
            )
        )

        try:
            with self._span_factory.source_span(config.source.name):
                source_iterator = iter(config.source.load(ctx))
                try:
                    first_row = next(source_iterator)
                except StopIteration:
                    self._events.emit(PhaseCompleted(phase=PipelinePhase.SOURCE, duration_seconds=time.perf_counter() - phase_start))
                    return iter(())
        except Exception as e:
            self._emit_phase_error(PipelinePhase.SOURCE, e, target=config.source.name)
            raise

        self._events.emit(PhaseCompleted(phase=PipelinePhase.SOURCE, duration_seconds=time.perf_counter() - phase_start))
        return chain((first_row,), source_iterator)

    def _run_main_processing_loop(
        self,
        loop_ctx: LoopContext,
        factory: RecorderFactory,
        run_id: str,
        source_id: NodeID,
        edge_map: Mapping[tuple[NodeID, str], str],
        *,
        shutdown_event: threading.Event | None = None,
    ) -> LoopResult:
        """Run the main processing loop: source iteration, quarantine, transform, flush.

        Owns the track_operation(source_load) context — everything inside executes
        within source_load operation attribution. Sink writes happen OUTSIDE this
        method in _flush_and_write_sinks() (separate track_operation per sink).

        Final progress emission and PhaseCompleted(PROCESS) are emitted by the
        caller AFTER sink writes, using the timing state in LoopResult.
        """

        # Destructure loop_ctx for local access
        config = loop_ctx.config
        ctx = loop_ctx.ctx
        processor = loop_ctx.processor
        counters = loop_ctx.counters
        pending_tokens = loop_ctx.pending_tokens
        coalesce_executor = loop_ctx.coalesce_executor
        coalesce_node_map = dict(loop_ctx.coalesce_node_map)
        agg_transform_lookup = dict(loop_ctx.agg_transform_lookup)

        start_time = time.perf_counter()
        last_progress_time = start_time

        # source_load operation covers the entire source consumption lifecycle
        with track_operation(
            recorder=factory.execution,
            run_id=run_id,
            node_id=source_id,
            operation_type="source_load",
            ctx=ctx,
            input_data={"source_plugin": config.source.name},
        ) as source_op_handle:
            # Generator-based sources execute on next() — restore operation_id
            # before each iteration so external calls are attributed to source_load
            source_operation_id = source_op_handle.operation.operation_id

            source_iterator = self._load_source_with_events(config, run_id, ctx)
            self._restore_source_iteration_context(
                ctx,
                source_id=source_id,
                source_operation_id=source_operation_id,
            )

            # Deferred recording flags — field resolution after first iteration,
            # schema contract after first VALID row. If begin_run already stored
            # a contract (FIXED mode), skip re-recording.
            field_resolution_recorded = False
            schema_contract_recorded = factory.run_lifecycle.get_run_contract(run_id) is not None

            # PROCESS phase
            phase_start = time.perf_counter()
            self._events.emit(PhaseStarted(phase=PipelinePhase.PROCESS, action=PhaseAction.PROCESSING))
            self._emit_telemetry(
                PhaseChanged(
                    timestamp=datetime.now(UTC),
                    run_id=run_id,
                    phase=PipelinePhase.PROCESS,
                    action=PhaseAction.PROCESSING,
                )
            )

            interrupted_by_shutdown = False
            try:
                for row_index, source_item in enumerate(source_iterator):
                    counters.rows_processed += 1

                    # Record field resolution on first iteration (generators execute body on first next())
                    if not field_resolution_recorded:
                        field_resolution_recorded = True
                        self._record_field_resolution(factory, run_id, config)

                    # Quarantine path — route directly to sink, skip normal processing
                    if source_item.is_quarantined:
                        self._handle_quarantine_row(
                            factory,
                            run_id,
                            source_id,
                            source_item,
                            row_index,
                            edge_map,
                            loop_ctx,
                        )
                        quarantine_sink = source_item.quarantine_destination
                        if quarantine_sink is not None and loop_ctx.pending_tokens[quarantine_sink]:
                            loop_ctx.last_token_id = loop_ctx.pending_tokens[quarantine_sink][-1][0].token_id
                        last_progress_time = self._maybe_emit_progress(
                            counters,
                            start_time,
                            last_progress_time,
                        )
                        self._restore_source_iteration_context(
                            ctx,
                            source_id=source_id,
                            source_operation_id=source_operation_id,
                        )
                        if shutdown_event is not None and shutdown_event.is_set():
                            interrupted_by_shutdown = True
                            break
                        continue

                    # Record schema contract on first VALID row (quarantined rows don't populate contract)
                    if not schema_contract_recorded and self._record_schema_contract(factory, run_id, source_id, config, ctx):
                        schema_contract_recorded = True

                    # Clear operation_id — source item is fetched, transforms set their own state_id
                    ctx.operation_id = None

                    # Check aggregation timeouts BEFORE processing (flush OLD batch first)
                    timeout_result = check_aggregation_timeouts(
                        config=config,
                        processor=processor,
                        ctx=ctx,
                        pending_tokens=pending_tokens,
                        agg_transform_lookup=agg_transform_lookup,
                    )
                    counters.accumulate_flush_result(timeout_result)

                    results = processor.process_row(
                        row_index=row_index,
                        source_row=source_item,
                        transforms=config.transforms,
                        ctx=ctx,
                    )
                    if results:
                        loop_ctx.last_token_id = results[-1].token.token_id
                    accumulate_row_outcomes(results, counters, pending_tokens)

                    # Check coalesce timeouts after each row
                    if coalesce_executor is not None:
                        handle_coalesce_timeouts(
                            coalesce_executor=coalesce_executor,
                            coalesce_node_map=coalesce_node_map,
                            processor=processor,
                            ctx=ctx,
                            counters=counters,
                            pending_tokens=pending_tokens,
                        )

                    last_progress_time = self._maybe_emit_progress(
                        counters,
                        start_time,
                        last_progress_time,
                    )

                    # Graceful shutdown — current row fully processed, safe to stop
                    if shutdown_event is not None and shutdown_event.is_set():
                        interrupted_by_shutdown = True
                        break

                    # Restore operation_id for next iteration (generators execute on next())
                    self._restore_source_iteration_context(
                        ctx,
                        source_id=source_id,
                        source_operation_id=source_operation_id,
                    )

                # Post-loop: restore operation_id, flush aggregation/coalesce, record deferred state
                self._finalize_source_iteration(
                    loop_ctx,
                    factory,
                    run_id,
                    source_id,
                    source_operation_id,
                    field_resolution_recorded,
                    schema_contract_recorded,
                    interrupted_by_shutdown=interrupted_by_shutdown,
                )

            except Exception as e:
                self._emit_phase_error(PipelinePhase.PROCESS, e, target=config.source.name)
                raise

        return LoopResult(
            interrupted=interrupted_by_shutdown,
            start_time=start_time,
            phase_start=phase_start,
            last_progress_time=last_progress_time,
        )

    def _run_resume_processing_loop(
        self,
        loop_ctx: LoopContext,
        unprocessed_rows: Sequence[tuple[str, int, dict[str, Any]]],
        schema_contract: SchemaContract,
        *,
        shutdown_event: threading.Event | None = None,
    ) -> bool:
        """Run the resume processing loop: iterate unprocessed rows, transform, flush, accumulate.

        Includes end-of-loop aggregation/coalesce flushes only when the resume
        source is actually exhausted. On graceful shutdown we keep buffered state
        pending rather than forcing end-of-source semantics.

        Simpler than the main loop:
        - No quarantine handling (rows already validated)
        - No field resolution (already recorded in original run)
        - No schema contract recording (passed via parameter)
        - No operation_id lifecycle (no source track_operation)
        - No progress emission (known gap — see design doc)

        Returns:
            True if interrupted by shutdown, False otherwise.
        """
        # Destructure loop_ctx for local access
        config = loop_ctx.config
        ctx = loop_ctx.ctx
        processor = loop_ctx.processor
        counters = loop_ctx.counters
        pending_tokens = loop_ctx.pending_tokens
        coalesce_executor = loop_ctx.coalesce_executor
        coalesce_node_map = dict(loop_ctx.coalesce_node_map)
        agg_transform_lookup = dict(loop_ctx.agg_transform_lookup)

        # A buffered-only resume can have zero unprocessed rows but still carry
        # restored aggregation/coalesce state. If shutdown is already requested,
        # honor it before any end-of-source flush work so buffered state is
        # checkpointed again instead of being flushed to sinks.
        interrupted_by_shutdown = shutdown_event is not None and shutdown_event.is_set()

        # Process each unprocessed row using process_existing_row
        # (rows already exist in DB, only tokens need to be created)
        for row_id, _row_index, row_data in unprocessed_rows:
            if interrupted_by_shutdown:
                break
            counters.rows_processed += 1

            # ─────────────────────────────────────────────────────────────────
            # Check for timed-out aggregations BEFORE processing this row
            # Ensures timeout flushes OLD batch before processing new row
            # ─────────────────────────────────────────────────────────────────
            # Call module function directly (no wrapper method)
            timeout_result = check_aggregation_timeouts(
                config=config,
                processor=processor,
                ctx=ctx,
                pending_tokens=pending_tokens,
                agg_transform_lookup=agg_transform_lookup,
            )
            counters.accumulate_flush_result(timeout_result)

            # Wrap row_data in PipelineRow with contract (PIPELINEROW MIGRATION)
            # Row data from resume is a plain dict, but process_existing_row expects PipelineRow
            pipeline_row = PipelineRow(data=row_data, contract=schema_contract)

            results = processor.process_existing_row(
                row_id=row_id,
                row_data=pipeline_row,
                transforms=config.transforms,
                ctx=ctx,
            )
            if results:
                loop_ctx.last_token_id = results[-1].token.token_id

            # Handle all results from this row
            accumulate_row_outcomes(results, counters, pending_tokens)

            # ─────────────────────────────────────────────────────────────────
            # Check for timed-out coalesces after processing each row
            # Must check coalesce timeouts after each row to flush stale barriers
            # ─────────────────────────────────────────────────────────────────
            if coalesce_executor is not None:
                handle_coalesce_timeouts(
                    coalesce_executor=coalesce_executor,
                    coalesce_node_map=coalesce_node_map,
                    processor=processor,
                    ctx=ctx,
                    counters=counters,
                    pending_tokens=pending_tokens,
                )

            # ─────────────────────────────────────────────────────────────
            # GRACEFUL SHUTDOWN CHECK
            # Check between row iterations — current row is fully
            # processed, outcomes recorded, safe to stop here.
            # No quarantine path in resume (rows already validated).
            # ─────────────────────────────────────────────────────────────
            if shutdown_event is not None and shutdown_event.is_set():
                interrupted_by_shutdown = True
                break

        if not interrupted_by_shutdown:
            # CRITICAL: Flush remaining aggregation buffers only at true end-of-source.
            if config.aggregation_settings:
                # Call module function directly (no wrapper method)
                flush_result = flush_remaining_aggregation_buffers(
                    config=config,
                    processor=processor,
                    ctx=ctx,
                    pending_tokens=pending_tokens,
                )
                counters.accumulate_flush_result(flush_result)

                # TERMINAL GUARANTEE: same assertion as _post_source_iteration_work.
                for agg_node_id_str in config.aggregation_settings:
                    remaining = processor.get_aggregation_buffer_count(NodeID(agg_node_id_str))
                    if remaining > 0:
                        raise OrchestrationInvariantError(
                            f"Aggregation buffer for node '{agg_node_id_str}' still has "
                            f"{remaining} tokens after end-of-source flush. "
                            f"These tokens would never reach a terminal state."
                        )

            # Flush pending coalesce operations only when resume processing exhausted all rows.
            if coalesce_executor is not None:
                flush_coalesce_pending(
                    coalesce_executor=coalesce_executor,
                    coalesce_node_map=coalesce_node_map,
                    processor=processor,
                    ctx=ctx,
                    counters=counters,
                    pending_tokens=pending_tokens,
                )

        return interrupted_by_shutdown

    def _execute_run(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
        graph: ExecutionGraph,
        settings: ElspethSettings | None = None,
        *,
        payload_store: PayloadStore,
        shutdown_event: threading.Event | None = None,
    ) -> RunResult:
        """Execute the run using the execution graph.

        Orchestrates the four phases: graph registration, context initialization,
        source+process loop, sink writes. Returns RunStatus.RUNNING — the public
        run() wrapper transitions to COMPLETED after finalize_run().
        """
        self._current_graph = graph

        # 1. Register graph nodes and edges
        artifacts = self._register_graph_nodes_and_edges(factory, run_id, config, graph)

        # 2. Initialize context + processor
        run_ctx = self._initialize_run_context(
            factory,
            run_id,
            config,
            graph,
            settings,
            artifacts,
            payload_store,
            shutdown_event=shutdown_event,
        )
        self._run_transform_runtime_preflights(factory, run_id, config, run_ctx.ctx)

        loop_ctx = LoopContext(
            counters=ExecutionCounters(),
            pending_tokens={name: [] for name in config.sinks},
            processor=run_ctx.processor,
            ctx=run_ctx.ctx,
            config=config,
            agg_transform_lookup=run_ctx.agg_transform_lookup,
            coalesce_executor=run_ctx.coalesce_executor,
            coalesce_node_map=run_ctx.coalesce_node_map,
        )

        try:
            # 3. Source + Process phase
            loop_result = self._run_main_processing_loop(
                loop_ctx,
                factory,
                run_id,
                artifacts.source_id,
                artifacts.edge_map,
                shutdown_event=shutdown_event,
            )

            # 4. Sink writes — outside source_load track_operation context.
            # Each sink write has its own track_operation (sink_write) in SinkExecutor.
            self._flush_and_write_sinks(
                factory,
                run_id,
                loop_ctx,
                artifacts.sink_id_map,
                artifacts.edge_map,
                loop_result.interrupted,
                on_token_written_factory=self._make_checkpoint_after_sink_factory(run_id, run_ctx.processor),
                shutdown_checkpoint_source_id=artifacts.source_id,
            )

            # ADR-019 Phase 4: deferred cross-table invariant sweep.
            #
            # AUDIT-TRAIL DURABILITY CONTRACT:
            # 1. The run is still RUNNING here; successful terminal finalization
            #    has not executed.
            # 2. If this raises AuditIntegrityError, the exception propagates to
            #    the public run() failure ceremony, which finalizes the run as
            #    FAILED and re-raises the original exception.
            # 3. The offending token_outcomes/batch rows are evidence and are
            #    not deleted by the sweep.
            # 4. GracefulShutdownError skips this naturally because sink flush
            #    raises before this post-sink call site.
            factory.data_flow.sweep_deferred_invariants_or_crash(run_id)

            # 5. Final progress + PROCESS phase completion — AFTER sink writes
            # so these events reflect concrete, durable results. On shutdown,
            # _flush_and_write_sinks raises GracefulShutdownError before we
            # reach here — matching the pre-extraction behavior where the
            # shutdown raise prevented progress/PhaseCompleted emission.
            progress_interval = 100
            current_time = time.perf_counter()
            time_since_last_progress = current_time - loop_result.last_progress_time
            if loop_ctx.counters.rows_processed % progress_interval != 0 or time_since_last_progress >= 1.0:
                elapsed = current_time - loop_result.start_time
                self._events.emit(
                    ProgressEvent(
                        rows_processed=loop_ctx.counters.rows_processed,
                        # elspeth-5069612f3c — rows_routed split. See the
                        # earlier emitter in this file for the full rationale;
                        # this final-progress emission must match so the last
                        # streaming snapshot before terminal events agrees
                        # with the forthcoming CompletedData payload.
                        rows_succeeded=loop_ctx.counters.rows_succeeded,
                        rows_failed=loop_ctx.counters.rows_failed,
                        rows_quarantined=loop_ctx.counters.rows_quarantined,
                        rows_routed_success=loop_ctx.counters.rows_routed_success,
                        rows_routed_failure=loop_ctx.counters.rows_routed_failure,
                        elapsed_seconds=elapsed,
                    )
                )

            self._events.emit(PhaseCompleted(phase=PipelinePhase.PROCESS, duration_seconds=current_time - loop_result.phase_start))
        except GracefulShutdownError:
            raise
        except Exception as exc:
            raise _RunFailedWithPartialResultError(
                original_error=exc,
                partial_result=loop_ctx.counters.to_run_result(run_id, status=RunStatus.FAILED),
            ) from exc

        finally:
            self._cleanup_plugins(config, run_ctx.ctx, include_source=True)

        self._current_graph = None
        return loop_ctx.counters.to_run_result(run_id, status=RunStatus.RUNNING)

    def _reconstruct_resume_state(
        self,
        resume_point: ResumePoint,
        payload_store: PayloadStore,
    ) -> ResumeState:
        """Reconstruct state needed to process resumed rows.

        Creates a fresh factory, handles incomplete batches, restores aggregation state,
        deserializes the source schema for type fidelity, validates the schema contract,
        and retrieves unprocessed rows from the payload store.

        Args:
            resume_point: ResumePoint from RecoveryManager.get_resume_point()
            payload_store: PayloadStore for retrieving row data

        Returns:
            ResumeState with all reconstruction results.

        Raises:
            ValueError: If checkpoint_manager is not initialized.
            OrchestrationInvariantError: If schema contract is missing from audit trail.
        """
        run_id = resume_point.checkpoint.run_id

        # Create fresh factory (stateless, like run())
        # Pass payload_store for external call payload persistence
        factory = RecorderFactory(self._db, payload_store=payload_store)

        # 1. Handle incomplete batches - call module function directly
        batch_id_mapping = handle_incomplete_batches(factory.execution, run_id)

        # 2. Update run status to running
        factory.run_lifecycle.update_run_status(run_id, RunStatus.RUNNING)

        # 3. Build restored aggregation state map, rebinding batch_ids to retry batches
        restored_state: dict[str, AggregationCheckpointState] = {}
        if resume_point.aggregation_state is not None:
            rebound_state = rebind_checkpoint_batch_ids(resume_point.aggregation_state, batch_id_mapping)
            restored_state[resume_point.node_id] = rebound_state
        restored_coalesce_state = resume_point.coalesce_state

        # 4. Get unprocessed row data from payload store
        from elspeth.core.checkpoint import RecoveryManager

        if self._checkpoint_manager is None:
            raise OrchestrationInvariantError(
                "CheckpointManager is required for resume - Orchestrator must be initialized with checkpoint_manager"
            )
        recovery = RecoveryManager(self._db, self._checkpoint_manager)

        # TYPE FIDELITY: Retrieve source schema from audit trail for type restoration
        # Resume must use the ORIGINAL run's schema, not the current source's schema
        # This enables proper type coercion (datetime/Decimal) from JSON payload strings
        source_schema_json = factory.run_lifecycle.get_source_schema(run_id)

        # Deserialize schema and recreate Pydantic model class with full type fidelity
        # Call module function directly (no wrapper method)
        schema_dict = json.loads(source_schema_json)
        source_schema_class = reconstruct_schema_from_json(schema_dict)

        # PIPELINEROW MIGRATION: Retrieve contract from audit trail for row wrapping
        # During resume, we need to wrap plain dicts in PipelineRow with contract
        # This ensures type fidelity and maintains the same data structures as main run
        schema_contract = factory.run_lifecycle.get_run_contract(run_id)
        if schema_contract is None:
            # TIER-1 AUDIT INTEGRITY: Crash if contract is missing from audit trail
            # Per CLAUDE.md: "Bad data in the audit trail = crash immediately"
            # Inferring a contract from row data would:
            # 1. Mask missing/corrupt audit data (evidence tampering)
            # 2. Produce incomplete contracts (fields appearing later are omitted)
            # 3. Violate the NO LEGACY CODE POLICY (no backward compatibility shims)
            raise OrchestrationInvariantError(
                f"Cannot resume run '{run_id}': schema contract is missing from audit trail. "
                f"This indicates either:\n"
                f"  1. The audit database is corrupt or incomplete\n"
                f"  2. The run was started with a version that didn't record contracts\n"
                f"Resume cannot proceed safely without the schema contract. "
                f"The audit trail must be complete and trustworthy."
            )

        # Resume replays persisted PipelineRow payloads through NullSource rather
        # than re-opening the original source plugin, so source-boundary evidence
        # is inherited from the original run. That is only sound if the current
        # declaration-contract and Tier-1 registries still exactly match the
        # manifest captured at original run start.
        recorded_runtime_val_manifest = factory.run_lifecycle.get_runtime_val_manifest(run_id)
        current_runtime_val_manifest = canonical_json(build_runtime_val_manifest())
        if current_runtime_val_manifest != recorded_runtime_val_manifest:
            raise OrchestrationInvariantError(
                f"Cannot resume run '{run_id}': runtime VAL manifest drift detected. "
                "The current contract registry no longer matches the registry "
                "captured in the original run header, so inherited source-boundary "
                "evidence is no longer trustworthy. Resume requires identical "
                "declaration-contract and Tier-1 registries."
            )

        unprocessed_rows = recovery.get_unprocessed_row_data(run_id, payload_store, source_schema_class=source_schema_class)

        return ResumeState(
            factory=factory,
            run_id=run_id,
            restored_aggregation_state=restored_state,
            restored_coalesce_state=restored_coalesce_state,
            unprocessed_rows=unprocessed_rows,
            schema_contract=schema_contract,
        )

    def resume(
        self,
        resume_point: ResumePoint,
        config: PipelineConfig,
        graph: ExecutionGraph,
        *,
        payload_store: PayloadStore,
        settings: ElspethSettings | None = None,
        shutdown_event: threading.Event | None = None,
    ) -> RunResult:
        """Resume a failed run from a checkpoint.

        STATELESS: Like run(), creates fresh factory and processor internally.
        This mirrors the reality that recovery happens in a new process.

        Args:
            resume_point: ResumePoint from RecoveryManager.get_resume_point()
            config: Same PipelineConfig used for original run()
            graph: Same ExecutionGraph used for original run()
            payload_store: PayloadStore for retrieving row data (required)
            settings: Full settings (optional, for retry config etc.)

        Returns:
            RunResult with recovery outcome

        Raises:
            ValueError: If payload_store is not provided
        """
        if payload_store is None:
            raise OrchestrationInvariantError("payload_store is required for resume - row data must be retrieved from stored payloads")

        # ADR-010 §Decision 3: freeze both registries at bootstrap, mirroring
        # run(). Recovery happens in a new process — the module import chain
        # registers PassThroughDeclarationContract, but without this call the
        # registries are never frozen, leaving a window where
        # register_declaration_contract() could succeed post-bootstrap on the
        # resume path.
        prepare_for_run()

        self._rebase_checkpoint_sequence(resume_point.sequence_number)
        state = self._reconstruct_resume_state(resume_point, payload_store)
        run_id = state.run_id
        factory = state.factory
        restored_state = state.restored_aggregation_state
        restored_coalesce_state = state.restored_coalesce_state
        if restored_coalesce_state is not None and not restored_coalesce_state.has_resumable_state:
            restored_coalesce_state = None
        schema_contract = state.schema_contract
        unprocessed_rows = state.unprocessed_rows
        resume_start_time = time.perf_counter()

        # 5. Process unprocessed rows (with graceful shutdown support)

        # When shutdown_event is provided (testing), skip signal handler
        # installation and use the caller's event directly.
        shutdown_ctx = nullcontext(shutdown_event) if shutdown_event is not None else self._shutdown_handler_context()

        try:
            if not unprocessed_rows and not restored_state and restored_coalesce_state is None:
                factory.data_flow.sweep_deferred_invariants_or_crash(run_id)

                # All rows were processed - complete the run.
                #
                # Phase 2.2 (elspeth-0de989c56d): the resume's local counters
                # are 0 here because nothing was reprocessed, but the audit DB
                # carries the truth.  Aggregate token_outcomes to derive the
                # correct four-value terminal status and feed it to both the
                # Landscape finalize and the local RunResult.
                (
                    terminal_status,
                    audit_rows_processed,
                    audit_rows_succeeded,
                    audit_rows_failed,
                    audit_rows_routed_success,
                    audit_rows_routed_failure,
                    audit_rows_quarantined,
                ) = self._derive_resume_terminal_status_from_audit(factory, run_id)
                factory.run_lifecycle.finalize_run(run_id, status=terminal_status)

                # Emit RunFinished telemetry (matching the normal completion path)
                self._emit_telemetry(
                    RunFinished(
                        timestamp=datetime.now(UTC),
                        run_id=run_id,
                        status=terminal_status,
                        row_count=audit_rows_processed,
                        duration_ms=0.0,
                    )
                )

                # Emit RunSummary event
                cli_status, exit_code = self._cli_completion_for(terminal_status)
                self._events.emit(
                    RunSummary(
                        run_id=run_id,
                        status=cli_status,
                        total_rows=audit_rows_processed,
                        succeeded=audit_rows_succeeded,
                        failed=audit_rows_failed,
                        quarantined=audit_rows_quarantined,
                        duration_seconds=0.0,
                        exit_code=exit_code,
                        routed_success=audit_rows_routed_success,
                        routed_failure=audit_rows_routed_failure,
                        routed_destinations=(),
                    )
                )

                # Delete checkpoints on successful completion
                self._delete_checkpoints(run_id)

                return RunResult(
                    run_id=run_id,
                    status=terminal_status,
                    rows_processed=audit_rows_processed,
                    rows_succeeded=audit_rows_succeeded,
                    rows_failed=audit_rows_failed,
                    rows_routed_success=audit_rows_routed_success,
                    rows_routed_failure=audit_rows_routed_failure,
                    rows_quarantined=audit_rows_quarantined,
                    routed_destinations={},
                )

            with shutdown_ctx as active_event:
                result = self._process_resumed_rows(
                    factory=factory,
                    run_id=run_id,
                    config=config,
                    graph=graph,
                    unprocessed_rows=unprocessed_rows,
                    restored_aggregation_state=restored_state,
                    restored_coalesce_state=restored_coalesce_state,
                    settings=settings,
                    payload_store=payload_store,
                    schema_contract=schema_contract,
                    shutdown_event=active_event,
                )

            # 6. Complete the run with reproducibility grade
            # SUCCESS PATH: Must be inside try block so RunFinished is emitted
            # BEFORE the finally block flushes telemetry to exporters.
            # Fix: elspeth-rapid-sg0q — previously this was after the finally block,
            # meaning RunFinished was emitted after telemetry flush (never exported).
            #
            # Phase 2.2 (elspeth-0de989c56d): pick the new four-value terminal
            # status from the row-count shape returned by the resume's row
            # processing.
            terminal_status = derive_terminal_run_status(
                rows_processed=result.rows_processed,
                rows_succeeded=result.rows_succeeded,
                rows_failed=result.rows_failed,
                rows_routed_success=result.rows_routed_success,
                rows_routed_failure=result.rows_routed_failure,
                rows_quarantined=result.rows_quarantined,
                rows_coalesce_failed=result.rows_coalesce_failed,
            )
            factory.run_lifecycle.finalize_run(run_id, status=terminal_status)
            result = replace(result, status=terminal_status)

            # 7. Emit RunFinished telemetry
            resume_duration_ms = (time.perf_counter() - resume_start_time) * 1000
            self._emit_telemetry(
                RunFinished(
                    timestamp=datetime.now(UTC),
                    run_id=run_id,
                    status=terminal_status,
                    row_count=result.rows_processed,
                    duration_ms=resume_duration_ms,
                )
            )

            # 8. Emit RunSummary event
            cli_status, exit_code = self._cli_completion_for(terminal_status)
            total_duration = time.perf_counter() - resume_start_time
            self._events.emit(
                RunSummary(
                    run_id=run_id,
                    status=cli_status,
                    total_rows=result.rows_processed,
                    succeeded=result.rows_succeeded,
                    failed=result.rows_failed,
                    quarantined=result.rows_quarantined,
                    duration_seconds=total_duration,
                    exit_code=exit_code,
                    routed_success=result.rows_routed_success,
                    routed_failure=result.rows_routed_failure,
                    routed_destinations=tuple(result.routed_destinations.items()),
                )
            )

            # 9. Delete checkpoints on successful completion
            self._delete_checkpoints(run_id)

            return result
        except GracefulShutdownError as shutdown_exc:
            with best_effort("Interrupted ceremony on resume graceful shutdown", run_id=run_id):
                self._emit_interrupted_ceremony(run_id, factory, shutdown_exc, resume_start_time)
            raise  # Propagate to CLI
        except _RunFailedWithPartialResultError as failed_exc:
            with best_effort("Partial-result failure ceremony on resume", run_id=run_id):
                self._emit_failed_ceremony(
                    run_id,
                    factory,
                    resume_start_time,
                    failed_exc.partial_result,
                )
            raise failed_exc.original_error.with_traceback(failed_exc.original_traceback) from None
        except Exception:
            # Finalize as FAILED to prevent the run from being stuck in RUNNING
            # permanently (which blocks future resume attempts). The outer broad-except
            # is justified — any unhandled exception during resume needs ceremony.
            with best_effort("Generic failure ceremony on resume", run_id=run_id):
                self._emit_failed_ceremony(run_id, factory, resume_start_time)
            raise
        finally:
            self._safe_flush_telemetry()

    def _process_resumed_rows(
        self,
        factory: RecorderFactory,
        run_id: str,
        config: PipelineConfig,
        graph: ExecutionGraph,
        unprocessed_rows: Sequence[tuple[str, int, dict[str, Any]]],
        restored_aggregation_state: Mapping[str, AggregationCheckpointState],
        restored_coalesce_state: CoalesceCheckpointState | None,
        settings: ElspethSettings | None = None,
        *,
        payload_store: PayloadStore,
        schema_contract: SchemaContract,
        shutdown_event: threading.Event | None = None,
    ) -> RunResult:
        """Process unprocessed rows during resume.

        Mirrors _execute_run() structure but with resume-specific divergences
        documented in the accounting block below. Returns RunStatus.RUNNING —
        the public resume() wrapper transitions to COMPLETED after finalize_run().
        """
        # ─────────────────────────────────────────────────────────────────
        # Divergence accounting: _process_resumed_rows vs _execute_run
        #
        # Source on_start():       Skipped (include_source_on_start=False)
        # Graph registration:     Loads from DB (_setup_resume_context)
        # Quarantine routing:     Not applicable (rows already validated)
        # Field resolution:       Skipped (loaded from DB in original run)
        # Schema contract:        Skipped (passed via parameter)
        # operation_id lifecycle: Not applicable (no source track_operation)
        # Progress emission:      None (known gap — T24 follow-up)
        # Checkpointing:          Same post-sink + shutdown semantics as run()
        # ─────────────────────────────────────────────────────────────────

        self._current_graph = graph

        # 1. Setup (loads graph artifacts from original run's DB records)
        artifacts = self._setup_resume_context(factory, run_id, config, graph)

        # 2. Initialize context + processor (source on_start skipped)
        run_ctx = self._initialize_run_context(
            factory,
            run_id,
            config,
            graph,
            settings,
            artifacts,
            payload_store,
            include_source_on_start=False,
            restored_aggregation_state=restored_aggregation_state,
            restored_coalesce_state=restored_coalesce_state,
            shutdown_event=shutdown_event,
        )

        # Restore contract from parameter (already retrieved by resume() caller)
        run_ctx.ctx.contract = schema_contract
        self._run_transform_runtime_preflights(factory, run_id, config, run_ctx.ctx)

        loop_ctx = LoopContext(
            counters=ExecutionCounters(),
            pending_tokens={name: [] for name in config.sinks},
            processor=run_ctx.processor,
            ctx=run_ctx.ctx,
            config=config,
            agg_transform_lookup=run_ctx.agg_transform_lookup,
            coalesce_executor=run_ctx.coalesce_executor,
            coalesce_node_map=run_ctx.coalesce_node_map,
        )

        try:
            # 3. Process loop (resume path)
            interrupted = self._run_resume_processing_loop(
                loop_ctx,
                unprocessed_rows,
                schema_contract,
                shutdown_event=shutdown_event,
            )

            # 4. Flush + write sinks with checkpoint advancement
            self._flush_and_write_sinks(
                factory,
                run_id,
                loop_ctx,
                artifacts.sink_id_map,
                artifacts.edge_map,
                interrupted,
                on_token_written_factory=self._make_checkpoint_after_sink_factory(run_id, run_ctx.processor),
                shutdown_checkpoint_source_id=artifacts.source_id,
            )

            # ADR-019 Phase 4: resumed row processing reaches stable I1a/I1b
            # postconditions only after resume sink writes finish.
            factory.data_flow.sweep_deferred_invariants_or_crash(run_id)
        except GracefulShutdownError:
            raise
        except Exception as exc:
            raise _RunFailedWithPartialResultError(
                original_error=exc,
                partial_result=loop_ctx.counters.to_run_result(run_id, status=RunStatus.FAILED),
            ) from exc

        finally:
            self._cleanup_plugins(config, run_ctx.ctx, include_source=False)

        self._current_graph = None
        return loop_ctx.counters.to_run_result(run_id, status=RunStatus.RUNNING)
