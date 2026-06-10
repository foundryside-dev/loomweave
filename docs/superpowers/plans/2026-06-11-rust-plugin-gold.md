# Rust Plugin Gold Closeout — Sprint 4 plan (2026-06-11)

Worktree `feat/rust-plugin-gold` off rc4 `d05cb1a`. Parent ticket `clarion-363a6ca7d3`
(thrusts `clarion-b748239b70` / `clarion-8d9e1e00e9` / `clarion-fc2cb8aff2`).
Defect tickets `clarion-dfeb905f46` (A) + `clarion-8245039f6b` (B) claimed `fixing`.

## Decisions baked (user-confirmed 2026-06-11)

- **D1 — Fix BOTH defects, hand off A.** Implement B (signed-off Amendment 4) AND A
  (new Amendment 5) Loomweave-side. Prepare Wardline handoff letters in
  `docs/federation/`; **never push at `~/wardline` (on rc5)**.
- **D2 — Re-baseline on all 4 pinned corpora** (re-cloned at SHAs to
  `~/corpora/{ripgrep,serde,tokio,rust-analyzer}`; DONE).
- **D3 (mine)** — absorb `clarion-a65cb18b02` (wrong-language `LMWV-PY-SYNTAX-ERROR`)
  + `clarion-69db8b2739` (entity_resolve python-only): small, operator-facing, no
  cross-product cost.
- **D4 (mine)** — edit `www/` source for factual drift; **no deploy** (user-gated).

## Verified ground truth (recon 2026-06-11)

- **Defect B is producer-rendering only.** `escape_reserved` + `strip_ws` already
  exist in `crates/loomweave-plugin-rust/src/qualname.rs`; `escape_reserved` is wired
  ONLY into `normalise_pred` (cfg path). The 4 generic-arg call sites bypass it:
  `trait_generic_args` Type@165 + Const@167, `self_ty_arg`@250, `self_ty_locator`
  const@224. Const arms also skip `strip_ws` (raw `to_token_stream`). **`entity_id.rs`
  stays untouched** — its `:` rejection IS the contract (Amendment 4 changes nothing
  there).
- **Defect A is self-contained in `emit_impl`** (`extract.rs:953`): method qualname
  `format!("{impl_q}.{}", m.sig.ident)` never consults `m.attrs`. Fix = per-block
  method-twin pre-pass (mirror module-level `twin_counts` @439-471) + append
  `cfg_suffix(&m.attrs)` when the method name is a cfg-twin within the block. Helpers
  `cfg_suffix`/`cfg_predicates` (@1112-1137) already work on any `&[Attribute]`.
- **No live duplicate-id warning.** `SymbolTable::duplicate_ids()` (`symbol_table.rs:43`)
  has only one non-test caller (`examples/qualname_check.rs`). Writer absorbs collisions
  via `ON CONFLICT(id) DO UPDATE` last-write-wins → silent chimera. Visibility fix in
  scope (see Thrust 1C).
- **Conformance corpus has NO hash guard.** Only `qualname_conformance.rs` asserts
  byte-for-byte; `expected[]` must be **generated from the live extractor**, never
  hand-authored. No `check-*.py` pins the fixture. `corpus_kinds_are_known` allowlist =
  10 kinds (mirrors `plugin.toml` lines 31-42); new rows use only vetted kinds.
- **Wardline:** Wave A (cfg-twin impls) closed both sides. Amendment 4 (defect B):
  decided + **owner-signed-off**, unimplemented both sides; Loomweave owns next move.
  Amendment 5 (defect A): new, unagreed. Wardline on `rc5` → handoff only.
- **Baseline BEFORE numbers (rc4 @ 48bdafd):** collisions ripgrep 8 / serde 4 /
  tokio 15 / rust-analyzer 15 / dogfood 0 → target 0. Reserved-`:` drops 3 / 2 / 18 /
  15 = 38 total → target 0. Run via `tests/qa/run_corpus_qa.sh <venv-bin> <corpus>
  <out> target/release/examples/qualname_check`.
- **Docs drift:** ADR-049 index row bare `Accepted` (no amended marker); 3 design-ladder
  falsehoods (`CLAUDE.md:14-15`, `requirements.md:1263` NG-15, `:1323` NG-25);
  `getting-started.md` 100% Python-framed + stale `doctor` v2.0 line @368-370; no
  known-limitations doc; www tool-count self-contradictory (~40 vs ~42, real 51) +
  Rust invisible + edge-kinds stale (`clarion-obs-b703534b91`).

