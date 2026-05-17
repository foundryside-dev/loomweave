"""Error and reason schema contracts.

Frozen dataclasses and TypedDict schemas for structured error payloads
in the audit trail.  These provide consistent shapes for executor error
recording.
"""

from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, ClassVar, Literal, NotRequired, Required, TypedDict

from elspeth.contracts.audit_evidence import AuditEvidenceBase
from elspeth.contracts.declaration_contracts import DeclarationContractViolation
from elspeth.contracts.freeze import deep_freeze, freeze_fields

# Re-export FrameworkBugError which lives in tier_registry to break the import
# cycle between the registry primitive and the public exception module. Apply
# @tier_1_error here so the decoration happens in errors.py (the canonical
# Tier-1 declaration site). The re-exported name is identical to the class
# object in tier_registry — isinstance/except identity is preserved.
from elspeth.contracts.tier_registry import FrameworkBugError as _FrameworkBugError
from elspeth.contracts.tier_registry import tier_1_error

FrameworkBugError = tier_1_error(
    reason="ADR-008: framework internal inconsistency — engine bug",
    caller_module=__name__,
)(_FrameworkBugError)

if TYPE_CHECKING:
    from elspeth.contracts.coalesce_metadata import CoalesceMetadata


@dataclass(frozen=True, slots=True)
class FailedTurnMetadata:
    """Route-visible metadata for a compose turn whose audit path failed."""

    assistant_message_id: str | None
    tool_calls_attempted: int
    tool_responses_persisted: int | None = None


# TIER-2: Frozen audit DTO (not a raiseable exception) — records structured error payloads to the Landscape audit trail.
@dataclass(frozen=True, slots=True)
class ExecutionError:
    """Frozen dataclass for execution error payloads.

    Used by executors when recording node state failures.
    Immutable and validated at construction time, consistent with
    other audit DTOs (TokenUsage, LLMCallRequest, etc.).

    The ``exception_type`` field is renamed from ``type`` to avoid
    shadowing the Python builtin.  ``to_dict()`` serializes it back
    as ``"type"`` for hash stability with existing audit records.

    The optional ``context`` field carries structured per-exception payload
    (e.g., ``PassThroughContractViolation.to_audit_dict()``). It is populated
    by ``NodeStateGuard.__exit__`` when the wrapped exception exposes
    ``to_audit_dict()`` — see ``engine/executors/state_guard.py``.
    Serialized as the top-level ``context`` key by ``to_dict()`` so triage
    queries can filter on ``json_extract(error_data, '$.context.<field>')``.
    """

    exception: str  # String representation of the exception
    exception_type: str  # Exception class name (e.g., "ValueError")
    traceback: str | None = None  # Optional full traceback
    phase: str | None = None  # Optional phase indicator (e.g., "flush" for sink flush errors)
    context: Mapping[str, Any] | None = None  # Structured per-exception audit payload (ADR-008)

    def __post_init__(self) -> None:
        """Validate that required error fields are non-empty.

        These fields are recorded in the audit trail. Empty strings would
        produce valid-looking but uninformative error records.
        """
        if not self.exception:
            raise ValueError("ExecutionError.exception must not be empty")
        if not self.exception_type:
            raise ValueError("ExecutionError.exception_type must not be empty")
        # Deep-freeze the structured context payload (ADR-008) so the Tier-1
        # audit-recording path cannot be mutated after construction.
        if self.context is not None:
            try:
                context_keys = self.context.keys()
            except AttributeError as exc:
                raise TypeError(f"ExecutionError.context must be a mapping, got {type(self.context).__name__}") from exc
            if any(type(key) is not str for key in context_keys):
                raise TypeError("ExecutionError.context keys must be strings")
            freeze_fields(self, "context")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to audit-trail dict.

        Uses ``"type"`` as the key name (not ``exception_type``) to
        maintain hash stability with existing audit records.
        Omits None-valued optional fields.
        """
        d: dict[str, Any] = {
            "exception": self.exception,
            "type": self.exception_type,
        }
        if self.traceback is not None:
            d["traceback"] = self.traceback
        if self.phase is not None:
            d["phase"] = self.phase
        if self.context is not None:
            d["context"] = self.context
        return d


@dataclass(frozen=True, slots=True)
class CoalesceFailureReason:
    """Frozen DTO for coalesce/barrier failure payloads.

    Used by CoalesceExecutor when recording fork-join barrier failures.
    These are internal engine errors, not transform or plugin errors.
    """

    failure_reason: str  # Why coalesce failed (e.g., "quorum_not_met")
    expected_branches: tuple[str, ...]  # Branches expected to arrive
    branches_arrived: tuple[str, ...]  # Branches that actually arrived
    merge_policy: str  # Merge policy in effect
    timeout_ms: int | None = None  # Timeout that triggered failure (if applicable)
    select_branch: str | None = None  # Target branch for select policy (if applicable)

    def __post_init__(self) -> None:
        """Validate coalesce failure record invariants."""
        if not self.failure_reason:
            raise ValueError("CoalesceFailureReason.failure_reason must not be empty")
        if not self.merge_policy:
            raise ValueError("CoalesceFailureReason.merge_policy must not be empty")
        if not self.expected_branches:
            raise ValueError("CoalesceFailureReason.expected_branches must not be empty")
        if self.timeout_ms is not None and self.timeout_ms < 0:
            raise ValueError(f"CoalesceFailureReason.timeout_ms must be non-negative, got {self.timeout_ms}")
        freeze_fields(self, "expected_branches", "branches_arrived")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to audit-trail dict.

        Omits None-valued optional fields for compact JSON.
        """
        d: dict[str, Any] = {
            "failure_reason": self.failure_reason,
            "expected_branches": list(self.expected_branches),
            "branches_arrived": list(self.branches_arrived),
            "merge_policy": self.merge_policy,
        }
        if self.timeout_ms is not None:
            d["timeout_ms"] = self.timeout_ms
        if self.select_branch is not None:
            d["select_branch"] = self.select_branch
        return d


class ConfigGateReason(TypedDict):
    """Reason from config-driven gate (expression evaluation).

    Used by gates defined via GateSettings with condition expressions.
    Constructed by GateExecutor.execute_config_gate().

    Fields:
        condition: The expression that was evaluated (e.g., "row['score'] > 100")
        result: The route label that matched (e.g., "true", "false")
    """

    condition: str
    result: str


# RoutingReason union is defined after TransformErrorReason (see RoutingReason below)


# Literal type for common transform actions (extensible - str also accepted)
TransformActionCategory = Literal[
    # Processing actions
    "processed",  # Generic successful processing
    "mapped",  # Field mapping completed
    "validated",  # Validation passed
    "enriched",  # Data enrichment from external source
    "transformed",  # Data transformation applied
    "normalized",  # Data normalization applied
    "filtered",  # Row passed filter criteria
    "classified",  # Classification assigned
    # Skip/passthrough actions
    "passthrough",  # No changes made (intentional)
    "skipped",  # Processing skipped (e.g., data already present)
    "cached",  # Result retrieved from cache
    # Plugin-specific actions
    "query_completed",  # LLM single-query completion
    "multi_query_enriched",  # LLM multi-query execution completed
    "rag_retrieval",  # RAG retrieval pipeline completed
]


