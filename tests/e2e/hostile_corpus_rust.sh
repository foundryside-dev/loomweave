#!/usr/bin/env bash
# Hostile-corpus acceptance gate for Rust-plugin hardening (ADR-050; plan
# 2026-06-10 Task 10; ticket clarion-7bc08e05c0).
#
# Generates a pathological Rust crate IN A TEMP DIR (nothing hostile is ever
# checked in): a 3000-deep paren bomb (pre-guard this SIGABRTed the plugin
# child), a 4000-`!` unary bomb, a ~12 MiB oversize file, a syntactically
# broken file, and one benign file. Runs the release `loomweave install` +
# `loomweave analyze .` against the staged Rust plugin and verifies:
#   - analyze exits 0 and the single runs row is `completed` (no plugin crash,
#     no stuck `running` row — the guards degraded everything)
#   - findings: LMWV-RUST-DEPTH-LIMIT x2 (paren + unary bombs),
#     LMWV-RUST-FILE-TOO-LARGE x1, LMWV-RUST-SYNTAX-ERROR x1
#   - the benign file's entities extracted normally
#   - degraded module entities carry parse_status depth_limit/file_too_large
#   - NO crash-loop / OOM / timeout / abort findings (breaker untripped)
#
# Dependencies: cargo, python3, sqlite3 CLI.
#
# Env overrides:
#   REPO_ROOT   — auto-detected via `git rev-parse`; override to test an external checkout.
#   CARGO_BUILD — set to "0" to skip `cargo build` (assumes target/release binaries present).

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
CARGO_BUILD="${CARGO_BUILD:-1}"

log() { printf '[hostile-corpus-rust] %s\n' "$*" >&2; }
fail() { printf '[hostile-corpus-rust] FAIL: %s\n' "$*" >&2; exit 1; }

cd "$REPO_ROOT"

# ── 1. Build release binaries (loomweave + the off-glob rust plugin bin) ─────
if [ "$CARGO_BUILD" = "1" ]; then
    log "building workspace (release) ..."
    cargo build --workspace --release
fi
LOOMWEAVE_BIN="$REPO_ROOT/target/release/loomweave"
RUST_PLUGIN_BIN="$REPO_ROOT/target/release/loomweave-rust-plugin"
[ -x "$LOOMWEAVE_BIN" ] || fail "loomweave binary missing at $LOOMWEAVE_BIN"
[ -x "$RUST_PLUGIN_BIN" ] || fail "loomweave-rust-plugin binary missing at $RUST_PLUGIN_BIN"

# ── 2. Stage the rust plugin under its discovery name + neighbor manifest ────
SCRATCH="$(mktemp -d -t loomweave-hostile-rust-XXXXXX)"
trap 'rm -rf "$SCRATCH"' EXIT
# Hermetic install (clarion-c5e3cc2818): `loomweave install` registers a
# Codex MCP entry in ~/.codex/config.toml unless this override points it
# at a scratch-local file. Never mutate the operator's real config.
export LOOMWEAVE_CODEX_CONFIG="$SCRATCH/codex-config.toml"
PLUGIN_DIR="$SCRATCH/plugin-bin"
mkdir -p "$PLUGIN_DIR"
# The cargo artifact is `loomweave-rust-plugin` (deliberately off the
# `loomweave-plugin-*` discovery glob); stage it under the manifest-declared
# basename with the neighbor `plugin.toml` (discovery rule 1).
cp "$RUST_PLUGIN_BIN" "$PLUGIN_DIR/loomweave-plugin-rust"
cp "$REPO_ROOT/crates/loomweave-plugin-rust/plugin.toml" "$PLUGIN_DIR/plugin.toml"

# ── 3. Generate the hostile corpus (never checked in) ────────────────────────
PROJ="$SCRATCH/proj"
mkdir -p "$PROJ/src"
cat > "$PROJ/Cargo.toml" <<'TOML'
[package]
name = "hostile_corpus"
version = "0.1.0"
edition = "2021"
TOML

log "generating hostile corpus in $PROJ/src ..."
python3 - "$PROJ/src" <<'PY'
import sys, pathlib
src = pathlib.Path(sys.argv[1])

# (a) bracket bomb: 3000-deep nested parens. syn 2.0.117 first-crash for
# parens is ~760 frames on an 8 MiB stack — pre-guard this SIGABRTed the
# plugin child (exit 134).
n = 3000
src.joinpath("deep_parens.rs").write_text(
    "pub fn f() { let _ = " + "(" * n + "1" + ")" * n + "; }\n"
)

# (b) unary bomb: 4000 consecutive `!` — bracket depth ~1, so only the
# prefix-run cap catches it (first-crash ~2386 at 8 MiB).
src.joinpath("unary_bomb.rs").write_text(
    "pub fn f() { let _ = " + "!" * 4000 + "true; }\n"
)

# (c) oversize file: ~12 MiB of VALID repeated fns (cap is 10 MiB; the size
# check fires on fs::metadata before the file is ever read).
chunk = "".join(
    f"pub fn filler_{i}() -> u64 {{\n    let x = {i}u64;\n    x.wrapping_mul(31).wrapping_add(7)\n}}\n"
    for i in range(140_000)
)
assert len(chunk) > 12 * 1024 * 1024, f"big.rs too small: {len(chunk)} bytes"
src.joinpath("big.rs").write_text(chunk)

