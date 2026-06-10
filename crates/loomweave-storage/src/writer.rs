//! Writer-actor implementation (L3 lock-in) per ADR-011.
//!
//! The actor owns the sole write `rusqlite::Connection`. Callers submit
//! commands via `Writer::sender()`. The actor loop pulls one command at a
//! time, applies the mutation inside an implicit transaction bound to the
//! current run, and commits every `batch_size` entity inserts (the
//! "per-N-files" transaction pattern, default N=50 per ADR-011).
//!
//! UQ-WP1-03 resolution: the `commits_observed` [`std::sync::Arc`]`<`[`std::sync::atomic::AtomicUsize`]`>` is
//! incremented on every `COMMIT` issued by the actor. Tests read it to
//! verify batch-boundary commits fire at the expected cadence. It is
//! present in release builds as a no-op counter; no `#[cfg(test)]` gating
//! is used.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::cache::{
    InferredEdgeCacheEntry, delete_summary_cache_for_entity, inferred_edge_cache_key_id,
    touch_summary_cache, upsert_inferred_edge_cache, upsert_summary_cache,
};
use crate::commands::{
    Ack, EdgeConfidence, EdgeRecord, EntityRecord, FindingRecord, InferredCallEdgeRecord,
    InferredEdgeWriteStats, RunStatus, WriterCmd,
};
use crate::error::{Result, StorageError};
use crate::pragma;
use crate::schema;
use crate::unresolved::replace_unresolved_call_sites_for_caller;

/// Default transaction batch size per ADR-011.
pub const DEFAULT_BATCH_SIZE: usize = 50;

/// Default `mpsc` channel capacity per ADR-011.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

pub struct Writer {
    tx: mpsc::Sender<WriterCmd>,
    /// Count of every `COMMIT` statement issued by the actor.
    ///
    /// Includes both per-batch boundary commits (every `batch_size` writes)
    /// and the final commit issued by `CommitRun`. Intended for test
    /// assertions and diagnostic counters; not a measure of completed runs.
    ///
    /// Read this field before dropping the [`Writer`]: the actor holds its
    /// own `Arc` clone that lives until the `JoinHandle` resolves.
    pub commits_observed: Arc<AtomicUsize>,
    /// Process-lifetime count of edges rejected by the writer.
    ///
    /// Per-kind contract failures increment this counter so malformed plugin
    /// edges are visible in the same run stat. Re-observed edge triples refresh
    /// metadata via upsert and are not counted as drops.
    pub dropped_edges_total: Arc<AtomicUsize>,
    /// Process-lifetime count of accepted ambiguous-confidence edges.
    pub ambiguous_edges_total: Arc<AtomicUsize>,
}

impl Writer {
    /// Spawn the writer-actor on the current tokio runtime.
    ///
    /// Returns the `Writer` handle and the [`JoinHandle`] of the actor task.
    /// Callers await the [`JoinHandle`] at shutdown to ensure the actor has
    /// flushed any pending commit.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] if the `rusqlite::Connection` cannot
    /// be opened, or [`StorageError::PragmaInvariant`] if write PRAGMAs fail.
    pub fn spawn(
        db_path: impl AsRef<Path>,
        batch_size: usize,
        channel_capacity: usize,
    ) -> Result<(Self, JoinHandle<Result<()>>)> {
        let mut conn = Connection::open(db_path.as_ref())?;
        pragma::apply_write_pragmas(&conn)?;
        // STO-02: refuse a database whose `user_version` is strictly greater
        // than CURRENT_SCHEMA_VERSION. Equal/less are normal — equal is the
        // already-migrated steady state, less is handled by the migration
        // runner (which `install` calls before the writer ever spawns).
        schema::verify_user_version(&conn)?;

        let (tx, rx) = mpsc::channel(channel_capacity);
        let commits_observed = Arc::new(AtomicUsize::new(0));
        let dropped_edges_total = Arc::new(AtomicUsize::new(0));
        let ambiguous_edges_total = Arc::new(AtomicUsize::new(0));
        let commits_for_actor = commits_observed.clone();
        let dropped_for_actor = dropped_edges_total.clone();
        let ambiguous_for_actor = ambiguous_edges_total.clone();
        let handle = tokio::task::spawn_blocking(move || -> Result<()> {
            run_actor(
                rx,
                &mut conn,
                batch_size,
                &commits_for_actor,
                &dropped_for_actor,
                &ambiguous_for_actor,
            );
            Ok(())
        });
        Ok((
            Writer {
                tx,
                commits_observed,
                dropped_edges_total,
                ambiguous_edges_total,
            },
            handle,
        ))
    }

    pub fn sender(&self) -> mpsc::Sender<WriterCmd> {
        self.tx.clone()
    }

    /// Convenience: send a command and await its ack.
    ///
    /// Intended for use by `loomweave analyze` (Task 7) and later WP
    /// consumers; Sprint 1 integration tests use a local test helper
    /// rather than this method. Kept as part of the L3 lock-in surface
    /// so callers have a stable entry point when they arrive.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::WriterGone`] if the actor has exited and the
    /// channel is closed. Returns [`StorageError::WriterNoResponse`] if the
    /// actor dropped the `oneshot` sender without replying. Otherwise
    /// propagates whatever error the actor returned for the command.
    pub async fn send_wait<T, F>(&self, build: F) -> Result<T>
    where
        F: FnOnce(oneshot::Sender<Result<T>>) -> WriterCmd,
        T: 'static,
    {
        let (tx, rx) = oneshot::channel();
        let cmd = build(tx);
        self.tx
            .send(cmd)
            .await
            .map_err(|_| StorageError::WriterGone)?;
        rx.await.map_err(|_| StorageError::WriterNoResponse)?
    }
}

