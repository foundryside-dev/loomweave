//! Cleanup sweep for worktree-isolated stores (worktree-index Task 6; see
//! `docs/superpowers/specs/2026-07-18-loomweave-worktree-indexes-design.md`,
//! "Cleanup").
//!
//! [`sweep_worktree_stores`] decides *what* gets deleted; every deletion it
//! performs routes through [`crate::worktree::confine`]'s confined
//! primitive, which decides *how* (never a bare `remove_dir_all` on a
//! string path — see that module's docs for the safety property). This
//! module enumerates two things and diffs them:
//!
//! - the **registered** set: every stable ID Git currently has an
//!   administrative entry for, and
//! - the **candidate** set: every `wt-[0-9a-f]{64}` directory that exists
//!   under `<repository-store>/worktrees/` right now.
//!
//! A candidate absent from the registered set is deleted; everything else
//! is left alone. [`sweep_best_effort`] is the entry point `serve` and
//! `analyze` actually call — its `()` return type is the mechanism, not a
//! convention, by which a sweep failure can never propagate to either
//! caller's own result.
//!
//! **Deriving the registered set.** `git worktree list --porcelain` does
//! not expose an entry's administrative directory name, and the stable ID
//! is a hash of exactly that (see
//! [`loomweave_core::worktree::stable_id_for_admin_identity`]). So this
//! module never runs `git worktree list`: it resolves the repository's
//! common Git directory once (one hardened `git rev-parse`, from
//! `ctx.primary_root`), then reads that directory's own `worktrees/`
//! administrative subdirectory with a single, cheap `readdir` — no
//! subprocess per entry. Every direct child there names one *registered*
//! worktree, whether or not that worktree's own working tree currently
//! exists on disk (a registered-but-prunable worktree on a temporarily
//! unmounted volume reads as registered, never as "unregistered" — see the
//! module docs on why probing each working tree with `rev-parse
//! --absolute-git-dir` would get this wrong). A Git-locked worktree's admin
//! entry is unaffected by its `locked` marker file, so it is preserved by
//! this same mechanism with no special-casing.
//!
//! **Abort semantics.** If the common Git directory cannot be resolved (no
//! `git`, not a repository, or the command fails), or if its `worktrees/`
//! administrative directory cannot be read once resolved, the sweep aborts
//! and deletes nothing — never partial, never a best-effort subset.
//!
//! **The override case.** A `[loomweave].store_dir` override
//! (`loomweave_core::store::store_dir_override`) is not scoped to one
//! repository: an absolute override can be shared between unrelated
//! repositories, so under an active override this repository's registered
//! set must never be used to authorize deleting another repository's `wt-*`
//! stores sitting in the same shared namespace. When an override is active
//! for `ctx.primary_root`, this module enumerates exactly as it otherwise
//! would and logs every candidate it *would* delete, but deletes nothing —
//! see [`SweepOutcome::ReportOnly`].
//!
//! **The accepted race.** There are no activity locks, so a store can be
//! swept while a `serve` process holds it open — this is a deliberate
//! simplification over the original design's lock ordering, not an
//! oversight; see the design doc's "Removal semantics" and this feature's
//! Non-goals. A store swept while its worktree still exists costs one
//! re-analyze (the open inode keeps the running server working). A store
//! swept after `git worktree remove` has nothing left to rebuild from; a
//! live `serve` against it surfaces `source-root-missing` (Task 5) rather
//! than stale answers.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use loomweave_core::store::store_dir_override;
use loomweave_core::worktree::{WorktreeContext, stable_id_for_admin_identity};
use tracing::{debug, info, warn};

use crate::worktree::confine::{DeleteOutcome, WorktreesRoot, matches_worktree_store_grammar};
use crate::worktree::store::WORKTREES_DIR_NAME;

/// The non-blocking advisory lock that serializes concurrent sweeps —
/// `<repository-store>/worktrees/gc.lock`, per the design's on-disk layout.
const GC_LOCK_FILE_NAME: &str = "gc.lock";

