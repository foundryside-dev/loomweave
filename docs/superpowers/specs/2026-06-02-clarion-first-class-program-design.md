# Clarion → First-Class — Program Specification

**Date:** 2026-06-02
**Status:** Program-level design (governs the next several spec→plan cycles)
**Scope:** Decomposes the entire Clarion "road to first-class" effort into discrete workstreams,
names each one's dependency gate and the artifact that already covers it, and sequences them into
execution waves. This is a **program map**, not a design of any single workstream — each workstream
gets its own spec→plan cycle when its wave opens.

**Inputs / authorities:**
- `2026-06-01-clarion-roadmap-to-first-class.md` — the final-form target (both halves)
- `/home/john/wardline/docs/superpowers/specs/2026-06-02-clarion-priority-brief.md` — suite execution order
- `/home/john/wardline/docs/superpowers/specs/2026-06-01-loom-stable-entity-identity-conformance.md` — SEI standard (canonical)
- `2026-06-02-clarion-integrated-delivery-plan.md` — task-level plan for the critical-path workstreams
- `docs/clarion/adr/ADR-038-sei-token-and-signature.md` — the two locked SEI decisions

---

## 1. Purpose & the two reconciled orderings

Clarion is the **long pole** of suite "core paradise" (the one-call dossier that survives a rename),
and — uniquely — every one of its blockers is its own autonomous work, not a cross-tool negotiation.
This program exists because the outstanding work is a **program, not a single plan**: 9 workstreams
with different dependency gates, of which only two (SEI authority, HTTP linkages) are on the suite
critical path and only one (SEI authority) is gated on an external event (SEI lock).

Two orderings are in tension, and this program reconciles them rather than picking a winner:

- **The roadmap's ordering** ("two co-equal halves; lead with code-intelligence"): a Clarion that is
  a perfect SEI authority but serves a thin MCP surface is **not** first-class. The code-intelligence
  half is the foundation of standalone value.
- **The priority brief's ordering** ("suite-critical-path first"): each suite-unlocking item
  (SEI, HTTP linkages) unblocks work *three other tools have already finished their half of*. For
  suite paradise, identity + linkage dominate, and the standalone-polish items are *sequenced behind
  the critical path — not cancelled*.

**Reconciliation:** the unit of work is the **workstream**, tagged with both its roadmap-half and its
dependency gate (§2). Execution order (§4) follows the priority brief — critical path first,
autonomous code-intelligence work in parallel as capacity allows. Both views are first-class; neither
is lost. The roadmap remains the definition of "done"; the brief defines the sequence.

---

## 2. The workstream catalogue

Nine workstreams. Each is independently specifiable and gets its own spec→plan cycle. "Specifiable
now?" means its design can be locked without waiting on an external event.

### WS1 — SEI authority

- **Half:** Suite-integration. **Gate:** SEI lock (suite event — all four subsystems report + oracle encodes). **Owns:** the suite's identity.
- **Scope:** SEI minting, the deterministic fail-closed re-binding matcher, append-only lineage, the
  `sei_bindings` identity store, the HTTP wire contract (`resolve`/`resolve_sei`/`lineage`/`_capabilities`),
  the MCP surface carrying SEI, and the one-time hard-cutover backfill (coordinated with Filigree +
  Wardline). Includes passing the **SEI conformance oracle** (SEI spec §8) — a named deliverable, not
  an afterthought.
- **Covered by:** ADR-038 (decisions) + integrated delivery plan Phase 2. **Specifiable now?** The
  shape is locked (ADR-038); *implementation* waits for SEI lock. The prior-index prerequisite lives
  in WS3 and is shippable now.
- **Cross-product coordination:** the cutover is a single scheduled release across Clarion/Filigree/
  Wardline (single-owner release control — SEI spec §7.1). This is the only workstream with a
  cross-tool release dependency.

### WS2 — HTTP linkages

- **Half:** Suite-integration. **Gate:** none (P0, autonomous). **Owns:** structural linkages over HTTP.
- **Scope:** `callers`/`callees` (+ batch) on the HTTP read API with pagination + confidence-tier
  filtering; `linkages: { http: true }` capability flag. Closes the dossier's structural half (today
  linkages are MCP-only — a real build gap, not a thin read).
- **Covered by:** integrated delivery plan Phase 1 (T1.5–T1.7). **Specifiable now?** Yes — fully.
  Storage queries (`call_edges_targeting`/`call_edges_from`) already exist; this is an HTTP surface.

### WS3 — Prior-index retention + incremental analysis

