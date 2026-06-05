---
name: wardline-gate
description: >
  Use when scanning for or fixing trust-boundary / taint findings, when a
  `wardline scan` reports a defect, or when wiring wardline into an agent's
  edit-verify loop. Explains the scan -> explain -> fix-at-the-boundary ->
  rescan cycle and the baseline-vs-waiver discipline.
---

# Wardline: the trust-boundary gate

Wardline is a deterministic, whole-program static taint analyzer. It marks trust
boundaries with two decorators from `wardline.decorators`: `@external_boundary`
(untrusted data arriving from outside) and `@trusted` (a producer that must only
receive validated data). When untrusted data reaches a trusted producer it raises
`PY-WL-101` at `ERROR`.

## The loop

1. **Scan.** Run `wardline scan . --fail-on ERROR` (or call the `scan` MCP tool).
   Read the gate verdict and the active (non-suppressed) findings — `active` is
   the population the gate enforces on.
2. **Explain.** For each active defect, call `explain_taint` with the finding's
   `fingerprint`, `path`+`line`, and its `qualname` as `sink_qualname`. Do this
   right after the scan and before editing — a stale fingerprint returns an error.
   With a Loomweave store configured, pass `chain: true` to walk the full taint
   chain back to the originating boundary.
3. **Fix at the BOUNDARY, not the sink.** Add validation or rejection at the hop
   where untrusted data should have been checked — not a band-aid at the sink.
4. **Re-scan.** Confirm the finding is gone.

## Exit codes (CLI path)

- `0` — clean (or gate not requested).
- `1` — the gate tripped: a non-suppressed defect at/above `--fail-on`.
- `2` — a wardline error (bad config, unreadable path). Not a finding.

Branch on the code. On a trip, read the structured report wardline just wrote —
the finding names the function, file, and lines, which is enough to locate the
leak.

## Suppression discipline

Prefer FIXING a finding. Suppress only a finding you have judged a true
non-issue, always with a reason:

- MCP `baseline` — snapshot current defects so only NEW findings surface.
  `overwrite: false` (default) refuses to clobber an existing baseline;
  `overwrite: true` re-derives it. A coarse, whole-set tool; requires a reason.
- `waiver_add` — waive ONE finding by fingerprint with a mandatory reason and an
  expiry date. An audited, time-boxed exception.
- `wardline judge` (opt-in, network) — an LLM pass that labels each defect
  TRUE/FALSE positive. Never runs automatically, never folded into scan; fails
  loud with no API key so "couldn't triage" is never mistaken for "nothing to
  triage". Above-floor false positives can be recorded as audited suppressions.

## CLI vs MCP

- **CLI:** `wardline scan`, `wardline judge`, `wardline baseline create/update`.
  Branch on the exit code; read the findings file it writes.
- **MCP:** `wardline mcp` exposes `scan`, `explain_taint`, `fix`, `judge`
  (network), `baseline`, `waiver_add`; resources
  `wardline://vocab|rules|config|config-schema`; and the `wardline:loop` prompt.
  The server is stateless — the read-only tools are pure functions of your code
  on disk and your config.