class TransformSuccessReason(TypedDict):
    """Metadata for successful transform operations.

    Provides structured audit information about what a transform did,
    beyond just the input/output data. This enables:
    - Efficient audit queries (fields_modified without diffing)
    - Data quality monitoring (validation_warnings for non-blocking warnings)
    - Conditional path tracking (action distinguishes code paths)

    Used when transforms return TransformResult.success() with optional
    success_reason parameter.

    Required field:
        action: What the transform did. Use TransformActionCategory values
                for common actions, or custom strings for plugin-specific actions.

    Optional fields:
        fields_modified: List of field names that were changed
        fields_added: List of field names that were added
        fields_removed: List of field names that were removed
        validation_warnings: Non-blocking validation issues (data quality flags)
        metadata: Additional plugin-specific context

    Example usage:
        # Simple action tracking
        TransformResult.success(row, success_reason={"action": "enriched"})

        # Field change tracking
        TransformResult.success(row, success_reason={
            "action": "mapped",
            "fields_modified": ["customer_id", "amount"],
            "fields_added": ["currency_code"],
        })

        # Data quality warning
        TransformResult.success(row, success_reason={
            "action": "validated",
            "validation_warnings": ["amount near threshold (995 of 1000 limit)"],
        })
    """

    action: str  # Use TransformActionCategory or custom string

    # Multi-query success context
    queries_completed: NotRequired[int]  # Number of queries completed in multi-query

    # Field tracking
    fields_modified: NotRequired[list[str]]
    fields_added: NotRequired[list[str]]
    fields_removed: NotRequired[list[str]]

    # Data quality
    validation_warnings: NotRequired[list[str]]

    # Extensibility
    metadata: NotRequired[dict[str, Any]]


# =============================================================================
# Transform Error Reason Types
# =============================================================================


class TemplateErrorEntry(TypedDict):
    """Entry in template_errors list for batch processing failures."""

    row_index: int
    error: str


class RowErrorEntry(TypedDict):
    """Entry in row_errors list for batch processing failures.

    The ``error`` field accepts both simple strings and structured dicts.
    Plugins handling external-API responses may store structured error
    bodies, which are persisted as-is in the audit trail.
    """

    row_index: int
    reason: str
    error: NotRequired[str | dict[str, Any]]


class UsageStats(TypedDict, total=False):
    """LLM token usage statistics.

    Values are ``int | None`` because providers may omit individual fields.
    ``None`` means "provider did not report this value" — distinct from ``0``.
    """

    prompt_tokens: int | None
    completion_tokens: int | None
    total_tokens: int | None


class QueryFailureDetail(TypedDict):
    """Detailed information about a failed query in multi-query transforms.

    Used when transforms need to report more than just the query name.
    """

    query: str
    error: NotRequired[str]
    error_type: NotRequired[str]
    status_code: NotRequired[int]


class ErrorDetail(TypedDict):
    """Detailed information about an error in batch processing.

    Used when more context is needed than a simple error message string.
    """

    message: str
    error_type: NotRequired[str]
    row_index: NotRequired[int]
    details: NotRequired[str]


# Literal type for compile-time validation of error categories
TransformErrorCategory = Literal[
    # API/Network errors
    "api_error",
    "api_call_failed",
    "llm_call_failed",
    "network_error",
    "permanent_error",
    "retry_timeout",
    "transient_error_no_retry",  # Transient error (connection/timeout) but retry disabled
    # Field/validation errors
    "missing_field",
    "missing_scan_field",
    "type_mismatch",
    "validation_failed",
    "invalid_input",
    # Template errors
    "template_rendering_failed",
    "template_context_failed",  # Multi-query template context build failed (missing field)
    "all_templates_failed",
    # JSON/response parsing errors
    "json_parse_failed",
    "invalid_json",  # Generic JSON parse failure
    "invalid_json_response",
    "invalid_json_type",
    "empty_choices",
    "malformed_response",
    "missing_output_field",
    "response_truncated",
    # Batch processing errors
    "batch_error",
    "batch_create_failed",
    "batch_failed",
    "batch_cancelled",
    "batch_expired",
    "batch_timeout",
    "batch_retrieve_failed",
    "file_upload_failed",
    "file_download_failed",
    "all_output_lines_malformed",
    "all_rows_failed",
    "result_not_found",
    "query_failed",
    "multi_query_failed",  # Non-retryable LLM error in multi-query (atomic failure)
    "context_length_exceeded",  # LLM context too long (not retryable — shorten prompt)
    "rate_limited",
    # Content extraction errors (Tier 3 boundary - external HTML/text parsing)
    "content_extraction_failed",
    # Retrieval errors (RAG retrieval transform)
    "retrieval_failed",
    "no_results",
    "no_regex_match",
    # Content filtering
    "blocked_content",
    "content_filtered",
    "content_safety_violation",
    "prompt_injection_detected",
    "unknown_category",  # Unknown category from external API (fail-closed)
    "non_string_field",  # Explicitly-configured field is non-string (security fail-closed)
    # Field type validation (Tier 3 - LLM output value type mismatch)
    "field_type_mismatch",
    # Field collision (output would overwrite input fields)
    "field_collision",
    # Contract violations (schema validation)
    "contract_violation",
    "multiple_contract_violations",
    # Numeric/computation errors
    "float_overflow",  # Arithmetic overflow producing inf (e.g., sum of large floats)
    "non_finite_usage",  # LLM API returned NaN/Infinity in usage metadata
    # Executor lifecycle
    "shutdown_requested",  # Worker stopped mid-retry due to executor shutdown
    "unexpected_pool_error",  # Worker future raised unexpected exception — buffer slot recovered
    # Generic (for tests and edge cases)
    "test_error",
    "property_test_error",
    "simulated_failure",
    "deliberate_failure",
    "intentional_failure",
    # Batch processing
    "empty_batch",
    "all_non_finite",  # All values in batch were NaN/Inf — no real data to aggregate
    # Transport/network (batch processing)
    "transport_exception",  # HTTP transport error during batch retrieval
    # Replication errors
    "invalid_copies",  # Invalid copies value in batch_replicate transform
]


