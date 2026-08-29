//! Cross-process advisory lock for `loomweave analyze`.
//!
//! Two concurrent `loomweave analyze` processes against the same project
//! corrupt the run-attribution graph: each opens its own writer-actor,
//! each calls `BeginRun` (insert a fresh `runs` row in `status='running'`),
//! and each races on entity/edge inserts under `SQLite` WAL. The in-process
//! `ActorState::current_run` guard (loomweave-storage `writer.rs`) prevents
//! a single writer from issuing two `BeginRun`s; it does nothing across
//! processes.
//!
//! This module acquires an exclusive `fs2`-advisory lock on a dedicated
//! sentinel file `.weft/loomweave/loomweave.lock` for the duration of the analyze
//! run. The lock file is separate from `loomweave.db` so `SQLite`'s own
//! locking (per-connection, transaction-scoped) is independent. The
//! guard's `Drop` releases the OS-level lock.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use loomweave_core::worktree::WorktreeContext;

const LOCK_FILE_NAME: &str = "loomweave.lock";

/// RAII guard holding the analyze lock. Drop releases the OS lock.
///
/// **Drop order is load-bearing.** The guard must outlive the writer-actor's
/// `JoinHandle::await` in `analyze::run_with_options`; otherwise a second
/// `loomweave analyze` can grab the lock while writer-actor 1's final
/// transaction is still landing through WAL. `fs2`'s `File` impl unlocks
/// on file close, so dropping the `File` releases the OS lock; we rely on
/// Drop rather than an explicit unlock so panic and happy paths behave
/// identically.
#[must_use = "Drop releases the analyze lock — bind to a named variable"]
#[derive(Debug)]
pub(crate) struct AnalyzeLockGuard {
    _file: File,
}

#[derive(Debug)]
pub(crate) enum TryAnalyzeLock {
    Acquired(AnalyzeLockGuard),
    Held { lock_path: PathBuf },
}

fn lock_path_for_context(ctx: &WorktreeContext) -> Result<PathBuf> {
    if let Some(stable_id) = ctx.stable_id.as_deref() {
        // The path contract (`<repository-store>/worktrees/<stable-id>.lock`)
        // is defined once, in loomweave-core, and shared with the MCP
        // server's builder-liveness probe — never re-derived here.
        let lock_path = loomweave_core::worktree::linked_worktree_analyze_lock_path(
            &ctx.repository_store,
            stable_id,
        );
        let namespace = lock_path
            .parent()
            .expect("linked_worktree_analyze_lock_path always nests under worktrees/");
        std::fs::create_dir_all(namespace).with_context(|| {
            format!(
                "create worktree analyze-lock namespace {}",
                namespace.display()
            )
        })?;
        Ok(lock_path)
    } else {
        Ok(ctx.effective_store.join(LOCK_FILE_NAME))
    }
}

pub(crate) fn try_acquire_analyze_lock_for_context(
    ctx: &WorktreeContext,
) -> Result<TryAnalyzeLock> {
    let lock_path = lock_path_for_context(ctx)?;
    try_acquire_lock_path(&lock_path)
}

pub(crate) fn acquire_analyze_lock_for_context(ctx: &WorktreeContext) -> Result<AnalyzeLockGuard> {
    match try_acquire_analyze_lock_for_context(ctx)? {
        TryAnalyzeLock::Acquired(guard) => Ok(guard),
        TryAnalyzeLock::Held { lock_path } => bail!(
            "another `loomweave analyze` is already in progress against this project \
             (lock held on {}). Wait for it to finish.",
            lock_path.display()
        ),
    }
}

/// Acquire an exclusive cross-process lock on `<loomweave_dir>/loomweave.lock`.
///
/// `loomweave_dir` is the `.weft/loomweave/` directory inside the project root. The
/// lock file is created on first use (0-byte sentinel) and kept across
/// runs. The returned guard holds the lock for its lifetime.
///
/// # Errors
///
/// - The lock file cannot be opened (missing `.weft/loomweave/` directory,
///   permission denied, filesystem read-only).
/// - Another `loomweave analyze` process already holds the lock. Returns
///   an error containing the lock-file path so the operator can identify
///   the conflict.
pub(crate) fn acquire_analyze_lock(loomweave_dir: &Path) -> Result<AnalyzeLockGuard> {
    match try_acquire_analyze_lock(loomweave_dir)? {
        TryAnalyzeLock::Acquired(guard) => Ok(guard),
        TryAnalyzeLock::Held { lock_path } => bail!(
            "another `loomweave analyze` is already in progress against this project \
             (lock held on {}). Wait for it to finish.",
            lock_path.display()
        ),
    }
}

