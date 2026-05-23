//! Cross-process advisory lock for the writer-actor (gap-register STO-01).
//!
//! ## Why this exists
//!
//! ADR-011 names the writer-actor as the **sole** `SQLite` write connection for
//! a Clarion project. The actor model serialises writes inside a single
//! process, but it does nothing to prevent a *second* Clarion process from
//! opening its own writer-actor against the same `.clarion/clarion.db`.
//! A second concurrent `clarion analyze` can interleave with the first
//! actor's open transaction, flip another run's `status='running'` row, or
//! contend on the WAL in ways the per-run accounting never anticipated
//! (`status` flips, orphaned `runs` rows, partial-batch entity rows).
//!
//! The fix is a file-system advisory lock acquired before the writer-actor
//! is spawned. Any second writer-mode invocation against the same project
//! root fails fast with a clear error rather than corrupting state.
//!
//! ## Scope
//!
//! - **`clarion analyze`**: must hold the lock. The whole analyze pipeline
//!   is a writer-mode operation.
//! - **`clarion serve`**: must hold the lock **only** when it spawns the
//!   optional MCP LLM summary-cache writer. A serve invocation without an
//!   LLM provider is read-only and is intentionally permitted to run
//!   concurrently with another serve (or with an analyze — but see below;
//!   the reader pool is unaffected by the writer lock).
//! - **Reader pools** (HTTP read API, MCP read-only tools): not gated by
//!   this lock. `SQLite` WAL allows readers concurrent with a writer; the
//!   single-writer constraint is the only thing this lock enforces.
//!
//! ## Mechanism
//!
//! [`acquire_project_lock`] opens `.clarion/clarion.lock` and asks
//! `fs2::FileExt::try_lock_exclusive` for an exclusive non-blocking lock.
//! On POSIX this is a per-fd `flock(2)`; on Windows it is `LockFileEx`
//! with `LOCKFILE_FAIL_IMMEDIATELY`. The lock is released when the
//! returned [`ProjectLock`] is dropped (POSIX semantics: closing the
//! file descriptor releases the flock).
//!
//! Callers must bind the returned guard to a local that lives at least as
//! long as the writer-actor. A `let _ = acquire_project_lock(...)?;` is a
//! bug (the temporary is dropped at the end of the statement); `let _lock
//! = acquire_project_lock(...)?;` is correct.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

/// Filename appended to `<project_root>/.clarion/` for the project advisory lock.
///
/// **Stated basis**: ADR-011 declares a single writer-actor per project DB.
/// STO-01 in `docs/implementation/v1.0-tag-cut/gap-register.md` identifies
/// the missing cross-process enforcement of that invariant. Co-locating
/// the lockfile under `.clarion/` keeps the on-disk surface uniform: all
/// project state — `clarion.db`, `instance.json`, the lock — lives in a
/// single operator-visible directory.
///
/// **Override surface**: not configurable. The lockfile is an
/// implementation detail of the writer-actor invariant, not an operator
/// tuning knob. Operators who need to inspect lock ownership use `lsof`
/// or `fuser` against the path.
///
/// **Retune trigger**: change only if `.clarion/` itself moves (which
/// would be a wider ADR-011 / project-layout decision). Renaming the
/// lockfile alone has no operator benefit.
///
/// **Coupling**: `clarion-cli/src/analyze.rs` and the writer-spawning
/// branch of `clarion-cli/src/serve.rs` both acquire this lock before
/// `Writer::spawn`. A renamed file path would let a stale CLI (still
/// looking at the old path) coexist with a newer one — defeating the
/// lock. See `docs/clarion/adr/ADR-035-operational-tuning-discipline.md`
/// (in-flight at v1.0 tag-cut) for the declaration-discipline
/// requirement that produced this annotation block.
pub(crate) const PROJECT_LOCK_FILENAME: &str = "clarion.lock";

/// Subdirectory under the project root where Clarion writes its state.
///
/// **Stated basis**: REQ-CORE-04 / ADR-011 — Clarion state is local to a
/// project under a single dotted directory.
///
/// **Override surface**: not configurable; `clarion install` is the
/// authoritative creator of this directory.
///
/// **Retune trigger**: would require an ADR change to the project
/// on-disk layout.
///
/// **Coupling**: `analyze.rs` and `serve.rs` both derive the database
/// path from `<project_root>/.clarion/clarion.db`. This constant exists
/// solely so the lockfile path stays in lockstep.
const PROJECT_STATE_SUBDIR: &str = ".clarion";