class TransformErrorReason(TypedDict):
    """Reason for transform processing error.

    Used when transforms return TransformResult.error().
    The reason field describes what category of error occurred.
    Additional fields provide context specific to the error type.

    Recorded in the audit trail (Tier 1 - full trust) for legal traceability.
    Every transform error must be attributable to its cause.

    Required field:
        reason: Error category from TransformErrorCategory literal type.
                Compile-time validated to prevent typos.

    Common context fields:
        error: Exception message or detailed error description
        field: Field name for field-related errors
        error_type: Sub-category (e.g., "http_error", "network_error")
        message: Human-readable error message (alternative to error)

    Multi-query/template context:
        query: Which query in multi-query failed
        template_hash: Template version for debugging
        template_errors: List of per-row template failures (batch processing)

    LLM response context:
        max_tokens: Configured max tokens limit
        completion_tokens: Actual tokens used in response
        prompt_tokens: Tokens used in prompt
        raw_response: Truncated raw LLM response content
        raw_response_preview: Alternative name for truncated preview
        content_after_fence_strip: Content after markdown fence removal
        usage: Token usage stats from LLM response
        response: Full response object for debugging
        response_keys: Keys present in response dict
        body_preview: HTTP body preview for errors
        content_type: Content-Type header value

    Type validation context:
        expected: Expected type or value
        actual: Actual type or value received
        actual_type: Actual Python type name for type checks
        value: The actual value (truncated for audit)

    Contract violation context:
        violation_type: Specific ContractViolation subclass name
        original_field: Original field name before normalization
        count: Number of violations (multiple_contract_violations only)
        violations: Per-violation reason entries (multiple_contract_violations only)

    RAG retrieval context:
        provider: Retrieval provider name (e.g., "azure_search", "chroma")
        cause: Sub-cause within an error category (e.g., "null_value", "empty_query")
        pattern: Regex pattern string (for no_regex_match errors)
        skipped_count: Number of candidate hits rejected before final output
        skipped_reasons: Structured reasons for candidate hits rejected before final output

    Rate limiting/timeout context:
        elapsed_seconds: Time elapsed before timeout
        max_seconds: Maximum allowed time
        status_code: HTTP status code

    Content filtering context:
        matched_pattern: Regex pattern that matched
        match_context: Context around the match
        categories: Content safety violation categories

    Batch processing context:
        batch_id: Azure/OpenRouter batch job ID
        operation: Computation step that failed within a batch transform
        queries_completed: Number of queries completed before failure
        row_errors: List of per-row error entries

    Example usage:
        # API error with exception details
        TransformResult.error({
            "reason": "api_error",
            "error": str(e),
            "error_type": "http_error",
        })

        # Field-related error
        TransformResult.error({
            "reason": "missing_field",
            "field": "customer_id",
        })

        # LLM response truncation
        TransformResult.error({
            "reason": "response_truncated",
            "error": "Response was truncated at 1000 tokens",
            "query": "sentiment",
            "max_tokens": 1000,
            "completion_tokens": 1000,
        })
    """

    # REQUIRED - error category (Literal-typed for compile-time validation)
    reason: TransformErrorCategory

    # Common context
    error: NotRequired[str]
    field: NotRequired[str]
    error_type: NotRequired[str]
    message: NotRequired[str]
    url: NotRequired[str]

    # Field collision context
    collisions: NotRequired[list[str]]  # Field names that would be overwritten

    # Multi-query/template context
    query: NotRequired[str]
    query_name: NotRequired[str]  # Named query identifier in multi-query
    query_index: NotRequired[int]  # Position of query in multi-query sequence
    failed_query_name: NotRequired[str]  # Name of query that caused atomic failure
    failed_query_index: NotRequired[int]  # Index of query that caused atomic failure
    discarded_successful_queries: NotRequired[int]  # Successful queries discarded (atomic failure)
    template_hash: NotRequired[str]
    template_file_path: NotRequired[str]  # Path to template file; absent = inline template
    template_errors: NotRequired[list[TemplateErrorEntry]]
    failed_queries: NotRequired[list[str | QueryFailureDetail]]  # Query names or detailed failures
    succeeded_count: NotRequired[int]  # Number of successful queries
    total_count: NotRequired[int]  # Total number of queries attempted

    # LLM response context
    available_fields: NotRequired[list[str]]  # Fields present in LLM JSON response (for missing_output_field)
    content_length: NotRequired[int]  # Length of LLM response content
    max_tokens: NotRequired[int]
    completion_tokens: NotRequired[int | None]
    prompt_tokens: NotRequired[int | None]
    finish_reason: NotRequired[str | None]  # LLM finish reason (e.g., "stop", "length")
    raw_response: NotRequired[str]  # LLM response text; absent = empty/unavailable
    raw_response_preview: NotRequired[str]  # Truncated preview; absent = empty/unavailable
    content_after_fence_strip: NotRequired[str]
    usage: NotRequired[UsageStats | dict[str, int]]
    response: NotRequired[dict[str, Any]]
    response_keys: NotRequired[list[str] | None]
    body_preview: NotRequired[str]  # HTTP body preview; absent = empty/unavailable
    content_type: NotRequired[str]

    # Type validation context
    expected: NotRequired[str]
    actual: NotRequired[str]
    actual_type: NotRequired[str]
    value: NotRequired[str]

    # Contract violation context
    violation_type: NotRequired[str]
    original_field: NotRequired[str]
    count: NotRequired[int]
    violations: NotRequired[list[dict[str, Any]]]

    # Rate limiting/timeout context
    elapsed_seconds: NotRequired[float]
    max_seconds: NotRequired[float]
    elapsed_hours: NotRequired[float]  # Batch timeout (hours scale)
    max_wait_hours: NotRequired[float]  # Batch max wait time (hours)
    status_code: NotRequired[int]

    # RAG retrieval context
    provider: NotRequired[str]  # Retrieval provider name (e.g., "azure_search", "chroma")
    cause: NotRequired[str]  # Sub-cause within error category (e.g., "null_value", "empty_query")
    pattern: NotRequired[str]  # Regex pattern string (for no_regex_match errors)
    skipped_count: NotRequired[int]  # Candidate hits rejected before final output
    skipped_reasons: NotRequired[list[dict[str, Any]]]  # Structured reasons for rejected candidate hits

    # Content filtering context
    matched_pattern: NotRequired[str]
    match_context: NotRequired[str]
    match_position: NotRequired[int]  # Start offset of match in field value
    match_length: NotRequired[int]  # Length of matched substring
    field_length: NotRequired[int]  # Total length of scanned field value
    categories: NotRequired[list[str] | dict[str, dict[str, Any]]]  # List of names OR detailed severity/threshold map
    attacks: NotRequired[dict[str, bool]]  # Prompt shield attack flags (user_prompt_attack, document_attack)

    # Batch processing context
    batch_id: NotRequired[str]
    operation: NotRequired[str]
    batch_size: NotRequired[int]  # Total rows in batch
    group_by: NotRequired[str]  # Grouping field for grouped batch errors
    group_value: NotRequired[Any]  # Group value for grouped batch errors
    valid_count: NotRequired[int]  # Rows that passed validation within batch
    queries_completed: NotRequired[int]
    row_errors: NotRequired[list[RowErrorEntry]]
    output_file_id: NotRequired[str]  # Batch output file reference
    malformed_count: NotRequired[int]  # Count of malformed batch lines
    errors: NotRequired[list[str | ErrorDetail]]  # Error messages or structured errors
    skipped_non_finite: NotRequired[int]  # Count of NaN/Inf values skipped
    skipped_non_finite_indices: NotRequired[list[int]]  # Row indices with non-finite values


class SourceQuarantineReason(TypedDict):
    """Reason for source quarantine routing.

    Used when source validation fails and the row is routed to a quarantine sink
    via a DIVERT edge. The quarantine_error field distinguishes this variant from
    gate and transform reasons.

    Required field:
        quarantine_error: Description of the validation failure that caused quarantine
    """

    quarantine_error: str


