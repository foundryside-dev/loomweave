# ADR-006: Clustering Algorithm — Leiden on Imports+Calls Subgraph with Louvain Fallback

**Status**: Accepted; amended by [ADR-032](./ADR-032-weighted-components-clustering-fallback.md)
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: Phase 3 subsystem discovery takes module-level structural edges and produces `subsystem` entities; algorithm choice shapes downstream LLM cost and query quality

## Summary

Phase 3 clustering runs **Leiden** over a directed, weighted subgraph of module-level `imports` + `calls` edges, with edge weights equal to reference counts. The algorithm is seeded (for determinism), filters out clusters smaller than `min_cluster_size` (default 3), and records `modularity_score` on each resulting `subsystem` entity. **Louvain** is a configured fallback — selectable via `clarion.yaml:analysis.clustering.algorithm: louvain`. The fallback exists because Leiden implementations in Rust are less common than Louvain; if the vendored/chosen implementation proves unstable at implementation time, Louvain is the deterministic cutover. Both algorithms produce subsystem entities under the core-reserved `subsystem` kind (ADR-022), identified as `core:subsystem:{cluster_hash}` (ADR-003). No hard modularity pass/fail threshold ships in v0.1 — the score is reported, not enforced.

## Context

Phase 3 is the clustering step in `clarion analyze` (system-design §6, `Phase3: Clustering (core, no LLM)`). It reads the module-level `imports` and `calls` edges emitted by plugins in Phase 1 and produces candidate `subsystem` entities that Phase 6 then passes to Opus for synthesised subsystem briefings. Two facts make algorithm choice load-bearing:

1. **Downstream cost.** Phase 6 makes one Opus call per subsystem (system-design `Writing Cadence` + detailed-design §4 profile presets). Clustering quality drives whether Phase 6 fires an Opus call per *meaningful* subsystem or wastes Opus budget on noise clusters. At elspeth scale (~1,100 modules, ~100k–200k entities), a 20-subsystem output versus a 60-subsystem output is a 3× Opus-cost delta.
2. **Query quality.** MCP tools (`enter_subsystem`, `find_similar`) and Phase-7 findings (`CLA-FACT-TIER-SUBSYSTEM-MIXING`) all depend on "this module belongs to subsystem X" being coherent. A cluster whose members aren't actually connected is worse than no cluster — queries return misleading results.

The design-review noted "Leiden on imports+calls" as the intended direction but left the algorithm's edge definition, weighting, and fallback story unspecified. The scope-commitment memo promoted this to P0 because Phase 6's Opus spend depends directly on it.

Leiden (Traag, Waltman, van Eck, 2019) is a refinement of Louvain that fixes a specific Louvain defect: Louvain can produce *disconnected* communities — groups whose members aren't actually connected in the graph. For a "subsystem membership" semantic, disconnected communities are a bug, not an imperfection. Leiden's refinement step breaks disconnected groups into connected sub-communities.

The trade-off: Leiden is less widely implemented than Louvain in the Rust ecosystem. `petgraph` ships neither directly (`detailed-design.md:1644`). v0.1 vendors a minimal Leiden implementation (~400 lines) or adopts a maintained crate if a suitable one exists at implementation time. Louvain implementations are more readily available. The fallback exists specifically to absorb implementation-side risk.

## Decision

### Input subgraph

- **Nodes**: `module` entities (and `package` entities at higher aggregation; v0.1 clusters at the module level only).
- **Edges**: union of `imports` and `calls`. `imports` is naturally module-level; `calls` is function-level and is aggregated to module-level by `parent_id` chain during subgraph construction.
- **Direction**: directed. `imports(A, B)` and `imports(B, A)` are distinct edges. Leiden is configured for directed graphs (directed modularity).
- **Weight**: `reference_count` — the number of source-level import statements or call sites between two modules. Rationale: a single import is not the same signal as 47 calls; weighted modularity is the standard treatment.
- **Filter**: self-loops removed; `python:unresolved:*` placeholder entities excluded.
- **Source of truth**: configurable via `clarion.yaml:analysis.clustering.edge_types` (default `[imports, calls]`) and `weight_by` (default `reference_count`).

### Algorithm: Leiden (default)

