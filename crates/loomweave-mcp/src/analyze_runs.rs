//! In-memory registry of `loomweave analyze` subprocesses launched over MCP
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
use std::process::Stdio;
use std::sync::{Arc, Mutex};

/// One supervised analyze subprocess.
pub(crate) struct RunHandle {
    /// The `loomweave analyze` child. `try_wait`/`wait` reap it; held by value so
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

/// Spawn `loomweave analyze` for `project_root` as a new process-group leader so
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
        .arg(progress_path)
        // Isolate the child's stdio. When analyze_start is driven from the
        // stdio MCP server, the child would otherwise inherit the server's
        // stdout — and `loomweave analyze` initializes tracing at `info`, so its
        // non-framed progress bytes would interleave with the MCP JSON-RPC
        // responses on the same stream and corrupt the client connection.
        // Progress is reported via --progress-file, not stdout.
        .stdout(Stdio::null())
        .stderr(Stdio::null());

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

/// Best-effort delete a finished run's progress file as its handle is evicted
/// from the registry. A missing file is success — a run may exit before writing
/// one. Keeps `.weft/loomweave/runs/*.progress.json` from accumulating across a
/// long-lived `loomweave serve` (clarion-7e0c21558a).
pub(crate) fn reap_progress_file(path: &std::path::Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::debug!(
                error = %err,
                path = %path.display(),
                "reap analyze progress file failed (left on disk)",
            );
        }
    }
}

/// Evict every terminal handle from the registry and reap its progress file,
/// returning the number removed. A still-running handle (`try_wait` ==
/// `Ok(None)`) is retained; an exited or already-cancelled handle is dropped.
///
/// Called at the head of `analyze_start`: since every run begins with a start,
/// sweeping there bounds the registry (and the `runs/` progress directory) to
/// the live run plus the one about to spawn, however many analyses a
/// long-lived `serve` runs over its lifetime (clarion-7e0c21558a).
pub(crate) fn reap_terminal_runs(registry: &mut HashMap<String, RunHandle>) -> usize {
    let dead: Vec<String> = registry
        .iter_mut()
        .filter_map(|(id, handle)| match handle.child.try_wait() {
            // Still running — keep it.
            Ok(None) => None,
            // Exited, or `try_wait` errored (already reaped after a cancel) —
            // evict it.
            Ok(Some(_)) | Err(_) => Some(id.clone()),
        })
        .collect();
    for id in &dead {
        if let Some(handle) = registry.remove(id) {
            reap_progress_file(&handle.progress_path);
        }
    }
    dead.len()
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
    if let Err(err) = loomweave_storage::pragma::apply_write_pragmas(&conn) {
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

// The only test here is Linux-only (it relies on `/proc/<pid>/fd`), so gate the
// whole module — otherwise `use super::*` is an unused import on non-Linux
// targets under `-D warnings` (clarion-12667da9f5).
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The stdio MCP server speaks JSON-RPC framing on its own stdout. A
    /// spawned `loomweave analyze` that inherited that stdout and emitted `info`
    /// tracing would interleave non-framed bytes onto the wire and corrupt the
    /// client connection. The child's stdout must be isolated. We prove it by
    /// having a stub record where its fd 1 actually points: `/dev/null` when
    /// isolated, a `pipe:`/file path when inherited.
    #[test]
    fn spawn_analyze_isolates_child_stdout_from_parent() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("stub.sh");
        let progress = dir.path().join("fd1.txt");
        // spawn_analyze appends `analyze <root> --run-id <id> --progress-file
        // <path>`, so the stub's $6 is the progress-file path.
        let mut file = std::fs::File::create(&script).unwrap();
        // Capture where the SHELL's fd 1 points via command substitution (so
        // readlink's own redirected fd 1 doesn't taint the answer), then write
        // it to "$6". `/dev/null` means the child stdout was isolated.
        writeln!(
            file,
            "#!/bin/sh\nt=$(readlink /proc/$$/fd/1)\nprintf '%s' \"$t\" > \"$6\"\n"
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        drop(file);

        let mut handle = spawn_analyze(
            &script,
            dir.path(),
            "run-x",
            &progress,
            "2026-05-30T00:00:00Z".to_owned(),
        )
        .expect("spawn stub");
        handle.child.wait().expect("reap stub");

        let where_fd1 = std::fs::read_to_string(&progress).expect("stub wrote fd1 target");
        assert_eq!(
            where_fd1.trim(),
            "/dev/null",
            "child stdout was not isolated from the parent: {where_fd1:?}"
        );
    }
}