class SinkDiversionReason(TypedDict):
    """Reason for sink diversion routing.

    Used when a sink's write() diverts a row to a failsink via a __failsink__
    DIVERT edge. The diversion_reason field distinguishes this variant from
    gate, transform, and quarantine reasons.

    Required field:
        diversion_reason: Description of why the external system rejected the row
    """

    diversion_reason: str


# Discriminated union - field presence distinguishes variants:
# - ConfigGateReason has "condition" and "result"
# - TransformErrorReason has "reason" (error category string)
# - SourceQuarantineReason has "quarantine_error"
# - SinkDiversionReason has "diversion_reason"
RoutingReason = ConfigGateReason | TransformErrorReason | SourceQuarantineReason | SinkDiversionReason


# =============================================================================
# Control Flow Exceptions
# =============================================================================


class MaxRetriesExceeded(Exception):
    """Raised when max retry attempts are exceeded.

    This is a control-flow exception used by RetryManager to signal that
    all retry attempts have been exhausted. FailureInfo uses this to
    construct structured error records for the audit trail.

    Attributes:
        attempts: Number of attempts made before giving up
        last_error: The exception from the final attempt
    """

    def __init__(self, attempts: int, last_error: BaseException) -> None:
        self.attempts = attempts
        self.last_error = last_error
        super().__init__(f"Max retries ({attempts}) exceeded: {last_error}")


# TIER-2: Control-flow signal for interrupted runs (SIGINT/SIGTERM) — run is resumable; not a system corruption or framework bug.
class GracefulShutdownError(Exception):
    """Raised when a pipeline run is interrupted by a shutdown signal.

    This is a CONTROL-FLOW SIGNAL. It indicates the
    orchestrator stopped processing new rows due to SIGINT/SIGTERM but
    completed all in-flight work (aggregation flush, sink writes, checkpoints).

    The run is marked INTERRUPTED and is resumable via ``elspeth resume``.
    """

    def __init__(
        self,
        rows_processed: int,
        run_id: str,
        *,
        rows_succeeded: int = 0,
        rows_failed: int = 0,
        rows_quarantined: int = 0,
        rows_routed_success: int = 0,
        rows_routed_failure: int = 0,
        routed_destinations: dict[str, int] | None = None,
    ) -> None:
        self.rows_processed = rows_processed
        self.run_id = run_id
        self.rows_succeeded = rows_succeeded
        self.rows_failed = rows_failed
        self.rows_quarantined = rows_quarantined
        self.rows_routed_success = rows_routed_success
        self.rows_routed_failure = rows_routed_failure
        self.routed_destinations: Mapping[str, int] = deep_freeze(dict(routed_destinations) if routed_destinations is not None else {})
        super().__init__(
            f"Pipeline interrupted after {rows_processed} rows (run_id={run_id}). Resume with: elspeth resume {run_id} --execute"
        )


@tier_1_error(
    reason="ADR-008: audit trail corruption — permanent OPEN state",
    caller_module=__name__,
)
class AuditIntegrityError(Exception):
    """Raised when audit database operations fail unexpectedly.

    This indicates catastrophic failure: database corruption, transaction
    failure, or bug in Landscape recording code. Per Tier 1 trust model,
    ELSPETH cannot continue with compromised audit integrity.

    Examples of conditions that trigger this:
    - Run record not found after INSERT/UPDATE
    - NodeState not found after completion update
    - Batch record not found after update
    - State marked terminal but still shows OPEN status

    Recovery: These errors are NOT recoverable. The pipeline must stop
    and the database must be investigated for corruption or tampering.
    """

    def __init__(
        self,
        *args: object,
        failed_turn: FailedTurnMetadata | None = None,
    ) -> None:
        super().__init__(*args)
        self.failed_turn = failed_turn


# TIER-2: Config-elected enforcement failure (union collision policy = fail). Not a system corruption; pipeline author chose fail-fast on merge conflicts.
class CoalesceCollisionError(Exception):
    """Raised when union_collision_policy=fail and a field collision occurs.

    This is NOT an engine bug or audit-integrity failure — it's a config-elected
    enforcement. The pipeline author chose to fail-fast on union merge collisions
    rather than allow last_wins/first_wins resolution. The full CoalesceMetadata
    is captured BEFORE raising so the orchestrator's failure path can persist
    the complete collision record (field_origins + collision_values) to the
    audit trail.

    Attributes:
        metadata: CoalesceMetadata with union_field_origins and
                  union_field_collision_values populated.
    """

    def __init__(self, message: str, *, metadata: "CoalesceMetadata") -> None:
        super().__init__(message)
        self.metadata = metadata


@tier_1_error(
    reason="ADR-008: orchestration invariant broken — executor bug",
    caller_module=__name__,
)
class OrchestrationInvariantError(Exception):
    """Raised when orchestration invariants are violated.

    This indicates a bug in the engine: node_id not set before execution,
    routing to non-existent edges, etc. These are programming errors
    that must crash immediately.

    Examples of conditions that trigger this:
    - Transform/Gate/Sink executed without node_id assigned
    - Edge resolution fails due to missing node_id
    - Last transform in pipeline has no node_id

    Recovery: These errors indicate bugs in orchestration code that must
    be fixed. They should never occur in correct operation.
    """

    pass


class DeclaredRequiredInputFieldsPayload(TypedDict):
    """Audit payload for ADR-013 declared-input-field mismatches."""

    declared: Required[list[str]]
    effective_input_fields: Required[list[str]]
    missing: Required[list[str]]


@tier_1_error(
    reason="ADR-013: undeclared transform input dependency corrupts attribution",
    caller_module=__name__,
)
class DeclaredRequiredInputFieldsViolation(DeclarationContractViolation):
    """Raised when a transform runs on input missing its declared fields.

    ``declared_input_fields`` is trusted as the transform's precondition
    surface. If runtime input does not satisfy that declaration, any later
    plugin crash or emitted output would be attributed on false premises.
    """

    payload_schema: ClassVar[type] = DeclaredRequiredInputFieldsPayload


class DeclaredOutputFieldRowViolationPayload(TypedDict):
    """Per-emitted-row evidence for ADR-011 declared-output-fields mismatches."""

    emitted_index: Required[int]
    runtime_observed: Required[list[str]]
    missing: Required[list[str]]


class DeclaredOutputFieldsPayload(TypedDict):
    """Audit payload for ADR-011 declared-output-fields mismatches."""

    declared: Required[list[str]]
    violations: Required[list[DeclaredOutputFieldRowViolationPayload]]


@tier_1_error(
    reason="ADR-011: declared output-field lie corrupts downstream lineage",
    caller_module=__name__,
)
class DeclaredOutputFieldsViolation(DeclarationContractViolation):
    """Raised when a transform emits rows missing declared output fields.

    ``declared_output_fields`` is trusted by DAG/schema propagation. If a
    transform advertises output fields the emitted rows do not actually carry,
    downstream lineage and required-field reasoning become silently wrong.
    This is audit-integrity corruption, not a row-level data error, so the
    violation is Tier 1 and must never be absorbed by ``on_error`` routing.
    """

    payload_schema: ClassVar[type] = DeclaredOutputFieldsPayload


