"""Phase-based protocol interfaces for PluginContext decomposition.

These protocols define what each plugin category actually needs from the
execution context. The concrete PluginContext class (in plugin_context.py)
satisfies all four protocols.

Protocol design rationale (D2 revision):
    The original D2 decision proposed field-category grouping
    (IdentityContext / AuditContext / ExecutionContext). Three independent
    code analyses mapped every field access across all 42 plugin files and
    found this doesn't match actual usage — most complex plugins need fields
    from all three categories. The revised split groups by consumer role.

Engine-internal methods not in any protocol:
    record_transform_error() — called by engine executors/processor, not by plugins. [R4]

See: docs/plans/2026-02-26-t17-plugincontext-protocol-split-design.md
"""

from __future__ import annotations

import threading
from collections.abc import Callable
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

if TYPE_CHECKING:
    from elspeth.contracts import Call, CallStatus, CallType
    from elspeth.contracts.audit_protocols import PluginAuditWriter
    from elspeth.contracts.config.runtime import RuntimeConcurrencyConfig
    from elspeth.contracts.identity import TokenInfo
    from elspeth.contracts.payload_store import PayloadStore
    from elspeth.contracts.plugin_context import ValidationErrorToken
    from elspeth.contracts.schema_contract import SchemaContract
    from elspeth.core.rate_limit import RateLimitRegistry


@runtime_checkable
class SourceContext(Protocol):
    """What source plugins need during load().

    Sources operate at the Tier 3 boundary (external data). They need:
    - Identity: run_id, node_id for audit attribution
    - Recording: record_validation_error() for quarantined rows,
      record_call() for external API calls (e.g., Azure Blob download)
    - Telemetry: telemetry_emit for operational visibility
    """

    @property
    def run_id(self) -> str: ...

    @property
    def node_id(self) -> str | None: ...

    @property
    def operation_id(self) -> str | None: ...

    @property
    def landscape(self) -> PluginAuditWriter | None: ...

    @property
    def telemetry_emit(self) -> Callable[[Any], None]: ...

    def record_validation_error(
        self,
        row: Any,
        error: str,
        schema_mode: str,
        destination: str,
        *,
        contract_violation: Any | None = None,
    ) -> ValidationErrorToken: ...

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
    ) -> Call | None: ...


@runtime_checkable
class TransformContext(Protocol):
    """What transform plugins need during process()/accept().

    Transforms operate on Tier 2 pipeline data. They need:
    - Per-row identity: state_id, token, batch_token_ids for audit attribution
    - Schema: contract for field resolution
    - Recording: record_call() for external API calls (LLM, HTTP)
    - Checkpoint: get/set/clear_checkpoint for crash recovery (batch transforms)
    """

    @property
    def run_id(self) -> str: ...

    @property
    def state_id(self) -> str | None: ...

    @property
    def node_id(self) -> str | None: ...

    @property
    def token(self) -> TokenInfo | None: ...

    @property
    def batch_token_ids(self) -> tuple[str, ...] | None: ...

    @property
    def contract(self) -> SchemaContract | None: ...

    @property
    def shutdown_event(self) -> threading.Event | None: ...

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
    ) -> Call | None: ...


@runtime_checkable
class SinkContext(Protocol):
    """What sink plugins need during write().

    Sinks output Tier 2 pipeline data. They need:
    - Identity: run_id for landscape queries
    - Schema: contract for display header resolution
    - Audit: landscape for field resolution queries
    - Recording: record_call() for external calls (SQL, blob upload)
    """

    @property
    def run_id(self) -> str: ...

    @property
    def contract(self) -> SchemaContract | None: ...

    @property
    def landscape(self) -> PluginAuditWriter | None: ...

    @property
    def operation_id(self) -> str | None: ...

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
    ) -> Call | None: ...


@runtime_checkable
class LifecycleContext(Protocol):
    """What plugins need during on_start()/on_complete().

    Lifecycle hooks capture infrastructure references that persist for the
    run. Complex transforms store these as instance variables in on_start()
    and use them throughout processing.

    This protocol is wider than the per-row protocols because lifecycle
    hooks need access to infrastructure (rate limiters, concurrency config)
    that per-row processing doesn't need directly.
    """

    @property
    def run_id(self) -> str: ...

    @property
    def node_id(self) -> str | None: ...  # [R1] Set by orchestrator before on_start()

    @property
    def operation_id(self) -> str | None: ...

    @property
    def landscape(self) -> PluginAuditWriter | None: ...

    @property
    def payload_store(self) -> PayloadStore | None: ...

    @property
    def rate_limit_registry(self) -> RateLimitRegistry | None: ...

    @property
    def telemetry_emit(self) -> Callable[[Any], None]: ...

    @property
    def concurrency_config(self) -> RuntimeConcurrencyConfig | None: ...

    @property
    def shutdown_event(self) -> threading.Event | None: ...