/// What one [`sweep_worktree_stores`] call did, for tests and diagnostics.
/// Callers at `serve` startup and after `analyze` completes go through
/// [`sweep_best_effort`] instead, which discards this — every variant here
/// is a normal, logged outcome, never a signal that the caller's own
/// operation should fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOutcome {
    /// `<repository-store>/worktrees/` does not exist yet — no worktree
    /// store has ever been created under this repository, so there is
    /// nothing to enumerate. The common case; short-circuited before any
    /// `git` subprocess or lock is touched.
    NoWorktreesStore,
    /// The non-blocking `gc.lock` was already held by another process;
    /// this sweep skipped entirely rather than wait.
    GcLockHeld,
    /// `gc.lock` could not be opened or locked for a reason other than
    /// contention (permission denied, read-only filesystem, ...).
    GcLockUnavailable,
    /// The repository's common Git directory could not be resolved (`git`
    /// missing, `ctx.primary_root` is not inside a Git repository, or the
    /// command failed) — aborted, nothing deleted.
    GitCommonDirUnresolvable,
    /// The common Git directory's own `worktrees/` administrative
    /// directory could not be read — aborted, nothing deleted.
    AdminDirUnreadable,
    /// `<repository-store>/worktrees/` itself could not be read — either
    /// while enumerating store candidates (this return site fires *before*
    /// the admin-directory read below) or while re-opening it as a confined
    /// root right before deletion (this return site fires *after* the
    /// admin-directory read has already succeeded) — aborted, nothing
    /// deleted either way. Distinct from [`Self::AdminDirUnreadable`]: this
    /// is Loomweave's own store directory, not Git's administrative one.
    StoreDirUnreadable,
    /// A `[loomweave].store_dir` override is active for `ctx.primary_root`:
    /// candidates were enumerated and logged, nothing was deleted.
    ReportOnly {
        /// Candidate stable IDs (directory names) that would have been
        /// deleted had no override been active.
        would_delete: Vec<String>,
    },
    /// The sweep ran to completion.
    Completed {
        /// Candidate stable IDs actually deleted.
        deleted: Vec<String>,
        /// Candidate stable IDs left in place because they are still
        /// registered.
        preserved: Vec<String>,
    },
}

/// Run the cleanup sweep and log the outcome; never panics, never returns a
/// `Result` — this is the type-level guarantee that a sweep failure cannot
/// propagate to `serve` or `analyze`'s own result. Call this, not
/// [`sweep_worktree_stores`], from every production call site.
pub fn sweep_best_effort(ctx: &WorktreeContext) {
    let outcome = sweep_worktree_stores(ctx);
    debug!(?outcome, "worktree cleanup sweep outcome");
}

