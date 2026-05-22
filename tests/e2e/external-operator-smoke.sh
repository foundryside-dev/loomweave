#!/usr/bin/env bash
# External-operator smoke test (publish gate) — automated harness.
#
# Closes the technical half of GOV-04 (external-operator smoke evidence).
# Steps 1–7 below are automated; step 8 (operator-improvisation tally)
# remains a human judgement that the operator fills into the generated
# results file.
#
# See tests/e2e/external-operator-smoke.md for the procedure and the
# rationale for each step.
#
# Modes
# -----
# - `CARGO_BUILD=1` (default in repo CI): builds clarion + installs the
#   plugin from the source tree, fetches the canonical corpus, runs the
#   walkthrough, writes a results file.
# - `CARGO_BUILD=0` + `CLARION_BIN=...` + `CLARION_PLUGIN_BIN=...`: skips
#   the source build; expects the operator to have already installed
#   clarion (via GitHub Release archive) and clarion-plugin-python (via
#   pipx) on $PATH.
#
# Outputs
# -------
# Writes a results file at $RESULTS_DIR/external-operator-smoke-results-
# YYYY-MM-DD-<host>.md. Steps 1–7 are auto-filled with PASS/FAIL/SKIP and
# a stdout excerpt; step 8 is left as a TODO for the operator to fill in.
#
# Exit codes
# ----------
# 0 — all technical steps PASS (or explicit SKIPs were declared upfront).
# 1 — any technical step FAILED.
# 78 — soft-failure exit (matches `clarion analyze`'s soft-fail convention)
#      reserved; not currently used but documented for future hardening.

set -euo pipefail

# -------- configuration --------

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
CARGO_BUILD="${CARGO_BUILD:-1}"
RESULTS_DIR="${RESULTS_DIR:-$REPO_ROOT/tests/e2e}"
VENV="${VENV:-$REPO_ROOT/plugins/python/.venv}"
CORPUS_REPO="${CORPUS_REPO:-https://github.com/psf/requests.git}"
CORPUS_REF="${CORPUS_REF:-v2.32.3}"

DATE_STAMP="$(date -u +%Y-%m-%d)"
HOST_TAG="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
RESULTS_FILE="${RESULTS_FILE:-$RESULTS_DIR/external-operator-smoke-results-${DATE_STAMP}-${HOST_TAG}.md}"

# Allow OPENROUTER_API_KEY to be unset; step 4.3(c) skips cleanly.
OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-}"

log()  { printf '[smoke] %s\n' "$*" >&2; }
fail() { printf '[smoke] FAIL: %s\n' "$*" >&2; exit 1; }

# Per-step PASS/FAIL/SKIP tracking. step_status[i]="PASS" / "FAIL" / "SKIP".
declare -A step_status
declare -A step_detail

record() {
    local step="$1" status="$2" detail="$3"
    step_status[$step]="$status"
    step_detail[$step]="$detail"
    log "step $step: $status — $detail"
}

# -------- preflight --------

cd "$REPO_ROOT"

if [ "$CARGO_BUILD" = "1" ]; then
    log "preflight: building clarion (release) ..."
    cargo build --workspace --release
    CLARION_BIN="${CLARION_BIN:-$REPO_ROOT/target/release/clarion}"
    if [ ! -d "$VENV" ]; then
        log "preflight: creating plugin venv at $VENV ..."
        python3 -m venv "$VENV"
    fi
    log "preflight: installing clarion-plugin-python (editable) ..."
    "$VENV/bin/pip" install --quiet -e "$REPO_ROOT/plugins/python[dev]"
    CLARION_PLUGIN_BIN="${CLARION_PLUGIN_BIN:-$VENV/bin/clarion-plugin-python}"
    export PATH="$REPO_ROOT/target/release:$VENV/bin:$PATH"
else
    CLARION_BIN="${CLARION_BIN:-$(command -v clarion || true)}"
    CLARION_PLUGIN_BIN="${CLARION_PLUGIN_BIN:-$(command -v clarion-plugin-python || true)}"
fi

