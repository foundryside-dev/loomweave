# Rust Language Plugin — Phase 1b Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take the Rust plugin from the inert Phase-1a identity foundation to a plugin that indexes Loomweave's own workspace end-to-end — the remaining 6 entity kinds + the `impl` entity, the `imports`/`implements` resolving edges, and a writer-proving `analyze`→storage E2E — without ever faking a Resolved edge or tripping the storage writer.

**Architecture:** Phase 1a already ships crate-root discovery, the ADR-049 qualname scheme, `module`/`struct`/`function` entities, the `contains` edge, SEI signatures for function/struct, and the identity stability+uniqueness gate. Phase 1b is *one genuinely-new subsystem* — a project symbol-table **path resolver** (qualname→id reverse index that turns a `use`/trait path into a unique in-project id, else Ambiguous) — wrapped in mostly-mechanical wire-up of the other entity kinds and one structural change (the `impl` entity + method re-parenting). A single host-side **seen-entity-set gate** closes three otherwise-separate hazards (external targets, gitignored-file supersets, mid-run staleness): a resolving edge whose target the host did not actually store is dropped, never written as a dangling FK.

**Tech Stack:** Rust, `syn` 2 (full/extra-traits/visit), `proc-macro2` (span-locations), `serde_json`; the existing Content-Length-framed JSON-RPC plugin host in `crates/loomweave-core/src/plugin/`; the storage writer in `crates/loomweave-storage/src/writer.rs`; the CLI `analyze` path in `crates/loomweave-cli/`.

---

## Decisions baked into this plan

