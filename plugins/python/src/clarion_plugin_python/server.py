"""WP2 L4 JSON-RPC server speaking Content-Length framing.

Implements the five L4 methods — ``initialize``, ``initialized``,
``analyze_file``, ``shutdown``, ``exit`` — exactly matching the Rust host's
typed request/response contracts in ``crates/clarion-core/src/plugin/protocol.rs``.

Response shapes (required by the Rust host's typed deserialise path):

- ``initialize`` → ``{name, version, ontology_version, capabilities}``
  (``InitializeResult``; WP2 scrub commit ``1ac32b1`` validates
  ``ontology_version`` is non-empty).
- ``analyze_file`` → ``{entities: [...]}`` (``AnalyzeFileResult``).
- ``shutdown`` → ``{}`` (empty ``ShutdownResult`` struct — *not* ``null``).
- ``initialized`` / ``exit`` — notifications, no response.

Task 2 ships the dispatch skeleton with ``analyze_file`` returning an empty
entity list. Task 6 wires the Wardline probe result into ``capabilities``;
Task 7 replaces ``handle_analyze_file`` with the extractor.
"""

from __future__ import annotations

import json
import sys
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import IO, Any

from clarion_plugin_python import __version__
from clarion_plugin_python.extractor import extract_with_stats
from clarion_plugin_python.pyright_session import PyrightSession
from clarion_plugin_python.stdout_guard import install_stdio
from clarion_plugin_python.wardline_probe import probe as wardline_probe

ONTOLOGY_VERSION = "0.6.0"

# Sprint 1 defaults for the Wardline version pin (WP3 L8 + plugin.toml
# `[integrations.wardline]`). Kept as module constants so Task 7's
# manifest values match by inspection; a future sprint can flow these
# through from the parsed manifest on demand.
WARDLINE_MIN_VERSION = "1.0.0"
WARDLINE_MAX_VERSION = "2.0.0"

# Plugin-side Content-Length sanity cap. Matches the host's ADR-021 §2b
# default (8 MiB) so the plugin never emits a frame the host would kill us
# for. Oversize outbound payloads trip this before reaching the wire.
MAX_CONTENT_LENGTH = 8 * 1024 * 1024
MAX_FILES_PER_PYRIGHT_SESSION = 25

# JSON-RPC 2.0 error codes (§5.1) plus LSP-style server extensions.
_ERR_INVALID_REQUEST = -32600
_ERR_METHOD_NOT_FOUND = -32601
_ERR_INTERNAL = -32603
_ERR_NOT_INITIALIZED = -32002


class ProtocolError(RuntimeError):
    """Unrecoverable framing or envelope error; the server loop exits."""


@dataclass
class ServerState:
    """Handshake + shutdown + project-root state across the dispatch loop."""

    initialized: bool = False
    shutdown_requested: bool = False
    project_root: Path | None = field(default=None)
    pyright: PyrightSession | None = field(default=None)
    pyright_files_since_restart: int = 0


def read_frame(stream: IO[bytes]) -> dict[str, Any] | None:
    """Read one Content-Length-framed JSON object. Returns ``None`` on EOF."""
    headers: dict[str, str] = {}
    while True:
        line = stream.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        decoded = line.decode("ascii").rstrip("\r\n")
        if ":" not in decoded:
            msg = f"malformed header line: {decoded!r}"
            raise ProtocolError(msg)
        name, value = decoded.split(":", 1)
        headers[name.strip().lower()] = value.strip()

    raw_length = headers.get("content-length")
    if raw_length is None:
        msg = "missing Content-Length header"
        raise ProtocolError(msg)
    try:
        length = int(raw_length)
    except ValueError as exc:
        msg = f"Content-Length not an integer: {raw_length!r}"
        raise ProtocolError(msg) from exc
    if length < 0 or length > MAX_CONTENT_LENGTH:
        msg = f"Content-Length out of range: {length}"
        raise ProtocolError(msg)

    body = stream.read(length)
    if len(body) != length:
        msg = f"short read: expected {length} bytes, got {len(body)}"
        raise ProtocolError(msg)

    try:
        parsed = json.loads(body)
    except json.JSONDecodeError as exc:
        msg = f"invalid JSON body: {exc}"
        raise ProtocolError(msg) from exc

    if not isinstance(parsed, dict):
        msg = f"expected JSON object at frame root, got {type(parsed).__name__}"
        raise ProtocolError(msg)
    return parsed


