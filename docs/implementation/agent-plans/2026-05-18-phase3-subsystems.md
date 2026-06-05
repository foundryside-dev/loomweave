# Phase 3 Subsystems - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILLS for implementation: use
> `superpowers:executing-plans` or `superpowers:subagent-driven-development`,
> then use `superpowers:test-driven-development` and
> `superpowers:verification-before-completion` for every task. This plan is
> the Phase 1 review artifact; do not start implementation until the human
> approves it in writing.

**Goal:** Add WP4 Phase 3 subsystem clustering to `loomweave analyze`, satisfying
`REQ-CATALOG-05`, `REQ-ANALYZE-01`, and ADR-006: module-level clustering over
`imports` plus `calls`, one `core:subsystem:{cluster_hash}` entity per cluster,
`in_subsystem` edges from member modules, run stats, the weak-modularity fact,
MCP membership lookup, determinism coverage, and operator documentation.

**Architecture:** `loomweave analyze` remains the single write path. Phase 3 plugs
in after plugin entities/edges are inserted and before `CommitRun`. Storage
gets read helpers that aggregate module-level dependency edges from the
persisted graph. A new CLI-side clustering module turns that graph into
subsystem entity/edge write commands. MCP reads the resulting graph; it does
not run clustering itself.

**Tech Stack:** Rust 2024 workspace, rusqlite, serde/serde_json,
serde_norway, sha2 for ADR-006 subsystem hashes, a graph-clustering crate
candidate qualified in Task 1, Python 3.11 AST extraction for import edges,
pytest, mypy, ruff, cargo nextest, cargo-deny, and existing shell E2E scripts.

**Spec:** ADR-006, ADR-003, ADR-022, `docs/loomweave/1.0/requirements.md`,
`docs/loomweave/1.0/system-design.md`, and the Phase 3 handoff
`docs/superpowers/handoffs/2026-05-18-phase3-subsystems-handoff.md`.

**Filigree umbrella:** `clarion-1dfeebfa36` (P1 work_package;
`release:v0.1`, `sprint:3`, `wp:4`, `adr:006`, `tier:a`). Current status:
`executing`, assigned to `codex`.

---

## Current-State Audit

1. **Schema accepts subsystem and in_subsystem without a new migration.**
   `entities.kind` is plugin-extensible and has no CHECK constraint
   (`crates/loomweave-storage/migrations/0001_initial_schema.sql:30-37`).
   `edges.kind` is also plugin-extensible with writer-side enforcement rather
   than a CHECK (`:80-85`). The migration header still documents the ADR-024
   edit-in-place policy and the 2026-05-18 CHECK edits (`:1-16`). The existing
   schema can store `subsystem` entities and `in_subsystem` edges.

2. **Structural ingest ends in `analyze::run` after plugin edges are inserted.**
   The per-plugin loop collects accepted entities, edges, unresolved sites, and
   stats (`crates/loomweave-cli/src/analyze.rs:216-405`). Entities are inserted
   before unresolved sites, then edges, so edge foreign keys resolve at insert
   time (`:322-396`). Run-commit outcome handling begins immediately after the
   plugin loop (`:407-424`). Phase 3 belongs between those two blocks.

3. **Subsystem entities can use the existing writer path.** `EntityRecord`
   already supports `source_* = None`, `content_hash = None`, and arbitrary
   `properties_json` (`crates/loomweave-storage/src/commands.rs:48-70`), and
   `WriterCmd::InsertEntity` is generic (`:112-174`). `writer.rs` inserts the
   generic record after `BeginRun` (`crates/loomweave-storage/src/writer.rs:332-385`).
   No `InsertSubsystem` command is required. A new `InsertFinding` command is
   required for the weak-modularity fact because findings currently have a
   table but no writer command.

4. **Module-level dependency data is incomplete today.** Module entities and
   `contains` parent edges exist. `calls` and `references` are emitted at
   function/entity level. There is no storage helper that aggregates those
   edges to module-to-module dependencies (`crates/loomweave-storage/src/query.rs`
   currently has call, reference, and contains helpers but no subsystem graph
   helper). The Python plugin manifest declares `contains`, `calls`, and
   `references` only (`plugins/python/plugin.toml:31-46`), and the extractor
   docs still say import emission is future scope
   (`plugins/python/src/loomweave_plugin_python/extractor.py:1-44`). Phase 3
   therefore needs Python `imports` edge emission plus a storage aggregation
   helper for both `imports` and function-level `calls`.

