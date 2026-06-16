# Rust Plugin Phase 2 Edge Completeness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the Rust plugin's Phase 2 edge surface: anchored `derives` and `references` edges with honest Resolved/Ambiguous discipline, plus a test-pinned audit of the `calls` MVP resolution envelope.

**Architecture:** Two new edge kinds ride the existing extract → resolve → emit pipeline (`extract.rs` walk supplies sites, `resolve.rs` adjudicates, `edges.rs` shapes the wire JSON, `serve.rs` batches). `derives` parses `#[derive(...)]` attribute paths on structs/enums and resolves them like `implements` (trait-filtered). `references` adds a type-position walker plus a body expression-path visitor, resolving kind-unfiltered, dropping-and-counting external/unresolved sites exactly as the Python plugin does. Storage learns `derives` as the 10th ontology kind; ontology_version bumps 0.4.0 → 0.5.0.

**Tech Stack:** Rust (syn 2.x visitor APIs, edition 2024), existing loomweave-plugin-rust test layout, in-test fixtures.

**Ticket tree:** parent `clarion-1bc661a122`; subtasks `clarion-db12ef69cf` (derives), `clarion-973cfe8047` (references), `clarion-bfb3e2be49` (calls audit).

---

## Design decisions (settled 2026-06-10, evidence in fact-finding)

| # | Decision | Call | Why |
|---|----------|------|-----|
| D1 | derives external targets | **Keep Phase-1b D1 drop-externals.** In-project trait targets only, via `resolve_trait_path`. Sparseness quantified by dogfood counts in the report. | Spec open Q1 was already resolved to D1 (spec L413). An external-ref representation is a storage/ontology change with cross-plugin blast radius — its own ADR if ever wanted. |
| D2 | derives edge shape | from = deriving struct/enum entity; to = resolved trait; anchored at the individual derive-path token span; confidence resolved/ambiguous; External dropped at emit (no counter — matches `implements`). Structs + enums only (`Item::Union` is not in the walk). | Spec row: "same resolution as `implements`", "captures the invocation". ADR-026 decision 3: the token IS the edge. |
| D3 | references envelope | **IN:** type-position paths — struct/enum(+variant) field types, fn param/return types, type-alias RHS, const/static declared types — recursing into nested generic args (`Vec<MyType>` mints a site for `MyType`); body/initializer expression paths — `Expr::Path` NOT in call-callee position, `Expr::Struct` literal paths. **OUT:** `use` (imports owns it), call callees (calls channel owns them), derive lists (derives), impl-header trait + self_ty (implements / impl entity), generic params, bounds, where-clauses, trait item bodies (never walked, same as calls), macro arguments (spec §5), `Self`/`self` keywords. | Python parity where it translates (Name-in-Load-context ≈ non-callee path mention; annotation ≈ type position). Bounded, testable envelope; each OUT is either owned by another edge kind or invisible to syn. |
| D4 | unresolved references | Drop + count. Populate existing `AnalyzeFileStats` fields: `reference_sites_total`, `references_resolved_total` (≥1 in-project candidate, i.e. resolved+ambiguous), `references_skipped_external_total` (every `Resolution::External` outcome — syn/resolver cannot distinguish "external crate" from "no match", so this counter absorbs both). `unresolved_reference_sites_total` and `references_skipped_cap_total` stay 0 for Rust — pyright reports externality and needs a cost cap, syn does neither; divergences documented here and in the module doc. **No** new unresolved channel; **no** UnresolvedCallSite extension (it is calls-specific per protocol.rs:437). | Python plugin drops-and-counts (pyright_session.py:556-561); consistency wins. The stats fields already exist plugin-agnostically (protocol.rs:368-389). |
| D5 | references confidence | resolved / ambiguous only. Never inferred — writer rejects scan-time inferred on anchored kinds (`LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT`, writer.rs:856). The spec table's "Ambiguous + Inferred" for references refers to the query-time MCP inference tier (ADR-028), not plugin emission. | Hard storage contract; Python emits resolved/ambiguous only. |
| D6 | ontology bump | `plugin.toml`: `edge_kinds += ["derives", "references"]`, `ontology_version = "0.5.0"`; `serve.rs:113` handshake string lockstep; wheel copy `packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml` re-synced byte-identically (guard: `scripts/check-rust-plugin-manifest-lockstep.py`). Writer: `ANCHORED_EDGE_KINDS += "derives"` (comment 9→10 kinds; `references` already present). ADR-026 per-kind table gains a `derives` row (MUST be Some — light amendment; `decorates`/`inherits_from` were pre-listed the same way). No new ADR (`implements` precedent, c970255). | Additive edge kinds = MINOR per ADR-027. Host validates emitted kinds against manifest edge_kinds (host.rs:1005); writer strictly rejects unknown kinds. |
| D7 | calls audit | Test-pin every boundary: path-call resolved / ambiguous(multi-candidate) / external→unresolved-site; method call→unresolved-site `.name`; non-path callee→`<expr>()`; UFCS/qself behavior pinned as-is; closure + nested-fn attribution to enclosing named fn; trait-default bodies not walked; per-caller ordinal monotonic across mixed resolved/unresolved sites in source order; `unresolved_call_sites_total == unresolved_call_sites.len()`. Cheap defects fixed in-sprint; structural gaps filed with dependency. | Prompt requirement; the envelope is currently documented only in comments. |
| D8 | host-side | No host changes expected: seen-entity-set gate is kind-generic (analyze.rs:4238-4253), import filter is imports-only (analyze.rs:5003), schema edge kind is free text (ADR-031). Add a unit test proving `derives`/`references` kinds ride the gate. | Fact-finding verified all three. |
| D9 | frozen contracts | No qualname changes, no entity-set changes, no entity_id.rs / storage/sei.rs changes, no workspace version bump. Edges only. | ADR-049 freeze; cross-repo Wardline contract. |

