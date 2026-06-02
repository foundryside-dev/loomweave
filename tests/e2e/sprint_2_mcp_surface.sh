#!/usr/bin/env bash
# Sprint 2 B.6 MCP-surface end-to-end test.
#
# Builds a real demo Clarion database through `clarion analyze`, starts
# `clarion serve`, and sends Content-Length framed MCP JSON-RPC requests for
# the MCP navigation tools. Filigree is represented by a local HTTP
# server that implements the B.7 reverse entity-association route.

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
import sqlite3
import subprocess
import sys
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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


conn = sqlite3.connect(project_dir / ".clarion" / "clarion.db")
world_hash = conn.execute(
    "SELECT content_hash FROM entities WHERE id = ?",
    ("python:function:demo.world",),
).fetchone()[0]
world_entity = conn.execute(
    """
    SELECT id, kind, name, source_file_path, source_line_start, source_line_end
    FROM entities
    WHERE id = ?
    """,
    ("python:function:demo.world",),
).fetchone()
conn.close()
world_source = Path(world_entity[3]).read_text(encoding="utf-8")
world_lines = world_source.splitlines(keepends=True)
world_excerpt = "".join(world_lines[int(world_entity[4]) - 1 : int(world_entity[5])])
world_prompt = (
    "You are summarising one Clarion entity at leaf scope only.\n"
    f"Entity id: {world_entity[0]}\n"
    f"Kind: {world_entity[1]}\n"
    f"Name: {world_entity[2]}\n"
    f"Source excerpt:\n{world_excerpt}\n"
    "Return JSON with purpose, behavior, relationships, and risks fields."
)
recording_fixture = [
    {
        "request": {
            "purpose": "Summary",
            "model_id": "anthropic/claude-sonnet-4.6",
            "prompt_id": "leaf-v1",
            "prompt": world_prompt,
            "max_output_tokens": 512,
        },
        "response": {
            "model_id": "anthropic/claude-sonnet-4.6",
            "output_json": json.dumps(
                {
                    "purpose": "openrouter-recorded",
                    "behavior": "Returns the demo constant.",
                    "relationships": ["called by hello"],
                    "risks": [],
                },
                separators=(",", ":"),
            ),
            "input_tokens": 21,
            "output_tokens": 9,
            "total_tokens": 30,
            "cost_usd": 0.0,
        },
    }
]
(project_dir / ".clarion" / "openrouter-recording.json").write_text(
    json.dumps(recording_fixture, separators=(",", ":")),
    encoding="utf-8",
)
filigree_requests: list[str] = []


class FiligreeHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path != "/api/entity-associations":
            self.send_error(404)
            return
        entity_id = urllib.parse.parse_qs(parsed.query).get("entity_id", [""])[0]
        filigree_requests.append(entity_id)
        associations: list[dict[str, str]] = []
        if entity_id == "python:function:demo.world":
            associations.append(
                {
                    "issue_id": "filigree-world",
                    "clarion_entity_id": entity_id,
                    "content_hash_at_attach": world_hash,
                    "attached_at": "2026-05-17T00:00:00.000Z",
                    "attached_by": "codex",
                }
            )
        elif entity_id == "python:function:demo.hello":
            associations.append(
                {
                    "issue_id": "filigree-hello-drifted",
                    "clarion_entity_id": entity_id,
                    "content_hash_at_attach": "old-hash",
                    "attached_at": "2026-05-17T00:00:00.000Z",
                    "attached_by": "codex",
                }
            )
        body = json.dumps({"associations": associations}).encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *_args: object) -> None:
        return


