//! In-memory registry of `clarion analyze` subprocesses launched over MCP
//! (`analyze_start` / `analyze_status` / `analyze_cancel`, clarion-7e0c21558a).
//!
//! Decomposition decision: the MCP owns the subprocess and the cancel kill +
//! terminal write; the `analyze` CLI stays unaware it is being supervised (no
//! signal handler, no cancel flag). On cancel the MCP SIGKILLs the run's
//! process group — which reaches the plugin and its `pyright-langserver`
//! grandchild, since neither detaches into a new session — then writes the
//! run's terminal state directly. Discard-on-cancel is acceptable for a cancel.
//!
//! This bends the ADR-011 single-writer posture for exactly one narrow write
//! (the guarded cancel UPDATE), and only after the analyze process — the
//! normal writer — is dead, so there is no concurrent writer. Stale-`running`
//! reconciliation for a crash of the *supervising* process is explicitly out of
//! scope here; that is what the deferred `owner_pid`/`heartbeat_at` work
//! (clarion-f9027d2187) closes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};

/// One supervised analyze subprocess.
pub(crate) struct RunHandle {
    /// The `clarion analyze` child. `try_wait`/`wait` reap it; held by value so
    /// the registry owns the process.
    pub child: Child,
    /// Process-group id (== child pid; the child is spawned as a group leader)
    /// so a single `killpg` reaches the plugin and pyright grandchildren.
    pub pgid: i32,
    /// ISO-8601 start time, for elapsed reporting.
    pub started_at: String,
    /// Where the run writes its structured progress snapshot.
    pub progress_path: PathBuf,
    /// Set by `analyze_cancel`; makes `analyze_status` report `cancelled` even
    /// before the terminal DB write is observed.
    pub cancelled: bool,
}

/// Shared map of run id → handle. A plain `std::sync::Mutex`: the critical
/// sections are short (`try_wait`, a SIGKILL, a brief reap) and the tool
/// surface is low-volume.
pub(crate) type RunRegistry = Arc<Mutex<HashMap<String, RunHandle>>>;

/// Spawn `clarion analyze` for `project_root` as a new process-group leader so
/// the whole subtree (plugin + pyright) can be group-killed on cancel.
///
/// `program` is the launcher (`current_exe()` in production; a stub in tests).
/// The run id and progress path are passed in so the caller can return the
/// handle without racing the run's first DB write or progress write.
pub(crate) fn spawn_analyze(
    program: &std::path::Path,
    project_root: &std::path::Path,
    run_id: &str,
    progress_path: &std::path::Path,
    started_at: String,
) -> std::io::Result<RunHandle> {
    let mut command = std::process::Command::new(program);
    command
        .arg("analyze")
        .arg(project_root)
        .arg("--run-id")
        .arg(run_id)
        .arg("--progress-file")
        .arg(progress_path);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 0 → the child becomes leader of a new group whose id is its own pid.
        command.process_group(0);
    }

    let child = command.spawn()?;
    // A pid always fits in i32 on the platforms we target; the group id is the
    // child's pid because it was spawned as a new group leader.
    #[allow(clippy::cast_possible_wrap)]
    let pgid = child.id() as i32;
    Ok(RunHandle {
        child,
        pgid,
        started_at,
        progress_path: progress_path.to_path_buf(),
        cancelled: false,
    })
}

/// SIGKILL the run's whole process group, then reap the analyze child.
///
/// On Unix this terminates the plugin and `pyright-langserver` grandchildren
/// too (acceptance: "terminates child plugin/Pyright processes"). The orphaned
/// grandchildren are re-parented to init and reaped there. Non-Unix falls back
/// to killing just the analyze process.
pub(crate) fn kill_run(handle: &mut RunHandle) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        // Negative-pid group kill; ignore ESRCH (already dead).
        let _ = killpg(Pid::from_raw(handle.pgid), Signal::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = handle.child.kill();
    }
    // Reap the analyze child so it does not linger as a zombie. After SIGKILL
    // this returns promptly.
    let _ = handle.child.wait();
    handle.cancelled = true;
}

/// Mark a cancelled run terminal in the database. The narrow single write this
/// module owns (see the module note): a guarded UPDATE that only touches a row
/// still in `running` — so a run that finished a beat before the cancel keeps
/// its real terminal state, and a run cancelled before it recorded a `runs` row
/// updates nothing. `stats.terminal_reason="cancelled"` is how `analyze_status`
/// tells a cancel from an ordinary failure. Best-effort: the analyze writer is
/// already dead, so there is no contention, but a failure here is logged and
/// dropped (the in-memory registry still reports `cancelled`).
pub(crate) fn mark_run_cancelled_in_db(db_path: &std::path::Path, run_id: &str, now: &str) {
    let stats = serde_json::json!({
        "terminal_reason": "cancelled",
        "failure_reason": "cancelled via MCP analyze_cancel",
    })
    .to_string();
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(error = %err, run_id, "cancel: open db for terminal write failed");
            return;
        }
    };
    if let Err(err) = clarion_storage::pragma::apply_write_pragmas(&conn) {
        tracing::warn!(error = %err, run_id, "cancel: write pragmas failed");
        return;
    }
    if let Err(err) = conn.execute(
        "UPDATE runs SET status = 'failed', completed_at = ?1, stats = ?2 \
         WHERE id = ?3 AND status = 'running'",
        rusqlite::params![now, stats, run_id],
    ) {
        tracing::warn!(
            error = %err,
            run_id,
            "failed to persist cancelled run terminal state (registry still reports cancelled)",
        );
    }
}