| # | Decision | Resolution | Evidence |
|---|----------|-----------|----------|
| D1 | External-target representation for resolving edges | **Drop** external targets (emit no edge); **`derives` slips to Phase 2** | `edges.to_id` is `NOT NULL REFERENCES entities(id)` with FKs on; the end-of-run force-flush (`analyze.rs:879-882`) inserts pending edges unconditionally → an external target FK-HardFails. "Ambiguous" is not an escape hatch (its `to_id` must be a real in-project candidate). User-confirmed. |
| D2 | Plugin walk vs host walk divergence | **Gate Resolved on the host seen-entity set** (don't merely detect divergence) | Plugin walk (`symbol_table.rs:147-152`) skips only `target/.git/.weft/node_modules`, is **not** gitignore-aware; the host walk is. The plugin table is a *superset* → a Resolved edge to a gitignored target FK-HardFails. |
| D3 | Cross-file-edge staleness (M5) | **Same seen-entity-set gate** (re-verify target was stored; drop if absent) | The writer rejects Inferred on anchored edges at scan time (`writer.rs:751`); imports/implements are anchored, so "downgrade-to-Inferred" is illegal and would FailRun the very edges it protects. D1+D2+D3 collapse into **one** mechanism. |
| `#[path]` | Honor `#[path="…"]` module overrides | **Defer**; correct the ADR-049 + Cargo.toml wording (Task 11) | `grep -rn '#\[path' crates/` (excluding plugin-rust) → **0 hits**. YAGNI: do not implement; make the docs honest. |
| Security | Plugin init-walk symlink/cycle safety | **In scope** (Task 9, plugin-side, cheap) | The plugin's `walk_rs_files` follows directory symlinks and is unjailed; once the table is *load-bearing* (1b resolves against it) a symlink out of the tree = out-of-tree reads / cycle crash. |
| Security | Host `RLIMIT_STACK`/`RLIMIT_CPU` + syn recursion depth | **Out of scope** — separate tracked item | No `setrlimit`/`pre_exec` anywhere in the host; this protects **all** plugins (Python included), not just Rust. Belongs in a host-hardening ticket, not this phase. Noted in Task 12. |

## ⚠ One open decision — confirm before executing Task 5

**Ordinal-churn resolution (your landmine #2).** ADR-049 §1 calls the inherent-impl discriminator "source-order-**independent**" but then assigns its ordinal "by source order within a file" — a direct self-contradiction: reordering two same-signature inherent impls of the same type churns both `impl`-entity locators. This plan resolves it via **Option (b): merge** same-`(type, generic-sig, cfg)` inherent impls into a single `impl` entity, which removes the ordinal entirely (the only place it was load-bearing was the `impl` entity 1b introduces; it is already vacuous for method locators because the method name disambiguates). Option (b) is the only scheme that is simultaneously unique, reorder-stable, *and* method-set-stable. **This is churn-free right now** because the repo holds no persisted index; after the first live dogfood it would not be. If you prefer **Option (a)** (keep the source-order ordinal, accept + document the rare reorder churn, soften the ADR wording), Task 5 changes as noted in its header — lower effort, leaves a known churn corner. Task 5 is written for **(b)**.

---

## File Structure

**New files:**
- `crates/loomweave-plugin-rust/src/scope.rs` — the shared crate-scope predicate (`src/`-only + redundant-`main.rs`) extracted from `symbol_table.rs`, used by **both** the symbol-table walk and `analyze_one_file`. One responsibility: "is this file part of the crate the qualname scheme names, and what is its `(crate_name, module_path)`?"
- `crates/loomweave-plugin-rust/src/resolve.rs` — the path resolver: the `qualname→id` reverse index over the symbol table, plus `resolve_use_path` / `resolve_trait_path` returning `Resolution::{Resolved(id), Ambiguous, External}`. The one genuinely-new subsystem.
- `crates/loomweave-plugin-rust/src/edges.rs` — `imports`/`implements` edge construction from `syn` AST + a `Resolution`. Keeps edge-shaping out of `extract.rs`.
- `crates/loomweave-plugin-rust/tests/analyze_e2e.rs` — the front-loaded writer-proving E2E (self-staging, real `loomweave analyze`, set-based id snapshot). Grows across the phase.
- `crates/loomweave-plugin-rust/tests/fixtures/e2e_crate/` — the vendored multi-file fixture crate (`src/lib.rs`, `src/sub.rs`, `tests/it.rs`, `build.rs`).
- `crates/loomweave-plugin-rust/tests/resolve.rs`, `.../tests/imports_edges.rs`, `.../tests/implements_edges.rs` — per-edge unit suites.

**Modified files:**
- `crates/loomweave-plugin-rust/src/symbol_table.rs` — delegate scope logic to `scope.rs`; add the `by_qualname` reverse index accessor.
- `crates/loomweave-plugin-rust/src/main.rs` — `analyze_one_file` consults `scope.rs` (returns empty for out-of-scope files) and threads the symbol table into extraction.
- `crates/loomweave-plugin-rust/src/extract.rs` — add the 6 leaf kinds; emit the `impl` entity + re-parent methods; emit resolving edges via `edges.rs`.
- `crates/loomweave-plugin-rust/src/qualname.rs` — `ImplDisc::inherent` drops the ordinal (Option b).
- `crates/loomweave-plugin-rust/src/signature.rs` — `trait_signature`, `impl_signature`.
- `crates/loomweave-plugin-rust/plugin.toml` — declare the new `entity_kinds` and the `implements`/`imports` `edge_kinds`; ADR-027 MINOR ontology bump.
- `crates/loomweave-core/src/plugin/...` (writer or analyze) — mint `implements` in the anchored-edge ontology; generalize the imports external-filter to cover `implements` (the seen-entity-set gate).
- `docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md`, `crates/loomweave-plugin-rust/Cargo.toml` — doc-truth corrections.

---

## Task 1: Front-loaded writer-proving E2E (RED — exposes the module-id collision)

This is the gate that converts the 1a `contains`/`parent_id` fix from in-memory-proven (the host-integration test never constructs a `Writer`) to **writer-proven**, and exposes landmine #1 (the `tests/`/`build.rs` module-id collision is a *silent* `ON CONFLICT` merge, **not** a FailRun — so the assertion must check the emitted id **set**, not just run status). Written first; it goes **red**; Task 2 makes it green.

**Files:**
- Create: `crates/loomweave-plugin-rust/tests/fixtures/e2e_crate/Cargo.toml`
- Create: `crates/loomweave-plugin-rust/tests/fixtures/e2e_crate/src/lib.rs`
- Create: `crates/loomweave-plugin-rust/tests/fixtures/e2e_crate/src/sub.rs`
- Create: `crates/loomweave-plugin-rust/tests/fixtures/e2e_crate/tests/it.rs`
- Create: `crates/loomweave-plugin-rust/tests/fixtures/e2e_crate/build.rs`
- Create: `crates/loomweave-plugin-rust/tests/analyze_e2e.rs`
- Reference (self-staging pattern): `crates/loomweave-cli/tests/wp2_e2e.rs` (the symlink-off-glob-binary-under-discovery-name-on-synthetic-`$PATH` harness)
- Reference (host writer FailRun shape): `crates/loomweave-storage/src/writer.rs:1186-1314`

- [ ] **Step 1: Write the vendored fixture crate.** Deliberately include the out-of-`src/` files that trigger the collision.

`fixtures/e2e_crate/Cargo.toml`:
```toml
[package]
name = "e2e_crate"
version = "0.0.0"
edition = "2021"
```
`fixtures/e2e_crate/src/lib.rs`:
```rust
pub mod sub;
pub struct Widget { pub n: i32 }
pub fn make() -> Widget { Widget { n: 0 } }
impl Widget { pub fn bump(&mut self) { self.n += 1; } }
```
`fixtures/e2e_crate/src/sub.rs`:
```rust
pub fn helper() -> i32 { 1 }
```
`fixtures/e2e_crate/tests/it.rs` (separate compilation unit — must NOT be attributed to the crate):
```rust
fn test_only_helper() {}
```
`fixtures/e2e_crate/build.rs` (separate compilation unit — must NOT be attributed to the crate):
```rust
fn main() {}
```

- [ ] **Step 2: Write the E2E test with a SET-based id assertion.** Drive the real CLI `analyze` over the fixture, then read the resulting index DB (or the run summary the CLI exposes) and assert the **exact** stored entity-id set, ordered by id. The set is the load-bearing assertion: the silent collision shows up as a *missing/merged* `rust:module:e2e_crate` and as `tests/`/`build.rs` items being absent (or wrongly present). Model the staging + `analyze` invocation on `wp2_e2e.rs:84-114`.

```rust
//! Front-loaded writer-proving E2E: real `loomweave analyze` over a vendored
//! multi-file crate must (a) COMPLETE (run status != failed → proves the
//! contains/parent_id dual-encoding round-trips through commit_run /
//! parent_contains_mismatch, which has never run against Rust output) and
//! (b) emit EXACTLY the expected entity-id set (proves the out-of-`src/`
//! files do NOT mint a colliding `rust:module:e2e_crate`, a SILENT ON CONFLICT
//! merge a run-status check cannot catch).

#[test]
fn analyze_over_multifile_crate_completes_with_exact_id_set() {
    let staged = stage_plugin_on_path();          // symlink off-glob bin under discovery name (see wp2_e2e.rs)
    let project = copy_fixture_to_tempdir("e2e_crate");
    let outcome = run_loomweave_analyze(&staged, &project);

    assert_eq!(outcome.run_status, "completed",
        "analyze must COMPLETE, not FailRun: {:?}", outcome.diagnostics);

    let mut got: Vec<String> = outcome.entity_ids();
    got.sort();
    let mut want = vec![
        "rust:module:e2e_crate".to_owned(),          // src/lib.rs — emitted ONCE
        "rust:module:e2e_crate.sub".to_owned(),      // src/sub.rs
        "rust:struct:e2e_crate.Widget".to_owned(),
        "rust:function:e2e_crate.make".to_owned(),
        "rust:function:e2e_crate.sub.helper".to_owned(),
        // Phase-1a ontology only: the impl method parents to the module here
        // (the impl ENTITY is Task 5; this snapshot GROWS then).
        "rust:function:e2e_crate.Widget.impl#<>.bump".to_owned(),
    ];
    want.sort();
    assert_eq!(got, want,
        "out-of-src tests/build.rs must NOT mint colliding/extra ids");
}
```
> Note: the exact `outcome` accessor shape (reading `runs.status` + the `entities` table) follows however `wp2_e2e.rs` reads its result; reuse that helper. The `Widget.impl#<>.bump` locator assumes Option (b) (ordinal dropped); if Task 5 lands as Option (a) it is `…impl#<>#0.bump` and this `want` row updates in Task 5.

- [ ] **Step 3: Run it — expect RED.** The plugin's `analyze_one_file` has no scope guard, so `tests/it.rs` and `build.rs` each mint a bare `rust:module:e2e_crate`; the writer's `ON CONFLICT(id) DO UPDATE` silently merges them and `test_only_helper`/`build.rs main` leak into the set.

Run: `cargo nextest run -p loomweave-plugin-rust --test analyze_e2e`
Expected: FAIL — the asserted id set does not match (extra/merged ids).

- [ ] **Step 4: Commit the red test.**
```bash
git add crates/loomweave-plugin-rust/tests/analyze_e2e.rs crates/loomweave-plugin-rust/tests/fixtures/e2e_crate
git commit -m "test(plugin-rust): front-loaded analyze->writer E2E exposes out-of-src module-id collision (RED)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Shared crate-scope helper (GREEN — closes the collision)

Extract the `src/`-only + redundant-`main.rs` discipline (today living only in `build_symbol_table`, `symbol_table.rs:68-87`) into a shared module, and make `analyze_one_file` honor it by returning **empty** output for out-of-scope files.

**Files:**
- Create: `crates/loomweave-plugin-rust/src/scope.rs`
- Modify: `crates/loomweave-plugin-rust/src/lib.rs` (add `pub mod scope;`)
- Modify: `crates/loomweave-plugin-rust/src/symbol_table.rs:61-103` (delegate to `scope.rs`)
- Modify: `crates/loomweave-plugin-rust/src/main.rs:140-172` (`analyze_one_file` consults `scope.rs`)

- [ ] **Step 1: Write the failing unit test** for the shared helper (`crates/loomweave-plugin-rust/src/scope.rs`, `#[cfg(test)] mod tests`):
```rust
#[test]
fn out_of_src_and_redundant_main_are_not_emittable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::create_dir_all(root.join("c/tests")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(root.join("c/src/main.rs"), "fn main() {}\n").unwrap();   // redundant (lib exists)
    std::fs::write(root.join("c/tests/it.rs"), "fn h() {}\n").unwrap();
    std::fs::write(root.join("c/build.rs"), "fn main() {}\n").unwrap();
    let roots = crate::crate_roots::discover_crate_roots(root);

    assert!(emittable_scope(&roots, &root.join("c/src/lib.rs")).is_some());
    assert_eq!(
        emittable_scope(&roots, &root.join("c/src/lib.rs")).unwrap(),
        ("c_crate".to_owned(), "c_crate".to_owned())
    );
    assert!(emittable_scope(&roots, &root.join("c/src/main.rs")).is_none(), "redundant main");
    assert!(emittable_scope(&roots, &root.join("c/tests/it.rs")).is_none(), "integration test");
    assert!(emittable_scope(&roots, &root.join("c/build.rs")).is_none(), "build script");
}
```

- [ ] **Step 2: Run it — expect FAIL** (`emittable_scope` undefined).
Run: `cargo nextest run -p loomweave-plugin-rust --test ... -- scope` (or the unit module)
Expected: FAIL to compile / unresolved `emittable_scope`.

- [ ] **Step 3: Implement `scope.rs`** by lifting the guard logic out of `symbol_table.rs`:
```rust
//! Shared crate-scope discipline (formerly inline in `symbol_table.rs`).
//! A file is *emittable* only when it is part of the library/binary crate the
//! ADR-049 qualname scheme names: under the crate's `src/` tree and not a
//! redundant `main.rs` shadowing a sibling `lib.rs`. `tests/`, `benches/`,
//! `examples/`, and `build.rs` are SEPARATE compilation units — folding them in
//! would mint colliding `rust:module:<crate>` locators (each one's bare-crate
//! fallback), a SILENT `ON CONFLICT` data loss at the storage writer.
use std::path::Path;
use crate::crate_roots::CrateRoots;
use crate::module_path::module_path_for;

/// `(crate_name, dotted_module_path)` for an emittable file, else `None`.
#[must_use]
pub fn emittable_scope(roots: &CrateRoots, file: &Path) -> Option<(String, String)> {
    let crate_name = roots.crate_name_for(file)?;
    let src_root = roots.crate_dir_for(file)?.join("src");
    if !file.starts_with(&src_root) {
        return None;                       // tests/ benches/ examples/ build.rs
    }
    if file == src_root.join("main.rs") && src_root.join("lib.rs").is_file() {
        return None;                       // redundant binary root; lib.rs is canonical
    }
    let module_path = module_path_for(&crate_name, &src_root, file);
    Some((crate_name, module_path))
}
```

- [ ] **Step 4: Refactor `build_symbol_table`** (`symbol_table.rs:61-103`) to use `emittable_scope` — replace the inline `src_root_of` / `file_is_redundant_main` / `module_path_for` block with a single `let Some((crate_name, module_path)) = emittable_scope(&roots, &file) else { continue };`. Delete the now-dead private helpers. The existing `symbol_table.rs` tests (`:159-244`) must stay green unchanged.

- [ ] **Step 5: Make `analyze_one_file` honor scope** (`main.rs:140-172`). Replace the unconditional crate/module derivation with:
```rust
let (crate_name, module_path) = match crate_roots.and_then(|r| {
    loomweave_plugin_rust::scope::emittable_scope(r, file)
}) {
    Some(cm) => cm,
    None => return (Vec::new(), Vec::new(), Vec::new()), // out-of-scope: emit nothing
};
```
> Removing the `stem`-fallback is intentional: a file the core hands us that resolves to no crate scope contributes nothing rather than a bare colliding module. (The degraded-parse fallback for *in-scope* unparseable files is unchanged — it still runs below for files that ARE emittable.)

- [ ] **Step 6: Run the Task-1 E2E — expect GREEN**, plus the full crate suite.
Run: `cargo nextest run -p loomweave-plugin-rust`
Expected: PASS — `analyze_e2e` set matches; `tests/it.rs`/`build.rs` contribute nothing.

- [ ] **Step 7: Commit.**
```bash
git add crates/loomweave-plugin-rust/src/scope.rs crates/loomweave-plugin-rust/src/lib.rs \
        crates/loomweave-plugin-rust/src/symbol_table.rs crates/loomweave-plugin-rust/src/main.rs
git commit -m "fix(plugin-rust): shared crate-scope guard on the emit path closes out-of-src module-id collision (GREEN)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: The six remaining leaf entity kinds

Add `enum`, `trait`, `type_alias`, `const`, `static`, `macro` as entities. Each is a free item riding the existing `free_item_qualname` + `entity()` + `push_with_contains` pattern (`extract.rs:210-251`) and must extend the cfg-twin key match (`extract.rs:188-197`). No new entity *shape*. (Trait/impl SEI **signatures** are Task 4; this task emits the entities with `None` signature for the kinds that don't yet have a builder, matching how 1a treated pre-signature kinds.)

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/extract.rs:188-197` (twin-count match) and `:210-291` (item match arms)
- Modify: `crates/loomweave-plugin-rust/plugin.toml:32` (entity_kinds) — ADR-027 MINOR bump
- Test: `crates/loomweave-plugin-rust/tests/leaf_kinds.rs`

- [ ] **Step 1: Write the failing test** (`tests/leaf_kinds.rs`):
```rust
use loomweave_plugin_rust::extract::extract_file;

fn ids(src: &str) -> Vec<String> {
    extract_file("k", "k.m", "/p/src/m.rs", src).unwrap()
        .iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn each_leaf_kind_is_emitted_with_its_kind_segment() {
    let src = "\
        pub enum E { A, B }\n\
        pub trait T { fn m(&self); }\n\
        pub type Alias = i32;\n\
        pub const C: i32 = 1;\n\
        pub static S: i32 = 1;\n\
        macro_rules! mac { () => {}; }\n";
    let got = ids(src);
    for want in [
        "rust:enum:k.m.E", "rust:trait:k.m.T", "rust:type_alias:k.m.Alias",
        "rust:const:k.m.C", "rust:static:k.m.S", "rust:macro:k.m.mac",
    ] {
        assert!(got.contains(&want.to_owned()), "missing {want}; got {got:?}");
    }
}

#[test]
fn cfg_twin_discriminant_is_item_general_for_leaf_kinds() {
    // Two cfg-gated enums of the same name must not collide.
    let got = ids("#[cfg(unix)] enum E {}\n#[cfg(windows)] enum E {}\n");
    let es: Vec<_> = got.iter().filter(|i| i.starts_with("rust:enum:")).collect();
    assert_eq!(es.len(), 2, "both cfg-twin enums emitted: {es:?}");
    assert_ne!(es[0], es[1], "cfg discriminant must separate them");
}
```

- [ ] **Step 2: Run — expect FAIL** (kinds not emitted).
Run: `cargo nextest run -p loomweave-plugin-rust --test leaf_kinds`
Expected: FAIL — ids absent.

- [ ] **Step 3: Extend the twin-count match** (`extract.rs:188-197`) to count the new kinds, and add their item arms (`extract.rs`, before the `_ => {}` at `:290`). Each arm mirrors the `struct` arm at `:230-251`. Use `syn` item types: `Item::Enum(ItemEnum{ident,attrs,..})`, `Item::Trait(ItemTrait{ident,attrs,..})`, `Item::Type(ItemType{ident,attrs,..})` → `type_alias`, `Item::Const(ItemConst{ident,attrs,..})`, `Item::Static(ItemStatic{ident,attrs,..})`, `Item::Macro(ItemMacro{mac,..})` (name from `mac.path` last segment; `macro_rules!` name is `ItemMacro.ident` in syn 2 — confirm via the type). Example (enum):
```rust
Item::Enum(ItemEnum { ident, attrs, .. }) => {
    let name = ident.to_string();
    let mut q = free_item_qualname(module_path, &name);
    if is_cfg_twin("enum", &name) && let Some(pred) = cfg_predicate(attrs) {
        q.push_str(&cfg_discriminant(&pred));
    }
    let child = entity("enum", &q, file_path, &source_range_of(item), Some(parent_id), None)?;
    push_with_contains(parent_id, child, out, edges);
}
```
Add the matching `("enum", ident.to_string())` (and the other five) to the twin-count `match` at `:188`. Update the `is_cfg_twin` callers accordingly.
> Decision (conscious, per review): trait **body** items (trait methods, associated consts/types) are **NOT** walked as entities in 1b — only the `trait` item itself. 1a likewise did not walk trait bodies. Defer to a later phase; record in §8 of the spec.

- [ ] **Step 4: Declare the kinds** in `plugin.toml` `entity_kinds` (add `enum, trait, type_alias, const, static, macro`). Bump the ontology version per ADR-027 (MINOR — additive kinds).

- [ ] **Step 5: Run — expect GREEN** + full suite + clippy.
Run: `cargo nextest run -p loomweave-plugin-rust && cargo clippy -p loomweave-plugin-rust --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit.**
```bash
git add crates/loomweave-plugin-rust/src/extract.rs crates/loomweave-plugin-rust/plugin.toml crates/loomweave-plugin-rust/tests/leaf_kinds.rs
git commit -m "feat(plugin-rust): emit enum/trait/type_alias/const/static/macro entities (Phase 1b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: SEI signatures for `trait` and `impl`

Spec §4.4 declares `trait{supertraits}` and `impl{target, trait}` SEI objects. Function/struct signatures already ship (`signature.rs:24,49`); reuse the `tidy()` helper and the `json!({"v":1, …})` shape.

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/signature.rs`
- Test: `crates/loomweave-plugin-rust/tests/sei_signatures.rs` (extend the existing file)

- [ ] **Step 1: Write the failing test** asserting the two new signature objects are stable and shaped per §4.4. (Mirror the existing function/struct cases in `sei_signatures.rs`.)
```rust
#[test]
fn trait_signature_lists_supertraits_sorted() {
    let it: syn::ItemTrait = syn::parse_str("trait T: Clone + std::fmt::Debug {}").unwrap();
    assert_eq!(trait_signature(&it), serde_json::json!({"v":1,"supertraits":["Clone","std::fmt::Debug"]}));
}
#[test]
fn impl_signature_carries_target_and_trait() {
    let it: syn::ItemImpl = syn::parse_str("impl std::fmt::Display for Foo {}").unwrap();
    assert_eq!(impl_signature(&it), serde_json::json!({"v":1,"target":"Foo","trait":"std::fmt::Display"}));
    let inh: syn::ItemImpl = syn::parse_str("impl Foo {}").unwrap();
    assert_eq!(impl_signature(&inh), serde_json::json!({"v":1,"target":"Foo","trait":serde_json::Value::Null}));
}
```

- [ ] **Step 2: Run — expect FAIL.** Run: `cargo nextest run -p loomweave-plugin-rust --test sei_signatures`. Expected: FAIL (undefined fns).

- [ ] **Step 3: Implement `trait_signature` / `impl_signature`** in `signature.rs`, reusing `tidy()` and `self_ty_name` (re-export or call `crate::qualname::self_ty_name`). Render supertraits via the bound paths, sorted for determinism; render the trait path of an impl via its `Path` tokens through `tidy()`.

- [ ] **Step 4: Run — expect GREEN.** Run: `cargo nextest run -p loomweave-plugin-rust --test sei_signatures`. Expected: PASS.

- [ ] **Step 5: Commit.**
```bash
git add crates/loomweave-plugin-rust/src/signature.rs crates/loomweave-plugin-rust/tests/sei_signatures.rs
git commit -m "feat(plugin-rust): trait/impl SEI signatures (spec 4.4)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: The `impl` entity + method re-parenting + ordinal-churn resolution (Option b)

> **⚠ Confirm the open decision first.** Written for **Option (b): merge** same-`(type, generic-sig, cfg)` inherent impls into one `impl` entity (drops the source-order ordinal → reorder-stable + method-set-stable). **Option (a)** alternative: keep `ImplDisc`'s ordinal as-is, emit the `impl` entity using the existing `impl_qualname(&type_q, &disc)`, skip Step 3's `qualname.rs` change, add a `KNOWN-LIMITATION` note that reordering same-signature inherent impls churns the `impl`-entity id, and soften ADR-049 §1's "source-order-independent" wording. Everything else in this task is identical.

The `impl` entity is the only structural change in 1b. It must atomically (a) emit the impl entity, (b) flip each method's `parent_id` from module→impl, (c) emit the method's `contains` edge as impl→method (not module→method), and (d) emit a module→impl `contains` edge — **all four**, or the writer's bidirectional `parent_contains_mismatch` (`writer.rs:1252-1314`) ROLLBACKs and FailRuns. Method *locators* do not churn (the discriminator is already folded in, `qualname.rs:108`); only `parent_id` (not the locator) changes.

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/qualname.rs:31-90,118-133` (drop the ordinal from `ImplDisc::inherent`)
- Modify: `crates/loomweave-plugin-rust/src/extract.rs:253-342` (`emit_impl_methods` → `emit_impl`: emit entity, merge, re-parent)
- Modify: `docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md` (§1 inherent-impl wording)
- Test: `crates/loomweave-plugin-rust/tests/impl_entity.rs`; extend `tests/identity_stability.rs`

- [ ] **Step 1: Write the failing tests.**
```rust
// tests/impl_entity.rs
use loomweave_plugin_rust::extract::extract_file_full;

#[test]
fn impl_entity_emitted_and_methods_reparent_to_it() {
    let x = extract_file_full("k", "k.m",
        "/p/src/m.rs", "struct Foo;\nimpl Foo { pub fn a(&self){} }\n").unwrap();
    let impl_id = "rust:impl:k.m.Foo.impl#<>";
    let method_id = "rust:function:k.m.Foo.impl#<>.a";
    let ids: Vec<_> = x.entities.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&impl_id), "impl entity present: {ids:?}");
    // method parents to the impl, not the module
    let m = x.entities.iter().find(|e| e["id"] == method_id).unwrap();
    assert_eq!(m["parent_id"], impl_id);
    // dual-encoding: module->impl AND impl->method contains both present
    let edges: Vec<(&str,&str)> = x.edges.iter()
        .filter(|e| e["kind"]=="contains")
        .map(|e| (e["from_id"].as_str().unwrap(), e["to_id"].as_str().unwrap())).collect();
    assert!(edges.contains(&("rust:module:k.m", impl_id)));
    assert!(edges.contains(&(impl_id, method_id)));
    // and NO leftover module->method edge (would dangle vs parent_id)
    assert!(!edges.contains(&("rust:module:k.m", method_id)));
}

