# Handoff — Phase 3 Subsystems (analyse + implement)

**Date**: 2026-05-18
**For**: an agent picking this up cold
**Predecessor context**: Sprint 2 closed GREEN; [arch-analysis 2026-05-20](../arch-analysis-2026-05-20-2124/04-final-report.md) supersedes the removed 2026-05-18 snapshot as the current RC1 code-geography reference; [Thread-1 publish-prep program](../../implementation/v0.1-publish/thread-1-pre-publish-blockers.md) runs in parallel to this and does not block.

---

## Why you were dispatched

The amended-v0.1 MCP-MVP delivers entity extraction + on-demand leaf summarisation, but the *headline capability* the v0.1 requirements promise — **subsystems as first-class entities derived from clustering** — is missing. `REQ-CATALOG-05` (subsystem entities), `REQ-ANALYZE-01`/`REQ-ANALYZE-05` (Phase 3 in the pipeline), and `ADR-006` plus `ADR-032` (Leiden / weighted-components on imports+calls) collectively specify this. Nothing in the storage schema, the analyze orchestrator, or the MCP surface ships it yet. An agent asking "what is the auth subsystem of this codebase" currently gets back individual function entities; the aggregation level above the module does not exist.

Closing this gap is the single highest-leverage move between the current MCP-MVP and the briefing's core pitch.

## What you are delivering

Two things, in order, with a human review gate between them.

1. **An implementation plan** (file: `docs/superpowers/plans/2026-XX-XX-phase3-subsystems.md`) in the existing plan-doc convention (see [`docs/superpowers/plans/2026-05-05-b2-class-module-entities.md`](../agent-plans/2026-05-05-b2-class-module-entities.md) for the canonical shape). Task-by-task, file-by-file, with exit criteria each task.
2. **The implementation itself**, executed task-by-task under TDD discipline, after the human approves the plan.

**Do not skip Phase 1.** Phase 3 clustering touches the schema, the analyze orchestrator, the MCP read surface, and (when WP6 module/subsystem aggregation lands in v0.2) the LLM pipeline. The plan is the surface where the human catches "you missed `runs.stats` serialisation" or "the `in_subsystem` edge needs the writer-actor's edge-contract validator updated" before you write the wrong code for an afternoon.

## Required sub-skills

- `superpowers:brainstorming` — REQUIRED for Phase 1. Phase 3 is not a mechanical translation; there are real design judgments (Leiden source: vendored vs crate; Phase 3 placement in the analyze lifecycle; what `in_subsystem` looks like in the existing edge ontology; what to do when clustering input is empty). Brainstorm before drafting.
- `superpowers:writing-plans` — for the plan doc itself.
- `superpowers:subagent-driven-development` or `superpowers:executing-plans` — for Phase 2 implementation.
- `superpowers:test-driven-development` — RIGID, not optional. The arch-analysis flagged `analyze::run`'s `SoftFailed` branch as the canonical cautionary tale about adding code paths without tests; the H-1 fix (`clarion-141ca7de30`) covered exactly this. Do not extend the analyze orchestrator without a test that exercises the new path.
- `superpowers:verification-before-completion` — before any "done" claim, run the full ADR-023 floor (fmt / clippy `-D warnings` / nextest / doc-D / deny / ruff / mypy / pytest) and the walking-skeleton E2E.

## Required reading (in this order)

