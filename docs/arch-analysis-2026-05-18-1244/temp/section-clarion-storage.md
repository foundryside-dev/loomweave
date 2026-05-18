## clarion-storage

**Location:** `crates/clarion-storage/`

**Responsibility:** Persists Clarion's entity/edge graph, run provenance, and LLM caches in a single SQLite database under `.clarion/clarion.db`. All mutations funnel through a single writer-actor task (sole `rusqlite::Connection`); all reads come from a `deadpool-sqlite` pool. The crate also owns the schema migration runner, the PRAGMA discipline, the edge-contract validator, and the typed query helpers consumed by `clarion-mcp` and `clarion-cli`.

### Internal structure

**Module roster** (`src/lib.rs:7–15`, all `pub mod`):

| Module | LOC | Role |
|---|---|---|
| `writer.rs` | 817 | Writer-actor: spawn, command loop, edge-contract enforcement, per-N batch commits, parent/contains consistency check at `CommitRun` |
| `query.rs` | 569 | Read-side helpers (graph navigation, FTS-or-LIKE search, unresolved call-site fan-out) |
| `cache.rs` | 251 | `SummaryCacheKey` (5-tuple per ADR-007) + `InferredEdgeCacheKey` (4-tuple) and their upsert/lookup/touch helpers |
| `commands.rs` | 183 | `WriterCmd` enum (9 variants) + POD records + `RunStatus` |
| `schema.rs` | 118 | Embed-and-apply migration runner (`include_str!` of the single `.sql` file) |
| `reader.rs` | 88 | `ReaderPool` wrapper around `deadpool-sqlite::Pool` |
| `unresolved.rs` | 50 | Replace-by-caller bookkeeping for unresolved call sites |
| `error.rs` | 48 | `StorageError` taxonomy (11 variants, `thiserror`) |
| `pragma.rs` | 45 | WAL/synchronous=NORMAL/busy_timeout=5000/foreign_keys=ON discipline |
| `lib.rs` | 35 | Curated `pub use` facade |

**Schema (ER summary)** — single migration `migrations/0001_initial_schema.sql` (289 LOC). Eight base tables, one FTS5 virtual table, three triggers, one view, two generated columns:

```
                    ┌─────────────────────────────────────────────┐
                    │  entities  (PK id TEXT)                     │
                    │  + virtual cols scope_level / scope_rank    │
                    │  + indexes on kind, plugin_id, parent_id,   │
                    │    source_file_id, source_file_path,        │
                    │    content_hash, last_seen_commit,          │
                    │    scope_rank (partial), git_churn (partial)│
                    └──┬──────────────────┬────────┬─────────────┘
                       │self-ref          │FK FK   │FK
            parent_id  │  source_file_id  │        │
                       ▼                  ▼        ▼
   ┌──────────────┐  ┌──────────────────────────────────────┐
   │ entity_tags  │  │ edges  WITHOUT ROWID                 │
   │ (entity_id,  │  │ PK (kind, from_id, to_id)            │
   │  tag) PK     │  │ CHECK confidence IN                  │
   │ ON DELETE    │  │   (resolved, ambiguous, inferred)    │
   │ CASCADE      │  │ FKs: from_id, to_id, source_file_id  │
   └──────────────┘  │   ALL ON DELETE CASCADE              │
                    └──────────────────────────────────────┘
                       ▲                  ▲
                       │entity_id FK      │caller_entity_id FK
   ┌─────────────────────┐  ┌────────────────────────────────┐
   │ findings (PK id)    │  │ entity_unresolved_call_sites   │
   │ CHECK kind ∈ 5      │  │ PK (caller_entity_id,          │
   │ CHECK severity ∈ 5  │  │     caller_content_hash,       │
   │ CHECK status ∈ 4    │  │     site_key)                  │
   │ FK entity_id        │  └────────────────────────────────┘
   └─────────────────────┘
                       ▲caller_entity_id FK
   ┌────────────────────────────┐    ┌──────────────────────────┐
   │ inferred_edge_cache         │    │ summary_cache            │
   │ PK (caller_entity_id,       │    │ PK 5-tuple (entity_id,   │
   │     caller_content_hash,    │    │     content_hash,        │
   │     model_id,               │    │     prompt_template_id,  │
   │     prompt_version)         │    │     model_tier,          │
   │ FK caller_entity_id         │    │     guidance_fingerprint)│
   └────────────────────────────┘    │ CHECK stale_semantic∈(0,1)│
                                     └──────────────────────────┘

   ┌──────────────────────────┐   ┌────────────────────────────┐
   │ runs (PK id)             │   │ schema_migrations          │
   │ CHECK status ∈ (running, │   │ (version PK, name,         │
   │   skipped_no_plugins,    │   │  applied_at)               │
   │   completed, failed)     │   └────────────────────────────┘
   └──────────────────────────┘

   FTS5 virtual: entity_fts (entity_id UNINDEXED, name, short_name,
     summary_text, content_text); kept in sync by triggers
     entities_ai / entities_au / entities_ad.

   View: guidance_sheets — projects entities WHERE kind='guidance'
     with json_extract of `properties` + json_group_array of tags.
```