def write_frame(stream: IO[bytes], payload: dict[str, Any]) -> None:
    """Serialise ``payload`` as one Content-Length-framed JSON frame."""
    body = json.dumps(payload, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    if len(body) > MAX_CONTENT_LENGTH:
        msg = f"outbound frame exceeds MAX_CONTENT_LENGTH ({len(body)} > {MAX_CONTENT_LENGTH})"
        raise ProtocolError(msg)
    header = f"Content-Length: {len(body)}\r\n\r\n".encode("ascii")
    stream.write(header)
    stream.write(body)
    stream.flush()


def _success(request_id: Any, result: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def _error(request_id: Any, code: int, message: str) -> dict[str, Any]:
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "error": {"code": code, "message": message},
    }


def handle_initialize(params: dict[str, Any], state: ServerState) -> dict[str, Any]:
    """Return the plugin's identity + capabilities; capture ``project_root``."""
    root_raw = params.get("project_root")
    if isinstance(root_raw, str) and root_raw:
        state.project_root = Path(root_raw).resolve()
    return {
        "name": "clarion-plugin-python",
        "version": __version__,
        "ontology_version": ONTOLOGY_VERSION,
        "capabilities": {
            "wardline": wardline_probe(WARDLINE_MIN_VERSION, WARDLINE_MAX_VERSION),
        },
    }


def _resolve_module_path(file_path_raw: str, state: ServerState) -> str:
    """Compute the entity ``module_path`` relative to ``project_root``.

    The host sends absolute paths (see ``crates/clarion-cli/src/analyze.rs``
    — ``project_root`` is canonicalised and file entries are built by
    ``entry.path()`` joins). To produce the expected L7 qualified names
    (``pkg.module.func`` rather than ``tmp.xyz.demo.func``), the plugin
    relativises each incoming path against the ``project_root`` captured
    at ``initialize``.
    """
    path = Path(file_path_raw)
    if state.project_root is not None and path.is_absolute():
        try:
            return str(path.resolve().relative_to(state.project_root))
        except ValueError:
            # Outside project_root — host's jail should have caught this.
            # Fall back to the raw path so the host's logs show the drift.
            return file_path_raw
    return file_path_raw


def handle_analyze_file(params: dict[str, Any], state: ServerState) -> dict[str, Any]:
    """Read the requested file, extract entities + edges, return AnalyzeFileResult shape."""
    empty_stats = {
        "unresolved_call_sites_total": 0,
        "unresolved_call_sites": [],
        "reference_sites_total": 0,
        "references_resolved_total": 0,
        "references_skipped_external_total": 0,
        "references_skipped_cap_total": 0,
        "unresolved_reference_sites_total": 0,
        "pyright_query_latency_ms": [],
        "pyright_index_parse_latency_ms": [],
        "extractor_parse_latency_ms": 0,
    }
    file_path_raw = params.get("file_path")
    if not isinstance(file_path_raw, str):
        return {"entities": [], "edges": [], "stats": empty_stats}
    path = Path(file_path_raw)
    if state.pyright is None:
        state.pyright = PyrightSession(state.project_root or path.parent)
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        sys.stderr.write(f"clarion-plugin-python: cannot read {file_path_raw}: {exc}\n")
        return {"entities": [], "edges": [], "stats": empty_stats}
    # Emit source.file_path exactly as received so the host's jail check
    # (which canonicalises against project_root) sees the original path.
    # Derive qualified-name dotting from the project-relative form.
    module_prefix = _resolve_module_path(file_path_raw, state)
    result = extract_with_stats(
        source,
        file_path_raw,
        module_prefix_path=module_prefix,
        call_resolver=state.pyright,
        reference_resolver=state.pyright,
    )
    state.pyright_files_since_restart += 1
    if state.pyright_files_since_restart >= MAX_FILES_PER_PYRIGHT_SESSION:
        state.pyright.close()
        state.pyright = None
        state.pyright_files_since_restart = 0
    stats = {
        "unresolved_call_sites_total": result.stats.unresolved_call_sites_total,
        "unresolved_call_sites": result.stats.unresolved_call_sites,
        "reference_sites_total": result.stats.reference_sites_total,
        "references_resolved_total": result.stats.references_resolved_total,
        "references_skipped_external_total": result.stats.references_skipped_external_total,
        "references_skipped_cap_total": result.stats.references_skipped_cap_total,
        "unresolved_reference_sites_total": result.stats.unresolved_reference_sites_total,
        "pyright_query_latency_ms": result.stats.pyright_query_latency_ms,
        "pyright_index_parse_latency_ms": result.stats.pyright_index_parse_latency_ms,
        "extractor_parse_latency_ms": result.stats.extractor_parse_latency_ms,
    }
    return {"entities": result.entities, "edges": result.edges, "stats": stats}


Handler = Callable[[dict[str, Any], ServerState], dict[str, Any]]

_HANDLERS: dict[str, Handler] = {
    "initialize": handle_initialize,
    "analyze_file": handle_analyze_file,
}


def dispatch(frame: dict[str, Any], state: ServerState) -> dict[str, Any] | None:
    """Process one frame; return the response envelope to send, or ``None``."""
    method = frame.get("method")
    params_raw = frame.get("params")
    params: dict[str, Any] = params_raw if isinstance(params_raw, dict) else {}
    request_id = frame.get("id")

    if method == "initialized":
        state.initialized = True
        return None
    if method == "exit":
        return None
    if method == "shutdown":
        state.shutdown_requested = True
        if state.pyright is not None:
            state.pyright.close()
            state.pyright = None
        return _success(request_id, {})
    if not isinstance(method, str):
        return _error(request_id, _ERR_INVALID_REQUEST, f"invalid method: {method!r}")
    if method == "analyze_file" and not state.initialized:
        return _error(request_id, _ERR_NOT_INITIALIZED, "analyze_file before initialized")
    handler = _HANDLERS.get(method)
    if handler is None:
        return _error(request_id, _ERR_METHOD_NOT_FOUND, f"method not found: {method}")
    try:
        result = handler(params, state)
    except Exception as exc:  # noqa: BLE001 - dispatch boundary: any handler bug becomes a response
        return _error(request_id, _ERR_INTERNAL, f"handler failed: {exc}")
    return _success(request_id, result)


def serve(stdin: IO[bytes], stdout: IO[bytes]) -> int:
    """Run the dispatch loop until EOF or ``exit`` notification."""
    state = ServerState()
    while True:
        frame = read_frame(stdin)
        if frame is None:
            return 0
        method = frame.get("method")
        response = dispatch(frame, state)
        if response is not None:
            write_frame(stdout, response)
        if method == "exit":
            return 0 if state.shutdown_requested else 1


def main() -> int:
    """Install stdout discipline, run the server loop, translate errors to exit codes."""
    stdin, stdout = install_stdio()
    try:
        return serve(stdin, stdout)
    except ProtocolError:
        return 1
