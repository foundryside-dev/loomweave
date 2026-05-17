"""CLI helper functions for plugin instantiation and database resolution."""

import os
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast

import structlog

import elspeth.contracts.errors as contract_errors
from elspeth.contracts.freeze import freeze_fields

slog = structlog.get_logger(__name__)

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence

    from elspeth.contracts import SinkProtocol, SourceProtocol, TransformProtocol
    from elspeth.contracts.run_result import RunResult
    from elspeth.core.config import AggregationSettings, ElspethSettings, LandscapeSettings, SourceSettings, TransformSettings
    from elspeth.core.dag import WiredTransform
    from elspeth.core.landscape.factory import RecorderFactory


@dataclass(frozen=True, slots=True)
class PluginBundle:
    """Pre-instantiated plugin instances from configuration.

    Typed fields enable mypy checking and IDE autocomplete on all
    access sites.
    """

    source: "SourceProtocol"
    source_settings: "SourceSettings"
    transforms: "Sequence[WiredTransform]"
    sinks: "Mapping[str, SinkProtocol]"
    aggregations: "Mapping[str, tuple[TransformProtocol, AggregationSettings]]"

    def __post_init__(self) -> None:
        freeze_fields(self, "transforms", "sinks", "aggregations")


def instantiate_plugins_from_config(
    config: "ElspethSettings",
    *,
    preflight_mode: bool = False,
) -> PluginBundle:
    """Instantiate all plugins from configuration.

    Creates plugin instances BEFORE graph construction,
    enabling schema extraction from instance attributes.

    When preflight_mode=True, plugin constructors observe
    plugin_preflight_mode_enabled() and may defer side-effectful client setup
    to lifecycle/operation methods.

    Args:
        config: Validated ElspethSettings instance
        preflight_mode: If True, sets the process-local preflight context so
            plugin constructors can defer external client initialisation.

    Returns:
        PluginBundle with typed fields for source, transforms, sinks, aggregations.

    Raises:
        ValueError: If config references unknown plugins (raised by PluginManager)
    """
    from elspeth.core.dag import WiredTransform
    from elspeth.plugins.infrastructure.manager import get_shared_plugin_manager
    from elspeth.plugins.infrastructure.preflight import plugin_preflight_mode

    manager = get_shared_plugin_manager()

    with plugin_preflight_mode(preflight_mode):
        # Instantiate source (raises on unknown plugin)
        source_cls = manager.get_source_by_name(config.source.plugin)
        source = source_cls(dict(config.source.options))
        # Bridge: inject on_success from settings level (lifted from options)
        source.on_success = config.source.on_success

        # Instantiate transforms
        transforms: list[WiredTransform] = []
        for plugin_config in config.transforms:
            transform_cls = manager.get_transform_by_name(plugin_config.plugin)
            transform = transform_cls(dict(plugin_config.options))
            # Bridge: inject routing from settings level (lifted from options)
            transform.on_success = plugin_config.on_success
            transform.on_error = plugin_config.on_error
            transforms.append(WiredTransform(plugin=transform, settings=plugin_config))

        # Instantiate aggregations
        # Aggregations REQUIRE batch-aware transforms (is_batch_aware=True).
        # Non-batch-aware transforms process rows individually, ignoring aggregation
        # triggers entirely - a silent misconfiguration that produces wrong results.
        aggregations = {}
        for agg_config in config.aggregations:
            transform_cls = manager.get_transform_by_name(agg_config.plugin)
            transform = transform_cls(dict(agg_config.options))
            # Bridge: inject routing from settings level (lifted from options)
            transform.on_success = agg_config.on_success
            transform.on_error = agg_config.on_error

            # Validate batch-aware requirement (fail-fast before graph construction)
            if not transform.is_batch_aware:
                raise ValueError(
                    f"Aggregation '{agg_config.name}' uses transform '{agg_config.plugin}' "
                    f"which has is_batch_aware=False. Aggregations require batch-aware "
                    f"transforms that can process multiple rows at once. "
                    f"Use a batch-aware transform like 'batch_stats' or 'batch_replicate', "
                    f"or set is_batch_aware=True on your custom transform."
                )

            aggregations[agg_config.name] = (transform, agg_config)

        # Instantiate sinks
        sinks = {}
        for sink_name, sink_config in config.sinks.items():
            sink_cls = manager.get_sink_by_name(sink_config.plugin)
            sinks[sink_name] = sink_cls(dict(sink_config.options))
            sinks[sink_name]._on_write_failure = sink_config.on_write_failure

        bundle = PluginBundle(
            source=source,
            source_settings=config.source,
            transforms=transforms,
            sinks=sinks,
            aggregations=aggregations,
        )

    # Value-source compliance check. Single source of truth: every
    # caller (composer ``/validate``, web service, every CLI command,
    # the dependency-runner callback) passes through this function, so
    # placing the check here means hand-authored YAML with hallucinated
    # values is rejected at construction time regardless of which entry
    # point built the bundle. Runs after ``plugin_preflight_mode``
    # context exits — the bundle exists and is well-formed; the only
    # remaining question is whether declared field values comply with
    # their plugins' VALUE_SOURCES contracts.
    from elspeth.engine.orchestrator.preflight import validate_value_source_compliance

    validate_value_source_compliance(_value_source_wired_transforms(bundle))
    return bundle


