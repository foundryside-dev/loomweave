# Wave 1 — execution prompt

**Date:** 2026-06-02
**Use:** Drop the fenced prompt below into an agent to plan and execute **Wave 1** (WS1 — SEI
authority) of the Loomweave first-class program.
**Gate:** ⛔ Gated. Requires (a) Wave 0 complete + merged, and (b) **SEI lock confirmed**
(program decision D1 — a suite event). The prompt forces a confirm-or-stop gate check first.
**Source of truth:** `docs/superpowers/plans/2026-06-02-loomweave-integrated-delivery-plan.md`
Phase 2 (T2.0–T2.6) + the conformance oracle + the hard-cutover backfill;
`docs/loomweave/adr/ADR-038-sei-token-and-signature.md` (locked decisions);
`/home/john/wardline/docs/superpowers/specs/2026-06-01-weft-stable-entity-identity-conformance.md` (the SEI standard).
**Companion:** [`2026-06-02-wave-0-execution.md`](./2026-06-02-wave-0-execution.md) (the prerequisite wave).

---

```
You are implementing **Wave 1** of the Loomweave "road to first-class" program, in the
Loomweave repo at /home/john/loomweave. Wave 1 is **WS1 — SEI authority**: the suite-wide
stable-entity-identity engine. It is the heaviest single workstream and the one that, once
shipped, makes every cross-tool binding survive a rename. Your job is to PLAN and EXECUTE
it — real code, real tests, all CI gates green, and the SEI conformance oracle passing.

## ⛔ GATE CHECK — do this FIRST, before any shape-committing code
Wave 1 is GATED. Confirm BOTH before writing migration 0005, the matcher, or any
SEI-shaped persistence:
1. **Wave 0 is complete and merged.** WS3's `sei_prior_index` table exists and is
   populated after every run (the matcher consumes it); WS2's HTTP linkages are live.
   Verify in the code, not just the plan.
2. **SEI lock is confirmed** (program decision D1 — a SUITE event, not Loomweave's alone:
   all four subsystems reported + the §8 oracle encodes the resolutions). Loomweave's shape
   obligation is already discharged (ADR-038), but the suite may still adjust the shape in
   response to another subsystem's emerging requirement until lock. **If lock is not yet
   confirmed, STOP and ask the owner.** You may do shape-independent prep (test scaffolds,
   ground-truth verification, reading) but MUST NOT commit migration 0005, minting, the
   matcher, or the wire contract until lock is confirmed.

## Read these first (authoritative, in order)
1. /home/john/wardline/docs/superpowers/specs/2026-06-01-weft-stable-entity-identity-conformance.md
   — the SEI standard. Read §1–§8 closely: §3 matcher, §4 wire contract, §5 your
   obligations, §7 migration, §8 the conformance oracle you must pass.
2. docs/loomweave/adr/ADR-038-sei-token-and-signature.md — the LOCKED Loomweave decisions
   (token, signature, persistence, reserved namespace). Implement these as written; do NOT
   re-decide them.
3. docs/superpowers/plans/2026-06-02-loomweave-integrated-delivery-plan.md — your task
   source. Wave 1 = **Phase 2 tasks T2.0 through T2.6**, plus the conformance oracle and
   the hard-cutover backfill. Read the "SEI persistence model" section and REQ-C-02 / the
   peer-review correction notes — they explain WHY the shape is what it is.
4. docs/superpowers/specs/2026-06-02-loomweave-first-class-program-design.md §5 invariants.
5. CLAUDE.md — CI gates, ADR immutability, Filigree workflow.

## Scope — WS1 SEI authority only
- **T2.0** migration `0005_sei.sql`: `sei_bindings` (durable identity store) + `sei_lineage`
  (append-only) + plain `entities.signature TEXT`. **No `entities.sei` column.** Bump
  CURRENT_SCHEMA_VERSION 4 → 5.
- **T2.1** `sei.rs`: `mint_sei`, the deterministic fail-closed matcher (`rebind_or_mint`),
  binding-state helpers, the typed `GitRenameSource` trait + `ShellGitRenameSource` v1 impl,
  and orphan detection. **Test-first** (RED before GREEN) — identity is correctness-critical.
- **T2.2** WriterCmds: `UpsertSeiBinding`, `OrphanSeiBinding`, `SetEntitySignature`,
  `AppendSeiLineage`, with dispatch arms.
- **T2.3** the analyze SEI mint pass (new sub-phase after extraction) that consumes the
  Wave-0 prior index + git renames, carries-or-mints, and records lineage.
- **T2.4** HTTP wire: `resolve` / `resolve_sei` / `lineage` (+ batch resolve), reading
  `sei_bindings`; `_capabilities` gains `sei: { supported: true, version: 1 }`.
- **T2.5** MCP surface carries SEI via the read-time join entities↔sei_bindings — on ALL
  tools that return an entity id.
- **T2.6** federation contracts + the cutover playbook.
- **Python plugin**: emit the signature JSON the manifest declares (`signature_schemas` +
  `signature_schema_version`) so `entities.signature` is populated. Python CI gates apply.
- **Conformance oracle** (SEI spec §8): build/run the fixtures — identity round-trip +
  opacity, rename, move, ambiguous (fail-closed), delete, capability-absent. Loomweave must
  pass all.
- **Hard-cutover backfill**: build + test the idempotent, resumable backfill that re-keys
  existing bindings locator→SEI. The actual coordinated cross-tool release is owner-gated
  (Filigree + Wardline must cut at the same time) — build and document it, surface it for
  scheduling, do NOT fire it unilaterally.

## LOCKED decisions (ADR-038) — implement exactly; do NOT re-derive
- **Token:** `loomweave:eid:<lowercase-hex(blake3(utf8(locator) ++ 0x00 ++ utf8(mint_run_id)))[:32]>`,
  where `mint_run_id` is the minting run's UUID. **NOT** `first_seen_commit` (it is never
  populated — a token keyed on it collides on locator reuse). SEI allocation is STATEFUL;
  reproducibility comes from `sei_bindings`, not from re-deriving the token. Add a test that
  a back-to-back unchanged re-run CARRIES (never re-mints) every SEI, and document that SEI
  *values* are not part of the byte-identical-run guarantee.
- **Persistence:** identity lives in `sei_bindings` keyed by SEI, NOT a column on `entities`
  (`entities` is cumulative/never-pruned, so a UNIQUE SEI column breaks on rename-carry).
  Orphaning is a `status` flip. Partial unique index: at most one `alive` binding per
  `current_locator`. Order writes so orphan/re-point happens before the corresponding carry,
  so the unique index never transiently doubles up.
- **Signature:** plain non-unique `entities.signature TEXT`, plugin-declared versioned JSON,
  compared by string equality. Near-redundant for the v1 deterministic move; carried for
  spec-conformance + the fuzzy future.
- **REQ-F-02:** `resolve(locator)` rejects any input with the reserved `loomweave:eid:` prefix
  ("not a valid locator") — NOT by colon count (an SEI has the same two colons). Reserve the
  `loomweave:eid:` namespace; no plugin locator may occupy it.
- **REQ-C-05:** git-rename behind the typed `GitRenameSource` interface so legis can supply
  it later without touching the model.
- **REQ-L-01:** lineage is append-only — no UPDATE path, no Loomweave-side hash-chain in v1.
- **No binding keyed on a locator on ANY surface** — MCP and HTTP both carry SEI. No MCP
  locator exception.
- **Fail-closed:** when the matcher cannot PROVE sameness, mint a new SEI and mark the old
  `orphaned`; never silently re-point.

## Hard boundaries — do NOT
- Do NOT start before the gate check passes (Wave 0 done + SEI lock confirmed).
- Do NOT build WS4 dossier participation or incremental-analysis skip behaviour — those are
  Wave 2. BUT: write orphan detection to take the "current run locator set" as an input, so
  Wave 2's incremental skip can extend it without a rewrite (a skipped-unchanged file's
  entities must NOT be falsely orphaned).
- Do NOT add an `entities.sei` column. Do NOT re-decide ADR-038. Do NOT edit any Accepted
  ADR body. Do NOT touch archived docs.
- Do NOT fire the cross-tool cutover release yourself — build/test/document it, surface it.

## Method
- Use superpowers:executing-plans (or subagent-driven-development), task-by-task. TDD: T2.1
  (matcher), T2.4 (resolve rejection), and the token test are test-first.
- Verify ground truth before building (the plan's code-facts may have drifted).
- All ADR-023 gates green (Rust: fmt, clippy -D warnings, build --bins, nextest, doc -D
  warnings, deny). **Python gates too** (ruff, ruff-format --check, mypy --strict, pytest)
  — Wave 1 touches the plugin for signature emission.
- Hold the §5 invariants: opt-in, fail-closed, enrich-only.

## Filigree
Track per CLAUDE.md (atomic start-work verbs, `--actor` your identity). One issue per Phase-2
task; cite ADR-038, the SEI-spec REQ-C-*/REQ-F-*/REQ-L-* IDs, and SEI spec §3/§4/§8 in the
bodies. Close as you land each.

## Definition of done (Wave 1)
- Every alive entity has an `alive` `sei_bindings` row after analysis.
- Matcher handles rename / move / ambiguous(fail-closed) / orphan per the test suite; a
  back-to-back unchanged re-run carries (never re-mints) every SEI.
- HTTP `resolve`/`resolve_sei`/`lineage` live, with the REQ-F-02 `loomweave:eid:` rejection;
  `_capabilities` reports `sei: { supported: true, version: 1 }`.
- MCP responses carry SEI via the binding join (no locator-keyed bindings anywhere).
- The §8 conformance oracle passes.
- Backfill built + tested (idempotent, resumable, reserved-prefix-safe); cutover playbook
  written; coordinated cross-tool release surfaced for owner scheduling.
- All Rust + Python CI gates green.

When implemented, tested, gate-green, and oracle-passing, request a code review of the diff
and surface the result plus the cutover-release readiness. Wave 1 ends at "identity is
refactor-stable suite-wide"; do NOT proceed into Wave 2 (dossier / incremental).
```
