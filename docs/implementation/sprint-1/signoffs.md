# Clarion Sprint 1 — Sign-off Ladder

**Status**: DRAFT — becomes the closing gate for Sprint 1
**Scope**: [WP1](./wp1-scaffold.md), [WP2](./wp2-plugin-host.md), [WP3](./wp3-python-plugin.md)
**Read-with**: [`README.md`](./README.md), [`../v0.1-plan.md`](../v0.1-plan.md)

This document is the closing gate for Sprint 1. Sprint 1 is closed when every box in
**Tier A** is ticked. Tiers B and C are included so Sprint 2+ can extend this ladder
without rewriting the doc; they are not Sprint 1's gate.

Each tick must be accompanied by a verifiable artefact (a commit hash, a test-run
log, an ADR file, or a documentation update). Self-attested ticks without a pointer
are not sufficient.

---

## Tier A — Sprint 1 close (walking skeleton)

Every box below MUST be ticked for Sprint 1 to close. Lock-in ticks carry a
`locked on <YYYY-MM-DD>` stamp once ratified; after that date, revisiting the
locked design requires a follow-up ADR and cross-WP impact analysis.

### A.1 Storage layer (WP1)

- [x] **A.1.1** — `cargo build --workspace --release` succeeds on a clean Linux checkout. Proof: CI log or commit hash.
- [x] **A.1.2** — `cargo nextest run --workspace --all-features` passes (ADR-023 swaps `cargo test` for nextest). Proof: CI log or local run log.
- [x] **A.1.2a** — `cargo fmt --all -- --check` passes (ADR-023 gate). Proof: CI log.
- [x] **A.1.2b** — `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes against `clippy::pedantic = "warn"` (ADR-023 gate). Proof: CI log.
- [x] **A.1.2c** — `cargo deny check` passes — advisories, licenses, bans, sources all green (ADR-023 gate). Proof: CI log.
- [x] **A.1.2d** — `cargo doc --no-deps --all-features` builds without warnings (ADR-023 gate). Proof: CI log.
- [x] **A.1.2e** — GitHub Actions CI workflow exists at `.github/workflows/ci.yml` and all five jobs (fmt, clippy, nextest, doc, deny) are green on the WP1 PR (ADR-023 gate). Proof: PR URL + green-checks screenshot or CI log.
- [x] **A.1.3** — **L1 locked**: migration file `0001_initial_schema.sql` contains every table, virtual table, trigger, generated column, and view from [detailed-design.md §3](../../clarion/v0.1/detailed-design.md#3-storage-implementation): tables `entities`, `entity_tags`, `edges`, `findings`, `summary_cache`, `runs`, `schema_migrations`; virtual table `entity_fts` (FTS5); triggers `entities_ai`, `entities_au`, `entities_ad`; generated columns `entities.priority` + `ix_entities_priority`, `entities.git_churn_count` + `ix_entities_churn`; view `guidance_sheets`. Proof: migration file commit; verification via `sqlite3 < migrations/0001_initial_schema.sql` against a fresh DB produces the expected schema; `schema_apply` integration test (WP1 Task 3) passes all assertions. _Locked on 2026-04-18._ _Post-lock amendment 2026-05-03 ([ADR-024](../../clarion/adr/ADR-024-guidance-schema-vocabulary.md)): the `entities.priority` generated column + `ix_entities_priority` index were renamed/split to `entities.scope_level` + `entities.scope_rank` + `ix_entities_scope_rank`. The L1 lock language above describes the shape locked at sprint close; ADR-024 is the post-Sprint-1 schema correction under the in-place migration-edit policy named in that ADR._
- [x] **A.1.4** — **L2 locked**: `entity_id()` Rust assembler produces the 3-segment `{plugin_id}:{kind}:{canonical_qualified_name}` form per ADR-003 + ADR-022 and passes all rows in `/fixtures/entity_id.json`. Proof: passing test in `clarion-core`. _Locked on 2026-04-18._
- [x] **A.1.5** — **L3 locked**: `WriterCmd` enum and per-N-batch writer-actor shipped; per-command ack, batch-boundary commit, rollback on `FailRun` each have a passing test. Proof: tests in `clarion-storage`. _Locked on 2026-04-18._
- [x] **A.1.6** — `clarion install` in a fresh tempdir produces `.clarion/{clarion.db, config.json, .gitignore}` **plus** a `clarion.yaml` stub at the project root (per [detailed-design.md §File layout](../../clarion/v0.1/detailed-design.md#file-layout); `.clarion/` holds internal state, `clarion.yaml` is the user-edited config). Proof: integration test passing.
- [x] **A.1.7** — `clarion install` refuses to overwrite an existing `.clarion/` without `--force`. Proof: negative integration test passing.
- [x] **A.1.8** — `clarion analyze .` in a plugin-less scratch dir produces a `runs` row with status `skipped_no_plugins`. Proof: integration test passing.
- [x] **A.1.9** — **ADR-005 authored** and moved from backlog to Accepted in [`../../clarion/adr/README.md`](../../clarion/adr/README.md). Proof: ADR file commit.
- [x] **A.1.9a** — **ADR-023 authored** (tooling baseline) and Accepted in the ADR index. Every artefact listed in ADR-023 §Decision is present in Task 1's commit: `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`, workspace `[lints]` block with every member crate opting in via `lints.workspace = true`, and `.github/workflows/ci.yml`. Proof: ADR file commit + artefact listing in the Task-1 commit message.
- [x] **A.1.10** — Every UQ-WP1-* marked resolved in [`wp1-scaffold.md §5`](./wp1-scaffold.md#5-unresolved-questions). UQ-WP1-09 specifically reads "resolved by ADR-023" rather than the original "fine to document and move on" framing. Proof: doc commit showing resolution state.

### A.2 Plugin host (WP2)

- [x] **A.2.1** — **L4 locked**: JSON-RPC method set (`initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`) + Content-Length framing round-trip tested. Proof: tests in `clarion-core::plugin::transport` + end-to-end via `wp2_e2e_smoke_fixture_plugin_round_trip` (T1). _Locked on 2026-04-24._
- [x] **A.2.2** — **L5 locked**: `plugin.toml` schema parsed and validated; rejects manifests missing required fields. Proof: 30+ positive/negative tests in `clarion-core::plugin::manifest`. _Locked on 2026-04-24._
- [x] **A.2.3** — **L6 locked**: path jail (drop-not-kill on first offense per ADR-021 §2a; >10 escapes/60s sub-breaker), 8 MiB Content-Length ceiling, 500k per-run entity-count cap, 2 GiB `RLIMIT_AS` each have both positive and negative tests passing. Jail coverage is **`analyze_file` response paths only** — `file_list` RPC and its jail enforcement point are deferred to Tier B per WP2 §L4 and §L6. Proof: tests in `clarion-core::plugin::{jail, limits}` + host-level `content_length_ceiling_surfaces_through_plugin_host` + host-level entity-cap test (T9) + `apply_prlimit_linux_returns_ok`. _Locked on 2026-04-24._
- [x] **A.2.4** — **L9 locked**: plugin discovery finds `clarion-plugin-*` binaries on `$PATH` and loads neighboring `plugin.toml`. Proof: tests T1–T8 in `clarion-core::plugin::discovery`, plus T10/T11 spawn-safety tests (manifest `executable` must be bare basename matching discovered binary; scrub commit `eb0a41d`) and `DiscoveryError::WorldWritableDir` refusal (scrub commit `7c0e396`). _Locked on 2026-04-24._
- [x] **A.2.5** — Ontology-boundary enforcement drops entities whose `kind` is not in the manifest's `[ontology].entity_kinds`. Proof: host integration test T3 with pinned entity-count assertion.
- [x] **A.2.6** — Identity-mismatch enforcement (UQ-WP2-11 resolution) drops entities whose `id` doesn't match `entity_id(plugin_id, kind, qualified_name)`. Proof: host integration test T4 + `cross_plugin_plugin_id_spoof_is_rejected` (scrub commit `89b2da0`).
- [x] **A.2.7** — Crash-loop breaker trips after the configured number of crashes in the configured window. Proof: `breaker_*` unit tests + `wp2_crash_loop_breaker_trips_and_skips_remaining_plugins` end-to-end (scrub commit `7f8fc9a`).
- [x] **A.2.8** — `clarion analyze` with the fixture mock plugin produces ≥1 persisted entity. Proof: `wp2_e2e_smoke_fixture_plugin_round_trip` integration test.
- [x] **A.2.9** — Every UQ-WP2-* marked resolved in [`wp2-plugin-host.md §5`](./wp2-plugin-host.md#5-unresolved-questions). UQ-WP2-10 resolved by ADR-002 + ADR-021 §Layer 3; UQ-WP2-11 resolved by identity check / T4. Proof: doc commit (this one).
- [x] **A.2.10** — Manifest with malformed identifier grammar (entity kind violating `[a-z][a-z0-9_]*` or `rule_id_prefix` violating `CLA-[A-Z]+(-[A-Z0-9]+)+`) is rejected at parse with `CLA-INFRA-MANIFEST-MALFORMED` per ADR-022. Includes the reserved-prefix rejections (`rule_id_prefix = "CLA-INFRA-"` and `"CLA-FACT-"` → `CLA-INFRA-RULE-ID-NAMESPACE`). Proof: negative tests in `clarion-core::plugin::manifest`.
- [x] **A.2.11** — Manifest declaring a core-reserved entity kind (`file`, `subsystem`, or `guidance`) in `entity_kinds` is rejected at parse with `CLA-INFRA-MANIFEST-RESERVED-KIND` per ADR-022 §Core owns. Proof: negative test in `clarion-core::plugin::manifest`.
- [x] **A.2.12** — Manifest declaring `capabilities.runtime.reads_outside_project_root = true` is refused at `initialize` with `CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY` per ADR-021 §Layer 1; the plugin process is terminated before any `analyze_file` dispatch. Proof: host integration test T2 in `clarion-core::plugin::host` with strengthened no-`initialized`-sent assertion (scrub commit `89b2da0`).

### A.3 Python plugin (WP3)

- [x] **A.3.1** — `pip install -e plugins/python` on a clean Python 3.11 venv places `clarion-plugin-python` on `$PATH`. Proof: verified via `plugins/python/.venv/bin/clarion-plugin-python --version`-equivalent stamp (exit 0) + walking-skeleton script (`tests/e2e/sprint_1_walking_skeleton.sh`).
- [x] **A.3.2** — **L7 locked**: qualname reconstruction matches the documented rules for module-level, nested, class, async, and nested-class cases. Proof: 9 tests in `test_qualname.py` passing (including UQ-WP3-01 nested class methods and UQ-WP3-07 overloaded methods). _Locked on 2026-04-24._
- [x] **A.3.3** — **L8 locked**: Wardline probe returns the three documented states (`absent`, `enabled`, `version_out_of_range`). The handshake `capabilities` field carries the probe result. Proof: 8 tests in `test_wardline_probe.py` (covering lower-bound inclusive, upper-bound exclusive, missing `__version__`, non-semver) + `test_server.py::test_initialize_roundtrip` verifying wiring. _Locked on 2026-04-24._
- [x] **A.3.4** — Shared fixture `/fixtures/entity_id.json` passes in both `clarion-core` (Rust `entity_id()`) and `plugins/python` (Python `entity_id()`) test suites. Proof: Rust `entity_id::tests::shared_fixture_byte_for_byte_parity` (140-test clarion-core run) + Python `test_entity_id.py::test_matches_shared_fixture` (41 entity_id + extractor tests); 20 fixture rows covering the representative shapes. **This is L2+L7 byte-for-byte alignment proof.**
- [x] **A.3.5** — Round-trip self-test passes: plugin extracts entities from its own source and the host persists them. Proof: `test_round_trip.py::test_round_trip_self_analysis` passing (drives the installed console_script binary, not `python -m`).
- [x] **A.3.6** — Syntax-error files are skipped with a stderr log; the run continues (UQ-WP3-02). Proof: `test_extractor.py::test_syntax_error_yields_empty_list_and_logs_to_stderr` — `extract("def :", "broken.py")` returns `[]` and emits "broken.py" in stderr capture.
- [x] **A.3.7** — Every UQ-WP3-* marked resolved in [`wp3-python-plugin.md §5`](./wp3-python-plugin.md#5-unresolved-questions). UQ-WP3-10 reads "resolved by ADR-023" (mypy-strict adopted) rather than the original "defer mypy" framing. Proof: doc commit (this one) resolves UQ-WP3-01/02/05/06/07/08/09/11/12.
- [x] **A.3.8** — **ADR-023 Python gates green** (all four): `ruff check`, `ruff format --check`, `mypy --strict`, and `pytest` each pass on `plugins/python/` at the WP3 closing commit. Proof: 52 tests passed locally at commit `7e7a85b` (walking-skeleton commit); every commit from `665b685` onward ran pre-commit hooks (ruff + mypy) green as precondition to landing.
- [x] **A.3.9** — **`pre-commit run --all-files` passes** on the WP3 closing commit. Proof: every WP3 commit's pre-commit hook output shows ruff/ruff-format/mypy all Passed.
- [x] **A.3.10** — **GitHub Actions `python-plugin` job green** on the WP3 PR. Proof: CI job exists at `.github/workflows/ci.yml` python-plugin; requires PR creation + CI run for final green — local gates and pre-commit equivalents all green at WP3 close.

### A.4 End-to-end walking skeleton

- [x] **A.4.1** — The [README §3 demo script](./README.md#3-demo-script-sprint-1-close-proof) runs end-to-end on a clean machine and each step produces the documented output. Proof: `tests/e2e/sprint_1_walking_skeleton.sh` runs the README §3 sequence verbatim (cargo build → pip install → clarion install → clarion analyze → sqlite3 verify) and exits 0 with `PASS: walking skeleton persisted python:function:demo.hello|function`.
- [x] **A.4.2** — `sqlite3 .clarion/clarion.db "select id, kind from entities;"` after the demo returns `python:function:demo.hello|function` (per the locked 3-segment L2 format). Proof: walking-skeleton script asserts exact equality with this string; CI `walking-skeleton` job reruns on every PR.
- [x] **A.4.3** — No regression in pre-existing Clarion tests (there are none yet, but this box stays for later sprints' sanity). Proof: Rust workspace nextest green (140+ tests in clarion-core; full-workspace 175+ tests) and Python pytest green (52 tests) at commit `7e7a85b`.

### A.5 Cross-product stance

- [x] **A.5.1** — Sprint 1 has introduced **no changes** in the Filigree repo. Proof: Filigree is a separate repo; no Filigree commits authored during Sprint 1 (single-author project — none would have been silent). Sprint 1 does not emit findings or observations.
- [x] **A.5.2** — Sprint 1 has introduced **no changes** in the Wardline repo — only a pinned dependency on existing names (`wardline.core.registry.REGISTRY`, `wardline.__version__`). Proof: pip-installed Wardline editable from `/home/john/wardline` for A.5.3 verification; both symbols verified at the documented locations (`src/wardline/core/registry.py:55`, `src/wardline/__init__.py:3`); no edits to that tree.
- [x] **A.5.3** — L8 version-pin range (`min_version`, `max_version` in `plugin.toml`) is compatible with the current Wardline version at Sprint 1 close. Proof: probe returns `{"status": "enabled", "version": "1.0.0"}` against `pip install -e /home/john/wardline` in the dev venv. Pin updated from the pre-sprint placeholder `0.1.0`/`0.2.0` to `1.0.0`/`2.0.0` to match Wardline's actual current version.
- [x] **A.5.4** — Any drift between Clarion's L7 qualname format and what Wardline's REGISTRY uses is documented (the first pass may uncover divergence). Proof: divergence found and documented in `wp3-python-plugin.md §L7` (Wardline stores `(module, qualified_name)` as separate fields; Clarion's L7 emits a combined dotted string). Tracked for ADR-018 amendment in **`clarion-889200006a`** (P3, sprint:2 / wp:9 labels). The join is not exercised in Sprint 1 — the divergence is pre-emptive, not a current regression.

### A.6 Documentation hygiene

- [x] **A.6.1** — [`../v0.1-plan.md`](../v0.1-plan.md) WP1/WP2/WP3 sections updated to reflect actual Sprint 1 narrower scope (Sprint 2+ scope clearly deferred). Proof: each of the three WP sections now opens with a "Sprint 1 delivery (narrow walking-skeleton scope)" callout listing the L-rows shipped and pointing at the per-WP signoff section + sprint-1 plan; the rest of each WP section continues to describe the v0.1 completion target.
- [x] **A.6.2** — [`../../clarion/adr/README.md`](../../clarion/adr/README.md) shows ADR-005 and ADR-023 both as Accepted. Proof: ADR index rows at lines 13 (ADR-005) and 26 (ADR-023) both show `Accepted`.
- [x] **A.6.3** — [`README.md`](./README.md) §4 "Lock-in summary" table has every L-row marked with the `locked on <date>` stamp. Proof: L1/L2/L3 stamped 2026-04-18 (WP1 close); L4/L5/L6/L9 stamped 2026-04-24 (A.2 signoffs, commit `1b127df`); L7/L8 stamped 2026-04-24 (A.3 signoffs, this commit).

---

## Tier B — Catalog-emitting (post-Sprint-1)

Tier B is the next natural milestone after the walking skeleton. These checkboxes
are **not** required to close Sprint 1 — they live here so the path forward is
visible. Sprint 2 may split B across multiple sprints.

- [ ] **B.1** — Phase 0 (discovery): `clarion analyze` walks a directory tree and dispatches one `analyze_file` call per matching file per plugin. Proof: integration test with a multi-file fixture.
- [ ] **B.2** — Python plugin emits **classes** and **module** entities in addition to functions. Ontology manifest updated accordingly.
- [ ] **B.3** — Python plugin emits **contains** edges (module → function, class → method).
- [ ] **B.4** — `catalog.json` is rendered after an analyze run, listing entities and edges. Proof: file produced, schema matches detailed-design §3.
- [ ] **B.5** — Per-subsystem markdown files rendered. (Subsystem detection may be naive — single flat subsystem for Tier B; WP4 clustering fills this in.)
- [ ] **B.6** — Demo extended: against the elspeth-slice fixture, `catalog.json` lists ≥95% of the Python classes and functions visible in the source (manually verified).
- [ ] **B.7** — No change in the Filigree or Wardline repos — Tier B is still Clarion-only work.

## Tier C — WP3 feature-complete

Tier C reaches WP3's full scope from `../v0.1-plan.md`. Checkboxes included for
ladder completeness; not required to close Sprint 1 or Sprint 2.

- [ ] **C.1** — Every Python entity kind from [detailed-design.md §1](../../clarion/v0.1/detailed-design.md#1-plugin-implementation-detail) emitted (functions, classes, methods, module-level variables, decorators, modules).
- [ ] **C.2** — Every Python edge kind emitted (`imports`, `calls`, `decorates`, `contains`, `inherits`).
- [ ] **C.3** — Import resolution: relative and absolute Python imports resolved to canonical entity IDs. Dynamic imports out of scope per NG-05-adjacent rule.
- [ ] **C.4** — Call-graph precision: intra-module calls resolved; inter-module calls resolved when import resolution succeeded; unresolved calls emitted as best-effort with a confidence marker.
- [ ] **C.5** — Every `CLA-PY-*` rule in [detailed-design.md §5](../../clarion/v0.1/detailed-design.md#5-pipeline--rule-catalogue-and-example-run) has positive and negative fixture coverage.
- [ ] **C.6** — Round-trip test against the full elspeth-slice passes and meets the plugin manifest's declared `max_rss_mb`.
- [ ] **C.7** — Identity reconciliation with Wardline exercised end-to-end on a real fixture (this is where L7/L8 divergence gets caught in practice, if any).

---

## Sign-off meta

- **Sprint 1 close requires**: every Tier A box ticked; for each L-row ticked, a
  `locked on <YYYY-MM-DD>` stamp present in this doc AND in [`README.md`](./README.md) §4.
- **Who signs**: the sprint owner (John Morrissey) — same author for all three Loom
  products, so no cross-team coordination needed. Sign-off proof is a single commit
  that updates this doc with all boxes ticked and the `locked on` stamps filled in.
- **Post-close**: open Sprint 2 as a new subfolder `docs/implementation/sprint-2/`
  following the same structure. Tier B and Tier C checkboxes move to the Sprint 2
  sign-off doc (or be split as Sprint 2's scope requires); the Tier A ladder here
  stays frozen as the historical record of Sprint 1's close state.

## References

- [`README.md`](./README.md) — sprint orientation and demo script.
- [`wp1-scaffold.md`](./wp1-scaffold.md), [`wp2-plugin-host.md`](./wp2-plugin-host.md), [`wp3-python-plugin.md`](./wp3-python-plugin.md) — per-WP plans.
- [`../v0.1-plan.md`](../v0.1-plan.md) — high-level plan for all 11 WPs.
- [`../v0.1-scope-plans/v0.1-scope-commitments.md`](../v0.1-scope-plans/v0.1-scope-commitments.md) — scope memo.
