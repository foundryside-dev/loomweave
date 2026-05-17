"""MCP server for ELSPETH Landscape audit database analysis.

A lightweight read-only server that exposes tools for querying
the audit trail. Uses the existing LandscapeDB and RecorderFactory
infrastructure.

Usage:
    # Direct execution
    python -m elspeth.mcp.server --database sqlite:///./state/audit.db

    # Or as an MCP server
    elspeth-mcp --database sqlite:///./state/audit.db

The analyzer logic lives in ``mcp.analyzer`` (facade) and
``mcp.analyzers.*`` (domain submodules). This file contains only
MCP protocol machinery: argument validation, tool registration,
dispatcher, CLI entry point.
"""

import argparse
import json
import logging
import sys
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any

from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import CallToolResult as CallToolResult
from mcp.types import TextContent, Tool

from elspeth.contracts.enums import RunStatus
from elspeth.mcp.analyzer import LandscapeAnalyzer

__all__ = [
    "CallToolResult",
    "create_server",
    "main",
    "run_server",
]

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class _ArgSpec:
    """Declarative schema for one MCP tool's arguments."""

    required_str: tuple[str, ...] = ()
    optional_str: tuple[str, ...] = ()  # defaults to None
    optional_str_defaults: tuple[tuple[str, str], ...] = ()  # (name, default)
    optional_int: tuple[tuple[str, int], ...] = ()  # (name, default)
    optional_int_min: tuple[tuple[str, int], ...] = ()  # (name, minimum) — semantic bounds
    optional_bool: tuple[tuple[str, bool], ...] = ()  # (name, default)
    optional_dict: tuple[str, ...] = ()  # defaults to None


@dataclass(frozen=True, slots=True)
class _ToolDef:
    """Complete definition for one MCP tool — single source of truth.

    Combines argument validation spec, JSON Schema for MCP registration,
    human-readable description, and dispatch handler. The list_tools()
    and call_tool() functions derive their behavior entirely from this.
    """

    description: str
    args: _ArgSpec
    handler: Callable[[LandscapeAnalyzer, dict[str, Any]], Any]
    schema_properties: Mapping[str, Any]

    def __post_init__(self) -> None:
        object.__setattr__(self, "schema_properties", MappingProxyType(dict(self.schema_properties)))


