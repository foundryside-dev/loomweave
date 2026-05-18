//! `clarion analyze` — discover plugins, walk the source tree, persist entities.
//!
//! WP2 Task 8 replaces the Sprint-1 stub with real plugin orchestration:
//! - Discover plugins via L9 `$PATH` convention (Task 5).
//! - For each plugin: spawn, handshake, walk the source tree, call
//!   `analyze_file` for every matching file, persist via writer-actor.
//! - Pattern A buffering: collect entities in the blocking task, flush
//!   `InsertEntity` commands from async context after the blocking task returns.
//! - On unrecoverable error (cap, escape, spawn, transport) → `FailRun`.
//! - Zero successful plugins discovered → `SkippedNoPlugins` (existing path).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::{DirEntry, WalkBuilder};
use rusqlite::Connection;
use time::{OffsetDateTime, macros::format_description};
use uuid::Uuid;

use clarion_core::{
    AcceptedEdge, AcceptedEntity, AnalyzeFileOutcome, CrashLoopBreaker, CrashLoopState,
    DiscoveredPlugin, FINDING_DISABLED_CRASH_LOOP, HostError, HostFinding, UnresolvedCallSite,
    discover,
};
use clarion_storage::{
    DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, UnresolvedCallSiteRecord, Writer,
    commands::{EdgeRecord, EntityRecord, FindingRecord, RunStatus, WriterCmd},
    module_dependency_edges,
};

use crate::clustering::{ClusterConfig, ModuleEdge, ModuleGraph, cluster_hash, cluster_modules};
use crate::config::{AnalyzeConfig, ClusteringConfig};
use crate::stats::P95Accumulator;

const WEAK_MODULARITY_RULE_ID: &str = "CLA-FACT-CLUSTERING-WEAK-MODULARITY";

// ── Public entry point ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub(crate) struct AnalyzeOptions {
    pub(crate) config_path: Option<PathBuf>,
}

/// Run the analyze command against `project_path`.
///
/// # Errors
///
/// Returns an error if the target directory does not exist, has no `.clarion/`
/// directory, or if the writer actor fails to start or process commands.
pub async fn run(project_path: PathBuf) -> Result<()> {
    run_with_options(project_path, AnalyzeOptions::default()).await
}

