# Loomweave — Metrics

> Bootstrapped 2026-06-11. **Updated 2026-06-24** (checkpoint). Baselines are
> real observed readings; targets are falsifiable (a number/boolean and a date).

## North star

**Graph trustworthiness on the reference QA sweep** — a consult agent can only
prefer the graph over grep if the graph is correct. Proxy: open
entity-identity defect families (collisions, fabricated edges, dropped files)
found by the adversarial 4-corpus QA sweep.

- `BASELINE (2026-06-11): 4 open collision families (Sprint-4 gold verdict)`
- `READING (2026-06-24): 0 open families` — **TARGET MET.** All four fixed
  (ADR-049 Amendments 6–9; PDR-0004). The original `0 by 2026-06-30` target is
  achieved ahead of its date.
- **OPEN QUESTION (owner):** the north-star target is now met and needs a fresh
  falsifiable successor. Candidate: fabricated-edge / dropped-file defect count
  on the same sweep stays 0 across the 1.3.x line — but the owner should set the
  real next target rather than have it invented here.

## Guardrails

1. **Trust-surface — does `loomweave-core` (plugin-host + SEI crate) link an
   outbound HTTP client (`reqwest`)?** Added 2026-06-24 for the Now bet
   (PDR-0003 / PRD-0001). The plugin-supervisor + identity crate must not also
   carry HTTP. *(Scope: `loomweave-core`-specific — `reqwest` is legitimate in
   `loomweave-federation` and `loomweave-cli`; this is not a workspace-wide ban.)*
   - `BASELINE (2026-06-24): yes` — `loomweave-core` links `reqwest` directly.
   - `TARGET: no` — verified by `cargo tree -p loomweave-core` resolving no
     `reqwest`; becomes met when PRD-0001 lands. **Currently: yes (bet open).**
2. **CI floor stays green** (ADR-023): fmt, clippy `-D warnings`, build, nextest,
   doc, deny, ruff, ruff-format, mypy --strict, pytest, e2e scripts.
   - `BASELINE (2026-06-11): green, ~1450 nextest tests`
   - `READING (2026-06-24): presumed green` — five releases (1.2.0–1.3.1) cut
     since, each implying a green floor; **not independently re-verified this
     session.** One security event handled: msgpack GHSA-6v7p-g79w-8964 bumped
     (1.3.1).
   - `TARGET: green on every release/merge — standing, no end date`
3. **MCP context tax under budget**: `tools/list` payload has a CI-enforced
   22,000-byte budget.
   - `BASELINE (2026-06-11): 13 bytes under budget`
   - `READING (2026-06-24): UNKNOWN — NEEDS RE-CHECK.` The MCP surface grew since
     bootstrap (entity dossier, app_only filters, caller-honesty fields, config
     tools). Margin was 13 bytes; growth may have breached or re-tightened it.
   - `TARGET: never exceeds budget; any schema growth buys bytes elsewhere first`
4. **Identity stability**: SEI churn on unchanged re-analyze of reference corpora.
   - `BASELINE (2026-06-11): 0 SEI churn (Sprint-3 sweep)`
   - `READING (2026-06-24): presumed 0` — no identity code changed; not re-swept
     this session.
   - `TARGET: stays 0 across the 1.3.x line — standing`

## Watchlist (not yet a target)

- **Subsystem-count drift** on unchanged re-analyze (clustering instability,
  clarion-14398b2536, confirmed). Promote to a guardrail when the fix lands.
- **B.4* analyze wall-time 24× regression** on elspeth_mini (3.99s → 96.99s;
  next-tier projection 3.1min → 96min), clarion-c20593d0d8 (triage). **Added
  2026-06-24.** Bears directly on the "graph fast enough to prefer over grep"
  north-star; promote to a perf guardrail once root-caused. (Note a B.4* week-2
  gate refresh read GREEN 2026-06-18 — reconcile the conflicting signals during
  triage.)
- **Adoption / operator installs** — no instrumentation exists; local-first
  design makes telemetry an explicit, escalation-gated product decision.
  `BASELINE: unknown → TARGET: TBD by owner`.
