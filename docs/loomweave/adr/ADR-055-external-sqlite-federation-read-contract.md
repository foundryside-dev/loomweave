# ADR-055: Versioned External SQLite Federation Read Contract

**Status**: Accepted
**Date**: 2026-07-12
**Deciders**: john
**Extends**: [ADR-011](./ADR-011-writer-actor-and-sqlite.md), [ADR-035](./ADR-035-sqlite-operational-tuning.md)

## Context

Loomweave's SQLite database is primarily an internal, regenerable catalogue.
Plainweave nevertheless has a legitimate local-only federation use case: it
reads public-surface entities, classifier tags, latest-run coverage, and stable
entity identity without starting an HTTP peer. Before this decision, the
consumer opened raw tables but Loomweave published neither a compatible
`PRAGMA user_version` range nor a stable column projection. The internal
`verify_user_version` function was insufficient: it intentionally accepts every
historical version known to the migration runner and answers only whether the
database is too new for a Loomweave writer.

That ambiguity creates two failure modes. A consumer can execute catalogue SQL
against a future or foreign database before discovering incompatibility, or it
can accidentally depend on an internal table/column that Loomweave never
promised to preserve.

## Decision

Loomweave publishes `loomweave.external-sqlite.v1`, implemented by
`loomweave_storage::external_sqlite_compatibility`.

The v1 header contract is:

- `PRAGMA application_id` is either `0x4c4d5756` (`LMWV`) or legacy `0`;
- `PRAGMA user_version` is in the deliberately frozen range `5..=15`;
- version `15` is `compatible` (current), versions `5..=14` are
  `older_supported`, and every other version is `incompatible`;
- legacy application ID `0` plus a matching structure establishes format
  compatibility, not authenticity or provenance.

The v1 safe projection is exactly:

| Table | Safe columns |
|---|---|
| `runs` | `id`, `started_at`, `completed_at`, `stats`, `status` |
| `entities` | `id`, `plugin_id`, `kind`, `source_file_path`, `source_byte_start`, `source_byte_end`, `source_line_start`, `source_line_end`, `properties`, `content_hash` |
| `entity_tags` | `entity_id`, `plugin_id`, `tag` |
| `sei_bindings` | `sei`, `current_locator`, `body_hash`, `status` |
| `sei_lineage` | `sei`, `event`, `old_locator`, `new_locator`, `run_id`, `recorded_at` |

No other table, view, index, trigger, column order, constraint, or query plan is
part of the external contract. Consumers name columns explicitly and never use
`SELECT *`.

The required consumer order is:

1. open the existing file read-only, with creation and migrations disabled;
2. read `application_id` and `user_version` only;
3. reject a foreign, unmigrated, too-old, or too-new header;
4. verify the required tables and columns;
5. only then issue catalogue SQL.

Header incompatibility is classified before table introspection. This ensures a
future header on a database with no familiar tables reports `too_new`, not a
misleading `no such table` error. Structural drift inside the accepted integer
range reports `missing_required_surface`.

`runs.stats` and `entities.properties` remain JSON containers. Their keys are
not implicitly stable: a consumer may rely only on separately versioned objects
documented by Loomweave, including `runs.stats.classifier_coverage` schema
`loomweave.classifier-coverage.v1`.

The external maximum is not an alias of `CURRENT_SCHEMA_VERSION`. A new
internal migration does not become externally readable automatically. Advancing
the external maximum requires reviewing the safe projection, updating its
snapshot tests and contract fixtures, and coordinating consumers.

## Consequences

### Positive

- Consumers reject incompatible files before catalogue SQL.
- Current and older-supported schemas are mechanically distinguishable.
- Loomweave remains free to evolve every unlisted storage detail.
- The same typed probe can power `loomweave doctor` without duplicating policy.

### Negative

- The raw-table contract couples Loomweave to the listed columns for the life of
  v1. Removing or renaming one requires a new external contract or compatibility
  view.
- Legacy `application_id=0` cannot authenticate a database as Loomweave; the
  structural check only establishes compatibility.
- A future internal schema is deliberately rejected until this boundary is
  reviewed, even when its migration appears additive.
- **The compatibility report is a point-in-time answer, not a lease.** It is
  valid for the connection and read transaction on which it was performed, and
  a consumer that holds a long-lived session across a Loomweave upgrade can
  observe a version it already cleared being migrated underneath it. `loomweave
  analyze` re-runs `apply_migrations` on every invocation, each migration
  commits its own transaction, and `PRAGMA user_version` is bumped only after
  the whole loop, so on the first `analyze` after a binary upgrade that ships a
  new migration there is a window where a reader sees the old `user_version`
  (reported compatible) while a newer migration has already committed. SQLite's
  per-migration atomicity means a reader can never observe a torn migration —
  only "some but not all" applied — so this is not corruption, but it is
  outside what the structural check can promise.

  Consumers should therefore either re-check compatibility per unit of work, or
  wrap their reads in a single `BEGIN DEFERRED` transaction so WAL pins one
  snapshot for the duration. Opening per call, as Plainweave's adapter does
  today, also avoids the window — but nothing in this contract enforces that,
  which is why it is recorded here rather than assumed.

## Alternatives Considered

### Accept every version understood by the migration runner

Rejected. Writer migration compatibility is broader than a consumer's reviewed
read contract and would silently widen whenever a migration lands.

### Publish versioned SQL views immediately

Deferred. The current consumer needs a small existing projection and versions
5–12 already contain it. Stable views remain the preferred successor if a
future migration needs to reshape these tables.

### Require only `application_id=LMWV`

Rejected for v1 because published legacy Loomweave databases may carry zero.
Zero is accepted with an explicit non-authenticating warning posture.

## Verification

- `cargo test -p loomweave-storage --test external_sqlite`
- `cargo test -p loomweave-storage schema`

## References

- [Federation contracts](../../federation/contracts.md#external-sqlite-read-contract)
- `crates/loomweave-storage/src/external_sqlite.rs`