ADR-031 `CHECK` discipline (lines 89, 107–112, 124–125, 153, 200–201): closed core-owned vocabularies receive `CHECK` clauses (`edges.confidence`, `findings.{kind, severity, status}`, `summary_cache.stale_semantic`, `runs.status`). Plugin-extensible vocabularies deliberately omit `CHECK` per ADR-022 — `entities.kind` (`migrations/0001_initial_schema.sql:33–36`) and `edges.kind` (`migrations/0001_initial_schema.sql:77–81`); enforcement at those columns is the writer-actor (`writer.rs::enforce_edge_contract` for edges, manifest acceptance for entity kinds).

**Writer-actor command set** (`commands.rs::WriterCmd`, 9 variants):

| Variant | Lifecycle | Notes |
|---|---|---|
| `BeginRun` | analyze-time | `runs` INSERT with `status='running'`, opens `BEGIN` (`writer.rs:308–330`) |
| `InsertEntity` | analyze-time | Single INSERT into `entities`; counts toward batch boundary (`writer.rs:332–390`) |
| `InsertEdge` | analyze-time | Calls `enforce_edge_contract` (`writer.rs:411–472`) then `INSERT OR IGNORE`; dedupe increments `dropped_edges_total`, ambiguous accepts bump `ambiguous_edges_total` (`writer.rs:474–520`) |
| `InsertInferredEdges` | query-time (MCP) | Upserts inferred-edge cache row, GCs stale inferred edges for the caller, inserts new ones; refuses to shadow static resolved/ambiguous calls (`writer.rs:522–599`) |
| `UpsertSummaryCache` | query-time (MCP) | 5-tuple upsert on `summary_cache` (`cache.rs:48–85`) |
| `TouchSummaryCache` | query-time (MCP) | `UPDATE summary_cache SET last_accessed_at` (`cache.rs:112–132`) |
| `ReplaceUnresolvedCallSitesForCaller` | analyze-time | Delete-then-insert pattern; replaces all sites for one caller atomically inside the run transaction (`writer.rs:601–621`, `unresolved.rs:20–50`) |
| `CommitRun` | analyze-time | Runs the B.3 parent/contains dual-encoding check **inside** the open transaction (`writer.rs:733–796`); on mismatch rolls back the run's writes and marks `runs.status='failed'` with `CLA-INFRA-PARENT-CONTAINS-MISMATCH` in `stats.failure_reason`; on success folds the `runs` UPDATE into the final COMMIT (`writer.rs:671–727`) |
| `FailRun` | analyze-time | ROLLBACK + `UPDATE runs SET status='failed'` (`writer.rs:798–817`) |

The actor multiplexes analyze-time and query-time mutations on the same connection. `query_time_write` (`writer.rs:647–669`) commits any open analyze-time batch, runs the MCP write, and reopens a `BEGIN` if a run is still active — so analyze-time and MCP traffic cannot deadlock or interleave on the same transaction. Batch cadence is `DEFAULT_BATCH_SIZE = 50` writes (`writer.rs:35`, `bump_writes_and_maybe_commit` at `writer.rs:628–645`); the `INSERT OR IGNORE` edge dedupe is workload-shape-invariant because UNIQUE conflicts still bump the batch counter.

Channel-closed cleanup (`writer.rs:251–273`): if the `Writer` is dropped mid-run, the actor self-heals by issuing `ROLLBACK` and marking the surviving run row `failed` with `failure_reason="writer channel closed unexpectedly"`. This is the durability backstop for the supervisor in `clarion-cli::analyze`.

**Edge contract** (`writer.rs::enforce_edge_contract`, line 411). Ontology is hard-coded as `STRUCTURAL_EDGE_KINDS = ["contains", "in_subsystem", "guides", "emits_finding"]` (`writer.rs:394`) and `ANCHORED_EDGE_KINDS = ["calls", "references", "imports", "decorates", "inherits_from"]` (`writer.rs:395–401`) — nine kinds total per ADR-026/028. Structural edges MUST have `confidence=resolved` and NULL `source_byte_*`; anchored edges MUST have both `source_byte_*` set, and may NOT be `inferred` at scan time (`writer.rs:440–449`) because inferred-tier edges are query-time-only. Violations return `StorageError::WriterProtocol` with one of three CLA codes (`CLA-INFRA-EDGE-CONFIDENCE-CONTRACT`, `CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT`, `CLA-INFRA-EDGE-UNKNOWN-KIND`) so the surrounding `runs.stats.failure_reason` carries the code (`writer.rs:402–410`).

