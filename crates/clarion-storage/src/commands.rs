//! Writer-actor command protocol (L3 lock-in).
//!
//! Per ADR-011, every persistent mutation is a `WriterCmd` variant. The
//! writer task owns the sole `rusqlite::Connection`; callers enqueue
//! commands via a bounded `mpsc::Sender<WriterCmd>`. Each variant carries
//! a `oneshot::Sender` for the per-command ack (UQ-WP1-03 resolution).
//!
//! Sprint 1 shipped four variants: `BeginRun`, `InsertEntity`, `CommitRun`,
//! `FailRun`. B.3 adds `InsertEdge` (ADR-026). Later WPs add `InsertFinding`,
//! etc. by appending variants — the pattern is frozen here.

use tokio::sync::oneshot;

pub use clarion_core::EdgeConfidence;

use crate::cache::{InferredEdgeCacheEntry, SummaryCacheEntry, SummaryCacheKey};
use crate::error::StorageError;
use crate::unresolved::UnresolvedCallSiteRecord;

pub type Ack<T> = oneshot::Sender<Result<T, StorageError>>;

/// Run status values. Extended in later WPs; Sprint 1 uses only
/// `SkippedNoPlugins` (from `clarion analyze` without plugins wired) and
/// `Failed` (explicit `FailRun`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Sprint 1 stub: analyze invoked with no plugins registered.
    SkippedNoPlugins,
    /// Normal successful completion.
    Completed,
    /// Explicit failure via `FailRun`.
    Failed,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::SkippedNoPlugins => "skipped_no_plugins",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
        }
    }
}

/// Plain-old-data entity record as seen by the writer. Content-hash and
/// timestamps are supplied by callers; the writer does not compute them.
#[derive(Debug, Clone)]
pub struct EntityRecord {
    pub id: String,
    pub plugin_id: String,
    pub kind: String,
    pub name: String,
    pub short_name: String,
    pub parent_id: Option<String>,
    pub source_file_id: Option<String>,
    pub source_file_path: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
    pub source_line_start: Option<i64>,
    pub source_line_end: Option<i64>,
    /// JSON string; writer inserts verbatim.
    pub properties_json: String,
    pub content_hash: Option<String>,
    pub summary_json: Option<String>,
    pub wardline_json: Option<String>,
    pub first_seen_commit: Option<String>,
    pub last_seen_commit: Option<String>,
    /// ISO-8601 UTC; writer inserts verbatim.
    pub created_at: String,
    pub updated_at: String,
}

