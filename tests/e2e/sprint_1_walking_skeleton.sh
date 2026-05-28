#!/usr/bin/env bash
# Sprint 1 walking-skeleton end-to-end demo (WP3 Task 9 / signoffs A.4).
#
# Runs the README §3 demo script end-to-end and verifies:
#   - `clarion install` creates `.clarion/clarion.db`
#   - `clarion analyze .` spawns the Python plugin and persists at least one entity
#   - `sqlite3 .clarion/clarion.db` returns Python module/function entities
#   - Python function rows include source path, source line range, and content hash
#   - resolved and ambiguous calls edges are persisted end-to-end
#   - resolved references edges are persisted end-to-end
#
# Dependencies: cargo, Python 3.11+, sqlite3 CLI.
#
# Env overrides:
#   REPO_ROOT   — auto-detected via `git rev-parse`; override to test an external checkout.
#   VENV        — defaults to $REPO_ROOT/plugins/python/.venv; override to reuse an existing venv.
#   CARGO_BUILD — set to "0" to skip `cargo build` (assumes target/release/clarion already present).

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
VENV="${VENV:-$REPO_ROOT/plugins/python/.venv}"
CARGO_BUILD="${CARGO_BUILD:-1}"

log() { printf '[walking-skeleton] %s\n' "$*" >&2; }
fail() { printf '[walking-skeleton] FAIL: %s\n' "$*" >&2; exit 1; }

cd "$REPO_ROOT"

# ── 1. Build clarion binary ──────────────────────────────────────────────────
if [ "$CARGO_BUILD" = "1" ]; then
    log "building clarion (release) ..."
    cargo build --workspace --release
fi
CLARION_BIN="$REPO_ROOT/target/release/clarion"
[ -x "$CLARION_BIN" ] || fail "clarion binary missing at $CLARION_BIN"

# ── 2. Install Python plugin (editable) ──────────────────────────────────────
if [ ! -d "$VENV" ]; then
    log "creating venv at $VENV ..."
    python3 -m venv "$VENV"
fi
log "installing clarion-plugin-python (editable) ..."
"$VENV/bin/pip" install --quiet -e "$REPO_ROOT/plugins/python[dev]"
PLUGIN_BIN="$VENV/bin/clarion-plugin-python"
[ -x "$PLUGIN_BIN" ] || fail "clarion-plugin-python missing at $PLUGIN_BIN"
PLUGIN_MANIFEST="$VENV/share/clarion/plugins/python/plugin.toml"
[ -f "$PLUGIN_MANIFEST" ] || fail "plugin.toml missing at $PLUGIN_MANIFEST (WP2 L9 install-prefix fallback)"

# ── 3. Scratch project ───────────────────────────────────────────────────────
DEMO_DIR="$(mktemp -d -t clarion-demo-XXXXXX)"
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

# ── 4. PATH wiring — clarion + plugin binary ────────────────────────────────
export PATH="$REPO_ROOT/target/release:$VENV/bin:$PATH"

# ── 5. clarion install ───────────────────────────────────────────────────────
log "running: clarion install"
clarion install
[ -f "$DEMO_DIR/.clarion/clarion.db" ] || fail ".clarion/clarion.db not created by clarion install"

# ── 6. clarion analyze ───────────────────────────────────────────────────────
log "running: clarion analyze ."
clarion analyze .

# ── 6b. Verify database integrity (STO-04) ───────────────────────────────────
log "verifying database integrity via PRAGMA integrity_check ..."
INTEGRITY=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" "PRAGMA integrity_check;")
if [ "$INTEGRITY" != "ok" ]; then
    log "integrity_check output:"
    printf '%s\n' "$INTEGRITY" >&2
    fail "expected PRAGMA integrity_check to report exactly 'ok'; got $INTEGRITY"
fi

# ── 7. Verify entity via sqlite3 ─────────────────────────────────────────────
log "verifying persisted entity via sqlite3 ..."
RESULT=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select id, kind from entities order by id;")
# B.2 (Sprint 2): every analyzed file emits a module entity in addition to
# its function/class entities. v1.0 also mints a core file entity for file
# identity and federation reads. B.4* adds direct and dict-dispatch call sites.
# B.5* adds a local annotation reference and a module-level name reference.
EXPECTED="core:file:demo.py|file
python:class:demo.Marker|class
python:function:demo.annotated|function
python:function:demo.hello|function
python:function:demo.via_dispatch|function
python:function:demo.world|function
python:function:demo.z_fallback|function
python:module:demo|module"

if [ "$RESULT" != "$EXPECTED" ]; then
    log "DB contents:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select * from entities;" >&2 || true
    message=$(printf 'expected exactly:\n%s\ngot:\n%s' "$EXPECTED" "$RESULT")
    fail "$message"
fi

# ── 8. Verify source metadata for MCP entity_at/summary cache (B.6a) ─────────
log "verifying persisted Python function source metadata ..."
SOURCE_METADATA=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select source_file_path, source_line_start, source_line_end, length(content_hash) from entities where id = 'python:function:demo.hello';")
SOURCE_METADATA_EXPECTED="$DEMO_DIR/demo.py|10|11|64"
if [ "$SOURCE_METADATA" != "$SOURCE_METADATA_EXPECTED" ]; then
    log "DB entity source metadata:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
        "select id, source_file_path, source_line_start, source_line_end, content_hash from entities order by id;" >&2 || true
    fail "expected Python function source metadata:\n$SOURCE_METADATA_EXPECTED\ngot:\n$SOURCE_METADATA"