[ -x "$CLARION_BIN" ] || fail "clarion binary missing at $CLARION_BIN (set CLARION_BIN= or CARGO_BUILD=1)"
[ -x "$CLARION_PLUGIN_BIN" ] || fail "clarion-plugin-python missing at $CLARION_PLUGIN_BIN (set CLARION_PLUGIN_BIN= or CARGO_BUILD=1)"

CLARION_VERSION="$("$CLARION_BIN" --version 2>&1 | head -1)"
PLUGIN_VERSION="$("$CLARION_PLUGIN_BIN" --version 2>&1 | head -1 || true)"
log "preflight: $CLARION_VERSION / $PLUGIN_VERSION"

# Scratch directory for the corpus.
WORK_DIR="$(mktemp -d -t clarion-smoke-XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT
log "scratch: $WORK_DIR"

# -------- step 1: clarion is on $PATH and --version works --------

if [ -n "$CLARION_VERSION" ]; then
    record "1" "PASS" "clarion --version: $CLARION_VERSION"
else
    record "1" "FAIL" "clarion --version produced no output"
fi

# -------- step 2: clarion-plugin-python on $PATH --------

if [ -x "$CLARION_PLUGIN_BIN" ]; then
    record "2" "PASS" "plugin binary at $CLARION_PLUGIN_BIN; $PLUGIN_VERSION"
else
    record "2" "FAIL" "plugin binary not found"
fi

# -------- step 3: clarion install against a small Python corpus --------

log "fetching corpus $CORPUS_REPO @ $CORPUS_REF ..."
cd "$WORK_DIR"
if ! git clone --quiet --depth 1 --branch "$CORPUS_REF" "$CORPUS_REPO" corpus; then
    record "3" "FAIL" "git clone of $CORPUS_REPO @ $CORPUS_REF failed"
    fail "cannot proceed without corpus"
fi
cd corpus

if "$CLARION_BIN" install >/dev/null 2>"$WORK_DIR/install.err"; then
    if [ -f .clarion/clarion.db ]; then
        record "3" "PASS" ".clarion/clarion.db created against psf/requests@$CORPUS_REF"
    else
        record "3" "FAIL" "install reported success but .clarion/clarion.db missing"
    fi
else
    record "3" "FAIL" "clarion install exited non-zero: $(tr '\n' ' ' < "$WORK_DIR/install.err")"
fi

# -------- step 4.1: clarion analyze (initial) --------

log "running clarion analyze ..."
if "$CLARION_BIN" analyze . >"$WORK_DIR/analyze1.out" 2>"$WORK_DIR/analyze1.err"; then
    ENTITY_COUNT_1="$(sqlite3 .clarion/clarion.db 'SELECT COUNT(*) FROM entities WHERE kind != "subsystem"' || echo 0)"
    EDGE_COUNT_1="$(sqlite3 .clarion/clarion.db 'SELECT COUNT(*) FROM edges' || echo 0)"
    if [ "$ENTITY_COUNT_1" -gt 0 ]; then
        record "4.1" "PASS" "analyze ok; entities=$ENTITY_COUNT_1 edges=$EDGE_COUNT_1"
    else
        record "4.1" "FAIL" "analyze ok but entity count is 0"
    fi
else
    record "4.1" "FAIL" "clarion analyze exited non-zero: $(tail -3 "$WORK_DIR/analyze1.err" | tr '\n' ' ')"
fi

# -------- step 4.2 + 4.3: clarion serve + MCP queries --------

log "driving MCP stdio ..."
MCP_REPORT="$WORK_DIR/mcp.json"
PROJECT_DIR_FOR_PY="$WORK_DIR/corpus"
CLARION_BIN_FOR_PY="$CLARION_BIN"

set +e
python3 - "$CLARION_BIN_FOR_PY" "$PROJECT_DIR_FOR_PY" "$MCP_REPORT" "${OPENROUTER_API_KEY:-NONE}" <<'PY'
import json, sys, subprocess, sqlite3
from pathlib import Path