filigree_server = ThreadingHTTPServer(("127.0.0.1", 0), FiligreeHandler)
filigree_thread = threading.Thread(target=filigree_server.serve_forever, daemon=True)
filigree_thread.start()
(project_dir / "clarion.yaml").write_text(
    f"""
version: 1
llm_policy:
  enabled: true
  provider: recording
  model_id: anthropic/claude-sonnet-4.6
  session_token_ceiling: 1000000
  recording_fixture_path: .clarion/openrouter-recording.json
integrations:
  filigree:
    enabled: true
    base_url: http://127.0.0.1:{filigree_server.server_port}
    actor: clarion-e2e
    token_env: FILIGREE_API_TOKEN
    timeout_seconds: 2
""".lstrip(),
    encoding="utf-8",
)

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
    (
        "summary",
        {
            "jsonrpc": "2.0",
            "id": "summary",
            "method": "tools/call",
            "params": {"name": "summary", "arguments": {"id": "python:function:demo.world"}},
        },
    ),
    (
        "issues",
        {
            "jsonrpc": "2.0",
            "id": "issues",
            "method": "tools/call",
            "params": {"name": "issues_for", "arguments": {"id": "python:module:demo"}},
        },
    ),
    (
        "source-for-entity",
        {
            "jsonrpc": "2.0",
            "id": "source-for-entity",
            "method": "tools/call",
            "params": {
                "name": "source_for_entity",
                "arguments": {"id": "python:function:demo.hello", "context_lines": 1},
            },
        },
    ),
    (
        "call-sites",
        {
            "jsonrpc": "2.0",
            "id": "call-sites",
            "method": "tools/call",
            "params": {
                "name": "call_sites",
                "arguments": {"id": "python:function:demo.hello", "role": "caller"},
            },
        },
    ),
    (
        "context",
        {
            "jsonrpc": "2.0",
            "id": "context",
            "method": "resources/read",
            "params": {"uri": "clarion://context"},
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
    filigree_server.shutdown()
    filigree_server.server_close()
    assert status == 0, f"clarion serve exited {status}; stderr={stderr}"

assert responses["initialize"]["result"]["protocolVersion"] == "2025-11-25"
init_result = responses["initialize"]["result"]
assert "clarion-workflow" in init_result["instructions"], init_result.get("instructions")
assert isinstance(init_result["capabilities"]["resources"], dict), init_result["capabilities"]
assert isinstance(init_result["capabilities"]["prompts"], dict), init_result["capabilities"]
tools = responses["tools"]["result"]["tools"]
tool_names = [tool["name"] for tool in tools]
assert tool_names == [
    "entity_at",
    "find_entity",
    "callers_of",
    "execution_paths_from",
    "summary",
    "issues_for",
    "neighborhood",
    "subsystem_members",
    "subsystem_of",
    "project_status",
    "summary_preview_cost",
    "source_for_entity",
    "call_sites",
    "orientation_pack",
    "analyze_start",
    "analyze_status",
    "analyze_cancel",
    "index_diff",
    "guidance_for",
    "findings_for",
    "wardline_for",
    "find_by_tag",
    "find_by_kind",
    "find_by_wardline",
    "find_circular_imports",
    "find_coupling_hotspots",
    "find_entry_points",
    "find_http_routes",
    "find_data_models",
    "find_tests",
    "find_deprecations",
    "find_todos",
    "what_tests_this",
    "high_churn",
    "recently_changed",
    "find_dead_code",
    "search_semantic",
], tool_names
# Single-source check (clarion-71f0d6c3dd): the initialize `instructions` tool
# enumeration is derived from list_tools(), so every advertised tool must appear
# in it. This catches drift between the tool set and the orientation prose
# without a second hardcoded list.
for name in tool_names:
    assert name in init_result["instructions"], (name, init_result["instructions"])
assert "leaf scope only" in tools[4]["description"]

entity_hit = assert_tool_ok(responses["entity-hit"])
assert entity_hit["result"]["entity"]["id"] == "python:function:demo.hello", entity_hit
assert entity_hit["truncated"] is False

entity_miss = assert_tool_ok(responses["entity-miss"])
assert entity_miss["result"]["entity"] is None, entity_miss

source_for_entity = assert_tool_ok(responses["source-for-entity"])
sfe_result = source_for_entity["result"]
assert sfe_result["source_status"] == "ok", source_for_entity
assert sfe_result["entity"]["id"] == "python:function:demo.hello", source_for_entity
# Line-numbered lines, with the entity's own lines flagged in_entity=True.
sfe_lines = sfe_result["lines"]
assert sfe_lines, source_for_entity
assert all("number" in line and "in_entity" in line for line in sfe_lines), source_for_entity
assert any(line["in_entity"] for line in sfe_lines), source_for_entity

call_sites = assert_tool_ok(responses["call-sites"])
cs_result = call_sites["result"]
assert cs_result["role"] == "caller", call_sites
assert "sites" in cs_result and "unresolved_sites" in cs_result, call_sites
# Every resolved site carries its edge kind, confidence, and source line text.
for site in cs_result["sites"]:
    assert site["edge_kind"] in ("calls", "references"), call_sites
    assert "confidence" in site and "line_text" in site, call_sites

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
# Compact shape (clarion-5b3eff9a91): paths are arrays of node-id strings into a
# deduplicated node table; the queried entity is echoed as root.
path_ids = paths["result"]["paths"]
assert ["python:function:demo.hello", "python:function:demo.world"] in path_ids, paths
assert paths["result"]["root"] == "python:function:demo.hello", paths
node_ids = {node["id"] for node in paths["result"]["nodes"]}
assert "python:function:demo.world" in node_ids, paths
assert all("content_hash" not in node for node in paths["result"]["nodes"]), paths
assert paths["truncated"] is False, paths

neighborhood = assert_tool_ok(responses["neighborhood"])
neighbor_callers = {item["entity"]["id"] for item in neighborhood["result"]["callers"]}
assert "python:function:demo.hello" in neighbor_callers, neighborhood
assert neighborhood["result"]["container"]["id"] == "python:module:demo", neighborhood

summary = assert_tool_ok(responses["summary"])
assert summary["result"]["cache"]["hit"] is False, summary
assert summary["result"]["summary"]["purpose"] == "openrouter-recorded", summary
assert summary["result"]["usage"]["tokens_input"] == 21, summary
assert summary["result"]["usage"]["tokens_output"] == 9, summary
assert summary["result"]["usage"]["tokens_total"] == 30, summary
assert summary["stats_delta"]["summary_cache_misses_total"] == 1, summary
assert summary["stats_delta"]["summary_tokens_total"] == 30, summary

issues = assert_tool_ok(responses["issues"])
assert issues["result"]["available"] is True, issues
matched_ids = {item["issue_id"] for item in issues["result"]["matched"]}
drifted_ids = {item["issue_id"] for item in issues["result"]["drifted"]}
assert "filigree-world" in matched_ids, issues
assert "filigree-hello-drifted" in drifted_ids, issues
# issues_for surfaces the resolved Filigree endpoint + a result_kind taxonomy
# (clarion-318f1254eb): a populated result is "matched" and reports the endpoint
# it was served from.
assert issues["result"]["result_kind"] == "matched", issues
assert issues["result"]["filigree_endpoint"]["enabled"] is True, issues
assert issues["result"]["filigree_endpoint"]["resolved_url"], issues
assert issues["stats_delta"]["filigree_requests_total"] >= 2, issues
assert "python:function:demo.world" in filigree_requests, filigree_requests
assert "python:function:demo.hello" in filigree_requests, filigree_requests

context = responses["context"]["result"]
ctx_text = context["contents"][0]["text"]
ctx = json.loads(ctx_text)
assert ctx["db_present"] is True, ctx
assert ctx["entity_count"] >= 1, ctx
assert "staleness" in ctx, ctx
# A live, healthy snapshot must report degraded=false; the field is always
# present so a consumer can tell a broken read from a genuinely empty index.
assert ctx["degraded"] is False, ctx
PY

log "PASS: MCP stdio surface returned the full tool catalogue and all expected tool responses"
