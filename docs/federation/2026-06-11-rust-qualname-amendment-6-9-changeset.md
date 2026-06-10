# Loomweave → Wardline: Rust qualname dialect — Amendments 6–9 change-set / handoff

**From:** Loomweave maintainers (Rust plugin)
**To:** Wardline engineering (Rust tree-sitter frontend)
**Date:** 2026-06-11
**Re:** Four qualname-dialect amendments now **implemented, adversarially review-hardened, and landed** in `crates/loomweave-plugin-rust` (rc4 HEAD `4de391e`). Together with Amendments 4–5 they take the four Sprint-4 gold-blocker collision families — and the real-corpus collision count — to **zero**. Wardline's second-producer frontend and its vendored corpus must mirror all four.
**Status:** **HANDOFF — action required on Wardline's side, but NOT pushed by Loomweave.** Wardline's Rust frontend remains on its `rc5` release branch; per cross-repo discipline Loomweave does **not** commit dialect changes into a sibling's release branch. This letter + the re-vendorable corpus blob is the handoff; the Loomweave owner will surface it for scheduling.
**Relationship to the Amendments 4–5 letter:** this letter **extends and supersedes** `2026-06-11-rust-qualname-amendment-4-5-changeset.md` for the corpus step. **ONE re-vendor of the blob pinned below covers Amendments 4+5+6+7+8+9** — do not vendor the 35-row blob the 4–5 letter pins; its §2–§3 rendering rules (generic-arg escape pipeline, method-level `@cfg`) remain normative as written and are *included in* this batch's obligations.
**Authority:** Loomweave remains the authoritative producer for the Rust dialect (**ADR-049**, amended through 2026-06-11 — all eight amendments). The corpus `expected` values are generated from the live extractor, never hand-authored; where this document and your frontend diverge, **Loomweave's emitted form is normative and Wardline conforms.** The amendment paragraphs in [`ADR-049`](../loomweave/adr/ADR-049-rust-qualname-canonicalization.md) (Amendments 6, 7, 8, 9) are the **normative text**; this letter summarizes, it does not re-derive.
**Corpus blob:** `fixtures/qualnames_rust.json` now carries **49 entity rows** plus a **new `module_mounts` section (8 rows)** (md5 `a784a2f97e2079c71b7aba87c11694dd`), up from 35 entity rows / no mounts section (md5 `bf8d09968b5d366a8bd033710d736744` in the 4–5 letter). Re-vendor verbatim to `tests/conformance/qualnames_rust.json`.
**Version:** No `ontology_version` bump — Amendments 6–8 refine qualname *strings* of existing kinds; Amendment 9 is an emitted-entity-**SET** amendment (`const` remains the kind for every *named* const).

---

## 1. Context — the residual-collision ladder, stated once

The Sprint-4 gold QA sweep (a per-id enumeration the Sprint-3 sweep lacked) found that the 27 residual real-corpus collisions were **four distinct dialect families**, none of them cfg-twin-methods. Amendments 6–9 close them. Amendments 6 and 7 share one normative mechanism — the **residual-collision ladder** (ADR-049 Amendment 6, where it is specified in full). Per extraction unit, impl qualnames are decided in **four stages, each keyed on the previous stage's output**:

```
(1) @cfg  →  (2) stage S (self-type written path)  →  (3) stage T (trait written path)  →  (4) method-@cfg
```

- **(1) `@cfg`** — the existing Amendment-1/5 impl-level machinery, computed on the **bare pre-cfg keys** exactly as today, so already-`@cfg`-split twins keep their current ids byte-for-byte.
- **(2) S** — Amendment 6: post-cfg qualname groups with ≥ 2 distinct self-type **written-path witnesses** re-render each qself-free `Type::Path` member's base as the escaped written path.
- **(3) T** — Amendment 7: post-S groups with ≥ 2 distinct trait written paths switch every member's `impl[…]` fragment to the escaped written trait path.
- **(4) method-`@cfg`** — Amendment 5, unchanged mechanics, keyed on the **final (post-S/T)** impl qualname + method name.

The ladder is **twin-gated** end to end: a lone impl never qualifies, un-fired groups change nothing, and cross-path cfg-twins (split at stage 1) leave S cold — pinned by `trait_path_cfg_twin_unqualified` and the `cross_path_cfg_twins_keep_todays_cfg_ids` stability test.