// Exhaustive single-`match` command-dispatch loop: each `WriterCmd` variant gets
// one short arm, so length scales with the command set, not with logic depth.
// Splitting it would only scatter the dispatch a reader wants in one place.
#[allow(clippy::too_many_lines)]
fn run_actor(
    mut rx: mpsc::Receiver<WriterCmd>,
    conn: &mut Connection,
    batch_size: usize,
    commits_observed: &AtomicUsize,
    dropped_edges_total: &AtomicUsize,
    ambiguous_edges_total: &AtomicUsize,
) {
    let mut state = ActorState::new(batch_size);

    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            WriterCmd::BeginRun {
                run_id,
                config_json,
                started_at,
                head_commit,
                ack,
            } => {
                reply(
                    ack,
                    begin_run(
                        conn,
                        &mut state,
                        &run_id,
                        &config_json,
                        &started_at,
                        head_commit.as_deref(),
                    ),
                );
            }
            WriterCmd::ResumeRun { run_id, ack } => {
                reply(ack, resume_run(conn, &mut state, &run_id));
            }
            WriterCmd::InsertEntity { entity, ack } => {
                let res = insert_entity(conn, &mut state, &entity, commits_observed);
                reply(ack, res);
            }
            WriterCmd::InsertEdge { edge, ack } => {
                let res = insert_edge(
                    conn,
                    &mut state,
                    &edge,
                    commits_observed,
                    dropped_edges_total,
                    ambiguous_edges_total,
                );
                reply(ack, res);
            }
            WriterCmd::ReplaceAnchoredEdgesForSourceFile {
                source_file_id,
                ack,
            } => {
                let res = replace_anchored_edges_for_source_file(
                    conn,
                    &mut state,
                    &source_file_id,
                    commits_observed,
                );
                reply(ack, res);
            }
            WriterCmd::InsertFinding { finding, ack } => {
                let res = insert_finding(conn, &mut state, &finding, commits_observed);
                reply(ack, res);
            }
            WriterCmd::PersistPostRunFinding { finding, ack } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    write_finding_row(conn, &finding)
                });
                reply(ack, res);
            }
            WriterCmd::FlushRunBatch { ack } => {
                let res = flush_run_batch(conn, &mut state, commits_observed);
                reply(ack, res);
            }
            WriterCmd::InsertInferredEdges {
                cache_entry,
                edges,
                ack,
            } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    insert_inferred_edges(conn, &cache_entry, &edges)
                });
                reply(ack, res);
            }
            WriterCmd::UpsertSummaryCache { entry, ack } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    upsert_summary_cache(conn, &entry)
                });
                reply(ack, res);
            }
            WriterCmd::TouchSummaryCache {
                key,
                last_accessed_at,
                ack,
            } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    touch_summary_cache(conn, &key, &last_accessed_at)
                });
                reply(ack, res);
            }
            WriterCmd::InvalidateSummaryCacheForEntity { entity_id, ack } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    delete_summary_cache_for_entity(conn, &entity_id)
                });
                reply(ack, res);
            }
            WriterCmd::UpsertWardlineTaintFact { fact, ack } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::wardline_taint::upsert_taint_fact(conn, &fact)
                });
                reply(ack, res);
            }
            WriterCmd::UpsertPriorIndex {
                entries,
                recorded_at,
                ack,
            } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::prior_index::replace_prior_index(conn, &entries, &recorded_at)
                });
                reply(ack, res);
            }
            WriterCmd::SweepStaleFindings {
                current_run_id,
                ack,
            } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::findings::sweep_stale_findings(conn, &current_run_id)
                });
                reply(ack, res);
            }
            WriterCmd::UpsertSeiBinding { record, ack } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::sei::upsert_sei_binding(conn, &record)
                });
                reply(ack, res);
            }
            WriterCmd::OrphanSeiBinding {
                sei,
                run_id,
                recorded_at,
                ack,
            } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::sei::orphan_sei_binding(conn, &sei, &run_id, &recorded_at)
                });
                reply(ack, res);
            }
            WriterCmd::SetEntitySignature {
                entity_id,
                signature,
                ack,
            } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::sei::set_entity_signature(conn, &entity_id, signature.as_deref())
                });
                reply(ack, res);
            }
            WriterCmd::AppendSeiLineage { entry, ack } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::sei::append_sei_lineage(conn, &entry)
                });
                reply(ack, res);
            }
            WriterCmd::ReplaceUnresolvedCallSitesForCaller {
                caller_entity_id,
                caller_content_hash,
                sites,
                ack,
            } => {
                let res = replace_unresolved_call_sites_in_run(
                    conn,
                    &mut state,
                    &caller_entity_id,
                    &caller_content_hash,
                    &sites,
                    commits_observed,
                );
                reply(ack, res);
            }
            WriterCmd::CommitRun {
                run_id,
                status,
                completed_at,
                stats_json,
                ack,
            } => {
                let res = commit_run(
                    conn,
                    &mut state,
                    &run_id,
                    status,
                    &completed_at,
                    &stats_json,
                    commits_observed,
                );
                // A committed run is the "snapshot" boundary: TRUNCATE-checkpoint
                // so the on-disk loomweave.db is a whole, committable artifact
                // (ADR-005 tracks it) without waiting for the process to exit.
                // Only `CommitRun` (end of an analyze run) reaches here — never the
                // serve summary-write path — so there is no per-write checkpoint
                // cost. Best-effort and run before the ack so a caller that reads
                // the file right after sees the truncated WAL (clarion-cdee445ed8).
                if res.is_ok() {
                    checkpoint_truncate(conn);
                }
                reply(ack, res);
            }
            WriterCmd::FailRun {
                run_id,
                reason,
                completed_at,
                ack,
            } => {
                let res = fail_run(conn, &mut state, &run_id, &reason, &completed_at);
                reply(ack, res);
            }
        }
    }
    cleanup_after_channel_close(conn, &mut state);
}

fn cleanup_after_channel_close(conn: &mut Connection, state: &mut ActorState) {
    // Two hazards to cover: an open entity transaction must be rolled back, and
    // a run in progress must not be left permanently as `'running'`.
    if state.in_tx {
        let _ = conn.execute_batch("ROLLBACK");
        state.in_tx = false;
    }
    if let Some(run_id) = state.current_run.take() {
        let stats_json =
            serde_json::json!({ "failure_reason": "writer channel closed unexpectedly" })
                .to_string();
        let _ = conn.execute(
            "UPDATE runs SET status = 'failed', \
                completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                stats = ?1, \
                owner_pid = NULL \
              WHERE id = ?2",
            params![stats_json, run_id],
        );
    }
}

/// Issue `PRAGMA wal_checkpoint(TRUNCATE)` on the writer's own connection,
/// best-effort. A concurrent reader (a live `serve` reader-pool connection) can
/// hold the checkpoint back from resetting the WAL — that returns a "busy" row,
/// not an error, and is harmless: the committed frames are already durable and
/// stay applied. A genuine failure is logged, never propagated, so a checkpoint
/// hiccup can never fail an otherwise-successful run commit (clarion-cdee445ed8).
fn checkpoint_truncate(conn: &Connection) {
    if let Err(err) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        tracing::warn!(
            error = %err,
            "loomweave writer: post-commit WAL checkpoint(TRUNCATE) failed (harmless; \
             committed frames remain durable)"
        );
    }
}

fn reply<T>(ack: Ack<T>, result: Result<T>) {
    // If the caller dropped the receiver, we discard the result. This is
    // correct behaviour — the writer is still responsible for its own
    // durability, and the caller chose to stop caring.
    let _ = ack.send(result);
}

struct ActorState {
    batch_size: usize,
    /// Writes (entity inserts + edge inserts attempted) accumulated in the
    /// current transaction. Renamed from `inserts_in_batch` in B.3 because
    /// an edge-heavy file would otherwise never trip the batch boundary.
    /// All `InsertEdge` calls count — including UNIQUE-conflict dedupes —
    /// so the batch cadence is workload-shape-invariant.
    writes_in_batch: usize,
    /// True if `BEGIN` has been issued and no `COMMIT`/`ROLLBACK` has fired.
    in_tx: bool,
    /// The run currently in progress, if any.
    current_run: Option<String>,
    /// Retry schedule for acquiring the write transaction (STO-05). Batch
    /// transactions open with `BEGIN IMMEDIATE` so cross-process write
    /// contention is resolved at lock-acquire (where `busy_timeout` is honored)
    /// rather than failing mid-statement on a deferred-lock upgrade.
    retry_policy: crate::retry::RetryPolicy,
}