#[test]
fn two_no_cfg_inherent_impls_merge_to_one_entity_and_are_reorder_stable() {
    let a = extract_file_full("k","k.m","/p/m.rs",
        "struct Foo;\nimpl Foo { fn a(&self){} }\nimpl Foo { fn b(&self){} }\n").unwrap();
    let b = extract_file_full("k","k.m","/p/m.rs",
        "struct Foo;\nimpl Foo { fn b(&self){} }\nimpl Foo { fn a(&self){} }\n").unwrap(); // reordered
    let impl_ids = |x: &loomweave_plugin_rust::extract::Extracted| {
        let mut v: Vec<String> = x.entities.iter()
            .filter(|e| e["kind"]=="impl").map(|e| e["id"].as_str().unwrap().to_owned()).collect();
        v.sort(); v
    };
    assert_eq!(impl_ids(&a), vec!["rust:impl:k.m.Foo.impl#<>".to_owned()]); // ONE entity, no #0/#1
    assert_eq!(impl_ids(&a), impl_ids(&b), "reordering source must not churn the impl id");
    // both methods present under the single impl
    let mids: std::collections::BTreeSet<_> = a.entities.iter()
        .filter(|e| e["kind"]=="function").map(|e| e["id"].as_str().unwrap()).collect();
    assert!(mids.contains("rust:function:k.m.Foo.impl#<>.a"));
    assert!(mids.contains("rust:function:k.m.Foo.impl#<>.b"));
}
```
Add to `tests/identity_stability.rs`: reorder two same-signature inherent impls → **no** id (impl or method) changes. (This replaces the 1a `inherent_impl_ordinal_is_load_bearing` premise; under Option (b) the ordinal is gone, so update/remove that uniqueness-corpus entry and its `identity_uniqueness.rs` assertions — the cfg-twin `go` family stays, separated by `@cfg`.)

- [ ] **Step 2: Run — expect FAIL** (`rust:impl:` entities absent; methods still parent to module).
Run: `cargo nextest run -p loomweave-plugin-rust --test impl_entity`
Expected: FAIL.

- [ ] **Step 3: Drop the ordinal from `ImplDisc`** (`qualname.rs`). `ImplDisc::inherent(generic_param_names: &[String])` (no `ordinal`); key becomes `format!("impl#<{positional_generics}>")` (no `#{ordinal}`). `impl_disc_for(it: &ItemImpl)` (no ordinal param). Update the `qualname.rs` unit tests accordingly (`:260-345`): `impl#<$0>#0` → `impl#<$0>`, etc.

