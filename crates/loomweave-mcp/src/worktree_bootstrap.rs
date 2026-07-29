//! Serve bootstrap for a linked worktree's isolated index (worktree-indexes
//! design, "Bootstrap" section).
//!
//! `serve` on a linked worktree with no analyze attempt does not
//! degrade to the no-index stdio loop (`serve_stdio_no_index`): Task 3's
//! eager, schema-initialized `loomweave.db` means the effective store's DB
//! path already exists, so file existence can no longer be the readiness
//! signal. Instead `serve` spawns a **detached** `loomweave worktree
//! analyze` for the worktree (this module's [`spawn_detached_worktree_analyze`])
//! and serves immediately; [`ServerState`](crate::ServerState)'s per-call
//! gate (`crate::lib`'s `handle_tool_call`) consults `read_worktree_readiness`
//! against the same `runs` table every graph-tool call until a terminal run
//! row appears.
//!
//! **Concurrent initialization and double-spawn are guarded by the stable
//! `analyze_lock.rs` fs2 lock.** `serve` acquires it before initializing the
//! store, and every spawned `loomweave worktree analyze` child reacquires it
//! before its own store validation or writes (`crates/loomweave-cli/src/analyze.rs`).
//! If two launchers still reach the spawn edge together, the losing child
//! fails fast on that same lock and exits having written nothing. A reaper
//! thread owns every spawned
//! [`Child`], waits it to completion, and records a synthetic failed run when
//! a non-zero child exits before `BeginRun`; this prevents zombie processes
//! and a permanently ambiguous no-row `Building` state.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

/// Readiness of a linked worktree's isolated index, recomputed from the
/// `runs` table on every gated tool call — never cached, never timed.
///
/// Governed by the most recent run row: analyze rebuilds tables in place, so
/// an older completed row is not a stable snapshot while a newer run is
/// writing or after that rebuild failed.
/// Public (not `pub(crate)`): `loomweave-cli`'s HTTP read API gates on the
/// same readiness consult as the MCP tools (clarion-ecf882f230), so the
/// classification and the query live here exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeReadiness {
    /// No run has ever completed: either no run row exists yet, or the most
    /// recent row (by `started_at`) is `running` (or an unrecognized future
    /// status).
    Building,
    /// The most recent run row has `completed`, or completed with
    /// `skipped_no_plugins` (a legitimate terminal state — no plugins
    /// installed is not a build failure, and gating on it forever would wedge
    /// the session with no way out).
    Ready,
    /// No run has ever completed, and the most recent row's status is
    /// `failed`.
    BuildFailed,
}

/// The outcome of a readiness read: the classified state plus the run id (if
/// any row exists at all) so callers can surface it in error diagnostics.
#[derive(Debug, Clone)]
pub struct ReadinessRead {
    pub readiness: WorktreeReadiness,
    pub run_id: Option<String>,
}

/// Classify readiness from a single `runs.status` value, or `None` when no
/// run row exists at all. The one place the
/// `running`/`completed`/`skipped_no_plugins`/`failed` → [`WorktreeReadiness`]
/// mapping lives — used by [`read_worktree_readiness`] to interpret whichever
/// most recent row. `tool_project_status`
/// (`crates/loomweave-mcp/src/tools/status.rs`) calls
/// `read_worktree_readiness` directly for its gating decision.
pub(crate) fn classify_readiness(status: Option<&str>) -> WorktreeReadiness {
    match status {
        Some("completed" | "skipped_no_plugins") => WorktreeReadiness::Ready,
        Some("failed") => WorktreeReadiness::BuildFailed,
        // No row, "running", or any future status this build doesn't yet
        // know about — treat conservatively as still building rather than
        // guess.
        None | Some(_) => WorktreeReadiness::Building,
    }
}

/// Read the readiness-governing run row and classify readiness.
///
/// The most recent row always governs. Analyze mutates the same tables in
/// place and commits bounded batches, so serving through a newer `running`
/// row would expose a partial graph; serving after a newer `failed` row would
/// retain that partial state. There is no separate immutable generation that
/// would make the older completed row safe to select.
///
/// Fail-safe on a query error: logs a warning and reports [`WorktreeReadiness::Building`]
/// rather than risk answering `Ready` (and unblocking graph tools) against a
/// read that could not actually be verified.
pub fn read_worktree_readiness(conn: &Connection) -> ReadinessRead {
    match conn.query_row(
        "SELECT id, status FROM runs \
         ORDER BY started_at DESC, rowid DESC \
         LIMIT 1",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    ) {
        Ok((run_id, status)) => ReadinessRead {
            readiness: classify_readiness(Some(&status)),
            run_id: Some(run_id),
        },
        Err(rusqlite::Error::QueryReturnedNoRows) => ReadinessRead {
            readiness: WorktreeReadiness::Building,
            run_id: None,
        },
        Err(err) => {
            tracing::warn!(error = %err, "worktree bootstrap: readiness query failed");
            ReadinessRead {
                readiness: WorktreeReadiness::Building,
                run_id: None,
            }
        }
    }
}