## Sequencing (dependency-correct)

```
Thrust 1B (defect B, qualname.rs) ─┐
Thrust 1A (defect A, extract.rs)  ─┼─> Thrust 2 (corpus rows + Wardline letter)
Thrust 1C (dup-id finding)        ─┘        │
                                            └─> Thrust 3 (docs that describe behavior)
Thrust 4 (Phase-3 memo) — independent, any time
Absorbs a65cb18b02 / 69db8b2739 — independent code, parallelizable
```

Code fixes (1A/1B/1C + absorbs) are strict TDD, done in-worktree by the lead (identity
contract — direct control). Docs (Thrust 3) + memo (Thrust 4) fan out as a workflow.

## Thrust 1B — Defect B (reserved-`:` generic args) [TDD]

1. **RED:** add `path_typed_generic_arg_inherent` / `_trait` + `const_generic_arg_spacing`
   to `fixtures/qualnames_rust.json` with the **amendment-mandated** expected strings
   (`Foo<std%3A%3Aio%3A%3AError>.impl#<>`, `Foo.impl[From<std%3A%3Aio%3A%3AError>]`,
   `Foo<{1+2}>.impl#<>`). `entities_match_byte_for_byte` goes red (extractor still emits
   raw `:` → id rejected → file degrades). Also add an integration case in
   `identity_uniqueness.rs` asserting `impl From<std::io::Error>` does NOT collapse to
   `syntax_error`.
2. **GREEN:** in `qualname.rs`, introduce one arg renderer `render_arg(arg) =
   escape_reserved(strip_ws(arg))` and apply at all 4 sites (Type@165, Const@167,
   self_ty_arg@250, self_ty_locator const@224). Keep bare self-type base un-escaped
   (corpus-pinned). Preserve escape ordering (`%`→`%25` before `:`→`%3A`). Positional
   `$N` branch untouched.
3. **VERIFY:** `cargo nextest run -p loomweave-plugin-rust`; corpus rows green.

## Thrust 1A — Defect A (cfg-twin methods) [TDD]

1. **RED:** add corpus rows `method_cfg_twin_inherent` / `_trait` to
   `fixtures/qualnames_rust.json` (two cfg-gated `fn go` in one impl block → distinct
   `…@cfg(unix).go` / `…@cfg(windows).go`) + a focused test in `identity_uniqueness.rs`
   asserting two distinct ids (mirror `cfg_discriminant_is_load_bearing...` @107). Red:
   collision today.
2. **GREEN:** in `emit_impl` (`extract.rs:953`): build a per-block method-name twin
   count over `it.items` (`ImplItem::Fn` only); after `let q = format!("{impl_q}.{}", …)`,
   if twin && `cfg_suffix(&m.attrs)` is Some, push the suffix. Composes with impl-level
   `@cfg` already in `impl_q`. Key on method-name only.
3. **VERIFY:** `qualname_check` over a synthetic twin → 0 duplicates;
   `cargo nextest run -p loomweave-plugin-rust`.

## Thrust 1C — Duplicate-id visibility finding [TDD, bounded]

- Surface `SymbolTable::duplicate_ids()` as an analyze-time finding (WARN/ERROR) so a
  future collision is never silent. Smallest landing: emit in the plugin after symbol-table
  build (where `qualname_check` reads it) → host stamps a finding. **Bounded check:** if
  wiring exceeds ~1 file of change or touches the writer contract, STOP and file
  `clarion`-with-dependency on the parent instead (per prompt). Decision recorded in the
  report either way.

## Thrust 2 — Corpus rows + Wardline handoff

- The corpus rows land WITH 1A/1B (generated from live extractor — run `extract_file` on
  each snippet, paste emitted `(qualname, kind)` verbatim). No `check-*.py` trips.
- **Handoff letter** `docs/federation/2026-06-11-rust-qualname-amendment-4-5-changeset.md`:
  records Amendment 4 (now implemented + rows landed) and Amendment 5 (new — cfg-twin
  methods, additive, root-cause table), the new corpus blob hash, and the Wardline
  re-vendor + re-implement obligation (`wardline-be5ee9cc34`, `wardline-e8f7c0508f`, plus
  a NEW wardline ticket request for Amendment 5). **No write to `~/wardline`.** Escalate
  the handoff to the user at exit.