def _value_source_wired_transforms(bundle: PluginBundle) -> "tuple[WiredTransform, ...]":
    """Return all transform-shaped plugin instances for value-source checks.

    Aggregations are backed by transform plugins too, but they live in a
    separate bundle field until pipeline assembly folds them into the runtime
    transform list. The assembly helper intentionally does not rerun the
    value-source walker, so the construction boundary must present both ordinary
    transforms and aggregation transforms here.
    """
    from elspeth.core.dag import WiredTransform

    aggregation_transforms = tuple(
        WiredTransform(plugin=transform, settings=cast("TransformSettings", agg_settings))
        for transform, agg_settings in bundle.aggregations.values()
    )
    return (*bundle.transforms, *aggregation_transforms)


def _make_sink_factory(config: "ElspethSettings") -> "Callable[[str], SinkProtocol]":
    """Create a factory that produces fresh sink instances from config.

    Used by the export phase, which runs after the pipeline's sinks have
    already been closed. The factory creates a new, unstarted instance
    each time it is called.
    """
    from elspeth.plugins.infrastructure.manager import get_shared_plugin_manager

    def factory(sink_name: str) -> "SinkProtocol":
        if sink_name not in config.sinks:
            raise ValueError(f"Export sink '{sink_name}' not found in sink configuration")
        sink_config = config.sinks[sink_name]
        manager = get_shared_plugin_manager()
        sink_cls = manager.get_sink_by_name(sink_config.plugin)
        sink = sink_cls(dict(sink_config.options))
        sink._on_write_failure = sink_config.on_write_failure
        return sink

    return factory


def resolve_database_url(
    database: str | None,
    settings_path: Path | None,
) -> tuple[str, "ElspethSettings | None"]:
    """Resolve database URL from CLI option or settings file.

    Priority: CLI --database > explicit --settings > settings.yaml landscape.url

    Args:
        database: Explicit database path from CLI (optional)
        settings_path: Path to settings.yaml file (optional)

    Returns:
        Tuple of (database_url, config_or_none)

    Raises:
        ValueError: If database file not found, settings invalid, or neither provided
    """
    from elspeth.core.config import load_settings

    config: ElspethSettings | None = None

    if database:
        db_path = Path(database).expanduser().resolve()
        # Fail fast with clear error if file doesn't exist
        if not db_path.exists():
            raise ValueError(f"Database file not found: {db_path}")
        if settings_path is not None:
            normalized_settings = settings_path.expanduser().resolve()
            if not normalized_settings.exists():
                raise ValueError(f"Settings file not found: {normalized_settings}")
        return f"sqlite:///{db_path}", None

    # Try explicit settings file
    if settings_path is not None:
        normalized_settings = settings_path.expanduser().resolve()
        if not normalized_settings.exists():
            raise ValueError(f"Settings file not found: {normalized_settings}")
        try:
            config = load_settings(normalized_settings)
        except Exception as e:
            raise ValueError(f"Error loading settings from {settings_path}: {e}") from e

    if config is not None:
        return config.landscape.url, config

    # Try default settings.yaml - DO NOT silently swallow errors
    default_settings = Path("settings.yaml")
    if default_settings.exists():
        try:
            config = load_settings(default_settings)
            return config.landscape.url, config
        except Exception as e:
            # Don't silently fall through - user should know why settings.yaml failed
            raise ValueError(f"Error loading default settings.yaml: {e}") from e

    raise ValueError("No database specified. Provide --database or ensure settings.yaml exists with landscape.url configured.")