clarion_bin, project_dir, report_path, openrouter_key = sys.argv[1:5]
project_dir = Path(project_dir)
report = {"step_4_2": None, "step_4_3a": None, "step_4_3b": None, "step_4_3c": None,
          "tools_listed": None, "find_entity_hits": None, "callers_of_hits": None,
          "summary_envelope_kind": None, "summary_skipped_reason": None}

def write_frame(proc, message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    proc.stdin.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n")
    proc.stdin.write(body)
    proc.stdin.flush()

def read_frame(proc):
    headers = {}
    while True:
        line = proc.stdout.readline()
        if not line:
            stderr = proc.stderr.read().decode("utf-8", "replace")
            raise AssertionError(f"server closed stdout; stderr={stderr}")
        if line == b"\r\n":
            break
        k, v = line.decode("ascii").strip().split(":", 1)
        headers[k.lower()] = v.strip()
    return json.loads(proc.stdout.read(int(headers["content-length"])))

def tool_call(proc, rid, name, args):
    write_frame(proc, {"jsonrpc": "2.0", "id": rid, "method": "tools/call",
                       "params": {"name": name, "arguments": args}})
    return read_frame(proc)

# Pick a real entity from the analyzed corpus to test against.
conn = sqlite3.connect(project_dir / ".clarion" / "clarion.db")
ent = conn.execute("""
    SELECT id, kind, name FROM entities
    WHERE kind = 'function' AND name = 'get'
    AND id LIKE 'python:function:%'
    LIMIT 1
""").fetchone()
if not ent:
    ent = conn.execute("""
        SELECT id, kind, name FROM entities
        WHERE kind = 'function' LIMIT 1
    """).fetchone()
session_ent = conn.execute("""
    SELECT id FROM entities WHERE kind = 'class' AND name = 'Session' LIMIT 1
""").fetchone()
conn.close()

if not ent:
    report["step_4_2"] = "FAIL: no function entities to query"
    Path(report_path).write_text(json.dumps(report))
    sys.exit(2)

proc = subprocess.Popen([clarion_bin, "serve"],
                       stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                       stderr=subprocess.PIPE, cwd=str(project_dir))

try:
    # MCP initialize
    write_frame(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize",
                       "params": {"protocolVersion": "2024-11-05",
                                  "capabilities": {},
                                  "clientInfo": {"name": "smoke", "version": "1"}}})
    read_frame(proc)
    write_frame(proc, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    # tools/list
    write_frame(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    listed = read_frame(proc)
    tools = [t["name"] for t in listed.get("result", {}).get("tools", [])]
    required = {"entity_at", "find_entity", "callers_of", "execution_paths_from",
                "summary", "issues_for", "neighborhood", "subsystem_members"}
    if required.issubset(set(tools)):
        report["step_4_2"] = f"PASS: tools/list returned {len(tools)} tools including all 8 required"
    else:
        missing = required - set(tools)
        report["step_4_2"] = f"FAIL: tools/list missing {sorted(missing)}"
    report["tools_listed"] = tools

    def unwrap(envelope):
        """Clarion MCP envelope wraps the tool payload under `result`."""
        return envelope.get("result") if isinstance(envelope, dict) else None

    # 4.3(a) find_entity
    pattern = "Session" if session_ent else ent[2]
    fe = tool_call(proc, 3, "find_entity", {"pattern": pattern, "limit": 5})
    fe_body = json.loads(fe["result"]["content"][0]["text"])
    inner = unwrap(fe_body) or {}
    matches = inner.get("entities") or inner.get("matches") or inner.get("results") or []
    report["find_entity_hits"] = len(matches)
    report["step_4_3a"] = (f"PASS: find_entity('{pattern}') returned {len(matches)} matches"
                          if matches else f"FAIL: find_entity('{pattern}') returned empty (envelope: {fe_body})")

    # 4.3(b) callers_of — find a function with at least one caller.
    conn2 = sqlite3.connect(project_dir / ".clarion" / "clarion.db")
    row = conn2.execute("""
        SELECT e.id, COUNT(*) AS c FROM entities e
        JOIN edges ed ON ed.to_id = e.id
        WHERE e.kind = 'function' AND ed.kind = 'calls'
        GROUP BY e.id ORDER BY c DESC LIMIT 1
    """).fetchone()
    conn2.close()
    if not row:
        report["step_4_3b"] = "FAIL: no `calls` edges in the analyzed graph"
    else:
        callers_target = row[0]
        co = tool_call(proc, 4, "callers_of", {"id": callers_target, "limit": 5})
        co_body = json.loads(co["result"]["content"][0]["text"])
        co_inner = unwrap(co_body) or {}
        callers = co_inner.get("callers") or []
        report["callers_of_hits"] = len(callers)
        if callers:
            report["step_4_3b"] = f"PASS: callers_of('{callers_target}') returned {len(callers)} callers"
        else:
            report["step_4_3b"] = f"FAIL: callers_of('{callers_target}') returned empty (envelope: {co_body})"

    # 4.3(c) summary — only runs if OPENROUTER_API_KEY is set
    if openrouter_key and openrouter_key != "NONE":
        sm = tool_call(proc, 6, "summary", {"id": ent[0]})
        sm_body = json.loads(sm["result"]["content"][0]["text"])
        sm_inner = unwrap(sm_body) or {}
        if sm_body.get("ok") and (sm_inner.get("summary") or sm_inner.get("available") is True):
            report["summary_envelope_kind"] = "live"
            report["step_4_3c"] = f"PASS: live summary for {ent[0]} returned non-empty"
        else:
            report["summary_envelope_kind"] = "live-empty"
            report["step_4_3c"] = f"FAIL: summary call ran but envelope had no usable payload: {sm_body}"
    else:
        report["step_4_3c"] = "SKIP: OPENROUTER_API_KEY not set"
        report["summary_skipped_reason"] = "no api key"

finally:
    try:
        proc.stdin.close()
    except Exception:
        pass
    proc.wait(timeout=10)

Path(report_path).write_text(json.dumps(report, indent=2))
PY
MCP_RC=$?
set -e

if [ "$MCP_RC" -ne 0 ]; then
    record "4.2" "FAIL" "MCP harness exited with code $MCP_RC"
    record "4.3a" "FAIL" "MCP harness failed before query a"
    record "4.3b" "FAIL" "MCP harness failed before query b"
    record "4.3c" "FAIL" "MCP harness failed before query c"
else
    # shellcheck disable=SC2002
    REPORT_JSON="$(cat "$MCP_REPORT")"
    parse_field() { python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(sys.argv[2]) or '')" "$REPORT_JSON" "$1"; }
    record "4.2" "$(parse_field step_4_2 | cut -d: -f1)" "$(parse_field step_4_2)"
    record "4.3a" "$(parse_field step_4_3a | cut -d: -f1)" "$(parse_field step_4_3a)"
    record "4.3b" "$(parse_field step_4_3b | cut -d: -f1)" "$(parse_field step_4_3b)"
    case "$(parse_field step_4_3c)" in
        PASS*) record "4.3c" "PASS" "$(parse_field step_4_3c)" ;;
        FAIL*) record "4.3c" "FAIL" "$(parse_field step_4_3c)" ;;
        SKIP*) record "4.3c" "SKIP" "$(parse_field step_4_3c)" ;;
        *)     record "4.3c" "FAIL" "unknown summary state: $(parse_field step_4_3c)" ;;
    esac
