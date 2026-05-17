#!/usr/bin/env bash
# Sprint 2 B.6a MCP-surface end-to-end test.
#
# Builds a real demo Clarion database through `clarion analyze`, starts
# `clarion serve`, and sends Content-Length framed MCP JSON-RPC requests for
# the five storage-backed navigation tools.

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
VENV="${VENV:-$REPO_ROOT/plugins/python/.venv}"
CARGO_BUILD="${CARGO_BUILD:-1}"

log() { printf '[mcp-surface] %s\n' "$*" >&2; }
fail() { printf '[mcp-surface] FAIL: %s\n' "$*" >&2; exit 1; }

cd "$REPO_ROOT"

if [ "$CARGO_BUILD" = "1" ]; then
    log "building clarion (release) ..."
    cargo build --workspace --release
fi
CLARION_BIN="$REPO_ROOT/target/release/clarion"
[ -x "$CLARION_BIN" ] || fail "clarion binary missing at $CLARION_BIN"

if [ ! -d "$VENV" ]; then
    log "creating venv at $VENV ..."
    python3 -m venv "$VENV"
fi
log "installing clarion-plugin-python (editable) ..."
"$VENV/bin/pip" install --quiet -e "$REPO_ROOT/plugins/python[dev]"
PLUGIN_BIN="$VENV/bin/clarion-plugin-python"
[ -x "$PLUGIN_BIN" ] || fail "clarion-plugin-python missing at $PLUGIN_BIN"

DEMO_DIR="$(mktemp -d -t clarion-mcp-demo-XXXXXX)"
trap 'rm -rf "$DEMO_DIR"' EXIT
log "scratch project: $DEMO_DIR"
cd "$DEMO_DIR"
cat > demo.py <<'PY'
def world():
    return 42

def z_fallback():
    return -1

class Marker:
    pass

def hello():
    return world()

DISPATCH = {"k": world, "z": z_fallback}

def via_dispatch(key: str = "k"):
    return DISPATCH[key]()

def annotated(x: Marker) -> Marker:
    return x

CONST_REF = world
PY

export PATH="$REPO_ROOT/target/release:$VENV/bin:$PATH"

log "running: clarion install"
clarion install
[ -f "$DEMO_DIR/.clarion/clarion.db" ] || fail ".clarion/clarion.db not created"

log "running: clarion analyze ."
clarion analyze .

log "driving MCP stdio requests ..."
python3 - "$CLARION_BIN" "$DEMO_DIR" <<'PY'
import json
import subprocess
import sys
from pathlib import Path

clarion_bin = Path(sys.argv[1])
project_dir = Path(sys.argv[2])


def write_frame(proc: subprocess.Popen[bytes], message: dict[str, object]) -> None:
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    proc.stdin.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n")
    proc.stdin.write(body)
    proc.stdin.flush()


def read_frame(proc: subprocess.Popen[bytes]) -> dict[str, object]:
    headers: dict[str, str] = {}
    while True:
        line = proc.stdout.readline()
        if line == b"":
            stderr = proc.stderr.read().decode("utf-8", "replace")
            raise AssertionError(f"server closed stdout while reading headers; stderr={stderr}")
        if line == b"\r\n":
            break
        name, value = line.decode("ascii").strip().split(":", 1)
        headers[name.lower()] = value.strip()
    body = proc.stdout.read(int(headers["content-length"]))
    return json.loads(body)


def assert_tool_ok(response: dict[str, object]) -> dict[str, object]:
    result = response["result"]
    assert isinstance(result, dict), result
    content = result["content"]
    assert isinstance(content, list) and content, result
    envelope = json.loads(content[0]["text"])
    assert envelope["ok"] is True, envelope
    assert envelope["error"] is None, envelope
    assert "stats_delta" in envelope, envelope
    return envelope


proc = subprocess.Popen(
    [str(clarion_bin), "serve", "--path", str(project_dir)],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)
assert proc.stdin is not None
assert proc.stdout is not None
assert proc.stderr is not None

