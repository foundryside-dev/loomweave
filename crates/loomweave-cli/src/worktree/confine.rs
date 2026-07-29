//! Confined deletion of worktree-index stores.
//!
//! Everything a worktree-index cleanup sweep deletes is cheap to regenerate
//! (a ~20-30 minute `analyze` re-run); everything *adjacent* to it is not:
//! `.weft/filigree/` holds Filigree's issue tracker and audit trail,
//! `.weft/wardline/` holds Wardline's baselines and waivers, and the user's
//! source tree sits right there too. A sweep that escapes its namespace eats
//! one of those. So deletion here is confined unconditionally, never by a
//! string-prefix check:
//!
//! - Traversal is rooted at a **pinned directory handle**
//!   ([`WorktreesRoot`]) for `<repository-store>/worktrees/`, never a
//!   re-resolved path string — a rename or symlink swap at the original
//!   path after the handle is pinned cannot redirect a deletion (see
//!   `deletion_is_rooted_at_pinned_handle_not_resolved_path` in
//!   `tests/worktree_confine.rs`).
//! - Only a direct child whose name matches exactly `wt-[0-9a-f]{64}` (a
//!   `BLAKE3` hex digest — see
//!   `loomweave_core::worktree::WorktreeContext::stable_id`) is ever
//!   eligible, checked before any filesystem call.
//! - On Linux, every entry (the candidate itself, and everything beneath
//!   it) is classified with `statat`/`AT_SYMLINK_NOFOLLOW` *before* it is
//!   ever opened as a directory: a symlink is caught there and refused as
//!   [`RefusalReason::SymlinkEncountered`], regardless of the entry's
//!   position. (An `openat2`-only check was tried first and found
//!   ambiguous: combined with `O_DIRECTORY`, a symlinked final path
//!   component reports `ENOTDIR` on this kernel, not `ELOOP` — indistinct
//!   from "not a directory".) Every directory that *is* opened goes
//!   through [`rustix::fs::openat2`] with `RESOLVE_BENEATH |
//!   RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV` regardless — confining it
//!   beneath the pinned root, re-enforcing the symlink check against a
//!   TOCTOU race between the `statat` and the `openat2` call, and refusing
//!   a crossed mount boundary — including a bind mount of the very same
//!   filesystem — via `EXDEV`. **This `openat2` confinement is where the
//!   safety property actually lives**; the `statat` classification only
//!   makes the *reported reason* precise. A candidate is *additionally*
//!   validated read-only, top to bottom, before any deletion begins
//!   ([`WorktreesRoot::delete_worktree_store`]'s two-pass shape); that pass exists only so
//!   a refusal anywhere in the tree deletes nothing at all, rather than
//!   whatever siblings a single interleaved pass had already removed by
//!   the time it hit the bad entry — it is not itself the confinement
//!   mechanism, and does not need to be to remain correct: the `statat` +
//!   `openat2` checks re-enforce confinement independently on every entry
//!   in the delete pass too.
//! - Off Linux — or on a Linux kernel old enough that `openat2` itself
//!   returns [`rustix::io::Errno::NOSYS`] — [`refuse_unsupported`] is used:
//!   nothing is deleted, unconditionally. This is the platform's hard
//!   floor, not a best-effort fallback: a platform without race-resistant,
//!   handle-relative, no-cross-mount traversal simply has no mechanism this
//!   module trusts.
//!
//! No `remove_dir_all` on a string path exists anywhere in this module —
//! every deletion is a single-component `unlinkat` relative to a
//! confinement-checked directory handle.
//!
//! [`rustix::fs::openat2`]: rustix::fs::openat2

use std::fmt;
use std::io;
use std::path::Path;
#[cfg(not(unix))]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use std::ffi::CStr;

#[cfg(unix)]
use rustix::fd::OwnedFd;
#[cfg(unix)]
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, statat};
#[cfg(target_os = "linux")]
use rustix::fs::{CWD, ResolveFlags, openat2};
use tracing::warn;