## File structure

- Modify: `crates/loomweave-storage/src/writer.rs` (ANCHORED_EDGE_KINDS + comment)
- Modify: `crates/loomweave-storage/tests/writer_actor.rs` (known-kinds test)
- Modify: `crates/loomweave-plugin-rust/plugin.toml` + `packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml` (identical)
- Modify: `crates/loomweave-plugin-rust/src/serve.rs` (handshake version; stats wiring)
- Modify: `crates/loomweave-plugin-rust/src/edges.rs` (derives_edge, references_edge helpers)
- Create: `crates/loomweave-plugin-rust/src/derives.rs` (attribute parsing → sites)
- Create: `crates/loomweave-plugin-rust/src/references.rs` (type-position + expression-path visitors → sites)
- Modify: `crates/loomweave-plugin-rust/src/extract.rs` (wire both into the item walk; thread reference stats)
- Modify: `crates/loomweave-plugin-rust/src/lib.rs` (module decls)
- Create: `crates/loomweave-plugin-rust/tests/derives_edges.rs`
- Create: `crates/loomweave-plugin-rust/tests/references_edges.rs`
- Modify: `crates/loomweave-plugin-rust/tests/calls_edges.rs` (audit boundary tests)
- Modify: `crates/loomweave-plugin-rust/tests/analyze_e2e.rs` (edge-SET assertion incl. new kinds)
- Modify: `crates/loomweave-cli/src/analyze.rs` tests (gate generality unit test)
- Modify: `docs/loomweave/adr/ADR-026-containment-wire-and-edge-identity.md` (derives row)

## Execution notes for workers

- Run everything from the worktree root `/home/john/loomweave/.claude/worktrees/rust-plugin-edges`.
- `cargo build --workspace --bins` BEFORE `cargo nextest run` whenever e2e/integration tests are involved (stale-binary hazard).
- Clippy is pedantic `-D warnings`; `unsafe_code = "deny"` (nothing here needs unsafe).
- Never include `.agents/skills/loomweave-workflow/SKILL.md` in commits.
- Edge tests assert the emitted edge SET, never just presence — silent `ON CONFLICT` merge is the historical failure mode.

---

### Task 1: Storage learns `derives` (writer ontology)

**Files:** Modify `crates/loomweave-storage/src/writer.rs`, `crates/loomweave-storage/tests/writer_actor.rs`