requests: list[tuple[str, dict[str, object]]] = [
    (
        "initialize",
        {
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "clarion-e2e", "version": "0.0.0"},
            },
        },
    ),
    ("tools", {"jsonrpc": "2.0", "id": "tools", "method": "tools/list", "params": {}}),
    (
        "entity-hit",
        {
            "jsonrpc": "2.0",
            "id": "entity-hit",
            "method": "tools/call",
            "params": {"name": "entity_at", "arguments": {"file": "demo.py", "line": 10}},
        },
    ),
    (
        "entity-miss",
        {
            "jsonrpc": "2.0",
            "id": "entity-miss",
            "method": "tools/call",
            "params": {"name": "entity_at", "arguments": {"file": "demo.py", "line": 99}},
        },
    ),
    (
        "find",
        {
            "jsonrpc": "2.0",
            "id": "find",
            "method": "tools/call",
            "params": {
                "name": "find_entity",
                "arguments": {"pattern": "python:function:demo", "limit": 2},
            },
        },
    ),
    (
        "callers-default",
        {
            "jsonrpc": "2.0",
            "id": "callers-default",
            "method": "tools/call",
            "params": {"name": "callers_of", "arguments": {"id": "python:function:demo.world"}},
        },
    ),
    (
        "callers-ambiguous",
        {
            "jsonrpc": "2.0",
            "id": "callers-ambiguous",
            "method": "tools/call",
            "params": {
                "name": "callers_of",
                "arguments": {
                    "id": "python:function:demo.z_fallback",
                    "confidence": "ambiguous",
                },
            },
        },
    ),
    (
        "paths",
        {
            "jsonrpc": "2.0",
            "id": "paths",
            "method": "tools/call",
            "params": {
                "name": "execution_paths_from",
                "arguments": {"id": "python:function:demo.hello", "max_depth": 2},
            },
        },
    ),
    (
        "neighborhood",
        {
            "jsonrpc": "2.0",
            "id": "neighborhood",
            "method": "tools/call",
            "params": {"name": "neighborhood", "arguments": {"id": "python:function:demo.world"}},
        },
    ),
]

responses: dict[str, dict[str, object]] = {}
try:
    for label, request in requests:
        write_frame(proc, request)
        response = read_frame(proc)
        assert response["id"] == request["id"], response
        responses[label] = response
finally:
    proc.stdin.close()
    status = proc.wait(timeout=5)
    stderr = proc.stderr.read().decode("utf-8", "replace")
    assert status == 0, f"clarion serve exited {status}; stderr={stderr}"

assert responses["initialize"]["result"]["protocolVersion"] == "2025-11-25"
tools = responses["tools"]["result"]["tools"]
assert len(tools) == 7, tools
assert [tool["name"] for tool in tools] == [
    "entity_at",
    "find_entity",
    "callers_of",
    "execution_paths_from",
    "summary",
    "issues_for",
    "neighborhood",
]
assert "leaf scope only" in tools[4]["description"]

entity_hit = assert_tool_ok(responses["entity-hit"])
assert entity_hit["result"]["entity"]["id"] == "python:function:demo.hello", entity_hit
assert entity_hit["truncated"] is False

entity_miss = assert_tool_ok(responses["entity-miss"])
assert entity_miss["result"]["entity"] is None, entity_miss

find_result = assert_tool_ok(responses["find"])
assert len(find_result["result"]["entities"]) == 2, find_result
assert find_result["result"]["next_cursor"] == "2", find_result

default_callers = assert_tool_ok(responses["callers-default"])
default_ids = {item["entity"]["id"] for item in default_callers["result"]["callers"]}
assert "python:function:demo.hello" in default_ids, default_callers
assert "python:function:demo.via_dispatch" not in default_ids, default_callers

ambiguous_callers = assert_tool_ok(responses["callers-ambiguous"])
ambiguous_ids = {item["entity"]["id"] for item in ambiguous_callers["result"]["callers"]}
assert "python:function:demo.via_dispatch" in ambiguous_ids, ambiguous_callers

paths = assert_tool_ok(responses["paths"])
path_ids = [[node["id"] for node in path] for path in paths["result"]["paths"]]
assert ["python:function:demo.hello", "python:function:demo.world"] in path_ids, paths
assert paths["truncated"] is False, paths

neighborhood = assert_tool_ok(responses["neighborhood"])
neighbor_callers = {item["entity"]["id"] for item in neighborhood["result"]["callers"]}
assert "python:function:demo.hello" in neighbor_callers, neighborhood
assert neighborhood["result"]["container"]["id"] == "python:module:demo", neighborhood
PY

log "PASS: MCP stdio surface returned storage-backed tool responses"
