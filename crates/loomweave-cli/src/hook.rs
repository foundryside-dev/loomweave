//! `loomweave hook session-start` — fail-soft session-start orientation.
//!
//! Never returns an error to the caller: the `SessionStart` hook must never
//! block an agent's session start. All failures degrade to a printed note.

use std::path::{Path, PathBuf};

use loomweave_mcp::snapshot::{ProjectSnapshot, Staleness, missing_db_snapshot, project_snapshot};
use rusqlite::{Connection, OpenFlags};

/// Run `loomweave hook session-start`. Always returns `Ok(())`.
///
/// The `anyhow::Result` return type is intentional even though no `Err` is
/// ever produced: it keeps the `main.rs` dispatch arm uniform with the other
/// subcommands and documents the fail-soft contract at the type level.
#[allow(clippy::unnecessary_wraps)]
pub fn session_start(path: &Path) -> anyhow::Result<()> {
    // (1) Re-sync the skill pack ONLY if it's already installed in at least one
    //     skill root, and drifted. A bare session-start never bootstraps a
    //     never-installed project — that's `loomweave install --skills`'s job. Note
    //     the resync normalises BOTH roots once triggered: if a project installed
    //     only `.claude/skills`, a drift repair also (re)creates
    //     `.agents/skills`, keeping the two roots in lock-step. A drift repair
    //     keeps installed copies honest across upgrades.
    resync_skill_if_present(path);

    // (2) Snapshot.
    let outcome = load_snapshot(path);
    print_snapshot(path, &outcome);

    // (3) Single-shot background refresh. A stale index nudges the agent to run
    //     `loomweave analyze` — but the agent rarely does, so kick it off here
    //     ourselves: spawn ONE detached analyze and return immediately. The hook
    //     must never block session start, so we never wait on the child; the
    //     analyze advisory lock (`analyze_lock.rs`) makes a colliding run a clean
    //     no-op, so this is single-shot in practice.
    if should_trigger_background_analyze(&outcome) {
        trigger_background_analyze(path);
    }
    Ok(())
}

/// What the hook actually did about a stale index — the one thing the printed
/// line must be honest about (clarion-f57c9e74a6).
#[derive(Debug, Clone, PartialEq, Eq)]
enum BackgroundAnalyzeOutcome {
    /// No analyze held the lock; a detached child was spawned.
    Started,
    /// The analyze advisory lock is already held: another analyze is running
    /// (or landing its final transaction) and ours would have been a no-op.
    AlreadyRunning {
        lock_path: PathBuf,
        /// Whether a follow-up refresh was queued for the running analyze to
        /// drain on exit.
        queued: bool,
    },
    /// The spawn itself failed; the manual nudge already printed stands.
    SpawnFailed,
}

/// Probe the analyze advisory lock, then spawn only when nobody holds it.
///
/// Before this probe the hook printed "started a background analyze" on every
/// successful *spawn*, even when the child then lost the lock and exited at
/// once — reproduced on a shared checkout mid-analyze (clarion-f57c9e74a6).
/// The probe guard is dropped BEFORE the spawn so the child can take the lock;
/// the residual race (a second analyze starting in that gap) is harmless — the
/// loser is the same clean no-op it always was, we merely stop claiming credit
/// for it. A lock-path resolution failure degrades to the old spawn-and-see
/// behaviour rather than suppressing the refresh.
fn probe_then_spawn_background_analyze(project_root: &Path) -> BackgroundAnalyzeOutcome {
    if let Ok(ctx) = loomweave_core::worktree::WorktreeContext::resolve(project_root) {
        match crate::analyze_lock::try_acquire_analyze_lock_for_context(&ctx) {
            Ok(crate::analyze_lock::TryAnalyzeLock::Held { lock_path }) => {
                // Queue a follow-up instead of forking a child doomed to lose
                // the lock: the running analyze drains the request on exit
                // (clarion-78d75e45c9), so a burst of N events costs two runs.
                let queued = match crate::analyze_lock::request_pending_analyze(&ctx) {
                    Ok(_) => true,
                    Err(err) => {
                        tracing::warn!(error = %err, "could not queue a follow-up analyze");
                        false
                    }
                };
                return BackgroundAnalyzeOutcome::AlreadyRunning { lock_path, queued };
            }
            Ok(crate::analyze_lock::TryAnalyzeLock::Acquired(guard)) => drop(guard),
            Err(err) => {
                tracing::warn!(error = %err, "analyze-lock probe failed; spawning anyway");
            }
        }
    }
    match spawn_detached_analyze(project_root) {
        Ok(()) => BackgroundAnalyzeOutcome::Started,
        Err(err) => {
            tracing::warn!(error = %err, "background analyze spawn failed");
            BackgroundAnalyzeOutcome::SpawnFailed
        }
    }
}