class SourceGuaranteedFieldsPayload(TypedDict):
    """Audit payload for ADR-016 source guaranteed-field mismatches."""

    declared: Required[list[str]]
    runtime_observed: Required[list[str]]
    missing: Required[list[str]]


@tier_1_error(
    reason="ADR-016: source guaranteed-field lie corrupts downstream propagation and audit lineage",
    caller_module=__name__,
)
class SourceGuaranteedFieldsViolation(DeclarationContractViolation):
    """Raised when a source emits a row missing a guaranteed field.

    Source guaranteed fields feed the framework's producer-side propagation
    logic. If a source advertises a stable field that the emitted row does not
    actually carry at runtime, downstream reasoning is built on fabricated
    provenance.
    """

    payload_schema: ClassVar[type] = SourceGuaranteedFieldsPayload


class SinkRequiredFieldsPayload(TypedDict):
    """Audit payload for ADR-017 sink required-field mismatches."""

    declared: Required[list[str]]
    runtime_observed: Required[list[str]]
    missing: Required[list[str]]


@tier_1_error(
    reason="ADR-017: sink required-field contract failure corrupts sink-intent attribution",
    caller_module=__name__,
)
class SinkRequiredFieldsViolation(DeclarationContractViolation):
    """Raised when a row reaches a sink missing declared required fields.

    This is the framework-owned Layer 1 sink boundary contract. It fires
    before schema validation and before sink I/O so attribution stays on the
    sink's declared intent surface rather than collapsing into a generic
    validation or transactional failure.
    """

    payload_schema: ClassVar[type] = SinkRequiredFieldsPayload


class SchemaConfigFieldMetadataMismatch(TypedDict):
    """Per-field metadata drift evidence for ADR-014 schema-mode mismatches."""

    field: Required[str]
    expected_type: Required[str]
    observed_type: Required[str]
    expected_required: Required[bool]
    observed_required: Required[bool]
    expected_nullable: Required[bool]
    observed_nullable: Required[bool]
    observed_present: Required[bool]


class SchemaConfigModePayload(TypedDict):
    """Audit payload for ADR-014 schema-mode/runtime-semantic mismatches."""

    declared_mode: Required[str]
    observed_mode: Required[str]
    declared_locked: Required[bool]
    observed_locked: Required[bool]
    runtime_observed_fields: NotRequired[list[str]]
    missing_required_fields: NotRequired[list[str]]
    undeclared_extra_fields: NotRequired[list[str]]
    field_metadata_mismatches: NotRequired[list[SchemaConfigFieldMetadataMismatch]]


@tier_1_error(
    reason="ADR-014: emitted runtime schema semantics diverge from declared config mode",
    caller_module=__name__,
)
class SchemaConfigModeViolation(DeclarationContractViolation):
    """Raised when emitted row contracts disagree with declared schema mode.

    ``_output_schema_config`` is the transform's runtime declaration surface for
    schema semantics. If emitted contracts advertise a different mode/lock
    posture, or a FIXED schema leaks undeclared output fields, auditors query a
    fabricated contract view rather than the transform's declared one.
    """

    payload_schema: ClassVar[type] = SchemaConfigModePayload


class UnexpectedEmptyEmissionPayload(TypedDict):
    """Audit payload for ADR-012 unexpected empty-emission mismatches."""

    passes_through_input: Required[bool]
    can_drop_rows: Required[bool]
    emitted_count: Required[int]


# TIER-2: Plugin declaration mismatch — row-level failure is fully auditable and does not imply framework or audit-record corruption.
class UnexpectedEmptyEmissionViolation(DeclarationContractViolation):
    """Raised when a pass-through transform emits zero rows without opting in.

    Tier 2 by design. The row-level terminal state remains auditable and the
    failure reflects a plugin declaration bug, not a corruption of Tier-1
    framework state.
    """

    payload_schema: ClassVar[type] = UnexpectedEmptyEmissionPayload


# TIER-2: Plugin retry signal — transient operational failure eligible for RetryManager retry, not a system corruption or framework bug.
class PluginRetryableError(Exception):
    """Base for plugin exceptions eligible for engine retry.

    All plugin error types that may be retried by the engine's RetryManager
    must inherit from this class. The processor catches PluginRetryableError
    and dispatches to retry logic based on the retryable attribute.

    Attributes:
        retryable: Whether the error is transient and should be retried.
        status_code: HTTP status code if applicable (for audit context).
    """

    def __init__(self, message: str, *, retryable: bool, status_code: int | None = None) -> None:
        super().__init__(message)
        self.retryable = retryable
        self.status_code = status_code


# TIER-2: Runtime provider readiness failed before row processing; the failure is auditable plugin/runtime evidence, not Tier-1 framework corruption.
class RuntimePreflightFailedError(AuditEvidenceBase, Exception):
    """Provider/runtime readiness check failed before row processing."""

    error_class = "pre_flight_failed"

    def __init__(
        self,
        *,
        plugin_name: str,
        provider: str,
        cause: BaseException,
    ) -> None:
        self.plugin_name = plugin_name
        self.provider = provider
        self.cause_type = type(cause).__name__
        message = (
            f"{self.error_class}: {plugin_name} provider {provider} failed runtime preflight "
            f"before row processing: {self.cause_type}: {cause}"
        )
        super().__init__(message)

    def to_audit_dict(self) -> dict[str, Any]:
        """Canonical structured payload for audit/event consumers."""
        return {
            "error_class": self.error_class,
            "plugin_name": self.plugin_name,
            "provider": self.provider,
            "cause_type": self.cause_type,
            "message": str(self),
        }


# TIER-2: Plugin contract violation — plugin bug (row-level failure). Recording FAILED state is accurate; not system corruption. Base class excluded from TIER_1_ERRORS (ADR-008).
class PluginContractViolation(AuditEvidenceBase, RuntimeError):
    """Raised when a plugin violates its contract with the framework.

    This indicates a bug in a plugin (Source, Transform, Gate, Sink) that must
    be fixed. Unlike user data errors (which are quarantined), plugin bugs
    MUST crash the pipeline per CLAUDE.md's "plugin bugs must crash" rule.

    Examples of conditions that trigger this:
    - Transform emits non-canonical data (NaN, Infinity, non-serializable types)
    - Plugin returns wrong type from method
    - Plugin violates interface contract

    Recovery: Fix the plugin. These errors indicate bugs in plugin code.

    Base class accepts a positional message, matching RuntimeError. Subclasses
    (e.g., PassThroughContractViolation) add structured fields and override
    to_audit_dict() to contribute them to the audit trail via
    ExecutionError.context.
    """

    def to_audit_dict(self) -> dict[str, Any]:
        """Canonical audit-recording payload for ExecutionError.context.

        Base implementation is message-only. Subclasses should override to
        surface structured fields. Return value must be JSON-serializable —
        the Landscape records it through canonical JSON serialization.
        """
        return {"exception_type": type(self).__name__, "message": str(self)}


