//! `loomweave db` maintenance subcommands.
//!
//! Currently a single verb: `backup`, an online, WAL-safe copy of
//! `.weft/loomweave/loomweave.db` (gap-register STO-04 / clarion-6d433b61ba).
//!
//! Why an online backup rather than `cp`: the live database runs in WAL mode,
//! so committed pages live in `loomweave.db-wal` separately from the main file.
//! A naive file copy taken during a `loomweave analyze` produces a *torn* copy —
//! the main file without its outstanding WAL frames. `rusqlite::backup::Backup`
//! reads through a real connection, so it captures a transactionally consistent
//! snapshot and writes it into a fresh single-file database (no WAL sidecar to
//! ship alongside).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OpenFlags};

/// Back up the project's `.weft/loomweave/loomweave.db` to `output`.
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
    let db_path = loomweave_core::store::db_path(project_root);
    ensure!(
        db_path.exists(),
        "Loomweave database not found at {}; run `loomweave analyze` first",
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

/// Force a `PRAGMA wal_checkpoint(TRUNCATE)` on the working store so the on-disk
/// `loomweave.db` becomes a clean point-in-time artifact: outstanding WAL frames
/// are flushed into the main file and the `-wal` sidecar is reset to zero length.
///
/// `analyze` already TRUNCATE-checkpoints at each committed run boundary (the
/// `loomweave-storage` writer), so the analyze path needs no manual checkpoint.
/// This verb is the on-demand companion for the `serve` summary-write path, where
/// the WAL can grow between the PASSIVE `wal_autocheckpoint` cadence and a
/// snapshot / backup / demo (Weft C-2 WAL-hygiene). Best-effort on contention: a
/// live reader (a `serve` reader-pool connection) can hold TRUNCATE back to a
/// `busy` result — the committed frames are already durable, so we report the
/// partial outcome rather than fail.
pub fn checkpoint(project_root: &Path) -> Result<()> {
    let db_path = loomweave_core::store::db_path(project_root);
    ensure!(
        db_path.exists(),
        "Loomweave database not found at {}; run `loomweave analyze` first",
        db_path.display()
    );

    let conn = Connection::open(&db_path)
        .with_context(|| format!("open database {}", db_path.display()))?;
    // `PRAGMA wal_checkpoint(TRUNCATE)` returns one row:
    //   (busy, log_frames, checkpointed_frames).
    // busy = 1 means a concurrent connection blocked the WAL reset.
    let (busy, log_frames, checkpointed): (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .with_context(|| format!("checkpoint {}", db_path.display()))?;

    if busy != 0 {
        println!(
            "Checkpoint incomplete: a concurrent reader held the WAL back (busy=1). \
             Committed data is durable; re-run when `serve` is idle to fully reset the WAL."
        );
    } else {
        println!(
            "Checkpointed {checkpointed}/{log_frames} WAL frame(s) into {} and truncated the WAL.",
            db_path.display()
        );
    }
    Ok(())
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

/// Sibling staging path for the atomic write (`<output>.loomweave-backup.tmp-<pid>`).
fn staging_path(output: &Path) -> std::path::PathBuf {
    let mut name = output.as_os_str().to_os_string();
    name.push(format!(".loomweave-backup.tmp-{}", std::process::id()));
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
