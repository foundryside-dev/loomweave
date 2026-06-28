# PDR-0006: Spend the open Now cycle on federation MCP-transport reliability (warpline churn-fill + filigree-mcp seam)

- **Date:** 2026-06-28
- **Status:** accepted (within the authority grant — reversible internal code + sanctioned merge-to-`main`; owner-directed work)
- **PRD:** none (reliability defects, not a PRD-scoped bet; the warpline churn-fill is feature-shaped but was driven as a NO-GO fix on an existing branch, not a fresh PRD)
- **Tracker:** clarion-a5bfcf5ef9 (filigree transport bug, **closed**, close-commit `main@b5aabe8`); clarion-obs-30c0ef3b0a (warpline deep-pagination + locator-dialect/NULL-sei keying-gap follow-ups, open observation); PRs #77 (warpline, **open** vs `main`) and #78 (filigree, **merged** `main@b5aabe8`)

## Context

At RESUME the Now horizon was open (PDR-0005 turned it over 2026-06-26) with
three recorded candidates on deck — incremental-analyze correctness cluster,
per-provider split, B.4\* perf — and DECIDE explicitly deferred to "the next
session." This session did **not** pick from those candidates. The owner directed
two pieces of federation work instead:

1. **Fix a live NO-GO on the warpline churn-fill feature** — the
   `entity_high_churn_list` / `entity_recent_change_list` MCP surfaces (dead by
   design in v1.0, lit up by consuming Warpline's frozen
   `warpline_entity_churn_count_get` at read time) **hung** whenever
   `integrations.warpline.enabled: true`.
2. **Then review filigree** — which surfaced the *same* federation-wide bug class.

Root cause (both seams): Loomweave's federation MCP **stdio** clients framed
requests with the Content-Length plugin framing (ADR-002), but the sibling MCP
servers speak **newline-delimited** JSON-RPC (`warpline-mcp` reads
`for line in sys.stdin`; `filigree-mcp` uses the MCP Python SDK's
`stdio_server`). The mis-framing hung the read. The warpline path had a second
defect: its default launcher was the invalid `warpline mcp` subcommand.

## What was done (this session)

- **warpline.rs (PR #77, open vs `main`):** newline-delimited transport on a
  worker thread bounded by `recv_timeout` + kill (degrades to honest
  `warpline-unreachable`, never hangs); launcher fixed to the real `warpline-mcp`
  binary; sends required `repo`, drops unsupported `actor`; parses
  `structuredContent`/`text`; added honored `timeout_seconds`. Plus an **honesty
  floor** the live run forced out: `churn_truncated` (warpline's 200-item overflow
  cap) and `churn_unresolved` (keying-miss `0` ≠ never-observed `0`) so neither
  fabricated zero is read as a clean answer. Validated live on `/home/john/lacuna`
  (real ranked churn; scoped → 54/58 `churn_unresolved`; disabled → honest-empty).
- **filigree.rs (PR #78, merged `main@b5aabe8`, CI green incl. aarch64):** mirrored
  the newline transport + bounded timeout + `filigree-mcp` fallback launcher.
  Repairs the stdio `observation_create`/`observation_dismiss` path
  (`propose_guidance`, guidance promotion) — the HTTP read path was unaffected.
- **Bug class closed:** `grep` confirmed warpline + filigree were the *only* two
  Content-Length stdio clients in `loomweave-federation`; both now newline-framed.
- Filed follow-ups (clarion-obs-30c0ef3b0a): deep-pagination via warpline's
  overflow dump for >200-candidate scopes, and the loomweave↔warpline
  locator-dialect + NULL-sei keying gap that undercounts churn at the real
  operating point. Closed clarion-a5bfcf5ef9.

## The call

**Take the federation-transport reliability cycle ahead of the three recorded Now
candidates** (owner-directed). A NO-GO on a feature surface plus a confirmed-broken
cross-product seam (silent observation-write failure) are reliability defects in
shipping/shipped capability and serve the vision's anti-goals directly — "honest
extraction envelope" and "federation degrades cleanly when a sibling misbehaves."
That outranks greenfield candidate work. **The three Now candidates remain
undecided and on deck** — this cycle did not consume them; it inserted ahead of
the DECIDE.

The warpline churn-fill (PR #77) is **in flight, not accepted** — it is functional
and honest-degrading, but the keying-gap means it undercounts churn for
SEI-mismatched entities (disclosed, not silent). Accepting/merging #77 and closing
the keying gap are next-session calls, not banked here.

## Reversal trigger

This is a completed reliability fix, not a reopenable bet, so the trigger is a
**re-emergence** condition — reopen the federation-transport theme if **either**:

1. a third Content-Length stdio client appears in `loomweave-federation` (the
   `grep -rn "write_frame\|read_frame\|ContentLengthCeiling"` over
   `crates/loomweave-federation/src/` returns a hit), **or**
2. after PR #77 merges, the warpline keying-gap (clarion-obs-30c0ef3b0a) shows
   the churn surfaces returning *materially wrong* data at the real operating
   point — i.e. `churn_unresolved.count` is a large fraction of in-scope
   candidates on the reference repos (functional-but-untrustworthy), promoting the
   keying gap from a disclosed caveat to a correctness defect that needs a
   cross-product (warpline-side) fix.