# TIER-2: Plugin success-empty misuse — row-level contract bug remains fully auditable and does not imply Tier-1 framework or audit-record corruption.
class ZeroEmissionSuccessContractViolation(PluginContractViolation, AuditEvidenceBase):
    """Raised when ``success_empty()`` is used outside the filter declaration path.

    Tier 2 by design. The engine can still record a row-level FAILED outcome,
    so this is a plugin contract bug rather than Tier-1 audit corruption.
    """

    def __init__(
        self,
        *,
        transform: str,
        transform_node_id: str,
        run_id: str,
        row_id: str,
        token_id: str,
        passes_through_input: bool,
        can_drop_rows: bool,
        emitted_count: int,
        message: str,
    ) -> None:
        super().__init__(message)
        self.transform = transform
        self.transform_node_id = transform_node_id
        self.run_id = run_id
        self.row_id = row_id
        self.token_id = token_id
        self.passes_through_input = passes_through_input
        self.can_drop_rows = can_drop_rows
        self.emitted_count = emitted_count

    def to_audit_dict(self) -> dict[str, Any]:
        return {
            "exception_type": "ZeroEmissionSuccessContractViolation",
            "message": str(self),
            "transform": self.transform,
            "transform_node_id": self.transform_node_id,
            "run_id": self.run_id,
            "row_id": self.row_id,
            "token_id": self.token_id,
            "passes_through_input": self.passes_through_input,
            "can_drop_rows": self.can_drop_rows,
            "emitted_count": self.emitted_count,
        }


@tier_1_error(
    reason=(
        "ADR-010 §H2 landing scope F3: sink transactional-boundary invariant "
        "distinct from pre-write VAL contract — must crash, never absorbed by on_error."
    ),
    caller_module=__name__,
)
class SinkTransactionalInvariantError(PluginContractViolation):
    """Raised by a sink's inline commit-boundary check when state diverges
    between contract evaluation and the transactional write.

    **Two-layer sink invariant architecture** (ADR-010 §H2 landing scope F3):

    - **Layer 1 — pre-write declaration contract** (currently
      ``SinkRequiredFieldsContract``): dispatcher-owned, fires BEFORE
      ``sink.write()``, raises a ``DeclarationContractViolation`` subclass
      with a ``payload_schema``. Triage SQL:
      ``WHERE exception_type = '<SubclassName>Violation'``.

    - **Layer 2 — transactional backstop, THIS class**: catches the rare
      case where row state diverges between contract evaluation and commit
      (e.g. cross-token mutation during batch assembly, or a required field
      deleted by a transformation layered between contract and commit that
      the contract could not see). Raises ``SinkTransactionalInvariantError``
      from the inline sink check. Triage SQL:
      ``WHERE exception_type = 'SinkTransactionalInvariantError'``.

    Before the F3 split (pre-ADR-010 §Semantics amendment, 2026-04-20) both layers
    raised ``PluginContractViolation`` and the audit table conflated pre-
    write contract failures with commit-boundary failures — the auditor
    could not distinguish "intent validation failed" from "state diverged
    mid-transaction" without reading the message text. The F3 amendment
    separates the triage surfaces by exception class.

    Inherits ``PluginContractViolation`` (not ``DeclarationContractViolation``)
    because the backstop is NOT dispatcher-owned. It is an inline offensive
    assertion at the sink's own commit boundary — plugin-level guarding,
    not framework-level contract dispatch.

    Registered in TIER_1_ERRORS via ``@tier_1_error`` so ``on_error``
    routing cannot absorb it; the orchestrator must propagate.
    """


@tier_1_error(
    reason="ADR-008: pass-through annotation lie corrupts batch audit fields",
    caller_module=__name__,
)
class PassThroughContractViolation(PluginContractViolation):
    """Raised by TransformExecutor when a passes_through_input=True transform
    drops input fields from its emitted row(s).

    This is a framework contract violation, not a row-level data error — hence
    registration in ``TIER_1_ERRORS``. It cannot be silenced by on_error
    routing or generic ``except Exception`` handlers. A mis-annotation is
    evidence tampering: the static validator was told the transform emits a
    superset of input, and runtime observed otherwise; routing past that
    divergence would corrupt the audit trail.

    Attributes:
        transform: Name of the offending transform class.
        transform_node_id: DAG node identifier.
        run_id: Pipeline run identifier for audit correlation.
        row_id: Source row identifier.
        token_id: DAG token identifier (post-fork/join lineage).
        static_contract: Fields the static validator computed for output.
        runtime_observed: Fields the emitted row actually exposes at runtime —
            the intersection of ``emitted_row.contract.fields`` and
            ``emitted_row.keys()``. ``PipelineRow`` treats contract and
            payload as independent references, so a field is only "kept"
            if it appears in both.
        divergence_set: ``input_fields - runtime_observed`` (the dropped fields).
    """

    def __init__(
        self,
        *,
        transform: str,
        transform_node_id: str,
        run_id: str,
        row_id: str,
        token_id: str,
        static_contract: frozenset[str],
        runtime_observed: frozenset[str],
        divergence_set: frozenset[str],
        message: str,
    ) -> None:
        super().__init__(message)
        self.transform = transform
        self.transform_node_id = transform_node_id
        self.run_id = run_id
        self.row_id = row_id
        self.token_id = token_id
        self.static_contract = static_contract
        self.runtime_observed = runtime_observed
        self.divergence_set = divergence_set

    def to_audit_dict(self) -> dict[str, Any]:
        """Return 9-key structured payload for ExecutionError.context.

        frozenset fields are sorted into lists for canonical JSON
        determinism — the Landscape serializer requires stable ordering.
        """
        return {
            "exception_type": "PassThroughContractViolation",
            "message": str(self),
            "transform": self.transform,
            "transform_node_id": self.transform_node_id,
            "run_id": self.run_id,
            "row_id": self.row_id,
            "token_id": self.token_id,
            "static_contract": sorted(self.static_contract),
            "runtime_observed": sorted(self.runtime_observed),
            "divergence_set": sorted(self.divergence_set),
        }


# =============================================================================
# Tier 1 Guard Tuple — Live View (ADR-010 §Decision 2)
# =============================================================================
# TIER_1_ERRORS is materialized from the tier_registry on each attribute
# access. This is a MODULE __getattr__ (PEP 562) — NOT a from-import
# re-export. The v0 plan used `from tier_registry import TIER_1_ERRORS` which
# captured a snapshot at errors.py import time; late registrations never
# reached callers doing `from elspeth.contracts.errors import TIER_1_ERRORS`.
# The live view closes reviewer finding B8.
#
# Usage: `except TIER_1_ERRORS: raise` before any `except Exception:` block.
#
# PluginContractViolation (the base class) is intentionally excluded: it
# represents a plugin bug (row-level failure), not system corruption.
# Recording FAILED state for a PluginContractViolation is accurate;
# recording it for the base members is misleading.
#
# PassThroughContractViolation IS included (ADR-008) because a pass-through-
# annotation lie is a framework contract violation (the static validator was
# told the transform emits a superset of input; runtime observed otherwise),
# not a row-level data error. Audit integrity demands it crash, not be
# absorbed by on_error routing.
# =============================================================================


if TYPE_CHECKING:
    # Declare the type of TIER_1_ERRORS for mypy / static tools.
    # At runtime the name is resolved via __getattr__ (PEP 562) below;
    # at type-check time this declaration wins, giving callers the right type.
    TIER_1_ERRORS: tuple[type[Exception], ...]