/// Enumerate `<repository-store>/worktrees/` and delete (via the confined
/// primitive) any `wt-[0-9a-f]{64}` store whose stable ID is not currently
/// registered with Git — see the module docs for the full algorithm,
/// abort semantics, and the override's report-only behavior.
///
/// Returns a [`SweepOutcome`] describing what happened; never panics. Every
/// branch is logged via `tracing` as it happens, so the returned value
/// exists for tests and for [`sweep_best_effort`]'s debug log, not because a
/// caller needs to branch on it — deliberately not `#[must_use]`, since
/// discarding it (as [`sweep_best_effort`] effectively does after logging)
/// is the expected production usage, not a bug.
pub fn sweep_worktree_stores(ctx: &WorktreeContext) -> SweepOutcome {
    let worktrees_dir = ctx.repository_store.join(WORKTREES_DIR_NAME);
    if !worktrees_dir.is_dir() {
        return SweepOutcome::NoWorktreesStore;
    }

    let _gc_lock = match acquire_gc_lock(&worktrees_dir) {
        Ok(GcLock::Acquired(file)) => file,
        Ok(GcLock::Held) => {
            info!(
                worktrees_dir = %worktrees_dir.display(),
                "worktree cleanup sweep: gc.lock is held by another process; skipping"
            );
            return SweepOutcome::GcLockHeld;
        }
        Err(err) => {
            warn!(
                worktrees_dir = %worktrees_dir.display(),
                error = %err,
                "worktree cleanup sweep: could not acquire gc.lock; skipping this cycle"
            );
            return SweepOutcome::GcLockUnavailable;
        }
    };

    let Some(common_dir) = git_common_dir(&ctx.primary_root) else {
        warn!(
            primary_root = %ctx.primary_root.display(),
            "worktree cleanup sweep: could not resolve the repository's common Git directory; \
             aborting, nothing deleted"
        );
        return SweepOutcome::GitCommonDirUnresolvable;
    };

    // ORDERING INVARIANT — candidates are read BEFORE the registered set,
    // never the reverse. Do not swap this back without re-deriving both
    // directions of the race below; a prior version of this function read
    // registered first and had a real bug in the unsafe direction.
    //
    // `ensure_isolated_store` (Task 3) always creates a worktree's isolated
    // store *after* `git worktree add` has already created its admin entry
    // (see `store_created_for_a_just_registered_worktree_is_preserved` in
    // `tests/worktree_sweep.rs`, which pins that ordering). So between this
    // function's two reads, only two kinds of change are possible for any
    // one worktree: it goes from "not yet registered, no store" to
    // "registered, store exists" (a fresh `git worktree add` +
    // `ensure_isolated_store` completing mid-sweep), or from "registered,
    // store exists" to "unregistered, store still exists" (`git worktree
    // remove`, which deletes the admin entry but has no idea this store
    // directory even exists).
    //
    // Reading registered FIRST (the old, unsafe order) gets the first case
    // wrong: a worktree registered and store-created entirely within the
    // window between the two reads is captured as "not registered" by the
    // (already-read) registered set, but its now-existing store IS captured
    // by the (later-read) candidate set — misclassified as unregistered,
    // and deleted: a live, just-registered worktree's store destroyed by
    // the sweep. Reading candidates FIRST closes that direction: a store
    // that does not exist yet at the candidate read is simply absent from
    // the candidate set and not considered at all this cycle — the next
    // sweep, run after the race has resolved one way or the other, sees it
    // correctly either way.
    //
    // The residual direction — a worktree unregistered (`git worktree
    // remove`) inside the window — has candidates (read first) capturing
    // the store as present, and registered (read second, after the
    // removal) correctly reporting it absent, so it IS deleted this cycle
    // rather than the next. That is not a hazard: the design's accepted
    // no-activity-lock race already treats "worktree just removed, its
    // store swept promptly" as correct (see the module docs, "The accepted
    // race" — nothing can rebuild that store once the worktree is gone
    // either way, one sweep cycle earlier changes nothing).
    let candidates = match store_candidates(&worktrees_dir) {
        Ok(names) => names,
        Err(err) => {
            warn!(
                worktrees_dir = %worktrees_dir.display(),
                error = %err,
                "worktree cleanup sweep: could not enumerate worktree store candidates; \
                 aborting, nothing deleted"
            );
            return SweepOutcome::StoreDirUnreadable;
        }
    };

    let admin_dir = common_dir.join(WORKTREES_DIR_NAME);
    let registered = match registered_stable_ids(&admin_dir) {
        Ok(ids) => ids,
        Err(err) => {
            warn!(
                admin_dir = %admin_dir.display(),
                error = %err,
                "worktree cleanup sweep: could not read the Git administrative worktrees \
                 directory; aborting, nothing deleted"
            );
            return SweepOutcome::AdminDirUnreadable;
        }
    };

    let (to_delete, preserved): (Vec<String>, Vec<String>) = candidates
        .into_iter()
        .partition(|name| !registered.contains(name));

    if store_dir_override(&ctx.primary_root).is_some() {
        for name in &to_delete {
            info!(
                candidate = name.as_str(),
                "worktree cleanup sweep: store_dir override active; would delete this \
                 unregistered candidate, but report-only mode deletes nothing"
            );
        }
        return SweepOutcome::ReportOnly {
            would_delete: to_delete,
        };
    }

    let root = match WorktreesRoot::open(&worktrees_dir) {
        Ok(root) => root,
        Err(err) => {
            warn!(
                worktrees_dir = %worktrees_dir.display(),
                error = %err,
                "worktree cleanup sweep: could not pin the worktrees/ directory handle; \
                 aborting, nothing deleted"
            );
            return SweepOutcome::StoreDirUnreadable;
        }
    };

    let mut deleted = Vec::new();
    for name in &to_delete {
        match root.delete_worktree_store(name, "worktree no longer registered") {
            DeleteOutcome::Deleted => deleted.push(name.clone()),
            DeleteOutcome::Refused(reason) => {
                warn!(
                    candidate = name.as_str(),
                    reason = %reason,
                    "worktree cleanup sweep: confined deletion refused; leaving candidate in place"
                );
            }
            DeleteOutcome::UnsupportedPlatform => {
                warn!(
                    candidate = name.as_str(),
                    "worktree cleanup sweep: no confined-deletion mechanism on this platform; \
                     leaving candidate in place"
                );
            }
        }
    }

    info!(
        deleted = deleted.len(),
        preserved = preserved.len(),
        "worktree cleanup sweep complete"
    );
    SweepOutcome::Completed { deleted, preserved }
}