/// Whether `serve`'s bootstrap should spawn `loomweave worktree analyze` for
/// this store: only when no analyze attempt has written a run row — per the
/// design, "serve on a linked worktree WITH NO INDEX ... spawn". Completed or
/// in-flight rows must not trigger redundant background work, and a failed row
/// requires the surfaced explicit recovery command rather than another doomed
/// automatic retry on every `serve` restart.
pub fn should_spawn_bootstrap_analyze(conn: &Connection) -> bool {
    read_worktree_readiness(conn).run_id.is_none()
}

/// The exact fallback command diagnostics carry: `loomweave worktree analyze
/// -- <target>`, the documented recovery path from every `index-building` /
/// `index-build-failed` response.
pub fn fallback_argv(target: &Path, explicit_config: Option<&Path>) -> Vec<String> {
    let mut argv = vec![
        "loomweave".to_owned(),
        "worktree".to_owned(),
        "analyze".to_owned(),
    ];
    if let Some(config) = explicit_config {
        argv.push("--config".to_owned());
        argv.push(config.display().to_string());
    }
    argv.push("--".to_owned());
    argv.push(target.display().to_string());
    argv
}

/// Spawn `loomweave worktree analyze -- <target>` as a new process-group
/// leader with null stdio, returning the live [`Child`] without waiting on
/// it.
///
/// This is the low-level primitive: production callers use
/// [`spawn_detached_worktree_analyze`], whose reaper thread owns and waits on
/// the child. Tests use this function directly so they can deterministically
/// `wait()` on the child instead of polling.
pub(crate) fn spawn_worktree_analyze(
    program: &Path,
    target: &Path,
    explicit_config: Option<&Path>,
) -> std::io::Result<Child> {
    let mut command = Command::new(program);
    command.arg("worktree").arg("analyze");
    // Forward ONLY an explicit `--config` (clarion-c39b92b868): with none,
    // the child re-derives the same source→primary ladder `serve` used, so
    // no forwarding is needed — but an operator-supplied explicit path is
    // information the child cannot re-derive.
    if let Some(config) = explicit_config {
        command.arg("--config").arg(config);
    }
    command
        .arg("--")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // 0 -> the child becomes leader of a new group whose id is its own
        // pid, detaching it from the `serve` process's own group (the same
        // technique `hook.rs::spawn_detached_analyze` uses).
        command.process_group(0);
    }
    command.spawn()
}

/// Spawn `loomweave worktree analyze -- <target>` detached and fire-and-forget:
/// the `Child` handle is dropped immediately, so this call returns at once
/// and the OS reparents the child when `serve` exits. A spawn failure (e.g.
/// the executable disappeared) is logged and swallowed — `serve` must keep
/// running in the `building` state either way; the documented fallback is
/// running `loomweave worktree analyze` by hand.
/// Returns whether the spawn SUCCEEDED — the failure is still logged and
/// swallowed here (serve keeps running in the `building` state either way),
/// but the caller records it on the gate so `project_status_get` can report
/// `bootstrap-spawn-failed` instead of an indistinguishable forever-building
/// state (clarion-917df0e1ad).
pub fn spawn_detached_worktree_analyze(
    program: &Path,
    target: &Path,
    explicit_config: Option<&Path>,
    db_path: &Path,
) -> bool {
    match spawn_worktree_analyze(program, target, explicit_config) {
        Ok(child) => supervise_bootstrap_child(child, db_path),
        Err(err) => {
            tracing::warn!(
                error = %err,
                program = %program.display(),
                target = %target.display(),
                "worktree bootstrap: failed to spawn detached `loomweave worktree analyze`; \
                 the index will stay in the building state until it is run manually"
            );
            false
        }
    }
}

