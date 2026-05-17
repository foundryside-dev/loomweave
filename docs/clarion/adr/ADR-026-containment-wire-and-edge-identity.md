# ADR-026: Containment Wire Shape and Edge Identity

**Status**: Accepted
**Date**: 2026-05-05
**Deciders**: qacona@gmail.com
**Context**: B.3 (Sprint 2 Tier B) introduces the first edge kind Clarion has ever persisted (`contains`). Sprint-1 locked entity wire shape but explicitly deferred edge wire shape: the kickoff handoff at `docs/superpowers/handoffs/2026-04-30-sprint-2-kickoff.md` §"Edge wire shape" states "B.3's first edge will define the protocol-level wire shape". Three coupled decisions arise — the wire envelope, the edge-row identity in storage, and the per-kind contract for `source_byte_start/end`. Locking them together prevents the four later edge kinds (`calls`, `imports`, `decorates`, `inherits_from`) from inheriting an under-specified precedent.

## Summary

Three decisions are locked here; each was unanimously or majority-affirmed by the five-reviewer panel for B.3 (systems-thinker, solution-architect, architecture-critic, Python/Rust engineer, quality-engineer):

1. **Edge wire envelope** — a top-level `edges: Vec<RawEdge>` field on `AnalyzeFileResult`. Edges are first-class peers of entities on the wire, not properties of entities.
2. **Edge row identity in storage** — drop the `id TEXT PRIMARY KEY` column from the `edges` table. `(kind, from_id, to_id)` becomes the natural primary key.
3. **Per-kind source-range contract** — `contains`, `in_subsystem`, `guides` MUST emit no `source_byte_start/end` (the edge has no source citation distinct from its endpoints' ranges). `calls`, `imports`, `decorates`, `emits_finding` MUST emit `source_byte_start/end` (the call site / import statement / decoration target IS the edge's location). The schema's existing nullability is repurposed as a per-kind invariant the writer-actor enforces, not a permissive default.

## Context

### Why these three decisions belong together

The wire envelope (decision 1) determines what crosses the JSON-RPC boundary. The storage row identity (decision 2) determines what crosses the writer-actor boundary. The source-range contract (decision 3) determines what every edge consumer (catalog renderer, briefing composer, ADR-006 clusterer) is permitted to assume about edge metadata. All three were ambiguous before this ADR; locking any two of three would leave the unlocked third site to set precedent under deadline pressure.

### Pre-decision state (Sprint-1 close, 2026-04-28)

- `AnalyzeFileResult { entities: Vec<Value> }` at `crates/clarion-core/src/plugin/protocol.rs:320-337`. No `edges` field. Sprint-1 plugins cannot emit edges; the host's typed-deserialise path at `host.rs:824` would silently ignore them.
- `edges` table at `crates/clarion-storage/migrations/0001_initial_schema.sql:66-79` carries `id TEXT PRIMARY KEY` AND `UNIQUE (kind, from_id, to_id)`. Two redundant identification surfaces. Three indexes (`ix_edges_from_kind`, `ix_edges_to_kind`, `ix_edges_kind`) — none on `id`.
- `WriterCmd` enum at `crates/clarion-storage/src/commands.rs:71-105` has no `InsertEdge` variant; the file-level comment names this as a later-WP addition.
- `EntityRecord.parent_id: Option<String>` exists; Sprint-1 always emits `None`.
- ADR-022:33 says plugins emit `contains` edges. ADR-022:52 says plugins emit `parent_id` chains. The dual-encoding question is addressed in the B.3 design doc; this ADR locks the wire/identity/source-range surface those encodings ride on.

### What B.3 unblocks

After ADR-026 lands, B.3 can:

- Add `edges: Vec<RawEdge>` to `AnalyzeFileResult` with `#[serde(default)]` (non-breaking for Sprint-1 plugins that emit no edges).
- Drop `edges.id` in a schema migration (the column has never been written; ADR-024's edit-in-place migration policy still applies pre-publication; retirement trigger is the same as ADR-024's).
- Emit `source_byte_start/end = NULL` on contains edges with the writer-actor enforcing the invariant by kind.

