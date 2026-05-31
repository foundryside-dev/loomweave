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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::{Connection, params};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::cache::{
    InferredEdgeCacheEntry, inferred_edge_cache_key_id, touch_summary_cache,
    upsert_inferred_edge_cache, upsert_summary_cache,
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
    /// Process-lifetime count of edges silently deduped or rejected by the writer.
    ///
    /// `InsertEdge` uses `INSERT OR IGNORE`; a UNIQUE conflict on
    /// `(kind, from_id, to_id)` increments this counter. Walking-skeleton
    /// e2e asserts this is zero post-analyze (B.3 §6). B.4* extends the
    /// counter to per-kind contract rejections so malformed plugin edges are
    /// visible in the same run stat.
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
        db_path: std::path::PathBuf,
        batch_size: usize,
        channel_capacity: usize,
    ) -> Result<(Self, JoinHandle<Result<()>>)> {
        let (tx, rx) = mpsc::channel(channel_capacity);
        let commits_observed = Arc::new(AtomicUsize::new(0));
        let dropped_edges_total = Arc::new(AtomicUsize::new(0));
        let ambiguous_edges_total = Arc::new(AtomicUsize::new(0));
        let commits_for_actor = commits_observed.clone();
        let dropped_for_actor = dropped_edges_total.clone();
        let ambiguous_for_actor = ambiguous_edges_total.clone();
        let handle = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut conn = Connection::open(&db_path)?;
            pragma::apply_write_pragmas(&conn)?;
            // STO-02: refuse a database whose `user_version` is strictly greater
            // than CURRENT_SCHEMA_VERSION. Equal/less are normal — equal is the
            // already-migrated steady state, less is handled by the migration
            // runner (which `install` calls before the writer ever spawns).
            schema::verify_user_version(&conn)?;
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
    /// Intended for use by `clarion analyze` (Task 7) and later WP
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
                ack,
            } => {
                reply(
                    ack,
                    begin_run(conn, &mut state, &run_id, &config_json, &started_at),
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
            WriterCmd::InsertFinding { finding, ack } => {
                let res = insert_finding(conn, &mut state, &finding, commits_observed);
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
            WriterCmd::UpsertWardlineTaintFact { fact, ack } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::wardline_taint::upsert_taint_fact(conn, &fact)
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
                stats = ?1 \
              WHERE id = ?2",
            params![stats_json, run_id],
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

fn begin_run(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    config_json: &str,
    started_at: &str,
) -> Result<()> {
    if state.current_run.is_some() {
        return Err(StorageError::WriterProtocol(
            "BeginRun received while a run is already in progress".to_owned(),
        ));
    }
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES (?1, ?2, NULL, ?3, '{}', 'running')",
        params![run_id, started_at, config_json],
    )?;
    begin_write_tx(conn, state)?;
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
    if state.current_run.is_some() {
        return Err(StorageError::WriterProtocol(
            "ResumeRun received while a run is already in progress".to_owned(),
        ));
    }
    let reopened = conn.execute(
        "UPDATE runs SET status = 'running', completed_at = NULL WHERE id = ?1",
        params![run_id],
    )?;
    if reopened == 0 {
        return Err(StorageError::WriterProtocol(format!(
            "ResumeRun: no run with id {run_id} to resume"
        )));
    }
    begin_write_tx(conn, state)?;
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
    // ON CONFLICT(id) DO UPDATE makes `clarion analyze` idempotent across runs:
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
    bump_writes_and_maybe_commit(conn, state, commits_observed)?;
    Ok(())
}

fn enforce_entity_kind_contract(entity: &EntityRecord) -> Result<()> {
    if entity.plugin_id != "core"
        && clarion_core::plugin::manifest::RESERVED_ENTITY_KINDS.contains(&entity.kind.as_str())
    {
        return Err(StorageError::WriterProtocol(format!(
            "CLA-INFRA-RESERVED-ENTITY-KIND: entity kind {kind:?} is reserved by core; \
             plugin_id={plugin_id:?} cannot insert entity {id:?}",
            kind = entity.kind,
            plugin_id = entity.plugin_id,
            id = entity.id,
        )));
    }
    Ok(())
}

// B.6 stores module ids as source anchors until core-minted `file` entities
// land; keep both accepted so the storage contract survives that handoff.
const SOURCE_FILE_ANCHOR_KINDS: &[&str] = &["file", "module"];

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
                "CLA-INFRA-SOURCE-FILE-MISSING: {context} {source_file_id:?} \
                 does not reference an existing entity"
            )),
            other => StorageError::Sqlite(other),
        })?;
    if !SOURCE_FILE_ANCHOR_KINDS.contains(&kind.as_str()) {
        let allowed = SOURCE_FILE_ANCHOR_KINDS;
        return Err(StorageError::WriterProtocol(format!(
            "CLA-INFRA-SOURCE-FILE-KIND-CONTRACT: {context} {source_file_id:?} \
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
            "CLA-INFRA-SOURCE-FILE-KIND-CONTRACT: InsertEntity source_file_id {source_file_id:?} \
             MUST reference a source-anchor entity with kind in {allowed:?}; \
             got kind={:?}",
            entity.kind
        )));
    }
    validate_source_file_anchor(conn, Some(source_file_id), "InsertEntity source_file_id")
}

