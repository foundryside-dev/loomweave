# Loomweave → Wardline: Rust qualname dialect — resolved & pinned

**From:** Loomweave maintainers (Rust plugin)
**To:** Wardline maintainer (Rust frontend)
**Date:** 2026-06-09
**Re:** `wardline/.worktrees/rust-plugin/docs/superpowers/specs/2026-06-08-wardline-rust-frontend-design.md` §3.6, §6 (the "Loomweave dialect unfixed" blocker, §12 Q1)
**Status:** **Dialect fixed.** Loomweave's Rust `qualified_name` dialect is resolved and frozen by **ADR-049**; the shared corpus `fixtures/qualnames_rust.json` and a byte-for-byte extractor parity test are landed on `feat/rust-plugin-spec`. Wardline may pin the corpus, freeze `RS-WL-*` finding identity within the dialect, and drop the blocker. One forward question (SEI fold) is surfaced below and is **non-blocking**.
**Authority:** Loomweave is the authoritative producer for the Rust dialect (ADR-049). Where this response diverges from your §6 proposal, **Loomweave's form is normative and Wardline conforms** — exactly the posture your §6.4 anticipated ("SP2 ... may revise the `:trait=` / `{closure#N}` spellings to match Loomweave").

---

## Verdict

**The premise "Loomweave's Rust entity-ID dialect is not yet fixed" (§3.6, §6.4) is now false.** The dialect was settled in `docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md` (Accepted 2026-06-08) and is *emitted today* by `crates/loomweave-plugin-rust`. The qualname is the SEI **locator** (ADR-038), so it had to be collision-free and benign-edit-stable before any code landed — which forced these decisions earlier and more precisely than a "spelling" question. Your §6 dialect is **directionally aligned** (dotted, crate-rooted, `.`-delimited, generics-agnostic at the definition, async-suffix-free) but diverges on the **impl-discriminator spelling** and on **closures/nested-fns**, where Loomweave's whole-tree extractor is the oracle. Adopt the forms below.

This is framed exactly like the Python precedent: Wardline is a **second producer** of the same qualname, validated against a shared corpus. ADR-049 §2 already contemplates a second producer (a future rust-analyzer enrichment) that "MUST reproduce §1's qualname byte-for-byte." Wardline's tree-sitter frontend is that second producer for the slice-1 surface.