/// The line the session-start hook prints for a stale index, keyed on what
/// actually happened. `None` for a failed spawn: the snapshot's manual
/// `loomweave analyze` nudge is already on screen and stays the truth.
fn background_analyze_line(outcome: &BackgroundAnalyzeOutcome) -> Option<String> {
    match outcome {
        BackgroundAnalyzeOutcome::Started => Some(
            "Loomweave: index is stale — started a background `loomweave analyze` \
             just now (detached, non-blocking). No need to run it manually; re-query \
             Loomweave once it finishes to pick up the refreshed graph."
                .to_owned(),
        ),
        BackgroundAnalyzeOutcome::AlreadyRunning { lock_path, queued } => Some(format!(
            "Loomweave: index is stale — another `loomweave analyze` is already running \
             (holds {}); nothing was started{}. No need to run it manually; re-query \
             Loomweave once it finishes (project_status_get reports the run) to pick \
             up the refreshed graph.",
            lock_path.display(),
            if *queued {
                ", a follow-up refresh is queued to run when it finishes"
            } else {
                ""
            }
        )),
        BackgroundAnalyzeOutcome::SpawnFailed => None,
    }
}

/// Whether a stale index should kick off a background re-analyze.
///
/// True only for a readable, *present* index whose freshness check says the
/// working tree moved since the last run (`Stale` / `StaleWorktree`). A fresh
/// index, a missing / never-analyzed db, or an unreadable db never triggers —
/// auto-analyze is a *refresh*, not a bootstrap (bootstrap stays the explicit
/// `loomweave install` + `loomweave analyze` path), and re-analyzing a fresh
/// index every session is wasted work.
fn should_trigger_background_analyze(outcome: &SnapshotOutcome) -> bool {
    let SnapshotOutcome::Ready(snapshot) = outcome else {
        return false;
    };
    snapshot.db_present()
        && matches!(
            snapshot.staleness(),
            Staleness::Stale | Staleness::StaleWorktree
        )
}

/// Spawn a detached `loomweave analyze <path>` and return without waiting.
///
/// Fail-soft: a spawn failure degrades to the manual `loomweave analyze` nudge
/// the snapshot already printed — it never errors out of the session-start hook.
fn trigger_background_analyze(project_root: &Path) {
    let outcome = probe_then_spawn_background_analyze(project_root);
    if let Some(line) = background_analyze_line(&outcome) {
        println!("{line}");
    }
}

/// Git-hook entry point (`loomweave hook git-sync`): spawn the same detached
/// background analyze the `SessionStart` hook uses, when — and only when — the
/// index is present and stale. Silent on the happy path (the managed git-hook
/// block discards output anyway) and never errors: a hook must not block git.
///
/// # Errors
///
/// Never returns an error today; the `anyhow::Result` keeps the signature
/// uniform with the other hook entry points.
#[allow(clippy::unnecessary_wraps)]
pub fn git_sync(path: &Path) -> anyhow::Result<()> {
    let outcome = load_snapshot(path);
    if should_trigger_background_analyze(&outcome) {
        // Same lock probe as session-start: a held lock means the refresh is
        // already underway, so don't fork a child just to have it lose the lock.
        let _ = probe_then_spawn_background_analyze(path);
    }
    Ok(())
}