1. **`docs/loomweave/adr/ADR-006-clustering-algorithm.md` plus ADR-032** — the authoritative spec. Read in full. Leiden on directed weighted imports+calls subgraph, seeded for determinism, with `weighted_components` as the named local fallback after ADR-032. Output is one `subsystem` entity per cluster + `in_subsystem` edges from members. Modularity reported, not enforced.
2. **`docs/loomweave/adr/ADR-022-core-plugin-ontology.md`** — `subsystem` is a core-reserved entity kind; `in_subsystem` is a core-reserved edge kind. Plugins cannot emit either. The writer-actor's edge-contract validator already knows about plugin-extensible vs core-reserved (`writer.rs:411` per arch-analysis).
3. **`docs/loomweave/adr/ADR-003-entity-id-scheme.md`** — subsystem IDs follow `core:subsystem:{cluster_hash}` per ADR-006 §Output. The hash is `sha256(sorted(member_module_ids))` truncated to 12 chars; verify the existing entity-ID validator accepts this shape (it should — `core` is a registered plugin_id per ADR-022).
4. **`docs/loomweave/1.0/requirements.md`** — REQ-CATALOG-05 (subsystem entities), REQ-ANALYZE-01 (phased pipeline), REQ-ANALYZE-05 (Phase-7 findings — relevant because `LMWV-FACT-CLUSTERING-WEAK-MODULARITY` is named in ADR-006 §Quality assessment).
5. **`docs/loomweave/1.0/system-design.md` §6** — pipeline phases. Phase 3 placement, what it reads from storage, what it writes.
6. **`docs/loomweave/1.0/detailed-design.md`** — search for `subsystem`, `cluster`, `Phase 3`. Schema shape if present, properties expected on the subsystem entity.
7. **Current code surface** — `cargo` over these files at minimum:
   - `crates/loomweave-storage/migrations/0001_initial_schema.sql` — does `entities.kind` already accept `subsystem`? Does `edges.kind` accept `in_subsystem`? ADR-031 added CHECK constraints on closed vocabularies; verify the subsystem path is not constrained-shut.
   - `crates/loomweave-storage/src/writer.rs` (around line 394–411) — `STRUCTURAL_EDGE_KINDS` and `ANCHORED_EDGE_KINDS` registers. Arch-analysis H-2 noted these duplicate the manifest's edge-kind list (now closed as `clarion-4e3cacac90`); confirm what the closure shipped.
   - `crates/loomweave-storage/src/query.rs` — what helpers exist for module-level edge enumeration?
   - `crates/loomweave-cli/src/analyze.rs` — where Phase 1 (entity ingest) and Phase 2 (graph completion if implemented) currently end. Find the seam Phase 3 plugs into. Note that the file is `#[allow(clippy::too_many_lines)]` and arch-analysis flagged it (H-1 closed); if you add to it, factor cleanly.
   - `crates/loomweave-mcp/src/lib.rs` — the 7 MCP tools. Specifically `neighborhood`, `find_entity`, `execution_paths_from` — these will become more useful with subsystems but may need shape changes. **Do not change MCP tool surfaces silently.** Surface every proposed change in the plan.
8. **`docs/implementation/sprint-2/scope-amendment-2026-05.md`** — what was explicitly deferred. WP4 Phase 3 was deferred *with this work in mind*; you are pulling it forward from v0.2 to closing-of-v0.1.
9. **`docs/implementation/arch-analysis-2026-05-20-2124/02-subsystem-catalog.md`** — current RC1 code geography. The `loomweave-storage` and `loomweave-cli` entries describe the writer-actor, query helpers, and analyze orchestrator in concrete terms; refresh line numbers against current `HEAD` before treating them as evidence.

## Scope discipline — what's IN and what's OUT

**In scope (deliver):**
- Phase 3 clustering as a new step in `analyze::run`, after structural entity/edge ingest, before run commit.
- Leiden-default clustering over the (`imports` ∪ `calls`) module-level subgraph, weighted by `reference_count`, seeded from `loomweave.yaml`, deterministic.
- Weighted-components as a config-selectable fallback (`analysis.clustering.algorithm: weighted_components`).
- Emit one `core:subsystem:{cluster_hash}` entity per cluster ≥ `min_cluster_size` (default 3). Properties per ADR-006 §Output.
- Emit `in_subsystem` edges from each member module to its subsystem entity. Edge ontology updated where required.
- New `loomweave.yaml` keys under `analysis.clustering` (algorithm / seed / resolution / min_cluster_size / edge_types / weight_by). Defaults match ADR-006.
- Persist `modularity_score` and the algorithm/seed/resolution to the subsystem entity's properties **and** to `runs.stats` for the run-level record.
- Emit `LMWV-FACT-CLUSTERING-WEAK-MODULARITY` (severity INFO) when overall modularity < 0.3, per ADR-006 §Quality assessment. (This is the only Phase-7 finding in scope here — the wider `LMWV-*` catalogue remains v0.2.)
- A new MCP tool `subsystem_members(id)` (or extension of existing `neighborhood` to recognise `core:subsystem:*` entities — design choice; raise it in the plan).
- E2E test: a multi-module Python fixture clusters into at least 2 subsystems with the expected memberships.
- Determinism test: two consecutive runs against the same fixture produce byte-identical subsystem IDs and modularity scores.
- Documentation: a one-page `docs/operator/clustering.md` covering the config knobs and how to read the resulting subsystem entities.