- **Half:** Both (the storage primitive serves identity *and* standalone speed). **Gate:** none (P0, autonomous). **Owns:** the last-run snapshot.
- **Scope:** the `sei_prior_index` side table (`locator → body_hash + signature`, rebuilt each run,
  shape-independent — no SEI column); file-level incremental analysis (skip unchanged files); the
  load-bearing guard that incremental skipping must not falsely orphan skipped-file entities (the
  WS1 matcher's "current locator set" must include skipped entities).
- **Covered by:** integrated delivery plan Phase 1 (T1.1–T1.4) + Phase 3 (T3.1). **Specifiable now?**
  Yes — this is the explicit shape-independent groundwork that is *safe to build before SEI lock* and
  must be **sequenced first**, because it is the matcher's prerequisite and the largest single build
  item behind SEI.

### WS4 — Dossier participation

- **Half:** Suite-integration. **Gate:** WS1 + WS2 (both internal Clarion gates — no sibling wait). **Owns:** Clarion's slice of the dossier.
- **Scope:** document and pin the exact Clarion surface the dossier *assembler* (Wardline) calls:
  `resolve(locator)` → SEI, linkages over HTTP, file context, Filigree associations. Two-axis freshness
  (SEI alive/orphaned + content fresh/stale). **Clarion is not the assembler** — it contributes a slice;
  the consumer composes.
- **Covered by:** integrated delivery plan Phase 3 (T3.2). **Specifiable now?** The contract can be
  drafted now; it *closes* only when WS1 + WS2 ship.

### WS5 — MCP catalogue completion

- **Half:** Code-intelligence. **Gate:** none (autonomous). **Owns:** the consult-mode agent surface.
- **Scope:** complete the consult MCP surface as a **stateless** catalogue (the system-design §8
  cursor/session model is ratified-away as never-built): read-side inspection (`guidance_for`,
  `findings_for`, `wardline_for`), faceted search, the exploration-elimination shortcuts, and
  `emit_observation`. Ground truth corrects the roadmap's "8 of ~35" — 19 tools already ship.
- **Covered by:** **DESIGNED** — `docs/superpowers/specs/2026-06-02-clarion-ws5-mcp-catalogue-design.md`.
  **Specifiable now?** Done; ready for an implementation plan.

### WS5b — Advanced MCP queries (semantic search + reachability)

- **Half:** Code-intelligence. **Gate:** soft (extends WS5). **Owns:** the two tools split out of WS5.
- **Scope:** `search_semantic` (Part A — opt-in `EmbeddingProvider`, git-ignored vector sidecar,
  policy-governed cost) and `find_dead_code` (Part B — conservative whole-graph reachability,
  heuristic findings that fail toward "live"). Split from WS5 because they need infrastructure
  beyond a catalog query — **scheduled, not deferred.**
- **Covered by:** **DESIGNED + PLANNED** —
  `docs/superpowers/plans/2026-06-02-clarion-ws5b-advanced-queries-plan.md`. One open owner-decision
  (D-WS5b-1, embedding provider). **Specifiable now?** Done; Part B can start immediately, Part A
  after D-WS5b-1.

### WS6 — Guidance maturity

- **Half:** Code-intelligence. **Gate:** none (autonomous). **Owns:** the LLM-context-enrichment mechanism.
- **Scope:** the `clarion guidance` CLI (create/edit/show/list/promote); Wardline-derived guidance
  auto-generation tested against real Wardline output; staleness signals (`CLA-FACT-GUIDANCE-CHURN-STALE`,
  `-ORPHAN`) surfaced reviewably; the `propose_guidance → observation → promote` anti-poisoning lifecycle;
  guidance import/export.
- **Covered by:** roadmap §1.2 (listed, **not designed**) + integrated plan parallel track (MCP-P4).
  **Specifiable now?** Yes — needs its own design spec.

### WS7 — Multi-language plugin support

- **Half:** Code-intelligence. **Gate:** none (autonomous). **Owns:** the plugin contract's generality.
- **Scope:** stabilise + **publish** the plugin manifest protocol as an external spec (today implemented,
  not documented); validate GitHub-Release plugin distribution (ADR-033) with a real third-party plugin;
  export `clarion-plugin-fixture` as the conformance harness new authors run; build a second-language
  plugin (TS/Go/Rust — customer-demand-driven). North-Star: any entity, any language, *other producers*
  not a core rewrite.
- **Covered by:** roadmap §1.3 (listed, **not designed**). **Specifiable now?** Yes — but see owner-decision
  D2 (which second language).

### WS8 — Operational quality

- **Half:** Code-intelligence. **Gate:** none (autonomous). **Owns:** operator-facing robustness.
- **Scope:** extend `clarion doctor` (shipped v1.1) to DB health, plugin availability, config validation;
  validate cost-estimate accuracy against the ±50% NFR-COST-03 bound on a real elspeth run; surface
  summary-cache semantic-staleness (`stale_semantic: true`) before operators act on stale briefings.
- **Covered by:** roadmap §1.5 — **explicitly cut from the integrated delivery plan as off-critical-path;
  re-homed here.** **Specifiable now?** Yes — smallest workstream; can be folded into another wave or
  done opportunistically.

### WS9 — `legis` governance consumption

- **Half:** Suite-integration. **Gate:** `legis` exists (external). **Owns:** nothing new — thin on Clarion's side.
- **Scope:** governance attestations key on SEI (Clarion is already the authority — no new surface);
  Clarion's `lineage` endpoint is the audit spine `legis` reads; carry `declared_tier`/`wardline_groups`
  verbatim (Clarion does **not** adjudicate trust — Wardline analyses, `legis` governs). Trust-vocabulary
  convergence is a suite pass Clarion does not lead.
- **Covered by:** roadmap §2.4. **Specifiable now?** Partially — the consumption contract can be sketched,
  but it is gated on `legis` shipping and is the lowest-priority workstream.

---

## 3. Dependency graph

```
  autonomous, un-gated ───────────────────────────────────────────────────────────────┐
                                                                                        │
   WS2 HTTP linkages ───────────────────────────────────────┐                          │
                                                             │                          │
   WS3 prior-index/incremental ──┐                           │                          │
                                 │                           ▼                          │
                       (SEI lock: suite event)        WS4 dossier participation         │
                                 ▼                     (gate: WS1 + WS2 — both internal) │
                          WS1 SEI authority ──────────►      → closes CORE PARADISE      │
                                 │                                                       │
                                 ▼                                                       │
                          WS9 legis governance                                          │
                          (gate: legis exists — external)                               │
                                                                                        │
   WS5 MCP catalogue · WS6 guidance · WS7 multi-language · WS8 op-quality               │
        (ungated; parallel band — independent of WS1–WS4) ◄──────────────────────────── ┘
```

Edges: WS3 → WS1 (matcher needs prior state); WS1 → WS4 and WS2 → WS4 (dossier gated on both);
WS1 → WS9 (governance keys on SEI); SEI lock gates WS1; WS5–WS8 depend on nothing.

Critical observations:
- **WS3 is the keystone prerequisite.** It gates WS1 (matcher needs prior state) and powers WS4-adjacent
  incremental analysis. It is autonomous and must be sequenced first.
- **WS1 is the only externally-gated critical-path item** (SEI lock). Clarion's *shape* obligation for
  lock is already discharged (ADR-038); lock now waits on the other three subsystems + the oracle.
- **WS4 has only internal gates** — the dossier closes when Clarion finishes WS1 + WS2, with no sibling
  wait. The suite's dossier gate (Wardline's milestone 4) therefore reduces to "Clarion ships WS1+WS2."
