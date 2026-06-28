# Loomweave — Current State (resume brief)

> Refreshed at checkpoint **2026-06-28**. Next session: start here, then
> `vision.md` (grant), `roadmap.md` + `metrics.md`, then reconcile the tracker
> IDs below against Filigree.

## The bet right now

**The Now horizon is still open — DECIDE has not run.** This session did not pick
a new Now bet. Instead, a federation MCP-transport reliability cycle (PDR-0006,
owner-directed) ran *ahead* of the DECIDE: the warpline churn-fill NO-GO and the
filigree-mcp seam bug. **The three recorded Now candidates remain on deck,
untouched** (roadmap.md):

1. **Incremental-analyze correctness cluster** — defends the north-star directly.
   Open: clarion-feab311907, clarion-14398b2536, clarion-a65cb18b02 (all confirmed).
2. **Per-provider split** (clarion-4328c5c757) — unblocked by the loomweave-llm extraction.
3. **B.4\* analyze 24× perf regression** (clarion-c20593d0d8, triage).

…OR the next DECIDE may instead **accept/merge the in-flight warpline churn-fill**
(PR #77) and close its keying gap. That choice is the first open question below.

## In flight (tracker authoritative for status)

- **Warpline churn-fill — PR #77, OPEN vs `main`** (branch `feat/warpline-churn-consumer`).
  Lights up the dead `entity_high_churn_list` / `entity_recent_change_list`
  surfaces via Warpline's churn read. NO-GO transport bug fixed; honesty
  disclosures (`churn_truncated`, `churn_unresolved`) added; validated live on
  lacuna. **Not accepted** — no PRD, no tracker issue (emergent branch). Merging it
  is within grant (internal delivery).
- **ADR-054 Rust reachability-root tags** (clarion-05fdd0490e, `building`,
  assignee `claude`, **a concurrent actor** — not this owner-session). Do not grab it.

## Decided this session (2026-06-28)

- **PDR-0006** — spent the open Now cycle on federation-transport reliability
  ahead of the three candidates (owner-directed). filigree-mcp newline-transport
  fix **shipped** (clarion-a5bfcf5ef9 closed; PR #78 → `main` `b5aabe8`, CI green
  incl. aarch64). warpline churn-fill driven to a validated fix (PR #77, open).
  Content-Length-vs-newline bug class **closed** (both federation stdio clients).

## Metric signals

- **CI floor — GREEN on PR #78** (Rust + aarch64 + Python + e2e); 131 federation
  tests + fmt/clippy/doc clean. Scoped verification (federation + downstream), not
  a full-workspace nextest. See `metrics.md`.
- **Federation MCP-transport correctness — 0** mis-framed stdio clients
  (grep-verified). New watchlist reading.
- **North star** (open collision families) — still **0**; **not re-swept** (this
  was transport, not graph correctness). Still needs a fresh successor target (owner).
- **`tools/list` 22 KB budget** — still UNKNOWN, carried from 2026-06-24; not re-measured.

## Open questions / awaiting owner

1. **Next Now:** accept/merge the in-flight warpline churn-fill (PR #77) and close
   its keying gap, **or** DECIDE a new Now from the three candidates. (DECIDE has
   not run since 2026-06-26.)
2. **Fresh north-star successor target** (collision target met; candidate identified).
3. **Warpline keying gap** (clarion-obs-30c0ef3b0a) — loomweave↔warpline
   locator-dialect + NULL-sei mismatch undercounts churn at the real operating
   point (disclosed via `churn_unresolved`, not silent). **Observation expires
   ~2026-07-12** — promote to a tracked issue or dismiss before then. Closing it
   likely needs a cross-product (warpline-side) change → would escalate.
4. **`tools/list` byte budget** — re-measure; may be breached.
5. **Adoption metric** — still undecided; telemetry is escalation-gated (local-first).
6. **ESCALATION (carried, outward-facing, gated):** Wardline Amendments 4–9 corpus
   re-vendor handoff — prepared, not pushed. Do not push without owner sign-off.
7. **Cleanup (awaiting owner OK to delete remote branches):**
   `origin/fix/filigree-mcp-newline-transport` (merged via #78) and
   `origin/feat/loomweave-llm-extraction` (merged via #76, carried). **Keep**
   `origin/feat/warpline-churn-consumer` — PR #77 is still open.
8. **Residual (disclosed in PR #78, not bounded):** `resolve_filigree_mcp_command`
   runs `filigree mcp-status --json` via a blocking `.output()` *before* the new
   timeout-bounded section — a hung mcp-status is an unbounded wait. Short-lived;
   bounding it is a follow-up.

## Where the next session starts

1. **DECIDE:** resolve open question 1 — either accept/merge warpline #77 (+ plan
   the keying-gap fix) or pick a new Now from the three candidates. Then set the
   north-star successor target, and DISPATCH (PRD + plan) as usual.
