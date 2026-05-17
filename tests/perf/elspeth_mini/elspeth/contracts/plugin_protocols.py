"""Plugin protocols — structural contracts for Source, Transform, Sink plugins.

These protocols define what methods plugins must implement.
They're primarily used for type checking (that's pluggy's job for runtime
enforcement), with one exception: TransformProtocol is @runtime_checkable
because the engine uses isinstance() to discriminate transforms from gates
and coalesce nodes during DAG traversal.

Plugin Types:
- Source: Loads data into the system (one per run)
- Transform: Processes rows (stateless)
- Sink: Outputs data (one or more per run)
"""

from collections.abc import Iterator, Mapping
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

from elspeth.contracts.enums import Determinism
from elspeth.contracts.header_modes import HeaderMode
from elspeth.contracts.schema import SchemaConfig

if TYPE_CHECKING:
    from elspeth.contracts.contexts import LifecycleContext, SinkContext, SourceContext, TransformContext
    from elspeth.contracts.data import PluginSchema
    from elspeth.contracts.diversion import SinkWriteResult
    from elspeth.contracts.results import SourceRow, TransformResult
    from elspeth.contracts.schema_contract import PipelineRow, SchemaContract
    from elspeth.contracts.sink import OutputValidationResult


class PluginConfigProtocol(Protocol):
    """Minimal type shape for plugin config classes.

    Captures only the interface that callers of get_config_model() actually
    use: from_dict() for validation and model_json_schema() for schema
    generation.  Defined here in L0/contracts to avoid a structural
    dependency from contracts → plugins/infrastructure/config_base.
    """

    @classmethod
    def from_dict(
        cls,
        config: object,
        *,
        plugin_name: str | None = None,
    ) -> Any:
        """Create config from dict with validation."""
        ...

    @classmethod
    def model_json_schema(cls) -> dict[str, Any]:
        """Return JSON Schema for this config model."""
        ...