5. **Entity ID shape is already valid.** `loomweave_core::entity_id` accepts
   `plugin_id = "core"`, `kind = "subsystem"`, and a 12-hex canonical name;
   an existing unit test already covers `core:subsystem:a1b2c3d4`
   (`crates/loomweave-core/src/entity_id.rs:90-105`, `:192-194`). Verification
   run during planning:

   ```bash
   cargo test -p loomweave-core core_reserved_subsystem_kind -- --nocapture
   cargo test -p loomweave-core shared_ -- --nocapture
   plugins/python/.venv/bin/pytest \
     plugins/python/tests/test_entity_id.py::test_matches_shared_fixture \
     plugins/python/tests/test_entity_id.py::test_matches_shared_contains_edge_fixture \
     plugins/python/tests/test_entity_id.py::test_matches_shared_calls_edge_fixture -q
   ```

   Results: all selected tests passed.

---

## Design Decisions

### 1. Leiden source

**Recommendation:** adopt a maintained crate for the default Leiden pass, with
a small adapter layer and a dependency-qualification gate in Task 1. The first
candidate is `xgraph` because current docs explicitly advertise directed and
undirected graph support, deterministic/randomized execution, seed support,
and modularity-oriented Leiden clustering. Post-landing ADR-032 names the local
fallback `weighted_components` because the implementation is deterministic BFS
over high-weight edges, not Louvain modularity optimisation.

Planning-time crate check, 2026-05-18:

