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
//! **Old Linux kernels (pre-5.6).** Linux is the supported platform
//! (`docs/operator/README.md`), but the pinned root handle is itself an
//! `openat2(2)` open, and a kernel older than 5.6 fails it with `ENOSYS`.
//! That one failure degrades instead of aborting: the sweep keeps full
//! report-only visibility through an unpinned `read_dir` view, and every
//! actual deletion is refused as
//! [`crate::worktree::confine::DeleteOutcome::UnsupportedPlatform`] — the
//! same graceful posture `confine.rs`'s module docs promise. The candidate
//! list a *deletion* acts on always comes from the pinned root; only the
//! report side may use the unpinned fallback (see the private `SweepRoot`
//! enum).
//!
//! **The override case.** A `[loomweave].store_dir` override is not scoped
//! to one repository: an absolute override can be shared between unrelated
//! repositories, so under an active override this repository's registered
//! set must never be used to authorize deleting another repository's `wt-*`
//! stores sitting in the same shared namespace. When an override was active
//! at context-resolve time, this module enumerates exactly as it otherwise
//! would and logs every candidate it *would* delete, but deletes nothing —
//! see [`SweepOutcome::ReportOnly`]. The decision reads
//! `ctx.store_dir_overridden` — the provenance recorded when
//! `ctx.repository_store` was derived — never a fresh `weft.toml` read at
//! sweep time: `analyze` resolves its context at start but sweeps at the
//! end of a long run, and an override removed in that window must not flip
//! the sweep into delete mode against the still-shared store the context
//! was resolved under (clarion-306ed41ce3).
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
use loomweave_core::worktree::{
    WorktreeContext, stable_id_for_admin_identity, stable_id_for_shared_store_project,
};
use tracing::{debug, info, warn};

use crate::worktree::confine::{
    DeleteOutcome, WorktreesRoot, error_signals_missing_openat2, refuse_unsupported,
    unpinned_candidate_names,
};
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
    /// `<repository-store>/worktrees/` itself could not be pinned or read
    /// while enumerating store candidates — aborted, nothing deleted.
    /// Distinct from [`Self::AdminDirUnreadable`]: this
    /// is Loomweave's own store directory, not Git's administrative one.
    ///
    /// One pin failure is deliberately **not** this outcome: a Linux kernel
    /// without `openat2` (pre-5.6, `ENOSYS`) degrades to an unpinned,
    /// report-only enumeration instead — the sweep still runs and reports,
    /// with every deletion refused as
    /// [`DeleteOutcome::UnsupportedPlatform`] (see `SweepRoot`'s docs).
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
/// Whether `repository_store` resolves through a symlink in any of its
/// components (clarion-a93b43923e). The path is built from the CANONICALIZED
/// primary root plus literal components (`WorktreeContext::resolve`
/// canonicalizes; an active override forces report-only before this check is
/// consulted), so it equals its own canonicalization exactly when no
/// component is a symlink. An unresolvable path — the store does not exist
/// yet — reports `false`: there is nothing beneath it to protect, and the
/// sweep's own enumeration fails first anyway.
fn store_path_reaches_through_symlink(repository_store: &Path) -> bool {
    match fs::canonicalize(repository_store) {
        Ok(canonical) => canonical != repository_store,
        Err(_) => false,
    }
}

/// Log every would-be deletion once with the report-only `cause` and return
/// [`SweepOutcome::ReportOnly`] — shared by the override and symlink-prefix
/// guards, which differ only in why deletion is withheld.
fn report_only(cause: &str, to_delete: Vec<String>) -> SweepOutcome {
    for name in &to_delete {
        info!(
            candidate = name.as_str(),
            cause,
            "worktree cleanup sweep: would delete this unregistered candidate, but report-only \
             mode deletes nothing"
        );
    }
    SweepOutcome::ReportOnly {
        would_delete: to_delete,
    }
}

fn pin_sweep_root(
    worktrees_dir: &Path,
    store_path_is_symlinked: bool,
) -> io::Result<WorktreesRoot> {
    // A symlink-reached store is report-only, but its candidate list should
    // still be inode-stable. Resolve that path once, then pin and enumerate
    // the resolved directory. The caller never authorizes deletion for it.
    let root_path = if store_path_is_symlinked {
        fs::canonicalize(worktrees_dir)?
    } else {
        worktrees_dir.to_owned()
    };
    WorktreesRoot::open(&root_path)
}

