# Loomweave → Wardline: Rust qualname dialect — Phase 1b conformance change-set

**From:** Loomweave maintainers (Rust plugin)
**To:** Wardline engineering (Rust tree-sitter frontend)
**Date:** 2026-06-09
**Re:** Phase 1b changes to the Rust `qualified_name` dialect that Wardline's second-producer frontend (and its vendored corpus) must mirror.
**Status:** **Change-set, action required.** The Phase 1b dialect amendments are landed and emitted today by `crates/loomweave-plugin-rust` on `feat/rust-plugin-spec`. The shared corpus `fixtures/qualnames_rust.json` and the byte-for-byte parity gate `tests/qualname_conformance.rs` are amended in lockstep. Wardline must update its tree-sitter engine and re-vendor the corpus.
**Supersedes:** the **inherent-impl-discriminator section** of `docs/federation/2026-06-09-rust-qualname-dialect-response.md` (decision 4b and the corresponding clause of its one-paragraph reply). That letter described an inherent-impl key of `Foo.impl#<positional>#<ordinal>` (a source-order ordinal). **Phase 1b drops the ordinal.** Everything else in the prior letter still stands; this document amends only what changed and adds the new surface (leaf kinds, the `impl` entity, edges).
**Authority:** Loomweave remains the authoritative producer for the Rust dialect (**ADR-049**, amended 2026-06-09). The corpus `expected` values are generated from the live extractor, never hand-authored; where this document and your frontend diverge, **Loomweave's emitted form is normative and Wardline conforms**.
**Version:** `ontology_version` bumped **0.1.0 → 0.4.0** (ADR-027 MINOR bumps: new entity kinds, then the `imports`/`implements` edge kinds). `plugin_id` is `rust`, so ids read `rust:<kind>:<qualname>` (e.g. `rust:macro:demo.m.make`, `rust:impl:demo.m.Foo.impl#<>`).

---

## 1. Context — what this changes and why

The prior letter froze the slice-1 dialect: dotted, crate-rooted, `impl[Trait]` / `impl#<sig>#<ordinal>` discriminators, `@cfg` twins, `function` id-kind for every callable, no closure/nested-fn entities. Wardline pinned that corpus and froze `RS-WL-*` identity against it.

Phase 1b makes four classes of change, all already emitted:

1. **Six new leaf entity kinds** (`enum`, `trait`, `type_alias`, `const`, `static`, `macro`) plus a **macro-definition rule**.
2. **The `impl` block is now its own entity** (`kind=impl`), and methods **re-parent** onto it: containment is `module → impl → method` (was `module → method`).
3. **BREAKING: the inherent-impl source-order `#<ordinal>` is dropped.** Same-`(type, positional-generic-sig, cfg)` inherent impls now **merge** into one `impl` entity. This is the amendment that supersedes the prior letter's decision 4b.
4. **Two new anchored edge kinds**, `imports` and `implements`.

Why the ordinal drop: the prior scheme called its inherent discriminator "source-order-independent" while keying it on a source-order ordinal — a self-contradiction (ADR-049 §1, amendment 2026-06-09, Option (b)). Dropping the ordinal and merging makes the discriminator genuinely **reorder-stable AND method-set-stable**: adding, removing, or reordering an inherent `impl` block of an already-seen `(type, sig, cfg)` does not churn any id.

---

## 2. Change-set summary

| # | Change | What Wardline must do |
|---|---|---|
| A | **New leaf kinds** `enum`/`trait`/`type_alias`/`const`/`static`/`macro` | Emit these item kinds as entities with id-kind = the kind name; free-item qualname `<crate>.<mods>.<name>`. |
| B | **`macro_rules! foo` is a `macro` entity** | Emit `rust:macro:<crate>.<mods>.foo` for a `macro_rules!` *definition*. A bare `foo!()` *invocation* emits **nothing** (neither engine expands macros). |
| C | **`impl` block is its own entity** (`kind=impl`) | Emit one `impl` entity per impl block (post-merge), parented to its module. Trait form `…<Type>.impl[<Trait>]`, inherent form `…<Type>.impl#<sig>`. |
| D | **Method re-parenting** | Containment is now `module → impl → method`. If you emit containment, parent impl methods to the `impl` entity, not the module. (Method **qualname** is unchanged — it already carried the impl discriminator.) |
| E | **BREAKING — inherent-impl ordinal dropped** | Stop emitting the trailing `#<ordinal>`. Merge same-`(type, positional-generic-sig, cfg)` inherent impls into ONE entity; union their methods under it. See §3. |
| F | **`@cfg` now splits impls** (inherent AND trait) | Render the `@cfg(<pred>)` suffix on cfg-twin impl keys (`impl#<>@cfg(unix)`, `impl[Display]@cfg(windows)`). This is the SOLE discriminant once the ordinal is gone. See §3, §7. |
| G | **`imports` edge** (anchored) | If you emit edges: resolve `use` leaves via your symbol table → anchored `imports` edge; external/unresolvable dropped. See §6. |
| H | **`implements` edge** (anchored) | Anchored edge from the trait-`impl` entity to the resolved trait; external trait dropped. See §6. |

