# Loomweave → Wardline: Rust qualname dialect — Amendments 4 & 5 change-set / handoff

**From:** Loomweave maintainers (Rust plugin)
**To:** Wardline engineering (Rust tree-sitter frontend)
**Date:** 2026-06-11
**Re:** Two qualname-dialect amendments now **implemented and landed** in `crates/loomweave-plugin-rust` (Sprint-4 gold closeout). Wardline's second-producer frontend and its vendored corpus must mirror both.
**Status:** **HANDOFF — action required on Wardline's side, but NOT pushed by Loomweave.** Wardline's Rust frontend has graduated to its `rc5` release branch; per cross-repo discipline Loomweave does **not** commit dialect changes into a sibling's release branch. This letter + the re-vendorable corpus blob is the handoff. The Loomweave owner will surface it for scheduling.
**Authority:** Loomweave remains the authoritative producer for the Rust dialect (**ADR-049**, amended 2026-06-10 and 2026-06-11). The corpus `expected` values are generated from the live extractor, never hand-authored; where this document and your frontend diverge, **Loomweave's emitted form is normative and Wardline conforms.**
**Corpus blob:** `fixtures/qualnames_rust.json` now carries **35 entity rows** (md5 `bf8d09968b5d366a8bd033710d736744`), up from 28. Re-vendor verbatim to `tests/conformance/qualnames_rust.json`.
**Version:** No `ontology_version` bump — neither amendment adds an entity kind or edge kind; both refine the qualname *string* of existing `impl`/`function` kinds.

---

## 1. Context — two waves, one handoff

These two amendments were tracked separately and reach you together because both landed in the same Sprint-4 closeout:

- **Amendment 4 (2026-06-10, reserved-colon + const spacing).** Already **signed off by the Wardline owner** (your `2026-06-10-wardline-loomweave-rust-qualname-amendment-requests.md`, signed 2026-06-10) and recorded in ADR-049 the same day — but **unimplemented on both sides** until now. Loomweave owed the producer implementation + the three decided corpus rows; **both are now landed** (this closes the Loomweave half of `wardline-be5ee9cc34` and the const-spacing half of `wardline-e8f7c0508f` — your re-vendor + frontend work is now unblocked).
- **Amendment 5 (2026-06-11, cfg-twin methods).** **NEW** — not previously requested. This letter is both the notification and the change-set. It closes the last real-corpus identity defect the Sprint-3 scale sweep found.

Both are **additive to already-emitted forms**: only qualnames that previously *collided* or *dropped a whole file* change. A correctly-conforming frontend re-vendors the corpus and implements the two rendering rules below; nothing already-passing regresses except the rows that encode the fixed behavior.

---

## 2. Amendment 4 — `escape_reserved(strip_ws(arg))` for every concrete generic arg

**The gap.** A concrete generic argument that is itself a `::`-path (`impl From<std::io::Error> for Foo`, ubiquitous in real Rust) rendered a colon-bearing locator. Loomweave's `entity_id()` rejected it and collapsed the **whole cleanly-parsed file** to one `syntax_error` module (38 valid files dropped across the QA corpora; on Loomweave itself, `plugin/host.rs` — so `PluginHost` was absent from our own graph). Wardline emitted the same bytes un-gated. The corpus (single-segment args only) could not see the gap.

**The rule.** Every concrete generic argument — **type or const**, in **both** the trait fragment and the self-type prefix — renders through one shared pipeline:

```
escape_reserved(strip_ws(arg))
```

- `strip_ws` first (so const args lose proc-macro2 token spacing: `Foo<{ 1 + 2 }>` → `Foo<{1+2}>`).
- then the **existing injective** escape already corpus-pinned on the cfg path: `%` → `%25` first, then `:` → `%3A`.

Worked examples (now pinned in the corpus):

| construct | emitted qualname fragment |
|---|---|
| `impl From<std::io::Error> for Foo` | `Foo.impl[From<std%3A%3Aio%3A%3AError>]` |
| `impl Foo<std::io::Error>` | `Foo<std%3A%3Aio%3A%3AError>.impl#<>` |
| `impl Foo<{ 1 + 2 }>` | `Foo<{1+2}>.impl#<>` |
| (composing) `impl Foo<{ usize::MAX }>` | `Foo<{usize%3A%3AMAX}>.impl#<>` |

**Rejected alternatives** (do not implement): last-segment truncation (re-opens `io::Error` ↔ `fmt::Error` collision), degrade-whole-file, Wardline-only normalization.

