# ADR-011: Writer-Actor Concurrency Model with Per-N-Files Transactions

**Status**: Accepted; amended by ADR-041
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: SQLite concurrency model for `loomweave analyze` + `loomweave serve` against a shared `.loomweave/loomweave.db`; design-review `§2.2` flagged the original single-transaction posture as CRITICAL

## Summary

Loomweave uses **one writer-actor per process** — a single `tokio::task` owning the sole write `rusqlite::Connection`, fed by a bounded `mpsc::Sender<WriteOp>` with backpressure. `loomweave analyze` commits every **N files** (default 50, configurable) rather than the full run in one transaction. Reader tasks (MCP tool calls, HTTP API handlers, plugin processes) take read-only connections from a `deadpool-sqlite` pool (default max 16). A `loomweave analyze --shadow-db` flag writes to `.loomweave/loomweave.db.new` and atomic-renames on completion for operators who need zero-stale-read snapshots during long runs. This shape assumes WAL mode plus per-batch transactions handles realistic analyze+serve contention without lock starvation — an assumption **not empirically validated at v0.1 scale** and called out here as a known gap for v0.2 verification.

## Context

The original design review (`design-review.md §2.2`, flagged CRITICAL) found the SQLite concurrency claims incoherent:

> §4 claims WAL mode "supports concurrent reads during writes" and says `loomweave analyze` "holds the writer lock for the duration of the batch." The example run in §6 shows a 38-minute batch. These are incompatible: WAL grows unboundedly during a long writer; checkpointing cannot complete while readers pinned to the pre-analyze snapshot hold the old page.

The flagged options were (a) writer-actor + per-N-files transactions and (b) shadow-DB + atomic swap. The detailed-design §3 Concurrency section (`detailed-design.md:758-769`) adopted both: writer-actor is the default, shadow-DB is an opt-in flag. This ADR formalises that adoption and names the unvalidated-assumption asterisk.

The concurrency story matters because:

- `loomweave analyze` at elspeth scale produces ~100k–200k code entities, ~500k–1M edges, ~200k summary-cache rows (`detailed-design.md:773-778`). Transactions must be short enough that WAL growth stays bounded.
- `loomweave serve` must keep answering reads while `loomweave analyze` is ingesting (operator might run a long analyze in one terminal and `loomweave consult` in another). Writer-actor preserves this because WAL lets readers advance past committed transactions without blocking.
- Consult-mode agents write summary-cache entries during queries; those writes are tiny and sparse but must share the writer actor's commit discipline.
- `--resume` semantics depend on transaction granularity. If the run crashes mid-batch, committed transactions persist; the resume path restarts at the first uncommitted file.

## Decision

### Writer-actor model

One `tokio::task` per process owns the sole write `rusqlite::Connection`. All mutations route through an `mpsc::Sender<WriteOp>` with a bounded channel (default 256 operations; backpressure when full). There is no in-process write contention because there is exactly one writer; there is no cross-process writer contention because `loomweave analyze` and `loomweave serve` each instantiate their own writer-actor — but both processes writing to the same `loomweave.db` simultaneously produces stale-snapshot reads for `serve` (addressed via `--shadow-db`, below).

### Per-N-files transactions

`loomweave analyze` commits every **N files** (default 50) rather than the full run in one transaction. The constant is configurable via `loomweave.yaml:storage.tx_batch_size` (floor 10, no declared ceiling but operators changing this should know WAL growth tracks linearly).

Justification:

- WAL stays bounded. `wal_autocheckpoint=1000` pages catches up between batches.
- Checkpointing (truncate mode) runs after every 10 analyze-transactions or at run completion.
- `--resume` semantics are meaningful: the resume path re-scans files not yet committed, not the whole run.
- `database is locked` errors on consult-mode writes during analyze become rare (writer holds the lock for the duration of one batch, not the whole run).

### Reader pool

Read-only connections come from `deadpool-sqlite` with `max_size` 16 by default, configurable via `loomweave.yaml:storage.reader_pool_max`. Readers include:

- MCP tool call handlers (consult mode)
- HTTP API handlers (`loomweave serve`)
- Plugin subprocesses reading prior-run state (rare)
- The markdown renderer

WAL mode lets readers see the committed snapshot at the moment they open the connection; writes in progress are invisible until the writer commits and the reader reopens. Reader connections are short-lived (per-request); connection reuse is handled by the pool.