impl ActorState {
    fn new(batch_size: usize) -> Self {
        Self {
            batch_size,
            writes_in_batch: 0,
            in_tx: false,
            current_run: None,
            retry_policy: crate::retry::RetryPolicy::writer_default(),
        }
    }
}

/// Open the write transaction for the current batch.
///
/// Uses `BEGIN IMMEDIATE` with the actor's retry policy (STO-05) rather than a
/// deferred `BEGIN`: the actor always writes inside the transaction, so taking
/// the write lock up front lets cross-process contention be resolved at
/// lock-acquire (where `busy_timeout` and our retry apply) instead of failing
/// mid-statement on a deferred-lock upgrade that the busy handler cannot serve.
fn begin_write_tx(conn: &Connection, state: &ActorState) -> Result<()> {
    crate::retry::begin_immediate(conn, &state.retry_policy)
}

fn owner_pid() -> i64 {
    i64::from(std::process::id())
}

fn begin_run(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    config_json: &str,
    started_at: &str,
    head_commit: Option<&str>,
) -> Result<()> {
    begin_run_inner(
        conn,
        state,
        run_id,
        config_json,
        started_at,
        head_commit,
        |_| {},
        |_| {},
    )
}

/// `begin_run` with two test seams.
///
/// `after_insert_committed` fires after the auto-committed `INSERT INTO runs`
/// (which deliberately publishes the row as `running` so cross-process
/// `analyze_status` pollers can see an in-progress run *before* the first batch
/// commits) and before the write transaction is opened. `on_write_tx_failed`
/// fires only when `begin_write_tx` returns `Err`, just before the cleanup
/// `UPDATE`. Production passes no-ops; tests use them to drive the review-#4
/// TOCTOU window deterministically (grab a competing write lock in the first
/// seam so `begin_write_tx` fails, release it in the second so the best-effort
/// cleanup can re-acquire the lock). This mirrors the `on_busy` seam discipline
/// in `retry.rs`.
fn begin_run_inner(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    config_json: &str,
    started_at: &str,
    head_commit: Option<&str>,
    mut after_insert_committed: impl FnMut(&Connection),
    mut on_write_tx_failed: impl FnMut(&Connection),
) -> Result<()> {
    if state.current_run.is_some() {
        return Err(StorageError::WriterProtocol(
            "BeginRun received while a run is already in progress".to_owned(),
        ));
    }
    conn.execute(
        "INSERT INTO runs ( \
            id, started_at, completed_at, config, stats, status, analyzed_at_commit, \
            owner_pid, heartbeat_at \
         ) VALUES (?1, ?2, NULL, ?3, '{}', 'running', ?4, ?5, ?2)",
        params![run_id, started_at, config_json, head_commit, owner_pid()],
    )?;
    after_insert_committed(conn);
    if let Err(err) = begin_write_tx(conn, state) {
        // TOCTOU repair (review #4). The INSERT above auto-committed the row as
        // `running` (visible to analyze_status), but under sustained
        // cross-process contention begin_write_tx can exhaust its retries here.
        // Without repair the row is stranded `running` with `current_run`
        // unset, so the actor's channel-close cleanup never marks it failed and
        // analyze_status reports a phantom in-progress run. Re-mark it failed
        // under a fresh implicit transaction (mirrors the CommitRun
        // failure-remark idiom). The INSERT is deliberately NOT moved inside
        // the tx (the ticket's literal suggestion) because that would hide the
        // `running` row from cross-process analyze_status until the first batch
        // COMMIT — the regression review #15 warns about. Best-effort: if the
        // cleanup itself loses the still-contended lock, mark_stale_running_runs_failed
        // sweeps the row on the next startup.
        on_write_tx_failed(conn);
        let _ = conn.execute(
            "UPDATE runs \
                SET status = 'failed', \
                    completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                    owner_pid = NULL \
              WHERE id = ?1",
            params![run_id],
        );
        return Err(err);
    }
    state.in_tx = true;
    state.writes_in_batch = 0;
    state.current_run = Some(run_id.to_owned());
    Ok(())
}

/// Reopen an existing run row instead of inserting a new one (the `--resume`
/// path, REQ-FINDING-05). `begin_run` does an `INSERT` that fails on the run
/// PK when handed an existing id; `resume_run` `UPDATE`s the row back to
/// `running` and clears `completed_at`, then binds it as the active run and
/// opens the write transaction exactly as `begin_run` does. The subsequent
/// re-walk upserts entities/edges idempotently (see
/// `insert_entity_is_idempotent_across_runs`), so a resumed run reproduces the
/// same durable graph as the original — `--resume` is a re-emit-without-flip
/// path, not an incremental checkpoint-recovery one.
fn resume_run(conn: &mut Connection, state: &mut ActorState, run_id: &str) -> Result<()> {
    resume_run_inner(conn, state, run_id, |_| {}, |_| {})
}