---

## 2. Amendment 6 — self-type written-path qualification (stage S)

Closes `clarion-8ff7f233fa`. The impl locator's `<Type>` base rendered only the **last path segment** of a path-qualified self type, so `impl Semaphore for bounded::Semaphore` + `impl Semaphore for unbounded::Semaphore` (tokio `chan.rs`) both rendered `…chan.Semaphore.impl[Semaphore]` and five like-named method ids silently collapsed at the writer's `ON CONFLICT(id) DO UPDATE` (a chimera entity).

**The rule (summary — ADR-049 Amendment 6 is normative).** Impls are grouped by **post-cfg** qualname. Each member's *witness* is its **written self-type path**: for a qself-free `Type::Path`, segment idents joined `::` — a leading `::` contributing a leading separator, so `impl Tr for ::a::X` and `impl Tr for a::X` carry distinct witnesses — with **no generic arguments** (args are already normalized and must not affect the witness); for a non-`Type::Path` or qself-bearing self type, the Amendment-4 textual render. A group with ≥ 2 distinct witnesses re-renders every qself-free `Type::Path` member's base as `escape_reserved(witness)` (`{m}.a%3A%3AX.impl[T]` vs `{m}.b%3A%3AX<$0>.impl#<>`; a leading `::` renders a leading `%3A%3A`); a qself-bearing or non-`Type::Path` member keeps its single-escaped Amendment-4 fallback (re-applying would double-escape). A single-segment witness renders byte-identically to today.

**Known residuals (by design):** identical-written-path coherence-illegal twins still collide (no witness can split them; reported by `duplicate_ids()`); the witness is as-written, so alias-vs-full-path re-spelling churns the id — but only while a twin exists.

**New corpus rows:** `self_type_path_twin_trait`, `self_type_path_twin_inherent`, `self_type_path_lone_unqualified_negative`, `self_type_path_mixed_bare_and_qualified`, `self_type_path_leading_colon_twin` (review-hardening: leading-`::` witness symmetry with stage T).

**Wardline action:** implement stage S in the tree-sitter frontend. The witness is **single-file computable** — written paths, no name resolution — which is exactly the second-producer constraint the rejected always-qualify and crate-root-normalization alternatives failed.

---

## 3. Amendment 7 — trait-path written-path qualification (stage T)

Closes `clarion-fa8bcf8731`, the sibling family. The `impl[…]` fragment keyed on the trait path's **last segment** only, so `impl tokio::io::AsyncRead for Compat<T>` + `impl futures_io::AsyncRead for Compat<T>` (tokio_util `compat.rs`) both rendered `Compat<$0>.impl[AsyncRead]` and five method ids collapsed. The cfg machinery *detects* this but cannot split it (no `#[cfg]` on either impl).

**The rule (summary — ADR-049 Amendment 7 is normative).** Impls are grouped by **post-S** qualname; each trait-impl member's rendering-witness is the **trait path as written** — `escape_reserved` over the `::`-joined segment idents (a leading `::` contributes a leading `%3A%3A`), final segment keeping the existing `trait_generic_args` rendering. A fired group switches **every** member's `impl[…]` fragment to the qualified rendering: `Compat<$0>.impl[tokio%3A%3Aio%3A%3AAsyncRead]` vs `Compat<$0>.impl[futures_io%3A%3AAsyncRead]`. A single-segment written path renders byte-identically to the bare fragment; inherent impls never fire T. Running T **after** S yields minimal qualification: a pair already split by S leaves T cold (`impl_ladder_self_splits_before_trait`). `implements`-edge resolution and the SEI signature's last-segment `target` are untouched.

**New corpus rows:** `trait_path_twin`, `trait_path_twin_single_vs_multi`, `trait_path_lone_multi_segment`, `trait_path_cfg_twin_unqualified`, plus the shared ladder rows `impl_ladder_self_splits_before_trait` and (review-hardening) `impl_ladder_self_then_trait_residual` (S fires, a residual group still fires T) and `method_cfg_twin_in_s_fired_merged_blocks` (stage 4 keyed on the final post-S/T qualname).

**Wardline action:** implement stage T, grouped on post-S qualnames, single-file computable as above. Note the qualified rendering is a *separate* render path applied only inside a fired group — the corpus-pinned bare fragment is untouched for un-fired groups.

