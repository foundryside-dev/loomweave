"""ELSPETH Command Line Interface.

Entry point for the elspeth CLI tool.
"""

from __future__ import annotations

import os
from collections.abc import Iterator, Mapping
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal

import typer
import yaml
from dynaconf.vendor.ruamel.yaml.parser import ParserError as YamlParserError
from dynaconf.vendor.ruamel.yaml.scanner import ScannerError as YamlScannerError
from pydantic import ValidationError

import elspeth.contracts.errors as contract_errors
from elspeth import __version__
from elspeth.contracts import ExecutionResult, SecretResolutionInput
from elspeth.contracts.errors import (
    CommencementGateFailedError,
    DependencyFailedError,
    GracefulShutdownError,
)
from elspeth.contracts.types import AggregationName
from elspeth.core.config import ElspethSettings, SourceSettings, load_settings, resolve_config
from elspeth.core.dag import ExecutionGraph, GraphValidationError
from elspeth.core.dependency_config import PreflightResult
from elspeth.core.security.config_secrets import SecretLoadError, load_secrets_from_config

if TYPE_CHECKING:
    from elspeth.cli_helpers import PluginBundle
    from elspeth.contracts import SinkProtocol, SourceProtocol
    from elspeth.contracts.payload_store import PayloadStore
    from elspeth.core.landscape import LandscapeDB
    from elspeth.engine import Orchestrator, PipelineConfig
    from elspeth.engine.orchestrator import RowPlugin
__all__ = [
    "app",
    "load_settings",  # Re-exported from config for convenience
]

app = typer.Typer(
    name="elspeth",
    help="ELSPETH: Auditable Sense/Decide/Act pipelines.",
    no_args_is_help=True,
)


def version_callback(value: bool) -> None:
    """Print version and exit."""
    if value:
        typer.echo(f"elspeth version {__version__}")
        raise typer.Exit()


def _load_dotenv(env_file: Path | None = None) -> bool:
    """Load environment variables from .env file.

    Args:
        env_file: Explicit path to .env file. If None, searches for .env
                 in current directory and parent directories.

    Returns:
        True if .env was found and loaded, False otherwise.

    Raises:
        typer.Exit: If explicit env_file path doesn't exist.
    """
    from dotenv import load_dotenv

    if env_file is not None:
        if not env_file.exists():
            typer.secho(
                f"Error: .env file not found: {env_file}",
                fg=typer.colors.RED,
                err=True,
            )
            raise typer.Exit(1)
        return load_dotenv(env_file, override=False)

    # load_dotenv searches current dir and parents by default
    return load_dotenv(override=False)  # Don't override existing env vars


@app.callback()
def main(
    version: bool | None = typer.Option(
        None,
        "--version",
        "-V",
        callback=version_callback,
        is_eager=True,
        help="Show version and exit.",
    ),
    no_dotenv: bool = typer.Option(
        False,
        "--no-dotenv",
        help="Skip loading .env file.",
    ),
    env_file: Path | None = typer.Option(
        None,
        "--env-file",
        help="Path to .env file (skips automatic search).",
        exists=False,  # We handle existence check ourselves for better error message
    ),
    verbose: bool = typer.Option(
        False,
        "--verbose",
        "-v",
        help="Enable verbose/debug logging.",
    ),
    json_logs: bool = typer.Option(
        False,
        "--json-logs",
        help="Output structured JSON logs (for machine processing).",
    ),
) -> None:
    """ELSPETH: Auditable Sense/Decide/Act pipelines."""
    # Configure logging at entry point (before any subcommands run)
    # CRITICAL: This must be called early so container logs appear in stdout
    from elspeth.core.logging import configure_logging

    log_level = "DEBUG" if verbose else "INFO"
    configure_logging(json_output=json_logs, level=log_level)

    if not no_dotenv:
        _load_dotenv(env_file=env_file)
    elif env_file is not None:
        typer.secho(
            "Warning: --env-file ignored because --no-dotenv is set.",
            fg=typer.colors.YELLOW,
            err=True,
        )


def _ensure_output_directories(config: ElspethSettings) -> list[str]:
    """Ensure required output directories exist, creating them if needed.

    Creates directories BEFORE attempting to create databases or files,
    providing clear error messages if creation fails.

    Args:
        config: Validated ElspethSettings

    Returns:
        List of error messages (empty if all directories exist or were created)
    """
    from sqlalchemy.engine.url import make_url

    errors: list[str] = []

    # 1. Ensure Landscape database directory exists (for SQLite)
    db_url = config.landscape.url
    parsed_url = make_url(db_url)

    if parsed_url.drivername.startswith("sqlite"):
        # Extract the file path from SQLite URL
        # SQLite URLs: sqlite:///./path/to/file.db or sqlite:////absolute/path.db
        db_path = parsed_url.database
        if db_path:
            # Handle relative paths (./foo) and absolute paths (/foo)
            db_file = Path(db_path)
            parent_dir = db_file.parent

            # Resolve to absolute path for clearer error messages
            resolved_parent = parent_dir.resolve()

            if not parent_dir.exists():
                try:
                    parent_dir.mkdir(parents=True, exist_ok=True)
                except OSError as e:
                    errors.append(f"Cannot create Landscape database directory: {resolved_parent}\n  Database URL: {db_url}\n  Error: {e}")
            elif not parent_dir.is_dir():
                errors.append(f"Landscape database path exists but is not a directory: {resolved_parent}\n  Database URL: {db_url}")
            elif not os.access(parent_dir, os.W_OK):
                errors.append(f"Landscape database directory is not writable: {resolved_parent}\n  Database URL: {db_url}")

    # 2. Ensure payload store directory exists
    payload_path = config.payload_store.base_path
    if not payload_path.exists():
        try:
            payload_path.mkdir(parents=True, exist_ok=True)
        except OSError as e:
            errors.append(f"Cannot create payload store directory: {payload_path.resolve()}\n  Error: {e}")
    elif not payload_path.is_dir():
        errors.append(f"Payload store path exists but is not a directory: {payload_path.resolve()}")
    elif not os.access(payload_path, os.W_OK):
        errors.append(f"Payload store directory is not writable: {payload_path.resolve()}")

    # 3. Ensure sink output directories exist (for file-based sinks)
    for sink_name, sink_config in config.sinks.items():
        # SinkSettings.options is always dict[str, Any]; "path" key present for file-based sinks
        sink_path = sink_config.options.get("path")
        if sink_path:
            sink_file = Path(sink_path)
            sink_parent = sink_file.parent

            if sink_parent and str(sink_parent) != ".":
                resolved_sink_parent = sink_parent.resolve()
                if not sink_parent.exists():
                    try:
                        sink_parent.mkdir(parents=True, exist_ok=True)
                    except OSError as e:
                        errors.append(
                            f"Cannot create sink '{sink_name}' output directory: {resolved_sink_parent}\n  Output path: {sink_path}\n  Error: {e}"
                        )
                elif not sink_parent.is_dir():
                    errors.append(f"Sink '{sink_name}' output path parent exists but is not a directory: {resolved_sink_parent}")

    return errors


def _validate_existing_sqlite_db_url(db_url: str, *, source: str) -> None:
    """Fail fast when a file-backed SQLite URL points to a missing file."""
    from urllib.parse import unquote, urlsplit

    from sqlalchemy.engine.url import make_url

    parsed_url = make_url(db_url)
    if not parsed_url.drivername.startswith("sqlite"):
        return

    db_path = parsed_url.database
    if db_path is None:
        return

    query = parsed_url.query
    raw_uri = query.get("uri")
    uri_enabled = False
    if raw_uri is not None:
        uri_value = raw_uri if isinstance(raw_uri, str) else raw_uri[0]
        uri_enabled = uri_value.lower() in ("1", "true", "yes", "on")

    raw_mode = query.get("mode")
    if raw_mode is not None:
        mode_value = raw_mode if isinstance(raw_mode, str) else raw_mode[0]
        if mode_value == "memory":
            return

    if db_path == ":memory:" or db_path.startswith("file::memory:"):
        return

    if uri_enabled and db_path.startswith("file:"):
        split = urlsplit(db_path)
        path_part = unquote(split.path)
        if split.netloc and split.netloc != "localhost":
            path_part = f"//{split.netloc}{path_part}"
        if not path_part:
            return
        resolved = Path(path_part).expanduser().resolve()
    else:
        resolved = Path(db_path).expanduser().resolve()

    if not resolved.exists():
        typer.echo(f"Error: Database file not found ({source}): {resolved}", err=True)
        raise typer.Exit(1) from None


def _load_raw_yaml(config_path: Path) -> dict[str, Any]:
    """Load YAML without environment variable resolution.

    This is used to extract the secrets config before loading secrets.
    The secrets config MUST use literal values (no ${VAR}) because
    secrets are loaded before environment variable resolution.

    Raises:
        ValueError: Parsed YAML root is not a mapping/object.
    """
    with open(config_path) as f:
        raw_config = yaml.safe_load(f)

    if raw_config is None:
        return {}
    if not isinstance(raw_config, dict):
        actual_type = type(raw_config).__name__
        raise ValueError(f"Settings YAML root must be a mapping/object, got {actual_type} (in {config_path}).")
    return raw_config