_TOOLS: dict[str, _ToolDef] = {
    "list_runs": _ToolDef(
        description="List pipeline runs with optional status filter",
        args=_ArgSpec(
            optional_int=(("limit", 50),),
            optional_int_min=(("limit", 1),),
            optional_str=("status",),
        ),
        handler=lambda a, args: a.list_runs(
            limit=args["limit"],
            status=args["status"],
        ),
        schema_properties={
            "limit": {"type": "integer", "description": "Max runs to return (default 50)", "default": 50},
            "status": {
                "type": ["string", "null"],
                "description": "Filter by status",
                "enum": [s.value for s in RunStatus],
            },
        },
    ),
    "get_run": _ToolDef(
        description="Get details of a specific pipeline run",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.get_run(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to retrieve"},
        },
    ),
    "get_run_summary": _ToolDef(
        description="Get summary statistics for a run: counts, durations, errors, outcome distribution",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.get_run_summary(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to analyze"},
        },
    ),
    "list_nodes": _ToolDef(
        description="List all nodes (plugin instances) for a run",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.list_nodes(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
        },
    ),
    "list_rows": _ToolDef(
        description="List source rows for a run with pagination",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_int=(("limit", 100), ("offset", 0)),
            optional_int_min=(("limit", 1), ("offset", 0)),
        ),
        handler=lambda a, args: a.list_rows(
            run_id=args["run_id"],
            limit=args["limit"],
            offset=args["offset"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
            "limit": {"type": "integer", "description": "Max rows (default 100)", "default": 100},
            "offset": {"type": "integer", "description": "Rows to skip (default 0)", "default": 0},
        },
    ),
    "list_tokens": _ToolDef(
        description="List tokens for a run or specific row",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_str=("row_id",),
            optional_int=(("limit", 100),),
            optional_int_min=(("limit", 1),),
        ),
        handler=lambda a, args: a.list_tokens(
            run_id=args["run_id"],
            row_id=args["row_id"],
            limit=args["limit"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
            "row_id": {"type": ["string", "null"], "description": "Optional row ID to filter by"},
            "limit": {"type": "integer", "description": "Max tokens (default 100)", "default": 100},
        },
    ),
    "get_token_children": _ToolDef(
        description="Get child tokens created from a parent (forward lineage) — trace what a COALESCED token merged into",
        args=_ArgSpec(required_str=("parent_token_id",)),
        handler=lambda a, args: a.get_token_children(args["parent_token_id"]),
        schema_properties={
            "parent_token_id": {"type": "string", "description": "Token ID to find children for"},
        },
    ),
    "list_operations": _ToolDef(
        description="List source/sink operations for a run (blob downloads, file writes, database inserts)",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_str=("operation_type", "status"),
            optional_int=(("limit", 100),),
            optional_int_min=(("limit", 1),),
        ),
        handler=lambda a, args: a.list_operations(
            run_id=args["run_id"],
            operation_type=args["operation_type"],
            status=args["status"],
            limit=args["limit"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
            "operation_type": {
                "type": ["string", "null"],
                "description": "Filter by type",
                "enum": ["source_load", "sink_write"],
            },
            "status": {
                "type": ["string", "null"],
                "description": "Filter by status",
                "enum": ["open", "completed", "failed", "pending"],
            },
            "limit": {"type": "integer", "description": "Max operations (default 100)", "default": 100},
        },
    ),
    "get_operation_calls": _ToolDef(
        description="Get external calls (HTTP, SQL, etc.) made during a source/sink operation",
        args=_ArgSpec(required_str=("operation_id",)),
        handler=lambda a, args: a.get_operation_calls(args["operation_id"]),
        schema_properties={
            "operation_id": {"type": "string", "description": "Operation ID to query"},
        },
    ),
    "explain_token": _ToolDef(
        description="Get complete lineage for a token: source row, node states, calls, routing, errors, outcome",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_str=("token_id", "row_id", "sink"),
        ),
        handler=lambda a, args: a.explain_token(
            run_id=args["run_id"],
            token_id=args["token_id"],
            row_id=args["row_id"],
            sink=args["sink"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID"},
            "token_id": {"type": ["string", "null"], "description": "Token ID (preferred for DAGs with forks)"},
            "row_id": {"type": ["string", "null"], "description": "Row ID (alternative to token_id)"},
            "sink": {"type": ["string", "null"], "description": "Sink name to disambiguate multiple terminals"},
        },
    ),
    "get_errors": _ToolDef(
        description="Get validation and/or transform errors for a run",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_str_defaults=(("error_type", "all"),),
            optional_int=(("limit", 100),),
            optional_int_min=(("limit", 1),),
        ),
        handler=lambda a, args: a.get_errors(
            run_id=args["run_id"],
            error_type=args["error_type"],
            limit=args["limit"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
            "error_type": {
                "type": "string",
                "description": "Error type filter",
                "enum": ["all", "validation", "transform"],
                "default": "all",
            },
            "limit": {"type": "integer", "description": "Max errors per type (default 100)", "default": 100},
        },
    ),
    "get_node_states": _ToolDef(
        description="Get node states (processing records) for a run",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_str=("node_id", "status"),
            optional_int=(("limit", 100),),
            optional_int_min=(("limit", 1),),
            optional_bool=(("include_context", False),),
        ),
        handler=lambda a, args: a.get_node_states(
            run_id=args["run_id"],
            node_id=args["node_id"],
            status=args["status"],
            limit=args["limit"],
            include_context=args["include_context"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
            "node_id": {"type": ["string", "null"], "description": "Optional node ID filter"},
            "status": {
                "type": ["string", "null"],
                "description": "Optional status filter",
                "enum": ["open", "pending", "completed", "failed"],
            },
            "limit": {"type": "integer", "description": "Max states (default 100)", "default": 100},
            "include_context": {
                "type": "boolean",
                "description": "Include context_after, error, and success_reason JSON fields (large, expensive)",
                "default": False,
            },
        },
    ),
    "list_collisions": _ToolDef(
        description="List coalesce collision events — shows merge conflicts with winner/loser values",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_int=(("limit", 100),),
            optional_int_min=(("limit", 1),),
        ),
        handler=lambda a, args: a.list_collisions(
            run_id=args["run_id"],
            limit=args["limit"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
            "limit": {"type": "integer", "description": "Max collision records (default 100)", "default": 100},
        },
    ),
    "get_calls": _ToolDef(
        description="Get external calls (LLM, HTTP, etc.) for a node state",
        args=_ArgSpec(required_str=("state_id",)),
        handler=lambda a, args: a.get_calls(args["state_id"]),
        schema_properties={
            "state_id": {"type": "string", "description": "Node state ID"},
        },
    ),
    "query": _ToolDef(
        description="Execute a read-only SQL query against the audit database (SELECT only)",
        args=_ArgSpec(
            required_str=("sql",),
            optional_dict=("params",),
        ),
        handler=lambda a, args: a.query(
            sql=args["sql"],
            params=args["params"],
        ),
        schema_properties={
            "sql": {"type": "string", "description": "SQL SELECT query"},
            "params": {"type": ["object", "null"], "description": "Optional query parameters"},
        },
    ),
    "get_dag_structure": _ToolDef(
        description="Get the DAG structure for a run: nodes, edges, and mermaid diagram for visualization",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.get_dag_structure(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to analyze"},
        },
    ),
    "get_performance_report": _ToolDef(
        description="Get performance analysis: slow nodes, bottlenecks, timing statistics, high-variance nodes",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.get_performance_report(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to analyze"},
        },
    ),
    "get_error_analysis": _ToolDef(
        description="Analyze errors: grouped by type, by node, with sample data for pattern matching",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.get_error_analysis(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to analyze"},
        },
    ),
    "get_llm_usage_report": _ToolDef(
        description="Get LLM usage statistics: call counts, latencies, success rates by plugin",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.get_llm_usage_report(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to analyze"},
        },
    ),
    "describe_schema": _ToolDef(
        description="Describe the database schema: tables, columns, primary keys, foreign keys (for ad-hoc SQL)",
        args=_ArgSpec(),
        handler=lambda a, args: a.describe_schema(),
        schema_properties={},
    ),
    "get_outcome_analysis": _ToolDef(
        description="Analyze token outcomes: terminal states, fork/join patterns, sink routing distribution",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.get_outcome_analysis(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to analyze"},
        },
    ),
    "diagnose": _ToolDef(
        description="\U0001f6a8 EMERGENCY: What's broken right now? Scans for failed runs, stuck runs, high error rates",
        args=_ArgSpec(),
        handler=lambda a, args: a.diagnose(),
        schema_properties={},
    ),
    "get_failure_context": _ToolDef(
        description="\U0001f50d Deep dive: Get comprehensive context about failures in a run (failed states, errors, patterns)",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_int=(("limit", 10),),
            optional_int_min=(("limit", 1),),
        ),
        handler=lambda a, args: a.get_failure_context(
            run_id=args["run_id"],
            limit=args["limit"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to investigate"},
            "limit": {"type": "integer", "description": "Max failures to return", "default": 10},
        },
    ),
    "get_recent_activity": _ToolDef(
        description="\U0001f4ca Timeline: What happened recently? Shows runs in the last N minutes",
        args=_ArgSpec(
            optional_int=(("minutes", 60),),
            optional_int_min=(("minutes", 1),),
        ),
        handler=lambda a, args: a.get_recent_activity(
            minutes=args["minutes"],
        ),
        schema_properties={
            "minutes": {"type": "integer", "description": "Look back this many minutes", "default": 60},
        },
    ),
    "get_run_contract": _ToolDef(
        description="Get schema contract for a run: mode, field mappings (original -> normalized), and inferred types",
        args=_ArgSpec(required_str=("run_id",)),
        handler=lambda a, args: a.get_run_contract(args["run_id"]),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
        },
    ),
    "explain_field": _ToolDef(
        description="Trace a field's provenance: how it was named at source, normalized, and typed",
        args=_ArgSpec(required_str=("run_id", "field_name")),
        handler=lambda a, args: a.explain_field(
            run_id=args["run_id"],
            field_name=args["field_name"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
            "field_name": {"type": "string", "description": "Field name (normalized or original)"},
        },
    ),
    "list_contract_violations": _ToolDef(
        description="List contract violations: type mismatches, missing fields, with field names and type info",
        args=_ArgSpec(
            required_str=("run_id",),
            optional_int=(("limit", 100),),
            optional_int_min=(("limit", 1),),
        ),
        handler=lambda a, args: a.list_contract_violations(
            run_id=args["run_id"],
            limit=args["limit"],
        ),
        schema_properties={
            "run_id": {"type": "string", "description": "Run ID to query"},
            "limit": {"type": "integer", "description": "Max violations to return (default 100)", "default": 100},
        },
    ),
}


