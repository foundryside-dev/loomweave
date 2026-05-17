# Sprint 2 — Mid-Sprint Scope Amendment

**Status**: ACCEPTED — Sprint 2 resumes under this amended scope; B.4* complete (see [B.4* exit criteria §9](./b4-calls-edges.md#9-exit-criteria-panel-revised))
**Date opened**: 2026-05-16
**Author**: John Morrissey
**Predecessor**: [`docs/superpowers/handoffs/2026-04-30-sprint-2-kickoff.md`](../../superpowers/handoffs/2026-04-30-sprint-2-kickoff.md)
**Successor**: [`docs/superpowers/handoffs/2026-05-16-sprint-2-resume.md`](../../superpowers/handoffs/2026-05-16-sprint-2-resume.md)

This memo serves three roles in one artifact:

1. A status report on Sprint 2's first half (2026-04-30 → 2026-05-05): what shipped, what's in flight, what hasn't started.
2. A scope amendment: original boxes B.1 / B.4 / B.5 are removed from the sprint and deferred to v0.2; new boxes B.4* / B.5* / B.6 / B.7 / B.8 are added for the MVP MCP surface.
3. A v0.1-plan.md resequencing record: which WPs are pulled forward, which are deferred to v0.2, with rationale.

There is no separate Sprint 2 close memo. This document is what closes the sprint's original scope and opens its amended scope. Sprint 2 ends when B.8 (elspeth scale-test) ships; that close gets its own ladder in the spirit of `sprint-1/signoffs.md` but is not pre-written here.

---

## 1. Status of Sprint 2 original scope (kickoff 2026-04-30)

The kickoff handoff named seven Tier B boxes. Two warmup-bug fixes were also in scope.

### Tier B status as of 2026-05-16

| Box | Owning WP | Status | Evidence |
|---|---|---|---|
| **B.1** — Phase 0 multi-file dispatch | WP4 | **not started** | no design doc, no impl |
| **B.2** — Python plugin emits class + module entities | WP3 | **shipped** | commits `6deab20` → `b821994`; `ontology v0.2.0` |
| **B.3** — Python plugin emits `contains` edges | WP3 | **design only** | `b3-contains-edges.md` (commit `5c510f1`); zero impl commits |
| **B.4** — `catalog.json` rendering | WP4 | **not started** | — |
| **B.5** — Per-subsystem markdown files | WP4 | **not started** | — |
| **B.6** — elspeth-slice demo (≥95% entities) | sprint demo | **not started** | — |
| **B.7** — No Filigree/Wardline changes (invariant) | — | satisfied | nothing touched |

### Carryover warmup-bug status

| Filigree issue | Status |
|---|---|
| `clarion-5e03cfdd21` — `read_applied_versions` swallows DB errors | **fixed** in PR #3 (merge `da45823`, fix `ad2936b`) |
| `clarion-ed5017139f` — `clarion install` partial `.clarion/` on failure | **still open** (P2, ready) |
| `clarion-b5b1029f5a` — `reader_pool` flaky 100ms sleep | **still open** (P2, ready) |
| `clarion-4cd11905e2` — `entities.priority` TEXT affinity wrong-order | **resolved** by ADR-024 vocab rename (priority→scope_rank with INTEGER affinity) |

### Out-of-scope work that also landed during Sprint 2

These were not in the kickoff but shipped during the same window:

- **ADR-024** — Guidance schema vocabulary rename (`priority→scope_level/scope_rank`; `critical→pinned`; `source→provenance`) + in-place migration policy.
- **ADR-025** — Minor shared standards registry; first entry MSS-1 locks `tier:*` filigree label namespace.
- **ADR-026** — Containment wire shape and edge identity (load-bearing for B.3 design).
- **ADR-027** — Ontology version semver policy (clarifies ADR-022).
- **Loom vocabulary glossary** — `docs/suite/loom.md` glossary clause + ADR-acceptance rule.
- **Skeleton audit doc** — `docs/implementation/sprint-2/...` audit pass with 5 findings (F-13 through F-17, all open in filigree).

The unplanned ADR cluster (024–027) was net positive: each was a real surface lock-in that downstream sprints need. The skeleton audit findings (F-13 through F-17) are not Sprint 2 blockers but should be triaged before WP9 (B.7 new scope, below) touches any of the affected surfaces.

---

## 2. Why we're amending instead of closing-and-kicking-off

Sprint 2's original scope was the *Tier B catalog-emitting slice*. Sprint 1's signoff ladder pointed at B.1 → B.6 as the next-milestone outline. Two things changed mid-sprint:

1. **The pyright/MCP-surface insight.** A consult-mode agent asking "what calls this function" or "what issues are about this code" delivers more value sooner than `catalog.json` or per-subsystem markdown — both of which are intermediate artifacts that other tools (the MCP server, the briefings story) consume but no human navigates directly. The MVP MCP surface lets a consult-mode agent become useful day one against the elspeth corpus; the catalog-emitting Tier B was a step toward that, not the value itself.
2. **The implementation-stack realisation.** A planning conversation on 2026-05-16 (see this memo's predecessor pointer) considered a Python-rewrite reset of the Rust workspace. The decision after looking at actual file inventories: **forward on the existing Rust + Python plugin stack** is faster than a reset because (a) the Rust plugin host is not blocking (B.2 shipped two new entity kinds with zero protocol changes), (b) the Python plugin already holds the analysis code (which is where pyright would integrate), and (c) the planning/review overhead of a reset (re-deriving 11 WPs and 25 ADRs in a different language) dominates the implementation savings under agentic-coding velocity.

The combination: the original Tier B's WP4 trajectory (catalog-rendering, per-subsystem markdown) is the wrong work to do next, but the existing stack is the right vehicle to do the right work on. That's a scope amendment, not a reset and not a sprint failure. The sprint identity shifts from *"Tier B catalog-emitting"* to *"Tier B catalog-emitting + MVP MCP surface"*.

The price of the amendment is sprint length: Sprint 2 will run longer than Sprint 1 (estimated 5–7 weeks total vs. Sprint 1's ~3 weeks). The alternative — closing Sprint 2 and naming the new work Sprint 3 — costs an extra close memo, an extra kickoff handoff, an extra label pass, and creates retro ambiguity ("was Sprint 2 successful or not?") when the honest answer is "Sprint 2 redirected mid-flight from one valuable target to a higher-value target."

Sprint 3, when it kicks off after B.8 ships, will be tighter and time-boxed in the conventional sense.

---

## 3. Amended Sprint 2 scope

### Boxes removed (deferred to v0.2)

| Original box | Where it goes |
|---|---|
| B.1 — Phase 0 multi-file dispatch | v0.2, WP4 Phase 0 |
| B.4 — `catalog.json` rendering | v0.2, WP4 catalog rendering |
| B.5 — Per-subsystem markdown | v0.2, WP4 subsystem rendering (requires WP4 Phase 3 clustering anyway) |

Rationale: each of these is a step toward briefings and clustered subsystem views, both of which v0.1 explicitly deferred. With the MVP MCP surface taking the day-one slot, these no longer earn priority over the new boxes below. The `catalog.json` artifact in particular has no consumer in the MVP scope — the MCP tools query the SQLite store directly.

### Boxes kept (unchanged)

| Box | Action |
|---|---|
| **B.2** — class + module entities | **shipped**; mark `clarion-daa9b13ce2` closed |
| **B.3** — `contains` edges | **finish implementation**; design is committed (`b3-contains-edges.md`), no impl exists; `clarion-39bc17bde8` → `in_progress` |
| **B.7** — original "No Filigree/Wardline changes" invariant | **VOIDED** — explicitly broken by new B.7 below (`entity_associations` binding requires a Filigree-side change). The original invariant was a Sprint-1-style guard; this is the right place to break it. |

### New boxes added

| New box | Owning WP | Deliverable | Anchoring ADRs |
|---|---|---|---|
| **B.4*** — `calls` edges via pyright + confidence tiers | WP3 + ADR-028 | Python plugin emits `calls` edges with `confidence` ∈ {`resolved`, `ambiguous`, `inferred`}; pyright integration; **week-2 go/no-go gate**: can pyright extract elspeth's calls in <5 min? | ADR-026, **ADR-028** |
| **B.5*** — `references` edges | WP3 + ADR-028 | Python plugin emits `references` edges; same confidence-tier discipline as B.4* | ADR-026, **ADR-028** |
| **B.6** — WP8 MCP surface (7 tools) | WP8 | New `clarion-mcp` crate exposes: `entity_at(file, line)`, `find_entity(pattern)`, `callers_of(id, confidence)`, `execution_paths_from(id, max_depth, confidence)`, `summary(id)`, `issues_for(id, include_contained)`, `neighborhood(id, confidence)` | ADR-012, **ADR-028**, **ADR-029**, **ADR-030** |
| **B.7** — WP9-A entity_associations binding | WP9-A (split from WP9) | Filigree-side `entity_associations` migration; Filigree MCP gains `add_entity_association` / `remove_entity_association` / `list_entity_associations`; Clarion MCP gains `issues_for` | **ADR-029** |
| **B.8** — elspeth scale-test (was original B.6) | original B.6 / WP11 spike | `clarion analyze` + `clarion serve` against elspeth-slice; agent can navigate via the 7 MCP tools; cost ceiling sanity-check on `summary(id)` | — |

Status drift requirements:

- `clarion-daa9b13ce2` (B.2) → **closed** (`mcp__filigree__close_issue`)
- `clarion-39bc17bde8` (B.3) → **in_progress** when impl starts
- New filigree issues created for B.4*, B.5*, B.6, B.7, B.8, each labeled `sprint:2`, `wp:N`, `adr:NNN` as appropriate.

### Three new ADRs accompanying the amendment

| ADR | What it locks |
|---|---|
| **ADR-028** — Edge confidence tiers | resolved / ambiguous / inferred; default MCP queries to `>=resolved`; lazy LLM compute at query time for inferred |
| **ADR-029** — Entity associations binding | Filigree-side `entity_associations` table; `add_entity_association` MCP tool on Filigree; `issues_for` MCP tool on Clarion; content-hash drift detection; federation §5 audit; WP9 split A/B |
| **ADR-030** — On-demand summary scope | Narrow WP6 from batched Phases 4–6 to MCP-driven `summary(id)`; 5-tuple cache key (ADR-007) unchanged; module/subsystem aggregation deferred to v0.2 |

All three are Accepted at the same time as this memo lands.

---

## 4. v0.1-plan.md resequencing

This memo is the authoritative resequence record. `v0.1-plan.md` gets a forward-pointer to this section; the WP-by-WP body is not rewritten in place (the resequencing is a delta the next agent reads here, not a rewrite of the plan that risks losing prior decisions).

### WPs pulled forward (originally downstream)

| WP | Original position | New position | Rationale |
|---|---|---|---|
| WP8 — MCP consult surface + HTTP read API | after WP7 | **into Sprint 2 (B.6)** | MCP surface IS the MVP value |
| WP9-A — entity_associations binding (split from WP9) | after WP6 | **into Sprint 2 (B.7)** | `issues_for` is one of the 7 MCP tools |

### WPs narrowed (smaller scope than original plan)

| WP | Original scope | Narrowed scope | Defers to v0.2 |
|---|---|---|---|
| WP3 — Python plugin v0.1 | Functions only at Sprint-1 close → full ontology + edges (calls, imports, decorated_by, inherits_from) + full `CLA-PY-*` rules | + class + module (B.2 ✅) + contains (B.3) + **calls with confidence** (B.4*) + **references** (B.5*) | imports, decorated_by, inherits_from, `CLA-PY-*` finding rules |
| WP6 — LLM dispatch + cache (Phases 4–6) | Batched-pipeline summarisation across leaf / module / subsystem tiers | **On-demand `summary(id)` MCP tool only**, leaf tier only (ADR-030) | Phases 4–6 batched pipeline; module/subsystem aggregation; `--prewarm-summaries` |
| WP9 — Loom integrations (Clarion-side) | Findings emission + entity binding + Wardline config ingest + suite-compat probe | **WP9-A only — `entity_associations` binding** (B.7) | WP9-B: findings emission to Filigree, Wardline config ingest, suite-compat probe, observation MCP-spawn |

### WPs deferred to v0.2 (out of Sprint 2 entirely)

| WP | Original Sprint-2 portion deferred | Rationale |
|---|---|---|
| **WP4** — Core-only pipeline | Phases 0–3 multi-file orchestration, Phase 3 clustering (ADR-006 Leiden/Louvain), Phase 7 cross-cutting `CLA-*` rules, Phase 8 entity-set diff | The MVP MCP surface queries the SQLite store directly; the pipeline orchestrator and clustering layer aren't on the critical path until briefings / subsystem rendering land |
| **WP5** — Pre-ingest secret scanner | All | Defensible to defer because Sprint 2's MVP runs on a known-safe corpus (elspeth-slice); production deployment against unknown corpora gates on this returning |
| **WP7** — Guidance system | All | The guidance composition algorithm and `CLA-FACT-GUIDANCE-*` findings have no MCP-tool consumer until briefings ship |
| **WP10** — Cross-product (Filigree-side `registry_backend`, SARIF translator) | All | Independent of MCP surface delivery; will land in v0.2 |
| **WP11** — Cost validation spike (originally scheduled) | The originally-scheduled batched-pipeline cost measurement | **Re-scoped to B.8** as the elspeth scale-test gate; measures `summary(id)` per-query cost rather than `clarion analyze` per-run cost, which is the more honest metric for an on-demand tool |

### Critical path under the amendment

```
B.3 contains-edges impl  →  B.4* calls + pyright + confidence  →  B.5* references
                                            ↓
            [week-2 go/no-go: pyright extracts elspeth calls in <5 min?]
                                            ↓
                                       B.6 WP8 MCP surface  ←  ADR-029 entity_associations
                                            ↓                       ↓
                                       B.7 WP9-A binding  ←────────┘
                                            ↓
                                       B.8 elspeth scale-test
```

Parallelisable: filigree-side migration for B.7 can start in parallel with B.4*/B.5* (it's a separate repo). The 2 warmup bugs (`clarion-ed5017139f`, `clarion-b5b1029f5a`) can land any time as warmups before each new design pass.

---

## 5. The week-2 go/no-go gate (load-bearing risk)

The MVP MCP surface's value depends on `calls` edges being extractable at elspeth scale (~425k LOC). Pyright is not a single JSON dump — three viable extraction strategies exist (pyright-as-LSP + per-symbol `callHierarchy`; AST walk + pyright as type oracle; `pycg` or similar). The choice between them has very different cost shapes.

**Gate definition** — at the end of week 2 of B.4* implementation, run the chosen pyright path against elspeth-slice (or a comparably-sized corpus) and measure call-edge extraction wall-clock time.

- **Green** (<5 min): proceed with B.4* implementation.
- **Yellow** (5–30 min): document the cost, decide whether to optimise or accept; consider parallelisation or caching.
- **Red** (>30 min): pause B.4*; re-design with the engineer panel. Options include: AST-walk-first with pyright as oracle only for ambiguous sites; switch to `pycg`; narrow the edge extraction to a subset of call patterns.

The gate's purpose is to discover scale-bound design problems in week 2, not week 5 staring down B.8.

---

## 6. Filigree state to update

Run this pass before starting implementation work:

```bash
# Close shipped:
filigree close clarion-daa9b13ce2 --reason="B.2 class + module entities shipped; ontology v0.2.0"

# Mark in-progress (design done, impl pending):
filigree update clarion-39bc17bde8 --status=in_progress

# Create new B.4*/B.5*/B.6/B.7/B.8 issues with appropriate labels (see handoff prompt for body templates).
```

Existing open Sprint-2 issues that stay open:
- `clarion-889200006a` (P3) — ADR-018 amendment trigger; pre-emptive read for B.7 (WP9-A) work.
- `clarion-ef9bd365bf`, `clarion-fb1b8fb5a0`, `clarion-fbe50aa6e1`, `clarion-523b2eebad`, `clarion-ba198ee96b` — audit findings F-13 through F-17, all P2/P3, triage before WP9 touches affected surfaces.
- `clarion-8befae708b` (P3) — B.2 follow-up: CI lint guard for `ontology_version` drift between `plugin.toml` and `server.py`; useful warmup.
- `clarion-ed5017139f`, `clarion-b5b1029f5a` (P2) — the two unfixed warmup bugs; either of these is a good rhythm-setter before B.3 impl.

---

## 7. What this memo does NOT change

- Sprint 1 lock-ins (L1–L9) — all unchanged. Walking-skeleton CI stays green.
- Loom federation axiom (`loom.md` §5) — ADR-029 explicitly audits against it; no relaxation.
- ADRs 001–027 — all Accepted, all unchanged. The three new ADRs (028/029/030) are additive.
- Sprint-1 signoff ladder format — Sprint 2's eventual close (when B.8 ships) will follow the same shape, but is not pre-written here.
- The original WP6 design (Phases 4–6 batched pipeline) — preserved as the v0.2 target architecture; ADR-030 narrows what ships in v0.1, not what the system aims at long-term.

---

## 8. References

- [v0.1-plan.md](../v0.1-plan.md) — the 11-WP plan; gets a forward-pointer to this memo
- [Sprint 1 signoffs](../sprint-1/signoffs.md) — the ladder format Sprint 2's eventual close will follow
- [Sprint 2 kickoff handoff](../../superpowers/handoffs/2026-04-30-sprint-2-kickoff.md) — the original 7-box scope
- [B.2 design](./b2-class-module-entities.md) — shipped
- [B.3 design](./b3-contains-edges.md) — implementation pending
- [ADR-028](../../clarion/adr/ADR-028-edge-confidence-tiers.md) — new
- [ADR-029](../../clarion/adr/ADR-029-entity-associations-binding.md) — new
- [ADR-030](../../clarion/adr/ADR-030-on-demand-summary-scope.md) — new
- [`loom.md` §5](../../suite/loom.md) — federation failure modes; ADR-029 audits against this
