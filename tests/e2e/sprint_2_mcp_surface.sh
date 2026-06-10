#!/usr/bin/env bash
# Sprint 2 B.6 MCP-surface end-to-end test.
#
# Builds a real demo Loomweave database through `loomweave analyze`, starts
# `loomweave serve`, and sends Content-Length framed MCP JSON-RPC requests for
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
    log "building loomweave (release) ..."
    cargo build --workspace --release
fi
LOOMWEAVE_BIN="$REPO_ROOT/target/release/loomweave"
[ -x "$LOOMWEAVE_BIN" ] || fail "loomweave binary missing at $LOOMWEAVE_BIN"

if [ ! -d "$VENV" ]; then
    log "creating venv at $VENV ..."
    python3 -m venv "$VENV"
fi
log "installing loomweave-plugin-python (editable) ..."
"$VENV/bin/pip" install --quiet -e "$REPO_ROOT/plugins/python[dev]"
PLUGIN_BIN="$VENV/bin/loomweave-plugin-python"
[ -x "$PLUGIN_BIN" ] || fail "loomweave-plugin-python missing at $PLUGIN_BIN"

DEMO_DIR="$(mktemp -d -t loomweave-mcp-demo-XXXXXX)"
trap 'rm -rf "$DEMO_DIR"' EXIT
# Hermetic install (clarion-c5e3cc2818): `loomweave install` registers a
# Codex MCP entry in ~/.codex/config.toml unless this override points it
# at a scratch-local file. Never mutate the operator's real config.
export LOOMWEAVE_CODEX_CONFIG="$DEMO_DIR/codex-config.toml"
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

class Special(Marker):
    pass

def tagged(fn):
    return fn

@tagged
def handler():
    return world()
PY

export PATH="$REPO_ROOT/target/release:$VENV/bin:$PATH"

log "running: loomweave install"
loomweave install
[ -f "$DEMO_DIR/.weft/loomweave/loomweave.db" ] || fail ".weft/loomweave/loomweave.db not created"

log "running: loomweave analyze ."
loomweave analyze .

log "driving MCP stdio requests ..."
python3 - "$LOOMWEAVE_BIN" "$DEMO_DIR" <<'PY'
import json
import sqlite3
import subprocess
import sys
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

loomweave_bin = Path(sys.argv[1])
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


