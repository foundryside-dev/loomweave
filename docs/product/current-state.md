# Loomweave — Current State (resume brief)

> Written at bootstrap, 2026-06-11. Next session: start here, then
> `vision.md` (grant), `roadmap.md` + `metrics.md`, then reconcile the
> tracker IDs below against Filigree.

## The bet right now

Ship the **1.1.0 release line (rc4)** with the Rust language plugin at
**gold** — the Sprint-4 closeout verdict was *not gold*; four entity-ID
collision families (self-type-path, trait-path, `#[path]`-module, `const _`)
remain as the gold blockers. Alongside: MCP-surface polish (4 of 6 audit
tickets already shipped) and incremental-analyze correctness bugs.

## In flight (tracker is authoritative for status)

- Nothing. The bootstrap-time WIP (clarion-7c9336163e) closed the same day:
  the dormant `wardline.yaml` manifest ingest was retired (rc4 @ 1bd27b0,
  retarget evaluated and rejected). clarion-f3eb3852d6 (Python deep-nesting
  characterization, a roadmap Next item) also landed (d5baac5).

## Recently landed (context, not work)

- MCP/command-surface audit follow-ups shipped and closed: callers-honesty
  (e5327dc), skill-dialect (43b7b25), token-budget (13b20bc),
  finding-filter validation (7722942).
- ADR-052 duplicate-qualname first-wins semantics frozen (4cd6c4f).
- Rust plugin merged to rc4 (2380c88); sprints 1–4 (hardening, edges,
  scale-QA, gold closeout) landed; ADR-049 Amendments 4–9 shipped.

## Decided at bootstrap (2026-06-11, owner-confirmed)

- **Authority grant CONFIRMED** as drafted (`vision.md`).
- **Gold gates 1.1.0** (PDR-0002): all 4 collision families fixed before the
  cut; reversal trigger 2026-06-30.

## Open questions

1. **North-star TARGET date** (gold by 2026-06-30) is still a placeholder.
2. **Wardline handoff** (Amendments 4–9 corpus re-vendor) is prepared but
   not pushed — outward-facing, escalation-gated.
3. **Adoption metric** — does the owner want one at all, given local-first?

## Where the next session starts

1. DECIDE/DISPATCH the top Now bet: the 4 collision-family fixes (check
   whether their Filigree issues are filed and ready — Sprint-4 memo says
   filed; they did not appear in the open-issue list at bootstrap, so verify
   where they were filed).