class SourceProtocol(Protocol):
    """Protocol for source plugins — type-checking only, not @runtime_checkable.

    Sources load data into the system. There is exactly one source per run.

    Lifecycle (main thread, called by orchestrator):
        1. __init__(config)   -- plugin instantiation
        2. on_start(ctx)      -- per-run init (optional). If raises, pipeline
                                 aborts; on_complete/close are NOT called.
        3. load(ctx)          -- yields SourceRow instances
        4. on_complete(ctx)   -- source exhausted or pipeline errored (optional).
                                 Called even on crash. Runs before close().
        5. close()            -- release resources. Called even on crash.

    on_complete/close run inside a finally block and are individually
    protected (one plugin's failure does not prevent others from cleaning up).
    During resume runs, source lifecycle is skipped entirely (NullSource used).

    Example:
        class CSVSource:
            name = "csv"
            output_schema = RowSchema

            def load(self, ctx: SourceContext) -> Iterator[SourceRow]:
                with open(self.path) as f:
                    reader = csv.DictReader(f)
                    for row in reader:
                        yield SourceRow.valid(row, contract=contract)
    """

    name: str
    output_schema: type["PluginSchema"]
    node_id: str | None  # Set by orchestrator after registration
    config: dict[str, Any]  # Configuration dict stored by all plugins

    # Audit metadata
    determinism: Determinism
    plugin_version: str
    source_file_hash: str | None

    # Sink name for quarantined rows, or "discard" to drop invalid rows
    # All sources must set this - config-based sources get it from SourceDataConfig
    _on_validation_failure: str

    # Success routing: sink name for rows that pass source validation
    # All sources must set this - config-based sources get it from SourceDataConfig
    on_success: str

    # Producer-guarantee declaration surface (ADR-016).
    # Set from the source's effective SchemaConfig guarantees at construction.
    declared_guaranteed_fields: frozenset[str]

    # Lifecycle guards (set by BaseSource.on_start()/on_complete()).
    # All sources must inherit BaseSource which manages these. Contract tests
    # use these flags as falsifiable post-conditions for lifecycle invocation
    # (a stub override that forgets super() flips the flag, breaking the test).
    _on_start_called: bool
    _on_complete_called: bool

    def __init__(self, config: dict[str, Any]) -> None:
        """Initialize with configuration."""
        ...

    def load(self, ctx: "SourceContext") -> Iterator["SourceRow"]:
        """Load and yield rows from the source.

        Args:
            ctx: Source context with run metadata and recording methods

        Yields:
            SourceRow for each row - either SourceRow.valid() for valid rows
            or SourceRow.quarantined() for invalid rows.
        """
        ...

    def close(self) -> None:
        """Release resources (file handles, connections).

        Called after on_complete(), inside a finally block. Guaranteed if
        on_start() succeeded, even on pipeline crash. Skipped during resume.
        """
        ...

    # === Lifecycle Hooks ===

    def on_start(self, ctx: "LifecycleContext") -> None:
        """Called once before load(). If raises, pipeline aborts; on_complete/close skipped."""
        ...

    def on_complete(self, ctx: "LifecycleContext") -> None:
        """Called after load() completes or on error, before close(). Individually protected."""
        ...

    # === Audit Trail Metadata ===

    def get_field_resolution(self) -> tuple[Mapping[str, str], str | None] | None:
        """Return field resolution mapping computed during load().

        Sources that perform field normalization should override this to return
        the mapping from original header names to final field names.

        Returns:
            Tuple of (resolution_mapping, normalization_version) or None if no
            normalization occurred.
        """
        ...

    # === Schema Contract Support ===

    def get_schema_contract(self) -> Any:
        """Return schema contract for this source.

        Sources with schema validation should override this to return their
        SchemaContract. For OBSERVED/FLEXIBLE modes, the contract is typically
        locked after the first row is processed (type inference happens).

        Returns:
            SchemaContract if available, None otherwise. Using Any return type
            to avoid circular import with contracts module in Protocol definition.
        """
        ...

    @classmethod
    def get_config_model(cls, config: dict[str, Any] | None = None) -> type[PluginConfigProtocol] | None:
        """Return the Pydantic config model for this plugin type.

        Returns None for sources with no config (e.g. NullSource).
        Override for dynamic dispatch based on config contents.
        """
        ...

    @classmethod
    def get_config_schema(cls) -> dict[str, Any]:
        """Return the full JSON Schema advertising this plugin's configuration.

        Contract
        --------
        The catalog flattens this schema at discovery time, before any
        runtime config has been selected, so the return value MUST
        describe the plugin's **complete** configuration surface —
        every field a caller could legally supply under every
        runtime-selected branch.

        Single-model plugins inherit a default (on
        :class:`~elspeth.plugins.infrastructure.base.BaseSource`) that
        renders ``cls.config_model.model_json_schema()`` (or ``{}`` when
        ``config_model`` is None, e.g. NullSource).

        Plugins whose *effective* configuration is a discriminated union
        — even if ``config_model`` is set to a base class used as a
        dispatch anchor, as LLMTransform does — **MUST** override this
        to emit ``oneOf`` + ``$defs`` over every branch. Rendering the
        anchor alone publishes a contract missing every variant-specific
        required field, which is exactly the contract this API exists to
        prevent.

        This docstring is the canonical contract cross-referenced from
        the Transform, BatchTransform, and Sink Protocol variants and
        from the three base-class default implementations.
        """
        ...