- **Implementation**: either vendored (~400 LoC) or a maintained crate resolved at implementation time. The design commits to the algorithm; the implementation source is a code-level detail recorded in the Cargo.lock at release.
- **Deterministic**: RNG seeded from `clarion.yaml:analysis.clustering.seed` (default `42`). Seed recorded in `runs/<run_id>/stats.json`.
- **Resolution parameter**: `γ = 1.0` default (standard modularity). Configurable via `clarion.yaml:analysis.clustering.resolution` if operators want finer or coarser communities.
- **Iteration cap**: 100 passes (configurable). Most runs converge in <10.

### Fallback: Louvain

- **Trigger**: `clarion.yaml:analysis.clustering.algorithm: louvain` explicit selection, *or* Leiden implementation-side failure detected at implementation time (this is an implementation-decision trigger, not a runtime fallback — if Leiden is broken at release, Louvain becomes the default for that release with an ADR revision).
- **Implementation**: `petgraph`-based; Louvain is simpler and the crate ecosystem offers stable options.
- **Behaviour delta**: Louvain may produce disconnected communities. Phase 3 post-processes Louvain output to split disconnected clusters by weakly-connected components, mitigating the defect at the cost of occasional fragmentation.

### Output

Each cluster above `min_cluster_size` (default 3) becomes a `subsystem` entity (ADR-022 core-reserved kind):

- `id`: `core:subsystem:{cluster_hash}` where `cluster_hash = sha256(sorted(member_module_ids))` truncated to 12 chars (ADR-003).
- `properties`:
  - `cluster_algorithm`: `"leiden"` or `"louvain"`
  - `modularity_score`: floating-point; reported, not enforced
  - `member_count`: integer
  - `synthesised_at`: timestamp (Phase-6 LLM-synthesised name/description; absent until Phase 6 runs)
  - `resolution`: γ parameter used
  - `seed`: RNG seed used
- `in_subsystem` edges (core-reserved, ADR-022) from each member module to the subsystem.

### Quality assessment

No hard modularity threshold passes/fails in v0.1. Modularity scores below 0.3 are conventionally "weak" clustering; Phase 3 emits `CLA-FACT-CLUSTERING-WEAK-MODULARITY` (severity INFO) when the overall score falls below that. Operators see the signal but Phase 3 does not refuse to emit clusters. Block C1's cost-model spike will validate whether weak-modularity subsystems produce useful Opus output or wasted cost; a v0.2 decision may add a hard threshold with `--refuse-weak-modularity` semantics.

## Alternatives Considered

### Alternative 1: Louvain as default

Skip Leiden; use Louvain as the v0.1 algorithm.

**Pros**: more widely implemented in Rust; simpler to reason about; faster per-iteration.

**Cons**: Louvain's disconnected-community defect is a semantic bug for subsystem membership. The post-processing split-by-connected-components mitigates but does not eliminate the problem — the resulting "subsystem A-1" and "subsystem A-2" are semantically two subsystems the algorithm failed to distinguish. Leiden produces the right answer in one step.

**Why rejected**: quality delta matters for Phase 6 Opus spend and query coherence; Leiden is the right answer and the implementation risk is absorbable via the Louvain fallback.

### Alternative 2: Graph-neural community detection

Use a GCN or similar learned community-detection model.

**Pros**: handles heterogeneous edge types natively; can learn codebase-specific patterns.

**Cons**: requires a trained model, GPU, inference cost, and an ML dependency in the core. Overkill for v0.1's local-first, single-binary posture (ADR-001, CON-LOCAL-01). Deterministic reproducibility is hard.

**Why rejected**: cost-disproportionate to the decision quality gain.

### Alternative 3: Hierarchical agglomerative clustering

Build a dendrogram; cut at an operator-chosen height.

**Pros**: produces multiple resolutions; dendrograms are human-interpretable.

**Cons**: O(n²) to O(n³) in naive implementations; at 1,100 modules this is borderline, at 10,000+ modules it's prohibitive. No natural "subsystem" boundary — operators must pick a cut height, introducing a tuning parameter that doesn't exist in modularity-based methods.

**Why rejected**: scales poorly; adds operator-tuning surface without improving quality.

### Alternative 4: Connected-components only (no modularity)

Take weakly-connected components of the imports+calls graph as subsystems.

