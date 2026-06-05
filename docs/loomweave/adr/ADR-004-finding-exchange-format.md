# ADR-004: Use Filigree-Native Intake as the v0.1 Finding Exchange Format

**Status**: Accepted
**Date**: 2026-04-17
**Deciders**: qacona@gmail.com
**Context**: how Loomweave emits findings into Filigree

## Summary

Loomweave will emit findings to Filigree using Filigree's existing `POST /api/v1/scan-results` JSON schema. Extension fields will be preserved under `metadata.loomweave.*`. SARIF remains an import and translation path, not the primary interop contract with Filigree.

## Context

The design review and integration reconnaissance both found that Filigree does not ingest SARIF directly. Its production intake is a flat JSON format with specific field names, severity vocabulary, and a `metadata` extension slot.

Loomweave still needs:

- lossless enough transport for richer fields
- compatibility with Filigree as it exists today
- a permanent place for SARIF translation because Wardline and other tools may emit SARIF

## Decision

We will treat Filigree's native scan-results intake as the canonical v0.1 finding exchange format.

Specifically:

- Loomweave posts Filigree-native JSON to `/api/v1/scan-results`
- extension fields live under `metadata.loomweave.*`
- SARIF import remains a translator workflow rather than the direct Filigree contract

## Alternatives Considered

### Alternative 1: Make SARIF the direct Loomweave-to-Filigree contract

**Description**: emit SARIF directly and require Filigree to ingest or extend toward SARIF.

**Pros**:

- aligns with broader tool ecosystem
- avoids a Loomweave-specific mapping at first glance

**Cons**:

- does not match Filigree's production reality
- would require sibling-tool work before Loomweave can rely on it
- increases v0.1 coordination risk

**Why rejected**: it makes Loomweave depend on functionality Filigree does not currently have.

### Alternative 2: Invent a suite-specific "SARIF-lite"

**Description**: define a new intermediate schema for the Weft suite.

**Pros**:

- could be tailored to suite needs
- might feel conceptually cleaner than native per-tool formats

**Cons**:

- creates another protocol to maintain
- still requires adapters on both ends
- risks monolith-style centralization pressure

**Why rejected**: it adds abstraction without reducing real integration work.

## Consequences

### Positive

- Loomweave can integrate with Filigree as it exists today
- richer Loomweave metadata survives under a namespaced extension slot
- SARIF translation remains available where it is genuinely needed

### Negative

- Loomweave owns an explicit mapping layer
- some internal semantics need round-trip preservation helpers

### Neutral

- SARIF still matters, but as a translator input/output path rather than the primary Filigree wire contract

## Related Decisions

- Related to: [ADR-003](./ADR-003-entity-id-scheme.md)
- [ADR-014](./ADR-014-filigree-registry-backend.md) — `file_id` resolution in `registry_backend: loomweave` mode produces the `file_id` field this ADR's wire format references.
- [ADR-015](./ADR-015-wardline-filigree-emission.md) — the SARIF-to-Filigree-native translator maps SARIF inputs into this ADR's format; `metadata.<driver>_properties.*` namespacing is consistent with `metadata.loomweave.*`.

## References

- [Loomweave v0.1 system design](../v0.1/system-design.md)
- [Loomweave v0.1 detailed design](../v0.1/detailed-design.md)
- [Loomweave v0.1 design review](../../implementation/v0.1-reviews/pre-restructure/design-review.md)
- [Loomweave v0.1 integration recon](../../implementation/v0.1-reviews/pre-restructure/integration-recon.md)
