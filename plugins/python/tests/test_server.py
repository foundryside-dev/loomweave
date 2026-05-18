"""Integration tests for the JSON-RPC server loop (WP3 Task 2).

Spawns the installed `clarion-plugin-python` binary as a subprocess, speaks
Content-Length-framed JSON-RPC to it over stdin/stdout, and asserts the
handshake response matches the Rust host's `InitializeResult` contract
(`{name, version, ontology_version, capabilities}` per
`crates/clarion-core/src/plugin/protocol.rs` line 293).
"""

from __future__ import annotations

import json
import subprocess
import sys
import textwrap
from typing import IO, TYPE_CHECKING, Any, cast

from clarion_plugin_python import server as server_module
from clarion_plugin_python.call_resolver import CallResolutionResult
from clarion_plugin_python.reference_resolver import ReferenceResolutionResult, ReferenceSite

if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path

    import pytest

# Invoke via ``sys.executable -m`` rather than the installed console script so
# the test works regardless of whether the venv's bin dir is on $PATH when
# pytest runs. Task 8's round-trip test exercises the entry-point binary; this
# test only needs ``main()`` reached via the package module.
_SERVER_CMD = [sys.executable, "-m", "clarion_plugin_python"]


def _encode_frame(payload: dict[str, Any]) -> bytes:
    body = json.dumps(payload).encode("utf-8")
    header = f"Content-Length: {len(body)}\r\n\r\n".encode("ascii")
    return header + body


def _read_frame(stream: IO[bytes]) -> dict[str, Any]:
    headers: dict[str, str] = {}
    while True:
        line = stream.readline()
        if not line:
            msg = "EOF before headers terminator"
            raise RuntimeError(msg)
        if line in (b"\r\n", b"\n"):
            break
        name, _, value = line.decode("ascii").rstrip("\r\n").partition(":")
        headers[name.strip().lower()] = value.strip()
    length = int(headers["content-length"])
    body = stream.read(length)
    parsed: dict[str, Any] = json.loads(body)
    return parsed


def test_initialize_roundtrip() -> None:
    """initialize → response carries all four InitializeResult fields."""
    proc = subprocess.Popen(  # noqa: S603 - invoking our own entry point under test
        _SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        assert proc.stdin is not None
        assert proc.stdout is not None

        request = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocol_version": "1.0", "project_root": "/tmp"},
        }
        proc.stdin.write(_encode_frame(request))
        proc.stdin.flush()

        response = _read_frame(proc.stdout)
        assert response["jsonrpc"] == "2.0"
        assert response["id"] == 1
        result = response["result"]
        assert result["name"] == "clarion-plugin-python"
        assert result["version"] == "0.1.5"
        assert result["ontology_version"] == "0.6.0"
        # Capabilities carry the L8 Wardline probe result. We don't pin a
        # specific status here because the probe's output depends on whether
        # wardline is installed in the test environment — all three legal
        # states (`absent`, `enabled`, `version_out_of_range`) pass.
        assert "wardline" in result["capabilities"]
        assert result["capabilities"]["wardline"]["status"] in {
            "absent",
            "enabled",
            "version_out_of_range",
        }

        # Graceful shutdown: shutdown → ack `{}`, then exit notification.
        proc.stdin.write(
            _encode_frame({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}}),
        )
        proc.stdin.flush()
        shutdown_response = _read_frame(proc.stdout)
        assert shutdown_response["id"] == 2
        assert shutdown_response["result"] == {}

        proc.stdin.write(_encode_frame({"jsonrpc": "2.0", "method": "exit"}))
        proc.stdin.flush()
        proc.stdin.close()

        assert proc.wait(timeout=5) == 0
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=2)


def test_analyze_file_before_initialized_returns_error() -> None:
    """Per JSON-RPC semantics, analyze_file without preceding initialized is rejected."""
    proc = subprocess.Popen(  # noqa: S603
        _SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        assert proc.stdin is not None
        assert proc.stdout is not None

        proc.stdin.write(
            _encode_frame(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "analyze_file",
                    "params": {"file_path": "/tmp/foo.py"},
                },
            ),
        )
        proc.stdin.flush()

        response = _read_frame(proc.stdout)
        assert response["id"] == 1
        assert "error" in response
        assert response["error"]["code"] == -32002

        # Tear down.
        proc.stdin.close()
        proc.wait(timeout=5)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=2)


def test_analyze_file_returns_extracted_entities(tmp_path: Path) -> None:
    """After initialize, analyze_file on a real .py file yields function entities."""
    demo = tmp_path / "demo.py"
    demo.write_text(
        textwrap.dedent("""
        def hello():
            pass

        class Foo:
            def bar(self):
                pass
    """).lstrip()
    )

    proc = subprocess.Popen(  # noqa: S603
        _SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        assert proc.stdin is not None
        assert proc.stdout is not None

        # Handshake with project_root = tmp_path so the plugin relativises paths.
        proc.stdin.write(
            _encode_frame(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocol_version": "1.0",
                        "project_root": str(tmp_path),
                    },
                },
            ),
        )
        proc.stdin.flush()
        _read_frame(proc.stdout)  # initialize response
        proc.stdin.write(
            _encode_frame({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        )
        proc.stdin.flush()

        # Analyze the file.
        proc.stdin.write(
            _encode_frame(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "analyze_file",
                    "params": {"file_path": str(demo)},
                },
            ),
        )
        proc.stdin.flush()
        response = _read_frame(proc.stdout)
        assert response["id"] == 2
        entities = response["result"]["entities"]
        function_ids = {e["id"] for e in entities if e["kind"] == "function"}
        class_ids = {e["id"] for e in entities if e["kind"] == "class"}
        module_ids = {e["id"] for e in entities if e["kind"] == "module"}

        assert module_ids == {"python:module:demo"}
        assert function_ids == {
            "python:function:demo.hello",
            "python:function:demo.Foo.bar",
        }
        assert class_ids == {"python:class:demo.Foo"}

        proc.stdin.close()
        proc.wait(timeout=5)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=2)