/// Non-blocking twin of [`acquire_analyze_lock`] that keeps "another
/// process holds the lock" (`Ok(Held)`, transient) distinct from "the lock
/// could not be taken at all" (`Err`: the sentinel cannot be opened, or the
/// filesystem refuses advisory locks — persistent, needs an operator). Callers
/// that report severity must not collapse the two.
pub(crate) fn try_acquire_analyze_lock(loomweave_dir: &Path) -> Result<TryAnalyzeLock> {
    try_acquire_lock_path(&loomweave_dir.join(LOCK_FILE_NAME))
}

fn try_acquire_lock_path(lock_path: &Path) -> Result<TryAnalyzeLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .with_context(|| format!("open analyze lock file {}", lock_path.display()))?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(TryAnalyzeLock::Acquired(AnalyzeLockGuard { _file: file })),
        Err(err) => {
            // fs2 returns ErrorKind::WouldBlock when another process holds
            // the lock; anything else is a real IO failure (e.g. NFS
            // without lockd). Surface both with the path so operators can
            // identify the conflict.
            let kind = err.kind();
            if kind == std::io::ErrorKind::WouldBlock {
                return Ok(TryAnalyzeLock::Held {
                    lock_path: lock_path.to_path_buf(),
                });
            }
            Err(err).with_context(|| {
                format!(
                    "acquire exclusive lock on {} (filesystem may not support advisory locks)",
                    lock_path.display()
                )
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two concurrent `acquire_analyze_lock` calls on the same `.weft/loomweave/`
    /// directory must fail the second call. This is the core STO-01
    /// invariant: a second analyze cannot start while the first holds
    /// the writer.
    #[test]
    fn second_acquire_fails_while_first_held() {
        let tmp = tempfile::tempdir().unwrap();
        let loomweave_dir = tmp.path();

        let first = acquire_analyze_lock(loomweave_dir).expect("first acquire");
        assert!(
            loomweave_dir.join(LOCK_FILE_NAME).exists(),
            "lock file created on first acquire"
        );

        let err = acquire_analyze_lock(loomweave_dir)
            .expect_err("second acquire must fail while first guard is held");
        let msg = format!("{err}");
        assert!(
            msg.contains("another `loomweave analyze`"),
            "error must name the conflict explicitly: {msg}"
        );
        drop(first);
    }

    /// Releasing the first lock (dropping the guard) must let the second
    /// acquire succeed. Guards the "we forgot to unlock on Drop" bug.
    #[test]
    fn second_acquire_succeeds_after_first_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let loomweave_dir = tmp.path();

        {
            let first = acquire_analyze_lock(loomweave_dir).expect("first acquire");
            drop(first);
        } // lock released on drop

        let second = acquire_analyze_lock(loomweave_dir)
            .expect("second acquire must succeed after first drops");
        drop(second);
    }

    /// The non-blocking probe must keep contention (`Held`) apart from a
    /// lock file that cannot be opened (`Err`): doctor grades them differently.
    #[test]
    fn try_acquire_distinguishes_held_from_unopenable() {
        let tmp = tempfile::tempdir().unwrap();
        let first = acquire_analyze_lock(tmp.path()).expect("first acquire");
        match try_acquire_analyze_lock(tmp.path()).expect("contention is Ok(Held)") {
            TryAnalyzeLock::Held { lock_path } => {
                assert_eq!(lock_path, tmp.path().join(LOCK_FILE_NAME));
            }
            TryAnalyzeLock::Acquired(_) => panic!("second acquire must not succeed"),
        }
        drop(first);

        let blocked = tempfile::tempdir().unwrap();
        // A directory squatting on the sentinel path: open() fails, no lock
        // is ever attempted, and that must surface as Err, not Held.
        std::fs::create_dir(blocked.path().join(LOCK_FILE_NAME)).unwrap();
        let err = try_acquire_analyze_lock(blocked.path()).expect_err("unopenable sentinel");
        let msg = format!("{err:#}");
        assert!(msg.contains("open analyze lock file"), "{msg}");
    }

    /// Missing `.weft/loomweave/` directory must surface as an IO error, not a
    /// `WouldBlock` masquerade. (Operator may have skipped `loomweave install`.)
    #[test]
    fn missing_loomweave_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("missing-loomweave-dir");
        let err = acquire_analyze_lock(&nonexistent).expect_err("missing dir must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("open analyze lock file"),
            "error must mention lock file open path: {msg}"
        );
    }
}
