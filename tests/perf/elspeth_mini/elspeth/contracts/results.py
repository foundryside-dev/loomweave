"""Operation outcomes and results.

These types answer: "What did an operation produce?"

IMPORTANT:
- TransformResult.status uses Literal["success", "error"], NOT an enum
- TransformResult and GateResult KEEP audit fields (input_hash, output_hash, duration_ms)
- ArtifactDescriptor matches architecture schema (artifact_type, content_hash REQUIRED, size_bytes REQUIRED)
- FailureInfo provides type-safe error details for RowResult
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass, field
from types import MappingProxyType
from typing import TYPE_CHECKING, Any, Literal

from elspeth.contracts.freeze import freeze_fields
from elspeth.contracts.url import SanitizedDatabaseUrl, SanitizedWebhookUrl

if TYPE_CHECKING:
    from elspeth.contracts.errors import MaxRetriesExceeded
    from elspeth.contracts.node_state_context import NodeStateContext
    from elspeth.contracts.schema_contract import PipelineRow, SchemaContract

from elspeth.contracts.enums import _LEGAL_TERMINAL_PAIRS, TerminalOutcome, TerminalPath
from elspeth.contracts.errors import (
    FrameworkBugError,
    OrchestrationInvariantError,
    PluginContractViolation,
    TransformErrorReason,
    TransformSuccessReason,
)
from elspeth.contracts.identity import TokenInfo
from elspeth.contracts.routing import RoutingAction


def _require_pipeline_row(value: object, *, location: str) -> PipelineRow:
    """Reject non-PipelineRow output before it can masquerade as valid data."""
    from elspeth.contracts.schema_contract import PipelineRow

    if not isinstance(value, PipelineRow):
        raise PluginContractViolation(
            f"{location} must be a PipelineRow, got {type(value).__name__}. "
            "Build transform output with PipelineRow(data, contract) before returning it."
        )
    return value


@dataclass(frozen=True, slots=True)
class ExceptionResult:
    """Wrapper for exceptions that should propagate through async pattern.

    When a worker thread encounters an uncaught exception (plugin bug),
    it wraps the exception in this container. The waiter then re-raises
    the original exception in the orchestrator thread, ensuring plugin
    bugs crash the pipeline as intended.

    Frozen: exception wrappers are immutable evidence — the captured
    exception and traceback must not be modified after construction.

    Used by:
    - engine/batch_adapter.py: Wraps exceptions in worker threads
    - plugins/batching/mixin.py: Creates ExceptionResult on worker failure
    - plugins/batching/ports.py: Type hint in BatchOutputPort protocol
    """

    exception: BaseException
    traceback: str


@dataclass(frozen=True, slots=True)
class FailureInfo:
    """Type-safe error details for RowResult.

    Captures structured failure information for FAILED outcomes.
    Use factory methods for common error types.

    Frozen: failure info is immutable evidence captured at the point of
    failure. Modifying it after construction would compromise audit integrity.

    Fields:
        exception_type: The exception class name (required)
        message: Human-readable error message (required)
        attempts: Number of retry attempts (optional, for retry failures)
        last_error: The underlying error message (optional)
    """

    exception_type: str
    message: str
    attempts: int | None = None
    last_error: str | None = None

    @classmethod
    def from_max_retries_exceeded(cls, e: MaxRetriesExceeded) -> FailureInfo:
        """Create FailureInfo from MaxRetriesExceeded exception.

        Args:
            e: The MaxRetriesExceeded exception

        Returns:
            FailureInfo with all retry details
        """
        return cls(
            exception_type="MaxRetriesExceeded",
            message=str(e),
            attempts=e.attempts,
            last_error=str(e.last_error),
        )


@dataclass
class TransformResult:
    """Result of a transform operation.

    Use the factory methods to create instances.

    IMPORTANT: status uses Literal["success", "error"], NOT enum, per architecture.
    Audit fields (input_hash, output_hash, duration_ms) are populated by executors.

    Multi-row output:
    - Single-row: success(row) sets row=row, rows=None
    - Multi-row: success_multi(rows) sets row=None, rows=rows
    - Empty success: success_empty() sets row=None, rows=()
    - Use is_multi_row property to distinguish
    - Use has_output_data property to check if ANY explicit output carrier exists
    """

    status: Literal["success", "error"]
    row: PipelineRow | None
    reason: TransformErrorReason | None
    retryable: bool = False
    rows: tuple[PipelineRow, ...] | None = None

    # Success metadata - REQUIRED for success results, None for error results
    # Invariant: status="success" implies success_reason is not None
    success_reason: TransformSuccessReason | None = None

    # Audit fields - set by executor, not by plugin
    input_hash: str | None = field(default=None, repr=False)
    output_hash: str | None = field(default=None, repr=False)
    duration_ms: float | None = field(default=None, repr=False)

    # Context snapshot for audit trail (optional)
    # Contains operational metadata like pool stats, ordering info
    # Enables pool metadata to flow to context_after_json
    context_after: NodeStateContext | None = field(default=None, repr=False)

    def __post_init__(self) -> None:
        """Validate invariants - success and error results MUST satisfy their contracts."""
        if self.status == "success" and self.success_reason is None:
            raise ValueError(
                "TransformResult with status='success' MUST provide success_reason. "
                "Use TransformResult.success(row, success_reason={'action': '...'}) "
                "to create success results. Missing success_reason is a plugin bug."
            )
        if self.status == "success" and self.row is None and self.rows is None:
            raise ValueError(
                "TransformResult with status='success' MUST have output data (row or rows). "
                "Use TransformResult.success(row, ...) or TransformResult.success_multi(rows, ...) "
                "to create success results. Missing output data is a plugin bug."
            )
        if self.status == "success" and self.row is not None and self.rows is not None:
            raise ValueError(
                "TransformResult with status='success' MUST provide exactly one of row or rows, not both. "
                "Use TransformResult.success(row, ...) for single-row or "
                "TransformResult.success_multi(rows, ...) for multi-row output."
            )
        if self.status == "success" and self.row is not None:
            _require_pipeline_row(self.row, location="TransformResult.row")
        if self.status == "success" and self.rows is not None:
            for i, row in enumerate(self.rows):
                _require_pipeline_row(row, location=f"TransformResult.rows[{i}]")
        if self.status == "error":
            if self.reason is None:
                raise ValueError(
                    "TransformResult with status='error' MUST provide reason. "
                    "Use TransformResult.error({'reason': '...'}) to create error results. "
                    "Missing reason is a plugin bug."
                )
            if "reason" not in self.reason:
                raise ValueError(
                    "TransformResult with status='error' MUST include reason['reason']. "
                    "Use TransformResult.error({'reason': '...'}) to create error results. "
                    "Missing reason['reason'] is a plugin bug."
                )
            if self.row is not None or self.rows is not None:
                raise ValueError(
                    "TransformResult with status='error' MUST NOT include output data (row or rows). "
                    "Error results carry reason only, not data. This is a plugin bug."
                )
            if self.success_reason is not None:
                raise ValueError(
                    "TransformResult with status='error' MUST NOT include success_reason. "
                    "Error results carry reason only. This is a plugin bug."
                )

    @property
    def is_multi_row(self) -> bool:
        """True if this result contains multiple output rows."""
        return self.rows is not None

    @property
    def has_output_data(self) -> bool:
        """True if this result has any explicit output carrier (row or rows)."""
        return self.row is not None or self.rows is not None

    @classmethod
    def success(
        cls,
        row: PipelineRow,
        *,
        success_reason: TransformSuccessReason,
        context_after: NodeStateContext | None = None,
    ) -> TransformResult:
        """Create successful result with single output row.

        Args:
            row: The transformed row data as a PipelineRow wrapping
                 the output dict with its schema contract.
            success_reason: REQUIRED metadata about what the transform did.
                           Must include at least 'action' field.
                           See TransformSuccessReason for available fields.
            context_after: Optional operational metadata for audit trail
                          (e.g., pool stats, ordering info).

        Returns:
            TransformResult with status="success" and the provided row.

        Example:
            return TransformResult.success(
                PipelineRow(output_dict, contract),
                success_reason={"action": "processed", "fields_modified": ["amount"]}
            )
        """
        row = _require_pipeline_row(row, location="TransformResult.success(row)")
        return cls(
            status="success",
            row=row,
            reason=None,
            rows=None,
            success_reason=success_reason,
            context_after=context_after,
        )

    @classmethod
    def success_multi(
        cls,
        rows: Sequence[PipelineRow],
        *,
        success_reason: TransformSuccessReason,
        context_after: NodeStateContext | None = None,
    ) -> TransformResult:
        """Create successful result with multiple output rows.

        Args:
            rows: List of PipelineRow instances (must not be empty).
            success_reason: REQUIRED metadata about what the transform did.
                           Must include at least 'action' field.
                           See TransformSuccessReason for available fields.
            context_after: Optional operational metadata for audit trail
                          (e.g., pool stats, ordering info).

        Returns:
            TransformResult with status="success", row=None, rows=rows

        Raises:
            ValueError: If rows is empty

        Example:
            return TransformResult.success_multi(
                [PipelineRow(r, contract) for r in output_rows],
                success_reason={"action": "split", "fields_added": ["row_index"]}
            )
        """
        output_rows = tuple(rows)
        if not output_rows:
            raise ValueError("success_multi requires at least one row")
        for i, row in enumerate(output_rows):
            _require_pipeline_row(row, location=f"TransformResult.success_multi(rows[{i}])")
        # All rows must share the same contract identity. Mixed contracts
        # would silently mislabel child tokens, corrupting downstream
        # contract-based validation. Transforms are system-owned code,
        # so mixed contracts = plugin bug.
        first_contract = output_rows[0].contract
        for i in range(1, len(output_rows)):
            if output_rows[i].contract is not first_contract:
                raise PluginContractViolation(
                    f"success_multi() received rows with inconsistent contracts: "
                    f"row 0 has {first_contract.mode if first_contract else None} contract "
                    f"with {len(first_contract.fields) if first_contract else 0} fields, "
                    f"but row {i} has {output_rows[i].contract.mode if output_rows[i].contract else None} contract "
                    f"with {len(output_rows[i].contract.fields) if output_rows[i].contract else 0} fields. "
                    f"All rows in a multi-row result must share the same contract instance."
                )
        return cls(
            status="success",
            row=None,
            reason=None,
            rows=output_rows,
            success_reason=success_reason,
            context_after=context_after,
        )

    @classmethod
    def success_empty(
        cls,
        *,
        success_reason: TransformSuccessReason,
        context_after: NodeStateContext | None = None,
    ) -> TransformResult:
        """Create successful result with explicit zero-row emission.

        This is distinct from ``success_multi([])`` which remains invalid.
        ``success_empty()`` is the auditable "the transform intentionally
        emitted nothing" shape used by the engine's zero-emission pathways.
        """
        return cls(
            status="success",
            row=None,
            reason=None,
            rows=(),
            success_reason=success_reason,
            context_after=context_after,
        )

    @classmethod
    def error(
        cls,
        reason: TransformErrorReason,
        *,
        retryable: bool = False,
        context_after: NodeStateContext | None = None,
    ) -> TransformResult:
        """Create error result with structured reason.

        Args:
            reason: Error details with required 'reason' field from
                    TransformErrorCategory (compile-time validated).
                    See TransformErrorReason for all available context fields.
            retryable: Whether the error is transient and should be retried.
            context_after: Optional operational metadata for audit trail
                          (e.g., pool stats from partial execution).

        Returns:
            TransformResult with status="error" and the provided reason.
            Error results never carry contracts (contract=None).
        """
        return cls(
            status="error",
            row=None,
            reason=reason,
            retryable=retryable,
            rows=None,
            context_after=context_after,
        )


@dataclass
class GateResult:
    """Result of a config-driven gate evaluation.

    Contains the (possibly modified) row and routing action.
    Constructed entirely by GateExecutor — gates are config-driven,
    not plugin-based.
    """

    row: dict[str, Any]
    action: RoutingAction

    # Schema contract for output (optional)
    # Enables conversion to PipelineRow via to_pipeline_row()
    contract: SchemaContract | None = field(default=None, repr=False)

    # Audit fields - set by GateExecutor
    input_hash: str | None = field(default=None, repr=False)
    output_hash: str | None = field(default=None, repr=False)
    duration_ms: float | None = field(default=None, repr=False)

    def to_pipeline_row(self) -> PipelineRow:
        """Convert to PipelineRow for downstream processing.

        Returns:
            PipelineRow wrapping row data with contract

        Raises:
            ValueError: If contract is None
        """
        from elspeth.contracts.schema_contract import PipelineRow

        if self.contract is None:
            raise FrameworkBugError(
                "GateResult has no contract - cannot create PipelineRow. "
                "The engine must set contract on GateResult before calling to_pipeline_row()."
            )
        return PipelineRow(self.row, self.contract)


@dataclass(frozen=True, slots=True)
class RowResult:
    """Final result of processing a row through the pipeline (ADR-019 two-axis).

    Frozen to prevent post-construction mutation of outcome/sink_name,
    which would bypass __post_init__ invariant checks.

    Fields:
        token: Token identity for this row instance
        final_data: Final row data as PipelineRow (may be original if failed early)
        outcome: Lifecycle answer (None for non-terminal BUFFERED rows)
        path: Provenance answer (always populated)
        sink_name: For paths that reach a sink, the destination sink name
        error: For ON_ERROR_ROUTED, type-safe error details for audit
    """

    token: TokenInfo
    final_data: PipelineRow
    outcome: TerminalOutcome | None
    path: TerminalPath
    sink_name: str | None = None
    error: FailureInfo | None = None

    def __post_init__(self) -> None:
        if self.outcome is not None and (self.outcome, self.path) not in _LEGAL_TERMINAL_PAIRS:
            raise OrchestrationInvariantError(f"RowResult: illegal (outcome, path) pair: ({self.outcome!r}, {self.path!r})")
        if self.outcome is None and self.path != TerminalPath.BUFFERED:
            raise OrchestrationInvariantError(f"RowResult: outcome=None requires path=BUFFERED, got path={self.path!r}")
        if self.outcome is not None and self.path == TerminalPath.BUFFERED:
            raise OrchestrationInvariantError(f"RowResult: path=BUFFERED requires outcome=None, got outcome={self.outcome!r}")
        if self.outcome is None and self.path == TerminalPath.BUFFERED and self.sink_name is not None:
            raise OrchestrationInvariantError("RowResult: BUFFERED rows must not set sink_name before terminal recording")

        if self.path == TerminalPath.DEFAULT_FLOW and self.sink_name is None:
            raise OrchestrationInvariantError("(SUCCESS, DEFAULT_FLOW) outcome requires sink_name to be set")
        if self.path == TerminalPath.GATE_ROUTED and self.sink_name is None:
            raise OrchestrationInvariantError("(SUCCESS, GATE_ROUTED) outcome requires sink_name to be set")
        if self.path == TerminalPath.ON_ERROR_ROUTED:
            if self.sink_name is None:
                raise OrchestrationInvariantError("(FAILURE, ON_ERROR_ROUTED) outcome requires sink_name to be set")
            if self.error is None:
                raise OrchestrationInvariantError(
                    "(FAILURE, ON_ERROR_ROUTED) outcome requires error (FailureInfo) to be set — "
                    "the originating transform error must be captured on the outcome "
                    "record for single-hop audit attributability."
                )
            if not isinstance(self.error, FailureInfo):
                raise OrchestrationInvariantError("(FAILURE, ON_ERROR_ROUTED) outcome requires error to be a FailureInfo instance")
        if self.path == TerminalPath.COALESCED and self.sink_name is None:
            raise OrchestrationInvariantError("(SUCCESS, COALESCED) outcome requires sink_name to be set")


@dataclass(frozen=True, slots=True)
class ArtifactDescriptor:
    """Descriptor for an artifact written by a sink.

    Matches architecture artifacts table schema:
    - artifact_type: NOT NULL (matches DB column name)
    - content_hash: NOT NULL (REQUIRED for audit integrity)
    - size_bytes: NOT NULL (REQUIRED for verification)

    Factory methods provide convenient construction for each artifact type.
    """

    artifact_type: Literal["file", "database", "webhook"]
    path_or_uri: str
    content_hash: str  # REQUIRED - audit integrity
    size_bytes: int  # REQUIRED - verification
    metadata: MappingProxyType[str, object] | None = None

    def __post_init__(self) -> None:
        freeze_fields(self, "metadata")

    @classmethod
    def for_file(
        cls,
        path: str,
        content_hash: str,
        size_bytes: int,
    ) -> ArtifactDescriptor:
        """Create descriptor for file-based artifacts."""
        return cls(
            artifact_type="file",
            path_or_uri=f"file://{path}",
            content_hash=content_hash,
            size_bytes=size_bytes,
        )

    @classmethod
    def for_database(
        cls,
        url: SanitizedDatabaseUrl,
        table: str,
        content_hash: str,
        payload_size: int,
        row_count: int,
    ) -> ArtifactDescriptor:
        """Create descriptor for database artifacts.

        URL must be pre-sanitized using SanitizedDatabaseUrl.from_raw_url().
        This ensures credentials are never stored in the audit trail.
        """
        # Type safety: enforce SanitizedDatabaseUrl, not duck-typed objects
        if not isinstance(url, SanitizedDatabaseUrl):
            raise TypeError(
                "url must be a SanitizedDatabaseUrl instance. Use SanitizedDatabaseUrl.from_raw_url(url) to sanitize raw database URLs."
            )

        metadata: dict[str, object] = {"table": table, "row_count": row_count}
        if url.fingerprint:
            metadata["url_fingerprint"] = url.fingerprint

        return cls(
            artifact_type="database",
            path_or_uri=f"db://{table}@{url.sanitized_url}",
            content_hash=content_hash,
            size_bytes=payload_size,
            metadata=MappingProxyType(metadata),
        )

    @classmethod
    def for_webhook(
        cls,
        url: SanitizedWebhookUrl,
        content_hash: str,
        request_size: int,
        response_code: int,
    ) -> ArtifactDescriptor:
        """Create descriptor for webhook artifacts.

        URL must be pre-sanitized using SanitizedWebhookUrl.from_raw_url().
        This ensures tokens are never stored in the audit trail.
        """
        # Type safety: enforce SanitizedWebhookUrl, not duck-typed objects
        if not isinstance(url, SanitizedWebhookUrl):
            raise TypeError(
                "url must be a SanitizedWebhookUrl instance. Use SanitizedWebhookUrl.from_raw_url(url) to sanitize raw webhook URLs."
            )

        metadata: dict[str, object] = {"response_code": response_code}
        if url.fingerprint:
            metadata["url_fingerprint"] = url.fingerprint

        return cls(
            artifact_type="webhook",
            path_or_uri=f"webhook://{url.sanitized_url}",
            content_hash=content_hash,
            size_bytes=request_size,
            metadata=MappingProxyType(metadata),
        )


@dataclass(frozen=True, slots=True)
class SourceRow:
    """Result from source loading - either valid data or quarantined invalid data.

    ALL rows from sources MUST be wrapped in SourceRow:
    - Valid rows: SourceRow.valid(row_dict, contract=contract)
    - Invalid rows: SourceRow.quarantined(row_data, error, destination)

    This makes source outcomes first-class engine concepts:
    - All rows get proper token_id for lineage
    - Metrics include both valid and quarantine counts
    - Audit trail shows complete source output
    - Quarantine sinks receive invalid data for investigation

    Example usage in a source:
        try:
            validated = schema.model_validate(row)
            yield SourceRow.valid(validated.to_row(), contract=contract)
        except ValidationError as e:
            if on_validation_failure != "discard":
                yield SourceRow.quarantined(
                    row=row,
                    error=str(e),
                    destination=on_validation_failure,
                )
            # else: don't yield, row is intentionally discarded
    """

    # Note: row is Any (not dict) because quarantined rows from external data
    # may not be dicts (e.g., JSON arrays containing primitives like numbers).
    # Valid rows are always dicts (they passed schema validation).
    row: Any
    is_quarantined: bool
    quarantine_error: str | None = None
    quarantine_destination: str | None = None
    contract: SchemaContract | None = None

    def __post_init__(self) -> None:
        """Validate quarantine field invariants.

        Quarantined rows MUST have error and destination (where to route them).
        Non-quarantined rows MUST NOT have quarantine fields set (prevents
        accidental misuse where quarantine metadata is silently ignored).
        """
        if self.is_quarantined:
            if self.quarantine_error is None:
                raise ValueError("Quarantined SourceRow must have quarantine_error")
            if self.quarantine_destination is None:
                raise ValueError("Quarantined SourceRow must have quarantine_destination")
        else:
            if self.quarantine_error is not None:
                raise ValueError(f"Non-quarantined SourceRow must not have quarantine_error, got: {self.quarantine_error!r}")
            if self.quarantine_destination is not None:
                raise ValueError(f"Non-quarantined SourceRow must not have quarantine_destination, got: {self.quarantine_destination!r}")
            # Valid rows MUST have a contract — the engine requires it at
            # tokenization. Catching it here prevents a misleading crash later.
            if self.contract is None:
                raise ValueError("Valid SourceRow must have a contract. Pass contract= to SourceRow.valid().")

    @classmethod
    def valid(
        cls,
        row: dict[str, Any],
        *,
        contract: SchemaContract,
    ) -> SourceRow:
        """Create a valid source row.

        Args:
            row: Validated row data
            contract: Schema contract for the row

        Returns:
            SourceRow with is_quarantined=False
        """
        return cls(row=row, is_quarantined=False, contract=contract)

    @classmethod
    def quarantined(
        cls,
        row: Any,
        error: str,
        destination: str,
    ) -> SourceRow:
        """Create a quarantined row result.

        Args:
            row: The original row data (before validation). May be non-dict
                 for malformed external data (e.g., JSON primitives).
            error: The validation error message
            destination: The sink name to route this row to
        """
        return cls(
            row=row,
            is_quarantined=True,
            quarantine_error=error,
            quarantine_destination=destination,
            contract=None,  # Quarantined rows don't have contracts
        )

    def to_pipeline_row(self) -> PipelineRow:
        """Convert to PipelineRow for processing.

        Returns:
            PipelineRow wrapping row data with contract

        Raises:
            ValueError: If row is quarantined
            FrameworkBugError: If contract is None (engine must set it)
        """
        from elspeth.contracts.schema_contract import PipelineRow

        if self.is_quarantined:
            raise ValueError("Cannot convert quarantined row to PipelineRow")
        if self.contract is None:
            raise FrameworkBugError(
                "SourceRow has no contract — cannot create PipelineRow. "
                "The engine must set contract on SourceRow before calling to_pipeline_row()."
            )

        return PipelineRow(self.row, self.contract)