- [ ] **Step 4: Rewrite `emit_impl_methods` → `emit_impl`** (`extract.rs:296-342`) to emit the entity, merge, and re-parent. Drive the inherent **cfg-twin** discriminant the same way free items do (count `(type_q, disc.key())` collisions in the item list; if >1 and cfgs differ, append `cfg_discriminant`). Maintain a `seen_impl_ids: &mut BTreeSet<String>` threaded through `walk_items` so a second source block with the same impl id does **not** re-emit the entity (merge), only appends its methods:
```rust
fn emit_impl(
    it: &ItemImpl, module_path: &str, module_id: &str, file_path: &str,
    impl_is_cfg_twin: &dyn Fn(&str) -> bool,
    seen_impl_ids: &mut std::collections::BTreeSet<String>,
    out: &mut Vec<Value>, edges: &mut Vec<Value>,
) -> Result<(), syn::Error> {
    let type_q = format!("{module_path}.{}", self_ty_name(&it.self_ty));
    let disc = impl_disc_for(it);                       // ordinal-free (Option b)
    let mut impl_q = impl_qualname(&type_q, &disc);
    if it.trait_.is_none() && impl_is_cfg_twin(&impl_q)
        && let Some(pred) = cfg_predicate(&it.attrs) {
        impl_q.push_str(&cfg_discriminant(&pred));
    }
    let impl_id = build_id("impl", &impl_q)?;
    if seen_impl_ids.insert(impl_id.clone()) {          // first block with this id → emit entity
        let e = entity("impl", &impl_q, file_path, &source_range_of(it),
            Some(module_id), Some(impl_signature(it)))?;
        edges.push(contains_edge(module_id, &impl_id)); // module -> impl
        out.push(e);
    }
    for member in &it.items {
        if let ImplItem::Fn(m) = member {
            let q = method_qualname(&type_q, &disc, &m.sig.ident.to_string());
            let child = entity("function", &q, file_path, &source_range_of(member),
                Some(&impl_id), Some(function_signature(&m.sig)))?;
            push_with_contains(&impl_id, child, out, edges);   // impl -> method
        }
    }
    Ok(())
}
```
Compute `impl_is_cfg_twin` once per item list (a closure over a `BTreeMap<String,usize>` counting impl-qualnames), and thread `seen_impl_ids` from `walk_items` (initialise empty per file, like `inherent_ordinals` was). Delete `inherent_ordinals`.
> The module docs at `extract.rs:9-19` ("methods parent to the module … the impl entity is Phase 1b") are now stale — update them to describe impl-parented methods.

