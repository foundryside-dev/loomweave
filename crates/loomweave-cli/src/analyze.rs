//! `loomweave analyze` — discover plugins, walk the source tree, persist entities.
//!
//! WP2 Task 8 replaces the Sprint-1 stub with real plugin orchestration:
//! - Discover plugins via L9 `$PATH` convention (Task 5).
//! - For each plugin: spawn, handshake, walk the source tree, call
//!   `analyze_file` for every matching file, persist via writer-actor.
//! - File output streams through a bounded channel to the writer actor; import
//!   edges are deferred until the plugin's module set is known.
//! - On unrecoverable error (cap, escape, spawn, transport) → `FailRun`.
//! - Zero successful plugins discovered → `SkippedNoPlugins` (existing path).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use ignore::{DirEntry, WalkBuilder};
use rusqlite::Connection;
use serde::Serialize;
use time::{OffsetDateTime, macros::format_description};
use uuid::Uuid;

use loomweave_core::plugin::host::FINDING_PLUGIN_ABORTED;
use loomweave_core::{
    AcceptedEdge, AcceptedEntity, AnalyzeFileOutcome, CrashLoopBreaker, CrashLoopState,
    DiscoveredPlugin, EmbeddingProvider, FINDING_DISABLED_CRASH_LOOP, HostError, HostFinding,
    UnresolvedCallSite, discover,
};
use loomweave_storage::{
    DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, EmbeddingKey, EmbeddingStore, GitRename,
    NewEntityDescriptor, PriorIndexEntry, SeiBindingRecord, SeiDecision, SeiLineageEntry,
    UnresolvedCallSiteRecord, Writer, alive_bindings_snapshot,
    commands::{EdgeConfidence, EdgeRecord, EntityRecord, FindingRecord, RunStatus, WriterCmd},
    mint_sei, module_dependency_edges, orphaned_bindings, prior_analyzed_commit, rebind_or_mint,
    sei::{BindingStatus, LineageEvent},
};

use loomweave_federation::config::{FiligreeConfig, McpConfig, SemanticSearchConfig};
use loomweave_federation::filigree::FiligreeHttpClient;
use loomweave_federation::filigree_url::resolve_filigree_url;
use loomweave_federation::scan_results::{
    CleanStaleRequest, CleanStaleResponse, EmitOptions, FindingForEmit, LOOMWEAVE_SCAN_SOURCE,
    PreparedBatch, ScanResultsResponse, clean_stale_url, prepare_batch, scan_results_url,
};

use crate::config::{AnalyzeConfig, ClusteringConfig};
use crate::stats::P95Accumulator;
use duplicate_guard::{DUPLICATE_LOCATOR_RULE_ID, DuplicateLocatorGuard};

mod duplicate_guard;
use loomweave_analysis::{
    ClusterAlgorithm, ClusterConfig, ModuleEdge, ModuleGraph, cluster_hash, cluster_modules,
};

const WEAK_MODULARITY_RULE_ID: &str = "LMWV-FACT-CLUSTERING-WEAK-MODULARITY";

/// REQ-ANALYZE-04: one finding per entity that vanished from source since the
/// prior run (deletion detection, Phase 7).
const ENTITY_DELETED_RULE_ID: &str = "LMWV-FACT-ENTITY-DELETED";

/// REQ-ANALYZE-04: a guidance sheet whose explicit `guides` edge now points at a
/// deleted entity — the guidance is stranded and should not enrich briefings for
/// an entity that no longer exists.
const GUIDANCE_ORPHAN_RULE_ID: &str = "LMWV-FACT-GUIDANCE-ORPHAN";

/// Bounded handoff from the blocking plugin worker to the async writer loop.
/// Mirrors detailed-design §11's `file_analyzed` backpressure cap.
const PLUGIN_FILE_BATCH_CHANNEL_CAPACITY: usize = 100;
const PROGRESS_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const SEMANTIC_EMBEDDING_BATCH_SIZE: usize = 64;
type DescribedEdgeRecord = (String, EdgeRecord);

/// REQ-GUIDANCE-05 (WS6 T4a): a guidance sheet whose `expires` instant is in the
/// past. The read path already excludes expired sheets from composition; this
/// finding surfaces the state operatively (the sheet is not deleted).
const GUIDANCE_EXPIRED_RULE_ID: &str = "LMWV-FACT-GUIDANCE-EXPIRED";

/// REQ-GUIDANCE-05 (WS6 T4a): a guidance sheet whose matched entities carry a high
/// aggregate `git_churn_count` — the code under the sheet has churned enough that
/// the guidance is likely stale. Heuristic (confidence 0.7); inert until the
/// churn-history pipeline (clarion-997c93ec4e) populates `git_churn_count`.
const GUIDANCE_CHURN_STALE_RULE_ID: &str = "LMWV-FACT-GUIDANCE-CHURN-STALE";

/// Aggregate `git_churn_count` (summed over a sheet's matched entities) at or above
/// which a non-pinned sheet is flagged `LMWV-FACT-GUIDANCE-CHURN-STALE`.
const CHURN_STALE_THRESHOLD: i64 = 50;

/// The lower (stricter) churn threshold for `pinned: true` sheets — pinned guidance
/// is asserted institutional knowledge, so it goes stale on less churn.
const CHURN_STALE_THRESHOLD_PINNED: i64 = 20;

/// REQ-ANALYZE-05: a subsystem whose tier-bearing members declare ≥2 distinct
/// Wardline tiers (a trust-boundary smell — the cluster straddles tiers).
const TIER_MIXING_RULE_ID: &str = "LMWV-FACT-TIER-SUBSYSTEM-MIXING";

/// REQ-ANALYZE-05: a subsystem whose tier-bearing members (≥2) all agree on one
/// Wardline tier — a positive signal for tier-consistency reporting.
const TIER_UNANIMOUS_RULE_ID: &str = "LMWV-FACT-SUBSYSTEM-TIER-UNANIMOUS";

/// The finding rules persisted via `PersistPostRunFinding` *after* `CommitRun`
/// (the SEI mint pass's deletion findings + the tier-subsystem pass), and so
/// after Phase-8 emission has already run. A second, additive emission pass
/// (Phase 8c, `clarion-ef8f64d5fd`) re-reads exactly these so they reach Filigree
/// in the same run rather than being stranded store-only. `LMWV-FACT-ENTITY-DELETED`
/// anchors to the deleted entity's own path-bearing row; the subsystem-anchored
/// tier rules (and, once authoring lands, the guidance-anchored orphan rule) are
/// path-less, so the Phase-8c pass anchors them to the project root (the
/// `EmitOptions::default_path` fallback) and flags them `synthetic_anchor` rather
/// than dropping them as `skipped_no_path`.
const POST_RUN_FINDING_RULES: &[&str] = &[
    ENTITY_DELETED_RULE_ID,
    GUIDANCE_ORPHAN_RULE_ID,
    GUIDANCE_EXPIRED_RULE_ID,
    GUIDANCE_CHURN_STALE_RULE_ID,
    TIER_MIXING_RULE_ID,
    TIER_UNANIMOUS_RULE_ID,
];

/// REQ-ANALYZE-06 "no silent fallbacks": a Python file that fails `ast.parse`
/// is surfaced by the plugin as a degraded `module` entity carrying
/// `parse_status="syntax_error"` (extractor.py). The core mints a persisted
/// finding from that property so the failure is visible in the store, not just
/// in the plugin's stderr. Pyright degradation findings now ride the plugin
/// findings wire, but the syntax-error module property remains the stable
/// source for parse failures because the degraded module entity is the anchor.
const SYNTAX_ERROR_RULE_ID: &str = "LMWV-PY-SYNTAX-ERROR";

/// Writes structured run progress to a JSON file for the MCP `analyze_status`
/// tool (clarion-7e0c21558a). A no-op unless `analyze_start` passed a
/// `--progress-file` path, so the normal CLI path pays nothing. Each write
/// stamps a fresh `heartbeat_at`, letting a reader tell "still making progress"
/// from "stalled" without scraping logs. Writes are best-effort and
/// last-write-wins via an atomic temp-file rename; a failed write is logged and
/// dropped (progress is advisory, never run-fatal).
struct ProgressReporter {
    inner: Option<Arc<ProgressInner>>,
}

struct ProgressInner {
    path: PathBuf,
    run_id: String,
    pid: u32,
    total_files: AtomicU64,
    processed_files: AtomicU64,
}

impl ProgressReporter {
    fn new(progress_file: Option<PathBuf>, run_id: String) -> Self {
        Self {
            inner: progress_file.map(|path| {
                Arc::new(ProgressInner {
                    path,
                    run_id,
                    pid: std::process::id(),
                    total_files: AtomicU64::new(0),
                    processed_files: AtomicU64::new(0),
                })
            }),
        }
    }

    /// Record the total file count discovered for the run (denominator for
    /// `processed_files`).
    fn set_total(&self, total: u64) {
        if let Some(inner) = &self.inner {
            inner.total_files.store(total, Ordering::Relaxed);
        }
    }

    /// Write a snapshot for a phase boundary (`discovering`, `analyzing`,
    /// `clustering`). `current_plugin`/`current_file` are `None` between
    /// plugins.
    fn phase(&self, phase: &str, current_plugin: Option<&str>, current_file: Option<&str>) {
        let Some(inner) = &self.inner else {
            return;
        };
        let snapshot = serde_json::json!({
            "run_id": inner.run_id,
            "pid": inner.pid,
            "phase": phase,
            "current_plugin": current_plugin,
            "current_file": current_file,
            "processed_files": inner.processed_files.load(Ordering::Relaxed),
            "total_files": inner.total_files.load(Ordering::Relaxed),
            "heartbeat_at": iso8601_now(),
        });
        Self::write_atomic_inner(inner, &snapshot);
    }

    /// Snapshot at the start of a file (so `current_file` reflects in-flight
    /// work); the file is counted as processed by [`Self::file_completed`].
    fn file_started(&self, plugin_id: &str, file: &str) {
        self.phase("analyzing", Some(plugin_id), Some(file));
    }

    fn file_heartbeat_guard(
        &self,
        plugin_id: String,
        file: String,
    ) -> Option<ProgressHeartbeatGuard> {
        self.file_heartbeat_guard_with_interval(plugin_id, file, PROGRESS_HEARTBEAT_INTERVAL)
    }

