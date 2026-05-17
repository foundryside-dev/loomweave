"""Shared contracts for cross-boundary data types.

All dataclasses, enums, TypedDicts, and NamedTuples that cross subsystem
boundaries MUST be defined here. Internal types are whitelisted in
config/cicd/contracts-whitelist.yaml.

This package is Layer 0 (L0) in the 4-layer dependency hierarchy and has
NO runtime imports from core, engine, or plugins. This is enforced by CI
(``enforce_tier_model.py`` with rule L1). TYPE_CHECKING imports from core
exist for type annotations only and do not create runtime coupling.

To maintain this property, Settings classes (RetrySettings, ElspethSettings, etc.)
are NOT re-exported here - import them from elspeth.core.config.

Import patterns:
    # Contracts (lightweight, no heavy dependencies)
    from elspeth.contracts import NodeType, TransformResult, Run

    # Settings classes (from core, pulls in heavy deps)
    from elspeth.core.config import RetrySettings, ElspethSettings
"""

from elspeth.contracts.aggregation_checkpoint import (
    AggregationCheckpointState,
    AggregationNodeCheckpoint,
    AggregationTokenCheckpoint,
)
from elspeth.contracts.audit import (
    Artifact,
    Batch,
    BatchMember,
    BatchOutput,
    BatchStatusUpdate,
    Call,
    Checkpoint,
    Edge,
    ExportStatusUpdate,
    Node,
    NodeState,
    NodeStateCompleted,
    NodeStateFailed,
    NodeStateOpen,
    NodeStatePending,
    NonCanonicalMetadata,
    Operation,
    RoutingEvent,
    Row,
    RowLineage,
    Run,
    SecretResolution,
    SecretResolutionInput,
    Token,
    TokenOutcome,
    TokenParent,
    TransformErrorRecord,
    ValidationErrorRecord,
)
from elspeth.contracts.auth import AuthProviderType
from elspeth.contracts.call_data import (
    CallPayload,
    HTTPCallError,
    HTTPCallRequest,
    HTTPCallResponse,
    LLMCallError,
    LLMCallRequest,
    LLMCallResponse,
    RawCallPayload,
)
from elspeth.contracts.checkpoint import ResumeCheck, ResumePoint
from elspeth.contracts.cli import ExecutionResult, ProgressEvent
from elspeth.contracts.coalesce_checkpoint import (
    CoalesceCheckpointState,
    CoalescePendingCheckpoint,
    CoalesceTokenCheckpoint,
)
from elspeth.contracts.coalesce_enums import CoalescePolicy, MergeStrategy
from elspeth.contracts.coalesce_metadata import ArrivalOrderEntry, CoalesceMetadata

# =============================================================================
# Settings classes are NOT re-exported from contracts
# =============================================================================
# To maintain contracts as a leaf module (no core dependencies), Settings classes
# must be imported directly from elspeth.core.config:
#     from elspeth.core.config import RetrySettings, ElspethSettings
#
# Config protocols are in contracts; Settings classes remain in core.config.
# =============================================================================
from elspeth.contracts.config import (
    # Alignment documentation
    EXEMPT_SETTINGS,
    FIELD_MAPPINGS,
    # Default registries
    INTERNAL_DEFAULTS,
    POLICY_DEFAULTS,
    SETTINGS_TO_RUNTIME,
    # Runtime config dataclasses
    ExporterConfig,
    # Runtime protocols (what engine components expect)
    RuntimeCheckpointProtocol,
    RuntimeConcurrencyProtocol,
    RuntimeRateLimitProtocol,
    RuntimeRetryProtocol,
    RuntimeTelemetryConfig,
    RuntimeTelemetryProtocol,
)
from elspeth.contracts.contexts import (
    LifecycleContext,
    SinkContext,
    SourceContext,
    TransformContext,
)

# Schema contracts — builder
from elspeth.contracts.contract_builder import ContractBuilder

