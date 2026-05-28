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
    // (1) Re-sync the skill pack ONLY if it's already installed and drifted.
    //     We don't install where absent — that's `clarion install --skills`'s
    //     job. A drift repair keeps an installed copy honest across upgrades.
    resync_skill_if_present(path);

    // (2) Snapshot.
    let snapshot = load_snapshot(path);
    print_snapshot(path, &snapshot);
    Ok(())
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

fn load_snapshot(project_root: &Path) -> ProjectSnapshot {
    let db_path = project_root.join(".clarion").join("clarion.db");
    if !db_path.exists() {
        return missing_db_snapshot();
    }
    match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => {
            let root = project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf());
            project_snapshot(&conn, &root)
        }
        Err(err) => {
            tracing::warn!(error = %err, "open .clarion/clarion.db read-only failed");
            missing_db_snapshot()
        }
    }
}

fn print_snapshot(project_root: &Path, snapshot: &ProjectSnapshot) {
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