def _validate_tool_args(name: str, arguments: dict[str, Any]) -> dict[str, Any]:
    """Validate MCP tool arguments at the Tier 3 boundary.

    Checks required fields exist, validates types (str, int, dict), and
    applies defaults for optional fields. Returns a new dict with only
    the declared fields, preventing unexpected keys from leaking through.

    Raises:
        ValueError: Missing required field or unknown tool.
        TypeError: Field has wrong type.
    """
    defn = _TOOLS.get(name)
    if defn is None:
        raise ValueError(f"Unknown tool: {name}")

    spec = defn.args
    validated: dict[str, Any] = {}

    for fname in spec.required_str:
        if fname not in arguments:
            raise ValueError(f"'{name}' requires '{fname}'")
        val = arguments[fname]
        if not isinstance(val, str):
            raise TypeError(f"'{name}': '{fname}' must be string, got {type(val).__name__}")
        validated[fname] = val

    for fname in spec.optional_str:
        val = arguments.get(fname)
        if val is not None and not isinstance(val, str):
            raise TypeError(f"'{name}': '{fname}' must be string or null, got {type(val).__name__}")
        validated[fname] = val

    for fname, str_default in spec.optional_str_defaults:
        val = arguments.get(fname, str_default)
        if not isinstance(val, str):
            raise TypeError(f"'{name}': '{fname}' must be string, got {type(val).__name__}")
        validated[fname] = val

    int_mins = dict(spec.optional_int_min)
    for fname, int_default in spec.optional_int:
        val = arguments.get(fname, int_default)
        # JSON has no int/float distinction — accept both, convert to int
        if isinstance(val, float) and val == int(val):
            val = int(val)
        if not isinstance(val, int) or isinstance(val, bool):
            raise TypeError(f"'{name}': '{fname}' must be integer, got {type(val).__name__}")
        if fname in int_mins and val < int_mins[fname]:
            raise ValueError(f"'{name}': '{fname}' must be >= {int_mins[fname]}, got {val}")
        validated[fname] = val

    for fname, bool_default in spec.optional_bool:
        val = arguments.get(fname, bool_default)
        if not isinstance(val, bool):
            raise TypeError(f"'{name}': '{fname}' must be boolean, got {type(val).__name__}")
        validated[fname] = val

    for fname in spec.optional_dict:
        val = arguments.get(fname)
        if val is not None and not isinstance(val, dict):
            raise TypeError(f"'{name}': '{fname}' must be object or null, got {type(val).__name__}")
        validated[fname] = val

    return validated