**Explicitly OUT of scope (do NOT do):**
- Subsystem *summarisation* (Phase 6 module/subsystem aggregation). ADR-030 defers this to v0.2; do not invoke the LLM on subsystem entities. `summary(id)` on a `core:subsystem:*` entity should return the existing leaf-summary-not-available envelope shape until v0.2.
- The full Phase-7 `LMWV-*` cross-cutting rule catalogue. Only the one weak-modularity finding above.
- Catalog artefacts (`catalog.json` + per-subsystem markdown) — REQ-ARTEFACT-01/02 are deferred and have no MVP consumer.
- Multi-language plugin support — `imports` and `calls` are Python-plugin emissions for v0.1. Cross-language subsystems are NG-15.
- Schema-breaking migrations. ADR-024 still allows edit-in-place; verify that's the right policy here (it should be — no external operator has built `.loomweave/loomweave.db` from a published Loomweave yet) and call it out explicitly in the plan.
- HTTP read API exposure. The HTTP read API is itself unimplemented; surfacing subsystems through it is v0.2.

## Phase 1 — Analysis (your first job)

Produce `docs/superpowers/plans/2026-XX-XX-phase3-subsystems.md` answering, at minimum:

### A. Current-state audit

Read the code; do not trust the ADRs as a description of the *current* implementation. Answer:

