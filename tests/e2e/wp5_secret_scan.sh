#!/usr/bin/env bash
# WP5 pre-ingest secret-scanner end-to-end smoke.

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
CARGO_BUILD="${CARGO_BUILD:-1}"

log() { printf '[wp5-secret-scan] %s\n' "$*" >&2; }
fail() { printf '[wp5-secret-scan] FAIL: %s\n' "$*" >&2; exit 1; }

cd "$REPO_ROOT"

if [ "$CARGO_BUILD" = "1" ]; then
    log "building loomweave (release) ..."
    cargo build --workspace --release
fi

LOOMWEAVE_BIN="$REPO_ROOT/target/release/loomweave"
[ -x "$LOOMWEAVE_BIN" ] || fail "loomweave binary missing at $LOOMWEAVE_BIN"

DEMO_DIR="$(mktemp -d -t loomweave-wp5-demo-XXXXXX)"
PLUGIN_DIR="$(mktemp -d -t loomweave-wp5-plugin-XXXXXX)"
trap 'rm -rf "$DEMO_DIR" "$PLUGIN_DIR"' EXIT
# Hermetic install (clarion-c5e3cc2818): `loomweave install` registers a
# Codex MCP entry in ~/.codex/config.toml unless this override points it
# at a scratch-local file. Never mutate the operator's real config.
export LOOMWEAVE_CODEX_CONFIG="$DEMO_DIR/codex-config.toml"

cat > "$PLUGIN_DIR/loomweave-plugin-secretfixture" <<'PY'
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
        write_frame({"jsonrpc":"2.0","id":ident,"result":{"name":"loomweave-plugin-secretfixture","version":"0.1.0","ontology_version":"0.1.0","capabilities":{}}})
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        name = "file_" + re.sub(r"[^A-Za-z0-9_]", "_", pathlib.Path(path).name)
        write_frame({"jsonrpc":"2.0","id":ident,"result":{"entities":[{"id":"secretfixture:module:"+name,"kind":"module","qualified_name":name,"source":{"file_path":path}}],"edges":[]}})
    elif method == "shutdown":
        write_frame({"jsonrpc":"2.0","id":ident,"result":{}})
    else:
        raise SystemExit(1)
PY
chmod 755 "$PLUGIN_DIR/loomweave-plugin-secretfixture"

cat > "$PLUGIN_DIR/plugin.toml" <<'TOML'
[plugin]
name = "loomweave-plugin-secretfixture"
plugin_id = "secretfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "loomweave-plugin-secretfixture"
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
rule_id_prefix = "LMWV-SECRET-FIXTURE-"
ontology_version = "0.1.0"
TOML

log "scratch project: $DEMO_DIR"
"$LOOMWEAVE_BIN" install --path "$DEMO_DIR"
printf "aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n" > "$DEMO_DIR/leaky.sec"
printf "aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n" > "$DEMO_DIR/.env"

PATH="$PLUGIN_DIR" "$LOOMWEAVE_BIN" analyze "$DEMO_DIR"

DB="$DEMO_DIR/.weft/loomweave/loomweave.db"
BLOCKED=$(sqlite3 "$DB" "select count(*) from entities where json_extract(properties, '\$.briefing_blocked') = 'secret_present';")
[ "$BLOCKED" = "3" ] || fail "expected three briefing_blocked secret_present entities (core file + plugin source entity + dotenv anchor), got $BLOCKED"

FINDINGS=$(sqlite3 "$DB" "select count(*) from findings where rule_id = 'LMWV-SEC-SECRET-DETECTED';")
[ "$FINDINGS" = "2" ] || fail "expected two LMWV-SEC-SECRET-DETECTED findings, got $FINDINGS"

DOTENV_ANCHOR=$(sqlite3 "$DB" "select count(*) from entities where plugin_id = 'core' and kind = 'file' and source_file_path = '$DEMO_DIR/.env' and json_extract(properties, '\$.briefing_blocked') = 'secret_present';")
[ "$DOTENV_ANCHOR" = "1" ] || fail "expected .env core file anchor with briefing_blocked secret_present, got $DOTENV_ANCHOR"

RUN_STATUS=$(sqlite3 "$DB" "select status from runs;")
[ "$RUN_STATUS" = "completed" ] || fail "expected completed run, got $RUN_STATUS"

log "PASS: WP5 secret scan blocks summaries and records finding"