fn supervise_bootstrap_child(child: Child, db_path: &Path) -> bool {
    let child = Arc::new(Mutex::new(Some(child)));
    let child_for_thread = Arc::clone(&child);
    let db_path = db_path.to_path_buf();
    match std::thread::Builder::new()
        .name("loomweave-worktree-bootstrap-reaper".to_owned())
        .spawn(move || {
            let mut child = child_for_thread
                .lock()
                .expect("bootstrap child mutex poisoned")
                .take()
                .expect("bootstrap child consumed once");
            match child.wait() {
                Ok(status) if status.success() => {}
                Ok(status) => record_early_bootstrap_failure(
                    &db_path,
                    &format!("bootstrap analyze exited with {status}"),
                ),
                Err(err) => record_early_bootstrap_failure(
                    &db_path,
                    &format!("wait for bootstrap analyze failed: {err}"),
                ),
            }
        }) {
        Ok(_join) => true,
        Err(err) => {
            if let Some(mut child) = child.lock().expect("bootstrap child mutex poisoned").take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            tracing::warn!(
                error = %err,
                "worktree bootstrap: could not start child reaper; killed the unsupervised \
                 analyze process"
            );
            false
        }
    }
}

fn record_early_bootstrap_failure(db_path: &Path, reason: &str) {
    if another_analyzer_may_own_lock(db_path) {
        tracing::warn!(
            db = %db_path.display(),
            reason,
            "worktree bootstrap child failed before BeginRun while another analyzer may own the lock; leaving readiness to the lock owner"
        );
        return;
    }
    let Ok(conn) = Connection::open(db_path) else {
        tracing::warn!(db = %db_path.display(), reason, "could not record early bootstrap failure");
        return;
    };
    let run_id = format!("bootstrap-spawn-{}", uuid::Uuid::new_v4());
    let stats = serde_json::json!({
        "bootstrap_spawn_failed": true,
        "reason": reason,
    })
    .to_string();
    match conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         SELECT ?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                '{\"bootstrap\":true}', ?2, 'failed' \
          WHERE NOT EXISTS (SELECT 1 FROM runs)",
        rusqlite::params![run_id, stats],
    ) {
        Ok(1) => tracing::warn!(reason, "worktree bootstrap child failed before BeginRun"),
        Ok(_) => {
            // Another analyze published a real row while this losing child
            // exited (the normal double-spawn race). Its row governs.
        }
        Err(err) => tracing::warn!(
            error = %err,
            db = %db_path.display(),
            reason,
            "could not persist early bootstrap failure"
        ),
    }
}

/// The linked-worktree analyze lock is a stable sibling of the replaceable
/// store directory: `<worktrees>/<stable-id>.lock`. A child that lost this
/// lock can exit before the winner publishes `BeginRun`; publishing a
/// synthetic failure in that interval would sort after the winner's captured
/// `started_at` and permanently govern readiness. Only synthesize when the
/// lock can be proved unowned.
fn another_analyzer_may_own_lock(db_path: &Path) -> bool {
    let Some(lock_path) = bootstrap_analyze_lock_path(db_path) else {
        return true;
    };
    let lock = match OpenOptions::new().read(true).write(true).open(&lock_path) {
        Ok(lock) => lock,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
        Err(err) => {
            tracing::warn!(
                error = %err,
                lock_path = %lock_path.display(),
                "could not inspect worktree analyze lock; suppressing synthetic bootstrap failure"
            );
            return true;
        }
    };
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => false,
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(err) => {
            tracing::warn!(
                error = %err,
                lock_path = %lock_path.display(),
                "could not probe worktree analyze lock; suppressing synthetic bootstrap failure"
            );
            true
        }
    }
}