/// `resume_run` with the same two test seams as [`begin_run_inner`].
///
/// Unlike `begin_run`, `resume_run` mutates a PRE-EXISTING row, so the
/// failure path must *restore* the row's prior terminal state rather than mark
/// it failed — leaving a previously-`completed` run flipped to `running` would
/// mis-report it (review #15). The prior `(status, completed_at)` are captured
/// before the flip and restored if `begin_write_tx` fails.
fn resume_run_inner(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    mut after_update_committed: impl FnMut(&Connection),
    mut on_write_tx_failed: impl FnMut(&Connection),
) -> Result<()> {
    if state.current_run.is_some() {
        return Err(StorageError::WriterProtocol(
            "ResumeRun received while a run is already in progress".to_owned(),
        ));
    }
    // Capture the row's prior terminal state BEFORE flipping it to `running`,
    // so it can be restored verbatim if we fail to open the write transaction.
    let prior = conn
        .query_row(
            "SELECT status, completed_at FROM runs WHERE id = ?1",
            params![run_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let Some((prior_status, prior_completed_at)) = prior else {
        return Err(StorageError::WriterProtocol(format!(
            "ResumeRun: no run with id {run_id} to resume"
        )));
    };
    let reopened = conn.execute(
        "UPDATE runs \
            SET status = 'running', \
                completed_at = NULL, \
                owner_pid = ?1, \
                heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
          WHERE id = ?2",
        params![owner_pid(), run_id],
    )?;
    if reopened == 0 {
        // Raced away between the SELECT and the UPDATE — treat as not-found.
        return Err(StorageError::WriterProtocol(format!(
            "ResumeRun: no run with id {run_id} to resume"
        )));
    }
    after_update_committed(conn);
    if let Err(err) = begin_write_tx(conn, state) {
        // The row pre-existed this resume, so restore its prior terminal state
        // rather than leave it stranded `running` (review #15). Best-effort:
        // mark_stale_running_runs_failed is the backstop if the restore also
        // loses the still-contended lock.
        on_write_tx_failed(conn);
        let _ = conn.execute(
            "UPDATE runs \
                SET status = ?1, completed_at = ?2, owner_pid = NULL \
              WHERE id = ?3",
            params![prior_status, prior_completed_at, run_id],
        );
        return Err(err);
    }
    state.in_tx = true;
    state.writes_in_batch = 0;
    state.current_run = Some(run_id.to_owned());
    Ok(())
}

fn insert_entity(
    conn: &mut Connection,
    state: &mut ActorState,
    entity: &EntityRecord,
    commits_observed: &AtomicUsize,
) -> Result<()> {
    if state.current_run.is_none() {
        return Err(StorageError::WriterProtocol(
            "InsertEntity received without a preceding BeginRun".to_owned(),
        ));
    }
    enforce_entity_kind_contract(entity)?;
    if !state.in_tx {
        begin_write_tx(conn, state)?;
        state.in_tx = true;
    }
    validate_entity_source_file_anchor(conn, entity)?;
    // ON CONFLICT(id) DO UPDATE makes `loomweave analyze` idempotent across runs:
    // a re-walk that produces the same entity updates the existing row instead
    // of raising UNIQUE. `created_at` and `first_seen_commit` are preserved
    // (the entity was first seen on its original run); `updated_at` and
    // `last_seen_commit` are refreshed from the latest run's record. The
    // AFTER UPDATE trigger on `entities` keeps `entity_fts` in sync.
    conn.execute(
        "INSERT INTO entities ( \
            id, plugin_id, kind, name, short_name, \
            parent_id, source_file_id, source_file_path, \
            source_byte_start, source_byte_end, \
            source_line_start, source_line_end, \
            properties, content_hash, summary, wardline, \
            first_seen_commit, last_seen_commit, \
            created_at, updated_at \
         ) VALUES ( \
            ?1, ?2, ?3, ?4, ?5, \
            ?6, ?7, ?8, \
            ?9, ?10, \
            ?11, ?12, \
            ?13, ?14, ?15, ?16, \
            ?17, ?18, \
            ?19, ?20 \
         ) \
         ON CONFLICT(id) DO UPDATE SET \
            plugin_id         = excluded.plugin_id, \
            kind              = excluded.kind, \
            name              = excluded.name, \
            short_name        = excluded.short_name, \
            parent_id         = excluded.parent_id, \
            source_file_id    = excluded.source_file_id, \
            source_file_path  = excluded.source_file_path, \
            source_byte_start = excluded.source_byte_start, \
            source_byte_end   = excluded.source_byte_end, \
            source_line_start = excluded.source_line_start, \
            source_line_end   = excluded.source_line_end, \
            properties        = excluded.properties, \
            content_hash      = excluded.content_hash, \
            summary           = excluded.summary, \
            wardline          = excluded.wardline, \
            last_seen_commit  = excluded.last_seen_commit, \
            updated_at        = excluded.updated_at",
        params![
            entity.id,
            entity.plugin_id,
            entity.kind,
            entity.name,
            entity.short_name,
            entity.parent_id,
            entity.source_file_id,
            entity.source_file_path,
            entity.source_byte_start,
            entity.source_byte_end,
            entity.source_line_start,
            entity.source_line_end,
            entity.properties_json,
            entity.content_hash,
            entity.summary_json,
            entity.wardline_json,
            entity.first_seen_commit,
            entity.last_seen_commit,
            entity.created_at,
            entity.updated_at,
        ],
    )?;
    conn.execute(
        "DELETE FROM entity_tags WHERE entity_id = ?1 AND plugin_id = ?2",
        params![entity.id, entity.plugin_id],
    )?;
    for tag in &entity.tags {
        conn.execute(
            "INSERT OR IGNORE INTO entity_tags (entity_id, plugin_id, tag) VALUES (?1, ?2, ?3)",
            params![entity.id, entity.plugin_id, tag],
        )?;
    }
    bump_writes_and_maybe_commit(conn, state, commits_observed)?;
    Ok(())
}

fn enforce_entity_kind_contract(entity: &EntityRecord) -> Result<()> {
    if entity.plugin_id != "core"
        && loomweave_core::plugin::manifest::RESERVED_ENTITY_KINDS.contains(&entity.kind.as_str())
    {
        return Err(StorageError::WriterProtocol(format!(
            "LMWV-INFRA-RESERVED-ENTITY-KIND: entity kind {kind:?} is reserved by core; \
             plugin_id={plugin_id:?} cannot insert entity {id:?}",
            kind = entity.kind,
            plugin_id = entity.plugin_id,
            id = entity.id,
        )));
    }
    Ok(())
}

// Core-minted file entities are the single canonical source anchor. Module
// entities live below the file in the parent/contains chain, but may not stand
// in for the file-level identity.
const SOURCE_FILE_ANCHOR_KINDS: &[&str] = &["file"];

fn validate_source_file_anchor(
    conn: &Connection,
    source_file_id: Option<&str>,
    context: &str,
) -> Result<()> {
    let Some(source_file_id) = source_file_id else {
        return Ok(());
    };
    let kind = conn
        .query_row(
            "SELECT kind FROM entities WHERE id = ?1",
            params![source_file_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => StorageError::WriterProtocol(format!(
                "LMWV-INFRA-SOURCE-FILE-MISSING: {context} {source_file_id:?} \
                 does not reference an existing entity"
            )),
            other => StorageError::Sqlite(other),
        })?;
    if !SOURCE_FILE_ANCHOR_KINDS.contains(&kind.as_str()) {
        let allowed = SOURCE_FILE_ANCHOR_KINDS;
        return Err(StorageError::WriterProtocol(format!(
            "LMWV-INFRA-SOURCE-FILE-KIND-CONTRACT: {context} {source_file_id:?} \
             MUST reference a source-anchor entity with kind in {allowed:?}; \
             got kind={kind:?}"
        )));
    }
    Ok(())
}

fn validate_entity_source_file_anchor(conn: &Connection, entity: &EntityRecord) -> Result<()> {
    let Some(source_file_id) = entity.source_file_id.as_deref() else {
        return Ok(());
    };
    if source_file_id == entity.id {
        if SOURCE_FILE_ANCHOR_KINDS.contains(&entity.kind.as_str()) {
            return Ok(());
        }
        let allowed = SOURCE_FILE_ANCHOR_KINDS;
        return Err(StorageError::WriterProtocol(format!(
            "LMWV-INFRA-SOURCE-FILE-KIND-CONTRACT: InsertEntity source_file_id {source_file_id:?} \
             MUST reference a source-anchor entity with kind in {allowed:?}; \
             got kind={:?}",
            entity.kind
        )));
    }
    validate_source_file_anchor(conn, Some(source_file_id), "InsertEntity source_file_id")
}

/// 10 ontology-defined edge kinds (ADR-026 + ADR-028). Unknown kinds reaching the
/// writer are a manifest/wire-version drift bug — reject strictly.
const STRUCTURAL_EDGE_KINDS: &[&str] = &["contains", "in_subsystem", "guides", "emits_finding"];
const ANCHORED_EDGE_KINDS: &[&str] = &[
    "calls",
    "references",
    "imports",
    "implements",
    "decorates",
    "inherits_from",
    "derives",
];

pub fn known_scan_time_edge_kinds() -> impl Iterator<Item = &'static str> {
    STRUCTURAL_EDGE_KINDS
        .iter()
        .chain(ANCHORED_EDGE_KINDS.iter())
        .copied()
}

/// Enforce the per-kind confidence + source-range contract documented in
/// `docs/implementation/sprint-2/b3-contains-edges.md` §3 Q5 and ADR-026
/// decision 3, extended by ADR-028. Returns a
/// [`StorageError::WriterProtocol`] whose message embeds
/// `LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT` (per-kind confidence mismatch),
/// `LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT` (structural/anchored mismatch), or
/// `LMWV-INFRA-EDGE-UNKNOWN-KIND` (kind not in the ontology),
/// so the surrounding `runs.stats.failure_reason` carries the code.
fn enforce_edge_contract(edge: &EdgeRecord) -> Result<()> {
    let has_start = edge.source_byte_start.is_some();
    let has_end = edge.source_byte_end.is_some();
    let has_any_range = has_start || has_end;
    if STRUCTURAL_EDGE_KINDS.contains(&edge.kind.as_str()) {
        if edge.confidence != EdgeConfidence::Resolved {
            return Err(StorageError::WriterProtocol(format!(
                "LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT: structural edge kind {kind:?} \
                 MUST carry confidence=resolved; got confidence={confidence:?} \
                 for ({from} -> {to})",
                kind = edge.kind,
                confidence = edge.confidence,
                from = edge.from_id,
                to = edge.to_id,
            )));
        }
        if has_any_range {
            return Err(StorageError::WriterProtocol(format!(
                "LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT: edge kind {kind:?} \
                 MUST have NULL source_byte_start/end; got start={start:?} end={end:?} \
                 for ({from} -> {to})",
                kind = edge.kind,
                start = edge.source_byte_start,
                end = edge.source_byte_end,
                from = edge.from_id,
                to = edge.to_id,
            )));
        }
    } else if ANCHORED_EDGE_KINDS.contains(&edge.kind.as_str()) {
        if edge.confidence == EdgeConfidence::Inferred {
            return Err(StorageError::WriterProtocol(format!(
                "LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT: inferred-tier edges are \
                 query-time-only at scan time; got confidence=inferred for \
                 anchored edge kind {kind:?} ({from} -> {to})",
                kind = edge.kind,
                from = edge.from_id,
                to = edge.to_id,
            )));
        }
        if !has_start || !has_end {
            return Err(StorageError::WriterProtocol(format!(
                "LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT: edge kind {kind:?} \
                 MUST have Some source_byte_start AND source_byte_end; got \
                 start={start:?} end={end:?} for ({from} -> {to})",
                kind = edge.kind,
                start = edge.source_byte_start,
                end = edge.source_byte_end,
                from = edge.from_id,
                to = edge.to_id,
            )));
        }
    } else {
        return Err(StorageError::WriterProtocol(format!(
            "LMWV-INFRA-EDGE-UNKNOWN-KIND: edge kind {kind:?} not in the writer \
             ontology; known kinds: {structural:?} + {anchored:?}",
            kind = edge.kind,
            structural = STRUCTURAL_EDGE_KINDS,
            anchored = ANCHORED_EDGE_KINDS,
        )));
    }
    Ok(())
}

fn insert_edge(
    conn: &mut Connection,
    state: &mut ActorState,
    edge: &EdgeRecord,
    commits_observed: &AtomicUsize,
    dropped_edges_total: &AtomicUsize,
    ambiguous_edges_total: &AtomicUsize,
) -> Result<()> {
    if state.current_run.is_none() {
        return Err(StorageError::WriterProtocol(
            "InsertEdge received without a preceding BeginRun".to_owned(),
        ));
    }
    if let Err(err) = enforce_edge_contract(edge) {
        dropped_edges_total.fetch_add(1, Ordering::Relaxed);
        return Err(err);
    }
    if !state.in_tx {
        begin_write_tx(conn, state)?;
        state.in_tx = true;
    }
    validate_source_file_anchor(
        conn,
        edge.source_file_id.as_deref(),
        "InsertEdge source_file_id",
    )?;
    conn.execute(
        "INSERT INTO edges ( \
            kind, from_id, to_id, properties, source_file_id, \
            source_byte_start, source_byte_end, confidence \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(kind, from_id, to_id) DO UPDATE SET \
            properties = excluded.properties, \
            source_file_id = excluded.source_file_id, \
            source_byte_start = excluded.source_byte_start, \
            source_byte_end = excluded.source_byte_end, \
            confidence = excluded.confidence",
        params![
            edge.kind,
            edge.from_id,
            edge.to_id,
            edge.properties_json,
            edge.source_file_id,
            edge.source_byte_start,
            edge.source_byte_end,
            edge.confidence.as_str(),
        ],
    )?;
    if edge.confidence == EdgeConfidence::Ambiguous {
        ambiguous_edges_total.fetch_add(1, Ordering::Relaxed);
    }
    bump_writes_and_maybe_commit(conn, state, commits_observed)?;
    Ok(())
}

fn replace_anchored_edges_for_source_file(
    conn: &mut Connection,
    state: &mut ActorState,
    source_file_id: &str,
    commits_observed: &AtomicUsize,
) -> Result<()> {
    if state.current_run.is_none() {
        return Err(StorageError::WriterProtocol(
            "ReplaceAnchoredEdgesForSourceFile received without a preceding BeginRun".to_owned(),
        ));
    }
    if !state.in_tx {
        begin_write_tx(conn, state)?;
        state.in_tx = true;
    }
    validate_source_file_anchor(
        conn,
        Some(source_file_id),
        "ReplaceAnchoredEdgesForSourceFile source_file_id",
    )?;
    for kind in ANCHORED_EDGE_KINDS {
        conn.execute(
            "DELETE FROM edges WHERE source_file_id = ?1 AND kind = ?2",
            params![source_file_id, kind],
        )?;
    }
    bump_writes_and_maybe_commit(conn, state, commits_observed)?;
    Ok(())
}

fn insert_finding(
    conn: &mut Connection,
    state: &mut ActorState,
    finding: &FindingRecord,
    commits_observed: &AtomicUsize,
) -> Result<()> {
    if state.current_run.is_none() {
        return Err(StorageError::WriterProtocol(
            "InsertFinding received without a preceding BeginRun".to_owned(),
        ));
    }
    if !state.in_tx {
        begin_write_tx(conn, state)?;
        state.in_tx = true;
    }
    write_finding_row(conn, finding)?;
    bump_writes_and_maybe_commit(conn, state, commits_observed)?;
    Ok(())
}

/// The raw `findings` upsert, with no run-state or transaction management — both
/// the run-scoped [`insert_finding`] (inside the open run tx) and the post-run
/// path (REQ-ANALYZE-04 deletion findings, emitted after `CommitRun` via
/// `query_time_write`) share this so the SQL has a single home.
fn write_finding_row(conn: &Connection, finding: &FindingRecord) -> Result<()> {
    // ON CONFLICT(id) DO UPDATE makes the finding path idempotent across BOTH a
    // `--resume` re-walk and a fresh re-analyze. A finding id is keyed on its
    // CONTENT (`core:finding:<discriminator>`, e.g. the anchor entity + rule +
    // evidence hash) and NOT on run_id (L1 fix, clarion-772ff358da / ADR-047):
    // the same logical finding regenerates the same id every run, so the upsert
    // refreshes it in place instead of inserting a duplicate. The run_id *column*
    // updates to the latest run (`run_id = excluded.run_id`), so `findings_for_emit`
    // (WHERE run_id = current) still returns exactly the reproduced set. The
    // conflict clause refreshes analysis-derived columns from the re-walk but
    // PRESERVES the lifecycle columns (`status`, `suppression_reason`,
    // `filigree_issue_id`) and `created_at` — so a finding's Filigree linkage and
    // suppression now SURVIVE re-analysis (a run_id-scoped id used to orphan them
    // by minting a fresh row each run). Same first-seen-preserving discipline
    // `insert_entity` applies.
    conn.execute(
        "INSERT INTO findings ( \
            id, tool, tool_version, run_id, rule_id, kind, severity, confidence, \
            confidence_basis, entity_id, related_entities, message, evidence, \
            properties, supports, supported_by, status, suppression_reason, \
            filigree_issue_id, created_at, updated_at \
         ) VALUES ( \
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, \
            ?9, ?10, ?11, ?12, ?13, \
            ?14, ?15, ?16, 'open', NULL, \
            NULL, ?17, ?18 \
         ) \
         ON CONFLICT(id) DO UPDATE SET \
            tool = excluded.tool, \
            tool_version = excluded.tool_version, \
            run_id = excluded.run_id, \
            rule_id = excluded.rule_id, \
            kind = excluded.kind, \
            severity = excluded.severity, \
            confidence = excluded.confidence, \
            confidence_basis = excluded.confidence_basis, \
            entity_id = excluded.entity_id, \
            related_entities = excluded.related_entities, \
            message = excluded.message, \
            evidence = excluded.evidence, \
            properties = excluded.properties, \
            supports = excluded.supports, \
            supported_by = excluded.supported_by, \
            updated_at = excluded.updated_at",
        params![
            finding.id,
            finding.tool,
            finding.tool_version,
            finding.run_id,
            finding.rule_id,
            finding.kind,
            finding.severity,
            finding.confidence,
            finding.confidence_basis,
            finding.entity_id,
            finding.related_entities_json,
            finding.message,
            finding.evidence_json,
            finding.properties_json,
            finding.supports_json,
            finding.supported_by_json,
            finding.created_at,
            finding.updated_at,
        ],
    )?;
    Ok(())
}

fn insert_inferred_edges(
    conn: &Connection,
    cache_entry: &InferredEdgeCacheEntry,
    edges: &[InferredCallEdgeRecord],
) -> Result<InferredEdgeWriteStats> {
    upsert_inferred_edge_cache(conn, cache_entry)?;
    let cache_key = inferred_edge_cache_key_id(&cache_entry.key);
    conn.execute(
        "DELETE FROM edges \
         WHERE kind = 'calls' \
           AND from_id = ?1 \
           AND confidence = 'inferred' \
           AND COALESCE(json_extract(properties, '$.inference_cache_key'), '') <> ?2",
        params![cache_entry.key.caller_entity_id, cache_key],
    )?;

    let mut stats = InferredEdgeWriteStats {
        inserted_edges: 0,
        skipped_static_duplicates: 0,
    };
    for edge in edges {
        validate_inferred_edge(edge)?;
        validate_source_file_anchor(
            conn,
            edge.source_file_id.as_deref(),
            "InsertInferredEdges source_file_id",
        )?;
        if static_call_edge_exists(conn, &edge.from_id, &edge.to_id)? {
            stats.skipped_static_duplicates += 1;
            continue;
        }
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO edges ( \
                kind, from_id, to_id, confidence, properties, source_file_id, \
                source_byte_start, source_byte_end \
             ) VALUES ('calls', ?1, ?2, 'inferred', ?3, ?4, ?5, ?6)",
            params![
                edge.from_id,
                edge.to_id,
                edge.properties_json,
                edge.source_file_id,
                edge.source_byte_start,
                edge.source_byte_end,
            ],
        )?;
        stats.inserted_edges += u64::try_from(inserted).unwrap_or(u64::MAX);
    }
    Ok(stats)
}

