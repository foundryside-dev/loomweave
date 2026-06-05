# Wave 2 — execution prompt

**Date:** 2026-06-02
**Use:** Drop the fenced prompt below into an agent to plan and execute **Wave 2** (WS4 dossier
participation + the deferred incremental-analysis skip) of the Loomweave first-class program.
This is the wave that **closes core paradise**.
**Gate:** Gated on Loomweave's own WS1 (SEI authority) + WS2 (HTTP linkages) — **both internal,
no sibling wait.** The prompt forces a confirm-or-stop gate check first.
**Source of truth:** `docs/superpowers/plans/2026-06-02-loomweave-integrated-delivery-plan.md`
Phase 3 (T3.1 incremental, T3.2 dossier participation);
`docs/superpowers/specs/2026-06-02-loomweave-first-class-program-design.md` §4 (Wave 2) + D3.
**Companion:** [`2026-06-02-wave-1-execution.md`](./2026-06-02-wave-1-execution.md) (the prerequisite wave).

---

```
You are implementing **Wave 2** of the Loomweave "road to first-class" program, in the
Loomweave repo at /home/john/loomweave. Wave 2 closes the suite's CORE PARADISE: when it lands,
`dossier(entity)` returns a complete, freshness-stamped, SEI-keyed view of a function that
stays correct after the function is renamed. Your job is to PLAN and EXECUTE it — real code,
real tests, all CI gates green.

Two workstreams, and a crucial framing: **Loomweave does NOT build the dossier.** The dossier
is assembled by the consumer (Wardline). Loomweave's job in Wave 2 is to (a) guarantee every
slice the assembler needs is reachable over HTTP and pin that contract, and (b) ship the
incremental-analysis skip that was deferred from Wave 1.

## ⛔ GATE CHECK — do this FIRST
Confirm BOTH before building (both are internal Loomweave gates — no SEI lock, no sibling):
1. **Wave 1 (WS1 SEI authority) is complete and merged.** `resolve`/`resolve_sei` are live,
   every alive entity has an `alive` `sei_bindings` row, the conformance oracle passes.
2. **Wave 0 (WS2 HTTP linkages + WS3 prior-index) is complete and merged.** `callers`/
   `callees` are live over HTTP; `sei_prior_index` is populated after every run.
   Verify in the code, not just the plan. If either is missing, STOP and ask the owner.

## Read these first (authoritative, in order)
1. docs/superpowers/specs/2026-06-02-loomweave-first-class-program-design.md — §4 (Wave 2),
   the §5 invariants, and decision D3 (why incremental-skip lands here, not in Wave 1).
2. docs/superpowers/plans/2026-06-02-loomweave-integrated-delivery-plan.md — Phase 3, tasks
   **T3.1 (incremental analysis)** and **T3.2 (dossier participation)**.
3. /home/john/wardline/docs/superpowers/specs/2026-06-01-wardline-weft-entity-dossier-design.md
   — the dossier the assembler builds. Read it to know EXACTLY which Loomweave surfaces it
   calls and what shape it expects back. You are serving this consumer, not replacing it.
4. docs/loomweave/adr/ADR-038-sei-token-and-signature.md — the SEI shape (already implemented
   in Wave 1; do not re-decide).
5. CLAUDE.md — CI gates, ADR immutability, Filigree workflow.

## Scope — two workstreams
### WS4 — dossier participation (plan T3.2) — mostly a contract, not heavy code
- Write the participation spec `docs/superpowers/specs/2026-06-02-loomweave-dossier-participation.md`
  naming the EXACT Loomweave surface the dossier assembler calls and what it returns:
  `resolve(locator)` → SEI + two-axis freshness; `callers`/`callees` over HTTP (structural
  linkages); file context (`GET /api/v1/files/...`); Filigree associations (open work).
- **Verify the whole surface is reachable over HTTP.** The assembler is an HTTP client (e.g.
  Wardline in CI). Check each slice: identity (resolve — Wave 1), linkages (Wave 0), file
  context (existing), and **Filigree associations** — `issues_for` is MCP-only today, so if
  the assembler needs associations over HTTP, that is a GAP. Either fill it with a small
  additive HTTP endpoint (consistent with the existing read API + HMAC) or, if out of scope,
  surface the gap explicitly with a recommendation — do NOT leave it silently unreachable.
- Pin every endpoint the dossier depends on in `docs/federation/contracts.md`.
- Surface the two-axis freshness in what you serve: identity axis (SEI alive/orphaned, from
  `resolve_sei`) and content axis (content_hash fresh/stale). Both explicit; neither inferred
  from the other.

### Incremental-analysis skip (plan T3.1) — the feature deferred from Wave 1 per D3
- Skip re-analysing a file whose current content hash matches its `sei_prior_index` body_hash
  from the prior run; reuse the prior-run entities (already in the DB). Emit a
  `skipped_unchanged` progress event; add `skipped_files: N` to `stats.json`.
- **LOAD-BEARING orphan guard.** Wave 1 built SEI orphan detection to take the "current run
  locator set" as an INPUT. With incremental skip on, that set MUST be the union of
  (re-analyzed entities) ∪ (entities of skipped-unchanged files, read from the prior index).
  If skipped entities are omitted, the matcher will **falsely orphan every entity in every
  unchanged file** — a silent, catastrophic regression. Add a regression test: analyze, then
  re-analyze with ONE file changed; assert entities in the unchanged files retain their SEI
  and are NOT orphaned.

## Hard boundaries — do NOT
- Do NOT build the dossier assembler or a Loomweave-owned dossier envelope. Loomweave contributes
  its slice; the consumer (Wardline) composes. Do NOT aggregate Wardline taint facts or
  Filigree issues into a Loomweave object — Loomweave serves, it does not assemble.
- Do NOT re-decide ADR-038 or change the SEI shape. Do NOT add an `entities.sei` column.
- Do NOT edit any Accepted ADR body. Do NOT touch archived docs.
- Do NOT start the parallel band (WS5 MCP catalogue, WS6 guidance, WS7 multi-language, WS8
  op-quality) or WS9 (legis) — those are separate cycles.

## Method
- Use superpowers:executing-plans (or subagent-driven-development), task-by-task. TDD: the
  incremental-skip orphan guard is test-first (RED before GREEN) — it is the correctness
  crux of this wave.
- Verify ground truth before building (Wave 0/1 code-facts may have evolved since the plan).
- All ADR-023 Rust gates green (fmt, clippy -D warnings, build --bins, nextest, doc -D
  warnings, deny). Python gates only if you touch the plugin (you likely will not in Wave 2).
- Hold the §5 invariants: opt-in (incremental skip must not change semantics, only speed),
  fail-closed (the orphan guard is the no-false-green discipline), enrich-only (the dossier
  surface is additive; Loomweave is not load-bearing for the assembler's own semantics).

## Filigree
Track per CLAUDE.md (atomic start-work verbs, `--actor` your identity). One issue for WS4 and
one for the incremental-skip feature; cite T3.1/T3.2 and the program workstream in the bodies;
close as you land each.

## Definition of done (Wave 2)
- Incremental analysis skips unchanged files; `skipped_files` reported in stats; the orphan
  guard regression test passes (unchanged-file entities keep their SEI, are not orphaned).
- The dossier participation spec is written and every endpoint it depends on is reachable
  over HTTP and pinned in `contracts.md` (or a remaining gap is explicitly surfaced with a
  recommendation).
- `dossier(entity)` is achievable by the Wardline assembler using only Loomweave's HTTP surface
  — demonstrate the full set of calls succeeds against a renamed-function fixture (SEI
  carried, facts not orphaned, freshness stamped).
- All CI gates green.

When implemented, tested, and gate-green, request a code review and surface the result plus
an honest statement of whether core paradise is reached end-to-end (renamed function →
complete, current, SEI-keyed dossier with no orphaned bindings). Wave 2 ends there; do NOT
proceed into the parallel band or WS9.
```