- **WS5–WS8 are entirely ungated** and parallelisable against the critical path.

---

## 4. Wave sequencing (what to spec→plan next)

Per the priority brief: critical path first; autonomous code-intelligence in parallel.

| Wave | Workstreams | Rationale | Output of the wave |
|---|---|---|---|
| **Wave 0 (now)** | WS2, WS3 *(side table now; incremental-skip behaviour lands with WS1 per D3)*, + the two ADR-038 decisions (done) | P0, autonomous, un-gated. WS3's prior-index is the matcher prerequisite; WS2 closes the dossier's structural half. | HTTP linkages live; prior-index retained; **SEI can lock** |
| **Wave 1 (on lock)** | WS1 (incl. cutover + oracle) | Unblocks the SEI-client halves Filigree + Wardline have already built; the hard cutover re-keys every binding | Identity is refactor-stable suite-wide |
| **Wave 2 (internal)** | WS4 | Closes once WS1 + WS2 are in — no sibling wait | `dossier(entity)` achievable → **core paradise** |
| **Parallel band** | WS5, WS5b, WS6, WS7, WS8 | Ungated; the standalone-first-class half. Run against Waves 0–2 as capacity allows; **sequenced behind, not cancelled** (priority brief). WS5b extends WS5 (semantic search + reachability — split out but scheduled, not deferred). | Clarion is first-class *standalone*, not only as a suite citizen |
| **Later** | WS9 | Gated on `legis`; thin on Clarion's side | Governed paradise (does not gate core) |

**Spec→plan cadence.** Each workstream entering a wave gets its own spec→plan cycle. Waves 0–2 (WS1–WS4)
are already specified at design+task level (ADR-038 + integrated delivery plan) — they need execution,
not more design. **WS5 (MCP catalogue) and WS5b (advanced queries) are now designed+planned**; the
remaining un-designed parallel-band work is **WS6 (guidance), WS7 (multi-language), WS8 (op-quality)** —
WS6 is the next highest-value brainstorm→spec target.