# Schema contracts — pipeline propagation
from elspeth.contracts.contract_propagation import (
    merge_contract_with_output,
    narrow_contract_to_output,
    propagate_contract,
)

# Schema contracts — audit trail records
from elspeth.contracts.contract_records import (
    ContractAuditRecord,
    FieldAuditRecord,
    ValidationErrorWithContract,
)
from elspeth.contracts.data import (
    CompatibilityResult,
    PluginSchema,
    SchemaValidationError,
    check_compatibility,
    validate_row,
)
from elspeth.contracts.diversion import RowDiversion, SinkWriteResult
from elspeth.contracts.engine import BufferEntry, PendingOutcome, RetryPolicy
from elspeth.contracts.enums import (
    BackpressureMode,
    BatchStatus,
    CallStatus,
    CallType,
    Determinism,
    ExportStatus,
    NodeStateStatus,
    NodeType,
    ReproducibilityGrade,
    RoutingKind,
    RoutingMode,
    RunMode,
    RunStatus,
    TelemetryGranularity,
    TerminalOutcome,
    TerminalPath,
    TriggerType,
    error_edge_label,
)
from elspeth.contracts.errors import (
    # Tier 1 guard tuple — single source of truth for "never catch" exceptions
    CoalesceFailureReason,
    CommencementGateFailedError,
    ConfigGateReason,
    # Schema contract violations
    ContractMergeError,
    ContractViolation,
    DependencyFailedError,
    DuplicateDocumentError,
    ErrorDetail,
    ExecutionError,
    ExtraFieldViolation,
    FrameworkBugError,
    GracefulShutdownError,
    MaxRetriesExceeded,
    MissingFieldViolation,
    OrchestrationInvariantError,
    PassThroughContractViolation,
    PluginContractViolation,
    QueryFailureDetail,
    RetrievalNotReadyError,
    RoutingReason,
    RowErrorEntry,
    SinkDiversionReason,
    SourceQuarantineReason,
    TemplateErrorEntry,
    TransformActionCategory,
    TransformErrorCategory,
    TransformErrorReason,
    TransformSuccessReason,
    TypeMismatchViolation,
    UsageStats,
    violations_to_error_reason,
)
from elspeth.contracts.events import (
    ExternalCallCompleted,
    FieldResolutionApplied,
    GateEvaluated,
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
    TelemetryEvent,
    TokenCompleted,
    TransformCompleted,
)
from elspeth.contracts.header_modes import (
    HeaderMode,
    parse_header_mode,
    resolve_headers,
)
from elspeth.contracts.identity import TokenInfo
from elspeth.contracts.node_state_context import (
    AggregationFlushContext,
    GateEvaluationContext,
    NodeStateContext,
    PoolConfigSnapshot,
    PoolExecutionContext,
    PoolStatsSnapshot,
    QueryOrderEntry,
)
from elspeth.contracts.payload_store import IntegrityError, PayloadNotFoundError, PayloadStore
from elspeth.contracts.pipeline_runner import PipelineRunner
from elspeth.contracts.plugin_context import (
    PluginContext,
    TransformErrorToken,
    ValidationErrorToken,
)
from elspeth.contracts.plugin_protocols import (
    BatchTransformProtocol,
    SinkProtocol,
    SourceProtocol,
    TransformProtocol,
)
from elspeth.contracts.probes import CollectionProbe, CollectionReadinessResult
from elspeth.contracts.results import (
    ArtifactDescriptor,
    ExceptionResult,
    FailureInfo,
    GateResult,
    RowResult,
    SourceRow,
    TransformResult,
)
from elspeth.contracts.routing import (
    EdgeInfo,
    RouteDestination,
    RouteDestinationKind,
    RoutingAction,
    RoutingSpec,
)