def _load_settings_with_secrets(
    settings_path: Path,
) -> tuple[ElspethSettings, list[SecretResolutionInput]]:
    """Load settings with Key Vault secret resolution.

    Three-phase loading pattern:
    1. Raw YAML parse (no ${VAR} resolution) - extract secrets config
    2. Secret injection from Key Vault (if configured) - populate os.environ
    3. Full Dynaconf loading (resolves ${VAR}) - now has secrets available

    This function encapsulates the secret-loading flow used by run, resume,
    and validate commands to ensure Key Vault-backed pipelines work consistently.

    Args:
        settings_path: Path to settings YAML file (must exist)

    Returns:
        Tuple of (validated ElspethSettings, list of secret resolution records)
        Resolution records are for deferred audit recording and contain:
        env_var_name, source, vault_url, secret_name, timestamp, latency_ms,
        secret_value (for fingerprinting - NOT for storage)

    Raises:
        FileNotFoundError: Settings file not found
        yaml.YAMLError: YAML syntax error
        ValueError: Settings YAML root is not a mapping/object
        ValidationError: Pydantic validation error (secrets config or full config)
        SecretLoadError: Key Vault secret loading failed
    """
    from elspeth.core.config import SecretsConfig

    # Phase 1: Parse YAML to extract secrets config (no ${VAR} resolution yet)
    # NOTE: vault_url must be literal per design - ${VAR} not supported
    raw_config = _load_raw_yaml(settings_path)

    # Extract and validate secrets config
    secrets_obj = raw_config.get("secrets", {})
    if secrets_obj is None:
        secrets_dict: dict[str, Any] = {}
    else:
        if not isinstance(secrets_obj, Mapping):
            raise ValueError(f"'secrets' must be a mapping/object, got {type(secrets_obj).__name__}")
        secrets_dict = dict(secrets_obj)
    secrets_config = SecretsConfig(**secrets_dict)

    # Phase 2: Load secrets from Key Vault if configured
    # Returns resolution records for later audit recording
    secret_resolutions = load_secrets_from_config(secrets_config)

    # Phase 3: Full config loading with Dynaconf (resolves ${VAR})
    # Now that secrets are in os.environ, Dynaconf can resolve them
    config = load_settings(settings_path)

    return config, secret_resolutions


def _execution_sinks_for_graph(
    config: ElspethSettings,
    sinks: Mapping[str, SinkProtocol],
) -> Mapping[str, SinkProtocol]:
    """Return only sinks that participate in row-flow graph validation/execution."""
    if config.landscape.export.enabled and config.landscape.export.sink:
        export_sink_name = config.landscape.export.sink
        return {name: sink for name, sink in sinks.items() if name != export_sink_name}
    return sinks


@app.command()
def run(
    settings: str = typer.Option(
        ...,
        "--settings",
        "-s",
        help="Path to settings YAML file.",
    ),
    dry_run: bool = typer.Option(
        False,
        "--dry-run",
        "-n",
        help="Validate and show what would run without executing.",
    ),
    execute: bool = typer.Option(
        False,
        "--execute",
        "-x",
        help="Actually execute the pipeline (required for safety).",
    ),
    verbose: bool = typer.Option(
        False,
        "--verbose",
        "-v",
        help="Show detailed output.",
    ),
    output_format: Literal["console", "json"] = typer.Option(
        "console",
        "--format",
        "-f",
        help="Output format: 'console' (human-readable) or 'json' (structured JSON).",
    ),
) -> None:
    """Execute a pipeline run.

    Requires --execute flag to actually run (safety feature).
    Use --dry-run to validate configuration without executing.
    """
    from elspeth.cli_helpers import instantiate_plugins_from_config

    settings_path = Path(settings).expanduser()

    # Load and validate config with Key Vault secrets (same flow as other commands)
    try:
        config, secret_resolutions = _load_settings_with_secrets(settings_path)
    except FileNotFoundError:
        typer.echo(f"Error: Settings file not found: {settings}", err=True)
        raise typer.Exit(1) from None
    except (YamlParserError, YamlScannerError) as e:
        typer.echo(f"YAML syntax error in {settings}: {e.problem if e.problem is not None else e}", err=True)
        raise typer.Exit(1) from None
    except yaml.YAMLError as e:
        typer.echo(f"YAML syntax error in {settings}: {e}", err=True)
        raise typer.Exit(1) from None
    except ValidationError as e:
        typer.echo("Configuration errors:", err=True)
        for error in e.errors():
            loc = ".".join(str(x) for x in error["loc"])
            typer.echo(f"  - {loc}: {error['msg']}", err=True)
        raise typer.Exit(1) from None
    except ValueError as e:
        typer.echo(f"Configuration error: {e}", err=True)
        raise typer.Exit(1) from None
    except SecretLoadError as e:
        typer.echo(f"Error loading secrets: {e}", err=True)
        raise typer.Exit(1) from None

    # Instantiate plugins before graph construction
    try:
        plugins = instantiate_plugins_from_config(config)
    except contract_errors.TIER_1_ERRORS:
        raise  # Tier 1 errors must crash with full traceback, not Exit(1)
    except Exception as e:
        typer.echo(f"Error instantiating plugins: {e}", err=True)
        raise typer.Exit(1) from None

    # Build and validate graph from plugin instances
    # Exclude export sink from graph - it's used post-run, not during pipeline execution.
    # The export sink receives audit records after the run completes, not pipeline data.
    execution_sinks = _execution_sinks_for_graph(config, plugins.sinks)

    try:
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
    except GraphValidationError as e:
        typer.echo(f"Pipeline graph error: {e}", err=True)
        raise typer.Exit(1) from None
    except ValueError as e:
        # Schema compatibility errors raised during graph construction (PHASE 2)
        typer.echo(f"Schema validation error: {e}", err=True)
        raise typer.Exit(1) from None

    # Console-only messages (don't emit in JSON mode to keep stream clean)
    if output_format == "console":
        if verbose:
            typer.echo(f"Graph validated: {graph.node_count} nodes, {graph.edge_count} edges")

        if dry_run:
            typer.echo("Dry run mode - would execute:")
            typer.echo(f"  Source: {config.source.plugin}")
            typer.echo(f"  Transforms: {len(config.transforms)}")
            typer.echo(f"  Sinks: {', '.join(config.sinks.keys())}")
            return

        # Safety check: require explicit --execute flag
        if not execute:
            typer.echo("Pipeline configuration valid.")
            typer.echo(f"  Source: {config.source.plugin}")
            typer.echo("")
            typer.echo("To execute, add --execute (or -x) flag:", err=True)
            typer.echo(f"  elspeth run -s {settings} --execute", err=True)
            raise typer.Exit(1)
    else:
        # JSON mode: early exits without console output
        if dry_run or not execute:
            raise typer.Exit(1)

    # Ensure output directories exist BEFORE attempting to create resources
    # Creates directories automatically, only errors if creation fails
    # NOTE: Only when actually executing (not dry-run or validation-only)
    dir_errors = _ensure_output_directories(config)
    if dir_errors:
        typer.echo("Output directory errors:", err=True)
        for dir_error in dir_errors:
            typer.echo(f"\n{dir_error}", err=True)
        raise typer.Exit(1)

    # Resolve SQLCipher passphrase (if backend=sqlcipher)
    from elspeth.cli_helpers import resolve_audit_passphrase

    try:
        passphrase = resolve_audit_passphrase(config.landscape)
    except RuntimeError as e:
        typer.echo(f"Error: {e}", err=True)
        raise typer.Exit(1) from None

    # Resolve dependencies and commencement gates before pipeline execution
    from elspeth.cli_helpers import bootstrap_and_run
    from elspeth.engine.bootstrap import resolve_preflight
    from elspeth.plugins.infrastructure.probe_factory import build_collection_probes

    try:
        probes = build_collection_probes(config.collection_probes) if config.collection_probes else []
        preflight = resolve_preflight(config, settings_path, probes=probes, runner=bootstrap_and_run)
    except (DependencyFailedError, CommencementGateFailedError, ValueError) as e:
        typer.echo(f"Pre-flight check failed: {e}", err=True)
        raise typer.Exit(1) from None
    except contract_errors.TIER_1_ERRORS as e:
        import traceback

        typer.echo(f"\nFATAL — {type(e).__name__}: {e}", err=True)
        typer.echo(traceback.format_exc(), err=True)
        raise typer.Exit(4) from e
    except Exception as e:
        import traceback

        typer.echo(f"\nFATAL — Unexpected pre-flight error ({type(e).__name__}): {e}", err=True)
        typer.echo(traceback.format_exc(), err=True)
        raise typer.Exit(4) from e

    # Execute pipeline with pre-instantiated plugins
    try:
        execution_result = _execute_pipeline_with_instances(
            config,
            graph,
            plugins,
            verbose=verbose,
            output_format=output_format,
            secret_resolutions=secret_resolutions,
            passphrase=passphrase,
            preflight_results=preflight,
        )
    except GracefulShutdownError as e:
        if output_format == "json":
            import json as json_mod

            typer.echo(
                json_mod.dumps(
                    {
                        "event": "interrupted",
                        "run_id": e.run_id,
                        "rows_processed": e.rows_processed,
                        "message": str(e),
                    }
                )
            )
        else:
            typer.echo(f"\nPipeline interrupted after {e.rows_processed} rows.")
            typer.echo(f"Resume with: elspeth resume {e.run_id} --execute")
        raise typer.Exit(3)  # noqa: B904 -- distinct exit code: 0=success, 1=error, 3=interrupted
    except contract_errors.TIER_1_ERRORS as e:
        # Tier 1 violations and framework bugs MUST be clearly distinguishable
        # from config errors. These indicate database corruption, tampering,
        # or bugs in ELSPETH itself — not operator mistakes.
        import traceback

        if output_format == "json":
            import json

            typer.echo(
                json.dumps(
                    {
                        "event": "fatal",
                        "error": str(e),
                        "error_type": type(e).__name__,
                        "traceback": traceback.format_exc(),
                    }
                ),
                err=True,
            )
        else:
            typer.echo(f"\nFATAL — {type(e).__name__}: {e}", err=True)
            typer.echo(traceback.format_exc(), err=True)
        raise typer.Exit(4) from e  # Exit 4: audit integrity / framework bug
    except Exception as e:
        # Always emit the full traceback — hiding it makes debugging impossible.
        # The AggregationName NameError hid for days because this block swallowed it.
        import traceback

        if output_format == "json":
            import json

            typer.echo(
                json.dumps(
                    {
                        "event": "error",
                        "error": str(e),
                        "error_type": type(e).__name__,
                        "traceback": traceback.format_exc(),
                    }
                ),
                err=True,
            )
        else:
            typer.echo(f"Error during pipeline execution: {e}", err=True)
            typer.echo(traceback.format_exc(), err=True)
        raise typer.Exit(4) from e  # Exit 4: unknown errors during execution are likely framework bugs

    # Emit final execution summary in JSON mode for machine consumption
    if output_format == "json":
        import json

        typer.echo(json.dumps({"event": "execution_result", **execution_result}))


