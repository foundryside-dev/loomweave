"""Plugin execution context.

The PluginContext carries everything a plugin needs during execution:
- Run metadata (run_id, config)
- Audit trail recording (landscape)
- External call recording (record_call, record_validation_error, record_transform_error)
- Batch transform support (checkpoints, token identity)
"""

from __future__ import annotations

import logging
import threading
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

from elspeth.contracts.audit import TokenRef
from elspeth.contracts.call_data import RawCallPayload
from elspeth.contracts.freeze import deep_freeze

if TYPE_CHECKING:
    from elspeth.contracts import Call, CallStatus, CallType, TransformErrorReason
    from elspeth.contracts.audit_protocols import PluginAuditWriter
    from elspeth.contracts.config.runtime import RuntimeConcurrencyConfig
    from elspeth.contracts.errors import ContractViolation
    from elspeth.contracts.identity import TokenInfo
    from elspeth.contracts.payload_store import PayloadStore
    from elspeth.contracts.schema_contract import PipelineRow, SchemaContract
    from elspeth.core.rate_limit import RateLimitRegistry

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class ValidationErrorToken:
    """Token returned when recording a validation error.

    Allows tracking the quarantined row through the audit trail.
    Frozen because these are Tier 1 audit records — immutable after creation.
    """

    row_id: str
    node_id: str
    error_id: str | None = None  # Set if recorded to landscape
    destination: str = "discard"  # Sink name or "discard"


@dataclass(frozen=True, slots=True)
class TransformErrorToken:
    """Token returned when recording a transform error.

    Allows tracking the errored row through the audit trail.
    This is for LEGITIMATE processing errors, not transform bugs.
    Frozen because these are Tier 1 audit records — immutable after creation.
    """

    token_id: str
    transform_id: str
    error_id: str | None = None  # Set if recorded to landscape
    destination: str = "discard"  # Sink name or "discard"


