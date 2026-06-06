"""Integration tests for the JSON-RPC server loop (WP3 Task 2).

Spawns the installed `loomweave-plugin-python` binary as a subprocess, speaks
Content-Length-framed JSON-RPC to it over stdin/stdout, and asserts the
handshake response matches the Rust host's `InitializeResult` contract
(`{name, version, ontology_version, capabilities}` per
`crates/loomweave-core/src/plugin/protocol.rs` line 293).
"""

from __future__ import annotations

import json
import subprocess
import sys
import textwrap
from typing import IO, TYPE_CHECKING, Any, cast

from loomweave_plugin_python import server as server_module
from loomweave_plugin_python.call_resolver import CallResolutionResult
from loomweave_plugin_python.pyright_session import (
    FINDING_PYRIGHT_RESTART,
    PyrightRunState,
    PyrightSession,
)
from loomweave_plugin_python.reference_resolver import ReferenceResolutionResult, ReferenceSite

if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path

    import pytest

# Invoke via ``sys.executable -m`` rather than the installed console script so
# the test works regardless of whether the venv's bin dir is on $PATH when
# pytest runs. Task 8's round-trip test exercises the entry-point binary; this
# test only needs ``main()`` reached via the package module.
_SERVER_CMD = [sys.executable, "-m", "loomweave_plugin_python"]


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
        assert result["name"] == "loomweave-plugin-python"
        assert result["version"] == "1.1.0rc1"
        assert result["ontology_version"] == "0.7.0"
        assert set(result["capabilities"]) == {"wardline"}
        assert result["capabilities"]["wardline"]["status"] in {
            "absent",
            "enabled",
            "version_skew",
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


def test_malformed_non_ascii_header_uses_protocol_error_exit_path() -> None:
    """Malformed header bytes exit cleanly without emitting framed stdout."""
    proc = subprocess.Popen(  # noqa: S603
        _SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    stdout, stderr = proc.communicate(b"Content-L\xe9ngth: 0\r\n\r\n", timeout=5)

    assert proc.returncode == 1
    assert stdout == b""
    assert b"Traceback" not in stderr


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


def test_initialize_project_descriptor_reports_wardline_enabled(tmp_path: Path) -> None:
    descriptor = tmp_path / ".wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir()
    descriptor.write_text(
        """\
version: wardline-generic-2
entries:
- canonical_name: trusted
  group: 1
  attrs:
    _wardline_level: TaintState
""",
        encoding="utf-8",
    )
    state = server_module.ServerState()

    response = server_module.handle_initialize(
        {"protocol_version": "1.0", "project_root": str(tmp_path)},
        state,
    )

    assert response["capabilities"]["wardline"] == {
        "status": "enabled",
        "descriptor_version": "wardline-generic-2",
        "source": "project",
    }
    assert state.wardline_vocabulary is not None


def test_analyze_file_threads_wardline_vocabulary(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakePyrightSession:
        def __init__(self, project_root: Path, **_kwargs: Any) -> None:
            self.project_root = project_root

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
            pass

    monkeypatch.setattr(server_module, "PyrightSession", FakePyrightSession, raising=False)
    descriptor = tmp_path / ".wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir()
    descriptor.write_text(
        """\
version: wardline-generic-2
entries:
- canonical_name: trusted
  group: 1
  attrs:
    _wardline_level: TaintState
""",
        encoding="utf-8",
    )
    demo = tmp_path / "demo.py"
    demo.write_text("@trusted\ndef compute():\n    return 1\n", encoding="utf-8")
    state = server_module.ServerState(initialized=True)
    server_module.handle_initialize(
        {"protocol_version": "1.0", "project_root": str(tmp_path)}, state
    )

    response = server_module.handle_analyze_file({"file_path": str(demo)}, state)

    compute = next(e for e in response["entities"] if e["id"] == "python:function:demo.compute")
    assert compute["wardline"]["decorators"][0]["canonical_name"] == "trusted"
    assert "wardline:trusted" in compute["tags"]


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
        def __init__(self, project_root: Path, **_kwargs: Any) -> None:
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
        def __init__(self, project_root: Path, **_kwargs: Any) -> None:
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
                findings=[
                    {
                        "subcode": "LMWV-PY-PYRIGHT-UNAVAILABLE",
                        "severity": "warning",
                        "message": "pyright unavailable",
                        "metadata": {"reason": "test"},
                    }
                ],
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
    assert response["findings"] == [
        {
            "subcode": "LMWV-PY-PYRIGHT-UNAVAILABLE",
            "severity": "warning",
            "message": "pyright unavailable",
            "metadata": {"reason": "test"},
        }
    ]
    assert any(edge["kind"] == "references" for edge in response["edges"])


def test_analyze_file_restarts_pyright_after_file_budget(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    sessions: list[Any] = []

    class FakePyrightSession:
        def __init__(self, project_root: Path, **_kwargs: Any) -> None:
            self.project_root = project_root
            self.closed = False
            sessions.append(self)

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
    monkeypatch.setattr(server_module, "MAX_FILES_PER_PYRIGHT_SESSION", 2)
    demo = tmp_path / "demo.py"
    demo.write_text("def hello():\n    pass\n", encoding="utf-8")
    state = server_module.ServerState(initialized=True, project_root=tmp_path)

    server_module.handle_analyze_file({"file_path": str(demo)}, state)
    assert state.pyright is sessions[0]
    first_session = cast("Any", sessions[0])
    assert first_session.closed is False

    server_module.handle_analyze_file({"file_path": str(demo)}, state)
    assert first_session.closed is True
    assert len(sessions) == 1
    assert state.pyright is None
    assert state.pyright_files_since_restart == 0


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


def test_analyze_file_returns_pyright_findings(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FindingPyrightSession:
        def __init__(self, project_root: Path, **kwargs: Any) -> None:
            _ = (project_root, kwargs)

        def resolve_calls(
            self,
            file_path: str,
            function_ids: list[str],
        ) -> CallResolutionResult:
            _ = (file_path, function_ids)
            return CallResolutionResult(
                findings=[
                    {
                        "subcode": FINDING_PYRIGHT_RESTART,
                        "severity": "warning",
                        "message": "pyright subprocess died and was restarted",
                        "metadata": {"restart_count": 1},
                    }
                ],
            )

        def resolve_references(
            self,
            file_path: str,
            sites: Sequence[ReferenceSite],
        ) -> ReferenceResolutionResult:
            _ = (file_path, sites)
            return ReferenceResolutionResult()

        def close(self) -> None:
            pass

    monkeypatch.setattr(server_module, "PyrightSession", FindingPyrightSession, raising=False)
    demo = tmp_path / "demo.py"
    demo.write_text("def hello():\n    pass\n", encoding="utf-8")
    state = server_module.ServerState(initialized=True, project_root=tmp_path)

    response = server_module.handle_analyze_file({"file_path": str(demo)}, state)

    assert response["findings"] == [
        {
            "subcode": FINDING_PYRIGHT_RESTART,
            "severity": "warning",
            "message": "pyright subprocess died and was restarted",
            "metadata": {"restart_count": 1},
        },
    ]


def test_restart_budget_survives_session_recycle(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Run-wide restart cap is not reset at the session-recycle boundary.

    With MAX_FILES_PER_PYRIGHT_SESSION=25 and 30 files, there are two recycles
    (files 1-25 in session 1, files 26-30 in session 2). A crashing pyright
    must exhaust the single run-wide 3-restart budget, not a fresh 3-restart
    budget per session.
    """

    # Each fake session increments run_state.restart_count directly, simulating
    # a pyright that crashes on every call.  Using run_state rather than a
    # session-local counter is what the fix is supposed to enforce.
    class CrashingFakePyrightSession:
        def __init__(self, project_root: Path, **kwargs: Any) -> None:
            self.project_root = project_root
            self.run_state: PyrightRunState = kwargs.get("run_state") or PyrightRunState()
            self.closed = False

        def resolve_calls(
            self,
            file_path: str,
            function_ids: list[str],
        ) -> CallResolutionResult:
            _ = (file_path, function_ids)
            if not self.run_state.disabled:
                # Simulate a crash: record a restart finding via run_state.
                self.run_state.restart_count += 1
                if self.run_state.restart_count > 3:
                    self.run_state.disabled = True
                    return CallResolutionResult(
                        findings=[
                            {
                                "subcode": FINDING_PYRIGHT_RESTART,
                                "severity": "warning",
                                "message": "pyright restart cap exceeded",
                                "metadata": {},
                            }
                        ],
                    )
                return CallResolutionResult(
                    findings=[
                        {
                            "subcode": FINDING_PYRIGHT_RESTART,
                            "severity": "warning",
                            "message": "pyright subprocess died and was restarted",
                            "metadata": {},
                        }
                    ],
                )
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

    monkeypatch.setattr(server_module, "PyrightSession", CrashingFakePyrightSession, raising=False)
    demo = tmp_path / "demo.py"
    demo.write_text("def hello():\n    pass\n", encoding="utf-8")
    state = server_module.ServerState(initialized=True, project_root=tmp_path)

    # Drive 30 analyze_file requests.  The recycle boundary falls at file 25.
    for _ in range(30):
        server_module.handle_analyze_file({"file_path": str(demo)}, state)

    # The run-wide budget must be consumed exactly once across both recycles,
    # not reset to 3 at the recycle boundary.
    assert state.pyright_run_state.restart_count <= 4  # 3 restarts + 1 cap trip
    assert state.pyright_run_state.disabled is True


def test_disabled_pyright_unavailable_does_not_redrive_per_session(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Once pyright is disabled (binary missing), recycles must not re-check the binary.

    Driving 30 files across the 25-file recycle boundary should emit exactly
    one FINDING_PYRIGHT_UNAVAILABLE finding, not one per recycle.
    """
    resolve_executable_call_count = 0

    def counting_resolve_executable(_self: PyrightSession) -> None:
        nonlocal resolve_executable_call_count
        resolve_executable_call_count += 1
        # Returning None simulates a missing binary.

    monkeypatch.setattr(
        PyrightSession,
        "_resolve_executable",
        counting_resolve_executable,
        raising=False,
    )

    demo = tmp_path / "demo.py"
    demo.write_text("def hello():\n    pass\n", encoding="utf-8")
    state = server_module.ServerState(initialized=True, project_root=tmp_path)

    # Drive 30 analyze_file requests across the 25-file recycle boundary.
    for _ in range(30):
        server_module.handle_analyze_file({"file_path": str(demo)}, state)

    # _resolve_executable must be called exactly once: the first time pyright
    # is needed.  The shared run_state.disabled=True short-circuits _ensure_process
    # before _start_process (and thus _resolve_executable) is re-entered.
    assert resolve_executable_call_count == 1