### SQLite PRAGMAs

Applied at connection open by both writer and readers:

```
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;   # ms
PRAGMA wal_autocheckpoint = 1000;   # pages
```

`synchronous = NORMAL` trades slightly weaker crash-durability (the last transaction in WAL may be lost on a power loss) for substantially faster commits. Acceptable for local-first workloads; operators on shared-infrastructure hosts with UPS can raise to `FULL` via `loomweave.yaml:storage.synchronous`.

### Shadow-DB opt-in (`loomweave analyze --shadow-db`)

`loomweave analyze --shadow-db` writes to `.loomweave/loomweave.db.new` (WAL files beside it). On completion, atomic-renames over `loomweave.db`. While analyze runs, `loomweave serve` reads the pre-run snapshot with zero staleness.

Trigger conditions for operators:

- Long analyze runs (minutes+) where consult-mode users need fresh reads from the pre-run state.
- Multi-user workstations where multiple agents hit `loomweave serve` during analyze.
- CI pipelines where the post-analyze `loomweave db verify` must read the fresh DB without locking the running `serve`.

Trade-offs:

- Doubles disk space during analyze (both `loomweave.db` and `loomweave.db.new` exist).
- Atomic-rename semantics work on Linux/macOS; on Windows, `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` with retries. WAL files handled alongside.
- `--resume` must be aware: shadow-DB resume reopens `.loomweave/loomweave.db.new`, not the live store. The `partial.json` run file records shadow-mode so resume picks the right DB.

### Operational posture

Running `loomweave analyze` and `loomweave serve` against the same `loomweave.db` simultaneously is supported but produces stale-snapshot reads in `serve` until `analyze` completes and checkpoint runs. `serve` emits `LMWV-INFRA-STALE-SNAPSHOT` once per detected staleness window. `--shadow-db` is the workaround for operators unwilling to accept that.

### Unvalidated assumption (named here)

The writer-actor + per-N-files design is a **design-time assumption** that WAL + N=50 + reader-pool-16 handles elspeth-scale concurrent analyze+serve without starvation or `database is locked` errors. It is based on SQLite's documented concurrency model and similar patterns in other local-first tools, but it is not empirically validated at v0.1 against realistic load. Specifically:

- Write throughput during analyze (does a batch commit fit within a few hundred milliseconds at 50 files × ~3k entities/file?).
- Checkpoint duration at WAL-size boundaries.
- Read-snapshot staleness windows when `serve` is under load from an agent.

The scope-commitment memo's Validation section observes analyze-phase timing; a v0.2 follow-up task (`NG-28` — proposed there) runs a dedicated analyze+serve concurrency test. This ADR ships the design; validation is explicitly future work.

## Alternatives Considered

### Alternative 1: Full-batch single transaction

Commit the entire `loomweave analyze` run in one transaction.

**Pros**: atomic view of the whole run; `--resume` is either "run it all again" or "accept the committed state" with no intermediate cases.

**Cons**: design-review `§2.2` flagged this specifically. WAL grows unboundedly; checkpointing blocked while readers pin the pre-analyse snapshot; `database is locked` errors surface to consult-mode writes during the full batch duration (minutes to an hour on realistic codebases).

**Why rejected**: incoherent under the stated process topology. Per-N-files is the standard fix.

### Alternative 2: Shadow-DB as the default

Every `loomweave analyze` writes to `loomweave.db.new` and atomic-renames; no write contention with `serve` ever.

**Pros**: zero-stale-read is the default; simpler contention model (analyze never locks the live DB).

**Cons**: doubles disk space for every run (v0.1 target DB size is 500 MB–2 GB — doubling is real). `--resume` must reopen the right file. Atomic-rename cross-platform edge cases become a default path, not an opt-in. Operators running one-off analyses who don't care about concurrent `serve` pay the disk cost for no benefit.

**Why rejected**: opt-in is cheaper by default. The flag is the right shape — operators who need it name that need.

### Alternative 3: Pooled writer connections

Multiple writer tasks, each owning a write connection, coordinate at the SQLite layer.

**Pros**: parallelism on analyze writes.

**Cons**: SQLite WAL supports exactly one writer at a time (documented). Any pooled approach collapses to serial-with-retries — the operating system's process scheduler plus `busy_timeout` decides who gets the lock. Net result is the same throughput as one writer with more scheduling overhead and `database is locked` errors becoming a hot path.