/// Spawn `loomweave analyze <project_root>` as a fire-and-forget child:
/// stdio to `/dev/null`, in its own process group, never waited on.
///
/// The new process group (set via the *safe* `process_group(0)`, not a
/// `pre_exec(setsid)` that would widen the crate's unsafe surface) detaches the
/// analyze from the hook's group, so the agent harness reaping the `SessionStart`
/// hook cannot signal the in-flight analyze. The `Child` handle is dropped
/// immediately — we never `wait()` — so the call returns at once and the OS
/// reparents the analyze when this hook process exits.
fn spawn_detached_analyze(project_root: &Path) -> std::io::Result<()> {
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("analyze")
        .arg(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Strip repository-selector env before spawning. When this runs from a git
    // hook, git exports GIT_DIR / GIT_INDEX_FILE / GIT_WORK_TREE etc.; the
    // analyze child runs its own git for SEI extraction, and inheriting those
    // would repoint or poison it (mid-operation index files especially).
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&key);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    cmd.spawn().map(|_child| ())
}

/// What [`load_snapshot`] could establish about the `.weft/loomweave/` index.
///
/// A *missing* db and a *present-but-unreadable* db are deliberately distinct:
/// the missing case nudges toward `install` + `analyze`, but that advice is
/// wrong for a present-but-corrupt/locked db (`install` refuses while `.weft/loomweave/`
/// exists; `analyze` cannot repair corruption). See [`print_snapshot`].
enum SnapshotOutcome {
    /// Either the db file is absent (a `missing_db_snapshot()`) or it opened and
    /// read cleanly (a real [`project_snapshot`]).
    Ready(ProjectSnapshot),
    /// The db file is present but could not be opened or read back — corrupt,
    /// locked by another process, or otherwise unreadable.
    DbUnreadable,
}

/// Resolve the store this checkout actually reads: `WorktreeContext`'s
/// `store_paths.db` (worktree-index Task 7) — never a bare
/// `db_path(project_root)`, which for a linked worktree is the *source*
/// root's own store, a location `loomweave worktree analyze` never
/// populates. Falls back to the root-derived path on the one error
/// `WorktreeContext::resolve` can return (a non-UTF-8 path component) —
/// this hook must never block session start on a resolution failure.
fn resolve_effective_db_path(project_root: &Path) -> PathBuf {
    match loomweave_core::worktree::WorktreeContext::resolve(project_root) {
        Ok(ctx) => ctx.store_paths.db,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "resolve worktree context for session-start snapshot failed; falling back to \
                 <project_root>/.weft/loomweave/loomweave.db"
            );
            loomweave_core::store::db_path(project_root)
        }
    }
}

fn resync_skill_if_present(project_root: &Path) {
    let installed = project_root
        .join(".claude/skills/loomweave-workflow/SKILL.md")
        .exists()
        || project_root
            .join(".agents/skills/loomweave-workflow/SKILL.md")
            .exists();
    if !installed {
        return;
    }
    if let Err(err) = crate::skill_pack::install_skill_pack(project_root) {
        tracing::warn!(error = %err, "loomweave-workflow skill resync failed");
    }
}

fn load_snapshot(project_root: &Path) -> SnapshotOutcome {
    let db_path = resolve_effective_db_path(project_root);
    if !db_path.exists() {
        return SnapshotOutcome::Ready(missing_db_snapshot());
    }
    let conn = match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(error = %err, "open .weft/loomweave/loomweave.db read-only failed");
            return SnapshotOutcome::DbUnreadable;
        }
    };
    // `Connection::open_with_flags(.. READ_ONLY)` lazily succeeds even on a
    // non-SQLite file ("NOT A SQLITE DB" opens fine); the corruption only
    // surfaces at first read. Probe with a cheap query so a present-but-corrupt
    // db is classified as unreadable rather than silently reported as 0 counts
    // (which would otherwise print the wrong "no analysis yet" nudge).
    if let Err(err) = conn.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0)) {
        tracing::warn!(error = %err, "probe read of .weft/loomweave/loomweave.db failed");
        return SnapshotOutcome::DbUnreadable;
    }
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    SnapshotOutcome::Ready(project_snapshot(&conn, &root))
}

