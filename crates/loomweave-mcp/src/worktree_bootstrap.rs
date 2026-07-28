//! Serve bootstrap for a linked worktree's isolated index (worktree-indexes
//! design, "Bootstrap" section).
//!
//! `serve` on a linked worktree with no completed analyze run does not
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
//! **Double-spawn is guarded by the pre-existing `analyze_lock.rs` fs2 lock**
//! that the spawned `loomweave worktree analyze` child acquires for itself
//! before writing any `runs` row (`crates/loomweave-cli/src/analyze.rs`) —
//! not by anything in this module. A second `serve` racing the same worktree
//! spawns its own child unconditionally; the loser fails fast on the lock
//! and exits having written nothing. This module never waits on, inspects,
//! or tracks the spawned child (no run registry, no owner-pid probe — the
//! design's ephemeral posture).

use std::path::Path;
use std::process::{Child, Command, Stdio};

use rusqlite::Connection;

/// Readiness of a linked worktree's isolated index, recomputed from the
/// `runs` table on every gated tool call — never cached, never timed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorktreeReadiness {
    /// No run row yet, or the most recent run (by `started_at`) is still
    /// `running`.
    Building,
    /// The most recent run completed successfully, or completed with
    /// `skipped_no_plugins` (a legitimate terminal state — no plugins
    /// installed is not a build failure, and gating on it forever would wedge
    /// the session with no way out).
    Ready,
    /// The most recent run's status is `failed`.
    BuildFailed,
}

/// The outcome of a readiness read: the classified state plus the run id (if
/// any row exists at all) so callers can surface it in error diagnostics.
#[derive(Debug, Clone)]
pub(crate) struct ReadinessRead {
    pub(crate) readiness: WorktreeReadiness,
    pub(crate) run_id: Option<String>,
}

/// Classify readiness from a `runs.status` value, or `None` when no run row
/// exists at all. The one place the `running`/`completed`/`skipped_no_plugins`/
/// `failed` → [`WorktreeReadiness`] mapping lives, shared by
/// [`read_worktree_readiness`] (the gate's own query) and
/// `tool_project_status` (which reuses the row `latest_run_row` already read
/// in the same reader closure, rather than re-querying the `runs` table a
/// second time for the same "most recent row" answer).
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

/// Read the most recent run row (by `started_at`) and classify readiness.
///
/// Fail-safe on a query error: logs a warning and reports [`WorktreeReadiness::Building`]
/// rather than risk answering `Ready` (and unblocking graph tools) against a
/// read that could not actually be verified.
pub(crate) fn read_worktree_readiness(conn: &Connection) -> ReadinessRead {
    match conn.query_row(
        "SELECT id, status FROM runs ORDER BY started_at DESC LIMIT 1",
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

/// The exact fallback command diagnostics carry: `loomweave worktree analyze
/// -- <target>`, the documented recovery path from every `index-building` /
/// `index-build-failed` response.
pub fn fallback_argv(target: &Path) -> Vec<String> {
    vec![
        "loomweave".to_owned(),
        "worktree".to_owned(),
        "analyze".to_owned(),
        "--".to_owned(),
        target.display().to_string(),
    ]
}

/// Spawn `loomweave worktree analyze -- <target>` as a new process-group
/// leader with null stdio, returning the live [`Child`] without waiting on
/// it.
///
/// This is the low-level primitive: production callers use
/// [`spawn_detached_worktree_analyze`], which drops the `Child` immediately
/// (true fire-and-forget, mirroring `hook.rs::spawn_detached_analyze`'s
/// detachment technique). Tests use this function directly so they can
/// deterministically `wait()` on the child instead of polling.
pub(crate) fn spawn_worktree_analyze(program: &Path, target: &Path) -> std::io::Result<Child> {
    let mut command = Command::new(program);
    command
        .arg("worktree")
        .arg("analyze")
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
pub fn spawn_detached_worktree_analyze(program: &Path, target: &Path) {
    match spawn_worktree_analyze(program, target) {
        Ok(_child) => {}
        Err(err) => {
            tracing::warn!(
                error = %err,
                program = %program.display(),
                target = %target.display(),
                "worktree bootstrap: failed to spawn detached `loomweave worktree analyze`; \
                 the index will stay in the building state until it is run manually"
            );
        }
    }
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
    fn readiness_follows_the_most_recent_run_by_started_at() {
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
    fn fallback_argv_names_the_exact_recovery_command() {
        let target = Path::new("/repos/primary/../linked");
        let argv = fallback_argv(target);
        assert_eq!(
            argv,
            vec![
                "loomweave".to_owned(),
                "worktree".to_owned(),
                "analyze".to_owned(),
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

        let mut child = spawn_worktree_analyze(&script, dir.path()).expect("spawn stub");
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
        let mut child = spawn_worktree_analyze(&script, &target).expect("spawn stub");
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
