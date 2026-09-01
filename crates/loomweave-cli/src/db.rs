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

use crate::atomic_fs::staging_file_in;
use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OpenFlags};

/// Resolve the store `project_root` actually reads: `WorktreeContext`'s
/// `store_paths.db` (worktree-index Task 7) — never a bare
/// `db_path(project_root)`, which for a linked worktree is the *source*
/// root's own store, a location `loomweave worktree analyze` never
/// populates. Falls back to the root-derived path on the one error
/// `WorktreeContext::resolve` can return (a non-UTF-8 path component).
fn resolve_effective_db_path(project_root: &Path) -> std::path::PathBuf {
    match loomweave_core::worktree::WorktreeContext::resolve(project_root) {
        Ok(ctx) => ctx.store_paths.db,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "resolve worktree context for db command failed; falling back to \
                 <project_root>/.weft/loomweave/loomweave.db"
            );
            loomweave_core::store::db_path(project_root)
        }
    }
}

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
    let db_path = resolve_effective_db_path(project_root);
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
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create backup output directory {}", parent.display()))?;
    // Exclusive staging (see `atomic_fs`): SQLite opens the path it is given,
    // so a guessable sibling name pre-planted as a symlink would have turned
    // the backup into a write-through. The `TempPath` unlinks the staging file
    // on every failure path, so an interrupted run leaves no debris.
    let staging = staging_file_in(parent, &staging_prefix(output))?.into_temp_path();

    run_backup(&db_path, &staging)?;
    staging
        .persist(output)
        .map_err(|err| err.error)
        .with_context(|| format!("rename backup staging -> {}", output.display()))?;
    println!("Backed up {} -> {}", db_path.display(), output.display());
    Ok(())
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
    let db_path = resolve_effective_db_path(project_root);
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

/// Prefix for the sibling staging file (`<output-name>.loomweave-backup.tmp-<random>`).
fn staging_prefix(output: &Path) -> String {
    let name = output
        .file_name()
        .map_or_else(|| "backup".to_owned(), |n| n.to_string_lossy().into_owned());
    format!("{name}.loomweave-backup.tmp-")
}

/// True if both paths denote the same on-disk file. Falls back to a lexical
/// comparison when a path does not yet exist (so it cannot be canonicalized).
fn paths_are_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn backup_never_follows_a_planted_staging_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        // A plain (non-git) project root resolves to the default store leaf.
        // Spelled out so the worktree store-path audit does not read this
        // test as an unclassified runtime resolution site.
        let live = project.join(".weft").join("loomweave").join("loomweave.db");
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&live).unwrap();
        conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (42);")
            .unwrap();
        drop(conn);

        let out_dir = dir.path().join("backups");
        std::fs::create_dir_all(&out_dir).unwrap();
        let output = out_dir.join("snap.db");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep\n").unwrap();
        // The staging name the PID-derived scheme used. SQLite opens whatever
        // path it is handed, so a symlink here was a write-through.
        let planted = out_dir.join(format!(
            "snap.db.loomweave-backup.tmp-{}",
            std::process::id()
        ));
        symlink(&victim, &planted).unwrap();

        super::backup(&project, &output, false).unwrap();

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep\n");
        assert!(
            !std::fs::symlink_metadata(&output)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let conn = rusqlite::Connection::open(&output).unwrap();
        let x: i64 = conn.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 42);
        assert!(
            std::fs::symlink_metadata(&planted)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