- [ ] **Step 5: Amend ADR-049 §1.** Replace the "ordinal … assigned by source order" paragraph: inherent impls of the same `(type, positional-generic-signature, cfg)` are **one** `impl` entity (`…impl#<sig>`); cfg-twin inherent impls are split by the `@cfg(...)` discriminant; there is no source-order ordinal, so the discriminator is genuinely source-order-independent.

- [ ] **Step 6: Update the Task-1 E2E `want` set** — the impl method row stays `…Foo.impl#<>.bump`, and add `"rust:impl:e2e_crate.Widget.impl#<>"`.

- [ ] **Step 7: Run — expect GREEN** + full suite (esp. `identity_uniqueness`, `identity_stability`, `analyze_e2e`, `entity_id` parity fixture rows that mention the ordinal — update those fixture rows).
Run: `cargo nextest run -p loomweave-plugin-rust`
Expected: PASS.

- [ ] **Step 8: Commit.**
```bash
git add -A crates/loomweave-plugin-rust docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md
git commit -m "feat(plugin-rust): impl entity + method re-parenting; merge inherent impls to kill source-order ordinal churn (ADR-049 amend)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Symbol-table reverse index + the path resolver (the genuinely-new subsystem)

Add a `qualname→id` inverse to `SymbolTable` and a resolver that turns a `use`/trait path into `Resolution::{Resolved(id), Ambiguous, External}` — Resolved only when the path resolves to **exactly one** in-project entity; globs/aliases/re-export ambiguity → `Ambiguous`; out-of-project → `External` (H5; never a faked Resolved).

**Files:**
- Create: `crates/loomweave-plugin-rust/src/resolve.rs`
- Modify: `crates/loomweave-plugin-rust/src/symbol_table.rs` (store + expose `by_qualname`)
- Modify: `crates/loomweave-plugin-rust/src/lib.rs` (`pub mod resolve;`)
- Test: `crates/loomweave-plugin-rust/tests/resolve.rs`

- [ ] **Step 1: Write the failing test.**
```rust
use loomweave_plugin_rust::symbol_table::build_symbol_table;
use loomweave_plugin_rust::resolve::{Resolver, Resolution};