def create_server(database_url: str, *, passphrase: str | None = None) -> Server:
    """Create MCP server with Landscape analysis tools.

    Args:
        database_url: SQLAlchemy connection URL
        passphrase: SQLCipher encryption passphrase (if database is encrypted)

    Returns:
        Configured MCP Server
    """
    server = Server("elspeth-landscape")
    analyzer = LandscapeAnalyzer(database_url, passphrase=passphrase)

    @server.list_tools()  # type: ignore[no-untyped-call,untyped-decorator]
    async def list_tools() -> list[Tool]:
        """List available analysis tools.

        Generated from _TOOLS registry — each tool's description, schema
        properties, and required fields are derived from its _ToolDef entry.
        """
        return [
            Tool(
                name=name,
                description=defn.description,
                inputSchema={
                    "type": "object",
                    "properties": defn.schema_properties,
                    **({"required": list(defn.args.required_str)} if defn.args.required_str else {}),
                },
            )
            for name, defn in _TOOLS.items()
        ]

    @server.call_tool()  # type: ignore[untyped-decorator]
    async def call_tool(name: str, arguments: dict[str, Any]) -> CallToolResult | list[TextContent]:
        """Handle tool calls.

        Arguments are validated at the Tier 3 boundary before dispatch.
        ``_validate_tool_args`` checks required fields, types, and defaults.
        The handler is looked up from the ``_TOOLS`` registry.
        """
        # Validate arguments at the Tier 3 boundary — immediately,
        # before any of the external data travels into analyzer methods.
        # ValueError/TypeError from validation are the caller's fault.
        try:
            args = _validate_tool_args(name, arguments)
        except (ValueError, TypeError) as e:
            return CallToolResult(
                content=[TextContent(type="text", text=f"Invalid arguments: {e!s}")],
                isError=True,
            )

        # Dispatch to analyzer — no blanket catch. Database errors,
        # serialization bugs, and analyzer bugs must propagate so they
        # surface as MCP protocol errors rather than silent "Error: ..."
        # text. Tier 1 audit data corruption must never be swallowed.
        defn = _TOOLS[name]  # Validated by _validate_tool_args above

        result = defn.handler(analyzer, args)
        return [TextContent(type="text", text=json.dumps(result, indent=2))]

    return server