fi

# ── 9. Verify contains edge via sqlite3 (B.3) ────────────────────────────────
log "verifying persisted contains edge via sqlite3 ..."
EDGE_RESULT=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select kind, from_id, to_id from edges where kind = 'contains' order by from_id, to_id;")
EDGE_EXPECTED="contains|python:module:demo|python:class:demo.Marker
contains|python:module:demo|python:function:demo.annotated
contains|python:module:demo|python:function:demo.hello
contains|python:module:demo|python:function:demo.via_dispatch
contains|python:module:demo|python:function:demo.world
contains|python:module:demo|python:function:demo.z_fallback"

if [ "$EDGE_RESULT" != "$EDGE_EXPECTED" ]; then
    log "DB edge contents:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select * from edges;" >&2 || true
    fail "expected edge row:\n$EDGE_EXPECTED\ngot:\n$EDGE_RESULT"
fi

# ── 10. Verify run stats include B.3 + B.4* + B.5* edges ────────────────────
log "verifying run stats include edges_inserted >= 10 ..."
EDGES_INSERTED=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select json_extract(stats, '\$.edges_inserted') from runs where status = 'completed';")
if [ "$EDGES_INSERTED" -lt 10 ]; then
    log "runs row:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select id, status, stats from runs;" >&2 || true
    fail "expected runs.stats.edges_inserted >= 10; got $EDGES_INSERTED"
fi

# ── 11. Verify dropped_edges_total == 0 (B.3 §6 / §9 exit criterion 6) ──────
log "verifying run stats include dropped_edges_total == 0 ..."
DROPPED=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select json_extract(stats, '\$.dropped_edges_total') from runs where status = 'completed';")
if [ "$DROPPED" != "0" ]; then
    log "runs row:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select id, status, stats from runs;" >&2 || true
    fail "expected runs.stats.dropped_edges_total == 0; got $DROPPED"
fi

# ── 12. Verify resolved + ambiguous calls edges (B.4*) ──────────────────────
log "verifying persisted resolved calls edge ..."
RESOLVED_CALLS=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select count(*) from edges where kind = 'calls' and confidence = 'resolved';")
if [ "$RESOLVED_CALLS" -lt 1 ]; then
    log "DB edge contents:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select kind, from_id, to_id, confidence, properties from edges order by kind, from_id, to_id;" >&2 || true
    fail "expected at least one resolved calls edge; got $RESOLVED_CALLS"
fi

log "verifying persisted ambiguous calls edge with properties.candidates ..."
AMBIGUOUS_WITH_CANDIDATES=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select count(*) from edges where kind = 'calls' and confidence = 'ambiguous' and json_type(properties, '\$.candidates') = 'array';")
if [ "$AMBIGUOUS_WITH_CANDIDATES" -lt 1 ]; then
    log "DB edge contents:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select kind, from_id, to_id, confidence, properties from edges order by kind, from_id, to_id;" >&2 || true
    fail "expected at least one ambiguous calls edge with properties.candidates; got $AMBIGUOUS_WITH_CANDIDATES"
fi

log "verifying run stats include ambiguous_edges_total >= 1 ..."
AMBIGUOUS_EDGES=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select json_extract(stats, '\$.ambiguous_edges_total') from runs where status = 'completed';")
if [ "$AMBIGUOUS_EDGES" -lt 1 ]; then
    log "runs row:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select id, status, stats from runs;" >&2 || true
    fail "expected runs.stats.ambiguous_edges_total >= 1; got $AMBIGUOUS_EDGES"
fi

log "verifying run stats include unresolved_call_sites_total == 0 ..."
UNRESOLVED_CALL_SITES=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select json_extract(stats, '\$.unresolved_call_sites_total') from runs where status = 'completed';")
if [ "$UNRESOLVED_CALL_SITES" != "0" ]; then
    log "runs row:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select id, status, stats from runs;" >&2 || true
    fail "expected runs.stats.unresolved_call_sites_total == 0; got $UNRESOLVED_CALL_SITES"
fi

# ── 13. Verify resolved references edges (B.5*) ─────────────────────────────
log "verifying persisted resolved references edges ..."
RESOLVED_REFERENCES=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select count(*) from edges where kind = 'references' and confidence = 'resolved';")
if [ "$RESOLVED_REFERENCES" -lt 2 ]; then
    log "DB edge contents:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select kind, from_id, to_id, confidence, properties from edges order by kind, from_id, to_id;" >&2 || true
    fail "expected at least two resolved references edges; got $RESOLVED_REFERENCES"
fi

log "verifying run stats include reference resolver counters ..."
REFERENCE_SITES=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select json_extract(stats, '\$.reference_sites_total') from runs where status = 'completed';")
REFERENCES_RESOLVED=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" \
    "select json_extract(stats, '\$.references_resolved_total') from runs where status = 'completed';")
if [ "$REFERENCE_SITES" -lt 2 ] || [ "$REFERENCES_RESOLVED" -lt 2 ]; then
    log "runs row:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select id, status, stats from runs;" >&2 || true
    fail "expected reference_sites_total and references_resolved_total >= 2; got sites=$REFERENCE_SITES resolved=$REFERENCES_RESOLVED"
fi

log "PASS: walking skeleton persisted module + function/class entities + source metadata + contains + calls + references edges"