/// The exact name grammar a worktree-store directory must match to ever be
/// eligible for deletion: `wt-` followed by exactly 64 lowercase hex
/// digits. Hand-rolled rather than a `regex` dependency — it is a single
/// fixed-length ASCII-class check.
///
/// A string-prefix match (e.g. `name.starts_with("wt-")`) is deliberately
/// never used to authorize anything in this module; every caller must go
/// through this exact check.
pub fn matches_worktree_store_grammar(name: &str) -> bool {
    let Some(hex) = name.strip_prefix("wt-") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Why [`WorktreesRoot::delete_worktree_store`] refused to delete a
/// candidate.
///
/// Every variant names a specific, positively-identified cause (a specific
/// errno, or a specific pre-flight check) rather than a generic "refused" —
/// so a caller, or a test, can assert *why* a deletion was refused instead
/// of only that it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// The candidate name does not match `wt-[0-9a-f]{64}`. Checked before
    /// any filesystem call is made.
    NameDoesNotMatchGrammar,
    /// A path component — the candidate itself, or an entry beneath it —
    /// resolved to a symlink. `relative_path` is the offending path,
    /// relative to the candidate's parent (the pinned `worktrees/` root).
    SymlinkEncountered {
        /// Path of the symlink, relative to the pinned root.
        relative_path: String,
    },
    /// A path component would cross a mount boundary — including a bind
    /// mount of the very same filesystem. `relative_path` is the offending
    /// path, relative to the candidate's parent.
    MountBoundaryCrossed {
        /// Path of the mount boundary, relative to the pinned root.
        relative_path: String,
    },
    /// An entry exists that is neither a regular file nor a directory (a
    /// device, socket, or FIFO). Refused rather than silently skipped, so
    /// smuggled non-file content can never cause a partial deletion.
    UnexpectedEntryType {
        /// Path of the unexpected entry, relative to the pinned root.
        relative_path: String,
    },
    /// The candidate does not exist, or is not a directory.
    NotADirectory,
    /// Any other I/O failure encountered while validating or deleting.
    /// Fails closed: nothing further is deleted once this is returned.
    Io {
        /// Path being processed when the error occurred, relative to the
        /// pinned root.
        relative_path: String,
        /// A human-readable description of the underlying error.
        message: String,
    },
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameDoesNotMatchGrammar => {
                write!(f, "name does not match wt-[0-9a-f]{{64}}")
            }
            Self::SymlinkEncountered { relative_path } => {
                write!(f, "symlink encountered at {relative_path}")
            }
            Self::MountBoundaryCrossed { relative_path } => {
                write!(f, "mount boundary crossed at {relative_path}")
            }
            Self::UnexpectedEntryType { relative_path } => {
                write!(f, "unexpected entry type at {relative_path}")
            }
            Self::NotADirectory => write!(f, "candidate is not a directory"),
            Self::Io {
                relative_path,
                message,
            } => write!(f, "I/O error at {relative_path}: {message}"),
        }
    }
}

/// Outcome of one [`WorktreesRoot::delete_worktree_store`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// The candidate matched the grammar, was fully confined beneath the
    /// pinned root, and was removed.
    Deleted,
    /// The candidate was refused; nothing was deleted.
    Refused(RefusalReason),
    /// This platform (or kernel) has no confined-deletion mechanism.
    /// Nothing was deleted, unconditionally.
    UnsupportedPlatform,
}

/// Cross-platform fallback: no confined-deletion mechanism is available, so
/// nothing is deleted.
///
/// Always compiled (not `#[cfg(target_os = "linux")]`) so it is directly
/// reachable from tests on every platform, and used internally as the
/// non-Linux arm of [`WorktreesRoot::delete_worktree_store`] and as the
/// response to a Linux kernel old enough that `openat2` itself is missing
/// ([`rustix::io::Errno::NOSYS`]). This is the platform's hard floor, not a
/// best-effort fallback: it never attempts a less-safe deletion instead.
pub fn refuse_unsupported(candidate_name: &str, reason: &str) -> DeleteOutcome {
    warn!(
        candidate = candidate_name,
        reason, "refusing deletion: no confined-deletion mechanism on this platform"
    );
    DeleteOutcome::UnsupportedPlatform
}

