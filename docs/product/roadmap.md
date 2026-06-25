# Loomweave — Roadmap (intent only)

> **Routing banner:** this roadmap records *intent* — which bets, in which
> horizon, and why. Sequencing, WSJF scoring, and dated forecasts are produced
> by `/axiom-program-management`, never here. No dates, no commitments.
>
> Bootstrapped 2026-06-11. **Updated: 2026-06-24 (PDR-0003, PDR-0004)** — the
> 1.1.0/gold bet shipped (now v1.3.1); the Now horizon turns over to the
> architecture-debt paydown. Tracker IDs are Filigree issues.

## Now — extract `loomweave-llm` (pay the head-of-critical-path debt)

The recorded bet. `loomweave-core` is the plugin-supervisor + SEI crate (it
forks sandboxed plugin subprocesses and mints stable entity identity) yet also
links an outbound HTTP client (`reqwest`) purely for the LLM/embedding
providers. Extracting the providers into a dedicated pure-leaf crate
(`loomweave-llm`) removes HTTP from that crate and unblocks the per-provider
split.

- Extract `loomweave-llm` from `loomweave-core` (clarion-141e9c08c8) — head of
  the tracker critical path; unblocks the per-provider split
  (clarion-4328c5c757). **Dispatched** this session: PRD-0001 (ready-for-
  planning), solution-architect-ratified boundary, implementation plan at
  `docs/plans/2026-06-24-loomweave-llm-extraction.md`. Next action: `/review-plan`
  then execute.
- Incremental-analyze correctness cluster (defends the north-star directly):
  stale anchored edges from deleted files never pruned (clarion-feab311907,
  confirmed), subsystem clustering unstable on unchanged re-analyze
  (clarion-14398b2536, confirmed), wrong-language double syntax-error findings
  (clarion-a65cb18b02, confirmed), incremental-move PARENT-CONTAINS-MISMATCH
  (clarion-abda98c869, triage).
- B.4* analyze wall-time 24× regression on elspeth_mini (clarion-c20593d0d8,
  triage) — bears on the "graph fast enough to prefer over grep" north-star.

**Metric this moves:** trust-surface (`loomweave-core` links outbound HTTP:
yes → no); critical-path length → 0 open; graph-correctness defects on the
4-corpus re-analyze sweep (see `metrics.md`).

## Shipped since 2026-06-11 (banked, no longer open bets)

- **1.1.0 GA + the 1.2/1.3 line** — PR #57; Rust plugin at gold (4 collision
  families fixed, PDR-0004). Now v1.3.1.
- **Dead-code public-surface reachability** (clarion-4ec50f3d92, done) — was a
  Later item; the no-`__all__` fallback root shipped early.
- **Doctor index-integrity repair** (PR #64) — `doctor --fix` repairs
  stale-file / parent-contains corruption.
- **Session-start auto-analyze + staleness refresh discipline** (1.3.x).
- **msgpack security bump** GHSA-6v7p-g79w-8964 (1.3.1).
- **Default write-tools-on** for the local agent loop; **public website** (`www/`).

## Next — finish launch parity and the federation-audit remainder

- Per-provider split of `llm_provider.rs` (clarion-4328c5c757) — unblocked once
  the Now bet lands.
- Split `analyze.rs` `run_with_options` (clarion-cb9676de57).
- Python plugin launch parity: pin the calls/references resolution envelope with
  audit tests (clarion-e9cfde2773).
- Federation-audit G-series gaps (G2 historical-locator resolve
  clarion-3c47f53e99, G10 project selector clarion-c37e1714fd, G14 canonical-JSON
  SEI oracle clarion-9d0e82513c, G16 rename-parser vectors clarion-73dff1d2d1).
- Shared `weft.toml` key-layout proposal for the hub to bless (clarion-00abdf2fcb).
- Wardline handoff for Amendments 4–9 corpus re-vendor (prepared, not pushed;
  **escalation-gated — outward-facing**, see `current-state.md`).

## Later — coverage expansion and deferred surfaces

- Python entity-kind coverage beyond function/class/module (clarion-a0ecac062f;
  additive under ADR-027).
- Rust plugin categorisation-tag parity so pure-Rust dead-code analysis works —
  visibility/entry-point/test/handler reachability roots, the Rust analog of the
  shipped Python `public-surface` work (clarion-05fdd0490e). Plus public-method
  reachability roots (clarion-961a1acb2c).
- ADR-021's `plugin_limits.*` loomweave.yaml config surface (clarion-271287b54b).
- `references` envelope extension: match/let pattern paths + discriminant exprs
  (clarion-efc8715d98).
- Guidance staleness-review UI (deferred from v1.0).
- Other-language plugins (TypeScript, Java) — v2.0+ (NG-15).