Changes A–F are **qualname/entity-set** changes — they are in the conformance corpus and the byte-for-byte gate WILL catch a divergence. Changes G–H are edge-shape changes (corpus is entity-only); conform to §6.

---

## 3. The BREAKING change in detail — the inherent-impl ordinal drop

**Before (frozen, what you implement today):** an inherent-impl method was `…<Type>.impl#<positional-generics>#<ordinal>.<method>`, the ordinal assigned by source order within the module scope (`impl#<>#0`, `impl#<>#1`, …), resetting inside nested `mod`s. Two same-signature inherent blocks produced **two distinct entities**.

**After (Phase 1b):** the ordinal is **gone**. The key is `…<Type>.impl#<positional-generics>` with no `#<ordinal>`. Same-`(type, positional-generic-sig, cfg)` inherent impls **merge into one `impl` entity**; the union of their methods hangs off that single entity.

### Before / after table

| case | old (frozen — prior letter) | new (Phase 1b — corpus) |
|---|---|---|
| single inherent impl method | `Foo.impl#<>#0.bar` | `Foo.impl#<>.bar` |
| two same-sig inherent impls | `Foo.impl#<>#0.a` + `Foo.impl#<>#1.b` (two `impl` entities) | both under ONE `Foo.impl#<>` → `Foo.impl#<>.a` + `Foo.impl#<>.b` |
| generic inherent impl | `Foo.impl#<$0>#0.get` | `Foo.impl#<$0>.get` |
| cfg-twin inherent impl | (was ordinal-separated) | `Foo.impl#<>@cfg(unix)` / `Foo.impl#<>@cfg(windows)` (NOT merged) |

(Qualnames above are shown without the crate/module prefix for readability; in the corpus they are fully rooted, e.g. `demo.m.Foo.impl#<>.bar`.)

### Merge semantics — the key is the triple `(type, positional-generic-sig, cfg)`

Two inherent `impl` blocks **merge into one `impl` entity iff** they share all three of:

- the **target type** (already disambiguated by the module prefix — a `Foo` in `mod inner` is a different type from a top-level `Foo`);
- the **positional-generic signature** (`impl#<>` for non-generic, `impl#<$0>` for one type param, De Bruijn-positional so a `<T>`→`<U>` rename does not churn);
- the **cfg** (the normalised `@cfg(<pred>)` suffix, or its absence).

`cfg` is part of the key — **that is exactly why cfg-twin impls do NOT merge.** Put the reference pair side by side:

- **`multiple_inherent_merge`** (corpus): `impl Foo { fn a }` + `impl Foo { fn b }`, no cfg → same triple → **ONE** entity `demo.m.Foo.impl#<>` carrying both `a` and `b`.
- **`inherent_impl_cfg_twin`** (corpus, new this change-set): `#[cfg(unix)] impl Foo { fn go }` + `#[cfg(windows)] impl Foo { fn go }` → cfg differs → triple differs → **TWO** entities `demo.m.Foo.impl#<>@cfg(unix)` and `demo.m.Foo.impl#<>@cfg(windows)`, each carrying its own `go`.