@runtime_checkable
class TransformProtocol(Protocol):
    """Protocol for stateless single-row transforms.

    Transforms process individual rows and emit results.

    For batch-aware transforms (is_batch_aware=True), use BatchTransformProtocol instead.
    The engine uses is_batch_aware to decide whether to buffer rows and call the batch protocol.

    Lifecycle (main thread, called by orchestrator):
        1. __init__(config)   -- plugin instantiation
        2. on_start(ctx)      -- per-run init (optional). If raises, pipeline
                                 aborts; on_complete/close are NOT called.
        3. process(row, ctx)  -- called once per row
        4. on_complete(ctx)   -- all rows processed or pipeline errored (optional).
                                 Called even on crash. Runs before close().
        5. close()            -- release resources. Called even on crash.

    on_complete/close run inside a finally block and are individually
    protected (one plugin's failure does not prevent others from cleaning up).

    Error Routing:
        All transforms must have on_error set (required by TransformSettings).
        on_error specifies where errored rows go: a sink name or "discard".

    Example:
        class EnrichTransform:
            name = "enrich"
            input_schema = InputSchema
            output_schema = OutputSchema
            is_batch_aware = False

            def process(self, row: PipelineRow, ctx: TransformContext) -> TransformResult:
                enriched = {**row.to_dict(), "timestamp": datetime.now().isoformat()}
                return TransformResult.success(
                    enriched,
                    success_reason={"action": "enriched", "fields_added": ["timestamp"]},
                )
    """

    name: str
    input_schema: type["PluginSchema"]
    output_schema: type["PluginSchema"]
    node_id: str | None  # Set by orchestrator after registration
    config: dict[str, Any]  # Configuration dict stored by all plugins

    # Audit metadata
    determinism: Determinism
    plugin_version: str
    source_file_hash: str | None

    # Lifecycle guards (set by BaseTransform.on_start()/on_complete()).
    # The TransformExecutor checks _on_start_called before process() to ensure
    # on_start() was called. All transforms must inherit BaseTransform which
    # manages these. Contract tests use these flags as falsifiable
    # post-conditions for lifecycle invocation (a stub override that forgets
    # super() flips the flag, breaking the test).
    _on_start_called: bool
    _on_complete_called: bool

    # Batch support flag (must be False for TransformProtocol)
    # When True, transform must implement BatchTransformProtocol instead
    is_batch_aware: bool

    # Batch-aware row-mode opt-in. Batch-aware plugins default to aggregation
    # placement unless they explicitly set this True.
    supports_row_mode_when_batch_aware: bool

    # Token creation flag for deaggregation
    # When True, process() may return TransformResult.success_multi(rows)
    # and new tokens will be created for each output row.
    # When False, success_multi() is only valid in passthrough aggregation mode.
    creates_tokens: bool

    # Pass-through contract flag (ADR-007).
    # When True, process() UNCONDITIONALLY emits rows containing every field
    # present on the input row (plus declared_output_fields), regardless of
    # row content or runtime state. Enables the DAG validator to propagate
    # predecessor guarantees through this transform. Verified at runtime by
    # TransformExecutor's cross-check; mis-annotation raises
    # PassThroughContractViolation (TIER_1).
    passes_through_input: bool

    # ADR-012 empty-emission governance declaration.
    # True means the transform may intentionally emit zero rows on success.
    can_drop_rows: bool

    # Field collision enforcement (centralized in TransformExecutor).
    # Transforms that add fields to the output row declare WHAT fields they add
    # at init time. The executor checks these against input keys BEFORE running
    # the transform, preventing wasted API calls and making collision detection
    # mandatory (not opt-in per plugin). Empty frozenset = no fields added = no check.
    declared_output_fields: frozenset[str]

    # ADR-013 pre-emission declaration surface.
    # Normalized from TransformDataConfig.required_input_fields at construction
    # time. Empty frozenset = no required-input declaration.
    declared_input_fields: frozenset[str]

    # Runtime preflight opt-in. The orchestrator checks this explicit flag
    # instead of probing for optional methods, preserving a closed lifecycle
    # surface.
    requires_runtime_preflight: bool

    # DAG contract: output schema for transforms that declare output fields.
    # Set by BaseTransform._build_output_schema_config() after declared_output_fields
    # is populated. None for shape-preserving transforms that add no fields.
    # The DAG builder validates that non-empty declared_output_fields always has
    # a corresponding _output_schema_config (raises FrameworkBugError otherwise).
    _output_schema_config: SchemaConfig | None

    def effective_static_contract(self) -> frozenset[str]:
        """Return the transform's static output guarantee surface.

        Runtime declaration dispatchers use this public method instead of
        reaching into ``_output_schema_config`` directly, so missing or stale
        output-schema declarations crash at the transform boundary.
        """
        ...

    # Error routing configuration
    # Injected by cli_helpers.py bridge from TransformSettings.on_error.
    # Always non-None at runtime (TransformSettings requires on_error).
    # Protocol retains str | None because injection happens post-construction.
    on_error: str | None

    # Success routing configuration
    # Injected by cli_helpers.py bridge from TransformSettings.on_success.
    # Always non-None at runtime (TransformSettings requires on_success).
    # Protocol retains str | None because injection happens post-construction.
    on_success: str | None

    def __init__(self, config: dict[str, Any]) -> None:
        """Initialize with configuration."""
        ...

    def process(
        self,
        row: "PipelineRow",
        ctx: "TransformContext",
    ) -> "TransformResult":
        """Process a single row.

        Args:
            row: Input row as PipelineRow (immutable, supports dual-name access)
            ctx: Transform context with per-row identity and recording methods

        Returns:
            TransformResult with processed row dict or error
        """
        ...

    def close(self) -> None:
        """Release resources (connections, file handles, thread pools).

        Called after on_complete(), inside a finally block. Guaranteed if
        on_start() succeeded, even on pipeline crash. Individually protected.
        """
        ...

    # === Lifecycle Hooks ===

    def on_start(self, ctx: "LifecycleContext") -> None:
        """Called once before any process() call. If raises, pipeline aborts; on_complete/close skipped."""
        ...

    def on_complete(self, ctx: "LifecycleContext") -> None:
        """Called after all rows processed or on error, before close(). Individually protected."""
        ...

    def runtime_preflight(self, ctx: "LifecycleContext") -> None:
        """Optional engine-time readiness check before source iteration."""
        ...

    @classmethod
    def get_config_model(cls, config: dict[str, Any] | None = None) -> type[PluginConfigProtocol] | None:
        """Return the Pydantic config model for this plugin type.

        Override for dynamic dispatch (e.g. LLMTransform selects provider-specific
        model based on config["provider"]).
        """
        ...

    @classmethod
    def get_config_schema(cls) -> dict[str, Any]:
        """Return the full JSON Schema advertising this plugin's configuration.

        Same contract as :meth:`SourceProtocol.get_config_schema` — see
        there for the canonical specification, including the
        MUST-override rule for plugins whose effective configuration is
        a discriminated union. LLMTransform is the
        motivating example: it dispatches on ``config["provider"]`` at
        runtime, so :meth:`~elspeth.plugins.transforms.llm.transform.LLMTransform.get_config_schema`
        overrides to emit ``oneOf`` + ``$defs`` over every provider
        variant; most other transforms inherit the default single-model
        rendering on :class:`~elspeth.plugins.infrastructure.base.BaseTransform`.
        """
        ...