## Decision

### Decision 1 — Edge wire envelope

`AnalyzeFileResult` extends to:

```rust
pub struct AnalyzeFileResult {
    pub entities: Vec<RawEntity>,
    #[serde(default)]
    pub edges: Vec<RawEdge>,
}

pub struct RawEdge {
    pub kind: String,
    pub from_id: String,
    pub to_id: String,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
    #[serde(default)]
    pub properties: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}
```

The `#[serde(default)]` on `edges` makes the addition non-breaking for any Sprint-1 plugin that pre-dates B.3. The `#[serde(flatten)] extra` matches the `RawEntity` precedent from B.2 — future edge-kind-specific properties (e.g., `decorated_by.stack_index`) ride through `extra` non-breakingly.

**Cross-file edges (forward-looking).** When `imports`/`calls` ship in later sprints, `from_id` and `to_id` may reference entity IDs that are not in the current file's `entities` list (the target is in a different file, or unresolved). The host MUST resolve those IDs against the global entity store, not against the local `AnalyzeFileResult`. Edge readers that assume per-file self-containment will break the moment cross-file edges land. This is documented here so B.4's catalog renderer and B.5's per-subsystem markdown writer cannot inherit the wrong assumption.

### Decision 2 — Edge row identity

The `edges` schema becomes:

```sql
CREATE TABLE edges (
    kind               TEXT NOT NULL,
    from_id            TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    to_id              TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    properties         TEXT,
    source_file_id     TEXT REFERENCES entities(id),
    source_byte_start  INTEGER,
    source_byte_end    INTEGER,
    PRIMARY KEY (kind, from_id, to_id)
) WITHOUT ROWID;
```

Two changes from the Sprint-1 schema: the `id TEXT PRIMARY KEY` column is removed; `(kind, from_id, to_id)` becomes the primary key (replacing the earlier `UNIQUE` constraint). `WITHOUT ROWID` is added because the natural PK makes the rowid redundant.

The `id` column never had a reader. No query in `clarion-storage`, `clarion-core`, or `clarion-cli` selects edges by `id`; lookups go through the three indexes (`ix_edges_from_kind`, `ix_edges_to_kind`, `ix_edges_kind`) which become covering indexes when the natural PK lands. The byte-cost analysis: at the v0.1 elspeth-slice target (~425k LOC × ~5 edges/entity ≈ 2M edges), an unused 16-byte hex-encoded `id` plus its B-tree costs ~120 MB of dead storage. Drop it.

The natural PK provides idempotency-by-construction: re-running `analyze` on the same file produces the same `(kind, from_id, to_id)` triples; SQLite's `INSERT OR IGNORE` (or its equivalent in the writer-actor's batch logic) handles the duplicate without surfacing a rejected-edge condition.

**Why not a hash-based synthesised id?** The panel split 2-2-1 between (b) core-hashed `id` and (c) drop the column. The decisive arguments for (c): (i) the rejected-edge counter (see B.3 design §6 observability requirement) provides everything a synthetic id would, namely "did the storage layer accept or drop this edge?"; (ii) `findings.entity_id` is the only existing cross-table reference into the structural graph, and it points at entities, not edges — there is no extant or planned reader for edge ids; (iii) ADR-024's edit-in-place migration policy applies (pre-publication, no consumer); the cost of recovering from this decision later (re-add `id` column with a real reader driving the format) is bounded, while the cost of keeping it now is recurring per-build storage waste.

### Decision 3 — Per-kind source-range contract

The `edges.source_byte_start` and `edges.source_byte_end` columns are nullable, but the nullability is per-kind invariant rather than free permission:

| Edge kind | `source_byte_start/end` |
|---|---|
| `contains` | MUST be `NULL` |
| `in_subsystem` | MUST be `NULL` |
| `guides` | MUST be `NULL` |
| `emits_finding` | MUST be `NULL` |
| `calls` | MUST be `Some` (the call-site location) |
| `imports` | MUST be `Some` (the import statement location) |
| `decorates` | MUST be `Some` (the decoration target location) |
| `inherits_from` | MUST be `Some` (the base-class declaration in the class header) |