fn validate_inferred_edge(edge: &InferredCallEdgeRecord) -> Result<()> {
    if edge.from_id.is_empty() || edge.to_id.is_empty() {
        return Err(StorageError::WriterProtocol(
            "InsertInferredEdges requires non-empty from_id and to_id".to_owned(),
        ));
    }
    if edge.source_byte_start < 0 || edge.source_byte_end <= edge.source_byte_start {
        return Err(StorageError::WriterProtocol(
            "InsertInferredEdges requires a non-empty source byte range".to_owned(),
        ));
    }
    if serde_json::from_str::<serde_json::Value>(&edge.properties_json).is_err() {
        return Err(StorageError::WriterProtocol(
            "InsertInferredEdges properties_json must be valid JSON".to_owned(),
        ));
    }
    Ok(())
}

fn static_call_edge_exists(conn: &Connection, from_id: &str, to_id: &str) -> Result<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS ( \
            SELECT 1 FROM edges \
            WHERE kind = 'calls' \
              AND from_id = ?1 \
              AND to_id = ?2 \
              AND confidence IN ('resolved', 'ambiguous') \
         )",
        params![from_id, to_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(exists != 0)
}

fn replace_unresolved_call_sites_in_run(
    conn: &mut Connection,
    state: &mut ActorState,
    caller_entity_id: &str,
    caller_content_hash: &str,
    sites: &[crate::unresolved::UnresolvedCallSiteRecord],
    commits_observed: &AtomicUsize,
) -> Result<()> {
    if state.current_run.is_none() {
        return Err(StorageError::WriterProtocol(
            "ReplaceUnresolvedCallSitesForCaller received without a preceding BeginRun".to_owned(),
        ));
    }
    if !state.in_tx {
        begin_write_tx(conn, state)?;
        state.in_tx = true;
    }
    for site in sites {
        validate_source_file_anchor(
            conn,
            site.source_file_id.as_deref(),
            "ReplaceUnresolvedCallSitesForCaller source_file_id",
        )?;
    }
    replace_unresolved_call_sites_for_caller(conn, caller_entity_id, caller_content_hash, sites)?;
    bump_writes_and_maybe_commit(conn, state, commits_observed)?;
    Ok(())
}