# Schema contracts — core types
from elspeth.contracts.schema_contract import (
    FieldContract,
    PipelineRow,
    SchemaContract,
)
from elspeth.contracts.schema_contract_factory import (
    create_contract_from_config,
    map_schema_mode,
)
from elspeth.contracts.sink import OutputValidationResult
from elspeth.contracts.token_usage import TokenUsage
from elspeth.contracts.transform_contract import (
    create_output_contract_from_schema,
    validate_output_against_contract,
)
from elspeth.contracts.type_normalization import classify_runtime_type, normalize_type_for_contract
from elspeth.contracts.types import (
    AggregationName,
    BranchName,
    CoalesceName,
    GateName,
    NodeID,
    SinkName,
    StepResolver,
)
from elspeth.contracts.url import (
    SENSITIVE_PARAMS,
    SanitizedDatabaseUrl,
    SanitizedWebhookUrl,
)
from elspeth.contracts.value_source import (
    CatalogValueSource,
    DerivedFromSiblingValueSource,
    ValueSource,
)

__all__ = [  # Grouped by category for readability
    # audit
    "Artifact",
    "Operation",
    # errors
    "CommencementGateFailedError",
    "DependencyFailedError",
    "DuplicateDocumentError",
    "FrameworkBugError",
    "GracefulShutdownError",
    "OrchestrationInvariantError",
    "MaxRetriesExceeded",
    "PassThroughContractViolation",
    "PluginContractViolation",
    "RetrievalNotReadyError",
    "CoalesceFailureReason",
    "ConfigGateReason",
    "ErrorDetail",
    "ExecutionError",
    "QueryFailureDetail",
    "RoutingReason",
    "RowErrorEntry",
    "SinkDiversionReason",
    "SourceQuarantineReason",
    "TemplateErrorEntry",
    "TransformActionCategory",
    "TransformErrorCategory",
    "TransformErrorReason",
    "TransformSuccessReason",
    "UsageStats",
    # schema contract violations
    "ContractMergeError",
    "ContractViolation",
    "ExtraFieldViolation",
    "MissingFieldViolation",
    "TypeMismatchViolation",
    "Batch",
    "BatchMember",
    "BatchOutput",
    "BatchStatusUpdate",
    "Call",
    "Checkpoint",
    "Edge",
    "ExportStatusUpdate",
    "Node",
    "NodeState",
    "NodeStateCompleted",
    "NodeStateFailed",
    "NodeStateOpen",
    "NodeStatePending",
    "NonCanonicalMetadata",
    "RoutingEvent",
    "Row",
    "RowLineage",
    "Run",
    "SecretResolution",
    "SecretResolutionInput",
    "Token",
    "TokenOutcome",
    "TokenParent",
    "TransformErrorRecord",
    "ValidationErrorRecord",
    # auth
    "AuthProviderType",
    # config - Runtime protocols (contracts, not core)
    "RuntimeCheckpointProtocol",
    "RuntimeConcurrencyProtocol",
    "RuntimeRateLimitProtocol",
    "RuntimeRetryProtocol",
    "RuntimeTelemetryProtocol",
    # config - Runtime config dataclasses
    "ExporterConfig",
    "RuntimeTelemetryConfig",
    # config - Default registries
    "INTERNAL_DEFAULTS",
    "POLICY_DEFAULTS",
    # config - Alignment documentation
    "EXEMPT_SETTINGS",
    "FIELD_MAPPINGS",
    "SETTINGS_TO_RUNTIME",
    # NOTE: Settings classes (RetrySettings, ElspethSettings, etc.) are NOT here
    # Import them from elspeth.core.config to avoid breaking the leaf boundary
    # enums
    "BackpressureMode",
    "BatchStatus",
    "CallStatus",
    "CallType",
    "Determinism",
    "ExportStatus",
    "NodeStateStatus",
    "NodeType",
    "ReproducibilityGrade",
    "RoutingKind",
    "RoutingMode",
    "RunMode",
    "RunStatus",
    "TelemetryGranularity",
    "TerminalOutcome",
    "TerminalPath",
    "TriggerType",
    "error_edge_label",
    # identity
    "TokenInfo",
    # checkpoint
    "AggregationCheckpointState",
    "AggregationNodeCheckpoint",
    "AggregationTokenCheckpoint",
    "CoalesceCheckpointState",
    "CoalescePendingCheckpoint",
    "CoalesceTokenCheckpoint",
    "ResumeCheck",
    "ResumePoint",
    # coalesce enums
    "CoalescePolicy",
    "MergeStrategy",
    # coalesce metadata
    "ArrivalOrderEntry",
    "CoalesceMetadata",
    # node state context
    "AggregationFlushContext",
    "GateEvaluationContext",
    "NodeStateContext",
    "PoolConfigSnapshot",
    "PoolExecutionContext",
    "PoolStatsSnapshot",
    "QueryOrderEntry",
    # types
    "AggregationName",
    "BranchName",
    "CoalesceName",
    "GateName",
    "NodeID",
    "SinkName",
    "StepResolver",
    # diversion
    "RowDiversion",
    "SinkWriteResult",
    # results (NOTE: AcceptResult deleted in aggregation structural cleanup)
    "ArtifactDescriptor",
    "ExceptionResult",
    "FailureInfo",
    "GateResult",
    "RowResult",
    "SourceRow",
    "TransformResult",
    # routing
    "EdgeInfo",
    "RouteDestination",
    "RouteDestinationKind",
    "RoutingAction",
    "RoutingSpec",
    # data
    "CompatibilityResult",
    "PluginSchema",
    "SchemaValidationError",
    "check_compatibility",
    "validate_row",
    # engine
    "BufferEntry",
    "PendingOutcome",
    "RetryPolicy",
    # payload_store
    "IntegrityError",
    "PayloadNotFoundError",
    "PayloadStore",
    # contexts (phase-based protocols)
    "LifecycleContext",
    "SinkContext",
    "SourceContext",
    "TransformContext",
    # plugin_context
    "PluginContext",
    "TransformErrorToken",
    "ValidationErrorToken",
    # plugin protocols
    "BatchTransformProtocol",
    "SinkProtocol",
    "SourceProtocol",
    "TransformProtocol",
    # events
    "ExternalCallCompleted",
    "FieldResolutionApplied",
    "GateEvaluated",
    "PhaseAction",
    "PhaseChanged",
    "PhaseCompleted",
    "PhaseError",
    "PhaseStarted",
    "PipelinePhase",
    "RowCreated",
    "RunCompletionStatus",
    "RunFinished",
    "RunStarted",
    "RunSummary",
    "TelemetryEvent",
    "TokenCompleted",
    "TransformCompleted",
    # cli
    "ExecutionResult",
    "ProgressEvent",
    # url
    "SENSITIVE_PARAMS",
    "SanitizedDatabaseUrl",
    "SanitizedWebhookUrl",
    # sink
    "OutputValidationResult",
    # token usage
    "TokenUsage",
    # call data (LLM + HTTP audit records)
    "CallPayload",
    "HTTPCallError",
    "HTTPCallRequest",
    "HTTPCallResponse",
    "LLMCallError",
    "LLMCallRequest",
    "LLMCallResponse",
    "RawCallPayload",
    # Schema contracts — core types
    "ContractBuilder",
    "create_contract_from_config",
    "FieldContract",
    "map_schema_mode",
    "classify_runtime_type",
    "normalize_type_for_contract",
    "PipelineRow",
    "PipelineRunner",
    "SchemaContract",
    # Schema contracts — audit trail records
    "ContractAuditRecord",
    "FieldAuditRecord",
    "ValidationErrorWithContract",
    # Schema contracts — pipeline propagation
    "create_output_contract_from_schema",
    "HeaderMode",
    "merge_contract_with_output",
    "narrow_contract_to_output",
    "parse_header_mode",
    "propagate_contract",
    "resolve_headers",
    "validate_output_against_contract",
    "violations_to_error_reason",
    # probes
    "CollectionProbe",
    "CollectionReadinessResult",
    # value-source declarations
    "CatalogValueSource",
    "DerivedFromSiblingValueSource",
    "ValueSource",
]