The structural / non-derivable distinction is the discriminator. `contains` and `in_subsystem` are facts about graph structure with no separate textual occurrence — the location of "module M contains function F" is identical to F's source range, already stored on the entity. `guides` and `emits_finding` are core-emitted edges connecting structural entities to guidance / findings; the guidance's or finding's own source citation lives elsewhere. `calls`, `imports`, `decorates`, `inherits_from` all have a specific token in the source code that IS the edge — the call site, the import statement, the `@decorator_name` line, the base-class identifier in `class X(Base):`.

The writer-actor enforces this invariant at insert time: an edge whose `kind` requires `Some` arrives with `None` (or vice versa) is rejected with `CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT`. This converts the schema's permissiveness into a contract every consumer can rely on.

### Plugin-side mechanical implications

For the Python plugin specifically (Sprint-2 B.3):

- `RawEdge` TypedDict at module level in `extractor.py`:
  ```python
  class RawEdge(TypedDict):
      kind: str
      from_id: str
      to_id: str
      source_byte_start: NotRequired[int]
      source_byte_end: NotRequired[int]
  ```
  Contains edges omit the byte-offset fields entirely (NotRequired absent ⇒ JSON omits the keys ⇒ Rust deserialises to `None`).

- `extract()` return type changes from `list[RawEntity]` to `tuple[list[RawEntity], list[RawEdge]]`. (B.3 design doc §4 spells the alternative — a return-typed dict — and rejects it.)

- `_walk` accumulates into both lists in parallel; `_build_*_entity` returns `tuple[RawEntity, str]` so the entity-id string is reused for the `contains`-edge `from_id` rather than re-derived via a second `entity_id()` call. This closes the parent-id reconstruction risk noted in B.3 design §3 Q3 (Python/Rust engineer panel finding).

## Alternatives Considered

### Alternative 1: Per-entity edges via `RawEntity.extra.edges` (Q1 option b)

Each entity carries an outbound-edges list. Convenient for `contains` (the parent entity owns its containment relationship); disastrous for `calls` / `imports` where the source entity may be a function in this file but the target is in a different file or unresolved.

**Why rejected**: serde-flatten doesn't compose with collections (it requires Map-like, not Vec-like). More importantly, encoding edges as entity sub-properties commits to a single-owner model the storage schema explicitly does not enforce — `edges.from_id` and `edges.to_id` are peer references to entities, not parent/child. The architecture-critic's framing: "encodes the wishful thinking that every edge has a single owning entity in the analysed file. Works for `contains`, falls apart for `imports`, is an active lie for `calls`."

### Alternative 2: Separate `analyze_file_edges` RPC method (Q1 option c)

Plugin returns entities first, host calls again for edges.

**Why rejected**: doubles round-trip latency for no architectural benefit. The 425k LOC elspeth-slice benchmark amortises file analysis across thousands of `analyze_file` calls; doubling the RPC count is a real perf regression. Also creates a consistency window where entities exist without their edges.

### Alternative 3: Plugin synthesises edge `id` (Q4 option a)

`python:contains:from_id->to_id` or similar.

**Why rejected**: the entity-id grammar (ADR-003) is plugin-owned because plugins know language-specific qualnames. Edge identity is derivable from `(kind, from_id, to_id)` mechanically; making plugins synthesise that derivation is busywork and a source of cross-plugin format drift.

### Alternative 4: Core-hashes the edge `id` (Q4 option b)

BLAKE3 (or SHA-256 truncated) of `(kind, from_id, to_id)` produces a deterministic stable identifier per edge.

**Why rejected**: solves a problem nothing has — no current or planned reader uses `edges.id`. The schema invariant `(kind, from_id, to_id) UNIQUE` already provides what hashing would give: deterministic, idempotent re-analyze produces the same logical edges. Adding 16-32 bytes per row of unused identifier is pure overhead.

### Alternative 5: `properties` field carries source range as JSON (decision 3)

Instead of typed `source_byte_start/end` columns, edges carry a `properties: { source: {start, end} }` JSON blob.