/// Run the analyze command against `project_path` with resolved CLI options.
///
/// # Errors
///
/// Returns an error if the target directory does not exist, has no `.clarion/`
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
    let clarion_dir = project_root.join(".clarion");
    if !clarion_dir.exists() {
        bail!(
            "{} has no .clarion/ directory. Run `clarion install` first.",
            project_root.display()
        );
    }
    let db_path = clarion_dir.join("clarion.db");
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
    let run_id = Uuid::new_v4().to_string();
    let started_at = iso8601_now();

    writer
        .send_wait(|ack| WriterCmd::BeginRun {
            run_id: run_id.clone(),
            config_json: analyze_config_json.clone(),
            started_at: started_at.clone(),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("BeginRun")?;

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
            // hides the failure from `clarion analyze && do_next` chains
            // and breaks CI gating that reads `$?`. The run row in the DB
            // is already marked `failed` above.
            bail!("analyze run {run_id} failed — {reason}");
        }

        tracing::warn!(run_id = %run_id, "no plugins discovered");
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
    let source_files = collect_source_files(&project_root, &wanted_extensions);
    tracing::info!(file_count = source_files.len(), "source tree walk complete");

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
    let mut unresolved_reference_sites_total: u64 = 0;
    let mut pyright_latency = P95Accumulator::default();
    let mut pyright_index_parse_latency = P95Accumulator::default();
    let mut extractor_parse_latency = P95Accumulator::default();
    let mut run_outcome: RunOutcome = RunOutcome::Completed;
    let mut breaker = CrashLoopBreaker::default();
    let mut crash_reasons: Vec<String> = Vec::new();

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

        tracing::info!(
            plugin_id = %plugin_id,
            file_count = plugin_files.len(),
            "processing plugin"
        );

        // Run the blocking plugin work on the tokio threadpool.
        // Pattern A: collect all entities into memory, return to async side.
        let manifest = plugin.manifest.clone();
        let project_root_clone = project_root.clone();
        let pid_clone = plugin_id.clone();
        let exec_clone = plugin.executable.clone();
        let files_clone = plugin_files.clone();

        // A JoinError here means the blocking task panicked (OOM, stack
        // overflow, internal unwrap, abort — anything that unwinds past the
        // top of `run_plugin_blocking`). Earlier revisions `?`-propagated
        // the JoinError out of `run()`, which bypassed the
        // CommitRun/FailRun block and left `runs.status = 'running'`
        // permanently. Treat the panic as a crash reason: it flows into the
        // existing crash-recording path below, ticks the crash-loop breaker,
        // and resolves the run via SoftFailed → CommitRun(Failed) with exit 1.
        let spawn_result: Result<BatchResult, PluginRunError> = handle_plugin_task_join_result(
            tokio::task::spawn_blocking(move || {
                run_plugin_blocking(
                    manifest,
                    &project_root_clone,
                    &pid_clone,
                    &exec_clone,
                    &files_clone,
                )
            })
            .await,
            &plugin_id,
        );

        match spawn_result {
            Err(plugin_error) => {
                log_plugin_findings(&plugin_id, &plugin_error.findings);
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
            Ok(BatchResult {
                entities,
                edges,
                unresolved_call_sites,
                stats,
                findings,
            }) => {
                unresolved_call_sites_total += stats.unresolved_call_sites_total;
                reference_sites_total += stats.reference_sites_total;
                references_resolved_total += stats.references_resolved_total;
                references_skipped_external_total += stats.references_skipped_external_total;
                references_skipped_cap_total += stats.references_skipped_cap_total;
                imports_skipped_external_total += stats.imports_skipped_external_total;
                unresolved_reference_sites_total += stats.unresolved_reference_sites_total;
                pyright_latency.record_many(stats.pyright_query_latency_ms);
                pyright_index_parse_latency.record_many(stats.pyright_index_parse_latency_ms);
                extractor_parse_latency.record_many(stats.extractor_parse_latency_ms);

                // Log findings individually (Tier B persistence is future
                // work). Logging only the count leaves operators guessing
                // whether the plugin tripped an ontology check, emitted
                // malformed JSON, or hit a path-jail violation.
                log_plugin_findings(&plugin_id, &findings);

                // Persist entities + edges via writer-actor (async side).
                //
                // A writer-actor error here (per-kind contract violation,
                // unique-key constraint, disk full) must NOT short-circuit
                // `run()` via `?` — that would bypass the CommitRun/FailRun
                // block below and leave `runs.status = 'running'` permanently.
                // Convert to a terminal `RunOutcome::HardFailed` so FailRun
                // marks the run. Entities are inserted before edges so the
                // edge FK references resolve at insert time (B.3 §5).
                let entity_count = entities.len() as u64;
                let edge_count = edges.len() as u64;
                let mut insert_err: Option<anyhow::Error> = None;
                for (id_str, record) in entities {
                    let res = writer
                        .send_wait(|ack| WriterCmd::InsertEntity {
                            entity: Box::new(record),
                            ack,
                        })
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))
                        .with_context(|| format!("InsertEntity for {id_str}"));
                    if let Err(e) = res {
                        insert_err = Some(e);
                        break;
                    }
                }
                if insert_err.is_none() {
                    for pending in unresolved_call_sites {
                        let caller_id = pending.caller_entity_id.clone();
                        let res = writer
                            .send_wait(|ack| WriterCmd::ReplaceUnresolvedCallSitesForCaller {
                                caller_entity_id: pending.caller_entity_id,
                                caller_content_hash: pending.caller_content_hash,
                                sites: pending.sites,
                                ack,
                            })
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))
                            .with_context(|| {
                                format!("ReplaceUnresolvedCallSitesForCaller for {caller_id}")
                            });
                        if let Err(e) = res {
                            insert_err = Some(e);
                            break;
                        }
                    }
                }
                if insert_err.is_none() {
                    for (descr, record) in edges {
                        let res = writer
                            .send_wait(|ack| WriterCmd::InsertEdge {
                                edge: Box::new(record),
                                ack,
                            })
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))
                            .with_context(|| format!("InsertEdge {descr}"));
                        if let Err(e) = res {
                            insert_err = Some(e);
                            break;
                        }
                    }
                }
                if let Some(e) = insert_err {
                    tracing::error!(
                        plugin_id = %plugin_id,
                        error = %e,
                        "writer-actor rejected insert; failing run"
                    );
                    run_outcome = RunOutcome::HardFailed {
                        reason: format!("{e:#}"),
                    };
                    break 'plugins;
                }
                total_entity_count += entity_count;
                total_edge_count += edge_count;
                tracing::info!(
                    plugin_id = %plugin_id,
                    entity_count, edge_count,
                    "plugin complete",
                );
            }
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

    let phase3_output = if matches!(run_outcome, RunOutcome::HardFailed { .. }) {
        Phase3Output::not_run()
    } else {
        match run_phase3_clustering(&writer, &db_path, &run_id, &analyze_config).await {
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
            let stats_json = serde_json::json!({
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
                "unresolved_reference_sites_total": unresolved_reference_sites_total,
                "pyright_query_latency_p95_ms": pyright_query_latency_p95_ms,
                "pyright_index_parse_latency_p95_ms": pyright_index_parse_latency_p95_ms,
                "extractor_parse_latency_p95_ms": extractor_parse_latency_p95_ms,
                "clustering": phase3_output.clustering_stats.clone(),
            })
            .to_string();
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
        }
        RunOutcome::SoftFailed { reason } => {
            // Commit entities inserted by healthy plugins AND mark the run
            // failed, atomically (writer folds the UPDATE into the open tx).
            // The stats JSON carries both fields so operators can see what
            // was persisted alongside the failure reason.
            let stats_json = serde_json::json!({
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
                "unresolved_reference_sites_total": unresolved_reference_sites_total,
                "pyright_query_latency_p95_ms": pyright_query_latency_p95_ms,
                "pyright_index_parse_latency_p95_ms": pyright_index_parse_latency_p95_ms,
                "extractor_parse_latency_p95_ms": extractor_parse_latency_p95_ms,
                "clustering": phase3_output.clustering_stats.clone(),
                "failure_reason": reason,
            })
            .to_string();
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

    println!(
        "analyze complete: run {run_id} completed \
         ({total_entity_count} entities, {total_edge_count} edges)"
    );
    Ok(())
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
        writer
            .send_wait(|ack| WriterCmd::InsertEntity {
                entity: Box::new(EntityRecord {
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
                    content_hash: None,
                    summary_json: None,
                    wardline_json: None,
                    first_seen_commit: None,
                    last_seen_commit: None,
                    created_at: now.clone(),
                    updated_at: now,
                }),
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
                        confidence: clarion_core::EdgeConfidence::Resolved,
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
    Ok(clarion_core::entity_id::entity_id("core", "subsystem", cluster_hash)?.to_string())
}

fn subsystem_display_name(member_ids: &[String], cluster_hash: &str) -> (String, String) {
    common_module_prefix(member_ids).map_or_else(
        || (format!("Subsystem {cluster_hash}"), cluster_hash.to_owned()),
        |prefix| (prefix.clone(), prefix),
    )
}

fn common_module_prefix(member_ids: &[String]) -> Option<String> {
    let mut names = member_ids.iter().filter_map(|id| module_qualified_name(id));
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

fn module_qualified_name(entity_id: &str) -> Option<&str> {
    let mut parts = entity_id.splitn(3, ':');
    let _plugin_id = parts.next()?;
    let kind = parts.next()?;
    let qualified = parts.next()?;
    if kind == "module" && !qualified.is_empty() {
        Some(qualified)
    } else {
        None
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
    let finding_id = format!("core:finding:{run_id}:weak-modularity");
    let related_entities_json = serde_json::to_string(&subsystem_ids)
        .context("serialize weak modularity related_entities")?;
    writer
        .send_wait(|ack| WriterCmd::InsertFinding {
            finding: Box::new(FindingRecord {
                id: finding_id.clone(),
                tool: "clarion".to_owned(),
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
    algorithm: crate::clustering::ClusterAlgorithm,
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
    /// `(entity_id_string, record)` pairs for every accepted entity.
    entities: Vec<(String, EntityRecord)>,
    /// `(descriptor, record)` pairs for every accepted edge — descriptor is
    /// `"(kind from_id -> to_id)"` for diagnostic messages on insert failure.
    edges: Vec<(String, EdgeRecord)>,
    /// Per-caller unresolved site replacements derived from authoritative
    /// plugin stats for this batch.
    unresolved_call_sites: Vec<PendingUnresolvedCallSites>,
    /// Per-file observability stats reported by the plugin and folded by the CLI.
    stats: BatchStats,
    /// Findings accumulated by the host during the session.
    findings: Vec<clarion_core::HostFinding>,
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

type Collected = (
    Vec<(String, EntityRecord)>,
    Vec<(String, EdgeRecord)>,
    Vec<PendingUnresolvedCallSites>,
    BatchStats,
);

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
fn run_plugin_blocking(
    manifest: clarion_core::Manifest,
    project_root: &Path,
    plugin_id: &str,
    executable: &Path,
    files: &[PathBuf],
) -> Result<BatchResult, PluginRunError> {
    use clarion_core::PluginHost;

    let (mut host, mut child) =
        PluginHost::spawn(manifest, project_root, executable).map_err(|e| match e {
            HostError::Spawn(msg) => {
                PluginRunError::new(format!("failed to spawn plugin {plugin_id}: {msg}"))
            }
            HostError::Handshake(ref me) => {
                PluginRunError::new(format!("plugin {plugin_id} refused handshake: {me}"))
            }
            other => {
                PluginRunError::new(format!("plugin {plugin_id} spawn/handshake error: {other}"))
            }
        })?;

    let work_result: Result<Collected, String> = (|| {
        let mut collected_entities: Vec<(String, EntityRecord)> = Vec::new();
        let mut collected_edges: Vec<(String, EdgeRecord)> = Vec::new();
        let mut collected_unresolved_call_sites: Vec<PendingUnresolvedCallSites> = Vec::new();
        let mut collected_stats = BatchStats::default();
        for file in files {
            let AnalyzeFileOutcome {
                entities,
                edges,
                stats,
            } = host
                .analyze_file(file)
                .map_err(|e| classify_host_error(plugin_id, e))?;
            collected_stats.unresolved_call_sites_total += stats.unresolved_call_sites_total;
            collected_stats.reference_sites_total += stats.reference_sites_total;
            collected_stats.references_resolved_total += stats.references_resolved_total;
            collected_stats.references_skipped_external_total +=
                stats.references_skipped_external_total;
            collected_stats.references_skipped_cap_total += stats.references_skipped_cap_total;
            collected_stats.unresolved_reference_sites_total +=
                stats.unresolved_reference_sites_total;
            collected_stats
                .pyright_query_latency_ms
                .extend(stats.pyright_query_latency_ms.iter().copied());
            collected_stats
                .pyright_index_parse_latency_ms
                .extend(stats.pyright_index_parse_latency_ms.iter().copied());
            if stats.extractor_parse_latency_ms > 0 {
                collected_stats
                    .extractor_parse_latency_ms
                    .push(stats.extractor_parse_latency_ms);
            }
            let source_file_id = entities
                .iter()
                .find(|entity| entity.kind == "module")
                .map(|entity| entity.id.to_string());
            let mut file_entities: Vec<(String, EntityRecord)> = Vec::new();
            for entity in &entities {
                let id_str = entity.id.to_string();
                let record = map_entity_to_record(entity, plugin_id, source_file_id.clone());
                file_entities.push((id_str.clone(), record.clone()));
                collected_entities.push((id_str, record));
            }
            let unresolved_for_file =
                map_unresolved_call_sites_for_file(&stats, &file_entities, &iso8601_now())
                    .map_err(|e| {
                        format!(
                            "plugin {plugin_id} emitted invalid unresolved call-site stats: {e:#}"
                        )
                    })?;
            collected_unresolved_call_sites.extend(unresolved_for_file);
            for edge in edges {
                let descr = format!(
                    "({kind} {from} -> {to})",
                    kind = edge.kind,
                    from = edge.from_id,
                    to = edge.to_id,
                );
                let record = map_edge_to_record(edge);
                collected_edges.push((descr, record));
            }
        }
        collected_stats.imports_skipped_external_total +=
            filter_external_import_edges(&collected_entities, &mut collected_edges);
        Ok((
            collected_entities,
            collected_edges,
            collected_unresolved_call_sites,
            collected_stats,
        ))
    })();

    // Try a graceful shutdown on the happy path; on error, skip straight to
    // kill — the plugin's behaviour is already untrusted. `analyze_file`
    // already issues `shutdown`/`exit` before returning PathEscapeBreaker or
    // EntityCap errors, so calling `host.shutdown()` again there would write
    // to a closed pipe; that's why we only call it on Ok.
    if work_result.is_ok() {
        if let Err(e) = host.shutdown() {
            tracing::warn!(
                plugin_id = %plugin_id,
                error = %e,
                "best-effort host shutdown failed; falling back to kill()",
            );
            let _ = child.kill();
        }
    } else {
        let _ = child.kill();
    }

    let mut findings = host.take_findings();
    drop(host);

    // Reap unconditionally. `Child::Drop` does not wait on Unix.
    reap_and_classify_exit(&mut child, plugin_id, &mut findings);

    match work_result {
        Ok((entities, edges, unresolved_call_sites, stats)) => Ok(BatchResult {
            entities,
            edges,
            unresolved_call_sites,
            stats,
            findings,
        }),
        Err(reason) => Err(PluginRunError::with_findings(reason, findings)),
    }
}

/// Wait on the child, inspect its exit status, and append an OOM finding if
/// the signal is consistent with `RLIMIT_AS` enforcement (ADR-021 §2d).
///
/// Linux kernel behaviour on `RLIMIT_AS` violation varies: typical signatures
/// are SIGKILL (OOM-killer path) and SIGSEGV (map/allocation failure that the
/// plugin did not handle). Both are treated as likely memory-limit events.
/// Other signals or non-zero exit codes get a warn log but no finding — the
/// cause is ambiguous without more bookkeeping.
fn reap_and_classify_exit(
    child: &mut std::process::Child,
    plugin_id: &str,
    findings: &mut Vec<HostFinding>,
) {
    reap_and_classify_exit_with_timeout(child, plugin_id, findings, PLUGIN_REAP_TIMEOUT);
}

const PLUGIN_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const PLUGIN_REAP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

fn reap_and_classify_exit_with_timeout(
    child: &mut std::process::Child,
    plugin_id: &str,
    findings: &mut Vec<HostFinding>,
    timeout: std::time::Duration,
) {
    match wait_child_with_timeout(child, timeout) {
        Ok(Some(status)) => classify_child_exit_status(status, plugin_id, findings),
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
            // SIGKILL (9) and SIGSEGV (11) are the observed signatures
            // of an RLIMIT_AS kill in Sprint-1 testing.
            if signal == 9 || signal == 11 {
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

fn filter_external_import_edges(
    entities: &[(String, EntityRecord)],
    edges: &mut Vec<(String, EdgeRecord)>,
) -> u64 {
    let module_entity_ids: BTreeSet<&str> = entities
        .iter()
        .filter(|(_, record)| record.kind == "module")
        .map(|(id, _)| id.as_str())
        .collect();
    let before = edges.len();
    edges.retain_mut(|(_, edge)| {
        if edge.kind != "imports" {
            return true;
        }
        if let Some(local_submodule) =
            absolute_from_import_submodule_target(edge, &module_entity_ids)
        {
            edge.to_id = local_submodule;
            return true;
        }
        module_entity_ids.contains(edge.to_id.as_str())
    });
    u64::try_from(before - edges.len()).unwrap_or(u64::MAX)
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

/// Map an `AcceptedEntity` to an `EntityRecord` for the writer-actor.
fn map_entity_to_record(
    entity: &AcceptedEntity,
    plugin_id: &str,
    source_file_id: Option<String>,
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
        content_hash: content_hash_for_entity(entity, source_line_range),
        summary_json: None,
        wardline_json: None,
        first_seen_commit: None,
        last_seen_commit: None,
        created_at: now.clone(),
        updated_at: now,
    }
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

fn content_hash_for_entity(
    entity: &AcceptedEntity,
    source_line_range: Option<SourceLineRange>,
) -> Option<String> {
    if entity.kind == "module" {
        let bytes = fs::read(&entity.source_file_path).ok()?;
        return Some(blake3::hash(&bytes).to_hex().to_string());
    }

    let range = source_line_range?;
    let source = fs::read_to_string(&entity.source_file_path).ok()?;
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

/// Map an `AcceptedEdge` to an `EdgeRecord` for the writer-actor (B.3).
fn map_edge_to_record(edge: AcceptedEdge) -> EdgeRecord {
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
        source_file_id: edge.source_file_id,
        source_byte_start: edge.raw.source_byte_start,
        source_byte_end: edge.raw.source_byte_end,
    }
}

fn map_unresolved_call_sites_for_file(
    stats: &clarion_core::AnalyzeFileStats,
    entities: &[(String, EntityRecord)],
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
            if record.kind != "function" {
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
/// Sprint 1 conservative set: VCS directories, clarion's own state, and
/// common virtual-environment directories.
const SKIP_DIRS: &[&str] = &[
    ".clarion",
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
/// Uses the `ignore` crate so `.gitignore` / `.ignore` / global gitignore
/// policy filters the source set before plugin dispatch. Symlinks are skipped
/// (path-jail concerns for Sprint 1).
///
/// Per-entry I/O errors (a dirent we couldn't stat, a file whose
/// `file_type()` probe failed) are logged at `warn` level and counted.
/// When the walk completes with non-zero skips, a summary is logged so
/// the operator can see that the file list is incomplete — silently
/// dropping those entries would mask the same "incomplete analysis"
/// class that the WP1 `read_applied_versions` `.ok()` pattern did.
fn collect_source_files(root: &Path, wanted_extensions: &BTreeSet<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut skipped: u64 = 0;
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
                        out.push(path);
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "source walk: skipping unreadable or ignored-path-error entry",
                );
                skipped += 1;
            }
        }
    }

    if skipped > 0 {
        tracing::warn!(
            skipped = skipped,
            root = %root.display(),
            "source walk skipped {skipped} unreadable entr{suffix}; analysis is \
             incomplete for those paths",
            suffix = if skipped == 1 { "y" } else { "ies" },
        );
    }
    out
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
        let mut files = collect_source_files(root, &wanted);
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

        let skipped = filter_external_import_edges(&entities, &mut edges);

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

        let skipped = filter_external_import_edges(&entities, &mut edges);

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

        let skipped = filter_external_import_edges(&entities, &mut edges);

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

        let skipped = filter_external_import_edges(&entities, &mut edges);

        assert_eq!(skipped, 1);
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
            crate::clustering::ClusterAlgorithm::WeightedComponents,
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

    fn from_import_edge(from_id: &str, to_id: &str, imported_name: &str) -> (String, EdgeRecord) {
        (
            format!("imports {from_id} -> {to_id}"),
            EdgeRecord {
                kind: "imports".to_owned(),
                from_id: from_id.to_owned(),
                to_id: to_id.to_owned(),
                confidence: clarion_core::EdgeConfidence::Resolved,
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
            entities: Vec::new(),
            edges: Vec::new(),
            unresolved_call_sites: Vec::new(),
            stats: BatchStats::default(),
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
            raw: clarion_core::plugin::host::RawEntity {
                id: "python:function:demo.hello".to_owned(),
                kind: "function".to_owned(),
                qualified_name: "demo.hello".to_owned(),
                source: clarion_core::plugin::host::RawSource {
                    file_path: source_path.display().to_string(),
                    extra: source_range.as_object().unwrap().clone(),
                },
                parent_id: Some("python:module:demo".to_owned()),
                extra: serde_json::Map::new(),
            },
        };

        let record = map_entity_to_record(&entity, "python", Some("python:module:demo".to_owned()));

        assert_eq!(
            record.source_file_path.as_deref(),
            Some(source_path.to_str().unwrap())
        );
        assert_eq!(record.source_file_id.as_deref(), Some("python:module:demo"));
        assert_eq!(record.source_line_start, Some(1));
        assert_eq!(record.source_line_end, Some(2));
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
            source_file_id: Some("python:module:demo".to_owned()),
            source_file_path: Some("demo.py".to_owned()),
            source_byte_start: None,
            source_byte_end: None,
            source_line_start: Some(1),
            source_line_end: Some(3),
            properties_json: "{}".to_owned(),
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
        let stats = clarion_core::AnalyzeFileStats {
            unresolved_call_sites_total: 1,
            unresolved_call_sites: vec![clarion_core::UnresolvedCallSite {
                caller_entity_id: caller.id.clone(),
                site_ordinal: 0,
                source_byte_start: 14,
                source_byte_end: 24,
                callee_expr: "dynamic_target".to_owned(),
            }],
            ..clarion_core::AnalyzeFileStats::default()
        };

        let mapped =
            map_unresolved_call_sites_for_file(&stats, &entities, "2026-05-17T00:00:00.000Z")
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
            Some("python:module:demo")
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
            source_file_id: Some("python:module:demo".to_owned()),
            source_file_path: Some("demo.py".to_owned()),
            source_byte_start: None,
            source_byte_end: None,
            source_line_start: Some(1),
            source_line_end: Some(3),
            properties_json: "{}".to_owned(),
            content_hash: Some("hash-python:function:demo.caller".to_owned()),
            summary_json: None,
            wardline_json: None,
            first_seen_commit: None,
            last_seen_commit: None,
            created_at: "2026-05-17T00:00:00.000Z".to_owned(),
            updated_at: "2026-05-17T00:00:00.000Z".to_owned(),
        };
        let entities = vec![("python:function:demo.caller".to_owned(), caller)];
        let stats = clarion_core::AnalyzeFileStats::default();

        let mapped =
            map_unresolved_call_sites_for_file(&stats, &entities, "2026-05-17T00:00:00.000Z")
                .unwrap();

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].caller_entity_id, "python:function:demo.caller");
        assert!(mapped[0].sites.is_empty());
    }
}