/// Outcome of trying to acquire the non-blocking `gc.lock`.
enum GcLock {
    /// The lock was acquired; the open, locked file must stay alive (bound
    /// to a variable, not immediately dropped) for the duration of the
    /// sweep — dropping it releases the OS lock.
    Acquired(File),
    /// Another process already holds it.
    Held,
}

/// Try to acquire the exclusive, non-blocking `gc.lock` under
/// `worktrees_dir`, creating the (0-byte) sentinel file on first use.
fn acquire_gc_lock(worktrees_dir: &Path) -> io::Result<GcLock> {
    let lock_path = worktrees_dir.join(GC_LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(GcLock::Acquired(file)),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(GcLock::Held),
        Err(err) => Err(err),
    }
}

/// [`loomweave_core::hardened_git::hardened_git_command`], plus one narrow
/// guard this module needs that `hardened_git_command` itself does not
/// (yet) provide: `GIT_DIR`, `GIT_COMMON_DIR`, and `GIT_WORK_TREE` are
/// explicitly removed from the child's environment.
///
/// **The hazard.** `-C <dir>` does not override an *exported* `GIT_DIR`: if
/// the Loomweave process inherits `GIT_DIR` from its parent — a Git hook
/// running in a different repository, `git rebase --exec 'loomweave ...'`,
/// a CI runner that exports it for its own purposes — `git rev-parse
/// --git-common-dir` answers for that FOREIGN repository, not
/// `primary_root`'s. Combined with [`registered_stable_ids`]'s
/// `NotFound` → empty-registered-set rule (see its doc comment), a foreign
/// repository that happens to have no `worktrees/` admin directory of its
/// own — the common case — makes the registered set empty, so *every* real
/// `wt-*` store under this repository reads as unregistered and gets
/// deleted, live ones included. `GIT_COMMON_DIR` and `GIT_WORK_TREE` are
/// stripped alongside `GIT_DIR` because either can also redirect Git's
/// discovery away from `primary_root`.
///
/// `Command::env_remove` is unconditional: the child process will not see
/// the named variable regardless of what this process's own environment
/// contains, so this closes the hazard completely for the one `git`
/// invocation the sweep makes — see
/// `git_common_dir_command_strips_foreign_git_env_vars` below, which
/// asserts exactly that on the constructed [`std::process::Command`]
/// without needing to mutate any real environment.
///
/// This is a narrow, sweep-local guard, not a general fix: full
/// Git-environment sanitization for `hardened_git_command` itself is
/// tracked separately (clarion-9202f4acec) and is deliberately not folded
/// in here — this module's own deletion-adjacent path needed to stop
/// trusting the ambient environment now, independent of when that broader
/// work lands.
fn hardened_git_command_for_sweep(dir: &Path) -> std::process::Command {
    let mut command = loomweave_core::hardened_git::hardened_git_command(dir);
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE");
    command
}

/// Resolve `primary_root`'s common Git directory via one hardened `git
/// rev-parse` — `None` on any failure (missing `git`, not a repository, a
/// non-zero exit, or non-UTF-8 output), which every caller treats as an
/// abort signal, never a hard error.
fn git_common_dir(primary_root: &Path) -> Option<PathBuf> {
    let output = hardened_git_command_for_sweep(primary_root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    let candidate = Path::new(text);
    Some(if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        // Old `git` without full `--path-format=absolute` support; resolve
        // relative to the directory the command ran in, same fallback
        // `loomweave_core::worktree::WorktreeContext::resolve` itself uses.
        primary_root.join(candidate)
    })
}