# ----------------------------------------------------------------------------
# baseline-suppression flow (clarion-55fc5aa885 §I11)
# ----------------------------------------------------------------------------
log "scenario: baseline-suppressed detection emits BASELINE-MATCH only, no block"
BASELINE_DIR="$(mktemp -d -t loomweave-wp5-baseline-XXXXXX)"
trap 'rm -rf "$DEMO_DIR" "$PLUGIN_DIR" "$BASELINE_DIR" "$OVERRIDE_DIR" "$MALFORMED_DIR"' EXIT
"$LOOMWEAVE_BIN" install --path "$BASELINE_DIR"
printf "aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n" > "$BASELINE_DIR/leaky.sec"
HASHED_SECRET=$(printf 'AKIAIOSFODNN7EXAMPLE' | sha1sum | awk '{print $1}')
cat > "$BASELINE_DIR/.weft/loomweave/secrets-baseline.yaml" <<YAML
version: "1.0"
results:
  "leaky.sec":
    - type: "AWS Access Key"
      hashed_secret: "$HASHED_SECRET"
      line_number: 1
      is_secret: false
      justification: "Documented public AWS example key, not a credential."
YAML
PATH="$PLUGIN_DIR" "$LOOMWEAVE_BIN" analyze "$BASELINE_DIR"
BDB="$BASELINE_DIR/.weft/loomweave/loomweave.db"
B_BLOCKED=$(sqlite3 "$BDB" "select count(*) from entities where json_extract(properties, '\$.briefing_blocked') = 'secret_present';")
[ "$B_BLOCKED" = "0" ] || fail "baseline-suppressed: expected 0 blocked entities, got $B_BLOCKED"
B_SECRET=$(sqlite3 "$BDB" "select count(*) from findings where rule_id = 'LMWV-SEC-SECRET-DETECTED';")
[ "$B_SECRET" = "0" ] || fail "baseline-suppressed: expected 0 SECRET_DETECTED findings, got $B_SECRET"
B_BASELINE=$(sqlite3 "$BDB" "select count(*) from findings where rule_id = 'LMWV-INFRA-SECRET-BASELINE-MATCH';")
[ "$B_BASELINE" = "1" ] || fail "baseline-suppressed: expected 1 BASELINE-MATCH finding, got $B_BASELINE"
log "PASS: baseline-suppressed detection clears the block and emits BASELINE-MATCH"

# ----------------------------------------------------------------------------
# --allow-unredacted-secrets override admission (clarion-55fc5aa885 §I5, §I11)
# ----------------------------------------------------------------------------
log "scenario: confirmed --allow-unredacted-secrets keeps run, emits BOTH SECRET_DETECTED and UNREDACTED-SECRETS-ALLOWED"
OVERRIDE_DIR="$(mktemp -d -t loomweave-wp5-override-XXXXXX)"
"$LOOMWEAVE_BIN" install --path "$OVERRIDE_DIR"
printf "aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n" > "$OVERRIDE_DIR/leaky.sec"
PATH="$PLUGIN_DIR" "$LOOMWEAVE_BIN" analyze \
    --allow-unredacted-secrets \
    --confirm-allow-unredacted-secrets=yes-i-understand \
    "$OVERRIDE_DIR"
ODB="$OVERRIDE_DIR/.weft/loomweave/loomweave.db"
O_BLOCKED=$(sqlite3 "$ODB" "select count(*) from entities where json_extract(properties, '\$.briefing_blocked') = 'secret_present';")
[ "$O_BLOCKED" = "0" ] || fail "override: expected 0 blocked entities, got $O_BLOCKED"
O_SECRET=$(sqlite3 "$ODB" "select count(*) from findings where rule_id = 'LMWV-SEC-SECRET-DETECTED';")
[ "$O_SECRET" = "1" ] || fail "override is additive per ADR-013: expected 1 SECRET_DETECTED finding, got $O_SECRET"
O_OVERRIDE=$(sqlite3 "$ODB" "select count(*) from findings where rule_id = 'LMWV-SEC-UNREDACTED-SECRETS-ALLOWED';")
[ "$O_OVERRIDE" = "1" ] || fail "override: expected 1 UNREDACTED-SECRETS-ALLOWED finding, got $O_OVERRIDE"
O_OVERRIDE_USED=$(sqlite3 "$ODB" "select json_extract(stats, '\$.secret_override_used') from runs;")
[ "$O_OVERRIDE_USED" = "1" ] || fail "override: expected stats.secret_override_used = 1, got $O_OVERRIDE_USED"
log "PASS: override admission lands both SECRET_DETECTED and UNREDACTED-SECRETS-ALLOWED"

