# Loomweave — Roadmap (intent only)

> **Routing banner:** this roadmap records *intent* — which bets, in which
> horizon, and why. Sequencing, WSJF scoring, and dated forecasts are produced
> by `/axiom-program-management`, never here. No dates, no commitments.
>
> Bootstrapped 2026-06-11. **Updated: 2026-06-29 (PDR-0008)** — the warpline churn
> keying gap root-caused + fixed loomweave-side (briefing-blocked entities' SEI now
> rides the read surface; owner-ratified posture reversal; clarion-4b3061b1ac,
> ADR-034 amendment) → moved to Shipped. Prior same-day: PDR-0007 (stale
> `weft/legis-conformance` branch disposed → clarion-0715faa9d6); PDR-0006
> (federation MCP-transport reliability cycle: filigree #78 shipped, warpline #77 in
> flight). No Now horizon moved. Tracker IDs are Filigree issues.

## Now — turning over (the `loomweave-llm` extraction shipped)

The recorded Now bet (`loomweave-llm` extraction, clarion-141e9c08c8) is
**accepted and merged** (PDR-0005, PR #76 → `main` `b346328`) — moved to "Shipped
since" below. The Now horizon is open again. **The next session's DECIDE picks the
new Now**; this checkpoint records intent only, it does not choose. Candidates,
each with the metric it moves:

- **Incremental-analyze correctness cluster** — defends the north-star directly
  (graph correctness is what lets an agent prefer the graph over grep). The
  cluster shrank this drift-window: clarion-abda98c869 (parent-contains-mismatch)
  **closed** (PR #75); plus two adjacent graph-correctness bugs closed —
  clarion-48af930f2a (same-locator shadowing, PR #74) and clarion-e12d424f1d
  (incremental dead-code false-positive, PR #71). Still open: stale anchored
  edges from deleted files (clarion-feab311907, confirmed), subsystem clustering
  unstable on unchanged re-analyze (clarion-14398b2536, confirmed), wrong-language
  double syntax-error findings (clarion-a65cb18b02, confirmed). → moves
  graph-correctness defect count (north-star successor candidate).
- **Per-provider split of `llm_provider.rs`** (clarion-4328c5c757) — **now
  unblocked** by the extraction; the downstream of the bet just shipped. Carries
  the trait-contract uniformity test re-scoped out of PRD-0001. → moves
  module-cohesion / launch-parity.
- **B.4\* analyze wall-time 24× regression** on elspeth_mini (clarion-c20593d0d8,
  triage) — bears on the "graph fast enough to prefer over grep" north-star.

## In flight (this session — federation-transport reliability, PDR-0006)

- **Warpline churn-fill TRANSPORT** (PR #77, open vs `main`; branch `feat/warpline-churn-consumer`)
  — lights up the dead-by-design `entity_high_churn_list` /
  `entity_recent_change_list` surfaces by consuming Warpline's frozen churn read.
  NO-GO transport bug fixed + honesty disclosures (`churn_truncated`,
  `churn_unresolved`) added; validated live. **Still open / not yet accepted** —
  the transport PR is the remaining in-flight piece. (The keying gap that PR #77
  disclosed is now FIXED separately — see Shipped.) → moves federation enrichment
  fidelity / "federation degrades cleanly."

## In flight (other actors — not this session's work)

- **ADR-054 Rust reachability-root tags** (clarion-05fdd0490e, `building`,
  assignee `claude`) — was a *Later* item; increments 1 + 2 shipped during the
  drift-window (e1790a4, c64fa6e, + adversarial fixes + edition-2024 FFI rooting).
  The Rust analog of the shipped Python `public-surface` work. Promoted out of
  Later to reflect reality; a concurrent actor owns it.

## Shipped since 2026-06-11 (banked, no longer open bets)

- **Warpline churn keying gap — loomweave-side fix** (clarion-4b3061b1ac, PDR-0008)
  — branch `fix/briefing-blocked-sei-federation-key` → `main`. loomweave nulled
  briefing-blocked (secret-bearing) entities' SEI on its MCP read surface, so
  warpline's `reresolve-sei` couldn't backfill and churn read `0` for those files.
  Now the content-free SEI rides the blocked-entity projections (owner-ratified
  posture reversal, ADR-034 2026-06-29 amendment); secret content still withheld;
  live-proven on lacuna. Deep-pagination half split to clarion-obs-acffc4e8a1
  (still open). NB: warpline must re-run `reresolve-sei` to heal already-minted
  NULL rows (warpline-side operational follow-up).
- **filigree-mcp newline-transport fix** (clarion-a5bfcf5ef9, PDR-0006) — PR #78 →
  `main` `b5aabe8`. Repaired the broken stdio observation-write seam
  (Content-Length → newline JSON-RPC) + bounded timeout + fallback launcher. Last
  of the two Content-Length stdio clients in `loomweave-federation` — bug class
  closed.
- **`loomweave-llm` extraction** (clarion-141e9c08c8, PDR-0005) — PR #76 → `main`
  `b346328`. `loomweave-core` no longer links `reqwest`; providers live in a new
  pure-leaf crate; standing trust-surface CI gate added. Unblocks
  clarion-4328c5c757.
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

- Per-provider split of `llm_provider.rs` (clarion-4328c5c757) — **now unblocked**
  (the `loomweave-llm` extraction landed); also a Now candidate above.
- Split `analyze.rs` `run_with_options` (clarion-cb9676de57).
- Python plugin launch parity: pin the calls/references resolution envelope with
  audit tests (clarion-e9cfde2773).
- Federation-audit G-series gaps (G2 historical-locator resolve
  clarion-3c47f53e99, G10 project selector clarion-c37e1714fd, G14 canonical-JSON
  SEI oracle clarion-9d0e82513c, G16 rename-parser vectors clarion-73dff1d2d1).
  The shared byte-pinned legis↔loomweave rename-feed conformance golden — the
  concrete consumer-side realization of G16 — is now tracked as **clarion-0715faa9d6**
  (PDR-0007; revives the deleted stale `weft/legis-conformance` against the new
  `/git/rename-feed` shape; the cross-member "vector home" coordination with legis
  is owner-gated/outward-facing).
- Shared `weft.toml` key-layout proposal for the hub to bless (clarion-00abdf2fcb).
- Wardline handoff for Amendments 4–9 corpus re-vendor (prepared, not pushed;
  **escalation-gated — outward-facing**, see `current-state.md`).

## Later — coverage expansion and deferred surfaces

- Python entity-kind coverage beyond function/class/module (clarion-a0ecac062f;
  additive under ADR-027).
- Rust plugin categorisation-tag parity: visibility/entry-point/test/handler
  reachability roots (clarion-05fdd0490e) is **now in flight** (see In flight
  above — increments 1+2 shipped). Remaining in Later: public-method reachability
  roots (clarion-961a1acb2c).
- ADR-021's `plugin_limits.*` loomweave.yaml config surface (clarion-271287b54b).
- `references` envelope extension: match/let pattern paths + discriminant exprs
  (clarion-efc8715d98).
- Guidance staleness-review UI (deferred from v1.0).
- Other-language plugins (TypeScript, Java) — v2.0+ (NG-15).