/// The sweep's view into `<repository-store>/worktrees/` — either the
/// pinned, confinement-checked handle, or the degraded report-only view a
/// pre-5.6 Linux kernel gets.
#[derive(Debug)]
enum SweepRoot {
    /// [`WorktreesRoot::open`] succeeded: this handle carries both report
    /// visibility *and* deletion authority, and the candidate list any
    /// deletion decision uses comes from this pinned inode (see
    /// `candidate_enumeration_stays_on_pinned_root_after_path_replacement`
    /// in `confine.rs`).
    Pinned(WorktreesRoot),
    /// The pin failed with `ENOSYS` — this kernel predates `openat2(2)`
    /// (Linux < 5.6), so no confined-deletion mechanism exists at all.
    /// Candidates are enumerated through a plain, unpinned `read_dir`
    /// ([`unpinned_candidate_names`]) purely so report-only outcomes
    /// (override, symlinked store, and the "would have deleted" log lines)
    /// keep working; every actual deletion is refused as
    /// [`DeleteOutcome::UnsupportedPlatform`], exactly the posture
    /// `confine.rs`'s module docs promise for a missing `openat2`.
    UnpinnedReportOnly,
}

impl SweepRoot {
    /// Enumerate the candidate set through whichever view this is. The
    /// unpinned view is only ever *reported from*; the deletion loop below
    /// refuses every candidate under it.
    fn candidate_names(&self, worktrees_dir: &Path) -> io::Result<Vec<String>> {
        match self {
            Self::Pinned(root) => root.candidate_names(),
            Self::UnpinnedReportOnly => unpinned_candidate_names(worktrees_dir),
        }
    }

    /// Attempt one confined deletion, or refuse it outright when no pinned
    /// root exists (the missing-`openat2` kernel).
    fn delete_worktree_store(&self, candidate_name: &str, reason: &str) -> DeleteOutcome {
        match self {
            Self::Pinned(root) => root.delete_worktree_store(candidate_name, reason),
            Self::UnpinnedReportOnly => refuse_unsupported(candidate_name, reason),
        }
    }
}

/// Turn [`pin_sweep_root`]'s result into the sweep's working view — the
/// degradation decision, isolated here so it is unit-testable: a real
/// `ENOSYS` cannot be provoked on a modern kernel, but this function can be
/// fed one directly (see
/// `enosys_pin_failure_selects_the_unpinned_report_only_root`).
///
/// Exactly one failure degrades instead of aborting: `ENOSYS`, meaning the
/// kernel has no `openat2` and the pin *cannot* exist — report-only
/// visibility survives, deletion stays refused. Every other failure means
/// the store directory itself is unreadable and propagates as an abort
/// ([`SweepOutcome::StoreDirUnreadable`] at the call site).
fn sweep_root_after_pin(pin_result: io::Result<WorktreesRoot>) -> io::Result<SweepRoot> {
    match pin_result {
        Ok(root) => Ok(SweepRoot::Pinned(root)),
        Err(err) if error_signals_missing_openat2(&err) => {
            warn!(
                error = %err,
                "worktree cleanup sweep: this kernel has no openat2 (pre-5.6); continuing with \
                 an unpinned report-only view — deletions will be refused as unsupported"
            );
            Ok(SweepRoot::UnpinnedReportOnly)
        }
        Err(err) => Err(err),
    }
}