async def run_server(database_url: str, *, passphrase: str | None = None) -> None:
    """Run the MCP server with stdio transport."""
    server = create_server(database_url, passphrase=passphrase)
    async with stdio_server() as (read_stream, write_stream):
        await server.run(read_stream, write_stream, server.create_initialization_options())


def _find_audit_databases(search_dir: str, max_depth: int = 5) -> list[str]:
    """Find potential audit databases in the given directory.

    Looks for .db files that might be ELSPETH audit databases,
    prioritizing files named 'audit.db' or 'landscape.db'.

    Args:
        search_dir: Directory to search from
        max_depth: Maximum directory depth to search

    Returns:
        List of absolute paths to found database files, sorted by relevance
    """
    from pathlib import Path

    found: list[tuple[int, float, str]] = []  # (priority, neg_mtime, path)
    search_path = Path(search_dir).resolve()

    for db_file in search_path.rglob("*.db"):
        # Skip hidden directories and common non-audit locations
        parts = db_file.relative_to(search_path).parts
        if any(p.startswith(".") for p in parts):
            continue
        if len(parts) > max_depth:
            continue
        if "node_modules" in parts or "__pycache__" in parts:
            continue

        # Prioritize by name and location
        name = db_file.name.lower()
        in_runs_dir = "runs" in parts

        # Databases in runs/ directories are likely active pipeline outputs
        if in_runs_dir and name == "audit.db":
            priority = 0
        elif in_runs_dir and "audit" in name:
            priority = 1
        elif name == "audit.db":
            priority = 2
        elif name == "landscape.db":
            priority = 3
        elif "audit" in name:
            priority = 4
        elif "landscape" in name:
            priority = 5
        else:
            priority = 10

        # Get modification time for sorting (most recent first)
        try:
            mtime = db_file.stat().st_mtime
        except OSError:
            mtime = 0

        found.append((priority, -mtime, str(db_file)))

    # Sort by priority, then by modification time (most recent first via negative mtime)
    found.sort(key=lambda x: (x[0], x[1]))
    return [path for _, _, path in found]


