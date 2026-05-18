#!/usr/bin/env bash
# Phase 3 subsystem clustering end-to-end test.
#
# Builds a real Clarion database through `clarion analyze`, verifies persisted
# subsystem entities / membership edges / clustering stats, checks deterministic
# subsystem signatures across two clean project copies, and exercises the MCP
# `subsystem_members` tool over stdio.

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
VENV="${VENV:-$REPO_ROOT/plugins/python/.venv}"
CARGO_BUILD="${CARGO_BUILD:-1}"
MIN_CLUSTER_SIZE=2

log() { printf '[phase3-subsystems] %s\n' "$*" >&2; }
fail() { printf '[phase3-subsystems] FAIL: %s\n' "$*" >&2; exit 1; }

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

ROOT="$(mktemp -d -t clarion-phase3-XXXXXX)"
trap 'rm -rf "$ROOT"' EXIT
PROJECT_A="$ROOT/project-a"
PROJECT_B="$ROOT/project-b"

write_fixture() {
    local dest="$1"
    mkdir -p "$dest/pkg/auth" "$dest/pkg/billing"
    : > "$dest/pkg/__init__.py"
    : > "$dest/pkg/auth/__init__.py"
    : > "$dest/pkg/billing/__init__.py"
    cat > "$dest/pkg/auth/login.py" <<'PY'
from pkg.auth import policy, token

def login(user: str) -> str:
    return token.issue(policy.normalize(user))
PY
    cat > "$dest/pkg/auth/token.py" <<'PY'
from pkg.auth import policy

def issue(user: str) -> str:
    return f"token:{policy.normalize(user)}"
PY
    cat > "$dest/pkg/auth/policy.py" <<'PY'
from pkg.auth import token

def normalize(user: str) -> str:
    return user.strip().lower()

def preview(user: str) -> str:
    return token.issue(user)
PY
    cat > "$dest/pkg/billing/invoice.py" <<'PY'
from pkg.billing import ledger, tax

def create(amount: int) -> int:
    return ledger.record(tax.apply(amount))
PY
    cat > "$dest/pkg/billing/ledger.py" <<'PY'
from pkg.billing import tax

def record(amount: int) -> int:
    return tax.apply(amount)
PY
    cat > "$dest/pkg/billing/tax.py" <<'PY'
from pkg.billing import ledger

def apply(amount: int) -> int:
    return amount + 1

def preview(amount: int) -> int:
    return ledger.record(amount)
PY
    cat > "$dest/pkg/app.py" <<'PY'
from pkg.auth import login
from pkg.billing import invoice

def run() -> tuple[str, int]:
    return login.login(" Ada "), invoice.create(10)
PY
    cat > "$dest/clarion.yaml" <<YAML
analysis:
  clustering:
    min_cluster_size: $MIN_CLUSTER_SIZE
YAML
}

run_analyze() {
    local project="$1"
    write_fixture "$project"
    (
        cd "$project"
        export PATH="$REPO_ROOT/target/release:$VENV/bin:$PATH"
        log "running: clarion install in $project"
        clarion install
        log "running: clarion analyze . in $project"
        clarion analyze .
    )
}

subsystem_signature() {
    local db="$1"
    sqlite3 "$db" \
        "SELECT id || '|' || json_extract(properties, '\$.member_module_ids') || '|' || printf('%.9f', json_extract(properties, '\$.modularity_score')) \
         FROM entities \
         WHERE kind = 'subsystem' \
         ORDER BY id;"
}

run_analyze "$PROJECT_A"
run_analyze "$PROJECT_B"

DB_A="$PROJECT_A/.clarion/clarion.db"
DB_B="$PROJECT_B/.clarion/clarion.db"

log "verifying subsystem rows ..."
SUBSYSTEM_COUNT=$(sqlite3 "$DB_A" "SELECT COUNT(*) FROM entities WHERE kind = 'subsystem';")
if [ "$SUBSYSTEM_COUNT" -lt 2 ]; then
    sqlite3 "$DB_A" "SELECT id, kind, properties FROM entities ORDER BY kind, id;" >&2 || true
    fail "expected at least two subsystem rows; got $SUBSYSTEM_COUNT"
