//! Analyze run lifecycle: `analyze_start`, `analyze_status`, `analyze_cancel`.
//!
//! Extracted from `lib.rs` (V11-ARCH-04). Methods attach to
//! [`crate::ServerState`] via an inherent `impl` block; `lib.rs` keeps the
//! shared free-function helpers, the tool catalogue, and the JSON-RPC dispatch.

use loomweave_core::McpErrorCode;
use serde_json::{Value, json};

use loomweave_storage::StorageError;

use crate::{
    ANALYZE_HEARTBEAT_STALE_SECS, CancelOutcome, LiveRun, ParamError, ServerState, elapsed_seconds,
    map_run_status, progress_observed, read_progress_snapshot, required_str,
    run_stats_is_cancelled, storage_retryable, success_envelope, tool_error_envelope,
};

impl ServerState {
    // Uniform async dispatch with the other tools; the body is sync (spawn +
    // registry insert), hence no await.
    #[allow(clippy::unused_async)]
    pub(crate) async fn tool_analyze_start(
        &self,
        _arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let program = match &self.analyze_program {
            Some(program) => program.clone(),
            None => match std::env::current_exe() {
                Ok(path) => path,
                Err(err) => {
                    return Ok(tool_error_envelope(
                        McpErrorCode::SpawnFailed,
                        &format!("cannot resolve the loomweave executable to launch analyze: {err}"),
                        false,
                    ));
                }
            },
        };

        let run_id = uuid::Uuid::new_v4().to_string();
        let runs_dir = self.project_root.join(".loomweave").join("runs");
        if let Err(err) = std::fs::create_dir_all(&runs_dir) {
            return Ok(tool_error_envelope(
                McpErrorCode::IoError,
                &format!("create runs directory {}: {err}", runs_dir.display()),
                false,
            ));
        }
        let progress_path = runs_dir.join(format!("{run_id}.progress.json"));
        let started_at = (self.clock)();

        let mut registry = self
            .analyze_runs
            .lock()
            .expect("analyze run registry mutex");
        // Evict finished handles (and reap their progress files) before
        // spawning, so a long-lived `serve` doing many analyses does not
        // accumulate dead entries / `runs/*.progress.json` files
        // (clarion-7e0c21558a).
        crate::analyze_runs::reap_terminal_runs(&mut registry);
        // Reject a concurrent run: a second `loomweave analyze` would fail to
        // acquire the project's cross-process lock anyway, so surface it as a
        // clear error rather than spawning a doomed child.
        let already_active = registry
            .values_mut()
            .any(|handle| !handle.cancelled && matches!(handle.child.try_wait(), Ok(None)));
        if already_active {
            return Ok(tool_error_envelope(
                McpErrorCode::AnalyzeAlreadyRunning,
                "an analyze run is already active for this project; cancel it or wait for it to finish",
                true,
            ));
        }

        let handle = match crate::analyze_runs::spawn_analyze(
            &program,
            &self.project_root,
            &run_id,
            &progress_path,
            started_at,
        ) {
            Ok(handle) => handle,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::SpawnFailed,
                    &format!("failed to spawn `loomweave analyze`: {err}"),
                    false,
                ));
            }
        };
        let pid = handle.child.id();
        registry.insert(run_id.clone(), handle);
        drop(registry);

        Ok(success_envelope(json!({
            "run_id": run_id,
            "status": "started",
            "pid": pid,
            "progress_file": progress_path.display().to_string(),
        })))
    }

    pub(crate) async fn tool_analyze_status(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let run_id = required_str(arguments, "run_id")?.to_owned();
        let now = (self.clock)();

        // Snapshot the live state under the lock; reap on exit.
        let live = {
            let mut registry = self
                .analyze_runs
                .lock()
                .expect("analyze run registry mutex");
            match registry.get_mut(&run_id) {
                Some(handle) => match handle.child.try_wait() {
                    Ok(None) => LiveRun::Alive {
                        started_at: handle.started_at.clone(),
                        progress_path: handle.progress_path.clone(),
                    },
                    Ok(Some(_)) | Err(_) => LiveRun::Exited {
                        started_at: handle.started_at.clone(),
                        cancelled: handle.cancelled,
                    },
                },
                None => LiveRun::Absent,
            }
        };

        match live {
            LiveRun::Alive {
                started_at,
                progress_path,
            } => {
                let elapsed = elapsed_seconds(&started_at, &now);
                let progress = read_progress_snapshot(&progress_path);
                let (status, heartbeat_at) = match &progress {
                    Some(snapshot) => (
                        "running",
                        snapshot.get("heartbeat_at").and_then(Value::as_str),
                    ),
                    // Spawned but no progress recorded yet (still in discovery /
                    // before the first write).
                    None => ("queued", None),
                };
                let observed = heartbeat_at
                    .is_some_and(|hb| progress_observed(hb, &now, ANALYZE_HEARTBEAT_STALE_SECS));
                Ok(success_envelope(json!({
                    "run_id": run_id,
                    "status": status,
                    "phase": progress.as_ref().and_then(|p| p.get("phase").cloned()),
                    "current_plugin": progress.as_ref().and_then(|p| p.get("current_plugin").cloned()),
                    "processed_files": progress.as_ref().and_then(|p| p.get("processed_files").cloned()),
                    "total_files": progress.as_ref().and_then(|p| p.get("total_files").cloned()),
                    "current_file": progress.as_ref().and_then(|p| p.get("current_file").cloned()),
                    "heartbeat_at": heartbeat_at,
                    "elapsed_seconds": elapsed,
                    "progress_observed": observed,
                })))
            }
            LiveRun::Exited {
                started_at,
                cancelled,
            } => {
                let row = self.read_run_row(&run_id).await;
                Ok(self.terminal_status_envelope(&run_id, cancelled, Some(&started_at), &now, row))
            }
            LiveRun::Absent => {
                // Not in the registry — may be a run from a prior session.
                let row = self.read_run_row(&run_id).await;
                match &row {
                    Ok(Some(_)) => {
                        Ok(self.terminal_status_envelope(&run_id, false, None, &now, row))
                    }
                    Ok(None) => Ok(tool_error_envelope(
                        McpErrorCode::RunNotFound,
                        &format!("no analyze run with id {run_id}"),
                        false,
                    )),
                    Err(err) => Ok(tool_error_envelope(
                        McpErrorCode::StorageError,
                        &err.to_string(),
                        storage_retryable(err),
                    )),
                }
            }
        }
    }

    pub(crate) async fn tool_analyze_cancel(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let run_id = required_str(arguments, "run_id")?.to_owned();
        let now = (self.clock)();

        let outcome = {
            let mut registry = self
                .analyze_runs
                .lock()
                .expect("analyze run registry mutex");
            match registry.get_mut(&run_id) {
                Some(handle) => match handle.child.try_wait() {
                    Ok(None) => {
                        crate::analyze_runs::kill_run(handle);
                        CancelOutcome::Cancelled
                    }
                    Ok(Some(_)) | Err(_) => CancelOutcome::AlreadyExited {
                        cancelled: handle.cancelled,
                    },
                },
                None => CancelOutcome::Absent,
            }
        };

        match outcome {
            CancelOutcome::Cancelled => {
                let db_path = self.project_root.join(".loomweave").join("loomweave.db");
                crate::analyze_runs::mark_run_cancelled_in_db(&db_path, &run_id, &now);
                Ok(success_envelope(json!({
                    "run_id": run_id,
                    "status": "cancelled",
                })))
            }
            // Idempotent: the run already finished — report its real terminal
            // state rather than pretending we cancelled it.
            CancelOutcome::AlreadyExited { cancelled } => {
                let row = self.read_run_row(&run_id).await;
                Ok(self.terminal_status_envelope(&run_id, cancelled, None, &now, row))
            }
            CancelOutcome::Absent => {
                let row = self.read_run_row(&run_id).await;
                match &row {
                    Ok(Some(_)) => {
                        Ok(self.terminal_status_envelope(&run_id, false, None, &now, row))
                    }
                    Ok(None) => Ok(tool_error_envelope(
                        McpErrorCode::RunNotFound,
                        &format!("no analyze run with id {run_id}"),
                        false,
                    )),
                    Err(err) => Ok(tool_error_envelope(
                        McpErrorCode::StorageError,
                        &err.to_string(),
                        storage_retryable(err),
                    )),
                }
            }
        }
    }

    /// Read a run's `(status, stats)` from the `runs` table via the reader pool.
    pub(crate) async fn read_run_row(
        &self,
        run_id: &str,
    ) -> std::result::Result<Option<(String, String)>, StorageError> {
        let run_id = run_id.to_owned();
        self.readers
            .with_reader(move |conn| {
                match conn.query_row(
                    "SELECT status, stats FROM runs WHERE id = ?1",
                    rusqlite::params![run_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                ) {
                    Ok(tuple) => Ok(Some(tuple)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(err) => Err(StorageError::from(err)),
                }
            })
            .await
    }

    /// Build a terminal `analyze_status` envelope from the DB row, honoring a
    /// registry cancel flag and surfacing recorded stats.
    #[allow(clippy::unused_self)]
    pub(crate) fn terminal_status_envelope(
        &self,
        run_id: &str,
        cancelled: bool,
        started_at: Option<&str>,
        now: &str,
        row: std::result::Result<Option<(String, String)>, StorageError>,
    ) -> Value {
        let (db_status, stats) = match row {
            Ok(Some((db_status, stats))) => (Some(db_status), stats),
            Ok(None) => (None, "{}".to_owned()),
            Err(err) => {
                return tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                );
            }
        };
        let mapped_status = if cancelled || run_stats_is_cancelled(&stats) {
            "cancelled"
        } else {
            match &db_status {
                Some(value) => map_run_status(value, &stats),
                // Process exited but never recorded a run row.
                None => "failed",
            }
        };
        let stats_value = serde_json::from_str::<Value>(&stats).unwrap_or(Value::Null);
        json!({
            "ok": true,
            "result": {
                "run_id": run_id,
                "status": mapped_status,
                "elapsed_seconds": started_at.and_then(|start| elapsed_seconds(start, now)),
                "stats": stats_value,
            },
            "error": null,
            "diagnostics": [],
            "truncated": false,
            "truncation_reason": null,
            "stats_delta": {}
        })
    }
}
