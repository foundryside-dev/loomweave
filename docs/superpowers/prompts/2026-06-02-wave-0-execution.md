# Wave 0 — execution prompt

**Date:** 2026-06-02
**Use:** Drop the fenced prompt below into an agent to plan and execute **Wave 0** (WS2 HTTP
linkages + WS3 prior-index retention) of the Loomweave first-class program.
**Gate:** None — Wave 0 is autonomous and un-gated. Start anytime.
**Source of truth:** `docs/superpowers/plans/2026-06-02-loomweave-integrated-delivery-plan.md`
Phase 1 (T1.1–T1.7); `docs/superpowers/specs/2026-06-02-loomweave-first-class-program-design.md` §4.
**Companion:** [`2026-06-02-wave-1-execution.md`](./2026-06-02-wave-1-execution.md) (the next wave; gated on SEI lock).

---

```
You are implementing **Wave 0** of the Loomweave "road to first-class" program, in the
Loomweave repo at /home/john/loomweave. Wave 0 is autonomous, un-gated, and on the suite
critical path: completing it lets the suite-wide SEI identity standard lock. Your job is to
PLAN and EXECUTE it end-to-end — real code, real tests, all CI gates green.

## Read these first (authoritative, in order)
1. docs/superpowers/specs/2026-06-02-loomweave-first-class-program-design.md — the program
   map. You are doing **Wave 0** only (§4). Read §1–§4 and the §5 invariants.
2. docs/superpowers/plans/2026-06-02-loomweave-integrated-delivery-plan.md — your task
   source. Wave 0 = **Phase 1 tasks T1.1 through T1.7**. (T1.0, ADR-038, is already DONE.)
3. docs/loomweave/adr/ADR-038-sei-token-and-signature.md — the locked SEI decisions. Do NOT
   relitigate these; they constrain you but you are not implementing them in Wave 0.
4. CLAUDE.md — the repo's CI gates, ADR/immutability rules, and Filigree workflow.

## Scope — exactly two workstreams, nothing else
- **WS2 — HTTP linkages** (plan T1.5–T1.7): add `callers`/`callees` (+ batch) to the HTTP
  read API with pagination + confidence-tier filtering, and a `linkages: { http: true }`
  capability flag. These wrap the EXISTING storage queries
  `loomweave-storage/src/query.rs::call_edges_targeting` (callers) and `call_edges_from`
  (callees). Routes are HMAC-protected, same as the `/api/v1/files` routes.
- **WS3 — prior-index retention, side table only** (plan T1.1–T1.4): migration
  `0004_sei_prior_index.sql` creating `sei_prior_index(locator, body_hash, signature,
  recorded_at)` — **NO SEI column** (shape-independent), the `prior_index.rs` storage
  helpers, the `WriterCmd::UpsertPriorIndex` plumbing, and the analyze-pipeline flush that
  rewrites the snapshot after every successful run.

## Hard boundaries — do NOT do any of this in Wave 0
- Do NOT start WS1 SEI authority: no `sei_bindings` table, no migration 0005, no
  mint/matcher/lineage, no `resolve`/`resolve_sei` endpoints, no `entities.sei` column.
- Do NOT pin the SEI token shape into anything load-bearing. The prior-index table is
  deliberately SEI-free so it ships before SEI lock.
- Do NOT build incremental-analysis skip behaviour (that is plan T3.1 / Wave 2, lands with
  WS1 per decision D3). Wave 0 builds the table and POPULATES it; it does not yet consume
  it to skip files.
- In Wave 0 the `entities.signature` column does NOT exist yet (it arrives with WS1
  migration 0005). So the prior-index flush writes `body_hash` now and leaves
  `sei_prior_index.signature` NULL — the column exists for WS1 to fill later. Do not try to
  read a non-existent `entities.signature`.
- Do NOT edit any Accepted ADR body (ADR files are immutable except status/supersession
  links). Do NOT touch archived docs under specs/archive or plans/archive.

## Method
- Use the superpowers:executing-plans (or subagent-driven-development) skill to work the
  plan task-by-task. Use TDD: for any correctness-critical logic, write the failing test
  first (RED), then implement (GREEN). The plan marks which steps are test-first.
- **Verify ground truth before building.** The plan rests on code facts (e.g.
  `call_edges_targeting`/`call_edges_from` exist; `entities` is upsert-only with no DELETE;
  the writer-actor pattern; migration numbering is at 3 so the next is 0004). Confirm each
  against the actual source before you depend on it — plans drift from code.
- Migrations stack (the in-place edit policy retired at 1.0). `0004` is a new file;
  bump `CURRENT_SCHEMA_VERSION` 3 → 4; the compile-time assert enforces the match.
- Every PR must pass the full ADR-023 gate floor (from CLAUDE.md): `cargo fmt --check`,
  `cargo clippy --workspace --all-targets --all-features -D warnings`,
  `cargo build --workspace --bins`, `cargo nextest run --workspace --all-features`,
  `RUSTDOCFLAGS="-D warnings" cargo doc`, `cargo deny check`. Wave 0 is Rust-only.
- Hold the §5 program invariants: opt-in (nothing new in the zero-cost base path),
  fail-closed, enrich-only. Federation surfaces stay additive.

## Filigree
Track the work in Filigree per CLAUDE.md (use the atomic start-work/start-next-work verbs,
`--actor` your identity). File one issue per workstream (WS2, WS3), cite the plan task IDs
(T1.1–T1.7) and the program workstream in the body, and close them as you land each.

## Definition of done (Wave 0)
- `GET /api/v1/entities/{id}/callers` and `/callees` (+ batch) live, paginated,
  confidence-filtered, HMAC-gated, and tested against known fixtures.
- `_capabilities` reports `linkages: { http: true }`.
- `sei_prior_index` exists (migration 0004) and is correctly repopulated after every
  successful `loomweave analyze` (stale rows from the prior run removed), with a regression
  test proving the snapshot equals the current run's entity set.
- `docs/federation/contracts.md` pins the new linkage routes + capability flag.
- All CI gates green.

When the two workstreams are implemented, tested, and gate-green, request a code review of
the diff and surface the result. Do NOT proceed into WS1/SEI work — Wave 0 ends at
"SEI can lock."
```