#[test]
fn resolves_unique_inproject_path_else_ambiguous_or_external() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub mod a;\npub trait Tr {}\n").unwrap();
    std::fs::write(root.join("c/src/a.rs"), "pub struct S;\n").unwrap();
    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // unique in-project struct -> Resolved
    assert_eq!(r.resolve_use_path("c_crate", "c_crate::a::S"),
               Resolution::Resolved("rust:struct:c_crate.a.S".to_owned()));
    // in-project trait -> Resolved (implements/imports share this)
    assert_eq!(r.resolve_trait_path("c_crate", "Tr"),
               Resolution::Resolved("rust:trait:c_crate.Tr".to_owned()));
    // glob -> Ambiguous (never faked Resolved, H5)
    assert_eq!(r.resolve_use_path("c_crate", "c_crate::a::*"), Resolution::Ambiguous);
    // external -> External (Task 7/8 drop it per D1)
    assert_eq!(r.resolve_use_path("c_crate", "serde::Serialize"), Resolution::External);
}
```

- [ ] **Step 2: Run — expect FAIL** (module/types absent). Run: `cargo nextest run -p loomweave-plugin-rust --test resolve`. Expected: FAIL.

- [ ] **Step 3: Add the reverse index to `SymbolTable`.** Keep the existing `by_id` (id→qualname); add `by_qualname: BTreeMap<String, Vec<String>>` (qualname→ids, `Vec` because one qualname can map to multiple kinds, e.g. a `struct S` and a `fn S` — kind is in the id, not the qualname). Populate it in `build_symbol_table`'s insert loop. Expose `pub fn ids_for_qualname(&self, q: &str) -> &[String]` and `pub fn iter_ids(&self) -> impl Iterator<Item=&str>`.

- [ ] **Step 4: Implement `resolve.rs`.**
```rust
//! Phase 1b path resolver. Turns a `use`/trait path into a unique in-project
//! entity id, else Ambiguous (globs/aliases/re-exports — H5) or External
//! (out of project). NEVER fabricates a Resolved target; the host-side
//! seen-entity-set gate (Task 8) is the second line that drops anything the
//! host did not actually store.
use crate::symbol_table::SymbolTable;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution { Resolved(String), Ambiguous, External }

pub struct Resolver<'t> { table: &'t SymbolTable }

impl<'t> Resolver<'t> {
    #[must_use] pub fn new(table: &'t SymbolTable) -> Self { Self { table } }

    /// Resolve a `use`/path string from `from_crate`. Glob/alias → Ambiguous;
    /// a path whose leading segment names no in-project crate → External; a
    /// path resolving to exactly one in-project id → Resolved.
    #[must_use]
    pub fn resolve_use_path(&self, from_crate: &str, path: &str) -> Resolution {
        if path.ends_with("::*") { return Resolution::Ambiguous; }
        let dotted = normalize_path(from_crate, path);      // crate::/self::/super:: -> dotted crate-rooted
        match self.table.ids_for_qualname(&dotted) {
            []  => Resolution::External,
            [one] => Resolution::Resolved(one.clone()),
            _   => Resolution::Ambiguous,                    // same qualname, >1 kind
        }
    }
    #[must_use]
    pub fn resolve_trait_path(&self, from_crate: &str, path: &str) -> Resolution {
        // trait paths resolve the same way but filter to rust:trait: ids.
        let dotted = normalize_path(from_crate, path);
        let traits: Vec<&String> = self.table.ids_for_qualname(&dotted)
            .iter().filter(|id| id.starts_with("rust:trait:")).collect();
        match traits.as_slice() {
            []  => Resolution::External,
            [one] => Resolution::Resolved((*one).clone()),
            _   => Resolution::Ambiguous,
        }
    }
}

/// `crate::a::B` / `self::B` / `super::B` / `c_crate::a::B` → `c_crate.a.B`.
/// A leading segment that is not `crate`/`self`/`super`/`from_crate` and not a
/// known in-project crate stays as-is (the caller's table lookup then misses →
/// External). Aliases (`use a::B as C`) are handled by the caller (it passes the
/// real path, not the alias).
fn normalize_path(from_crate: &str, path: &str) -> String {
    let segs: Vec<&str> = path.split("::").collect();
    let mut out: Vec<String> = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        match (i, *s) {
            (0, "crate" | "self") => out.push(from_crate.to_owned()),
            (0, _) => out.push((*s).to_owned()),
            (_, "super") => { /* pop handled by caller's module ctx in 1b-min: treat as crate-root */ }
            _ => out.push((*s).to_owned()),
        }
    }
    out.join(".")
}
```
> `super::` full handling needs the *defining module path* of the `use`; for 1b-minimal, resolve `super::` conservatively (a miss → External or Ambiguous is acceptable, never a wrong Resolved). Cover a `super::` case in the test asserting it does NOT produce a wrong Resolved.

- [ ] **Step 5: Run — expect GREEN.** Run: `cargo nextest run -p loomweave-plugin-rust --test resolve`. Expected: PASS.

- [ ] **Step 6: Commit.**
```bash
git add crates/loomweave-plugin-rust/src/resolve.rs crates/loomweave-plugin-rust/src/symbol_table.rs crates/loomweave-plugin-rust/src/lib.rs crates/loomweave-plugin-rust/tests/resolve.rs
git commit -m "feat(plugin-rust): symbol-table reverse index + path resolver (Resolved/Ambiguous/External)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Thread the symbol table into `analyze_file` and emit `imports`