def test_method_not_found_returns_error() -> None:
    """Unknown method → -32601 response, server stays up."""
    proc = subprocess.Popen(  # noqa: S603
        _SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        assert proc.stdin is not None
        assert proc.stdout is not None

        proc.stdin.write(
            _encode_frame(
                {"jsonrpc": "2.0", "id": 1, "method": "bogus_method", "params": {}},
            ),
        )
        proc.stdin.flush()

        response = _read_frame(proc.stdout)
        assert response["error"]["code"] == -32601

        proc.stdin.close()
        proc.wait(timeout=5)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=2)


def test_analyze_file_lazy_initializes_pyright(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakePyrightSession:
        def __init__(self, project_root: Path) -> None:
            self.project_root = project_root
            self.closed = False

        def resolve_calls(
            self,
            file_path: str,
            function_ids: list[str],
        ) -> CallResolutionResult:
            _ = (file_path, function_ids)
            return CallResolutionResult()

        def resolve_references(
            self,
            file_path: str,
            sites: Sequence[ReferenceSite],
        ) -> ReferenceResolutionResult:
            _ = (file_path, sites)
            return ReferenceResolutionResult()

        def close(self) -> None:
            self.closed = True

    monkeypatch.setattr(server_module, "PyrightSession", FakePyrightSession, raising=False)
    demo = tmp_path / "demo.py"
    demo.write_text("def hello():\n    pass\n", encoding="utf-8")
    state = server_module.ServerState(initialized=True, project_root=tmp_path)

    server_module.handle_analyze_file({"file_path": str(demo)}, state)

    assert isinstance(state.pyright, FakePyrightSession)
    assert state.pyright.project_root == tmp_path


def test_analyze_file_reports_call_resolver_stats(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakePyrightSession:
        def __init__(self, project_root: Path) -> None:
            self.project_root = project_root

        def resolve_calls(
            self,
            file_path: str,
            function_ids: list[str],
        ) -> CallResolutionResult:
            _ = (file_path, function_ids)
            return CallResolutionResult(
                unresolved_call_sites_total=3,
                unresolved_call_sites=[
                    {
                        "caller_entity_id": "python:function:demo.caller",
                        "site_ordinal": 0,
                        "source_byte_start": 12,
                        "source_byte_end": 20,
                        "callee_expr": "dynamic_target",
                    },
                ],
                pyright_query_latency_ms=[11, 29],
                pyright_index_parse_latency_ms=[5],
            )

        def resolve_references(
            self,
            file_path: str,
            sites: Sequence[ReferenceSite],
        ) -> ReferenceResolutionResult:
            _ = file_path
            assert len(sites) == 1
            site = sites[0]
            return ReferenceResolutionResult(
                edges=[
                    {
                        "kind": "references",
                        "from_id": "python:module:demo",
                        "to_id": "python:function:demo.world",
                        "confidence": "resolved",
                        "source_byte_start": site.source_byte_start,
                        "source_byte_end": site.source_byte_end,
                    },
                ],
                reference_sites_total=1,
                references_resolved_total=1,
                references_skipped_external_total=2,
                references_skipped_cap_total=3,
                unresolved_reference_sites_total=4,
                pyright_query_latency_ms=[31],
                pyright_index_parse_latency_ms=[7],
            )

        def close(self) -> None:
            pass

    monkeypatch.setattr(server_module, "PyrightSession", FakePyrightSession, raising=False)
    demo = tmp_path / "demo.py"
    demo.write_text("def world():\n    return 42\n\nCONST_REF = world\n", encoding="utf-8")
    state = server_module.ServerState(initialized=True, project_root=tmp_path)

    response = server_module.handle_analyze_file({"file_path": str(demo)}, state)

    stats = response["stats"]
    extractor_parse_latency_ms = stats.pop("extractor_parse_latency_ms")
    assert isinstance(extractor_parse_latency_ms, int)
    assert extractor_parse_latency_ms > 0
    assert stats == {
        "unresolved_call_sites_total": 3,
        "unresolved_call_sites": [
            {
                "caller_entity_id": "python:function:demo.caller",
                "site_ordinal": 0,
                "source_byte_start": 12,
                "source_byte_end": 20,
                "callee_expr": "dynamic_target",
            },
        ],
        "reference_sites_total": 1,
        "references_resolved_total": 1,
        "references_skipped_external_total": 2,
        "references_skipped_cap_total": 3,
        "unresolved_reference_sites_total": 4,
        "pyright_query_latency_ms": [11, 29, 31],
        "pyright_index_parse_latency_ms": [5, 7],
    }
    assert any(edge["kind"] == "references" for edge in response["edges"])


def test_shutdown_closes_pyright_session() -> None:
    class FakePyrightSession:
        def __init__(self) -> None:
            self.closed = False

        def close(self) -> None:
            self.closed = True

    fake = FakePyrightSession()
    state = server_module.ServerState(initialized=True)
    state.pyright = cast("Any", fake)

    response = server_module.dispatch(
        {"jsonrpc": "2.0", "id": 1, "method": "shutdown", "params": {}},
        state,
    )

    assert response == {"jsonrpc": "2.0", "id": 1, "result": {}}
    assert fake.closed is True
    assert state.pyright is None