## Thrust 3 — Documentation gold (fan-out)

- **Design ladder:** `CLAUDE.md:14-15` (Rust plugin shipped in 1.x; only Java/TS v2.0+);
  `requirements.md` NG-15 `:1263` (add `> **Amended:**` blockquote, drop Rust from
  deferred); NG-25 `:1323` (Rust plugin now consumes the descriptor).
- **ADR index:** `docs/loomweave/adr/README.md` ADR-049 row → `Accepted; amended`; add
  the Amendment 5 block + Status-line enumeration in `ADR-049-*.md`.
- **Known-limitations doc:** new `docs/operator/rust-known-limitations.md` (macro bodies
  unexpanded, external edge targets dropped/derives sparse, closures/nested-fns not
  entities, references envelope boundaries `clarion-efc8715d98`, parse-guard caps +
  operator-visible trip behavior, watchdog deadlines + env overrides, cfg-twin/`const _`
  residuals, pure-Rust dead-code signal gap `clarion-e1899a109f`). Link from
  getting-started.
- **Operator:** `getting-started.md` — add Rust plugin to prerequisites + install + the
  separate wheel; fix stale "doctor is v2.0 roadmap" @368-370; state doctor per-plugin
  discovery.
- **www/** source: reconcile tool-count to one current figure; add Rust analyzability;
  add `implements`/`derives`/`inherits_from`/`decorates` to edge-kind lists
  (`index.html:50-51,113`, `concepts.html:122`) — closes `clarion-obs-b703534b91`.

## Thrust 4 — Phase-3 go/no-go memo

- `docs/implementation/2026-06-11-phase3-rust-analyzer-go-no-go.md`: demand = unresolved
  call-site rates (Sprint 2/3); cost = bounded-additivity (RA must reproduce ADR-049
  byte-for-byte or fork every id). One-sentence recommendation + tickets to file. Do NOT
  start Phase 3.

## Absorbs (parallel code, TDD)

- `clarion-a65cb18b02` — host stamps hardcoded `LMWV-PY-SYNTAX-ERROR` on every plugin's
  degraded files. Make the degraded-file rule id plugin-prefix-aware (analyze.rs). Red
  test: a degraded Rust file gets `LMWV-RUST-SYNTAX-ERROR` only, no PY twin.
- `clarion-69db8b2739` — `entity_resolve` hardcodes `python:function:`. Make
  candidate-minting kind/plugin-agnostic so Rust qualnames resolve. Red test: resolve a
  known Rust qualname.

## Acceptance gate (paste real output — verification-before-completion)

1. `run_corpus_qa.sh` on all 4 corpora → collisions 0, reserved-`:` drops 0, the 38
   files + `plugin/host.rs` (PluginHost in our own graph) ingest. Before/after quoted.
2. `qualname_conformance.rs` green with new rows; corpus blob re-hashed; Wardline letter
   committed (no `~/wardline` write).
3. SEI churn: re-analyze Loomweave + one corpus before/after — re-mint confined to
   previously colliding/dropped entities; unaffected entities zero churn (`index_diff`).
4. Docs reconciled; ADR index accurate; known-limitations linked; Phase-3 memo committed.
5. Full CLAUDE.md floor green (Rust + Python gates, lockstep guards, **bins before
   nextest**) + all `tests/e2e/*.sh` incl `hostile_corpus_rust.sh` +
   `rust_plugin_wheel_smoke.sh`.
6. Tracker: absorbed tickets closed w/ reason; leave-behinds commented; parent last.

## Leave-behinds (file/comment, not absorb)

`clarion-feab311907` (deleted-file edge staleness — pre-existing, not Rust-specific),
`clarion-14398b2536` (subsystem count instability — analysis-layer determinism, entities
stable), `clarion-f3eb3852d6` (Python recursion characterization — contained by ADR-050),
`clarion-efc8715d98` (references envelope — P3 MVP deferral, documented as limitation),
`clarion-271287b54b` (plugin_limits yaml — P3 config, env-vars cover it),
`clarion-e1899a109f` (pure-Rust dead-code tags — documented as limitation),
`const _` unnamed-const collision (sibling of A — file new ticket w/ dependency).
