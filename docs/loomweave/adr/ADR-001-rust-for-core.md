# ADR-001: Rust for the Core

**Status**: Accepted
**Date**: 2026-04-17
**Deciders**: qacona@gmail.com
**Context**: Loomweave v0.1 core implementation language

## Summary

Loomweave's core will be implemented in Rust. The choice is anchored to requirements the design already assumes (single-binary distribution, long-lived SQLite workload, robust subprocess supervision) rather than to author preference alone.

## Context

Loomweave's core is responsible for:

- single-binary distribution across developer machines (`NFR-OPS-04`: operator installs via `pipx` or a released binary with no runtime install)
- SQLite-backed storage that serves both batch `loomweave analyze` and long-lived `loomweave serve` simultaneously (`CON-SQLITE-01`, ADR-011 writer-actor)
- subprocess supervision for language plugins over Content-Length framed JSON-RPC (`REQ-PLUGIN-01`..`REQ-PLUGIN-06`, ADR-002)
- bounded-cost LLM orchestration with per-run cache behaviour and cancellation semantics (`NFR-COST-01`..`NFR-COST-03`)
- local HTTP and MCP serving with a bounded trust posture (`NFR-SEC-*`, ADR-012 historically; ADR-014 for the registry-backend HTTP read API)

The product posture is local-first and operationally lightweight (`CON-LOCAL-01`, `NFR-OPS-01`..`NFR-OPS-03`). The implementation language therefore affects packaging, concurrency, storage ergonomics, and deployment complexity — and the acceptable envelope for each is already set by requirements rather than preference.

## Decision

We will implement the Loomweave core in Rust.

## Alternatives Considered

### Alternative 1: Go

**Description**: use Go for the core binary and service surfaces.

**Pros**:

- Static single-binary distribution is a first-class Go capability (satisfies `NFR-OPS-04`).
- Approachable contributor pool; likely larger than Rust's for infrastructure work.
- Mature HTTP and gRPC tooling; good subprocess supervision via `os/exec`.
- Runtime concurrency model (goroutines + channels) maps cleanly to "writer actor + reader pool" needs.

**Cons**:

- **SQLite ergonomics**: Go's canonical `database/sql` interface plus `mattn/go-sqlite3` works, but the common pattern around long-lived connections, per-statement caching, and the writer-actor-plus-reader-pool design assumed by ADR-011 is more idiomatic in `rusqlite` (direct control of connection-per-task, transaction lifetimes, and pragma tuning without `database/sql`'s abstractions getting in the way).
- **Subprocess framing**: Content-Length framed JSON-RPC (ADR-002) is not a standard library primitive in Go; Rust's `tokio::io::AsyncBufRead` + `serde_json` makes framing and streaming framed reads lower-ceremony than Go's hand-rolled scanner pattern.
- **LLM-orchestration backpressure**: per-run cost caps and prompt-cache segment management (`NFR-COST-*`) benefit from `tokio`'s structured concurrency and cancellation semantics; Go's context-cancellation model requires more manual discipline at each call site.
- **Garbage-collection pauses**: not disqualifying at Loomweave's scale, but Rust's predictable allocation profile simplifies reasoning about `loomweave serve` tail latency under MCP load.

**Why rejected**: Go would be a defensible choice — the first two cons are ergonomic, not blocking. The decision is driven by the concrete fit between `rusqlite` + `tokio` and the storage/concurrency/subprocess-framing workload this design actually specifies, not a judgment that Go could not work. The author directive aligns with the technical fit; it is not the sole basis for the decision.

### Alternative 2: Python or TypeScript

**Description**: implement the core in a higher-level runtime language.

**Pros**:

- Faster initial prototyping.
- Lower barrier for contributors.
- Python specifically would eliminate the core/plugin subprocess boundary (plugins could run in-process).

**Cons**:

- **Runtime dependency**: shipping requires either a bundled interpreter (contradicts `NFR-OPS-04` single-binary) or a user-installed interpreter (contradicts `CON-LOCAL-01` "works on developer's machine without system prep").
- **Long-lived service profile**: `loomweave serve` needs stable memory and latency characteristics under MCP load; GC-driven runtimes add noise to tail latency that is awkward to tune.
- **Plugin supervision**: in-process Python plugins remove the subprocess boundary that ADR-002 uses as the Rust/Python compatibility seam — a plugin crash would take the core with it.

**Why rejected**: Python/TypeScript reintroduce runtime-management overhead the product is explicitly trying to avoid (`NFR-OPS-*`, `CON-LOCAL-01`). In-process Python would also foreclose the language-agnostic plugin model ADR-002 establishes.

## Consequences

### Positive

- straightforward single-binary distribution
- mature ecosystem for HTTP, async orchestration, and SQLite
- clean subprocess boundary with Python plugins

### Negative

- higher contribution and recruiting bar than a higher-level language
- more implementation ceremony for some product surfaces

### Neutral

- plugin authors remain decoupled from the core language because plugin transport is subprocess-based

## Related Decisions

- Related to: [ADR-002](./ADR-002-plugin-transport-json-rpc.md) (plugin transport — out-of-process subprocess is what decouples plugin authoring language from the core), [ADR-003](./ADR-003-entity-id-scheme.md) (entity IDs — Rust string handling and serde shape the canonical-name representation)
- [ADR-011](./ADR-011-writer-actor-concurrency.md) — writer-actor concurrency lives inside this ADR's commitment to `rusqlite` + `tokio` + `deadpool-sqlite`; ADR-011 picks the concurrency shape Rust's ecosystem supports.
- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — the `tokio::process::Child` supervision plus `prlimit`/`setrlimit` spawn-time resource capping in ADR-021 Layer 2 is an ADR-001 ecosystem consequence.

## References

- [Loomweave v0.1 system design](../v0.1/system-design.md)
- [Loomweave v0.1 detailed design](../v0.1/detailed-design.md)
