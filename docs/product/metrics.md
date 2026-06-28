# Loomweave — Metrics

> Bootstrapped 2026-06-11. **Updated 2026-06-28** (checkpoint). Baselines are
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
- `READING (2026-06-26): 0 open collision families` — holds. Supporting signal:
  the broader graph-correctness frontier is being actively defended — three
  graph-correctness bugs closed in the drift-window since the last checkpoint
  (clarion-abda98c869 parent-contains-mismatch, clarion-48af930f2a same-locator
  shadowing, clarion-e12d424f1d incremental dead-code false-positive). This is
  live evidence the candidate successor below is the right shape.
- `READING (2026-06-28): 0 open collision families` — **not re-swept this
  session** (the cycle was federation transport, not graph correctness — PDR-0006).
  Carried forward unchanged; no identity/extraction code touched.
- **OPEN QUESTION (owner, carried):** the collision-family target is met and needs
  a fresh falsifiable successor. Candidate: fabricated-edge / dropped-file /
  collision defect count on the adversarial sweep stays 0 across the 1.3.x line
  (now partially evidenced — three such bugs closed). The owner should ratify the
  real next target rather than have it invented here.

## Guardrails

1. **Trust-surface — does `loomweave-core` (plugin-host + SEI crate) link an
   outbound HTTP client (`reqwest`)?** Added 2026-06-24 for the Now bet
   (PDR-0003 / PRD-0001). The plugin-supervisor + identity crate must not also
   carry HTTP. *(Scope: `loomweave-core`-specific — `reqwest` is legitimate in
   `loomweave-federation` and `loomweave-cli`; this is not a workspace-wide ban.)*
   - `BASELINE (2026-06-24): yes` — `loomweave-core` links `reqwest` directly.
   - `READING (2026-06-26): no` — **TARGET MET.** PRD-0001 landed (PDR-0005, PR
     #76 → `main` `b346328`). `cargo tree -p loomweave-core --edges normal`
     resolves no `reqwest`; now enforced as a standing CI gate in `verify.yml`
     (fails the build if `reqwest` re-enters core's tree).
   - `TARGET: no — now MET and standing (CI-enforced).`
2. **CI floor stays green** (ADR-023): fmt, clippy `-D warnings`, build, nextest,
   doc, deny, ruff, ruff-format, mypy --strict, pytest, e2e scripts.
   - `BASELINE (2026-06-11): green, ~1450 nextest tests`
   - `READING (2026-06-24): presumed green` — five releases (1.2.0–1.3.1) cut
     since, each implying a green floor; not independently re-verified that session.
   - `READING (2026-06-26): GREEN — independently verified.` On the loomweave-llm
     merge commit `b346328` (PR #76): Rust + Rust(aarch64) + Python + e2e all
     pass; locally nextest 1948 / pytest 220 green under CI-equivalent conditions.
     **Caveat (CI blind spot):** the `wardline_taint_fact_conformance_oracle`
     reads the live `~/wardline` sibling repo and *skip-cleans* when absent — so a
     vendored-golden drift from that sibling turns the **local** floor red while
     **CI stays green** (no sibling on the runner). Filed as clarion-72e1c1a07d
     (pre-existing, not from this bet). Promote to a real guardrail if the
     CI-invisibility recurs.
   - `READING (2026-06-28): GREEN on PR #78` — the filigree transport fix merged to
     `main` `b5aabe8` with all CI checks passing (Rust + Rust aarch64 + Python + e2e
     walking-skeleton). 131 loomweave-federation tests green locally; fmt + clippy
     (-D warnings, federation/mcp/cli) + cargo doc clean. (Scoped verification — the
     federation crate + downstream; not a full-workspace nextest this session.)
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

- **Federation MCP-transport correctness** — count of stdio clients in
  `loomweave-federation` that mis-frame against their newline-JSON-RPC sibling
  servers (Content-Length instead of newline). `READING (2026-06-28): 0` — both
  clients (warpline, filigree) now newline-framed; grep over
  `crates/loomweave-federation/src/` for `write_frame|read_frame|ContentLengthCeiling`
  returns none (PDR-0006). Falsifiable re-emergence guard: stays 0 as new
  federation seams are added. Promote to a guardrail if a third seam reintroduces
  it. Caveat: `resolve_filigree_mcp_command`'s `filigree mcp-status` spawn is a
  blocking `.output()` outside the new timeout — a residual hang vector, disclosed
  in PR #78, not yet bounded.
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