/// RAII guard for the project-wide writer advisory lock.
///
/// The lock is released when this struct is dropped — either when the
/// guard goes out of scope (normal completion) or when the stack unwinds
/// past it (panic).
///
/// Bind to a named local (`let _lock = acquire_project_lock(...)?;`)
/// rather than `let _ = ...` so the guard lives for the writer-actor's
/// lifetime instead of being dropped at the end of the let-statement.
#[derive(Debug)]
pub(crate) struct ProjectLock {
    /// The held file handle. Closing the handle releases the POSIX
    /// `flock`. We retain the handle in the struct rather than calling
    /// `fs2::FileExt::unlock` explicitly so a panic between acquisition
    /// and a hypothetical explicit unlock still releases the lock.
    _file: File,
    /// The lock-file path, kept for diagnostic logging in the Drop impl.
    path: PathBuf,
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        // We intentionally do not call `FileExt::unlock` here. POSIX
        // releases the lock when the last fd referencing the inode is
        // closed; the `File` field is closed as part of normal struct
        // drop. Calling `unlock` explicitly would be redundant and would
        // surface an additional I/O error path we cannot meaningfully
        // recover from in `Drop`.
        tracing::trace!(
            lockfile = %self.path.display(),
            "released Clarion project lock"
        );
    }
}

/// Acquire the exclusive project-writer lock at `<project_root>/.clarion/clarion.lock`.
///
/// The caller owns the returned [`ProjectLock`]; dropping it releases the
/// lock. The lock must be acquired **before** spawning the writer-actor
/// and held until after the writer's `JoinHandle` resolves.
///
/// # Errors
///
/// - The `.clarion/` directory does not exist (caller should have run
///   `clarion install` first). Returned as `anyhow::Error` with the
///   resolved path for the operator.
/// - The lock file cannot be created or opened (filesystem permission
///   error, full disk, etc.).
/// - Another `clarion` process already holds the lock. The error
///   message is operator-facing and names both the project root and the
///   lockfile path.
pub(crate) fn acquire_project_lock(project_root: &Path) -> Result<ProjectLock> {
    let clarion_dir = project_root.join(PROJECT_STATE_SUBDIR);
    if !clarion_dir.is_dir() {
        anyhow::bail!(
            "{} has no {PROJECT_STATE_SUBDIR}/ directory. Run `clarion install` first.",
            project_root.display()
        );
    }
    let lock_path = clarion_dir.join(PROJECT_LOCK_FILENAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open Clarion project lockfile {}", lock_path.display()))?;

    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => {
            tracing::debug!(
                lockfile = %lock_path.display(),
                "acquired Clarion project lock"
            );
            Ok(ProjectLock {
                _file: file,
                path: lock_path,
            })
        }
        Err(err) => {
            // fs2 surfaces `ErrorKind::WouldBlock` for "already locked"
            // and other variants for genuine I/O failures. We treat
            // *every* failure as "another process holds the lock" only
            // when the kernel says so; otherwise we surface the I/O
            // error verbatim.
            if err.kind() == std::io::ErrorKind::WouldBlock {
                anyhow::bail!(
                    "another clarion analyze or serve is in progress against this \
                     project (lockfile: {})",
                    lock_path.display()
                );
            }
            Err(anyhow::Error::from(err)
                .context(format!("acquire exclusive lock on {}", lock_path.display())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_project(tmp: &TempDir) -> PathBuf {
        let root = tmp.path().to_path_buf();
        fs::create_dir_all(root.join(PROJECT_STATE_SUBDIR)).expect("create .clarion dir");
        root
    }

    #[test]
    fn acquire_succeeds_when_no_other_holder() {
        let tmp = TempDir::new().expect("tempdir");
        let root = make_project(&tmp);
        let lock = acquire_project_lock(&root).expect("first acquisition");
        // Lock-file exists after acquisition.
        assert!(
            root.join(PROJECT_STATE_SUBDIR)
                .join(PROJECT_LOCK_FILENAME)
                .exists()
        );
        drop(lock);
    }

    #[test]
    fn second_acquisition_in_same_process_fails_fast() {
        // Note: POSIX flock semantics allow the same process to upgrade
        // its own lock, but fs2 uses `flock(LOCK_EX | LOCK_NB)` and we
        // open a fresh `File` per call — distinct fds in the same
        // process still contend. (This is the intended semantics; if it
        // ever stopped holding we'd want to know.)
        let tmp = TempDir::new().expect("tempdir");
        let root = make_project(&tmp);
        let _first = acquire_project_lock(&root).expect("first acquisition");
        let err = acquire_project_lock(&root).expect_err("second must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("another clarion analyze or serve is in progress"),
            "unexpected error message: {msg}"
        );
        assert!(
            msg.contains(PROJECT_LOCK_FILENAME),
            "error message must name the lockfile: {msg}"
        );
    }

    #[test]
    fn lock_released_on_drop() {
        let tmp = TempDir::new().expect("tempdir");
        let root = make_project(&tmp);
        {
            let _first = acquire_project_lock(&root).expect("first acquisition");
        }
        // After the guard drops, a fresh acquisition succeeds.
        let _second = acquire_project_lock(&root).expect("re-acquire after drop");
    }

    #[test]
    fn missing_clarion_dir_is_a_clear_error() {
        let tmp = TempDir::new().expect("tempdir");
        // Note: we do NOT create `.clarion/`.
        let err = acquire_project_lock(tmp.path()).expect_err("should fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(".clarion/"),
            "error must mention the missing dir: {msg}"
        );
        assert!(
            msg.contains("clarion install"),
            "error must suggest install: {msg}"
        );
    }
}