    fn file_heartbeat_guard_with_interval(
        &self,
        plugin_id: String,
        file: String,
        interval: std::time::Duration,
    ) -> Option<ProgressHeartbeatGuard> {
        let inner = Arc::clone(self.inner.as_ref()?);
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            loop {
                match stop_rx.recv_timeout(interval) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        let snapshot = serde_json::json!({
                            "run_id": inner.run_id,
                            "pid": inner.pid,
                            "phase": "analyzing",
                            "current_plugin": plugin_id,
                            "current_file": file,
                            "processed_files": inner.processed_files.load(Ordering::Relaxed),
                            "total_files": inner.total_files.load(Ordering::Relaxed),
                            "heartbeat_at": iso8601_now(),
                        });
                        ProgressReporter::write_atomic_inner(&inner, &snapshot);
                    }
                }
            }
        });
        Some(ProgressHeartbeatGuard {
            stop_tx: Some(stop_tx),
            handle: Some(handle),
        })
    }

    /// Increment the processed-file counter after a file finishes.
    fn file_completed(&self) {
        if let Some(inner) = &self.inner {
            inner.processed_files.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Snapshot for a file the Wave 2 / T3.1 incremental fast path skipped
    /// (unchanged since the prior run): emit an `analyzing` snapshot tagged
    /// `skipped_unchanged`, then count the file as processed — it is done, just
    /// not re-parsed, so the progress denominator still resolves.
    fn file_skipped_unchanged(&self, plugin_id: &str, file: &str) {
        if let Some(inner) = &self.inner {
            let snapshot = serde_json::json!({
                "run_id": inner.run_id,
                "pid": inner.pid,
                "phase": "analyzing",
                "event": "skipped_unchanged",
                "current_plugin": plugin_id,
                "current_file": file,
                "processed_files": inner.processed_files.load(Ordering::Relaxed),
                "total_files": inner.total_files.load(Ordering::Relaxed),
                "heartbeat_at": iso8601_now(),
            });
            Self::write_atomic_inner(inner, &snapshot);
            inner.processed_files.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn write_atomic_inner(inner: &ProgressInner, snapshot: &serde_json::Value) {
        let body = snapshot.to_string();
        let tmp = inner.path.with_extension("json.tmp");
        if let Err(err) = fs::write(&tmp, &body).and_then(|()| fs::rename(&tmp, &inner.path)) {
            tracing::debug!(
                error = %err,
                path = %inner.path.display(),
                "failed to write analyze progress snapshot (advisory; ignored)",
            );
        }
    }
}

struct ProgressHeartbeatGuard {
    stop_tx: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ProgressHeartbeatGuard {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

// A flat bag of resolved CLI options (each field mirrors one `analyze` flag), not
// a state machine — the bools are independent toggles, so the pedantic
// excessive-bools lint does not apply.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub(crate) struct AnalyzeOptions {
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) secret_scan: crate::secret_scan::SecretScanOptions,
    /// Caller-supplied run id (MCP `analyze_start`); `None` generates one.
    pub(crate) run_id: Option<String>,
    /// `--resume RUN_ID` (REQ-FINDING-05): reopen this prior run's row instead
    /// of opening a fresh one, and emit findings with `mark_unseen=false` so a
    /// re-emit does not flip the prior run's findings to `unseen_in_latest` on
    /// the Filigree peer. Takes precedence over `run_id` as the run identifier.
    pub(crate) resume_run_id: Option<String>,
    /// `--prune-unseen` (REQ-FINDING-06): after emission, ask Filigree to
    /// soft-archive its stale `unseen_in_latest` Loomweave findings. Enrich-only:
    /// a failure or a disabled integration never fails the run.
    pub(crate) prune_unseen: bool,
    /// When set, structured progress is written here as the run proceeds.
    pub(crate) progress_file: Option<PathBuf>,
    /// `--no-sei`: skip the Wave 1 SEI mint pass (ADR-038). A diagnostic escape
    /// hatch for runs against a pre-migration DB or when identity is irrelevant;
    /// the durable graph is unaffected (SEI is enrich-only).
    pub(crate) no_sei: bool,
    /// `--no-incremental`: force a full re-analysis, disabling the Wave 2 / T3.1
    /// skip of unchanged files. A full re-analysis replays the per-source-file
    /// edge replacement boundary; use it when a clean graph refresh matters.
    pub(crate) no_incremental: bool,
    /// `--legis-url`: `legis`'s read-API base URL, enabling the WS9 git-rename
    /// provider seam (REQ-C-05). Enrich-only and capability-aware: the operative
    /// working-tree window stays on the shell source, so an unset/unreachable
    /// `legis` leaves behaviour byte-identical to pre-WS9. `None` ⇒ shell only.
    pub(crate) legis_url: Option<String>,
    /// `--json`: after the human-readable completion line, emit an additive
    /// structured-output block (graph totals + the TYPED `weft-reason` carrier for
    /// the Filigree emit seam) for machine consumers (the seam-health roll-up / L2
    /// strategic view / tag-board feed), so they read a `reason_class` they can
    /// switch on instead of grepping the completion line (G3 / PDR-0023).
    pub(crate) json: bool,
}

/// Run the analyze command against `project_path` with resolved CLI options.
///
/// # Errors
///
/// Returns an error if the target directory does not exist, has no `.weft/loomweave/`
/// directory, if analyze config is invalid, or if the writer actor fails to
/// start or process commands.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_with_options(project_path: PathBuf, options: AnalyzeOptions) -> Result<()> {
    if !project_path.exists() {
        bail!(
            "target directory does not exist: {}. Pass a valid path or cd to it first.",
            project_path.display()
        );
    }
    let project_root = project_path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise path {}", project_path.display()))?;
    let loomweave_dir = loomweave_core::store::store_dir(&project_root);
    if !loomweave_dir.exists() {
        bail!(
            "{} has no .weft/loomweave/ store. Run `loomweave install` first.",
            project_root.display()
        );
    }
    let db_path = loomweave_dir.join("loomweave.db");

    // Cross-process advisory lock (STO-01). Must outlive the writer-actor's
    // `handle.await` at the bottom of this function — see the drop-order
    // note on `AnalyzeLockGuard`. Drop on function exit releases the lock.
    let _analyze_lock = crate::analyze_lock::acquire_analyze_lock(&loomweave_dir)?;

    // Apply any pending schema migrations before opening the writer. `install`
    // is the usual migrator, but a binary upgrade that adds a migration the run
    // path writes (WS9: `runs.analyzed_at_commit`) must not hard-fail `analyze`
    // on a DB that `install` has not re-touched. Idempotent (only pending
    // migrations run) and safe under the analyze lock acquired above; the writer
    // still verifies `user_version` on spawn to reject a forward-incompatible file.
    {
        let mut conn =
            Connection::open(&db_path).context("open database to apply pending migrations")?;
        loomweave_storage::pragma::apply_write_pragmas(&conn)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        loomweave_storage::schema::apply_migrations(&mut conn)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("apply pending migrations")?;
        let repaired = loomweave_storage::mark_stale_running_runs_failed(&conn)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("mark stale running analyze runs failed")?;
        if repaired > 0 {
            tracing::warn!(
                repaired,
                "marked stale running analyze runs failed before starting new analyze"
            );
        }
    }

    let analyze_config = AnalyzeConfig::load(&project_root, options.config_path.as_deref())?;
    let analyze_config_json = analyze_config.to_json_string()?;

    // ── Writer actor ──────────────────────────────────────────────────────────
    let (writer, handle) = Writer::spawn(
        db_path.clone(),
        DEFAULT_BATCH_SIZE,
        DEFAULT_CHANNEL_CAPACITY,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context("spawn writer actor")?;
    // `--resume RUN_ID` reuses the prior run's id (and reopens its row below);
    // absent that, the hidden MCP `--run-id` is honoured, else a fresh id.
    let resume = options.resume_run_id.is_some();
    let run_id = options
        .resume_run_id
        .clone()
        .or_else(|| options.run_id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let started_at = iso8601_now();

    // WS9 / SEI §6 git-rename windowing. Capture the HEAD this run analyzes
    // against (persisted on the run row), and read the *prior* run's recorded
    // commit to drive the committed window `<prior_commit>..HEAD`. The prior read
    // happens here — before `open_run` writes/reopens this run's row — and
    // excludes `run_id`, so it can never resolve to the current run (which
    // `CommitRun` marks `completed` before the SEI mint pass runs). Both are
    // best-effort: a non-git corpus or a read failure degrades to the
    // working-tree-only window, exactly as pre-WS9.
    let head_commit = crate::sei_git::git_head_sha(&project_root);
    let prior_commit = match Connection::open(&db_path) {
        Ok(conn) => prior_analyzed_commit(&conn, &run_id).unwrap_or(None),
        Err(_) => None,
    };

    // Structured progress sink (MCP `analyze_start` sets `progress_file`); a
    // no-op when absent so the normal CLI path is unchanged.
    let progress = Arc::new(ProgressReporter::new(
        options.progress_file.clone(),
        run_id.clone(),
    ));
    progress.phase("discovering", None, None);

    // ── Discover plugins ──────────────────────────────────────────────────────
    let discovery_results = discover();
    let mut plugins: Vec<DiscoveredPlugin> = Vec::new();
    let mut discovery_errors: Vec<String> = Vec::new();
    for result in discovery_results {
        match result {
            Ok(p) => {
                tracing::info!(
                    plugin_id = %p.manifest.plugin.plugin_id,
                    executable = %p.executable.display(),
                    "discovered plugin"
                );
                plugins.push(p);
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(error = %msg, "skipping plugin: discovery error");
                discovery_errors.push(msg);
            }
        }
    }

    if plugins.is_empty() {
        // Distinguish "no plugins installed" (SkippedNoPlugins — expected on a
        // bare machine) from "plugins present but all failed discovery" (FailRun
        // — a real configuration error the operator must see). Reporting the
        // latter as `skipped_no_plugins` hides bugs.
        if !discovery_errors.is_empty() {
            let reason = format!(
                "all {} discovered plugin manifest(s) failed to parse: {}",
                discovery_errors.len(),
                discovery_errors.join("; ")
            );
            tracing::error!(run_id = %run_id, reason = %reason, "failing run: discovery errors");
            crate::run_lifecycle::open_run(
                &writer,
                resume,
                &run_id,
                &analyze_config_json,
                &started_at,
                head_commit.as_deref(),
            )
            .await?;
            let completed_at = iso8601_now();
            writer
                .send_wait(|ack| WriterCmd::FailRun {
                    run_id: run_id.clone(),
                    reason: reason.clone(),
                    completed_at,
                    ack,
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("FailRun(discovery errors)")?;

            drop(writer);
            handle
                .await
                .map_err(|e| anyhow::anyhow!("writer actor panic: {e}"))?
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            // Non-zero exit. Printing to stdout + returning Ok(()) here
            // hides the failure from `loomweave analyze && do_next` chains
            // and breaks CI gating that reads `$?`. The run row in the DB
            // is already marked `failed` above.
            bail!("analyze run {run_id} failed — {reason}");
        }

        tracing::warn!(run_id = %run_id, "no plugins discovered");
        crate::run_lifecycle::open_run(
            &writer,
            resume,
            &run_id,
            &analyze_config_json,
            &started_at,
            head_commit.as_deref(),
        )
        .await?;
        let completed_at = iso8601_now();
        writer
            .send_wait(|ack| WriterCmd::CommitRun {
                run_id: run_id.clone(),
                status: RunStatus::SkippedNoPlugins,
                completed_at: completed_at.clone(),
                stats_json: serde_json::json!({
                    "entities_inserted": 0,
                    "edges_inserted": 0,
                    "dropped_edges_total": 0,
                    "ambiguous_edges_total": 0,
                    "unresolved_call_sites_total": 0,
                    "reference_sites_total": 0,
                    "references_resolved_total": 0,
                    "references_skipped_external_total": 0,
                    "references_skipped_cap_total": 0,
                    "imports_skipped_external_total": 0,
                    "unresolved_reference_sites_total": 0,
                    "pyright_query_latency_p95_ms": 0,
                    "pyright_index_parse_latency_p95_ms": 0,
                    "extractor_parse_latency_p95_ms": 0,
                })
                .to_string(),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("CommitRun(SkippedNoPlugins)")?;

        drop(writer);
        handle
            .await
            .map_err(|e| anyhow::anyhow!("writer actor panic: {e}"))?
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        println!("analyze complete: run {run_id} skipped_no_plugins");
        return Ok(());
    }

    // ── Build extension union for the tree walk ───────────────────────────────
    let mut wanted_extensions: BTreeSet<String> = BTreeSet::new();
    for p in &plugins {
        for ext in &p.manifest.plugin.extensions {
            wanted_extensions.insert(ext.to_ascii_lowercase());
        }
    }

    // ── Walk the source tree (once, union of all extensions) ─────────────────
    let source_walk = collect_source_files(&project_root, &wanted_extensions);
    let source_walk_skipped_entries =
        u64::try_from(source_walk.skipped_errors.len()).unwrap_or(u64::MAX);
    let source_walk_error_samples = source_walk
        .skipped_errors
        .iter()
        .take(SOURCE_WALK_ERROR_SAMPLE_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    let source_walk_errors_omitted = source_walk
        .skipped_errors
        .len()
        .saturating_sub(source_walk_error_samples.len());
    let source_files = source_walk.files;
    tracing::info!(file_count = source_files.len(), "source tree walk complete");
    progress.set_total(source_files.len() as u64);
    progress.phase("analyzing", None, None);

    let secret_scan_files = crate::secret_scan::collect_scan_files(&project_root, &source_files);
    tracing::info!(
        file_count = secret_scan_files.len(),
        "secret scan file walk complete"
    );
    let mut secret_scan_outcome =
        crate::secret_scan::pre_ingest(&project_root, &secret_scan_files, &options.secret_scan)?;
    crate::run_lifecycle::open_run(
        &writer,
        resume,
        &run_id,
        &analyze_config_json,
        &started_at,
        head_commit.as_deref(),
    )
    .await?;

    // ── Wave 2 / T3.1: incremental-analysis skip state ────────────────────────
    //
    // Recover the prior run's per-file whole-file hashes (to detect unchanged
    // files), the locators each file contributed (so a skipped file's still-
    // present entities are never falsely orphaned by the SEI matcher), and the
    // full prior-index snapshot (to re-feed skipped entries into the rebuilt
    // index — otherwise the snapshot would blank them out and the skip would
    // decay after one run). Read from a fresh connection BEFORE this run writes
    // anything, so it reflects exactly the previous successful run. `--no-
    // incremental` and a first run (empty prior index) both degrade to a full
    // analysis. Skipped files deliberately leave their durable rows untouched;
    // changed files pass through the per-source-file edge replacement boundary
    // before their current edge set is inserted.
    //
    // Caveat (benign): a skipped file's core `file` entity keeps last run's
    // `briefing_blocked` / `language` properties, which a full re-analysis would
    // refresh. This can only go stale TOWARD blocked (a withheld briefing that
    // could now be served — the conservative direction); a file that should
    // NEWLY block changed by definition (the secret is a content change), so the
    // whole-file hash re-dispatches it, and every file is scanned by `pre_ingest`
    // before the partition — it cannot silently under-block.
    // `--no-incremental` clears any such staleness.
    //
    // Secret-bearing UNCHANGED files are skipped like any other
    // (weft-4165f1ed71, the analyze fixed point — they used to be carved out
    // because their finding anchor could only be resolved from entities emitted
    // in the same run, re-processing one file forever). Their pre-ingest
    // findings re-anchor through `prior_anchor_by_file`: the preferred anchor
    // entity (module first) resolved from the already-committed rows, seeded
    // into the scan outcome for every skipped file below.
    let incremental = !options.no_incremental;
    let (prior_file_hashes, mut prior_locs_by_file, prior_index_snapshot, prior_anchor_by_file) =
        if incremental {
            match Connection::open(&db_path) {
                Ok(conn) => {
                    let files =
                        loomweave_storage::previously_analyzed_files(&conn).unwrap_or_default();
                    let locs = loomweave_storage::prior_locators_by_file(&conn).unwrap_or_default();
                    let snapshot = loomweave_storage::load_prior_index(&conn).unwrap_or_default();
                    // Anchor continuity for a skipped secret-bearing file
                    // (weft-4165f1ed71). Read the entity_id the prior run ACTUALLY
                    // keyed its secret finding to (L1: ground truth from the
                    // `findings` rows), and only fall back to the module/plugin/core
                    // re-derivation heuristic for files with no prior secret finding
                    // (first-time anchors, which cannot duplicate). The heuristic
                    // diverges from the in-run `remember_finding_anchors` tie-break on
                    // same-rank candidates (Rust inline `mod` blocks), which would
                    // flip the anchor-keyed finding id and mint a duplicate row.
                    let mut anchors =
                        loomweave_storage::preferred_finding_anchor_by_file(&conn)
                            .unwrap_or_default();
                    anchors.extend(
                        loomweave_storage::stored_secret_finding_anchor_by_file(&conn)
                            .unwrap_or_default(),
                    );
                    (files, locs, snapshot, anchors)
                }
                Err(err) => {
                    tracing::warn!(error = %err, "incremental skip disabled: cannot open read connection");
                    (
                        HashMap::new(),
                        HashMap::new(),
                        HashMap::new(),
                        HashMap::new(),
                    )
                }
            }
        } else {
            (
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
            )
        };
    // Locators of skipped-unchanged entities — fed into the SEI matcher's
    // current-locator union AND re-appended to the prior-index rebuild below.
    let mut retained_locators: HashSet<String> = HashSet::new();
    let mut skipped_files_total: u64 = 0;

    // ── Per-plugin processing ─────────────────────────────────────────────────
    //
    // A per-plugin crash (spawn / handshake / analyze_file Err) does NOT tank
    // the whole run — other plugins still get a chance. Crashes are recorded
    // on the shared `CrashLoopBreaker`; once >3 in 60 s the breaker trips,
    // the host emits `FINDING_DISABLED_CRASH_LOOP`, and remaining plugins are
    // skipped. A run with any crashes still resolves to `RunOutcome::Failed`
    // (plus exit 1 per the bail!() below) so CI sees the problem — continue-
    // past-crash preserves partial work, not failure signal.
    //
    // Writer-actor errors (InsertEntity rejected) ARE run-fatal: the DB
    // layer is unusable for the rest of this run.
    let mut total_entity_count: u64 = 0;
    let mut total_edge_count: u64 = 0;
    let mut unresolved_call_sites_total: u64 = 0;
    let mut reference_sites_total: u64 = 0;
    let mut references_resolved_total: u64 = 0;
    let mut references_skipped_external_total: u64 = 0;
    let mut references_skipped_cap_total: u64 = 0;
    let mut imports_skipped_external_total: u64 = 0;
    // Anchored resolving edges (`imports`/`implements`) whose endpoints were
    // never stored this run — dropped-and-counted by the seen-entity-set gate
    // (D1 external / D2 gitignored-superset / D3 mid-run staleness). Flushing
    // such an edge would FK-HardFail the whole run (`edges.to_id` is
    // `NOT NULL REFERENCES entities(id)` with FKs on); the gate trades the edge
    // for a counter so the run still completes.
    let mut plugin_edges_dropped_unseen_total: u64 = 0;
    let mut unresolved_reference_sites_total: u64 = 0;
    let mut pyright_latency = P95Accumulator::default();
    let mut pyright_index_parse_latency = P95Accumulator::default();
    let mut extractor_parse_latency = P95Accumulator::default();
    let mut run_outcome: RunOutcome = RunOutcome::Completed;
    let mut breaker = CrashLoopBreaker::default();
    let mut crash_reasons: Vec<String> = Vec::new();
    // Wave 0 / WS3: accumulate this run's prior-index snapshot as entities are
    // inserted. `entities` is cumulative (never pruned, no run-scoping), so the
    // current run's set cannot be recovered by querying it — it must be gathered
    // here. Entities with no `content_hash` (no body to hash) are omitted: the
    // snapshot's `body_hash` is NOT NULL and such entities are not move-matchable.
    let mut prior_index_entries: Vec<PriorIndexEntry> = Vec::new();
    // Wave 1 / WS1: accumulate this run's entity descriptors (locator + body
    // hash + signature) for the SEI mint pass, which runs after CommitRun and
    // before the prior-index flush. Gathered here for the same reason as the
    // prior index — `entities` is cumulative and cannot recover the run's set.
    let mut sei_descriptors: Vec<NewEntityDescriptor> = Vec::new();
    // REQ-ANALYZE-06: failure findings accumulated through the run and persisted
    // before CommitRun, so a recoverable failure is visible in the store rather
    // than only in logs. Parse errors anchor to their (degraded) module entity;
    // plugin-level findings (crash, ontology/protocol violations) anchor to the
    // synthetic project entity minted just before persistence.
    let mut failure_findings: Vec<FindingRecord> = Vec::new();
    let project_anchor = project_anchor_id(&project_root);
    if source_walk_skipped_entries > 0 {
        failure_findings.push(source_walk_finding_record(
            &project_root,
            source_walk_skipped_entries,
            &source_walk_error_samples,
            source_walk_errors_omitted,
            &project_anchor,
            &run_id,
            &started_at,
        ));
    }
    let handshake_timeout = plugin_handshake_timeout();
    let file_timeout = plugin_file_timeout();
    let shutdown_timeout = plugin_shutdown_timeout();
    let briefing_blocks = secret_scan_outcome.briefing_blocks_shared();
    let scanned_files = secret_scan_outcome.scanned_files_shared();
    'plugins: for plugin in plugins {
        let plugin_id = plugin.manifest.plugin.plugin_id.clone();
        let plugin_extensions: BTreeSet<String> = plugin
            .manifest
            .plugin
            .extensions
            .iter()
            .map(|e| e.to_ascii_lowercase())
            .collect();

        // Filter source files to this plugin's extensions.
        let plugin_files: Vec<PathBuf> = source_files
            .iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| plugin_extensions.contains(&e.to_ascii_lowercase()))
            })
            .cloned()
            .collect();

        if plugin_files.is_empty() {
            tracing::info!(plugin_id = %plugin_id, "no files match plugin extensions; skipping");
            continue;
        }

        // Wave 2 / T3.1: partition into files to re-analyse (changed, new,
        // unhashable → fail toward work) and files to skip (whole-file hash
        // identical to the prior run). Each skipped file's prior entities stay
        // in the DB; we record their locators for the matcher union and
        // re-append their prior-index rows so the rebuilt snapshot keeps them.
        // A secret-bearing UNCHANGED file skips too (weft-4165f1ed71): its
        // finding anchor is seeded from the committed rows below, so the skip
        // no longer re-anchors (and thereby duplicates) the finding.
        let (plugin_files, skipped_files): (Vec<PathBuf>, Vec<PathBuf>) = plugin_files
            .into_iter()
            .partition(|path| file_needs_reanalysis(&project_root, path, &prior_file_hashes));
        // Locators of THIS plugin's skipped-unchanged entities. These rows stay in
        // the committed DB untouched this run (they are guarded against orphan
        // deletion via `retained_locators` below — see the SEI matcher's
        // current-locator union), so they are exactly the endpoints an anchored
        // edge from a CHANGED file may resolve against even though the host never
        // re-dispatched the file that owns them. Seeding them into
        // `seen_plugin_entity_ids` (below) lets such an edge drain ready instead of
        // being dropped-and-counted as if its endpoint were never stored. We seed
        // ONLY skipped-file locators (never the full prior index): a CHANGED file
        // that drops a symbol does NOT re-emit it this run, and — crucially —
        // because it was re-dispatched its locator is absent from THIS set, so an
        // edge into that now-dead symbol still drops. Seeding from the full prior
        // index instead would mark the dropped symbol seen (its row lingers —
        // `entities` is cumulative) and resurrect a stale edge to a symbol the
        // source no longer defines.
        let mut skipped_file_entity_ids: Vec<String> = Vec::new();
        // id → owning canonical source path for entities anchored in this
        // plugin's skipped-unchanged files. These rows survive the run
        // untouched, so a RE-ANALYZED file emitting one of these ids is a
        // genuine cross-run locator collision (clarion-b19fe90c3e) — the old
        // file still claims it. A genuine move never lands here: the moved-from
        // file either changed (re-dispatched, so not skipped) or vanished (its
        // entities are orphan-deleted by the SEI pass). Full runs leave this
        // empty.
        let mut skipped_locator_owners: BTreeMap<String, String> = BTreeMap::new();
        for path in &skipped_files {
            skipped_files_total += 1;
            progress.file_skipped_unchanged(&plugin_id, &path.to_string_lossy());
            // Anchor continuity for the pre-ingest secret scan
            // (weft-4165f1ed71): this file is not dispatched, so no plugin
            // entity registers an anchor this run — resolve the same anchor the
            // analysed run used (module-preferred) from the committed rows.
            let canonical = crate::secret_scan::canonical_or_original(path);
            if let Some(anchor) = prior_anchor_by_file
                .get(&canonical.display().to_string())
                .or_else(|| prior_anchor_by_file.get(&path.display().to_string()))
            {
                secret_scan_outcome.seed_finding_anchor(canonical, anchor.clone());
            }
            if let Some(key) = canonical_path_key(path)
                && let Some(locators) = prior_locs_by_file.remove(&key)
            {
                for locator in locators {
                    if let Some(entry) = prior_index_snapshot.get(&locator) {
                        prior_index_entries.push(entry.clone());
                    }
                    skipped_locator_owners.insert(locator.clone(), key.clone());
                    skipped_file_entity_ids.push(locator.clone());
                    retained_locators.insert(locator);
                }
            }
        }
        if plugin_files.is_empty() {
            tracing::info!(
                plugin_id = %plugin_id,
                skipped = skipped_files.len(),
                "all plugin files unchanged; skipping plugin dispatch (incremental)"
            );
            continue;
        }

        tracing::info!(
            plugin_id = %plugin_id,
            file_count = plugin_files.len(),
            skipped = skipped_files.len(),
            "processing plugin"
        );

        // Run the blocking plugin work on the tokio threadpool. Completed file
        // output flows through a bounded channel so writer backpressure applies
        // during extraction rather than after the whole plugin has returned.
        let manifest = plugin.manifest.clone();
        let project_root_clone = project_root.clone();
        let pid_clone = plugin_id.clone();
        let exec_clone = plugin.executable.clone();
        let files_clone = plugin_files.clone();
        let briefing_blocks_clone = Arc::clone(&briefing_blocks);
        let scanned_files_clone = Arc::clone(&scanned_files);
        let progress_clone = Arc::clone(&progress);

        // Seed the dual-declaration claim set with the file_scope (module)
        // entity ids anchored in this plugin's SKIPPED files. A module id can
        // legitimately be emitted by more than one file (clarion-6ec7317628:
        // tokio's inline `pub(crate) mod unix {}` facade in `process/mod.rs`
        // colliding with the path-derived module of `process/unix/mod.rs`).
        // The first emitter claims the `file -> module` contains edge and the
        // matching `parent_id` (ADR-026 dual encoding); later emitters are
        // suppressed. On an incremental run the claim owner may be a SKIPPED
        // file whose stored edge survives untouched — seeding its module ids
        // here keeps a re-analyzed duplicate emitter from minting a second
        // contains parent against the surviving one. The owner==anchor
        // invariant holds because the claiming emission is the only one whose
        // entity record is stored, so a skipped file's locator list names
        // exactly the modules it owns. Full runs seed empty.
        let claim_kind_roles = PluginKindRoles::from_manifest(&plugin.manifest);
        let prior_file_scope_claims: BTreeSet<String> = skipped_file_entity_ids
            .iter()
            .filter(|id| {
                id.split(':')
                    .nth(1)
                    .is_some_and(|kind| claim_kind_roles.is_file_scope(kind))
            })
            .cloned()
            .collect();

        let (batch_tx, mut batch_rx) =
            tokio::sync::mpsc::channel(PLUGIN_FILE_BATCH_CHANNEL_CAPACITY);
        let join_handle = tokio::task::spawn_blocking(move || {
            run_plugin_blocking(
                manifest,
                &project_root_clone,
                &pid_clone,
                &exec_clone,
                &files_clone,
                &briefing_blocks_clone,
                &scanned_files_clone,
                &progress_clone,
                handshake_timeout,
                file_timeout,
                shutdown_timeout,
                prior_file_scope_claims,
                skipped_locator_owners,
                &batch_tx,
            )
        });

        let mut insert_err: Option<anyhow::Error> = None;
        let mut plugin_entity_count: u64 = 0;
        let mut plugin_edge_count: u64 = 0;
        // Seed the seen-entity gate with this plugin's skipped-file entity ids: an
        // anchored edge from a re-analyzed file into an UNCHANGED file's entity must
        // drain ready (its endpoint exists in the committed DB and survives this
        // run), not be dropped at end-of-plugin. Full runs have no skipped files, so
        // this is empty and behaviour is unchanged.
        let mut seen_plugin_entity_ids: BTreeSet<String> =
            skipped_file_entity_ids.into_iter().collect();
        let mut pending_plugin_edges: Vec<DescribedEdgeRecord> = Vec::new();
        while let Some(message) = batch_rx.recv().await {
            if insert_err.is_some() {
                continue;
            }

            match message {
                PluginBatchMessage::File(mut batch) => {
                    unresolved_call_sites_total += batch.stats.unresolved_call_sites_total;
                    reference_sites_total += batch.stats.reference_sites_total;
                    references_resolved_total += batch.stats.references_resolved_total;
                    references_skipped_external_total +=
                        batch.stats.references_skipped_external_total;
                    references_skipped_cap_total += batch.stats.references_skipped_cap_total;
                    imports_skipped_external_total += batch.stats.imports_skipped_external_total;
                    unresolved_reference_sites_total +=
                        batch.stats.unresolved_reference_sites_total;
                    pyright_latency.record_many(batch.stats.pyright_query_latency_ms.clone());
                    pyright_index_parse_latency
                        .record_many(batch.stats.pyright_index_parse_latency_ms.clone());
                    extractor_parse_latency
                        .record_many(batch.stats.extractor_parse_latency_ms.clone());

                    secret_scan_outcome.remember_finding_anchors(&batch.entities);
                    let batch_entity_ids: Vec<String> =
                        batch.entities.iter().map(|(id, _)| id.clone()).collect();
                    let batch_edges = std::mem::take(&mut batch.edges);
                    match persist_plugin_file_batch(
                        &writer,
                        batch,
                        &run_id,
                        &started_at,
                        head_commit.as_deref(),
                    )
                    .await
                    {
                        Ok(effects) => {
                            plugin_entity_count += effects.entity_count;
                            seen_plugin_entity_ids.extend(batch_entity_ids);
                            pending_plugin_edges.extend(batch_edges);
                            let ready_edges = drain_ready_plugin_edges(
                                &mut pending_plugin_edges,
                                &seen_plugin_entity_ids,
                            );
                            match persist_plugin_edges(&writer, ready_edges).await {
                                Ok(edge_count) => {
                                    plugin_edge_count += edge_count;
                                }
                                Err(e) => {
                                    insert_err = Some(e);
                                }
                            }
                            prior_index_entries.extend(effects.prior_index_entries);
                            sei_descriptors.extend(effects.sei_descriptors);
                            failure_findings.extend(effects.failure_findings);
                        }
                        Err(e) => {
                            insert_err = Some(e);
                        }
                    }
                }
                PluginBatchMessage::DeferredImportEdges {
                    edges,
                    imports_skipped_external,
                } => {
                    imports_skipped_external_total += imports_skipped_external;
                    pending_plugin_edges.extend(edges);
                    let ready_edges = drain_ready_plugin_edges(
                        &mut pending_plugin_edges,
                        &seen_plugin_entity_ids,
                    );
                    match persist_plugin_edges(&writer, ready_edges).await {
                        Ok(edge_count) => {
                            plugin_edge_count += edge_count;
                            // `DeferredImportEdges` is this plugin's LAST message
                            // (`seen_plugin_entity_ids`/`pending_plugin_edges` are
                            // re-initialised per plugin at the top of the `'plugins`
                            // loop), so whatever is still pending after the final
                            // drain has an endpoint the host never stored. Such an
                            // edge cannot FK-resolve — flushing it would HardFail
                            // the whole run. Drop it and COUNT it (never silently
                            // lose, never downgrade-to-Inferred); the run completes.
                            plugin_edges_dropped_unseen_total +=
                                drop_unready_plugin_edges(&mut pending_plugin_edges);
                        }
                        Err(e) => {
                            insert_err = Some(e);
                        }
                    }
                }
            }
        }

        // A JoinError here means the blocking task panicked (OOM, stack
        // overflow, internal unwrap, abort — anything that unwinds past the
        // top of `run_plugin_blocking`). Earlier revisions `?`-propagated
        // the JoinError out of `run()`, which bypassed the
        // CommitRun/FailRun block and left `runs.status = 'running'`
        // permanently. Treat the panic as a crash reason: it flows into the
        // existing crash-recording path below, ticks the crash-loop breaker,
        // and resolves the run via SoftFailed → CommitRun(Failed) with exit 1.
        let spawn_result: Result<BatchResult, PluginRunError> =
            handle_plugin_task_join_result(join_handle.await, &plugin_id);

        if let Some(e) = insert_err {
            tracing::error!(
                plugin_id = %plugin_id,
                error = %e,
                "writer-actor rejected streamed insert; failing run"
            );
            run_outcome = RunOutcome::HardFailed {
                reason: format!("{e:#}"),
            };
            break 'plugins;
        }

        match spawn_result {
            Err(plugin_error) => {
                log_plugin_findings(&plugin_id, &plugin_error.findings);
                // REQ-ANALYZE-06: persist the host findings collected before the
                // crash. A per-file timeout already rides in as a LMWV-PY-TIMEOUT
                // finding (and is the root cause), so suppress the generic
                // LMWV-INFRA-PLUGIN-CRASH in that case to avoid double-reporting.
                let timed_out = plugin_error
                    .findings
                    .iter()
                    .any(|hf| hf.subcode == PLUGIN_TIMEOUT_RULE_ID);
                for hf in &plugin_error.findings {
                    let anchor_id = host_finding_anchor_id(hf, &project_root, &project_anchor);
                    failure_findings.push(host_finding_to_record(
                        hf,
                        &plugin_id,
                        &anchor_id,
                        &run_id,
                        &started_at,
                    ));
                }
                if !timed_out {
                    failure_findings.push(crash_finding_record(
                        &plugin_id,
                        &plugin_error.reason,
                        &project_anchor,
                        &run_id,
                        &started_at,
                    ));
                }
                tracing::warn!(
                    plugin_id = %plugin_id,
                    reason = %plugin_error.reason,
                    "plugin crashed; recording crash and continuing to next plugin",
                );
                crash_reasons.push(format!("{plugin_id}: {}", plugin_error.reason));
                let state = breaker.record_crash();
                if state == CrashLoopState::Tripped {
                    tracing::warn!(
                        subcode = FINDING_DISABLED_CRASH_LOOP,
                        crash_count = crash_reasons.len(),
                        "crash-loop breaker tripped; skipping remaining plugins in this run",
                    );
                    break 'plugins;
                }
                // Fall through to the next iteration — nothing else to do
                // for a crashed plugin, and there's no code after the match.
            }
            Ok(BatchResult { findings }) => {
                // Log findings individually (operator-facing stderr) and persist
                // them (REQ-ANALYZE-06) so an ontology check, malformed-JSON drop,
                // or path-jail violation is visible in the store, not just logs.
                log_plugin_findings(&plugin_id, &findings);
                for hf in &findings {
                    let anchor_id = host_finding_anchor_id(hf, &project_root, &project_anchor);
                    failure_findings.push(host_finding_to_record(
                        hf,
                        &plugin_id,
                        &anchor_id,
                        &run_id,
                        &started_at,
                    ));
                }

                total_entity_count += plugin_entity_count;
                total_edge_count += plugin_edge_count;
                tracing::info!(
                    plugin_id = %plugin_id,
                    entity_count = plugin_entity_count,
                    edge_count = plugin_edge_count,
                    "plugin complete",
                );
            }
        }
    }

    if !matches!(run_outcome, RunOutcome::HardFailed { .. })
        && let Err(e) = secret_scan_outcome
            .persist_findings(
                &writer,
                &run_id,
                &project_root,
                &started_at,
                head_commit.as_deref(),
            )
            .await
    {
        tracing::error!(run_id = %run_id, error = %e, "secret finding persistence failed");
        run_outcome = RunOutcome::HardFailed {
            reason: format!("secret finding persistence failed: {e:#}"),
        };
    }

    // REQ-ANALYZE-06: persist accumulated failure findings (parse errors,
    // host/protocol diagnostics, plugin crashes). Runs after entity inserts so
    // each finding's `entity_id` anchor resolves, and only when the run is being
    // committed (a HardFailed run is rolled back).
    if !matches!(run_outcome, RunOutcome::HardFailed { .. }) {
        // Mint the synthetic project anchor first, but only if a finding actually
        // anchors to it (parse-error findings anchor to their module entity and
        // need no project entity).
        let needs_project_anchor = failure_findings
            .iter()
            .any(|f| f.entity_id == project_anchor);
        if needs_project_anchor
            && let Err(e) =
                ensure_project_anchor(&writer, &project_root, &started_at, head_commit.as_deref())
                    .await
        {
            tracing::error!(run_id = %run_id, error = %e, "project finding-anchor insert failed");
            run_outcome = RunOutcome::HardFailed {
                reason: format!("project finding-anchor insert failed: {e:#}"),
            };
        }
    }

    // Captured for stats.json (REQ-ANALYZE-06 "visible in stats.json") so the
    // count is reported regardless of whether Filigree emission runs.
    let failure_finding_count = failure_findings.len();
    if !matches!(run_outcome, RunOutcome::HardFailed { .. }) {
        for finding in failure_findings {
            let finding_id = finding.id.clone();
            if let Err(e) = writer
                .send_wait(|ack| WriterCmd::InsertFinding {
                    finding: Box::new(finding),
                    ack,
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .with_context(|| format!("InsertFinding {finding_id}"))
            {
                tracing::error!(run_id = %run_id, error = %e, "failure-finding persistence failed");
                run_outcome = RunOutcome::HardFailed {
                    reason: format!("failure-finding persistence failed: {e:#}"),
                };
                break;
            }
        }
        if failure_finding_count > 0 {
            tracing::info!(
                run_id = %run_id,
                finding_count = failure_finding_count,
                "persisted failure findings"
            );
        }
    }

    // ── Commit or fail the run ────────────────────────────────────────────────
    //
    // Writer-actor failures set `run_outcome = HardFailed` above (and break).
    // If only plugin crashes occurred (no writer-actor failure), `run_outcome`
    // is still `Completed` — promote it to `SoftFailed` so the pending entity
    // batch commits AND the run row marks failed. Crash-free completions
    // stay `Completed` regardless of entity count.
    if matches!(run_outcome, RunOutcome::Completed) && !crash_reasons.is_empty() {
        run_outcome = RunOutcome::SoftFailed {
            reason: format!(
                "{} plugin(s) crashed: {}",
                crash_reasons.len(),
                crash_reasons.join("; "),
            ),
        };
    }

    progress.phase("clustering", None, None);
    let phase3_output = if matches!(run_outcome, RunOutcome::HardFailed { .. }) {
        Phase3Output::not_run()
    } else {
        match run_phase3_clustering(
            &writer,
            &db_path,
            &run_id,
            &analyze_config,
            head_commit.as_deref(),
        )
        .await
        {
            Ok(output) => {
                total_entity_count += output.subsystems_inserted;
                total_edge_count += output.in_subsystem_edges_inserted;
                if output.weak_modularity_finding {
                    tracing::info!(run_id = %run_id, "phase3 emitted weak-modularity finding");
                }
                output
            }
            Err(e) => {
                tracing::error!(run_id = %run_id, error = %e, "phase3 clustering failed");
                run_outcome = RunOutcome::HardFailed {
                    reason: format!("phase3 clustering failed: {e:#}"),
                };
                Phase3Output::not_run()
            }
        }
    };

    // Phase 8 (WP9-B): emit findings to Filigree for non-hard-failed runs,
    // before CommitRun so the emission outcome rides along in `stats.json`.
    // Best-effort: a Filigree outage never changes the run's own outcome.
    let filigree_emission = if matches!(
        run_outcome,
        RunOutcome::Completed | RunOutcome::SoftFailed { .. }
    ) {
        emit_findings_to_filigree(
            &writer,
            &db_path,
            &project_root,
            &run_id,
            // `mark_unseen` sweeps findings this scan did NOT report as gone —
            // only sound when the scan examined the whole corpus, i.e. it skipped
            // no files (`skipped_files_total == 0`: a `--no-incremental` run or a
            // first run with an empty prior index). An incremental run that
            // skipped unchanged files has no authority to sweep their findings,
            // and an incremental no-op would otherwise POST an empty batch with
            // `mark_unseen=true` — which Filigree rejects 400 ("requires at least
            // one finding or scanned path"). `--resume` never sweeps
            // (REQ-FINDING-05).
            !resume && skipped_files_total == 0,
            // Final/only completing batch for the during-run findings; the
            // Phase-8c follow-up (if any) is additive (`complete_scan_run=false`).
            true,
            // No rule filter: emit every finding the run wrote up to this point.
            None,
            options.config_path.as_deref(),
        )
        .await
    } else {
        serde_json::Value::Null
    };
    // Capture the degraded-emit marker BEFORE `filigree_emission` is moved into the
    // `stats.json` blob below. The durable blob records the full emit outcome; this
    // marker is the loud, stdout-facing distillation a caller gates on without
    // reading the DB. A clean (emitted / disabled / true-negative) emit yields
    // `None` and the completion line stays unchanged (PDR-0023 honesty invariant).
    let primary_emit_marker = emit_status_marker(&filigree_emission);
    // The TYPED `weft-reason` carrier for the same emit blob (G3): the
    // structured-output sibling of the substring marker above, read from the same
    // source so the two never disagree on which shapes are clean vs degraded.
    let primary_emit_reason = emit_reason_carrier(&filigree_emission);
    // The Phase-8c (post-commit) emit, captured in the Completed arm below.
    let mut postrun_emit_marker: Option<String> = None;
    let mut postrun_emit_reason: Option<EmitReason> = None;

    // Phase 8b (WP9-B, REQ-FINDING-06): `--prune-unseen` retention sweep. Runs
    // after emission for the same non-hard-failed outcomes, so a fresh run's
    // `mark_unseen=true` has just (re)established the unseen set the sweep
    // archives. Best-effort and enrich-only, exactly like emission.
    let filigree_prune = if matches!(
        run_outcome,
        RunOutcome::Completed | RunOutcome::SoftFailed { .. }
    ) {
        prune_unseen_findings_in_filigree(
            &project_root,
            &run_id,
            options.prune_unseen,
            options.config_path.as_deref(),
        )
        .await
    } else {
        serde_json::Value::Null
    };

    let completed_at = iso8601_now();
    // Snapshot the writer's process-lifetime dropped-edges counter so the
    // run's durable stats record the dedupe count (B.3 §6). Read BEFORE
    // CommitRun so the value reflects exactly this run's inserts.
    let dropped_edges_total = writer
        .dropped_edges_total
        .load(std::sync::atomic::Ordering::Relaxed) as u64;
    let ambiguous_edges_total = writer
        .ambiguous_edges_total
        .load(std::sync::atomic::Ordering::Relaxed) as u64;
    let pyright_query_latency_p95_ms = pyright_latency.p95_ms();
    let pyright_index_parse_latency_p95_ms = pyright_index_parse_latency.p95_ms();
    let extractor_parse_latency_p95_ms = extractor_parse_latency.p95_ms();
    // Extract the failure reason (if any) before the match consumes run_outcome.
    let fail_reason: Option<String> = match &run_outcome {
        RunOutcome::SoftFailed { reason } | RunOutcome::HardFailed { reason } => {
            Some(reason.clone())
        }
        RunOutcome::Completed => None,
    };

    match run_outcome {
        RunOutcome::Completed => {
            let mut stats_json = serde_json::json!({
                "entities_inserted": total_entity_count,
                "edges_inserted": total_edge_count,
                "dropped_edges_total": dropped_edges_total,
                "ambiguous_edges_total": ambiguous_edges_total,
                "unresolved_call_sites_total": unresolved_call_sites_total,
                "reference_sites_total": reference_sites_total,
                "references_resolved_total": references_resolved_total,
                "references_skipped_external_total": references_skipped_external_total,
                "references_skipped_cap_total": references_skipped_cap_total,
                "imports_skipped_external_total": imports_skipped_external_total,
                "plugin_edges_dropped_unseen_total": plugin_edges_dropped_unseen_total,
                "source_walk_skipped_entries": source_walk_skipped_entries,
                "source_walk_error_samples": source_walk_error_samples,
                "source_walk_errors_omitted": source_walk_errors_omitted,
                "skipped_files": skipped_files_total,
                "unresolved_reference_sites_total": unresolved_reference_sites_total,
                "pyright_query_latency_p95_ms": pyright_query_latency_p95_ms,
                "pyright_index_parse_latency_p95_ms": pyright_index_parse_latency_p95_ms,
                "extractor_parse_latency_p95_ms": extractor_parse_latency_p95_ms,
                "clustering": phase3_output.clustering_stats.clone(),
                "failure_findings": failure_finding_count,
            });
            secret_scan_outcome.augment_stats(&mut stats_json);
            if !filigree_emission.is_null() {
                stats_json["filigree_emission"] = filigree_emission;
            }
            if !filigree_prune.is_null() {
                stats_json["filigree_prune"] = filigree_prune;
            }
            let stats_json = stats_json.to_string();
            writer
                .send_wait(|ack| WriterCmd::CommitRun {
                    run_id: run_id.clone(),
                    status: RunStatus::Completed,
                    completed_at,
                    stats_json,
                    ack,
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("CommitRun(Completed)")?;
            // Wave 1 / WS1: SEI mint pass (ADR-038). Runs AFTER CommitRun (the
            // entity graph is durable, so reads see the complete run) and BEFORE
            // the prior-index flush (it reads the prior alive bindings; both are
            // independent tables but the SEI pass is the identity authority and
            // goes first). Enrich-only and best-effort: a failure logs and is
            // swallowed — identity is additive and never un-commits a graph the
            // run already persisted (the §5 enrich-only invariant). `--no-sei`
            // skips it entirely.
            if options.no_sei {
                tracing::info!(run_id = %run_id, "SEI mint pass skipped (--no-sei)");
            } else {
                match run_sei_mint_pass(
                    &writer,
                    &db_path,
                    &project_root,
                    &run_id,
                    sei_descriptors,
                    &retained_locators,
                    options.legis_url.as_deref(),
                    prior_commit.as_deref(),
                    head_commit.as_deref(),
                )
                .await
                {
                    Ok(stats) => tracing::info!(
                        run_id = %run_id,
                        minted = stats.minted,
                        carried = stats.carried,
                        orphaned = stats.orphaned,
                        deletion_findings = stats.deletion_findings,
                        "SEI mint pass complete"
                    ),
                    Err(e) => tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "SEI mint pass failed; identity bindings skipped for this run \
                         (run already committed successfully)"
                    ),
                }
            }
            // Wave 0 / WS3: rewrite the prior-index snapshot to exactly this
            // run's entities (stale rows from the prior run removed). Runs AFTER
            // CommitRun — the run is already durably `completed`, so this is a
            // best-effort, enrich-only retention write: a failure here logs and
            // is swallowed, never failing an analysis whose graph is committed
            // (mirrors the Filigree-emission "outage never changes the outcome"
            // posture). Nothing consumes the snapshot in Wave 0; the WS1 matcher
            // and incremental skip degrade to a full pass when it is absent.
            // ONLY the Completed arm flushes: SoftFailed/HardFailed runs are
            // recorded as `failed`, so the snapshot deliberately stays at the
            // last fully-successful run (a WS1 consumer must treat snapshot vs
            // durable graph as possibly divergent after a soft-fail, not assume
            // equality).
            if let Err(e) = writer
                .send_wait(|ack| WriterCmd::UpsertPriorIndex {
                    entries: prior_index_entries,
                    recorded_at: iso8601_now(),
                    ack,
                })
                .await
            {
                tracing::warn!(
                    run_id = %run_id,
                    error = %e,
                    "prior-index snapshot flush failed; retention skipped for this run \
                     (run already committed successfully)"
                );
            }
            // REQ-ANALYZE-05 Phase-7 structural findings (tier × subsystem). Runs
            // AFTER CommitRun (the in_subsystem edges are durable) and is
            // best-effort + enrich-only like the SEI pass: a failure logs and is
            // swallowed, never un-committing the graph. Honest-empty when no
            // Wardline tier facts exist (analyze never writes them).
            match emit_tier_subsystem_findings(&writer, &db_path, &run_id, &iso8601_now()).await {
                Ok(emitted) if emitted > 0 => tracing::info!(
                    run_id = %run_id,
                    tier_subsystem_findings = emitted,
                    "tier-subsystem findings emitted"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    run_id = %run_id,
                    error = %e,
                    "tier-subsystem findings skipped (run already committed successfully)"
                ),
            }
            let mcp_config = load_mcp_config(&project_root, options.config_path.as_deref());
            match crate::serve::build_embedding_provider(&mcp_config.semantic_search, |name| {
                std::env::var(name).ok()
            }) {
                Ok(Some(provider)) => match populate_semantic_embeddings(
                    &project_root,
                    &db_path,
                    &mcp_config.semantic_search,
                    provider,
                )
                .await
                {
                    Ok(stats) if stats.embedded > 0 || stats.skipped_fresh > 0 => tracing::info!(
                        run_id = %run_id,
                        model_id = %stats.model_id,
                        considered = stats.considered,
                        skipped_fresh = stats.skipped_fresh,
                        embedded = stats.embedded,
                        tokens_input = stats.tokens_input,
                        "semantic embedding population complete"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "semantic embedding population skipped (run already committed successfully)"
                    ),
                },
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    run_id = %run_id,
                    error = %e,
                    "semantic embedding provider unavailable (run already committed successfully)"
                ),
            }
            // REQ-GUIDANCE-05 (WS6 T4a): guidance-staleness findings (EXPIRED +
            // CHURN-STALE). Runs on EVERY analyze, deliberately OUTSIDE the SEI
            // `if no_sei { … } else { … }` block above and independent of any
            // deletion: these surface a sheet's own state, not an identity event,
            // so `--no-sei` must NOT suppress them. Best-effort + enrich-only like
            // the tier pass: a failure logs and never un-commits the graph.
            match emit_guidance_staleness_findings(
                &writer,
                &db_path,
                &project_root,
                &run_id,
                &iso8601_now(),
            )
            .await
            {
                Ok(emitted) if emitted > 0 => tracing::info!(
                    run_id = %run_id,
                    guidance_staleness_findings = emitted,
                    "guidance-staleness findings emitted"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    run_id = %run_id,
                    error = %e,
                    "guidance-staleness findings skipped (run already committed successfully)"
                ),
            }
            // Phase 8c (clarion-ef8f64d5fd): the deletion + tier findings above
            // are persisted via `PersistPostRunFinding` *after* the Phase-8
            // emission already ran, so without this they reach the store but
            // never the same-run Filigree emission. A second, additive pass
            // re-reads only the post-commit rules (the during-run findings were
            // already emitted at Phase 8) and posts them: `LMWV-FACT-ENTITY-DELETED`
            // against the deleted entity's own path, and the path-less
            // subsystem-anchored tier facts against the project root (the
            // `default_path` fallback supplied inside `emit_findings_to_filigree`
            // for the filtered pass), flagged `synthetic_anchor`. `mark_unseen`
            // mirrors Phase 8 so a finding's prior-run position transitions to
            // `unseen_in_latest`; `complete_scan_run=false` because this batch
            // *appends* to the run Phase 8 already completed. Best-effort +
            // enrich-only and logged-only (not folded into `stats.json`:
            // `CommitRun` is already durable, exactly like the SEI and tier passes
            // above). When there are no post-commit findings the batch is empty
            // and no POST is made.
            let postrun_emission = emit_findings_to_filigree(
                &writer,
                &db_path,
                &project_root,
                &run_id,
                // Runs that skipped files do not sweep (see Phase-8 note); only a
                // run that examined the whole corpus may mark findings unseen.
                !resume && skipped_files_total == 0,
                false,
                Some(POST_RUN_FINDING_RULES),
                options.config_path.as_deref(),
            )
            .await;
            match postrun_emission.get("status").and_then(|s| s.as_str()) {
                Some("emitted") => tracing::info!(
                    run_id = %run_id,
                    emission = %postrun_emission,
                    "post-commit findings emitted to Filigree"
                ),
                Some("unreachable") => tracing::warn!(
                    run_id = %run_id,
                    emission = %postrun_emission,
                    "post-commit finding emission could not reach Filigree (enrich-only)"
                ),
                _ => {}
            }
            // Surface a degraded Phase-8c emit on the completion line too (rolled up
            // with the Phase-8 marker at summary time): a post-commit emit that
            // never reached Filigree is the same dead-seam family as the during-run
            // one and must not exit-0-silent.
            postrun_emit_marker = emit_status_marker(&postrun_emission);
            postrun_emit_reason = Some(emit_reason_carrier(&postrun_emission));
            // Stale-finding sweep (clarion-87c1eba2bd / ADR-048): retire findings
            // whose code no longer reproduces them. Runs LAST in the Completed arm
            // — after every during-run `InsertFinding` AND every post-commit
            // `PersistPostRunFinding` pass (SEI deletion, tier, guidance) — so a
            // reproduced finding already carries the current run_id and only a
            // genuinely-vanished finding keeps an older one. Gated to a CLEAN FULL
            // PASS so `run_id <> current` unambiguously means "the current run
            // walked this finding's file and stopped reproducing it":
            //   • !resume               — a `--resume` run REUSES the prior run_id
            //     (its not-yet-re-emitted findings already match current, so the
            //     run_id signal can't distinguish them — never sweep on resume).
            //   • skipped_files == 0    — an incremental run leaves unchanged
            //     files' findings at their PRIOR run_id; sweeping them would
            //     wrongly retire still-reproducing findings.
            //   • source_walk_skipped_entries == 0 — a file/dir that ERRORED
            //     during the source walk (IO / permission / path-jail) was never
            //     read, so its findings were not re-emitted and keep a prior
            //     run_id; the run still reaches `Completed`, so without this guard
            //     a single walk error would retire a whole unwalked subtree's
            //     still-reproducing findings ("never looked" ≠ "looked, fixed").
            //   • !no_sei               — the SEI pass (entity-deleted /
            //     guidance-orphan facts) was skipped, so those findings were NOT
            //     refreshed this run and must not be mistaken for vanished.
            // Best-effort + enrich-only like the SEI/tier/guidance passes above: a
            // failure logs and never un-commits the already-durable graph. Findings
            // linger until the next clean full analyze — accepted (findings are
            // regenerable current-state, ADR-047).
            if !resume
                && skipped_files_total == 0
                && source_walk_skipped_entries == 0
                && !options.no_sei
            {
                match writer
                    .send_wait(|ack| WriterCmd::SweepStaleFindings {
                        current_run_id: run_id.clone(),
                        ack,
                    })
                    .await
                {
                    Ok(retired) if retired > 0 => tracing::info!(
                        run_id = %run_id,
                        stale_findings_retired = retired,
                        "stale-finding sweep retired findings whose code no longer reproduces"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "stale-finding sweep skipped (run already committed successfully)"
                    ),
                }
            }
            // Rule-scoped sweep for the secret-scan rule family
            // (weft-7256739b31 / dogfood-4 B10). The pre-ingest secret scan is a
            // FULL pass every run — every source file and sidecar, BEFORE the
            // incremental skip partition — so its findings are fully re-emitted
            // each completed run and "run_id != current" unambiguously means
            // "scanned, no longer detected". That lets these rules be swept on
            // the incremental runs the general sweep above must skip (where
            // stale secret rows otherwise accumulate forever). Gated on:
            //   • !resume — a resume reuses the prior run_id, so the run_id
            //     signal cannot distinguish re-emitted from vanished;
            //   • source_walk_skipped_entries == 0 — an unwalked file was never
            //     handed to the scan, so its rows were not re-emitted
            //     ("never looked" ≠ "looked, clean").
            // The scan's "full pass" is full only over the CURRENTLY-installed
            // plugins' extension union, so uninstalling/disabling a plugin
            // between runs silently drops its files from the scan WITHOUT any
            // walk error — `source_walk_skipped_entries` stays 0, so that gate
            // cannot catch this scope shrinkage (L3). We therefore additionally
            // bound the sweep to the files the scan actually examined this run
            // (`scanned_files`, canonical-absolute — the form entities store):
            // a finding survives unless its anchor entity's source file was
            // re-examined.
            // Same lifecycle preservation + best-effort posture as above.
            if !resume && source_walk_skipped_entries == 0 {
                let rule_ids: Vec<String> = crate::secret_scan::per_run_swept_rule_ids()
                    .iter()
                    .map(|&rule| rule.to_owned())
                    .collect();
                let examined_source_files: Vec<String> = scanned_files
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect();
                match writer
                    .send_wait(|ack| WriterCmd::SweepStaleFindingsForRules {
                        current_run_id: run_id.clone(),
                        rule_ids,
                        examined_source_files,
                        ack,
                    })
                    .await
                {
                    Ok(retired) if retired > 0 => tracing::info!(
                        run_id = %run_id,
                        stale_secret_findings_retired = retired,
                        "scoped sweep retired secret-scan findings the full pre-ingest scan no longer reproduces"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "scoped secret-finding sweep skipped (run already committed successfully)"
                    ),
                }
            }
        }
        RunOutcome::SoftFailed { reason } => {
            // Commit entities inserted by healthy plugins AND mark the run
            // failed, atomically (writer folds the UPDATE into the open tx).
            // The stats JSON carries both fields so operators can see what
            // was persisted alongside the failure reason.
            let mut stats_json = serde_json::json!({
                "entities_inserted": total_entity_count,
                "edges_inserted": total_edge_count,
                "dropped_edges_total": dropped_edges_total,
                "ambiguous_edges_total": ambiguous_edges_total,
                "unresolved_call_sites_total": unresolved_call_sites_total,
                "reference_sites_total": reference_sites_total,
                "references_resolved_total": references_resolved_total,
                "references_skipped_external_total": references_skipped_external_total,
                "references_skipped_cap_total": references_skipped_cap_total,
                "imports_skipped_external_total": imports_skipped_external_total,
                "plugin_edges_dropped_unseen_total": plugin_edges_dropped_unseen_total,
                "source_walk_skipped_entries": source_walk_skipped_entries,
                "source_walk_error_samples": source_walk_error_samples,
                "source_walk_errors_omitted": source_walk_errors_omitted,
                "skipped_files": skipped_files_total,
                "unresolved_reference_sites_total": unresolved_reference_sites_total,
                "pyright_query_latency_p95_ms": pyright_query_latency_p95_ms,
                "pyright_index_parse_latency_p95_ms": pyright_index_parse_latency_p95_ms,
                "extractor_parse_latency_p95_ms": extractor_parse_latency_p95_ms,
                "clustering": phase3_output.clustering_stats.clone(),
                "failure_findings": failure_finding_count,
                "failure_reason": reason,
            });
            secret_scan_outcome.augment_stats(&mut stats_json);
            if !filigree_emission.is_null() {
                stats_json["filigree_emission"] = filigree_emission;
            }
            if !filigree_prune.is_null() {
                stats_json["filigree_prune"] = filigree_prune;
            }
            let stats_json = stats_json.to_string();
            writer
                .send_wait(|ack| WriterCmd::CommitRun {
                    run_id: run_id.clone(),
                    status: RunStatus::Failed,
                    completed_at,
                    stats_json,
                    ack,
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("CommitRun(Failed) — soft fail")?;
        }
        RunOutcome::HardFailed { reason } => {
            writer
                .send_wait(|ack| WriterCmd::FailRun {
                    run_id: run_id.clone(),
                    reason,
                    completed_at,
                    ack,
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("FailRun — hard fail")?;
        }
    }

    drop(writer);
    handle
        .await
        .map_err(|e| anyhow::anyhow!("writer actor panic: {e}"))?
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // On FailRun: bail so the process exits non-zero. The run row is
    // already marked `failed` in the DB by the FailRun branch above; this
    // is purely about surfacing failure to the operator's shell / CI.
    if let Some(reason) = fail_reason {
        bail!("analyze run {run_id} failed — {reason}");
    }

    // Report the WHOLE-GRAPH totals (the same numbers `project_status` and the
    // session-start hook show), not this run's insert delta. The delta is
    // misleadingly small on incremental runs that skip unchanged files — it
    // counts only the phase3 subsystems re-emitted — so an operator could read
    // it as the graph having shrunk. Fall back to the run delta only if the
    // post-commit count read fails, so a cosmetic hiccup never masks a
    // successful run.
    // Roll the Phase-8 + Phase-8c emit outcomes into one completion-line marker.
    // `None` for a clean emit (the line is byte-identical to pre-PDR-0023); a loud
    // `emit:<status>(reason)` for a dead/partial/skipped-pre-wire seam so the
    // best-effort emit can no longer swallow a total failure as a confident exit-0.
    let emit_marker = combined_emit_marker(primary_emit_marker, postrun_emit_marker);
    // The typed sibling of `emit_marker` (G3): rolled up the same way for the
    // structured `--json` block, so a typed consumer reads a `reason_class` it can
    // switch on instead of grepping the completion line's substring.
    let emit_reason = combined_emit_reason(primary_emit_reason, postrun_emit_reason);
    let emit_suffix = emit_marker
        .as_deref()
        .map_or_else(String::new, |m| format!("; {m}"));
    // Resolve the whole-graph totals once: both the human completion line and the
    // structured `--json` block report the SAME numbers. `None` ⇒ the totals read
    // failed and we fall back to this run's insert delta (a cosmetic hiccup never
    // masks a successful run).
    let graph_totals = match Connection::open(&db_path) {
        Ok(conn) => match (
            loomweave_storage::entity_total(&conn),
            loomweave_storage::subsystem_total(&conn),
            loomweave_storage::edge_total(&conn),
        ) {
            (Ok(entities), Ok(subsystems), Ok(edges)) => Some((entities, subsystems, edges)),
            _ => None,
        },
        Err(err) => {
            tracing::warn!(
                error = %err,
                "analyze complete: graph-total read failed; reporting run delta"
            );
            None
        }
    };
    let summary = match graph_totals {
        Some((entities, subsystems, edges)) => format_analyze_complete(
            &run_id,
            entities,
            subsystems,
            edges,
            skipped_files_total,
            emit_marker.as_deref(),
        ),
        None => format!(
            "analyze complete: run {run_id} completed \
             ({total_entity_count} entities, {total_edge_count} edges){emit_suffix}"
        ),
    };
    println!("{summary}");
    // Additive structured-output surface (G3 / PDR-0023). The human line above is
    // unchanged for interactive use; `--json` adds a machine-readable block whose
    // emit outcome is a TYPED `weft-reason` carrier (not a stdout substring), for
    // the seam-health roll-up / L2 strategic view / tag-board feed. Falls back to
    // the run delta when the whole-graph read failed, matching the human line.
    if options.json {
        let (entities, subsystems, edges) = graph_totals.unwrap_or((
            i64::try_from(total_entity_count).unwrap_or(i64::MAX),
            0,
            i64::try_from(total_edge_count).unwrap_or(i64::MAX),
        ));
        let payload = AnalyzeJsonOutput {
            run_id: &run_id,
            entities,
            subsystems,
            edges,
            skipped_files: skipped_files_total,
            emit: emit_reason,
        };
        // A serialization failure here must not mask an otherwise-successful run;
        // surface it on stderr and continue (the human line already printed).
        match serde_json::to_string_pretty(&payload) {
            Ok(json) => println!("{json}"),
            Err(err) => tracing::warn!(
                error = %err,
                "analyze complete: structured --json output could not be serialized"
            ),
        }
    }
    Ok(())
}

/// Outcome counts of one SEI mint pass (for logging / observability).
#[derive(Debug, Default, Clone, Copy)]
struct SeiPassStats {
    minted: u64,
    carried: u64,
    orphaned: u64,
    /// Count of REQ-ANALYZE-04 deletion findings (`LMWV-FACT-ENTITY-DELETED` +
    /// `LMWV-FACT-GUIDANCE-ORPHAN`) persisted from this run's orphaned set.
    deletion_findings: u64,
}

/// One entity's planned identity write, computed before any DB write so the
/// orphan-first ordering (T2.2 Step 5) can be applied.
struct PlannedSeiWrite {
    descriptor: NewEntityDescriptor,
    decision: SeiDecision,
}

/// Collapse SEI descriptors to one per locator, LAST write wins — matching the
/// entity layer's `INSERT ... ON CONFLICT(id) DO UPDATE`, which tolerates a
/// plugin emitting the same id twice in a run (the architecture permits it).
/// Without this, two descriptors at one locator would plan two `alive` bindings
/// there and violate the `ux_sei_alive_locator` partial unique index. The
/// `BTreeMap` also yields the deterministic, locator-sorted processing order the
/// cross-entity carry dedup in [`run_sei_mint_pass`] relies on.
/// Render the operator-facing `analyze complete` summary line.
///
/// Reports the **whole-graph** totals (entities incl. subsystems, edges) — the
/// same numbers `project_status` and the session-start hook show — rather than
/// the per-run insert delta, which is misleadingly small on incremental runs
/// that skip unchanged files. When unchanged files were skipped, the line is
/// annotated so an operator does not mistake a fast incremental pass for a graph
/// that shrank.
fn format_analyze_complete(
    run_id: &str,
    entities: i64,
    subsystems: i64,
    edges: i64,
    skipped_files: u64,
    emit_marker: Option<&str>,
) -> String {
    let incremental = if skipped_files > 0 {
        let noun = if skipped_files == 1 { "file" } else { "files" };
        format!("; incremental: {skipped_files} unchanged {noun} skipped")
    } else {
        String::new()
    };
    // A degraded Filigree emit rides on the completion line so a caller gating on
    // `analyze` stdout (not the durable `stats.json` blob) can see the dead/partial
    // seam without re-deriving it. A healthy, disabled, or clean-true-negative emit
    // adds nothing — only a degraded emit is loud (PDR-0023 honesty invariant).
    let emit = emit_marker.map_or_else(String::new, |m| format!("; {m}"));
    format!(
        "analyze complete: run {run_id} completed \
         (graph: {entities} entities incl. {subsystems} subsystems, {edges} edges{incremental}){emit}"
    )
}

/// Classify a `filigree_emission` stats blob into a loud, machine-readable
/// completion-line marker for a DEGRADED emit, or `None` when the emit was clean.
///
/// The blob is the same one folded into `stats.json` ([`emit_findings_to_filigree`]
/// / [`post_findings_batch`] / [`unreachable_stats`]). Three blob shapes are clean
/// and intentionally produce NO marker — they must stay byte-distinguishable from a
/// degraded emit and from each other (PDR-0023):
///   • disabled         — `Null` (integration off, or `emit_findings=false`);
///   • clean true-negative — `status:"emitted"` with no intake warnings (a real
///     emit, whether of zero or many findings — zero findings emitted because there
///     were zero findings is success, not degradation);
///   • a `skipped` filtered (Phase-8c) no-op — nothing emittable remained.
/// Every other shape is degraded and surfaced with a `reason`:
///   • `emit:unreachable(...)` — the POST never landed (outage / transport / panic);
///   • `emit:partial(N warnings)` — the POST landed but Filigree rejected/coerced
///     some findings (per-finding intake warnings) — a partial ingest that the bare
///     `status:"emitted"` would otherwise read as full success;
///   • `emit:skipped(reason)` — a pre-POST skip that is NOT the clean filtered no-op
///     (a flush/read/open failure swallowed before the wire — the same family as the
///     dead-for-weeks seam: an emit that never even reached the network).
fn emit_status_marker(emission: &serde_json::Value) -> Option<String> {
    if emission.is_null() {
        return None;
    }
    let status = emission.get("status").and_then(serde_json::Value::as_str);
    match status {
        Some("emitted") => {
            let warnings = emission
                .get("warnings")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            (warnings > 0).then(|| {
                let noun = if warnings == 1 { "warning" } else { "warnings" };
                format!("emit:partial ({warnings} intake {noun})")
            })
        }
        Some("unreachable") => {
            let reason = emission
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unreachable");
            Some(format!("emit:unreachable ({reason})"))
        }
        Some("skipped") => {
            let reason = emission
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("skipped");
            // The filtered (Phase-8c) "nothing emittable remained" skip is a clean
            // true-negative, not a degraded seam — emit nothing for it. Every other
            // skip is a pre-wire failure (flush/read/open) the run must not bury.
            (reason != "no_postrun_findings_with_path")
                .then(|| format!("emit:skipped ({reason})"))
        }
        // An unknown/absent status string is itself a contract drift — surface it
        // rather than silently reading it as clean.
        _ => Some(format!(
            "emit:degraded (unrecognized status {})",
            status.unwrap_or("<missing>")
        )),
    }
}

/// Combine the Phase-8 and (optional) Phase-8c emit markers into the single,
/// worst-case completion-line marker. The during-run (Phase-8) emit is the primary
/// seam; a degraded Phase-8c (post-commit) emit also surfaces, but the primary
/// marker wins when both degrade (one loud marker is enough for a caller to gate).
fn combined_emit_marker(
    primary: Option<String>,
    postrun: Option<String>,
) -> Option<String> {
    primary.or(postrun)
}

/// The typed `weft-reason` carrier for the Filigree emit seam (PDR-0023 / G3 /
/// `weft-reason-contract-G1`). The human-readable completion-line marker
/// ([`emit_status_marker`]) is a *substring* — a typed consumer (the seam-health
/// Layer-3 roll-up, the L2 strategic-view MCP, the tag-board feed) needs to switch
/// on a machine-readable class, not grep stdout. This is the typed sibling of that
/// marker: it reads the *same* `filigree_emission` blob and lands the outcome on
/// the canonical 11-class vocab.
///
/// A `clean` carrier omits `cause`/`fix` (the empty is earned and complete, and a
/// present-but-`clean` carrier is never ambiguous with a forgotten one); every
/// non-`clean` class carries both a `cause` (what mechanically happened) and a
/// recruiting `fix` (the action that gets the caller what they wanted) — `fix` is
/// MANDATORY on the non-clean classes per the contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct EmitReason {
    /// One of the canonical 11 `reason_class` strings.
    reason_class: &'static str,
    /// CAUSE — what mechanically happened. Omitted only on `clean`.
    #[serde(skip_serializing_if = "Option::is_none")]
    cause: Option<String>,
    /// FIX — the recruiting action. Omitted only on `clean`; present on every
    /// non-`clean` class.
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

impl EmitReason {
    /// A clean, earned true-negative (or a disabled integration): no cause/fix.
    fn clean(reason_class: &'static str) -> Self {
        Self {
            reason_class,
            cause: None,
            fix: None,
        }
    }

    /// A non-clean class: cause + a mandatory recruiting fix.
    fn degraded(reason_class: &'static str, cause: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            reason_class,
            cause: Some(cause.into()),
            fix: Some(fix.into()),
        }
    }
}

/// Map a `filigree_emission` stats blob onto the canonical `weft-reason` vocab as a
/// TYPED carrier (not a stdout substring). The blob is the same one classified by
/// [`emit_status_marker`] for the human line; the two stay in lock-step on which
/// shapes are clean vs degraded, but this returns a class a consumer can switch on.
///
/// The marker-string → `reason_class` mapping, each justified against the contract:
///   • `Null` (integration off / `emit_findings=false`)        → `disabled`
///     — not on, not a failure; no cause/fix (clean-family).
///   • `status:"emitted"`, no intake warnings                  → `clean`
///     — a real emit of zero-or-many findings is success, the earned true-negative.
///   • `status:"emitted"` WITH intake warnings                 → `partial`
///     — the POST landed but Filigree rejected/coerced some findings; an incomplete
///       ingest the bare `emitted` would read as full success (qualified-trust).
///   • `status:"unreachable"` (POST never landed)              → `unreachable`
///     — outage / transport / panic at the producer→consumer wire.
///   • `status:"skipped"`, `no_postrun_findings_with_path`     → `clean`
///     — the Phase-8c filtered no-op: nothing emittable remained. A true-negative.
///   • `status:"skipped"`, any other reason                    → `error`
///     — a pre-wire flush/read/open failure swallowed before the network. NOT
///       `unreachable` (the wire was never reached) and NOT `dead_path` (the seam is
///       wired, an internal step failed): it is an internal pre-wire error, the loud
///       catch-all for "something broke before we could emit". Justified as `error`
///       over `unreachable` because the failure is on OUR side of the socket.
///   • unrecognized / absent `status`                          → `error`
///     — contract drift; the loud catch-all, never silently read as clean.
fn emit_reason_carrier(emission: &serde_json::Value) -> EmitReason {
    if emission.is_null() {
        return EmitReason::clean("disabled");
    }
    let status = emission.get("status").and_then(serde_json::Value::as_str);
    match status {
        Some("emitted") => {
            let warnings = emission
                .get("warnings")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            if warnings == 0 {
                EmitReason::clean("clean")
            } else {
                let noun = if warnings == 1 { "warning" } else { "warnings" };
                EmitReason::degraded(
                    "partial",
                    format!(
                        "Filigree accepted the batch but flagged {warnings} intake {noun} \
                         (findings rejected or coerced)"
                    ),
                    "inspect filigree_emission.warnings in stats.json; fix the flagged \
                     fields and re-emit to complete the ingest",
                )
            }
        }
        Some("unreachable") => {
            let reason = emission
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unreachable");
            EmitReason::degraded(
                "unreachable",
                format!("the findings POST never landed: {reason}"),
                "check the Filigree endpoint is up and reachable, then re-run analyze \
                 (or `analyze --resume RUN_ID`) to re-emit",
            )
        }
        Some("skipped") => {
            let reason = emission
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("skipped");
            if reason == "no_postrun_findings_with_path" {
                // The Phase-8c filtered no-op is a clean true-negative — nothing
                // emittable remained — not a degraded seam.
                EmitReason::clean("clean")
            } else {
                EmitReason::degraded(
                    "error",
                    format!("emit aborted before the wire (pre-POST failure: {reason})"),
                    "check the analyze logs for the flush/read/open failure behind \
                     this skip, then re-run analyze to re-emit",
                )
            }
        }
        // An unknown/absent status string is itself a contract drift — the loud
        // catch-all, never silently read as clean.
        other => EmitReason::degraded(
            "error",
            format!(
                "unrecognized Filigree emit status {}",
                other.unwrap_or("<missing>")
            ),
            "(contract drift) report the unexpected emit status to the weft hub",
        ),
    }
}

/// Combine the Phase-8 (during-run) and (optional) Phase-8c (post-commit) typed
/// emit carriers into the single, worst-case carrier for the structured block.
/// Mirrors [`combined_emit_marker`]: the primary seam wins when both are non-clean
/// (one loud carrier is enough for a consumer to gate); otherwise a non-clean
/// post-run carrier surfaces over a clean primary, so a degraded Phase-8c emit is
/// never masked by a clean during-run one.
fn combined_emit_reason(primary: EmitReason, postrun: Option<EmitReason>) -> EmitReason {
    if primary.reason_class != "clean" && primary.reason_class != "disabled" {
        return primary;
    }
    match postrun {
        Some(p) if p.reason_class != "clean" && p.reason_class != "disabled" => p,
        // Both clean/disabled: keep the primary (it carries the during-run posture;
        // `disabled` beats `clean` only when the primary itself was disabled).
        _ => primary,
    }
}

/// The additive structured-output payload emitted by `analyze --json`: the
/// whole-graph totals plus the TYPED emit carrier. The human-readable completion
/// line is still printed for interactive use; this block is the machine-readable
/// surface a typed consumer reads instead of grepping stdout (G3).
#[derive(Debug, Serialize)]
struct AnalyzeJsonOutput<'a> {
    run_id: &'a str,
    entities: i64,
    subsystems: i64,
    edges: i64,
    skipped_files: u64,
    /// The typed `weft-reason` carrier for the Filigree emit seam.
    emit: EmitReason,
}

fn dedup_descriptors_by_locator(descriptors: Vec<NewEntityDescriptor>) -> Vec<NewEntityDescriptor> {
    descriptors
        .into_iter()
        .map(|d| (d.locator.clone(), d))
        .collect::<BTreeMap<String, NewEntityDescriptor>>()
        .into_values()
        .collect()
}

/// Wave 1 / WS1 SEI mint pass (ADR-038 §3, SEI spec §3). For every entity in the
/// completed run, carry-or-mint an SEI against the prior alive bindings + the
/// git-rename signal, record lineage, and orphan vanished-unmatched bindings.
///
/// Determinism boundary (ADR-038): SEI *values* are not part of the
/// byte-identical-run guarantee — two from-scratch runs mint different SEIs. The
/// guarantee is that the carry/mint *decisions* are deterministic given the same
/// `sei_bindings` + source. A back-to-back unchanged re-run therefore CARRIES
/// (never re-mints) every SEI (the locator-unchanged path).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn run_sei_mint_pass(
    writer: &Writer,
    db_path: &Path,
    project_root: &Path,
    run_id: &str,
    descriptors: Vec<NewEntityDescriptor>,
    retained_locators: &HashSet<String>,
    legis_url: Option<&str>,
    prior_commit: Option<&str>,
    head_commit: Option<&str>,
) -> anyhow::Result<SeiPassStats> {
    // Read the prior alive bindings (this run has written no SEI yet, so this is
    // exactly the previous run's identity state).
    let alive = {
        let conn = Connection::open(db_path).context("open read connection for SEI mint pass")?;
        alive_bindings_snapshot(&conn).map_err(|e| anyhow::anyhow!("{e}"))?
    };

    let descriptors = dedup_descriptors_by_locator(descriptors);
    // LOAD-BEARING (Wave 2 / T3.1): the current-run locator set is the union of
    // the re-analysed entities AND the skipped-unchanged files' entities (which
    // are still present, just not re-parsed). Both `rebind_or_mint` (vanish
    // detection — never steal a still-present SEI) and `orphaned_bindings` (never
    // orphan a still-present entity) consume this set. Omitting the skipped
    // locators would falsely orphan every entity in every unchanged file.
    let mut current_locators: HashSet<String> =
        descriptors.iter().map(|d| d.locator.clone()).collect();
    current_locators.extend(retained_locators.iter().cloned());

    // The git-rename signal (best-effort, typed seam REQ-C-05), unioned across
    // two complementary windows (WS9 / SEI §6): the working tree (uncommitted
    // renames, shell `git diff -M HEAD`) and — when `legis` is configured and a
    // prior commit differs from HEAD — the committed range `<prior>..HEAD`
    // (committed renames, served by `legis` via `git log -M`, else a shell
    // fallback). The working-tree window is never handed to `legis` (its
    // committed-only endpoint cannot see it); without `legis` this is exactly the
    // one pre-WS9 working-tree call. Skipped entirely on non-repo corpora. The
    // matcher is fail-closed (a rename is a hint, confirmed by body hash), so an
    // over-broad union only ever misses a carry, never causes a false one.
    let descriptor_locators: Vec<String> = descriptors.iter().map(|d| d.locator.clone()).collect();
    let git_renames: Vec<GitRename> = crate::sei_git::gather_git_renames(
        project_root,
        legis_url,
        prior_commit,
        head_commit,
        &descriptor_locators,
    );

    // sei -> prior (vanished) locator, for the rematched set + lineage old_locator.
    let sei_to_old_locator: HashMap<String, String> = alive
        .iter()
        .map(|(loc, b)| (b.sei.clone(), loc.clone()))
        .collect();

    // Decide every entity; dedup carries of the same SEI (fail-closed re-mint —
    // two entities cannot both prove they are the one prior binding).
    let mut claimed: HashSet<String> = HashSet::new();
    let mut rematched: HashSet<String> = HashSet::new();
    let mut planned: Vec<PlannedSeiWrite> = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let mut decision =
            rebind_or_mint(&descriptor, &alive, &current_locators, &git_renames, run_id);
        if let SeiDecision::Carry { sei, .. } = &decision
            && !claimed.insert(sei.clone())
        {
            decision = SeiDecision::Mint {
                sei: mint_sei(&descriptor.locator, run_id),
            };
        }
        if let SeiDecision::Carry {
            sei,
            event: Some(_),
        } = &decision
            && let Some(old_loc) = sei_to_old_locator.get(sei)
        {
            rematched.insert(old_loc.clone());
        }
        planned.push(PlannedSeiWrite {
            descriptor,
            decision,
        });
    }

    let orphans = orphaned_bindings(&alive, &current_locators, &rematched);
    let recorded_at = iso8601_now();
    let mut stats = SeiPassStats::default();

    // REQ-ANALYZE-04: the orphaned set is exactly "prior-run entity ids minus
    // current-run set, excluding renames" — `orphaned_bindings` already excludes
    // `rematched` (carried-across-a-rename) bindings, so a renamed entity is NOT
    // reported as deleted. A locator IS an entity id (ADR-038 demotes the ADR-003
    // id to the SEI locator; `descriptor.locator == entities.id`), so the orphan's
    // `old_locator` is the deleted entity's id for the Phase-7 deletion findings.
    let mut deleted_entity_ids: Vec<String> = Vec::new();

    // WRITE ORDER (T2.2 Step 5): orphan/re-point vanished bindings FIRST so a
    // carry/mint that claims a freed locator never transiently doubles up the
    // alive-locator partial unique index.
    for sei in &orphans {
        let old_locator = sei_to_old_locator.get(sei).cloned();
        if let Some(locator) = &old_locator {
            deleted_entity_ids.push(locator.clone());
        }
        writer
            .send_wait(|ack| WriterCmd::OrphanSeiBinding {
                sei: sei.clone(),
                run_id: run_id.to_owned(),
                recorded_at: recorded_at.clone(),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("OrphanSeiBinding")?;
        writer
            .send_wait(|ack| WriterCmd::AppendSeiLineage {
                entry: Box::new(SeiLineageEntry {
                    sei: sei.clone(),
                    event: LineageEvent::Orphaned,
                    old_locator: old_locator.clone(),
                    new_locator: None,
                    run_id: run_id.to_owned(),
                    recorded_at: recorded_at.clone(),
                }),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("AppendSeiLineage(orphaned)")?;
        stats.orphaned += 1;
    }

    for PlannedSeiWrite {
        descriptor,
        decision,
    } in planned
    {
        // Persist the signature (next run's matcher input; identity is separate).
        writer
            .send_wait(|ack| WriterCmd::SetEntitySignature {
                entity_id: descriptor.locator.clone(),
                signature: descriptor.signature.clone(),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("SetEntitySignature")?;

        let is_mint = matches!(decision, SeiDecision::Mint { .. });
        let (sei, lineage_event) = match decision {
            SeiDecision::Carry { sei, event } => (sei, event),
            SeiDecision::Mint { sei } => (sei, Some(LineageEvent::Born)),
        };

        writer
            .send_wait(|ack| WriterCmd::UpsertSeiBinding {
                record: Box::new(SeiBindingRecord {
                    sei: sei.clone(),
                    current_locator: Some(descriptor.locator.clone()),
                    body_hash: descriptor.body_hash.clone(),
                    signature: descriptor.signature.clone(),
                    status: BindingStatus::Alive,
                    // Ignored on carry: ON CONFLICT(sei) preserves the original
                    // born_run_id; only an INSERT (mint) uses this value.
                    born_run_id: run_id.to_owned(),
                    updated_run_id: run_id.to_owned(),
                    updated_at: recorded_at.clone(),
                }),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("UpsertSeiBinding")?;

        if let Some(event) = lineage_event {
            let (old_locator, new_locator) = match event {
                LineageEvent::LocatorChanged | LineageEvent::Moved => (
                    sei_to_old_locator.get(&sei).cloned(),
                    Some(descriptor.locator.clone()),
                ),
                _ => (None, Some(descriptor.locator.clone())),
            };
            writer
                .send_wait(|ack| WriterCmd::AppendSeiLineage {
                    entry: Box::new(SeiLineageEntry {
                        sei: sei.clone(),
                        event,
                        old_locator,
                        new_locator,
                        run_id: run_id.to_owned(),
                        recorded_at: recorded_at.clone(),
                    }),
                    ack,
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("AppendSeiLineage")?;
        }

        if is_mint {
            stats.minted += 1;
        } else {
            stats.carried += 1;
        }
    }

    // REQ-ANALYZE-04 deletion findings (Phase 7). Deterministic order
    // (REQ-ANALYZE-07): `orphaned_bindings` returns a set, so sort + dedup before
    // emitting so back-to-back runs persist an identical id set. Runs after the
    // orphan bindings are written so a guidance-orphan scan sees the settled
    // identity state. Best-effort like the rest of the pass: a failure here logs
    // via the caller and never un-commits the already-durable graph.
    deleted_entity_ids.sort();
    deleted_entity_ids.dedup();
    stats.deletion_findings =
        emit_deletion_findings(writer, db_path, run_id, &deleted_entity_ids, &recorded_at).await?;

    Ok(stats)
}

/// Persist REQ-ANALYZE-04 Phase-7 deletion findings for `deleted_entity_ids`
/// (already sorted + deduped by the caller for determinism), returning the total
/// finding count.
///
/// For each deleted entity: emit one `LMWV-FACT-ENTITY-DELETED` (anchored to the
/// entity's own row — `entities` is never pruned, so the FK resolves) and
/// invalidate its cached summaries. Then, for every guidance sheet stranded on a
/// deleted entity — via an explicit `guides` edge OR a `match_rules`
/// `{"type":"entity","id":X}` entry (detailed-design.md §5) — emit one
/// `LMWV-FACT-GUIDANCE-ORPHAN` (anchored to the sheet, deleted target as a related
/// id). A sheet that strands the same target via both paths emits one finding.
///
/// Returns `Ok(0)` for an empty deleted set without opening a connection.
async fn emit_deletion_findings(
    writer: &Writer,
    db_path: &Path,
    run_id: &str,
    deleted_entity_ids: &[String],
    now: &str,
) -> anyhow::Result<u64> {
    if deleted_entity_ids.is_empty() {
        return Ok(0);
    }
    let deleted_set: HashSet<&str> = deleted_entity_ids.iter().map(String::as_str).collect();
    let mut count: u64 = 0;

    for entity_id in deleted_entity_ids {
        let finding = entity_deleted_finding(entity_id, run_id, now);
        let finding_id = finding.id.clone();
        writer
            .send_wait(|ack| WriterCmd::PersistPostRunFinding {
                finding: Box::new(finding),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("InsertFinding {finding_id}"))?;
        count += 1;

        writer
            .send_wait(|ack| WriterCmd::InvalidateSummaryCacheForEntity {
                entity_id: entity_id.clone(),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("InvalidateSummaryCacheForEntity {entity_id}"))?;
    }

    // Guidance sheets stranded on a now-deleted entity are orphaned via EITHER an
    // explicit `guides` edge OR a `match_rules` `{"type":"entity","id":X}` entry
    // pointing at a deleted target (detailed-design.md §5). Collect both into one
    // de-duped, sorted `(sheet, target)` set so a sheet that orphans the same
    // target via both paths emits exactly ONE finding. Both survive the target's
    // vanishing because `entities` is never pruned.
    let orphaned_guidance: std::collections::BTreeSet<(String, String)> = {
        let conn =
            Connection::open(db_path).context("open read connection for guidance-orphan scan")?;

        let mut pairs: std::collections::BTreeSet<(String, String)> = conn
            .prepare("SELECT from_id, to_id FROM edges WHERE kind = 'guides'")
            .context("prepare guides-edge scan")?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("query guides edges")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect guides edges")?
            .into_iter()
            .filter(|(_, to_id)| deleted_set.contains(to_id.as_str()))
            .collect();

        // Scan every guidance sheet's `match_rules` for `{type:entity, id:X}`
        // entries whose X is in the deleted set. Reuse the shared rule shape
        // (`loomweave_storage::rule_match` reads `{"type":"entity","id":…}`), not a
        // hand-rolled key.
        for sheet in loomweave_storage::list_guidance_sheets(&conn)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("list guidance sheets for match-rule orphan scan")?
        {
            let Some(rules) = sheet
                .properties
                .get("match_rules")
                .and_then(serde_json::Value::as_array)
            else {
                continue;
            };
            for rule in rules {
                if rule.get("type").and_then(serde_json::Value::as_str) == Some("entity")
                    && let Some(target) = rule.get("id").and_then(serde_json::Value::as_str)
                    && deleted_set.contains(target)
                {
                    pairs.insert((sheet.id.clone(), target.to_owned()));
                }
            }
        }
        pairs
    };

    for (guidance_id, deleted_target) in &orphaned_guidance {
        let finding = guidance_orphan_finding(guidance_id, deleted_target, run_id, now);
        let finding_id = finding.id.clone();
        writer
            .send_wait(|ack| WriterCmd::PersistPostRunFinding {
                finding: Box::new(finding),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("InsertFinding {finding_id}"))?;
        count += 1;
    }

    Ok(count)
}

/// Build a `LMWV-FACT-ENTITY-DELETED` finding anchored to the deleted entity's own
/// (never-pruned) row. The id is deterministic and content-keyed so re-analysis (and `--resume`)
/// re-walk regenerates the same id and `InsertFinding`'s upsert is idempotent.
fn entity_deleted_finding(entity_id: &str, run_id: &str, now: &str) -> FindingRecord {
    FindingRecord {
        id: format!("core:finding:entity-deleted:{entity_id}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: ENTITY_DELETED_RULE_ID.to_owned(),
        kind: "fact".to_owned(),
        severity: "INFO".to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some("entity absent from current run's locator set".to_owned()),
        entity_id: entity_id.to_owned(),
        related_entities_json: "[]".to_owned(),
        message: format!("Entity {entity_id} was deleted since the prior analyze run"),
        evidence_json: serde_json::json!({ "deleted_entity_id": entity_id }).to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

/// Build a `LMWV-FACT-GUIDANCE-ORPHAN` finding anchored to the guidance sheet
/// whose `guides` edge targets `deleted_entity_id`. Run-scoped, deterministic id.
fn guidance_orphan_finding(
    guidance_id: &str,
    deleted_entity_id: &str,
    run_id: &str,
    now: &str,
) -> FindingRecord {
    FindingRecord {
        id: format!("core:finding:guidance-orphan:{guidance_id}:{deleted_entity_id}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: GUIDANCE_ORPHAN_RULE_ID.to_owned(),
        kind: "fact".to_owned(),
        severity: "WARN".to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some("guidance sheet target deleted".to_owned()),
        entity_id: guidance_id.to_owned(),
        related_entities_json: serde_json::json!([deleted_entity_id]).to_string(),
        message: format!(
            "Guidance sheet {guidance_id} points at deleted entity {deleted_entity_id}"
        ),
        evidence_json: serde_json::json!({
            "guidance_id": guidance_id,
            "deleted_entity_id": deleted_entity_id,
        })
        .to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

/// REQ-GUIDANCE-05 (WS6 T4a): persist guidance-staleness findings over the
/// committed graph and return the count. Independent signals per sheet:
///
/// - **`LMWV-FACT-GUIDANCE-EXPIRED`** — the sheet's `expires` instant is lexically
///   `< now` (both are the fixed-width `YYYY-MM-DDTHH:MM:SS.sssZ` form
///   [`iso8601_now`] emits, so a byte compare is a valid instant compare). Absent
///   or malformed `expires` ⇒ skip.
/// - **`LMWV-FACT-GUIDANCE-CHURN-STALE`** — the aggregate `git_churn_count` over the
///   sheet's matched entities meets the staleness threshold (asymmetric: 20 for
///   `pinned` sheets, 50 otherwise).
///
/// Runs post-`CommitRun`, unconditionally (NOT gated on the SEI pass or on
/// deletions) — see the call site. Deterministic: sheets in
/// [`loomweave_storage::list_guidance_sheets`] order; matched ids sorted.
///
/// Churn proxy note: the design wants "churn since `authored_at`/`reviewed_at`",
/// but there is no churn-history to compute a true delta and `git_churn_count` is
/// not populated by analyze in v1.0 (so this is honest-empty in production). We
/// implement the computable proxy — the aggregate current `git_churn_count` over
/// matched entities vs the threshold. A true since-authored delta awaits the
/// churn-history pipeline (clarion-997c93ec4e); `authored_at`/`reviewed_at` are
/// deliberately unused here because no real delta is computable.
enum PendingGuidanceStaleness {
    Expired(String),
    ChurnStale {
        sheet_id: String,
        agg: i64,
        matched: Vec<String>,
    },
}

fn plan_guidance_staleness_findings(
    db_path: &Path,
    project_root: &Path,
    now: &str,
) -> anyhow::Result<Vec<PendingGuidanceStaleness>> {
    let conn = Connection::open(db_path)
        .context("open read connection for guidance-staleness findings")?;
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let sheets = loomweave_storage::list_guidance_sheets(&conn)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("list guidance sheets for staleness scan")?;

    // Entities carrying a populated churn count (the only ones that can move an
    // aggregate). Empty in production today (see fn doc).
    let churned: Vec<(String, i64)> = conn
        .prepare(
            "SELECT id, git_churn_count FROM entities \
                 WHERE git_churn_count IS NOT NULL ORDER BY id",
        )
        .context("prepare churned-entity scan")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .context("query churned entities")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collect churned entities")?;

    let mut plan = Vec::new();
    for sheet in &sheets {
        // EXPIRED: lexical (instant) compare against `now`.
        if let Some(expires) = sheet
            .properties
            .get("expires")
            .and_then(serde_json::Value::as_str)
            && expires < now
        {
            plan.push(PendingGuidanceStaleness::Expired(sheet.id.clone()));
        }

        // CHURN-STALE: aggregate churn over matched entities vs asymmetric
        // threshold. Reuse the shared matcher; only churned entities can matter.
        let pinned = sheet
            .properties
            .get("pinned")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let threshold = if pinned {
            CHURN_STALE_THRESHOLD_PINNED
        } else {
            CHURN_STALE_THRESHOLD
        };

        let mut agg: i64 = 0;
        let mut matched: Vec<String> = Vec::new();
        for (entity_id, churn) in &churned {
            if loomweave_storage::guidance_sheet_matches_entity(
                &conn,
                sheet,
                entity_id,
                &canonical_root,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("match {entity_id} against {}", sheet.id))?
            {
                agg = agg.saturating_add(*churn);
                matched.push(entity_id.clone());
            }
        }
        if agg >= threshold {
            matched.sort();
            plan.push(PendingGuidanceStaleness::ChurnStale {
                sheet_id: sheet.id.clone(),
                agg,
                matched,
            });
        }
    }
    Ok(plan)
}

async fn emit_guidance_staleness_findings(
    writer: &Writer,
    db_path: &Path,
    project_root: &Path,
    run_id: &str,
    now: &str,
) -> anyhow::Result<u64> {
    // Build the (sheet, [matched churn pairs]) plan in one read pass, then emit.
    // Drive the churn scan off the populated churn set only — `WHERE
    // git_churn_count IS NOT NULL` — so the work is O(sheets × churned), and so
    // production (no churn populated) yields an empty candidate set and CHURN-STALE
    // never fires, with no special-casing.
    let plan = plan_guidance_staleness_findings(db_path, project_root, now)?;
    let mut count: u64 = 0;
    for pending in &plan {
        let finding = match pending {
            PendingGuidanceStaleness::Expired(sheet_id) => {
                guidance_expired_finding(sheet_id, run_id, now)
            }
            PendingGuidanceStaleness::ChurnStale {
                sheet_id,
                agg,
                matched,
            } => guidance_churn_stale_finding(sheet_id, *agg, matched, run_id, now),
        };
        let finding_id = finding.id.clone();
        writer
            .send_wait(|ack| WriterCmd::PersistPostRunFinding {
                finding: Box::new(finding),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("PersistPostRunFinding {finding_id}"))?;
        count += 1;
    }
    Ok(count)
}

/// Build a `LMWV-FACT-GUIDANCE-EXPIRED` finding anchored to the expired sheet.
/// Run-scoped, deterministic id; INFO, confidence 1.0.
fn guidance_expired_finding(guidance_id: &str, run_id: &str, now: &str) -> FindingRecord {
    FindingRecord {
        id: format!("core:finding:guidance-expired:{guidance_id}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: GUIDANCE_EXPIRED_RULE_ID.to_owned(),
        kind: "fact".to_owned(),
        severity: "INFO".to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some("guidance sheet past its `expires`".to_owned()),
        entity_id: guidance_id.to_owned(),
        related_entities_json: "[]".to_owned(),
        message: format!("Guidance sheet {guidance_id} is past its `expires` instant"),
        evidence_json: serde_json::json!({ "guidance_id": guidance_id }).to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

/// Build a `LMWV-FACT-GUIDANCE-CHURN-STALE` finding anchored to the sheet, carrying
/// the matched entities (sorted) as related ids and the aggregate churn +
/// threshold as evidence. Run-scoped, deterministic id; WARN, confidence 0.7
/// (heuristic).
fn guidance_churn_stale_finding(
    guidance_id: &str,
    aggregate_churn: i64,
    matched: &[String],
    run_id: &str,
    now: &str,
) -> FindingRecord {
    FindingRecord {
        id: format!("core:finding:guidance-churn-stale:{guidance_id}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: GUIDANCE_CHURN_STALE_RULE_ID.to_owned(),
        kind: "fact".to_owned(),
        severity: "WARN".to_owned(),
        confidence: Some(0.7),
        confidence_basis: Some("heuristic".to_owned()),
        entity_id: guidance_id.to_owned(),
        related_entities_json: serde_json::to_string(matched).unwrap_or_else(|_| "[]".to_owned()),
        message: format!(
            "Guidance sheet {guidance_id} covers high-churn code (aggregate git_churn_count = {aggregate_churn})"
        ),
        evidence_json: serde_json::json!({
            "guidance_id": guidance_id,
            "aggregate_git_churn_count": aggregate_churn,
            "matched_entities": matched,
        })
        .to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

/// Extract a subsystem-member's Wardline tier from its opaque `wardline_json`
/// blob: the best-effort top-level `tier` field, stringified. Kept byte-identical
/// to the MCP `find_by_wardline` read path (`facet_matches`) so the analyze-side
/// consensus and the query-side filter never disagree. A blob with no scalar
/// `tier` field contributes no tier (the entity is excluded from consensus).
fn extract_wardline_tier(wardline_json: &str) -> Option<String> {
    let blob: serde_json::Value = serde_json::from_str(wardline_json).ok()?;
    match blob.get("tier") {
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(serde_json::Value::Number(value)) => Some(value.to_string()),
        Some(serde_json::Value::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

/// REQ-ANALYZE-05 Phase-7 structural findings combining Phase-3 clustering with
/// Wardline tier declarations — signals no single sibling holds alone.
///
/// Wardline tiers land on functions (`python:function:<qualname>`), not modules,
/// so each tier-bearing entity is resolved up its `contains` chain to the
/// subsystem it belongs to (`subsystem_of_entity`). Per subsystem, over its
/// tier-bearing members: ≥2 distinct tiers ⇒ `LMWV-FACT-TIER-SUBSYSTEM-MIXING`;
/// exactly one tier across ≥2 members ⇒ `LMWV-FACT-SUBSYSTEM-TIER-UNANIMOUS`. A
/// single tier-bearing member yields neither (no consensus from one voice).
///
/// Conditional on a prior Wardline ingest: `analyze` never writes tier facts (the
/// enrich-only axiom), so a project that has not ingested Wardline produces no
/// tier findings — correct, not a gap. Runs post-`CommitRun` (the `in_subsystem`
/// edges are durable by then); persists via `PersistPostRunFinding`. Returns the
/// finding count. Deterministic: subsystems and members are sorted before emit.
async fn emit_tier_subsystem_findings(
    writer: &Writer,
    db_path: &Path,
    run_id: &str,
    now: &str,
) -> anyhow::Result<u64> {
    use std::collections::BTreeMap;

    // (subsystem_id -> sorted members [(entity_id, tier)]). Read-only over the
    // committed graph. The tier-bearing set is bounded by Wardline-tagged
    // entities; read it whole (no cap) — a partial set would compute the WRONG
    // consensus, which REQ-ANALYZE-06's no-silent-fallback discipline forbids.
    let by_subsystem: BTreeMap<String, Vec<(String, String)>> = {
        let conn = Connection::open(db_path)
            .context("open read connection for tier-subsystem findings")?;
        let mut stmt = conn
            .prepare("SELECT entity_id, wardline_json FROM wardline_taint_facts ORDER BY entity_id")
            .context("prepare wardline-taint scan")?;
        let tagged: Vec<(String, String)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("query wardline taint facts")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect wardline taint facts")?;

        let mut map: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (entity_id, wardline_json) in tagged {
            let Some(tier) = extract_wardline_tier(&wardline_json) else {
                continue;
            };
            if let Some(subsystem) = loomweave_storage::subsystem_of_entity(&conn, &entity_id)
                .map_err(|e| anyhow::anyhow!("{e}"))
                .with_context(|| format!("resolve subsystem for {entity_id}"))?
            {
                map.entry(subsystem.subsystem_id)
                    .or_default()
                    .push((entity_id, tier));
            }
        }
        // Members arrive in entity_id order (the scan is ORDERed); keep it.
        map
    };

    let mut count: u64 = 0;
    for (subsystem_id, members) in &by_subsystem {
        if members.len() < 2 {
            continue;
        }
        let distinct: std::collections::BTreeSet<&str> =
            members.iter().map(|(_, tier)| tier.as_str()).collect();
        let finding = if distinct.len() >= 2 {
            tier_mixing_finding(subsystem_id, members, run_id, now)
        } else {
            // Exactly one distinct tier across ≥2 members.
            let tier = distinct.iter().next().expect("one tier present");
            tier_unanimous_finding(subsystem_id, tier, members, run_id, now)
        };
        let finding_id = finding.id.clone();
        writer
            .send_wait(|ack| WriterCmd::PersistPostRunFinding {
                finding: Box::new(finding),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("PersistPostRunFinding {finding_id}"))?;
        count += 1;
    }
    Ok(count)
}

/// Build a `LMWV-FACT-TIER-SUBSYSTEM-MIXING` finding anchored to the subsystem,
/// carrying its tier-bearing members as related ids and the tier distribution as
/// evidence. Members are pre-sorted by the caller; the id is content-keyed.
fn tier_mixing_finding(
    subsystem_id: &str,
    members: &[(String, String)],
    run_id: &str,
    now: &str,
) -> FindingRecord {
    let member_ids: Vec<&str> = members.iter().map(|(id, _)| id.as_str()).collect();
    let mut tier_counts: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for (_, tier) in members {
        *tier_counts.entry(tier.as_str()).or_default() += 1;
    }
    FindingRecord {
        id: format!("core:finding:tier-mixing:{subsystem_id}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: TIER_MIXING_RULE_ID.to_owned(),
        kind: "fact".to_owned(),
        severity: "WARN".to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some("subsystem members declare disagreeing Wardline tiers".to_owned()),
        entity_id: subsystem_id.to_owned(),
        related_entities_json: serde_json::to_string(&member_ids)
            .unwrap_or_else(|_| "[]".to_owned()),
        message: format!(
            "Subsystem {subsystem_id} mixes {} Wardline tiers",
            tier_counts.len()
        ),
        evidence_json: serde_json::json!({ "tier_distribution": tier_counts }).to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

/// Build a `LMWV-FACT-SUBSYSTEM-TIER-UNANIMOUS` finding (positive signal) anchored
/// to the subsystem whose ≥2 tier-bearing members all share `tier`.
fn tier_unanimous_finding(
    subsystem_id: &str,
    tier: &str,
    members: &[(String, String)],
    run_id: &str,
    now: &str,
) -> FindingRecord {
    let member_ids: Vec<&str> = members.iter().map(|(id, _)| id.as_str()).collect();
    FindingRecord {
        id: format!("core:finding:tier-unanimous:{subsystem_id}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: TIER_UNANIMOUS_RULE_ID.to_owned(),
        kind: "fact".to_owned(),
        severity: "INFO".to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some(
            "all tier-bearing subsystem members share one Wardline tier".to_owned(),
        ),
        entity_id: subsystem_id.to_owned(),
        related_entities_json: serde_json::to_string(&member_ids)
            .unwrap_or_else(|_| "[]".to_owned()),
        message: format!("Subsystem {subsystem_id} is unanimous in Wardline tier {tier}"),
        evidence_json: serde_json::json!({
            "tier": tier,
            "member_count": members.len(),
        })
        .to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

// ── Phase 3 subsystem materialisation ─────────────────────────────────────────

#[derive(Debug, Clone)]
struct Phase3Output {
    subsystems_inserted: u64,
    in_subsystem_edges_inserted: u64,
    weak_modularity_finding: bool,
    clustering_stats: serde_json::Value,
}

impl Phase3Output {
    fn not_run() -> Self {
        Self {
            subsystems_inserted: 0,
            in_subsystem_edges_inserted: 0,
            weak_modularity_finding: false,
            clustering_stats: serde_json::Value::Null,
        }
    }
}

#[derive(Debug, Clone)]
struct InsertedSubsystem {
    id: String,
    member_count: usize,
}

#[allow(clippy::too_many_lines)]
async fn run_phase3_clustering(
    writer: &Writer,
    db_path: &Path,
    run_id: &str,
    analyze_config: &AnalyzeConfig,
    head_commit: Option<&str>,
) -> Result<Phase3Output> {
    let started = std::time::Instant::now();
    let config = &analyze_config.analysis.clustering;
    if !config.enabled {
        return Ok(Phase3Output {
            subsystems_inserted: 0,
            in_subsystem_edges_inserted: 0,
            weak_modularity_finding: false,
            clustering_stats: phase3_stats_json(
                config,
                config.algorithm,
                "disabled",
                Some("disabled"),
                0,
                0,
                0,
                None,
                0,
                0,
                false,
                started,
            ),
        });
    }

    writer
        .send_wait(|ack| WriterCmd::FlushRunBatch { ack })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("FlushRunBatch before phase3 clustering")?;

    let conn = Connection::open(db_path).context("open read connection for phase3 clustering")?;
    let module_ids = module_entity_ids(&conn).context("load module entities for phase3")?;
    let edge_type_names = config
        .edge_types
        .iter()
        .map(|edge_type| edge_type.as_str())
        .collect::<Vec<_>>();
    let dependency_edges = module_dependency_edges(&conn, &edge_type_names)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("load module dependency edges for phase3")?;

    if dependency_edges.is_empty() {
        return Ok(Phase3Output {
            subsystems_inserted: 0,
            in_subsystem_edges_inserted: 0,
            weak_modularity_finding: false,
            clustering_stats: phase3_stats_json(
                config,
                config.algorithm,
                "skipped",
                Some("no_module_dependency_edges"),
                module_ids.len(),
                0,
                0,
                None,
                0,
                0,
                false,
                started,
            ),
        });
    }

    let graph = ModuleGraph {
        modules: module_ids,
        edges: dependency_edges
            .iter()
            .map(|edge| ModuleEdge {
                from: edge.from_module_id.clone(),
                to: edge.to_module_id.clone(),
                reference_count: edge.reference_count,
            })
            .collect(),
    };
    let cluster_config = ClusterConfig {
        algorithm: config.algorithm,
        seed: config.seed,
        resolution: config.resolution,
        max_iterations: config.max_iterations,
        min_cluster_size: config.min_cluster_size,
    };
    let cluster_result = cluster_modules(&graph, &cluster_config).context("cluster modules")?;

    if cluster_result.communities.is_empty() {
        return Ok(Phase3Output {
            subsystems_inserted: 0,
            in_subsystem_edges_inserted: 0,
            weak_modularity_finding: false,
            clustering_stats: phase3_stats_json(
                config,
                cluster_result.algorithm_used,
                "skipped",
                Some("no_clusters_emitted"),
                graph.modules.len(),
                graph.edges.len(),
                0,
                Some(cluster_result.modularity_score),
                0,
                0,
                false,
                started,
            ),
        });
    }

    let mut inserted_subsystems = Vec::new();
    let mut in_subsystem_edges_inserted = 0_u64;
    let edge_type_values = config
        .edge_types
        .iter()
        .map(|edge_type| edge_type.as_str())
        .collect::<Vec<_>>();
    for community in &cluster_result.communities {
        let hash = cluster_hash(community);
        let subsystem_id = subsystem_entity_id(&hash)
            .with_context(|| format!("assemble subsystem entity id for hash {hash}"))?;
        let (subsystem_name, subsystem_short_name) = subsystem_display_name(community, &hash);
        let now = iso8601_now();
        let properties_json = serde_json::json!({
            "algorithm": cluster_result.algorithm_used.as_str(),
            "seed": config.seed,
            "resolution": config.resolution,
            "max_iterations": config.max_iterations,
            "modularity_score": cluster_result.modularity_score,
            "cluster_hash": hash,
            "member_module_ids": community,
            "member_count": community.len(),
            "edge_types": edge_type_values,
            "weight_by": config.weight_by.as_str(),
        })
        .to_string();
        let mut entity = EntityRecord {
            id: subsystem_id.clone(),
            plugin_id: "core".to_owned(),
            kind: "subsystem".to_owned(),
            name: subsystem_name,
            short_name: subsystem_short_name,
            parent_id: None,
            source_file_id: None,
            source_file_path: None,
            source_byte_start: None,
            source_byte_end: None,
            source_line_start: None,
            source_line_end: None,
            properties_json,
            tags: Vec::new(),
            content_hash: None,
            summary_json: None,
            wardline_json: None,
            first_seen_commit: None,
            last_seen_commit: None,
            created_at: now.clone(),
            updated_at: now,
        };
        stamp_entity_git_provenance(&mut entity, head_commit);
        writer
            .send_wait(|ack| WriterCmd::InsertEntity {
                entity: Box::new(entity),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("InsertEntity subsystem {subsystem_id}"))?;

        for module_id in community {
            writer
                .send_wait(|ack| WriterCmd::InsertEdge {
                    edge: Box::new(EdgeRecord {
                        kind: "in_subsystem".to_owned(),
                        from_id: module_id.clone(),
                        to_id: subsystem_id.clone(),
                        confidence: loomweave_core::EdgeConfidence::Resolved,
                        properties_json: None,
                        source_file_id: None,
                        source_byte_start: None,
                        source_byte_end: None,
                    }),
                    ack,
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .with_context(|| {
                    format!("InsertEdge in_subsystem {module_id} -> {subsystem_id}")
                })?;
            in_subsystem_edges_inserted += 1;
        }

        inserted_subsystems.push(InsertedSubsystem {
            id: subsystem_id,
            member_count: community.len(),
        });
    }

    let weak_modularity_finding_emitted = if config.weak_modularity_threshold > 0.0
        && cluster_result.modularity_score < config.weak_modularity_threshold
    {
        insert_weak_modularity_finding(
            writer,
            run_id,
            config,
            &inserted_subsystems,
            cluster_result.modularity_score,
        )
        .await?
    } else {
        false
    };

    let subsystems_inserted = u64::try_from(inserted_subsystems.len()).unwrap_or(u64::MAX);
    Ok(Phase3Output {
        subsystems_inserted,
        in_subsystem_edges_inserted,
        weak_modularity_finding: weak_modularity_finding_emitted,
        clustering_stats: phase3_stats_json(
            config,
            cluster_result.algorithm_used,
            "completed",
            None,
            graph.modules.len(),
            graph.edges.len(),
            inserted_subsystems.len(),
            Some(cluster_result.modularity_score),
            subsystems_inserted,
            in_subsystem_edges_inserted,
            weak_modularity_finding_emitted,
            started,
        ),
    })
}

fn subsystem_entity_id(cluster_hash: &str) -> Result<String> {
    Ok(loomweave_core::entity_id::entity_id("core", "subsystem", cluster_hash)?.to_string())
}

fn subsystem_display_name(member_ids: &[String], cluster_hash: &str) -> (String, String) {
    common_module_prefix(member_ids).map_or_else(
        || (format!("Subsystem {cluster_hash}"), cluster_hash.to_owned()),
        |prefix| (prefix.clone(), prefix),
    )
}

fn common_module_prefix(member_ids: &[String]) -> Option<String> {
    let mut names = member_ids.iter().filter_map(|id| entity_qualified_name(id));
    let first = names.next()?;
    let mut common = first.split('.').collect::<Vec<_>>();
    for name in names {
        let parts = name.split('.').collect::<Vec<_>>();
        let shared = common
            .iter()
            .zip(parts.iter())
            .take_while(|(left, right)| left == right)
            .count();
        common.truncate(shared);
        if common.is_empty() {
            return None;
        }
    }
    if common.is_empty() {
        None
    } else {
        Some(common.join("."))
    }
}

fn entity_qualified_name(entity_id: &str) -> Option<&str> {
    let mut parts = entity_id.splitn(3, ':');
    let _plugin_id = parts.next()?;
    let _kind = parts.next()?;
    let qualified = parts.next()?;
    if qualified.is_empty() {
        None
    } else {
        Some(qualified)
    }
}

async fn insert_weak_modularity_finding(
    writer: &Writer,
    run_id: &str,
    config: &ClusteringConfig,
    subsystems: &[InsertedSubsystem],
    modularity_score: f64,
) -> Result<bool> {
    let Some(anchor) = subsystems
        .iter()
        .max_by_key(|subsystem| (subsystem.member_count, std::cmp::Reverse(&subsystem.id)))
    else {
        return Ok(false);
    };
    let subsystem_ids = subsystems
        .iter()
        .map(|subsystem| subsystem.id.clone())
        .collect::<Vec<_>>();
    let now = iso8601_now();
    let finding_id = "core:finding:weak-modularity".to_owned();
    let related_entities_json = serde_json::to_string(&subsystem_ids)
        .context("serialize weak modularity related_entities")?;
    writer
        .send_wait(|ack| WriterCmd::InsertFinding {
            finding: Box::new(FindingRecord {
                id: finding_id.clone(),
                tool: "loomweave".to_owned(),
                tool_version: env!("CARGO_PKG_VERSION").to_owned(),
                run_id: run_id.to_owned(),
                rule_id: WEAK_MODULARITY_RULE_ID.to_owned(),
                kind: "fact".to_owned(),
                severity: "INFO".to_owned(),
                confidence: Some(1.0),
                confidence_basis: Some("deterministic module graph modularity".to_owned()),
                entity_id: anchor.id.clone(),
                related_entities_json,
                message: "Module graph has weak subsystem modularity".to_owned(),
                evidence_json: serde_json::json!({
                    "modularity_score": modularity_score,
                    "threshold": config.weak_modularity_threshold,
                    "subsystem_count": subsystems.len(),
                })
                .to_string(),
                properties_json: serde_json::json!({
                    "algorithm": config.algorithm.as_str(),
                    "modularity_score": modularity_score,
                    "threshold": config.weak_modularity_threshold,
                    "subsystem_count": subsystems.len(),
                })
                .to_string(),
                supports_json: "[]".to_owned(),
                supported_by_json: "[]".to_owned(),
                created_at: now.clone(),
                updated_at: now,
            }),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("InsertFinding {finding_id}"))?;
    Ok(true)
}

/// Build a `LMWV-PY-SYNTAX-ERROR` finding for an accepted entity the plugin
/// flagged `parse_status="syntax_error"`, or `None` for cleanly-parsed entities.
///
/// The finding anchors to the degraded entity itself (the plugin still emits one
/// manifest-declared degraded-syntax entity for a syntax-failed file), so no
/// synthetic anchor is needed.
/// The id is deterministic and content-keyed so re-analysis (and `--resume`) re-walk regenerates the
/// same id and `InsertFinding`'s upsert is idempotent (REQ-FINDING-05).
fn syntax_error_finding(
    record: &EntityRecord,
    kind_roles: &PluginKindRoles,
    run_id: &str,
    now: &str,
) -> Option<FindingRecord> {
    if !kind_roles.is_syntax_degraded_module(&record.kind) {
        return None;
    }
    let props: serde_json::Value = serde_json::from_str(&record.properties_json).ok()?;
    if props
        .get("parse_status")
        .and_then(serde_json::Value::as_str)
        != Some("syntax_error")
    {
        return None;
    }
    Some(FindingRecord {
        id: format!("core:finding:syntax-error:{}", record.id),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: SYNTAX_ERROR_RULE_ID.to_owned(),
        kind: "defect".to_owned(),
        severity: "WARN".to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some("plugin parse_status".to_owned()),
        entity_id: record.id.clone(),
        related_entities_json: "[]".to_owned(),
        message: format!(
            "{}: syntax error prevented full extraction; file ingested as a degraded plugin entity",
            record.name
        ),
        evidence_json: serde_json::json!({
            "parse_status": "syntax_error",
            "source_file_path": record.source_file_path,
        })
        .to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    })
}

/// Core-emitted crash subcode (REQ-ANALYZE-06). Distinct from the crash-loop
/// breaker subcode (`FINDING_DISABLED_CRASH_LOOP`): this fires per plugin crash,
/// the breaker subcode fires once when the breaker trips.
const INFRA_CRASH_RULE_ID: &str = "LMWV-INFRA-PLUGIN-CRASH";
const SOURCE_WALK_SKIPPED_RULE_ID: &str = "LMWV-INFRA-SOURCE-WALK-SKIPPED";
const SOURCE_WALK_ERROR_SAMPLE_LIMIT: usize = 10;

/// Anchor entity id for project/plugin-level findings that are not file-scoped
/// (plugin crash, OOM, protocol/ontology violations). `findings.entity_id` is
/// NOT NULL + FK, and no project entity otherwise exists, so the run mints one
/// synthetic `core:project:{name}` anchor (idempotent upsert) before persisting.
fn project_anchor_id(project_root: &Path) -> String {
    let name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("root");
    format!("core:project:{name}")
}

/// Idempotently mint the synthetic project anchor entity. Mirrors the secret-scan
/// file anchor (`core:file:{path}`): `finding_anchor=true`, `plugin_id="core"`.
async fn ensure_project_anchor(
    writer: &Writer,
    project_root: &Path,
    started_at: &str,
    head_commit: Option<&str>,
) -> Result<String> {
    let id = project_anchor_id(project_root);
    let name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("root")
        .to_owned();
    let properties = serde_json::json!({ "finding_anchor": true }).to_string();
    let mut record = EntityRecord {
        id: id.clone(),
        plugin_id: "core".to_owned(),
        kind: "project".to_owned(),
        name: name.clone(),
        short_name: name,
        parent_id: None,
        source_file_id: None,
        // Deliberately NO source_file_path (weft-4165f1ed71): the anchor is the
        // project itself, not a source file. Recording the project ROOT
        // DIRECTORY here put a directory into every freshness stat scan — and
        // its PARENT (e.g. $HOME) into the former structural watch set — which
        // wedged the staleness flag to a permanent false "stale". The read side
        // also skips directory paths defensively for stores written before
        // this fix.
        source_file_path: None,
        source_byte_start: None,
        source_byte_end: None,
        source_line_start: None,
        source_line_end: None,
        properties_json: properties,
        tags: Vec::new(),
        content_hash: None,
        summary_json: None,
        wardline_json: None,
        first_seen_commit: None,
        last_seen_commit: None,
        created_at: started_at.to_owned(),
        updated_at: started_at.to_owned(),
    };
    stamp_entity_git_provenance(&mut record, head_commit);
    writer
        .send_wait(|ack| WriterCmd::InsertEntity {
            entity: Box::new(record),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("InsertEntity for project finding anchor {id}"))?;
    Ok(id)
}

/// Core-emitted per-file analysis-timeout subcode (REQ-ANALYZE-06). Host-side:
/// the plugin is killed when a single `analyze_file` exceeds the deadline.
const PLUGIN_TIMEOUT_RULE_ID: &str = "LMWV-PY-TIMEOUT";
const PLUGIN_JAIL_OPEN_RULE_ID: &str = "LMWV-INFRA-PLUGIN-JAIL-OPEN-FAILED";

/// Per-file `analyze_file` deadline. ADR-035 tuning: basis — a single file's
/// extraction (incl. pyright queries) completes in well under a second on
/// healthy plugins, so minutes of no progress means a hung plugin, not slow
/// work; override — env `LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS`; retune — raise if a
/// legitimate analyzer (very large generated file) trips it in practice.
const DEFAULT_PLUGIN_FILE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const PLUGIN_WATCHDOG_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Resolve the per-file analysis timeout, honouring the env override.
fn plugin_file_timeout() -> std::time::Duration {
    std::env::var("LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(
            DEFAULT_PLUGIN_FILE_TIMEOUT,
            std::time::Duration::from_millis,
        )
}

/// Handshake (`initialize`) deadline (ADR-050). ADR-035 tuning: basis — a
/// plugin may legitimately do whole-repo work inside `initialize` (the Rust
/// plugin builds its symbol table there; at syn's ~40 MB/s parse throughput,
/// 300 s covers a ~10 M-LOC repo), so this budget scales with repo size, not
/// per-file work; override — env `LOOMWEAVE_PLUGIN_HANDSHAKE_TIMEOUT_MS`;
/// retune — raise if a legitimate repo trips it in practice.
const DEFAULT_PLUGIN_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Resolve the handshake timeout, honouring the env override.
fn plugin_handshake_timeout() -> std::time::Duration {
    std::env::var("LOOMWEAVE_PLUGIN_HANDSHAKE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(
            DEFAULT_PLUGIN_HANDSHAKE_TIMEOUT,
            std::time::Duration::from_millis,
        )
}

/// Subcode for a plugin that went silent at `shutdown` after the work
/// completed (ADR-050 D7). WARN, never ERROR: the entities are durable and
/// the run outcome is unchanged — only the plugin's exit etiquette failed.
const PLUGIN_SHUTDOWN_TIMEOUT_RULE_ID: &str = "LMWV-INFRA-PLUGIN-SHUTDOWN-TIMEOUT";

/// Shutdown (`shutdown`/`exit`) deadline (ADR-050). ADR-035 tuning: basis —
/// after the file loop there is no legitimate work left; shutdown is pure
/// exit etiquette, and a healthy plugin acknowledges within milliseconds, so
/// 10 s is generous for a loaded machine; override — env
/// `LOOMWEAVE_PLUGIN_SHUTDOWN_TIMEOUT_MS`; retune — raise if a legitimate
/// plugin needs longer to flush state at exit.
const DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Resolve the shutdown timeout, honouring the env override.
fn plugin_shutdown_timeout() -> std::time::Duration {
    std::env::var("LOOMWEAVE_PLUGIN_SHUTDOWN_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(
            DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT,
            std::time::Duration::from_millis,
        )
}

/// Map a host-layer subcode to an ADR-017 severity. Crash / kill / OOM / timeout
/// are `ERROR` (the plugin or a file was lost); drop-and-continue diagnostics
/// (malformed/undeclared/oversize) are `WARN`.
fn infra_severity(subcode: &str) -> &'static str {
    match subcode {
        INFRA_CRASH_RULE_ID
        | PLUGIN_TIMEOUT_RULE_ID
        | FINDING_DISABLED_CRASH_LOOP
        | FINDING_PLUGIN_ABORTED
        // A duplicate locator is silent last-write-wins data loss in the
        // store (clarion-b19fe90c3e) — an ERROR even though the run completes.
        | DUPLICATE_LOCATOR_RULE_ID
        | "LMWV-INFRA-PLUGIN-OOM-KILLED"
        | "LMWV-INFRA-PLUGIN-DISABLED-PATH-ESCAPE" => "ERROR",
        // The default WARN arm deliberately covers
        // `PLUGIN_SHUTDOWN_TIMEOUT_RULE_ID`: a shutdown timeout is exit
        // etiquette, never lost work (ADR-050 D7).
        _ => "WARN",
    }
}

/// Convert a collected [`HostFinding`] into a persistable [`FindingRecord`]
/// anchored to `anchor_id` (REQ-ANALYZE-06). The id is deterministic
/// (run + plugin + subcode + message digest) so `InsertFinding`'s upsert is
/// idempotent across `--resume` re-walks and dedups identical diagnostics.
fn host_finding_to_record(
    hf: &HostFinding,
    plugin_id: &str,
    anchor_id: &str,
    run_id: &str,
    now: &str,
) -> FindingRecord {
    let discriminator =
        blake3::hash(format!("{plugin_id}\u{0}{}\u{0}{}", hf.subcode, hf.message).as_bytes())
            .to_hex();
    let evidence = serde_json::json!({
        "plugin_id": plugin_id,
        "metadata": hf.metadata,
    })
    .to_string();
    FindingRecord {
        id: format!("core:finding:infra:{discriminator}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: hf.subcode.clone(),
        kind: "defect".to_owned(),
        severity: infra_severity(hf.subcode.as_str()).to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some("host enforcement".to_owned()),
        entity_id: anchor_id.to_owned(),
        related_entities_json: "[]".to_owned(),
        message: hf.message.clone(),
        evidence_json: evidence,
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

fn host_finding_anchor_id(hf: &HostFinding, project_root: &Path, project_anchor: &str) -> String {
    hf.metadata
        .get("anchor_file_path")
        .and_then(|path| core_file_entity_id(project_root, Path::new(path)).ok())
        .unwrap_or_else(|| project_anchor.to_owned())
}

fn verified_plugin_dispatch_path(project_root: &Path, file: &Path) -> Result<PathBuf> {
    let _handle = loomweave_core::plugin::jail::safe_open(project_root, file)
        .with_context(|| format!("safe-open {}", file.display()))?;
    let jailed = loomweave_core::plugin::jail::jail_to_string(project_root, file)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("jail-check {}", file.display()))?;
    Ok(PathBuf::from(jailed))
}

fn jail_open_failed_finding(file: &Path, error: &anyhow::Error) -> HostFinding {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "anchor_file_path".to_owned(),
        file.to_string_lossy().into_owned(),
    );
    metadata.insert("file_path".to_owned(), file.to_string_lossy().into_owned());
    metadata.insert("error".to_owned(), format!("{error:#}"));
    HostFinding {
        subcode: PLUGIN_JAIL_OPEN_RULE_ID.to_owned(),
        message: format!(
            "source file skipped before plugin dispatch because jail-safe open failed: {}",
            file.display()
        ),
        metadata,
    }
}

/// Build the `LMWV-INFRA-PLUGIN-CRASH` finding for a plugin that crashed mid-run
/// (REQ-ANALYZE-06). Anchored to the project entity; the crash reason is the
/// evidence.
fn crash_finding_record(
    plugin_id: &str,
    reason: &str,
    anchor_id: &str,
    run_id: &str,
    now: &str,
) -> FindingRecord {
    let discriminator = blake3::hash(format!("{plugin_id}\u{0}{reason}").as_bytes()).to_hex();
    FindingRecord {
        id: format!("core:finding:crash:{discriminator}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: INFRA_CRASH_RULE_ID.to_owned(),
        kind: "defect".to_owned(),
        severity: "ERROR".to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some("host supervision".to_owned()),
        entity_id: anchor_id.to_owned(),
        related_entities_json: "[]".to_owned(),
        message: format!("plugin {plugin_id} crashed mid-run: {reason}"),
        evidence_json: serde_json::json!({ "plugin_id": plugin_id, "reason": reason }).to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

fn source_walk_finding_record(
    project_root: &Path,
    skipped_entries: u64,
    error_samples: &[String],
    errors_omitted: usize,
    anchor_id: &str,
    run_id: &str,
    now: &str,
) -> FindingRecord {
    let discriminator =
        blake3::hash(format!("{}\u{0}{skipped_entries}", project_root.display()).as_bytes())
            .to_hex();
    FindingRecord {
        id: format!("core:finding:source-walk:{discriminator}"),
        tool: "loomweave".to_owned(),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_id: run_id.to_owned(),
        rule_id: SOURCE_WALK_SKIPPED_RULE_ID.to_owned(),
        kind: "defect".to_owned(),
        severity: "WARN".to_owned(),
        confidence: Some(1.0),
        confidence_basis: Some("source tree walk".to_owned()),
        entity_id: anchor_id.to_owned(),
        related_entities_json: "[]".to_owned(),
        message: format!(
            "source tree walk skipped {skipped_entries} unreadable or invalid entr{}; analysis is incomplete for those paths",
            if skipped_entries == 1 { "y" } else { "ies" }
        ),
        evidence_json: serde_json::json!({
            "project_root": project_root.display().to_string(),
            "skipped_entries": skipped_entries,
            "error_samples": error_samples,
            "errors_omitted": errors_omitted,
        })
        .to_string(),
        properties_json: "{}".to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    }
}

/// Load the MCP-side config (Filigree integration) from the same `loomweave.yaml`
/// `loomweave serve` reads. A missing or unparseable file falls back to the
/// default (Filigree disabled), so a config problem never fails the run — it
/// just means no emission.
pub(crate) fn load_mcp_config(project_root: &Path, config_path: Option<&Path>) -> McpConfig {
    let path = config_path.map_or_else(|| project_root.join("loomweave.yaml"), Path::to_path_buf);
    if !path.exists() {
        return McpConfig::default();
    }
    McpConfig::from_path(&path).unwrap_or_else(|err| {
        tracing::warn!(
            path = %path.display(),
            error = %err,
            "load MCP config for finding emission failed; emission disabled",
        );
        McpConfig::default()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticEmbeddingStats {
    considered: u64,
    skipped_fresh: u64,
    embedded: u64,
    tokens_input: u64,
    model_id: String,
}

#[derive(Debug)]
struct SemanticEmbeddingCandidate {
    entity_id: String,
    content_hash: String,
    text: String,
}

async fn populate_semantic_embeddings(
    project_root: &Path,
    db_path: &Path,
    config: &SemanticSearchConfig,
    provider: Arc<dyn EmbeddingProvider>,
) -> Result<SemanticEmbeddingStats> {
    let model_id = provider.model_id().to_owned();
    let mut stats = SemanticEmbeddingStats {
        considered: 0,
        skipped_fresh: 0,
        embedded: 0,
        tokens_input: 0,
        model_id: model_id.clone(),
    };
    if !config.enabled {
        return Ok(stats);
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("open Loomweave database {}", db_path.display()))?;
    let store = EmbeddingStore::open_in_store_dir(project_root)
        .map_err(|err| anyhow::anyhow!("{err}"))
        .context("open semantic embedding sidecar")?;
    let pending = semantic_embedding_candidates(&conn, &store, &model_id, &mut stats)?;
    if pending.is_empty() {
        return Ok(stats);
    }

    let token_estimates: Vec<u32> = pending
        .iter()
        .map(|candidate| {
            u32::try_from(provider.estimate_tokens(std::slice::from_ref(&candidate.text)))
                .unwrap_or(u32::MAX)
        })
        .collect();
    stats.tokens_input = token_estimates
        .iter()
        .map(|tokens| u64::from(*tokens))
        .sum();
    if stats.tokens_input > config.session_token_ceiling {
        bail!(
            "semantic embedding token estimate {} exceeds semantic_search.session_token_ceiling {}",
            stats.tokens_input,
            config.session_token_ceiling
        );
    }

    let now = iso8601_now();
    for (batch_index, batch) in pending.chunks(SEMANTIC_EMBEDDING_BATCH_SIZE).enumerate() {
        let texts: Vec<String> = batch
            .iter()
            .map(|candidate| candidate.text.clone())
            .collect();
        let vectors = provider
            .embed(&texts)
            .await
            .with_context(|| format!("embed {} semantic candidate(s)", texts.len()))?;
        if vectors.len() != batch.len() {
            bail!(
                "embedding provider returned {} vectors for {} semantic candidate(s)",
                vectors.len(),
                batch.len()
            );
        }
        for (local_index, (candidate, vector)) in batch.iter().zip(vectors.iter()).enumerate() {
            if vector.len() != provider.dimensions() {
                bail!(
                    "embedding provider returned {} dims for {}; expected {}",
                    vector.len(),
                    candidate.entity_id,
                    provider.dimensions()
                );
            }
            let token_index = batch_index * SEMANTIC_EMBEDDING_BATCH_SIZE + local_index;
            store
                .upsert(
                    &EmbeddingKey {
                        entity_id: candidate.entity_id.clone(),
                        content_hash: candidate.content_hash.clone(),
                        model_id: model_id.clone(),
                    },
                    vector,
                    0.0,
                    token_estimates[token_index],
                    &now,
                )
                .map_err(|err| anyhow::anyhow!("{err}"))
                .with_context(|| {
                    format!("persist semantic embedding for {}", candidate.entity_id)
                })?;
            stats.embedded += 1;
        }
    }

    Ok(stats)
}

fn semantic_embedding_candidates(
    conn: &Connection,
    store: &EmbeddingStore,
    model_id: &str,
    stats: &mut SemanticEmbeddingStats,
) -> Result<Vec<SemanticEmbeddingCandidate>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, short_name, properties, content_hash \
             FROM entities \
             WHERE content_hash IS NOT NULL \
               AND briefing_blocked IS NULL \
             ORDER BY id",
        )
        .context("query semantic embedding candidates")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .context("read semantic embedding candidates")?;

    let mut pending = Vec::new();
    for row in rows {
        let (entity_id, name, short_name, properties_json, content_hash) =
            row.context("read semantic embedding candidate")?;
        stats.considered += 1;
        let fresh = store
            .get_vector(&entity_id, &content_hash, model_id)
            .map_err(|err| anyhow::anyhow!("{err}"))
            .with_context(|| format!("check semantic embedding freshness for {entity_id}"))?;
        if fresh.is_some() {
            stats.skipped_fresh += 1;
            continue;
        }
        pending.push(SemanticEmbeddingCandidate {
            entity_id,
            content_hash,
            text: semantic_embedding_text(&short_name, &name, &properties_json),
        });
    }
    Ok(pending)
}

fn semantic_embedding_text(short_name: &str, name: &str, properties_json: &str) -> String {
    if let Ok(properties) = serde_json::from_str::<serde_json::Value>(properties_json)
        && let Some(docstring) = properties
            .get("docstring")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|docstring| !docstring.is_empty())
    {
        return format!("{short_name}\n{docstring}");
    }
    if name == short_name {
        short_name.to_owned()
    } else {
        format!("{short_name}\n{name}")
    }
}

/// Phase 8 (WP9-B, REQ-FINDING-03): POST this run's persisted findings to
/// Filigree's native `POST /api/v1/scan-results` intake.
///
/// Best-effort and enrich-only: gated behind
/// `integrations.filigree.{enabled,emit_findings}`, and any failure (Filigree
/// down, transport error, build error) is recorded in the returned stats blob
/// and logged as `LMWV-INFRA-FILIGREE-UNREACHABLE` rather than propagated — the
/// analyze run never fails because a sibling tool is unreachable. Returns
/// [`serde_json::Value::Null`] when emission is disabled; otherwise a
/// `filigree_emission` stats object folded into `stats.json`.
///
/// Findings written during the run (including the phase-3 weak-modularity fact)
/// are flushed before reading so the emission batch is complete.
///
/// `rule_filter` restricts the batch to a fixed set of rule IDs. The Phase-8
/// (pre-`CommitRun`) call passes `None` and emits everything the run wrote so
/// far. The Phase-8c (post-`CommitRun`) call passes [`POST_RUN_FINDING_RULES`]
/// so the second pass re-sends only the findings persisted after Phase 8, never
/// the during-run findings already emitted; in that filtered mode an empty
/// emittable batch skips the POST entirely (no wasted call when a run deletes
/// nothing). `complete_scan_run` rides into the wire request: `true` for the
/// final/only batch, `false` for an additive follow-up batch.
async fn emit_findings_to_filigree(
    writer: &Writer,
    db_path: &Path,
    project_root: &Path,
    run_id: &str,
    mark_unseen: bool,
    complete_scan_run: bool,
    rule_filter: Option<&[&str]>,
    config_path: Option<&Path>,
) -> serde_json::Value {
    let mcp_config = load_mcp_config(project_root, config_path);
    let filigree_cfg = &mcp_config.integrations.filigree;
    if !filigree_cfg.enabled || !filigree_cfg.emit_findings {
        return serde_json::Value::Null;
    }

    // Make findings durable so a fresh read connection observes them. Only the
    // Phase-8 (pre-`CommitRun`) call has an open run batch to flush; the Phase-8c
    // (post-`CommitRun`, `rule_filter.is_some()`) call runs after the run is
    // already committed and its post-commit findings were each written via
    // `PersistPostRunFinding` (a query-time write that auto-commits), so there is
    // no batch to flush — and `FlushRunBatch` would in fact error
    // (`WriterProtocol: without a preceding BeginRun`) and wrongly skip emission.
    if rule_filter.is_none()
        && let Err(err) = writer
            .send_wait(|ack| WriterCmd::FlushRunBatch { ack })
            .await
    {
        tracing::warn!(run_id, error = %err, "flush before finding emission failed; skipping emission");
        return serde_json::json!({"status": "skipped", "reason": "flush_failed"});
    }

    let rows = match Connection::open(db_path) {
        Ok(conn) => match loomweave_storage::findings_for_emit(&conn, run_id) {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(run_id, error = %err, "read findings for emission failed; skipping emission");
                return serde_json::json!({"status": "skipped", "reason": "read_failed"});
            }
        },
        Err(err) => {
            tracing::warn!(run_id, error = %err, "open read conn for emission failed; skipping emission");
            return serde_json::json!({"status": "skipped", "reason": "read_open_failed"});
        }
    };
    let rows: Vec<_> = match rule_filter {
        Some(allow) => rows
            .into_iter()
            .filter(|r| allow.contains(&r.rule_id.as_str()))
            .collect(),
        None => rows,
    };
    let total_findings = rows.len();
    let rows = rows
        .into_iter()
        .map(federation_finding_for_emit)
        .map(|mut row| {
            // Filigree's scan-results intake requires project-relative finding
            // paths; Loomweave stores absolute `source_file_path`s, so strip the
            // project-root prefix before emit. An out-of-root path is left as-is
            // (Filigree rejects it loudly, which beats silently rewriting it). A
            // path equal to the project root relativizes to "" — that is a
            // project-anchored (file-less) finding, not a real file location, so
            // drop it to `None` (path-less → skipped) rather than emit an empty
            // path Filigree rejects.
            let rel = row
                .source_file_path
                .as_deref()
                .map(|path| relativize_for_emit(project_root, path))
                .filter(|rel| !rel.is_empty());
            // Clear line numbers past EOF before emit. Loomweave can record a
            // line beyond a file's last line for a syntax-error/degraded entity;
            // Filigree's strict batch intake 400s the WHOLE batch on such a
            // finding (its single-finding path merely clears them), so mirror
            // that lenient clamp here.
            if let Some(rel) = rel.as_deref() {
                let (start, end) = clamp_lines_to_file(
                    project_root,
                    rel,
                    row.source_line_start,
                    row.source_line_end,
                );
                row.source_line_start = start;
                row.source_line_end = end;
            }
            row.source_file_path = rel;
            row
        })
        .collect::<Vec<_>>();

    // Path-less findings have no real file to anchor to, and Filigree's
    // scan-results intake rejects every synthetic stand-in: an absolute project
    // root (absolute paths rejected), AND the relative "." — its
    // `_normalize_scan_path` collapses "." to "", rejected as "path is empty
    // after normalization". There is no valid project-level path, so a path-less
    // finding is skipped (`skipped_no_path` — surfaced, never silently dropped)
    // rather than forced under a bogus path that 400s the whole batch. The
    // file-anchored defects still reach Filigree: `lw-duplicate-locator` now
    // carries an `anchor_file_path` (see `duplicate_guard`) so it resolves to a
    // real file instead of the file-less project anchor. Genuinely project-level
    // facts (e.g. the weak-modularity subsystem fact) stay store-only by design.
    let default_path = None;

    let batch = prepare_batch(
        &rows,
        &EmitOptions {
            scan_run_id: Some(run_id.to_owned()),
            mark_unseen,
            complete_scan_run,
            default_path,
        },
    );
    let emitted = batch.emitted;
    let skipped_no_path = batch.skipped_no_path;

    // In filtered (Phase-8c) mode, suppress the POST when nothing emittable
    // remains: a run that deleted nothing has no post-commit findings, and a run
    // whose only post-commit findings are path-less (tier/guidance) has nothing
    // the wire contract accepts. The unfiltered Phase-8 call always posts (even
    // an empty batch) so its `complete_scan_run` signal still rides through.
    if rule_filter.is_some() && emitted == 0 {
        return serde_json::json!({
            "status": "skipped",
            "reason": "no_postrun_findings_with_path",
            "findings_total": total_findings,
            "skipped_no_path": skipped_no_path,
        });
    }

    post_findings_batch(
        filigree_cfg,
        project_root,
        run_id,
        batch,
        total_findings,
        mark_unseen,
    )
    .await
}

/// Strip the project-root prefix so a stored absolute `source_file_path` emits
/// as the project-relative path Filigree's scan-results intake requires. A path
/// that does not live under the root is returned unchanged (Filigree then
/// rejects it loudly, which is preferable to silently rewriting it).
fn relativize_for_emit(project_root: &Path, path: &str) -> String {
    Path::new(path)
        .strip_prefix(project_root)
        .ok()
        .and_then(|rel| rel.to_str())
        .map_or_else(|| path.to_owned(), str::to_owned)
}

/// Clear a finding's line numbers when they fall past end-of-file, mirroring
/// Filigree's lenient single-finding line-attribution (`scanner_reporting`).
/// Loomweave can record a line beyond a file's last line for a
/// syntax-error/degraded entity (the parser reports a position past EOF);
/// Filigree's *strict batch* intake 400s the whole batch on such a finding, so
/// clamp here. `line_start` past EOF clears both (no usable anchor); otherwise a
/// `line_end` past EOF is dropped. Line counting matches Filigree's (`str::lines`
/// counts a final unterminated line, like Python's file iteration). An unreadable
/// file leaves the numbers untouched.
fn clamp_lines_to_file(
    project_root: &Path,
    rel_path: &str,
    line_start: Option<i64>,
    line_end: Option<i64>,
) -> (Option<i64>, Option<i64>) {
    let Ok(content) = fs::read_to_string(project_root.join(rel_path)) else {
        return (line_start, line_end);
    };
    let count = i64::try_from(content.lines().count()).unwrap_or(i64::MAX);
    if line_start.is_some_and(|start| start > count) {
        return (None, None);
    }
    (line_start, line_end.filter(|&end| end <= count))
}

fn federation_finding_for_emit(row: loomweave_storage::FindingForEmitRow) -> FindingForEmit {
    FindingForEmit {
        id: row.id,
        rule_id: row.rule_id,
        kind: row.kind,
        severity: row.severity,
        confidence: row.confidence,
        confidence_basis: row.confidence_basis,
        message: row.message,
        entity_id: row.entity_id,
        related_entities_json: row.related_entities_json,
        supports_json: row.supports_json,
        supported_by_json: row.supported_by_json,
        source_file_path: row.source_file_path,
        source_line_start: row.source_line_start,
        source_line_end: row.source_line_end,
    }
}

/// POST a prepared scan-results batch to the live Filigree endpoint and render
/// the `filigree_emission` stats blob. Split out of [`emit_findings_to_filigree`]
/// so the Phase-8 read/filter logic and this network lifecycle stay independently
/// readable. Best-effort: a build/transport failure becomes an
/// `LMWV-INFRA-FILIGREE-UNREACHABLE` stats blob via [`unreachable_stats`].
async fn post_findings_batch(
    filigree_cfg: &FiligreeConfig,
    project_root: &Path,
    run_id: &str,
    batch: PreparedBatch,
    total_findings: usize,
    mark_unseen: bool,
) -> serde_json::Value {
    let emitted = batch.emitted;
    let skipped_no_path = batch.skipped_no_path;

    // Resolve the live Filigree URL (ephemeral port over stale config), the same
    // resolution `loomweave serve` and `project_status` use.
    let resolution =
        resolve_filigree_url(filigree_cfg, project_root, |name| std::env::var(name).ok());
    let mut resolved_cfg = filigree_cfg.clone();
    if let Some(url) = resolution.resolved_url {
        resolved_cfg.base_url = url;
    }
    let endpoint = scan_results_url(&resolved_cfg.base_url, resolved_cfg.project.as_deref());

    // `reqwest::blocking` builds and drops its own inner tokio runtime; doing
    // that on a tokio worker — even inside `spawn_blocking`, which still carries
    // an ambient runtime handle — panics on drop. Run the whole client
    // lifecycle (build → POST → drop) on a plain OS thread with no ambient
    // runtime, and join it off the async executor.
    let request = batch.request;
    let thread_cfg = resolved_cfg;
    let thread_root = project_root.to_path_buf();
    let worker = std::thread::spawn(move || -> Result<ScanResultsResponse, String> {
        let client = FiligreeHttpClient::from_config_with_project_root(
            &thread_cfg,
            |name| std::env::var(name).ok(),
            Some(&thread_root),
        )
        .map_err(|err| format!("build Filigree client: {err}"))?
        .ok_or_else(|| "Filigree integration disabled".to_owned())?;
        client
            .post_scan_results(&request)
            .map_err(|err| err.to_string())
    });
    let joined = tokio::task::spawn_blocking(move || worker.join()).await;

    match joined {
        Ok(Ok(Ok(response))) => {
            for warning in &response.warnings {
                tracing::warn!(run_id, warning = %warning, "Filigree scan-results intake warning");
            }
            tracing::info!(
                run_id,
                endpoint = %endpoint,
                emitted,
                skipped_no_path,
                created = response.findings_created,
                updated = response.findings_updated,
                warnings = response.warnings.len(),
                "posted findings to Filigree",
            );
            serde_json::json!({
                "status": "emitted",
                "endpoint": endpoint,
                "findings_total": total_findings,
                "emitted": emitted,
                "skipped_no_path": skipped_no_path,
                "mark_unseen": mark_unseen,
                "findings_created": response.findings_created,
                "findings_updated": response.findings_updated,
                "warnings": response.warnings,
            })
        }
        Ok(Ok(Err(err))) => unreachable_stats(
            run_id,
            &endpoint,
            total_findings,
            emitted,
            skipped_no_path,
            &err,
        ),
        Ok(Err(_panic)) => unreachable_stats(
            run_id,
            &endpoint,
            total_findings,
            emitted,
            skipped_no_path,
            "emission thread panicked",
        ),
        Err(err) => unreachable_stats(
            run_id,
            &endpoint,
            total_findings,
            emitted,
            skipped_no_path,
            &format!("emission task: {err}"),
        ),
    }
}

/// Build the `filigree_emission` stats blob for a failed POST and log it as
/// `LMWV-INFRA-FILIGREE-UNREACHABLE`. The infra finding is recorded in
/// `stats.json` and the log (two of the three surfaces REQ-ANALYZE-06 names);
/// the local `findings` table is not used because its `entity_id` is a
/// non-null FK to `entities` and an infra finding has no anchor entity — the
/// same reason every other `LMWV-INFRA-*` finding is log-only today.
fn unreachable_stats(
    run_id: &str,
    endpoint: &str,
    total_findings: usize,
    emitted: usize,
    skipped_no_path: usize,
    error: &str,
) -> serde_json::Value {
    tracing::warn!(
        run_id,
        endpoint,
        rule_id = "LMWV-INFRA-FILIGREE-UNREACHABLE",
        error,
        "could not post findings to Filigree; continuing (enrich-only)",
    );
    serde_json::json!({
        "status": "unreachable",
        "rule_id": "LMWV-INFRA-FILIGREE-UNREACHABLE",
        "endpoint": endpoint,
        "findings_total": total_findings,
        "emitted_attempted": emitted,
        "skipped_no_path": skipped_no_path,
        "error": error,
    })
}

/// `--prune-unseen` retention sweep (WP9-B, REQ-FINDING-06): asks Filigree to
/// soft-archive its own `unseen_in_latest` Loomweave findings older than the
/// configured age. Returns [`serde_json::Value::Null`] when not requested;
/// otherwise a `filigree_prune` stats object folded into `stats.json`. Like
/// emission, this is enrich-only — a disabled integration or a Filigree outage
/// is recorded in stats, never fails the run. `scan_source` scoping is enforced
/// by Filigree, so the sweep can only touch Loomweave's findings.
async fn prune_unseen_findings_in_filigree(
    project_root: &Path,
    run_id: &str,
    prune_unseen: bool,
    config_path: Option<&Path>,
) -> serde_json::Value {
    if !prune_unseen {
        return serde_json::Value::Null;
    }
    let mcp_config = load_mcp_config(project_root, config_path);
    let filigree_cfg = &mcp_config.integrations.filigree;
    if !filigree_cfg.enabled {
        tracing::info!(
            run_id,
            "--prune-unseen requested but Filigree integration disabled; skipping"
        );
        return serde_json::json!({"status": "skipped", "reason": "filigree_disabled"});
    }
    let older_than_days = filigree_cfg.prune_unseen_days;

    // Resolve the live Filigree URL (ephemeral port over stale config), the
    // same resolution emission uses.
    let resolution =
        resolve_filigree_url(filigree_cfg, project_root, |name| std::env::var(name).ok());
    let mut resolved_cfg = filigree_cfg.clone();
    if let Some(url) = resolution.resolved_url {
        resolved_cfg.base_url = url;
    }
    let endpoint = clean_stale_url(&resolved_cfg.base_url);
    let request = CleanStaleRequest {
        scan_source: LOOMWEAVE_SCAN_SOURCE.to_owned(),
        older_than_days,
        actor: resolved_cfg.actor.clone(),
    };

    // Same blocking-reqwest-on-a-plain-OS-thread dance as emission: build → POST
    // → drop the client off the tokio executor so the inner runtime drop is safe.
    let thread_cfg = resolved_cfg;
    let thread_root = project_root.to_path_buf();
    let worker = std::thread::spawn(move || -> Result<CleanStaleResponse, String> {
        let client = FiligreeHttpClient::from_config_with_project_root(
            &thread_cfg,
            |name| std::env::var(name).ok(),
            Some(&thread_root),
        )
        .map_err(|err| format!("build Filigree client: {err}"))?
        .ok_or_else(|| "Filigree integration disabled".to_owned())?;
        client
            .post_clean_stale(&request)
            .map_err(|err| err.to_string())
    });
    let joined = tokio::task::spawn_blocking(move || worker.join()).await;

    match joined {
        Ok(Ok(Ok(response))) => {
            tracing::info!(
                run_id,
                endpoint = %endpoint,
                findings_fixed = response.findings_fixed,
                older_than_days,
                "pruned unseen findings in Filigree",
            );
            serde_json::json!({
                "status": "pruned",
                "endpoint": endpoint,
                "findings_fixed": response.findings_fixed,
                "older_than_days": older_than_days,
            })
        }
        Ok(Ok(Err(err))) => prune_unreachable_stats(run_id, &endpoint, older_than_days, &err),
        Ok(Err(_panic)) => {
            prune_unreachable_stats(run_id, &endpoint, older_than_days, "prune thread panicked")
        }
        Err(err) => prune_unreachable_stats(
            run_id,
            &endpoint,
            older_than_days,
            &format!("prune task: {err}"),
        ),
    }
}

/// Build the `filigree_prune` stats blob for a failed sweep and log it as
/// `LMWV-INFRA-FILIGREE-UNREACHABLE` — the enrich-only degrade, identical in
/// spirit to [`unreachable_stats`] for emission.
fn prune_unreachable_stats(
    run_id: &str,
    endpoint: &str,
    older_than_days: u32,
    error: &str,
) -> serde_json::Value {
    tracing::warn!(
        run_id,
        endpoint,
        rule_id = "LMWV-INFRA-FILIGREE-UNREACHABLE",
        error,
        "could not prune unseen findings in Filigree; continuing (enrich-only)",
    );
    serde_json::json!({
        "status": "unreachable",
        "rule_id": "LMWV-INFRA-FILIGREE-UNREACHABLE",
        "endpoint": endpoint,
        "older_than_days": older_than_days,
        "error": error,
    })
}

fn module_entity_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT id FROM entities WHERE kind = 'module' ORDER BY id")
        .context("prepare module entity query")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .context("query module entities")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("collect module entities")
}

#[allow(clippy::too_many_arguments)]
fn phase3_stats_json(
    config: &ClusteringConfig,
    algorithm: ClusterAlgorithm,
    status: &str,
    skipped_reason: Option<&str>,
    module_count: usize,
    module_edge_count: usize,
    subsystem_count: usize,
    modularity_score: Option<f64>,
    subsystems_inserted: u64,
    in_subsystem_edges_inserted: u64,
    weak_modularity_finding_emitted: bool,
    started: std::time::Instant,
) -> serde_json::Value {
    serde_json::json!({
        "enabled": config.enabled,
        "algorithm": algorithm.as_str(),
        "configured_algorithm": config.algorithm.as_str(),
        "status": status,
        "seed": config.seed,
        "resolution": config.resolution,
        "max_iterations": config.max_iterations,
        "min_cluster_size": config.min_cluster_size,
        "edge_types": config.edge_types.iter().map(|edge_type| edge_type.as_str()).collect::<Vec<_>>(),
        "weight_by": config.weight_by.as_str(),
        "module_count": module_count,
        "module_edge_count": module_edge_count,
        "subsystem_count": subsystem_count,
        "modularity_score": modularity_score,
        "duration_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "subsystems_inserted": subsystems_inserted,
        "in_subsystem_edges_inserted": in_subsystem_edges_inserted,
        "weak_modularity_threshold": config.weak_modularity_threshold,
        "weak_modularity_finding_emitted": weak_modularity_finding_emitted,
        "skipped_reason": skipped_reason,
    })
}

// ── Run-outcome ───────────────────────────────────────────────────────────────
//
// Three terminal states because plugin crashes and writer-actor failures need
// different SQL paths:
//
// - `Completed`: all plugins ran cleanly → `CommitRun(Completed)`.
// - `SoftFailed`: one or more plugins crashed, but other plugins produced
//   entities that should persist → `CommitRun(Failed)`. The writer folds
//   `UPDATE runs ... status='failed'` into the open entity transaction so
//   the batch commits and the run row marks failed atomically. Exit 1.
// - `HardFailed`: the writer rejected a mutation or the Phase 3 pre-flush
//   validation rejected the pending graph (DB locked, disk full,
//   parent/contains mismatch, etc.) → `FailRun`. The writer rolls back the
//   still-open transaction before the run row is marked failed. Exit 1.
//   Continuing past this makes no sense — the DB is unusable or inconsistent.

#[derive(Debug)]
enum RunOutcome {
    Completed,
    SoftFailed { reason: String },
    HardFailed { reason: String },
}

fn log_plugin_findings(plugin_id: &str, findings: &[HostFinding]) {
    if findings.is_empty() {
        return;
    }
    tracing::warn!(
        plugin_id = %plugin_id,
        finding_count = findings.len(),
        "plugin host collected findings"
    );
    for f in findings {
        tracing::warn!(
            plugin_id = %plugin_id,
            subcode = %f.subcode,
            message = %f.message,
            metadata = ?f.metadata,
            "plugin host finding",
        );
    }
}

// ── JoinError handling ────────────────────────────────────────────────────────

/// Convert a `spawn_blocking` join result into the plugin-crash-shaped
/// `Result<BatchResult, PluginRunError>` the caller already knows how to handle.
///
/// The `Err(JoinError)` arm is the load-bearing one: a panic inside
/// `run_plugin_blocking` would otherwise `?`-propagate past the run-outcome
/// machinery and leave `runs.status = 'running'` forever. By normalising the
/// panic into a crash reason string, it feeds the existing crash-recording
/// path (ticks the crash-loop breaker, resolves to `SoftFailed` if no writer
/// error occurred).
fn handle_plugin_task_join_result(
    result: Result<Result<BatchResult, PluginRunError>, tokio::task::JoinError>,
    plugin_id: &str,
) -> Result<BatchResult, PluginRunError> {
    match result {
        Ok(inner) => inner,
        Err(join_err) => {
            tracing::error!(
                plugin_id = %plugin_id,
                error = %join_err,
                "plugin task panicked; recording as crash",
            );
            Err(PluginRunError::new(format!(
                "plugin task for {plugin_id} panicked: {join_err}"
            )))
        }
    }
}

// ── Blocking plugin worker ────────────────────────────────────────────────────

/// Returned from the blocking plugin task on success.
struct BatchResult {
    /// Findings accumulated by the host during the session.
    findings: Vec<loomweave_core::HostFinding>,
}

#[allow(clippy::large_enum_variant)]
enum PluginBatchMessage {
    File(PluginFileBatch),
    DeferredImportEdges {
        edges: Vec<(String, EdgeRecord)>,
        imports_skipped_external: u64,
    },
}

struct PluginFileBatch {
    /// Core file entity id for the analyzed file. Used as the authoritative
    /// replacement key for scan-time anchored edges from that source file.
    source_file_id: String,
    /// `(entity_id_string, record)` pairs accepted from one analyzed file.
    entities: Vec<(String, EntityRecord)>,
    /// Manifest-declared semantic roles for this plugin's entity kinds.
    kind_roles: PluginKindRoles,
    /// Non-import edges accepted from one analyzed file. Import edges are
    /// deferred because local-vs-external classification needs the plugin's
    /// complete module set.
    edges: Vec<(String, EdgeRecord)>,
    /// Per-caller unresolved site replacements derived from authoritative
    /// plugin stats for this file.
    unresolved_call_sites: Vec<PendingUnresolvedCallSites>,
    /// Observability stats reported by the plugin for this file.
    stats: BatchStats,
    /// `locator -> canonical SEI signature JSON` for entities the plugin
    /// declared a signature for (WS1 / ADR-038). The SEI mint pass reads it as
    /// the move-case matcher input and persists it to `entities.signature`.
    signatures: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
struct PluginKindRoles {
    file_scope: BTreeSet<String>,
    callable: BTreeSet<String>,
    syntax_degraded_module: BTreeSet<String>,
}

impl PluginKindRoles {
    fn from_manifest(manifest: &loomweave_core::Manifest) -> Self {
        let mut roles = Self::default();
        for kind in &manifest.ontology.entity_kinds {
            if manifest
                .ontology
                .kind_has_role(kind, loomweave_core::OntologyEntityRole::FileScope)
            {
                roles.file_scope.insert(kind.clone());
            }
            if manifest
                .ontology
                .kind_has_role(kind, loomweave_core::OntologyEntityRole::Callable)
            {
                roles.callable.insert(kind.clone());
            }
            if manifest.ontology.kind_has_role(
                kind,
                loomweave_core::OntologyEntityRole::SyntaxDegradedModule,
            ) {
                roles.syntax_degraded_module.insert(kind.clone());
            }
        }
        roles
    }

    fn is_file_scope(&self, kind: &str) -> bool {
        self.file_scope.contains(kind)
    }

    fn is_callable(&self, kind: &str) -> bool {
        self.callable.contains(kind)
    }

    fn is_syntax_degraded_module(&self, kind: &str) -> bool {
        self.syntax_degraded_module.contains(kind)
    }
}

struct PersistedPluginBatch {
    entity_count: u64,
    prior_index_entries: Vec<PriorIndexEntry>,
    sei_descriptors: Vec<NewEntityDescriptor>,
    failure_findings: Vec<FindingRecord>,
}

#[derive(Debug)]
struct PluginRunError {
    reason: String,
    findings: Vec<HostFinding>,
}

impl PluginRunError {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            findings: Vec::new(),
        }
    }

    fn with_findings(reason: String, findings: Vec<HostFinding>) -> Self {
        Self { reason, findings }
    }
}

async fn persist_plugin_file_batch(
    writer: &Writer,
    batch: PluginFileBatch,
    run_id: &str,
    started_at: &str,
    head_commit: Option<&str>,
) -> Result<PersistedPluginBatch> {
    let entity_count = batch.entities.len() as u64;
    let mut prior_index_entries = Vec::new();
    let mut sei_descriptors = Vec::new();
    let mut failure_findings = Vec::new();

    for (id_str, mut record) in batch.entities {
        // Capture the prior-index row and the SEI descriptor BEFORE `record`
        // is moved into the command. `signature` (WS1) is the
        // plugin-declared matcher input, now carried into both the
        // prior-index snapshot and the SEI descriptor list.
        let signature = batch.signatures.get(&id_str).cloned();
        let prior_entry = record
            .content_hash
            .clone()
            .map(|body_hash| PriorIndexEntry {
                locator: record.id.clone(),
                body_hash,
                signature: signature.clone(),
            });
        // Every accepted entity gets a descriptor (even ones with no body
        // hash — they still carry/mint an SEI on the locator-unchanged path;
        // only the move case needs a body).
        let descriptor = NewEntityDescriptor {
            locator: record.id.clone(),
            body_hash: record.content_hash.clone(),
            signature,
        };
        // REQ-ANALYZE-06: capture a parse-failure finding from the degraded
        // entity BEFORE `record` is moved into the command. Anchors to this
        // same entity (inserted just below), so the finding's FK resolves.
        if let Some(finding) = syntax_error_finding(&record, &batch.kind_roles, run_id, started_at)
        {
            failure_findings.push(finding);
        }
        stamp_entity_git_provenance(&mut record, head_commit);
        writer
            .send_wait(|ack| WriterCmd::InsertEntity {
                entity: Box::new(record),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("InsertEntity for {id_str}"))?;
        // Recorded only after a successful insert so neither the snapshot nor
        // the SEI pass claims an entity the durable graph lacks.
        if let Some(prior_entry) = prior_entry {
            prior_index_entries.push(prior_entry);
        }
        sei_descriptors.push(descriptor);
    }

    writer
        .send_wait(|ack| WriterCmd::ReplaceAnchoredEdgesForSourceFile {
            source_file_id: batch.source_file_id.clone(),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| {
            format!(
                "ReplaceAnchoredEdgesForSourceFile for {}",
                batch.source_file_id
            )
        })?;

    for pending in batch.unresolved_call_sites {
        let caller_id = pending.caller_entity_id.clone();
        writer
            .send_wait(|ack| WriterCmd::ReplaceUnresolvedCallSitesForCaller {
                caller_entity_id: pending.caller_entity_id,
                caller_content_hash: pending.caller_content_hash,
                sites: pending.sites,
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("ReplaceUnresolvedCallSitesForCaller for {caller_id}"))?;
    }

    Ok(PersistedPluginBatch {
        entity_count,
        prior_index_entries,
        sei_descriptors,
        failure_findings,
    })
}

fn stamp_entity_git_provenance(record: &mut EntityRecord, head_commit: Option<&str>) {
    if let Some(commit) = head_commit {
        record.first_seen_commit = Some(commit.to_owned());
        record.last_seen_commit = Some(commit.to_owned());
    }
}

async fn persist_plugin_edges(writer: &Writer, edges: Vec<(String, EdgeRecord)>) -> Result<u64> {
    let edge_count = edges.len() as u64;
    for (descr, record) in edges {
        writer
            .send_wait(|ack| WriterCmd::InsertEdge {
                edge: Box::new(record),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("InsertEdge {descr}"))?;
    }
    Ok(edge_count)
}

fn drain_ready_plugin_edges(
    pending_edges: &mut Vec<DescribedEdgeRecord>,
    seen_entity_ids: &BTreeSet<String>,
) -> Vec<DescribedEdgeRecord> {
    let mut ready = Vec::new();
    let mut waiting = Vec::new();
    for (descr, edge) in pending_edges.drain(..) {
        if seen_entity_ids.contains(&edge.from_id) && seen_entity_ids.contains(&edge.to_id) {
            ready.push((descr, edge));
        } else {
            waiting.push((descr, edge));
        }
    }
    *pending_edges = waiting;
    ready
}

/// End-of-plugin reconciliation of the pending anchored-edge buffer: every edge
/// still pending after the last [`drain_ready_plugin_edges`] call has a `from_id`
/// or `to_id` the host never stored this run (external target, gitignored-file
/// superset, or mid-run staleness — D1/D2/D3). Such an edge can NEVER FK-resolve:
/// `edges.to_id`/`from_id` are `NOT NULL REFERENCES entities(id)` with FKs on, so
/// an `INSERT` of it hard-fails the whole run. Drop the whole buffer and return
/// the count (drop-AND-count — never silently lose; never downgrade to Inferred,
/// which the writer rejects on anchored edges). Behaviour-preserving for the
/// Python plugin: its surviving `imports` targets are plugin-emitted file-scope
/// modules in `seen_plugin_entity_ids`, so they drain ready and the buffer is
/// empty here (a no-op).
fn drop_unready_plugin_edges(pending_edges: &mut Vec<DescribedEdgeRecord>) -> u64 {
    let dropped = u64::try_from(pending_edges.len()).unwrap_or(u64::MAX);
    pending_edges.clear();
    dropped
}

#[derive(Debug, Default)]
struct BatchStats {
    unresolved_call_sites_total: u64,
    reference_sites_total: u64,
    references_resolved_total: u64,
    references_skipped_external_total: u64,
    references_skipped_cap_total: u64,
    imports_skipped_external_total: u64,
    unresolved_reference_sites_total: u64,
    pyright_query_latency_ms: Vec<u64>,
    pyright_index_parse_latency_ms: Vec<u64>,
    extractor_parse_latency_ms: Vec<u64>,
}

#[derive(Debug, Clone)]
struct PendingUnresolvedCallSites {
    caller_entity_id: String,
    caller_content_hash: String,
    sites: Vec<UnresolvedCallSiteRecord>,
}

/// Plugin lifecycle phase a watchdog deadline guards.
///
/// `Handshake` covers the blocking `initialize` exchange (a plugin may do
/// whole-repo work there — the Rust plugin builds its symbol table inside
/// `initialize`); `File` covers each `analyze_file`; `Shutdown` covers the
/// best-effort `shutdown`/`exit` exchange after the file loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WatchdogPhase {
    Handshake,
    File,
    Shutdown,
}

/// Plugin lifecycle-timeout watchdog (REQ-ANALYZE-06, `LMWV-PY-TIMEOUT`).
///
/// Every plugin exchange blocks on a synchronous read of the plugin's stdout,
/// which has no read deadline. The watchdog runs on its own thread holding a
/// shared handle to the child process (the reader lives in the *host*, a
/// separate value, so killing the child unblocks the read without touching the
/// host). The main thread `arm`s before each blocking exchange — naming the
/// lifecycle [`WatchdogPhase`] being guarded — and `disarm`s after; if the
/// deadline passes while armed, the watchdog kills the child and records which
/// phase timed out.
struct PluginWatchdog {
    /// Active deadline plus the phase it guards, or `None` when disarmed.
    /// Guarded so `disarm` and the watchdog's fire-check observe a consistent
    /// value (no kill-after-disarm).
    deadline: std::sync::Mutex<Option<(std::time::Instant, WatchdogPhase)>>,
    /// First phase whose deadline expired, or `None` if no timeout fired.
    /// First-wins: the first expiry is the one that broke the run.
    timed_out: std::sync::Mutex<Option<WatchdogPhase>>,
    stop: std::sync::atomic::AtomicBool,
}

impl PluginWatchdog {
    fn new() -> Self {
        Self {
            deadline: std::sync::Mutex::new(None),
            timed_out: std::sync::Mutex::new(None),
            stop: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn arm(&self, timeout: std::time::Duration, phase: WatchdogPhase) {
        *self.deadline.lock().expect("watchdog deadline poisoned") =
            Some((std::time::Instant::now() + timeout, phase));
    }

    fn disarm(&self) {
        *self.deadline.lock().expect("watchdog deadline poisoned") = None;
    }

    /// Phase whose deadline expired first, or `None` if no timeout fired.
    fn timed_out_phase(&self) -> Option<WatchdogPhase> {
        *self.timed_out.lock().expect("watchdog timed_out poisoned")
    }

    /// Fire-check: if the armed deadline has passed, disarm (kill at most
    /// once per arm), record the phase (first expiry wins), and return the
    /// phase so the watchdog thread kills the child.
    fn expire_if_due(&self) -> Option<WatchdogPhase> {
        let expired = {
            let mut guard = self.deadline.lock().expect("watchdog deadline poisoned");
            match *guard {
                Some((deadline, phase)) if std::time::Instant::now() >= deadline => {
                    *guard = None; // disarm so we kill at most once
                    Some(phase)
                }
                _ => None,
            }
        };
        if let Some(phase) = expired {
            let mut recorded = self.timed_out.lock().expect("watchdog timed_out poisoned");
            if recorded.is_none() {
                *recorded = Some(phase);
            }
        }
        expired
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Spawn the watchdog thread. It polls the shared deadline; on expiry it
/// records the timed-out phase, clears the deadline (kill at most once), and
/// kills the child. Returns the join handle so the caller can stop + join
/// before reaping.
fn spawn_plugin_watchdog(
    watchdog: Arc<PluginWatchdog>,
    child: Arc<std::sync::Mutex<std::process::Child>>,
    plugin_id: String,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !watchdog.stop.load(Ordering::SeqCst) {
            std::thread::sleep(PLUGIN_WATCHDOG_POLL_INTERVAL);
            if let Some(phase) = watchdog.expire_if_due() {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    phase = ?phase,
                    "plugin exceeded lifecycle deadline; killing child",
                );
                if let Ok(mut c) = child.lock() {
                    let _ = c.kill();
                }
            }
        }
    })
}

/// Spawn the plugin, handshake, run `analyze_file` for each file, collect results.
///
/// All I/O is synchronous — this is designed to run inside `spawn_blocking`.
/// On unrecoverable error, returns `Err(reason_string)`.
///
/// Regardless of success or failure the child process is always reaped: on
/// the happy path via `host.shutdown()` + `child.wait()`, on the error path
/// via `child.kill()` + `child.wait()`. `std::process::Child::Drop` does NOT
/// kill or reap on Unix, so discarding `child` without `wait()` would leak a
/// zombie into the kernel process table per spawn.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn run_plugin_blocking(
    manifest: loomweave_core::Manifest,
    project_root: &Path,
    plugin_id: &str,
    executable: &Path,
    files: &[PathBuf],
    briefing_blocks: &Arc<BTreeMap<PathBuf, loomweave_core::BriefingBlockReason>>,
    scanned_source_files: &Arc<BTreeSet<PathBuf>>,
    progress: &ProgressReporter,
    handshake_timeout: std::time::Duration,
    file_timeout: std::time::Duration,
    shutdown_timeout: std::time::Duration,
    prior_file_scope_claims: BTreeSet<String>,
    skipped_locator_owners: BTreeMap<String, String>,
    batch_tx: &tokio::sync::mpsc::Sender<PluginBatchMessage>,
) -> Result<BatchResult, PluginRunError> {
    use loomweave_core::PluginHost;

    let manifest_language = manifest.plugin.language.clone();
    let kind_roles = PluginKindRoles::from_manifest(&manifest);
    let (mut host, child) = PluginHost::spawn_unhandshaken(manifest, project_root, executable)
        .map_err(|e| match e {
            HostError::Spawn(msg) => {
                PluginRunError::new(format!("failed to spawn plugin {plugin_id}: {msg}"))
            }
            other => PluginRunError::new(format!("plugin {plugin_id} spawn error: {other}")),
        })?;
    host.set_briefing_blocks(Arc::clone(briefing_blocks));
    host.set_scanned_source_files(Arc::clone(scanned_source_files));

    // Lifecycle-timeout watchdog (REQ-ANALYZE-06, ADR-050). Shares the child
    // handle so it can kill a hung plugin and unblock the synchronous read.
    // Started BEFORE the handshake: a plugin may do whole-repo work inside
    // `initialize` (the Rust plugin builds its symbol table there), and a
    // hung handshake would otherwise wedge the run record and the analyze
    // advisory lock forever.
    let child = Arc::new(std::sync::Mutex::new(child));
    let watchdog = Arc::new(PluginWatchdog::new());
    let watchdog_handle = spawn_plugin_watchdog(
        Arc::clone(&watchdog),
        Arc::clone(&child),
        plugin_id.to_owned(),
    );

    // Handshake under its own wall-clock deadline. `spawn_unhandshaken`'s
    // contract: the caller owns kill+reap on handshake failure (Child::Drop
    // does not reap on Unix) — preserved here CLI-side via
    // `reap_and_classify_exit`.
    watchdog.arm(handshake_timeout, WatchdogPhase::Handshake);
    let handshake_result = host.handshake();
    watchdog.disarm();
    if let Err(handshake_err) = handshake_result {
        watchdog.request_stop();
        let _ = watchdog_handle.join();
        let handshake_timed_out = watchdog.timed_out_phase() == Some(WatchdogPhase::Handshake);
        let mut child = Arc::try_unwrap(child)
            .unwrap_or_else(|_| unreachable!("watchdog joined; no other Arc holders remain"))
            .into_inner()
            .expect("child mutex poisoned");
        // clarion-371efa3e07: check whether the child had ALREADY exited
        // before we kill it. A child that died on its own mid-handshake
        // (e.g. a real RLIMIT_AS OOM kill during `initialize`) must stay
        // honestly classified; a child that is still alive — a protocol
        // refusal, for instance — is about to receive OUR SIGKILL, which
        // must not be misreported as an OOM event (the handshake-failure
        // reason already tells the operator story).
        let child_already_exited = matches!(child.try_wait(), Ok(Some(_)));
        let _ = child.kill();
        let mut findings = host.take_findings();
        drop(host);
        let reason = if handshake_timed_out {
            let reason = format!(
                "plugin {plugin_id} exceeded the handshake timeout ({} ms) and was killed",
                handshake_timeout.as_millis()
            );
            findings.push(plugin_timeout_finding(
                plugin_id,
                "handshake",
                handshake_timeout,
                reason.clone(),
            ));
            reason
        } else {
            match handshake_err {
                HostError::Handshake(ref me) => {
                    format!("plugin {plugin_id} refused handshake: {me}")
                }
                other => format!("plugin {plugin_id} spawn/handshake error: {other}"),
            }
        };
        // Suppress kill-classification when the watchdog fired OR when the
        // child was still alive and the kill above is the host's own
        // (clarion-371efa3e07).
        reap_and_classify_exit(
            &mut child,
            plugin_id,
            &mut findings,
            handshake_timed_out || !child_already_exited,
        );
        return Err(PluginRunError::with_findings(reason, findings));
    }

    let mut dispatch_findings: Vec<HostFinding> = Vec::new();
    let work_result: Result<(), String> = (|| {
        let mut file_scope_entity_ids: BTreeSet<String> = BTreeSet::new();
        // Dual-declaration claims (clarion-6ec7317628): a file_scope (module)
        // entity id may be emitted by MORE THAN ONE file — e.g. an inline
        // `mod sub { ... }` facade in one file colliding with the path-derived
        // module of `sub/mod.rs`. ADR-026's dual encoding allows exactly ONE
        // `file -> module` contains parent per entity, so the FIRST emitter
        // claims it (record + parent_id + contains edge); later emitters are
        // suppressed entirely, keeping the entity anchored at — and parented
        // to — its claim owner. Pre-seeded with module ids owned by this
        // plugin's skipped-unchanged files, whose stored claim survives this
        // run untouched.
        let mut file_scope_claims: BTreeSet<String> = prior_file_scope_claims;
        // Duplicate-locator alarm (clarion-b19fe90c3e): tracks id → first-seen
        // source path across ALL of this plugin's emissions this run, plus the
        // skipped-unchanged owners for the cross-run rule. Detection only —
        // the writer's ON CONFLICT upsert still applies and the run outcome
        // is unchanged.
        let mut duplicate_guard = DuplicateLocatorGuard::new(skipped_locator_owners);
        // Every stored in-project entity id (any kind), accumulated across all
        // files so the deferred import-filter can retain edges whose target is a
        // non-module item — function / struct / const / trait (clarion-d1e3dc67dc).
        let mut all_entity_ids: BTreeSet<String> = BTreeSet::new();
        let mut deferred_import_edges: Vec<(String, EdgeRecord)> = Vec::new();
        for file in files {
            let file_display = file.to_string_lossy().into_owned();
            progress.file_started(plugin_id, &file_display);
            let heartbeat_guard = progress.file_heartbeat_guard(plugin_id.to_owned(), file_display);
            let dispatch_file = match verified_plugin_dispatch_path(project_root, file) {
                Ok(path) => path,
                Err(e) => {
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        file = %file.display(),
                        error = %e,
                        "source file skipped before plugin dispatch; jail-safe open failed",
                    );
                    dispatch_findings.push(jail_open_failed_finding(file, &e));
                    drop(heartbeat_guard);
                    progress.file_completed();
                    continue;
                }
            };
            watchdog.arm(file_timeout, WatchdogPhase::File);
            let analyze_outcome = host.analyze_file(&dispatch_file);
            watchdog.disarm();
            drop(heartbeat_guard);
            let AnalyzeFileOutcome {
                entities,
                edges,
                stats,
            } = analyze_outcome.map_err(|e| classify_host_error(plugin_id, e))?;
            progress.file_completed();
            let mut file_stats = BatchStats {
                unresolved_call_sites_total: stats.unresolved_call_sites_total,
                reference_sites_total: stats.reference_sites_total,
                references_resolved_total: stats.references_resolved_total,
                references_skipped_external_total: stats.references_skipped_external_total,
                references_skipped_cap_total: stats.references_skipped_cap_total,
                imports_skipped_external_total: 0,
                unresolved_reference_sites_total: stats.unresolved_reference_sites_total,
                pyright_query_latency_ms: stats.pyright_query_latency_ms.clone(),
                pyright_index_parse_latency_ms: stats.pyright_index_parse_latency_ms.clone(),
                extractor_parse_latency_ms: Vec::new(),
            };
            if stats.extractor_parse_latency_ms > 0 {
                file_stats
                    .extractor_parse_latency_ms
                    .push(stats.extractor_parse_latency_ms);
            }
            let mut file_entities: Vec<(String, EntityRecord)> = Vec::new();
            let mut file_edges: Vec<(String, EdgeRecord)> = Vec::new();
            let mut file_signatures: BTreeMap<String, String> = BTreeMap::new();
            let (file_entity_id, file_record) = core_file_entity_record(
                project_root,
                &dispatch_file,
                &manifest_language,
                briefing_blocks,
                scanned_source_files,
            )
            .map_err(|e| format!("core file entity for {}: {e:#}", dispatch_file.display()))?;
            file_entities.push((file_entity_id.clone(), file_record));
            for entity in &entities {
                let id_str = entity.id.to_string();
                // First-claim-wins for file_scope entities: a duplicate
                // emission of an already-claimed module id (dual declaration,
                // clarion-6ec7317628) is dropped wholesale — no record, no
                // signature, no second `file -> module` contains edge — so the
                // claim owner's `parent_id` keeps agreeing with the single
                // stored contains edge (ADR-026). The id is already in the
                // claim owner's batch (or survives from a skipped file), so
                // edges referencing it still drain through the seen-set gate.
                if kind_roles.is_file_scope(&entity.kind)
                    && !file_scope_claims.insert(id_str.clone())
                {
                    // Carve-out boundary (clarion-b19fe90c3e): a cross-file
                    // dual claim is the reconciled, legitimate shape and stays
                    // silent; a SAME-file re-emission is a plugin bug and is
                    // flagged as a duplicate locator.
                    if let Some(finding) = duplicate_guard
                        .record_suppressed_file_scope(&id_str, &entity.source_file_path)
                    {
                        dispatch_findings.push(finding);
                    }
                    tracing::debug!(
                        plugin_id = %plugin_id,
                        entity_id = %id_str,
                        file = %dispatch_file.display(),
                        "suppressing duplicate file_scope emission; first claim wins",
                    );
                    continue;
                }
                if let Some(finding) = duplicate_guard.record(
                    &id_str,
                    &entity.source_file_path,
                    kind_roles.is_file_scope(&entity.kind),
                ) {
                    dispatch_findings.push(finding);
                }
                // Capture the plugin-declared SEI signature (ADR-038 REQ-C-01),
                // canonicalised for stable string-equality comparison. The core
                // never interprets the JSON — it only re-serialises the value.
                if let Some(sig) = &entity.raw.signature {
                    file_signatures.insert(id_str.clone(), canonical_signature(sig));
                }
                let mut record = map_entity_to_record(
                    project_root,
                    entity,
                    plugin_id,
                    Some(file_entity_id.clone()),
                    &kind_roles,
                );
                if kind_roles.is_file_scope(&entity.kind) {
                    file_scope_entity_ids.insert(id_str.clone());
                    record.parent_id = Some(file_entity_id.clone());
                    file_edges.push((
                        format!(
                            "(contains {from} -> {to})",
                            from = file_entity_id,
                            to = entity.id
                        ),
                        core_file_contains_edge(&file_entity_id, entity.id.as_str()),
                    ));
                }
                all_entity_ids.insert(id_str.clone());
                file_entities.push((id_str.clone(), record.clone()));
            }
            let unresolved_for_file = map_unresolved_call_sites_for_file(
                &stats,
                &file_entities,
                &kind_roles,
                &iso8601_now(),
            )
            .map_err(|e| {
                format!("plugin {plugin_id} emitted invalid unresolved call-site stats: {e:#}")
            })?;
            for edge in edges {
                let descr = format!(
                    "({kind} {from} -> {to})",
                    kind = edge.kind,
                    from = edge.from_id,
                    to = edge.to_id,
                );
                let record = map_edge_to_record(edge, Some(file_entity_id.clone()));
                file_edges.push((descr, record));
            }
            let (immediate_edges, import_edges) = split_deferred_import_edges(file_edges);
            deferred_import_edges.extend(import_edges);
            batch_tx
                .blocking_send(PluginBatchMessage::File(PluginFileBatch {
                    source_file_id: file_entity_id.clone(),
                    entities: file_entities,
                    kind_roles: kind_roles.clone(),
                    edges: immediate_edges,
                    unresolved_call_sites: unresolved_for_file,
                    stats: file_stats,
                    signatures: file_signatures,
                }))
                .map_err(|_| "plugin batch receiver closed".to_owned())?;
        }
        let imports_skipped_external = filter_external_import_edges_by_module_ids(
            &file_scope_entity_ids,
            &all_entity_ids,
            &mut deferred_import_edges,
        );
        batch_tx
            .blocking_send(PluginBatchMessage::DeferredImportEdges {
                edges: deferred_import_edges,
                imports_skipped_external,
            })
            .map_err(|_| "plugin batch receiver closed".to_owned())?;
        Ok(())
    })();

    // Read the file-loop verdict BEFORE arming the shutdown deadline: the
    // watchdog records the FIRST expired phase, so a `Shutdown` record can
    // only exist when no earlier phase fired (and shutdown only runs on the
    // Ok path anyway).
    let timed_out = watchdog.timed_out_phase() == Some(WatchdogPhase::File);

    // A timeout forces the failure branch: the watchdog already killed the child,
    // so any in-flight read failed (or, in a near-deadline race, a stale Ok no
    // longer reflects a live plugin). Replace the reason with a clear timeout.
    let work_result = if timed_out {
        Err(format!(
            "plugin {plugin_id} exceeded the per-file analysis timeout ({} ms) and was killed",
            file_timeout.as_millis()
        ))
    } else {
        work_result
    };

    // Try a graceful shutdown on the happy path, under its own wall-clock
    // deadline (ADR-050 D7): the watchdog is still alive here, so a plugin
    // that goes silent at `shutdown` is killed, the blocked read unblocks,
    // and the etiquette failure surfaces as a WARN finding below — the run
    // outcome is unchanged. On error, skip straight to kill — the plugin's
    // behaviour is already untrusted. `analyze_file` already issues
    // `shutdown`/`exit` before returning PathEscapeBreaker or EntityCap
    // errors, so calling `host.shutdown()` again there would write to a
    // closed pipe; that's why we only call it on Ok.
    if work_result.is_ok() {
        watchdog.arm(shutdown_timeout, WatchdogPhase::Shutdown);
        let shutdown_result = host.shutdown();
        watchdog.disarm();
        if let Err(e) = shutdown_result {
            tracing::warn!(
                plugin_id = %plugin_id,
                error = %e,
                "best-effort host shutdown failed; falling back to kill()",
            );
            if let Ok(mut c) = child.lock() {
                let _ = c.kill();
            }
        }
    } else if let Ok(mut c) = child.lock() {
        let _ = c.kill();
    }

    // Stop and join the watchdog before reaping so it no longer holds the child
    // handle (lets us reclaim the owned `Child` for the reap path).
    watchdog.request_stop();
    let _ = watchdog_handle.join();
    let shutdown_timed_out = watchdog.timed_out_phase() == Some(WatchdogPhase::Shutdown);
    let mut child = Arc::try_unwrap(child)
        .unwrap_or_else(|_| unreachable!("watchdog joined; no other Arc holders remain"))
        .into_inner()
        .expect("child mutex poisoned");

    let mut findings = host.take_findings();
    findings.extend(dispatch_findings);
    drop(host);

    // REQ-ANALYZE-06: a per-file timeout is a recoverable failure that must be
    // visible. Add a LMWV-PY-TIMEOUT host finding; it rides out through
    // PluginRunError.findings and is persisted by the run's crash path.
    if timed_out {
        findings.push(plugin_timeout_finding(
            plugin_id,
            "file",
            file_timeout,
            format!(
                "plugin {plugin_id} exceeded the per-file analysis timeout ({} ms) and was killed",
                file_timeout.as_millis()
            ),
        ));
    }

    // ADR-050 D7: a shutdown timeout is visible (WARN) but does NOT touch
    // `work_result` — the entities are durable, only exit etiquette failed.
    // It rides the Ok path, so it never ticks the crash-loop breaker.
    if shutdown_timed_out {
        let mut metadata = BTreeMap::new();
        metadata.insert("plugin_id".to_owned(), plugin_id.to_owned());
        metadata.insert(
            "timeout_ms".to_owned(),
            shutdown_timeout.as_millis().to_string(),
        );
        findings.push(HostFinding {
            subcode: PLUGIN_SHUTDOWN_TIMEOUT_RULE_ID.to_owned(),
            message: format!(
                "plugin {plugin_id} exceeded the shutdown timeout ({} ms) and was killed; \
                 analysis results are unaffected",
                shutdown_timeout.as_millis()
            ),
            metadata,
        });
    }

    // Reap unconditionally. `Child::Drop` does not wait on Unix. When any
    // lifecycle deadline fired, the SIGKILL in the exit status is the
    // watchdog's own — suppress the OOM classification (ADR-050).
    reap_and_classify_exit(
        &mut child,
        plugin_id,
        &mut findings,
        watchdog.timed_out_phase().is_some(),
    );

    match work_result {
        Ok(()) => Ok(BatchResult { findings }),
        Err(reason) => Err(PluginRunError::with_findings(reason, findings)),
    }
}

/// Build the `LMWV-PY-TIMEOUT` host finding for a lifecycle-deadline kill
/// (ADR-050). `phase` names the [`WatchdogPhase`] that fired (`"handshake"` /
/// `"file"`) so a consumer can tell a hung `initialize` from a hung
/// `analyze_file` without parsing the message.
fn plugin_timeout_finding(
    plugin_id: &str,
    phase: &str,
    timeout: std::time::Duration,
    message: String,
) -> HostFinding {
    let mut metadata = BTreeMap::new();
    metadata.insert("plugin_id".to_owned(), plugin_id.to_owned());
    metadata.insert("phase".to_owned(), phase.to_owned());
    metadata.insert("timeout_ms".to_owned(), timeout.as_millis().to_string());
    HostFinding {
        subcode: PLUGIN_TIMEOUT_RULE_ID.to_owned(),
        message,
        metadata,
    }
}

/// Wait on the child, inspect its exit status, and append a classification
/// finding for abnormal terminations (ADR-050):
///
/// - SIGABRT (6) → [`HostFinding::aborted`] — the stack-overflow / explicit
///   `abort()` signature; always reported.
/// - SIGKILL (9) / SIGSEGV (11) → [`HostFinding::oom_killed`] — the observed
///   `RLIMIT_AS` signatures (ADR-021 §2d; kernel behaviour varies between
///   the OOM-killer path and an unhandled map failure) — UNLESS
///   `suppress_kill_classification` is set: when the lifecycle watchdog
///   already killed the child, the SIGKILL is the host's own and reporting
///   it as an OOM event would double-report a single timeout.
///
/// Other signals or non-zero exit codes get a warn log but no finding — the
/// cause is ambiguous without more bookkeeping.
fn reap_and_classify_exit(
    child: &mut std::process::Child,
    plugin_id: &str,
    findings: &mut Vec<HostFinding>,
    suppress_kill_classification: bool,
) {
    reap_and_classify_exit_with_timeout(
        child,
        plugin_id,
        findings,
        PLUGIN_REAP_TIMEOUT,
        suppress_kill_classification,
    );
}

const PLUGIN_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const PLUGIN_REAP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

fn reap_and_classify_exit_with_timeout(
    child: &mut std::process::Child,
    plugin_id: &str,
    findings: &mut Vec<HostFinding>,
    timeout: std::time::Duration,
    suppress_kill_classification: bool,
) {
    match wait_child_with_timeout(child, timeout) {
        Ok(Some(status)) => {
            classify_child_exit_status(status, plugin_id, findings, suppress_kill_classification);
        }
        Ok(None) => {
            tracing::warn!(
                plugin_id = %plugin_id,
                timeout_ms = timeout.as_millis(),
                "plugin did not exit before reap timeout; killing child",
            );
            if let Err(e) = child.kill() {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    error = %e,
                    "failed to kill plugin child after reap timeout",
                );
            }
            match child.wait() {
                Ok(status) => tracing::warn!(
                    plugin_id = %plugin_id,
                    status = ?status,
                    "plugin child reaped after timeout kill",
                ),
                Err(e) => tracing::warn!(
                    plugin_id = %plugin_id,
                    error = %e,
                    "failed to wait on plugin child after timeout kill",
                ),
            }
        }
        Err(e) => {
            tracing::warn!(
                plugin_id = %plugin_id,
                error = %e,
                "failed to wait on plugin child",
            );
        }
    }
}

fn wait_child_with_timeout(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        std::thread::sleep(PLUGIN_REAP_POLL_INTERVAL.min(deadline - now));
    }
}

fn classify_child_exit_status(
    status: std::process::ExitStatus,
    plugin_id: &str,
    findings: &mut Vec<HostFinding>,
    suppress_kill_classification: bool,
) {
    if status.success() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            tracing::warn!(
                plugin_id = %plugin_id,
                signal,
                "plugin terminated by signal",
            );
            // SIGABRT (6) is the stack-overflow / explicit-abort signature
            // (ADR-050). Never suppressed: the watchdog kills with SIGKILL,
            // so an abort is always the plugin's own.
            if signal == 6 {
                findings.push(HostFinding::aborted(plugin_id, signal));
            }
            // SIGKILL (9) and SIGSEGV (11) are the observed signatures
            // of an RLIMIT_AS kill in Sprint-1 testing — but when the
            // lifecycle watchdog already killed this child, the SIGKILL is
            // the host's own (the timeout finding is the root cause).
            if (signal == 9 || signal == 11) && !suppress_kill_classification {
                findings.push(HostFinding::oom_killed(plugin_id, signal));
            }
        } else if let Some(code) = status.code() {
            tracing::warn!(
                plugin_id = %plugin_id,
                code,
                "plugin exited non-zero",
            );
        }
    }
    #[cfg(not(unix))]
    {
        tracing::warn!(
            plugin_id = %plugin_id,
            "plugin exited non-successfully (exit-status inspection is Unix-only)",
        );
    }
}

/// Map a `HostError` from `analyze_file` to a human-readable fail-run reason.
fn classify_host_error(plugin_id: &str, e: HostError) -> String {
    match e {
        HostError::EntityCapExceeded(_) => {
            format!("plugin {plugin_id} exceeded entity-count cap")
        }
        HostError::PathEscapeBreakerTripped => {
            format!("plugin {plugin_id} tripped path-escape breaker")
        }
        HostError::Spawn(msg) => {
            format!("failed to spawn plugin {plugin_id}: {msg}")
        }
        HostError::Handshake(ref me) => {
            format!("plugin {plugin_id} refused handshake: {me}")
        }
        HostError::Transport(ref te) => {
            format!("plugin {plugin_id} transport/protocol error: {te}")
        }
        HostError::Protocol(ref pe) => {
            format!(
                "plugin {plugin_id} transport/protocol error: code={}, message={}",
                pe.code, pe.message
            )
        }
        other => format!("plugin {plugin_id} error: {other}"),
    }
}

#[cfg(test)]
fn filter_external_import_edges(
    entities: &[(String, EntityRecord)],
    kind_roles: &PluginKindRoles,
    edges: &mut Vec<(String, EdgeRecord)>,
) -> u64 {
    let module_entity_ids: BTreeSet<&str> = entities
        .iter()
        .filter(|(_, record)| kind_roles.is_file_scope(&record.kind))
        .map(|(id, _)| id.as_str())
        .collect();
    let all_entity_ids: BTreeSet<&str> = entities.iter().map(|(id, _)| id.as_str()).collect();
    filter_external_import_edges_by_module_refs(&module_entity_ids, &all_entity_ids, edges)
}

fn filter_external_import_edges_by_module_ids(
    module_entity_ids: &BTreeSet<String>,
    all_entity_ids: &BTreeSet<String>,
    edges: &mut Vec<(String, EdgeRecord)>,
) -> u64 {
    let module_entity_ids: BTreeSet<&str> = module_entity_ids.iter().map(String::as_str).collect();
    let all_entity_ids: BTreeSet<&str> = all_entity_ids.iter().map(String::as_str).collect();
    filter_external_import_edges_by_module_refs(&module_entity_ids, &all_entity_ids, edges)
}

fn filter_external_import_edges_by_module_refs(
    module_entity_ids: &BTreeSet<&str>,
    all_entity_ids: &BTreeSet<&str>,
    edges: &mut Vec<(String, EdgeRecord)>,
) -> u64 {
    let before = edges.len();
    edges.retain_mut(|(_, edge)| {
        if edge.kind != "imports" {
            return true;
        }
        // The `from X import sub` → `X.sub` collapse stays MODULE-keyed: we only
        // rewrite the target onto an absolute submodule when that submodule is
        // itself a file-scope module. A Python `from pkg import func` therefore
        // keeps pointing at the module `pkg`, never re-targeting onto the function
        // `pkg.func` — preserving the established Python import shape
        // (clarion-d1e3dc67dc).
        if let Some(local_submodule) =
            absolute_from_import_submodule_target(edge, module_entity_ids)
        {
            edge.to_id = local_submodule;
            return true;
        }
        // Retain any edge whose target is a stored in-project entity of ANY kind
        // (module, function, struct, const, trait, …). The Rust resolver targets
        // `use` paths at non-module items; the Python plugin only ever mints module
        // targets, so this is a strict superset for Python (clarion-d1e3dc67dc).
        // A target absent from the store is still genuinely external and dropped.
        all_entity_ids.contains(edge.to_id.as_str())
    });
    u64::try_from(before - edges.len()).unwrap_or(u64::MAX)
}

fn split_deferred_import_edges(
    edges: Vec<DescribedEdgeRecord>,
) -> (Vec<DescribedEdgeRecord>, Vec<DescribedEdgeRecord>) {
    edges
        .into_iter()
        .partition(|(_, edge)| edge.kind != "imports")
}

fn absolute_from_import_submodule_target(
    edge: &EdgeRecord,
    module_entity_ids: &BTreeSet<&str>,
) -> Option<String> {
    let properties = edge
        .properties_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())?;
    if properties
        .get("import_style")
        .and_then(|value| value.as_str())
        != Some("from_import")
    {
        return None;
    }
    if properties.get("level").and_then(serde_json::Value::as_u64) != Some(0) {
        return None;
    }
    let imported_name = properties
        .get("imported_name")
        .and_then(|value| value.as_str())?;
    if imported_name == "*" || imported_name.is_empty() {
        return None;
    }
    let candidate = format!("{}.{}", edge.to_id, imported_name);
    module_entity_ids
        .contains(candidate.as_str())
        .then_some(candidate)
}

fn core_file_entity_record(
    project_root: &Path,
    file: &Path,
    manifest_language: &str,
    briefing_blocks: &BTreeMap<PathBuf, loomweave_core::BriefingBlockReason>,
    scanned_source_files: &BTreeSet<PathBuf>,
) -> Result<(String, EntityRecord)> {
    let canonical_root = project_root
        .canonicalize()
        .with_context(|| format!("canonicalize project root {}", project_root.display()))?;
    let canonical_file = file
        .canonicalize()
        .with_context(|| format!("canonicalize source file {}", file.display()))?;
    let (id, qualified_name) =
        core_file_entity_id_from_canonical(&canonical_root, &canonical_file)?;
    let briefing_blocked = briefing_blocks.get(&canonical_file).copied().or_else(|| {
        (!scanned_source_files.contains(&canonical_file))
            .then_some(loomweave_core::BriefingBlockReason::UnscannedSource)
    });
    let source_file_path = canonical_file
        .into_os_string()
        .into_string()
        .map_err(|path| {
            anyhow::anyhow!("source file path is not valid UTF-8: {}", path.display())
        })?;
    let short_name = Path::new(&source_file_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&qualified_name)
        .to_owned();
    let content_hash = whole_file_hash(&canonical_root, Path::new(&source_file_path))
        .with_context(|| format!("read source file {source_file_path}"))?;
    let mut properties = serde_json::Map::new();
    properties.insert(
        "language".to_owned(),
        serde_json::Value::String(manifest_language.to_owned()),
    );
    if let Some(reason) = briefing_blocked {
        properties.insert(
            "briefing_blocked".to_owned(),
            serde_json::Value::String(reason.as_str().to_owned()),
        );
    }
    let properties_json = serde_json::Value::Object(properties).to_string();
    let now = iso8601_now();

    Ok((
        id.clone(),
        EntityRecord {
            id,
            plugin_id: "core".to_owned(),
            kind: "file".to_owned(),
            name: qualified_name,
            short_name,
            parent_id: None,
            source_file_id: None,
            source_file_path: Some(source_file_path),
            source_byte_start: None,
            source_byte_end: None,
            source_line_start: None,
            source_line_end: None,
            properties_json,
            tags: Vec::new(),
            content_hash: Some(content_hash),
            summary_json: None,
            wardline_json: None,
            first_seen_commit: None,
            last_seen_commit: None,
            created_at: now.clone(),
            updated_at: now,
        },
    ))
}

fn core_file_entity_id(project_root: &Path, file: &Path) -> Result<String> {
    let canonical_root = project_root
        .canonicalize()
        .with_context(|| format!("canonicalize project root {}", project_root.display()))?;
    let canonical_file = file
        .canonicalize()
        .with_context(|| format!("canonicalize source file {}", file.display()))?;
    let (id, _) = core_file_entity_id_from_canonical(&canonical_root, &canonical_file)?;
    Ok(id)
}

fn core_file_entity_id_from_canonical(
    canonical_root: &Path,
    canonical_file: &Path,
) -> Result<(String, String)> {
    let relative = canonical_file
        .strip_prefix(canonical_root)
        .with_context(|| {
            format!(
                "source file {} is outside project root {}",
                canonical_file.display(),
                canonical_root.display()
            )
        })?;
    let qualified_name = project_relative_posix(relative)?;
    let id = loomweave_core::entity_id::entity_id("core", "file", &qualified_name)?.to_string();
    Ok((id, qualified_name))
}

fn project_relative_posix(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "source file path component is not valid UTF-8: {}",
                        part.display()
                    )
                })?;
                parts.push(part);
            }
            std::path::Component::CurDir => {}
            _ => {
                bail!(
                    "source file path is not project-relative: {}",
                    path.display()
                );
            }
        }
    }
    let relative = parts.join("/");
    if relative.is_empty() {
        bail!("source file path must not resolve to the project root");
    }
    Ok(relative)
}

/// Map an `AcceptedEntity` to an `EntityRecord` for the writer-actor.
fn map_entity_to_record(
    project_root: &Path,
    entity: &AcceptedEntity,
    plugin_id: &str,
    source_file_id: Option<String>,
    kind_roles: &PluginKindRoles,
) -> EntityRecord {
    let short_name = entity
        .qualified_name
        .rsplit('.')
        .next()
        .unwrap_or(&entity.qualified_name)
        .to_owned();

    let properties_json =
        serde_json::to_string(&entity.raw.extra).unwrap_or_else(|_| "{}".to_owned());

    let now = iso8601_now();
    let source_line_range = source_line_range(entity);

    EntityRecord {
        id: entity.id.to_string(),
        plugin_id: plugin_id.to_owned(),
        kind: entity.kind.clone(),
        name: entity.qualified_name.clone(),
        short_name,
        parent_id: entity.raw.parent_id.clone(),
        source_file_id,
        source_file_path: Some(entity.source_file_path.clone()),
        source_byte_start: None,
        source_byte_end: None,
        source_line_start: source_line_range.map(|range| range.start_line),
        source_line_end: source_line_range.map(|range| range.end_line),
        properties_json,
        tags: normalised_entity_tags(&entity.raw.tags),
        content_hash: content_hash_for_entity(project_root, entity, source_line_range, kind_roles),
        summary_json: None,
        wardline_json: None,
        first_seen_commit: None,
        last_seen_commit: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

fn normalised_entity_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct SourceLineRange {
    start_line: i64,
    end_line: i64,
}

fn source_line_range(entity: &AcceptedEntity) -> Option<SourceLineRange> {
    let source_range = entity.raw.source.extra.get("source_range")?;
    let start_line = source_range.get("start_line")?.as_i64()?;
    let end_line = source_range.get("end_line")?.as_i64()?;
    if start_line <= 0 || end_line < start_line {
        return None;
    }
    Some(SourceLineRange {
        start_line,
        end_line,
    })
}

/// The blake3 hex of a file's whole contents — the single canonical whole-file
/// hash used everywhere the "did this file change?" question is asked: the core
/// `file` entity's `content_hash` (`core_file_entity`), a plugin file-scope
/// entity's `content_hash` (`content_hash_for_entity`), and the Wave 2
/// incremental-skip check. They MUST agree byte-for-byte or the skip silently
/// never matches; one helper guarantees that. `None` when the file cannot be
/// read — callers fail toward re-analysis.
fn whole_file_hash(project_root: &Path, path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = loomweave_core::plugin::jail::safe_open(project_root, path).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(blake3::hash(&bytes).to_hex().to_string())
}

/// The canonical UTF-8 path string for a source file, formed exactly as
/// `core_file_entity` forms `source_file_path` (canonicalise → UTF-8), so the
/// incremental-skip lookup keys cleanly against `previously_analyzed_files` /
/// `prior_locators_by_file`. `None` when the path cannot be canonicalised or is
/// not UTF-8 — callers fail toward re-analysis.
fn canonical_path_key(path: &Path) -> Option<String> {
    path.canonicalize()
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

/// Whether `path` must be re-analysed (Wave 2 / T3.1). Re-analyses — the safe,
/// fail-toward-work direction — on any uncertainty: the path cannot be
/// canonicalised, the prior run recorded no whole-file hash for it (a new file),
/// or the file is unhashable now. Skips only on a confident byte-identical match.
fn file_needs_reanalysis(
    project_root: &Path,
    path: &Path,
    prior_file_hashes: &HashMap<String, String>,
) -> bool {
    let Some(key) = canonical_path_key(path) else {
        return true;
    };
    let Some(prior) = prior_file_hashes.get(&key) else {
        return true;
    };
    match whole_file_hash(project_root, path) {
        Some(current) => &current != prior,
        None => true,
    }
}

fn content_hash_for_entity(
    project_root: &Path,
    entity: &AcceptedEntity,
    source_line_range: Option<SourceLineRange>,
    kind_roles: &PluginKindRoles,
) -> Option<String> {
    use std::io::Read;

    if kind_roles.is_file_scope(&entity.kind) {
        return whole_file_hash(project_root, Path::new(&entity.source_file_path));
    }

    let range = source_line_range?;
    let mut file =
        loomweave_core::plugin::jail::safe_open(project_root, Path::new(&entity.source_file_path))
            .ok()?;
    let mut source = String::new();
    file.read_to_string(&mut source).ok()?;
    let lines: Vec<&str> = source.lines().collect();
    let start = usize::try_from(range.start_line - 1).ok()?;
    let mut end = usize::try_from(range.end_line).ok()?;
    end = end.min(lines.len());
    if start >= end {
        return None;
    }
    let normalized = lines[start..end].join("\n");
    Some(blake3::hash(normalized.as_bytes()).to_hex().to_string())
}

/// Canonicalise a plugin-declared SEI signature for stable string-equality
/// comparison (ADR-038 REQ-C-01). The core re-serialises the value (keys sorted
/// by `serde_json`'s default `BTreeMap`-backed object), never interpreting its
/// semantics. Both the current run and the prior binding pass through this same
/// path, so the comparison is self-consistent run-to-run.
fn canonical_signature(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

/// Map an `AcceptedEdge` to an `EdgeRecord` for the writer-actor (B.3).
fn core_file_contains_edge(file_entity_id: &str, child_entity_id: &str) -> EdgeRecord {
    EdgeRecord {
        kind: "contains".to_owned(),
        from_id: file_entity_id.to_owned(),
        to_id: child_entity_id.to_owned(),
        confidence: EdgeConfidence::Resolved,
        properties_json: None,
        source_file_id: Some(file_entity_id.to_owned()),
        source_byte_start: None,
        source_byte_end: None,
    }
}

fn map_edge_to_record(edge: AcceptedEdge, source_file_id: Option<String>) -> EdgeRecord {
    let properties_json = edge
        .raw
        .properties
        .as_ref()
        .and_then(|v| serde_json::to_string(v).ok());
    EdgeRecord {
        kind: edge.kind,
        from_id: edge.from_id,
        to_id: edge.to_id,
        confidence: edge.confidence,
        properties_json,
        source_file_id,
        source_byte_start: edge.raw.source_byte_start,
        source_byte_end: edge.raw.source_byte_end,
    }
}

fn map_unresolved_call_sites_for_file(
    stats: &loomweave_core::AnalyzeFileStats,
    entities: &[(String, EntityRecord)],
    kind_roles: &PluginKindRoles,
    created_at: &str,
) -> Result<Vec<PendingUnresolvedCallSites>> {
    let entities_by_id: BTreeMap<&str, &EntityRecord> = entities
        .iter()
        .map(|(id, record)| (id.as_str(), record))
        .collect();
    let authoritative =
        u64::try_from(stats.unresolved_call_sites.len()) == Ok(stats.unresolved_call_sites_total);
    let mut grouped: BTreeMap<String, PendingUnresolvedCallSites> = BTreeMap::new();

    if authoritative {
        for (id, record) in entities {
            if !kind_roles.is_callable(&record.kind) {
                continue;
            }
            let Some(content_hash) = &record.content_hash else {
                continue;
            };
            grouped.insert(
                id.clone(),
                PendingUnresolvedCallSites {
                    caller_entity_id: id.clone(),
                    caller_content_hash: content_hash.clone(),
                    sites: Vec::new(),
                },
            );
        }
    }

    for site in &stats.unresolved_call_sites {
        validate_unresolved_call_site(site)?;
        let caller = entities_by_id
            .get(site.caller_entity_id.as_str())
            .with_context(|| {
                format!(
                    "unresolved call site refers to caller not emitted in same file: {}",
                    site.caller_entity_id
                )
            })?;
        if !kind_roles.is_callable(&caller.kind) {
            bail!(
                "unresolved call site caller kind {:?} is not manifest-declared callable: {}",
                caller.kind,
                site.caller_entity_id
            );
        }
        let content_hash = caller.content_hash.clone().with_context(|| {
            format!(
                "unresolved call site caller lacks content_hash: {}",
                site.caller_entity_id
            )
        })?;
        let entry = grouped
            .entry(site.caller_entity_id.clone())
            .or_insert_with(|| PendingUnresolvedCallSites {
                caller_entity_id: site.caller_entity_id.clone(),
                caller_content_hash: content_hash.clone(),
                sites: Vec::new(),
            });
        entry.sites.push(UnresolvedCallSiteRecord {
            caller_entity_id: site.caller_entity_id.clone(),
            caller_content_hash: content_hash,
            site_key: unresolved_call_site_key(
                &site.caller_entity_id,
                site.source_byte_start,
                site.source_byte_end,
                &site.callee_expr,
            ),
            site_ordinal: site.site_ordinal,
            source_file_id: caller.source_file_id.clone(),
            source_byte_start: site.source_byte_start,
            source_byte_end: site.source_byte_end,
            callee_expr: site.callee_expr.clone(),
            created_at: created_at.to_owned(),
        });
    }

    Ok(grouped.into_values().collect())
}

fn validate_unresolved_call_site(site: &UnresolvedCallSite) -> Result<()> {
    if site.site_ordinal < 0 {
        bail!("unresolved call site has negative site_ordinal");
    }
    if site.source_byte_start < 0 {
        bail!("unresolved call site has negative source_byte_start");
    }
    if site.source_byte_end <= site.source_byte_start {
        bail!("unresolved call site has empty or reversed source byte range");
    }
    if site.callee_expr.is_empty() {
        bail!("unresolved call site has empty callee_expr");
    }
    if site.callee_expr.len() > 512 {
        bail!("unresolved call site callee_expr exceeds 512 bytes");
    }
    Ok(())
}

fn unresolved_call_site_key(
    caller_entity_id: &str,
    source_byte_start: i64,
    source_byte_end: i64,
    callee_expr: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(caller_entity_id.as_bytes());
    hasher.update(&source_byte_start.to_be_bytes());
    hasher.update(&source_byte_end.to_be_bytes());
    hasher.update(callee_expr.as_bytes());
    hasher.finalize().to_hex().to_string()
}

// ── Source-tree walk ──────────────────────────────────────────────────────────

/// Skip-list for directory names during the source walk.
///
/// Sprint 1 conservative set: VCS directories, the shared .weft/ runtime state, and
/// common virtual-environment directories.
const SKIP_DIRS: &[&str] = &[
    ".weft",
    ".git",
    ".hg",
    ".svn",
    ".jj",
    ".venv",
    "__pycache__",
    "node_modules",
];

/// Collect all source files under `root` whose extension is in `wanted`.
///
#[derive(Debug, Default)]
struct SourceWalkResult {
    files: Vec<PathBuf>,
    skipped_errors: Vec<String>,
}

/// Uses the `ignore` crate so `.gitignore` / `.ignore` / global gitignore
/// policy filters the source set before plugin dispatch. Matching files must
/// also pass the path-jail safe-open check before they enter the shared source
/// list used by secret scanning and plugin dispatch.
///
/// Per-entry I/O errors (a dirent we couldn't stat, a file whose
/// `file_type()` probe failed) are logged at `warn` level and counted.
/// When the walk completes with non-zero skips, a summary is logged so
/// the operator can see that the file list is incomplete — silently
/// dropping those entries would mask the same "incomplete analysis"
/// class that the WP1 `read_applied_versions` `.ok()` pattern did.
fn collect_source_files(root: &Path, wanted_extensions: &BTreeSet<String>) -> SourceWalkResult {
    let mut out = Vec::new();
    let mut skipped_errors = Vec::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(false)
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|entry| !is_skipped_dir(entry));

    for result in builder.build() {
        match result {
            Ok(entry) => {
                let Some(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_file() {
                    continue;
                }
                let path = entry.into_path();
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let ext_lower = ext.to_ascii_lowercase();
                    if wanted_extensions.contains(&ext_lower) {
                        match verified_plugin_dispatch_path(root, &path) {
                            Ok(safe_path) => out.push(safe_path),
                            Err(err) => skipped_errors.push(format!("{}: {err:#}", path.display())),
                        }
                    }
                }
            }
            Err(err) => {
                let message = err.to_string();
                tracing::warn!(
                    error = %message,
                    "source walk: skipping unreadable or ignored-path-error entry",
                );
                skipped_errors.push(message);
            }
        }
    }

    if !skipped_errors.is_empty() {
        let skipped = skipped_errors.len();
        tracing::warn!(
            skipped = skipped,
            root = %root.display(),
            "source walk skipped {skipped} unreadable entr{suffix}; analysis is \
             incomplete for those paths",
            suffix = if skipped == 1 { "y" } else { "ies" },
        );
    }
    SourceWalkResult {
        files: out,
        skipped_errors,
    }
}

fn is_skipped_dir(entry: &DirEntry) -> bool {
    entry
        .file_type()
        .is_some_and(|file_type| file_type.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| SKIP_DIRS.contains(&name))
}

// ── Time helpers ──────────────────────────────────────────────────────────────

const ISO8601_MILLIS_UTC: &[time::format_description::FormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

/// Format `OffsetDateTime::now_utc()` as an `ISO-8601` UTC string with
/// millisecond precision (`YYYY-MM-DDTHH:MM:SS.sssZ`).
fn iso8601_now() -> String {
    OffsetDateTime::now_utc()
        .format(ISO8601_MILLIS_UTC)
        .expect("fixed ISO-8601 format description should format")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;

    #[test]
    fn relativize_for_emit_strips_project_root_else_passes_through() {
        let root = Path::new("/home/u/proj");
        // Under-root absolute path → project-relative (Filigree's requirement).
        assert_eq!(
            relativize_for_emit(root, "/home/u/proj/specimen/a.py"),
            "specimen/a.py"
        );
        // An out-of-root path is left untouched (Filigree rejects it loudly
        // rather than us silently rewriting it into something wrong).
        assert_eq!(
            relativize_for_emit(root, "/etc/passwd"),
            "/etc/passwd"
        );
        // The project root itself relativizes to "" — the emit map filters this
        // to None (a file-less, project-anchored finding) rather than emitting an
        // empty path Filigree rejects ("path is empty after normalization").
        assert_eq!(relativize_for_emit(root, "/home/u/proj"), "");
    }

    #[test]
    fn clamp_lines_to_file_clears_positions_past_eof() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A 3-line file (no trailing newline) — `str::lines()` counts 3, matching
        // Filigree's Python file-iteration line count.
        fs::write(dir.path().join("three.py"), "a\nb\nc").expect("write");
        // line_end past EOF is dropped; in-range line_start kept (the A4 case:
        // a syntax-error finding reporting line_end 4 on a 3-line file).
        assert_eq!(
            clamp_lines_to_file(dir.path(), "three.py", Some(1), Some(4)),
            (Some(1), None)
        );
        // line_start past EOF clears both (no usable anchor).
        assert_eq!(
            clamp_lines_to_file(dir.path(), "three.py", Some(9), Some(9)),
            (None, None)
        );
        // In-range positions are preserved verbatim.
        assert_eq!(
            clamp_lines_to_file(dir.path(), "three.py", Some(2), Some(3)),
            (Some(2), Some(3))
        );
        // An unreadable file leaves the positions untouched (no false clamp).
        assert_eq!(
            clamp_lines_to_file(dir.path(), "absent.py", Some(1), Some(99)),
            (Some(1), Some(99))
        );
    }

    #[test]
    fn dedup_descriptors_keeps_one_per_locator_last_wins() {
        // A plugin may legally emit the same id twice in a run (the entity layer
        // tolerates it via ON CONFLICT(id) DO UPDATE, last-wins). The SEI
        // descriptor list must collapse to one per locator, last-wins, so the
        // mint pass never plans two `alive` bindings on one locator (H1).
        let descriptors = vec![
            NewEntityDescriptor {
                locator: "python:function:m.f".to_owned(),
                body_hash: Some("first".to_owned()),
                signature: Some("s0".to_owned()),
            },
            NewEntityDescriptor {
                locator: "python:function:m.g".to_owned(),
                body_hash: Some("g".to_owned()),
                signature: None,
            },
            NewEntityDescriptor {
                locator: "python:function:m.f".to_owned(),
                body_hash: Some("last".to_owned()),
                signature: Some("s1".to_owned()),
            },
        ];
        let deduped = dedup_descriptors_by_locator(descriptors);
        // Exactly one entry per locator, sorted by locator.
        assert_eq!(
            deduped
                .iter()
                .map(|d| d.locator.as_str())
                .collect::<Vec<_>>(),
            vec!["python:function:m.f", "python:function:m.g"]
        );
        // Last write wins for the duplicated locator (matches the entity row).
        let f = deduped
            .iter()
            .find(|d| d.locator == "python:function:m.f")
            .unwrap();
        assert_eq!(f.body_hash.as_deref(), Some("last"));
        assert_eq!(f.signature.as_deref(), Some("s1"));
    }

    #[test]
    fn analyze_complete_full_run_reports_whole_graph_totals() {
        // A full run (no unchanged files skipped) reports the graph totals with
        // the subsystem breakdown, matching `project_status` phrasing.
        let line = format_analyze_complete("run-1", 263, 5, 496, 0, None);
        assert_eq!(
            line,
            "analyze complete: run run-1 completed \
             (graph: 263 entities incl. 5 subsystems, 496 edges)"
        );
    }

    #[test]
    fn analyze_complete_incremental_run_annotates_skipped_files() {
        // An incremental run that skipped unchanged files reports the SAME graph
        // totals (not the tiny insert delta) plus an explicit incremental marker.
        let line = format_analyze_complete("run-2", 263, 5, 496, 29, None);
        assert_eq!(
            line,
            "analyze complete: run run-2 completed \
             (graph: 263 entities incl. 5 subsystems, 496 edges; \
             incremental: 29 unchanged files skipped)"
        );
    }

    #[test]
    fn analyze_complete_incremental_singular_file_uses_singular_noun() {
        let line = format_analyze_complete("run-3", 10, 0, 4, 1, None);
        assert_eq!(
            line,
            "analyze complete: run run-3 completed \
             (graph: 10 entities incl. 0 subsystems, 4 edges; \
             incremental: 1 unchanged file skipped)"
        );
    }

    #[test]
    fn analyze_complete_degraded_emit_rides_the_completion_line() {
        // PDR-0023 honesty invariant: a dead Filigree emit must be visible on the
        // command's stdout completion line, not swallowed as a confident exit-0.
        // The marker carries a machine-readable status + reason a caller can gate on.
        let line = format_analyze_complete(
            "run-4",
            10,
            0,
            4,
            0,
            Some("emit:unreachable (connection refused)"),
        );
        assert_eq!(
            line,
            "analyze complete: run run-4 completed \
             (graph: 10 entities incl. 0 subsystems, 4 edges); \
             emit:unreachable (connection refused)"
        );
    }

    // --- emit_status_marker: the honesty-invariant classifier (PDR-0023) ---
    //
    // The three CLEAN shapes must yield NO marker (byte-indistinguishable from a
    // plain success on the completion line), and must stay distinguishable from
    // each DEGRADED shape — which is the whole point of the invariant.

    #[test]
    fn emit_marker_none_when_integration_disabled() {
        // A `Null` blob = integration off / emit_findings=false: a clean disabled
        // state, not a degraded seam. No marker.
        assert_eq!(emit_status_marker(&serde_json::Value::Null), None);
    }

    #[test]
    fn emit_marker_none_for_clean_true_negative_emit() {
        // A real POST that landed with zero intake warnings is success — whether it
        // carried zero findings (a clean true-negative) or many. No marker.
        let blob = serde_json::json!({
            "status": "emitted",
            "findings_total": 0,
            "emitted": 0,
            "findings_created": 0,
            "warnings": [],
        });
        assert_eq!(emit_status_marker(&blob), None);
        let blob_nonempty = serde_json::json!({
            "status": "emitted",
            "emitted": 7,
            "findings_created": 7,
            "warnings": [],
        });
        assert_eq!(emit_status_marker(&blob_nonempty), None);
    }

    #[test]
    fn emit_marker_none_for_filtered_no_op_skip() {
        // The Phase-8c "nothing emittable remained" skip is a clean true-negative,
        // NOT a degraded seam. No marker.
        let blob = serde_json::json!({
            "status": "skipped",
            "reason": "no_postrun_findings_with_path",
            "findings_total": 0,
        });
        assert_eq!(emit_status_marker(&blob), None);
    }

    #[test]
    fn emit_marker_surfaces_total_unreachable_failure() {
        // THE STRIKE: a total emit failure (the POST never landed) is the
        // best-effort swallow. It must surface a degraded marker with its reason,
        // distinguishable from `emitted` and from `disabled`.
        let blob = serde_json::json!({
            "status": "unreachable",
            "rule_id": "LMWV-INFRA-FILIGREE-UNREACHABLE",
            "endpoint": "http://127.0.0.1:9/api/v1/scan-results",
            "emitted_attempted": 12,
            "error": "error sending request: connection refused",
        });
        assert_eq!(
            emit_status_marker(&blob).as_deref(),
            Some("emit:unreachable (error sending request: connection refused)")
        );
    }

    #[test]
    fn emit_marker_surfaces_partial_intake() {
        // A POST that landed but whose findings were rejected/coerced (intake
        // warnings) is a PARTIAL emit — the bare `status:"emitted"` would read as
        // full success. Surface it as partial with the warning count.
        let blob = serde_json::json!({
            "status": "emitted",
            "emitted": 5,
            "findings_created": 3,
            "warnings": ["coerced severity for f1", "unknown scan_run_id for f2"],
        });
        assert_eq!(
            emit_status_marker(&blob).as_deref(),
            Some("emit:partial (2 intake warnings)")
        );
    }

    #[test]
    fn emit_marker_surfaces_pre_wire_skip() {
        // A skip that is NOT the clean filtered no-op (a flush/read/open failure
        // swallowed before the wire — an emit that never even reached the network,
        // the same family as the dead-for-weeks seam) must surface.
        let blob = serde_json::json!({"status": "skipped", "reason": "flush_failed"});
        assert_eq!(
            emit_status_marker(&blob).as_deref(),
            Some("emit:skipped (flush_failed)")
        );
    }

    #[test]
    fn emit_marker_surfaces_unrecognized_status_as_drift() {
        // An unknown/absent status string is itself a contract drift — never read
        // as clean.
        let blob = serde_json::json!({"status": "bizarro"});
        assert_eq!(
            emit_status_marker(&blob).as_deref(),
            Some("emit:degraded (unrecognized status bizarro)")
        );
    }

    #[test]
    fn combined_emit_marker_primary_wins_then_postrun() {
        // Both degraded → the during-run (primary) marker is the one surfaced.
        assert_eq!(
            combined_emit_marker(
                Some("emit:unreachable (a)".to_owned()),
                Some("emit:unreachable (b)".to_owned())
            )
            .as_deref(),
            Some("emit:unreachable (a)")
        );
        // Only the post-commit emit degraded → it surfaces.
        assert_eq!(
            combined_emit_marker(None, Some("emit:unreachable (b)".to_owned())).as_deref(),
            Some("emit:unreachable (b)")
        );
        // Both clean → no marker.
        assert_eq!(combined_emit_marker(None, None), None);
    }

    // --- emit_reason_carrier: the TYPED weft-reason carrier (G3 / PDR-0023) ---
    //
    // The structured-output sibling of `emit_status_marker`: the degraded shapes
    // must land a TYPED `{reason_class, cause, fix}` a consumer switches on (not a
    // stdout substring), and the clean/disabled shapes must omit cause/fix. The
    // reason_class strings are constrained to the canonical 11-class vocab.

    #[test]
    fn emit_reason_disabled_when_integration_off() {
        // A `Null` blob = integration off / emit_findings=false → `disabled`
        // (clean-family: no cause/fix), NOT `clean` — the two stay distinguishable.
        let r = emit_reason_carrier(&serde_json::Value::Null);
        assert_eq!(r.reason_class, "disabled");
        assert_eq!(r.cause, None);
        assert_eq!(r.fix, None);
    }

    #[test]
    fn emit_reason_clean_for_true_negative_emit() {
        // A real POST with zero intake warnings is the earned true-negative →
        // `clean`, cause/fix omitted (and so absent from the JSON).
        let blob = serde_json::json!({
            "status": "emitted",
            "emitted": 0,
            "warnings": [],
        });
        let r = emit_reason_carrier(&blob);
        assert_eq!(r.reason_class, "clean");
        assert_eq!(r.cause, None);
        assert_eq!(r.fix, None);
        // A clean carrier serializes WITHOUT cause/fix keys (skip_serializing_if).
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json, serde_json::json!({ "reason_class": "clean" }));
    }

    #[test]
    fn emit_reason_clean_for_filtered_no_op_skip() {
        // The Phase-8c "nothing emittable remained" skip is a clean true-negative.
        let blob = serde_json::json!({
            "status": "skipped",
            "reason": "no_postrun_findings_with_path",
        });
        assert_eq!(emit_reason_carrier(&blob).reason_class, "clean");
    }

    #[test]
    fn emit_reason_degraded_emits_typed_carrier_not_a_substring() {
        // THE G3 STRIKE: an unrecognized (drift) emit status must surface a TYPED
        // {reason_class, cause, fix} carrier a consumer can switch on — not a
        // grepped stdout substring. `reason_class` is the canonical `error` class
        // (the loud catch-all for contract drift); cause + a recruiting fix are
        // both present and machine-readable.
        let blob = serde_json::json!({ "status": "bizarro" });
        let r = emit_reason_carrier(&blob);
        assert_eq!(r.reason_class, "error");
        assert_eq!(
            r.cause.as_deref(),
            Some("unrecognized Filigree emit status bizarro")
        );
        // FIX is mandatory on every non-clean class (contract): present + non-empty.
        let fix = r.fix.as_deref().expect("non-clean carrier must carry a fix");
        assert!(!fix.is_empty());
        // And it serializes as a typed object, NOT a bare string a consumer would
        // have to substring-match.
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.is_object());
        assert_eq!(json["reason_class"], "error");
        assert_eq!(json["cause"], "unrecognized Filigree emit status bizarro");
        assert!(json["fix"].is_string());
    }

    #[test]
    fn emit_reason_unreachable_carries_cause_and_fix() {
        // POST never landed → canonical `unreachable`, with the transport error in
        // the cause and a recruiting retry in the fix.
        let blob = serde_json::json!({
            "status": "unreachable",
            "error": "error sending request: connection refused",
        });
        let r = emit_reason_carrier(&blob);
        assert_eq!(r.reason_class, "unreachable");
        assert!(
            r.cause
                .as_deref()
                .is_some_and(|c| c.contains("connection refused"))
        );
        assert!(r.fix.is_some());
    }

    #[test]
    fn emit_reason_partial_for_intake_warnings() {
        // A landed POST whose findings were rejected/coerced → `partial` (qualified
        // trust): the bare `emitted` would read as full success.
        let blob = serde_json::json!({
            "status": "emitted",
            "emitted": 5,
            "warnings": ["coerced severity for f1", "unknown scan_run_id for f2"],
        });
        let r = emit_reason_carrier(&blob);
        assert_eq!(r.reason_class, "partial");
        assert!(r.cause.as_deref().is_some_and(|c| c.contains("2 intake")));
        assert!(r.fix.is_some());
    }

    #[test]
    fn emit_reason_pre_wire_skip_is_error_not_unreachable() {
        // A pre-POST flush/read/open skip is an INTERNAL pre-wire failure (our side
        // of the socket, the wire was never reached) → canonical `error`, the loud
        // catch-all — justified over `unreachable` (which means the POST itself
        // failed on the network).
        let blob = serde_json::json!({ "status": "skipped", "reason": "flush_failed" });
        let r = emit_reason_carrier(&blob);
        assert_eq!(r.reason_class, "error");
        assert!(r.cause.as_deref().is_some_and(|c| c.contains("flush_failed")));
        assert!(r.fix.is_some());
    }

    #[test]
    fn emit_reason_carrier_stays_in_lockstep_with_substring_marker() {
        // The typed carrier and the human substring marker read the SAME blob and
        // must never disagree on clean-vs-degraded: a marker `None` ⇔ a clean/
        // disabled class; a `Some(_)` marker ⇔ a non-clean class.
        let cases = [
            serde_json::Value::Null,
            serde_json::json!({ "status": "emitted", "warnings": [] }),
            serde_json::json!({ "status": "emitted", "warnings": ["w"] }),
            serde_json::json!({ "status": "unreachable", "error": "x" }),
            serde_json::json!({ "status": "skipped", "reason": "no_postrun_findings_with_path" }),
            serde_json::json!({ "status": "skipped", "reason": "flush_failed" }),
            serde_json::json!({ "status": "bizarro" }),
        ];
        for blob in cases {
            let marker_clean = emit_status_marker(&blob).is_none();
            let class = emit_reason_carrier(&blob).reason_class;
            let carrier_clean = class == "clean" || class == "disabled";
            assert_eq!(
                marker_clean, carrier_clean,
                "marker/carrier disagree on clean-ness for blob {blob}"
            );
        }
    }

    #[test]
    fn combined_emit_reason_primary_wins_then_postrun() {
        let unreachable = EmitReason::degraded("unreachable", "a", "retry");
        let error = EmitReason::degraded("error", "b", "report");
        // Both degraded → the during-run (primary) carrier surfaces.
        assert_eq!(
            combined_emit_reason(unreachable.clone(), Some(error.clone())).reason_class,
            "unreachable"
        );
        // Primary clean, post-run degraded → the post-run carrier surfaces (a
        // degraded Phase-8c emit is never masked by a clean during-run one).
        assert_eq!(
            combined_emit_reason(EmitReason::clean("clean"), Some(error.clone())).reason_class,
            "error"
        );
        // Both clean → clean.
        assert_eq!(
            combined_emit_reason(EmitReason::clean("clean"), None).reason_class,
            "clean"
        );
        // Primary disabled, post-run clean → disabled is preserved (the during-run
        // posture), not flattened to clean.
        assert_eq!(
            combined_emit_reason(EmitReason::clean("disabled"), Some(EmitReason::clean("clean")))
                .reason_class,
            "disabled"
        );
    }

    #[test]
    fn progress_reporter_is_noop_without_a_path() {
        // No progress file → no panics, no writes; the normal CLI path.
        let reporter = ProgressReporter::new(None, "run-x".to_owned());
        reporter.set_total(10);
        reporter.phase("analyzing", Some("python"), Some("a.py"));
        reporter.file_started("python", "a.py");
        reporter.file_completed();
    }

    #[test]
    fn progress_reporter_writes_phase_and_counters_with_heartbeat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runs").join("run-1.progress.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let reporter = ProgressReporter::new(Some(path.clone()), "run-1".to_owned());

        reporter.set_total(3);
        reporter.file_started("python", "src/a.py");
        reporter.file_completed();
        reporter.file_started("python", "src/b.py");

        let snapshot: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("progress file")).unwrap();
        assert_eq!(snapshot["run_id"], "run-1");
        assert_eq!(snapshot["phase"], "analyzing");
        assert_eq!(snapshot["current_plugin"], "python");
        assert_eq!(snapshot["current_file"], "src/b.py");
        assert_eq!(snapshot["processed_files"], 1);
        assert_eq!(snapshot["total_files"], 3);
        assert!(
            snapshot["heartbeat_at"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "heartbeat_at must be a non-empty timestamp"
        );

        // A later phase write overwrites with the new phase (last-write-wins).
        reporter.phase("clustering", None, None);
        let snapshot: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("progress file")).unwrap();
        assert_eq!(snapshot["phase"], "clustering");
        assert!(snapshot["current_plugin"].is_null());
    }

    #[test]
    fn progress_reporter_refreshes_heartbeat_for_in_flight_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runs").join("run-1.progress.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let reporter = ProgressReporter::new(Some(path.clone()), "run-1".to_owned());

        reporter.file_started("python", "src/slow.py");
        let before: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("progress file")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let guard = reporter.file_heartbeat_guard_with_interval(
            "python".to_owned(),
            "src/slow.py".to_owned(),
            std::time::Duration::from_millis(10),
        );
        std::thread::sleep(std::time::Duration::from_millis(35));
        drop(guard);

        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("progress file")).unwrap();
        assert_eq!(after["phase"], "analyzing");
        assert_eq!(after["current_plugin"], "python");
        assert_eq!(after["current_file"], "src/slow.py");
        assert_ne!(before["heartbeat_at"], after["heartbeat_at"]);
    }

    #[test]
    fn subsystem_entity_id_rejects_invalid_hash_segment() {
        let err = subsystem_entity_id("bad:hash").expect_err("colon must be rejected");

        assert!(
            err.to_string()
                .contains("canonical_qualified_name contains reserved ':' separator"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn source_walk_honours_root_gitignore() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        fs::write(root.join(".gitignore"), "ignored/\n*.generated.py\n").expect("gitignore");
        fs::write(root.join("kept.py"), "print('kept')\n").expect("kept source");
        fs::write(root.join("skip.generated.py"), "print('ignored pattern')\n")
            .expect("ignored source");
        fs::create_dir(root.join("ignored")).expect("ignored dir");
        fs::write(root.join("ignored").join("hidden.py"), "print('hidden')\n")
            .expect("ignored dir source");

        let wanted = BTreeSet::from(["py".to_owned()]);
        let SourceWalkResult {
            mut files,
            skipped_errors,
        } = collect_source_files(root, &wanted);
        assert!(skipped_errors.is_empty());
        files.sort();
        let relative = files
            .into_iter()
            .map(|path| {
                path.strip_prefix(root)
                    .expect("under temp root")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();

        assert_eq!(relative, vec!["kept.py"]);
    }

    #[test]
    fn source_walk_returns_errors_instead_of_only_logging_them() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let missing_root = tempdir.path().join("missing");
        let wanted = BTreeSet::from(["py".to_owned()]);

        let result = collect_source_files(&missing_root, &wanted);

        assert!(result.files.is_empty());
        assert!(
            !result.skipped_errors.is_empty(),
            "missing root must be carried as a skipped walk error"
        );
    }

    #[test]
    fn source_walk_finding_record_is_project_anchored_with_samples() {
        let rec = source_walk_finding_record(
            Path::new("/tmp/project"),
            2,
            &["permission denied".to_owned()],
            1,
            "core:project:project",
            "run-1",
            "2026-06-04T00:00:00.000Z",
        );

        assert_eq!(rec.rule_id, SOURCE_WALK_SKIPPED_RULE_ID);
        assert_eq!(rec.severity, "WARN");
        assert_eq!(rec.entity_id, "core:project:project");
        let evidence: serde_json::Value =
            serde_json::from_str(&rec.evidence_json).expect("evidence json");
        assert_eq!(evidence["skipped_entries"], 2);
        assert_eq!(evidence["error_samples"][0], "permission denied");
        assert_eq!(evidence["errors_omitted"], 1);
    }

    #[test]
    fn filter_import_edges_prefers_absolute_from_import_submodule_when_local() {
        let entities = vec![
            module_record("python:module:pkg"),
            module_record("python:module:pkg.service"),
        ];
        let mut edges = vec![from_import_edge(
            "python:module:consumer",
            "python:module:pkg",
            "service",
        )];

        let skipped = filter_external_import_edges(&entities, &python_kind_roles(), &mut edges);

        assert_eq!(skipped, 0);
        assert_eq!(edges[0].1.to_id, "python:module:pkg.service");
    }

    #[test]
    fn filter_import_edges_keeps_parent_for_absolute_from_import_reexport() {
        let entities = vec![module_record("python:module:pkg")];
        let mut edges = vec![from_import_edge(
            "python:module:consumer",
            "python:module:pkg",
            "helper",
        )];

        let skipped = filter_external_import_edges(&entities, &python_kind_roles(), &mut edges);

        assert_eq!(skipped, 0);
        assert_eq!(edges[0].1.to_id, "python:module:pkg");
    }

    #[test]
    fn filter_import_edges_accepts_namespace_package_submodule() {
        let entities = vec![module_record("python:module:pkg.service")];
        let mut edges = vec![from_import_edge(
            "python:module:consumer",
            "python:module:pkg",
            "service",
        )];

        let skipped = filter_external_import_edges(&entities, &python_kind_roles(), &mut edges);

        assert_eq!(skipped, 0);
        assert_eq!(edges[0].1.to_id, "python:module:pkg.service");
    }

    #[test]
    fn filter_import_edges_counts_only_truly_external_imports() {
        let entities = vec![module_record("python:module:consumer")];
        let mut edges = vec![from_import_edge(
            "python:module:consumer",
            "python:module:external",
            "service",
        )];

        let skipped = filter_external_import_edges(&entities, &python_kind_roles(), &mut edges);

        assert_eq!(skipped, 1);
        assert!(edges.is_empty());
    }

    /// A minimal anchored resolving edge (`implements`) for the seen-set gate
    /// tests: both endpoints supplied so membership is what decides readiness.
    fn implements_edge_record(from_id: &str, to_id: &str) -> (String, EdgeRecord) {
        (
            format!("implements {from_id} -> {to_id}"),
            EdgeRecord {
                kind: "implements".to_owned(),
                from_id: from_id.to_owned(),
                to_id: to_id.to_owned(),
                confidence: loomweave_core::EdgeConfidence::Resolved,
                properties_json: None,
                source_file_id: Some(from_id.to_owned()),
                source_byte_start: Some(0),
                source_byte_end: Some(4),
            },
        )
    }

    /// The seen-set gate's happy path: an edge whose BOTH endpoints are already
    /// stored drains ready; one whose `to_id` is not yet seen stays pending (it
    /// may become ready when a later batch stores the target — D3 reconciliation).
    #[test]
    fn drain_ready_plugin_edges_releases_only_fully_seen_endpoints() {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert("rust:impl:c.Foo.impl[Tr]".to_owned());
        seen.insert("rust:trait:c.Tr".to_owned());

        let mut pending = vec![
            // ready: both endpoints stored.
            implements_edge_record("rust:impl:c.Foo.impl[Tr]", "rust:trait:c.Tr"),
            // not ready: target not yet seen.
            implements_edge_record("rust:impl:c.Foo.impl[Other]", "rust:trait:c.Other"),
        ];

        let ready = drain_ready_plugin_edges(&mut pending, &seen);

        assert_eq!(ready.len(), 1, "only the fully-seen edge drains ready");
        assert_eq!(ready[0].1.to_id, "rust:trait:c.Tr");
        assert_eq!(pending.len(), 1, "the unseen-target edge stays pending");
        assert_eq!(pending[0].1.to_id, "rust:trait:c.Other");
    }

    /// The seen-set gate's drop-and-count: anything STILL pending at end-of-plugin
    /// has an endpoint the host never stored (D1 external / D2 gitignored-superset
    /// / D3 stale). Flushing it would FK-HardFail the run, so the gate drops the
    /// whole buffer and returns the count — never silently lost, never flushed.
    #[test]
    fn drop_unready_plugin_edges_drops_and_counts_never_flushes() {
        let mut pending = vec![
            implements_edge_record("rust:impl:c.Foo.impl[Ext]", "rust:trait:ext.Trait"),
            implements_edge_record("rust:impl:c.Bar.impl[Gone]", "rust:trait:c.Gone"),
        ];

        let dropped = drop_unready_plugin_edges(&mut pending);

        assert_eq!(dropped, 2, "both never-seen edges are counted as dropped");
        assert!(
            pending.is_empty(),
            "the pending buffer is fully drained (dropped, not retained)"
        );
    }

    /// Like [`implements_edge_record`] but kind-parameterized, for the gate
    /// generality pins below.
    fn anchored_edge_record(kind: &str, from_id: &str, to_id: &str) -> (String, EdgeRecord) {
        (
            format!("{kind} {from_id} -> {to_id}"),
            EdgeRecord {
                kind: kind.to_owned(),
                from_id: from_id.to_owned(),
                to_id: to_id.to_owned(),
                confidence: loomweave_core::EdgeConfidence::Resolved,
                properties_json: None,
                source_file_id: Some(from_id.to_owned()),
                source_byte_start: Some(0),
                source_byte_end: Some(4),
            },
        )
    }

    /// PIN test (D8, Phase 2): the seen-set gate is kind-GENERIC — it switches
    /// on endpoint membership only, never on `kind` — so the new `derives` and
    /// `references` edge kinds ride it unchanged. This cannot go red-first (no
    /// code change accompanies it); it pins the generality so a future
    /// kind-aware refactor of the gate cannot silently strand the new kinds.
    #[test]
    fn drain_ready_plugin_edges_is_kind_generic_for_derives_and_references() {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert("rust:struct:c.Foo".to_owned());
        seen.insert("rust:trait:c.Tr".to_owned());
        seen.insert("rust:function:c.make".to_owned());
        seen.insert("rust:const:c.MAX".to_owned());

        let mut pending = vec![
            // ready: both endpoints stored.
            anchored_edge_record("derives", "rust:struct:c.Foo", "rust:trait:c.Tr"),
            anchored_edge_record("references", "rust:function:c.make", "rust:const:c.MAX"),
            // not ready: target never stored (gitignored-superset / stale — D2/D3).
            anchored_edge_record("derives", "rust:struct:c.Foo", "rust:trait:c.Hidden"),
            anchored_edge_record("references", "rust:function:c.make", "rust:struct:c.Gone"),
        ];

        let ready = drain_ready_plugin_edges(&mut pending, &seen);

        assert_eq!(
            ready.len(),
            2,
            "both fully-seen edges drain ready regardless of kind"
        );
        assert_eq!(ready[0].1.kind, "derives");
        assert_eq!(ready[0].1.to_id, "rust:trait:c.Tr");
        assert_eq!(ready[1].1.kind, "references");
        assert_eq!(ready[1].1.to_id, "rust:const:c.MAX");
        assert_eq!(
            pending.len(),
            2,
            "both unseen-target edges stay pending regardless of kind"
        );
        assert_eq!(pending[0].1.to_id, "rust:trait:c.Hidden");
        assert_eq!(pending[1].1.to_id, "rust:struct:c.Gone");
    }

    /// PIN test (D8, Phase 2): end-of-plugin reconciliation drop-counts pending
    /// `derives`/`references` edges exactly like `implements` — kind never
    /// enters the decision. See the drain pin above for why no red state exists.
    #[test]
    fn drop_unready_plugin_edges_counts_derives_and_references() {
        let mut pending = vec![
            anchored_edge_record("derives", "rust:struct:c.Foo", "rust:trait:c.Hidden"),
            anchored_edge_record("references", "rust:function:c.make", "rust:struct:c.Gone"),
        ];

        let dropped = drop_unready_plugin_edges(&mut pending);

        assert_eq!(
            dropped, 2,
            "both never-seen Phase-2 edges are counted as dropped"
        );
        assert!(
            pending.is_empty(),
            "the pending buffer is fully drained (dropped, not retained)"
        );
    }

    #[test]
    fn filter_import_edges_retains_non_module_in_project_target() {
        // clarion-d1e3dc67dc: the Rust resolver targets `use` paths at functions /
        // structs / consts / traits, not just file-scope modules. Such an in-project,
        // non-module target was dropped as "external" by the module-only gate even
        // though its entity was stored. It must now survive.
        let entities = vec![
            module_record("rust:module:demo.sub"),
            record_of_kind("rust:function:demo.sub.helper", "function"),
        ];
        let mut edges = vec![resolved_import_edge(
            "rust:module:demo",
            "rust:function:demo.sub.helper",
        )];

        // python_kind_roles has file_scope = {module}, so `function` is correctly
        // NOT file-scope — exercising exactly the non-module-target path.
        let skipped = filter_external_import_edges(&entities, &python_kind_roles(), &mut edges);

        assert_eq!(skipped, 0, "an in-project function target is not external");
        assert_eq!(
            edges.len(),
            1,
            "the import edge to a function must be retained"
        );
        assert_eq!(edges[0].1.to_id, "rust:function:demo.sub.helper");
    }

    #[test]
    fn filter_import_edges_still_drops_unstored_target() {
        // The broadened fallback must not become a blanket keep: a target that is
        // NOT a stored in-project entity (any kind) is still external and dropped.
        let entities = vec![module_record("rust:module:demo")];
        let mut edges = vec![resolved_import_edge(
            "rust:module:demo",
            "rust:function:other_crate.thing",
        )];

        let skipped = filter_external_import_edges(&entities, &python_kind_roles(), &mut edges);

        assert_eq!(skipped, 1, "an unstored target is still external");
        assert!(edges.is_empty());
    }

    #[test]
    fn subsystem_display_name_uses_common_module_prefix() {
        let (name, short_name) = subsystem_display_name(
            &[
                "python:module:pkg.auth.login".to_owned(),
                "python:module:pkg.auth.policy".to_owned(),
                "python:module:pkg.auth.token".to_owned(),
            ],
            "abc123def456",
        );

        assert_eq!(name, "pkg.auth");
        assert_eq!(short_name, "pkg.auth");
    }

    #[test]
    fn subsystem_display_name_falls_back_to_hash_without_common_prefix() {
        let (name, short_name) = subsystem_display_name(
            &[
                "python:module:auth.login".to_owned(),
                "python:module:billing.invoice".to_owned(),
            ],
            "abc123def456",
        );

        assert_eq!(name, "Subsystem abc123def456");
        assert_eq!(short_name, "abc123def456");
    }

    #[test]
    fn phase3_stats_distinguishes_configured_and_used_algorithm() {
        let config = AnalyzeConfig::default().analysis.clustering;

        let stats = phase3_stats_json(
            &config,
            ClusterAlgorithm::WeightedComponents,
            "completed",
            None,
            3,
            2,
            2,
            Some(0.5),
            2,
            3,
            false,
            std::time::Instant::now(),
        );

        assert_eq!(stats["configured_algorithm"].as_str(), Some("leiden"));
        assert_eq!(stats["algorithm"].as_str(), Some("weighted_components"));
    }

    fn module_record(id: &str) -> (String, EntityRecord) {
        (
            id.to_owned(),
            EntityRecord {
                id: id.to_owned(),
                plugin_id: "python".to_owned(),
                kind: "module".to_owned(),
                name: id.trim_start_matches("python:module:").to_owned(),
                short_name: id.rsplit('.').next().unwrap_or(id).to_owned(),
                parent_id: None,
                source_file_id: None,
                source_file_path: None,
                source_byte_start: None,
                source_byte_end: None,
                source_line_start: None,
                source_line_end: None,
                properties_json: "{}".to_owned(),
                tags: Vec::new(),
                content_hash: None,
                summary_json: None,
                wardline_json: None,
                first_seen_commit: None,
                last_seen_commit: None,
                created_at: "2026-05-17T00:00:00.000Z".to_owned(),
                updated_at: "2026-05-17T00:00:00.000Z".to_owned(),
            },
        )
    }

    fn record_of_kind(id: &str, kind: &str) -> (String, EntityRecord) {
        let (_, mut record) = module_record(id);
        record.kind = kind.to_owned();
        (id.to_owned(), record)
    }

    fn entity_with_properties(id: &str, properties_json: &str) -> EntityRecord {
        EntityRecord {
            id: id.to_owned(),
            plugin_id: "python".to_owned(),
            kind: "module".to_owned(),
            name: id.trim_start_matches("python:module:").to_owned(),
            short_name: id.rsplit('.').next().unwrap_or(id).to_owned(),
            parent_id: None,
            source_file_id: None,
            source_file_path: Some("pkg/broken.py".to_owned()),
            source_byte_start: None,
            source_byte_end: None,
            source_line_start: None,
            source_line_end: None,
            properties_json: properties_json.to_owned(),
            tags: Vec::new(),
            content_hash: None,
            summary_json: None,
            wardline_json: None,
            first_seen_commit: None,
            last_seen_commit: None,
            created_at: "2026-06-02T00:00:00.000Z".to_owned(),
            updated_at: "2026-06-02T00:00:00.000Z".to_owned(),
        }
    }

    fn python_kind_roles() -> PluginKindRoles {
        PluginKindRoles {
            file_scope: BTreeSet::from(["module".to_owned()]),
            callable: BTreeSet::from(["function".to_owned()]),
            syntax_degraded_module: BTreeSet::from(["module".to_owned()]),
        }
    }

    #[test]
    fn syntax_error_finding_minted_for_degraded_entity() {
        let record = entity_with_properties(
            "python:module:pkg.broken",
            r#"{"parse_status":"syntax_error"}"#,
        );
        let finding = syntax_error_finding(
            &record,
            &python_kind_roles(),
            "run-1",
            "2026-06-02T00:00:00.000Z",
        )
        .expect("syntax_error entity must mint a finding");
        assert_eq!(finding.rule_id, SYNTAX_ERROR_RULE_ID);
        assert_eq!(finding.entity_id, "python:module:pkg.broken");
        assert_eq!(finding.kind, "defect");
        assert_eq!(finding.severity, "WARN");
        assert_eq!(finding.tool, "loomweave");
        // Deterministic, content-keyed id keeps InsertFinding idempotent across runs.
        assert_eq!(
            finding.id,
            "core:finding:syntax-error:python:module:pkg.broken"
        );
    }

    #[test]
    fn syntax_error_finding_absent_for_clean_or_unflagged_entities() {
        let ok = entity_with_properties("python:module:pkg.ok", r#"{"parse_status":"ok"}"#);
        assert!(syntax_error_finding(&ok, &python_kind_roles(), "run-1", "t").is_none());
        let plain = entity_with_properties("python:module:pkg.plain", "{}");
        assert!(syntax_error_finding(&plain, &python_kind_roles(), "run-1", "t").is_none());
        let malformed = entity_with_properties("python:module:pkg.bad", "not json");
        assert!(syntax_error_finding(&malformed, &python_kind_roles(), "run-1", "t").is_none());
    }

    #[test]
    fn syntax_error_finding_requires_manifest_role() {
        let record = entity_with_properties(
            "fixture:widget:pkg.broken",
            r#"{"parse_status":"syntax_error"}"#,
        );

        assert!(syntax_error_finding(&record, &PluginKindRoles::default(), "run-1", "t").is_none());
    }

    #[test]
    fn entity_deleted_finding_is_fact_anchored_to_the_deleted_entity() {
        let finding = entity_deleted_finding(
            "python:function:pkg.gone",
            "run-1",
            "2026-06-02T00:00:00.000Z",
        );
        assert_eq!(finding.rule_id, ENTITY_DELETED_RULE_ID);
        assert_eq!(finding.kind, "fact");
        assert_eq!(finding.severity, "INFO");
        // Anchors to the deleted entity's own (never-pruned) row.
        assert_eq!(finding.entity_id, "python:function:pkg.gone");
        // Deterministic, content-keyed id keeps InsertFinding idempotent across runs.
        assert_eq!(
            finding.id,
            "core:finding:entity-deleted:python:function:pkg.gone"
        );
    }

    #[test]
    fn extract_wardline_tier_matches_facet_scalar_semantics() {
        // String / number / bool tier fields all stringify (parity with the MCP
        // `facet_matches` read path); a missing or non-scalar tier yields None.
        assert_eq!(
            extract_wardline_tier(r#"{"tier":"public"}"#).as_deref(),
            Some("public")
        );
        assert_eq!(extract_wardline_tier(r#"{"tier":2}"#).as_deref(), Some("2"));
        assert_eq!(
            extract_wardline_tier(r#"{"tier":true}"#).as_deref(),
            Some("true")
        );
        assert_eq!(extract_wardline_tier(r#"{"group":"x"}"#), None);
        assert_eq!(extract_wardline_tier(r#"{"tier":["a"]}"#), None);
        assert_eq!(extract_wardline_tier("not json"), None);
    }

    #[test]
    fn tier_mixing_finding_records_distribution_and_anchors_to_subsystem() {
        let members = vec![
            ("python:function:a".to_owned(), "public".to_owned()),
            ("python:function:b".to_owned(), "internal".to_owned()),
        ];
        let finding = tier_mixing_finding("core:subsystem:abc", &members, "run-1", "t");
        assert_eq!(finding.rule_id, TIER_MIXING_RULE_ID);
        assert_eq!(finding.kind, "fact");
        assert_eq!(finding.severity, "WARN");
        assert_eq!(finding.entity_id, "core:subsystem:abc");
        assert_eq!(finding.id, "core:finding:tier-mixing:core:subsystem:abc");
        let evidence: serde_json::Value = serde_json::from_str(&finding.evidence_json).unwrap();
        assert_eq!(evidence["tier_distribution"]["public"], 1);
        assert_eq!(evidence["tier_distribution"]["internal"], 1);
    }

    #[test]
    fn tier_unanimous_finding_is_info_and_records_member_count() {
        let members = vec![
            ("python:function:a".to_owned(), "trusted".to_owned()),
            ("python:function:b".to_owned(), "trusted".to_owned()),
        ];
        let finding =
            tier_unanimous_finding("core:subsystem:abc", "trusted", &members, "run-1", "t");
        assert_eq!(finding.rule_id, TIER_UNANIMOUS_RULE_ID);
        assert_eq!(finding.severity, "INFO");
        assert_eq!(finding.entity_id, "core:subsystem:abc");
        let evidence: serde_json::Value = serde_json::from_str(&finding.evidence_json).unwrap();
        assert_eq!(evidence["tier"], "trusted");
        assert_eq!(evidence["member_count"], 2);
    }

    #[test]
    fn guidance_orphan_finding_anchors_to_sheet_and_carries_deleted_target() {
        let finding = guidance_orphan_finding(
            "core:guidance:g1",
            "python:function:pkg.gone",
            "run-1",
            "2026-06-02T00:00:00.000Z",
        );
        assert_eq!(finding.rule_id, GUIDANCE_ORPHAN_RULE_ID);
        assert_eq!(finding.kind, "fact");
        assert_eq!(finding.severity, "WARN");
        // Anchors to the guidance sheet; the deleted target is a related entity.
        assert_eq!(finding.entity_id, "core:guidance:g1");
        let related: serde_json::Value =
            serde_json::from_str(&finding.related_entities_json).unwrap();
        assert_eq!(related, serde_json::json!(["python:function:pkg.gone"]));
        assert_eq!(
            finding.id,
            "core:finding:guidance-orphan:core:guidance:g1:python:function:pkg.gone"
        );
    }

    #[test]
    fn project_anchor_id_uses_dir_name() {
        assert_eq!(
            project_anchor_id(std::path::Path::new("/tmp/demo")),
            "core:project:demo"
        );
    }

    #[test]
    fn infra_severity_escalates_crash_and_kill() {
        assert_eq!(infra_severity(INFRA_CRASH_RULE_ID), "ERROR");
        assert_eq!(infra_severity("LMWV-INFRA-PLUGIN-OOM-KILLED"), "ERROR");
        assert_eq!(infra_severity("LMWV-INFRA-PLUGIN-MALFORMED-ENTITY"), "WARN");
    }

    #[test]
    fn host_finding_to_record_anchors_and_carries_subcode() {
        let hf = HostFinding {
            subcode: "LMWV-INFRA-PLUGIN-MALFORMED-ENTITY".to_owned(),
            message: "entity failed to deserialise".to_owned(),
            metadata: std::collections::BTreeMap::new(),
        };
        let rec = host_finding_to_record(&hf, "python", "core:project:demo", "run-1", "t");
        assert_eq!(rec.rule_id, "LMWV-INFRA-PLUGIN-MALFORMED-ENTITY");
        assert_eq!(rec.entity_id, "core:project:demo");
        assert_eq!(rec.severity, "WARN");
        assert_eq!(rec.kind, "defect");
        assert_eq!(rec.tool, "loomweave");
        assert!(rec.evidence_json.contains("python"));
    }

    #[test]
    fn host_finding_anchor_id_uses_file_anchor_metadata_when_present() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let source = tempdir.path().join("pkg").join("demo.py");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "def demo():\n    pass\n").unwrap();
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert(
            "anchor_file_path".to_owned(),
            source.to_string_lossy().into_owned(),
        );
        let hf = HostFinding {
            subcode: "LMWV-PY-PYRIGHT-RESTART".to_owned(),
            message: "pyright restarted".to_owned(),
            metadata,
        };

        let anchor = host_finding_anchor_id(&hf, tempdir.path(), "core:project:demo");

        assert_eq!(anchor, "core:file:pkg/demo.py");
    }

    #[cfg(unix)]
    #[test]
    fn verified_plugin_dispatch_path_rejects_out_of_tree_symlink() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let project = tempdir.path().join("project");
        let outside = tempdir.path().join("outside.py");
        fs::create_dir(&project).expect("create project");
        fs::write(&outside, "def secret():\n    pass\n").expect("write outside");
        let link = project.join("demo.py");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink outside");

        let err = verified_plugin_dispatch_path(&project, &link)
            .expect_err("out-of-tree symlink must not be dispatched");

        assert!(
            format!("{err:#}").contains("safe-open"),
            "dispatch failure should identify safe-open boundary: {err:#}"
        );
    }

    #[test]
    fn plugin_watchdog_arm_disarm_and_severity() {
        let wd = PluginWatchdog::new();
        assert!(
            wd.timed_out_phase().is_none(),
            "fresh watchdog has not fired"
        );
        wd.arm(std::time::Duration::from_secs(60), WatchdogPhase::File);
        assert!(wd.deadline.lock().unwrap().is_some(), "arm sets a deadline");
        assert!(
            wd.expire_if_due().is_none(),
            "an unexpired deadline does not fire"
        );
        wd.disarm();
        assert!(
            wd.deadline.lock().unwrap().is_none(),
            "disarm clears the deadline"
        );
        // A timeout is an ERROR-severity loss of work; a shutdown timeout is
        // exit etiquette only (ADR-050 D7) — WARN, never ERROR.
        assert_eq!(infra_severity(PLUGIN_TIMEOUT_RULE_ID), "ERROR");
        assert_eq!(infra_severity(PLUGIN_SHUTDOWN_TIMEOUT_RULE_ID), "WARN");
    }

    #[test]
    fn plugin_watchdog_expiry_records_armed_phase() {
        let wd = PluginWatchdog::new();
        wd.arm(std::time::Duration::ZERO, WatchdogPhase::Handshake);
        assert_eq!(
            wd.expire_if_due(),
            Some(WatchdogPhase::Handshake),
            "an expired deadline fires with the armed phase"
        );
        assert_eq!(
            wd.timed_out_phase(),
            Some(WatchdogPhase::Handshake),
            "the fired phase is recorded"
        );
        // Kill-at-most-once per arm: expiry disarms.
        assert!(
            wd.expire_if_due().is_none(),
            "a fired deadline does not fire twice"
        );
        // A later expiry still triggers a kill, but the FIRST recorded phase
        // wins — it names the phase that broke the run.
        wd.arm(std::time::Duration::ZERO, WatchdogPhase::Shutdown);
        assert_eq!(wd.expire_if_due(), Some(WatchdogPhase::Shutdown));
        assert_eq!(
            wd.timed_out_phase(),
            Some(WatchdogPhase::Handshake),
            "first recorded phase wins"
        );
    }

    #[test]
    fn crash_finding_record_is_error_and_anchored_to_project() {
        let rec = crash_finding_record("python", "boom", "core:project:demo", "run-1", "t");
        assert_eq!(rec.rule_id, INFRA_CRASH_RULE_ID);
        assert_eq!(rec.severity, "ERROR");
        assert_eq!(rec.entity_id, "core:project:demo");
        assert!(rec.message.contains("boom"));
    }

    fn resolved_import_edge(from_id: &str, to_id: &str) -> (String, EdgeRecord) {
        (
            format!("imports {from_id} -> {to_id}"),
            EdgeRecord {
                kind: "imports".to_owned(),
                from_id: from_id.to_owned(),
                to_id: to_id.to_owned(),
                confidence: loomweave_core::EdgeConfidence::Resolved,
                properties_json: Some(
                    serde_json::json!({ "import_style": "import", "level": 0 }).to_string(),
                ),
                source_file_id: Some(from_id.to_owned()),
                source_byte_start: Some(0),
                source_byte_end: Some(10),
            },
        )
    }

    fn from_import_edge(from_id: &str, to_id: &str, imported_name: &str) -> (String, EdgeRecord) {
        (
            format!("imports {from_id} -> {to_id}"),
            EdgeRecord {
                kind: "imports".to_owned(),
                from_id: from_id.to_owned(),
                to_id: to_id.to_owned(),
                confidence: loomweave_core::EdgeConfidence::Resolved,
                properties_json: Some(
                    serde_json::json!({
                        "imported_name": imported_name,
                        "import_style": "from_import",
                        "level": 0
                    })
                    .to_string(),
                ),
                source_file_id: Some(from_id.to_owned()),
                source_byte_start: Some(0),
                source_byte_end: Some(10),
            },
        )
    }

    // ── handle_plugin_task_join_result ────────────────────────────────────────
    //
    // Covers the JoinError-bypass regression filed as clarion-cf17e4e779. The
    // production path is a `spawn_blocking`-wrapped call to
    // `run_plugin_blocking`; if the task panics, `spawn_blocking` yields
    // `Err(JoinError)`. Earlier code `?`-propagated that error out of `run()`,
    // bypassing the CommitRun/FailRun block and leaving `runs.status =
    // 'running'`. The helper converts the panic into a crash reason string so
    // the existing crash-recording path handles it.

    #[test]
    fn handle_task_passes_through_ok_ok() {
        let br = BatchResult {
            findings: Vec::new(),
        };
        let out = handle_plugin_task_join_result(Ok(Ok(br)), "python");
        assert!(out.is_ok());
    }

    #[test]
    fn handle_task_passes_through_ok_err() {
        let out = handle_plugin_task_join_result(
            Ok(Err(PluginRunError::new("spawn failed: ENOENT"))),
            "python",
        );
        match out {
            Err(e) => {
                assert_eq!(e.reason, "spawn failed: ENOENT");
                assert!(e.findings.is_empty());
            }
            Ok(_) => panic!("expected Err pass-through"),
        }
    }

    #[tokio::test]
    async fn handle_task_real_spawn_blocking_panic_is_converted() {
        // Drive a real JoinError through the helper by panicking inside
        // spawn_blocking. Asserting on the structure-of-Err (not the exact
        // message) so this stays robust across tokio's internal formatting.
        let join_result = tokio::task::spawn_blocking(|| -> Result<BatchResult, PluginRunError> {
            panic!("simulated plugin-task panic");
        })
        .await;
        assert!(
            join_result.is_err(),
            "spawn_blocking should surface panic as JoinError"
        );
        let out = handle_plugin_task_join_result(join_result, "python");
        match out {
            Err(e) => {
                assert!(
                    e.reason.contains("plugin task for python panicked"),
                    "reason must identify plugin_id; got: {}",
                    e.reason
                );
                assert!(e.findings.is_empty());
            }
            Ok(_) => panic!("JoinError must convert to Err, not Ok"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn reap_timeout_kills_stubborn_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleeping child");
        let mut findings = Vec::new();
        let start = std::time::Instant::now();

        reap_and_classify_exit_with_timeout(
            &mut child,
            "stubborn",
            &mut findings,
            std::time::Duration::from_millis(50),
            false,
        );

        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "bounded reap should not wait for the child sleep"
        );
        assert!(
            child.try_wait().expect("query child status").is_some(),
            "timed-out child should be killed and reaped"
        );
        assert!(
            findings.is_empty(),
            "timeout kill should not be misclassified as an OOM finding: {findings:?}"
        );
    }

    /// ADR-050: SIGABRT (signal 6) — the stack-overflow / explicit-abort
    /// signature — is classified distinctly as `LMWV-INFRA-PLUGIN-ABORTED`,
    /// never as an OOM kill, and is ERROR severity.
    #[test]
    #[cfg(unix)]
    fn reap_classifies_sigabrt_as_aborted_finding() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "kill -ABRT $$"])
            .spawn()
            .expect("spawn self-aborting child");
        let mut findings = Vec::new();

        reap_and_classify_exit_with_timeout(
            &mut child,
            "abrt",
            &mut findings,
            std::time::Duration::from_secs(5),
            false,
        );

        assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
        assert_eq!(findings[0].subcode, FINDING_PLUGIN_ABORTED);
        assert_eq!(
            findings[0].metadata.get("signal").map(String::as_str),
            Some("6")
        );
        assert_eq!(infra_severity(FINDING_PLUGIN_ABORTED), "ERROR");
    }

    /// ADR-050 `timed_out` gate: when the lifecycle watchdog killed the child,
    /// the resulting SIGKILL must not be double-reported as an OOM event —
    /// but without suppression the SIGKILL classification is unchanged.
    #[test]
    #[cfg(unix)]
    fn reap_suppresses_oom_classification_for_watchdog_kill() {
        for (suppress, expected_oom) in [(true, 0_usize), (false, 1_usize)] {
            let mut child = std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .expect("spawn sleeping child");
            child.kill().expect("kill child (watchdog stand-in)");
            let mut findings = Vec::new();

            reap_and_classify_exit_with_timeout(
                &mut child,
                "killed",
                &mut findings,
                std::time::Duration::from_secs(5),
                suppress,
            );

            let oom_count = findings
                .iter()
                .filter(|f| f.subcode == "LMWV-INFRA-PLUGIN-OOM-KILLED")
                .count();
            assert_eq!(
                oom_count, expected_oom,
                "suppress={suppress} must yield {expected_oom} OOM findings: {findings:?}"
            );
        }
    }

    #[test]
    fn map_entity_persists_source_metadata_and_content_hash() {
        let tempdir = tempfile::tempdir().unwrap();
        let source_path = tempdir.path().join("demo.py");
        std::fs::write(&source_path, "def hello():\n    return 'hé'\n\n").unwrap();
        let source_range = serde_json::json!({
            "source_range": {
                "start_line": 1,
                "start_col": 0,
                "end_line": 2,
                "end_col": 15
            }
        });
        let entity = AcceptedEntity {
            id: "python:function:demo.hello".parse().unwrap(),
            kind: "function".to_owned(),
            qualified_name: "demo.hello".to_owned(),
            source_file_path: source_path.display().to_string(),
            raw: loomweave_core::plugin::host::RawEntity {
                id: "python:function:demo.hello".to_owned(),
                kind: "function".to_owned(),
                qualified_name: "demo.hello".to_owned(),
                source: loomweave_core::plugin::host::RawSource {
                    file_path: source_path.display().to_string(),
                    extra: source_range.as_object().unwrap().clone(),
                },
                parent_id: Some("python:module:demo".to_owned()),
                signature: Some(
                    serde_json::json!({"v": 1, "params": ["x: int"], "return_ann": "bool"}),
                ),
                tags: vec![
                    "entry-point".to_owned(),
                    "entry-point".to_owned(),
                    " ".to_owned(),
                ],
                extra: serde_json::Map::new(),
            },
        };

        let record = map_entity_to_record(
            tempdir.path(),
            &entity,
            "python",
            Some("core:file:demo.py".to_owned()),
            &python_kind_roles(),
        );

        assert_eq!(
            record.source_file_path.as_deref(),
            Some(source_path.to_str().unwrap())
        );
        assert_eq!(record.source_file_id.as_deref(), Some("core:file:demo.py"));
        assert_eq!(record.source_line_start, Some(1));
        assert_eq!(record.source_line_end, Some(2));
        assert_eq!(record.tags, vec!["entry-point".to_owned()]);
        let expected_hash = blake3::hash("def hello():\n    return 'hé'".as_bytes())
            .to_hex()
            .to_string();
        assert_eq!(record.content_hash.as_deref(), Some(expected_hash.as_str()));
    }

    #[test]
    fn map_unresolved_call_sites_groups_by_current_caller_hash() {
        let caller = EntityRecord {
            id: "python:function:demo.caller".to_owned(),
            plugin_id: "python".to_owned(),
            kind: "function".to_owned(),
            name: "demo.caller".to_owned(),
            short_name: "caller".to_owned(),
            parent_id: Some("python:module:demo".to_owned()),
            source_file_id: Some("core:file:demo.py".to_owned()),
            source_file_path: Some("demo.py".to_owned()),
            source_byte_start: None,
            source_byte_end: None,
            source_line_start: Some(1),
            source_line_end: Some(3),
            properties_json: "{}".to_owned(),
            tags: Vec::new(),
            content_hash: Some("hash-python:function:demo.caller".to_owned()),
            summary_json: None,
            wardline_json: None,
            first_seen_commit: None,
            last_seen_commit: None,
            created_at: "2026-05-17T00:00:00.000Z".to_owned(),
            updated_at: "2026-05-17T00:00:00.000Z".to_owned(),
        };
        let module = {
            let mut record = caller.clone();
            record.id = "python:module:demo".to_owned();
            record.kind = "module".to_owned();
            record.content_hash = Some("hash-python:module:demo".to_owned());
            record
        };
        let entities = vec![
            ("python:module:demo".to_owned(), module),
            ("python:function:demo.caller".to_owned(), caller.clone()),
        ];
        let stats = loomweave_core::AnalyzeFileStats {
            unresolved_call_sites_total: 1,
            unresolved_call_sites: vec![loomweave_core::UnresolvedCallSite {
                caller_entity_id: caller.id.clone(),
                site_ordinal: 0,
                source_byte_start: 14,
                source_byte_end: 24,
                callee_expr: "dynamic_target".to_owned(),
            }],
            ..loomweave_core::AnalyzeFileStats::default()
        };

        let mapped = map_unresolved_call_sites_for_file(
            &stats,
            &entities,
            &python_kind_roles(),
            "2026-05-17T00:00:00.000Z",
        )
        .unwrap();

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].caller_entity_id, "python:function:demo.caller");
        assert_eq!(
            mapped[0].caller_content_hash,
            "hash-python:function:demo.caller"
        );
        assert_eq!(mapped[0].sites.len(), 1);
        assert_eq!(
            mapped[0].sites[0].source_file_id.as_deref(),
            Some("core:file:demo.py")
        );
        assert_eq!(mapped[0].sites[0].callee_expr, "dynamic_target");
        assert_eq!(
            mapped[0].sites[0].site_key,
            unresolved_call_site_key("python:function:demo.caller", 14, 24, "dynamic_target")
        );
    }

    #[test]
    fn map_unresolved_call_sites_clears_hash_present_callers_when_authoritative_empty() {
        let caller = EntityRecord {
            id: "python:function:demo.caller".to_owned(),
            plugin_id: "python".to_owned(),
            kind: "function".to_owned(),
            name: "demo.caller".to_owned(),
            short_name: "caller".to_owned(),
            parent_id: Some("python:module:demo".to_owned()),
            source_file_id: Some("core:file:demo.py".to_owned()),
            source_file_path: Some("demo.py".to_owned()),
            source_byte_start: None,
            source_byte_end: None,
            source_line_start: Some(1),
            source_line_end: Some(3),
            properties_json: "{}".to_owned(),
            tags: Vec::new(),
            content_hash: Some("hash-python:function:demo.caller".to_owned()),
            summary_json: None,
            wardline_json: None,
            first_seen_commit: None,
            last_seen_commit: None,
            created_at: "2026-05-17T00:00:00.000Z".to_owned(),
            updated_at: "2026-05-17T00:00:00.000Z".to_owned(),
        };
        let entities = vec![("python:function:demo.caller".to_owned(), caller)];
        let stats = loomweave_core::AnalyzeFileStats::default();

        let mapped = map_unresolved_call_sites_for_file(
            &stats,
            &entities,
            &python_kind_roles(),
            "2026-05-17T00:00:00.000Z",
        )
        .unwrap();

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].caller_entity_id, "python:function:demo.caller");
        assert!(mapped[0].sites.is_empty());
    }

    #[tokio::test]
    async fn semantic_embedding_population_skips_fresh_sidecar_rows() {
        use std::sync::Arc;

        use loomweave_core::{EmbeddingProvider, EmbeddingRecording, RecordingEmbeddingProvider};
        use loomweave_federation::config::SemanticSearchConfig;
        use loomweave_storage::{EmbeddingKey, EmbeddingStore, pragma, schema};

        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(loomweave_core::store::store_dir(project.path())).unwrap();
        let db_path = loomweave_core::store::db_path(project.path());
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO entities \
                (id, plugin_id, kind, name, short_name, properties, content_hash, created_at, updated_at) \
             VALUES \
                ('python:function:demo.fresh', 'python', 'function', 'demo.fresh', 'fresh', \
                 '{\"docstring\":\"already embedded\"}', 'hash-fresh', 't', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        let store = EmbeddingStore::open_in_store_dir(project.path()).unwrap();
        store
            .upsert(
                &EmbeddingKey {
                    entity_id: "python:function:demo.fresh".to_owned(),
                    content_hash: "hash-fresh".to_owned(),
                    model_id: "test-model".to_owned(),
                },
                &[1.0, 0.0],
                0.0,
                2,
                "t",
            )
            .unwrap();
        drop(store);

        let provider = Arc::new(RecordingEmbeddingProvider::from_recordings(
            "test-model",
            2,
            Vec::<EmbeddingRecording>::new(),
        ));
        let stats = populate_semantic_embeddings(
            project.path(),
            &db_path,
            &SemanticSearchConfig {
                enabled: true,
                model_id: "test-model".to_owned(),
                dimensions: 2,
                ..SemanticSearchConfig::default()
            },
            provider.clone() as Arc<dyn EmbeddingProvider>,
        )
        .await
        .unwrap();

        assert_eq!(stats.considered, 1);
        assert_eq!(stats.skipped_fresh, 1);
        assert_eq!(stats.embedded, 0);
        assert!(
            provider.invocations().is_empty(),
            "fresh sidecar rows must not be re-embedded"
        );
    }

    #[tokio::test]
    async fn semantic_embedding_population_skips_briefing_blocked_entities() {
        use std::sync::Arc;

        use loomweave_core::{EmbeddingProvider, EmbeddingRecording, RecordingEmbeddingProvider};
        use loomweave_federation::config::SemanticSearchConfig;
        use loomweave_storage::{pragma, schema};

        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(loomweave_core::store::store_dir(project.path())).unwrap();
        let db_path = loomweave_core::store::db_path(project.path());
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO entities \
                (id, plugin_id, kind, name, short_name, properties, content_hash, created_at, updated_at) \
             VALUES \
                ('python:function:demo.secret', 'python', 'function', 'demo.secret', 'secret', \
                 '{\"docstring\":\"SECRET_TOKEN=abc123\", \"briefing_blocked\":\"secret_present\"}', \
                 'hash-secret', 't', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        let provider = Arc::new(RecordingEmbeddingProvider::from_recordings(
            "test-model",
            2,
            Vec::<EmbeddingRecording>::new(),
        ));
        let stats = populate_semantic_embeddings(
            project.path(),
            &db_path,
            &SemanticSearchConfig {
                enabled: true,
                model_id: "test-model".to_owned(),
                dimensions: 2,
                ..SemanticSearchConfig::default()
            },
            provider.clone() as Arc<dyn EmbeddingProvider>,
        )
        .await
        .unwrap();

        assert_eq!(stats.considered, 0);
        assert_eq!(stats.embedded, 0);
        assert!(
            provider.invocations().is_empty(),
            "briefing-blocked docstrings must not be sent to the embedding provider"
        );
    }
}
