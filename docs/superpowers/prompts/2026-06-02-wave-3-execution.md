# Wave 3 — execution prompt (Governed paradise · WS9)

**Date:** 2026-06-02
**Use:** Drop the fenced prompt below into an agent to plan and execute **Wave 3 — WS9 `legis`
governance consumption**, the post-core-paradise wave that closes **governed paradise**.
**Status caveat:** The program map (`…-first-class-program-design.md` §4) files WS9 as **"Later"**,
not a numbered wave — it is labelled "Wave 3" here because it is the one *sequential, gated,
post-paradise* workstream. It is **thin on Clarion's side**: Clarion is already the identity
authority, so this wave mostly exposes what Wave 1 built and swaps one provider.
**Gate:** ⛔ Gated on (a) Wave 2 complete (core paradise) **and** (b) **`legis` actually exists**
(it is design-ready, not implemented). Until `legis` ships, this prompt is forward-staged — the
gate check blocks.
**Source of truth:** program §2 (WS9), roadmap §2.4, SEI spec §5/§6 (consumer obligations + the
git-rename provider hook), `loom.md` (enrich-only federation).

---

```
You are implementing **Wave 3 — WS9: `legis` governance consumption** of the Clarion
first-class program, in the Clarion repo at /home/john/clarion. This wave closes GOVERNED
PARADISE: `legis` adds IRAP-grade governance (attestations, sign-offs, custody, audit
lineage) keyed to Clarion's stable identity, as an OPT-IN layer invisible to a solo project.

Read this framing first, because it bounds the whole wave: **this is thin on Clarion's
side.** Clarion is ALREADY the identity authority (Wave 1). Governance attestations key on
SEI that already exists; Clarion's lineage endpoint already exists. Clarion does NOT build a
new identity surface, does NOT govern or adjudicate trust, and does NOT re-establish lineage
integrity — `legis` does those at its own boundary. Your job is to make Clarion's existing
surfaces consumable by `legis`, swap one provider, and pin the contract. If you find
yourself building a large new Clarion subsystem, you have misread the scope — stop.

## ⛔ GATE CHECK — do this FIRST
Confirm BOTH before any work:
1. **Wave 2 is complete and merged** (core paradise: `dossier(entity)` works; SEI resolve /
   lineage are live). Verify in code.
2. **`legis` actually exists** as a running/implemented subsystem (check /home/john/legis —
   it is design-ready, NOT implemented as of 2026-06-02). WS9 is GATED on `legis` shipping.
   If `legis` does not yet exist, STOP and tell the owner: you may sketch the consumption
   contract (a doc), but do NOT build provider integration against a subsystem that isn't there.

## Read these first
1. docs/superpowers/specs/2026-06-02-clarion-first-class-program-design.md — §2 (WS9), §5
   invariants, §6 (D-WS5b-1 is unrelated; D1/D4 may matter).
2. /home/john/wardline/docs/superpowers/specs/2026-06-01-loom-stable-entity-identity-conformance.md
   §5 (legis obligations) + §6 (legis as the git-rename PROVIDER the §3 matcher consumes).
3. /home/john/wardline/docs/superpowers/specs/2026-06-01-loom-goal-state-case-study.md §1.5
   (graded enforcement / custody axiom) — the model `legis` governs under.
4. /home/john/legis/docs/federation/ (its SEI-consumer posture + charter), if present.
5. docs/clarion/adr/ADR-038-sei-token-and-signature.md (the SEI surfaces `legis` consumes);
   CLAUDE.md (gates, ADR immutability, Filigree).

## Scope — Clarion's thin slice of WS9
- **Audit spine (read-only consumption).** Verify `legis` can read Clarion's SEI + lineage as
  its audit trail over the EXISTING Wave-1 surface (`resolve_sei`, `lineage`). Fill only genuine
  gaps `legis` surfaces (e.g. a batch lineage read), additively. Pin the consumption contract in
  docs/federation/. `legis` re-establishes integrity at ITS boundary — do NOT build a Clarion-side
  lineage hash-chain or signature (REQ-L-01 / the custody axiom; signed lineage is North Star).
- **Git-rename provider swap (REQ-C-05).** The Wave-1 matcher consumes "a git-rename signal"
  behind the typed `GitRenameSource` interface; v1 used `ShellGitRenameSource`. `legis` owns the
  git interface, so add a `LegisGitRenameSource` impl behind the SAME interface — no change to the
  SEI model. `ShellGitRenameSource` remains the fallback when `legis` is absent (enrich-only).
- **Trust vocabulary, carried verbatim.** Clarion continues to carry `declared_tier` /
  `wardline_groups` exactly as Wardline emits them. Clarion does NOT adjudicate trust (Wardline
  analyses, `legis` governs — one judge, not two). Participate in, do not lead, any suite
  trust-vocabulary convergence.

## Hard boundaries — do NOT
- Do NOT govern, adjudicate, or re-judge trust. Do NOT add a Clarion policy/attestation engine —
  attestations live in `legis`, keyed on Clarion's SEI.
- Do NOT build a new identity surface — Clarion is already the authority.
- Do NOT build a Clarion-side lineage hash-chain / tamper-evidence in v1 (legis's boundary owns it).
- Do NOT make `legis` required for any Clarion semantics. Clarion MUST work fully with `legis`
  absent — `LegisGitRenameSource` degrades to `ShellGitRenameSource`; the audit surface is just
  unconsumed. Enrich-only (loom.md §5) — verify Clarion solo + Clarion-without-legis both intact.
- Do NOT edit any Accepted ADR body. Do NOT touch archived docs.
- Do NOT start unrelated workstreams (WS5/WS5b/WS6/WS7/WS8 are the parallel band, separate cycles).

## Method
- Use superpowers:executing-plans / subagent-driven-development. TDD for the
  `LegisGitRenameSource` (it must produce the same `GitRename` shape the matcher already tests
  against) and for the enrich-only degrade path (legis-absent → shell fallback).
- Verify ground truth: confirm the Wave-1 `GitRenameSource` trait and lineage surfaces are as the
  plan describes before integrating.
- All ADR-023 Rust gates green. Federation surfaces stay additive and pass the loom.md §5 test.

## Filigree
Track per CLAUDE.md (atomic start-work, `--actor`). Issues for: audit-spine contract, the
git-rename provider swap, trust-vocabulary carriage. Cite WS9, SEI spec §5/§6, REQ-C-05, REQ-L-01.

## Definition of done (Wave 3 / WS9)
- `legis` reads Clarion's SEI + lineage as its audit spine over a pinned, contract-documented
  surface (only additive gaps filled; no Clarion-side integrity machinery).
- `LegisGitRenameSource` supplies the matcher's git-rename signal behind the typed interface;
  `ShellGitRenameSource` remains the fallback; tests prove identical `GitRename` output and clean
  degrade when `legis` is absent.
- Clarion carries `declared_tier`/`wardline_groups` verbatim; no trust adjudication added.
- Clarion solo + Clarion-without-legis both fully functional (enrich-only verified).
- All CI gates green.

When done, request a code review and state plainly that GOVERNED paradise is reached as an
opt-in layer that a solo project never sees. Governed paradise does not gate core paradise —
core paradise (Wave 2) stands on its own.
```
