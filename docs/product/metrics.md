# Loomweave — Metrics

> Bootstrapped 2026-06-11. Baselines are real observed readings; **TARGET
> numbers and dates are placeholders for the human owner to set** — they are
> drafted falsifiably (a number and a date) so they can be confirmed or
> rejected, never left directional.

## North star

**Graph trustworthiness on the reference QA sweep** — a consult agent can
only prefer the graph over grep if the graph is correct. Proxy: open
entity-identity defect families (collisions, fabricated edges, dropped files)
found by the adversarial 4-corpus QA sweep (ripgrep / tokio / + 2).

- `BASELINE (2026-06-11): 4 open collision families (Sprint-4 gold verdict)`
- `TARGET: 0 open families, gold verdict recorded — by 2026-06-30 [PLACEHOLDER — confirm]`

## Guardrails

1. **CI floor stays green** (ADR-023): fmt, clippy `-D warnings`, build,
   nextest, doc, deny, ruff, mypy --strict, pytest, e2e scripts.
   - `BASELINE (2026-06-11): green, ~1450 nextest tests`
   - `TARGET: green on every rc4 merge — standing, no end date`
2. **MCP context tax stays under budget**: `tools/list` payload has a
   CI-enforced 22,000-byte budget.
   - `BASELINE (2026-06-11): 13 bytes under budget`
   - `TARGET: never exceeds budget; any schema growth buys bytes elsewhere first — standing`
3. **Identity stability**: SEI churn on unchanged re-analyze of reference
   corpora.
   - `BASELINE (2026-06-11): 0 SEI churn (Sprint-3 sweep)`
   - `TARGET: stays 0 across the rc4 line — standing`

## Watchlist (not yet a target)

- **Subsystem-count drift** on unchanged re-analyze (ripgrep 9→14,
  tokio 28→42 in the Sprint-3 sweep) — clustering instability,
  clarion-14398b2536. Promote to a guardrail when the fix lands.
- **Adoption / operator installs** — no instrumentation exists; local-first
  design makes telemetry an explicit product decision (escalate before
  adding any). `BASELINE: unknown → TARGET: TBD by owner`.
