# WS8 — Operational Quality — Design & Delivery Plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:subagent-driven-development` or
> `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Date:** 2026-06-02
**Status:** Design + delivery plan (design folded in; not a bare task list)
**Workstream:** WS8 of the Clarion first-class program — **Wave 8 (Track B)**. Code-intelligence
half; ungated/concurrent. The last workstream to be planned — this locks the program at the
plan level (all nine planned).
**Goal:** Operator-grade robustness — extend `clarion doctor` beyond orientation surfaces, make the
cost estimate trustworthy, and surface summary-cache semantic staleness before operators act on a
stale briefing.

**Inputs / authorities:**
- `docs/clarion/1.0/system-design.md` §5 (Policy Engine / cost estimate / summary-cache staleness)
- `docs/clarion/1.0/requirements.md` NFR-COST-03 (±50% estimate accuracy), NFR-OPS-*
- `docs/clarion/adr/ADR-035-operational-tuning-discipline.md` (the declared-basis discipline this
  serves), ADR-007 (summary-cache key + staleness)
- Ground truth (verify before building):
  - `clarion doctor` (`crates/clarion-cli/src/doctor.rs`) today checks **only** three orientation
    surfaces (skill pack, hook, `.mcp.json`) + the index snapshot, on a report-or-`--fix` model. DB
    health / plugin availability / config validation are **net-new** checks.
  - `summary_cache.stale_semantic` **exists** (schema + the MCP status tool returns it,
    `clarion-mcp/src/tools/status.rs`). **Open question to verify:** does neighborhood-drift
    detection actually *set* it during `analyze`, or is it only ever stored false? Scope T5 by the answer.
  - **A grep for the cost estimator (`estimate_cost`/`dry_run`/`CostEstimate`/`on_exceed`) found
    nothing in core/cli.** Either it is named differently or **it is not implemented.** Verify
    first — it changes T4 from "validate + tune" to "implement + validate" (see the §2 fork).

---

## 1. Design decisions

**1.1 Extend `doctor`, don't replace it.** Keep the existing per-check ✓/✗ report + conservative
`--fix` model; add three check families:
- **DB health** — integrity (`PRAGMA integrity_check`/`quick_check`), `user_version` == expected
  schema, WAL state sane, no orphaned WAL/shadow files. `--fix` is conservative here: it may
  checkpoint/clean sidecars, but **never auto-mutates entity data** (report-only for anything that
  could lose data).
- **Plugin availability** — declared plugins in `plugins.toml` resolve on `$PATH`/at their recorded
  path; the Python version requirement is satisfiable; manifest loads. Report, don't auto-install.
- **Config validation** — `clarion.yaml` parses against its schema; referenced env vars
  (`api_key_env`, `token_env`, …) are present; integration endpoints are well-formed. Report; `--fix`
  only for safe normalisations.

**1.2 Make the cost estimate trustworthy (NFR-COST-03 = ±50%).** Instrument estimated-vs-actual per
level/tier; **validate against a real corpus run** (elspeth-class); close the two named
overestimation sources — subsystem-synthesis cost variance and prompt-cache hit-rate assumptions.
Output an accuracy report so the ±50% claim is *measured*, not asserted (ADR-035 declared-basis
discipline). **Prerequisite:** confirm the estimator exists (see §2).

**1.3 Surface semantic staleness where operators act.** `stale_semantic` (neighborhood-drift > the
configured threshold) must (a) actually be *computed and set* during `analyze` — verify the drift
detection runs, not just the column — and (b) be visible at the point of use: flagged on
briefing/`summary` responses (a header/field), listable (`doctor` or a CLI surface), so an operator
sees "this briefing's neighborhood drifted" *before* trusting it. No auto-refresh; the next analyze
refreshes — this is an honesty signal, not a mutation.

## 2. Scope fork to resolve first (not an owner-architecture call — a ground-truth check)
- **Does the dry-run cost estimator exist?** If yes → T4 is "instrument + validate + tune." If no →
  T4 is "implement the estimator per system-design §5 (per-level entity counts × per-tier pricing,
  cache-hit modelling) + validate," which is materially larger. **Determine this before estimating
  T4's size; surface the finding** — do not silently absorb a build into a "tune" task.
- **Validating ±50% needs a real paid run** on an elspeth-class corpus (budget + time). Treat the
  validation run as an owner-scheduled step (cost), not something to fire unprompted.

## 3. Tasks

- [ ] **T1 — doctor: DB health.** Integrity/quick check, schema-version match, WAL/sidecar sanity.
  Conservative `--fix` (checkpoint/clean only; never mutate entity data). Test-first on the
  pass/fail classification.
- [ ] **T2 — doctor: plugin availability.** Resolve `plugins.toml` entries, version requirement,
  manifest load. Report-only.
- [ ] **T3 — doctor: config validation.** `clarion.yaml` schema + env-var presence + endpoint
  well-formedness. Safe-normalisation `--fix` only.
- [ ] **T4 — cost-estimate accuracy.** First resolve the §2 fork (estimator exists?). Then
  instrument estimated-vs-actual, close the two overestimation paths, emit an accuracy report,
  and validate ±50% against a real run (owner-scheduled). Test-first on the estimator's math.
- [ ] **T5 — semantic-staleness surfacing.** Verify drift detection sets `stale_semantic` during
  analyze (fix if it never sets true); surface the flag on briefing/`summary` responses + a
  list/doctor view. Test that a drifted entity is flagged.
- [ ] **T6 — docs.** `clarion doctor` doc update; operator runbook for the new checks + the cost
  report; rule/finding catalogue if any new finding is emitted.

## 4. Hard boundaries — do NOT
- Do NOT let `doctor --fix` mutate or delete entity data — DB-health fixes are checkpoint/clean
  only; anything lossy is report-only.
- Do NOT auto-refresh stale briefings — staleness is a signal; the next analyze refreshes.
- Do NOT rebuild the policy engine or LLM orchestration — T4 instruments/validates (or implements
  the *estimator* only, if the §2 fork says so), nothing more.
- Do NOT fire a paid validation run unprompted — it is owner-scheduled.
- Do NOT edit Accepted ADRs; do NOT touch archived docs.

## 5. Method & gates
- Ungated/concurrent. Mostly Rust (CLI + storage + policy). superpowers:executing-plans /
  subagent-driven-development; TDD on doctor check classification, the estimator math, and the
  drift-flag surfacing. Verify the three ground-truth items (§0) before scoping. All ADR-023 Rust
  gates green; Python gates if plugin-availability checks touch the plugin side.
- Invariants: fail-closed (doctor reports honestly, conservative `--fix`; staleness surfaced not
  hidden — no false-green), opt-in (no base-path cost), ADR-035 declared-basis discipline (the cost
  report makes the ±50% claim measured).

## 6. Definition of done (Wave 8 / WS8)
- `clarion doctor` checks DB health, plugin availability, and config validation, with conservative
  `--fix`, on the existing report model.
- The cost estimate is *measured* against a real run and meets ±50% (NFR-COST-03), with the two
  overestimation paths closed and an accuracy report emitted — OR, if the estimator was unbuilt, it
  is built per §5 and then validated (the §2 fork resolved and recorded).
- `stale_semantic` is verified to be set by drift detection and is surfaced on briefing/`summary`
  responses + a list view; a drifted entity is provably flagged before an operator acts.
- All CI gates green. Any owner-scheduled item (the paid validation run) is surfaced, not silently
  skipped.
