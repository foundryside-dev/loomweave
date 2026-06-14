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
use std::path::Path;

use anyhow::{Context, Result, bail};
use fs2::FileExt;

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
    let lock_path = loomweave_dir.join(LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open analyze lock file {}", lock_path.display()))?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(AnalyzeLockGuard { _file: file }),
        Err(err) => {
            // fs2 returns ErrorKind::WouldBlock when another process holds
            // the lock; anything else is a real IO failure (e.g. NFS
            // without lockd). Surface both with the path so operators can
            // identify the conflict.
            let kind = err.kind();
            if kind == std::io::ErrorKind::WouldBlock {
                bail!(
                    "another `loomweave analyze` is already in progress against this project \
                     (lock held on {}). Wait for it to finish, or remove the lock file if \
                     no other process is running.",
                    lock_path.display()
                );
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