- [ ] **Step 1: Red test** — in `writer_actor.rs`, find the known-kinds test (~:1634) and the existing anchored-contract tests; add a test inserting a `derives` edge with confidence=resolved and byte range via the writer path used by sibling tests, asserting acceptance, plus assert `known_scan_time_edge_kinds()` contains `"derives"`. Run: `cargo nextest run -p loomweave-storage derives` → expect FAIL (`LMWV-INFRA-EDGE-UNKNOWN-KIND`).
- [ ] **Step 2: Implement** — writer.rs: add `"derives"` to `ANCHORED_EDGE_KINDS`; update the `/// 9 ontology-defined edge kinds` comment to 10.
- [ ] **Step 3: Green** — re-run the filter; then `cargo nextest run -p loomweave-storage` full crate.
- [ ] **Step 4: ADR-026 row** — add `| derives | MUST be Some (the derived-trait path token inside #[derive(...)]) |` to the per-kind table; one context sentence following the existing style. No status change to the ADR.
- [ ] **Step 5: Commit** — `feat(storage): derives joins the anchored edge ontology (10 kinds)`

### Task 2: Manifest + handshake ontology bump

**Files:** Modify `crates/loomweave-plugin-rust/plugin.toml`, `packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml`, `crates/loomweave-plugin-rust/src/serve.rs:113`

- [ ] **Step 1:** plugin.toml: `edge_kinds = ["contains", "imports", "implements", "calls", "derives", "references"]`; `ontology_version = "0.5.0"`; update its adjacent comment (additive MINOR, this sprint).
- [ ] **Step 2:** serve.rs initialize handler: `ontology_version: "0.5.0".to_owned()`.
- [ ] **Step 3:** `cp crates/loomweave-plugin-rust/plugin.toml packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml`
- [ ] **Step 4:** `python3 scripts/check-rust-plugin-manifest-lockstep.py` → exit 0. Any serve.rs test pinning "0.4.0" updated in the same commit.
- [ ] **Step 5: Commit** — `feat(plugin-rust): declare derives+references edge kinds, ontology 0.5.0`

### Task 3: `derives` extraction

**Files:** Create `crates/loomweave-plugin-rust/src/derives.rs`, `tests/derives_edges.rs`; Modify `src/edges.rs`, `src/extract.rs`, `src/lib.rs`

- [ ] **Step 1: Red tests** in `tests/derives_edges.rs` (mirror `implements_edges.rs` harness — `extract_file_with_edges` style, in-test source strings):
  - in-project derive resolves: crate defines `trait Pretty` + `#[derive(Pretty)] struct Foo;` → exactly one `derives` edge, from Foo's struct id to the trait id, confidence `"resolved"`, byte span == the `Pretty` token inside the attribute.
  - external derives dropped: `#[derive(Debug, Clone)] struct Bar;` → zero `derives` edges.
  - mixed list: `#[derive(Debug, Pretty)]` → exactly one edge (Pretty), span on the second path.
  - enum target: `#[derive(Pretty)] enum E { A }` → edge from enum id.
  - ambiguous: two `Pretty` traits in cfg-gated twin modules (or two modules) sharing a qualname-colliding short path imported gluelessly is hard to stage; instead use two traits with the same name in two modules + a bare `#[derive(Pretty)]` where the resolver's crate-prefix fallback finds >1 — if unstageable with the real resolver semantics, assert the External path instead and note it. Follow `implements_edges.rs`'s existing ambiguity staging if present.
  - non-derive attributes ignored: `#[serde(rename_all = "lowercase")]`, `#[cfg(test)]` mint nothing.
- [ ] **Step 2:** Run `cargo nextest run -p loomweave-plugin-rust derives` → FAIL (module missing).
- [ ] **Step 3: Implement** `src/derives.rs`:

```rust
//! `#[derive(...)]` invocation sites (Phase 2). Captures the *invocation*,
//! never the macro-generated impl body (spec §5) — resolution mirrors
//! `implements` (trait-filtered, externals dropped per D1).
use syn::punctuated::Punctuated;
use syn::{Attribute, Path, Token};

use crate::spans::SourceRange;

/// A single derive-path site: the path as written plus the span of that
/// path token inside the attribute list (ADR-026: the token IS the edge).
pub struct DeriveSite {
    pub path: String,
    pub span: SourceRange,
}