The table is built at `initialize` then dropped (`main.rs:86-88` calls only `.len()`). Hoist it (like `crate_roots`, `main.rs:38`) and thread it into extraction so `imports` can resolve. `imports` is the lowest-friction resolving edge: its writer kind already exists (anchored, `writer.rs:702`), its external-drop filter already exists (`analyze.rs:4713`), and the deferred channel already exists — the Python plugin rides them. No core change.

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/main.rs` (stash + thread the `SymbolTable`)
- Create: `crates/loomweave-plugin-rust/src/edges.rs` (`use`-statement → edge)
- Modify: `crates/loomweave-plugin-rust/src/extract.rs` (collect `use` items; build imports via `edges.rs` + a `Resolver`)
- Test: `crates/loomweave-plugin-rust/tests/imports_edges.rs`

- [ ] **Step 1: Write the failing test** (`tests/imports_edges.rs`): an in-project `use c_crate::a::S;` yields a `Resolved` `imports` edge to `rust:struct:c_crate.a.S`; `use c_crate::a::*;` yields an `Ambiguous` imports edge; `use serde::Serialize;` yields **no** edge (External dropped per D1). Drive through `extract_file_full` with a `Resolver` argument (new signature) so the test exercises real resolution.

- [ ] **Step 2: Run — expect FAIL.** Run: `cargo nextest run -p loomweave-plugin-rust --test imports_edges`. Expected: FAIL.

- [ ] **Step 3: Add an edges-aware extraction entry point.** Add `extract_file_with_edges(crate_name, module_path, file_path, src, resolver: &Resolver) -> Result<Extracted, syn::Error>` (the existing `extract_file_full` stays, calling the new one with a no-op resolver, so the identity/uniqueness/symbol-table callers are unaffected). In `walk_items`, collect `Item::Use` items, expand the use-tree to leaf paths, and for each call `resolver.resolve_use_path` → build an `imports` edge via `edges.rs` for `Resolved`/`Ambiguous`, drop `External`. The `imports` edge is **anchored** (carries the `use` statement's byte span) — capture `source_byte_start/end` from the `Item::Use` span (`source_range_of`).
> `imports` confidence: `Resolved` carries the resolved `to_id`; `Ambiguous` carries a best-effort candidate `to_id` (a real in-project id — never null) per the existing Ambiguous contract. A glob with no single candidate but a resolvable module prefix points the Ambiguous edge at the module id.

- [ ] **Step 4: Thread the table** in `main.rs`: keep the `SymbolTable` in an `Option<SymbolTable>` stashed at `initialize` (alongside `crate_roots`), and in `analyze_one_file` build a `Resolver::new(&table)` and call `extract_file_with_edges`. (`extract_file_degraded_aware` gains the resolver param too.)

- [ ] **Step 5: Run — expect GREEN.** Run: `cargo nextest run -p loomweave-plugin-rust`. Expected: PASS.

- [ ] **Step 6: Commit.**
```bash
git add crates/loomweave-plugin-rust/src/{main,edges,extract}.rs crates/loomweave-plugin-rust/tests/imports_edges.rs
git commit -m "feat(plugin-rust): thread symbol table into analyze_file; emit imports edges (Resolved/Ambiguous, external dropped)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: `implements` edge + the host seen-entity-set gate (D1+D2+D3)

`impl Trait for Type` → trait. The highest-value Rust edge. This task (a) mints `implements` in the host's anchored-edge ontology atomically with the manifest declaration, (b) adds byte-offset plumbing (the **first** anchored edge this plugin emits — `contains` is structural/null-offset), and (c) generalizes the host's imports-only external-filter to `implements`, which is the **single mechanism** that closes D1 (external), D2 (gitignored superset), and D3 (staleness): a target the host did not store is dropped, never written as a dangling FK.

**Files:**
- Modify: `crates/loomweave-core/src/plugin/...` — add `"implements"` to `ANCHORED_EDGE_KINDS` (`writer.rs:699-705`); **no SQL migration** (schema has no CHECK on `edges.kind`, schema line 83).
- Modify: `crates/loomweave-core/src/analyze.rs:4713` — generalize the external/seen-set filter from imports-only to also cover `implements`.
- Modify: `crates/loomweave-plugin-rust/plugin.toml` — add `implements` (and `imports`) to `edge_kinds` (atomic with the writer const — manifest-only → host silently drops with `undeclared_edge_kind`, `host.rs:1183`; writer-only → manifest validator rejects).
- Modify: `crates/loomweave-plugin-rust/src/{extract,edges}.rs` — emit `implements` from `Item::Impl` with `it.trait_`.
- Test: `crates/loomweave-plugin-rust/tests/implements_edges.rs`; extend `analyze_e2e.rs`.

- [ ] **Step 1: Write the failing tests.** Unit: `impl Tr for Foo` where `Tr` is in-project → a `Resolved` `implements` edge `rust:impl:…Foo.impl[Tr]` → `rust:trait:…Tr`, carrying non-null `source_byte_start/end` (the trait-path span). `impl std::fmt::Display for Foo` → **no** edge (External, dropped). E2E: add a trait + an in-project impl of it to the fixture and assert the stored edge set includes the `implements` row and that an external-trait impl produces no edge and the run still **completes** (the seen-set gate dropped it, no FK-fail).

- [ ] **Step 2: Run — expect FAIL** (unknown edge kind → `LMWV-INFRA-EDGE-UNKNOWN-KIND`, or missing edge). Run: `cargo nextest run -p loomweave-plugin-rust --test implements_edges`. Expected: FAIL.

- [ ] **Step 3: Mint the kind + the gate (core).** Add `"implements"` to the anchored-edge const (`writer.rs`). At `analyze.rs:4713`, change the imports-only external filter to a predicate over `{imports, implements}` (and structure it so future resolving edges opt in): for an anchored edge whose `to_id` is not in the run's seen-entity set, **drop it with a counted diagnostic** rather than letting it reach the unconditional force-flush (`analyze.rs:879-882`). Also add the **counted end-of-loop reconciliation** of `pending_plugin_edges` so a legitimate late-seen in-project target is flushed, and a never-seen one is dropped-and-counted (not silently lost — Survey #3 hazard).

- [ ] **Step 4: Emit `implements` (plugin).** In `emit_impl` (Task 5), when `it.trait_` is `Some((_, path, _))`, resolve via `resolver.resolve_trait_path` and build an anchored `implements` edge (from = impl entity id, to = resolved trait id) with the **trait path's byte span** as `source_byte_start/end`. Drop `External`; `Ambiguous` carries a best-effort in-project candidate.

- [ ] **Step 5: Declare `imports`/`implements`** in `plugin.toml` `edge_kinds` (atomic with Step 3).

- [ ] **Step 6: Run — expect GREEN** + the **core** suite (the writer/analyze change touches shared code — run `cargo nextest run -p loomweave-core -p loomweave-storage` too).
Run: `cargo nextest run -p loomweave-plugin-rust -p loomweave-core -p loomweave-storage`
Expected: PASS.

- [ ] **Step 7: Commit.**
```bash
git add crates/loomweave-core/src/plugin/ crates/loomweave-core/src/analyze.rs \
        crates/loomweave-plugin-rust/src/{extract,edges}.rs crates/loomweave-plugin-rust/plugin.toml \
        crates/loomweave-plugin-rust/tests/implements_edges.rs crates/loomweave-plugin-rust/tests/analyze_e2e.rs
git commit -m "feat(plugin-rust): implements edge + host seen-entity-set gate closing external/gitignored/stale targets (D1+D2+D3)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Plugin init-walk symlink/cycle safety (landmine #3, plugin-side)

Now that the symbol table is load-bearing, harden its walk: do not follow directory symlinks (out-of-tree reads + symlink-cycle stack crash). The host's path-jail (`jail.rs`) protects `analyze_file` paths but **not** the plugin's own init walk.

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/symbol_table.rs:130-144` (`walk`)
- Test: `crates/loomweave-plugin-rust/src/symbol_table.rs` `#[cfg(test)]` (gate on `#[cfg(unix)]` for symlink creation)

- [ ] **Step 1: Write the failing test** (`#[cfg(unix)]`): create `root/src/lib.rs` plus a directory symlink `root/loop -> root` (a cycle) and a symlink `root/escape -> /tmp/outside`; assert `build_symbol_table(root)` returns (does not hang/overflow) and contains no entity whose `file_path` is outside `root`.

- [ ] **Step 2: Run — expect FAIL/HANG.** Run: `cargo nextest run -p loomweave-plugin-rust -- symlink`. Expected: FAIL (or timeout — set a generous nextest slow-timeout so a regression is visible, not infinite).

