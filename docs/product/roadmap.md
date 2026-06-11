# Loomweave — Roadmap (intent only)

> **Routing banner:** this roadmap records *intent* — which bets, in which
> horizon, and why. Sequencing, WSJF scoring, and dated forecasts are produced
> by `/axiom-program-management`, never here. No dates, no commitments.
>
> Bootstrapped 2026-06-11 from observed direction (rc4 commit history, open
> tracker items, sprint memos). Tracker IDs are Filigree issues.

## Now — ship the 1.1.0 release line (rc4) with the Rust plugin at gold

The dominant observed bet: the first-party Rust language plugin merged into
rc4 and four sprints (hardening, edges, scale-QA, gold closeout) drove it
toward "gold" — but the Sprint-4 verdict was **not gold**: four new entity-ID
collision families (self-type-path, trait-path, `#[path]`-module, `const _`)
were found and filed as the remaining gold blockers.

- Close the 4 collision families blocking the Rust-plugin gold verdict.
- Finish the in-flight orphaned-input fix: Wardline manifest-ingest reads a
  `wardline.yaml` format Wardline no longer produces (clarion-7c9336163e,
  in progress with uncommitted working-tree changes).
- Incremental-analyze correctness: stale anchored edges from deleted files
  never pruned (clarion-feab311907, major), subsystem clustering unstable on
  unchanged re-analyze (clarion-14398b2536), wrong-language double
  syntax-error findings (clarion-a65cb18b02).
- MCP/HTTP surface convergence remainder: X-6 pagination idiom + slim row
  projection (clarion-b24df21158), version the wardline HTTP group before the
  contract freeze (clarion-29b3ddcb0c), schema polish (clarion-e323e32b53).

**Metric this moves:** Rust-plugin gold blockers → 0; graph-correctness
defects on the 4-corpus QA sweep (see `metrics.md`).

## Next — pay the architecture debt and reach Python/Rust launch parity

- Extract `loomweave-llm` crate from `loomweave-core` (clarion-141e9c08c8) —
  head of the tracker's critical path; unblocks the per-provider split
  (clarion-4328c5c757). Trust-surface argument: LLM HTTP transport out of the
  plugin-supervisor crate.
- Split `analyze.rs` `run_with_options` (clarion-cb9676de57).
- Python plugin launch parity: pin the calls/references resolution envelope
  with audit tests (clarion-e9cfde2773); characterize deep-recursion behavior
  on hostile input (clarion-f3eb3852d6).
- Federation-audit G-series gaps from the 2026-06-10 weft-hub audit
  (G2 historical-locator resolve, G10 project selector, G14 canonical-JSON
  SEI oracle, G15 serde alias test, G16 rename-parser vectors, G24/G25).
- Shared `weft.toml` key-layout proposal for the hub to bless
  (clarion-00abdf2fcb).
- Wardline handoff for Amendments 4–9 corpus re-vendor (prepared, not pushed;
  escalation-gated — outward-facing).

**Metric this moves:** critical-path length → 0 open; launch-parity label
count → 0.

## Later — coverage expansion and deferred surfaces

- Python entity-kind coverage beyond function/class/module — module-level
  consts/vars, type aliases (clarion-a0ecac062f; additive under ADR-027).
- Rust plugin categorisation-tag parity so pure-Rust dead-code analysis works
  (clarion-e1899a109f).
- ADR-021's `plugin_limits.*` loomweave.yaml config surface
  (clarion-271287b54b).
- `references` envelope extension: match/let pattern paths + discriminant
  exprs (clarion-efc8715d98).
- Guidance staleness-review UI (deferred from v1.0).
- Other-language plugins (TypeScript, Java) — v2.0+ (NG-15).
