//! `loomweave hook session-start` — fail-soft session-start orientation.
//!
//! Never returns an error to the caller: the `SessionStart` hook must never
//! block an agent's session start. All failures degrade to a printed note.

use std::path::Path;

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
    Ok(())
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
    let db_path = loomweave_core::store::db_path(project_root);
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
            let db_path = loomweave_core::store::db_path(project_root);
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
}