- `xgraph` docs: directed/undirected graph algorithms, Leiden clustering,
  deterministic mode, and `seed` in `CommunityConfig`
  (https://docs.rs/xgraph/latest/xgraph/ and
  https://docs.rs/xgraph/latest/src/xgraph/graph/algorithms/leiden_clustering.rs.html).
- `leiden-rs` docs: maintained Leiden implementation, modularity quality
  functions, and a `from_gryf_directed` adapter, but the docs snippet did not
  establish a seeded deterministic API clearly enough for ADR-006 on its own
  (https://docs.rs/leiden-rs/latest/leiden_rs/).
- `single-clustering` docs: Leiden, Louvain, `seed: Some(42)`, and modularity,
  but Louvain is documented as work-in-progress and directed graph semantics
  were not explicit (https://docs.rs/crate/single-clustering/0.6.1).
- `sdivi_detection` docs: deterministic native Leiden with modularity/CPM, but
  no Louvain fallback surface was evident in the docs
  (https://docs.rs/sdivi-detection/latest/sdivi_detection/).

Task 1 is a hard gate: if the chosen crate fails cargo-deny, does not compile
on the pinned Rust toolchain, or cannot preserve directed weighted semantics
with a fixed seed in tests, stop and amend this plan before Task 2.

### 2. in_subsystem edge placement

**Recommendation:** keep `in_subsystem` structural and core-owned. The writer
already lists it in `STRUCTURAL_EDGE_KINDS`
(`crates/loomweave-storage/src/writer.rs:450-457`) and rejects source ranges on
structural edges (`:478-502`). The Python plugin must not declare or emit it;
the core clustering module emits it after the subsystem entity is inserted.

Direction: `member_module -> subsystem`. The MCP membership helper queries
`edges.kind = 'in_subsystem' AND edges.to_id = subsystem_id`.

### 3. MCP surface

**Recommendation:** add a new `subsystem_members(id)` MCP tool rather than
folding the behavior into `neighborhood`. Requirements already name
`subsystem_members` (`docs/loomweave/1.0/requirements.md:365-371`), and a
separate tool keeps the response schema narrow: subsystem metadata plus an
ordered member list. `neighborhood` can later include subsystem links
additively, but that should not be the only way to inspect a subsystem.

Also add a policy branch to `summary(id)` for `kind = 'subsystem'`: return the
existing non-LLM policy/error envelope before content-hash lookup or provider
dispatch. Subsystem summarization remains out of scope.

### 4. Empty-input handling

**Recommendation:** emit zero subsystems, no finding, and explicit run stats:

```json
{
  "clustering": {
    "enabled": true,
    "algorithm": "leiden",
    "status": "skipped",
    "skipped_reason": "no_module_dependency_edges",
    "module_count": 1,
    "module_edge_count": 0,
    "subsystem_count": 0,
    "modularity_score": null
  }
}
```

Do not create `LMWV-FACT-CLUSTERING-NO-INPUT`; the handoff explicitly limits
Phase 7 scope to the weak-modularity finding. Single-module and no-plugin
fixtures should remain quiet but inspectable through `runs.stats`.

### 5. runs.stats shape

**Recommendation:** add a single additive `clustering` object inside the
existing JSON stats blob. Existing top-level counters stay untouched.

Successful run example:

```json
{
  "entities_inserted": 120,
  "edges_inserted": 340,
  "dropped_edges_total": 0,
  "ambiguous_edges_total": 0,
  "clustering": {
    "enabled": true,
    "algorithm": "leiden",
    "status": "completed",
    "seed": 42,
    "resolution": 1.0,
    "max_iterations": 100,
    "min_cluster_size": 3,
    "edge_types": ["imports", "calls"],
    "weight_by": "reference_count",
    "module_count": 18,
    "module_edge_count": 44,
    "subsystem_count": 3,
    "modularity_score": 0.417321,
    "duration_ms": 37,
    "weak_modularity_finding_emitted": false,
    "skipped_reason": null
  }
}
```

Soft-failed runs that reach Phase 3 should include the same `clustering`
object. Hard failures before Phase 3 keep the existing failure stats.

### 6. Migration policy

**Recommendation:** no schema migration. Use ADR-024 edit-in-place only if a
test fixture needs an index or helper view added; the current implementation
should not need that. `entities.kind` and `edges.kind` are already open by
policy, and `findings.kind`/`severity` already accept `fact` and `INFO`
(`crates/loomweave-storage/migrations/0001_initial_schema.sql:103-136`).

### 7. Weak-modularity finding anchor

**Recommendation:** persist `LMWV-FACT-CLUSTERING-WEAK-MODULARITY` as a `fact`
with severity `INFO`, anchored to the largest emitted subsystem entity. Put all
subsystem IDs in `related_entities` and include `run_id`, `modularity_score`,
`threshold`, and `algorithm` in `properties`.

Reason: the current findings schema requires `entity_id NOT NULL`
(`crates/loomweave-storage/migrations/0001_initial_schema.sql:119`). Making
run-level findings nullable or inventing a `core:run:*` entity is a schema and
ontology change outside this handoff. If modularity is unavailable because
there are no emitted subsystems, record the skip reason in `runs.stats` and do
not emit a finding.

---

## File Map

| File | Role | Tasks |
|---|---|---|
| `Cargo.toml` | Add graph/hash dependencies after Task 1 qualification | 1 |
| `Cargo.lock` | Lock dependency graph | 1 |
| `crates/loomweave-cli/src/main.rs` | Pass analyze config path into `analyze::run` | 2 |
| `crates/loomweave-cli/src/cli.rs` | Add `loomweave analyze --config <path>` | 2 |
| `crates/loomweave-cli/src/config.rs` | New analyze config parser and defaults for `analysis.clustering` | 2 |
| `crates/loomweave-cli/src/analyze.rs` | Accept config, persist config JSON, filter candidate `imports` before writer insertion, call Phase 3 before commit, merge clustering stats | 2, 3, 5 |
| `crates/loomweave-cli/src/clustering.rs` | New Phase 3 graph/algorithm/subsystem writer orchestration | 1, 4, 5 |
| `crates/loomweave-cli/Cargo.toml` | Wire dependencies used by config/clustering | 1, 2 |
| `crates/loomweave-cli/tests/analyze.rs` | Config/run-stats/import-filter/Phase-3 integration tests | 2, 3, 5 |
| `plugins/python/plugin.toml` | Add `imports`; bump plugin and ontology versions | 3 |
| `plugins/python/src/loomweave_plugin_python/__init__.py` | Bump package version | 3 |
| `plugins/python/src/loomweave_plugin_python/server.py` | Bump ontology constant; pass import resolver inputs if needed | 3 |
| `plugins/python/src/loomweave_plugin_python/extractor.py` | Extract AST import sites and emit candidate `imports` edges | 3 |
| `plugins/python/tests/test_extractor.py` | Import-edge unit coverage | 3 |
| `plugins/python/tests/test_round_trip.py` | Manifest/round-trip coverage for `imports` | 3 |
| `fixtures/entity_id.json` | Shared import-edge fixture rows if parity helpers grow | 3 |
| `crates/loomweave-core/src/entity_id.rs` | Optional shared fixture parity for `imports` edge shape | 3 |
| `crates/loomweave-storage/src/query.rs` | Module dependency graph and subsystem membership helpers | 4, 6 |
| `crates/loomweave-storage/src/lib.rs` | Re-export new query structs/helpers if needed | 4, 6 |
| `crates/loomweave-storage/src/commands.rs` | Add `FindingRecord` and `WriterCmd::InsertFinding` | 5 |
| `crates/loomweave-storage/src/writer.rs` | Persist findings through writer actor | 5 |
| `crates/loomweave-storage/tests/writer_actor.rs` | Finding writer and subsystem edge contract tests | 5 |
| `crates/loomweave-storage/tests/schema_apply.rs` | Guard existing open vocabulary assumptions | 4, 5 |
| `crates/loomweave-mcp/src/lib.rs` | Add `subsystem_members`; summary policy branch for subsystems | 6 |
| `crates/loomweave-mcp/tests/storage_tools.rs` | MCP tool list, membership response, summary no-LLM tests | 6 |
| `tests/e2e/phase3_subsystems.sh` | New multi-module subsystem E2E and determinism check | 7 |
| `tests/e2e/sprint_1_walking_skeleton.sh` | Verify unchanged; only edit if necessary to keep current assertions honest | 7 |
| `tests/fixtures/phase3_subsystems/` | New multi-module Python fixture, if the E2E does not build it inline | 7 |
| `docs/operator/clustering.md` | One-page operator guide for clustering config/results | 7 |
| `docs/superpowers/plans/2026-05-18-phase3-subsystems.md` | Track task checkboxes during Phase 2 | 1-7 |

---

## Task 1: Dependency Qualification and Algorithm Adapter Skeleton

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/loomweave-cli/Cargo.toml`
- Add: `crates/loomweave-cli/src/clustering.rs`
- Modify: `crates/loomweave-cli/src/main.rs` only to `mod clustering;`

**Scope:** Qualify the graph crate and create a tiny adapter boundary before
any analyze integration. This task does not write database rows.

- [x] Write failing unit tests in `clustering.rs`:
  - `fixed_seed_leiden_is_byte_stable`
  - `directed_weighted_edges_affect_partition`
  - `weighted_components_fallback_is_config_selectable`
  - `cluster_hash_uses_sha256_sorted_member_ids_truncated_to_12`
- [x] Add `sha2 = "0.10"` and the selected graph crate to workspace deps.
- [x] Implement:
  - `ClusterAlgorithm::{Leiden, WeightedComponents}`
  - `ModuleGraph { modules, edges }`
  - `ClusterConfig { algorithm, seed, resolution, max_iterations, min_cluster_size }`
  - `ClusterResult { communities, modularity_score, algorithm_used }`
  - `cluster_modules(&ModuleGraph, &ClusterConfig) -> anyhow::Result<ClusterResult>`
  - `cluster_hash(member_ids: &[String]) -> String`
- [x] Run qualification gates:

```bash
cargo test -p loomweave-cli clustering -- --nocapture
cargo deny check
cargo tree -p loomweave-cli
```

**Exit:** The tests prove fixed-seed determinism, directed weighted behavior,
config-selectable weighted-components fallback, and ADR-006 hash shape. `cargo deny check` passes
without broadening `deny.toml`. If not, stop and amend this plan.

**Commit:** `feat(wp4): qualify clustering adapter (phase3 task 1)`

---

## Task 2: Analyze Config Plumbing

**Files:**
- Modify: `crates/loomweave-cli/src/cli.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Add: `crates/loomweave-cli/src/config.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/tests/analyze.rs`

**Scope:** Give `loomweave analyze` the `analysis.clustering` configuration
surface and persist the resolved config in `runs.config`. No subsystem rows yet.

- [x] Write failing tests:
  - `analyze_default_config_records_clustering_defaults`
  - `analyze_config_file_overrides_clustering_seed_and_algorithm`
  - `analyze_rejects_invalid_clustering_algorithm`
- [x] Add `AnalyzeConfig` with default:

```yaml
analysis:
  clustering:
    enabled: true
    algorithm: leiden
    seed: 42
    resolution: 1.0
    max_iterations: 100
    min_cluster_size: 3
    edge_types: ["imports", "calls"]
    weight_by: reference_count
```

- [x] Add `loomweave analyze --config <path>`, defaulting to
  `<project-root>/loomweave.yaml` if present, otherwise defaults.
- [x] Change `analyze::run(path)` to accept an options/config value while
  preserving tests that call the default path.
- [x] Replace the current `BeginRun.config_json = "{}"` with the resolved
  analyze config JSON.

**Exit:** Focused tests pass, invalid config fails before `BeginRun`, and
existing no-plugin/skipped tests still pass.

```bash
cargo test -p loomweave-cli analyze_default_config_records_clustering_defaults -- --nocapture
cargo test -p loomweave-cli analyze_config_file_overrides_clustering_seed_and_algorithm -- --nocapture
cargo test -p loomweave-cli analyze_rejects_invalid_clustering_algorithm -- --nocapture
cargo test -p loomweave-cli analyze_without_plugins_writes_skipped_run_row -- --nocapture
```

**Commit:** `feat(wp4): add analyze clustering config (phase3 task 2)`

---

## Task 3: Python imports Edge Emission

**Files:**
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/tests/analyze.rs`
- Modify: `plugins/python/plugin.toml`
- Modify: `plugins/python/src/loomweave_plugin_python/__init__.py`
- Modify: `plugins/python/src/loomweave_plugin_python/server.py`
- Modify: `plugins/python/src/loomweave_plugin_python/extractor.py`
- Modify: `plugins/python/tests/test_extractor.py`
- Modify: `plugins/python/tests/test_round_trip.py`
- Modify: `fixtures/entity_id.json` if shared parity is extended
- Modify: `crates/loomweave-core/src/entity_id.rs` if shared parity is extended

**Scope:** Emit anchored `imports` candidate edges from module entities. The
host filters candidate imports whose `to_id` module is not present in the same
analysis batch before writer insertion, so external imports do not trip edge
foreign keys.

- [x] Write failing Python tests:
  - `test_import_statement_emits_module_import_edge`
  - `test_from_import_emits_import_edge_to_parent_module`
  - `test_relative_import_emits_package_relative_module_edge`
  - `test_import_edges_have_source_byte_range_and_resolved_confidence`
- [x] Write failing Rust host test:
  - `analyze_filters_external_import_edges_before_writer_insert`
- [x] Add `ImportsEdgeProperties` with at least:
  - `imported_name`
  - `import_style` (`import` or `from_import`)
  - `level` for relative imports
- [x] Add an AST import-site collector. Source range comes from the AST node's
  byte offsets using the same source-buffer convention as calls/references.
- [x] Emit edges from current module entity to `python:module:{target}`.
- [x] In `run_plugin_blocking`, after all files in a plugin batch have been
  analyzed and before returning `collected_edges` to the async writer-insert
  path, filter `imports` edges to targets present in the batch's accepted
  module entity IDs. Preserve internal imports; drop external or unresolved
  imports with an additive `imports_skipped_external_total` counter in
  `BatchStats`/`runs.stats` rather than allowing SQLite FK failure during
  `WriterCmd::InsertEdge`.
- [x] Add `imports` to plugin manifest `edge_kinds`, bump ontology version and
  package patch version.
- [x] Add or update shared edge parity fixture only if the current fixture
  structure supports a fourth edge family cleanly. Not extended in this slice;
  extractor and host-filter regressions cover the new edge family directly.

**Exit:** Python unit tests, strict typing, linting, and round trip pass. The
walking skeleton still passes because a single file has no internal import
target after host filtering.

```bash
plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py -q
plugins/python/.venv/bin/pytest plugins/python/tests/test_round_trip.py -q
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
cargo test -p loomweave-cli analyze_filters_external_import_edges_before_writer_insert -- --nocapture
```

**Commit:** `feat(wp3): emit python imports edges (phase3 task 3)`

---

## Task 4: Storage Query Helpers for the Module Graph

**Files:**
- Modify: `crates/loomweave-storage/src/query.rs`
- Modify: `crates/loomweave-storage/src/lib.rs`
- Modify: `crates/loomweave-storage/tests/schema_apply.rs`
- Add or modify storage query tests in the existing storage test suite

**Scope:** Read the persisted graph into a module-level dependency graph, and
read subsystem memberships. No analyze integration yet.

- [x] Write failing storage tests with seeded SQLite data:
  - `module_dependency_edges_include_imports`
  - `module_dependency_edges_roll_up_function_calls_to_parent_modules`
  - `module_dependency_edges_weight_by_reference_count`
  - `module_dependency_edges_skip_self_edges`
  - `subsystem_members_returns_modules_ordered_by_name`
- [x] Add query structs:

```rust
pub struct ModuleDependencyEdge {
    pub from_module_id: String,
    pub to_module_id: String,
    pub reference_count: u64,
    pub edge_kinds: Vec<String>,
}

pub struct SubsystemMember {
    pub id: String,
    pub name: String,
    pub source_file_path: Option<String>,
}
```

- [x] Implement:
  - `module_dependency_edges(conn, edge_types) -> Result<Vec<ModuleDependencyEdge>>`
  - `subsystem_members(conn, subsystem_id) -> Result<Vec<SubsystemMember>>`
  - `subsystem_for_member(conn, module_id) -> Result<Option<String>>`
- [x] Preserve the existing rule that `imports` and `calls` are anchored edge
  kinds, while `in_subsystem` is structural.

**Exit:** Query tests pass and existing writer/schema tests still pass.

```bash
cargo test -p loomweave-storage module_dependency_edges -- --nocapture
cargo test -p loomweave-storage subsystem_members -- --nocapture
cargo test -p loomweave-storage schema_accepts_open_entity_and_edge_kinds -- --nocapture
```

**Commit:** `feat(storage): add subsystem graph queries (phase3 task 4)`

---

## Task 5: Analyze Phase 3 Writes, Stats, and Weak-Modularity Finding

**Files:**
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/clustering.rs`
- Modify: `crates/loomweave-cli/tests/analyze.rs`
- Modify: `crates/loomweave-storage/src/commands.rs`
- Modify: `crates/loomweave-storage/src/writer.rs`
- Modify: `crates/loomweave-storage/tests/writer_actor.rs`

**Scope:** Insert subsystem entities and `in_subsystem` edges during
`loomweave analyze`, then persist run-level clustering stats and the weak
modularity fact.

- [x] Write failing Rust tests:
  - `analyze_phase3_emits_subsystem_entities_and_edges`
  - `analyze_phase3_is_deterministic_across_two_runs`
  - `analyze_phase3_skips_empty_graph_with_stats`
  - `analyze_phase3_emits_weak_modularity_fact_when_below_threshold`
  - `writer_inserts_fact_findings`
- [x] Add `FindingRecord` and `WriterCmd::InsertFinding`.
- [x] Implement writer insertion for `findings`, including JSON fields as
  serialized strings and `status = 'open'`.
- [x] Add `Phase3Output`:
  - `subsystems_inserted`
  - `in_subsystem_edges_inserted`
  - `clustering_stats`
  - `weak_modularity_finding`
- [x] In `analyze::run`, after plugin insertion and before outcome commit:
  - read module dependency graph
  - cluster if enabled
  - insert subsystem entities
  - insert `in_subsystem` edges
  - insert weak-modularity finding if applicable
  - merge clustering stats into completed or soft-failed stats JSON
- [x] Subsystem entity properties:

```json
{
  "algorithm": "leiden",
  "seed": 42,
  "resolution": 1.0,
  "max_iterations": 100,
  "modularity_score": 0.417321,
  "cluster_hash": "abc123def456",
  "member_module_ids": ["python:module:a", "python:module:b"],
  "member_count": 2,
  "edge_types": ["imports", "calls"],
  "weight_by": "reference_count"
}
```

**Exit:** Focused analyze/storage tests pass. Existing `ambiguous_edges_total`
and skipped/no-plugin stats tests still pass, proving stats additions are
additive.

```bash
cargo test -p loomweave-cli analyze_phase3 -- --nocapture
cargo test -p loomweave-storage writer_inserts_fact_findings -- --nocapture
cargo test -p loomweave-cli analyze_stats_reports_ambiguous_edges_total -- --nocapture
cargo test -p loomweave-cli analyze_without_plugins_writes_skipped_run_row -- --nocapture
```

**Commit:** `feat(wp4): write subsystem clusters in analyze (phase3 task 5)`

---

## Task 6: MCP subsystem_members and Subsystem Summary Policy

**Files:**
- Modify: `crates/loomweave-mcp/src/lib.rs`
- Modify: `crates/loomweave-mcp/tests/storage_tools.rs`
- Modify: `crates/loomweave-storage/src/query.rs` only if Task 4 helpers need
  response-shape refinements

**Scope:** Surface persisted subsystems through MCP without invoking the LLM.

- [x] Write failing MCP tests:
  - `tools_list_includes_subsystem_members`
  - `subsystem_members_returns_member_modules`
  - `subsystem_members_rejects_non_subsystem_id`
  - `summary_on_subsystem_returns_policy_envelope_without_llm_call`
- [x] Add tool definition:

```json
{
  "name": "subsystem_members",
  "description": "List module entities assigned to a subsystem entity.",
  "inputSchema": {
    "type": "object",
    "required": ["id"],
    "properties": {
      "id": { "type": "string" }
    }
  }
}
```

- [x] Response shape:

```json
{
  "subsystem": {
    "id": "core:subsystem:abc123def456",
    "properties": { "member_count": 4, "modularity_score": 0.42 }
  },
  "members": [
    { "id": "python:module:pkg.auth", "name": "pkg.auth", "source_file_path": "pkg/auth.py" }
  ]
}
```

- [x] Add a `summary` early branch for subsystem kind that returns the
  existing policy/error envelope and does not consult cache or provider.

**Exit:** MCP tests pass and the existing seven tools are unchanged except for
the additive eighth tool.

```bash
cargo test -p loomweave-mcp subsystem_members -- --nocapture
cargo test -p loomweave-mcp summary_on_subsystem_returns_policy_envelope_without_llm_call -- --nocapture
cargo test -p loomweave-mcp tools_list -- --nocapture
```

**Commit:** `feat(mcp): expose subsystem membership tool (phase3 task 6)`

---

## Task 7: E2E Fixture, Determinism Gate, Docs, and Performance Measurement

**Files:**
- Add: `tests/e2e/phase3_subsystems.sh`
- Add: `tests/fixtures/phase3_subsystems/` if not generated inline
- Verify or modify: `tests/e2e/sprint_1_walking_skeleton.sh`
- Add: `docs/operator/clustering.md`
- Modify: `docs/superpowers/plans/2026-05-18-phase3-subsystems.md`

**Scope:** Prove the user-facing workflow, document it, and run the performance
gate before declaring Phase 3 ready.

- [x] Write a multi-module Python fixture with at least two dense internal
  dependency groups and sparse cross-group edges.
- [x] Add E2E script:
  - install project
  - run `loomweave analyze`
  - assert at least two `subsystem` rows
  - assert each subsystem has at least `min_cluster_size` module members
  - assert `runs.stats.clustering.status = "completed"`
  - run a second clean analysis and assert subsystem IDs and modularity match
  - run `loomweave serve` fixture interaction or a direct MCP harness call for
    `subsystem_members`
- [x] Add `docs/operator/clustering.md` with:
  - config keys and defaults
  - what subsystem entities contain
  - how to call `subsystem_members`
  - what weak modularity means
  - how empty/single-module input appears in stats
- [x] Performance gate:
  - Use the existing elspeth-slice/full harness from Sprint 2.
  - Capture baseline wall time and RSS from the latest B.8 artifact.
  - Measure Phase 3 wall time separately with tracing or stats.
  - Acceptance: Phase 3 over the elspeth module graph adds less than 60s wall
    time and less than 500 MiB peak RSS over the existing analyze path.
  - If the gate fails, stop with a results memo and propose either weighted-components
    default, graph-pruning config, or ADR amendment.
- [x] Run full verification floor.
- [x] Mark completed tasks in this plan with `[x]`.

**Exit:** E2E, determinism, walking skeleton, MCP membership, docs, and
performance gate are all green.

```bash
bash tests/e2e/phase3_subsystems.sh
bash tests/e2e/sprint_1_walking_skeleton.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python/tests -q
```

**Commit:** `test(wp4): add subsystem e2e and docs (phase3 task 7)`

---

## Whole-Workstream Exit Criteria

- ADR-006 default: Leiden clustering over directed weighted `imports` plus
  `calls` module graph, seeded from config, deterministic.
- Weighted-components fallback is config-selectable and test-covered.
- Subsystems are persisted as `core:subsystem:{sha256(sorted(member_ids))[0..12]}`
  with properties carrying algorithm, seed, resolution, modularity, member IDs,
  edge types, and weight policy.
- `in_subsystem` edges link every member module to its subsystem.
- `runs.stats.clustering` records status, config, counts, modularity, duration,
  and skip/finding state.
- `LMWV-FACT-CLUSTERING-WEAK-MODULARITY` persists when modularity is below 0.3
  and there is an emitted subsystem anchor.
- `subsystem_members(id)` is available through MCP.
- `summary(id)` on a subsystem returns the no-subsystem-summary policy envelope
  and does not spend an LLM call.
- New subsystem E2E and determinism tests pass.
- Sprint-1 walking-skeleton E2E still passes.
- ADR-023 floor passes locally.
- Elspeth-slice performance gate is recorded and within the stated envelope,
  or implementation stops with a memo and ADR/plan amendment instead of
  forcing a red gate through.

---

## Risks and Unknowns

- **Graph crate suitability.** The current Rust graph ecosystem has several
  plausible Leiden crates, but none is a perfect one-line match for
  ADR-006 plus weighted-components fallback. Task 1 is intentionally a hard qualification
  gate.
- **Directed modularity semantics.** If the selected crate silently treats the
  graph as undirected, the adapter test must fail and the plan must be amended.
- **Import resolution precision.** AST import strings do not prove a target
  module exists. Host-side filtering against collected module IDs avoids FK
  failures but means external imports are skipped for Phase 3.
- **Finding anchoring.** Weak modularity is run-level in meaning but entity-
  anchored in the current schema. This plan chooses largest-subsystem anchoring
  to avoid a schema break; if reviewers dislike that, decide before Task 5.
- **Analyze orchestrator size.** `analyze.rs` is already large. Tasks 2 and 5
  should extract small helpers instead of growing the main function blindly.
- **Performance.** B.8 measured healthy analyze behavior on the elspeth slice;
  Phase 3 must be measured as an incremental cost, not assumed cheap.
- **MCP surface drift.** The plan adds exactly one MCP tool. Any further shape
  changes to `neighborhood`, `summary`, or related tools require a plan update
  and review before implementation continues.

---

## Phase 1 Self-Review Checklist

- [x] Current-state audit answers all five handoff questions.
- [x] Design decisions include recommendation, alternatives, and reasons.
- [x] File map lists every planned create/modify target.
- [x] Task breakdown has seven tasks, each sized to a reviewable slice.
- [x] Every task names tests, commands, exit criteria, and commit message.
- [x] Scope exclusions remain respected: no subsystem summarization, no catalog
  artifacts, no HTTP API, no multi-language clustering.
- [x] Plan stops before implementation pending human approval.