# (d) syntactically invalid file (degraded-parse fallback, pre-existing).
src.joinpath("broken.rs").write_text("fn broken( {{{ this is not rust\n")

# (e) benign control file: must extract normally.
src.joinpath("benign.rs").write_text("pub fn greet() -> u32 {\n    42\n}\n")
PY
[ "$(stat -c %s "$PROJ/src/big.rs")" -gt $((11 * 1024 * 1024)) ] || fail "big.rs is not oversize"

# ── 4. PATH wiring — staged plugin first, then the release loomweave ─────────
export PATH="$PLUGIN_DIR:$REPO_ROOT/target/release:$PATH"

# ── 5. loomweave install + analyze (exit 0 is itself an assert: set -e) ──────
cd "$PROJ"
log "running: loomweave install"
loomweave install
DB="$PROJ/.weft/loomweave/loomweave.db"
[ -f "$DB" ] || fail ".weft/loomweave/loomweave.db not created by loomweave install"

log "running: loomweave analyze . (hostile corpus)"
loomweave analyze .
log "analyze exited 0"

# ── 6. Run record: exactly one row, completed (nothing stuck `running`) ──────
log "verifying single completed runs row ..."
RUNS=$(sqlite3 "$DB" "select count(*) || '|' || group_concat(status) from runs;")
if [ "$RUNS" != "1|completed" ]; then
    sqlite3 "$DB" "select id, status, failure_reason from runs;" >&2 || true
    fail "expected exactly one completed runs row; got: $RUNS"
fi

# ── 7. Guard findings: every hostile file degraded to a visible finding ──────
log "verifying guard findings (DEPTH-LIMIT x2, FILE-TOO-LARGE x1, SYNTAX-ERROR x1) ..."
GUARDS=$(sqlite3 "$DB" "
    select
        (select count(*) from findings where rule_id = 'LMWV-RUST-DEPTH-LIMIT')
        || '|' ||
        (select count(*) from findings where rule_id = 'LMWV-RUST-FILE-TOO-LARGE')
        || '|' ||
        (select count(*) from findings where rule_id = 'LMWV-RUST-SYNTAX-ERROR');")
if [ "$GUARDS" != "2|1|1" ]; then
    log "findings table:"
    sqlite3 "$DB" "select rule_id, severity, message from findings order by rule_id;" >&2 || true
    fail "expected DEPTH-LIMIT|FILE-TOO-LARGE|SYNTAX-ERROR counts 2|1|1; got: $GUARDS"
fi

# ── 8. Degraded modules persisted with their parse_status ────────────────────
log "verifying degraded module entities carry parse_status ..."
DEGRADED=$(sqlite3 "$DB" "
    select id || '|' || json_extract(properties, '\$.parse_status')
    from entities
    where json_extract(properties, '\$.parse_status') in ('depth_limit', 'file_too_large')
    order by id;")
DEGRADED_EXPECTED="rust:module:hostile_corpus.big|file_too_large
rust:module:hostile_corpus.deep_parens|depth_limit
rust:module:hostile_corpus.unary_bomb|depth_limit"
if [ "$DEGRADED" != "$DEGRADED_EXPECTED" ]; then
    log "entities with parse_status:"
    sqlite3 "$DB" "select id, json_extract(properties, '\$.parse_status') from entities;" >&2 || true
    fail "expected degraded modules:\n$DEGRADED_EXPECTED\ngot:\n$DEGRADED"
fi

# ── 9. Benign file extracted normally ────────────────────────────────────────
log "verifying benign.rs extracted normally ..."
BENIGN=$(sqlite3 "$DB" "
    select count(*) from entities
    where id in ('rust:module:hostile_corpus.benign',
                 'rust:function:hostile_corpus.benign.greet');")
if [ "$BENIGN" != "2" ]; then
    log "rust entities:"
    sqlite3 "$DB" "select id, kind from entities where plugin_id = 'rust' order by id;" >&2 || true
    fail "expected benign module + function entities; matched $BENIGN of 2"
fi

# ── 10. Floor stayed quiet: no crash-loop / OOM / timeout / abort findings ───
log "verifying no crash-loop / OOM / timeout / abort findings (breaker untripped) ..."
INFRA=$(sqlite3 "$DB" "
    select count(*) from findings
    where rule_id in (
        'LMWV-INFRA-PLUGIN-DISABLED-CRASH-LOOP',
        'LMWV-INFRA-PLUGIN-OOM-KILLED',
        'LMWV-INFRA-PLUGIN-ABORTED',
        'LMWV-PY-TIMEOUT',
        'LMWV-INFRA-PLUGIN-CRASH',
        'LMWV-INFRA-PLUGIN-SHUTDOWN-TIMEOUT');")
if [ "$INFRA" != "0" ]; then
    log "infra findings:"
    sqlite3 "$DB" "select rule_id, message from findings where rule_id like 'LMWV-INFRA-%' or rule_id = 'LMWV-PY-TIMEOUT';" >&2 || true
    fail "expected zero crash/OOM/timeout/abort findings; got $INFRA"
fi

log "PASS: hostile corpus fully degraded — run completed, guards fired (2x depth, 1x size, 1x syntax), benign file extracted, breaker untripped"