fn print_snapshot(project_root: &Path, outcome: &SnapshotOutcome) {
    for line in snapshot_outcome_lines(project_root, outcome) {
        println!("{line}");
    }
}

/// Load the index snapshot and render it to lines, for reuse by both the
/// `SessionStart` hook (which prints them) and `loomweave doctor` (which appends
/// them under an `--- index ---` heading). Fail-soft: a missing/unreadable db
/// yields an advisory line, never an error.
#[must_use]
pub fn snapshot_report(project_root: &Path) -> Vec<String> {
    let outcome = load_snapshot(project_root);
    snapshot_outcome_lines(project_root, &outcome)
}

/// Render a [`SnapshotOutcome`] to the exact lines the session-start hook has
/// always printed (one element per former `println!`), so behaviour is
/// preserved while the strings become reusable.
fn snapshot_outcome_lines(project_root: &Path, outcome: &SnapshotOutcome) -> Vec<String> {
    let mut lines = Vec::new();
    let snapshot = match outcome {
        SnapshotOutcome::Ready(snapshot) => snapshot,
        SnapshotOutcome::DbUnreadable => {
            let db_path = resolve_effective_db_path(project_root);
            lines.push(format!(
                "Loomweave: an index exists at {} but could not be opened (it may be \
                 corrupt, locked by another process, or unreadable). Check permissions, \
                 ensure no other loomweave process holds it, or remove .weft/loomweave/ and re-run \
                 `loomweave install` + `loomweave analyze`. (Run with RUST_LOG=warn for the \
                 open error.)",
                db_path.display()
            ));
            return lines;
        }
    };
    if !snapshot.db_present() {
        if let Ok(ctx) = loomweave_core::worktree::WorktreeContext::resolve(project_root)
            && ctx.kind == loomweave_core::worktree::WorktreeKind::Linked
        {
            lines.push(format!(
                "Loomweave: no index at {}. Run `loomweave worktree analyze -- {}` \
                 to build this linked worktree's isolated index.",
                ctx.store_paths.db.display(),
                project_root.display()
            ));
            return lines;
        }
        lines.push(format!(
            "Loomweave: no index at {}/.weft/loomweave/loomweave.db. \
             Run `loomweave install --path {}` then `loomweave analyze {}`.",
            project_root.display(),
            project_root.display(),
            project_root.display()
        ));
        return lines;
    }
    // Subsystems ARE entities (kind = 'subsystem'), so subsystem_count is a
    // subset of entity_count, not a parallel category — say so, or the two read
    // as disjoint (clarion-e4e80eff3f).
    lines.push(format!(
        "Loomweave index: {} entities (incl. {} subsystems), {} findings.",
        snapshot.entity_count(),
        snapshot.subsystem_count(),
        snapshot.finding_count()
    ));
    if snapshot.degraded() {
        // A backing query folded to a safe default, so the counts above may
        // understate a populated index. Distinct from the present-but-empty
        // case (which is not degraded). Operator detail is in the warn log.
        lines.push(
            "Loomweave: ⚠ snapshot is degraded — at least one index query failed and \
             the counts above may be incomplete. (Run with RUST_LOG=warn for details.)"
                .to_string(),
        );
    }
    match snapshot.staleness() {
        Staleness::Fresh => {
            // Surface the analyzed commit (when the run recorded one) so the
            // "fresh" claim names the commit it reflects — short form for the
            // banner; project_status_get carries the full `git_sha`.
            let at_commit = snapshot
                .indexed_at_commit()
                .map(|c| format!(", commit {}", c.chars().take(12).collect::<String>()))
                .unwrap_or_default();
            lines.push(format!(
                "Index is fresh (last analyzed {}{}). Ask Loomweave before re-exploring \
                 the tree; see the loomweave-workflow skill.",
                snapshot.last_analyzed_at().unwrap_or("unknown"),
                at_commit
            ));
            // Honest caveat (clarion-26c7e52027): freshness compares the mtimes of
            // *already-indexed* source files, so brand-new files in a not-yet-
            // indexed top-level directory — or any uncommitted additions, which the
            // untrusted-corpus git posture cannot safely detect — can sit unseen
            // behind a "fresh" verdict. Re-analyze is the remedy.
            lines.push(
                "Caveat: \"fresh\" reflects already-indexed files only; it will NOT \
                 detect brand-new modules in a not-yet-indexed directory. If you just \
                 added or moved source, run `loomweave analyze` before relying on \
                 graph answers (e.g. \"what calls X\")."
                    .to_string(),
            );
        }
        Staleness::Stale => {
            lines.push(format!(
                "Index may be stale: source files changed since the last run. \
                 Run `loomweave analyze {}` to refresh.",
                project_root.display()
            ));
        }
        Staleness::StaleWorktree => {
            // The ingested files are individually fresh, but the working tree has
            // untracked source of an already-indexed type the index has not seen
            // (the new-top-level-dir blind spot the mtime passes can't reach;
            // clarion-26c7e52027). Concrete, not a caveat — name the remedy.
            lines.push(format!(
                "Index does NOT reflect the working tree: untracked source files of \
                 already-indexed types are present (new modules not yet analyzed). \
                 Run `loomweave analyze {}` before relying on graph answers \
                 (e.g. \"what calls X\").",
                project_root.display()
            ));
        }
        Staleness::NeverAnalyzed => {
            lines.push(format!(
                "No analysis recorded yet. Run `loomweave analyze {}` to build the index.",
                project_root.display()
            ));
        }
        Staleness::NoSourcePaths => {
            lines.push(format!(
                "Index freshness not checked: no ingested entity has a recorded \
                 source path to compare against (last analyzed {}). The index is \
                 present and queryable.",
                snapshot.last_analyzed_at().unwrap_or("unknown")
            ));
        }
        Staleness::Unknown => {
            lines.push(format!(
                "Index freshness unknown (a freshness check failed). If briefings \
                 look empty, run `loomweave analyze {}`.",
                project_root.display()
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    use rusqlite::Connection;

    use loomweave_storage::{pragma, schema};

    /// Build a `Fresh` snapshot for `project_root`: one ingested source file that
    /// exists and is older than a completed run. `commit` populates
    /// `runs.analyzed_at_commit` (or leaves it NULL). Mirrors the snapshot
    /// module's own fixtures; the `TempDir` holding the db is returned so the
    /// caller keeps it alive.
    fn fresh_snapshot(
        project_root: &Path,
        commit: Option<&str>,
    ) -> (tempfile::TempDir, ProjectSnapshot) {
        std::fs::write(project_root.join("a.py"), "x = 1\n").unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(db_dir.path().join("loomweave.db")).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, source_file_path, created_at, updated_at) \
             VALUES ('python:module:a', 'python', 'module', 'a', 'a', '{}', 'a.py', \
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status, analyzed_at_commit) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed', ?1)",
            rusqlite::params![commit],
        )
        .unwrap();
        let snapshot = project_snapshot(&conn, project_root);
        assert_eq!(
            snapshot.staleness(),
            Staleness::Fresh,
            "fixture must be Fresh: {snapshot:?}"
        );
        (db_dir, snapshot)
    }

    /// Build a `Stale` snapshot: one ingested source file that exists and is
    /// *newer* than a completed run (the mtime path → `Stale` in a non-git
    /// tempdir). Mirrors [`fresh_snapshot`] with the run pushed into the past.
    fn stale_snapshot(project_root: &Path) -> (tempfile::TempDir, ProjectSnapshot) {
        std::fs::write(project_root.join("a.py"), "x = 1\n").unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(db_dir.path().join("loomweave.db")).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, source_file_path, created_at, updated_at) \
             VALUES ('python:module:a', 'python', 'module', 'a', 'a', '{}', 'a.py', \
                     '2000-01-01T00:00:00.000Z', '2000-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2000-01-01T00:00:00.000Z', '2000-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        let snapshot = project_snapshot(&conn, project_root);
        assert_eq!(
            snapshot.staleness(),
            Staleness::Stale,
            "fixture must be Stale: {snapshot:?}"
        );
        (db_dir, snapshot)
    }

    #[test]
    fn stale_index_triggers_background_analyze() {
        let root = tempfile::tempdir().unwrap();
        let (_db, snapshot) = stale_snapshot(root.path());
        assert!(
            should_trigger_background_analyze(&SnapshotOutcome::Ready(snapshot)),
            "a present, stale index must trigger the single-shot background analyze"
        );
    }

    #[test]
    fn held_analyze_lock_reports_already_running_not_started() {
        // clarion-f57c9e74a6: the hook must not claim it started an analyze
        // when the advisory lock says one is already running.
        let root = tempfile::tempdir().unwrap();
        let loomweave_dir = root.path().join(".weft").join("loomweave");
        std::fs::create_dir_all(&loomweave_dir).unwrap();
        let ctx = loomweave_core::worktree::WorktreeContext::resolve(root.path())
            .expect("plain directory resolves as its own store");
        let _held = crate::analyze_lock::acquire_analyze_lock_for_context(&ctx).unwrap();

        let outcome = probe_then_spawn_background_analyze(root.path());
        let BackgroundAnalyzeOutcome::AlreadyRunning { lock_path, queued } = &outcome else {
            panic!("held lock must report AlreadyRunning, got {outcome:?}");
        };
        assert!(*queued, "a held lock must queue a follow-up refresh");
        assert!(
            crate::analyze_lock::pending_analyze_requested(&ctx),
            "the pending marker must exist beside the lock"
        );
        assert!(crate::analyze_lock::take_pending_analyze(&ctx));
        assert!(!crate::analyze_lock::pending_analyze_requested(&ctx));
        assert!(
            !crate::analyze_lock::take_pending_analyze(&ctx),
            "taking twice is a no-op"
        );
        assert!(
            lock_path.starts_with(&loomweave_dir),
            "{}",
            lock_path.display()
        );
        let line = background_analyze_line(&outcome).expect("already-running prints a line");
        assert!(line.contains("already running"), "{line}");
        assert!(line.contains("nothing was started"), "{line}");
        assert!(!line.contains("started a background"), "{line}");
    }

    #[test]
    fn background_analyze_lines_are_honest_per_outcome() {
        let started = background_analyze_line(&BackgroundAnalyzeOutcome::Started).unwrap();
        assert!(
            started.contains("started a background `loomweave analyze`"),
            "{started}"
        );
        assert!(
            background_analyze_line(&BackgroundAnalyzeOutcome::SpawnFailed).is_none(),
            "a failed spawn must not print a success line; the manual nudge stands"
        );
    }

    #[test]
    fn fresh_index_does_not_trigger_background_analyze() {
        let root = tempfile::tempdir().unwrap();
        let (_db, snapshot) = fresh_snapshot(root.path(), None);
        assert!(
            !should_trigger_background_analyze(&SnapshotOutcome::Ready(snapshot)),
            "a fresh index must NOT re-analyze — that would be wasted work every session"
        );
    }

    #[test]
    fn missing_and_unreadable_db_never_trigger_background_analyze() {
        // A never-analyzed project bootstraps via explicit install+analyze, not a
        // background refresh; an unreadable db cannot be safely re-analyzed blind.
        assert!(
            !should_trigger_background_analyze(&SnapshotOutcome::Ready(missing_db_snapshot())),
            "missing db must not trigger a background analyze"
        );
        assert!(
            !should_trigger_background_analyze(&SnapshotOutcome::DbUnreadable),
            "unreadable db must not trigger a background analyze"
        );
    }

    #[test]
    fn fresh_banner_carries_honest_caveat_and_commit() {
        // The bare "fresh ... ask Loomweave before re-exploring" line lied about
        // brand-new uncommitted modules (clarion-26c7e52027). The Fresh arm must
        // now (a) name the indexed commit and (b) carry the re-analyze caveat.
        let root = tempfile::tempdir().unwrap();
        let (_db, snapshot) = fresh_snapshot(root.path(), Some("abc123def4567890"));
        let lines = snapshot_outcome_lines(root.path(), &SnapshotOutcome::Ready(snapshot));
        let banner = lines.join("\n");

        assert!(
            banner.contains("Index is fresh"),
            "missing fresh line: {banner}"
        );
        // Short commit form is surfaced (12 chars), not the full 16-char fixture.
        assert!(
            banner.contains("commit abc123def456"),
            "missing indexed commit: {banner}"
        );
        assert!(
            banner.contains("loomweave analyze") && banner.contains("brand-new"),
            "Fresh banner must disclose the not-yet-indexed blind spot and point at \
             re-analyze: {banner}"
        );
    }

    #[test]
    fn fresh_banner_omits_commit_clause_when_run_recorded_none() {
        // A run analyzed outside a git repo has NULL analyzed_at_commit: the banner
        // must not invent a commit clause, but still carries the caveat.
        let root = tempfile::tempdir().unwrap();
        let (_db, snapshot) = fresh_snapshot(root.path(), None);
        let lines = snapshot_outcome_lines(root.path(), &SnapshotOutcome::Ready(snapshot));
        let banner = lines.join("\n");

        assert!(
            banner.contains("Index is fresh"),
            "missing fresh line: {banner}"
        );
        assert!(
            !banner.contains(", commit "),
            "must not fabricate a commit: {banner}"
        );
        assert!(
            banner.contains("brand-new"),
            "caveat must still be present: {banner}"
        );
    }

    #[test]
    fn stale_worktree_banner_names_untracked_source_and_remedy() {
        // In a git work tree, a mtime-fresh index with an untracked module yields
        // StaleWorktree (clarion-26c7e52027, ADR-045); the banner must say so
        // concretely and point at re-analyze, not the soft Fresh caveat.
        use std::process::Command;
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| -> bool {
            Command::new("git")
                .args(args)
                .current_dir(root.path())
                .status()
                .is_ok_and(|s| s.success())
        };
        if !git(&["init", "-q"]) {
            return; // git unavailable → skip
        }
        let _ = git(&["config", "user.email", "t@t"]);
        let _ = git(&["config", "user.name", "t"]);
        std::fs::write(root.path().join("a.py"), "x = 1\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);

        let db_dir = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(db_dir.path().join("loomweave.db")).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, source_file_path, created_at, updated_at) \
             VALUES ('python:module:a', 'python', 'module', 'a', 'a', '{}', 'a.py', \
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        // Brand-new untracked module the index never saw.
        std::fs::write(root.path().join("hub.py"), "y = 2\n").unwrap();

        let snapshot = project_snapshot(&conn, root.path());
        assert_eq!(
            snapshot.staleness(),
            Staleness::StaleWorktree,
            "fixture must be StaleWorktree: {snapshot:?}"
        );
        let lines = snapshot_outcome_lines(root.path(), &SnapshotOutcome::Ready(snapshot));
        let banner = lines.join("\n");
        assert!(
            banner.contains("does NOT reflect the working tree")
                && banner.contains("loomweave analyze"),
            "StaleWorktree banner must name the gap and the re-analyze remedy: {banner}"
        );
    }

    #[test]
    fn unbuilt_linked_worktree_banner_points_to_the_isolated_analyze_command() {
        use std::process::Command;

        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".weft/loomweave")).unwrap();
        std::fs::write(repo.join(".weft/loomweave/.gitignore"), "# marker\n").unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .expect("spawn git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feature", "../linked"],
        );
        let linked = root.path().join("linked");
        let ctx = loomweave_core::worktree::WorktreeContext::resolve(&linked)
            .expect("resolve linked context");

        let banner = snapshot_report(&linked).join("\n");

        assert!(
            banner.contains(&ctx.store_paths.db.display().to_string()),
            "the missing index location must name the routed isolated store: {banner}"
        );
        assert!(
            banner.contains(&format!(
                "loomweave worktree analyze -- {}",
                linked.display()
            )),
            "the remediation must build the isolated store: {banner}"
        );
        assert!(
            !banner.contains(&format!("loomweave install --path {}", linked.display())),
            "installing inside the linked checkout would create a decoy store: {banner}"
        );
    }
}