@app.command()
def explain(
    run_id: str = typer.Option(
        ...,
        "--run",
        "-r",
        help="Run ID to explain (or 'latest').",
    ),
    row: str | None = typer.Option(
        None,
        "--row",
        help="Row ID to explain.",
    ),
    token: str | None = typer.Option(
        None,
        "--token",
        "-t",
        help="Token ID for precise lineage.",
    ),
    database: str | None = typer.Option(
        None,
        "--database",
        "-d",
        help="Path to Landscape database file (SQLite). Required for explain.",
    ),
    settings: str | None = typer.Option(
        None,
        "--settings",
        "-s",
        help="Path to settings YAML file.",
    ),
    sink: str | None = typer.Option(
        None,
        "--sink",
        help="Sink name to disambiguate when row has multiple terminal tokens.",
    ),
    no_tui: bool = typer.Option(
        False,
        "--no-tui",
        help="Output text instead of interactive TUI.",
    ),
    json_output: bool = typer.Option(
        False,
        "--json",
        help="Output as JSON.",
    ),
) -> None:
    """Explain lineage for a row or token.

    Use --no-tui for text output or --json for JSON output.
    Without these flags, launches an interactive TUI.

    Examples:

        # JSON output for a specific token
        elspeth explain --run latest --token tok-abc --json --database ./audit.db

        # Text output for a row
        elspeth explain --run run-123 --row row-456 --no-tui --database ./audit.db

        # Interactive TUI
        elspeth explain --run latest --database ./audit.db
    """
    import json as json_module

    from elspeth.cli_helpers import resolve_database_url, resolve_run_id
    from elspeth.core.landscape import (
        LandscapeDB,
        LineageTextFormatter,
        dataclass_to_dict,
    )
    from elspeth.core.landscape import explain as explain_lineage
    from elspeth.core.landscape.factory import RecorderFactory

    if database is None:
        message = "--database is required for explain."
        if json_output:
            typer.echo(json_module.dumps({"error": message}))
        else:
            typer.echo(f"Error: {message}", err=True)
        raise typer.Exit(1) from None

    # Resolve database URL
    settings_path = Path(settings) if settings else None
    try:
        db_url, config = resolve_database_url(database, settings_path)
    except ValueError as e:
        if json_output:
            typer.echo(json_module.dumps({"error": str(e)}))
        else:
            typer.echo(f"Error: {e}", err=True)
        raise typer.Exit(1) from None

    # Resolve SQLCipher passphrase.
    # When --database is provided without --settings, config is None because
    # the CLI path overrides settings-based URL. If --settings was provided,
    # resolve_database_url() loads it for passphrase resolution.
    # Keep the fallback for older callers that may still return config=None.
    from elspeth.cli_helpers import resolve_audit_passphrase

    landscape_settings = config.landscape if config else None
    if landscape_settings is None and settings_path is not None and settings_path.exists():
        try:
            settings_for_passphrase, _ = _load_settings_with_secrets(Path(settings_path).expanduser())
            landscape_settings = settings_for_passphrase.landscape
        except (FileNotFoundError, yaml.YAMLError, YamlParserError, YamlScannerError) as e:
            # User explicitly provided --settings (guarded by settings_path is not None
            # on line above), so YAML errors are fatal — don't silently proceed
            # with passphrase=None.
            if json_output:
                typer.echo(json_module.dumps({"error": f"Settings YAML error: {e}"}))
            else:
                typer.echo(f"Error loading settings: {e}", err=True)
            raise typer.Exit(1) from None
        except (ValidationError, SecretLoadError) as e:
            # Settings loaded but failed validation or secret resolution.
            # User explicitly provided --settings, so surface the error.
            if json_output:
                typer.echo(json_module.dumps({"error": f"Settings loading failed: {e}"}))
            else:
                typer.echo(f"Error loading settings: {e}", err=True)
            raise typer.Exit(1) from None

    try:
        passphrase = resolve_audit_passphrase(landscape_settings)
    except RuntimeError as e:
        if json_output:
            typer.echo(json_module.dumps({"error": str(e)}))
        else:
            typer.echo(f"Error: {e}", err=True)
        raise typer.Exit(1) from None

    # Connect to database
    # Initialize db = None for proper cleanup in finally block
    db: LandscapeDB | None = None
    try:
        db = LandscapeDB.from_url(db_url, passphrase=passphrase, create_tables=False)
    except Exception as e:
        if json_output:
            typer.echo(json_module.dumps({"error": f"Database connection failed: {e}"}))
        else:
            typer.echo(f"Error connecting to database: {e}", err=True)
        raise typer.Exit(1) from None

    try:
        factory = RecorderFactory(db)

        # Resolve 'latest' run_id
        resolved_run_id = resolve_run_id(run_id, factory)
        if resolved_run_id is None:
            if json_output:
                typer.echo(json_module.dumps({"error": "No runs found in database"}))
            else:
                typer.echo("Error: No runs found in database", err=True)
            raise typer.Exit(1) from None

        # Must provide either token or row for JSON/no-tui modes
        if (json_output or no_tui) and token is None and row is None:
            if json_output:
                typer.echo(json_module.dumps({"error": "Must provide either --token or --row"}))
            else:
                typer.echo("Error: Must provide either --token or --row", err=True)
            raise typer.Exit(1) from None

        # Query lineage (only for JSON/no-tui modes)
        if json_output or no_tui:
            try:
                lineage_result = explain_lineage(
                    factory.query,
                    factory.data_flow,
                    run_id=resolved_run_id,
                    token_id=token,
                    row_id=row,
                    sink=sink,
                )
            except ValueError as e:
                # Ambiguous row (multiple tokens) or invalid args
                if json_output:
                    typer.echo(json_module.dumps({"error": str(e)}))
                else:
                    typer.echo(f"Error: {e}", err=True)
                raise typer.Exit(1) from None

            if lineage_result is None:
                if json_output:
                    typer.echo(json_module.dumps({"error": "Token or row not found, or no terminal tokens exist yet"}))
                else:
                    typer.echo("Token or row not found, or no terminal tokens exist yet.", err=True)
                raise typer.Exit(1) from None

            # Output based on mode
            if json_output:
                typer.echo(json_module.dumps(dataclass_to_dict(lineage_result), indent=2))
                raise typer.Exit(0)

            if no_tui:
                formatter = LineageTextFormatter()
                typer.echo(formatter.format(lineage_result))
                raise typer.Exit(0)

        # TUI mode
        from elspeth.tui.explain_app import ExplainApp

        tui_app = ExplainApp(
            db=db,
            run_id=resolved_run_id,
        )
        tui_app.run()

    finally:
        if db is not None:
            db.close()


@dataclass(frozen=True, slots=True)
class _OrchestratorContext:
    """Shared context yielded by _orchestrator_context()."""

    pipeline_config: PipelineConfig
    orchestrator: Orchestrator


