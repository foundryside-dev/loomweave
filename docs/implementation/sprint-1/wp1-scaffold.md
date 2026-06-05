# WP1 — Scaffold + Storage (Sprint 1)

**Status**: DRAFT — pending sprint kickoff
**Anchoring design**: [system-design.md §4 (Storage)](../../loomweave/v0.1/system-design.md#4-storage), [detailed-design.md §3 (Storage impl)](../../loomweave/v0.1/detailed-design.md#3-storage-implementation)
**Accepted ADRs**: [ADR-001](../../loomweave/adr/ADR-001-rust-for-core.md), [ADR-003](../../loomweave/adr/ADR-003-entity-id-scheme.md), [ADR-011](../../loomweave/adr/ADR-011-writer-actor-concurrency.md), [ADR-023](../../loomweave/adr/ADR-023-tooling-baseline.md)
**Backlog ADR that may surface**: ADR-005 (`.loomweave/` git-committable subpaths)
**Predecessor**: none — WP1 is the foundation of Sprint 1.
**Blocks**: WP2, WP3.

---

## 1. Scope (Sprint 1 narrow)

WP1 in Sprint 1 delivers only what the walking skeleton needs. The full WP1 scope
from [`../v0.1-plan.md`](../v0.1-plan.md#wp1--core-scaffold-and-storage-layer) — every
table, every CLI subcommand, `--shadow-db`, stress-tested writer-actor — is deferred
to later sprints where those surfaces are first exercised.

**In scope for Sprint 1:**

- Cargo workspace at repo root.
- SQLite schema covering the full table set from
  [detailed-design.md §3](../../loomweave/v0.1/detailed-design.md#3-storage-implementation):
  tables `entities`, `entity_tags`, `edges`, `findings`, `summary_cache`,
  `runs`, plus the `entity_fts` FTS5 virtual table and its three
  insert/update/delete triggers, the generated-column `ALTER TABLE`
  statements plus partial indexes (`ix_entities_priority`,
  `ix_entities_churn` — note: `ix_entities_priority` was renamed to
  `ix_entities_scope_rank` and the underlying column was split into
  `scope_level` + `scope_rank` by [ADR-024](../../loomweave/adr/ADR-024-guidance-schema-vocabulary.md);
  the L1 lock at sprint close described the pre-rename shape),
  the `guidance_sheets` view, and a `schema_migrations` meta table. The walking skeleton only writes rows into
  `entities` and `runs`, but every other table, virtual table, trigger,
  generated column, and view is created by migration `0001` so its shape is
  frozen. The full schema is locked here (L1) precisely because doing so
  leaves no room for later migrations to be rushed — we write them now,
  under design pressure, not under sprint pressure later. (Note: there is
  no `summaries` table — summary data lives in `summary_cache`. There are
  no `briefings`, `guidance`, or `observations` tables either —
  `guidance_sheets` is a view over `entities WHERE kind='guidance'`;
  briefings and observations live in JSON columns on `entities` /
  `findings`.)
- Writer-actor with command types sufficient for entity insert + run begin/commit.
- Reader-pool with at least one idle connection.
- Migration runner (sequentially numbered SQL files, idempotent apply).
- Entity-ID 3-segment format (L2) per ADR-003 — `{plugin_id}:{kind}:{canonical_qualified_name}`.
  WP1 ships the Rust assembler; WP3 ships the Python-side producer of the
  `canonical_qualified_name` segment.
- `loomweave` binary with two subcommands:
  - `loomweave install` — creates `.loomweave/` (DB + `config.json` + `.gitignore`
    seeded with the run-log exclusion) and writes a stub `loomweave.yaml` at the
    project root per
    [detailed-design.md §File layout](../../loomweave/v0.1/detailed-design.md#file-layout)
    (see L1 note below and ADR-005 trigger in §7).
  - `loomweave analyze <path>` — CLI wiring only. Accepts the path, opens the DB,
    begins a run, but does **not** yet spawn a plugin (that's WP2's wiring). Exits
    cleanly with run status = `skipped_no_plugins`.
- RecordingProvider trait stub in core (no implementation — the file exists so
  WP6 has a hook point; see the v0.1-plan §5.1 primitive).

**Explicitly out of scope for Sprint 1:**

- Plugin spawning, JSON-RPC, anything subprocess — WP2.
- `loomweave serve`, MCP, HTTP — WP8.
- `--shadow-db` flag — deferred; only the default in-place write mode is shipped.
- Checkpoint/resume mid-run — deferred; runs are either complete or failed, no
  mid-run restart.
- `loomweave analyze --no-llm`, `--dry-run` — deferred.
- Findings, clustering, briefings — WP4/WP6/WP7.
- Any multi-platform support — Linux only; macOS and Windows are future work.

## 2. Lock-in callouts

Sprint 1 decides these. Sprint 2+ reads and writes against them.

### L1 — SQLite schema shape

**What locks**: the full table set from [detailed-design.md §3](../../loomweave/v0.1/detailed-design.md#3-storage-implementation),
not just the tables the walking skeleton uses. All tables, columns, types, primary
keys, foreign keys, indexes, and `PRAGMA` settings are written into migration
`0001_initial_schema.sql` and committed. The walking skeleton only writes rows into
`entities` and `runs`, but every table's *shape* is frozen.

**Why now, not later**: changing schema mid-sprint-2 (after WP4 or WP6 are writing
real data) means data-migration work on live analyses. Schema churn is cheap when
there is no data; changing later forces migration scripts that each need their own
tests. Locking the shape now — using the detailed-design as the authoritative source
— pushes all that rework up-front.

**Canonical source**: [detailed-design.md §3](../../loomweave/v0.1/detailed-design.md#3-storage-implementation).
If the detailed-design and the migration file disagree, the migration file wins from
Sprint 1 onward; the detailed-design must be updated to match.

**Downstream impact**:
- WP4 (clustering, findings) reads from `entities`/`edges` and writes to
  `findings`/`subsystems` — locked shape means WP4 can begin without touching WP1.
- WP6 writes to `summary_cache`; its composite primary key (`entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint`) is locked here. `entity_id` is the L2 3-segment string.
- `↗` WP10 — Filigree's `registry_backend: loomweave` reads `entities` via the
  documented schema and joins on the 3-segment `EntityId` (L2). Changing
  column names or the ID shape after Sprint 1 breaks Filigree's
  `RegistryProtocol` impl.

**Note on `.gitignore` selectivity**: the migration file + the DB itself are
git-tracked per ADR-005 (backlog). Run logs, shadow-DB artefacts, and any
`.loomweave/tmp/` subpath are not. `loomweave install` writes the `.gitignore` rules;
this is the point at which ADR-005 stops being backlog and gets authored (see §7
trigger).

### L2 — Entity-ID canonical-name format

**What locks**: the function that assembles an `EntityId` string per
[ADR-003](../../loomweave/adr/ADR-003-entity-id-scheme.md) and
[ADR-022](../../loomweave/adr/ADR-022-core-plugin-ontology.md). The Rust
implementation ships in WP1; WP3 ships the Python-side producer of the
`canonical_qualified_name` component that matches byte-for-byte.

**Format** (authoritative text in ADR-003): three colon-separated segments —
`{plugin_id}:{kind}:{canonical_qualified_name}`.

- `plugin_id` — matches the emitting plugin's manifest `[plugin].name` (e.g.,
  `python` for the WP3 plugin; `core` for core-minted file/subsystem/guidance
  entities per ADR-022).
- `kind` — the entity kind string; must be declared in the plugin manifest's
  `[ontology].entity_kinds` or be one of the three core-reserved kinds
  (`file`, `subsystem`, `guidance`) per ADR-022.
- `canonical_qualified_name` — language-native dotted qualified name rooted at
  the project's canonical module root (per `detailed-design.md §1` Python
  normalisation rules for the WP3 plugin). File path is **not** part of the
  identifier; it lives on the entity record as a property.

Example (WP3's demo function): `python:function:demo.hello` for a
module-level `def hello()` in `demo.py`. Sprint 1's Rust implementation plus
the shared fixture (§Task 5 in WP3) are the executable spec.

**Why now**: this is the cross-sibling identity — every table FK, every finding
locator, every Filigree `registry_backend: loomweave` join (ADR-014), and every
Wardline qualname reconciliation (ADR-018) keys off this string. Changing it
later requires a schema migration *and* coordinated Filigree/Wardline changes.

**Downstream impact**:
- L7 (Python qualname production) produces the `canonical_qualified_name`
  segment; WP1's Rust assembler concatenates `plugin_id`, `kind`, and that
  segment. Shared fixture (WP3 §Task 5) is the byte-for-byte parity proof.
- `↗` WP10 — Filigree's `registry_backend: loomweave` joins on this 3-segment
  ID; `{kind}` is load-bearing because ADR-014's protocol uses the
  core-owned `file` kind as the join surface.
- `↗` ADR-018 — Wardline qualname reconciliation uses the
  `canonical_qualified_name` segment (not the full ID) as its join key on the
  Loomweave side.

### L3 — Writer-actor command protocol

**What locks**: the command message type, the per-N transaction batch
contract, and the ack mechanism per [ADR-011](../../loomweave/adr/ADR-011-writer-actor-concurrency.md).
The writer-actor is a `tokio::task` (not an OS thread) per ADR-011's
Decision section.

**Shape**: a single enum `WriterCmd` with variants for each persistent
operation the core performs. Sprint 1 ships `WriterCmd::BeginRun`,
`WriterCmd::InsertEntity`, `WriterCmd::CommitRun`, and `WriterCmd::FailRun`.
Additional variants (`InsertEdge`, `InsertFinding`, etc.) are added as later
WPs need them, but the *pattern* — command enum + bounded
`tokio::sync::mpsc` channel + `tokio::sync::oneshot` per-command reply +
per-N auto-commit — is locked.

**Per-N batch default**: `N = 50` per ADR-011 §Per-N-files transactions, with
`mpsc` channel bounded at 256 ops (ADR-011's `mpsc::Sender<WriteOp>`
capacity). Configurable via `loomweave.yaml:storage.tx_batch_size` in later
WPs; WP1 ships the ADR-locked default.

**Why now**: every persistence call site in every later WP will follow this
pattern. Changing the pattern later means touching every call site.

**Downstream impact**: all later WPs' persistence is a new `WriterCmd`
variant + a handler arm. The pattern is read-only to them.

## 3. File decomposition

The Cargo workspace layout is a *recommendation*, not a lock-in — the boundary may
shift during WP2 or WP3 if shared types emerge. But a reasonable starting shape:

```
/Cargo.toml                              # workspace root
/crates/
  loomweave-core/                          # library: domain types, traits, stub LlmProvider
    src/
      lib.rs
      entity_id.rs                       # L2 implementation + unit tests
      entity.rs                          # entity, edge, kind types
      llm_provider.rs                    # trait stub for WP6
  loomweave-storage/                       # SQLite layer + writer-actor
    src/
      lib.rs
      schema.rs                          # migration runner, schema-version check
      writer.rs                          # writer-actor (L3)
      reader.rs                          # reader pool
      commands.rs                        # WriterCmd enum (L3)
    migrations/
      0001_initial_schema.sql            # L1 — full schema, committed now
  loomweave-cli/                           # binary entry point
    src/
      main.rs
      install.rs                         # `loomweave install`
      analyze.rs                         # `loomweave analyze` (no-plugin variant)
```

Tests live under each crate's `tests/` directory (integration) or inline `#[cfg(test)]`
modules (unit). A top-level `fixtures/` directory holds shared test fixtures — a
single-file Python fixture (`fixtures/demo.py`) is all Sprint 1 needs.

## 4. External dependencies being locked

Sprint 1 pins a small set of third-party Rust crates in the workspace root
`Cargo.toml`. Choices here become convention — later WPs adding crates should follow
the same error-handling and async conventions.

| Purpose | Candidate | Locks what for later WPs |
|---|---|---|
| Rust edition | **2024** (per ADR-023) | Every crate, every future WP compiles against 2024 |
| SQLite binding | `rusqlite` (bundled SQLite) — per ADR-011 | Error-handling shape (wrapped, not re-exported; see UQ-WP1-06) |
| SQLite read pool | `deadpool-sqlite` — per ADR-011 | Reader acquisition pattern for WP2/WP6/WP8 |
| CLI parsing | `clap` | Subcommand/flag conventions |
| Error handling | `thiserror` (lib) + `anyhow` (bin) | The "library uses typed errors, binary uses anyhow" split |
| Logging | `tracing` + `tracing-subscriber` | Log shape for later serve/analyze output |
| Async runtime | `tokio` (locked by ADR-011) | Writer-actor is a `tokio::task`; WP2 plugin I/O and WP8 HTTP inherit this runtime |
| Test runner | **`cargo-nextest`** (per ADR-023) — `assert_cmd` for CLI + `tokio::test` for async | Integration-test style for later CLI-touching WPs; exit criteria and demo scripts use `cargo nextest run`, not `cargo test` |
| Lint floor | **`clippy::pedantic = "warn"` + `unsafe_code = "forbid"`** via workspace `[lints]` block (per ADR-023) | Every new crate declares `lints.workspace = true`; baseline is a floor that later WPs may tighten but not loosen |
| Formatting | **`rustfmt.toml`** with `edition = "2024"`, `max_width = 100`, Unix newlines (per ADR-023) | CI gates `cargo fmt --all -- --check` on every PR |
| Supply chain | **`cargo-deny`** with `deny.toml` (v2 schema — advisories, license allowlist, bans, unknown-registry deny) (per ADR-023) | License allowlist expansion requires an explicit commit; new deps with unlisted licenses fail the merge gate |
| CI | **GitHub Actions** workflow at `.github/workflows/ci.yml` running fmt-check + pedantic clippy + nextest + cargo-doc + cargo-deny (per ADR-023) | Every subsequent WP's merge gate passes through the same five steps |

**No cross-sibling dependencies in Sprint 1.** Filigree and Wardline do not appear in
`Cargo.toml`. WP3's Wardline import is Python-side only.

## 5. Unresolved questions

Liberal per the sprint directive — questions a reviewer could reasonably ask, even
if they don't block tasks. Each has a proposed resolution-by trigger.

- **UQ-WP1-01** — **SQLite crate choice**: ~~resolved~~ — **ADR-011 locks
  `rusqlite`** (with `deadpool-sqlite` for the reader pool). WP1 uses
  `rusqlite` from Task 1. **Resolved**: Task 1.
- **UQ-WP1-02** — **Async runtime adoption**: ~~proposed sync-in-WP1 then
  port-at-WP2~~ — **resolved by ADR-011**. ADR-011's Decision section locks
  tokio from day one: writer-actor is a `tokio::task` owning the sole write
  `rusqlite::Connection`; readers come from a `deadpool-sqlite` pool. WP1
  adopts tokio from Task 1 to avoid throwaway work. **Resolved**: Task 1.
- **UQ-WP1-03** — **Writer-actor ack granularity**: per-command oneshot ack (caller
  awaits each insert) or per-batch ack (caller gets confirmation when its batch
  commits)? Per-command is simpler; per-batch is more efficient under high entity
  volume. **Proposal**: per-command ack in Sprint 1; optimise later if WP3 or WP4
  hit throughput issues. **Resolved**: Task 6 — per-command oneshot ack; `Writer.commits_observed` `Arc<AtomicUsize>` counts COMMITs issued.
- **UQ-WP1-04** — **What `.gitignore` rules does `loomweave install` seed?** ADR-005
  is backlog; Sprint 1 must decide. **Proposal** (authors ADR-005 as a side effect):
  ignore `.loomweave/tmp/`, `.loomweave/logs/`, `.loomweave/*.shadow.db`, `.loomweave/*.wal`,
  `.loomweave/*.shm`; track `.loomweave/loomweave.db`, `.loomweave/config.json`, and the
  migration history in the DB itself. (`loomweave.yaml` lives at project root and
  is outside `.loomweave/`; its tracking is governed by the user's existing
  repo-root `.gitignore`, not by Loomweave.) **Resolved**: Task 5 — ADR-005 authored and Accepted; `.gitignore` contents shipped in `crates/loomweave-cli/src/install.rs`.
- **UQ-WP1-05** — **Schema for `runs` table**: does Sprint 1's `runs` row include
  plugin-invocation metadata (plugin name, manifest version) or only run-level
  status/timestamps? WP2 will add plugin-invocation metadata; if the schema is
  locked now, we either over-specify (columns WP1 doesn't populate) or plan a
  migration. **Proposal**: lock the full shape from detailed-design §3 now,
  including plugin-invocation columns; WP1 inserts with NULL, WP2 fills them in.
  **Resolved**: Task 3 — full `runs` shape shipped in `0001_initial_schema.sql`; plugin-invocation metadata goes into the `config` JSON column, populated by WP2.
- **UQ-WP1-06** — **Error-type boundary**: does `loomweave-storage` re-export
  `rusqlite::Error`, or wrap it in a crate-local `StorageError` via `thiserror`?
  Re-export leaks the dependency; wrapping adds boilerplate. **Proposal**: wrap;
  the crate boundary is a decoupling point. **Resolved**: Task 3 — `StorageError` wraps `rusqlite::Error` via `thiserror` `#[from]` at the `loomweave-storage` crate boundary.
- **UQ-WP1-07** — **Segment-separator collisions**: ADR-003's 3-segment form
  uses `:` as the segment separator. A segment (any of `plugin_id`, `kind`,
  `canonical_qualified_name`) containing a literal `:` would produce an
  ambiguous ID. `plugin_id` and `kind` are grammar-restricted by ADR-022
  (`[a-z][a-z0-9_]*`) so cannot contain `:`. `canonical_qualified_name` is a
  Python dotted name in Sprint 1 and also cannot contain `:`. **Proposal**:
  Sprint 1 asserts-unreachable on any segment containing `:`; the assertion
  documents the grammar contract. If a future non-Python plugin needs `:` in
  qualified names, introduce escaping via a follow-up ADR amending ADR-003.
  **Resolved**: Task 2 — `EntityIdError::SegmentContainsColon` surfaces any attempted `:` injection; grammar-restricted segments (`plugin_id`, `kind`) cannot produce it in practice.
- **UQ-WP1-08** — **Does `loomweave install` refuse to overwrite an existing
  `.loomweave/`?** **Proposal**: yes, unless `--force`; `--force` is not implemented
  in Sprint 1 but the error message names it for future use. **Resolved**: Task 5 — `loomweave install` refuses an existing `.loomweave/`; `--force` accepted by clap but errors out (Sprint 1 does not implement overwrite).
- **UQ-WP1-09** — **Rust toolchain + workspace tooling baseline**:
  ~~"2021 edition, stable channel. MSRV floats with the latest stable at
  sprint start; fine to document and move on."~~ — **reopened 2026-04-18
  and re-resolved by [ADR-023](../../loomweave/adr/ADR-023-tooling-baseline.md)**.
  The original "move on" framing was the canonical tell for an
  unexamined default. ADR-023 adopts **edition 2024**, workspace-level
  `[lints]` with `clippy::pedantic = "warn"` + `unsafe_code = "forbid"`,
  pinned `rustfmt.toml` + `clippy.toml`, **`cargo-nextest`** as the test
  runner, **`cargo-deny`** for supply-chain hygiene, and **GitHub Actions
  CI** running fmt-check + pedantic clippy + nextest + cargo-doc +
  cargo-deny on every PR. `rust-toolchain.toml` pins `channel = "stable"`
  with `clippy`, `rustfmt`, and `llvm-tools-preview` components. The
  retrofit cost of adopting any of these after real code landed was
  materially higher than adopting them at commit zero — the asymmetry is
  explicit in ADR-023. **Resolved**: Task 1.

## 6. Task ledger

Each task is a discrete test → implement → verify → commit cycle. Tasks are ordered;
do not parallelise within WP1. Commits are one-per-task unless noted.

### Task 1 — Workspace skeleton + tooling baseline (ADR-023)

**Files**:
- Create `/Cargo.toml` (workspace root — `[workspace.package]`, `[workspace.dependencies]`, and `[workspace.lints]` per ADR-023)
- Create `/crates/loomweave-core/{Cargo.toml,src/lib.rs}`
- Create `/crates/loomweave-storage/{Cargo.toml,src/lib.rs}`
- Create `/crates/loomweave-cli/{Cargo.toml,src/main.rs}`
- Create `/rust-toolchain.toml` pinning stable with `clippy` + `rustfmt` + `llvm-tools-preview`
- Create `/rustfmt.toml` (`edition = "2024"`, `max_width = 100`, Unix newlines)
- Create `/clippy.toml` (relaxed pedantic thresholds per ADR-023)
- Create `/deny.toml` (cargo-deny v2 schema — advisories, license allowlist, bans, sources)
- Create `/.github/workflows/ci.yml` (fmt-check + pedantic clippy + nextest + cargo-doc + cargo-deny)
- Create `/.gitignore` entries for `/target`, `*-wal`, `*-shm` (project-level `.loomweave/loomweave.db` is tracked per ADR-005 — do **not** blanket-ignore `*.db`)

Steps:

- [ ] Write workspace `Cargo.toml` listing the three members. `[workspace.package]` pins `edition = "2024"` (ADR-023), license, repository, and `rust-version = "1.85"` (or the current stable MSRV at sprint start). Declare `tokio`, `rusqlite` (with `bundled` feature), `deadpool-sqlite`, `thiserror`, `tracing`, `clap`, `anyhow`, `serde` + `serde_json`, plus the `tempfile` and `assert_cmd` dev-deps as workspace dependencies (ADR-011-locked stack; dev-deps centralised).
- [ ] Add `[workspace.lints.rust]` with `unsafe_code = "forbid"` and `[workspace.lints.clippy]` with `pedantic = "warn"` plus the pragmatic allows from ADR-023 (`module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`).
- [ ] Write each crate's `Cargo.toml`. Every crate declares `lints.workspace = true` so a later-added crate cannot drift off the ADR-023 floor. `loomweave-core` takes `thiserror` + `serde` + `serde_json`. `loomweave-storage` takes core + `rusqlite` + `deadpool-sqlite` + `tokio` (features `rt-multi-thread`, `macros`, `sync`). `loomweave-cli` takes both + `clap` + `anyhow` + `tracing` + `tracing-subscriber` + `tokio` (same features).
- [ ] Write `rust-toolchain.toml` per ADR-023 (channel `stable`; components `clippy`, `rustfmt`, `llvm-tools-preview`; `profile = "minimal"`).
- [ ] Write `rustfmt.toml`, `clippy.toml`, `deny.toml` as specified in ADR-023.
- [ ] Write `.github/workflows/ci.yml` with jobs for fmt-check, pedantic clippy, nextest, cargo-doc, cargo-deny. Use `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, and `taiki-e/install-action` for nextest + cargo-deny installs.
- [ ] Add `lib.rs` / `main.rs` stubs that compile cleanly against pedantic (avoid `pub fn hello()` defaults that trigger `missing_docs_in_private_items` or similar — a crate-level `//!` doc comment + a module-level stub are sufficient).
- [ ] Verify locally (all gates must pass on a clean checkout):
  - `cargo build --workspace`
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo nextest run --all-features` (install via `cargo install cargo-nextest --locked` if not already present)
  - `cargo deny check` (install via `cargo install cargo-deny --locked` if not present)
  - `cargo doc --no-deps --all-features`
- [ ] Commit: `feat(wp1): workspace skeleton + ADR-023 tooling baseline`.

### Task 2 — Entity-ID assembler (L2)

**Files**:
- Create `/crates/loomweave-core/src/entity_id.rs`
- Modify `/crates/loomweave-core/src/lib.rs` to `pub mod entity_id;`

Steps:

- [ ] Write failing unit tests in `entity_id.rs` covering ADR-003's 3-segment format. Start with at least five cases, including: module-level function (`python:function:demo.hello`), class method (`python:function:demo.Foo.bar`), nested function (`python:function:demo.outer.<locals>.inner`), a core-reserved file entity (`core:file:src/demo.py`), and a core-reserved subsystem entity (`core:subsystem:<cluster_hash>`).
- [ ] Run `cargo test -p loomweave-core entity_id`; expect failures referencing the missing `entity_id()` function.
- [ ] Implement `pub fn entity_id(plugin_id: &str, kind: &str, canonical_qualified_name: &str) -> EntityId` (newtype-wrapped `String`) matching ADR-003's three-segment form. Validate each segment against the ADR-022 grammar (`kind` matches `[a-z][a-z0-9_]*`; `plugin_id` matches the same grammar); return a typed error on malformed input. Assert-unreachable on any segment containing a literal `:` (UQ-WP1-07 — segment separator collisions are a bug at the caller).
- [ ] Run `cargo test -p loomweave-core entity_id`; expect all pass.
- [ ] Commit: `feat(wp1): L2 entity-ID assembler per ADR-003 + ADR-022`.

### Task 3 — Schema migration file (L1)

**Files**:
- Create `/crates/loomweave-storage/migrations/0001_initial_schema.sql`
- Create `/crates/loomweave-storage/src/schema.rs`
- Modify `/crates/loomweave-storage/src/lib.rs` to expose the migration runner

Steps:

- [ ] Transcribe the full schema from [detailed-design.md §3](../../loomweave/v0.1/detailed-design.md#3-storage-implementation) into `0001_initial_schema.sql`. Concretely:
  - Tables: `entities`, `entity_tags`, `edges`, `findings`, `summary_cache`, `runs`, and `schema_migrations` (meta). Every column, primary key, foreign key, and explicit index as written in §3.
  - Virtual table: `entity_fts` (FTS5, `tokenize = 'porter unicode61'`).
  - Triggers: `entities_ai`, `entities_au`, `entities_ad` keeping `entity_fts` synchronised with `entities`.
  - Generated columns + indexes: `entities.priority` + `ix_entities_priority`, `entities.git_churn_count` + `ix_entities_churn` (both partial indexes on `IS NOT NULL`). _Post-Sprint-1 note: ADR-024 renamed these to `entities.scope_level` + `entities.scope_rank` + `ix_entities_scope_rank`; the WP1 task as completed shipped the pre-rename shape, which the rename then superseded._
  - View: `guidance_sheets` over `entities WHERE kind = 'guidance'`.
  - `PRAGMA foreign_keys = ON` applied at connection open (not in the migration file itself — PRAGMA is connection-scoped and is set in `reader.rs` / `writer.rs` on each open per ADR-011's connection open list).
  - Do not truncate; every table, trigger, generated column, and view ships in Sprint 1 even though only `entities` and `runs` are written.
- [ ] Write failing integration test at `/crates/loomweave-storage/tests/schema_apply.rs` that opens a fresh DB, runs migrations, and asserts:
  - Every expected table exists via `SELECT name FROM sqlite_master WHERE type='table'`.
  - The FTS5 virtual table `entity_fts` exists and is queryable (`SELECT * FROM entity_fts LIMIT 0`).
  - All three FTS triggers exist via `SELECT name FROM sqlite_master WHERE type='trigger'`.
  - The generated columns round-trip: insert an entity with a `properties` JSON containing `priority`, `SELECT priority FROM entities` returns the extracted value. _Post-Sprint-1 note: per ADR-024 the test now uses `scope_level`/`scope_rank` instead of `priority`._
  - The `guidance_sheets` view exists and is queryable.
  - Idempotency: running migrations twice does not fail.
- [ ] Run `cargo test -p loomweave-storage schema_apply`; expect failures referencing missing `apply_migrations()`.
- [ ] Implement `schema.rs` with `pub fn apply_migrations(conn: &Connection) -> Result<()>` that reads files from `migrations/` in lexical order, skips those already in `schema_migrations`, and records each successful apply.
- [ ] Run the integration test; expect pass.
- [ ] Commit: `feat(wp1): L1 SQLite schema migration framework`.

### Task 4 — Reader pool

**Files**:
- Create `/crates/loomweave-storage/src/reader.rs`
- Modify `/crates/loomweave-storage/src/lib.rs` to export `ReaderPool`

Steps:

- [ ] Write failing `#[tokio::test]` integration test: open a DB, apply migrations, construct `ReaderPool::new(path, max_size=2)`, acquire two readers concurrently via `tokio::join!`, each runs a trivial `SELECT 1` without blocking the other.
- [ ] Run `cargo test -p loomweave-storage reader`; expect failure.
- [ ] Implement `ReaderPool` as a thin wrapper around `deadpool-sqlite` (per ADR-011) with `max_size` default 16 (ADR-011 §Reader pool). Readers are opened with `OpenFlags::SQLITE_OPEN_READ_ONLY` and the ADR-011 PRAGMAs applied on each open via the pool's `Manager::post_create` hook.
- [ ] Run the test; expect pass.
- [ ] Commit: `feat(wp1): reader pool for concurrent read connections`.

### Task 5 — `loomweave install` subcommand

**Files**:
- Create `/crates/loomweave-cli/src/install.rs`
- Modify `/crates/loomweave-cli/src/main.rs` for clap subcommand wiring

Steps:

- [ ] Write failing integration test at `/crates/loomweave-cli/tests/install.rs` using `assert_cmd`: run `loomweave install` in a tempdir; assert `.loomweave/loomweave.db` exists, `.loomweave/config.json` exists with `{"schema_version": 1}`, `.loomweave/.gitignore` exists with expected rules (UQ-WP1-04 resolution), **`<project>/loomweave.yaml`** (project root, not `.loomweave/`; per [detailed-design.md §File layout](../../loomweave/v0.1/detailed-design.md#file-layout) and [system-design.md §Config resolution](../../loomweave/v0.1/system-design.md#config-resolution)) exists with stub content, and `schema_migrations` row count = 1.
- [ ] Second test: running `install` twice in the same dir without `--force` returns a non-zero exit and a clear error message referencing `--force`.
- [ ] Run tests; expect failure.
- [ ] Implement `install.rs`:
  - Refuse if `.loomweave/` already exists (UQ-WP1-08).
  - Create `.loomweave/` directory.
  - Open fresh DB and apply migrations.
  - Write `.loomweave/config.json` with the internal-state stub (`{"schema_version": 1, "last_run_id": null}`).
  - Write **`loomweave.yaml` at the project root** (not inside `.loomweave/`) — a comment-only stub noting "config TBD, see v0.1 design". This matches detailed-design §File layout: `.loomweave/` holds internal state; `loomweave.yaml` is a user-edited config that lives beside the user's source.
  - Write `.loomweave/.gitignore` with the UQ-WP1-04 rules.
- [ ] Author ADR-005 as a side effect of this task — short doc recording the `.gitignore` decision. Commit ADR-005 alongside this task.
- [ ] Run tests; expect pass.
- [ ] Commit: `feat(wp1): loomweave install subcommand; author ADR-005`.

### Task 6 — Writer-actor (L3)

**Files**:
- Create `/crates/loomweave-storage/src/commands.rs`
- Create `/crates/loomweave-storage/src/writer.rs`
- Modify `/crates/loomweave-storage/src/lib.rs` to export `Writer` and `WriterCmd`

Steps:

- [ ] In `commands.rs`, define `WriterCmd` enum with variants `BeginRun`, `InsertEntity`, `CommitRun`, `FailRun`. Each variant carries the data needed for the operation plus a `tokio::sync::oneshot::Sender<Result<..., StorageError>>` for the reply (ADR-011 per-command ack; UQ-WP1-03).
- [ ] Write failing `#[tokio::test]` integration test: open DB + reader pool + writer, send `BeginRun`, send `InsertEntity` with an `EntityId` from Task 2, send `CommitRun`, then query `entities` via a reader-pool acquire and assert the entity is present.
- [ ] Second failing test: send 150 `InsertEntity` commands with `batch_size = 50`, verify 3 `COMMIT` statements fired (instrument via a test-only `Writer` hook that counts commits).
- [ ] Third failing test: `FailRun` rolls back the in-flight transaction; entities inserted in the failed run do not appear on subsequent read.
- [ ] Run tests; expect failures.
- [ ] Implement `writer.rs`:
  - `Writer::spawn(conn: rusqlite::Connection, batch_size: usize) -> (Writer, JoinHandle<()>)` — returns an mpsc sender handle and the `tokio::task::JoinHandle` of the spawned task per ADR-011. The task owns the sole write `Connection`; all callers submit through `mpsc::Sender<WriterCmd>`.
  - Because `rusqlite::Connection` is `!Send` across await points, the task wraps blocking SQL calls in `tokio::task::spawn_blocking` (or, equivalently, uses `deadpool-sqlite`'s `Object::interact` pattern for the write connection so the runtime yields during I/O).
  - Loop handles each `WriterCmd`, tracks per-run transaction state, commits every `batch_size` inserts or on explicit `CommitRun`.
  - Use `rusqlite::Transaction`; begin on `BeginRun`, commit/rollback on `CommitRun`/`FailRun` or auto-commit on batch boundary. Apply ADR-011 PRAGMAs (`journal_mode = WAL`, `synchronous = NORMAL`, `busy_timeout = 5000`, `wal_autocheckpoint = 1000`) at connection open.
- [ ] Run tests; expect pass.
- [ ] Commit: `feat(wp1): L3 writer-actor (tokio::task) with per-N transaction batch`.

### Task 7 — `loomweave analyze` skeleton (no plugin)

**Files**:
- Create `/crates/loomweave-cli/src/analyze.rs`
- Modify `/crates/loomweave-cli/src/main.rs` for subcommand wiring

Steps:

- [ ] Write failing integration test: `loomweave install` in tempdir, `loomweave analyze .`, assert exit code 0, assert a row exists in `runs` with status `skipped_no_plugins`, assert no entities persisted.
- [ ] Run test; expect failure.
- [ ] Implement `analyze.rs`:
  - Resolve `.loomweave/` (error if missing, naming `loomweave install`).
  - Open DB, reader pool, writer.
  - `BeginRun` → `CommitRun` immediately with status `skipped_no_plugins`. Log a `tracing::info!` message "no plugins registered (WP2 will wire this)".
- [ ] Run test; expect pass.
- [ ] Commit: `feat(wp1): loomweave analyze skeleton (plugin wiring deferred to WP2)`.

### Task 8 — LlmProvider trait stub

**Files**:
- Create `/crates/loomweave-core/src/llm_provider.rs`
- Modify `/crates/loomweave-core/src/lib.rs` to re-export

Steps:

- [ ] Define `pub trait LlmProvider { fn name(&self) -> &str; }` — intentionally trivial; WP6 fills it out. Provide a `NoopProvider` unit struct that panics if `name()` is called (guard: nothing in Sprint 1 should call it).
- [ ] Add a unit test that asserts the trait compiles and `NoopProvider` implements it.
- [ ] `cargo test -p loomweave-core`.
- [ ] Commit: `feat(wp1): LlmProvider trait stub for WP6`.

### Task 9 — End-to-end WP1 smoke test

**Files**:
- Create `/crates/loomweave-cli/tests/wp1_e2e.rs`

Steps:

- [ ] Integration test that runs the full Sprint 1 WP1 slice: `loomweave install` in tempdir, `loomweave analyze .`, then open the DB with `rusqlite` directly and assert schema version, run count, and entity count (0 entities, 1 run with `skipped_no_plugins` status).
- [ ] Run; expect pass.
- [ ] Commit: `test(wp1): end-to-end smoke test`.

## 7. ADR triggers

- **ADR-005** (`.loomweave/` git-committable subpaths) — **authored during Task 5**.
  Sprint 1 must decide which subpaths land in `.gitignore` (see UQ-WP1-04); writing
  the decision down is the act of authoring ADR-005. The ADR file lives at
  [`../../loomweave/adr/ADR-005-loomweave-dir-tracking.md`](../../loomweave/adr/) and is
  moved from backlog to Accepted in the ADR index.

## 8. Exit criteria

WP1 is done for Sprint 1 when **all** of the following hold:

- `cargo build --workspace --release` succeeds on a clean Linux checkout.
- `cargo fmt --all -- --check` passes (ADR-023 gate).
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes against `clippy::pedantic = "warn"` (ADR-023 gate).
- `cargo nextest run --workspace --all-features` passes (all task-introduced tests + pre-existing; ADR-023 swaps `cargo test` for `cargo nextest`).
- `cargo deny check` passes (ADR-023 gate; license allowlist + advisories + bans + sources all green).
- `cargo doc --no-deps --all-features` builds without warnings (ADR-023 gate).
- **GitHub Actions CI green** on the WP1 PR across all five jobs (fmt, clippy, nextest, doc, deny) (ADR-023 gate).
- `loomweave install && loomweave analyze .` in a fresh tempdir produces the expected
  `skipped_no_plugins` run row with zero entities.
- L1 (full schema migration `0001`), L2 (`entity_id()` implementation), L3
  (WriterCmd + per-N-batch writer-actor) are each covered by at least one test.
- ADR-005 is Accepted and linked from the ADR index.
- ADR-023 is Accepted and linked from the ADR index (authored pre-Task-1; exit gate is that every Task-1 artefact listed in ADR-023's Decision section is present).
- Every UQ-WP1-* is marked resolved with the chosen outcome recorded as a comment
  in the code, an ADR amendment, or an update to this doc's §5.

See also [`signoffs.md` Tier A](./signoffs.md#tier-a--sprint-1-close-walking-skeleton)
for the cross-WP sign-off list.