> **Opacity note (ADR-003/ADR-038).** The locator is opaque — *consumers* must not parse it. Wardline is not a consumer here; it is a **co-producer** minting the same string from source and folding it into its own fingerprint. That is the sanctioned path (ADR-049 §2's rust-analyzer clause), not a violation of opacity.

## Resolved dialect (your §6 proposal → Loomweave normative form)

| # | Decision | Your §6 proposal | **Loomweave normative (ADR-049, emitted)** | Verdict |
|---|---|---|---|---|
| 1 | Delimiter | `.` | `.` | **Agree.** Keep `.`; reuse your ~20 `.`-split sites. |
| 1b | Crate root | crate-rooted | crate-rooted; **crate name `-`→`_` normalised** (`loomweave-cli`→`loomweave_cli`), discovered from `Cargo.toml [package].name` read as **text**, longest-path-prefix match | **Agree + pin underscoring + manifest-text rule.** Crate-name derivation is **sp2** for you (needs manifest reads). |
| 2 | Closures | `crate.mod.func.<locals>.{closure#N}` | **Not emitted** — extractor never descends into fn bodies | **Diverge — Loomweave normative.** No closure entity. A finding inside a closure attributes to the nearest enclosing named item. **Drop `{closure#N}` entirely.** |
| 2b | Nested `fn` | `crate.mod.outer.<locals>.inner` | **Not emitted** (same reason) | **Diverge — Loomweave normative.** No `.<locals>.inner` entity; attribute to `outer`. |
| 3 | Generics | strip all | free items strip; **trait-impl keeps concrete args** (`impl[From<i32>]` ≠ `impl[From<u32>]`); lifetimes dropped | **Refine — Loomweave normative.** Strip at the *definition name*; **keep** the trait's concrete type/const generic args in the impl discriminator. Single-file computable. |
| 4 | Trait disambig | `Foo.bar:trait=Trait` (suffix) | `Foo.impl[<Trait-last-seg-with-generics>].bar` (path **segment**) | **Diverge — Loomweave normative.** Use the `impl[...]` segment, not a `:`-suffix (`:` is a reserved char in `entity_id.rs` and would be rejected). `.`-split-safe: no `.` inside `[...]`. |
| 4b | Inherent disambig | (none) | `Foo.impl#<positional-generics>#<ordinal>.bar` (e.g. `impl#<>#0`, `impl#<$0>#0`) | **Diverge — Loomweave normative.** Positional `$i` generics (rename-stable) + **per-item-list source-order ordinal** that **resets inside nested `mod`s**. Within-file ordinals are slice-1; **cross-file** inherent-impl ordinals are **sp2**. |
| 4c | cfg twins | (none) | a path-colliding cfg-gated sibling gets a normalised `@cfg(<pred>)` suffix, **item-general**: `f@cfg(unix)`, `S@cfg(unix)` (struct), `inner@cfg(windows)` (inline mod); unique paths get nothing | **Adopt — Loomweave normative.** Predicate whitespace-stripped, `any()`/`all()` args sorted; counted per-kind. Slice-1. *(Hardened during this review — see note.)* |
| 5 | `kind` | `function` / `method` semantic | id-kind is **`function`** for *every* callable (free fn, method, assoc fn); `struct`/`module` otherwise; **no `method` id-kind** | **Diverge — Loomweave normative.** The locator's kind segment is `function`. Your function/method semantic split must ride `Entity` metadata, never the qualname (you already proposed this for the trait distinction in §6.2 — extend it to method-ness). |
| 6 | Macros | skip (Tier-C opaque) | skip (`syn` does not expand; `Item::Macro` is ignored) | **Agree.** Neither engine indexes macro-generated items. Both must agree they do not exist. Future rust-analyzer enrichment re-enters this same corpus+SEI contract (ADR-049 §2, finding H4). |
| 7 | SEI fold | folds qualname; SP2 may rekey | qualname = locator (benign-edit-stable, **not** rename/move-stable); SEI token (ADR-038) is the rename-stable handle | **Surface — see below. Non-blocking.** |

### Why the divergences are sound, not arbitrary

- **Closures/nested-fns dropped (2, 2b).** Your `{closure#N}` is positional and churns under edits — the exact instability the SEI directive exists to kill (Clarion's pre-SEI ids orphaned on edit). Loomweave sidesteps it by **not minting an entity** for body-local items at all; the enclosing named item is the stable anchor, and your fingerprint already carries `line_start` to localise within it. This is strictly *more* stable than a positional token, and it costs you nothing you can reproduce single-file anyway.
- **`impl[...]` segment over `:trait=` suffix (4).** `:` is rejected by `entity_id.rs` (only reserved char). The `impl[...]`/`impl#...#N` segments use `[]<>#@` — all permitted, all `.`-split-safe — and they sit *before* the method component, so `rsplit('.',1)` still recovers the method name and `split('.<locals>.')[0]` is unaffected (there are no `<locals>` in Rust ids). Your ~20 format sites survive.
- **Generics kept in the trait key (3).** `impl From<i32>` and `impl From<u32>` are genuinely distinct methods that would otherwise collapse to one locator and silently overwrite each other in the writer (`writer.rs:570` `ON CONFLICT(id) DO UPDATE`). You can read these args from the same impl block, so it is slice-1 reproducible.
- **cfg-twins are item-general (4c) — a fix this review forced.** ADR-049 §3 mandates the `@cfg` discriminant for "each cfg-gated **item**," but the extractor implemented it for **functions only** — a `#[cfg(unix)] struct S;` / `#[cfg(windows)] struct S;` pair both emitted bare `…m.S` and the writer's `ON CONFLICT` silently dropped one (the exact data-loss family ADR-049 exists to prevent, escaping the Phase-1a zero-collision gate). Generalised to `struct` and inline `mod`, counted per-kind, with a `struct_cfg_twin` corpus row and two new uniqueness/parity guards. **Wardline takeaway:** a single-file frontend is equally exposed — apply your `@cfg` rule to every emitted item kind, not just `fn`.

## Reproducibility tiers (the actual cross-engine contract)

Every corpus case is tagged `reproducibility: slice-1 | sp2`. This is the load-bearing part for you, because your slice-1 frontend sees one file at a time:

- **`slice-1`** — the qualname **suffix** (everything the single file's syntax fixes: the `impl[...]`/`impl#...#N` discriminator, `@cfg`, within-file ordinal, positional generics, the closure/nested-fn *folding*) is reproducible by your tree-sitter pass **now**.
- **`sp2`** — needs Loomweave's whole-tree view: **crate-name from `Cargo.toml`**, **cross-file module route**, **`#[path]`**, **cross-file inherent-impl ordinals**. The **crate-root prefix of every entity is itself sp2** until you read manifests; slice-1 cases are reproducible *modulo that shared prefix*.

This matches your own staging exactly: `RS-WL-*` stays **provisional / baseline-ineligible** (§3.6) until the sp2 surface lands; slice-1 is your **format drift-gate**; the **frozen** cross-engine corpus is your SP2 completion gate. The corpus hands you the complete authoritative target *and* tells you which rows you can hit today versus at SP2 — so you never accumulate a baseline on an sp2 row that an SP2 rekey would orphan.

### Known gap, pinned honestly

`#[path = "..."]` module mounting is **not yet honoured** by `module_path.rs` (ADR-049 lists it; the implementation routes purely by file path). The corpus row `path_attr_known_gap` pins what the extractor **actually emits today** (`demo.renamed`, the mechanical file-path result) with `known_gap: true`, not the `#[path]`-correct module. Both engines must match the *emitted* form; `#[path]`-correct routing is a shared sp2 task. Pinning the real behaviour now prevents a silent drift when it is fixed.

## Corpus hosting & vendoring

- **Loomweave hosts** the authoritative `fixtures/qualnames_rust.json` (repo root, beside `fixtures/entity_id.json`). This **inverts** the Python arrangement (Wardline seeded `qualnames.json`; Loomweave vendored it) because for Rust **Loomweave's whole-tree extractor is the oracle** — the dialect is *defined by* what it emits, so the seed must live where it is generated and parity-tested.
- **Wardline vendors** a pinned copy to `tests/conformance/qualnames_rust.json` (the early format drift-gate you named in §6.4) and reproduces `expected` byte-for-byte. On any Loomweave dialect change, re-copy verbatim; your conformance test then fails loudly on divergence — fix the producer or resync, never the vendored copy silently. (Same resync discipline as the existing `loomweave_qualname_parity.json`.)
- **Shape:** `module_route` cases pin path→module routing (`module_path_for`); `entities` cases give `{name, crate, module_path, rel_path, source, reproducibility, expected:[{qualname,kind}]}` and pin the full emission in source order (including the file-scope and nested `module` entities). The Python `qualnames.json` contract and its parity tests are **untouched**.

## SEI vs qualname fold (decision 7) — surfaced, with recommendation, non-blocking

**Freezing the qualname *dialect* is what drops your blocker** — that is delivered here and is sufficient on its own. The fold question is separable and forward-looking:

- **Recommendation: keep folding the qualname** into `fingerprint` (you do today). It is the only identifier you can reproduce single-file, and within a repo version it is unique and stable. Do **not** attempt to fold Loomweave's SEI token — you cannot reproduce it: the ADR-038 token folds content signatures Loomweave mints whole-tree; a single-file frontend has no way to compute the same bytes.
- **The cross-rename/move stability you'd want from an SEI fold is Loomweave's job, not your fingerprint's.** The locator is stable across *benign* edits (reorder, sibling add/remove, generic-param rename) but **churns on a rename/move** of the entity itself — by design; the SEI *matcher* (carry-or-mint, keyed on the prior locator) is what carries a binding across a rename, server-side. The conformance oracle that would let you resolve an old qualname to a carried entity (`GET /api/v1/entities/resolve?scheme=...`) is **deferred** (noted DEFERRED in `wardline-qualname-normalization.json`), so cross-rename carry is not available to either of us yet.
- **The one genuine product question** (does not block the freeze): when the resolve oracle ships, should Wardline re-key historical findings through it at scan time (so a renamed entity's findings carry), or keep fingerprints qualname-pinned and accept churn-on-rename as a baseline reset? That is a Wardline-side baseline-policy call; Loomweave's commitment is only to expose the carry via the (future) oracle. Flagging it so it is tracked, not so it gates slice 1.

## One-paragraph reply (for your blocker)

> **Loomweave's Rust qualname dialect is fixed (ADR-049) and frozen against a shared corpus.** Final dialect: `.`-delimited, crate-rooted (`crate` name `-`→`_`, from `Cargo.toml` text); free items `crate.mods.Name`; **trait-impl methods carry an `impl[<Trait-with-concrete-generics>]` path segment** (`Foo.impl[Display].fmt`, `Foo.impl[From<i32>].from`) and **inherent-impl methods carry `impl#<positional-generics>#<ordinal>`** (`Foo.impl#<>#0.bar`, `Foo.impl#<$0>#0.get`, ordinal source-order per module scope, resets in nested `mod`s); cfg-twins get a normalised `@cfg(pred)` suffix; `async fn` renders identically to `fn`; the locator **kind segment is always `function`** for callables (no `method` kind — ride metadata). **Adopt the `impl[...]`/`impl#...#N` segment forms, not your `:trait=` suffix** (`:` is reserved/rejected). **Closure stability: closures and nested `fn` items are NOT entities** — Loomweave never descends into bodies, so drop `{closure#N}`/`.<locals>.inner` and attribute body-local findings to the enclosing named item; this removes the positional-churn instability entirely. **SEI fold: keep folding the qualname** (the only single-file-reproducible id); do **not** fold the ADR-038 SEI token (unreproducible single-file) — cross-rename carry is Loomweave's server-side SEI matcher + the deferred resolve oracle, not your fingerprint. Corpus is **Loomweave-hosted** at `fixtures/qualnames_rust.json` on `feat/rust-plugin-spec`, every case tagged `slice-1` vs `sp2`; vendor a pinned copy to `tests/conformance/qualnames_rust.json`. You can drop the "Loomweave dialect unfixed" blocker; `RS-WL-*` slice-1 rows are dialect-frozen (still baseline-ineligible until your sp2 surface lands, per your §3.6 staging).

## Loomweave-side landed artifacts (`feat/rust-plugin-spec`)

- `docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md` — the authoritative decision (pre-existing; this response is its federation-facing projection).
- `fixtures/qualnames_rust.json` — shared corpus, generated from the live extractor, reproducibility-tiered.
- `crates/loomweave-plugin-rust/tests/qualname_conformance.rs` — byte-for-byte extractor parity test (drives `extract_file` / `module_path_for` over the corpus).
- `fixtures/entity_id.json` — pre-existing cross-tool entity-id parity fixture; already carries the ADR-049 Rust rows (`impl[Display]`, `impl#<$0>#0`, `@cfg(unix)`).