def resolve_latest_run_id(factory: "RecorderFactory") -> str | None:
    """Get the most recently started run ID.

    Args:
        factory: RecorderFactory with database connection

    Returns:
        Run ID of most recent run, or None if no runs exist
    """
    runs = factory.run_lifecycle.list_runs()
    if not runs:
        return None
    # list_runs returns ordered by started_at DESC
    return runs[0].run_id


def resolve_run_id(run_id: str, factory: "RecorderFactory") -> str | None:
    """Resolve run_id, handling 'latest' keyword.

    Args:
        run_id: Explicit run ID or 'latest'
        factory: RecorderFactory for looking up latest

    Returns:
        Resolved run ID, or None if 'latest' requested but no runs exist
    """
    if run_id.lower() == "latest":
        return resolve_latest_run_id(factory)
    return run_id


def bootstrap_and_run(settings_path: Path) -> "RunResult":
    """Load config, instantiate plugins, build graph, run pipeline.

    This is the programmatic equivalent of ``elspeth run --execute``.
    Used by the dependency resolver to run sub-pipelines.

    Delegates dependency resolution and commencement gates to
    ``resolve_preflight()`` if configured in the pipeline settings.

    Handles:
    - Secret resolution (Key Vault secrets injected via ``_load_settings_with_secrets()``)
    - SQLCipher passphrase resolution for encrypted audit databases
    - Collection probe construction for commencement gates

    Args:
        settings_path: Absolute or relative path to pipeline settings YAML.

    Returns:
        RunResult from orchestrator.run()

    Raises:
        Any exception from config loading, plugin instantiation, graph validation,
        or pipeline execution. Caller is responsible for error handling.
    """
    from elspeth.cli import _execution_sinks_for_graph, _load_settings_with_secrets, _orchestrator_context
    from elspeth.core.dag import ExecutionGraph
    from elspeth.core.landscape import LandscapeDB
    from elspeth.core.payload_store import FilesystemPayloadStore
    from elspeth.engine.bootstrap import resolve_preflight
    from elspeth.plugins.infrastructure.probe_factory import build_collection_probes

    # Phase 1: Load and validate config with secret resolution
    config, secret_resolutions = _load_settings_with_secrets(settings_path)

    # Phase 2: Instantiate plugins (value-source compliance is checked
    # inside ``instantiate_plugins_from_config`` for all callers — see
    # the validate_value_source_compliance call there).
    plugins = instantiate_plugins_from_config(config)

    # Phase 3: Build and validate execution graph
    # Exclude export sink from graph (same logic as CLI)
    execution_sinks = _execution_sinks_for_graph(config, plugins.sinks)

    graph = ExecutionGraph.from_plugin_instances(
        source=plugins.source,
        source_settings=plugins.source_settings,
        transforms=plugins.transforms,
        sinks=execution_sinks,
        aggregations=plugins.aggregations,
        gates=list(config.gates),
        coalesce_settings=list(config.coalesce) if config.coalesce else None,
    )
    graph.validate()

    # Ensure output directories exist before preflight — dependency pipelines
    # can mutate external state, so reject unwritable paths before running them.
    from elspeth.cli import _ensure_output_directories

    dir_errors = _ensure_output_directories(config)
    if dir_errors:
        raise ValueError(f"Failed to create output directories: {'; '.join(dir_errors)}")

    probes = build_collection_probes(config.collection_probes) if config.collection_probes else []
    preflight = resolve_preflight(config, settings_path, probes=probes, runner=bootstrap_and_run)

    # Phase 4: Construct infrastructure and run
    passphrase = resolve_audit_passphrase(config.landscape)

    # Warn if JSONL journal is enabled alongside SQLCipher — journal is plaintext.
    # Mirrors the same guard in cli.py run command.
    if passphrase is not None and config.landscape.dump_to_jsonl:
        import structlog

        structlog.get_logger().warning(
            "JSONL journal is not encrypted",
            hint="The JSONL change journal is written in plaintext even when the audit database is encrypted with SQLCipher.",
        )

    db = LandscapeDB.from_url(
        config.landscape.url,
        passphrase=passphrase,
        dump_to_jsonl=config.landscape.dump_to_jsonl,
        dump_to_jsonl_path=config.landscape.dump_to_jsonl_path,
        dump_to_jsonl_fail_on_error=config.landscape.dump_to_jsonl_fail_on_error,
        dump_to_jsonl_include_payloads=config.landscape.dump_to_jsonl_include_payloads,
        dump_to_jsonl_payload_base_path=(
            str(config.payload_store.base_path)
            if config.landscape.dump_to_jsonl_payload_base_path is None
            else config.landscape.dump_to_jsonl_payload_base_path
        ),
    )

    if config.payload_store.backend != "filesystem":
        raise ValueError(f"Unsupported payload store backend '{config.payload_store.backend}'. Only 'filesystem' is currently supported.")
    payload_store = FilesystemPayloadStore(config.payload_store.base_path)

    try:
        with _orchestrator_context(
            config,
            graph,
            plugins,
            db=db,
            output_format="none",
        ) as ctx:
            return ctx.orchestrator.run(
                ctx.pipeline_config,
                graph=graph,
                settings=config,
                payload_store=payload_store,
                preflight_results=preflight,
                secret_resolutions=secret_resolutions,
                sink_factory=_make_sink_factory(config),
            )
    finally:
        import sys

        pending_exc = sys.exc_info()[1]
        try:
            db.close()
        except contract_errors.TIER_1_ERRORS:
            raise  # System bugs always crash through
        except Exception as close_exc:
            # db.close() failure must not mask the original pipeline exception.
            # The pipeline error is operationally more important than a cleanup
            # failure. If there is no pipeline error, close() failure propagates.
            if pending_exc is not None:
                slog.warning(
                    "db.close() failed during exception cleanup — suppressed",
                    close_error=f"{type(close_exc).__name__}: {close_exc}",
                )
            else:
                raise