/// Extract derive paths from an item's attributes. Non-`derive` attributes
/// and unparseable derive lists yield nothing (degrade silently — the file
/// already parsed; a malformed derive is the macro's problem, not ours).
pub fn derive_sites(attrs: &[Attribute], /* plus whatever span context the crate's span helpers need */) -> Vec<DeriveSite> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("derive") {
            continue;
        }
        let Ok(paths) = attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated) else {
            continue;
        };
        for p in paths {
            // path string rendering + span extraction follow the same
            // helpers calls.rs/spans.rs use (path_to_string / SourceRange::from_spanned).
            out.push(/* DeriveSite from p */);
        }
    }
    out
}
```

Use the crate's EXISTING path-rendering and span helpers (grep `spans.rs` and how `calls.rs` renders `Expr::Path` + builds `SourceRange`); do not invent new ones.
- [ ] **Step 4:** `src/edges.rs`: add `derives_edge(from_id, to_id, confidence, span)` — copy `implements_edge` doc-comment style and shape, kind `"derives"`.
- [ ] **Step 5:** `src/extract.rs`: in the `Item::Struct` and `Item::Enum` arms (after entity emission, resolver present), for each `derive_sites(&attrs)`: `resolver.resolve_trait_path(crate_name, &site.path)` → Resolved → `derives_edge(.., "resolved", ..)`; Ambiguous → `"ambiguous"`; External → drop. Mirror exactly how the `Item::Impl` arm consumes `resolve_trait_path` today.
- [ ] **Step 6:** Green: `cargo nextest run -p loomweave-plugin-rust` full crate.
- [ ] **Step 7: Commit** — `feat(plugin-rust): anchored derives edges (in-project traits, D1)`

### Task 4: `references` extraction

**Files:** Create `src/references.rs`, `tests/references_edges.rs`; Modify `src/edges.rs`, `src/extract.rs`, `src/lib.rs`

- [ ] **Step 1: Red tests** in `tests/references_edges.rs` covering the D3 envelope row by row (one test per row, in-test sources):
  - struct field type → edge struct→type, resolved, span on the type path token
  - nested generic arg: `field: Vec<MyType>` → edge to MyType (and NOT to Vec)
  - fn param + return types → edges from the fn entity
  - type-alias RHS → edge from type_alias entity
  - const/static declared type AND initializer path → edges from const/static entity
  - enum variant field type → edge from enum entity
  - body non-call path: `let x = LIMIT;` (const) → edge fn→const
  - struct literal: `Foo { a: 1 }` → edge fn→Foo
  - call callee NOT minted: `helper()` mints calls/unresolved-site only, zero references edges for `helper` (args still walked)
  - method receiver path IS minted: `CONFIG.get()` → reference to CONFIG
  - `use` statements mint no references (imports owns them)
  - derive list mints no references; impl header trait/self_ty mint none
  - external/primitive types (`u32`, `String`) → no edge, counted external
  - unknown path → no edge, counted unresolved
  - ambiguity per §7: a glob-importable collision lands ambiguous, never faked resolved (stage like the imports/implements ambiguity tests)
  - stats accounting: a file with N sites asserts the exact counter quadruple
  - edge SET dedup: same type referenced twice → ONE edge row (PK kind,from,to), span = first site; assert the SET exactly
- [ ] **Step 2:** Run filter → FAIL.
- [ ] **Step 3: Implement** `src/references.rs`: a `syn::visit::Visit` type-walker (`visit_type_path` collecting every nested `TypePath` path — skipping `Self`) applied to field types / sig types / alias RHS / const-static types, plus an expression visitor for bodies and initializers (`visit_expr_path` collecting, with the call-callee carve-out: in `visit_expr_call`, when `func` is `Expr::Path` visit only args; `visit_expr_struct` collects the struct path then walks fields; skip `visit_macro` bodies entirely per §5). Each site → `ReferenceSite { path: String, span: SourceRange }`. Resolution in the caller (extract.rs): `resolver.resolve_use_path` (kind-unfiltered) — Resolved → edge `"resolved"`; Ambiguous → `"ambiguous"`; External → `skipped_external += 1`; (the resolver has no "no match" distinct from External — both land External; count them as external, set unresolved counter from sites whose resolution returned External AND whose path is single-segment? NO — keep it simple and honest: External outcome → `references_skipped_external_total`; `unresolved_reference_sites_total` stays 0 for Rust and the divergence is documented in D4's plan row and the module doc-comment. Python distinguishes because pyright reports externality; syn cannot.)
  Dedup before emit: BTreeSet on (from_id, to_id), first-span-wins, per file.
- [ ] **Step 4:** `src/edges.rs`: `references_edge(...)` helper, kind `"references"`, same anchored shape + doc-comment.
- [ ] **Step 5:** `src/extract.rs`: wire sites→resolution→emission in the Struct/Enum/Fn/Type/Const/Static arms; thread the four counters up through the existing `extract_file_with_edges` return path into `AnalyzeFileStats` (find how `unresolved_call_sites_total` travels through serve.rs:139 and ride alongside).
- [ ] **Step 6:** serve.rs: populate `reference_sites_total`, `references_resolved_total`, `references_skipped_external_total` (and the always-0 fields) on the per-file stats.
- [ ] **Step 7:** Green: full crate nextest.
- [ ] **Step 8: Commit** — `feat(plugin-rust): anchored references edges (type positions + expression paths)`

### Task 5: Calls-envelope audit

**Files:** Modify `tests/calls_edges.rs` (extend; split a new `tests/calls_envelope_audit.rs` if it crowds)

- [ ] **Step 1:** Write the boundary-pin tests enumerated in D7 (one per boundary, each named `envelope_<boundary>`); run; for any FAIL decide: cheap fix (do it TDD in this task) vs structural (file filigree issue with `--dep clarion-bfb3e2be49`, mark the test `#[ignore = "clarion-XXXX"]` ONLY if the behavior is wrong, otherwise pin current behavior with a comment naming it intentional).
- [ ] **Step 2:** Audit `unresolved_call_sites_total` accounting + serve-loop batching: assert per-file total == vec len on a mixed fixture; assert ordinals strictly increasing in source order across resolved AND unresolved sites interleaved.
- [ ] **Step 3: Commit** — `test(plugin-rust): calls resolution envelope pinned (audit)` plus any `fix(...)` commits separately.

