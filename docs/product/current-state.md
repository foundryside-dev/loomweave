# Loomweave — Current State (resume brief)

> Refreshed at checkpoint **2026-06-24**. Next session: start here, then
> `vision.md` (grant), `roadmap.md` + `metrics.md`, then reconcile the tracker
> IDs below against Filigree.

## The bet right now

**Extract `loomweave-llm` from `loomweave-core`** (clarion-141e9c08c8, PDR-0003)
— pay the head-of-critical-path architecture debt and remove outbound HTTP from
the plugin-supervisor + SEI crate. **Dispatched, not yet executed:**
- PRD: `docs/product/prd/PRD-0001-loomweave-llm-extraction.md` (ready-for-planning).
- Boundary ratified by a solution-architect pass (pure-leaf crate, no cycle).
- Implementation plan: `docs/plans/2026-06-24-loomweave-llm-extraction.md`.
- **Next action:** `/review-plan` that plan, then execute (subagent-driven or a
  fresh `executing-plans` session). Metric it moves: trust-surface
  `loomweave-core links reqwest: yes → no`.

## In flight (tracker authoritative for status)

- Nothing claimed/in-progress. The Now bet's tracker item (clarion-141e9c08c8)
  is dispatched (PRD + plan) but not started — no code changed this session.
- Active defect cluster (Now/Next): clarion-feab311907, clarion-14398b2536,
  clarion-a65cb18b02 (all confirmed); clarion-abda98c869, clarion-c20593d0d8
  (triage).

## Decided this session (2026-06-24)

- **Authority grant CONFIRMED as-is** by owner; `Last reviewed` stamped
  2026-06-24 (content unchanged).
- **PDR-0003** — Now bet = `loomweave-llm` extraction.
- **PDR-0004** — accepted the 1.1.0 / Rust-plugin-gold bet as **complete**
  (PDR-0002 gate satisfied; all 4 collision families fixed; now v1.3.1).

## Metric signals

- North star (open collision families) **4 → 0, TARGET MET** — needs a fresh
  successor target (owner). See `metrics.md`.
- New guardrail: trust-surface (`loomweave-core` HTTP) — currently `yes`, target
  `no`, met when the Now bet lands.
- **Re-check needed:** `tools/list` 22 KB budget (MCP surface grew; margin was 13
  bytes — reading unknown). CI floor presumed green at 1.3.1, not re-verified.

## Open questions / awaiting owner

1. **Fresh north-star target** now that the collision-family target is met.
2. **`tools/list` byte budget** — re-measure; may be breached.
3. **Adoption metric** — still undecided; telemetry is escalation-gated (local-first).
4. **ESCALATION (carried forward):** the Wardline Amendments 4–9 corpus
   re-vendor handoff is **prepared but not pushed** — outward-facing, gated. Do
   not push without owner sign-off.

## Where the next session starts

1. `/review-plan docs/plans/2026-06-24-loomweave-llm-extraction.md`, then execute
   the `loomweave-llm` extraction (the recorded Now bet). On completion, run the
   PRD-0001 acceptance gate and bank the trust-surface metric flip.
