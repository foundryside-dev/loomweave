"""TypedDict definitions for Landscape export records.

Each TypedDict defines the exact shape of one record type yielded by
``LandscapeExporter._iter_records()``. Replaces ``dict[str, Any]``
so that mypy can verify field names and types at construction sites.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict


class RunExportRecord(TypedDict):
    record_type: Literal["run"]
    run_id: str
    status: str
    started_at: str | None
    completed_at: str | None
    canonical_version: str
    config_hash: str
    initiated_by_user_id: str | None
    auth_provider_type: str | None
    settings: Any  # Resolved config — structure varies by pipeline
    reproducibility_grade: str | None


class SecretResolutionExportRecord(TypedDict):
    record_type: Literal["secret_resolution"]
    run_id: str
    resolution_id: str
    timestamp: float  # Epoch seconds — not ISO-formatted, stored raw
    env_var_name: str
    source: str
    vault_url: str | None
    secret_name: str | None
    fingerprint: str | None
    resolution_latency_ms: float | None


class NodeExportRecord(TypedDict):
    record_type: Literal["node"]
    run_id: str
    node_id: str
    plugin_name: str
    node_type: str
    plugin_version: str | None
    source_file_hash: str | None
    determinism: str
    config_hash: str
    config: Any  # Resolved config — structure varies by plugin
    schema_hash: str | None
    schema_mode: str | None
    schema_fields: list[dict[str, object]] | None
    sequence_in_pipeline: int | None
    registered_at: str


class EdgeExportRecord(TypedDict):
    record_type: Literal["edge"]
    run_id: str
    edge_id: str
    from_node_id: str
    to_node_id: str
    label: str | None
    default_mode: str
    created_at: str


class OperationExportRecord(TypedDict):
    record_type: Literal["operation"]
    run_id: str
    operation_id: str
    node_id: str
    operation_type: str
    status: str
    started_at: str | None
    completed_at: str | None
    duration_ms: float | None
    error_message: str | None
    input_data_ref: str | None
    input_data_hash: str | None
    output_data_ref: str | None
    output_data_hash: str | None


class ValidationErrorExportRecord(TypedDict):
    record_type: Literal["validation_error"]
    run_id: str
    error_id: str
    node_id: str | None
    row_id: str | None
    row_hash: str
    row_data_json: str | None
    error: str
    schema_mode: str
    destination: str
    created_at: str
    violation_type: str | None
    original_field_name: str | None
    normalized_field_name: str | None
    expected_type: str | None
    actual_type: str | None


class TransformErrorExportRecord(TypedDict):
    record_type: Literal["transform_error"]
    run_id: str
    error_id: str
    token_id: str
    transform_id: str
    row_hash: str
    row_data_json: str | None
    error_details_json: str | None
    destination: str
    created_at: str


class CallExportRecord(TypedDict):
    """External call record — parented by either a node_state or an operation.

    Exactly one of ``state_id`` and ``operation_id`` is non-None per record.
    """

    record_type: Literal["call"]
    run_id: str
    call_id: str
    state_id: str | None
    operation_id: str | None
    call_index: int
    call_type: str
    status: str
    request_hash: str | None
    response_hash: str | None
    latency_ms: float | None
    request_ref: str | None
    response_ref: str | None
    error_json: str | None
    created_at: str | None


class RowExportRecord(TypedDict):
    record_type: Literal["row"]
    run_id: str
    row_id: str
    row_index: int
    source_node_id: str
    source_data_hash: str | None
    source_data_ref: str | None
    created_at: str


class TokenExportRecord(TypedDict):
    record_type: Literal["token"]
    run_id: str
    token_id: str
    row_id: str
    step_in_pipeline: int | None
    branch_name: str | None
    fork_group_id: str | None
    join_group_id: str | None
    expand_group_id: str | None
    created_at: str


class TokenParentExportRecord(TypedDict):
    record_type: Literal["token_parent"]
    run_id: str
    token_id: str
    parent_token_id: str
    ordinal: int


class TokenOutcomeExportRecord(TypedDict):
    record_type: Literal["token_outcome"]
    run_id: str
    outcome_id: str
    token_id: str
    outcome: str | None
    path: str
    completed: bool
    recorded_at: str
    sink_name: str | None
    batch_id: str | None
    fork_group_id: str | None
    join_group_id: str | None
    expand_group_id: str | None
    error_hash: str | None
    context_json: str | None
    expected_branches_json: str | None


class NodeStateExportRecord(TypedDict):
    """Processing record for a token passing through a node.

    The underlying NodeState type has four variants (Open, Pending, Completed,
    Failed). This export record flattens them into a single shape; fields that
    only apply to certain variants are None for others.
    """

    record_type: Literal["node_state"]
    run_id: str
    state_id: str
    token_id: str
    node_id: str
    step_index: int
    attempt: int
    status: str
    input_hash: str | None
    output_hash: str | None
    duration_ms: float | None
    started_at: str
    completed_at: str | None
    context_before_json: str | None
    context_after_json: str | None
    error_json: str | None
    success_reason_json: str | None


class RoutingEventExportRecord(TypedDict):
    record_type: Literal["routing_event"]
    run_id: str
    event_id: str
    state_id: str
    edge_id: str | None
    routing_group_id: str | None
    ordinal: int
    mode: str
    reason_hash: str | None
    reason_ref: str | None
    created_at: str | None


class BatchExportRecord(TypedDict):
    record_type: Literal["batch"]
    run_id: str
    batch_id: str
    aggregation_node_id: str
    attempt: int
    status: str
    trigger_type: str | None
    trigger_reason: str | None
    created_at: str | None
    completed_at: str | None


class BatchMemberExportRecord(TypedDict):
    record_type: Literal["batch_member"]
    run_id: str
    batch_id: str
    token_id: str
    ordinal: int


class ArtifactExportRecord(TypedDict):
    record_type: Literal["artifact"]
    run_id: str
    artifact_id: str
    sink_node_id: str
    produced_by_state_id: str | None
    artifact_type: str
    path_or_uri: str | None
    content_hash: str | None
    size_bytes: int | None
    idempotency_key: str | None
    created_at: str


ExportRecord = (
    RunExportRecord
    | SecretResolutionExportRecord
    | NodeExportRecord
    | EdgeExportRecord
    | OperationExportRecord
    | ValidationErrorExportRecord
    | TransformErrorExportRecord
    | CallExportRecord
    | RowExportRecord
    | TokenExportRecord
    | TokenParentExportRecord
    | TokenOutcomeExportRecord
    | NodeStateExportRecord
    | RoutingEventExportRecord
    | BatchExportRecord
    | BatchMemberExportRecord
    | ArtifactExportRecord
)