**Why rejected**: queries that filter by edge location ("show me all calls in this byte range") become JSON-extraction queries (slow, unindexable). The columns already exist; using them for the load-bearing case is faster than blob-querying.

## Consequences

### Positive

- **First edge ships with a documented contract.** Every later edge kind inherits the wire envelope, the per-kind source-range invariant, and the natural PK identity surface. No precedent is set silently.
- **Storage is honest.** The `edges` table's permissive nullability becomes a per-kind contract enforced at write time. Consumers can write `assert edge.source_byte_start is not None` for `calls`/`imports` and rely on it without defensive guards.
- **Cross-file edge resolution is explicit.** The wire envelope decision documents that `from_id`/`to_id` may reference entities outside the current file; B.4's catalog renderer cannot inherit a per-file self-containment assumption.
- **No dead bytes.** ~120 MB of unused `edges.id` storage at v0.1 scale is reclaimed; SQLite's `WITHOUT ROWID` optimisation works because the natural PK is stable.

### Negative

- **Migration churn.** Dropping `edges.id` is a schema migration. Per ADR-024, this is permissible pre-publication (the column has never been written). Once a published Clarion build emits its first edge row, this ADR's edit-in-place permission retires; future column changes require additive migration files.
- **Invariant enforcement adds a writer-actor responsibility.** The per-kind source-range contract is enforced by the writer; an edge violating it is rejected with a finding (`CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT`). One more thing the writer can refuse, one more failure mode plugin authors must understand.
- **`#[serde(flatten)] extra` on `RawEdge`** — like `RawEntity.extra`, consumers should not depend on unflattening this map for typed fields. Adding a typed field on `RawEdge` later requires migrating consumers off `extra`.

### Neutral

- The cross-file edge-resolution rule has no v0.1 implementation cost (B.3 only emits within-file `contains`). The cost is documentation discipline, paid in this ADR and the B.3 design doc.
- `WITHOUT ROWID` is a SQLite-specific optimisation. If Clarion ever adopts a non-SQLite storage backend (not currently planned), this clause needs reconsideration.

## Related Decisions

- [ADR-002](./ADR-002-plugin-transport-json-rpc.md) — JSON-RPC subprocess transport. ADR-026 extends `AnalyzeFileResult`; the wire envelope is unchanged.
- [ADR-003](./ADR-003-entity-id-scheme.md) — entity ID grammar. Edges reference entity IDs by these strings; no new identifier grammar is introduced for edges.
- [ADR-007](./ADR-007-summary-cache-key.md) — summary-cache key. Edges are not in the cache key 5-tuple; new edge kinds do not invalidate caches by themselves. Future per-edge `properties` may; out of scope here.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — core-reserved edge kinds. ADR-026 honours the reservation: `contains`, `in_subsystem`, `guides`, `emits_finding` retain core-defined semantics; this ADR specifies the source-range portion of those semantics for the four reserved kinds plus the four plugin-emitted kinds (`calls`, `imports`, `decorates`, `inherits_from`).
- [ADR-024](./ADR-024-guidance-schema-vocabulary.md) — edit-in-place migration policy. Dropping `edges.id` from migration `0001_initial_schema.sql` falls under that policy; the same retirement trigger (first external operator pulls a published build) applies.
- [ADR-027](./ADR-027-ontology-version-semver.md) — ontology_version semver policy. B.3 bumps `ontology_version` 0.2.0 → 0.3.0 (MINOR — additive `contains` edge kind); the policy ADR-027 names is what makes that bump unambiguous.

## References

- [B.3 design doc](../../implementation/sprint-2/b3-contains-edges.md) — wire shape, emission policy, parent_id provenance, manifest bump, test plan.
- [Sprint-2 kickoff handoff §"Edge wire shape"](../../superpowers/handoffs/2026-04-30-sprint-2-kickoff.md) — the deferral this ADR resolves.
- [Sprint-1 walking-skeleton e2e](../../../tests/e2e/sprint_1_walking_skeleton.sh) — extends to assert at least one edge row post-B.3.
- Five-reviewer panel transcripts (in B.3 brainstorming conversation log on `sprint-2/b3-design` branch).
