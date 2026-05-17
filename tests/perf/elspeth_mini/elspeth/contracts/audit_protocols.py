"""Cross-layer audit protocols for plugin-facing audit operations.

These protocols define the interface that plugins use to record audit events.
They live in L0 (contracts/) so plugins (L3) can depend on them without
importing L1 (core/) at runtime.

Method-count budget: Do not exceed 20 methods.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

from elspeth.contracts import (
    Call,
    CallStatus,
    CallType,
    NodeState,
    RoutingEvent,
    RoutingMode,
    RoutingReason,
    RoutingSpec,
)
from elspeth.contracts.audit import TokenRef
from elspeth.contracts.call_data import CallPayload
from elspeth.contracts.errors import ContractViolation, TransformErrorReason
from elspeth.contracts.schema_contract import SchemaContract

if TYPE_CHECKING:
    from elspeth.contracts.schema_contract import PipelineRow


class CallRecorder(Protocol):
    """Narrow protocol for clients that record external calls.

    AuditedClientBase and its subclasses (HTTP, LLM) only need these two
    methods. This protocol is satisfied by both ExecutionRepository (direct
    injection) and PluginAuditWriter (via adapter), eliminating the need
    for type: ignore[arg-type] at plugin call sites.
    """

    def allocate_call_index(self, state_id: str) -> int: ...

    def allocate_operation_call_index(self, operation_id: str) -> int: ...

    def record_call(
        self,
        state_id: str,
        call_index: int,
        call_type: CallType,
        status: CallStatus,
        request_data: CallPayload,
        response_data: CallPayload | None = None,
        error: CallPayload | None = None,
        latency_ms: float | None = None,
        *,
        request_ref: str | None = None,
        response_ref: str | None = None,
    ) -> Call: ...

    def record_operation_call(
        self,
        operation_id: str,
        call_type: CallType,
        status: CallStatus,
        request_data: CallPayload,
        response_data: CallPayload | None = None,
        error: CallPayload | None = None,
        latency_ms: float | None = None,
        *,
        call_index: int | None = None,
        request_ref: str | None = None,
        response_ref: str | None = None,
    ) -> Call: ...


@runtime_checkable
class PluginAuditWriter(Protocol):
    """Protocol for plugin-facing audit recording operations.

    Composes methods from ExecutionRepository, DataFlowRepository, and
    RunLifecycleRepository into a single interface that plugins can depend
    on without importing core/.

    Method-count budget: Do not exceed 20 methods.
    """

    # ── ExecutionRepository methods ──────────────────────────────────────

    def allocate_call_index(self, state_id: str) -> int: ...

    def allocate_operation_call_index(self, operation_id: str) -> int: ...

    def record_call(
        self,
        state_id: str,
        call_index: int,
        call_type: CallType,
        status: CallStatus,
        request_data: CallPayload,
        response_data: CallPayload | None = None,
        error: CallPayload | None = None,
        latency_ms: float | None = None,
        *,
        request_ref: str | None = None,
        response_ref: str | None = None,
    ) -> Call: ...

    def record_operation_call(
        self,
        operation_id: str,
        call_type: CallType,
        status: CallStatus,
        request_data: CallPayload,
        response_data: CallPayload | None = None,
        error: CallPayload | None = None,
        latency_ms: float | None = None,
        *,
        call_index: int | None = None,
        request_ref: str | None = None,
        response_ref: str | None = None,
    ) -> Call: ...

    def get_node_state(self, state_id: str) -> NodeState | None: ...

    def record_routing_event(
        self,
        state_id: str,
        edge_id: str,
        mode: RoutingMode,
        reason: RoutingReason | None = None,
        *,
        event_id: str | None = None,
        routing_group_id: str | None = None,
        ordinal: int = 0,
        reason_ref: str | None = None,
    ) -> RoutingEvent: ...

    def record_routing_events(
        self,
        state_id: str,
        routes: list[RoutingSpec],
        reason: RoutingReason | None = None,
    ) -> list[RoutingEvent]: ...

    # ── DataFlowRepository methods ───────────────────────────────────────

    def record_validation_error(
        self,
        run_id: str,
        node_id: str | None,
        row_data: Any,
        error: str,
        schema_mode: str,
        destination: str,
        *,
        contract_violation: ContractViolation | None = None,
    ) -> str: ...

    def record_transform_error(
        self,
        ref: TokenRef,
        transform_id: str,
        row_data: Mapping[str, object] | PipelineRow,
        error_details: TransformErrorReason,
        destination: str,
    ) -> str: ...

    def update_node_output_contract(
        self,
        run_id: str,
        node_id: str,
        contract: SchemaContract,
    ) -> None: ...

    def get_node_contracts(
        self,
        run_id: str,
        node_id: str,
        *,
        allow_missing: bool = False,
    ) -> tuple[SchemaContract | None, SchemaContract | None]: ...

    # ── RunLifecycleRepository methods ───────────────────────────────────

    def get_source_field_resolution(self, run_id: str) -> dict[str, str] | None: ...

    def record_readiness_check(
        self,
        run_id: str,
        *,
        name: str,
        collection: str,
        reachable: bool,
        count: int | None,
        message: str,
    ) -> None: ...
