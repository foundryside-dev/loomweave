//! `clarion hook session-start` — fail-soft session-start orientation.
//!
//! Never returns an error to the caller: the `SessionStart` hook must never
//! block an agent's session start. All failures degrade to a printed note.

use std::path::Path;

use clarion_mcp::snapshot::{ProjectSnapshot, Staleness, missing_db_snapshot, project_snapshot};
use rusqlite::{Connection, OpenFlags};

/// Run `clarion hook session-start`. Always returns `Ok(())`.
///
/// The `anyhow::Result` return type is intentional even though no `Err` is
/// ever produced: it keeps the `main.rs` dispatch arm uniform with the other
/// subcommands and documents the fail-soft contract at the type level.
#[allow(clippy::unnecessary_wraps)]
pub fn session_start(path: &Path) -> anyhow::Result<()> {
    // (1) Re-sync the skill pack ONLY if it's already installed in at least one
    //     skill root, and drifted. A bare session-start never bootstraps a
    //     never-installed project — that's `clarion install --skills`'s job. Note
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

/// What [`load_snapshot`] could establish about the `.clarion/` index.
///
/// A *missing* db and a *present-but-unreadable* db are deliberately distinct:
/// the missing case nudges toward `install` + `analyze`, but that advice is
/// wrong for a present-but-corrupt/locked db (`install` refuses while `.clarion/`
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
        .join(".claude/skills/clarion-workflow/SKILL.md")
        .exists()
        || project_root
            .join(".agents/skills/clarion-workflow/SKILL.md")
            .exists();
    if !installed {
        return;
    }
    if let Err(err) = crate::skill_pack::install_skill_pack(project_root) {
        tracing::warn!(error = %err, "clarion-workflow skill resync failed");
    }
}

fn load_snapshot(project_root: &Path) -> SnapshotOutcome {
    let db_path = project_root.join(".clarion").join("clarion.db");
    if !db_path.exists() {
        return SnapshotOutcome::Ready(missing_db_snapshot());
    }
    let conn = match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(error = %err, "open .clarion/clarion.db read-only failed");
            return SnapshotOutcome::DbUnreadable;
        }
    };
    // `Connection::open_with_flags(.. READ_ONLY)` lazily succeeds even on a
    // non-SQLite file ("NOT A SQLITE DB" opens fine); the corruption only
    // surfaces at first read. Probe with a cheap query so a present-but-corrupt
    // db is classified as unreadable rather than silently reported as 0 counts
    // (which would otherwise print the wrong "no analysis yet" nudge).
    if let Err(err) = conn.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0)) {
        tracing::warn!(error = %err, "probe read of .clarion/clarion.db failed");
        return SnapshotOutcome::DbUnreadable;
    }
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    SnapshotOutcome::Ready(project_snapshot(&conn, &root))
}

fn print_snapshot(project_root: &Path, outcome: &SnapshotOutcome) {
    let snapshot = match outcome {
        SnapshotOutcome::Ready(snapshot) => snapshot,
        SnapshotOutcome::DbUnreadable => {
            let db_path = project_root.join(".clarion").join("clarion.db");
            println!(
                "Clarion: an index exists at {} but could not be opened (it may be \
                 corrupt, locked by another process, or unreadable). Check permissions, \
                 ensure no other clarion process holds it, or remove .clarion/ and re-run \
                 `clarion install` + `clarion analyze`. (Run with RUST_LOG=warn for the \
                 open error.)",
                db_path.display()
            );
            return;
        }
    };
    if !snapshot.db_present {
        println!(
            "Clarion: no index at {}/.clarion/clarion.db. \
             Run `clarion install --path {}` then `clarion analyze {}`.",
            project_root.display(),
            project_root.display(),
            project_root.display()
        );
        return;
    }
    println!(
        "Clarion index: {} entities, {} subsystems, {} findings.",
        snapshot.entity_count, snapshot.subsystem_count, snapshot.finding_count
    );
    if snapshot.degraded {
        // A backing query folded to a safe default, so the counts above may
        // understate a populated index. Distinct from the present-but-empty
        // case (which is not degraded). Operator detail is in the warn log.
        println!(
            "Clarion: ⚠ snapshot is degraded — at least one index query failed and \
             the counts above may be incomplete. (Run with RUST_LOG=warn for details.)"
        );
    }
    match snapshot.staleness {
        Staleness::Fresh => {
            println!(
                "Index is fresh (last analyzed {}). Ask Clarion before re-exploring \
                 the tree; see the clarion-workflow skill.",
                snapshot.last_analyzed_at.as_deref().unwrap_or("unknown")
            );
        }
        Staleness::Stale => {
            println!(
                "Index may be stale: source files changed since the last run. \
                 Run `clarion analyze {}` to refresh.",
                project_root.display()
            );
        }
        Staleness::NeverAnalyzed => {
            println!(
                "No analysis recorded yet. Run `clarion analyze {}` to build the index.",
                project_root.display()
            );
        }
        Staleness::Unknown => {
            println!(
                "Index freshness unknown. If briefings look empty, run \
                 `clarion analyze {}`.",
                project_root.display()
            );
        }
    }
}