1. Does `entities.kind` currently accept `subsystem`? Does `edges.kind` accept `in_subsystem`? Cite the migration line numbers.
2. Where in `analyze::run` does structural ingest end and run-commit begin? Cite the line range.
3. Does the writer-actor have a path for emitting a subsystem entity (no plugin source)? Or do you need a new `WriterCmd` variant (`InsertCoreEntity` / `InsertSubsystem`)?
4. What module-level edge data exists post-Phase-1? (Calls are emitted at function-level per B.4*; you need to aggregate them to module-level — confirm this isn't already done.)
5. What does the existing entity-ID validator (`loomweave-core::entity_id`) say about `core:subsystem:abc123def456`? Run the fixtures.

### B. Open design decisions to surface

Each gets a recommendation + the alternatives + the reason for the recommendation:

1. **Leiden source.** Vendor (~400 LOC per ADR-006) or adopt a maintained crate. Check `crates.io` at planning time. ADR-006 explicitly leaves this open.
2. **`in_subsystem` edge placement.** Is it `structural` or `anchored` in the existing edge ontology? Does it appear in `STRUCTURAL_EDGE_KINDS`? Does the manifest need to declare it (no — core-owned per ADR-022)?
3. **`subsystem_members` MCP tool vs extending `neighborhood`.** Both work. Which keeps the tool catalogue cleaner?
4. **Empty-input handling.** When the imports+calls subgraph is empty (single-module fixture, or analysis fails Phase 1), what does Phase 3 do? Skip silently? Emit zero subsystems and a `LMWV-FACT-CLUSTERING-NO-INPUT` finding? Recommend.
5. **`runs.stats` shape.** The existing `runs.stats` is a JSON blob. Where does the modularity score sit in it? Define the JSON shape additively.
6. **Migration policy.** Does this need a new migration, or does ADR-024 edit-in-place still apply? Justify.

### C. File map

A table of every file you will create / modify, with the task that touches it. Same shape as the B.2 plan's file map.

### D. Task breakdown

5–8 tasks, each sized to ≤500 LOC + tests, each with: scope, files, tests, exit. Sequenced.

### E. Exit criteria for the whole workstream

- ADR-023 floor green.
- E2E test on the multi-module fixture green.
- Determinism test green.
- The Sprint-1 walking-skeleton E2E continues to pass (no regression on the single-file fixture).
- The MCP `summary` tool on a subsystem entity returns the policy envelope (not a budget-consuming LLM call).
- `loomweave analyze` against the existing elspeth-slice perf harness still completes within the NFR-PERF-01 envelope; if Phase 3 adds noticeable cost, the plan must include a measurement step.

### F. Risks and unknowns

Name them. The week-2 go/no-go gate concept from B.4* (`docs/implementation/sprint-2/scope-amendment-2026-05.md` §5) is worth borrowing — define a measurable "Leiden over elspeth-slice's module subgraph runs in < N seconds" gate halfway through implementation. If it fails, the weighted-components fallback is the documented response.

## Review gate

**Stop after Phase 1.** Write the plan, commit it, and surface it to the human. Do not start implementation until the human approves the plan in writing (either a Filigree comment or a direct message). The plan-review skill (`axiom-planning:plan-review`) is available if the human wants a structured second-opinion pass before approving.

## Phase 2 — Implementation

Execute the plan task-by-task under TDD. For each task:

1. Write the failing test first.
2. Make it pass with the smallest change that compiles and clippies cleanly.
3. Run the ADR-023 floor locally before declaring the task complete.
4. Mark the task complete in the plan file (`- [x]`) and commit; one commit per task with a message that names the task and cites the relevant ADR/REQ ID.

**Do not** batch multiple tasks into a single commit. The plan's checkbox shape exists so a reviewer can read the commit log against the plan and verify task-by-task. Sprint 2's B.4* + B.6 commit ranges followed this discipline; mimic them.

**Do not** silently change MCP tool surfaces. If during implementation you discover the `neighborhood` tool needs a shape tweak you didn't predict, stop and surface it — update the plan, commit the plan update, then resume.

## Filigree workflow

Before starting Phase 1:

```bash
filigree create --type=work_package --title="WP4 Phase 3 — Subsystem clustering (REQ-CATALOG-05, ADR-006)" \
  --labels="release:v0.1,sprint:3,wp:4,adr:006,tier:a" --priority=P1
# Capture the new issue ID; this is the umbrella.

filigree start-work <umbrella-id> --assignee <your-name>
```

Then, when the plan is committed and approved, create per-task issues blocked-by the umbrella so an outside reader of the Filigree dashboard can see exactly what's being worked.

When Phase 2 finishes:

```bash
filigree close <umbrella-id> --reason="Phase 3 clustering shipped; subsystem entities live; ADR-006 satisfied"
```

## Authorities and overrides

- **ADR-006 is authoritative on algorithm.** If your implementation reveals the ADR is wrong (e.g., directed-modularity Leiden produces nonsense on some pattern of the elspeth graph), do NOT silently switch. Stop, document the empirical finding, and propose an ADR amendment. ADRs are immutable once Accepted (`CLAUDE.md` editorial conventions); the right response is a new ADR that supersedes, not a silent code-level change.
- **The Weft federation axiom (`docs/suite/weft.md` §5)** is load-bearing. Phase 3 is internal to Loomweave and does not touch sibling products, so this should not bite. If you find yourself proposing a cross-product change, you've gone out of scope.
- **The tooling baseline (ADR-023)** is non-negotiable per PR. If a clippy lint blocks you, fix the lint; do not `#[allow]` it without writing the justification into the code and the plan.

## Done condition

This handoff is satisfied when:

- The plan exists at `docs/superpowers/plans/2026-XX-XX-phase3-subsystems.md`, approved by the human.
- All plan tasks are checked off and committed.
- The Filigree umbrella is `closed` with a close reason that names the merge commits.
- The walking-skeleton E2E + a new subsystems E2E + the determinism test are all green in CI.
- A subsequent `loomweave analyze && loomweave serve` on a multi-module fixture lets a consult-mode agent ask "what are the subsystems of this project" and get back a non-empty, sensibly-named list of subsystem entities.

The last bullet is the actual capability the work delivers. If it works in CI but a real agent session against a real fixture doesn't surface meaningful subsystems, you haven't shipped the feature.

## References

- [ADR-006 — Clustering algorithm](../../loomweave/adr/ADR-006-clustering-algorithm.md)
- [ADR-022 — Core/plugin ontology](../../loomweave/adr/ADR-022-core-plugin-ontology.md)
- [ADR-003 — Entity-ID scheme](../../loomweave/adr/ADR-003-entity-id-scheme.md)
- [ADR-024 — Migration edit-in-place policy](../../loomweave/adr/ADR-024-guidance-schema-vocabulary.md)
- [ADR-031 — Schema validation policy (CHECK constraints)](../../loomweave/adr/ADR-031-schema-validation-policy.md)
- [REQ-CATALOG-05, REQ-ANALYZE-01, REQ-ANALYZE-05](../../loomweave/v0.1/requirements.md)
- [v0.1-plan.md WP4](../../implementation/v0.1-plan.md#wp4--core-only-pipeline-phases-03-7-8)
- [Sprint-2 scope amendment — explicit Phase 3 deferral with retirement path](../../implementation/sprint-2/scope-amendment-2026-05.md)
- [Arch-analysis 2026-05-20 — current RC1 code geography](../arch-analysis-2026-05-20-2124/04-final-report.md)
- [B.2 plan-doc shape — canonical example](../agent-plans/2026-05-05-b2-class-module-entities.md)
- [Sprint-2 B.4* — the week-2 go/no-go gate pattern to mimic](../../implementation/sprint-2/scope-amendment-2026-05.md#5-the-week-2-gono-go-gate-load-bearing-risk)
