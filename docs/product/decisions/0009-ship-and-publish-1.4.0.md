# PDR-0009: Ship and publish loomweave 1.4.0 (minor release, all distribution channels)

- **Date:** 2026-07-01
- **Status:** accepted — **owner-authorized in-session** (the release + its public
  publish are outward-facing one-way doors; both were explicitly directed by the
  owner this session: "please push it and verify the package", and later "complete
  the 1.4.0 crates.io publish … do this please"). Not a pending escalation — done.
- **PRD:** none (a distribution/packaging milestone, not a fresh PRD-scoped bet; it
  ships already-accepted bets — PDR-0005 loomweave-llm extraction, PDR-0006/0008
  warpline federation — plus 1.3.1→1.4.0 accumulated work)
- **Tracker:** no dedicated issue (release chore). Related PRs this cycle: #80
  (P2 review fixes — the immediate trigger), #81 (version bump + CHANGELOG), #82
  (.gitignore), #83 (crates.io publish-order fix), #84 (test time-bomb fix).

## Context

45 commits had accumulated on `main` since `v1.3.1` (2026-06-22) with **no release
cut** — including user-facing features (the Warpline churn consumer lighting up
`entity_high_churn_list` / `entity_recent_change_list`; ADR-054 Rust dead-code
reachability roots) and a new workspace crate (`loomweave-llm`, PDR-0005). The
session opened resolving two P2 review findings (#80: the plugin `anchor_entity_id`
trust-boundary strip + re-vendoring the Wardline taint golden — the same
conformance drift the prior checkpoint flagged, open question 4b). With those
landed and the floor green, cutting a release was the natural close.

Two facts forced the versioning call and the "publish now" decision:
1. **SemVer says minor, not patch.** Features shipped since 1.3.1 → `1.4.0`, not
   `1.3.2`. Presented as such (not a coin-flip) and confirmed by the owner.
2. **A tag push is irreversible.** `release.yml` on a `v*` tag publishes to real
   PyPI (append-only) + a public GitHub Release. So the tag was *prepared and
   held* first (branch → 20-file lockstep bump → CHANGELOG for the 45 commits →
   PR #81 → merge → annotated tag at the merged HEAD), and pushed only on explicit
   owner say-so.

## Options

1. **Don't release; keep accumulating on `main`.** Rejected: 45 commits of shipped
   value (incl. a new crate and two user-facing features) unreleased is a widening
   gap between `main` and what operators can `uv tool install`; the north-star
   ("prefer the graph over grep") only pays off for users on a released build.
2. **Cut a 1.3.2 patch.** Rejected: features shipped → SemVer requires a minor.
3. **Cut & publish 1.4.0 to all channels** (chosen) — minor bump, full lockstep,
   published to PyPI + GitHub Release + crates.io, with the tag held until the
   owner authorized the irreversible push.

## The call

**Option 3 — owner-authorized in-session.** Prepared as a held tag first
(reviewable CHANGELOG, no premature publish), then pushed on the owner's explicit
"push it". PyPI (all three packages) + GitHub Release published cleanly; cosign/
Rekor asset verification passed. The installed local `loomweave` was reinstalled
to 1.4.0 via uv.

**Release-process defects surfaced and fixed in the same cycle** (context, not
separate decisions):
- **crates.io partial-publish** — `release.yml`'s hand-maintained publish-order
  list never included the new `loomweave-llm` crate, so `cargo publish -p
  loomweave-mcp` failed ("no matching package named loomweave-llm found") and left
  `loomweave-llm` / `-mcp` / `-cli` unpublished on crates.io (PyPI + GitHub Release
  unaffected — separate channels). Fixed the order list (#83) **and** completed the
  publish by hand (owner-authorized) from a `v1.4.0` worktree; all 9 workspace
  crates now `PRESENT@1.4.0`.
- **Date time-bomb** — two `orientation_pack` summary-cache tests hardcoded
  `created_at: 2026-01-01` and read it back through a 180-day freshness window
  under the real wall clock, so `main` went red on 2026-07-01 (181 days) with no
  code change. Fixed by pinning a fixed clock (#84).

## Reversal trigger

Reopen (i.e. cut a `1.4.1` patch) if a **critical regression in 1.4.0** is found on
a released build — a correctness break in the graph/identity surface, a broken
`uv tool install loomweave==1.4.0`, or a plugin that fails to launch from the
published wheels. Metric anchor: the CI-floor guardrail (`metrics.md` §2) must stay
green on `main` post-release; a red floor traced to a 1.4.0-shipped change is the
signal. A published version is never unpublished — the remedy is always a new
patch, never a re-push of `1.4.0`.