**Reader pool** (`reader.rs`). `ReaderPool::open` builds a `deadpool_sqlite::Pool` with `Runtime::Tokio1` and a caller-supplied `max_size` (the CLI passes its own value; tests use small caps). `with_reader` acquires from the pool, submits a `'static` closure to deadpool's `interact()` blocking task pool, and applies read-side PRAGMAs (`busy_timeout=5000`, `foreign_keys=ON`) on every acquisition. Retry-on-`SQLITE_BUSY` is delegated to SQLite itself via `busy_timeout` rather than an application-level loop — both writer and readers wait up to 5 s for the lock. WAL mode (set on the writer's first connection, `pragma.rs:16–31`) is what lets readers proceed concurrently without seeing in-flight writes. `waiting_count()` (`reader.rs:85–87`) is exposed `#[doc(hidden)]` for deterministic test polling.

**Cache keys.** `SummaryCacheKey` (`cache.rs:7–14`) materialises ADR-007's 5-tuple exactly: `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`. `ontology_version` is *not* in the key (correct per ADR-007 — that field is handshake validation only). `InferredEdgeCacheKey` (`cache.rs:30–36`) is a 4-tuple `(caller_entity_id, caller_content_hash, model_id, prompt_version)`. **Boundary clarification**: cache lookup/upsert helpers in `cache.rs` are pure storage operations; on a miss, `clarion-mcp::lib.rs` decides whether to call the LLM (via `clarion-core::LlmProvider`), then enqueues the result via `WriterCmd::UpsertSummaryCache` or `WriterCmd::InsertInferredEdges`. This crate does not depend on `clarion-core::LlmProvider`; its only `clarion-core` dependency is `EdgeConfidence` (`commands.rs:14`, `query.rs:6`).

**Query helpers re-exported via `lib.rs`** (`lib.rs:27–32`): `entity_by_id`, `entity_at_line` (innermost-entity-at-line with tie-break by source-range size then kind preference function→class→module), `find_entities` (FTS5 if the pattern is alnum/underscore; LIKE-with-escape otherwise — see `is_fts_safe` at `query.rs:552`), `call_edges_from` / `call_edges_targeting` (apply ADR-028 confidence ceiling; for `ambiguous` edges, expand the `properties.candidates[]` JSON array into multiple match rows — `query.rs:218–235`, `523–534`), `contained_entity_ids` (iterative DFS over `contains` edges with cycle guard and `max_entities` truncation — `query.rs:354–388`), `unresolved_call_sites_for_caller`, `unresolved_callers_for_target` (LIKE-suffix match on `callee_expr` with same-file preference — `query.rs:294–332`), `candidate_entities_for_unresolved_sites`, `child_entity_ids`, `normalize_source_path` (project-root jail; both lexical normalisation and `canonicalize()` are checked — `query.rs:76–104`).

### External interface

`lib.rs` (35 LOC) re-exports a closed surface: the `WriterCmd`/`EdgeRecord`/`EntityRecord`/`RunStatus` typed boundary; `Writer` and the two channel/batch constants; `ReaderPool`; the query helpers; the cache key types and their three pure helpers (`summary_cache_lookup`, `inferred_edge_cache_lookup`, `inferred_edge_cache_key_id`); `StorageError`/`Result`. Internal modules `pragma` and `schema` are `pub mod`, used by `clarion-cli::install` (`crates/clarion-cli/src/install.rs:20`). `clarion-mcp::lib.rs:22–30` consumes 18 named symbols; `clarion-cli::analyze.rs:24–27` consumes 4 (writer/command shapes only).

### Dependencies

- **Inbound** (verified via `use clarion_storage::` grep):
  - `clarion-mcp` — full read surface + the four query-time `WriterCmd` variants
  - `clarion-cli` — `analyze.rs` (writer + commands), `install.rs` (`pragma` + `schema`), `serve.rs` (`Writer` + `ReaderPool` + batch constants)
- **Outbound** (`Cargo.toml`):
  - `clarion-core` — only for `EdgeConfidence` (used in `commands.rs` + `query.rs`); intentionally minimal
  - `deadpool-sqlite 0.8` — async-friendly read pool (ADR-011)
  - `rusqlite 0.31` — bundled SQLite, sole write driver
  - `tokio` — `mpsc` + `oneshot` channels, `spawn_blocking` for the writer task
  - `serde_json` — JSON shape validation on `InferredCallEdgeRecord.properties_json` + `ambiguous` `candidates[]` decoding
  - `thiserror`, `tracing`

