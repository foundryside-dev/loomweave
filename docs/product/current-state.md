# Loomweave — Current State (resume brief)

> Refreshed **2026-06-29** (PDR-0008 — warpline keying-gap fix shipped). Next
> session: start here, then `vision.md` (grant), `roadmap.md` + `metrics.md`, then
> reconcile the tracker IDs below against Filigree.

## The bet right now

**The Now horizon is still open — DECIDE has not run.** No new Now bet was picked.
The session segments since 2026-06-26 spent on (a) a federation MCP-transport
reliability cycle (PDR-0006) and (b) repo-hygiene cleanup (PDR-0007) — both ahead
of / beside the DECIDE, not consuming it. **The three recorded Now candidates
remain on deck, untouched** (roadmap.md):

1. **Incremental-analyze correctness cluster** — defends the north-star directly.
   Open: clarion-feab311907, clarion-14398b2536, clarion-a65cb18b02 (all confirmed).
2. **Per-provider split** (clarion-4328c5c757) — unblocked by the loomweave-llm extraction.
3. **B.4\* analyze 24× perf regression** (clarion-c20593d0d8, triage).

…OR the next DECIDE may instead **accept/merge the in-flight warpline churn-fill**
(PR #77) and close its keying gap. That choice is the first open question below.

## In flight (tracker authoritative for status)

- **Warpline churn-fill TRANSPORT — PR #77, OPEN vs `main`** (branch `feat/warpline-churn-consumer`).
  Lights up the dead `entity_high_churn_list` / `entity_recent_change_list`
  surfaces via Warpline's churn read. NO-GO transport bug fixed; honesty
  disclosures (`churn_truncated`, `churn_unresolved`) added; validated live on
  lacuna. **Not yet accepted** — accept/merge is the remaining call. (The keying
  gap it disclosed is now FIXED + merged — see "Decided/shipped" below.)
- **ADR-054 Rust reachability-root tags** (clarion-05fdd0490e, `building`,
  assignee `claude`, **a concurrent actor** — not this owner-session). Do not grab it.

## Decided this session (2026-06-29)

- **PDR-0008** — **warpline churn keying gap root-caused + FIXED** (merged to `main`).
  Root cause was loomweave nulling briefing-blocked (secret-bearing) entities' SEI
  on its MCP read surface (NOT the dialect mismatch the observation guessed), which
  defeated warpline's `reresolve-sei` backfill → churn `0` for those files. Fix:
  the content-free SEI now rides the blocked-entity projections via a `blocked_sei`
  helper (owner-ratified secret-handling posture reversal; ADR-034 2026-06-29
  amendment); secret content still withheld; live-proven on lacuna. Issue
  clarion-4b3061b1ac (closed by merge); deep-pagination half split to
  clarion-obs-acffc4e8a1. **Warpline-side follow-up:** re-run `reresolve-sei` to
  heal already-minted NULL `entity_keys.sei` rows.
- **PDR-0007** — disposed of the stale `weft/legis-conformance` branch (orphaned,
  no PR; tested the deleted `parse_legis_rename_json` against the pre-#73
  `/git/renames` shape → won't compile). Deleted it (owner-authorized); carried its
  intent forward as **clarion-0715faa9d6** (rewrite the shared rename-feed
  conformance golden against the new shape). Obsolete commit `9c30ce0` preserved in
  the issue.
- **Repo hygiene:** 7 merged remote branches deleted (PRs #53/#54/#74/#75/#76/#78
  + the stale legis branch); only `feat/warpline-churn-consumer` (open PR #77) plus
  historical (`rc4`, `rename/clarion-to-loomweave`) and auto-managed
  (`dependabot/*`) branches remain. Resolves the prior checkpoint's open-question #7.

## Metric signals

- **No new readings this session** (cleanup + investigation only — no CI runs or
  sweeps). Carried unchanged from 2026-06-28: CI floor GREEN on PR #78;
  federation MCP-transport correctness = 0 mis-framed stdio clients; north-star
  (open collision families) = 0, not re-swept. See `metrics.md`.
- **`tools/list` 22 KB budget** — still UNKNOWN, carried from 2026-06-24; not re-measured.

## Open questions / awaiting owner

1. **Next Now:** accept/merge the in-flight warpline churn-fill (PR #77) and close
   its keying gap, **or** DECIDE a new Now from the three candidates. (DECIDE has
   not run since 2026-06-26.)
2. **Fresh north-star successor target** (collision target met; candidate identified).
3. ~~Warpline keying gap (clarion-obs-30c0ef3b0a)~~ — **RESOLVED 2026-06-29
   (PDR-0008).** Root cause was loomweave-side (briefing-blocked SEI nulled), fixed
   + merged (clarion-4b3061b1ac). Remaining: (a) **warpline-side** — re-run
   `reresolve-sei` to heal already-minted NULL rows (operational, cross-product);
   (b) deep-pagination half now tracked as clarion-obs-acffc4e8a1 (open).
4. **`tools/list` byte budget** — re-measure; may be breached.
5. **Adoption metric** — still undecided; telemetry is escalation-gated (local-first).
6. **ESCALATION (carried, outward-facing, gated):** Wardline Amendments 4–9 corpus
   re-vendor handoff — prepared, not pushed. Do not push without owner sign-off.
7. **Legis conformance golden (clarion-0715faa9d6, PDR-0007)** — when picked up, the
   cross-member "agreed vector home" step (legis vendoring the byte-identical
   golden + pinning the same sha) is **outward-facing → escalates**. The loomweave
   consumer half is in-grant; the legis push is not.
8. **Residual (disclosed in PR #78, not bounded):** `resolve_filigree_mcp_command`
   runs `filigree mcp-status --json` via a blocking `.output()` *before* the
   timeout-bounded section — a hung mcp-status is an unbounded wait. Short-lived;
   bounding it is a follow-up.

## Where the next session starts

1. **DECIDE:** resolve open question 1 — either accept/merge warpline #77 (+ plan
   the keying-gap fix) or pick a new Now from the three candidates. Then set the
   north-star successor target, and DISPATCH (PRD + plan) as usual.
