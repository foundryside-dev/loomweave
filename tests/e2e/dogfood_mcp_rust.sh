#!/usr/bin/env bash
# Rust-plugin MCP dogfood gate (plan 2026-06-10 rust-plugin-scale-qa, decision
# D8 committed half): prove the MCP surface answers correctly for RUST entities
# against a SELF-BUILT fixture index, with assertions on entity ids / qualnames
# the fixture controls — not just exit codes.
#
# Fixture (generated in a temp dir, never checked in): a small Rust crate
# `mcp_fixture` with two modules, a struct deriving an in-project trait,
# cross-module resolved calls + references, a method call (unresolved by the
# syntactic resolver → exercises the dead-code suppression shield), and one
# dead function never called anywhere. One tiny Python file rides along ONLY
# to supply a dead-code reachability root: the Rust plugin emits no
# categorisation tags (entry-point/test/exported-api...), so a pure-Rust index
# trips entity_dead_list's empty-root guard (honest signal-unavailable, zero
# candidates). The real dogfood index is mixed-language the same way.
#
# MCP checks (Content-Length framed stdio JSON-RPC, sprint_2_mcp_surface.sh
# harness pattern):
#   - entity_find by name / by kind / by full ADR-049 qualname
#   - entity_resolve by ADR-049 Rust qualname  [KNOWN-GAP — see check]
#   - entity_callers_list on Rust functions (cross-module + same-module)
#   - entity_neighborhood_get on a Rust struct (references_in direct) and a
#     Rust module (references_in/out rolled up, imports)
#   - entity_at on a .rs file:line
#   - entity_dead_list (dead fn flagged; method-called fn shielded;
#     unresolved_call_site_suppressed disclosed)
#   - entity_subsystem_get + subsystem_member_list (Rust modules cluster)
#   - project_status_get (per-plugin counts, completed run, SEI populated)
#
# Dependencies: cargo, python3.
#
# Env overrides:
#   REPO_ROOT   — auto-detected via `git rev-parse`; override to test an external checkout.
#   CARGO_BUILD — set to "0" to skip `cargo build` (assumes target/debug binaries present).
#   VENV        — Python-plugin venv (default plugins/python/.venv).

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
VENV="${VENV:-$REPO_ROOT/plugins/python/.venv}"
CARGO_BUILD="${CARGO_BUILD:-1}"

log() { printf '[dogfood-mcp-rust] %s\n' "$*" >&2; }
fail() { printf '[dogfood-mcp-rust] FAIL: %s\n' "$*" >&2; exit 1; }

cd "$REPO_ROOT"

# ── 1. Build dev binaries (loomweave + the off-glob rust plugin bin) ─────────
if [ "$CARGO_BUILD" = "1" ]; then
    log "building workspace bins (debug) ..."
    cargo build --workspace --bins
fi
LOOMWEAVE_BIN="$REPO_ROOT/target/debug/loomweave"
RUST_PLUGIN_BIN="$REPO_ROOT/target/debug/loomweave-rust-plugin"
[ -x "$LOOMWEAVE_BIN" ] || fail "loomweave binary missing at $LOOMWEAVE_BIN"
[ -x "$RUST_PLUGIN_BIN" ] || fail "loomweave-rust-plugin binary missing at $RUST_PLUGIN_BIN"

# ── 2. Python plugin venv (supplies the dead-code root tag — see header) ─────
if [ ! -d "$VENV" ]; then
    log "creating venv at $VENV ..."
    python3 -m venv "$VENV"
fi
log "installing loomweave-plugin-python (editable) ..."
"$VENV/bin/pip" install --quiet -e "$REPO_ROOT/plugins/python[dev]"
PY_PLUGIN_BIN="$VENV/bin/loomweave-plugin-python"
[ -x "$PY_PLUGIN_BIN" ] || fail "loomweave-plugin-python missing at $PY_PLUGIN_BIN"

# ── 3. Stage the rust plugin under its discovery name + neighbor manifest ────
SCRATCH="$(mktemp -d -t loomweave-dogfood-mcp-rust-XXXXXX)"
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