/// Read every direct-child directory name of `admin_dir` (the common Git
/// directory's own `worktrees/`) and hash each one into the stable ID it
/// names — the *registered* set. One `readdir`, no subprocess per entry;
/// see the module docs for why this, and not `git worktree list`, is the
/// source of truth.
///
/// `admin_dir` not existing at all is **not** an enumeration failure: Git
/// only creates `<common-git-dir>/worktrees/` lazily on the first `git
/// worktree add`, and — verified empirically against a real `git worktree
/// remove` — deletes that directory again once its last entry is removed.
/// A repository that has never had a linked worktree, or has had every one
/// removed, legitimately has zero registered worktrees; that reads as an
/// empty set here, not an abort. Every *other* read failure (permission
/// denied, an I/O error, `admin_dir` existing as a non-directory) still
/// aborts the sweep, per the module docs.
fn registered_stable_ids(admin_dir: &Path) -> io::Result<HashSet<String>> {
    let mut ids = HashSet::new();
    let entries = match fs::read_dir(admin_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(ids),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            // Git's own admin entries are always directories; a stray file
            // here is not a registration and is ignored, not hashed.
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            // Non-UTF-8 admin entry name: cannot be the input to the same
            // UTF-8-keyed hash `WorktreeContext::resolve` would have used,
            // so it can never match a real candidate either way. Skipped,
            // not fatal to the whole enumeration.
            continue;
        };
        let admin_identity = format!("{WORKTREES_DIR_NAME}/{name}");
        ids.insert(stable_id_for_admin_identity(&admin_identity));
    }
    Ok(ids)
}

/// Read every direct-child directory name of `worktrees_dir` (Loomweave's
/// own `<repository-store>/worktrees/`) that matches the
/// `wt-[0-9a-f]{64}` grammar — the *candidate* set. `gc.lock` and any other
/// non-matching entry (a stray file, a malformed name) is never a
/// candidate, filtered out here before any deletion decision is made.
fn store_candidates(worktrees_dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(worktrees_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if matches_worktree_store_grammar(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The review-critical env-leakage guard (see
    /// [`hardened_git_command_for_sweep`]'s doc comment for the hazard):
    /// `GIT_DIR`, `GIT_COMMON_DIR`, and `GIT_WORK_TREE` must be explicit
    /// removals on the `Command` this module hands to `git rev-parse
    /// --git-common-dir`, so a foreign value inherited from the process
    /// environment can never reach that invocation.
    ///
    /// This is a unit test, not an integration test mutating the real
    /// process environment via `std::env::set_var`/`remove_var`, because
    /// this workspace denies `unsafe_code` everywhere except one documented
    /// site in the plugin host (`CLAUDE.md`), and `std::env::set_var` /
    /// `remove_var` are `unsafe fn` on this toolchain — confirmed by
    /// attempting exactly that in `tests/worktree_sweep.rs` and getting a
    /// hard compiler error (`-D unsafe-code`), not just a lint warning.
    /// `Command::env_remove` is documented to unconditionally exclude the
    /// named variable from the child's environment regardless of what the
    /// parent process's environment contains, so asserting the removal is
    /// present on the constructed command is a complete, deterministic
    /// proof of the behavioral guarantee — not merely a proxy for one —
    /// without needing to touch any real environment variable at all.
    #[test]
    fn git_common_dir_command_strips_foreign_git_env_vars() {
        let dir = Path::new("/does/not/need/to/exist/for/this/check");
        let command = hardened_git_command_for_sweep(dir);

        // `env_remove` records the key with a `None` value in the command's
        // env-modification table; a `Some(_)` entry would be an explicit
        // `.env(...)` SET, not a removal — only `None` entries count here.
        let removed: std::collections::HashSet<&str> = command
            .get_envs()
            .filter_map(|(key, value)| (value.is_none()).then(|| key.to_str()).flatten())
            .collect();

        for hazardous in ["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"] {
            assert!(
                removed.contains(hazardous),
                "{hazardous} must be an explicit removal on the git_common_dir \
                 command's environment; got removed={removed:?}"
            );
        }
    }
}
