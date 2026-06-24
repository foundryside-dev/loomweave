# PDR-0003: Now bet — extract `loomweave-llm` from `loomweave-core`

- **Date:** 2026-06-24
- **Status:** accepted (autonomous within grant; Now-bet selection confirmed by owner this session)
- **PRD:** PRD-0001 (`docs/product/prd/PRD-0001-loomweave-llm-extraction.md`)
- **Tracker:** clarion-141e9c08c8 (head of critical path) → unblocks clarion-4328c5c757

## Context

The 2026-06-11 bootstrap Now bet ("ship 1.1.0/rc4 with the Rust plugin at
gold") is complete and shipped — the product is now at v1.3.1 (see PDR-0004).
With that done, the workspace had **no recorded Now bet**. The Filigree
critical path's head is unchanged from bootstrap: extract `loomweave-llm`
(clarion-141e9c08c8), which unblocks the per-provider split
(clarion-4328c5c757).

## Options

1. **Extract `loomweave-llm`** — pay the head-of-critical-path architecture
   debt; a behavior-preserving lift-and-shift.
2. **Incremental-analyze correctness cluster first** — close the 5 open graph
   re-analyze bugs (defends the north-star directly).
3. **Triage the B.4* 24× analyze perf regression first** (clarion-c20593d0d8).

## The call

Option 1. It is the head of the critical path (unblocks the most downstream
work) and carries a real **trust-surface** argument: `loomweave-core` is the
crate that forks sandboxed plugin subprocesses and mints stable entity
identity (SEI), yet it also links an outbound HTTP client (`reqwest`) purely
for the LLM/embedding providers. Moving the providers to a dedicated leaf crate
removes HTTP from the plugin-supervisor + SEI crate. Owner confirmed this as the
Now bet this session.

The bet was de-risked the same session: a solution-architect trace confirmed the
two provider modules import **no** workspace code, so `loomweave-llm` is a pure
leaf crate (no `core → llm` dependency, no cycle) — a clean lift-and-shift.

## Reversal trigger

Reopen / re-shape the bet if extraction is found to force a
`loomweave-core → loomweave-llm` dependency (which would re-link `reqwest`
transitively and void the trust-surface goal). Measured by the PRD-0001
acceptance gate: `cargo tree -p loomweave-core` must resolve **no** `reqwest`.
If it cannot, the bet is not a clean move and returns to `DECIDE`. (Trace says
this is low-risk, but the gate is the falsifier.)