# ── 4. Generate the fixture project (never checked in) ───────────────────────
PROJ="$SCRATCH/proj"
mkdir -p "$PROJ/src"
cat > "$PROJ/Cargo.toml" <<'TOML'
[package]
name = "mcp_fixture"
version = "0.1.0"
edition = "2021"
TOML

# The Phase-1b resolver (resolve.rs) resolves crate-qualified paths
# (`crate::ops::entry`) and bare names at CRATE ROOT only; a bare call inside
# a nested module and a multi-segment non-`crate::` path (`ops::entry`)
# under-resolve to unresolved call sites by design (H5-safe). The fixture
# therefore uses the forms the resolver supports, plus one deliberate
# unresolved method call to exercise the dead-code suppression shield.
cat > "$PROJ/src/lib.rs" <<'RS'
pub mod ops;
pub mod shapes;

/// Crate-root sibling called bare (crate-root-relative fallback probe).
pub fn seed() -> usize {
    1
}

/// Crate-root caller: bare same-file call + crate-qualified cross-module call.
pub fn run() -> usize {
    seed() + crate::ops::entry()
}
RS

# Line numbers below are load-bearing: the entity_at check targets line 17
# (inside `entry`'s body). Keep this file byte-stable or update that check.
cat > "$PROJ/src/ops.rs" <<'RS'
use crate::shapes::Widget;

/// Builds the canonical widget (return-type + struct-literal references).
pub fn build_widget() -> crate::shapes::Widget {
    crate::shapes::Widget {
        label: String::from("fixture"),
    }
}

/// Measures a widget by delegating to its method (unresolved call site).
pub fn widget_size(widget: &crate::shapes::Widget) -> usize {
    widget.label_len()
}

/// Entry: crate-qualified same-module resolved calls.
pub fn entry() -> usize {
    let widget = crate::ops::build_widget();
    crate::ops::widget_size(&widget)
}

/// Never called by anything — the dead-code probe.
pub fn orphan_helper() -> u32 {
    7
}
RS

cat > "$PROJ/src/shapes.rs" <<'RS'
/// In-project trait targeted by the derive below.
pub trait Describe {
    fn describe(&self) -> String;
}

/// Struct deriving an in-project trait via a crate-qualified derive path
/// (the resolvable form — see the resolver note above). The derive never
/// has to compile — analyze parses, it does not build.
#[derive(crate::shapes::Describe)]
pub struct Widget {
    pub label: String,
}

impl Widget {
    /// Reached ONLY via a method call (`widget.label_len()`), which the
    /// syntactic resolver records as an unresolved call site — this method
    /// must be shielded from dead-code by the suppression set.
    pub fn label_len(&self) -> usize {
        self.label.len()
    }
}
RS

# Dead-code reachability root (see header): a Python test function carries the
# `test` categorisation tag; the Rust plugin emits no tags at all.
cat > "$PROJ/test_anchor.py" <<'PY'
def test_anchor() -> bool:
    return True
PY

# ── 5. PATH wiring — staged rust plugin, dev loomweave, python plugin ────────
export PATH="$PLUGIN_DIR:$REPO_ROOT/target/debug:$VENV/bin:$PATH"

cd "$PROJ"
log "running: loomweave install"
loomweave install
[ -f "$PROJ/.weft/loomweave/loomweave.db" ] || fail ".weft/loomweave/loomweave.db not created"

log "running: loomweave analyze ."
loomweave analyze .

# ── 6. Drive the MCP stdio surface and assert on fixture-controlled content ──
log "driving MCP stdio requests ..."
python3 - "$LOOMWEAVE_BIN" "$PROJ" <<'PY'
import json
import subprocess
import sys
from pathlib import Path

loomweave_bin = Path(sys.argv[1])
project_dir = Path(sys.argv[2])

