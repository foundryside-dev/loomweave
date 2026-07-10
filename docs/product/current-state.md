# Loomweave — Current State (resume brief)

> Refreshed **2026-07-10** (PDR-0010 — loomweave **1.4.1 public-surface tag
> checkpoint banked locally**; uv tools installed; no public push/publish). Prior:
> 2026-07-01 (PDR-0009 — loomweave 1.4.0 shipped to all channels). Next session:
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
reliability cycle (PDR-0006), (b) repo-hygiene cleanup (PDR-0007), (c) the
2026-07-01 segment — two P2 review fixes then cutting & publishing **1.4.0**
(PDR-0009), and (d) the 2026-07-10 local **1.4.1** public-surface/Plainweave
denominator checkpoint (PDR-0010) — all ahead of / beside the DECIDE, not
consuming it. **The three recorded Now candidates remain on deck, untouched**
(roadmap.md):

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
  assignee `claude`, **a concurrent actor** — not this owner-session). Filigree
  currently reports the claim as stale (>48h / lease expired). Do not grab it
  without an explicit owner decision.

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

## Decided/shipped this session (2026-07-10)

- **PDR-0010 — loomweave 1.4.1 public-surface tag checkpoint banked locally.**
  The missing functionality was Loomweave-side, not Plainweave-side: Plainweave's
  denominator now has Loomweave tags/MCP shortcuts for `entry-point`, `http-route`,
  `exported-api`, and `cli-command`. Python extraction now covers augmented
  `__all__`, re-exported modules, manual CLI patterns, and common main-guard
  wrappers (`SystemExit`, `sys.exit`, bare `exit` / `quit`, `asyncio.run`,
  `anyio.run`, `typer.run`).
- **Review gaps closed before banking:** both root and standalone Rust plugin
  distribution lockfiles resolve `crossbeam-epoch` to `0.9.20`, clearing the
  RUSTSEC-2026-0204 lockfile issue; the Warpline churn conformance oracle is now
  opt-in for sibling checks (`WARPLINE_REPO`) instead of auto-running against
  `/home/john/warpline`, while `LOOMWEAVE_DRIFT_REQUIRED=1` still fails if the
  sibling is not configured.
- **Local install completed, no external release.** `loomweave`,
  `loomweave-plugin-python`, and `loomweave-plugin-rust` are installed via local
  uv at **1.4.1**. `main` was fetched/pruned on 2026-07-10 and had no remote
  changes to merge; local `main` is ahead of `origin/main` by the checkpoint
  commits. No `v1.4.1` tag, PyPI/GitHub Release, crates.io publish, or push was
  performed.

## Metric signals

- **CI floor GREEN across the 1.4.0 release (2026-07-01)** — the release verify
  gate + PRs #80/#83/#84 all passed (Rust + Rust aarch64 + Python + e2e); local
  full-workspace nextest **1977 passed** with `WARDLINE_REPO` set. All three
  distribution channels published at 1.4.0. See `metrics.md`.
- **CI floor GREEN for the local 1.4.1 checkpoint (2026-07-10)** — version
  lockstep scripts, Rust fmt/checks, Python plugin tests (**239 passed**, coverage
  87.08%), MCP shortcut/context-budget nextest slices, root + dist cargo-deny
  advisory gates, and the Warpline oracle hermeticity checks passed. Wardline scan
  returned gate PASS but reported the current trust-boundary gate is inert (0
  recognized boundaries), so do not count it as meaningful taint coverage yet.
- **CI floor GREEN on PR #79** (`a980ef2`): all 4 CI checks (Rust + aarch64 +
  Python + Sprint-1 e2e); locally fmt + workspace clippy (-D warnings) + doc clean,
  nextest **1972/1973**. See `metrics.md`.
- **CI blind spot — RESOLVED 2026-07-10:** the
  `wardline_taint_fact_conformance_oracle` drift flagged 2026-06-29 was re-vendored
  this cycle (#80, PDR-0009), and the reusable verify workflow's required Rust
  job now fetches the authority fixture from public
  `foundryside-dev/wardline@main` and compares it byte-for-byte. The local
  Layer-2 test may still skip when the sibling is absent, but cross-repo drift
  now blocks PR merges and releases (clarion-72e1c1a07d).
- North-star (open collision families) = 0, **not re-swept** (this was federation
  correctness, not graph identity — no identity/extraction code touched).
- **`tools/list` 22 KB budget** — re-measured GREEN on 2026-07-10 after the new
  public-surface shortcuts (`tools_list_fits_the_context_budget` passed).

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
4b. ~~Promote clarion-72e1c1a07d to a guardrail.~~ **RESOLVED 2026-07-10.**
   The reusable verify workflow's required Rust job now checks the vendored
   taint-fact golden against the public Wardline `main` authority on every CI
   run. Missing authority data and byte drift block PR merges and releases.
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
9. **Externalize 1.4.1?** Local uv is installed at 1.4.1, but `main` has not been
   pushed and no public release artifacts exist. Pushing/tagging/publishing is
   outward-facing and requires explicit owner sign-off.

## Where the next session starts

1. **Choose the next product move:** either explicitly externalize the banked 1.4.1
   patch (owner-gated push/tag/publish), or proceed to **DECIDE a new Now** (the
   field remains the three recorded candidates: incremental-analyze correctness
   cluster / per-provider split / B.4\* perf). Set the north-star successor target,
   then DISPATCH (PRD + plan). The wardline-drift guardrail (open question 4b) is a
   cheap, in-grant pickup if a smaller bite is wanted first.