**Self-type fallback (completion).** The escape must ALSO cover a **non-`Type::Path` self type** (reference / tuple / slice / raw pointer) that carries a `::`-path: `impl Serializer for &mut fmt::Formatter` renders `&mutfmt%3A%3AFormatter`, not the raw-colon form that drops the file. A `:`-free fallback (`&Foo`, `(A,B)`) is unchanged. (This was the 38th dropped file in the Sprint-3 sweep, mis-attributed to concrete generic args.)

**New corpus rows:** `path_typed_generic_arg_trait`, `path_typed_generic_arg_inherent`, `const_generic_arg_spacing`, `reference_self_type_path_escape`.

**Wardline action:** apply `escape_reserved(strip_ws(arg))` at every concrete-generic-arg render site AND every non-`Type::Path` self-type fallback in the tree-sitter frontend (trait fragment + self-type prefix, type + const args, reference/tuple/slice/ptr self types). Do **not** relax your id-validator's `:` rejection — the escape happens in the producer, before the id is assembled.

---

## 3. Amendment 5 — method-level `@cfg(...)` for cfg-twin methods

**The gap.** The `@cfg(...)` discriminant was applied to impl *keys* and module-level free *items*, but **never to `ImplItem::Fn`**. Two cfg-gated twin methods that land on the **same impl entity** with the same name — inside **one** impl block, or across **several blocks that merge** (same `(type, sig, no-cfg)`) — both rendered `…Foo.impl#<>.go` and the writer silently kept one (chimera entity; SEI/taint keyed 2–3 functions as one). Every qualname collision the Sprint-3 sweep found (ripgrep 8, serde 4, tokio `AsyncWrite::poll_*`, rust-analyzer) is this gap.

**The rule.** A cfg-gated twin **method** carries its own `@cfg(<pred>)` suffix **after the method name**, exactly as a free item does, composing on top of any impl-level `@cfg`:

```
impl Foo { #[cfg(unix)] fn go(&self){} #[cfg(windows)] fn go(&self){} }
  → demo.m.Foo.impl#<>.go@cfg(unix)
  → demo.m.Foo.impl#<>.go@cfg(windows)
```

Twin-ness is computed against the **final** impl qualname (post impl-level cfg) + method name, so:

- an impl-level cfg-twin (`#[cfg(unix)] impl Foo {…} #[cfg(windows)] impl Foo {…}`) — already split into distinct impl entities — gets **no redundant** method suffix;
- a method-twin *inside* a cfg-twin block still gets one (composes: `…impl#<>@cfg(unix).go@cfg(a)`);
- methods that merge across blocks (`impl Foo { #[cfg(unix)] fn go }` + `impl Foo { #[cfg(windows)] fn go }`) are counted across all merged blocks.

The `@cfg(<pred>)` predicate uses the **same normalization** as the impl/free-item cfg suffix (whitespace-stripped, `any()`/`all()` args sorted, all stacked cfgs folded `&`-joined and sorted, reserved chars escaped).

**Rejected alternatives:** a per-method *signature* discriminant (cfg twins have byte-identical signatures), a source-order ordinal (the collision is *within* one merged entity).

**New corpus rows:** `method_cfg_twin_inherent`, `method_cfg_twin_trait`, `method_cfg_twin_cross_merged_block`.

**Wardline action:** in your frontend, apply the cfg discriminant to impl-member methods, keyed on the final impl key + method name (mirroring the merge semantics above). Open a Wardline ticket for this (analogous to your Amendment-4 tickets); there is no prior request to reference because this amendment is new.

---

## 4. What Wardline must do (checklist)

1. **Re-vendor** `fixtures/qualnames_rust.json` (md5 `bf8d09968b5d366a8bd033710d736744`, 35 rows) → `tests/conformance/qualnames_rust.json`, verbatim.
2. **Amendment 4:** route every concrete generic arg (type + const, trait fragment + self-type prefix) through `escape_reserved(strip_ws(arg))`.
3. **Amendment 5:** apply the method-level `@cfg(...)` suffix to cfg-twin `ImplItem::Fn`, keyed on the final impl key + method name.
4. **Re-run** your byte-for-byte parity gate; it should now exercise all 6 new rows.
5. Close `wardline-be5ee9cc34` / the const-spacing half of `wardline-e8f7c0508f` (Amendment 4) and your new Amendment-5 ticket in lockstep.

**Loomweave-side state:** producer implemented (`qualname.rs` arg renderer + `extract.rs` method-twin discriminant), 6 corpus rows landed, conformance + dogfood-uniqueness green, the 38 previously-dropped files now ingest, and the Sprint-3 collision count (8/4/15/15) is zero. No Loomweave action remains except this handoff.