OPS_MOD = "rust:module:mcp_fixture.ops"
SHAPES_MOD = "rust:module:mcp_fixture.shapes"
ROOT_MOD = "rust:module:mcp_fixture"
RUN_FN = "rust:function:mcp_fixture.run"
SEED_FN = "rust:function:mcp_fixture.seed"
ENTRY_FN = "rust:function:mcp_fixture.ops.entry"
BUILD_FN = "rust:function:mcp_fixture.ops.build_widget"
SIZE_FN = "rust:function:mcp_fixture.ops.widget_size"
ORPHAN_FN = "rust:function:mcp_fixture.ops.orphan_helper"
WIDGET = "rust:struct:mcp_fixture.shapes.Widget"
DESCRIBE = "rust:trait:mcp_fixture.shapes.Describe"
LABEL_LEN = "rust:function:mcp_fixture.shapes.Widget.impl#<>.label_len"


def write_frame(proc: subprocess.Popen, message: dict) -> None:
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    proc.stdin.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n")
    proc.stdin.write(body)
    proc.stdin.flush()


def read_frame(proc: subprocess.Popen) -> dict:
    headers = {}
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


def tool_envelope(response: dict) -> dict:
    result = response["result"]
    assert isinstance(result, dict), result
    content = result["content"]
    assert isinstance(content, list) and content, result
    envelope = json.loads(content[0]["text"])
    assert envelope["ok"] is True, envelope
    assert envelope["error"] is None, envelope
    return envelope


proc = subprocess.Popen(
    [str(loomweave_bin), "serve", "--path", str(project_dir)],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)
assert proc.stdin is not None and proc.stdout is not None and proc.stderr is not None

next_id = [0]


def call(name: str, arguments: dict) -> dict:
    next_id[0] += 1
    write_frame(
        proc,
        {
            "jsonrpc": "2.0",
            "id": f"req-{next_id[0]}",
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        },
    )
    return read_frame(proc)


failures = []


