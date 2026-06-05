# Clustering Operator Notes

Loomweave Phase 3 runs after plugin entity and edge extraction. It reads the
persisted module dependency graph, clusters modules, and writes subsystem
entities plus `in_subsystem` edges back into `.loomweave/loomweave.db`.

## Configuration

`loomweave analyze` snapshots the resolved config into `runs.config`.

```yaml
analysis:
  clustering:
    enabled: true
    algorithm: leiden
    seed: 42
    resolution: 1.0
    max_iterations: 100
    min_cluster_size: 3
    edge_types: [imports, calls]
    weight_by: reference_count
    weak_modularity_threshold: 0.3
```

Supported algorithms are `leiden` and `weighted_components`. The
`weighted_components` fallback builds connected components over edges whose
weight is at least the graph's average positive edge weight; it is deterministic
and does not perform Louvain modularity optimisation. `edge_types` may include
`imports`, `calls`, or both. `weight_by` is currently `reference_count`.

When `algorithm: leiden` produces zero or one community, Loomweave computes the
local `weighted_components` fallback and uses it only if it produces more
communities than Leiden. Stored subsystem properties and
`runs.stats.clustering.algorithm` record the algorithm actually used, while
`runs.stats.clustering.configured_algorithm` keeps the requested config value.
So a run configured as `leiden` can report `weighted_components` when this
fallback fires.

## Stored Subsystems

Each emitted subsystem is an entity:

- `id`: `core:subsystem:{cluster_hash}`
- `plugin_id`: `core`
- `kind`: `subsystem`
- `properties`: algorithm, seed, resolution, max iterations, modularity score,
  cluster hash, member module IDs, member count, edge types, and weight mode

Every member module has an `in_subsystem` edge pointing to the subsystem:

```sql
SELECT from_id AS module_id, to_id AS subsystem_id
FROM edges
WHERE kind = 'in_subsystem';
```

## MCP Access

Use `subsystem_members` to inspect the modules assigned to a subsystem:

```json
{
  "name": "subsystem_members",
  "arguments": {
    "id": "core:subsystem:abc123def456"
  }
}
```

The response includes subsystem properties and ordered member modules. Calling
`summary` on a subsystem returns `available=false` with reason
`summary-scope-deferred`; subsystem summarization is deferred to v0.2 and does
not call the LLM provider in v0.1.

## Weak Modularity

Loomweave emits a fact finding with rule
`LMWV-FACT-CLUSTERING-WEAK-MODULARITY` when clustering succeeds but the
modularity score is below `analysis.clustering.weak_modularity_threshold`
(default `0.3`; set `0.0` to disable the finding). This means the graph did not
separate cleanly into strong communities. Treat it as operator guidance, not a
defect: inspect the subsystem membership, then decide whether the project needs
different config, graph pruning, or an ADR amendment.

## Empty Inputs

If no module dependency edges exist, Loomweave emits no subsystems and records:

```json
{
  "clustering": {
    "status": "skipped",
    "skipped_reason": "no_module_dependency_edges",
    "subsystem_count": 0
  }
}
```

Single-module or too-small clusters similarly produce no subsystem rows when
they do not satisfy `min_cluster_size`; check `runs.stats.clustering` for the
exact skip reason.
