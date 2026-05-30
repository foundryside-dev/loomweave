//! `clarion db` maintenance subcommands.
//!
//! Currently a single verb: `backup`, an online, WAL-safe copy of
//! `.clarion/clarion.db` (gap-register STO-04 / clarion-6d433b61ba).
//!
//! Why an online backup rather than `cp`: the live database runs in WAL mode,
//! so committed pages live in `clarion.db-wal` separately from the main file.
//! A naive file copy taken during a `clarion analyze` produces a *torn* copy —
//! the main file without its outstanding WAL frames. `rusqlite::backup::Backup`
//! reads through a real connection, so it captures a transactionally consistent
//! snapshot and writes it into a fresh single-file database (no WAL sidecar to
//! ship alongside).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OpenFlags};

/// Back up the project's `.clarion/clarion.db` to `output`.
///
/// The copy is taken with `rusqlite::backup::Backup` (a consistent online
/// snapshot) and staged into a sibling temp file that is renamed over `output`
/// only after the snapshot completes and passes `PRAGMA integrity_check`, so an
/// interrupted backup never leaves a half-written file at the destination.
///
/// # Errors
///
/// Returns an error if the source database is missing, if `output` already
/// exists and `force` is not set, if `output` resolves to the source database
/// itself, or if the backup / integrity check fails.
pub fn backup(project_root: &Path, output: &Path, force: bool) -> Result<()> {
    let db_path = project_root.join(".clarion").join("clarion.db");
    ensure!(
        db_path.exists(),
        "Clarion database not found at {}; run `clarion analyze` first",
        db_path.display()
    );

    // Refuse to overwrite the live database — both the obvious same-path case
    // and the canonicalized-alias case (symlink / `./` games).
    if paths_are_same(&db_path, output) {
        bail!("refusing to back up {} onto itself", db_path.display());
    }

    if output.exists() {
        ensure!(
            force,
            "{} already exists; pass --force to overwrite",
            output.display()
        );
    }

    // Stage into a sibling temp file so a crash mid-copy can never leave a
    // truncated file sitting at `output`. Renaming is atomic on the same
    // filesystem; staging as a sibling keeps us on it.
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create backup output directory {}", parent.display()))?;
    }
    let staging = staging_path(output);
    // Clear any stale staging file from a previous interrupted run.
    if staging.exists() {
        std::fs::remove_file(&staging)
            .with_context(|| format!("clear stale staging file {}", staging.display()))?;
    }

    let result = run_backup(&db_path, &staging);
    match result {
        Ok(()) => {
            std::fs::rename(&staging, output).with_context(|| {
                format!(
                    "rename backup {} -> {}",
                    staging.display(),
                    output.display()
                )
            })?;
            println!("Backed up {} -> {}", db_path.display(), output.display());
            Ok(())
        }
        Err(err) => {
            // Best-effort cleanup so a failed run leaves no debris behind.
            let _ = std::fs::remove_file(&staging);
            Err(err)
        }
    }
}

/// Run the online backup into `staging`, then verify the copy is intact.
fn run_backup(db_path: &Path, staging: &Path) -> Result<()> {
    let src = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open source database {}", db_path.display()))?;
    let mut dst = Connection::open(staging)
        .with_context(|| format!("open staging database {}", staging.display()))?;

    {
        let backup =
            rusqlite::backup::Backup::new(&src, &mut dst).context("initialise online backup")?;
        // Copy the whole database in steps of 256 pages with no pause between
        // steps; the source is read-only so there is no writer to yield to.
        backup
            .run_to_completion(256, Duration::from_millis(0), None)
            .context("run online backup to completion")?;
    }

    // Prove the copy is a structurally valid SQLite database before we promote
    // it over `output`. integrity_check returns the single row "ok" on success.
    let status: String = dst
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .context("integrity_check on backup copy")?;
    if status != "ok" {
        return Err(anyhow!("backup integrity_check failed: {status}"));
    }
    Ok(())
}

/// Sibling staging path for the atomic write (`<output>.clarion-backup.tmp-<pid>`).
fn staging_path(output: &Path) -> std::path::PathBuf {
    let mut name = output.as_os_str().to_os_string();
    name.push(format!(".clarion-backup.tmp-{}", std::process::id()));
    std::path::PathBuf::from(name)
}

/// True if both paths denote the same on-disk file. Falls back to a lexical
/// comparison when a path does not yet exist (so it cannot be canonicalized).
fn paths_are_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}
