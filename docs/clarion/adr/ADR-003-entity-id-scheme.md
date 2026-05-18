# ADR-003: Entity IDs Use Symbolic Canonical Names

**Status**: Accepted
**Date**: 2026-04-17
**Deciders**: qacona@gmail.com
**Context**: stable cross-tool identity for Clarion entities

## Summary

Clarion entity IDs will use symbolic canonical qualified names rather than file-path-embedded identifiers. File paths remain properties, while rename tracking beyond the 80% case is deferred to a later `EntityAlias` mechanism.

## Context

Clarion's entity IDs are referenced by:

- the Clarion catalog itself
- Filigree issues and findings
- guidance sheets
- Wardline-derived reconciliation surfaces

Path-embedded IDs make routine moves and re-exports detach cross-tool references. The validating customer is a large Python codebase where that kind of movement is expected.

## Decision

We will identify source entities as `{plugin_id}:{kind}:{canonical_qualified_name}` and keep file path as a property rather than as part of the primary ID.

For v0.1:

- definition site wins for canonical naming
- file moves without qualified-name changes preserve identity
- manual alias repair is available where needed

For later versions:

- `EntityAlias` will handle richer rename tracking

## Alternatives Considered

### Alternative 1: Path-embedded IDs

**Description**: identify entities using file path plus local symbol name.

**Pros**:

- simple to derive
- easy for humans to read at first glance

**Cons**:

- routine file moves break identity
- re-exports and aliasing create duplicate or unstable references
- cross-tool links rot quickly

**Why rejected**: it fails the stability requirement for a catalog meant to anchor multiple tools.

### Alternative 2: Full alias tracking in v0.1

**Description**: ship `EntityAlias` rename tracking immediately.

**Pros**:

- stronger rename preservation from day one
- fewer manual repair cases

**Cons**:

- significantly more implementation complexity
- higher risk for v0.1 scope

**Why rejected**: the symbolic-ID scheme covers the most common failure mode while keeping v0.1 tractable.

## Consequences

### Positive

- file moves no longer rot most cross-tool references
- canonical naming aligns better with language-native reasoning
- identity translation stays with Clarion rather than leaking into sibling tools

### Negative

- pure symbol renames still detach references in v0.1
- manual repair remains necessary in some workflows until `EntityAlias` exists

### Neutral

- file path remains important operational data, but not part of primary identity

## Related Decisions

- Related to: [ADR-002](./ADR-002-plugin-transport-json-rpc.md), [ADR-004](./ADR-004-finding-exchange-format.md)
- [ADR-006](./ADR-006-clustering-algorithm.md) — produces `core:subsystem:{cluster_hash}` entities whose identity format this ADR defines; cluster-hash renames follow the same alias story.
- [ADR-018](./ADR-018-identity-reconciliation.md) — translates Wardline qualnames / exception locations / SARIF logical locations into the `EntityId`s this ADR defines; the v0.1 rename-without-file-move limitation named here is the specific case ADR-018's heuristic fallback covers.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — constrains what `{kind}` in `{plugin_id}:{kind}:{canonical_qualified_name}` can be; reserves `file`, `subsystem`, `guidance` as core-owned kinds with `plugin_id: core`.

## References

- [Clarion v0.1 system design](../v0.1/system-design.md)
- [Clarion v0.1 detailed design](../v0.1/detailed-design.md)
- [Clarion v0.1 design review](../../implementation/v0.1-reviews/pre-restructure/design-review.md)