fi

UNDER_MIN=$(sqlite3 "$DB_A" \
    "SELECT COUNT(*) FROM ( \
         SELECT to_id, COUNT(*) AS members \
         FROM edges \
         WHERE kind = 'in_subsystem' \
         GROUP BY to_id \
         HAVING members < $MIN_CLUSTER_SIZE \
     );")
if [ "$UNDER_MIN" != "0" ]; then
    sqlite3 "$DB_A" \
        "SELECT to_id, COUNT(*) FROM edges WHERE kind = 'in_subsystem' GROUP BY to_id;" >&2 || true
    fail "every subsystem should have at least min_cluster_size=$MIN_CLUSTER_SIZE members"
fi

CLUSTERING_STATUS=$(sqlite3 "$DB_A" \
    "SELECT json_extract(stats, '\$.clustering.status') FROM runs ORDER BY started_at DESC LIMIT 1;")
if [ "$CLUSTERING_STATUS" != "completed" ]; then
    sqlite3 "$DB_A" "SELECT id, status, stats FROM runs;" >&2 || true
    fail "expected runs.stats.clustering.status=completed; got $CLUSTERING_STATUS"
fi

log "verifying deterministic subsystem signature across clean runs ..."
SIG_A="$(subsystem_signature "$DB_A")"
SIG_B="$(subsystem_signature "$DB_B")"
if [ "$SIG_A" != "$SIG_B" ]; then
    fail "$(printf 'subsystem signatures differ:\nA:\n%s\nB:\n%s' "$SIG_A" "$SIG_B")"
fi

log "driving MCP subsystem_members ..."
python3 - "$CLARION_BIN" "$PROJECT_A" <<'PY'
import json
import sqlite3
import subprocess
import sys
from pathlib import Path

clarion_bin = Path(sys.argv[1])
project_dir = Path(sys.argv[2])
conn = sqlite3.connect(project_dir / ".clarion" / "clarion.db")
subsystem_id = conn.execute(
    "SELECT id FROM entities WHERE kind = 'subsystem' ORDER BY id LIMIT 1"
).fetchone()[0]
conn.close()


def write_frame(proc: subprocess.Popen[bytes], message: dict[str, object]) -> None:
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    assert proc.stdin is not None
    proc.stdin.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n")
    proc.stdin.write(body)
    proc.stdin.flush()


def read_frame(proc: subprocess.Popen[bytes]) -> dict[str, object]:
    assert proc.stdout is not None
    headers: dict[str, str] = {}
    while True:
        line = proc.stdout.readline()
        if line == b"":
            stderr = proc.stderr.read().decode("utf-8", "replace") if proc.stderr else ""
            raise AssertionError(f"server closed stdout; stderr={stderr}")
        if line == b"\r\n":
            break
        name, value = line.decode("ascii").strip().split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(proc.stdout.read(int(headers["content-length"])))


proc = subprocess.Popen(
    [str(clarion_bin), "serve", "--path", str(project_dir)],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)
try:
    write_frame(
        proc,
        {
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "phase3-e2e", "version": "0.0.0"},
            },
        },
    )
    read_frame(proc)
    write_frame(
        proc,
        {
            "jsonrpc": "2.0",
            "id": "members",
            "method": "tools/call",
            "params": {
                "name": "subsystem_members",
                "arguments": {"id": subsystem_id},
            },
        },
    )
    response = read_frame(proc)
    text = response["result"]["content"][0]["text"]
    envelope = json.loads(text)
    assert envelope["ok"] is True, envelope
    assert envelope["result"]["subsystem"]["id"] == subsystem_id, envelope
    assert len(envelope["result"]["members"]) >= 2, envelope
finally:
    assert proc.stdin is not None
    proc.stdin.close()
    proc.wait(timeout=5)
PY

log "phase3 subsystem e2e passed"
