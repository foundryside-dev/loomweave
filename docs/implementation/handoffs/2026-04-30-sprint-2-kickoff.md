# Clarion Sprint 2 — kickoff handoff

This file is the starting prompt for the next Claude Code session that
opens Sprint 2. Paste it as the user's first message; it is
self-contained.

Supersedes `2026-04-24-post-wp2-scrub-handoff.md` (Sprint 1 work) and
all prior WP2 scrub handoffs. Sprint 1 is closed and tagged at
`v0.1-sprint-1` on `main`; this handoff is a fresh start, not a
continuation of an in-flight branch.

---

# Begin Clarion Sprint 2 — Tier B (catalog-emitting) + carryover

Sprint 1's walking-skeleton tagged at `v0.1-sprint-1` on 2026-04-28.
Tier A is fully ticked; nine lock-ins (L1–L9) are ratified. Sprint 2's
job is the next milestone in `docs/implementation/sprint-1/signoffs.md`
**Tier B (catalog-emitting)** plus the carryover of unfinished defects
and a real cross-product join.

The Sprint 1 walking-skeleton is not a throwaway — it is the contract
Sprint 2 reads and writes against. Tier B builds on the locked L1–L9
shapes; if you find yourself wanting to change one, write a new ADR
that supersedes the relevant Accepted one rather than editing in place.

## Working directory + branch

- Directory: `/home/john/clarion`
- Branch: start a new branch off `main` for Sprint 2 work, e.g.
  `sprint-2/wp4-pipeline` or per-WP branches as you go. The
  `sprint-1/wp2-plugin-host` branch is deleted both locally and on
  origin; do not try to resume it.
- `main` HEAD: `5aeeff4` (CLAUDE.md repository-state refresh, post-merge).
- Tag: `v0.1-sprint-1` points at merge commit `48b9bb0`.

## Sprint 1 in one paragraph (so this handoff is self-contained)

`clarion analyze` against a fixture project with a single
`def hello(): ...` Python file persists exactly one entity row,
`python:function:demo.hello`, with `kind="function"`, into
`.clarion/clarion.db`. The pipeline is: Rust plugin host (WP2) discovers
the editable Python plugin (WP3) on `$PATH`, spawns it, completes a
JSON-RPC handshake, sends one `analyze_file` request, receives one
entity, and writes it through the writer-actor (WP1). Every stage has
both positive and negative tests. CI runs three jobs (rust,
python-plugin, walking-skeleton); all green at sprint close.

## What's in scope for Sprint 2