def _prompt_for_database(databases: list[str], search_dir: str) -> str | None:
    """Prompt user to select a database from the list.

    Args:
        databases: List of database paths
        search_dir: Directory that was searched (for display)

    Returns:
        Selected database path, or None if user cancelled
    """
    from pathlib import Path

    search_path = Path(search_dir).resolve()

    sys.stderr.write(f"\nFound {len(databases)} database(s) in {search_path}:\n\n")

    for i, db_path in enumerate(databases, 1):
        # Show relative path if possible
        try:
            rel_path = Path(db_path).relative_to(search_path)
            display = f"./{rel_path}"
        except ValueError:
            display = db_path
        sys.stderr.write(f"  [{i}] {display}\n")

    sys.stderr.write("\n")

    while True:
        sys.stderr.write("Select database [1]: ")
        sys.stderr.flush()

        try:
            choice = input().strip()
        except (EOFError, KeyboardInterrupt):
            sys.stderr.write("\nCancelled.\n")
            return None

        if not choice:
            choice = "1"

        try:
            idx = int(choice) - 1
            if 0 <= idx < len(databases):
                return databases[idx]
            sys.stderr.write(f"Please enter a number between 1 and {len(databases)}\n")
        except ValueError:
            sys.stderr.write("Please enter a number\n")


def main() -> None:
    """CLI entry point."""
    parser = argparse.ArgumentParser(
        description="ELSPETH Landscape MCP Server - Audit database analysis tools",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    # Run with SQLite database
    elspeth-mcp --database sqlite:///./state/audit.db

    # Run with PostgreSQL
    elspeth-mcp --database postgresql://user:pass@host/dbname

    # Run with SQLCipher-encrypted database
    export ELSPETH_AUDIT_KEY="my-passphrase"
    elspeth-mcp --database sqlite:///./state/audit.db --passphrase-env ELSPETH_AUDIT_KEY

    # Interactive mode - finds and prompts for databases
    elspeth-mcp

Environment Variables:
    ELSPETH_DATABASE_URL: Default database URL if --database not specified
""",
    )
    parser.add_argument(
        "--database",
        "-d",
        default=None,
        help="Database connection URL (SQLAlchemy format)",
    )
    parser.add_argument(
        "--search-dir",
        default=".",
        help="Directory to search for databases (default: current directory)",
    )
    parser.add_argument(
        "--passphrase-env",
        default=None,
        metavar="VAR",
        help="Environment variable holding the SQLCipher passphrase (required for encrypted databases)",
    )

    args = parser.parse_args()

    # Get database URL from args or environment
    import os

    database_url: str | None = args.database
    if database_url is None and "ELSPETH_DATABASE_URL" in os.environ:
        database_url = os.environ["ELSPETH_DATABASE_URL"]

    if database_url is None:
        # Auto-discovery mode: find databases in search directory
        databases = _find_audit_databases(args.search_dir)

        if not databases:
            sys.stderr.write(f"No .db files found in {os.path.abspath(args.search_dir)}\n")
            sys.stderr.write("Use --database to specify a database URL directly.\n")
            sys.exit(1)

        # Check if we're running interactively (TTY) or as MCP server (stdio)
        is_interactive = sys.stdin.isatty()

        db_path: str
        if len(databases) == 1:
            # Only one database - use it directly
            db_path = databases[0]
            sys.stderr.write(f"Using database: {db_path}\n")
        elif is_interactive:
            # Multiple databases in interactive mode - prompt for selection
            selected = _prompt_for_database(databases, args.search_dir)
            if selected is None:
                sys.exit(1)
            db_path = selected
        else:
            # Multiple databases in non-interactive mode - use best match
            db_path = databases[0]
            sys.stderr.write(f"Auto-selected database: {db_path}\n")
            sys.stderr.write("(Use --database to specify a different one)\n")

        database_url = f"sqlite:///{db_path}"

    # Resolve SQLCipher passphrase when --passphrase-env is provided.
    # Without this flag we pass None — sending a passphrase to a plain
    # SQLite database causes "file is not a database".
    passphrase: str | None = None
    if args.passphrase_env is not None:
        passphrase = os.environ.get(args.passphrase_env)
        if passphrase is None or not passphrase.strip():
            sys.stderr.write(
                f"Error: environment variable {args.passphrase_env} is not set or is empty.\n"
                f'Set it with: export {args.passphrase_env}="your-passphrase"\n'
            )
            sys.exit(1)

    import asyncio

    asyncio.run(run_server(database_url, passphrase=passphrase))


if __name__ == "__main__":
    main()