/// Shared post-write bookkeeping: increment the batch counter and, if the
/// batch boundary is crossed, COMMIT and re-open. State transitions happen
/// BEFORE the fallible COMMIT — `SQLite` aborts the transaction on COMMIT
/// failure regardless, so setting `in_tx=false` first keeps our state
/// conservatively correct if the COMMIT errors.
fn bump_writes_and_maybe_commit(
    conn: &mut Connection,
    state: &mut ActorState,
    commits_observed: &AtomicUsize,
) -> Result<()> {
    state.writes_in_batch += 1;
    if state.writes_in_batch >= state.batch_size {
        state.writes_in_batch = 0;
        state.in_tx = false;
        refresh_current_run_heartbeat(conn, state)?;
        conn.execute_batch("COMMIT")?;
        commits_observed.fetch_add(1, Ordering::Relaxed);
        // Open the next batch eagerly so the next write doesn't pay
        // another `BEGIN` round-trip.
        begin_write_tx(conn, state)?;
        state.in_tx = true;
    }
    Ok(())
}

fn refresh_current_run_heartbeat(conn: &Connection, state: &ActorState) -> Result<()> {
    let Some(run_id) = state.current_run.as_deref() else {
        return Ok(());
    };
    conn.execute(
        "UPDATE runs \
            SET heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), owner_pid = ?1 \
          WHERE id = ?2",
        params![owner_pid(), run_id],
    )?;
    Ok(())
}