@dataclass
class PluginContext:
    """Context passed to every plugin operation.

    Provides access to:
    - Run metadata (run_id, config)
    - Audit trail (landscape)
    - External call recording (record_call)
    - Validation/transform error recording
    - Batch checkpoint management

    Example:
        def process(self, row: PipelineRow, ctx: PluginContext) -> TransformResult:
            result = do_work(row, ctx.config)
            return TransformResult.success(result, success_reason={"action": "processed"})
    """

    run_id: str
    config: Mapping[str, Any]

    # === Audit & Infrastructure ===
    landscape: PluginAuditWriter | None = None
    payload_store: PayloadStore | None = None
    rate_limit_registry: RateLimitRegistry | None = None
    concurrency_config: RuntimeConcurrencyConfig | None = None
    shutdown_event: threading.Event | None = None

    # Additional metadata
    node_id: str | None = field(default=None)

    # === Row-Level Pipelining (BatchTransformMixin) ===
    # Set by orchestrator/executor when calling accept() on batch transforms.
    # Used by RowReorderBuffer for FIFO ordering and audit attribution.
    # IMPORTANT: This is derivative state - the executor must keep it synchronized
    # with the authoritative token flowing through the pipeline.
    token: TokenInfo | None = field(default=None)

    # === Batch Token Identity (Aggregation) ===
    # Set by AggregationExecutor.execute_flush() before calling batch-aware transforms.
    # Maps row index in the batch to the originating token_id. Batch transforms
    # use this to pass per-row token_id to audited clients for correct telemetry
    # attribution. When None, the transform falls back to ctx.token (single-token mode).
    batch_token_ids: tuple[str, ...] | None = field(default=None)

    # === Schema Contract ===
    # Set by executor when processing transforms to enable contract-aware template
    # access (original header names). When transforms receive a plain dict (not
    # PipelineRow), they can still access the contract via ctx.contract.
    # This allows templates using {{ row["Original Header"] }} to resolve correctly.
    contract: SchemaContract | None = field(default=None)

    # === State & Call Recording ===
    # Set by executor to enable transforms to record external calls
    # Exactly one of state_id or operation_id should be set when recording calls
    state_id: str | None = field(default=None)  # For transform calls (via node_states)
    operation_id: str | None = field(default=None)  # For source/sink calls (via operations)
    # Note: call_index allocation is delegated to PluginAuditWriter.allocate_call_index()
    # to ensure coordination with audited clients.

    # === Telemetry Callback ===
    # Callback to emit telemetry events for external calls.
    # Always present - when telemetry is disabled, orchestrator sets this to a no-op.
    # Plugins ALWAYS call this after successful Landscape recording - no None checks.
    telemetry_emit: Callable[[Any], None] = field(default=lambda event: None)

    # Validation errors that must later be linked to a persisted quarantine row.
    # Entries are (match_key, error_id), where match_key hashes the raw row payload
    # before orchestrator normalization/wrapping.
    _pending_quarantine_validation_errors: list[tuple[str, str]] = field(default_factory=list)

    def __post_init__(self) -> None:
        # Deep-freeze config so plugins cannot mutate the run configuration
        # after the audit snapshot (settings_json, config_hash) is recorded.
        # PluginContext is not frozen (checkpoint/token need mutation), but
        # config must be immutable for audit integrity.
        self.config = deep_freeze(self.config)

    @staticmethod
    def _validation_error_match_key(row: Any) -> str:
        """Build a stable lookup key for a raw validation-error payload."""
        from elspeth.contracts.hashing import repr_hash, stable_hash

        try:
            return stable_hash(row)
        except (ValueError, TypeError):
            return repr_hash(row)

    def pop_pending_quarantine_validation_error_id(self, row: Any) -> str | None:
        """Consume the queued validation error ID matching a quarantined row payload."""
        match_key = self._validation_error_match_key(row)
        for index, (pending_match_key, error_id) in enumerate(self._pending_quarantine_validation_errors):
            if pending_match_key == match_key:
                del self._pending_quarantine_validation_errors[index]
                return error_id
        return None

    def record_call(
        self,
        call_type: CallType,
        status: CallStatus,
        request_data: dict[str, Any],
        response_data: dict[str, Any] | None = None,
        error: dict[str, Any] | None = None,
        latency_ms: float | None = None,
        *,
        provider: str = "unknown",
    ) -> Call | None:
        """Record an external API call to the audit trail and emit telemetry.

        Provides a convenient way for plugins to record external calls
        without managing call indices manually. Routes to the appropriate
        recorder method based on whether state_id or operation_id is set.

        After recording to Landscape (the legal record), emits an
        ExternalCallCompleted telemetry event for operational visibility.

        Args:
            call_type: Type of call (LLM, HTTP, SQL, FILESYSTEM)
            status: Outcome (SUCCESS, ERROR)
            request_data: Request payload (will be hashed)
            response_data: Response payload (optional for errors)
            error: Error details if status is ERROR
            latency_ms: Call duration in milliseconds
            provider: Provider name for telemetry (e.g., "openrouter", "azure")

        Returns:
            The recorded Call, or None if landscape not configured

        Raises:
            FrameworkBugError: If neither or both of state_id and operation_id are set
        """
        from elspeth.contracts import FrameworkBugError

        if self.landscape is None:
            raise FrameworkBugError(
                f"record_call() called without landscape. "
                f"Context state: run_id={self.run_id}, state_id={self.state_id}, "
                f"operation_id={self.operation_id}. "
                f"This is a framework bug — orchestrator must inject landscape before plugin execution."
            )

        # Enforce XOR: exactly one of state_id or operation_id must be set
        has_state = self.state_id is not None
        has_operation = self.operation_id is not None

        if has_state and has_operation:
            raise FrameworkBugError(
                f"record_call() called with BOTH state_id and operation_id set. "
                f"state_id={self.state_id}, operation_id={self.operation_id}. "
                f"This is a framework bug - context should have exactly one parent."
            )

        if not has_state and not has_operation:
            raise FrameworkBugError(
                f"record_call() called without state_id or operation_id. "
                f"Context state: run_id={self.run_id}, node_id={self.node_id}. "
                f"This is a framework bug - context should have been set by orchestrator/executor."
            )

        # Route to appropriate recorder method
        if has_state:
            # Delegate call_index allocation to centralized PluginAuditWriter.
            # This ensures UNIQUE(state_id, call_index) when mixing ctx.record_call()
            # with audited clients (AuditedLLMClient, AuditedHTTPClient), which also
            # use recorder.allocate_call_index().
            if self.state_id is None:
                raise FrameworkBugError("record_call has_state=True but state_id is None")
            call_index = self.landscape.allocate_call_index(self.state_id)

            recorded_call = self.landscape.record_call(
                state_id=self.state_id,
                call_index=call_index,
                call_type=call_type,
                status=status,
                request_data=RawCallPayload(request_data),
                response_data=RawCallPayload(response_data) if response_data is not None else None,
                error=RawCallPayload(error) if error is not None else None,
                latency_ms=latency_ms,
            )
            parent_id: str = self.state_id
        else:
            # Operation call - recorder handles call index allocation
            if self.operation_id is None:
                raise FrameworkBugError("record_call has_operation=True but operation_id is None")
            recorded_call = self.landscape.record_operation_call(
                operation_id=self.operation_id,
                call_type=call_type,
                status=status,
                request_data=RawCallPayload(request_data),
                response_data=RawCallPayload(response_data) if response_data is not None else None,
                error=RawCallPayload(error) if error is not None else None,
                latency_ms=latency_ms,
            )
            parent_id = self.operation_id

        # Resolve token_id from authoritative state_id lookup BEFORE telemetry.
        # This is a data integrity check — FrameworkBugError must NOT be swallowed
        # by the telemetry error handler below.
        # Resolve from state_id, not from self.token_id which may be stale.
        token_id = None
        if has_state:
            if self.state_id is None:
                raise FrameworkBugError("record_call has_state=True but state_id is None (token_id lookup)")
            node_state = self.landscape.get_node_state(self.state_id)
            if node_state is None:
                raise FrameworkBugError(
                    f"record_call() has state_id={self.state_id} but get_node_state() "
                    f"returned None. This is a framework bug — state_id should always "
                    f"resolve to a valid node_state."
                )
            token_id = node_state.token_id
            # Validate that ctx.token (if set) is consistent with the authoritative source
            if self.token is not None and self.token.token_id != token_id:
                raise FrameworkBugError(
                    f"record_call() token mismatch: ctx.token.token_id={self.token.token_id} "
                    f"but node_state.token_id={token_id} for state_id={self.state_id}. "
                    f"This is a framework bug — ctx.token is out of sync with state_id."
                )

        # Emit telemetry AFTER successful Landscape recording
        # Wrapped in try/except to prevent telemetry failures from affecting callers
        try:
            from elspeth.contracts.enums import CallType as CallTypeEnum
            from elspeth.contracts.events import ExternalCallCompleted

            # Pass data directly to RawCallPayload. No defensive copy needed:
            # RawCallPayload.__init__ calls deep_freeze(), which creates an
            # independent frozen copy. Callers mutating the original dict after
            # record_call() won't affect the telemetry payload.
            # (Existing test: test_request_payload_snapshot_is_immutable_after_call)
            request_snapshot = request_data
            response_snapshot = response_data

            # Extract token usage for LLM calls if available.
            # response_snapshot may contain frozen containers (MappingProxyType) —
            # use Mapping ABC for isinstance checks, not dict.
            token_usage = None
            if call_type == CallTypeEnum.LLM and response_snapshot is not None:
                from collections.abc import Mapping

                from elspeth.contracts.token_usage import TokenUsage

                raw_usage = response_snapshot.get("usage")
                if isinstance(raw_usage, Mapping):
                    tu = TokenUsage.from_dict(raw_usage)
                    token_usage = tu if tu.has_data else None

            # Wrap data in RawCallPayload for typed telemetry payload.
            # RawCallPayload.__init__ calls deep_freeze(), creating an independent
            # frozen copy — no prior snapshot/deepcopy step is needed.
            request_payload = RawCallPayload(request_snapshot)
            response_payload = RawCallPayload(response_snapshot) if response_snapshot is not None else None

            # Use hashes from the recorded Call object — the recorder is the
            # single source of truth for hashing (via core.canonical.stable_hash).
            # Recomputing here would risk divergence if the hash implementations differ.
            self.telemetry_emit(
                ExternalCallCompleted(
                    timestamp=datetime.now(UTC),
                    run_id=self.run_id,
                    # Use correct field based on context type
                    state_id=self.state_id if has_state else None,
                    operation_id=self.operation_id if has_operation else None,
                    token_id=token_id,
                    call_type=call_type,
                    provider=provider,
                    status=status,
                    latency_ms=latency_ms if latency_ms is not None else 0.0,
                    request_hash=recorded_call.request_hash,
                    response_hash=recorded_call.response_hash,
                    request_payload=request_payload,
                    response_payload=response_payload,
                    token_usage=token_usage,
                )
            )
        except (OSError, ConnectionError, TimeoutError) as tel_err:
            # Telemetry transport failures are expected and must not corrupt
            # the call recording. All other exceptions (including Tier 1,
            # programming errors like KeyError) propagate naturally.
            logger.warning(
                "telemetry_emit_failed in record_call",
                extra={
                    "error": str(tel_err),
                    "error_type": type(tel_err).__name__,
                    "run_id": self.run_id,
                    "parent_id": parent_id,
                },
            )

        return recorded_call

    def record_validation_error(
        self,
        row: Any,
        error: str,
        schema_mode: str,
        destination: str,
        *,
        contract_violation: ContractViolation | None = None,
    ) -> ValidationErrorToken:
        """Record a validation error for audit trail.

        Called by sources when row validation fails. The row will be
        quarantined (not processed further) but the error is recorded
        for complete audit coverage.

        Args:
            row: The row data that failed validation (may be non-dict for
                 malformed external data like JSON arrays containing primitives)
            error: Description of the validation failure
            schema_mode: "fixed", "flexible", "observed", or "parse" (parse = file-level parse error)
            destination: Sink name where row is routed, or "discard"
            contract_violation: Optional contract violation details for structured auditing

        Returns:
            ValidationErrorToken for tracking the quarantined row
        """
        from elspeth.contracts.hashing import repr_hash, stable_hash

        # Generate row_id from content hash if not present
        # External data may be non-dict (e.g., JSON array containing primitives),
        # so we must check isinstance before accessing dict keys
        if isinstance(row, dict) and "id" in row:
            row_id = str(row["id"])
        else:
            # Try canonical hash first, fall back to repr() hash for non-serializable data
            # This is Tier-3 (external data) - we must record what we saw, even if malformed
            try:
                row_id = stable_hash(row)[:16]
            except (ValueError, TypeError) as e:
                # Non-canonical data (NaN, Infinity, or other non-serializable types)
                # Hash the repr() instead - not canonical, but preserves audit trail.
                # Log only the error type, not row content (logging policy: no row data outside Landscape).
                logger.warning(
                    "Row data not canonically serializable, using repr() hash: %s (%s)",
                    type(e).__name__,
                    str(e),
                )
                row_id = repr_hash(row)[:16]

        if self.node_id is None:
            from elspeth.contracts import FrameworkBugError

            raise FrameworkBugError(
                f"record_validation_error() called without node_id. "
                f"Context state: run_id={self.run_id}. "
                f"This is a framework bug — orchestrator must set node_id before validation."
            )

        if self.landscape is None:
            from elspeth.contracts import FrameworkBugError

            raise FrameworkBugError(
                f"record_validation_error() called without landscape. "
                f"Context state: run_id={self.run_id}, node_id={self.node_id}. "
                f"This is a framework bug — orchestrator must inject landscape before source validation."
            )

        # Record to landscape audit trail
        error_id = self.landscape.record_validation_error(
            run_id=self.run_id,
            node_id=self.node_id,
            row_data=row,
            error=error,
            schema_mode=schema_mode,
            destination=destination,
            contract_violation=contract_violation,
        )

        if destination != "discard" and error_id is not None:
            match_key = self._validation_error_match_key(row)
            self._pending_quarantine_validation_errors.append((match_key, error_id))

        return ValidationErrorToken(
            row_id=row_id,
            node_id=self.node_id,
            error_id=error_id,
            destination=destination,
        )

    def record_transform_error(
        self,
        token_id: str,
        transform_id: str,
        row: dict[str, Any] | PipelineRow,
        error_details: TransformErrorReason,
        destination: str,
    ) -> TransformErrorToken:
        """Record a transform processing error for audit trail.

        Called when a transform returns TransformResult.error().
        This is for legitimate errors, NOT transform bugs (which crash).

        Args:
            token_id: Token ID for the row being processed
            transform_id: Transform that returned the error
            row: The row data that could not be processed
            error_details: Error details from TransformResult.error() (TransformErrorReason TypedDict)
            destination: Sink name where row is routed, or "discard"

        Returns:
            TransformErrorToken for tracking
        """
        if self.landscape is None:
            from elspeth.contracts import FrameworkBugError

            raise FrameworkBugError(
                f"record_transform_error() called without landscape. "
                f"Context state: run_id={self.run_id}, node_id={self.node_id}. "
                f"This is a framework bug — orchestrator must inject landscape before transform execution."
            )

        error_id = self.landscape.record_transform_error(
            ref=TokenRef(token_id=token_id, run_id=self.run_id),
            transform_id=transform_id,
            row_data=row,
            error_details=error_details,
            destination=destination,
        )

        return TransformErrorToken(
            token_id=token_id,
            transform_id=transform_id,
            error_id=error_id,
            destination=destination,
        )