@contextmanager
def _orchestrator_context(
    config: ElspethSettings,
    graph: ExecutionGraph,
    plugins: PluginBundle,
    *,
    db: LandscapeDB,
    formatter_prefix: str = "Run",
    output_format: Literal["console", "json", "none"] = "console",
    checkpoint_always: bool = False,
) -> Iterator[_OrchestratorContext]:
    """Shared orchestrator setup and teardown for run/resume CLI paths.

    Handles:
    - Plugin unpacking and aggregation wiring
    - PipelineConfig construction
    - EventBus + formatter subscription
    - Runtime*Config creation (rate_limit, concurrency, checkpoint, telemetry)
    - RateLimitRegistry, CheckpointManager, TelemetryManager lifecycle
    - Orchestrator construction

    Teardown (always runs): closes rate_limit_registry and telemetry_manager.

    Args:
        config: Validated ElspethSettings
        graph: Validated ExecutionGraph (schemas populated)
        plugins: Pre-instantiated PluginBundle from instantiate_plugins_from_config()
        db: LandscapeDB connection (caller owns close lifecycle)
        formatter_prefix: Prefix for console formatters ("Run" or "Resume")
        output_format: 'console', 'json', or 'none' for programmatic execution
        checkpoint_always: If True, always create CheckpointManager (resume needs it).
            If False, only create when checkpoint config is enabled (normal run).

    Yields:
        _OrchestratorContext with pipeline_config and orchestrator
    """
    from elspeth.cli_formatters import create_console_formatters, create_json_formatters, subscribe_formatters
    from elspeth.contracts.config.runtime import (
        RuntimeCheckpointConfig,
        RuntimeConcurrencyConfig,
        RuntimeRateLimitConfig,
        RuntimeTelemetryConfig,
    )
    from elspeth.core import EventBus, EventBusProtocol, NullEventBus
    from elspeth.core.checkpoint import CheckpointManager
    from elspeth.core.config import AggregationSettings
    from elspeth.core.rate_limit import RateLimitRegistry
    from elspeth.engine import Orchestrator as _Orchestrator
    from elspeth.engine import PipelineConfig as _PipelineConfig
    from elspeth.telemetry import create_telemetry_manager

    # Unpack pre-instantiated plugins
    source: SourceProtocol = plugins.source
    sinks: Mapping[str, SinkProtocol] = plugins.sinks

    # Build transforms list: row_plugins + aggregations (with node_id)
    transforms: list[RowPlugin] = [wired.plugin for wired in plugins.transforms]

    agg_id_map = graph.get_aggregation_id_map()
    aggregation_settings: dict[str, AggregationSettings] = {}

    for agg_name, (transform, agg_config) in plugins.aggregations.items():
        node_id = agg_id_map[AggregationName(agg_name)]
        aggregation_settings[node_id] = agg_config
        transform.node_id = node_id
        transforms.append(transform)

    # Build PipelineConfig
    pipeline_config = _PipelineConfig(
        source=source,
        transforms=transforms,
        sinks=sinks,
        config=resolve_config(config),
        gates=list(config.gates),
        aggregation_settings=aggregation_settings,
    )

    # EventBus + formatters. Programmatic dependency execution uses the
    # explicit no-output mode so sub-pipelines cannot pollute parent CLI output.
    if output_format == "none":
        event_bus: EventBusProtocol = NullEventBus()
    else:
        event_bus = EventBus()
        formatters = create_json_formatters() if output_format == "json" else create_console_formatters(prefix=formatter_prefix)
        subscribe_formatters(event_bus, formatters)

    # Runtime configs
    rate_limit_config = RuntimeRateLimitConfig.from_settings(config.rate_limit)
    concurrency_config = RuntimeConcurrencyConfig.from_settings(config.concurrency)
    checkpoint_config = RuntimeCheckpointConfig.from_settings(config.checkpoint)
    telemetry_config = RuntimeTelemetryConfig.from_settings(config.telemetry)

    rate_limit_registry: RateLimitRegistry | None = None
    telemetry_manager = None

    try:
        rate_limit_registry = RateLimitRegistry(rate_limit_config)
        telemetry_manager = create_telemetry_manager(telemetry_config)

        # Checkpoint manager: always for resume, conditional for run
        checkpoint_manager = CheckpointManager(db) if checkpoint_always or checkpoint_config.enabled else None

        orchestrator = _Orchestrator(
            db,
            event_bus=event_bus,
            rate_limit_registry=rate_limit_registry,
            concurrency_config=concurrency_config,
            checkpoint_manager=checkpoint_manager,
            checkpoint_config=checkpoint_config,
            telemetry_manager=telemetry_manager,
        )

        yield _OrchestratorContext(
            pipeline_config=pipeline_config,
            orchestrator=orchestrator,
        )
    finally:
        if rate_limit_registry is not None:
            rate_limit_registry.close()
        if telemetry_manager is not None:
            telemetry_manager.close()


def _execute_pipeline_with_instances(
    config: ElspethSettings,
    graph: ExecutionGraph,
    plugins: PluginBundle,
    verbose: bool = False,
    output_format: Literal["console", "json"] = "console",
    secret_resolutions: list[SecretResolutionInput] | None = None,
    passphrase: str | None = None,
    preflight_results: PreflightResult | None = None,
) -> ExecutionResult:
    """Execute pipeline using pre-instantiated plugin instances.

    Args:
        config: Validated ElspethSettings
        graph: Validated ExecutionGraph (schemas populated)
        plugins: Pre-instantiated PluginBundle from instantiate_plugins_from_config()
        verbose: Show detailed output
        output_format: 'console' or 'json'
        secret_resolutions: Optional list of secret resolution records from
            load_secrets_from_config(). Passed to orchestrator for audit recording.
        passphrase: Optional SQLCipher passphrase for encrypted audit DB
        preflight_results: Optional PreflightResult from dependency resolution
            and commencement gates. Passed to orchestrator for audit recording.

    Returns:
        ExecutionResult with run_id, status, rows_processed
    """
    from elspeth.core.landscape import LandscapeDB

    # Warn if JSONL journal is enabled alongside SQLCipher — journal is plaintext
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

    # Create payload store for audit compliance
    from elspeth.core.payload_store import FilesystemPayloadStore

    if config.payload_store.backend != "filesystem":
        typer.echo(
            f"Error: Unsupported payload store backend '{config.payload_store.backend}'. Only 'filesystem' is currently supported.",
            err=True,
        )
        raise typer.Exit(1)
    payload_store = FilesystemPayloadStore(config.payload_store.base_path)

    try:
        if verbose:
            typer.echo("Starting pipeline execution...")

        with _orchestrator_context(
            config,
            graph,
            plugins,
            db=db,
            formatter_prefix="Run",
            output_format=output_format,
        ) as ctx:
            from elspeth.cli_helpers import _make_sink_factory

            result = ctx.orchestrator.run(
                ctx.pipeline_config,
                graph=graph,
                settings=config,
                payload_store=payload_store,
                secret_resolutions=secret_resolutions,
                preflight_results=preflight_results,
                sink_factory=_make_sink_factory(config),
            )

            return {
                "run_id": result.run_id,
                "status": result.status,  # RunStatus enum (str subclass)
                "rows_processed": result.rows_processed,
            }
    finally:
        db.close()


def _format_validation_error(
    title: str,
    message: str,
    hint: str | None = None,
    details: list[str] | None = None,
) -> None:
    """Display a formatted validation error with optional hint and details."""
    from rich.console import Console
    from rich.panel import Panel
    from rich.text import Text

    console = Console(stderr=True)

    # Build error content
    content = Text()
    content.append(message, style="white")

    if details:
        content.append("\n\n")
        for detail in details:
            content.append(f"  • {detail}\n", style="dim")

    if hint:
        content.append("\n")
        content.append("Hint: ", style="yellow bold")
        content.append(hint, style="yellow")

    panel = Panel(
        content,
        title=f"[red bold]❌ {title}[/]",
        border_style="red",
        padding=(0, 1),
    )
    console.print(panel)


@app.command()
def validate(
    settings: str = typer.Option(
        ...,
        "--settings",
        "-s",
        help="Path to settings YAML file.",
    ),
) -> None:
    """Validate pipeline configuration without running."""
    from elspeth.cli_helpers import instantiate_plugins_from_config

    settings_path = Path(settings).expanduser()

    # Load and validate config with Key Vault secrets (same flow as 'run' command)
    # This ensures ${VAR} placeholders are resolved correctly for keyvault-backed configs
    try:
        config, _secret_resolutions = _load_settings_with_secrets(settings_path)
    except (YamlParserError, YamlScannerError) as e:
        # YAML syntax errors from Dynaconf/ruamel (malformed YAML) - show helpful message
        _format_validation_error(
            title="YAML Syntax Error",
            message=f"Failed to parse {settings_path.name}",
            details=[str(e.problem)] if e.problem is not None else None,
            hint="Check for unclosed brackets, incorrect indentation, or invalid characters.",
        )
        raise typer.Exit(1) from None
    except yaml.YAMLError as e:
        # YAML syntax errors from PyYAML (used in _load_raw_yaml) - show helpful message
        _format_validation_error(
            title="YAML Syntax Error",
            message=f"Failed to parse {settings_path.name}",
            details=[str(e)],
            hint="Check for unclosed brackets, incorrect indentation, or invalid characters.",
        )
        raise typer.Exit(1) from None
    except FileNotFoundError:
        _format_validation_error(
            title="File Not Found",
            message=f"Settings file does not exist: {settings}",
            hint="Check the path and ensure the file exists.",
        )
        raise typer.Exit(1) from None
    except SecretLoadError as e:
        # Key Vault secret loading errors - show helpful message
        _format_validation_error(
            title="Secret Loading Failed",
            message=str(e),
            hint="Check your Azure credentials and Key Vault configuration.",
        )
        raise typer.Exit(1) from None
    except ValidationError as e:
        # Pydantic validation errors (must be before ValueError - ValidationError inherits from it!)
        details = []
        for error in e.errors():
            loc = ".".join(str(x) for x in error["loc"])
            details.append(f"{loc}: {error['msg']}")
        _format_validation_error(
            title="Configuration Validation Failed",
            message=f"Invalid settings in {settings_path.name}",
            details=details,
            hint="Check field names, types, and required values.",
        )
        raise typer.Exit(1) from None
    except ValueError as e:
        # Environment variable expansion errors (must be AFTER ValidationError!)
        error_msg = str(e)
        if "environment variable" in error_msg.lower():
            # Extract variable name from error message if possible
            import re

            match = re.search(r"'(\w+)'", error_msg)
            var_name = match.group(1) if match else "VARIABLE"
            _format_validation_error(
                title="Missing Environment Variable",
                message=error_msg,
                hint=f'Set the variable: export {var_name}="your-value"\n         Or use optional syntax: ${{{var_name}:-default}}',
            )
        else:
            _format_validation_error(
                title="Configuration Error",
                message=error_msg,
            )
        raise typer.Exit(1) from None

    # NOTE: _secret_resolutions captured but not used for validation
    # Validation is a dry-run check, no audit recording needed

    # Instantiate plugins BEFORE graph construction
    try:
        plugins = instantiate_plugins_from_config(config)
    except ValueError as e:
        # Plugin configuration errors (e.g., invalid schema for sink type)
        error_msg = str(e)
        _format_validation_error(
            title="Plugin Configuration Error",
            message=error_msg,
            hint="Check plugin options match the plugin's requirements.",
        )
        raise typer.Exit(1) from None
    except contract_errors.TIER_1_ERRORS:
        raise  # Tier 1 errors must crash with full traceback, not Exit(1)
    except Exception as e:
        _format_validation_error(
            title="Plugin Instantiation Failed",
            message=str(e),
            hint="Check that all plugin options are valid and dependencies are available.",
        )
        raise typer.Exit(1) from None

    # Build and validate graph from plugin instances
    execution_sinks = _execution_sinks_for_graph(config, plugins.sinks)
    try:
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
    except GraphValidationError as e:
        _format_validation_error(
            title="Pipeline Graph Error",
            message=str(e),
            hint="Check for cycles, missing sinks, or invalid routing.",
        )
        raise typer.Exit(1) from None
    except ValueError as e:
        # Schema compatibility errors raised during graph construction
        _format_validation_error(
            title="Schema Validation Error",
            message=str(e),
            hint="Ensure upstream nodes provide fields required by downstream nodes.",
        )
        raise typer.Exit(1) from None

    typer.echo("✅ Pipeline configuration valid!")
    typer.echo(f"  Source: {config.source.plugin}")
    typer.echo(f"  Transforms: {len(config.transforms)}")
    typer.echo(f"  Aggregations: {len(config.aggregations)}")
    typer.echo(f"  Sinks: {', '.join(config.sinks.keys())}")
    typer.echo(f"  Graph: {graph.node_count} nodes, {graph.edge_count} edges")