def __getattr__(name: str) -> tuple[type[Exception], ...]:
    if name == "TIER_1_ERRORS":
        from elspeth.contracts.tier_registry import _TIER_1_ERRORS_VIEW

        # Materialise a fresh tuple on every access so callers can use it in
        # ``except`` clauses (which require a tuple, not a custom view) while
        # still seeing any registrations that occurred after import time.
        # This is distinct from the _Tier1ErrorsView live-view object in
        # tier_registry (which supports membership tests and iteration but is
        # NOT a tuple).
        return tuple(_TIER_1_ERRORS_VIEW)  # type: ignore[arg-type]  # _Tier1ErrorsView yields BaseException subclasses; Exception is a subtype
    raise AttributeError(name)


# =============================================================================
# Schema Contract Violation Types (Tier 3 - External Data)
# =============================================================================
# These exceptions represent validation failures on external/user data.
# They result in row quarantine, NOT crashes. Per CLAUDE.md Three-Tier Trust Model,
# Tier 3 data (external) can be "literal trash" and must be handled gracefully.
#
# Error messages follow "'original' (normalized)" format for debuggability:
#   "Required field 'Customer ID' (customer_id) is missing"
# This shows both what the user sees (original) and what code uses (normalized).
# =============================================================================


# TIER-2: Tier-3 data validation base — external data error resulting in row quarantine, not system corruption.
class ContractViolation(Exception):
    """Base exception for schema contract violations.

    All schema contract violations track both the normalized field name
    (used internally by code) and the original field name (from user's
    perspective, e.g., CSV column headers).

    Attributes:
        normalized_name: Internal field name used by code (e.g., "customer_id")
        original_name: Original field name from external data (e.g., "Customer ID")
    """

    def __init__(self, *, normalized_name: str, original_name: str) -> None:
        """Initialize ContractViolation.

        Args:
            normalized_name: Internal field name used by code
            original_name: Original field name from external data
        """
        self.normalized_name = normalized_name
        self.original_name = original_name
        super().__init__(self._format_message())

    def _format_message(self) -> str:
        """Format the error message. Override in subclasses for specific messages."""
        return f"Contract violation on field '{self.original_name}' ({self.normalized_name})"

    def to_error_reason(self) -> dict[str, Any]:
        """Convert violation to TransformErrorReason dict.

        Returns:
            Dict with 'reason' key suitable for TransformResult.error().
            The violation_type is derived from the class name.
        """
        return {
            "reason": "contract_violation",
            "violation_type": self.__class__.__name__,
            "field": self.normalized_name,
            "original_field": self.original_name,
        }


# TIER-2: Tier-3 data validation — required field absent in external data; results in row quarantine.
class MissingFieldViolation(ContractViolation):
    """Raised when a required field is missing from the data.

    This is a Tier 3 data violation - the external data is missing a field
    that the schema declares as required. Results in row quarantine.

    Example:
        >>> raise MissingFieldViolation(normalized_name="customer_id", original_name="Customer ID")
        MissingFieldViolation: Required field 'Customer ID' (customer_id) is missing
    """

    def _format_message(self) -> str:
        """Format message showing required field is missing."""
        return f"Required field '{self.original_name}' ({self.normalized_name}) is missing"


# TIER-2: Tier-3 data validation — external data type mismatch; results in row quarantine.
class TypeMismatchViolation(ContractViolation):
    """Raised when a field value has the wrong type.

    This is a Tier 3 data violation - the external data has a value that
    doesn't match the expected type. Results in row quarantine.

    Attributes:
        expected_type: The type that was expected (e.g., int, str)
        actual_type: The type that was received (e.g., str, float)
        actual_value: The actual value received (for debugging)

    Note:
        The `actual_value` attribute is accessible programmatically for debugging
        purposes, but is intentionally excluded from the error message (str/repr).
        This prevents potentially sensitive user data from being exposed in logs,
        audit trails, or error reports. Callers needing the value for investigation
        should access the attribute directly.

    Example:
        >>> raise TypeMismatchViolation(
        ...     normalized_name="amount",
        ...     original_name="Amount",
        ...     expected_type=int,
        ...     actual_type=str,
        ...     actual_value="not_a_number"
        ... )
        TypeMismatchViolation: Field 'Amount' (amount) expected type 'int', got 'str'
    """

    def __init__(
        self,
        *,
        normalized_name: str,
        original_name: str,
        expected_type: type,
        actual_type: type,
        actual_value: Any,
    ) -> None:
        """Initialize TypeMismatchViolation.

        Args:
            normalized_name: Internal field name used by code
            original_name: Original field name from external data
            expected_type: The type that was expected
            actual_type: The type that was received
            actual_value: The actual value received
        """
        self.expected_type = expected_type
        self.actual_type = actual_type
        self.actual_value = actual_value
        super().__init__(normalized_name=normalized_name, original_name=original_name)

    def _format_message(self) -> str:
        """Format message showing type mismatch."""
        return f"Field '{self.original_name}' ({self.normalized_name}) expected type '{self.expected_type.__name__}', got '{self.actual_type.__name__}'"

    def to_error_reason(self) -> dict[str, Any]:
        """Convert violation to TransformErrorReason dict with type details.

        Returns:
            Dict with 'reason' key and type-specific fields suitable for
            TransformResult.error(). Includes expected and actual type names
            only. Raw values are intentionally excluded to prevent sensitive
            or unbounded data from reaching the audit trail.
        """
        base = super().to_error_reason()
        base.update(
            {
                "expected": self.expected_type.__name__,
                "actual": self.actual_type.__name__,
            }
        )
        return base


# TIER-2: Tier-3 data validation — unexpected field in FIXED schema mode; results in row quarantine.
class ExtraFieldViolation(ContractViolation):
    """Raised when an unexpected field is present in FIXED schema mode.

    This is a Tier 3 data violation - the external data contains a field
    that is not declared in the schema, and the schema is in FIXED mode
    (no extra fields allowed). Results in row quarantine.

    Example:
        >>> raise ExtraFieldViolation(normalized_name="unknown_col", original_name="Unknown Col")
        ExtraFieldViolation: Extra field 'Unknown Col' (unknown_col) not allowed in FIXED mode
    """

    def _format_message(self) -> str:
        """Format message showing extra field not allowed."""
        return f"Extra field '{self.original_name}' ({self.normalized_name}) not allowed in FIXED mode"


# TIER-2: Configuration error — fork/join schema type conflict (pipeline design issue), not an external data or system error.
class ContractMergeError(ValueError):
    """Raised when schema contracts cannot be merged due to type conflicts.

    This occurs during fork/join (coalesce) operations when parallel paths
    produce incompatible types for the same field. This is a configuration
    error (pipeline design issue), not a data error.

    Inherits from ValueError because it represents an invalid combination
    of schema contracts, not an external data issue.

    Attributes:
        field: The field name with conflicting types
        type_a: The type from one path
        type_b: The type from another path

    Example:
        >>> raise ContractMergeError(field="amount", type_a="int", type_b="str")
        ContractMergeError: Cannot merge contracts: field 'amount' has conflicting types 'int' and 'str'
    """

    def __init__(self, *, field: str, type_a: str, type_b: str) -> None:
        """Initialize ContractMergeError.

        Args:
            field: The field name with conflicting types
            type_a: The type from one path
            type_b: The type from another path
        """
        self.field = field
        self.type_a = type_a
        self.type_b = type_b
        super().__init__(f"Cannot merge contracts: field '{field}' has conflicting types '{type_a}' and '{type_b}'")