**Pros**: trivial algorithm; fully deterministic.

**Cons**: most Python codebases are one giant component via shared library imports (or via a logger module, or a config module). Connected-components produces "one subsystem" outputs that are useless.

**Why rejected**: too coarse for the "discover meaningful subsystems" goal.

### Alternative 5: Manifest-declared subsystems

Operators declare subsystems in `clarion.yaml`; clustering is skipped.

**Pros**: deterministic, operator-controlled.

**Cons**: undermines REQ-CATALOG-06 (discover structure without manual configuration). Adds cognitive load; the whole point of Phase 3 is that operators shouldn't have to know their subsystems in advance.

**Why rejected**: categorical mismatch with the product goal.

## Consequences

### Positive

- Leiden's connected-community guarantee makes `CLA-FACT-TIER-SUBSYSTEM-MIXING` and subsystem-navigation MCP tools semantically sound — a "subsystem" is always a coherent group.
- Phase 6's per-subsystem Opus call lands against meaningful clusters, not noise. Cost spend tracks semantically-relevant subsystems.
- Determinism (seeded RNG) makes `--resume` and cross-run diffing of subsystem structure possible. Operators see "this module left subsystem X" as a real signal, not RNG noise.
- Louvain fallback absorbs implementation risk without redesigning the decision.

### Negative

- Leiden implementations in Rust are less mature than Louvain's. Vendoring ~400 LoC or depending on a possibly-young crate is real implementation risk. Mitigation: the fallback exists specifically for this.
- Modularity is a quality *signal* not a hard threshold. Weak clusterings ship. Operators reading `CLA-FACT-CLUSTERING-WEAK-MODULARITY` may not know how to act on it. Mitigation: v0.2 validation work (and the C1 cost-model spike) clarifies whether a hard threshold is warranted.
- Module-level only. Classes, functions, and decorators don't get their own clusters in v0.1. Operators wanting finer grains (sub-module clusters) get the "module is the unit" limitation — noted in release documentation.

### Neutral

- Algorithm selection is a config, not a code change. Switching from Leiden to Louvain mid-project (if a critical implementation bug emerges) is a `clarion.yaml` edit + reindex.
- Resolution parameter (`γ`) is exposed for operators with unusual codebases; most never touch it.
- Subsystem entity IDs are content-addressed over sorted member IDs; renaming a module changes the hash. This is correct — "the subsystem" changed composition, so a new identity is right. Filigree issues tagged to the old ID follow the ADR-003 EntityAlias story.

## Related Decisions

- [ADR-003](./ADR-003-entity-id-scheme.md) — `core:subsystem:{cluster_hash}` identity; this ADR produces the entities ADR-003 names.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — `subsystem` is a core-reserved entity kind; clustering is the core-owned algorithm that produces them. This ADR is the concrete case ADR-022's "core owns algorithms" principle covers.
- [ADR-007](./ADR-007-summary-cache-key.md) — the summary cache key's `guidance_fingerprint` interacts with clustering at Phase 6; changes in cluster composition indirectly invalidate cached subsystem briefings through the content-hash component.
- [ADR-011](./ADR-011-writer-actor-concurrency.md) — subsystem entity emission at Phase 3 lands through the writer-actor's per-N-files transactions; cluster discovery is an in-memory operation (petgraph projection), commit is standard.

## References

- [Traag, Waltman, van Eck 2019 — "From Louvain to Leiden: guaranteeing well-connected communities"](https://www.nature.com/articles/s41598-019-41695-z) — the paper defining Leiden's refinement step and the disconnected-community defect in Louvain.
- [Clarion v0.1 system design §2, §6 (Phase 3 Clustering)](../v0.1/system-design.md) — Phase 3 position in the pipeline; core-ownership of clustering.
- [Clarion v0.1 detailed design §4 (clarion.yaml clustering config)](../v0.1/detailed-design.md) (lines 937-941) — the config surface this ADR commits to.
- [Clarion v0.1 detailed design Appendix — petgraph dependency note](../v0.1/detailed-design.md) (line 1644) — implementation-level note on vendoring.
- [Clarion v0.1 requirements — REQ-CATALOG-06](../v0.1/requirements.md) — structural discovery requirement this ADR serves.