def check(name: str, fn) -> bool:
    try:
        fn()
    except AssertionError as err:
        failures.append(name)
        print(f"[dogfood-mcp-rust] FAIL: {name}: {err}", file=sys.stderr)
        return False
    print(f"[dogfood-mcp-rust] PASS: {name}", file=sys.stderr)
    return True


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
                "clientInfo": {"name": "dogfood-mcp-rust", "version": "0.0.0"},
            },
        },
    )
    init = read_frame(proc)
    assert init["result"]["protocolVersion"] == "2025-11-25", init

    # 1. entity_find by short name — the dead probe is findable by name.
    def find_by_name():
        env = tool_envelope(call("entity_find", {"pattern": "orphan_helper", "limit": 10}))
        ids = {e["id"] for e in env["result"]["entities"]}
        assert ORPHAN_FN in ids, ids
        return ids

    check("entity_find by name (orphan_helper)", find_by_name)

    # 2. entity_find with a kind filter — Rust struct discoverable as "struct".
    def find_by_kind():
        env = tool_envelope(call("entity_find", {"pattern": "Widget", "kind": "struct", "limit": 10}))
        entities = env["result"]["entities"]
        ids = {e["id"] for e in entities}
        assert WIDGET in ids, ids
        assert all(e["kind"] == "struct" for e in entities), entities
        return ids

    check("entity_find by kind=struct (Widget)", find_by_kind)

    # 3. entity_find by full ADR-049 qualname — id-substring recall makes the
    # dotted Rust qualname (incl. the impl discriminator) directly searchable.
    def find_by_qualname():
        env = tool_envelope(call("entity_find", {"pattern": "mcp_fixture.ops.entry", "limit": 10}))
        ids = {e["id"] for e in env["result"]["entities"]}
        assert ENTRY_FN in ids, ids
        env2 = tool_envelope(
            call("entity_find", {"pattern": "Widget.impl#<>.label_len", "limit": 10})
        )
        ids2 = {e["id"] for e in env2["result"]["entities"]}
        assert LABEL_LEN in ids2, ids2

    check("entity_find by ADR-049 qualname (free fn + impl-discriminated method)", find_by_qualname)

    # 4. KNOWN-GAP — entity_resolve cannot resolve a Rust qualname. The
    # resolver (loomweave-storage wardline_taint.rs::function_candidate)
    # hardcodes the candidate id as `python:function:{qualname}`, so an
    # existing `rust:function:mcp_fixture.ops.entry` comes back "unresolved".
    # This check PINS the current gap behavior: when the resolver learns
    # plugin-aware candidates, this assertion fails and the gap marker must
    # be replaced with a real resolution assertion.
    def resolve_qualname_gap():
        env = tool_envelope(call("entity_resolve", {"qualnames": ["mcp_fixture.ops.entry"]}))
        results = env["result"]["results"]
        assert len(results) == 1, results
        assert results[0]["qualname"] == "mcp_fixture.ops.entry", results
        assert results[0]["result_kind"] == "unresolved", results
        assert results[0]["candidates"] == [], results

    if check("entity_resolve Rust qualname pins the unresolved gap", resolve_qualname_gap):
        print(
            "[dogfood-mcp-rust] KNOWN-GAP: entity_resolve hardcodes python:function: "
            "candidates (wardline_taint.rs) — Rust qualnames return result_kind="
            "\"unresolved\"; use entity_find as the Rust reverse-lookup for now",
            file=sys.stderr,
        )

    # 5. Callers of a Rust function — cross-module crate-qualified call
    # (`run` calls `crate::ops::entry()`).
    def callers_cross_module():
        env = tool_envelope(call("entity_callers_list", {"id": ENTRY_FN}))
        ids = {item["entity"]["id"] for item in env["result"]["callers"]}
        assert RUN_FN in ids, ids

    check("entity_callers_list cross-module (run -> crate::ops::entry)", callers_cross_module)

    # 6. Callers — same-module crate-qualified call (entry -> build_widget)
    # and crate-root bare call (run -> seed), the two resolved call forms.
    def callers_same_module():
        env = tool_envelope(call("entity_callers_list", {"id": BUILD_FN}))
        ids = {item["entity"]["id"] for item in env["result"]["callers"]}
        assert ENTRY_FN in ids, ids
        env2 = tool_envelope(call("entity_callers_list", {"id": SEED_FN}))
        ids2 = {item["entity"]["id"] for item in env2["result"]["callers"]}
        assert RUN_FN in ids2, ids2

    check("entity_callers_list same-module + crate-root bare (entry->build_widget, run->seed)", callers_same_module)

    # 7. Neighborhood of a Rust struct — direct (un-rolled) references_in from
    # the ops functions that mention Widget in type/expression position.
    def neighborhood_struct():
        env = tool_envelope(call("entity_neighborhood_get", {"id": WIDGET}))
        result = env["result"]
        assert result["entity"]["id"] == WIDGET, result["entity"]
        assert result["references_rolled_up"] is False, result
        ref_in = {item["entity"]["id"] for item in result["references_in"]}
        assert BUILD_FN in ref_in, ref_in
        assert SIZE_FN in ref_in, ref_in
        assert result["container"]["id"] == SHAPES_MOD, result["container"]

    check("entity_neighborhood_get struct (references_in populated)", neighborhood_struct)

    # 8. Neighborhood of a Rust module — references rolled up over contained
    # symbols: ops' references_out reaches Widget; shapes' references_in names
    # the referencing ops symbols via `via`.
    def neighborhood_module():
        env = tool_envelope(call("entity_neighborhood_get", {"id": OPS_MOD}))
        result = env["result"]
        assert result["references_rolled_up"] is True, result
        ref_out = {item["entity"]["id"] for item in result["references_out"]}
        assert WIDGET in ref_out, ref_out

        env2 = tool_envelope(call("entity_neighborhood_get", {"id": SHAPES_MOD}))
        result2 = env2["result"]
        assert result2["references_rolled_up"] is True, result2
        in_items = result2["references_in"]
        in_ids = {item["entity"]["id"] for item in in_items}
        assert BUILD_FN in in_ids, in_ids
        vias = {item["via"]["id"] for item in in_items if item.get("via")}
        assert WIDGET in vias, vias

    check("entity_neighborhood_get module (rolled-up references_in/out)", neighborhood_module)

    # 9. entity_at on a .rs file:line — line 17 is inside `entry`'s body
    # (see the load-bearing comment above src/ops.rs generation).
    def entity_at_rs():
        env = tool_envelope(call("entity_at", {"file": "src/ops.rs", "line": 17}))
        entity = env["result"]["entity"]
        assert entity is not None, env["result"]
        assert entity["id"] == ENTRY_FN, entity
        miss = tool_envelope(call("entity_at", {"file": "src/ops.rs", "line": 999}))
        assert miss["result"]["entity"] is None, miss["result"]

    check("entity_at on .rs file:line (entry hit + line-999 miss)", entity_at_rs)

    # 10. Dead code — orphan_helper flagged; label_len (reached only through a
    # method call the syntactic resolver cannot resolve) shielded by the
    # unresolved-call-site suppression, with the suppressed count disclosed.
    def dead_code():
        env = tool_envelope(call("entity_dead_list", {}))
        result = env["result"]
        ids = {item["entity"]["id"] for item in result["dead_code"]}
        assert ORPHAN_FN in ids, ids
        assert LABEL_LEN not in ids, ids
        assert "unresolved_call_site_suppressed" in result, result
        # label_len is the only fixture entity whose short name matches an
        # unresolved callee leaf (".label_len"), so the count is exactly 1.
        assert result["unresolved_call_site_suppressed"] == 1, result
        flagged = next(item for item in result["dead_code"] if item["entity"]["id"] == ORPHAN_FN)
        assert flagged["rule_id"] == "LMWV-FACT-DEAD-CODE-CANDIDATE", flagged

    check("entity_dead_list (orphan flagged, method-callee shielded, count=1)", dead_code)

    # 11. Subsystem — the three connected Rust modules (root --calls--> ops
    # --imports--> shapes) clear min_cluster_size=3 and cluster together; the
    # isolated Python module does not join.
    def subsystem():
        env = tool_envelope(call("entity_subsystem_get", {"id": OPS_MOD}))
        result = env["result"]
        assert result["entity"]["id"] == OPS_MOD, result
        subsystem = result["subsystem"]
        assert subsystem is not None, result
        assert subsystem["id"].startswith("core:subsystem:"), subsystem
        members_env = tool_envelope(call("subsystem_member_list", {"id": subsystem["id"]}))
        member_ids = {m["id"] for m in members_env["result"]["members"]}
        assert {ROOT_MOD, OPS_MOD, SHAPES_MOD} <= member_ids, member_ids

    check("entity_subsystem_get + subsystem_member_list (3 Rust modules)", subsystem)

    # 12. Project status — per-plugin entity counts include rust, the run
    # completed, and SEI bindings are populated for Rust entities.
    def project_status():
        env = tool_envelope(call("project_status_get", {}))
        result = env["result"]
        assert result["latest_run"]["status"] == "completed", result["latest_run"]
        plugins = {row["plugin_id"]: row["entity_count"] for row in result["plugins"]}
        assert plugins.get("rust", 0) >= 9, plugins
        assert plugins.get("python", 0) >= 1, plugins
        assert result["counts"]["entities"] >= 10, result["counts"]
        assert result["counts"]["edges"] >= 5, result["counts"]
        assert result["counts"]["subsystems"] >= 1, result["counts"]
        assert result["sei"]["populated"] is True, result["sei"]

    check("project_status_get (rust plugin counts, completed run, SEI)", project_status)
finally:
    proc.stdin.close()
    status = proc.wait(timeout=10)
    stderr = proc.stderr.read().decode("utf-8", "replace")
    assert status == 0, f"loomweave serve exited {status}; stderr={stderr}"

if failures:
    print(f"[dogfood-mcp-rust] {len(failures)} check(s) failed: {failures}", file=sys.stderr)
    sys.exit(1)
PY

log "PASS: MCP surface answered for Rust entities (find/qualname/callers/neighborhood/entity_at/dead-code/subsystem/status); 1 KNOWN-GAP pinned (entity_resolve is python-only)"