/// A pinned handle to a repository's `<store>/worktrees/` directory.
///
/// [`delete_worktree_store`](Self::delete_worktree_store) is always
/// confined beneath this open file descriptor, never a re-resolved path
/// string: once opened, a later rename, symlink swap, or replacement at the
/// original path string cannot redirect a deletion — see the module docs.
#[derive(Debug)]
pub struct WorktreesRoot {
    /// The pinned directory fd (unix). On non-unix targets the field is a
    /// unit placeholder: no fd primitive exists there, and
    /// [`delete_worktree_store`](Self::delete_worktree_store) refuses with
    /// [`DeleteOutcome::UnsupportedPlatform`] before ever consulting a
    /// handle, so the placeholder carries no authority
    /// (clarion-4cd5b0b3b9).
    #[cfg(unix)]
    handle: OwnedFd,
    #[cfg(not(unix))]
    handle: (),
    #[cfg(not(unix))]
    path: PathBuf,
}

impl WorktreesRoot {
    /// Pin a handle to `worktrees_dir` (normally
    /// `<repository-store>/worktrees/`).
    ///
    /// On Linux the entire path is opened with `openat2(RESOLVE_NO_SYMLINKS)`,
    /// so neither the final `worktrees` component nor an intermediate store
    /// prefix can be substituted with a symlink before the handle is pinned.
    /// Other Unix targets retain a final-component `O_NOFOLLOW` check and
    /// subsequently refuse deletion as unsupported.
    ///
    /// # Errors
    ///
    /// Returns an error if `worktrees_dir` cannot be opened as a directory
    /// (missing, not a directory, or a symlink).
    pub fn open(worktrees_dir: &Path) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let handle = openat2(
                CWD,
                worktrees_dir,
                OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::NO_SYMLINKS,
            )?;
            Ok(Self { handle })
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            let handle = rustix::fs::open(
                worktrees_dir,
                OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            Ok(Self { handle })
        }
        #[cfg(not(unix))]
        {
            // No fd-pinning primitive here; preserve the two properties the
            // unix open provides that std can express — the path must be a
            // directory and must not itself be a symlink. Deletion beneath
            // it refuses `UnsupportedPlatform` regardless, so this handle
            // carries no authority on this platform.
            let meta = std::fs::symlink_metadata(worktrees_dir)?;
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "worktrees directory is a symlink",
                ));
            }
            if !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "worktrees path is not a directory",
                ));
            }
            Ok(Self {
                handle: (),
                path: worktrees_dir.to_path_buf(),
            })
        }
    }

    /// Enumerate grammar-valid direct-child store directories through this
    /// pinned root. On Unix both classification and naming are handle-relative,
    /// so a rename or pathname replacement cannot split candidate decisions
    /// from the inode later used by [`Self::delete_worktree_store`].
    pub(crate) fn candidate_names(&self) -> io::Result<Vec<String>> {
        #[cfg(unix)]
        {
            let mut names = Vec::new();
            let dir = Dir::read_from(&self.handle).map_err(io::Error::from)?;
            for entry in dir {
                let entry = entry.map_err(io::Error::from)?;
                let raw_name = entry.file_name();
                if raw_name.to_bytes() == b"." || raw_name.to_bytes() == b".." {
                    continue;
                }
                let stat = statat(&self.handle, raw_name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(io::Error::from)?;
                if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
                    continue;
                }
                let Ok(name) = raw_name.to_str() else {
                    continue;
                };
                if matches_worktree_store_grammar(name) {
                    names.push(name.to_owned());
                }
            }
            names.sort();
            Ok(names)
        }
        #[cfg(not(unix))]
        {
            let _ = &self.handle;
            let mut names = Vec::new();
            for entry in std::fs::read_dir(&self.path)? {
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
            names.sort();
            Ok(names)
        }
    }

    /// Attempt to delete `candidate_name`'s store, confined beneath this
    /// pinned root. `reason` is a short, operator-facing string logged
    /// alongside the outcome (e.g. `"worktree no longer registered"`).
    ///
    /// Validates `candidate_name` against the `wt-[0-9a-f]{64}` grammar
    /// first, on every platform. On Linux, every subsequent traversal is
    /// confined via `openat2`'s `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
    /// RESOLVE_NO_XDEV`, and the candidate is validated read-only in full
    /// before any deletion begins — see the module docs. Off Linux, or if
    /// the running kernel lacks `openat2`, returns
    /// [`DeleteOutcome::UnsupportedPlatform`] and deletes nothing.
    ///
    /// Every outcome is logged once via `tracing`, with `candidate_name`
    /// and `reason` attached.
    pub fn delete_worktree_store(&self, candidate_name: &str, reason: &str) -> DeleteOutcome {
        if !matches_worktree_store_grammar(candidate_name) {
            warn!(
                candidate = candidate_name,
                reason, "refusing deletion: name does not match wt-[0-9a-f]{{64}}"
            );
            return DeleteOutcome::Refused(RefusalReason::NameDoesNotMatchGrammar);
        }

        #[cfg(target_os = "linux")]
        {
            linux::delete_confined(&self.handle, candidate_name, reason)
        }
        #[cfg(not(target_os = "linux"))]
        {
            // `handle` is otherwise read only inside `mod linux`, which
            // doesn't exist on this platform — without this, `handle`
            // would be flagged dead code under `-D warnings` even though
            // it's a real field, written by `open` and load-bearing on
            // Linux. There is nothing to do with it here; `open` already
            // proved `worktrees_dir` was a real, non-symlinked directory,
            // and this arm deletes nothing regardless.
            let _ = &self.handle;
            refuse_unsupported(candidate_name, reason)
        }
    }
}