def resolve_audit_passphrase(
    settings: "LandscapeSettings | None",
) -> str | None:
    """Resolve the SQLCipher passphrase from the environment.

    The passphrase is always read from an environment variable (never from config
    files or URLs) to prevent it from appearing in logs, tracebacks, or the audit
    trail itself.

    When settings is None (e.g. ad-hoc CLI access via ``--database``), returns
    None — encryption requires explicit ``backend: sqlcipher`` configuration.
    This prevents ELSPETH_AUDIT_KEY from accidentally opening plain SQLite
    databases through SQLCipher.

    Args:
        settings: LandscapeSettings determining which env var to read.
            If None, returns None (no encryption without explicit config).

    Returns:
        Passphrase string if backend is sqlcipher, None otherwise.

    Raises:
        RuntimeError: If backend is sqlcipher but the env var is not set.
    """
    if settings is not None and settings.backend == "sqlcipher":
        env_var = settings.encryption_key_env
        passphrase = os.environ.get(env_var)
        if passphrase is None or not passphrase.strip():
            raise RuntimeError(
                f'SQLCipher backend requires a non-empty encryption passphrase.\nSet the environment variable: export {env_var}="your-passphrase"'
            )
        return passphrase

    # No settings, or settings.backend is not sqlcipher → no encryption.
    # We intentionally do NOT fall back to ELSPETH_AUDIT_KEY when settings
    # is None — that env var may be set for a different pipeline, and passing
    # a passphrase to a plain SQLite database causes "file is not a database".
    return None