pub fn sweep_worktree_stores(ctx: &WorktreeContext) -> SweepOutcome {
    let worktrees_dir = ctx.repository_store.join(WORKTREES_DIR_NAME);
    if !worktrees_dir.is_dir() {
        return SweepOutcome::NoWorktreesStore;
    }
    let store_path_is_symlinked = store_path_reaches_through_symlink(&ctx.repository_store);

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

    let root = match sweep_root_after_pin(pin_sweep_root(&worktrees_dir, store_path_is_symlinked)) {
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
    let candidates = match root.candidate_names(&worktrees_dir) {
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
    let shared_store_project_root = ctx
        .store_dir_overridden
        .then_some(ctx.primary_root.as_path());
    let registered = match registered_stable_ids(&admin_dir, shared_store_project_root) {
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

    if ctx.store_dir_overridden {
        return report_only("store_dir override active", to_delete);
    }

    // Symlink analogue of the override case (clarion-a93b43923e): a store
    // path that reaches through a symlink (`.weft` or `.weft/loomweave`
    // linking elsewhere — stores relocated to another disk) may be SHARED
    // between repositories exactly like an absolute override, and carries no
    // `weft.toml` signal to key report-only mode on. `openat2`'s confinement
    // only starts AT whatever the prefix resolved to, so the pinned-handle
    // mechanism cannot see this either. `ctx.repository_store` is the
    // canonicalized primary root plus literal components, so it equals its
    // own canonicalization exactly when no appended component is a symlink.
    if store_path_is_symlinked {
        return report_only(
            "store path resolves through a symlink (possibly shared between repositories)",
            to_delete,
        );
    }

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

/// [`loomweave_core::hardened_git::hardened_git_command`], which since the
/// clarion-9202f4acec sanitization rebuilds the child environment from
/// nothing (`env_clear()` + an explicit allow-list), so `GIT_DIR`,
/// `GIT_COMMON_DIR`, and `GIT_WORK_TREE` can never reach the child.
///
/// **The hazard this closes.** `-C <dir>` does not override an *exported*
/// `GIT_DIR`: if the Loomweave process inherits `GIT_DIR` from its parent —
/// a Git hook running in a different repository, `git rebase --exec
/// 'loomweave ...'`, a CI runner that exports it for its own purposes —
/// `git rev-parse --git-common-dir` answers for that FOREIGN repository,
/// not `primary_root`'s. Combined with [`registered_stable_ids`]'s
/// `NotFound` → empty-registered-set rule (see its doc comment), a foreign
/// repository that happens to have no `worktrees/` admin directory of its
/// own — the common case — makes the registered set empty, so *every* real
/// `wt-*` store under this repository reads as unregistered and gets
/// deleted, live ones included. `GIT_COMMON_DIR` and `GIT_WORK_TREE` are
/// hazardous for the same reason: either can redirect Git's discovery away
/// from `primary_root`.
///
/// This wrapper existed as a narrow, sweep-local `env_remove` guard while
/// the general sanitization was still pending; that work has landed in
/// `hardened_git_command` itself, whose closed-set env test pins the
/// guarantee. `git_common_dir_command_keeps_foreign_git_env_out` below
/// asserts this call path stays on the hardened builder and never
/// re-introduces the hazardous variables.
fn hardened_git_command_for_sweep(dir: &Path) -> std::process::Command {
    loomweave_core::hardened_git::hardened_git_command(dir)
}

/// Resolve `primary_root`'s common Git directory via one hardened, bounded
/// `git rev-parse` — `None` on any failure (missing `git`, not a repository, a
/// non-zero exit, non-UTF-8 output, or the probe's deadline / stdout cap),
/// which every caller treats as an abort signal, never a hard error.
fn git_common_dir(primary_root: &Path) -> Option<PathBuf> {
    let mut command = hardened_git_command_for_sweep(primary_root);
    command.args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    // Bounded (clarion-9202f4acec): a probe that hangs or floods here would
    // stall the sweep, and every failure already aborts it rather than deleting
    // on incomplete evidence.
    let output = loomweave_core::run_git_probe_default(command).ok()?;
    let text = output.stdout_utf8().ok()?.trim();
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
fn registered_stable_ids(
    admin_dir: &Path,
    shared_store_project_root: Option<&Path>,
) -> io::Result<HashSet<String>> {
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
        let stable_id = shared_store_project_root.map_or_else(
            || stable_id_for_admin_identity(&admin_identity),
            |primary_root| stable_id_for_shared_store_project(primary_root, &admin_identity),
        );
        ids.insert(stable_id);
    }
    Ok(ids)
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
    /// `remove_var` are `unsafe fn` on this toolchain.
    ///
    /// The hardened builder calls `env_clear()` and rebuilds the child
    /// environment from an explicit allow-list, so a hazardous variable can
    /// only reach the child as an explicit `.env(...)` SET — a `Some(_)`
    /// entry in `get_envs()`. (After `env_clear()`, `env_remove` records no
    /// `(key, None)` marker, which is why this test does not look for
    /// removals.) Asserting no hazardous SET exists, plus one
    /// hardened-builder signature key, proves this call path stays on the
    /// cleared-and-rebuilt environment without touching any real variable.
    /// The behavioral proof that an inherited `GIT_DIR` cannot redirect a
    /// probe lives with the hardened builder's own closed-set env test and
    /// `doctor_git_probes_ignore_a_hijacked_git_dir_in_the_environment`.
    #[test]
    fn git_common_dir_command_keeps_foreign_git_env_out() {
        let dir = Path::new("/does/not/need/to/exist/for/this/check");
        let command = hardened_git_command_for_sweep(dir);

        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();

        // Signature of the cleared-and-rebuilt hardened environment.
        assert!(
            envs.contains(&("GIT_CONFIG_NOSYSTEM".to_owned(), Some("1".to_owned()))),
            "sweep must build its git command via the hardened builder; envs={envs:?}"
        );
        // Nothing on this call path re-introduces a discovery-redirecting var.
        for hazardous in ["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"] {
            assert!(
                !envs.iter().any(|(k, v)| k == hazardous && v.is_some()),
                "{hazardous} must never be SET on the git_common_dir command; envs={envs:?}"
            );
        }
    }

    /// An `ENOSYS` os error, as `openat2` reports it on a pre-5.6 kernel —
    /// injected because a real one cannot be provoked on a modern kernel.
    #[cfg(unix)]
    fn enosys_error() -> io::Error {
        io::Error::from_raw_os_error(rustix::io::Errno::NOSYS.raw_os_error())
    }

    /// The regression this seam exists for: a missing-`openat2` kernel
    /// (`ENOSYS` from the pinned open) must degrade to the unpinned
    /// report-only view, never abort the sweep as `StoreDirUnreadable` —
    /// and that fallback view must actually enumerate candidates, so
    /// report-only runs keep full visibility on old kernels.
    #[test]
    #[cfg(unix)]
    fn enosys_pin_failure_selects_the_unpinned_report_only_root() {
        let root = sweep_root_after_pin(Err(enosys_error()))
            .expect("ENOSYS must degrade, not abort the sweep");
        assert!(
            matches!(root, SweepRoot::UnpinnedReportOnly),
            "expected the unpinned report-only view, got {root:?}"
        );

        // Report-only enumeration works through the fallback path.
        let tmp = tempfile::tempdir().unwrap();
        let candidate = format!("wt-{}", "a".repeat(64));
        fs::create_dir(tmp.path().join(&candidate)).unwrap();
        fs::write(tmp.path().join(GC_LOCK_FILE_NAME), b"").unwrap();
        fs::create_dir(tmp.path().join("not-a-candidate")).unwrap();

        let names = root
            .candidate_names(tmp.path())
            .expect("the unpinned view must still enumerate");
        assert_eq!(
            names,
            vec![candidate.clone()],
            "the fallback must apply the same wt-[0-9a-f]{{64}} grammar filter"
        );

        // And deletion through the fallback view is refused, untouched.
        assert_eq!(
            root.delete_worktree_store(&candidate, "test"),
            DeleteOutcome::UnsupportedPlatform,
            "no pinned root means no deletion authority, ever"
        );
        assert!(
            tmp.path().join(&candidate).is_dir(),
            "a refused deletion must leave the candidate in place"
        );
    }

    /// Every non-`ENOSYS` pin failure keeps the pre-existing abort
    /// semantics: it propagates, and the caller turns it into
    /// [`SweepOutcome::StoreDirUnreadable`].
    #[test]
    fn other_pin_failures_still_abort_the_sweep() {
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        assert!(
            sweep_root_after_pin(Err(err)).is_err(),
            "a permission failure must abort, not silently degrade to report-only"
        );
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert!(
            sweep_root_after_pin(Err(missing)).is_err(),
            "a missing store directory must abort, not silently degrade to report-only"
        );
    }
}