fi

# -------- step 5: re-run analyze for idempotency --------

log "running clarion analyze (re-run for idempotency) ..."
if "$CLARION_BIN" analyze . >"$WORK_DIR/analyze2.out" 2>"$WORK_DIR/analyze2.err"; then
    ENTITY_COUNT_2="$(sqlite3 .clarion/clarion.db 'SELECT COUNT(*) FROM entities WHERE kind != "subsystem"')"
    EDGE_COUNT_2="$(sqlite3 .clarion/clarion.db 'SELECT COUNT(*) FROM edges')"
    if [ "$ENTITY_COUNT_2" = "$ENTITY_COUNT_1" ] && [ "$EDGE_COUNT_2" = "$EDGE_COUNT_1" ]; then
        record "5" "PASS" "idempotent: entities=$ENTITY_COUNT_2 edges=$EDGE_COUNT_2 unchanged"
    else
        record "5" "FAIL" "non-idempotent: entities $ENTITY_COUNT_1 -> $ENTITY_COUNT_2, edges $EDGE_COUNT_1 -> $EDGE_COUNT_2"
    fi
else
    record "5" "FAIL" "re-run analyze exited non-zero"
fi

# -------- step 6: plant secret + re-run analyze; verify briefing_blocked --------

log "planting .env with fake AWS credential; re-running analyze ..."
cat > .env <<'ENV'
AWS_ACCESS_KEY_ID=AKIA0123456789ABCDEF
AWS_SECRET_ACCESS_KEY=examplefakefakefakefakefakefakefakefake1234
ENV