/// Plain-old-data edge record as seen by the writer. Per ADR-026 the
/// natural key is `(kind, from_id, to_id)`. `source_byte_start`/`end` are
/// kind-dispatched (NULL for structural edges like `contains`; required for
/// AST-anchored edges like `calls`); the writer enforces the per-kind
/// contract on `InsertEdge`.
#[derive(Debug, Clone)]
pub struct EdgeRecord {
    pub kind: String,
    pub from_id: String,
    pub to_id: String,
    pub confidence: EdgeConfidence,
    /// JSON string; writer inserts verbatim. None ⇒ NULL.
    pub properties_json: Option<String>,
    /// Module entity id for the file the edge was emitted from. Derived by
    /// the host, not the plugin (ADR-022 boundary).
    pub source_file_id: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct InferredCallEdgeRecord {
    pub from_id: String,
    pub to_id: String,
    pub source_file_id: Option<String>,
    pub source_byte_start: i64,
    pub source_byte_end: i64,
    pub properties_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferredEdgeWriteStats {
    pub inserted_edges: u64,
    pub skipped_static_duplicates: u64,
}

/// Plain-old-data finding record as seen by the writer. JSON-typed fields are
/// serialized by the caller and inserted verbatim; lifecycle status is owned by
/// the writer and starts as `open`.
#[derive(Debug, Clone)]
pub struct FindingRecord {
    pub id: String,
    pub tool: String,
    pub tool_version: String,
    pub run_id: String,
    pub rule_id: String,
    pub kind: String,
    pub severity: String,
    pub confidence: Option<f64>,
    pub confidence_basis: Option<String>,
    pub entity_id: String,
    pub related_entities_json: String,
    pub message: String,
    pub evidence_json: String,
    pub properties_json: String,
    pub supports_json: String,
    pub supported_by_json: String,
    /// ISO-8601 UTC; writer inserts verbatim.
    pub created_at: String,
    pub updated_at: String,
}

/// All writer operations as a single enum so the actor loop exhausts
/// everything via one match.
#[derive(Debug)]
pub enum WriterCmd {
    /// Open a new run. The writer inserts a row into `runs` with status
    /// `running`, begins an implicit transaction on the entities write
    /// path, and binds `run_id` into its state.
    BeginRun {
        run_id: String,
        config_json: String,
        started_at: String,
        ack: Ack<()>,
    },
    /// Reopen an existing run row for the `--resume` path (REQ-FINDING-05).
    /// `BeginRun` does an `INSERT` that conflicts on the run PK when handed an
    /// id that already exists; `ResumeRun` instead `UPDATE`s the row back to
    /// `running` (clearing `completed_at`), then binds it as the active run and
    /// opens the write transaction exactly as `BeginRun` does. Errors if no row
    /// with `run_id` exists. A re-walk upserts entities/edges idempotently, so
    /// a resumed run reproduces the same durable graph as the original.
    ResumeRun { run_id: String, ack: Ack<()> },
    /// Insert an entity; also advances the per-batch write counter and
    /// commits the in-flight transaction if the batch boundary is crossed.
    InsertEntity {
        entity: Box<EntityRecord>,
        ack: Ack<()>,
    },
    /// Insert an edge under the natural PK `(kind, from_id, to_id)`. The
    /// writer enforces the per-kind source-range contract (ADR-026) and
    /// silently dedupes UNIQUE conflicts via `INSERT OR IGNORE`, incrementing
    /// `Writer::dropped_edges_total` on dedupe. Also advances the per-batch
    /// write counter — edges and entities share one batch boundary.
    InsertEdge { edge: Box<EdgeRecord>, ack: Ack<()> },
    /// Insert one finding. The writer initializes lifecycle status to `open`
    /// and leaves suppression / Filigree-link fields empty. Idempotent on
    /// `id` (ON CONFLICT DO UPDATE): a `--resume` re-walk regenerates the same
    /// run-scoped finding ids and refreshes the analysis-derived columns while
    /// preserving `created_at` and the lifecycle columns.
    InsertFinding {
        finding: Box<FindingRecord>,
        ack: Ack<()>,
    },
    /// Commit the current analyze batch and reopen it so readers on separate
    /// `SQLite` connections can observe graph rows before `CommitRun`.
    FlushRunBatch { ack: Ack<()> },
    /// Upsert one inferred-edge cache row and materialize its current inferred
    /// call edges. This query-time MCP write does not require an active
    /// analyze run and does not use scan-time edge contracts.
    InsertInferredEdges {
        cache_entry: Box<InferredEdgeCacheEntry>,
        edges: Vec<InferredCallEdgeRecord>,
        ack: Ack<InferredEdgeWriteStats>,
    },
    /// Upsert one on-demand summary cache row. This query-time MCP write does
    /// not require an active analyze run.
    UpsertSummaryCache {
        entry: Box<SummaryCacheEntry>,
        ack: Ack<()>,
    },
    /// Touch one on-demand summary cache row. Returns whether a row was
    /// updated. This query-time MCP write does not require an active analyze
    /// run.
    TouchSummaryCache {
        key: SummaryCacheKey,
        last_accessed_at: String,
        ack: Ack<bool>,
    },
    /// Replace all unresolved call-site rows for one caller. This is an
    /// analyze-time mapping command that requires an active run transaction so
    /// stale rows from previous content hashes cannot survive re-analysis.
    ReplaceUnresolvedCallSitesForCaller {
        caller_entity_id: String,
        caller_content_hash: String,
        sites: Vec<UnresolvedCallSiteRecord>,
        ack: Ack<()>,
    },
    /// Commit the in-flight transaction, update the run row to the given
    /// terminal status + `completed_at` + `stats_json`, and clear per-run
    /// state.
    CommitRun {
        run_id: String,
        status: RunStatus,
        completed_at: String,
        stats_json: String,
        ack: Ack<()>,
    },
    /// Roll back the in-flight transaction, update the run row to
    /// `failed`, and clear per-run state.
    FailRun {
        run_id: String,
        reason: String,
        completed_at: String,
        ack: Ack<()>,
    },
}
