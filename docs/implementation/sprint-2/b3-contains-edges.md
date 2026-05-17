# B.3 — Python plugin: `contains` edges (Sprint 2 / Tier B / first edge kind)

**Status**: DRAFT — Sprint 2 Tier-B B.3 work-package design
**Anchoring design**: [system-design.md §6 (Guidance composition; clustering)](../../clarion/v0.1/system-design.md#6-guidance-system), [detailed-design.md §3 (Schemas, edge tables)](../../clarion/v0.1/detailed-design.md), [B.2 design doc](./b2-class-module-entities.md) (predecessor)
**Accepted ADRs**: [ADR-002](../../clarion/adr/ADR-002-plugin-transport-json-rpc.md), [ADR-003](../../clarion/adr/ADR-003-entity-id-scheme.md), [ADR-006](../../clarion/adr/ADR-006-clustering-algorithm.md), [ADR-007](../../clarion/adr/ADR-007-summary-cache-key.md), [ADR-022](../../clarion/adr/ADR-022-core-plugin-ontology.md), [ADR-023](../../clarion/adr/ADR-023-tooling-baseline.md), [ADR-024](../../clarion/adr/ADR-024-guidance-schema-vocabulary.md), [ADR-026](../../clarion/adr/ADR-026-containment-wire-and-edge-identity.md), [ADR-027](../../clarion/adr/ADR-027-ontology-version-semver.md)
**Predecessor**: [B.2 — class + module entity emission](./b2-class-module-entities.md)
**Successor**: B.1 (multi-file dispatch — WP4 Phase 0+1) and B.4 (catalog rendering)
**Sprint-2 kickoff handoff**: [`docs/superpowers/handoffs/2026-04-30-sprint-2-kickoff.md`](../../superpowers/handoffs/2026-04-30-sprint-2-kickoff.md) §"What's in scope for Sprint 2" Tier B row B.3

---

## 1. Scope

B.3 introduces the first edge kind Clarion has ever persisted. Three things change in lockstep:

- **Plugin emits `contains` edges** for every immediate-parent containment relationship (module → top-level function/class, class → method, class → nested class, function → nested function, function → nested class).
- **Plugin emits `parent_id` on every entity** (dual encoding with the contains edge for the same fact). The writer-actor enforces consistency.
- **Storage gains the first non-empty `edges` rows.** Schema tightened per ADR-026 (drop `edges.id` synthetic PK; promote `(kind, from_id, to_id)` to natural PK; per-kind source-range contract enforced at write time).

**Out of scope** (deferred to later sprints):

- Decorator, calls, imports, inherits_from edges (later WP3-feature-complete).
- Cross-file edge emission (B.1's multi-file dispatch is the prerequisite; cross-file resolution is later still).
- `in_subsystem`, `guides`, `emits_finding` core-emitted edges (different ownership; not plugin work).
- Top-level `__init__.py` edge cases — same skip policy as B.2.

## 2. Locked surfaces from Sprint 1 + B.2 (B.3 reads and writes against these)

These are caller-observable surfaces locked at `v0.1-sprint-1` close + B.2 close. B.3 must not change them; if a change is genuinely needed, write an ADR amendment per the kickoff-handoff convention.

- **Wire shape entity portion** (`crates/clarion-core/src/plugin/host.rs:132-154`): `RawEntity { id, kind, qualified_name, source: RawSource, extra }` with `#[serde(flatten)] extra`. `RawSource { file_path, extra }` likewise. B.3 adds `parent_id: Option<String>` as a NEW typed field on `RawEntity` (NOT via `extra`) — it's load-bearing for the writer's parent-id consistency check (§3 Q2 below).
- **L7 qualname format**: `{dotted_module}.{__qualname__}`. Class-in-class chains class names with no `<locals>`; function-parent boundary inserts `<locals>`. B.3 unchanged.
- **L4 JSON-RPC method set**: `initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`. B.3 changes none of these.
- **L5 manifest schema**: B.3 amends only `[ontology].edge_kinds` (adds `"contains"`), `[ontology].ontology_version` (`0.2.0` → `0.3.0`), and corresponding constants. No structural change.
- **L8 Wardline pin**: unchanged.
- **`extract()` signature** (B.2 baseline): `extract(source: str, file_path: str, *, module_prefix_path: str | None = None) -> list[RawEntity]`. **B.3 changes the return type** to `tuple[list[RawEntity], list[RawEdge]]` per ADR-026's wire envelope (§3 Q1).
- **`_walk` accumulator pattern** (B.2 baseline): mutates an `out: list[RawEntity]` parameter. **B.3 extends to dual accumulators** (`out_entities`, `out_edges`).
- **Per-kind builder split** (B.2 §3 Q3 lock-in): `_build_function_entity`, `_build_class_entity`, `_build_module_entity`. **B.3 changes return type** of the function/class builders to `tuple[RawEntity, str]` so `_walk` reuses the entity-id string for the contains-edge `from_id` (per Python/Rust engineer panel finding §3 Q3 below).
- **Module-entity `source_range` sentinel** (B.2 §3 Q4 lock-in): `end_col=0` is a sentinel for module entities only. Class and function entities use real `ast.*.end_col_offset` data. B.3 unchanged; `contains` edges carry NO `source_byte_start/end` (per ADR-026 decision 3).

## 3. Design decisions (Q1–Q6 panel-resolved)

Each decision below was taken to a five-reviewer panel (systems thinker, solution architect, architecture critic, Python engineer, quality engineer). Vote tallies and reconciliations are in §11. Q1 was unanimous; Q2 and Q4 split and were reconciled; Q3, Q5, Q6 unanimous.

### Q1 — Edge wire envelope

**Decision**: Top-level `edges: Vec<RawEdge>` field on `AnalyzeFileResult`. Wire shape becomes `{"entities": [...], "edges": [...]}`. `#[serde(default)]` on the `edges` field makes the addition non-breaking for any plugin that pre-dates B.3 (Sprint-1 plugins emit no edges).

**`RawEdge` shape** (Rust, per ADR-026 decision 1):

```rust
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

`RawEdge.extra` matches the `RawEntity.extra` precedent from B.2 — future edge-kind-specific properties (e.g., `decorated_by.stack_index` per `detailed-design.md:153`) ride through `extra` non-breakingly.

**`RawEdge` shape** (Python TypedDict, B.3 introduces):

```python
class RawEdge(TypedDict):
    kind: str
    from_id: str
    to_id: str
    source_byte_start: NotRequired[int]
    source_byte_end: NotRequired[int]
```

Contains edges omit the byte-offset fields entirely (NotRequired absent ⇒ JSON omits the keys ⇒ Rust deserialises to `None`).

**Why**: Five-reviewer panel unanimous (5×(a) high confidence). Edges are not entity sub-fields semantically; calls/imports/decorates/inherits will all want the same wire shape; per-entity nesting (option b) breaks for cross-file edges; separate RPC (option c) doubles round-trip latency with no benefit. ADR-026 documents the reasoning canonically.

**Cross-file edge resolution (forward-looking)**: when `imports`/`calls` ship later, `from_id`/`to_id` may reference entities outside the current file. The host MUST resolve those IDs against the global entity store, not the local `AnalyzeFileResult`. Documented in ADR-026 §"Decision 1" so B.4's catalog renderer cannot inherit a per-file-self-containment assumption.

### Q2 — `parent_id` provenance: dual encoding with writer-actor enforcement

**Decision**: Plugin emits BOTH `parent_id` on `RawEntity` (as a typed top-level field) AND a `contains` edge for the same fact. The writer-actor enforces the consistency invariant on commit: every entity's `parent_id` MUST match exactly one `contains` edge `(kind=contains, from_id=parent_id, to_id=entity.id)`; mismatches reject the run with `CLA-INFRA-PARENT-CONTAINS-MISMATCH`.

**Why**:

- The panel split 3-2 between (a) dual encoding and (b) plugin-emits-only-edges. The (b) votes (architecture-critic, systems-thinker) named a real risk: dual encoding without enforcement drifts. The (a) votes (solution-architect, Python/Rust engineer, quality-engineer) named the implementation cost: deriving `parent_id` from edges in the writer-actor requires either reordering writes (entities → edges → UPDATE entities.parent_id) or buffering the run. Reconciliation: lock (a) BUT make the architecture-critic's enforcement requirement non-optional. Both encodings exist; the writer treats divergence as a contract violation.
- Both encodings have indexed consumers: `parent_id` → the `ix_entities_parent` index for "find children of X" queries (B.4 catalog rendering); `contains` edges → ADR-006 clusterer subgraph (treated alongside `imports`/`calls` uniformly).
- The 12-byte-per-entity overhead is negligible at v0.1 elspeth-slice scale (~425k LOC).
- Architecture critic's specific concern: "you haven't specified the resolution rule" → the rule is now spec'd: writer rejects on mismatch with a named finding code, not silent-last-writer-wins.

**Wire-shape change to `RawEntity`** (B.3 amends B.2):

```python
class RawEntity(TypedDict):
    id: str
    kind: str
    qualified_name: str
    source: EntitySource
    parent_id: NotRequired[str]    # NEW in B.3 — None for module entities; str for function/class
    parse_status: NotRequired[Literal["ok", "syntax_error"]]
```

`parent_id` is `NotRequired` (omitted from JSON) for module entities only — they have no parent within the file. Function and class entities always include it.

The Rust-side `RawEntity` gets a new typed field (`pub parent_id: Option<String>`), NOT a serde-flatten extra entry. The architecture-critic + Python/Rust engineer agree: `parent_id` is load-bearing for the writer's read path; routing it through the opaque `extra` map and reading it out by string-key would silently drop the field on a typo.

### Q3 — Emission policy: all immediate-parent containments

**Decision**: Plugin emits one `contains` edge per non-module entity, with `from_id` = immediate-parent entity id, `to_id` = child entity id. Includes function-internal nesting (`def f(): def g()`); does NOT include transitive (module → grandchild) edges.

The full case list:

- module → top-level function/class
- class → class method (function in class body)
- class → nested class
- function → nested function (def in def, under `<locals>` qualname boundary)
- function → nested class (class in def, under `<locals>` boundary)
- nested class → method (function in nested class)

**Why**: Five-reviewer panel unanimous (5×(a) high confidence). Function-scoped nesting included per architecture-critic's framing: "emitter is exhaustive; renderer-side filtering owns presentation." Skipping function-scoped nesting (option b) bakes a Python-shaped assumption into emission policy that clusterer/catalog/briefing consumers might or might not want; transitive (option c) creates O(N²) edge inflation with no analytic benefit (clusterer takes transitive closure if needed; transitive form is a derived view).

**Non-goal locked here**: B.4 catalog rendering and B.5 per-subsystem markdown are responsible for filtering function-scoped contains edges out of their output if they don't want them. B.3 emits everything; downstream renderers decide what to show.

### Q4 — Edge row identity: drop the `id` column

**Decision**: Drop the `id TEXT PRIMARY KEY` column from `crates/clarion-storage/migrations/0001_initial_schema.sql:66-79`. Promote `(kind, from_id, to_id)` from `UNIQUE` constraint to `PRIMARY KEY`. Add `WITHOUT ROWID` clause for SQLite optimization (the natural PK makes the rowid redundant).

**Why**: Panel split 2-2-1; reconciliation favored (c) drop-the-column based on the strongest concrete arguments:

- **Architecture-critic (vote c, 0.80)**: "There is no query in Sprint-1 or B.3 that selects edges by `id`; lookups go via `(kind, from_id, to_id)` which is already UNIQUE-indexed... at ~2M edges (425k LOC × ~5 edges/entity), a 16-byte truncated SHA-256 is ~32 MB of pure ceremony plus the PK B-tree."
- **Python/Rust engineer (vote c, high)**: "Schema-only change with no application code to update since `InsertEdge` does not yet exist."
- **Counter-arguments from QA-engineer (vote b, 0.8)**: "drop-the-column makes finding-attachment to edges impossible later." Resolution: `findings.entity_id` (`migrations/0001_initial_schema.sql:92`) is the only existing cross-table reference into the structural graph, and it points at entities, not edges. A future "attach finding to specific edge" use case can be designed when it has a real consumer driving the format; for now, `(kind, from_id, to_id)` IS a stable reference (deterministic, idempotent under re-analyze).

**Migration mechanics** (per ADR-024's edit-in-place migration policy):

We're at the zero-cost frontier for the edges table — it has never been written. Edit `migrations/0001_initial_schema.sql` in place rather than authoring a new migration file. ADR-024's retirement trigger (first external operator pulls a published Clarion build) hasn't fired; the in-place edit is permissible.

The schema becomes:

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
CREATE INDEX ix_edges_from_kind ON edges(from_id, kind);
CREATE INDEX ix_edges_to_kind   ON edges(to_id,   kind);
CREATE INDEX ix_edges_kind      ON edges(kind);
```

The three indexes survive (they're query-driving, not PK-derived).

### Q5 — Source range on `contains` edges: emit nothing

**Decision**: `contains` edges emit no `source_byte_start`, no `source_byte_end`. The Python plugin omits the JSON keys entirely (NotRequired absent); the Rust host's `serde(default)` produces `None`; storage's nullable columns persist `NULL`.

**Per-kind contract** (per ADR-026 decision 3, enforced by writer-actor):

| Edge kind | `source_byte_start/end` |
|---|---|
| `contains` | MUST be `NULL` |
| `in_subsystem` | MUST be `NULL` |
| `guides` | MUST be `NULL` |
| `emits_finding` | MUST be `NULL` |
| `calls` | MUST be `Some` |
| `imports` | MUST be `Some` |
| `decorates` | MUST be `Some` |
| `inherits_from` | MUST be `Some` |

Writer rejects on violation with `CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT`.

**Why**: Five-reviewer panel unanimous (5×(a) high confidence). Containment is a structural fact derived from the AST; its "location" is identical to the contained entity's source range (already on the entity). Adding line columns to the schema (option b) is scope creep beyond B.3; computing byte offsets in the plugin (option c) requires byte-offset tracking the plugin doesn't otherwise need. The per-kind contract converts the schema's ambient `NULL` permissiveness into a kind-dispatched invariant — consumers can write `assert edge.source_byte_start is not None` for `calls`/`imports` and rely on it.

### Q6 — Manifest + ontology_version bump

**Decision**: Mechanical, following B.2 + ADR-027 precedent.

| File | Change |
|---|---|
| `plugins/python/plugin.toml` | `[ontology].edge_kinds = ["contains"]`; `[ontology].ontology_version = "0.3.0"` |
| `plugins/python/src/clarion_plugin_python/server.py` | `ONTOLOGY_VERSION = "0.3.0"` |
| `plugins/python/src/clarion_plugin_python/__init__.py` | `__version__ = "0.1.2"` (PATCH — no breaking API change; new edge emission is additive via `serde(default)`) |

ADR-027 is the policy citation: this is a MINOR ontology bump (additive `contains` edge kind). PATCH on package version because the public Python API surface (`extract()` signature) becomes `tuple[list[RawEntity], list[RawEdge]]` — that IS technically a return-type change, but the only consumer is the plugin's own server module which is updated in lockstep, so the patch designation is faithful to "no breaking external API change."

## 4. Wire shape additions

Summary of all wire-shape changes B.3 introduces:

**`AnalyzeFileResult`** (Rust):
- New: `edges: Vec<RawEdge>` with `#[serde(default)]`.
- Existing: `entities: Vec<RawEntity>` (was `Vec<Value>` placeholder; B.3 graduates to typed `RawEntity`).

Note: graduating `entities` from `Vec<Value>` to `Vec<RawEntity>` is a follow-up cleanup tracked separately (Sprint-1 `protocol.rs:328` calls it out as Task-6 work). B.3 does NOT depend on that graduation; the `Vec<Value>` opaque deserialise path continues to work, and the host's existing edge-deserialise path will use the typed `RawEdge`.

**`RawEntity`** (B.2 baseline + B.3 additions):
- New: `parent_id: NotRequired[str]` (Python TypedDict) / `pub parent_id: Option<String>` (Rust). Typed field, NOT via extra.
- Existing: `id`, `kind`, `qualified_name`, `source`, `parse_status: NotRequired[Literal[...]]`, `extra` (Rust serde-flatten).

**`RawEdge`** (B.3 introduces, both sides):
- Required: `kind`, `from_id`, `to_id`.
- Optional: `source_byte_start`, `source_byte_end` (per-kind contract; contains edges omit).
- Future: `properties` (JSON value), `extra` (Rust serde-flatten map; Python doesn't expose at this layer).

## 5. Storage protocol additions

**`WriterCmd::InsertEdge`** new variant in `crates/clarion-storage/src/commands.rs`:

```rust
#[derive(Debug, Clone)]
pub struct EdgeRecord {
    pub kind: String,
    pub from_id: String,
    pub to_id: String,
    pub properties_json: Option<String>,
    pub source_file_id: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
}

pub enum WriterCmd {
    // ... existing variants ...
    InsertEdge {
        edge: Box<EdgeRecord>,
        ack: Ack<()>,
    },
}
```

**Batch counter rename** (Python/Rust engineer panel finding):

The writer-actor's `inserts_in_batch` counter (entity-only today) becomes `writes_in_batch` and increments for both `InsertEntity` and `InsertEdge`. Otherwise edge-heavy files would never trigger an intermediate commit, and the batch boundary at every N writes would silently break.

**Source-file-id derivation** (Python/Rust engineer panel finding):

The plugin does NOT emit `source_file_id` on edges. The host derives it from the entity's `source.file_path` for the corresponding module entity. Plugin-side coupling to entity-id formulas for OTHER entity kinds (`file`) is the exact coupling the plugin boundary should avoid (per ADR-022). The host's edge-insert path looks up `source_file_id = module_entity_id_for(edge.from_id)` and populates it before issuing `InsertEdge`.

For B.3 specifically, every `contains` edge has `from_id` rooted at a within-file entity (no cross-file), so the source-file derivation is straightforward: walk up the parent chain from `from_id` to the module entity, use that module entity's `id` as `source_file_id`. (For later cross-file `calls`/`imports`, this gets more complex; out of scope here.)

**Parent-id consistency check** (Q2 enforcement):

After all entities and edges for a run are inserted (at `CommitRun` time), the writer-actor runs:

```sql
SELECT e.id, e.parent_id, ce.from_id
FROM entities e
LEFT JOIN edges ce
  ON ce.kind = 'contains' AND ce.to_id = e.id
WHERE e.parent_id IS NOT NULL
  AND (ce.from_id IS NULL OR ce.from_id != e.parent_id);
```

Any non-empty result rejects the run with `CLA-INFRA-PARENT-CONTAINS-MISMATCH` and rolls back the in-flight transaction. Inverse check (every contains edge has a matching parent_id):

```sql
SELECT ce.from_id, ce.to_id, e.parent_id
FROM edges ce
JOIN entities e ON e.id = ce.to_id
WHERE ce.kind = 'contains'
  AND (e.parent_id IS NULL OR e.parent_id != ce.from_id);
```

Both checks are constant-cost relative to the run's row count (single SELECT each); they fit inside the writer-actor's existing transaction commit path.

## 6. Observability additions

**Dropped-edge counter** (QA panel finding, recurring):

The writer-actor exposes a per-run `dropped_edges_total` field on `AnalyzeFileOutcome` (or whatever the run-summary struct is named — verify against current code). Dropped edges include:

- `(kind, from_id, to_id)` UNIQUE conflicts (idempotent re-analyze).
- Dangling FK references (edge whose `from_id` or `to_id` is not in the entities table).
- Per-kind source-range contract violations (`CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT`).

The walking-skeleton e2e (`tests/e2e/sprint_1_walking_skeleton.sh`) asserts `dropped_edges_total == 0` post-analyze. Without this assertion, "VER-without-VAL" applies: schema is correct, tests pass, catalog is wrong because edges silently dropped.

## 7. Implementation task ledger

### Task 1 — Storage schema migration (drop edges.id, add WITHOUT ROWID)

Files:
- Modify: `crates/clarion-storage/migrations/0001_initial_schema.sql` (in-place edit per ADR-024 / ADR-026 retirement rule).
- Modify: `crates/clarion-storage/tests/schema_apply.rs` (any test asserting on `edges.id` column).

Steps:
- Failing test: schema_apply test asserting edges has primary key `(kind, from_id, to_id)` and no `id` column.
- Run, verify fail.
- Edit migration: drop `id TEXT PRIMARY KEY`, drop `UNIQUE (kind, from_id, to_id)`, add `PRIMARY KEY (kind, from_id, to_id)`, add `WITHOUT ROWID`.
- Run, verify pass.
- Verify no other Rust test references `edges.id` (grep).
- Commit: `feat(storage): drop edges.id; promote (kind,from_id,to_id) to PK (B.3 ADR-026)`.

### Task 2 — `WriterCmd::InsertEdge` + `EdgeRecord` + writer-actor integration

Files:
- Modify: `crates/clarion-storage/src/commands.rs` (add `EdgeRecord`, add `WriterCmd::InsertEdge`).
- Modify: `crates/clarion-storage/src/writer.rs` (add INSERT logic; rename `inserts_in_batch` → `writes_in_batch`; per-kind source-range contract enforcement; dropped-edge counter).
- Modify: `crates/clarion-storage/tests/writer_actor.rs` (round-trip insert tests; per-kind contract violation tests; dropped-edge counter tests).

Steps:
- Failing test: writer-actor inserts a contains edge, asserts row visible to a reader.
- Implement minimal `InsertEdge` handler.
- Failing test: per-kind source-range contract — emit `calls` edge with no source range, expect rejection.
- Implement contract enforcement.
- Failing test: re-analyze produces idempotent edge state (UNIQUE conflicts increment `dropped_edges_total`).
- Implement dropped-edge counter; expose on `AnalyzeFileOutcome`.
- Failing test: `inserts_in_batch` rename test (a file with many edges, few entities, triggers intermediate commit at the configured batch boundary).
- Implement counter rename + edge increment.
- Commit: `feat(storage): WriterCmd::InsertEdge + per-kind source-range contract (B.3)`.

### Task 3 — Host wire shape: `RawEdge` + `RawEntity.parent_id` + `AnalyzeFileResult.edges`

Files:
- Modify: `crates/clarion-core/src/plugin/protocol.rs` (add `RawEdge` struct; add `edges: Vec<RawEdge>` to `AnalyzeFileResult`; add `parent_id: Option<String>` to `RawEntity` if/when it graduates from `Vec<Value>` — Sprint-1 `protocol.rs:328` notes this is Task 6 graduation work).
- Modify: `crates/clarion-core/src/plugin/host.rs` (deserialise `edges` from `AnalyzeFileResult`; pass through to writer; derive `source_file_id` from module entity).
- Modify: `crates/clarion-core/tests/host_subprocess.rs` (test edge round-trip).

Steps:
- Failing test: a fixture plugin returns one entity and one contains edge; host deserialises both; storage persists both.
- Implement minimal `RawEdge` deserialise + pass-through.
- Failing test: parent-id consistency check (entity declares parent_id but no matching contains edge → run rejected with `CLA-INFRA-PARENT-CONTAINS-MISMATCH`).
- Implement consistency check at commit time.
- Commit: `feat(host): RawEdge wire shape + parent_id consistency check (B.3)`.

### Task 4 — Python plugin: emit `contains` edges + `parent_id` on entities

Files:
- Modify: `plugins/python/src/clarion_plugin_python/extractor.py` (add `RawEdge` TypedDict; change `extract()` return to `tuple[list[RawEntity], list[RawEdge]]`; per-kind builders return `tuple[RawEntity, str]`; `_walk` accumulates dual lists; module entity `parent_id` is None — module is the root within-file).
- Modify: `plugins/python/src/clarion_plugin_python/server.py` (update `handle_analyze_file` to pass both entities and edges through to the response).
- Modify: `plugins/python/tests/test_extractor.py` (new tests for edge emission and parent_id).

Steps:
- Failing test: `test_module_emits_no_parent_id` — module entities have no parent_id (NotRequired absent).
- Failing test: `test_top_level_function_has_module_parent_id_and_contains_edge` — function at module level has parent_id = module's id AND there's a contains edge from module → function with that pair.
- Failing test: `test_class_method_has_class_parent_id_and_contains_edge`.
- Failing test: `test_nested_class_emits_two_contains_edges` — `class A: class B: pass` produces `(contains, A, A.B)` AND `(contains, module, A)`.
- Failing test: `test_function_in_function_emits_contains_edge_with_locals_qualname` — `def f(): def g()` produces `(contains, f, f.<locals>.g)`.
- Failing test: `test_class_in_function_emits_contains_edge` — `def f(): class C: pass` produces `(contains, f, f.<locals>.C)`.
- Implement: add `RawEdge` TypedDict; refactor `_build_function_entity` and `_build_class_entity` to return `tuple[RawEntity, str]`; refactor `_walk` to dual-accumulator; add parent-id propagation.
- Verify all tests pass.
- Commit: `feat(wp3): emit contains edges + parent_id (B.3 Q1-Q3)`.

### Task 5 — Python plugin: ontology lockstep bump

Files:
- Modify: `plugins/python/plugin.toml` (`[ontology].edge_kinds = ["contains"]`; `[ontology].ontology_version = "0.3.0"`).
- Modify: `plugins/python/src/clarion_plugin_python/server.py` (`ONTOLOGY_VERSION = "0.3.0"`).
- Modify: `plugins/python/src/clarion_plugin_python/__init__.py` (`__version__ = "0.1.2"`).
- Modify: `plugins/python/tests/test_server.py` and `plugins/python/tests/test_package.py` (update version-string assertions).

Steps:
- Update version literals.
- Run pytest; existing tests should pick up new ONTOLOGY_VERSION via the constant; literal-asserting tests need updating.
- Commit: `feat(wp3): ontology v0.3.0 — edge_kinds += contains (B.3)`.

### Task 6 — Cross-language fixture parity for edges

Files:
- Modify: `fixtures/entity_id.json` — extend with edge fixtures (top-level array gains an `edges` section, OR a separate `fixtures/edge_id.json` is added; QA panel suggested extending the existing file with a parallel top-level array).

Steps:
- Add 3-5 edge fixture rows: `(contains, python:module:demo, python:function:demo.hello)`, `(contains, python:class:demo.Foo, python:function:demo.Foo.bar)`, `(contains, python:function:demo.f, python:class:demo.f.<locals>.C)`, etc.
- Add Rust + Python parity tests that consume the fixture.
- Verify cross-language byte-for-byte parity (edges have no plugin-side id derivation — natural key is `(kind, from_id, to_id)` — so parity is structural).
- Commit: `test(wp3): cross-language fixture parity for contains edges (B.3)`.

### Task 7 — Round-trip self-test extension

Files:
- Modify: `plugins/python/tests/test_round_trip.py` (assert contains edges are present in the response; assert parent_id matches contains edges).

Steps:
- Failing test: round-trip self-analysis returns `extracted["edges"]` non-empty; every entity with `parent_id` has a matching contains edge.
- Verify the binary picks up Task 4 changes (editable install).
- Commit: `test(wp3): round-trip asserts contains edges + parent_id (B.3)`.

### Task 8 — Walking-skeleton e2e extension

Files:
- Modify: `tests/e2e/sprint_1_walking_skeleton.sh` (assert at least one contains edge row; assert `dropped_edges_total == 0`).

Steps:
- Update sqlite query to also assert on edges count.
- Run e2e; verify pass.
- Commit: `test(wp3): walking skeleton asserts contains edge persistence (B.3)`.

### Task 9 — Documentation lock + close

Files:
- Modify: `docs/implementation/sprint-1/wp3-python-plugin.md` (forward-pointer to B.3 design).
- Verify all ADR-023 gates green on the closing commit.
- Commit: `docs(wp3): forward-pointer Sprint-1 → B.3 + close design`.

## 8. Filigree umbrella + tracking

A single B.3 umbrella issue is filed (parallel to B.2's `clarion-daa9b13ce2`) with labels `sprint:2`, `wp:3`, `release:v0.1`, `tier:b`. Sub-tasks (Tasks 1–9 above) live as inline checklist items rather than separate filigree issues.

## 9. Exit criteria

B.3 is done for Sprint 2 when ALL of:

- Every analyzed `.py` file produces at least one `contains` edge for non-trivial files (verified by walking-skeleton e2e + unit + round-trip self-test).
- Every function/class entity carries `parent_id`; every module entity does not.
- `parent_id` and matching `contains` edge are consistent for every entity (writer-actor enforced; mismatch rejects run).
- `plugin.toml::edge_kinds == ["contains"]`; `ontology_version == "0.3.0"`; `server.py::ONTOLOGY_VERSION == "0.3.0"`.
- Cross-language fixture parity passes on Rust + Python sides.
- Walking-skeleton e2e PASSES with `dropped_edges_total == 0`.
- All ADR-023 gates green on the closing commit.
- ADR-026 + ADR-027 in Accepted state, indexed in `docs/clarion/adr/README.md`.

## 10. Open questions for the implementation phase (lower stakes)

- **Module entity's parent_id**: the plan above says module entities have NotRequired-absent `parent_id`. Alternative: emit `parent_id` pointing at the (core-emitted, future) `file` entity. **Recommendation**: defer — the `file` entity comes from core-side discovery (ADR-022:52), B.3 is plugin-only work, and the plugin doesn't know the file entity id. The plugin can populate `parent_id = None` for modules in B.3; a later sprint that introduces the `file_list` RPC (ADR-022:52) can revisit.
- **Edge fixture file location**: extend `fixtures/entity_id.json` with a top-level `edges:` array (single-file model), or split into `fixtures/edge_id.json` (separate file)? **Recommendation**: extend the existing file (QA panel preference) — keeps cross-language fixture in one place, easier to spot drift.
- **`cargo deny` / `cargo doc` warnings on the new `WITHOUT ROWID` clause**: SQLite-specific; verify no clippy/doc warnings fire on the migration test. **Recommendation**: handle in implementation; not a design decision.
- **CI lint guard for ontology_version drift**: filed as `clarion-8befae708b` (P3 follow-up from B.2). NOT part of B.3.

## 11. Panel-review record

Six design questions taken to a five-reviewer panel (systems-thinker, solution-architect, architecture-critic, Python/Rust engineer, quality-engineer) before being locked here. Panel verdicts:

| Q | Decision | Vote pattern | Reconciliation |
|---|---|---|---|
| Q1 | (a) top-level `edges` field on `AnalyzeFileResult` | 5×(a) high confidence | unanimous; cross-file resolution explicitly documented in ADR-026 |
| Q2 | (a) dual-encoding parent_id + contains edge WITH writer-actor enforcement | 3×(a), 2×(b) split | reconciled to (a)+enforcement per architecture-critic's `CLA-INFRA-PARENT-CONTAINS-MISMATCH` requirement |
| Q3 | (a) all immediate-parent containments | 5×(a) high confidence | unanimous; non-goal locked: "emitter is exhaustive; renderer-side filtering owns presentation" |
| Q4 | (c) drop `edges.id` column | 2×(b), 2×(c), 1×(b)+ADR | reconciled to (c) per architecture-critic + Python/Rust engineer concrete byte-cost arguments + no-current-reader |
| Q5 | (a) None — emit nothing for contains source range | 5×(a) high confidence | unanimous; per-kind contract documented in ADR-026 decision 3 |
| Q6 | mechanical bump per ADR-027 policy | all approve mechanically | semver policy gap closed by ADR-027 |

Cross-cutting concerns absorbed:

- ADR-026 (containment wire + edge identity) created — locks Q1 + Q4 + Q5 in one ADR per solution-architect's recommendation.
- ADR-027 (ontology_version semver) created — closes the semver policy gap architecture-critic + solution-architect both flagged.
- Dropped-edge counter (QA-engineer recurring): in §6 + Task 2 + e2e assertion in Task 8.
- Writer batch counter rename (Python/Rust engineer): in Task 2.
- Parent-id reconstruction risk (Python/Rust engineer): per-kind builders return `tuple[RawEntity, str]` so `_walk` reuses the entity-id string; in Task 4.

Reviewer transcripts and verbatim verdicts are in the brainstorming conversation log on the `sprint-2/b3-design` branch.
