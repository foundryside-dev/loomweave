# PDR-0001: Bootstrap the product workspace from observed state

- **Date:** 2026-06-11
- **Status:** accepted

## Context

`/own-product` ran with no existing workspace at `docs/product/`. Ownership
is stateful; a stateless agent's only memory is this workspace, so one had to
be constructed.

## What was observed

- README.md + `docs/loomweave/1.0/` design ladder: purpose, audience,
  local-first and federation-optional anti-goals.
- Git history on `rc4`: four Rust-plugin sprints (hardening, edges,
  scale-QA, gold closeout), MCP-surface audit follow-ups, ADR-049/050/051/052.
- Filigree tracker: 25 open items, 1 in progress (clarion-7c9336163e),
  critical path = loomweave-llm crate extraction → per-provider split.
- Sprint-4 QA verdict: **not gold** — 4 collision families filed as blockers.

## The call

Seed `vision.md`, `roadmap.md` (intent only), `metrics.md` (falsifiable
placeholders), and `current-state.md` from that evidence. Infer the Now bet
as "ship 1.1.0/rc4 with the Rust plugin at gold". Write the authority grant
from the standard escalation taxonomy, marked `DRAFT — unconfirmed`.

## Reversal trigger

Revisit the entire bootstrap once the human confirms (or corrects) the
vision, the inferred Now bet, and the authority grant. Anything marked
**[ASSUMED]** or **[PLACEHOLDER]** is presumed wrong until confirmed.
