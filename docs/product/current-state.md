# Loomweave — Current State (resume brief)

> Refreshed at checkpoint **2026-06-26**. Next session: start here, then
> `vision.md` (grant), `roadmap.md` + `metrics.md`, then reconcile the tracker
> IDs below against Filigree.

## The bet right now

**Now horizon is turning over.** The recorded Now bet — the `loomweave-llm`
extraction (clarion-141e9c08c8) — is **accepted and merged** this session
(PDR-0005, PR #76 → `main` `b346328`). `loomweave-core` no longer links `reqwest`;
a standing trust-surface CI gate enforces it. **The next session's DECIDE picks
the new Now.** Candidates (intent only — roadmap.md):

1. **Incremental-analyze correctness cluster** — defends the north-star directly;
   already shrinking (3 graph-correctness bugs closed in the drift-window). Still
   open: clarion-feab311907, clarion-14398b2536, clarion-a65cb18b02 (all confirmed).
2. **Per-provider split** (clarion-4328c5c757) — now **unblocked** by the extraction.
3. **B.4\* analyze 24× perf regression** (clarion-c20593d0d8, triage).

## In flight (tracker authoritative for status)

- **clarion-05fdd0490e** (ADR-054 Rust reachability-root tags) — `building`,
  assignee `claude` (**a concurrent actor**, not this owner-session; increments
  1+2 already shipped). Do not grab it.
- Nothing else claimed by this session.

## Decided this session (2026-06-26)

- **PDR-0005** — accepted the `loomweave-llm` extraction bet as complete (all 6
  PRD-0001 criteria met on the merge commit). Closed clarion-141e9c08c8;
  unblocked clarion-4328c5c757.
- **Authority grant re-confirmed as-is** by owner; `Last reviewed` stamped 2026-06-26.
- **Owner steer:** execute loomweave-llm this session (vs. reconcile-first or
  steer-to-cluster) — done.
- Filed **clarion-72e1c1a07d** (pre-existing wardline conformance drift; not from this bet).

## Metric signals

- **Trust-surface guardrail — TARGET MET** (`yes → no`): `loomweave-core` links no
  `reqwest`; CI-enforced. See `metrics.md`.
- **North star** (open collision families) — still **0**; needs a fresh successor
  target (owner). The graph-correctness cluster shrinking is live support for the
  candidate ("fabricated-edge / dropped-file / collision count stays 0").
- **CI floor — GREEN, independently verified** on `b346328`. Caveat: the
  wardline-taint conformance oracle makes the *local* floor red / *CI* green when
  the `~/wardline` sibling drifts (clarion-72e1c1a07d) — a CI blind spot.
- **`tools/list` 22 KB budget** — still UNKNOWN, carried from 2026-06-24; not re-measured.

## Open questions / awaiting owner

1. **New Now bet** — DECIDE from the candidates above (the horizon turned over).
2. **Fresh north-star successor target** (collision target met; candidate identified).
3. **`tools/list` byte budget** — re-measure; may be breached.
4. **Adoption metric** — still undecided; telemetry is escalation-gated (local-first).
5. **ESCALATION (carried forward):** Wardline Amendments 4–9 corpus re-vendor
   handoff — prepared, not pushed, **outward-facing, gated.** Do not push without
   owner sign-off.
6. **Cleanup (minor):** remote branch `origin/feat/loomweave-llm-extraction` is
   merged but still on origin — awaiting owner OK to delete (per standing rule).

## Where the next session starts

1. **DECIDE the new Now** from the three candidates (cluster / per-provider split /
   B.4\* perf), and set the north-star successor target. Then DISPATCH (PRD +
   plan) as usual.