/// Confine a single path-component name as a [`CStr`] for `rustix` calls.
///
/// Only called with `candidate_name` after it has already passed
/// [`matches_worktree_store_grammar`] (`wt-` + 64 lowercase hex digits) —
/// that grammar is what rules out an interior NUL byte, which is the only
/// way [`CStr::from_bytes_with_nul`] can fail here. If this is ever called
/// on an unvalidated string, that invariant breaks and this panics; the
/// panic is not caused by "did we append a trailing NUL" (that part always
/// succeeds by construction).
#[cfg(target_os = "linux")]
fn as_component<'a>(buf: &'a mut Vec<u8>, name: &str) -> &'a CStr {
    buf.clear();
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);
    CStr::from_bytes_with_nul(buf)
        .expect("candidate_name must already be grammar-validated (no interior NUL)")
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CStr;

    use rustix::fd::OwnedFd;
    use rustix::fs::{
        AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags, openat2, statat, unlinkat,
    };
    use rustix::io::Errno;
    use tracing::{info, warn};

    use super::{DeleteOutcome, RefusalReason, refuse_unsupported};

    /// Confinement flags applied to every `openat2` call beneath the pinned
    /// root: never resolve outside the subtree rooted at the starting
    /// handle (`BENEATH`), never resolve a symlink at any path component
    /// (`NO_SYMLINKS`), never cross into a different mount, including a
    /// bind mount of the same filesystem (`NO_XDEV`).
    const CONFINE: ResolveFlags = ResolveFlags::BENEATH
        .union(ResolveFlags::NO_SYMLINKS)
        .union(ResolveFlags::NO_XDEV);

    /// A refusal reason, or a signal that this kernel lacks `openat2`
    /// entirely and the caller should fall back to
    /// [`refuse_unsupported`].
    enum Refusal {
        Reason(RefusalReason),
        Unsupported,
    }

    pub(super) fn delete_confined(
        root: &OwnedFd,
        candidate_name: &str,
        reason: &str,
    ) -> DeleteOutcome {
        // Pass 1: read-only validation of the whole subtree. See the
        // module docs — this pass exists only so a refusal anywhere in the
        // tree deletes nothing at all; `openat2`'s flags are what actually
        // enforce confinement, independently, in pass 2 as well.
        if let Err(refusal) = validate_confined(root, candidate_name) {
            return match refusal {
                Refusal::Unsupported => refuse_unsupported(candidate_name, reason),
                Refusal::Reason(r) => {
                    warn!(candidate = candidate_name, reason, refusal = %r, "refusing deletion");
                    DeleteOutcome::Refused(r)
                }
            };
        }

        match delete_tree(root, candidate_name) {
            Ok(()) => {
                info!(candidate = candidate_name, reason, "deleted worktree store");
                DeleteOutcome::Deleted
            }
            Err(Refusal::Unsupported) => refuse_unsupported(candidate_name, reason),
            Err(Refusal::Reason(r)) => {
                warn!(
                    candidate = candidate_name,
                    reason,
                    refusal = %r,
                    "refusing deletion (during delete pass)"
                );
                DeleteOutcome::Refused(r)
            }
        }
    }

    /// Open `name` (a single path component, never containing `/`) as a
    /// directory beneath `parent`, confined via [`CONFINE`]. `rel` is the
    /// path so far, for diagnostics.
    fn open_dir_confined(parent: &OwnedFd, name: &CStr, rel: &str) -> Result<OwnedFd, Refusal> {
        openat2(
            parent,
            name,
            OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
            CONFINE,
        )
        .map_err(|errno| classify_error(errno, rel))
    }

    /// `lstat` a single entry (never following a symlink) to classify it,
    /// without trusting `readdir`'s `d_type` — some filesystems report
    /// `DT_UNKNOWN`, and `statat` gives one definitive answer either way.
    fn entry_type(dir_fd: &OwnedFd, name: &CStr, rel: &str) -> Result<FileType, Refusal> {
        let stat = statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|errno| classify_error(errno, rel))?;
        Ok(FileType::from_raw_mode(stat.st_mode))
    }

    fn classify_error(errno: Errno, rel: &str) -> Refusal {
        match errno {
            Errno::NOSYS => Refusal::Unsupported,
            Errno::LOOP => Refusal::Reason(RefusalReason::SymlinkEncountered {
                relative_path: rel.to_owned(),
            }),
            Errno::XDEV => Refusal::Reason(RefusalReason::MountBoundaryCrossed {
                relative_path: rel.to_owned(),
            }),
            Errno::NOTDIR | Errno::NOENT => Refusal::Reason(RefusalReason::NotADirectory),
            other => Refusal::Reason(RefusalReason::Io {
                relative_path: rel.to_owned(),
                message: other.to_string(),
            }),
        }
    }

    /// Classify `candidate_name` under `root` via `statat` (never following
    /// a symlink) *before* attempting to open it as a directory.
    ///
    /// This is deliberate, not redundant with `open_dir_confined`'s own
    /// `openat2` errno: combined with `O_DIRECTORY`, a symlinked final
    /// component reports `ENOTDIR` on this kernel, not `ELOOP` — an
    /// ambiguity this module's own test suite caught
    /// (`sibling_weft_directories_are_unreachable` initially asserted
    /// `SymlinkEncountered` and got `NotADirectory` from the open-only
    /// path). Statting first gives a precise, positively-identified
    /// [`RefusalReason`] regardless of that quirk; the subsequent
    /// `openat2` call still re-enforces confinement independently against
    /// a TOCTOU swap between the two calls.
    fn classify_candidate(root: &OwnedFd, name: &CStr, rel: &str) -> Result<(), Refusal> {
        match entry_type(root, name, rel)? {
            FileType::Directory => Ok(()),
            FileType::Symlink => Err(Refusal::Reason(RefusalReason::SymlinkEncountered {
                relative_path: rel.to_owned(),
            })),
            _ => Err(Refusal::Reason(RefusalReason::NotADirectory)),
        }
    }

    fn validate_confined(root: &OwnedFd, candidate_name: &str) -> Result<(), Refusal> {
        let mut buf = Vec::new();
        let name = super::as_component(&mut buf, candidate_name);
        classify_candidate(root, name, candidate_name)?;
        let candidate_fd = open_dir_confined(root, name, candidate_name)?;
        validate_dir_recursive(&candidate_fd, candidate_name)
    }

    fn validate_dir_recursive(dir_fd: &OwnedFd, rel_prefix: &str) -> Result<(), Refusal> {
        let dir = Dir::read_from(dir_fd).map_err(|e| classify_error(e, rel_prefix))?;
        for entry in dir {
            let entry = entry.map_err(|e| classify_error(e, rel_prefix))?;
            let name = entry.file_name();
            if is_dot_or_dotdot(name) {
                continue;
            }
            let rel = format!("{rel_prefix}/{}", name.to_string_lossy());
            match entry_type(dir_fd, name, &rel)? {
                FileType::Directory => {
                    let child_fd = open_dir_confined(dir_fd, name, &rel)?;
                    validate_dir_recursive(&child_fd, &rel)?;
                }
                FileType::RegularFile => {}
                FileType::Symlink => {
                    return Err(Refusal::Reason(RefusalReason::SymlinkEncountered {
                        relative_path: rel,
                    }));
                }
                _ => {
                    return Err(Refusal::Reason(RefusalReason::UnexpectedEntryType {
                        relative_path: rel,
                    }));
                }
            }
        }
        Ok(())
    }

    fn delete_tree(root: &OwnedFd, candidate_name: &str) -> Result<(), Refusal> {
        let mut buf = Vec::new();
        let name = super::as_component(&mut buf, candidate_name);
        classify_candidate(root, name, candidate_name)?;
        let candidate_fd = open_dir_confined(root, name, candidate_name)?;
        delete_dir_contents(&candidate_fd, candidate_name)?;
        unlinkat(root, name, AtFlags::REMOVEDIR).map_err(|e| classify_error(e, candidate_name))?;
        Ok(())
    }

    fn delete_dir_contents(dir_fd: &OwnedFd, rel_prefix: &str) -> Result<(), Refusal> {
        // Snapshot entry names before mutating: unlinking/rmdir-ing while a
        // `Dir` iterator over the same fd is still live is not something
        // this module relies on being safe across kernels/libcs.
        let mut names = Vec::new();
        let dir = Dir::read_from(dir_fd).map_err(|e| classify_error(e, rel_prefix))?;
        for entry in dir {
            let entry = entry.map_err(|e| classify_error(e, rel_prefix))?;
            if is_dot_or_dotdot(entry.file_name()) {
                continue;
            }
            names.push(entry.file_name().to_owned());
        }

        for name in names {
            let rel = format!("{rel_prefix}/{}", name.to_string_lossy());
            match entry_type(dir_fd, &name, &rel)? {
                FileType::Directory => {
                    let child_fd = open_dir_confined(dir_fd, &name, &rel)?;
                    delete_dir_contents(&child_fd, &rel)?;
                    unlinkat(dir_fd, &name, AtFlags::REMOVEDIR)
                        .map_err(|e| classify_error(e, &rel))?;
                }
                FileType::RegularFile => {
                    unlinkat(dir_fd, &name, AtFlags::empty())
                        .map_err(|e| classify_error(e, &rel))?;
                }
                FileType::Symlink => {
                    // Should already have been caught by `validate_confined`;
                    // fail closed anyway rather than unlink-and-continue.
                    return Err(Refusal::Reason(RefusalReason::SymlinkEncountered {
                        relative_path: rel,
                    }));
                }
                _ => {
                    return Err(Refusal::Reason(RefusalReason::UnexpectedEntryType {
                        relative_path: rel,
                    }));
                }
            }
        }
        Ok(())
    }

    fn is_dot_or_dotdot(name: &CStr) -> bool {
        name.to_bytes() == b"." || name.to_bytes() == b".."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn candidate_enumeration_stays_on_pinned_root_after_path_replacement() {
        let tmp = tempfile::tempdir().unwrap();
        let worktrees = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees).unwrap();
        let old_name = format!("wt-{}", "a".repeat(64));
        std::fs::create_dir(worktrees.join(&old_name)).unwrap();

        let root = WorktreesRoot::open(&worktrees).unwrap();
        let moved = tmp.path().join("worktrees-moved");
        std::fs::rename(&worktrees, &moved).unwrap();
        std::fs::create_dir(&worktrees).unwrap();
        let replacement_name = format!("wt-{}", "b".repeat(64));
        std::fs::create_dir(worktrees.join(&replacement_name)).unwrap();

        let candidates = root.candidate_names().unwrap();

        assert_eq!(candidates, vec![old_name]);
        assert!(
            !candidates.contains(&replacement_name),
            "candidate decisions must come from the same pinned inode deletion will use"
        );
    }
}