# Plugins subcommand group
plugins_app = typer.Typer(help="Plugin management commands.")
app.add_typer(plugins_app, name="plugins")


@dataclass(frozen=True, slots=True)
class PluginInfo:
    """Metadata for a registered plugin.

    Attributes:
        name: The plugin identifier used in configuration files.
        description: Human-readable description of the plugin's purpose.
    """

    name: str
    description: str


def _build_plugin_registry() -> dict[str, list[PluginInfo]]:
    """Build plugin registry dynamically from discovered plugins.

    Uses PluginManager to discover all plugins and extracts descriptions
    from their docstrings.

    Returns:
        Dict mapping plugin type to list of PluginInfo for each plugin.
    """
    from elspeth.plugins.infrastructure.discovery import get_plugin_description
    from elspeth.plugins.infrastructure.manager import get_shared_plugin_manager

    manager = get_shared_plugin_manager()

    return {
        "source": [PluginInfo(name=cls.name, description=get_plugin_description(cls)) for cls in manager.get_sources()],
        "transform": [PluginInfo(name=cls.name, description=get_plugin_description(cls)) for cls in manager.get_transforms()],
        "sink": [PluginInfo(name=cls.name, description=get_plugin_description(cls)) for cls in manager.get_sinks()],
    }


@plugins_app.command("list")
def plugins_list(
    plugin_type: str | None = typer.Option(
        None,
        "--type",
        "-t",
        help="Filter by plugin type (source, transform, sink).",
    ),
) -> None:
    """List available plugins."""
    # Build registry dynamically from discovered plugins
    registry = _build_plugin_registry()
    valid_types = set(registry.keys())

    if plugin_type and plugin_type not in valid_types:
        typer.echo(f"Error: Invalid type '{plugin_type}'.", err=True)
        typer.echo(f"Valid types: {', '.join(sorted(valid_types))}", err=True)
        raise typer.Exit(1)

    types_to_show = [plugin_type] if plugin_type else list(registry.keys())

    for ptype in types_to_show:
        # types_to_show only contains keys from registry (either filtered by validated plugin_type
        # or directly from registry.keys()), so direct access is safe
        plugins = registry[ptype]
        if plugins:
            typer.echo(f"\n{ptype.upper()}S:")
            for plugin in plugins:
                typer.echo(f"  {plugin.name:20} - {plugin.description}")
        else:
            typer.echo(f"\n{ptype.upper()}S:")
            typer.echo("  (none available)")

    typer.echo()  # Final newline


@app.command()
def purge(
    database: str | None = typer.Option(
        None,
        "--database",
        "-d",
        help="Path to Landscape database file (SQLite).",
    ),
    payload_dir: str | None = typer.Option(
        None,
        "--payload-dir",
        "-p",
        help="Path to payload storage directory.",
    ),
    retention_days: int | None = typer.Option(
        None,
        "--retention-days",
        "-r",
        help="Delete payloads older than this many days (default: from config or 90).",
    ),
    dry_run: bool = typer.Option(
        False,
        "--dry-run",
        help="Show what would be deleted without deleting.",
    ),
    yes: bool = typer.Option(
        False,
        "--yes",
        "-y",
        help="Skip confirmation prompt.",
    ),
) -> None:
    """Purge old payloads to free storage.

    Deletes PayloadStore blobs older than retention period.
    Landscape metadata (hashes) is preserved for audit trail.

    Examples:

        # See what would be deleted
        elspeth purge --dry-run --database ./landscape.db

        # Delete payloads older than 30 days
        elspeth purge --retention-days 30 --yes --database ./landscape.db
    """
    from elspeth.core.landscape import LandscapeDB
    from elspeth.core.payload_store import FilesystemPayloadStore
    from elspeth.core.retention.purge import PurgeManager

    # Try to load settings from settings.yaml if database not provided
    db_url: str | None = None
    payload_path: Path | None = None
    effective_retention_days: int = 90  # Fallback default
    config: ElspethSettings | None = None

    # Try loading settings.yaml first (for payload_store config)
    # Uses _load_settings_with_secrets to support Key Vault-backed configs
    settings_path = Path("settings.yaml")
    if settings_path.exists():
        try:
            config, _secret_resolutions = _load_settings_with_secrets(settings_path)
        except (YamlParserError, YamlScannerError) as e:
            problem = e.problem if e.problem is not None else str(e)
            if not database:
                typer.echo(f"YAML syntax error in settings.yaml: {problem}", err=True)
                typer.echo("Specify --database to provide path directly.", err=True)
                raise typer.Exit(1) from None
            typer.echo(f"Warning: YAML syntax error in settings.yaml: {problem}", err=True)
        except yaml.YAMLError as e:
            if not database:
                typer.echo(f"YAML error in settings.yaml: {e}", err=True)
                typer.echo("Specify --database to provide path directly.", err=True)
                raise typer.Exit(1) from None
            typer.echo(f"Warning: YAML error in settings.yaml: {e}", err=True)
        except ValidationError as e:
            if not database:
                typer.echo("Configuration errors in settings.yaml:", err=True)
                for error in e.errors():
                    loc = ".".join(str(x) for x in error["loc"])
                    typer.echo(f"  - {loc}: {error['msg']}", err=True)
                typer.echo("Specify --database to provide path directly.", err=True)
                raise typer.Exit(1) from None
            typer.echo("Warning: Configuration errors in settings.yaml (continuing with --database)", err=True)
        except ValueError as e:
            if not database:
                typer.echo(f"Configuration error in settings.yaml: {e}", err=True)
                typer.echo("Specify --database to provide path directly.", err=True)
                raise typer.Exit(1) from None
            typer.echo(f"Warning: Configuration error in settings.yaml: {e}", err=True)
        except SecretLoadError as e:
            if not database:
                typer.echo(f"Error loading secrets: {e}", err=True)
                typer.echo("Specify --database to provide path directly.", err=True)
                raise typer.Exit(1) from None
            typer.echo(f"Warning: Could not load secrets: {e}", err=True)

    if database:
        db_path = Path(database).expanduser().resolve()
        # Fail fast with clear error if file doesn't exist
        # (prevents silent creation of empty DB on typoed paths)
        if not db_path.exists():
            typer.echo(f"Error: Database file not found: {db_path}", err=True)
            raise typer.Exit(1) from None
        db_url = f"sqlite:///{db_path}"
    elif config:
        db_url = config.landscape.url
        _validate_existing_sqlite_db_url(db_url, source="settings.yaml")
        typer.echo(f"Using database from settings.yaml: {db_url}")
    else:
        typer.echo("Error: No settings.yaml found and --database not provided.", err=True)
        typer.echo("Specify --database to provide path to Landscape database.", err=True)
        raise typer.Exit(1) from None

    # Determine payload path: CLI override > config > default
    if payload_dir:
        payload_path = Path(payload_dir).expanduser()
    elif config:
        # Use config's payload_store.base_path
        payload_path = config.payload_store.base_path.expanduser()
        # Verify backend is supported
        if config.payload_store.backend != "filesystem":
            typer.echo(
                f"Error: Payload store backend '{config.payload_store.backend}' is not supported for purge. "
                f"Only 'filesystem' backend is currently implemented.",
                err=True,
            )
            raise typer.Exit(1) from None
        typer.echo(f"Using payload directory from config: {payload_path}")
    else:
        # Default to ./payloads relative to database location
        if database:
            payload_path = Path(database).parent / "payloads"
        else:
            payload_path = Path("payloads")

    # Determine retention days: CLI override > config > default (90)
    if retention_days is not None:
        if retention_days <= 0:
            typer.echo("Error: --retention-days must be greater than 0.", err=True)
            raise typer.Exit(1)
        effective_retention_days = retention_days
    elif config:
        effective_retention_days = config.payload_store.retention_days
        typer.echo(f"Using retention_days from config: {effective_retention_days}")
    # else: use the fallback default of 90

    # Resolve SQLCipher passphrase
    from elspeth.cli_helpers import resolve_audit_passphrase

    try:
        passphrase = resolve_audit_passphrase(config.landscape if config else None)
    except RuntimeError as e:
        typer.echo(f"Error: {e}", err=True)
        raise typer.Exit(1) from None

    # Initialize database and payload store
    # Note: purge is read-only for audit data, no JSONL journaling needed
    try:
        db = LandscapeDB.from_url(db_url, passphrase=passphrase, create_tables=False)
    except Exception as e:
        typer.echo(f"Error connecting to database: {e}", err=True)
        raise typer.Exit(1) from None

    try:
        payload_store = FilesystemPayloadStore(payload_path)
        purge_manager = PurgeManager(db, payload_store)

        # Find all expired payload refs (rows, calls, routing reasons)
        expired_refs = purge_manager.find_expired_payload_refs(effective_retention_days)

        if not expired_refs:
            typer.echo(f"No payloads older than {effective_retention_days} days found.")
            return

        if dry_run:
            typer.echo(f"Would delete {len(expired_refs)} payload(s) older than {effective_retention_days} days:")
            for ref in expired_refs[:10]:  # Show first 10
                exists = payload_store.exists(ref)
                status = "exists" if exists else "already deleted"
                typer.echo(f"  {ref[:16]}... ({status})")
            if len(expired_refs) > 10:
                typer.echo(f"  ... and {len(expired_refs) - 10} more")
            return

        # Confirm unless --yes
        if not yes:
            confirm = typer.confirm(f"Delete {len(expired_refs)} payload(s) older than {effective_retention_days} days?")
            if not confirm:
                typer.echo("Aborted.")
                raise typer.Exit(1)

        # Execute purge
        result = purge_manager.purge_payloads(expired_refs)

        typer.echo(f"Purge completed in {result.duration_seconds:.2f}s:")
        typer.echo(f"  Deleted: {result.deleted_count}")
        typer.echo(f"  Skipped (not found): {result.skipped_count}")
        if result.failed_refs:
            typer.echo(f"  Failed: {len(result.failed_refs)}")
            for ref in result.failed_refs[:5]:
                typer.echo(f"    {ref[:16]}...")
        if result.grade_update_failures:
            typer.echo(f"  Grade update failures: {len(result.grade_update_failures)} (runs may have stale reproducibility grades)")
            for run_id in result.grade_update_failures[:5]:
                typer.echo(f"    {run_id}")
    finally:
        db.close()