- [ ] **Step 3: Fix `walk`** to skip symlinked directories: use `entry.file_type()` and `if file_type.is_symlink() { continue; }` before the `is_dir()` recursion. (Do not canonicalize-and-contain here — simply not following symlinked dirs both prevents escape and breaks cycles, and matches "the crate source tree is real directories.")

- [ ] **Step 4: Run — expect GREEN.** Run: `cargo nextest run -p loomweave-plugin-rust`. Expected: PASS.

- [ ] **Step 5: Commit.**
```bash
git add crates/loomweave-plugin-rust/src/symbol_table.rs
git commit -m "fix(plugin-rust): init walk skips symlinked dirs (no out-of-tree reads / cycle crash)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Doc-truth corrections (landmine #4)

Make the docs honest about what is implemented.

**Files:**
- Modify: `crates/loomweave-plugin-rust/Cargo.toml:12-19` (the comment claims `loomweave install` stages the plugin — it does not yet)
- Modify: `docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md:§1` (claims `#[path="…"]` is "honored" — it is deferred)
- Modify: `docs/superpowers/specs/2026-06-08-rust-language-plugin-design.md:§8` (record: trait bodies not walked; `#[path]` deferred; host rlimit hardening tracked separately)

- [ ] **Step 1:** Edit the Cargo.toml comment to state the binary is off-glob and that **automated tests self-stage it** (host_integration / analyze_e2e), and that **live `loomweave install` plugin staging is a separate deferred follow-up** (not yet implemented in `install.rs`).
- [ ] **Step 2:** Edit ADR-049 §1's module-path paragraph: `#[path="…"]` overrides are **deferred** (0 occurrences in the dogfood corpus); the resolver treats a `#[path]`-relocated module by its default file path until implemented.
- [ ] **Step 3:** No tests; run `RUSTDOCFLAGS="-D warnings" cargo doc -p loomweave-plugin-rust --no-deps` to confirm doc builds.
- [ ] **Step 4: Commit.**
```bash
git add crates/loomweave-plugin-rust/Cargo.toml docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md docs/superpowers/specs/2026-06-08-rust-language-plugin-design.md
git commit -m "docs(plugin-rust): correct install-staging + #[path] overstatements; record 1b deferrals

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: Grow the E2E to the full ontology + the Phase-1b exit gate

Bring the front-loaded E2E to its end-state (all 10 kinds + `contains`/`imports`/`implements`) and re-prove the identity gate over Loomweave's own `crates/`.

**Files:**
- Modify: `crates/loomweave-plugin-rust/tests/analyze_e2e.rs` + the fixture crate (add an enum/trait/type_alias/const/static/macro, an in-project `use`, an in-project trait impl)
- Reference: the 1a dogfood gate (`tests/` symbol-table uniqueness over `crates/`, ~2,836 entities)

- [ ] **Step 1: Extend the fixture + `want` set** to cover every entity kind and an `imports` + `implements` edge; keep the snapshot **set-based / `ORDER BY id`** (the host's `collect_source_files` walk is unsorted `readdir` — an order-sensitive snapshot flakes across filesystems).
- [ ] **Step 2: Run the full E2E** and assert run status `completed` + exact id set + expected edge set.
Run: `cargo nextest run -p loomweave-plugin-rust --test analyze_e2e`
Expected: PASS.
- [ ] **Step 3: Re-run the 1a identity gate** over `crates/` (the symbol-table uniqueness test) — must stay zero-collision after the ordinal change.
Run: `cargo nextest run -p loomweave-plugin-rust` (the gate test)
Expected: PASS — 0 duplicate locators.
- [ ] **Step 4: Run the whole CLAUDE.md CI floor.**
Run:
```bash
cargo fmt --all -- --check && \
cargo clippy --workspace --all-targets --all-features -- -D warnings && \
cargo build --workspace --bins && \
cargo nextest run && \
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps && \
cargo deny check
```
Expected: all green.
- [ ] **Step 5: Commit.**
```bash
git add crates/loomweave-plugin-rust/tests/analyze_e2e.rs crates/loomweave-plugin-rust/tests/fixtures
git commit -m "test(plugin-rust): Phase 1b exit gate — full-ontology analyze->writer E2E + zero-collision over crates/

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase 1b exit gate (definition of done)

1. The identity stability+uniqueness gate is **still green** over Loomweave's own `crates/` after the ordinal change (Task 11 Step 3).
2. A full `analyze`→writer **E2E completes** (run status `completed`, never `failed`) over a vendored multi-file fixture, asserting the exact entity-id **set** *and* edge set with a set-based / `ORDER BY id` snapshot (Task 11 Step 2).
3. All Phase-1b work clears the CLAUDE.md CI floor (Task 11 Step 4).
4. Merge `feat/rust-plugin-spec` → `rc3` (per the always-merge-to-working-release constraint), once the user confirms.

## Out of scope (tracked separately — do NOT fold in)
- **Host `RLIMIT_STACK`/`RLIMIT_CPU` + syn recursion-depth guard** (landmine #3, host side): protects *all* plugins; a host-hardening ticket, not Rust-plugin work. Uses the `pre_exec`/`setrlimit` `unsafe` carve-out (SAFETY-commented).
- **Live `loomweave install` plugin staging** (`install.rs`): the automated E2E self-stages; the *live* "install it and dogfood it" path needs install-side staging — a cross-cutting follow-up.
- **`derives` edge**: slipped to Phase 2 per D1 (near-zero in-project signal; nearly all derive targets are std/external).
- **`calls` / `references`**: Phase 2.
- **Trait-body items** (trait methods / associated consts) as entities: a later phase.
- **`#[path="…"]` module overrides**: deferred (0 uses in the corpus).
- **M1 subsystem attribution / M2 `entity_at` tiebreak**: documented known-limitations; degrade query results, do not FailRun — out of 1b.

---

## Self-Review

**Spec coverage:** §3.1 entity kinds — all 10 (3 in 1a + 6 leaf in Task 3 + impl entity in Task 5). §3.2 edges — `imports` (Task 7), `implements` (Task 8); `derives` consciously slipped (D1); `calls`/`references` Phase 2. §6 Phase-1b scope — covered, with `contains`/SEI(fn,struct) correctly NOT re-scoped (shipped 1a). §7 testing — golden-snapshot set-based E2E (Tasks 1/11), per-edge unit suites, identity gate re-run. §8 open decisions — D1/D2/D3 resolved; `#[path]` deferred. ✅
**Placeholder scan:** no TBD/"add error handling"/"similar to Task N"; the one open decision (ordinal) is explicitly flagged with both branches specified, not left blank. ✅
**Type consistency:** `emittable_scope` (Task 2) consumed by `analyze_one_file` (Task 2) and `build_symbol_table` (Task 2); `Resolver`/`Resolution` (Task 6) consumed by `imports` (Task 7) and `implements` (Task 8); `ImplDisc::inherent` ordinal-drop (Task 5) consistent with the `impl#<>` locators asserted in Tasks 1/5/11; `extract_file_full` kept intact for identity/uniqueness callers while `extract_file_with_edges` adds the resolver path (Task 7). ✅
