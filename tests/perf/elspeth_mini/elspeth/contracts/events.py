"""Observability events for pipeline execution.

These domain events provide visibility into pipeline phases, progress,
and completion status. Events are emitted by the orchestrator and consumed
by CLI formatters for human-readable or structured output.
"""

import copy
import dataclasses
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from types import MappingProxyType
from typing import Any, ClassVar

from elspeth.contracts.call_data import CallPayload
from elspeth.contracts.enums import (
    CallStatus,
    CallType,
    NodeStateStatus,
    RoutingMode,
    RunStatus,
    TerminalOutcome,
    TerminalPath,
)
from elspeth.contracts.freeze import freeze_fields, require_int
from elspeth.contracts.token_usage import TokenUsage


class PipelinePhase(StrEnum):
    """Pipeline lifecycle phases for observability events.

    Uses (str, Enum) pattern for consistency with existing codebase
    (see contracts/enums.py RunStatus).
    """

    CONFIG = "config"
    GRAPH = "graph"
    PLUGINS = "plugins"
    AGGREGATIONS = "aggregations"
    DATABASE = "database"
    SCHEMA_VALIDATION = "schema_validation"
    SOURCE = "source"
    PROCESS = "process"
    EXPORT = "export"


class PhaseAction(StrEnum):
    """Actions within a pipeline phase."""

    LOADING = "loading"
    VALIDATING = "validating"
    BUILDING = "building"
    CONNECTING = "connecting"
    INITIALIZING = "initializing"
    PROCESSING = "processing"
    EXPORTING = "exporting"


class RunCompletionStatus(StrEnum):
    """Final status for RunSummary events."""

    COMPLETED = "completed"
    FAILED = "failed"
    PARTIAL = "partial"
    INTERRUPTED = "interrupted"


@dataclass(frozen=True, slots=True)
class PhaseStarted:
    """Emitted when a pipeline phase begins.

    Phases represent major lifecycle stages:
    - config: Loading and validating settings
    - graph: Building and validating execution graph
    - plugins: Instantiating source, transforms, and sinks
    - aggregations: Instantiating aggregation plugins
    - database: Connecting to Landscape database
    - schema_validation: Validating plugin schemas
    - source: Loading source data
    - process: Processing rows through transforms
    - export: Exporting results (when enabled)

    Attributes:
        phase: The lifecycle phase starting
        action: What's happening (e.g., "loading", "validating")
        target: Optional target (e.g., file path, plugin name)
    """

    phase: PipelinePhase
    action: PhaseAction
    target: str | None = None


@dataclass(frozen=True, slots=True)
class PhaseCompleted:
    """Emitted when a pipeline phase completes successfully."""

    phase: PipelinePhase
    duration_seconds: float


@dataclass(frozen=True, slots=True)
class PhaseError:
    """Emitted when a pipeline phase fails.

    Stores the full exception object to preserve traceback, exception type,
    and chained causes for debugging and audit trail integrity.
    """

    phase: PipelinePhase
    error: BaseException
    target: str | None = None  # What failed (plugin name, file path, etc.)

    @property
    def error_message(self) -> str:
        """Human-readable error message for formatting."""
        return str(self.error)


@dataclass(frozen=True, slots=True)
class RunSummary:
    """Summary emitted when pipeline run finishes (success or failure).

    Provides final metrics for CI integration: exit codes, row counts,
    routing breakdown.

    Routing breakdown:
    - routed_success: Rows routed via gate route_to_sink (intentional MOVE — success-side routing)
    - routed_failure: Rows routed via transform on_error (DIVERT — failure-side routing)
    - routed_destinations: Count per destination sink {sink_name: count}; the per-sink
      breakdown is not split by routing intent — see ADR-004 for rationale.
    """

    run_id: str
    status: RunCompletionStatus
    total_rows: int
    succeeded: int
    failed: int
    quarantined: int
    duration_seconds: float
    exit_code: int  # 0=success, 1=partial failure, 2=total failure
    routed_success: int = 0  # Rows routed via gate route_to_sink (intentional MOVE)
    routed_failure: int = 0  # Rows routed via transform on_error (DIVERT)
    routed_destinations: tuple[tuple[str, int], ...] = ()  # (sink_name, count) pairs

    def __post_init__(self) -> None:
        require_int(self.total_rows, "total_rows", min_value=0)
        require_int(self.succeeded, "succeeded", min_value=0)
        require_int(self.failed, "failed", min_value=0)
        require_int(self.quarantined, "quarantined", min_value=0)
        require_int(self.exit_code, "exit_code", min_value=0)
        require_int(self.routed_success, "routed_success", min_value=0)
        require_int(self.routed_failure, "routed_failure", min_value=0)


