# PDR-0005: Accept the `loomweave-llm` extraction bet as complete

- **Date:** 2026-06-26
- **Status:** accepted (ACCEPT against PRD-0001's acceptance criteria; bet selection was PDR-0003, owner-confirmed)
- **PRD:** PRD-0001 (`docs/product/prd/PRD-0001-loomweave-llm-extraction.md`)
- **Tracker:** clarion-141e9c08c8 (closed; close-commit `main@b346328`) → unblocks clarion-4328c5c757

## Context

PDR-0003 selected the `loomweave-llm` extraction as the Now bet and dispatched
it (PRD-0001 ready-for-planning, solution-architect-ratified pure-leaf boundary,
implementation plan). At this session's RESUME the bet was still `proposed` —
dispatched but not executed. The owner confirmed "execute it this session"
(`/own-product` resume). It was then planned-reviewed, executed, and merged.

## What was done (this session)

- A right-sized plan review (Reality dimension) caught that the plan's line
  numbers had drifted (~25 commits since 2026-06-24) but all 8 import-site
  exact-strings still matched; verdict APPROVED_WITH_WARNINGS.
- Behavior-preserving lift-and-shift on `feat/loomweave-llm-extraction`: the two
  provider modules moved verbatim into a new pure-leaf `loomweave-llm` crate
  (git records R100 renames); `loomweave-core` dropped `reqwest`/`async-trait`/
  `fs2`; `cli` + `mcp` repointed provider imports.
- A standing trust-surface CI gate added to `verify.yml` (fixed a vacuous
  `^reqwest` anchor that would never have fired; hardened under `set -euo pipefail`).
- Merged via PR #76 → `main` merge commit `b346328` with green CI (incl. the
  CI-only aarch64 cross-check and the new gate).

## The call

**Accept the bet as complete.** All six PRD-0001 acceptance criteria are met on
the merge commit: (1) `cargo tree -p loomweave-core` resolves no `reqwest`,
providers in the new crate; (2) trust-surface guardrail `yes → no` flipped +
enforced by CI; (3) full floor green (nextest 1948, pytest 220, 3 e2e); (4)
`entity_id.rs` untouched, no SEI churn; (5) no consumer regression, Recording*
replay tests pass unchanged; (6) pure lift-and-shift (R100 renames). The bet
leaves "in flight" and is banked.

The original 2026-05-22 ticket text (pre-`loomweave` rename) named a
trait-contract uniformity test and a design-doc boundary note; PRD-0001
re-scoped the bet to relocation-only and deferred contract-uniformity testing to
the downstream per-provider split (clarion-4328c5c757). The design-doc already
names `loomweave-llm` — no stale note remained. This is a recorded re-scope, not
dropped work.

## Reversal trigger

If `reqwest` re-enters `loomweave-core`'s dependency tree (caught by the new CI
gate, or `cargo tree -p loomweave-core --edges normal --prefix none | grep
'^reqwest v'`), the trust boundary regressed — reopen as a fresh bet. The
acceptance itself stands; the trust-surface invariant is now a standing
guardrail, not an open bet.