The signoff ladder names the next milestone as
[`signoffs.md` Tier B](../../implementation/sprint-1/signoffs.md#tier-b--catalog-emitting-post-sprint-1).
Seven boxes; B.1–B.5 are real implementation work, B.6 is the demo,
B.7 is the cross-product invariant.

Mapping to v0.1-plan.md WPs:

| Tier B | Owning WP | Deliverable |
|---|---|---|
| B.1 — Phase 0 (discovery): multi-file dispatch | WP4 | Pipeline walks the source tree and dispatches one `analyze_file` per matching file per plugin. |
| B.2 — Python plugin emits classes + module entities | WP3-feature-complete | Extractor lifts beyond functions; ontology manifest updated. |
| B.3 — Python plugin emits `contains` edges | WP3-feature-complete | Module → function, class → method `contains` relationships emitted. |
| B.4 — `catalog.json` rendered after analyze | WP4 | File materialised under `.clarion/`; schema per `detailed-design.md` §3. |
| B.5 — Per-subsystem markdown files | WP4 | Rendered to `.clarion/`; subsystem detection may be naive (single flat subsystem) for Tier B; clustering proper is WP4 Phase 3. |
| B.6 — Demo against elspeth-slice fixture | sprint demo | `catalog.json` lists ≥95% of visible Python classes/functions. |
| B.7 — No Filigree/Wardline changes | invariant | Same rule as Sprint 1; this is still Clarion-internal work. |

**Recommended sequencing** (no flag-day refactors; each step lands a
working DB row):

1. Two warm-up bug fixes from the WP1-review-2 P2 list (familiar code,
   small risk, sets the rhythm). See *Carryover* below.
2. WP3-feature-complete subset for Tier B: classes + module entities
   (B.2). Ontology-version bump from `0.1.0` to `0.2.0` in
   `plugin.toml` since the entity-kind set changes — bumping is the
   ADR-007 cache-key signal that downstream caches must invalidate.
3. WP3-feature-complete: `contains` edges (B.3). This is the first
   edge type Clarion has ever persisted; the writer-actor (WP1) wrote
   the schema for it but never exercised it. Watch for L3 surprises.
4. WP4 Phase 0 + Phase 1: multi-file walk + per-file dispatch (B.1).
   The CLI's `analyze.rs` already does a basic walk — extend it to
   drive multi-file `analyze_file` calls and persist edges.
5. WP4 catalog rendering (B.4 + B.5).
6. Demo (B.6) — the elspeth-slice test fixture must materialise ≥95%
   of its Python entities.

## Carryover from Sprint 1 — triage list

These are open issues filed during Sprint 1 with explicit deferral
rationale. None block Sprint 2 from starting; they are the Sprint-2
backlog.

### ADR-018 amendment trigger (P3, sprint:2 / wp:9)

`clarion-889200006a` — L7 qualname divergence with Wardline's
`FingerprintEntry` storage. Wardline stores `(module, qualified_name)`
as separate fields; Clarion's L7 emits the combined dotted string.

When this fires: WP9 (Loom integrations) attempts the first
cross-product join. If Sprint 2 stays Clarion-internal (Tier B), this
ticket can stay deferred — but it's the natural pre-emptive read for
anyone touching WP9 plans.

### WP1 review-2 P2 bugs (good warm-ups)

Small, well-scoped, in code you've already touched:

- `clarion-5e03cfdd21` — `read_applied_versions` uses `.ok()` and
  swallows DB-locked / corrupt errors. Surface them as `Err`.
- `clarion-ed5017139f` — `clarion install` leaves a partial `.clarion/`
  on failure, blocking re-install. Add cleanup or atomic move.
- `clarion-b5b1029f5a` — `reader_pool` test uses a 100ms sleep to
  assert a second reader is blocked — flaky under load. Replace with a
  deterministic synchronisation primitive.
- `clarion-4cd11905e2` — `entities.priority` generated column uses
  TEXT affinity, breaking numeric priority ordering. Surfaces as
  wrong-order results in any `ORDER BY priority` query.

### WP2 deferred items (each has a documented trigger)

Do not reopen unless the documented trigger fires.

- `clarion-48c5d06578` (P3) — supervisor needs drain/discard strategy
  for poisoned inbox frames. **Trigger**: WP4 writes its own
  supervisor read loop.
- `clarion-928349b60f` (P3) — `jail()` return is canonicalize-time
  only; TOCTOU window for any later open by downstream code.
  **Trigger**: WP6 briefing-serving code reads
  `AcceptedEntity.source_file_path`.
- `clarion-35688034f0` (P3) — `read_frame` has no deadline; plugin
  hang blocks CLI indefinitely. **Trigger**: needs a cross-module I/O
  design pass, do as its own session.
- `clarion-c0977ac293` (P4) — `RLIMIT_AS` kill not observed
  end-to-end. Unit tests + code review sufficient for Sprint 1; can
  remain deferred unless an Tier-B test surfaces a real OOM.
- `clarion-adeff0916d` (P3) — fixture binary self-build; CI works
  around with `cargo build --workspace --bins` step, but a proper
  `escargot`/`build.rs` solution is cleaner long-term.

### Other ready WP2/WP1 cleanups

About a dozen P3/P4 cleanup tasks (entity_id duplicate char-class,
manifest.rs file size, `id: i64` JSON-RPC narrow, etc). Skim
`mcp__filigree__get_ready` at session start; do not block Sprint 2 on
them.

## Caller-observable surfaces from Sprint 1 (locked)

Sprint 2 reads and writes against these without re-deriving — they are
locked at `v0.1-sprint-1`. If you want one to change, write an ADR.

### From WP2 (locked on 2026-04-24)

- **Wire entity shape** (`crates/clarion-core/src/plugin/host.rs:132-154`):
  ```json
  {
    "id": "...",
    "kind": "...",
    "qualified_name": "...",
    "source": {"file_path": "...", ...}
  }
  ```
  `module_path` and `source_range` go inside `source.*` (via serde
  flatten — see WP3 walking-skeleton commit `7e7a85b` for the bug
  this caught).
- **Edge wire shape**: not yet exercised in Sprint 1. The schema
  exists in `detailed-design.md §3`; B.3's first edge will define the
  protocol-level wire shape. Mirror the entity pattern: `{kind, src,
  dst, ...}`. Worth a short ADR if any non-obvious choice arises.
- **Manifest fields the host reads**:
  `[plugin].executable` must be a bare basename matching the
  discovered binary; `[capabilities.runtime].reads_outside_project_root`
  must be `false` (true is rejected at handshake);
  `[ontology].entity_kinds` is the allowlist for emitted entity
  kinds (host drops unknown kinds with `CLA-INFRA-PLUGIN-UNDECLARED-KIND`).
  When B.2 adds `class` and `module` kinds, both
  `plugin.toml::[ontology].entity_kinds` and the extractor must change
  in lockstep.
- **`ontology_version` bump policy** (ADR-007): bump on every
  entity-kind / edge-kind / rule-ID change. `plugin.toml`'s
  `ontology_version` AND `server.py`'s `ONTOLOGY_VERSION` constant
  must move together. The host validates non-empty at handshake and
  stores it; cache invalidation downstream depends on it.
- **stderr is captured to a 64 KiB ring**, not inherited. Use
  `host.stderr_tail()` for diagnostics.
- **Resource limits applied to plugin child**: `RLIMIT_AS` from
  manifest `expected_max_rss_mb` (min of that and 2 GiB),
  `RLIMIT_NOFILE = 256`, `RLIMIT_NPROC = 32`. Realistic Python plugin
  ceiling is 256 MiB+; current `plugin.toml` declares 512 MiB.

### From WP3 (locked on 2026-04-28)

- **L7 qualname format**: `{dotted_module}.{__qualname__}`.
  `module_dotted_name()` strips a leading `src/` segment and collapses
  `pkg/__init__.py` to `pkg`. The `<locals>` marker appears only at
  function-parent boundaries, never class.
- **L8 Wardline pin**: `min_version = "1.0.0"`,
  `max_version = "2.0.0"` (Wardline shipped 1.0.0 between sprint
  kickoff and close — original `0.1.0/0.2.0` placeholders were
  retired). Probe is fail-soft: missing or out-of-range Wardline does
  not fail the plugin.
- **`extract()` signature**: `extract(source: str, file_path: str, *,
  module_prefix_path: str | None = None)`. `file_path` lands in
  `source.file_path` on the wire (host jail validates it);
  `module_prefix_path` defaults to `file_path` and feeds dotted-name
  derivation. Server-side relativisation against `project_root` is in
  `server._resolve_module_path`.
- **Server handler signature**: `Handler = Callable[[dict[str, Any],
  ServerState], dict[str, Any]]`. `ServerState` carries
  `initialized`, `shutdown_requested`, `project_root: Path | None`.
  When B.2 adds class/module handling, extend the extractor without
  changing the handler signature.

### From CI / tooling (ADR-023 floor)

- **Five Rust gates** on every commit: fmt, clippy `-D warnings`
  (pedantic), nextest, doc `-D warnings`, deny.
  **Critical addition (CI-only)**: `cargo build --workspace --bins`
  must run before `cargo nextest run` so `clarion-plugin-fixture` is
  on disk for `wp2_e2e` integration tests (commit `be7fa60`).
- **Four Python gates**: ruff check, ruff format check,
  `mypy --strict`, pytest. ruff config has pragmatic excludes for
  `D` (docstring), `COM812`/`ISC001` (format conflicts), `CPY`
  (copyright headers), `ANN401` (Any in JSON-RPC payloads), `TRY003`
  (long exception messages). Tests further allow `S101`, `PLR2004`,
  `ANN`, `INP001`, `S108`, `E501`. `pylint.max-returns = 10`,
  `pylint.max-branches = 15`, `mccabe.max-complexity = 15`.
- **Pre-commit hooks** at repo root (`/.pre-commit-config.yaml`,
  not under `plugins/python/`); `pass_filenames: false` on the mypy
  hook so intra-package imports resolve. Pinned to
  `ruff-pre-commit@v0.15.11` and `mirrors-mypy@v1.20.2`.
- **CI walking-skeleton job** runs
  `tests/e2e/sprint_1_walking_skeleton.sh`. Test scripts that need
  `clarion analyze` to succeed must scrub `$PATH` (the runner has
  world-writable dirs that trip WP2's discovery refusal — see commits
  `7c0e396` for the refusal and `be7fa60` for the test-side fix).

## Methodology

Same as Sprint 1. Not negotiable.

### Phase 1 — Plan + brainstorm (before writing code)

- `superpowers:brainstorming` if the WP shape is genuinely open. Tier B
  has WP-doc precedents but no executed plan yet — brainstorm is more
  valuable for B.2/B.3 than the bug warm-ups.
- `superpowers:writing-plans` after brainstorm, but the WP3
  feature-complete WP doc plus `system-design.md §6` and
  `detailed-design.md §5` are detailed enough that an inline task
  list (no `/docs/superpowers/plans/` file) is often the right move.
  Sprint 1's advisor verdict on Sprint-1's WP3 — "the WP doc is the
  plan; writing a duplicate introduces drift" — applies again here.
- `axiom-planning:review-plan` if the plan is non-trivial.
- **Call advisor before substantive work**, especially before B.2
  (entity-kind expansion) and the first edge emission in B.3.

### Phase 2 — TDD implementation

One commit per task. Each commit:

1. Failing test first (TDD discipline).
2. Minimum code to pass.
3. All ADR-023 gates green before commit (Rust five + Python four +
   pre-commit hooks).
4. Commit message cites the filigree issue ID.

### Phase 3 — Sprint 2 close

- Tick Tier B in `signoffs.md` (the section already exists; no need
  to author it).
- Update `README.md` §4 lock-in summary if any new lock-in lands
  (ontology-version semantics may justify an L10).
- Tag `v0.2-sprint-2` (or per the project's tag scheme — Sprint 1
  used `v0.1-sprint-1`).
- Close the relevant filigree work-package umbrellas + sprint-close
  issue.

## Session hygiene

- **filigree workflow**: MCP tools in `.mcp.json`. Prefer
  `mcp__filigree__*` over CLI.
- **No reopening of the deferred items above** without their
  documented trigger firing. If you discover a reason to revisit, file
  a new issue and reference the old one.
- **Commit discipline**: one logical fix per commit, `git add
  <specific files>` (not `git add -A`).
- **Never skip hooks** (`--no-verify`).
- **ADR-023 gates on every commit**, not every N commits. The
  `cargo build --workspace --bins` step is a Sprint-2 addition to the
  Rust gate sequence on CI; locally it is implicit (most workflows
  already have a built binary lying around) but worth running
  explicitly when a fresh checkout is in play.
- **Respect the rename-over-stub policy** (CLAUDE.md): if anything
  moves, use `git mv`.

## Starting checklist

1. `git status && git log --oneline -10` — confirm branch state.
   `main` HEAD should be `5aeeff4`; tag `v0.1-sprint-1` should resolve
   to `48b9bb0`.
2. `cargo nextest run --workspace --all-features 2>&1 | tail -3` —
   confirm 175-test green baseline carried forward (175 tests at
   close).
3. `plugins/python/.venv/bin/pytest plugins/python 2>&1 | tail -3` —
   confirm 52-test green baseline.
4. `bash tests/e2e/sprint_1_walking_skeleton.sh 2>&1 | tail -3` —
   walking skeleton still green.
5. `mcp__filigree__get_ready` — show available work; confirm Sprint-1
   issues are all `delivered` and the carryover items above appear.
6. Read this doc + `docs/implementation/sprint-1/signoffs.md` Tier B
   + `docs/implementation/v0.1-plan.md` WP4 + WP3-feature-complete
   notes.
7. Ask the user: *"Bug warm-ups first (WP1 review-2 P2s), or straight
   into B.2 classes + module entities?"* Both are valid Sprint-2
   starts; the answer determines task #1.
8. Branch off `main` with the chosen scope: `git checkout -b
   sprint-2/<scope>`.

Good luck. Sprint 2 lands the catalog — the artefact every later
sprint reads. Get the entity-kind set right; wrong kinds at this
boundary force ontology-version churn forever after.
