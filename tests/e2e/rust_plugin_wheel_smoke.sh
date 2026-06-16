#!/usr/bin/env bash
# End-to-end acceptance gate for LIVE Rust-plugin activation via the wheel.
#
# Builds the `loomweave` + `loomweave-plugin-rust` wheels, installs BOTH into a
# fresh venv (--no-deps; this is a pure-Rust path, no Python plugin needed),
# then runs the INSTALLED `loomweave analyze` on a tiny .rs crate and asserts
# that the Rust plugin was discovered live and persisted Rust entities + a
# resolved calls edge. This exercises exactly the failure modes the normal
# `cargo`/`pytest` suites cannot reach: a wrong installed bin name, a misrouted
# `share/loomweave/plugins/rust/plugin.toml`, or a discovery-fallback miss.
#
# Slow (release wheel builds). Run from the repo root:
#   bash tests/e2e/rust_plugin_wheel_smoke.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST_CRATE="$REPO_ROOT/packaging/rust-plugin-dist"
OUT="$DIST_CRATE/dist"
SMOKE="$(mktemp -d "${TMPDIR:-/tmp}/lw-rust-wheel-smoke.XXXXXX")"
trap 'rm -rf "$SMOKE"' EXIT
# Hermetic install (clarion-c5e3cc2818): `loomweave install` registers a
# Codex MCP entry in ~/.codex/config.toml unless this override points it
# at a scratch-local file. Never mutate the operator's real config.
export LOOMWEAVE_CODEX_CONFIG="$SMOKE/codex-config.toml"

command -v maturin >/dev/null || { echo "FAIL: maturin not on PATH"; exit 1; }
command -v uv >/dev/null || { echo "FAIL: uv not on PATH"; exit 1; }

echo "== building wheels (release) =="
maturin build --release --manifest-path "$REPO_ROOT/crates/loomweave-cli/Cargo.toml" --out "$OUT" >/dev/null
maturin build --release --manifest-path "$DIST_CRATE/Cargo.toml" --out "$OUT" >/dev/null

echo "== fresh venv, install both wheels (--no-deps) =="
uv venv "$SMOKE/venv" -q
VBIN="$SMOKE/venv/bin"
uv pip install --python "$VBIN/python" -q --no-deps \
    "$OUT"/loomweave-*.whl "$OUT"/loomweave_plugin_rust-*.whl

# Layout assertions: both binaries co-located + manifest staged for discovery.
test -x "$VBIN/loomweave-plugin-rust" || { echo "FAIL: glob-named plugin bin missing in venv/bin"; exit 1; }
test -f "$SMOKE/venv/share/loomweave/plugins/rust/plugin.toml" || { echo "FAIL: plugin.toml not staged under share/"; exit 1; }

echo "== analyze a tiny .rs crate via the INSTALLED binary =="
PROJ="$SMOKE/proj"
mkdir -p "$PROJ/src"
cat > "$PROJ/Cargo.toml" <<'TOML'
[package]
name = "smoke_crate"
version = "0.1.0"
edition = "2021"
TOML
cat > "$PROJ/src/lib.rs" <<'RS'
pub struct Widget { pub n: u32 }
pub fn make_widget() -> Widget { Widget { n: 1 } }
pub fn use_widget() -> u32 { let w = make_widget(); w.n }
RS

"$VBIN/loomweave" install --path "$PROJ" >/dev/null
"$VBIN/loomweave" analyze "$PROJ" >/dev/null

echo "== assert Rust entities + resolved calls edge landed =="
DB="$PROJ/.weft/loomweave/loomweave.db"
"$VBIN/python" - "$DB" <<'PY'
import sqlite3, sys
db = sys.argv[1]
c = sqlite3.connect(db)
rust = {r[0] for r in c.execute("SELECT id FROM entities WHERE plugin_id='rust'")}
want = {
    "rust:module:smoke_crate",
    "rust:struct:smoke_crate.Widget",
    "rust:function:smoke_crate.make_widget",
    "rust:function:smoke_crate.use_widget",
}
missing = want - rust
assert not missing, f"FAIL: missing Rust entities: {missing} (got {rust})"
edge = c.execute(
    "SELECT confidence FROM edges WHERE kind='calls' "
    "AND from_id='rust:function:smoke_crate.use_widget' "
    "AND to_id='rust:function:smoke_crate.make_widget'"
).fetchone()
assert edge and edge[0] == "resolved", f"FAIL: expected resolved use_widget->make_widget calls edge, got {edge}"
print("PASS: rust plugin discovered live; entities + resolved calls edge persisted")
PY
echo "== rust_plugin_wheel_smoke: PASS =="