No outbound dependency on `clarion-mcp`, `clarion-cli`, or any plugin crate. Crate-level acyclicity holds.

### Patterns observed

- **Actor + pool split (ADR-011).** Single writer task owns the write connection; all multi-row mutations are batched into a transaction sized by writes (entity inserts + edge insert attempts, including dedupes). The pattern is documented as L3 lock-in (`writer.rs:1–13`).
- **Typed command boundary.** Every mutation is a `WriterCmd` variant carrying its own `oneshot::Sender<Result<T>>` ack — per-command response, no batched fan-in. Adding a new mutation is a single-file append (`commands.rs`) plus a match arm (`writer.rs:152–249`).
- **Defence in depth on closed vocabularies (ADR-031).** Two enforcement layers: the writer-actor (canonical) and SQL `CHECK` (backstop). The migration's per-column comments name which ADR closes each vocabulary; plugin-extensible columns are explicitly tagged "no CHECK by policy."
- **Edge-contract failure codes are findings.** When `enforce_edge_contract` rejects, the error message embeds `CLA-INFRA-EDGE-*` codes that surface in `runs.stats.failure_reason` — making writer-rejected edges observable as machine-greppable findings rather than opaque protocol errors.
- **`query_time_write` interleaves cleanly.** Query-time MCP writes commit the analyze-batch first, then reopen `BEGIN` if a run is still in progress — the actor never holds an MCP cache row open inside an analyze transaction.
- **Validation depth on path inputs.** `normalize_source_path` does lexical normalisation *and* `canonicalize()`, and checks containment against the canonicalized project root in both forms (`query.rs:76–104`). Prevents symlink/`..` escape against `entity_at_line` and `find_entity`.
- **B.3 dual-encoding check at commit.** Parent/contains consistency is verified inside the transaction at `CommitRun` time (`writer.rs:733–796`), so an inconsistent run rolls back rather than persisting a half-corrupt graph.

### Concerns

- **Single migration, edit-in-place under ADR-024.** `migrations/0001_initial_schema.sql` has been edited three times (initial; 2026-05-03 ADR-024 vocabulary rename; 2026-05-18 ADR-031 `CHECK` clauses). The retirement trigger is documented in-file (`0001_initial_schema.sql:10–16`) but no automated check fires when the trigger condition (external operator builds `.clarion/clarion.db` from a published Clarion build) is met. Manual discipline only. Mitigated by the migration's own `schema_migrations` row idempotence (`schema.rs:81–89`).
- **Edge ontology is duplicated.** `STRUCTURAL_EDGE_KINDS` + `ANCHORED_EDGE_KINDS` are hard-coded in `writer.rs:394–401`; ADR-026/028 are the design source; the Python plugin's manifest declares `edge_kinds = ["contains", "calls", "references"]` independently. A new kind requires edits in at least three places (manifest, writer, ADR). No compile-time enforcement that these stay in sync.
- **Schema-shape FK in `entities.source_file_id` is self-referential** (`migrations/0001_initial_schema.sql:40`). Works because source-file entities are inserted before their contained functions/classes (plugin traversal order), but there is no constraint that enforces insertion order. A plugin emitting children before parents would fail with an FK violation, surfacing as an opaque `rusqlite::Error` rather than a writer-protocol error.
- **`busy_timeout=5000` is the only `SQLITE_BUSY` mitigation.** Under heavy contention a reader can fail with a SQLite-level busy error rather than being retried at the application layer. The B.8 scale test exercises this path in practice; no per-attempt retry loop exists in `with_reader`.
- **`InsertEdge` and `InsertEntity` share a single batch counter.** An edge-heavy file (e.g., a module with many `references` edges) can flush the batch boundary mid-file. Documented behaviour (`writer.rs:285–289`) but worth flagging — long transactions are not bounded by file boundary.
- **No write-side throttling on `WriterCmd` channel.** `DEFAULT_CHANNEL_CAPACITY = 256` (`writer.rs:38`); a faster producer than the actor will block via `Sender::send().await` backpressure, which is correct, but no metric is exposed for "time spent blocked on writer queue."

### Confidence

**Confidence:** High — Read 100% of every `src/*.rs` module (10 files, 1 950 LOC) and the migration in full (289 LOC). Cross-validated dependency direction by grepping `use clarion_storage::` across the workspace: only `clarion-mcp` and `clarion-cli` consume it; no inbound cycles. Confirmed `WriterCmd` variant count (9) matches the actor's match arms one-to-one. Schema CHECK constraints verified at exact line numbers against ADR-031's "closed vs. extensible" decision. Edge-contract code-paths (`enforce_edge_contract` and the three CLA codes it emits) read end-to-end. ADR cross-references are inline in both the migration and the writer source, so the "ADR says X / code does Y" gap is small.