fn flush_run_batch(
    conn: &mut Connection,
    state: &mut ActorState,
    commits_observed: &AtomicUsize,
) -> Result<()> {
    if state.current_run.is_none() {
        return Err(StorageError::WriterProtocol(
            "FlushRunBatch received without a preceding BeginRun".to_owned(),
        ));
    }
    if let Some(mismatch) = parent_contains_mismatch(conn)? {
        if state.in_tx {
            let _ = conn.execute_batch("ROLLBACK");
            state.in_tx = false;
            state.writes_in_batch = 0;
        }
        return Err(StorageError::WriterProtocol(mismatch));
    }
    if state.in_tx {
        state.in_tx = false;
        state.writes_in_batch = 0;
        refresh_current_run_heartbeat(conn, state)?;
        conn.execute_batch("COMMIT")?;
        commits_observed.fetch_add(1, Ordering::Relaxed);
    }
    begin_write_tx(conn, state)?;
    state.in_tx = true;
    Ok(())
}

fn query_time_write<T>(
    conn: &mut Connection,
    state: &mut ActorState,
    commits_observed: &AtomicUsize,
    write: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let reopen_run_transaction = state.current_run.is_some();
    if state.in_tx {
        state.in_tx = false;
        state.writes_in_batch = 0;
        refresh_current_run_heartbeat(conn, state)?;
        conn.execute_batch("COMMIT")?;
        commits_observed.fetch_add(1, Ordering::Relaxed);
    }

    let result = write(conn);

    if reopen_run_transaction {
        begin_write_tx(conn, state)?;
        state.in_tx = true;
    }

    result
}

fn commit_run(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    status: RunStatus,
    completed_at: &str,
    stats_json: &str,
    commits_observed: &AtomicUsize,
) -> Result<()> {
    ensure_current_run_matches(state, "CommitRun", run_id)?;
    // The run-row UPDATE and the final write-batch COMMIT must be atomic,
    // otherwise a crash or SQL error between them would leave entities/edges
    // durable but `runs.status = 'running'` — indistinguishable from an
    // in-progress run.
    if state.in_tx {
        // A write batch is open: run the B.3 parent-id consistency check
        // inside the transaction so a failure rolls back this run's writes,
        // then fold the UPDATE in and commit once.
        if let Some(mismatch) = parent_contains_mismatch(conn)? {
            let _ = conn.execute_batch("ROLLBACK");
            state.in_tx = false;
            state.writes_in_batch = 0;
            // The run row was inserted in BeginRun's auto-committed write;
            // re-mark it failed under a separate implicit transaction.
            let failure_stats = serde_json::json!({
                "failure_reason": mismatch.clone(),
            })
            .to_string();
            let changed = conn.execute(
                "UPDATE runs \
                    SET status = 'failed', completed_at = ?1, stats = ?2, owner_pid = NULL \
                  WHERE id = ?3",
                params![completed_at, failure_stats, run_id],
            )?;
            if let Err(err) = ensure_run_update_changed_one(changed, run_id) {
                state.current_run = None;
                return Err(err);
            }
            state.current_run = None;
            return Err(StorageError::WriterProtocol(mismatch));
        }
        let changed = conn.execute(
            "UPDATE runs \
                SET status = ?1, completed_at = ?2, stats = ?3, owner_pid = NULL \
              WHERE id = ?4",
            params![status.as_str(), completed_at, stats_json, run_id],
        )?;
        if let Err(err) = ensure_run_update_changed_one(changed, run_id) {
            let _ = conn.execute_batch("ROLLBACK");
            state.in_tx = false;
            state.current_run = None;
            state.writes_in_batch = 0;
            return Err(err);
        }
        state.in_tx = false;
        conn.execute_batch("COMMIT")?;
        commits_observed.fetch_add(1, Ordering::Relaxed);
    } else {
        // No write batch open (e.g. SkippedNoPlugins path, or every batch
        // already committed at a boundary). A single-statement UPDATE is
        // atomic under SQLite's implicit transaction. No entities/edges were
        // staged-and-not-committed, so the parent-id check has nothing to
        // catch that would change the durable state.
        let changed = conn.execute(
            "UPDATE runs \
                SET status = ?1, completed_at = ?2, stats = ?3, owner_pid = NULL \
              WHERE id = ?4",
            params![status.as_str(), completed_at, stats_json, run_id],
        )?;
        if let Err(err) = ensure_run_update_changed_one(changed, run_id) {
            state.current_run = None;
            state.writes_in_batch = 0;
            return Err(err);
        }
    }
    state.current_run = None;
    state.writes_in_batch = 0;
    Ok(())
}