# =============================================================================
# Contract Violation to Error Conversion Helpers
# =============================================================================


def violations_to_error_reason(violations: list[ContractViolation]) -> dict[str, Any]:
    """Convert list of violations to TransformErrorReason.

    Provides a convenient way to convert one or more contract violations
    into a dict suitable for TransformResult.error().

    Args:
        violations: List of ContractViolation instances

    Returns:
        Single violation: its to_error_reason() directly
        Multiple violations: wrapped dict with count and list

    Raises:
        ValueError: If violations list is empty

    Example:
        >>> violations = [
        ...     MissingFieldViolation(normalized_name="id", original_name="ID"),
        ...     TypeMismatchViolation(
        ...         normalized_name="amount",
        ...         original_name="Amount",
        ...         expected_type=int,
        ...         actual_type=str,
        ...         actual_value="bad"
        ...     ),
        ... ]
        >>> reason = violations_to_error_reason(violations)
        >>> reason["reason"]
        'multiple_contract_violations'
        >>> reason["count"]
        2
    """
    if not violations:
        raise ValueError("violations list cannot be empty")

    if len(violations) == 1:
        return violations[0].to_error_reason()

    return {
        "reason": "multiple_contract_violations",
        "count": len(violations),
        "violations": [v.to_error_reason() for v in violations],
    }


# =============================================================================
# RAG Ingestion Pipeline Errors
# =============================================================================


# TIER-2: Pipeline dependency failure signal — upstream dependency did not complete; pipeline cannot proceed, but this is not a framework/audit corruption.
class DependencyFailedError(Exception):
    """A pipeline dependency failed to complete successfully."""

    def __init__(self, *, dependency_name: str, run_id: str, reason: str) -> None:
        if not dependency_name:
            raise ValueError("dependency_name must not be empty")
        if not run_id:
            raise ValueError("run_id must not be empty")
        if not reason:
            raise ValueError("reason must not be empty")
        self.dependency_name = dependency_name
        self.run_id = run_id
        self.reason = reason
        super().__init__(f"Dependency '{dependency_name}' failed (run_id={run_id}): {reason}")


# TIER-2: Commencement gate failure signal — config-driven pre-flight check rejected the run; not a framework bug or audit corruption.
class CommencementGateFailedError(Exception):
    """A commencement gate evaluated to falsy or raised an error."""

    def __init__(
        self,
        *,
        gate_name: str,
        condition: str,
        reason: str,
        context_snapshot: Mapping[str, Any],
    ) -> None:
        if not gate_name:
            raise ValueError("gate_name must not be empty")
        if not condition:
            raise ValueError("condition must not be empty")
        if not reason:
            raise ValueError("reason must not be empty")
        self.gate_name = gate_name
        self.condition = condition
        self.reason = reason
        self.context_snapshot: Mapping[str, Any] = deep_freeze(context_snapshot)
        super().__init__(f"Commencement gate '{gate_name}' failed: {reason} (condition: {condition})")


# TIER-2: Retrieval readiness signal — collection unavailable at run time; operational failure, not audit corruption or framework bug.
class RetrievalNotReadyError(Exception):
    """A retrieval provider's collection is empty or unreachable."""

    def __init__(self, *, collection: str, reason: str) -> None:
        if not collection:
            raise ValueError("collection must not be empty")
        if not reason:
            raise ValueError("reason must not be empty")
        self.collection = collection
        self.reason = reason
        super().__init__(f"Collection {collection!r} not ready: {reason}")


# TIER-2: Sink duplicate-write rejection — on_duplicate='error' policy enforcement; not a framework bug or audit corruption.
class DuplicateDocumentError(Exception):
    """Sink rejected a write because document IDs already exist in the collection.

    Raised when on_duplicate='error' and pre-existing IDs are detected.
    """

    def __init__(self, *, collection: str, duplicate_ids: list[str]) -> None:
        if not collection:
            raise ValueError("collection must not be empty")
        if not duplicate_ids:
            raise ValueError("duplicate_ids must not be empty — DuplicateDocumentError requires at least one duplicate")
        self.collection = collection
        self.duplicate_ids = tuple(duplicate_ids)
        super().__init__(f"Duplicate document IDs in collection {collection!r}: {list(self.duplicate_ids)}")


# Capacity error classification for pooled API transforms.
#
# Capacity errors are transient overload conditions that should be retried
# with AIMD throttling. They are distinct from "normal" errors (auth failures,
# malformed requests) which use standard retry limits.
#
# HTTP Status Codes:
# - 429: Too Many Requests (universal)
# - 503: Service Unavailable (universal)
# - 529: Overloaded (Azure, some other providers)
#
# Lives in contracts/ (L0) so engine retry classification can import it without
# crossing the L2→L3 layer boundary; pooled transforms in plugins/ raise it.

CAPACITY_ERROR_CODES: frozenset[int] = frozenset({429, 503, 529})


def is_capacity_error(status_code: int) -> bool:
    """Check if HTTP status code indicates a capacity error.

    Capacity errors are transient overload conditions that should trigger
    AIMD throttle backoff and be retried with increasing delays.
    """
    return status_code in CAPACITY_ERROR_CODES


# TIER-2: pooled-API capacity/rate-limit signal — transient overload, retried until budget exhausted.
class CapacityError(Exception):
    """Exception for capacity/rate limit errors.

    Raised when an API call fails due to capacity limits.
    These errors trigger AIMD throttle and are retried until
    max_capacity_retry_seconds is exceeded.

    Attributes:
        status_code: HTTP status code that triggered this error
        retryable: Always True for capacity errors
    """

    def __init__(self, status_code: int, message: str) -> None:
        if not (100 <= status_code <= 599):
            raise ValueError(f"CapacityError.status_code must be a valid HTTP status (100-599), got {status_code}")
        super().__init__(message)
        self.status_code = status_code
        self.retryable = True


# TIER-2: telemetry-subsystem configuration/initialization failure.
# Lives in contracts/ so the engine can reference it without crossing the
# L2→L3 layer boundary; the telemetry package re-exports it for its
# internal subsystem consumers.
# TIER-2: telemetry exporter configuration/initialization failure, not audit-integrity corruption.
class TelemetryExporterError(Exception):
    """Raised when an exporter encounters a configuration or initialization error.

    This is raised during exporter setup (configure/initialization), NOT during
    export operations. Export operations must not raise — they log errors instead.

    Attributes:
        exporter_name: Name of the exporter that failed
        message: Human-readable error description
    """

    def __init__(self, exporter_name: str, message: str) -> None:
        self.exporter_name = exporter_name
        self.message = message
        super().__init__(f"Exporter '{exporter_name}' failed: {message}")