class BatchTransformProtocol(Protocol):
    """Protocol for batch-aware transforms — type-checking only, not @runtime_checkable.

    Batch transforms receive lists of rows and emit results. Used in aggregation
    nodes where the engine buffers rows until trigger fires.

    The engine passes list[PipelineRow] - each row is guaranteed to be a PipelineRow
    instance with its schema contract. Transforms should use row.to_dict() to get
    mutable dicts when constructing output.

    Lifecycle (main thread, called by orchestrator):
        1. __init__(config)    -- plugin instantiation
        2. on_start(ctx)       -- per-run init (optional). If raises, pipeline
                                  aborts; on_complete/close are NOT called.
        3. process(rows, ctx)  -- called when aggregation trigger fires
        4. on_complete(ctx)    -- all rows processed or pipeline errored (optional).
                                  Called even on crash. Runs before close().
        5. close()             -- release resources. Called even on crash.

    on_complete/close run inside a finally block and are individually
    protected (one plugin's failure does not prevent others from cleaning up).

    Error Routing:
        Batch transforms that can return TransformResult.error() must set on_error
        to specify where errored batches go.

    Example:
        class BatchStats:
            name = "batch_stats"
            input_schema = InputSchema
            output_schema = OutputSchema
            is_batch_aware = True

            def process(
                self,
                rows: list[PipelineRow],
                ctx: TransformContext,
            ) -> TransformResult:
                total = sum(row["amount"] for row in rows)
                return TransformResult.success(
                    {"count": len(rows), "total": total},
                    success_reason={"action": "aggregated"},
                )
    """

    name: str
    input_schema: type["PluginSchema"]
    output_schema: type["PluginSchema"]
    node_id: str | None  # Set by orchestrator after registration
    config: dict[str, Any]  # Configuration dict stored by all plugins

    # Audit metadata
    determinism: Determinism
    plugin_version: str
    source_file_hash: str | None

    # Lifecycle guards (set by BaseTransform.on_start()/on_complete()).
    # Batch transforms inherit BaseTransform which manages these. Contract tests
    # use these flags as falsifiable post-conditions for lifecycle invocation
    # (a stub override that forgets super() flips the flag, breaking the test).
    _on_start_called: bool
    _on_complete_called: bool

    # Batch support flag (must be True for BatchTransformProtocol)
    is_batch_aware: bool

    # Batch-aware row-mode opt-in. Batch-aware plugins default to aggregation
    # placement unless they explicitly set this True.
    supports_row_mode_when_batch_aware: bool

    # Token creation flag for deaggregation
    # When True, process() may return TransformResult.success_multi(rows)
    # and new tokens will be created for each output row.
    creates_tokens: bool

    # Pass-through contract flag (ADR-007).
    # When True, process() UNCONDITIONALLY emits rows containing every field
    # present on each input row. Enables DAG validator propagation; verified
    # at runtime by TransformExecutor's cross-check (per-row in batch mode).
    # Mis-annotation raises PassThroughContractViolation (TIER_1).
    passes_through_input: bool

    # ADR-013 pre-emission declaration surface.
    declared_input_fields: frozenset[str]

    # Runtime preflight opt-in. The orchestrator checks this explicit flag
    # instead of probing for optional methods, preserving a closed lifecycle
    # surface.
    requires_runtime_preflight: bool

    # Error routing configuration
    # Injected by cli_helpers.py bridge from AggregationSettings/TransformSettings.
    on_error: str | None

    # Success routing configuration
    # Injected by cli_helpers.py bridge from AggregationSettings.on_success.
    on_success: str | None

    def __init__(self, config: dict[str, Any]) -> None:
        """Initialize with configuration."""
        ...

    def process(
        self,
        rows: list["PipelineRow"],
        ctx: "TransformContext",
    ) -> "TransformResult":
        """Process a batch of rows.

        Args:
            rows: List of input rows as PipelineRow instances
            ctx: Transform context with per-row identity and recording methods

        Returns:
            TransformResult with aggregated result or multiple output rows
        """
        ...

    def close(self) -> None:
        """Release resources. Called after on_complete(), inside a finally block. Individually protected."""
        ...

    # === Lifecycle Hooks ===

    def on_start(self, ctx: "LifecycleContext") -> None:
        """Called once before any process() call. If raises, pipeline aborts; on_complete/close skipped."""
        ...

    def on_complete(self, ctx: "LifecycleContext") -> None:
        """Called after all rows processed or on error, before close(). Individually protected."""
        ...

    def runtime_preflight(self, ctx: "LifecycleContext") -> None:
        """Optional engine-time readiness check before source iteration."""
        ...

    @classmethod
    def get_config_model(cls, config: dict[str, Any] | None = None) -> type[PluginConfigProtocol] | None:
        """Return the Pydantic config model for this plugin type.

        Override for dynamic dispatch based on config contents.
        """
        ...

    @classmethod
    def get_config_schema(cls) -> dict[str, Any]:
        """Return the full JSON Schema advertising this plugin's configuration.

        Same contract as :meth:`SourceProtocol.get_config_schema` — see
        there for the canonical specification, including the
        MUST-override rule for plugins whose effective configuration is
        a discriminated union.
        """
        ...


