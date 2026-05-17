"""ExecutionService protocol — called from FastAPI route handlers."""

from __future__ import annotations

from pathlib import Path
from typing import Protocol, runtime_checkable
from uuid import UUID

from elspeth.web.auth.models import UserIdentity
from elspeth.web.composer.state import CompositionState
from elspeth.web.execution.schemas import RunAccounting, RunStatusResponse, ValidationResult
from elspeth.web.sessions.protocol import RunRecord


class ValidationSettings(Protocol):
    """Settings needed by direct runtime preflight validation.

    Structurally satisfied by WebSettings (data_dir is a Path) and any test
    stub that exposes a Path-typed data_dir attribute.
    """

    @property
    def data_dir(self) -> Path: ...


class StateAccessError(Exception):
    """Raised when a client-supplied ``state_id`` cannot be accessed.

    Deliberately does NOT distinguish between "state does not exist"
    and "state exists but belongs to another user's session". Echoing
    that distinction through the HTTP response body is an IDOR oracle:
    an authenticated user can probe arbitrary UUIDs against their own
    ``/execute`` endpoint and learn which ones exist in OTHER users'
    sessions. The two cases are therefore folded into a single
    exception type whose handler MUST return a byte-identical
    ``{"detail": "State not found"}`` 404. The ``send_message`` route in
    ``sessions/routes.py`` carries the same contract on the chat-message
    IDOR surface.

    If a future refactor needs diagnostic precision about which of the
    two branches tripped (e.g. for ops dashboards), it MUST route that
    signal through server-side audit/telemetry, never through the HTTP
    response body or status code.
    """


class YamlGenerator(Protocol):
    """Protocol for objects that generate YAML from CompositionState.

    The production implementation is the ``elspeth.web.composer.yaml_generator``
    module, which satisfies this protocol via its module-level ``generate_yaml``
    function. Injected as a dependency so tests can substitute a stub.
    """

    def generate_yaml(self, state: CompositionState) -> str: ...


@runtime_checkable
class ExecutionService(Protocol):
    """Protocol for pipeline execution operations.

    All methods are called from FastAPI route handlers in the async context.
    execute() returns immediately; the pipeline runs in a background thread.
    """

    async def validate(self, session_id: UUID, *, user_id: str | None = None) -> ValidationResult:
        """Async dry-run validation using real engine code paths.

        Loads the current CompositionState for the session, generates YAML,
        and runs it through load_settings -> instantiate_plugins_from_config
        -> ExecutionGraph.from_plugin_instances -> graph.validate().

        When user_id and a secret_service are provided, also validates
        that all {"secret_ref": "NAME"} patterns are resolvable.

        Async because the implementation wraps the sync validate_pipeline()
        call via run_in_executor to avoid blocking the event loop.
        """
        ...

    async def validate_state(self, state: CompositionState, *, user_id: str | None = None) -> ValidationResult:
        """Async dry-run validation for an already materialized composition state.

        Callers that have already read a CompositionState use this overload so
        validation and any adjacent projections are computed from the same
        composition version.
        """
        ...

    async def execute(
        self,
        session_id: UUID,
        state_id: UUID | None = None,
        *,
        user_id: str | None = None,
        auth_provider_type: str | None = None,
        fanout_ack_token: str | None = None,
    ) -> UUID:
        """Start a background pipeline run.

        Returns the run_id immediately. Raises RunAlreadyActiveError if
        a pending or running Run already exists for this session.

        Args:
            session_id: Session to execute.
            state_id: Specific state to execute (latest if None).
            user_id: Authenticated user's ID for scoped secret resolution.
            auth_provider_type: Auth provider namespace for Landscape run attribution.
            fanout_ack_token: Optional launch acknowledgement for high-fanout
                LLM/provider-call risk.

        Note: async because it calls SessionService (async) for active-run
        check and run creation. The actual pipeline runs in a background
        thread via ThreadPoolExecutor — only the setup is async.
        """
        ...

    async def get_status(
        self,
        run_id: UUID,
        *,
        accounting: RunAccounting | None = None,
        run_record: RunRecord | None = None,
    ) -> RunStatusResponse:
        """Return current run status from the Run database record."""
        ...

    async def cancel(self, run_id: UUID) -> None:
        """Cancel a run. Sets the shutdown Event for active runs.

        Idempotent — cancelling a terminal run is a no-op.
        Note: async because cancelling a pending run calls
        SessionService.update_run_status() directly (not via _call_async,
        since we're in the event loop thread).
        """
        ...

    async def verify_run_ownership(self, user: UserIdentity, run_id: str) -> bool:
        """Verify that a run belongs to the authenticated user's session."""
        ...

    async def shutdown(self) -> None:
        """Drain executor-owned runs without blocking the event loop."""
        ...