### Task 6: Writer-proving e2e + host gate

**Files:** Modify `tests/analyze_e2e.rs`, fixture `tests/fixtures/e2e_crate/`, `crates/loomweave-cli/src/analyze.rs` (test mod only)

- [ ] **Step 1:** Extend `e2e_crate` fixture minimally: an in-project trait + `#[derive(ThatTrait)]` + an external derive + cross-module type references (field + fn sig + body const ref). No qualname/entity-set assertions elsewhere may break — the fixture ALREADY carries all entity kinds; adding items changes the expected entity set in analyze_e2e.rs:210-244 — update that expected set in the same commit (this is an allowed entity-set change: it is a TEST fixture, not the frozen dialect).
- [ ] **Step 2: Red:** new e2e assertion querying `SELECT kind, from_id, to_id, confidence FROM edges WHERE kind IN ('derives','references')` (plus existing kinds) and asserting the exact expected SET.
- [ ] **Step 3:** Green via `cargo build --workspace --bins && cargo nextest run -p loomweave-plugin-rust analyze_e2e`.
- [ ] **Step 4:** Host gate unit test (analyze.rs test mod, next to the existing drain/drop gate tests from c970255): a `derives` edge whose to_id is not in seen set stays pending and is drop-counted; same for `references`; run still proceeds (pure unit on `drain_ready_plugin_edges`/`drop_unready_plugin_edges`).
- [ ] **Step 5: Commit** — `test(e2e): edge-set proof through real analyze incl. derives+references; gate generality pinned`

### Task 7: Dogfood numbers + gates + merge

- [ ] **Step 1:** Build release-ish binary in worktree, run `loomweave install --path` + `loomweave analyze` over a temp copy of `crates/` (or in-place with a temp store per analyze_e2e's pattern), then `SELECT kind, COUNT(*) FROM edges GROUP BY kind` + entity count. Record numbers for the report (derives sparseness evidence).
- [ ] **Step 2:** Full CI floor from worktree root: fmt, clippy, build --bins, nextest full workspace, doc, deny, all four Python gates, every `scripts/check-*.py`.
- [ ] **Step 3:** Three e2e smokes + `tests/e2e/hostile_corpus_rust.sh`.
- [ ] **Step 4:** Merge `--no-ff` into rc4 (re-merge rc4 into branch first if it moved), push, re-verify on main checkout (build bins → nextest), close tickets with `--reason`, remove worktree.
