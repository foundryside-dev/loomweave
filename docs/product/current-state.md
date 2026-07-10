# Loomweave — Current State (resume brief)

> Refreshed **2026-07-01** (PDR-0009 — loomweave **1.4.0 shipped** to all
> channels). Prior: 2026-06-29 (PDR-0008 — warpline keying-gap fix). Next session:
> start here, then `vision.md` (grant), `roadmap.md` + `metrics.md`, then reconcile
> the tracker IDs below against Filigree.
>
> **Concurrency note:** this checkpoint folded forward the 2026-06-29 owner-session
> checkpoint (`1823911`) that was committed to local `main` but never pushed (it
> diverged behind this session's release PRs). Its content is preserved here + on
> `origin/main`; the local `1823911` is now superseded and can be dropped.

## The bet right now

**The Now horizon is still open — DECIDE has not run.** No new Now bet was picked.
The session segments since 2026-06-26 spent on (a) a federation MCP-transport
reliability cycle (PDR-0006), (b) repo-hygiene cleanup (PDR-0007), and (c) the
2026-07-01 segment — two P2 review fixes then cutting & publishing **1.4.0**
(PDR-0009) — all ahead of / beside the DECIDE, not consuming it. **The three
recorded Now candidates remain on deck, untouched** (roadmap.md):

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

## Decided/shipped this session (2026-07-01)

- **PDR-0009 — loomweave 1.4.0 shipped to all channels** (owner-authorized). 45
  commits had accrued on `main` since `v1.3.1` with no release; two P2 review
  fixes (#80) landed, then a minor bump (features shipped → 1.4.0, not a patch):
  20-file lockstep bump + CHANGELOG (#81), tag `v1.4.0` **prepared and held**, then
  pushed on explicit owner say-so. **PyPI** (all 3 packages) + **GitHub Release**
  (cosign/Rekor-verified) + **crates.io** (all 9 crates) now at 1.4.0. Local
  `loomweave` reinstalled to 1.4.0 via uv.
- **P2 review fixes (#80)** — the plugin `anchor_entity_id` trust-boundary strip
  (a plugin finding could otherwise forge the trusted anchor → FK-hard-fail the
  analyze run or silently mis-anchor) **and** re-vendoring the Wardline taint
  golden — the same conformance drift the prior checkpoint flagged (open question
  4b). Fixes the *drift*; the CI blind-spot itself remains (see 4b).
- **Two release-process defects found + fixed** (context, not new bets): the
  crates.io publish-order list omitted the new `loomweave-llm` crate → partial
  publish; fixed (#83) **and** completed the publish by hand from a `v1.4.0`
  worktree. And a date **time-bomb** — two summary-cache tests hardcoded
  `created_at: 2026-01-01` read through a 180-day window under the real clock, so
  `main` went red on 2026-07-01 with no code change; fixed by pinning the clock
  (#84).

## Metric signals

- **CI floor GREEN across the 1.4.0 release (2026-07-01)** — the release verify
  gate + PRs #80/#83/#84 all passed (Rust + Rust aarch64 + Python + e2e); local
  full-workspace nextest **1977 passed** with `WARDLINE_REPO` set. All three
  distribution channels published at 1.4.0. See `metrics.md`.
- **CI floor GREEN on PR #79** (`a980ef2`): all 4 CI checks (Rust + aarch64 +
  Python + Sprint-1 e2e); locally fmt + workspace clippy (-D warnings) + doc clean,
  nextest **1972/1973**. See `metrics.md`.
- **CI blind spot — drift RE-VENDORED, spot REMAINS:** the
  `wardline_taint_fact_conformance_oracle` drift flagged 2026-06-29 was re-vendored
  this cycle (#80, PDR-0009), so the *drift* is cleared — but the underlying
  local-red/CI-green divergence (the oracle skip-cleans when the `~/wardline`
  sibling is absent, so a drift still passes CI) is structural and **still open →
  promote clarion-72e1c1a07d** (open question 4b).
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
4b. **Promote clarion-72e1c1a07d to a guardrail (carried, 2026-06-29).** The
   `wardline_taint_fact_conformance_oracle` local-red/CI-green divergence recurred
   (flagged 2026-06-26, fired 2026-06-29). The *drift* was re-vendored 2026-07-01
   (#80, PDR-0009) — but that is another manual re-vendor, not a fix: the oracle
   still skip-cleans when `~/wardline` is absent, so the next sibling drift passes
   CI again. Still needs a CI-visible check or a re-vendor cadence. **Promotion
   remains open.**
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

1. **DECIDE a new Now** (warpline is shipped **and 1.4.0 is now published to all
   channels**, so no release chore is pending — the field is the three recorded
   candidates: incremental-analyze correctness cluster / per-provider split /
   B.4\* perf). Set the north-star successor target, then DISPATCH (PRD + plan).
   The wardline-drift guardrail (open question 4b) is a cheap, in-grant pickup if a
   smaller bite is wanted first.