---

## 5. Cross-cutting invariants

Every workstream inherits these (from the priority brief §4–§5, the SEI standard, and `loom.md`):

1. **Opt-in layers, never weight in the base.** `clarion analyze` + `clarion serve` (MCP) stay zero-cost;
   SEI infra, dossier, governance are switches. A solo Python user pays for nothing they didn't enable.
2. **Opacity.** SEI is opaque; only `resolve`/`resolve_sei` interpret it. Nothing parses `clarion:eid:…`.
3. **No binding keyed on a locator, on any surface** (HTTP *and* MCP carry SEI once WS1 ships). No MCP
   locator exception.
4. **Fail-closed / no false-green.** Unprovable match → mint + orphan; unknown/orphan/stale surfaced
   honestly, never silently patched.
5. **Enrich-only federation.** Clarion is not the dossier assembler, not the trust-vocabulary lead, not a
   shared store. Each cross-product surface passes the `loom.md` §5 failure test.
6. **Conformance proven, not assumed.** WS1 passes the SEI oracle; no grandfathering.
7. **Each workstream is a unit.** One purpose, well-defined interface, independently testable and
   specifiable. A workstream that can't be specced without dragging in another's internals has the wrong
   boundary.

---

## 6. Owner-decisions still open

These are not blockers for Wave 0 but should be resolved before the waves that depend on them:

- **D1 — SEI lock timing (suite event, not Clarion's to call alone).** Clarion's shape obligation is
  discharged (ADR-038). Lock waits on Filigree/`legis`/Wardline reporting + the oracle encoding the
  resolutions. *Decision needed:* when to convene lock. *Affects:* Wave 1 start.
- **D2 — Second-language plugin (WS7).** TypeScript, Go, or Rust, prioritised by customer demand
  (elspeth is Python, so the first *validating* customer does not force this). *Decision needed:* which,
  and whether WS7 ships the published protocol only (no second plugin yet) in the first pass.
- **D3 — Incremental analysis vs. SEI sequencing (WS3).** WS3's two consumers (incremental analysis,
  SEI matcher) can ship independently. *Decision needed:* whether incremental analysis ships in Wave 0
  (standalone speed win) or defers until the matcher consumes the same primitive. Recommendation: ship
  the side table in Wave 0; land incremental-skip behaviour with WS1 so the orphan-guard is co-designed.
- **D4 — Hash-granularity harmonisation (SEI spec §2 note).** Filigree's `content_hash_at_attach`
  (entity-body) vs. Wardline's taint-fact freshness (whole-file) are two freshness granularities. The SEI
  standard flags this as adjacent, out-of-scope work. *Decision needed:* whether Clarion drives a
  suite-wide reconciliation and in which wave. Recommendation: defer to a post-paradise suite pass; name
  it now so it isn't silently inherited.
- **D5 — Guidance lifecycle depth (WS6).** How much of `propose → observation → promote` and the
  staleness-review surface lands in the first WS6 pass vs. later. *Decision needed:* at WS6 spec time.
- **D-WS5b-1 — Embedding provider (WS5b Part A).** Local bundled model (no network/key, truest to
  local-first) vs. API embedding endpoint (simplest to ship, opt-in + degrade). *Decision needed:*
  before WS5b Part A task A.T2. Recommendation: ship the `EmbeddingProvider` trait + an API impl
  first, add a local-model impl behind the same trait — so the trait, not the choice, is
  load-bearing. *Affects:* WS5b Part A only; Part B (`find_dead_code`) is unaffected and can start now.

---

## 7. Definition of done (program level)

The program is complete when the roadmap's goal-state checklist is met:

- [ ] **WS1+WS2+WS3+WS4 → core paradise:** a rename/move of a function preserves every Wardline fact and
      Filigree association on it (or surfaces an honest orphan); `dossier(entity)` returns a complete,
      freshness-stamped, SEI-keyed envelope; Clarion serves linkages over HTTP.
- [ ] **WS5+WS6+WS7+WS8 → standalone first-class:** full MCP catalogue, mature guidance, a published
      plugin protocol with a second-language plugin, and operator-grade robustness — Clarion is the best
      code-intelligence engine in the suite *on its own*, not only as a citizen.
- [ ] **WS9 → governed paradise:** `legis` keys attestations on SEI and reads lineage as its audit spine;
      trust vocabulary converged suite-wide. Opt-in; invisible to a solo project.
- [ ] Every cross-product surface is demonstrably an instance of the `loom.md` §2 custody axiom and passes
      the §5 failure test.