def _execute_resume_with_instances(
    config: ElspethSettings,
    graph: ExecutionGraph,
    plugins: PluginBundle,
    resume_point: Any,
    payload_store: PayloadStore | None,
    db: LandscapeDB,
    output_format: Literal["console", "json"] = "console",
) -> Any:  # Returns RunResult from orchestrator.resume()
    """Execute resume using pre-instantiated plugins.

    Args:
        config: Validated ElspethSettings
        graph: Validated ExecutionGraph
        plugins: Pre-instantiated plugins (with NullSource)
        resume_point: Resume point information
        payload_store: Payload store for retrieving row data
        db: LandscapeDB connection (caller owns close lifecycle)
        output_format: 'console' or 'json'

    Returns:
        RunResult from orchestrator.resume()
    """
    if payload_store is None:
        raise ValueError("payload_store is required for resume operations")

    with _orchestrator_context(
        config,
        graph,
        plugins,
        db=db,
        formatter_prefix="Resume",
        output_format=output_format,
        checkpoint_always=True,
    ) as ctx:
        return ctx.orchestrator.resume(
            resume_point=resume_point,
            config=ctx.pipeline_config,
            graph=graph,
            payload_store=payload_store,
            settings=config,
        )


def _build_resume_graphs(
    settings_config: ElspethSettings,
    plugins: PluginBundle,
) -> tuple[ExecutionGraph, ExecutionGraph]:
    """Build both validation and execution graphs for resume from pre-instantiated plugins.

    Returns:
        Tuple of (validation_graph, execution_graph):
        - validation_graph: Uses original source for topology hash matching
        - execution_graph: Uses NullSource since resume data comes from stored payloads
    """
    from elspeth.plugins.sources.null_source import NullSource

    gate_settings = list(settings_config.gates)
    coalesce_settings = list(settings_config.coalesce) if settings_config.coalesce else None

    # Validation graph uses the ORIGINAL source to match the topology hash
    # computed during the original run
    validation_graph = ExecutionGraph.from_plugin_instances(
        source=plugins.source,
        source_settings=plugins.source_settings,
        transforms=plugins.transforms,
        sinks=plugins.sinks,
        aggregations=plugins.aggregations,
        gates=gate_settings,
        coalesce_settings=coalesce_settings,
    )
    validation_graph.validate()

    # Execution graph uses NullSource — resume data comes from stored payloads.
    # NullSource inherits the original source's on_success (which may be a connection
    # name or sink name — the DAG builder validates it during graph construction).
    null_source_on_success = plugins.source.on_success
    null_source = NullSource({})
    null_source.on_success = null_source_on_success
    null_source_settings = SourceSettings(plugin="null", on_success=null_source_on_success)
    execution_graph = ExecutionGraph.from_plugin_instances(
        source=null_source,
        source_settings=null_source_settings,
        transforms=plugins.transforms,
        sinks=plugins.sinks,
        aggregations=plugins.aggregations,
        gates=gate_settings,
        coalesce_settings=coalesce_settings,
    )
    execution_graph.validate()

    return validation_graph, execution_graph


