"""Round-trip self-test: plugin analyses its own source (WP3 Task 8).

Drives the *installed* ``loomweave-plugin-python`` entry-point binary
(not ``sys.executable -m``) so the pip-install entry point is exercised
end-to-end. The plugin's own ``extractor.py`` is the analysis target; the
test asserts the module's public API functions appear in the returned
entity list with the expected 3-segment L2 EntityId shape.
"""

from __future__ import annotations

import json
import subprocess
import sysconfig
from pathlib import Path
from typing import IO, Any

import pytest


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


def _locate_binary() -> Path:
    scripts = Path(sysconfig.get_path("scripts"))
    binary = scripts / "loomweave-plugin-python"
    if not binary.exists():
        pytest.fail(
            f"loomweave-plugin-python not found at {binary}. It is the installed "
            "console entry point of loomweave-plugin-python; a missing binary means "
            "`pip install -e plugins/python[dev]` was not run or the install is "
            "broken. Skipping this round-trip test would mask that regression in CI.",
        )
    return binary


def test_round_trip_self_analysis() -> None:  # noqa: PLR0915 - by-kind invariants are flat asserts
    """Plugin → analyze_file on its own extractor.py → expected entities appear."""
    binary = _locate_binary()

    # plugins/python/src is the package root; using it as project_root lets
    # the plugin relativise extractor.py to `loomweave_plugin_python/extractor.py`,
    # whose dotted module name is `loomweave_plugin_python.extractor`.
    plugin_src = Path(__file__).resolve().parents[1] / "src"
    target = plugin_src / "loomweave_plugin_python" / "extractor.py"
    assert target.is_file(), f"target source not found at {target}"

    proc = subprocess.Popen(  # noqa: S603 - invoking our own installed entry point
        [str(binary)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        assert proc.stdin is not None
        assert proc.stdout is not None

        # Handshake.
        proc.stdin.write(
            _encode_frame(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocol_version": "1.0",
                        "project_root": str(plugin_src),
                    },
                },
            ),
        )
        proc.stdin.flush()
        init_response = _read_frame(proc.stdout)
        assert init_response["id"] == 1
        assert init_response["result"]["name"] == "loomweave-plugin-python"

        proc.stdin.write(
            _encode_frame({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        )
        proc.stdin.flush()

        # Analyze extractor.py.
        proc.stdin.write(
            _encode_frame(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "analyze_file",
                    "params": {"file_path": str(target)},
                },
            ),
        )
        proc.stdin.flush()
        response = _read_frame(proc.stdout)
        assert response["id"] == 2

        entities = response["result"]["entities"]
        edges = response["result"]["edges"]
        function_entities = [e for e in entities if e["kind"] == "function"]
        module_entities = [e for e in entities if e["kind"] == "module"]
        class_entities = [e for e in entities if e["kind"] == "class"]
        function_ids = {e["id"] for e in function_entities}

        # Invariants — no exact totals (those become merge-conflict generators
        # the moment someone adds a private helper to extractor.py).
        assert len(module_entities) == 1, "exactly one module entity per analyzed file"
        assert module_entities[0]["id"] == "python:module:loomweave_plugin_python.extractor"
        assert module_entities[0].get("parse_status") == "ok"

        # Public extractor API must be present.
        assert (
            "python:function:loomweave_plugin_python.extractor.module_dotted_name" in function_ids
        )
        assert "python:function:loomweave_plugin_python.extractor.extract" in function_ids
        # Private walker is a FunctionDef too, so it emits.
        assert "python:function:loomweave_plugin_python.extractor._walk" in function_ids
        # B.2 renamed `_build_entity` → `_build_function_entity` and added
        # `_build_class_entity` + `_build_module_entity` (and `_module_source_range`).
        assert (
            "python:function:loomweave_plugin_python.extractor._build_function_entity"
            in function_ids
        )
        assert (
            "python:function:loomweave_plugin_python.extractor._build_class_entity" in function_ids
        )
        assert (
            "python:function:loomweave_plugin_python.extractor._build_module_entity" in function_ids
        )

        # extractor.py defines its wire-shape TypedDicts at module level
        # (SourceRange, EntitySource, RawEntity); these are AST ClassDefs
        # and so emit as `class` entities. Subset assertion only —
        # exhaustive enumeration would be brittle.
        class_ids = {e["id"] for e in class_entities}
        assert "python:class:loomweave_plugin_python.extractor.SourceRange" in class_ids
        assert "python:class:loomweave_plugin_python.extractor.EntitySource" in class_ids
        assert "python:class:loomweave_plugin_python.extractor.RawEntity" in class_ids

        # Every entity carries the absolute source.file_path we sent
        # (project_root relativisation only affects the qualified_name prefix).
        for entity in entities:
            assert entity["source"]["file_path"] == str(target)

        # B.3 contains-edge round-trip (Task 7) + B.4* calls-edge smoke.
        # Every non-module entity declares parent_id; every contains edge
        # in the response matches some entity's parent_id (dual-encoding
        # invariant, ADR-026 decision 2). Calls edges are anchored and
        # confidence-bearing per ADR-028.
        assert edges, "extractor.py must produce contains edges (non-empty file)"
        contains_pairs = {(e["from_id"], e["to_id"]) for e in edges if e["kind"] == "contains"}
        calls_edges = [e for e in edges if e["kind"] == "calls"]
        references_edges = [e for e in edges if e["kind"] == "references"]
        imports_edges = [e for e in edges if e["kind"] == "imports"]
        resolved_calls = [e for e in calls_edges if e["confidence"] == "resolved"]
        ambiguous_calls = [e for e in calls_edges if e["confidence"] == "ambiguous"]
        resolved_references = [e for e in references_edges if e["confidence"] == "resolved"]
        assert imports_edges, "extractor.py self-analysis must emit import candidate edges"
        assert resolved_calls, "extractor.py self-analysis must emit at least one resolved call"
        assert resolved_references, (
            "extractor.py self-analysis must emit at least one resolved references edge"
        )
        assert len(ambiguous_calls) <= len(calls_edges) // 2, (
            "ambiguous calls should not dominate extractor.py self-analysis"
        )
        for edge in edges:
            if edge["kind"] == "contains":
                # Contains edges MUST NOT carry source range fields (ADR-026 §3).
                assert "source_byte_start" not in edge
                assert "source_byte_end" not in edge
            elif edge["kind"] in {"calls", "references", "imports", "inherits_from", "decorates"}:
                assert edge["source_byte_start"] < edge["source_byte_end"]
                assert edge["confidence"] in {"resolved", "ambiguous"}
            else:
                message = f"unexpected edge kind: {edge['kind']!r}"
                raise AssertionError(message)
        for entity in entities:
            if entity["kind"] == "module":
                assert "parent_id" not in entity, (
                    "module entity must have no parent_id within the file"
                )
                continue
            assert "parent_id" in entity, f"non-module entity {entity['id']} missing parent_id"
            pair = (entity["parent_id"], entity["id"])
            assert pair in contains_pairs, f"no contains edge matches parent_id for {entity['id']}"

        # Graceful shutdown.
        proc.stdin.write(
            _encode_frame({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}}),
        )
        proc.stdin.flush()
        _read_frame(proc.stdout)  # shutdown ack
        proc.stdin.write(_encode_frame({"jsonrpc": "2.0", "method": "exit"}))
        proc.stdin.flush()
        proc.stdin.close()
        assert proc.wait(timeout=5) == 0
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=2)