---

## 4. Amendment 8 — `#[path]` mount overlay (module routing)

Closes `clarion-bdb1eccf48` and supersedes ADR-049 §1's original `#[path]` deferral. Two producers minted the same module id: the file walk routed a mounted file by filesystem path (tokio `src/process/unix/mod.rs` → `tokio.process.unix`, ignoring its `#[cfg(unix)] #[path = "unix/mod.rs"] mod imp;` mount) while the AST walk emitted the inline facade `mod unix { … }` at the same dotted path — `rust:module:tokio.process.unix` emitted from two files.

**The rule (summary — ADR-049 Amendment 8 is normative; it is long, read it).** A targeted **mount overlay with a filesystem default**: at `initialize`, every literal `#[path = "…"] mod name;` declaration is collected under rustc's relative-path rule and resolved through a memoized fixed point (mounts chain; cycles drop to filesystem fallback; a doubly-claimed target resolves first-by-sorted-(declaring-file, offset)). `logical_module_path` then routes every emission: exact mount hit, else longest mounted-subtree prefix, else the unchanged pure-filesystem `module_path_for`. Twin mounts are counted **across both inline-`mod` and decl-`mod` forms** per declaring item list and split by `@cfg`; a mount declared *inside* a cfg-twin inline mod composes that mod's `@cfg`-suffixed segment into its logical **prefix** (the review-hardening family — composing the bare name routed both twins' targets to one id). Tokio after: mounted trees route `tokio.process.imp@cfg(unix)` / `…imp@cfg(windows)`, the facades keep `tokio.process.unix` / `….windows` — four distinct ids, and every file under a mounted directory re-keys with it.

**Invisible by dialect rule (do not resolve these):** a **macro-wrapped** mount (inside an unexpanded macro invocation) and a **`#[cfg_attr(pred, path = "…")]`-delivered** mount are NOT mounts — only a literal `#[path]` attribute is. Their targets route by filesystem fallback. No producer expands macros or evaluates cfg predicates.

**Corpus:** the **new `module_mounts` section** (8 rows) pins the mounted routes end-to-end: `path_mount_dir_module`, `path_mount_child_prefix`, `path_mount_chain`, `path_mount_macro_invisible_fallback`, `path_mount_inline_nested_decl`, `path_mount_inside_cfg_twin_inline_mod`, `path_mount_lone_cfg_no_suffix`, `path_mount_one_file_two_mounts_first_wins`. The existing `module_route` row `path_attr_known_gap` is **de-gapped**: re-pointed from a known-wrong-emission pin to a FALLBACK pin of `module_path_for`'s (unchanged) no-mount-context route — a route with no mount covering the file MUST be byte-identical to it.

**Wardline action:** implement the overlay — mount discovery needs the **parent chain** (a declaring file's directory anchors the relative target), so this is **sp2 scope**, not single-file. Note module routing was **already sp2 for you** (`path_attr_known_gap` has carried `reproducibility: "sp2"` since it was first vendored); the overlay extends that existing scope rather than breaching the single-file constraint anywhere new. The entity-row witnesses of §§2–3 remain single-file.

---

## 5. Amendment 9 — unnamed `const _` skip-emission

Closes `clarion-83870dc534` — the **largest** residual family: all 15 rust-analyzer residuals were repeated `const _: () = …;` compile-time assertions in one module, every pair rendering the identical `…<module>._`.

**The rule (summary — ADR-049 Amendment 9 is normative).** An `Item::Const` whose ident is `_` is **not an entity**: no entity, no `contains` edge, no Phase-2 `references` sites (a finding inside one attributes to the enclosing module, like nested fns and closures). The skip is **unconditional on `ident == "_"`** — a lone anonymous const is skipped too, because skip-only-when-twinned would make the emitted set sibling-dependent and churn SEI. `_` is non-identifying by construction: nothing can ever name the item, so no discriminant can rescue it (ordinal, span, content-hash, and cfg discriminants are all rejected in the ADR text). Module-level only by construction (rustc/syn reject the other positions).

**New corpus rows:** `unnamed_const_skip` (named `LIMIT` + two `const _` twins → only the module and `LIMIT` emit; the skip pinned as an ABSENT expected row, precedent `nested_fn_is_not_an_entity`) and `unnamed_const_cfg_twin_skip`.

**Wardline action:** skip `const _` at the free-item emission arm. Your **parity gate self-enforces this once the corpus is vendored** — `expected` arrays are complete emissions, so an extra `…._` row fails the gate without any new frontend assertion.

---

## 6. Real-corpus acceptance evidence

The Sprint-4 gold sweep's 27 residual collisions go to **zero** at the same pinned SHAs, re-swept post-review-hardening at rc4 HEAD `4de391e`:

| corpus | commit | collisions (was) |
|---|---|---|
| ripgrep | `82313cf9` | **0** (was 0) |
| serde | `5f0f18b9` | **0** (was 0) |
| tokio | `2e7930fe` | **0** (was 12) |
| rust-analyzer | `587ce15e` | **0** (was 15) |

Oracle: `cargo run -p loomweave-plugin-rust --example qualname_check` (the `duplicate_ids()` enumeration).

---

## 7. Riding the same handoff: `ResolveRequest` plugin-hint (OPEN, not yet implemented)

Loomweave's qualname resolver is now plugin-aware (ADR-036, amended 2026-06-11, `clarion-69db8b2739`): a qualname owned by more than one plugin resolves `Ambiguous`, and **the federation wire degrades ambiguous to `unresolved`** — indistinguishable from not-found in the `ResolveResponse` you consume. The eventual disambiguator is a **plugin-hint field on `ResolveRequest`**. That struct is `#[serde(deny_unknown_fields)]` on the Loomweave side, so Wardline cannot simply start sending the field — adding it is a **deliberate, coordinated contract change** to the resolve wire shape. It is deferred to, and rides, this same escalation-gated rc5 handoff: when the owner schedules the 4–9 re-vendor, the plugin-hint shape should be agreed in the same exchange. See ADR-036's 2026-06-11 amendment paragraph for the normative resolver semantics.

---

## 8. What Wardline must do (checklist)

1. **Re-vendor** `fixtures/qualnames_rust.json` (md5 `a784a2f97e2079c71b7aba87c11694dd`; 49 entity rows + 6 `module_route` rows + 8 `module_mounts` rows) → `tests/conformance/qualnames_rust.json`, verbatim. **This one re-vendor covers Amendments 4–9** — skip the 35-row blob from the 4–5 letter.
2. **Amendments 4–5** (if not yet implemented): the obligations in the 4–5 letter stand as written — `escape_reserved(strip_ws(arg))` at every generic-arg and non-`Type::Path` self-type render site; method-level `@cfg` on cfg-twin `ImplItem::Fn`.
3. **Amendments 6–7:** implement the residual-collision ladder (`@cfg → S → T → method-@cfg`), witnesses computed from written paths (single-file), method-`@cfg` re-keyed on the final post-S/T impl qualname.
4. **Amendment 8:** implement the mount overlay in your sp2 module-routing layer (parent-chain discovery, rustc relative-path rule, chained/cycling/doubly-claimed resolution, cross-form `@cfg` twin rule incl. the inline-mod-prefix composition). Treat macro-wrapped and `cfg_attr`-delivered mounts as invisible.
5. **Amendment 9:** skip `const _` at the free-item arm; your parity gate then self-enforces it.
6. **Re-run** the byte-for-byte parity gate; it should exercise all 14 new entity rows, the 8 mount rows, and the de-gapped `path_attr_known_gap` fallback pin.
7. Open Wardline tickets per amendment (analogous to your Amendment-4 tickets — there is no prior Wardline request to reference; all four families were found by Loomweave's Sprint-4 sweep) and agree the `ResolveRequest` plugin-hint shape in the same exchange (§7).

**Loomweave-side state:** all four amendments implemented and landed (`f7f8a69` const-_-skip, `c4791aa` S+T ladder, `05b44f3` mount overlay, `4de391e` adversarial-review hardening incl. one **new** family — cfg-twin-inline-mod mount prefixes — found and fixed pre-handoff), 14 entity rows + the `module_mounts` section landed, conformance + dogfood-uniqueness + the full CI floor green (1643 workspace tests), and the real-corpus collision count is **0/0/0/0**. No Loomweave action remains except this handoff and the §7 contract change.