@app.command()
def resume(
    run_id: str = typer.Argument(..., help="Run ID to resume"),
    database: str | None = typer.Option(
        None,
        "--database",
        "-d",
        help="Path to Landscape database file (SQLite).",
    ),
    settings_file: str | None = typer.Option(
        None,
        "--settings",
        "-s",
        help="Path to settings YAML file (default: settings.yaml).",
    ),
    execute: bool = typer.Option(
        False,
        "--execute",
        "-x",
        help="Actually execute the resume (default is dry-run).",
    ),
    output_format: Literal["console", "json"] = typer.Option(
        "console",
        "--format",
        "-f",
        help="Output format: 'console' (human-readable) or 'json' (structured JSON).",
    ),
) -> None:
    """Resume a failed run from its last checkpoint.

    By default, shows what WOULD happen (dry run). Use --execute to
    actually resume processing.

    Examples:

        # Dry run - show resume info
        elspeth resume run-abc123

        # Actually resume processing
        elspeth resume run-abc123 --execute

        # Resume with explicit database path
        elspeth resume run-abc123 --database ./landscape.db --execute
    """
    from elspeth.core.checkpoint import CheckpointManager, RecoveryManager
    from elspeth.core.landscape import LandscapeDB

    # Settings are REQUIRED for topology validation
    settings_path = Path(settings_file).expanduser() if settings_file else Path("settings.yaml")

    if not settings_path.exists():
        typer.echo(f"Error: Settings file not found: {settings_path}", err=True)
        typer.echo("Settings are required to validate checkpoint compatibility.", err=True)
        raise typer.Exit(1)

    # Load settings with Key Vault secrets (same flow as 'run' command)
    # This ensures ${VAR} placeholders are resolved correctly for keyvault-backed configs
    try:
        settings_config, _secret_resolutions = _load_settings_with_secrets(settings_path)
    except FileNotFoundError:
        typer.echo(f"Error: Settings file not found: {settings_path}", err=True)
        raise typer.Exit(1) from None
    except yaml.YAMLError as e:
        typer.echo(f"YAML syntax error in {settings_path}: {e}", err=True)
        raise typer.Exit(1) from None
    except ValidationError as e:
        typer.echo("Configuration errors:", err=True)
        for error in e.errors():
            loc = ".".join(str(x) for x in error["loc"])
            typer.echo(f"  - {loc}: {error['msg']}", err=True)
        raise typer.Exit(1) from None
    except ValueError as e:
        typer.echo(f"Configuration error: {e}", err=True)
        raise typer.Exit(1) from None
    except SecretLoadError as e:
        typer.echo(f"Error loading secrets: {e}", err=True)
        raise typer.Exit(1) from None

    # NOTE: _secret_resolutions captured but not used for resume
    # Resume inherits the original run's secret resolution audit records

    # Resolve database URL
    if database:
        db_path = Path(database).expanduser().resolve()
        # Fail fast with clear error if file doesn't exist
        # (prevents silent creation of empty DB on typoed paths)
        if not db_path.exists():
            typer.echo(f"Error: Database file not found: {db_path}", err=True)
            raise typer.Exit(1) from None
        db_url = f"sqlite:///{db_path}"
    else:
        db_url = settings_config.landscape.url
        _validate_existing_sqlite_db_url(db_url, source="settings.yaml")
        typer.echo(f"Using database from settings.yaml: {db_url}")

    # Resolve SQLCipher passphrase
    from elspeth.cli_helpers import resolve_audit_passphrase

    try:
        passphrase = resolve_audit_passphrase(settings_config.landscape)
    except RuntimeError as e:
        typer.echo(f"Error: {e}", err=True)
        raise typer.Exit(1) from None

    # Initialize database and recovery manager
    try:
        db = LandscapeDB.from_url(db_url, passphrase=passphrase, create_tables=False)
    except Exception as e:
        typer.echo(f"Error connecting to database: {e}", err=True)
        raise typer.Exit(1) from None

    # Verify database has Landscape schema (resume requires existing tables).
    # With create_tables=False, tables are NOT created — so an existing file
    # without Landscape tables (empty DB, wrong file) would crash later with
    # "OperationalError: no such table: runs". Validate upfront.
    from sqlalchemy import inspect as sa_inspect

    try:
        inspector = sa_inspect(db.engine)
        existing_tables = set(inspector.get_table_names())
    except Exception as e:
        typer.echo(f"Error inspecting database schema: {e}", err=True)
        db.close()
        raise typer.Exit(1) from None

    required_tables = {"runs", "tokens", "node_states"}
    missing_tables = required_tables - existing_tables
    if missing_tables:
        typer.echo(
            f"Error: Database exists but is not a Landscape database "
            f"(missing tables: {', '.join(sorted(missing_tables))}). "
            f"Check the database path.",
            err=True,
        )
        db.close()
        raise typer.Exit(1) from None

    try:
        checkpoint_manager = CheckpointManager(db)
        recovery_manager = RecoveryManager(db, checkpoint_manager)

        # Instantiate plugins once — reused for validation graph, execution graph, and sink checks
        from elspeth.cli_helpers import instantiate_plugins_from_config

        try:
            plugins = instantiate_plugins_from_config(settings_config)
        except contract_errors.TIER_1_ERRORS:
            raise  # Tier 1 errors must crash with full traceback, not Exit(1)
        except Exception as e:
            typer.echo(f"Error instantiating plugins: {e}", err=True)
            raise typer.Exit(1) from None

        # Build both graphs from the same plugin instances
        try:
            validation_graph, execution_graph = _build_resume_graphs(settings_config, plugins)
        except contract_errors.TIER_1_ERRORS:
            raise  # Tier 1 errors must crash with full traceback, not Exit(1)
        except Exception as e:
            typer.echo(f"Error building validation graph: {e}", err=True)
            raise typer.Exit(1) from None

        # Check if run can be resumed (with topology validation)
        check = recovery_manager.can_resume(run_id, validation_graph)

        if not check.can_resume:
            typer.echo(f"Cannot resume run {run_id}: {check.reason}", err=True)
            raise typer.Exit(1)

        # Get resume point information
        resume_point = recovery_manager.get_resume_point(run_id, validation_graph)
        if resume_point is None:
            typer.echo(f"Error: Could not get resume point for run {run_id}", err=True)
            raise typer.Exit(1)

        # Get count of unprocessed rows
        unprocessed_row_ids = recovery_manager.get_unprocessed_rows(run_id)

        # Display resume point information
        resume_info = {
            "run_id": run_id,
            "can_resume": True,
            "resume_point": {
                "token_id": resume_point.token_id,
                "node_id": resume_point.node_id,
                "sequence_number": resume_point.sequence_number,
                "has_aggregation_state": resume_point.aggregation_state is not None,
                "has_coalesce_state": resume_point.coalesce_state is not None,
            },
            "unprocessed_rows": len(unprocessed_row_ids),
        }

        if output_format == "json" and not execute:
            import json as json_module

            resume_info["dry_run"] = True
            typer.echo(json_module.dumps(resume_info, indent=2))
            return

        if output_format != "json":
            typer.echo(f"Run {run_id} can be resumed.")
            typer.echo("\nResume point:")
            typer.echo(f"  Token ID: {resume_point.token_id}")
            typer.echo(f"  Node ID: {resume_point.node_id}")
            typer.echo(f"  Sequence number: {resume_point.sequence_number}")
            if resume_point.aggregation_state is not None:
                typer.echo("  Has aggregation state: Yes")
            else:
                typer.echo("  Has aggregation state: No")
            if resume_point.coalesce_state is not None:
                typer.echo("  Has coalesce state: Yes")
            else:
                typer.echo("  Has coalesce state: No")
            typer.echo(f"  Unprocessed rows: {len(unprocessed_row_ids)}")

        if not execute:
            if output_format != "json":
                typer.echo("\nDry run - use --execute to actually resume processing.")
                typer.echo("Topology validation passed - checkpoint is compatible with current config.")
            return

        # Execute resume (graph already built above for validation)
        if output_format != "json":
            typer.echo(f"\nResuming run {run_id}...")

        # Get payload store from settings
        from elspeth.core.payload_store import FilesystemPayloadStore

        if settings_config.payload_store.backend != "filesystem":
            typer.echo(
                f"Error: Unsupported payload store backend '{settings_config.payload_store.backend}'. "
                f"Only 'filesystem' is currently supported.",
                err=True,
            )
            raise typer.Exit(1)

        payload_path = settings_config.payload_store.base_path
        if not payload_path.exists():
            typer.echo(f"Error: Payload directory not found: {payload_path}", err=True)
            raise typer.Exit(1)

        payload_store = FilesystemPayloadStore(payload_path)

        # CRITICAL: Validate and configure sinks for resume mode
        # Uses the already-instantiated sinks from plugins dict
        from elspeth.plugins.sources.null_source import NullSource

        resume_sinks = {}

        for sink_name, sink in plugins.sinks.items():
            # Check if sink supports resume
            if not sink.supports_resume:
                typer.echo(
                    f"Error: Cannot resume with sink '{sink_name}' (plugin: {sink.name}). "
                    f"This sink does not support resume/append mode.\n"
                    f"Hint: Use a different sink type or start a new run.",
                    err=True,
                )
                raise typer.Exit(1)

            # Configure sink for resume (switches to append mode)
            try:
                sink.configure_for_resume()
            except NotImplementedError as e:
                typer.echo(f"Error: {e}", err=True)
                raise typer.Exit(1) from None

            # For sinks with headers: original, provide field resolution
            # mapping BEFORE validation so they can correctly compare display names
            if sink.needs_resume_field_resolution:
                from elspeth.core.landscape.factory import RecorderFactory

                factory = RecorderFactory(db)
                field_resolution = factory.run_lifecycle.get_source_field_resolution(run_id)
                if field_resolution is None:
                    typer.echo(
                        f"Error: Cannot resume with sink '{sink_name}' (plugin: {sink.name}). "
                        "This sink uses headers: original but the source field-resolution mapping "
                        "is missing from the audit trail.",
                        err=True,
                    )
                    raise typer.Exit(1)
                try:
                    sink.set_resume_field_resolution(field_resolution)
                except (NotImplementedError, ValueError) as e:
                    typer.echo(f"Error: Cannot resume with sink '{sink_name}': {e}", err=True)
                    raise typer.Exit(1) from None

            # Validate output target schema compatibility
            validation = sink.validate_output_target()
            if not validation.valid:
                typer.echo(
                    f"\nError: Cannot resume with sink '{sink_name}'.\nOutput target schema mismatch: {validation.error_message}",
                    err=True,
                )
                if validation.missing_fields:
                    typer.echo(f"  Missing fields: {list(validation.missing_fields)}", err=True)
                if validation.extra_fields:
                    typer.echo(f"  Extra fields: {list(validation.extra_fields)}", err=True)
                if validation.order_mismatch:
                    typer.echo(
                        "  Note: Fields present but in wrong order (strict mode)",
                        err=True,
                    )
                typer.echo(
                    "\nHint: Either fix the output file to match schema, or start a new run.",
                    err=True,
                )
                raise typer.Exit(1)

            resume_sinks[sink_name] = sink

        # Override source with NullSource for resume (data comes from payloads)
        from dataclasses import replace

        null_source_on_success = plugins.source.on_success
        null_source = NullSource({})
        null_source.on_success = null_source_on_success
        resume_plugins = replace(
            plugins,
            source=null_source,
            sinks=resume_sinks,  # Use append-mode sinks
        )

        # Execute resume with execution graph (NullSource)
        try:
            result = _execute_resume_with_instances(
                config=settings_config,
                graph=execution_graph,
                plugins=resume_plugins,
                resume_point=resume_point,
                payload_store=payload_store,
                db=db,
                output_format=output_format,
            )
        except GracefulShutdownError as e:
            if output_format == "json":
                import json as json_mod

                typer.echo(
                    json_mod.dumps(
                        {
                            "event": "interrupted",
                            "run_id": e.run_id,
                            "rows_processed": e.rows_processed,
                            "message": str(e),
                        }
                    )
                )
            else:
                typer.echo(f"\nResume interrupted after {e.rows_processed} rows.")
                typer.echo(f"Resume with: elspeth resume {e.run_id} --execute")
            raise typer.Exit(3)  # noqa: B904 -- distinct exit code: 0=success, 1=error, 3=interrupted
        except contract_errors.TIER_1_ERRORS as e:
            # Tier 1 violations and framework bugs MUST be clearly distinguishable
            # from config errors — same pattern as the `run` command handler.
            import traceback

            if output_format == "json":
                import json as json_mod_fatal

                typer.echo(
                    json_mod_fatal.dumps(
                        {
                            "event": "fatal",
                            "error": str(e),
                            "error_type": type(e).__name__,
                            "traceback": traceback.format_exc(),
                        }
                    ),
                    err=True,
                )
            else:
                typer.echo(f"\nFATAL — {type(e).__name__}: {e}", err=True)
                typer.echo(traceback.format_exc(), err=True)
            raise typer.Exit(4) from e  # Exit 4: audit integrity / framework bug
        except Exception as e:
            import traceback

            if output_format == "json":
                import json as json_mod_err

                typer.echo(
                    json_mod_err.dumps(
                        {
                            "event": "error",
                            "error": str(e),
                            "error_type": type(e).__name__,
                            "traceback": traceback.format_exc(),
                        }
                    ),
                    err=True,
                )
            else:
                typer.echo(f"Error during resume: {e}", err=True)
                typer.echo(traceback.format_exc(), err=True)
            raise typer.Exit(4) from e  # Exit 4: unknown errors during execution are likely framework bugs

        if output_format == "json":
            import json as json_module

            typer.echo(
                json_module.dumps(
                    {
                        **resume_info,
                        "result": {
                            "rows_processed": result.rows_processed,
                            "rows_succeeded": result.rows_succeeded,
                            "rows_failed": result.rows_failed,
                            "status": result.status.value,
                        },
                    },
                    indent=2,
                )
            )
        else:
            typer.echo("\nResume complete:")
            typer.echo(f"  Rows processed: {result.rows_processed}")
            typer.echo(f"  Rows succeeded: {result.rows_succeeded}")
            typer.echo(f"  Rows failed: {result.rows_failed}")
            typer.echo(f"  Status: {result.status.value}")

    finally:
        db.close()