class SinkProtocol(Protocol):
    """Protocol for sink plugins — type-checking only, not @runtime_checkable.

    Sinks output data to external destinations.
    There can be multiple sinks per run.

    Lifecycle (main thread, called by orchestrator):
        1. __init__(config)   -- plugin instantiation
        2. on_start(ctx)      -- per-run init (optional). If raises, pipeline
                                 aborts; on_complete/close are NOT called.
        3. write(rows, ctx)   -- called in batches as rows reach this sink
        4. flush()            -- called before checkpoints for durability
        5. on_complete(ctx)   -- all rows written or pipeline errored (optional).
                                 Called even on crash. Runs before close().
        6. close()            -- release resources. Called even on crash.

    on_complete/close run inside a finally block and are individually
    protected (one plugin's failure does not prevent others from cleaning up).

    Idempotency:
    - Sinks receive idempotency keys: {run_id}:{row_id}:{sink_name}
    - Sinks that cannot guarantee idempotency should set idempotent=False

    Example:
        class CSVSink:
            name = "csv"
            input_schema = RowSchema
            idempotent = False  # Appends are not idempotent

            def write(self, rows: list[dict], ctx: SinkContext) -> SinkWriteResult:
                for row in rows:
                    self._writer.writerow(row)
                return SinkWriteResult(
                    artifact=ArtifactDescriptor.for_file(
                        path=self._path,
                        content_hash=self._compute_hash(),
                        size_bytes=self._file.tell(),
                    ),
                )

            def flush(self) -> None:
                self._file.flush()
    """

    name: str
    input_schema: type["PluginSchema"]
    idempotent: bool  # Can this sink handle retries safely?
    node_id: str | None  # Set by orchestrator after registration
    config: dict[str, Any]  # Configuration dict stored by all plugins

    # Audit metadata
    determinism: Determinism
    plugin_version: str
    source_file_hash: str | None

    # Resume capability
    supports_resume: bool  # Can this sink append to existing output on resume?

    # Required-field enforcement (centralized in SinkExecutor).
    # Sinks that declare required fields have them checked BEFORE write().
    # Empty frozenset = no required-field check = all fields optional.
    declared_required_fields: frozenset[str]

    # Write failure routing — injected by cli_helpers from SinkSettings.
    # "discard" = drop failed rows with audit record, else = failsink name.
    _on_write_failure: str | None

    # Lifecycle guards (set by BaseSink.on_start()/on_complete()).
    # All sinks must inherit BaseSink which manages these. Contract tests
    # use these flags as falsifiable post-conditions for lifecycle invocation
    # (a stub override that forgets super() flips the flag, breaking the test).
    _on_start_called: bool
    _on_complete_called: bool

    def _reset_diversion_log(self) -> None:
        """Clear diversion log before each write() call."""
        ...

    def __init__(self, config: dict[str, Any]) -> None:
        """Initialize with configuration."""
        ...

    def write(
        self,
        rows: list[dict[str, Any]],
        ctx: "SinkContext",
    ) -> "SinkWriteResult":
        """Write a batch of rows to the sink.

        Args:
            rows: List of row dicts to write
            ctx: Sink context with run identity and recording methods

        Returns:
            SinkWriteResult with artifact descriptor and optional diversions
        """
        ...

    def flush(self) -> None:
        """Ensure all buffered writes are durable.

        MUST guarantee that when this method returns:
        - All data passed to write() is persisted
        - Data survives process crash
        - Data survives power loss (for file/block storage)

        This method MUST block until durability is guaranteed.

        For file-based sinks: Call file.flush() + os.fsync(file.fileno())
        For database sinks: Call connection.commit()
        For async sinks: Await flush completion

        This is called by the orchestrator BEFORE creating checkpoints.
        If this method returns successfully, the checkpoint system assumes
        all data is durable and will NOT replay these writes on resume.
        """
        ...

    def close(self) -> None:
        """Release resources (file handles, connections).

        Called after on_complete(), inside a finally block. Guaranteed if
        on_start() succeeded, even on pipeline crash. Individually protected.
        """
        ...

    # === Lifecycle Hooks ===

    def on_start(self, ctx: "LifecycleContext") -> None:
        """Called once before any write() call. If raises, pipeline aborts; on_complete/close skipped."""
        ...

    def on_complete(self, ctx: "LifecycleContext") -> None:
        """Called after all rows written or on error, before close(). Individually protected."""
        ...

    def configure_for_resume(self) -> None:
        """Configure sink for resume mode (append instead of truncate).

        Called by engine when resuming a run. Sinks that support resume
        (supports_resume=True) MUST implement this to switch to append mode.

        Sinks that don't support resume (supports_resume=False) will never
        have this called - the CLI will reject resume before execution.

        Raises:
            NotImplementedError: If sink cannot be resumed despite claiming support.
        """
        ...

    def validate_output_target(self) -> "OutputValidationResult":
        """Validate existing output target matches configured schema.

        Called by engine/CLI before write operations in append/resume mode.
        Returns valid=True by default (dynamic schema or no validation needed).

        Sinks that support resume SHOULD override to validate that existing
        output target (file/table) schema matches configured schema.

        Returns:
            OutputValidationResult indicating compatibility.
        """
        ...

    @property
    def needs_resume_field_resolution(self) -> bool:
        """Whether this sink needs field resolution mapping for resume.

        True when headers mode is ORIGINAL — the CLI resume path must
        provide the source field resolution mapping before validation.
        """
        ...

    def set_resume_field_resolution(self, resolution_mapping: dict[str, str]) -> None:
        """Set field resolution mapping for resume validation.

        Called by CLI during `elspeth resume` to provide the source field resolution
        mapping BEFORE calling validate_output_target(). This allows sinks using
        headers: original to correctly compare expected display names against
        existing file headers.

        Args:
            resolution_mapping: Dict mapping original header name -> normalized field name.
                This is the same format returned by Landscape.get_source_field_resolution().

        Note:
            BaseSink raises NotImplementedError when
            needs_resume_field_resolution is True. Only sinks configured with
            headers: original need to override this.
        """
        ...

    @classmethod
    def get_config_model(cls, config: dict[str, Any] | None = None) -> type[PluginConfigProtocol] | None:
        """Return the Pydantic config model for this plugin type.

        Override for dynamic dispatch based on config contents.
        """
        ...

    @classmethod
    def get_config_schema(cls) -> dict[str, Any]:
        """Return the full JSON Schema advertising this plugin's configuration.

        Same contract as :meth:`SourceProtocol.get_config_schema` — see
        there for the canonical specification, including the
        MUST-override rule for plugins whose effective configuration is
        a discriminated union.
        """
        ...


class DisplayHeaderHost(Protocol):
    """Structural type for sinks that use display header functions.

    Any sink that calls init_display_headers() will satisfy this protocol.
    Provides type safety for the display_headers module functions instead
    of using Any. This is an internal protocol — engine and CLI code should
    use SinkProtocol, not this.

    NOT @runtime_checkable — the protocol's members are private attributes,
    and isinstance() only checks method signatures, not attribute presence.
    Use mypy structural checking instead.
    """

    _headers_mode: HeaderMode
    _headers_custom_mapping: dict[str, str] | None
    _resolved_display_headers: dict[str, str] | None
    _display_headers_resolved: bool
    _needs_resume_field_resolution: bool
    _output_contract: "SchemaContract | None"