fn bootstrap_analyze_lock_path(db_path: &Path) -> Option<PathBuf> {
    let store = db_path.parent()?;
    let stable_id = store.file_name()?;
    Some(
        store
            .parent()?
            .join(format!("{}.lock", stable_id.to_string_lossy())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_empty_runs_db(dir: &Path) -> std::path::PathBuf {
        let db_path = dir.join("loomweave.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        loomweave_storage::pragma::apply_write_pragmas(&conn).expect("write pragmas");
        loomweave_storage::schema::apply_migrations(&mut conn).expect("apply migrations");
        db_path
    }

    fn seed_run(db_path: &Path, id: &str, started_at: &str, status: &str) {
        let conn = Connection::open(db_path).expect("open db");
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES (?1, ?2, NULL, '{}', '{}', ?3)",
            rusqlite::params![id, started_at, status],
        )
        .expect("insert run row");
    }

    #[test]
    fn no_run_rows_reads_as_building_with_no_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        let conn = Connection::open(&db_path).unwrap();

        let read = read_worktree_readiness(&conn);
        assert_eq!(read.readiness, WorktreeReadiness::Building);
        assert_eq!(read.run_id, None);
    }

    #[test]
    fn most_recent_running_row_reads_as_building() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        seed_run(&db_path, "r1", "2026-01-01T00:00:00.000Z", "running");

        let conn = Connection::open(&db_path).unwrap();
        let read = read_worktree_readiness(&conn);
        assert_eq!(read.readiness, WorktreeReadiness::Building);
        assert_eq!(read.run_id.as_deref(), Some("r1"));
    }

    #[test]
    fn most_recent_completed_or_skipped_row_reads_as_ready() {
        for status in ["completed", "skipped_no_plugins"] {
            let dir = tempfile::tempdir().unwrap();
            let db_path = open_empty_runs_db(dir.path());
            seed_run(&db_path, "r1", "2026-01-01T00:00:00.000Z", status);

            let conn = Connection::open(&db_path).unwrap();
            let read = read_worktree_readiness(&conn);
            assert_eq!(
                read.readiness,
                WorktreeReadiness::Ready,
                "status {status} must read as Ready"
            );
        }
    }

    #[test]
    fn most_recent_failed_row_reads_as_build_failed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        seed_run(&db_path, "r1", "2026-01-01T00:00:00.000Z", "failed");

        let conn = Connection::open(&db_path).unwrap();
        let read = read_worktree_readiness(&conn);
        assert_eq!(read.readiness, WorktreeReadiness::BuildFailed);
        assert_eq!(read.run_id.as_deref(), Some("r1"));
    }

    #[test]
    fn latest_row_governs_only_when_no_run_ever_completed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        seed_run(&db_path, "old", "2026-01-01T00:00:00.000Z", "failed");
        seed_run(&db_path, "new", "2026-02-01T00:00:00.000Z", "running");

        let conn = Connection::open(&db_path).unwrap();
        let read = read_worktree_readiness(&conn);
        assert_eq!(read.readiness, WorktreeReadiness::Building);
        assert_eq!(read.run_id.as_deref(), Some("new"));
    }

    #[test]
    fn a_later_running_rebuild_blocks_reads_despite_an_older_completed_row() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        seed_run(&db_path, "done", "2026-01-01T00:00:00.000Z", "completed");
        // Analyze rebuilds the same tables in place and commits in batches,
        // so the older completed row is not a stable snapshot while this run
        // is mutating the database.
        seed_run(&db_path, "rebuild", "2026-02-01T00:00:00.000Z", "running");

        let conn = Connection::open(&db_path).unwrap();
        let read = read_worktree_readiness(&conn);
        assert_eq!(read.readiness, WorktreeReadiness::Building, "{read:?}");
        assert_eq!(read.run_id.as_deref(), Some("rebuild"), "{read:?}");
    }

    #[test]
    fn a_later_failed_rebuild_blocks_reads_despite_an_older_completed_row() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        seed_run(&db_path, "done", "2026-01-01T00:00:00.000Z", "completed");
        seed_run(
            &db_path,
            "rebuild-failed",
            "2026-02-01T00:00:00.000Z",
            "failed",
        );

        let conn = Connection::open(&db_path).unwrap();
        let read = read_worktree_readiness(&conn);
        assert_eq!(read.readiness, WorktreeReadiness::BuildFailed, "{read:?}");
        assert_eq!(read.run_id.as_deref(), Some("rebuild-failed"), "{read:?}");
    }

    #[test]
    fn the_most_recent_of_several_completed_rows_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        seed_run(&db_path, "first", "2026-01-01T00:00:00.000Z", "completed");
        seed_run(&db_path, "second", "2026-02-01T00:00:00.000Z", "completed");

        let conn = Connection::open(&db_path).unwrap();
        let read = read_worktree_readiness(&conn);
        assert_eq!(read.readiness, WorktreeReadiness::Ready, "{read:?}");
        assert_eq!(read.run_id.as_deref(), Some("second"), "{read:?}");
    }

    #[test]
    fn should_spawn_is_false_while_a_rebuild_row_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        seed_run(&db_path, "done", "2026-01-01T00:00:00.000Z", "completed");
        seed_run(&db_path, "rebuild", "2026-02-01T00:00:00.000Z", "running");

        let conn = Connection::open(&db_path).unwrap();
        assert!(
            !should_spawn_bootstrap_analyze(&conn),
            "a built worktree must not trigger an unconditional respawn on every serve restart"
        );
    }

    #[test]
    fn should_spawn_only_when_no_attempt_has_written_a_run_row() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = open_empty_runs_db(dir.path());
        let conn = Connection::open(&db_path).unwrap();
        assert!(
            should_spawn_bootstrap_analyze(&conn),
            "no runs at all -> unbuilt -> must spawn"
        );
        drop(conn);

        seed_run(&db_path, "r-fail", "2026-01-01T00:00:00.000Z", "failed");
        let conn = Connection::open(&db_path).unwrap();
        assert!(
            !should_spawn_bootstrap_analyze(&conn),
            "a recorded failure requires explicit recovery, not an automatic doomed respawn on every serve restart"
        );
    }

    #[test]
    fn early_failure_is_not_published_while_another_analyzer_holds_the_lock() {
        use fs2::FileExt as _;
        use std::fs::OpenOptions;

        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("worktrees/wt-test");
        std::fs::create_dir_all(&store).unwrap();
        let db_path = open_empty_runs_db(&store);
        let lock_path = dir.path().join("worktrees/wt-test.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        lock.lock_exclusive().unwrap();

        record_early_bootstrap_failure(&db_path, "losing child exited on held analyze lock");

        let conn = Connection::open(&db_path).unwrap();
        let run_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            run_count, 0,
            "the lock owner may be between timestamp capture and BeginRun; the losing child must not publish a newer synthetic failure"
        );
    }

    #[test]
    fn fallback_argv_names_the_exact_recovery_command() {
        let target = Path::new("/repos/primary/../linked");
        let config = Path::new("/configs/custom.yaml");
        let argv = fallback_argv(target, Some(config));
        assert_eq!(
            argv,
            vec![
                "loomweave".to_owned(),
                "worktree".to_owned(),
                "analyze".to_owned(),
                "--config".to_owned(),
                config.display().to_string(),
                "--".to_owned(),
                target.display().to_string(),
            ]
        );
    }

    /// The child's stdout/stderr must be isolated exactly like
    /// `analyze_runs::spawn_analyze`'s child: the stdio MCP server owns the
    /// parent's stdout for JSON-RPC framing, so an inherited stdout would
    /// interleave the spawned analyze's own `info` tracing onto the wire.
    #[test]
    #[cfg(target_os = "linux")]
    fn spawn_worktree_analyze_isolates_child_stdout_from_parent() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("stub.sh");
        let marker = dir.path().join("fd1.txt");
        let mut file = std::fs::File::create(&script).unwrap();
        // argv is `worktree analyze -- <target>`; the stub ignores its own
        // args and just reports where fd 1 points, writing to a fixed marker
        // path baked into the script itself (simpler than parsing `--`).
        writeln!(
            file,
            "#!/bin/sh\nt=$(readlink /proc/$$/fd/1)\nprintf '%s' \"$t\" > \"{}\"\n",
            marker.display()
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        drop(file);

        let mut child = spawn_worktree_analyze(&script, dir.path(), None).expect("spawn stub");
        child.wait().expect("reap stub");

        let where_fd1 = std::fs::read_to_string(&marker).expect("stub wrote fd1 target");
        assert_eq!(
            where_fd1.trim(),
            "/dev/null",
            "child stdout was not isolated from the parent: {where_fd1:?}"
        );
    }

    #[test]
    fn spawn_worktree_analyze_argv_is_worktree_analyze_dash_dash_target() {
        use std::io::Write as _;
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("argv_stub.sh");
        let argv_dump = dir.path().join("argv.txt");
        let mut file = std::fs::File::create(&script).unwrap();
        writeln!(
            file,
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"{}\"\n",
            argv_dump.display()
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        drop(file);

        let target = dir.path().join("target-worktree");
        let mut child = spawn_worktree_analyze(&script, &target, None).expect("spawn stub");
        child.wait().expect("reap stub");

        let argv = std::fs::read_to_string(&argv_dump).expect("stub wrote argv");
        let forwarded: Vec<&str> = argv.lines().collect();
        assert_eq!(
            forwarded,
            vec!["worktree", "analyze", "--", target.to_str().unwrap()],
            "argv must be exactly `worktree analyze -- <target>`, not the hook's plain-analyze argv"
        );
    }
}