@app.command()
def health(
    ctx: typer.Context,
    json_output: bool = typer.Option(
        False,
        "--json",
        "-j",
        help="Output as JSON.",
    ),
    verbose: bool = typer.Option(
        False,
        "--verbose",
        "-v",
        help="Include detailed check information.",
    ),
    host: str | None = typer.Option(
        None,
        "--host",
        help="Web server host to check. Defaults to ELSPETH_WEB__HOST or 127.0.0.1.",
    ),
    port: int | None = typer.Option(
        None,
        "--port",
        "-p",
        help="Web server port to check. Defaults to ELSPETH_WEB__PORT or 8451.",
    ),
    skip_web: bool = typer.Option(
        True,
        "--skip-web/--check-web",
        help="Skip web interface check. Default: skip (batch containers). Use --check-web to probe.",
    ),
) -> None:
    """Check system health for deployment verification.

    Verifies that ELSPETH is properly configured and can connect to
    required services. Used by deployment scripts and container health checks.

    Examples:

        # Basic health check (batch/CLI containers — skips web by default)
        elspeth health

        # JSON output for automation
        elspeth health --json

        # Check web server (for web containers)
        elspeth health --check-web

        # Check web server on custom port
        elspeth health --check-web --port 9000

        # Verbose with details
        elspeth health --verbose
    """
    import json as json_module
    import subprocess
    import sys

    from click.core import ParameterSource

    # Infer web checking from explicit host/port arguments.
    # If user provides --host or --port, they likely want web checking.
    # BUT: honor explicit --skip-web even when host/port are supplied, so
    # wrappers can always pass host/port but conditionally suppress the check.
    skip_web_source = ctx.get_parameter_source("skip_web")
    skip_web_was_default = skip_web_source == ParameterSource.DEFAULT
    if (host is not None or port is not None) and skip_web and skip_web_was_default:
        skip_web = False

    # Health check results
    checks: dict[str, dict[str, str | bool]] = {}
    overall_healthy = True

    # Check 1: Version (always passes if we got here)
    checks["version"] = {
        "status": "ok",
        "value": __version__,
    }

    # Check 2: Git commit SHA (if available)
    git_sha = os.environ.get("GIT_COMMIT_SHA", "")
    if not git_sha:
        # Try to get from git
        try:
            git_result = subprocess.run(
                ["git", "rev-parse", "--short", "HEAD"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            if git_result.returncode == 0:
                git_sha = git_result.stdout.strip()
        except (subprocess.SubprocessError, FileNotFoundError, OSError):
            git_sha = "unavailable"

    checks["commit"] = {
        "status": "ok" if git_sha and git_sha != "unavailable" else "warn",
        "value": git_sha or "unavailable",
    }

    # Check 3: Python version
    python_version = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    checks["python"] = {
        "status": "ok",
        "value": python_version,
    }

    # Check 4: Database connectivity (if DATABASE_URL is set)
    db_url = os.environ.get("DATABASE_URL", "")
    if db_url:
        try:
            from sqlalchemy import create_engine, text

            engine = create_engine(db_url)
            with engine.connect() as conn:
                conn.execute(text("SELECT 1"))
            checks["database"] = {
                "status": "ok",
                "value": "connected",
            }
        except Exception as e:
            checks["database"] = {
                "status": "error",
                "value": str(e),
            }
            overall_healthy = False
    else:
        checks["database"] = {
            "status": "skip",
            "value": "DATABASE_URL not set",
        }

    # Check 5: Config directory readable (container path)
    config_paths = ["/app/config", "./config"]
    config_readable = False
    config_path_used = ""
    for path in config_paths:
        if os.path.isdir(path) and os.access(path, os.R_OK):
            config_readable = True
            config_path_used = path
            break

    if config_readable:
        checks["config_dir"] = {
            "status": "ok",
            "value": config_path_used,
        }
    else:
        checks["config_dir"] = {
            "status": "warn",
            "value": "not found or not readable",
        }

    # Check 6: Output directory writable (container path)
    output_paths = ["/app/output", "./output"]
    output_writable = False
    output_path_used = ""
    for path in output_paths:
        if os.path.isdir(path) and os.access(path, os.W_OK):
            output_writable = True
            output_path_used = path
            break

    if output_writable:
        checks["output_dir"] = {
            "status": "ok",
            "value": output_path_used,
        }
    else:
        checks["output_dir"] = {
            "status": "warn",
            "value": "not found or not writable",
        }

    # Check 7: Plugins loaded
    try:
        from elspeth.plugins.infrastructure.manager import get_shared_plugin_manager

        manager = get_shared_plugin_manager()
        source_count = len(manager.get_sources())
        transform_count = len(manager.get_transforms())
        sink_count = len(manager.get_sinks())
        checks["plugins"] = {
            "status": "ok",
            "value": f"{source_count} sources, {transform_count} transforms, {sink_count} sinks",
        }
    except Exception as e:
        checks["plugins"] = {
            "status": "error",
            "value": str(e),
        }
        overall_healthy = False

    # Check 8: Web interface reachable
    # Priority: CLI flags > env vars > defaults
    # Note: env vars must be set externally (e.g., docker -e) — they are NOT
    # automatically propagated from `elspeth web` which runs in a separate process.
    if skip_web:
        checks["web"] = {
            "status": "skip",
            "value": "skipped via --skip-web",
        }
    else:
        import urllib.error
        import urllib.request

        web_host_resolved = host or os.environ.get("ELSPETH_WEB__HOST", "127.0.0.1")
        # Wildcard bind addresses (0.0.0.0, ::) are listen-all addresses, not
        # connectable destinations. Translate to loopback for probing.
        _wildcard_to_loopback = {"0.0.0.0": "127.0.0.1", "::": "::1"}
        probe_host = _wildcard_to_loopback.get(web_host_resolved, web_host_resolved)
        # RFC 3986: IPv6 literals in URIs must be bracketed to distinguish
        # port separator from address colons. "::1" → "[::1]".
        host_for_url = f"[{probe_host}]" if ":" in probe_host else probe_host
        try:
            # Parse port inside try so malformed env var produces unhealthy check,
            # not CLI crash. Health probes must distinguish "misconfigured" from "crashed".
            web_port_resolved = port or int(os.environ.get("ELSPETH_WEB__PORT", "8451"))
            web_url = f"http://{host_for_url}:{web_port_resolved}/api/health"

            # Bypass HTTP_PROXY/HTTPS_PROXY only for loopback self-checks.
            # Corporate environments set proxy env vars for outbound traffic,
            # but routing localhost probes through a proxy causes false unhealthy
            # reports. Remote probes must still honor proxy settings.
            is_loopback = probe_host == "localhost" or probe_host == "::1" or probe_host.startswith("127.")
            if is_loopback:
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            else:
                opener = urllib.request.build_opener()  # Default: honors proxy env vars
            with opener.open(web_url, timeout=5) as response:
                body = response.read().decode("utf-8")
                data = json_module.loads(body)
                if data.get("status") == "ok":
                    checks["web"] = {
                        "status": "ok",
                        "value": f"{web_host_resolved}:{web_port_resolved}",
                    }
                else:
                    checks["web"] = {
                        "status": "error",
                        "value": f"unexpected response: {data}",
                    }
                    overall_healthy = False
        except urllib.error.URLError as e:
            checks["web"] = {
                "status": "error",
                "value": f"connection failed: {e.reason}",
            }
            overall_healthy = False
        except Exception as e:
            checks["web"] = {
                "status": "error",
                "value": str(e),
            }
            overall_healthy = False

    # Determine overall status
    status = "healthy" if overall_healthy else "unhealthy"

    # Output results
    if json_output:
        result = {
            "status": status,
            "version": __version__,
            "commit": git_sha,
            "checks": checks,
        }
        typer.echo(json_module.dumps(result, indent=2))
    else:
        typer.echo(f"Status: {status}")
        typer.echo(f"Version: {__version__}")
        typer.echo(f"Commit: {git_sha}")

        if verbose:
            typer.echo("\nChecks:")
            for name, info in checks.items():
                check_status = info["status"]
                if check_status == "ok":
                    status_icon = "✓"
                elif check_status in ("warn", "skip"):
                    status_icon = "⚠"
                else:
                    status_icon = "✗"
                typer.echo(f"  {status_icon} {name}: {info['value']}")

    # Exit with appropriate code
    if not overall_healthy:
        raise typer.Exit(1)


@app.command()
def web(
    port: int = typer.Option(8451, help="Port to listen on"),
    host: str = typer.Option("127.0.0.1", help="Host to bind to"),
    auth: str = typer.Option("local", help="Auth provider: local, oidc, entra"),
    reload: bool = typer.Option(False, help="Enable auto-reload for development"),
) -> None:
    """Start the ELSPETH web application."""
    try:
        import uvicorn
    except ImportError:
        typer.echo(
            "Error: Web UI requires the [webui] extra. Install with: uv pip install -e '.[webui]'",
            err=True,
        )
        raise typer.Exit(1) from None

    # Fail-fast guard: non-local host with no explicit secret key is always wrong.
    # Full validation happens in create_app(), but this catches the obvious case
    # before uvicorn starts — deployments that set ELSPETH_WEB__SECRET_KEY pass.
    _local_hosts = {"127.0.0.1", "localhost", "::1"}
    if host not in _local_hosts and "ELSPETH_WEB__SECRET_KEY" not in os.environ:
        typer.echo(
            "Error: Non-local host requires an explicit secret_key. "
            "Set ELSPETH_WEB__SECRET_KEY or use --host 127.0.0.1 for local development.",
            err=True,
        )
        raise typer.Exit(1)

    # Bridge CLI args to create_app() via environment variables.
    # uvicorn's factory protocol calls create_app() with no arguments,
    # so we set ELSPETH_WEB__* env vars that _settings_from_env() reads.
    os.environ["ELSPETH_WEB__HOST"] = host
    os.environ["ELSPETH_WEB__PORT"] = str(port)
    os.environ["ELSPETH_WEB__AUTH_PROVIDER"] = auth

    uvicorn.run(
        "elspeth.web.app:create_app",
        host=host,
        port=port,
        reload=reload,
        factory=True,
    )


if __name__ == "__main__":
    app()