# ----------------------------------------------------------------------------
# malformed-baseline abort (clarion-55fc5aa885 §I11)
# ----------------------------------------------------------------------------
log "scenario: malformed baseline aborts analyze before BeginRun (exit 78)"
MALFORMED_DIR="$(mktemp -d -t loomweave-wp5-malformed-XXXXXX)"
"$LOOMWEAVE_BIN" install --path "$MALFORMED_DIR"
printf "harmless = 'nothing'\n" > "$MALFORMED_DIR/clean.sec"
printf "not: valid: yaml: [\n" > "$MALFORMED_DIR/.weft/loomweave/secrets-baseline.yaml"
set +e
PATH="$PLUGIN_DIR" "$LOOMWEAVE_BIN" analyze "$MALFORMED_DIR" 2>/dev/null
MALFORMED_EXIT=$?
set -e
[ "$MALFORMED_EXIT" -ne 0 ] || fail "malformed baseline: expected non-zero exit, got $MALFORMED_EXIT"
MDB="$MALFORMED_DIR/.weft/loomweave/loomweave.db"
M_RUNS=$(sqlite3 "$MDB" "select count(*) from runs;")
[ "$M_RUNS" = "0" ] || fail "malformed baseline must abort BEFORE BeginRun; got $M_RUNS run rows"
log "PASS: malformed baseline aborts with non-zero exit and no runs row"

# ----------------------------------------------------------------------------
# retry-after-baseline-add: rerun the baseline-suppression scenario but where
# the baseline is added AFTER an initial blocked run, simulating the
# operator workflow (clarion-55fc5aa885 §I11). This re-runs analyze on
# DEMO_DIR (which started blocked) after we wipe and re-add a baseline.
# ----------------------------------------------------------------------------
log "scenario: retry-after-baseline-add unblocks a previously-blocked file"
RETRY_DIR="$(mktemp -d -t loomweave-wp5-retry-XXXXXX)"
trap 'rm -rf "$DEMO_DIR" "$PLUGIN_DIR" "$BASELINE_DIR" "$OVERRIDE_DIR" "$MALFORMED_DIR" "$RETRY_DIR"' EXIT
"$LOOMWEAVE_BIN" install --path "$RETRY_DIR"
printf "aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n" > "$RETRY_DIR/leaky.sec"
# First run blocks (no baseline yet).
PATH="$PLUGIN_DIR" "$LOOMWEAVE_BIN" analyze "$RETRY_DIR"
RDB="$RETRY_DIR/.weft/loomweave/loomweave.db"
R_BLOCKED_BEFORE=$(sqlite3 "$RDB" "select count(*) from entities where json_extract(properties, '\$.briefing_blocked') = 'secret_present';")
[ "$R_BLOCKED_BEFORE" = "2" ] || fail "retry: first run must block the source file entity and plugin entity, got $R_BLOCKED_BEFORE"
# Operator commits a baseline acknowledging the example key.
cat > "$RETRY_DIR/.weft/loomweave/secrets-baseline.yaml" <<YAML
version: "1.0"
results:
  "leaky.sec":
    - type: "AWS Access Key"
      hashed_secret: "$HASHED_SECRET"
      line_number: 1
      is_secret: false
      justification: "Documented public AWS example key — false positive."
YAML
# Re-running analyze should NOT block this file.
PATH="$PLUGIN_DIR" "$LOOMWEAVE_BIN" analyze "$RETRY_DIR"
R_BLOCKED_AFTER=$(sqlite3 "$RDB" "select count(*) from entities where json_extract(properties, '\$.briefing_blocked') = 'secret_present' and source_file_path like '%leaky.sec';")
[ "$R_BLOCKED_AFTER" = "0" ] || fail "retry-after-baseline: leaky.sec must no longer be blocked, got $R_BLOCKED_AFTER"
R_BASELINE_HITS=$(sqlite3 "$RDB" "select count(*) from findings where rule_id = 'LMWV-INFRA-SECRET-BASELINE-MATCH';")
[ "$R_BASELINE_HITS" -ge "1" ] || fail "retry-after-baseline: expected BASELINE-MATCH finding, got $R_BASELINE_HITS"
log "PASS: retry-after-baseline-add unblocks the previously-blocked file"

log "PASS: WP5 secret scan — all 5 scenarios (smoke, baseline, override, malformed, retry)"