/// B.3 §5 parent-id consistency check (dual-encoding enforcement, ADR-026
/// decision 2). Runs inside the open write transaction at `CommitRun` time.
/// Returns `Ok(None)` when consistent; `Ok(Some(msg))` carrying the
/// `LMWV-INFRA-PARENT-CONTAINS-MISMATCH` finding code when not.
fn parent_contains_mismatch(conn: &Connection) -> Result<Option<String>> {
    // Direction 1: every entity.parent_id has a matching contains edge.
    if let Some((eid, parent, ce_from)) = conn
        .query_row(
            "SELECT e.id, e.parent_id, ce.from_id \
             FROM entities e \
             LEFT JOIN edges ce \
               ON ce.kind = 'contains' AND ce.to_id = e.id \
             WHERE e.parent_id IS NOT NULL \
               AND (ce.from_id IS NULL OR ce.from_id != e.parent_id) \
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?
    {
        return Ok(Some(format!(
            "LMWV-INFRA-PARENT-CONTAINS-MISMATCH: entity {eid:?} declares \
             parent_id={parent:?} but no matching `contains` edge exists \
             (closest contains.from_id={ce_from:?})"
        )));
    }
    // Direction 2: every contains edge has a matching child parent_id.
    if let Some((from, to, parent)) = conn
        .query_row(
            "SELECT ce.from_id, ce.to_id, e.parent_id \
             FROM edges ce \
             JOIN entities e ON e.id = ce.to_id \
             WHERE ce.kind = 'contains' \
               AND (e.parent_id IS NULL OR e.parent_id != ce.from_id) \
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?
    {
        return Ok(Some(format!(
            "LMWV-INFRA-PARENT-CONTAINS-MISMATCH: contains edge \
             ({from:?} -> {to:?}) has no matching child parent_id \
             (child.parent_id={parent:?})"
        )));
    }
    Ok(None)
}

fn fail_run(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    reason: &str,
    completed_at: &str,
) -> Result<()> {
    ensure_current_run_matches(state, "FailRun", run_id)?;
    if state.in_tx {
        let _ = conn.execute_batch("ROLLBACK");
        state.in_tx = false;
    }
    let stats_json = serde_json::json!({ "failure_reason": reason }).to_string();
    let changed = conn.execute(
        "UPDATE runs \
            SET status = 'failed', completed_at = ?1, stats = ?2, owner_pid = NULL \
          WHERE id = ?3",
        params![completed_at, stats_json, run_id],
    )?;
    if let Err(err) = ensure_run_update_changed_one(changed, run_id) {
        state.current_run = None;
        state.writes_in_batch = 0;
        return Err(err);
    }
    state.current_run = None;
    state.writes_in_batch = 0;
    Ok(())
}

fn ensure_current_run_matches(
    state: &ActorState,
    command: &'static str,
    run_id: &str,
) -> Result<()> {
    match state.current_run.as_deref() {
        Some(current) if current == run_id => Ok(()),
        Some(current) => Err(StorageError::WriterProtocol(format!(
            "{command} run_id={run_id:?} does not match active run_id={current:?}",
        ))),
        None => Err(StorageError::WriterProtocol(format!(
            "{command} received without a preceding BeginRun",
        ))),
    }
}

fn ensure_run_update_changed_one(changed: usize, run_id: &str) -> Result<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(StorageError::WriterProtocol(format!(
            "UPDATE runs affected {changed} rows for run_id={run_id}",
        )))
    }
}

#[cfg(test)]
mod run_lifecycle_failpoint_tests {
    //! Deterministic, single-threaded coverage for the `begin_run` / `resume_run`
    //! TOCTOU repair paths (reviews #4 / #15). The competing write lock is held
    //! and released through `begin_run_inner` / `resume_run_inner`'s two test
    //! seams, so the failure window is hit without threads or wall-clock races.

    use std::time::Duration;

    use rusqlite::Connection;

    use super::{ActorState, begin_run_inner, resume_run_inner};
    use crate::error::StorageError;
    use crate::schema;

    /// On-disk DB (BEGIN IMMEDIATE needs a real file to contend on) with the
    /// busy handler disabled so contention surfaces as an immediate `SQLITE_BUSY`.
    fn migrated_conn(path: &std::path::Path) -> Connection {
        let mut conn = Connection::open(path).expect("open");
        conn.busy_timeout(Duration::from_millis(0))
            .expect("busy_timeout");
        schema::apply_migrations(&mut conn).expect("apply_migrations");
        conn
    }

    /// A writer state whose write-tx acquire fails fast (single attempt, no
    /// backoff) so a held competing lock trips it immediately.
    fn fastfail_state() -> ActorState {
        let mut state = ActorState::new(50);
        state.retry_policy = crate::retry::RetryPolicy {
            max_attempts: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        };
        state
    }

    fn run_status(conn: &Connection, run_id: &str) -> Option<String> {
        conn.query_row("SELECT status FROM runs WHERE id = ?1", [run_id], |row| {
            row.get::<_, String>(0)
        })
        .ok()
    }

    #[test]
    fn begin_run_marks_row_failed_when_write_tx_cannot_be_acquired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let mut conn = migrated_conn(&path);
        // Second connection to the same DB; grabs the write lock in the TOCTOU
        // window so begin_write_tx busies out.
        let competitor = migrated_conn(&path);
        let mut state = fastfail_state();

        let err = begin_run_inner(
            &mut conn,
            &mut state,
            "run-toctou",
            "{}",
            "2026-01-01T00:00:00.000Z",
            None,
            // after_insert_committed: the `running` row is now durable; grab the
            // write lock so the upcoming begin_write_tx fails.
            |_| {
                competitor
                    .execute_batch("BEGIN IMMEDIATE")
                    .expect("competitor takes the write lock");
            },
            // on_write_tx_failed: release the lock so the best-effort cleanup
            // UPDATE can re-acquire it.
            |_| {
                competitor
                    .execute_batch("COMMIT")
                    .expect("competitor releases the write lock");
            },
        )
        .expect_err("begin_write_tx must fail while the competitor holds the lock");

        assert!(
            matches!(err, StorageError::Sqlite(_)),
            "expected a busy SQLite error, got {err:?}"
        );
        assert_eq!(
            run_status(&conn, "run-toctou").as_deref(),
            Some("failed"),
            "a stranded 'running' row must be repaired to 'failed', not left phantom-running"
        );
        assert!(
            state.current_run.is_none(),
            "current_run must stay unset on the failure path"
        );
    }

    #[test]
    fn resume_run_restores_prior_status_when_write_tx_cannot_be_acquired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let mut conn = migrated_conn(&path);
        // Pre-seed a previously-completed run.
        conn.execute(
            "INSERT INTO runs ( \
                id, started_at, completed_at, config, stats, status, analyzed_at_commit, \
                owner_pid, heartbeat_at \
             ) VALUES (?1, ?2, ?3, '{}', '{}', 'completed', NULL, NULL, ?2)",
            rusqlite::params![
                "run-resume",
                "2026-01-01T00:00:00.000Z",
                "2026-01-01T00:05:00.000Z"
            ],
        )
        .unwrap();

        let competitor = migrated_conn(&path);
        let mut state = fastfail_state();

        let err = resume_run_inner(
            &mut conn,
            &mut state,
            "run-resume",
            |_| {
                competitor
                    .execute_batch("BEGIN IMMEDIATE")
                    .expect("competitor takes the write lock");
            },
            |_| {
                competitor
                    .execute_batch("COMMIT")
                    .expect("competitor releases the write lock");
            },
        )
        .expect_err("begin_write_tx must fail while the competitor holds the lock");

        assert!(
            matches!(err, StorageError::Sqlite(_)),
            "expected a busy SQLite error, got {err:?}"
        );
        let (status, completed_at): (String, Option<String>) = conn
            .query_row(
                "SELECT status, completed_at FROM runs WHERE id = ?1",
                ["run-resume"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            status, "completed",
            "a pre-existing completed run must be restored, not left flipped to 'running'"
        );
        assert_eq!(
            completed_at.as_deref(),
            Some("2026-01-01T00:05:00.000Z"),
            "completed_at must be restored to its prior value"
        );
        assert!(state.current_run.is_none());
        // owner_pid sanity: restored row is unowned.
        let owner: Option<i64> = conn
            .query_row(
                "SELECT owner_pid FROM runs WHERE id = ?1",
                ["run-resume"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(owner.is_none(), "restored run must be unowned");
    }
}