If a Wardline impl-extractor omitted the `@cfg` impl suffix, the two cfg-twin blocks would collapse to one `Foo.impl#<>` and one `go` would be silently dropped (the writer's `ON CONFLICT(id) DO UPDATE` overwrite — the exact data-loss family ADR-049 exists to prevent). The new `inherent_impl_cfg_twin` / `trait_impl_cfg_twin` corpus rows are the conformance trip-wire for precisely this omission (see §7).

**Emit ordering (matches the corpus):** the merged `impl` entity is emitted **once**, at the first contributing block in source order; later same-key blocks contribute only their methods. Reorder-stability holds because the key never depends on which block came first.

---

## 4. The `impl` entity + method re-parenting

**The `impl` block is now its own entity**, `kind=impl`, with these key forms:

- **Trait impl:** `…<Type>.impl[<TraitPath-last-segment-with-concrete-generics>]` — e.g. `Foo.impl[Display]`, `Foo.impl[From<i32>]`. Lifetimes dropped; concrete type/const generic args **kept** (`From<i32>` ≠ `From<u32>`).
- **Inherent impl:** `…<Type>.impl#<positional-generic-sig>` — e.g. `Foo.impl#<>`, `Foo.impl#<$0>`. No ordinal (§3).

**Containment is now `module → impl → method`** (was `module → method`):

- the `impl` entity is parented to the enclosing **module** (`module → impl`);
- each impl method is re-parented onto the **`impl` entity** (`impl → method`), NOT the module.

The method **qualname is unchanged** — it already carried the impl discriminator (`Foo.impl[Display].fmt`, `Foo.impl#<>.bar`), so the re-parent does not churn any method id. **What changes for Wardline:** (a) you must emit the `impl` row itself (the corpus `expected` lists it — see §7), and (b) if you emit containment, the method's parent is the impl entity.

Corpus reference rows: `inherent_method`, `trait_method`, `trait_method_collision` (Display::fmt vs Debug::fmt stay distinct via the impl segment), `generic_trait_impl_concrete_args`, `positional_generic_param` (+ `_renamed` for rename-stability).

---

## 5. The new leaf kinds + the macro rule

Six new free-item kinds, each emitted with id-kind = the kind name and free-item qualname `<crate>.<mods>.<name>`:

`enum`, `trait`, `type_alias`, `const`, `static`, `macro`.

(`trait` here is the trait *definition* item — distinct from `impl[Trait]`, which is a discriminator on an impl entity.)

**Macro rule (corpus `macro_invocation_generates_no_entity`):**

- `macro_rules! foo { … }` — the **definition** is a `macro` entity → `rust:macro:<crate>.<mods>.foo`.
- `foo!()` — a bare **invocation** emits **NOTHING**. Neither engine expands macros (`syn` does not expand; tree-sitter cannot see expansion), so the compile-time-generated items do not exist for either producer. Both engines must agree they do not exist.

The full contract-vetted id-kind set is now exactly these ten (mirrors `plugin.toml entity_kinds`, and the corpus `corpus_kinds_are_known` guard): `module`, `struct`, `function`, `enum`, `trait`, `type_alias`, `const`, `static`, `macro`, `impl`. There is still **no `method` id-kind** — every callable (free fn, inherent method, assoc fn, trait method) is id-kind `function`.

---

## 6. The new edges — `imports` and `implements`

Both are **anchored** (ADR-026 decision 3): they carry the source byte span, so they may **never** be `inferred` confidence. Both are **resolved-or-dropped**: an unresolvable / external target yields no edge (never a dangling edge). On the Loomweave side both are additionally subject to the host **seen-entity-set gate** — an edge whose target entity was not stored (e.g. gitignored, out-of-`src`) is dropped at the host, so an emitted edge can still be gated away if its target never landed. Wire shape (the host deserialises into `RawEdge`):

```json
{ "kind": "imports",    "from_id": "<module entity id>", "to_id": "<resolved target id>", "source_byte_start": <int>, "source_byte_end": <int>, "confidence": "resolved" | "ambiguous" }
{ "kind": "implements", "from_id": "<impl entity id>",   "to_id": "<resolved trait id>",  "source_byte_start": <int>, "source_byte_end": <int>, "confidence": "resolved" | "ambiguous" }
```

**`imports`** — one per file-scope `use` leaf, resolved against the project symbol table:

- `from_id` is the enclosing **module** entity (a file-scope `use` is a module property), NOT the crate root.
- a unique in-project target → `confidence: "resolved"`; a glob / multi-kind candidate → `confidence: "ambiguous"`; an external or unresolvable path → **NOTHING** (external dropped, D1).
- the byte span anchors the `use` statement.

**`implements`** — one per resolved trait impl:

- `from_id` is the trait-**`impl` entity** (`…<Type>.impl[<Trait>]`); `to_id` is the resolved trait entity.
- the byte span anchors the **implemented-trait path** (the `Tr` in `impl Tr for Foo`), not the whole impl block.
- a unique in-project trait → `resolved`; a multi-kind candidate → `ambiguous`; an external trait → **no edge** (dropped at emit).

The shared corpus is **entity-only** — it does not pin edges. The contract for edges is this section; conform to the shape and the resolved-or-dropped / anchored-never-inferred rules.

---

## 7. The conformance corpus

The authoritative corpus is **Loomweave-hosted** at `fixtures/qualnames_rust.json` on `feat/rust-plugin-spec`, generated from the live extractor and parity-tested by `crates/loomweave-plugin-rust/tests/qualname_conformance.rs`. **Wardline re-vendors a pinned copy to `tests/conformance/qualnames_rust.json`** and reproduces `expected` byte-for-byte; on any Loomweave dialect change, re-copy verbatim and let your conformance test fail loudly — fix the producer or resync, never edit the vendored copy silently.

**New cases this change-set (Phase 1b), reproducibility `slice-1`:**

- **`inherent_impl_cfg_twin`** — `struct Foo; #[cfg(unix)] impl Foo { fn go } #[cfg(windows)] impl Foo { fn go }` →
  - `demo.m.Foo.impl#<>@cfg(unix)` (impl), `demo.m.Foo.impl#<>@cfg(unix).go` (function)
  - `demo.m.Foo.impl#<>@cfg(windows)` (impl), `demo.m.Foo.impl#<>@cfg(windows).go` (function)
- **`trait_impl_cfg_twin`** — `struct Foo; #[cfg(unix)] impl Display for Foo { fn fmt } #[cfg(windows)] impl Display for Foo { fn fmt }` →
  - `demo.m.Foo.impl[Display]@cfg(unix)` (impl), `demo.m.Foo.impl[Display]@cfg(unix).fmt` (function)
  - `demo.m.Foo.impl[Display]@cfg(windows)` (impl), `demo.m.Foo.impl[Display]@cfg(windows).fmt` (function)

Both also carry the `demo.m` module row and the `demo.m.Foo` struct row. These two cases close a coverage gap: before them, every `@cfg` corpus row was a **free-item** twin (`f@cfg`, `S@cfg`), so the gate never exercised `@cfg`-on-an-impl-key rendering — a Wardline impl-extractor that omitted the `@cfg` impl suffix would have **passed** conformance while silently merging cfg-twin impls. Note where the suffix lands: **after** the impl discriminator (`impl#<>@cfg(unix)`, `impl[Display]@cfg(unix)`), and the method appends **after that** (`impl#<>@cfg(unix).go`).

**The two comparison rules (unchanged, restated):**

1. **`entities_match_byte_for_byte` — full-set rule.** The corpus `expected` is Loomweave's FULL emission in source order, including `module` rows and `impl` rows. The byte-exact obligation is the `qualname` of every NON-`module`, NON-`impl` row (the string folded into the fingerprint); it must match character-for-character.
2. **Subset-consumer rule.** A method-only / function-only consumer (Wardline's `discover_file_entities`, where modules and impl blocks are scope-only and §6.2 tags impl fns `kind=method`) MUST NOT list-equality-compare against `expected`. Instead: take your function/method entities and assert each produces a `qualname` present in the case's non-`module`/non-`impl` `expected` qualnames. `module` rows validate against the `module_route` section; `impl` rows are scope entities a method-only consumer skips exactly as it skips `module` rows. `kind` is informational (the locator id-kind) — map your semantic `method` → corpus `function`, or compare qualname-only. Do not edit the vendored copy to drop rows; apply this comparison rule.

---

## 8. Tree-sitter engine guidance

Concrete steps to conform your frontend:

**(a) Drop the trailing `#<ordinal>`.** Remove the source-order ordinal from the inherent-impl key entirely. The inherent key is now `…<Type>.impl#<positional-generic-sig>` with nothing after the `>`.

**(b) Merge same-key inherent impls.** Key each inherent `impl` block by the triple `(target-type-path, positional-generic-sig, normalised-cfg)`. Blocks sharing a key produce **one** `impl` entity (emitted at the first block in source order); accumulate methods from all same-key blocks under it. Do not assign per-block ids.

**(c) Render `@cfg(<normalised-pred>)` on cfg-twin impls.** When two impls of the same `(type, sig)` are gated on different cfgs, append the `@cfg(<pred>)` suffix to the impl key — inherent → `impl#<>@cfg(unix)`, trait → `impl[Display]@cfg(unix)` — and let each method inherit it (`…@cfg(unix).go`). **The `@cfg` predicate normalisation must match Loomweave's exactly:** predicate whitespace-stripped, `any()`/`all()` args sorted. The corpus cfg rows (`cfg_twin`, `struct_cfg_twin`, and now `inherent_impl_cfg_twin` / `trait_impl_cfg_twin`) are the reference rendering — diff against them. Apply `@cfg` to **every** emitted item kind that can twin (free fn, struct, inline mod, AND impls), not just `fn`.

**(d) Emit the `impl` entity + re-parent methods.** Emit one `impl` row per impl block (post-merge) with the key from (a)/(b)/(c). Parent it to the enclosing module; parent each impl method to the `impl` entity (`module → impl → method`). Method qualnames are unchanged.

**(e) Add the leaf/macro kinds.** Emit `enum`/`trait`/`type_alias`/`const`/`static`/`macro` as free-item entities. `macro_rules! foo` → a `macro` entity; a bare `foo!()` invocation emits nothing.

**(f) Edges (if you emit them).** Conform to §6: anchored, byte-span-carrying, resolved-or-dropped; `imports` from the module entity, `implements` from the trait-impl entity; never `inferred`.

After these, run your vendored-corpus conformance test (re-vendor first) and the subset-consumer parity (§7 rule 2).

---

## 9. What is UNCHANGED

The bulk of the frozen dialect is stable — Phase 1b is additive plus the one ordinal-drop amendment. Unchanged:

- **`.`-delimited, crate-rooted** path; crate name `-`→`_` normalised, from `Cargo.toml [package].name` read as text (still **sp2** for you — needs manifest reads).
- **Module routing:** lib.rs/main.rs/mod.rs contribute no segment; inline `mod foo {}` nests; `#[path]` still NOT honoured (KNOWN-GAP, the `path_attr_known_gap` corpus row pins the emitted-today behaviour).
- **The `impl[Trait]` trait-impl form** — unchanged (trait last segment, concrete generics kept, lifetimes dropped, `From<i32>` ≠ `From<u32>`).
- **Methods carry their impl's discriminator** (`Foo.impl[Display].fmt` ≠ `Foo.impl[Debug].fmt`) — unchanged; the re-parent is a containment change only.
- **Positional De Bruijn generics** (`impl#<$0>`, rename-stable) — unchanged.
- **`@cfg` predicate normalisation** (whitespace-stripped, args sorted, per-kind) — unchanged; Phase 1b only **extends** where it applies (now impls too).
- **Closures and nested `fn` items are NOT entities** — the extractor never descends into bodies; attribute body-local findings to the nearest enclosing named item. Unchanged (corpus `closure_is_not_an_entity`, `nested_fn_is_not_an_entity`).
- **`function` id-kind for every callable** — no `method` id-kind; a semantic function/method split rides Entity metadata, not the locator. Unchanged.
- **`async fn` renders identically to `fn`** (no suffix). Unchanged.
- **SEI fold = qualname only** — keep folding the qualname into your fingerprint; do not attempt to fold the ADR-038 SEI token (unreproducible single-file). Cross-rename carry remains Loomweave's server-side SEI matcher + the deferred resolve oracle. Unchanged.
- **Reproducibility tiers** (`slice-1` / `sp2`) — unchanged; all Phase 1b corpus cases (incl. the two new ones) are `slice-1` modulo the shared crate-root-prefix sp2 caveat.

---

## Loomweave-side landed artifacts (`feat/rust-plugin-spec`)

- `docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md` — authoritative decision, amended 2026-06-09 (§1 ordinal drop, Option (b)).
- `fixtures/qualnames_rust.json` — shared corpus, generated from the live extractor; carries the Phase 1b `impl` entity rows, the merge case, and the two new `inherent_impl_cfg_twin` / `trait_impl_cfg_twin` cfg-twin-impl cases.
- `crates/loomweave-plugin-rust/tests/qualname_conformance.rs` — byte-for-byte parity gate (`entities_match_byte_for_byte`, `module_routes_match_byte_for_byte`, `corpus_kinds_are_known`).
- `crates/loomweave-plugin-rust/plugin.toml` — `ontology_version = "0.4.0"`, `entity_kinds` = the ten kinds, `edge_kinds = ["contains", "imports", "implements"]`.
- `docs/federation/2026-06-09-rust-qualname-dialect-response.md` — the prior letter, left intact; this document supersedes only its inherent-impl-discriminator section.
