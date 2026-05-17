"""MCP server for ELSPETH pipeline composition.

Exposes discovery and mutation tools from the web composer, plus
session management tools, over the MCP protocol. Blob and secret
tools are excluded (no session database or secret service in CLI mode).

Layer: L3 (application). Imports from L0 (contracts), L3 (web.composer,
web.catalog, composer_mcp.session).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import time
import uuid
from collections.abc import Awaitable, Callable, Mapping
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, TypedDict, cast

from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import CallToolResult, TextContent, Tool
from pydantic import BaseModel

from elspeth.composer_mcp.audit import JsonlEventRecorder
from elspeth.composer_mcp.session import InvalidSessionIdError, SessionManager, SessionNotFoundError, _validate_session_id
from elspeth.contracts.composer_audit import (
    ComposerToolInvocation,
    ComposerToolRecorder,
    ComposerToolStatus,
)
from elspeth.contracts.freeze import deep_thaw
from elspeth.core.canonical import canonical_json, stable_hash
from elspeth.web.catalog.protocol import CatalogService
from elspeth.web.composer.audit import build_canonicalization_sentinel
from elspeth.web.composer.protocol import ToolArgumentError
from elspeth.web.composer.redaction import redact_source_storage_path
from elspeth.web.composer.state import CompositionState, PipelineMetadata
from elspeth.web.composer.tools import (
    _DISCOVERY_TOOLS,
    _MUTATION_TOOLS,
    RuntimePreflight,
    _apply_merge_patch,
    execute_tool,
    get_tool_definitions,
    validate_composer_file_sink_collision_policy,
)
from elspeth.web.composer.yaml_generator import generate_yaml
from elspeth.web.execution.runtime_preflight import (
    RuntimePreflightCoordinator,
    RuntimePreflightFailure,
    RuntimePreflightKey,
)
from elspeth.web.execution.schemas import ValidationResult

__all__ = ["create_server", "main"]

logger = logging.getLogger(__name__)


class _ValidationEntryPayload(TypedDict):
    component: str
    message: str
    severity: str


_EdgeContractPayload = TypedDict(
    "_EdgeContractPayload",
    {
        "from": str,
        "to": str,
        "producer_guarantees": list[str],
        "consumer_requires": list[str],
        "missing_fields": list[str],
        "satisfied": bool,
    },
)


class _SemanticEdgeContractPayload(TypedDict):
    from_id: str
    to_id: str
    consumer_plugin: str
    producer_plugin: str | None
    producer_field: str
    consumer_field: str
    outcome: str
    requirement_code: str


class _ValidationPayload(TypedDict):
    is_valid: bool
    errors: list[_ValidationEntryPayload]
    warnings: list[_ValidationEntryPayload]
    suggestions: list[_ValidationEntryPayload]
    edge_contracts: list[_EdgeContractPayload]
    semantic_contracts: list[_SemanticEdgeContractPayload]


# Composer tools exposed via MCP (excludes blob and secret tools).
_COMPOSER_TOOL_NAMES: frozenset[str] = frozenset(_DISCOVERY_TOOLS) | frozenset(_MUTATION_TOOLS)

# Session tool definitions (added on top of filtered composer tools).
_SESSION_TOOL_DEFS: list[dict[str, Any]] = [
    {
        "name": "new_session",
        "description": "Create a new empty composition session. Returns session_id.",
        "parameters": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Pipeline name (default: 'Untitled Pipeline').",
                },
            },
            "required": [],
        },
    },
    {
        "name": "save_session",
        "description": "Save the current composition state to a session file.",
        "parameters": {
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Session ID to save to.",
                },
            },
            "required": ["session_id"],
        },
    },
    {
        "name": "load_session",
        "description": "Load a previously saved composition session.",
        "parameters": {
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Session ID to load.",
                },
            },
            "required": ["session_id"],
        },
    },
    {
        "name": "list_sessions",
        "description": "List all saved composition sessions.",
        "parameters": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "delete_session",
        "description": "Delete a saved composition session.",
        "parameters": {
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Session ID to delete.",
                },
            },
            "required": ["session_id"],
        },
    },
    {
        "name": "generate_yaml",
        "description": "Generate ELSPETH pipeline YAML from the current composition state.",
        "parameters": {"type": "object", "properties": {}, "required": []},
    },
]

_SESSION_TOOL_NAMES: frozenset[str] = frozenset(d["name"] for d in _SESSION_TOOL_DEFS)


def _build_tool_defs() -> list[dict[str, Any]]:
    """Build the combined list of tool definitions for MCP registration.

    Filters the web composer tool definitions to only include discovery
    and mutation tools (excluding blob and secret tools), then appends
    the session management tools.

    Returns:
        List of tool definition dicts with ``name``, ``description``,
        and ``parameters`` keys.
    """
    composer_defs = [d for d in get_tool_definitions() if d["name"] in _COMPOSER_TOOL_NAMES]
    return composer_defs + list(_SESSION_TOOL_DEFS)


def _ensure_serializable(obj: Any) -> Any:
    """Recursively convert Pydantic models and other non-serializable types to plain dicts/lists."""
    if isinstance(obj, BaseModel):
        return obj.model_dump()
    if isinstance(obj, dict):
        return {k: _ensure_serializable(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_ensure_serializable(item) for item in obj]
    return obj


def _state_file_sink_collision_control_error(state: CompositionState) -> str | None:
    """Return an MCP control error for any file sink missing collision policy."""
    for output in state.outputs:
        error = validate_composer_file_sink_collision_policy(
            output.plugin,
            deep_thaw(output.options),
            require_explicit=True,
        )
        if error is not None:
            return f"Output '{output.name}': {error}"
    return None


def _tool_file_sink_collision_control_error(
    tool_name: str,
    arguments: Mapping[str, Any],
    state: CompositionState,
) -> str | None:
    """Validate MCP mutation args that can create or update file sink options."""
    if tool_name == "set_output":
        return validate_composer_file_sink_collision_policy(
            arguments["plugin"],
            arguments.get("options", {}),
            require_explicit=True,
        )

    if tool_name == "set_pipeline":
        for out_args in arguments["outputs"]:
            output_name = out_args.get("sink_name", "?")
            error = validate_composer_file_sink_collision_policy(
                out_args["plugin"],
                out_args.get("options", {}),
                require_explicit=True,
            )
            if error is not None:
                return f"Output '{output_name}': {error}"
        return None

    if tool_name == "patch_output_options":
        current = next((o for o in state.outputs if o.name == arguments["sink_name"]), None)
        if current is None:
            return None
        new_options = _apply_merge_patch(current.options, arguments["patch"])
        return validate_composer_file_sink_collision_policy(
            current.plugin,
            new_options,
            require_explicit=True,
        )

    return None


McpRuntimePreflight = Callable[[CompositionState], Awaitable[ValidationResult]]
SessionScopeProvider = Callable[[], str]


async def _mcp_preview_runtime_preflight(
    state: CompositionState,
    *,
    coordinator: RuntimePreflightCoordinator,
    session_scope: str,
    settings_hash: str,
    timeout_seconds: float,
    run_preflight: McpRuntimePreflight,
) -> ValidationResult:
    key = RuntimePreflightKey(
        session_scope=session_scope,
        state_version=state.version,
        settings_hash=settings_hash,
    )

    async def worker() -> ValidationResult:
        return await asyncio.wait_for(run_preflight(state), timeout=timeout_seconds)

    entry = await coordinator.run(key, worker)
    if isinstance(entry, RuntimePreflightFailure):
        raise entry.original_exc
    return entry


def _dispatch_tool(
    tool_name: str,
    arguments: dict[str, Any],
    state: CompositionState,
    catalog: CatalogService,
    scratch_dir: Path,
    baseline: CompositionState | None = None,
    runtime_preflight: RuntimePreflight | None = None,
) -> dict[str, Any]:
    """Dispatch a tool call and return a result dict.

    Session tools are handled locally. Composer tools delegate to
    ``execute_tool()``. Unknown tools return a failure dict.

    The result dict always has ``success``, ``state`` (serialized
    CompositionState), and may include ``data``.
    """
    if tool_name in _SESSION_TOOL_NAMES:
        return _dispatch_session_tool(tool_name, arguments, state, scratch_dir)

    if tool_name in _COMPOSER_TOOL_NAMES:
        control_error = _tool_file_sink_collision_control_error(tool_name, arguments, state)
        if control_error is not None:
            return {
                "success": False,
                "error": control_error,
                "state": state.to_dict(),
            }
        result = execute_tool(tool_name, arguments, state, catalog, data_dir=None, baseline=baseline, runtime_preflight=runtime_preflight)
        response = result.to_dict()
        response["state"] = result.updated_state.to_dict()
        # Discovery tools return Pydantic models (PluginSummary, PluginSchemaInfo)
        # that aren't JSON-serializable. Recursively convert them.
        if "data" in response:
            response["data"] = _ensure_serializable(response["data"])
        return response

    return {
        "success": False,
        "error": f"Unknown tool: {tool_name}",
        "state": state.to_dict(),
    }


def _edge_contract_to_payload(contract: Any) -> _EdgeContractPayload:
    """Serialize an edge contract without leaking a dict[str, Any] return."""
    payload = contract.to_dict()
    return {
        "from": payload["from"],
        "to": payload["to"],
        "producer_guarantees": payload["producer_guarantees"],
        "consumer_requires": payload["consumer_requires"],
        "missing_fields": payload["missing_fields"],
        "satisfied": payload["satisfied"],
    }


def _semantic_edge_contract_to_payload(
    contract: Any,
) -> _SemanticEdgeContractPayload:
    """Serialize a SemanticEdgeContract for MCP. Field names + enum values only.

    SemanticEdgeContract intentionally has no .to_dict() method —
    serialization happens at consumption sites (HTTP, MCP, tools) so
    L0 stays free of JSON-encoding concerns. The keys here mirror
    the Pydantic SemanticEdgeContractResponse used by /validate.
    """
    return {
        "from_id": contract.from_id,
        "to_id": contract.to_id,
        "consumer_plugin": contract.consumer_plugin,
        "producer_plugin": contract.producer_plugin,
        "producer_field": contract.producer_field,
        "consumer_field": contract.consumer_field,
        "outcome": contract.outcome.value,
        "requirement_code": contract.requirement.requirement_code,
    }


def _validation_to_dict(validation: Any) -> _ValidationPayload:
    """Serialize validation for MCP session-tool error payloads."""
    return {
        "is_valid": validation.is_valid,
        "errors": [entry.to_dict() for entry in validation.errors],
        "warnings": [entry.to_dict() for entry in validation.warnings],
        "suggestions": [entry.to_dict() for entry in validation.suggestions],
        "edge_contracts": [_edge_contract_to_payload(contract) for contract in validation.edge_contracts],
        "semantic_contracts": [_semantic_edge_contract_to_payload(contract) for contract in validation.semantic_contracts],
    }


def _session_id_argument(arguments: dict[str, Any]) -> str:
    """Return a validated session_id or raise the Tier-3 argument exception."""
    try:
        session_id = arguments["session_id"]
    except KeyError as exc:
        raise ToolArgumentError(
            argument="session_id",
            expected="a 12-character lowercase hex string",
            actual_type="missing",
        ) from exc

    try:
        _validate_session_id(session_id)
    except InvalidSessionIdError as exc:
        raise ToolArgumentError(
            argument="session_id",
            expected="a 12-character lowercase hex string",
            actual_type="invalid_session_id",
        ) from exc
    except TypeError as exc:
        raise ToolArgumentError(
            argument="session_id",
            expected="a 12-character lowercase hex string",
            actual_type=type(session_id).__name__,
        ) from exc

    return cast(str, session_id)


def _dispatch_session_tool(
    tool_name: str,
    arguments: dict[str, Any],
    state: CompositionState,
    scratch_dir: Path,
) -> dict[str, Any]:
    """Handle session management tools."""
    manager = SessionManager(scratch_dir)

    if tool_name == "new_session":
        name = arguments.get("name", "Untitled Pipeline")
        session_id, new_state = manager.new_session(name=name)
        manager.save(session_id, new_state)
        return {
            "success": True,
            "data": {"session_id": session_id, "name": name},
            "state": new_state.to_dict(),
        }

    if tool_name == "save_session":
        session_id = _session_id_argument(arguments)
        manager.save(session_id, state)
        return {
            "success": True,
            "data": {"session_id": session_id},
            "state": state.to_dict(),
        }

    if tool_name == "load_session":
        session_id = _session_id_argument(arguments)
        try:
            loaded = manager.load(session_id)
        except SessionNotFoundError:
            return {
                "success": False,
                "error": f"Session not found: {session_id}",
                "state": state.to_dict(),
            }
        return {
            "success": True,
            "data": {"session_id": session_id},
            "state": loaded.to_dict(),
        }

    if tool_name == "list_sessions":
        sessions = manager.list_sessions()
        return {
            "success": True,
            "data": {"sessions": sessions},
            "state": state.to_dict(),
        }

    if tool_name == "delete_session":
        session_id = _session_id_argument(arguments)
        try:
            manager.delete(session_id)
        except SessionNotFoundError:
            return {
                "success": False,
                "error": f"Session not found: {session_id}",
                "state": state.to_dict(),
            }
        return {
            "success": True,
            "data": {"session_id": session_id},
            "state": state.to_dict(),
        }

    if tool_name == "generate_yaml":
        control_error = _state_file_sink_collision_control_error(state)
        if control_error is not None:
            return {
                "success": False,
                "error": control_error,
                "state": state.to_dict(),
            }
        validation = state.validate()
        if not validation.is_valid:
            return {
                "success": False,
                "error": "Current composition state is invalid. Fix validation errors before calling generate_yaml.",
                "validation": _validation_to_dict(validation),
                "state": state.to_dict(),
            }
        yaml_str = generate_yaml(state)
        return {
            "success": True,
            "data": yaml_str,
            "state": state.to_dict(),
        }

    # Should not be reachable — _SESSION_TOOL_NAMES is derived from
    # _SESSION_TOOL_DEFS which is the only caller path.
    raise AssertionError(f"Unhandled session tool: {tool_name}")


def create_server(
    catalog: CatalogService,
    scratch_dir: Path,
    runtime_preflight: McpRuntimePreflight | None = None,
    runtime_preflight_settings_hash: str | None = None,
    runtime_preflight_timeout_seconds: float = 5.0,
    runtime_preflight_coordinator: RuntimePreflightCoordinator | None = None,
    session_scope_provider: SessionScopeProvider | None = None,
    recorder: ComposerToolRecorder | None = None,
) -> Server:
    """Create an MCP server for pipeline composition.

    Args:
        catalog: Plugin catalog for discovery tools.
        scratch_dir: Directory for session persistence.
        runtime_preflight: Optional async callable for runtime-equivalent preflight.
            When provided with runtime_preflight_settings_hash, preview_pipeline
            will include runtime validation results.
        runtime_preflight_settings_hash: Hash of settings relevant to runtime
            validation. Required when runtime_preflight is configured.
        runtime_preflight_timeout_seconds: Per-call timeout for runtime preflight.
        runtime_preflight_coordinator: Shared coordinator for in-flight deduplication.
            When embedded in-process with the web server, pass the same coordinator
            used by ComposerServiceImpl so HTTP and MCP share a single-flight lock.
        session_scope_provider: Optional callable returning the current session scope
            string. When None, scope is derived from scratch_dir and session_id.

    Returns:
        Configured MCP Server ready for stdio transport.
    """
    server = Server("elspeth-composer")
    coordinator = runtime_preflight_coordinator or RuntimePreflightCoordinator()
    session_id_ref: list[str | None] = [None]
    audit_recorder: ComposerToolRecorder = (
        recorder
        if recorder is not None
        else JsonlEventRecorder(
            scratch_dir,
            lambda: session_id_ref[0],
        )
    )

    def current_session_scope() -> str:
        if session_scope_provider is not None:
            return session_scope_provider()
        session_id = session_id_ref[0] if session_id_ref[0] is not None else "unsaved"
        return f"composer-mcp:{scratch_dir.resolve()}:{session_id}"

    # Mutable state container — list-of-one pattern allows the
    # inner closures to mutate without nonlocal.
    initial_state = CompositionState(
        source=None,
        nodes=(),
        edges=(),
        outputs=(),
        metadata=PipelineMetadata(),
        version=1,
    )
    state_ref: list[CompositionState] = [initial_state]
    # B5: Baseline for diff_pipeline — captured at session create/load.
    baseline_ref: list[CompositionState] = [initial_state]

    tool_defs = _build_tool_defs()

    @server.list_tools()  # type: ignore[no-untyped-call,untyped-decorator]
    async def list_tools() -> list[Tool]:
        return [
            Tool(
                name=d["name"],
                description=d["description"],
                inputSchema=d["parameters"],
            )
            for d in tool_defs
        ]

    @server.call_tool()  # type: ignore[untyped-decorator]
    async def call_tool(
        name: str,
        arguments: dict[str, Any],
    ) -> CallToolResult | list[TextContent]:
        runtime_preflight_callback: RuntimePreflight | None = None
        # Audit envelope around the entire dispatch. The try/finally
        # makes "audit fires before return" structurally enforceable —
        # success path, ARG_ERROR path, and PLUGIN_CRASH path all flow
        # through the recorder before this coroutine yields. Mirrors
        # the AuditedLLMClient.chat_completion shape in
        # plugins/infrastructure/clients/llm.py.
        tool_call_id = uuid.uuid4().hex
        started_at = datetime.now(UTC)
        started_ns = time.monotonic_ns()
        version_before = state_ref[0].version
        # Pre-compute canonical args + hash so the audit record is
        # complete on every exit path. canonical_json may itself raise
        # ValueError on non-finite floats (Tier-3 boundary policy);
        # that's a structural input violation that pre-dates the tool
        # dispatch and is recorded as ARG_ERROR.
        try:
            arguments_canonical = canonical_json(arguments)
            arguments_hash = stable_hash(arguments)
            canonicalization_failed: BaseException | None = None
        except (ValueError, TypeError) as canon_exc:
            # Shared sentinel discipline (see web.composer.audit
            # build_canonicalization_sentinel docstring): captures
            # type-name + ``str(exc)`` only for rfc8785 messages
            # (value-free by spec) + sorted top-level argument keys.
            # Audit-trail forensic fingerprint without leak risk.
            sentinel = build_canonicalization_sentinel(canon_exc, arguments)
            arguments_canonical = canonical_json(sentinel)
            arguments_hash = stable_hash(sentinel)
            canonicalization_failed = canon_exc

        result_dict: dict[str, Any] | None = None
        status: ComposerToolStatus = ComposerToolStatus.SUCCESS
        error_class: str | None = None
        error_message: str | None = None
        # Captured for ARG_ERROR/PLUGIN_CRASH paths so result_canonical can
        # mirror what the LLM saw (Solution-architect review H4 — the LLM
        # made a decision against this payload, audit primacy demands it
        # is recorded).
        error_payload_for_audit: dict[str, Any] | None = None
        clear_session_after_audit = False

        def _argument_error_result(exc: Exception) -> CallToolResult:
            nonlocal status, error_class, error_message, error_payload_for_audit
            # Bad LLM arguments only. ToolArgumentError messages are
            # safe by construction; the canonicalization pre-dispatch
            # ValueError path uses class-name only to avoid echoing raw
            # argument values.
            status = ComposerToolStatus.ARG_ERROR
            error_class = type(exc).__name__
            error_message = type(exc).__name__
            # Build a structured payload so the LLM and the audit row
            # see the same string (Solution-architect H4 symmetry fix).
            safe_message = str(exc.args[0]) if type(exc) is ToolArgumentError and exc.args else error_message
            error_payload_for_audit = {
                "error": f"Tool error: {safe_message}",
                "isError": True,
            }
            return CallToolResult(
                content=[TextContent(type="text", text=f"Tool error: {safe_message}")],
                isError=True,
            )

        def _capture_plugin_crash(exc: Exception) -> None:
            nonlocal status, error_class, error_message
            # PLUGIN_CRASH path. CLAUDE.md "Plugin Ownership": let
            # the exception propagate. Record the crash before re-raise
            # so the audit trail captures the bug.
            status = ComposerToolStatus.PLUGIN_CRASH
            error_class = type(exc).__name__
            error_message = type(exc).__name__

        try:
            if canonicalization_failed is not None:
                # Pre-dispatch ARG_ERROR: malformed LLM arguments.
                return _argument_error_result(ValueError(f"arguments not canonicalizable ({type(canonicalization_failed).__name__})"))

            if name == "preview_pipeline" and runtime_preflight is not None:
                try:
                    if runtime_preflight_settings_hash is None:
                        raise ValueError("runtime_preflight_settings_hash is required when runtime_preflight is configured")
                    preview_preflight = await _mcp_preview_runtime_preflight(
                        state_ref[0],
                        coordinator=coordinator,
                        session_scope=current_session_scope(),
                        settings_hash=runtime_preflight_settings_hash,
                        timeout_seconds=runtime_preflight_timeout_seconds,
                        run_preflight=runtime_preflight,
                    )
                except Exception as exc:
                    _capture_plugin_crash(exc)
                    raise

                _captured = preview_preflight

                def _make_mcp_callback(
                    _result: ValidationResult = _captured,
                ) -> RuntimePreflight:
                    def _cb(_state: CompositionState) -> ValidationResult:
                        return _result

                    return _cb

                runtime_preflight_callback = _make_mcp_callback()

            try:
                result_dict = _dispatch_tool(
                    name,
                    arguments,
                    state_ref[0],
                    catalog,
                    scratch_dir,
                    baseline=baseline_ref[0],
                    runtime_preflight=runtime_preflight_callback,
                )
            except ToolArgumentError as exc:
                return _argument_error_result(exc)
            except Exception as exc:
                _capture_plugin_crash(exc)
                raise

            # Success path: handle state update, redaction, and MCP-visible
            # response serialization. Wrap in its own try/except per
            # Solution-architect review H2: a Tier-1 invariant breach reading
            # back our own dispatch output (e.g. CompositionState.from_dict
            # KeyError on a malformed result) MUST be audited as PLUGIN_CRASH,
            # not laundered as SUCCESS. JSON serialization is included because
            # a response that cannot be sent to the client is not a successful
            # dispatch.
            try:
                if "state" in result_dict:
                    new_state = CompositionState.from_dict(result_dict["state"])
                    state_ref[0] = new_state
                    # Capture baseline when session is created or loaded.
                    # load_session can return success=False on SessionNotFoundError;
                    # in that case, leave session_id_ref unchanged so the scratch
                    # session scope ("unsaved") is used.
                    if name in ("new_session", "load_session"):
                        baseline_ref[0] = new_state
                        if result_dict["success"]:
                            resolved_sid: str = result_dict["data"]["session_id"]
                            session_id_ref[0] = resolved_sid
                            audit_recorder.resolve_session(resolved_sid)
                    # Keep the deleted session id active until the finally
                    # block records this destructive success. Then clear it
                    # so subsequent calls use the unsaved scope unless a
                    # new/load_session resolves a fresh id.
                    if name == "delete_session" and result_dict["success"]:
                        clear_session_after_audit = True
                    # B4: Redact storage paths from the response sent to the agent.
                    result_dict["state"] = redact_source_storage_path(result_dict["state"])
                response_text = json.dumps(result_dict, indent=2)
            except (KeyError, TypeError, ValueError) as readback_exc:
                # Tier-1 read-back failure on our own dispatch output.
                # Reclassify as PLUGIN_CRASH and re-raise — the original
                # success the dispatcher claimed is no longer truthful.
                status = ComposerToolStatus.PLUGIN_CRASH
                error_class = type(readback_exc).__name__
                error_message = type(readback_exc).__name__
                result_dict = None
                raise

            return [TextContent(type="text", text=response_text)]
        finally:
            finished_at = datetime.now(UTC)
            latency_ms = (time.monotonic_ns() - started_ns) // 1_000_000

            result_canonical: str | None
            result_hash: str | None
            version_after: int | None

            if status == ComposerToolStatus.SUCCESS and result_dict is not None:
                # Result canonicalization happens AFTER state-mutation +
                # redaction so the recorded result mirrors what was sent
                # back to the LLM. Wrap in try/except per Solution-architect
                # review H3: a non-finite float / non-serializable type in
                # ``result_dict`` would otherwise raise from finally and
                # mask the success return entirely. Fall back to a sentinel
                # canonical so the audit row still lands.
                try:
                    result_canonical = canonical_json(result_dict)
                    result_hash = stable_hash(result_dict)
                except (ValueError, TypeError) as canon_result_exc:
                    # Shared sentinel discipline — see
                    # web.composer.audit.build_canonicalization_sentinel.
                    sentinel = build_canonicalization_sentinel(canon_result_exc, result_dict)
                    result_canonical = canonical_json(sentinel)
                    result_hash = stable_hash(sentinel)
                version_after = state_ref[0].version
            elif status == ComposerToolStatus.ARG_ERROR and error_payload_for_audit is not None:
                # ARG_ERROR: record the error payload that was returned to
                # the LLM (Solution-architect H4 symmetry with web side).
                result_canonical = canonical_json(error_payload_for_audit)
                result_hash = stable_hash(error_payload_for_audit)
                version_after = None
            else:
                # PLUGIN_CRASH path: no result was sent to the LLM (the
                # exception propagates as a server error). version_after
                # is None to signal "dispatch did not complete".
                result_canonical = None
                result_hash = None
                version_after = None

            invocation = ComposerToolInvocation(
                tool_call_id=tool_call_id,
                tool_name=name,
                arguments_canonical=arguments_canonical,
                arguments_hash=arguments_hash,
                result_canonical=result_canonical,
                result_hash=result_hash,
                status=status,
                error_class=error_class,
                error_message=error_message,
                version_before=version_before,
                version_after=version_after,
                started_at=started_at,
                finished_at=finished_at,
                latency_ms=latency_ms,
                actor="composer-mcp:cli",
            )
            audit_recorder.record(invocation)
            if clear_session_after_audit:
                session_id_ref[0] = None

    return server


async def run_server(catalog: CatalogService, scratch_dir: Path) -> None:
    """Run the MCP server with stdio transport."""
    server = create_server(catalog, scratch_dir)
    async with stdio_server() as (read_stream, write_stream):
        await server.run(read_stream, write_stream, server.create_initialization_options())


# ---------------------------------------------------------------------------
# WORKAROUND — Linux-only kernel guard against orphan busy-spin.
# Tracked by filigree issue elspeth-7f99eba6ef. See _install_parent_death_signal_workaround
# docstring for full diagnosis and deletion criteria.
# ---------------------------------------------------------------------------
def _install_parent_death_signal_workaround() -> None:
    """Kernel-level guarantee that this process dies when its parent dies.

    WORKAROUND for an upstream bug in ``mcp.server.stdio.stdio_server`` (the
    official MCP Python SDK). When the controlling Claude Code session
    terminates abnormally (crash, SIGKILL, suspend-without-resume), the
    SDK's stdio read coroutine fails to detect parent-pipe EOF and instead
    busy-spins on empty reads. Observed in production: a single orphaned
    pair burned ~120% CPU for 9 days before discovery (~10.8 core-days of
    waste). Diagnosis trace: ``~/.claude/plans/can-you-investigate-why-mighty-sphinx.md``.

    This function uses Linux's ``prctl(PR_SET_PDEATHSIG, SIGTERM)`` so the
    kernel sends SIGTERM to this process when its parent dies, regardless
    of what the SDK's read loop does. The follow-up ``getppid() == 1``
    check covers the race window between process start and the prctl call:
    if the parent died before we registered, PDEATHSIG cannot fire (it
    requires a parent transition we already missed), so we exit immediately.

    NOT A FIX — this is belt-and-braces only. The underlying SDK bug
    affects every platform; this guard only protects Linux. Non-Linux
    platforms (macOS, Windows, BSD) remain vulnerable and need either an
    SDK upgrade or a portable watchdog (e.g. periodic ``getppid()`` poll).

    REVIEW SCHEDULE — filigree issue ``elspeth-7f99eba6ef`` tracks the
    deletion criteria. Re-evaluate at every MCP SDK upgrade. Delete this
    function and its caller in ``main()`` when:

    1. The pinned MCP SDK version handles stdin EOF correctly in
       ``stdio_server()``.
    2. A behavioural test confirms unclean parent-kill produces clean
       child exit *without* this workaround.

    Sibling vulnerability: ``filigree-mcp`` (separate codebase, same SDK)
    has the identical bug and needs its own fix; this workaround does not
    cover it.
    """
    import ctypes
    import ctypes.util
    import os
    import signal
    import sys

    if sys.platform != "linux":
        return  # Non-Linux: no portable equivalent here. See review issue.

    libc_name = ctypes.util.find_library("c")
    if libc_name is None:
        # No discoverable libc on a Linux system — exotic/embedded build.
        # Skip the guard rather than crash; the workaround is non-essential.
        return
    libc = ctypes.CDLL(libc_name, use_errno=True)

    # PR_SET_PDEATHSIG: see ``man 2 prctl``. Constant is stable kernel ABI.
    pr_set_pdeathsig = 1
    rc = libc.prctl(pr_set_pdeathsig, signal.SIGTERM, 0, 0, 0)
    if rc != 0:
        # prctl(PR_SET_PDEATHSIG, ...) is documented as never failing for
        # valid arguments. A non-zero return here means we passed something
        # the kernel rejected — that's a bug in this function, not a runtime
        # condition to absorb. Crash with the errno preserved.
        errno = ctypes.get_errno()
        raise OSError(
            errno,
            f"prctl(PR_SET_PDEATHSIG, SIGTERM) failed: {os.strerror(errno)}",
        )

    # Race-window cleanup: if the parent died between fork/exec and the
    # prctl call above, PDEATHSIG will not fire because the parent
    # transition already happened. Exit immediately rather than wait for
    # a signal that will never arrive.
    if os.getppid() == 1:
        sys.exit(0)


def main() -> None:
    """CLI entry point for elspeth-composer MCP server."""
    # WORKAROUND first — see _install_parent_death_signal_workaround
    # docstring and filigree issue elspeth-7f99eba6ef.
    _install_parent_death_signal_workaround()

    parser = argparse.ArgumentParser(
        description="ELSPETH Composer MCP Server",
    )
    parser.add_argument(
        "--scratch-dir",
        type=Path,
        default=Path(".composer-scratch"),
        help="Directory for session persistence (default: .composer-scratch)",
    )
    args = parser.parse_args()

    # Lazy import to avoid pulling in the full catalog at module level.
    from elspeth.web.dependencies import create_catalog_service

    catalog = create_catalog_service()

    import asyncio

    asyncio.run(run_server(catalog, args.scratch_dir))
