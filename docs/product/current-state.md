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

The warpline churn-fill (transport **and** keying gap) has now shipped — see
"Decided/shipped" below — so it is no longer a DECIDE candidate.

## In flight (tracker authoritative for status)

- **(none of this owner-session's)** — both warpline pieces merged (PR #77 transport
  `1d2b4fa`; PR #79 keying-gap fix `a980ef2`). The federation-transport cycle
  (PDR-0006) and its keying-gap follow-up (PDR-0008) are fully banked.
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
- **This-session housekeeping (execution of PDR-0008, not new decisions):**
  PR #77 (warpline transport) merged to `main` `1d2b4fa` by the owner/Bid-1 flow,
  concurrent with the keying-gap work; the merged `feat/warpline-churn-consumer`
  (#77) + `fix/briefing-blocked-sei-federation-key` (#79) remote branches deleted +
  local refs pruned (incl. the disposed `weft/legis-conformance` leftover); stale
  "#77 open" product docs corrected on `main` `a138d9a`; the fixed `loomweave`
  binary rebuilt from `main` and atomically installed into the local uv tool
  (hash `bc4f162b`), live-verified to expose the blocked-entity SEI. Remote now:
  `main` + historical (`rc4`, `rename/clarion-to-loomweave`) + auto-managed
  `dependabot/*` only.

## Metric signals

- **CI floor GREEN on PR #79** (`a980ef2`): all 4 CI checks (Rust + aarch64 +
  Python + Sprint-1 e2e); locally fmt + workspace clippy (-D warnings) + doc clean,
  nextest **1972/1973**. See `metrics.md`.
- **CI blind spot RECURRED:** the lone local nextest failure was
  `wardline_taint_fact_conformance_oracle` (vendored golden drifted from live
  `~/wardline`) — the local-red/CI-green divergence flagged 2026-06-26, now
  recurred → **promote clarion-72e1c1a07d to a guardrail** (new open question).
- North-star (open collision families) = 0, **not re-swept** (this was federation
  correctness, not graph identity — no identity/extraction code touched).
- **`tools/list` 22 KB budget** — still UNKNOWN, carried from 2026-06-24; not re-measured.

## Open questions / awaiting owner

1. **Next Now:** DECIDE a new Now from the three candidates (warpline churn-fill +
   its keying gap have now SHIPPED, so they are no longer a candidate). DECIDE has
   not run since 2026-06-26.
2. **Fresh north-star successor target** (collision target met; candidate identified).
3. ~~Warpline keying gap (clarion-obs-30c0ef3b0a)~~ — **RESOLVED 2026-06-29
   (PDR-0008).** Root cause was loomweave-side (briefing-blocked SEI nulled), fixed
   + merged (clarion-4b3061b1ac). Remaining: (a) **warpline-side** — re-run
   `reresolve-sei` to heal already-minted NULL rows (operational, cross-product);
   (b) deep-pagination half now tracked as clarion-obs-acffc4e8a1 (open).
4. **`tools/list` byte budget** — re-measure; may be breached.
4b. **Promote clarion-72e1c1a07d to a guardrail (NEW, 2026-06-29).** The
   `wardline_taint_fact_conformance_oracle` local-red/CI-green divergence (vendored
   golden drifts from live `~/wardline`) has now recurred (flagged 2026-06-26, fired
   2026-06-29) — its own trigger says promote on recurrence. Needs a CI-visible
   check or a re-vendor cadence so a sibling drift can't pass CI.
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

1. **DECIDE a new Now** (the warpline work is fully shipped, so the field is the
   three recorded candidates: incremental-analyze correctness cluster /
   per-provider split / B.4\* perf). Set the north-star successor target, then
   DISPATCH (PRD + plan). The wardline-drift guardrail (open question 4b) is a
   cheap, in-grant pickup if a smaller bite is wanted first.