ANALYZE_3_EXIT=0
"$CLARION_BIN" analyze . >"$WORK_DIR/analyze3.out" 2>"$WORK_DIR/analyze3.err" || ANALYZE_3_EXIT=$?

# Expected: soft-failure (exit 78) or success with briefing_blocked recorded.
BLOCKED_COUNT="$(sqlite3 .clarion/clarion.db "SELECT COUNT(*) FROM entities WHERE json_extract(properties, '\$.briefing_blocked') IS NOT NULL" 2>/dev/null || echo 0)"
FINDING_COUNT="$(sqlite3 .clarion/clarion.db "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-SEC-SECRET-DETECTED'" 2>/dev/null || echo 0)"

if [ "$BLOCKED_COUNT" -gt 0 ] && [ "$FINDING_COUNT" -gt 0 ]; then
    record "6" "PASS" "post-plant: $BLOCKED_COUNT blocked entities, $FINDING_COUNT secret findings (analyze exit $ANALYZE_3_EXIT)"
elif [ "$BLOCKED_COUNT" -gt 0 ]; then
    record "6" "PASS" "post-plant: $BLOCKED_COUNT blocked entities (no CLA-SEC-SECRET-DETECTED finding rows; finding code may have changed)"
elif [ "$FINDING_COUNT" -gt 0 ]; then
    record "6" "FAIL" "secret finding emitted but no entity marked briefing_blocked"
else
    record "6" "FAIL" "no briefing_blocked entities and no CLA-SEC-SECRET-DETECTED finding after planting"
fi

# -------- step 7: serve against post-block DB; summary on blocked entity returns blocked envelope --------

if [ "$BLOCKED_COUNT" -gt 0 ]; then
    log "verifying blocked-entity summary refusal ..."
    BLOCKED_ID="$(sqlite3 .clarion/clarion.db "SELECT id FROM entities WHERE json_extract(properties, '\$.briefing_blocked') IS NOT NULL LIMIT 1")"
    if [ -n "$BLOCKED_ID" ]; then
        set +e
        python3 - "$CLARION_BIN" "$WORK_DIR/corpus" "$BLOCKED_ID" "$WORK_DIR/step7.json" <<'PY'
import json, sys, subprocess
from pathlib import Path

clarion_bin, project_dir, blocked_id, out_path = sys.argv[1:5]

def write_frame(proc, message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    proc.stdin.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n")
    proc.stdin.write(body)
    proc.stdin.flush()

def read_frame(proc):
    headers = {}
    while True:
        line = proc.stdout.readline()
        if not line: raise AssertionError("eof")
        if line == b"\r\n": break
        k, v = line.decode("ascii").strip().split(":", 1)
        headers[k.lower()] = v.strip()
    return json.loads(proc.stdout.read(int(headers["content-length"])))

proc = subprocess.Popen([clarion_bin, "serve"],
                       stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                       stderr=subprocess.PIPE, cwd=project_dir)
try:
    write_frame(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize",
                       "params": {"protocolVersion": "2024-11-05",
                                  "capabilities": {},
                                  "clientInfo": {"name": "smoke", "version": "1"}}})
    read_frame(proc)
    write_frame(proc, {"jsonrpc": "2.0", "method": "notifications/initialized"})
    write_frame(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                       "params": {"name": "summary", "arguments": {"id": blocked_id}}})
    resp = read_frame(proc)
    body = json.loads(resp["result"]["content"][0]["text"])
    inner = body.get("result") if isinstance(body, dict) else None
    blocked = bool(inner and inner.get("briefing_blocked"))
    Path(out_path).write_text(json.dumps({"blocked": blocked, "envelope": body}, indent=2))
