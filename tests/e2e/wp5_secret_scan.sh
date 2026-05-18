#!/usr/bin/env bash
# WP5 pre-ingest secret-scanner end-to-end smoke.

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
CARGO_BUILD="${CARGO_BUILD:-1}"

log() { printf '[wp5-secret-scan] %s\n' "$*" >&2; }
fail() { printf '[wp5-secret-scan] FAIL: %s\n' "$*" >&2; exit 1; }

cd "$REPO_ROOT"

if [ "$CARGO_BUILD" = "1" ]; then
    log "building clarion (release) ..."
    cargo build --workspace --release
fi

CLARION_BIN="$REPO_ROOT/target/release/clarion"
[ -x "$CLARION_BIN" ] || fail "clarion binary missing at $CLARION_BIN"

DEMO_DIR="$(mktemp -d -t clarion-wp5-demo-XXXXXX)"
PLUGIN_DIR="$(mktemp -d -t clarion-wp5-plugin-XXXXXX)"
trap 'rm -rf "$DEMO_DIR" "$PLUGIN_DIR"' EXIT

cat > "$PLUGIN_DIR/clarion-plugin-secretfixture" <<'PY'
#!/usr/bin/python3
import json
import pathlib
import re
import sys

def read_frame():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if line in (b"", b"\r\n"):
            break
        name, value = line.decode("ascii").strip().split(":", 1)
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    return json.loads(sys.stdin.buffer.read(length))

def write_frame(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n")
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read_frame()
    method = msg.get("method")
    if method == "initialized":
        continue
    if method == "exit":
        raise SystemExit(0)
    ident = msg["id"]
    if method == "initialize":
        write_frame({"jsonrpc":"2.0","id":ident,"result":{"name":"clarion-plugin-secretfixture","version":"0.1.0","ontology_version":"0.1.0","capabilities":{}}})
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        name = "file_" + re.sub(r"[^A-Za-z0-9_]", "_", pathlib.Path(path).name)
        write_frame({"jsonrpc":"2.0","id":ident,"result":{"entities":[{"id":"secretfixture:module:"+name,"kind":"module","qualified_name":name,"source":{"file_path":path}}],"edges":[]}})
    elif method == "shutdown":
        write_frame({"jsonrpc":"2.0","id":ident,"result":{}})
    else:
        raise SystemExit(1)
PY
chmod 755 "$PLUGIN_DIR/clarion-plugin-secretfixture"

cat > "$PLUGIN_DIR/plugin.toml" <<'TOML'
[plugin]
name = "clarion-plugin-secretfixture"
plugin_id = "secretfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-secretfixture"
language = "secretfixture"
extensions = ["sec"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "CLA-SECRET-FIXTURE-"
ontology_version = "0.1.0"
TOML

log "scratch project: $DEMO_DIR"
"$CLARION_BIN" install --path "$DEMO_DIR"
printf "aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n" > "$DEMO_DIR/leaky.sec"

PATH="$PLUGIN_DIR" "$CLARION_BIN" analyze "$DEMO_DIR"

DB="$DEMO_DIR/.clarion/clarion.db"
BLOCKED=$(sqlite3 "$DB" "select json_extract(properties, '\$.briefing_blocked') from entities;")
[ "$BLOCKED" = "secret_present" ] || fail "expected briefing_blocked secret_present, got $BLOCKED"

FINDINGS=$(sqlite3 "$DB" "select count(*) from findings where rule_id = 'CLA-SEC-SECRET-DETECTED';")
[ "$FINDINGS" = "1" ] || fail "expected one CLA-SEC-SECRET-DETECTED finding, got $FINDINGS"

RUN_STATUS=$(sqlite3 "$DB" "select status from runs;")
[ "$RUN_STATUS" = "completed" ] || fail "expected completed run, got $RUN_STATUS"

log "PASS: WP5 secret scan blocks summaries and records finding"
