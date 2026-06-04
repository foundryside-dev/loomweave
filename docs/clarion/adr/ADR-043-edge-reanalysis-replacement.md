# ADR-043: Edge Reanalysis Replacement

**Status**: Accepted
**Date**: 2026-06-04
**Amends**: [ADR-026](./ADR-026-containment-wire-and-edge-identity.md)

## Context

ADR-026 made `(kind, from_id, to_id)` the natural edge identity and used
duplicate handling to make re-analysis idempotent. Later scan-time edges carry
source-file anchors, byte ranges, confidence tiers, and properties. With
`INSERT OR IGNORE`, a re-analysis cannot update that metadata when an edge
triple survives but moves in source, and it cannot retract an anchored edge
triple that disappeared from the file.

That made the durable graph additive across changed source files. MCP graph
traversal, HTTP linkages, coupling analysis, circular-import detection, and
federation consumers could then read stale topology as current truth.

## Decision

Keep ADR-026's natural key: `(kind, from_id, to_id)` remains the edge row
identity, and the schema does not gain an edge id.

Change scan-time write semantics:

1. Before inserting the current edge set for an analyzed source file, the writer
   deletes existing AST-anchored scan-time edges for that `source_file_id`.
2. Structural edges such as `contains`, `in_subsystem`, `guides`, and
   `emits_finding` are not deleted by this replacement step; they remain
   governed by their own invariants and producers.
3. `InsertEdge` upserts on `(kind, from_id, to_id)`, refreshing `properties`,
   `source_file_id`, `source_byte_start`, `source_byte_end`, and `confidence`
   when the same triple is re-observed.
4. Duplicate triples are accepted refreshes, not dropped-edge events.
   `dropped_edges_total` counts writer rejections, not idempotent re-observed
   triples.
5. Query-time inferred-edge materialization keeps its existing cache-scoped
   replacement behavior; this ADR covers scan-time anchored edges.

## Consequences

Re-analysis now treats each changed source file's anchored edge set as
authoritative. Removed calls/imports/references/decorations/inheritance edges
from that file are retracted, and surviving triples refresh source metadata.

Incremental analysis still skips unchanged files deliberately. A full
`--no-incremental` analyze remains the operator escape hatch for a clean graph
refresh across every file.

The natural edge key remains stable for consumers; only write semantics change.
Consumers that observed `dropped_edges_total` as a duplicate counter must stop
doing so.

## Verification

Storage writer regressions cover both required properties:

- duplicate anchored triples update range/properties/confidence metadata;
- source-file replacement removes stale anchored edges while preserving
  structural edges and anchored edges from other source files.