/// 9 ontology-defined edge kinds (ADR-026 + ADR-028). Unknown kinds reaching the
/// writer are a manifest/wire-version drift bug — reject strictly.
const STRUCTURAL_EDGE_KINDS: &[&str] = &["contains", "in_subsystem", "guides", "emits_finding"];
const ANCHORED_EDGE_KINDS: &[&str] = &[
    "calls",
    "references",
    "imports",
    "decorates",
    "inherits_from",
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
/// `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT` (per-kind confidence mismatch),
/// `CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT` (structural/anchored mismatch), or
/// `CLA-INFRA-EDGE-UNKNOWN-KIND` (kind not in the ontology),
/// so the surrounding `runs.stats.failure_reason` carries the code.
fn enforce_edge_contract(edge: &EdgeRecord) -> Result<()> {
    let has_start = edge.source_byte_start.is_some();
    let has_end = edge.source_byte_end.is_some();
    let has_any_range = has_start || has_end;
    if STRUCTURAL_EDGE_KINDS.contains(&edge.kind.as_str()) {
        if edge.confidence != EdgeConfidence::Resolved {
            return Err(StorageError::WriterProtocol(format!(
                "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT: structural edge kind {kind:?} \
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
                "CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT: edge kind {kind:?} \
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
                "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT: inferred-tier edges are \
                 query-time-only at scan time; got confidence=inferred for \
                 anchored edge kind {kind:?} ({from} -> {to})",
                kind = edge.kind,
                from = edge.from_id,
                to = edge.to_id,
            )));
        }
        if !has_start || !has_end {
            return Err(StorageError::WriterProtocol(format!(
                "CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT: edge kind {kind:?} \
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
            "CLA-INFRA-EDGE-UNKNOWN-KIND: edge kind {kind:?} not in the writer \
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
    let changed = conn.execute(
        "INSERT OR IGNORE INTO edges ( \
            kind, from_id, to_id, properties, source_file_id, \
            source_byte_start, source_byte_end, confidence \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
    if changed == 0 {
        // UNIQUE conflict on (kind, from_id, to_id) — silent dedupe is the
        // idempotent-re-analyze contract (B.3 §6).
        dropped_edges_total.fetch_add(1, Ordering::Relaxed);
    } else if edge.confidence == EdgeConfidence::Ambiguous {
        ambiguous_edges_total.fetch_add(1, Ordering::Relaxed);
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
    // ON CONFLICT(id) DO UPDATE makes the finding path idempotent under
    // `--resume`: a finding id embeds its run_id (`core:finding:{run_id}:…`),
    // so cross-run ids never collide and a fresh run only ever INSERTs. A
    // resume re-walks under the *same* run_id and re-generates the same ids;
    // without the upsert it would fail on `UNIQUE constraint: findings.id`.
    // The conflict clause refreshes analysis-derived columns from the re-walk
    // but PRESERVES the lifecycle columns (`status`, `suppression_reason`,
    // `filigree_issue_id`) and `created_at` — the same first-seen-preserving
    // discipline `insert_entity` applies. (These lifecycle columns are never
    // mutated locally today; preserving them keeps that invariant if they are.)
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
    bump_writes_and_maybe_commit(conn, state, commits_observed)?;
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
        conn.execute_batch("COMMIT")?;
        commits_observed.fetch_add(1, Ordering::Relaxed);
        // Open the next batch eagerly so the next write doesn't pay
        // another `BEGIN` round-trip.
        begin_write_tx(conn, state)?;
        state.in_tx = true;
    }
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
                "UPDATE runs SET status = 'failed', completed_at = ?1, stats = ?2 \
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
            "UPDATE runs SET status = ?1, completed_at = ?2, stats = ?3 WHERE id = ?4",
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
            "UPDATE runs SET status = ?1, completed_at = ?2, stats = ?3 WHERE id = ?4",
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
/// `CLA-INFRA-PARENT-CONTAINS-MISMATCH` finding code when not.
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
            "CLA-INFRA-PARENT-CONTAINS-MISMATCH: entity {eid:?} declares \
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
            "CLA-INFRA-PARENT-CONTAINS-MISMATCH: contains edge \
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
        "UPDATE runs SET status = 'failed', completed_at = ?1, stats = ?2 WHERE id = ?3",
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