**Why rejected**: incompatible with SQLite's concurrency model. Benchmarks consistently show single-writer outperforms pooled-writer on SQLite.

### Alternative 4: Separate store process (analyze writes, serve reads via RPC)

`loomweave analyze` runs as one process; `loomweave serve` as another; a third "store" process owns the DB and serves both over RPC.

**Pros**: physical isolation; no shared-file-descriptor concerns.

**Cons**: introduces a Loomweave-database daemon — violates single-binary posture (ADR-001, CON-LOCAL-01). Adds process supervision, protocol version management, IPC encoding/decoding to the hot path. The problems shadow-DB solves (zero-stale-read during long analyze) are bigger when RPC adds its own latency.

**Why rejected**: categorical single-binary violation. Weft §6 ("no central store or database") reads harder at the process level.

### Alternative 5: Alternative storage engine (DuckDB, Kuzu, custom)

Already rejected in ADR-001 for the storage-engine selection. Re-cited here because the concurrency model is a function of the engine.

**Why not revisiting**: ADR-001 commits SQLite; this ADR lives inside that commitment.

## Consequences

### Positive

- WAL growth is bounded by N (per-batch commits let checkpointing catch up).
- `loomweave analyze` and `loomweave serve` coexist against the same DB without catastrophic lock contention — stale snapshots are the failure mode, not "database is locked" exceptions.
- `--resume` semantics are meaningful: crashed-mid-run analyze resumes at the first uncommitted file, not from scratch.
- Shadow-DB provides a clean escape hatch for operators with concurrency-critical workflows. Opt-in keeps the default lightweight.
- Design-review `§2.2` CRITICAL flag retires: the concurrency model is coherent under the stated topology.

### Negative

- N=50 is an educated guess. It is configurable, but operators choosing the wrong N can produce WAL growth (too-large N) or write overhead (too-small N). Mitigation: default ships with elspeth-scale validation (when C1 runs); operators adjusting see the `storage.tx_batch_size` documentation.
- The `--shadow-db` mode doubles disk space during analyze. A 2 GB DB becomes ~4 GB peak. Operators with constrained disk space need to know the flag's cost.
- Unvalidated concurrency assumption. If v0.2 concurrency testing reveals starvation patterns under realistic load, this ADR needs a dated revision. Named here so the gap is visible rather than hidden.

### Neutral

- `synchronous = NORMAL` is a durability-vs-speed trade-off. Acceptable default for local-first; escape via config.
- Writer-actor is a `tokio::task`, not an OS thread. The same Tokio runtime hosts the rest of Loomweave's async I/O — no thread-pool tuning surface.
- Reader pool max 16 is sufficient for one consult-agent + one Wardline-equivalent state puller (`requirements.md:699`). Raising it is free modulo memory.

## Related Decisions

- [ADR-001](./ADR-001-rust-for-core.md) — Rust + `rusqlite` + `tokio` is the framework this ADR lives inside. `deadpool-sqlite` is named in ADR-001's ecosystem argument.
- [ADR-005](./ADR-005-loomweave-dir-tracking.md) — `.loomweave/` git-committable default. `loomweave.db.new` (shadow-DB intermediate) must be `.gitignore`d; ADR-005 picks the exact ignore rules.
- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — the per-run entity-count cap (Layer 2d) interacts with transaction granularity: the cap triggers a final flush and abort, which lands cleanly because per-N-files keeps the write path predictable.

## References

- [Loomweave v0.1 design review §2.2](../../implementation/v0.1-reviews/pre-restructure/design-review.md) (lines 56-66) — original CRITICAL flag; writer-actor and shadow-DB options.
- [Loomweave v0.1 detailed design §3 (Concurrency)](../v0.1/detailed-design.md) (lines 758-769) — the implementation detail this ADR formalises.
- [Loomweave v0.1 requirements §NFR-RELIABILITY-02](../v0.1/requirements.md) (line 857) — WAL + writer-actor + checkpoint discipline as crash-safety requirement.
- [Loomweave v0.1 system design §4 Storage](../v0.1/system-design.md) — SQLite rationale; WAL mode claim.
- [Loomweave v0.1 scope commitments — ADR sprint + validation](../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md) (lines 181-195, 249-251) — P0 promotion from P1 and the explicit follow-up validation task.