conn = sqlite3.connect(project_dir / ".weft" / "loomweave" / "loomweave.db")
world_hash = conn.execute(
    "SELECT content_hash FROM entities WHERE id = ?",
    ("python:function:demo.world",),
).fetchone()[0]
world_sei = conn.execute(
    "SELECT sei FROM sei_bindings WHERE current_locator = ? AND status = 'alive'",
    ("python:function:demo.world",),
).fetchone()[0]
hello_sei = conn.execute(
    "SELECT sei FROM sei_bindings WHERE current_locator = ? AND status = 'alive'",
    ("python:function:demo.hello",),
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
    "You are summarising one Loomweave entity at leaf scope only.\n"
    f"Entity id: {world_entity[0]}\n"
    f"Kind: {world_entity[1]}\n"
    f"Name: {world_entity[2]}\n"
    "Matching guidance:\nNo matching guidance.\n"
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
# Write the recording fixture to the path the serve config points at. The store
# moved under .weft/loomweave/ (ADR-046) but this fixture write lagged behind in
# .weft/, so serve could never find it and exited 1 before any tool assertion —
# the same outside-the-blocking-floor drift that hid the stale tool list.
(project_dir / ".weft" / "loomweave" / "openrouter-recording.json").write_text(
    json.dumps(recording_fixture, separators=(",", ":")),
    encoding="utf-8",
)
filigree_requests: list[str] = []


class FiligreeHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        parsed = urllib.parse.urlparse(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        if parsed.path == "/api/entity-associations":
            entity_id = query.get("entity_id", [""])[0]
            filigree_requests.append(entity_id)
            associations: list[dict[str, str]] = []
            if entity_id in {"python:function:demo.world", world_sei}:
                associations.append(
                    {
                        "issue_id": "filigree-world",
                        "loomweave_entity_id": entity_id,
                        "content_hash_at_attach": world_hash,
                        "attached_at": "2026-05-17T00:00:00.000Z",
                        "attached_by": "codex",
                    }
                )
            elif entity_id in {"python:function:demo.hello", hello_sei}:
                associations.append(
                    {
                        "issue_id": "filigree-hello-drifted",
                        "loomweave_entity_id": entity_id,
                        "content_hash_at_attach": "old-hash",
                        "attached_at": "2026-05-17T00:00:00.000Z",
                        "attached_by": "codex",
                    }
                )
            body = json.dumps({"associations": associations}).encode("utf-8")
        elif parsed.path == "/api/weft/files":
            path_prefix = query.get("path_prefix", [""])[0]
            items = []
            if query.get("scan_source", [""])[0] == "wardline" and path_prefix == "demo.py":
                items = [
                    {
                        "file_id": "wardline-demo-py",
                        "path": "demo.py",
                        "language": "python",
                        "file_type": "source",
                    }
                ]
            body = json.dumps({"items": items, "has_more": False}).encode("utf-8")
        elif parsed.path == "/api/weft/findings":
            body = json.dumps({"items": [], "has_more": False}).encode("utf-8")
        else:
            self.send_error(404)
            return
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
(project_dir / "loomweave.yaml").write_text(
    f"""
version: 1
llm_policy:
  enabled: true
  provider: recording
  model_id: anthropic/claude-sonnet-4.6
  session_token_ceiling: 1000000
  recording_fixture_path: .weft/loomweave/openrouter-recording.json
serve:
  mcp:
    enable_write_tools: true
integrations:
  filigree:
    enabled: true
    base_url: http://127.0.0.1:{filigree_server.server_port}
    actor: loomweave-e2e
    token_env: WEFT_FEDERATION_TOKEN
    timeout_seconds: 2
""".lstrip(),
    encoding="utf-8",
)

proc = subprocess.Popen(
    [str(loomweave_bin), "serve", "--path", str(project_dir)],
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
                "clientInfo": {"name": "loomweave-e2e", "version": "0.0.0"},
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
        "relations-in",
        {
            "jsonrpc": "2.0",
            "id": "relations-in",
            "method": "tools/call",
            "params": {
                "name": "entity_relation_list",
                "arguments": {"id": "python:class:demo.Marker", "direction": "in"},
            },
        },
    ),
    (
        "relations-out-decorator",
        {
            "jsonrpc": "2.0",
            "id": "relations-out-decorator",
            "method": "tools/call",
            "params": {
                "name": "entity_relation_list",
                "arguments": {"id": "python:function:demo.tagged", "direction": "out"},
            },
        },
    ),
    (
        "neighborhood-marker",
        {
            "jsonrpc": "2.0",
            "id": "neighborhood-marker",
            "method": "tools/call",
            "params": {"name": "neighborhood", "arguments": {"id": "python:class:demo.Marker"}},
        },
    ),
    (
        "context",
        {
            "jsonrpc": "2.0",
            "id": "context",
            "method": "resources/read",
            "params": {"uri": "loomweave://context"},
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
    assert status == 0, f"loomweave serve exited {status}; stderr={stderr}"

assert responses["initialize"]["result"]["protocolVersion"] == "2025-11-25"
init_result = responses["initialize"]["result"]
assert "loomweave-workflow" in init_result["instructions"], init_result.get("instructions")
assert isinstance(init_result["capabilities"]["resources"], dict), init_result["capabilities"]
assert isinstance(init_result["capabilities"]["prompts"], dict), init_result["capabilities"]
tools = responses["tools"]["result"]["tools"]
tool_names = [tool["name"] for tool in tools]
assert tool_names == [
    "entity_at",
    "entity_find",
    "entity_callers_list",
    "entity_execution_path_list",
    "entity_summary_get",
    "entity_issue_list",
    "entity_neighborhood_get",
    "subsystem_member_list",
    "entity_subsystem_get",
    "project_status_get",
    "entity_summary_preview_cost_get",
    "entity_source_get",
    "entity_call_site_list",
    "entity_orientation_pack_get",
    "analyze_start",
    "analyze_status_get",
    "analyze_cancel",
    "index_diff_get",
    "entity_guidance_list",
    "propose_guidance",
    "promote_guidance",
    "entity_finding_list",
    "entity_wardline_get",
    "entity_tag_list",
    "entity_kind_list",
    "entity_wardline_list",
    "module_circular_import_list",
    "entity_coupling_hotspot_list",
    "entity_entry_point_list",
    "entity_http_route_list",
    "entity_data_model_list",
    "entity_test_list",
    "entity_deprecation_list",
    "entity_todo_list",
    "entity_test_caller_list",
    "entity_high_churn_list",
    "entity_recent_change_list",
    "entity_dead_list",
    "entity_semantic_search_list",
    "project_finding_list",
    "entity_resolve",
    "entity_relation_list",
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
assert world_sei in filigree_requests, filigree_requests
assert hello_sei in filigree_requests, filigree_requests
assert issues["result"]["wardline_findings"]["result_kind"] == "no_matches", issues

# Relation read surface (clarion-ae5b43ea40, direction semantics ADR-051):
# "what subclasses Marker" is direction=in on inherits_from, through a REAL
# analyze-built index — not a seeded DB.
relations_in = assert_tool_ok(responses["relations-in"])
rel_rows = relations_in["result"]["relations"]
assert len(rel_rows) == 1, relations_in
rel = rel_rows[0]
assert rel["kind"] == "inherits_from", relations_in
assert rel["entity"]["id"] == "python:class:demo.Special", relations_in
assert rel["edge_confidence"] == "resolved", relations_in
assert rel["line_text"] == "class Special(Marker):", relations_in
assert rel["file"].endswith("demo.py"), relations_in
assert rel["source_status"] == "ok", relations_in
assert relations_in["result"]["truncated"] is False, relations_in

# "what does @tagged decorate" is direction=out on the DECORATOR (the from
# side); the anchor line is the @tagged token at the decoration site.
relations_out = assert_tool_ok(responses["relations-out-decorator"])
deco_rows = relations_out["result"]["relations"]
assert len(deco_rows) == 1, relations_out
deco = deco_rows[0]
assert deco["kind"] == "decorates", relations_out
assert deco["entity"]["id"] == "python:function:demo.handler", relations_out
assert deco["line_text"] == "@tagged", relations_out
assert deco["source_status"] == "ok", relations_out

# The neighborhood overview carries the same edges as kind-tagged buckets.
nb_marker = assert_tool_ok(responses["neighborhood-marker"])
nb_rel_in = nb_marker["result"]["relations_in"]
assert {(r["kind"], r["entity"]["id"]) for r in nb_rel_in} == {
    ("inherits_from", "python:class:demo.Special")
}, nb_marker
assert nb_marker["result"]["truncated"]["relations_in"] is False, nb_marker
assert nb_marker["result"]["relations_out"] == [], nb_marker

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
