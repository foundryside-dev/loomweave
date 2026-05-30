# ADR-002: Plugin Transport via Content-Length Framed JSON-RPC Subprocess

**Status**: Accepted
**Date**: 2026-04-17
**Deciders**: qacona@gmail.com
**Context**: transport between the Rust core and language plugins

## Summary

Clarion plugins will run as subprocesses and speak JSON-RPC 2.0 over a Content-Length framed stream. This keeps plugin authorship language-specific while preserving resumable, binary-safe transport semantics.

## Context

The Clarion core must communicate with language plugins that:

- run out of process
- emit structured analysis results
- need reliable framing across partial reads and crashes
- should not couple the core to an embedded language runtime

The design review identified framing ambiguity as a real defect: "LSP-style JSON-RPC" was named without specifying the framing contract.

## Decision

We will use subprocess-based JSON-RPC 2.0 with explicit Content-Length framing.

## Alternatives Considered

### Alternative 1: Newline-delimited JSON

**Description**: send one JSON object per line over stdio.

**Pros**:

- simpler implementation
- easy to inspect manually

**Cons**:

- unsafe for embedded newlines or larger payload handling
- weaker resumability semantics after partial writes

**Why rejected**: framing correctness matters more than implementation simplicity for this boundary.

### Alternative 2: Embedded Python runtime

**Description**: load the Python plugin directly into the core process.

**Pros**:

- lower message-passing overhead
- simpler call model

**Cons**:

- couples the core to Python runtime management
- weakens process isolation
- conflicts with the plugin-owns-ontology boundary

**Why rejected**: it compromises the core/plugin separation that the product relies on.

### Alternative 3: Wasm plugins

**Description**: standardize on Wasm as the plugin execution boundary.

**Pros**:

- strong sandboxing story
- portable execution model

**Cons**:

- poor ergonomics for current Python-first plugin authoring
- unnecessary complexity for v0.1

**Why rejected**: too early and too disruptive for the validating Python plugin.

## Consequences

### Positive

- binary-safe framing
- clear recovery semantics after transport interruption
- language plugins stay isolated from the core process

### Negative

- explicit protocol implementation work on both sides
- more moving parts than an in-process plugin API

### Neutral

- plugin authors must follow stdout/stderr hygiene strictly because stdout is protocol-only

## Related Decisions

- Related to: [ADR-001](./ADR-001-rust-for-core.md), [ADR-003](./ADR-003-entity-id-scheme.md)
- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — the subprocess supervision loop defined here is the enforcement surface for Content-Length ceiling, per-plugin RSS cap, and crash-loop counting of Layer 2 violations.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — the validation checkpoints at `initialize` (manifest acceptance) and at `file_analyzed` notifications (emission acceptance) enforce the ontology-shape rules this ADR's RPC surface carries.

## References

- [Clarion v0.1 system design](../v0.1/system-design.md)
- [Clarion v0.1 detailed design](../v0.1/detailed-design.md)
- [Clarion v0.1 design review](../../implementation/v0.1-reviews/pre-restructure/design-review.md)