finally:
    try: proc.stdin.close()
    except Exception: pass
    proc.wait(timeout=10)
PY
        STEP7_RC=$?
        set -e
        if [ "$STEP7_RC" -eq 0 ] && python3 -c "import json,sys;sys.exit(0 if json.loads(open(sys.argv[1]).read()).get('blocked') else 1)" "$WORK_DIR/step7.json"; then
            record "7" "PASS" "summary on blocked entity '$BLOCKED_ID' returned briefing-blocked envelope (no LLM call)"
        else
            record "7" "FAIL" "summary on blocked entity did not return briefing-blocked envelope"
        fi
    else
        record "7" "FAIL" "blocked count > 0 but could not select an id"
    fi
else
    record "7" "SKIP" "skipped because step 6 found no blocked entities"
fi

# -------- step 8: operator-improvisation tally (human-judged) --------

record "8" "TODO" "operator must fill in: count of source-reads outside README + getting-started"

# -------- write results file --------

log "writing results to $RESULTS_FILE ..."
mkdir -p "$(dirname "$RESULTS_FILE")"
{
    echo "# External-operator smoke result"
    echo
    echo "**Date (UTC)**: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "**Host**: $(uname -srm)"
    echo "**Clarion**: $CLARION_VERSION"
    echo "**Plugin**: $PLUGIN_VERSION"
    echo "**Corpus**: $CORPUS_REPO @ $CORPUS_REF"
    echo "**Mode**: $([ "$CARGO_BUILD" = "1" ] && echo "in-repo build (CARGO_BUILD=1)" || echo "external binary")"
    echo
    echo "## Step results"
    echo
    echo "| # | Step | Status | Detail |"
    echo "|---|------|--------|--------|"
    for step in 1 2 3 4.1 4.2 4.3a 4.3b 4.3c 5 6 7 8; do
        echo "| $step | $(grep -A1 "^| $step " tests/e2e/external-operator-smoke.md 2>/dev/null | head -1 | cut -d'|' -f3 | sed 's/^ *//;s/ *$//' || echo "see md") | ${step_status[$step]:-MISSING} | ${step_detail[$step]:-} |"
    done
    echo
    echo "## Step 8 (operator-fill)"
    echo
    echo "Improvisation tally: TODO — record the count of moments where you (the operator)"
    echo "had to read source code outside \`README.md\` and \`docs/operator/getting-started.md\`"
    echo "to know what to do next."
    echo
    echo "Target: 0. Any non-zero count is a B.1/B.2 docs bug, not a runtime defect."
    echo
    echo "Improvisation events observed:"
    echo
    echo "- [ ] (fill in below; one bullet per event)"
    echo
    echo "## Verdict"
    echo
    echo "**Technical** (steps 1–7): set automatically by this harness."
    echo
    echo "**Operator** (step 8): fill in after walking through the procedure."
    echo
    echo "**Gate passed**: only if all non-skip steps PASS *and* the operator improvisation"
    echo "tally is 0."
    echo
    echo "## Attestation"
    echo
    echo "_Sign off (operator name, date) once step 8 is filled in:_"
    echo
    echo "Signed: TODO"
    echo "Date: TODO"
} > "$RESULTS_FILE"

# -------- final exit code --------

FAIL_COUNT=0
for step in 1 2 3 4.1 4.2 4.3a 4.3b 4.3c 5 6 7; do
    if [ "${step_status[$step]:-}" = "FAIL" ]; then
        FAIL_COUNT=$((FAIL_COUNT+1))
    fi
done

if [ "$FAIL_COUNT" -gt 0 ]; then
    log "FAIL: $FAIL_COUNT technical step(s) did not pass. Results: $RESULTS_FILE"
    exit 1
fi

log "PASS: all technical steps passed (1–7). Results: $RESULTS_FILE"
log "     Operator must still fill in step 8 (improvisation tally) before declaring the gate green."
