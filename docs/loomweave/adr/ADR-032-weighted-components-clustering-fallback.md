# ADR-032: Weighted-Components Clustering Fallback Naming

**Status**: Accepted
**Date**: 2026-05-18
**Deciders**: qacona@gmail.com
**Context**: Phase 3 landed with a deterministic fallback implementation that groups modules by connected components over high-weight edges, but ADR-006 and the public config name called it Louvain.

## Summary

Loomweave v0.1 keeps Leiden as the default Phase 3 clustering algorithm. The
fallback algorithm is named `weighted_components`, not `louvain`, because the
implementation does not perform Louvain modularity optimisation. The config
surface is:

```yaml
analysis:
  clustering:
    algorithm: weighted_components
```

Subsystem `properties_json.algorithm`, MCP subsystem envelopes, and
`runs.stats.clustering.algorithm` record `weighted_components` when this
fallback supplies the partition. `runs.stats.clustering.configured_algorithm`
records the requested config value for auditability.

When the configured algorithm is `leiden`, the fallback trigger is explicit:
Loomweave computes `weighted_components` if Leiden returns zero or one community,
and uses the fallback only when it produces more communities than Leiden.

## Decision

Rename the local fallback from Louvain to weighted-components before v0.1
publishes stored subsystem rows. The fallback:

- treats the module dependency graph as an undirected graph for local grouping;
- computes the average positive edge weight;
- keeps edges whose weight is at least that threshold;
- emits connected components whose size is at least `min_cluster_size`.

This is a deterministic, explainable local cut. It is not Louvain, and must not
be reported as Louvain in config, stored properties, stats, or MCP output.

## Consequences

- `analysis.clustering.algorithm` supports `leiden` and `weighted_components`.
- A run configured as `leiden` can persist `weighted_components` in subsystem
  properties and run stats when the auto-fallback trigger fires.
- Existing pre-v0.1 artifacts that recorded `louvain` should be regenerated or
  treated as pre-publish test data.
- A future real Louvain implementation remains allowed, but must land as a new
  algorithm value with tests and ADR coverage.
- ADR-006 remains authoritative for the Leiden default, module graph, output
  shape, and weak-modularity reporting. This ADR amends only the fallback
  implementation name and behavior.