# =============================================================================
# Telemetry Events (Row-Level Observability)
# =============================================================================
# These events are emitted by the engine and consumed by telemetry exporters.
# They provide operational visibility alongside the Landscape audit trail.


@dataclass(frozen=True, slots=True)
class TelemetryEvent:
    """Base class for all telemetry events.

    All events include:
    - timestamp: When the event occurred (UTC)
    - run_id: Pipeline run this event belongs to

    Events are immutable (frozen) for thread-safety and to prevent
    accidental modification during export.
    """

    timestamp: datetime
    run_id: str

    def to_dict(self) -> dict[str, Any]:
        """Serialize event to a plain dict for telemetry export.

        Replaces ``dataclasses.asdict()`` which cannot deep-copy
        ``MappingProxyType`` fields (raises ``TypeError: cannot pickle
        'mappingproxy' object``).  This method adds ``MappingProxyType``
        to the recursive dispatch so frozen mapping fields serialize
        correctly while remaining immutable at runtime.
        """
        # _event_field_to_serializable returns dict for dataclass inputs;
        # the Any return type is for the recursive leaf cases.
        result: dict[str, Any] = _event_field_to_serializable(self)
        return result


def _event_field_to_serializable(obj: Any) -> Any:
    """Recursively convert a value to a plain-dict tree.

    Handles the same cases as ``dataclasses.asdict()`` plus
    ``MappingProxyType``.
    """
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        return {f.name: _event_field_to_serializable(getattr(obj, f.name)) for f in dataclasses.fields(obj)}
    if isinstance(obj, MappingProxyType):
        return {k: _event_field_to_serializable(v) for k, v in obj.items()}
    if isinstance(obj, dict):
        return {_event_field_to_serializable(k): _event_field_to_serializable(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return type(obj)(_event_field_to_serializable(v) for v in obj)
    return copy.deepcopy(obj)


@dataclass(frozen=True, slots=True)
class TransformCompleted(TelemetryEvent):
    """Emitted when a transform finishes processing a row.

    Note: input_hash and output_hash are optional because:
    - Failed transforms may not have produced output (output_hash=None)
    - Edge cases during error handling may not have computed input hash
    """

    row_id: str
    token_id: str
    node_id: str
    plugin_name: str
    status: NodeStateStatus
    duration_ms: float
    input_hash: str | None
    output_hash: str | None


@dataclass(frozen=True, slots=True)
class GateEvaluated(TelemetryEvent):
    """Emitted when a gate makes a routing decision."""

    row_id: str
    token_id: str
    node_id: str
    plugin_name: str
    routing_mode: RoutingMode
    destinations: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class TokenCompleted(TelemetryEvent):
    """Emitted when a token reaches its terminal state (ADR-019 two-axis)."""

    row_id: str
    token_id: str
    outcome: TerminalOutcome | None
    path: TerminalPath
    sink_name: str | None


# =============================================================================
# Lifecycle Events
# =============================================================================


@dataclass(frozen=True, slots=True)
class RunStarted(TelemetryEvent):
    """Emitted when a pipeline run begins.

    Attributes:
        config_hash: Hash of the pipeline configuration for change detection
        source_plugin: Name of the source plugin being used
    """

    config_hash: str
    source_plugin: str


@dataclass(frozen=True, slots=True)
class RunFinished(TelemetryEvent):
    """Emitted when a pipeline run finishes (success or failure).

    Pairs with RunStarted for telemetry lifecycle tracking.

    Attributes:
        status: Final run status (completed, failed)
        row_count: Total rows processed
        duration_ms: Total run duration in milliseconds
    """

    status: RunStatus
    row_count: int
    duration_ms: float

    def __post_init__(self) -> None:
        require_int(self.row_count, "row_count", min_value=0)


@dataclass(frozen=True, slots=True)
class PhaseChanged(TelemetryEvent):
    """Emitted when pipeline transitions between phases.

    Phases represent major lifecycle stages (config, graph, plugins,
    database, source, process, export). This event fires on phase
    entry with the action being performed.

    Attributes:
        phase: The pipeline phase being entered
        action: What's happening in this phase (loading, validating, etc.)
    """

    phase: PipelinePhase
    action: PhaseAction


@dataclass(frozen=True, slots=True)
class FieldResolutionApplied(TelemetryEvent):
    """Emitted when source field normalization is applied.

    Captures the mapping from original external headers to normalized
    field names. Useful for debugging field name issues and monitoring
    normalization patterns across runs.

    Attributes:
        source_plugin: Name of the source plugin
        field_count: Number of fields in the mapping
        normalization_version: Algorithm version used (None if no normalization)
        resolution_mapping: Complete original->normalized mapping
    """

    source_plugin: str
    field_count: int
    normalization_version: str | None
    resolution_mapping: Mapping[str, str]

    def __post_init__(self) -> None:
        """Snapshot + freeze: always copy to decouple from caller's dict."""
        require_int(self.field_count, "field_count", min_value=0)
        freeze_fields(self, "resolution_mapping")


# =============================================================================
# Row-Level Events
# =============================================================================


@dataclass(frozen=True, slots=True)
class RowCreated(TelemetryEvent):
    """Emitted when a new row enters the pipeline from the source.

    Attributes:
        row_id: Stable source row identity
        token_id: Token instance for this row in the DAG
        content_hash: Hash of the row content for deduplication
    """

    row_id: str
    token_id: str
    content_hash: str


# =============================================================================
# External Call Events
# =============================================================================


@dataclass(frozen=True, slots=True)
class ExternalCallCompleted(TelemetryEvent):
    """Emitted when an external call (LLM, HTTP, SQL) completes.

    Calls can originate from two contexts:
    - Transform context: Call made during transform processing (has state_id)
    - Operation context: Call made during source load or sink write (has operation_id)

    Exactly one of state_id or operation_id should be set.

    Attributes:
        state_id: Node state that made the call (for transform context)
        operation_id: Operation that made the call (for source/sink context)
        token_id: Token associated with the transform context, if available
        call_type: Type of external call (llm, http, sql, filesystem)
        provider: Service provider (e.g., "azure-openai", "anthropic")
        status: Call result (success, error)
        latency_ms: Call duration in milliseconds
        request_hash: Hash of request payload for debugging (optional)
        response_hash: Hash of response payload for debugging (optional)
        request_payload: Full request data for observability (optional).
            For LLM calls: contains 'messages' (prompt), 'model', 'temperature', etc.
            For HTTP calls: contains 'method', 'url', 'json', 'headers', etc.
        response_payload: Full response data for observability (optional).
            For LLM calls: contains 'content' (completion), 'model', 'usage', etc.
            For HTTP calls: contains 'status_code', 'headers', 'body', etc.
        token_usage: LLM token counts if applicable (optional)
    """

    call_type: CallType
    provider: str
    status: CallStatus
    latency_ms: float
    state_id: str | None = None
    operation_id: str | None = None
    token_id: str | None = None
    request_hash: str | None = None
    response_hash: str | None = None
    request_payload: CallPayload | None = None
    response_payload: CallPayload | None = None
    token_usage: TokenUsage | None = None

    def __post_init__(self) -> None:
        """Validate XOR constraint.

        No deep-copy needed: frozen DTOs are immutable, and RawCallPayload
        receives pre-copied data from PluginContext.record_call().
        """
        has_state = self.state_id is not None
        has_operation = self.operation_id is not None
        if has_state == has_operation:  # Both True or both False
            raise ValueError(
                f"ExternalCallCompleted requires exactly one of state_id or operation_id. "
                f"Got state_id={self.state_id!r}, operation_id={self.operation_id!r}"
            )

    # Fields that have their own to_dict() — skip in the generic recursive walk
    # to avoid O(payload-size) double-serialization.
    _DTO_FIELDS: ClassVar[frozenset[str]] = frozenset({"request_payload", "response_payload", "token_usage"})

    def to_dict(self) -> dict[str, Any]:
        """Serialize event, using DTO-aware serialization for payloads.

        Overrides base to_dict() because the generic _event_field_to_serializable
        would decompose DTOs by dataclass fields (producing wrong shapes for DTOs
        that omit None fields or spread extra_kwargs). Calling .to_dict() on each
        payload produces the correct audit-stable dict representation.

        Excludes DTO fields from the initial recursive walk to avoid
        double-serialization (O(payload-size) CPU/memory amplification).
        """
        d: dict[str, Any] = {}
        for f in dataclasses.fields(self):
            if f.name in self._DTO_FIELDS:
                continue
            d[f.name] = _event_field_to_serializable(getattr(self, f.name))
        # Serialize DTO fields via their own to_dict() (correct shape),
        # or None if not set. Always present in output for shape stability.
        d["request_payload"] = self.request_payload.to_dict() if self.request_payload is not None else None
        d["response_payload"] = self.response_payload.to_dict() if self.response_payload is not None else None
        d["token_usage"] = self.token_usage.to_dict() if self.token_usage is not None else None
        return d
