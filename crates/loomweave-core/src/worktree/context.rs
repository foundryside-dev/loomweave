//! Resolve a typed [`WorktreeContext`] before any runtime storage path is
//! chosen.
//!
//! Loomweave's on-disk store lives at `store_dir(project_root)`
//! (`.weft/loomweave/` by default). For a plain checkout or the main
//! worktree of a Git repository that path is unambiguous. For a *linked* Git
//! worktree (`git worktree add`), writing to `store_dir(source_root)` would
//! either collide with the primary checkout's `SQLite` writer (ADR-011 assumes
//! one writer per store) or silently point at empty state disconnected from
//! the primary's history, depending on how the caller derived the root. This
//! module resolves, once, which of those situations applies and hands back
//! every path the rest of the system needs.
//!
//! [`WorktreeContext::resolve`] shells out to `git rev-parse` and
//! `git worktree list --porcelain -z` through [`hardened_git_command`] to
//! identify the *primary* worktree — the entry whose own Git directory
//! equals the repository's common Git directory — without trusting branch
//! or directory names (a linked worktree can be named `main`; that must not
//! misclassify it). A primary whose common Git directory has no working
//! tree (a bare hub used only to host linked worktrees) is out of scope for
//! isolation, so checkouts routed through it fall back to
//! [`WorktreeKind::Standalone`] and use their own local store — as does any
//! checkout where the resolver cannot positively prove the linked
//! relationship at all (missing `git`, a corrupted worktree administrative
//! link, or simply no Git repository present). The resolver never guesses:
//! failing to prove "linked" is always the safe, local fallback.
//!
//! The stable ID that isolates a linked worktree's store
//! (`wt-<BLAKE3 hex>`) is derived from the Git *administrative* identity —
//! the path fragment under the common Git directory (e.g.
//! `worktrees/federation-seam-followups`) — never from the branch, `HEAD`,
//! dirty state, or absolute filesystem path. That is what lets the ID
//! survive a branch switch ([`resolve`]'s own admin identity is keyed by
//! worktree *directory* name, which `git worktree add`/`move` controls, not
//! `git checkout`) and, once `git worktree repair` has restored the
//! administrative links, a repository relocation.
//!
//! [`resolve`]: WorktreeContext::resolve

use std::path::{Path, PathBuf};

use crate::hardened_git::hardened_git_command;
use crate::store::store_dir;
use crate::worktree::paths::StorePaths;

/// The tracked, user-edited Loomweave config file name at a project root.
const CONFIG_FILE_NAME: &str = "loomweave.yaml";

/// How a checkout relates to Git's worktree structure, and therefore which
/// store it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeKind {
    /// No Git repository was found, the repository's primary is bare (no
    /// working tree to isolate under), or the resolver could not positively
    /// identify the primary at all. Uses the current checkout's own,
    /// unchanged store — the historical, pre-worktree-isolation behavior.
    Standalone,
    /// The main/primary working tree of a Git repository: the checkout
    /// whose own Git directory equals the repository's common Git
    /// directory. This holds whether or not any linked worktree currently
    /// exists elsewhere. Uses the current checkout's own, unchanged store.
    Main,
    /// A linked worktree (`git worktree add`) whose primary was positively
    /// identified and is not bare. Uses an isolated store nested under the
    /// primary's store, named by [`WorktreeContext::stable_id`].
    Linked,
}

/// Which `loomweave.yaml` precedence rung supplied (or would supply) the
/// effective config path.
///
/// Precedence: an explicit `--config` path, then `<source-root>/loomweave.yaml`,
/// then `<primary-root>/loomweave.yaml`, then Loomweave's built-in defaults
/// (with the primary root as the write target for newly created config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigOrigin {
    /// An explicit `--config` path was supplied by the caller.
    ///
    /// [`WorktreeContext::resolve`] takes no config-path argument, so this
    /// resolver never produces this variant itself — it exists so a future
    /// caller that *does* know about an explicit `--config` flag (the CLI
    /// wiring task) can construct or override a `WorktreeContext` with it
    /// without changing this enum's shape.
    Explicit,
    /// `<source-root>/loomweave.yaml` exists and was selected.
    Source,
    /// `<primary-root>/loomweave.yaml` exists and was selected (no
    /// source-root file was present).
    Primary,
    /// Neither a source-root nor a primary-root `loomweave.yaml` exists.
    /// Built-in defaults apply; `<primary-root>/loomweave.yaml` is the
    /// write target if config is created.
    DefaultTarget,
}

/// Non-UTF-8 input encountered while resolving a [`WorktreeContext`].
///
/// Loomweave persists worktree metadata as JSON and hashes the Git
/// administrative identity as a UTF-8 string, so a non-UTF-8 source root,
/// primary root, Git administrative directory, or derived admin-identity
/// fragment is rejected here, before any store directory is created or any
/// git output is trusted further. This is a deliberate departure from
/// [`crate::hardened_git::list_untracked_files`]'s lossy decode: that
/// function's output is display-only, so silently substituting the Unicode
/// replacement character for invalid bytes is safe there. Here the decoded
/// value becomes a filesystem path and a hash input, so silent mangling
/// could route two different worktrees to the same (or a bogus) store.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeContextError {
    /// `field` identifies which resolution input failed to decode as UTF-8:
    /// `"source_root"`, `"git-dir"`, `"git-common-dir"`, `"worktree-list"`,
    /// or `"admin-identity"`.
    #[error("{field} is not valid UTF-8 (lossy: {lossy:?})")]
    NonUtf8Path {
        /// Which resolution input failed to decode.
        field: &'static str,
        /// A best-effort lossy rendering of the offending bytes, for
        /// diagnostics only — never used to build a path or a hash input.
        lossy: String,
    },
}

impl WorktreeContextError {
    fn non_utf8_bytes(field: &'static str, bytes: &[u8]) -> Self {
        Self::NonUtf8Path {
            field,
            lossy: String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    fn non_utf8_path(field: &'static str, path: &Path) -> Self {
        Self::NonUtf8Path {
            field,
            lossy: path.to_string_lossy().into_owned(),
        }
    }
}

/// A resolved, typed worktree context: which store a checkout uses, and why.
///
/// Every command and service that reads or writes Loomweave runtime state
/// should resolve one of these (or receive its [`StorePaths`]) instead of
/// re-deriving `store_dir(project_root)` — the whole point of this type is
/// that re-derivation from a source root is exactly what breaks isolation
/// for a linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeContext {
    /// How `source_root` relates to Git's worktree structure.
    pub kind: WorktreeKind,
    /// The canonicalized path that was resolved (a symlink resolves to its
    /// target here, so a checkout reached through a symlink gets the same
    /// identity as one reached directly).
    pub source_root: PathBuf,
    /// The canonicalized main-worktree path. Equals `source_root` unless
    /// `kind` is [`WorktreeKind::Linked`].
    pub primary_root: PathBuf,
    /// `store_dir(primary_root)` — the primary checkout's store directory,
    /// unaffected by any `[loomweave].store_dir` override composing
    /// correctly regardless of which worktree resolved it.
    pub repository_store: PathBuf,
    /// The store this checkout actually reads and writes:
    /// `repository_store` unless `kind` is [`WorktreeKind::Linked`], in
    /// which case `repository_store/worktrees/<stable_id>`.
    pub effective_store: PathBuf,
    /// Explicit leaf paths under `effective_store`.
    pub store_paths: StorePaths,
    /// Which `loomweave.yaml` precedence rung applies.
    pub config_origin: ConfigOrigin,
    /// `wt-<BLAKE3 hex of the Git administrative identity>`, present only
    /// when `kind` is [`WorktreeKind::Linked`].
    pub stable_id: Option<String>,
    /// The Git administrative identity `stable_id` is a `BLAKE3` hash of —
    /// the path fragment under the common Git directory (e.g.
    /// `worktrees/federation-seam-followups`), present only when `kind` is
    /// [`WorktreeKind::Linked`]. Informational only (persisted into a linked
    /// worktree's `metadata.json` for operator/debugging visibility per the
    /// design's on-disk schema): nothing in this module or its callers
    /// derives `stable_id` from this field, or uses it in any
    /// mismatch/validity check — `stable_id` is computed directly from the
    /// hash the resolver already took, and `source_root` (compared
    /// canonicalized) remains the sole store-identity check.
    pub git_admin_identity: Option<String>,
}

impl WorktreeContext {
    /// Resolve the typed worktree context for `source_root`.
    ///
    /// Canonicalizes `source_root` first (falling back to the given path,
    /// uncanonicalized, if it cannot be resolved on disk — e.g. it does not
    /// exist), then classifies it via `git rev-parse` and
    /// `git worktree list --porcelain -z`. Any failure to positively prove a
    /// linked relationship — no Git repository, a bare primary, or an
    /// unparseable/absent worktree administrative link — falls back to
    /// [`WorktreeKind::Standalone`] rather than guessing. The only error
    /// this returns is [`WorktreeContextError::NonUtf8Path`].
    pub fn resolve(source_root: &Path) -> Result<Self, WorktreeContextError> {
        let canonical_source = require_utf8("source_root", canonicalize_or_given(source_root))?;

        match probe_git(&canonical_source)? {
            GitProbe::Main => Ok(Self::own_store(WorktreeKind::Main, canonical_source)),
            GitProbe::Linked {
                primary_root,
                stable_id,
                admin_identity,
            } => Ok(Self::linked_store(
                canonical_source,
                primary_root,
                stable_id,
                admin_identity,
            )),
            GitProbe::Unresolvable => {
                Ok(Self::own_store(WorktreeKind::Standalone, canonical_source))
            }
        }
    }

    /// Build a context that uses its own, unchanged store (standalone or
    /// main).
    fn own_store(kind: WorktreeKind, source_root: PathBuf) -> Self {
        let repository_store = store_dir(&source_root);
        let config_origin = resolve_config_origin(&source_root, &source_root);
        Self {
            kind,
            primary_root: source_root.clone(),
            effective_store: repository_store.clone(),
            store_paths: StorePaths::under(&repository_store),
            repository_store,
            config_origin,
            stable_id: None,
            git_admin_identity: None,
            source_root,
        }
    }

    /// Build a context for a positively identified linked worktree.
    fn linked_store(
        source_root: PathBuf,
        primary_root: PathBuf,
        stable_id: String,
        admin_identity: String,
    ) -> Self {
        let repository_store = store_dir(&primary_root);
        let effective_store = repository_store.join("worktrees").join(&stable_id);
        let config_origin = resolve_config_origin(&source_root, &primary_root);
        Self {
            kind: WorktreeKind::Linked,
            source_root,
            primary_root,
            store_paths: StorePaths::under(&effective_store),
            repository_store,
            effective_store,
            config_origin,
            stable_id: Some(stable_id),
            git_admin_identity: Some(admin_identity),
        }
    }
}

/// The outcome of probing Git for `source_root`'s worktree relationship.
enum GitProbe {
    /// `source_root` is the primary/main working tree.
    Main,
    /// `source_root` is a linked worktree of a positively identified,
    /// non-bare primary.
    Linked {
        primary_root: PathBuf,
        stable_id: String,
        admin_identity: String,
    },
    /// No Git repository, a bare primary, or the primary could not be
    /// positively identified. Callers fall back to `Standalone`.
    Unresolvable,
}

/// A parsed `git worktree list --porcelain -z` entry.
struct WorktreeListEntry {
    path: PathBuf,
    bare: bool,
}

fn probe_git(source: &Path) -> Result<GitProbe, WorktreeContextError> {
    let Some(own_git_dir) = git_dir_of(source)? else {
        return Ok(GitProbe::Unresolvable);
    };

    let Some(common_dir_raw) = run_git_stdout(
        source,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) else {
        return Ok(GitProbe::Unresolvable);
    };
    let common_dir_str = decode_git_line(&common_dir_raw, "git-common-dir")?;
    let common_dir = canonicalize_or_given(&resolve_absolute(source, &common_dir_str));

    let Some(list_raw) = run_git_stdout(source, &["worktree", "list", "--porcelain", "-z"]) else {
        return Ok(GitProbe::Unresolvable);
    };
    let entries = parse_worktree_list(&list_raw)?;

    // Identify the primary by the entry whose OWN git-dir equals the common
    // git-dir — never by directory or branch name (a linked worktree may be
    // misleadingly named "main").
    let mut primary_entry = None;
    for entry in entries {
        let Some(entry_git_dir) = git_dir_of(&entry.path)? else {
            continue;
        };
        if entry_git_dir == common_dir {
            primary_entry = Some(entry);
            break;
        }
    }

    let Some(primary_entry) = primary_entry else {
        return Ok(GitProbe::Unresolvable);
    };
    if primary_entry.bare {
        // A worktree hub with no main working tree: out of scope for
        // isolation (design doc, "Worktree context").
        return Ok(GitProbe::Unresolvable);
    }

    if own_git_dir == common_dir {
        return Ok(GitProbe::Main);
    }

    // `canonicalize_or_given` resolves symlinks; that can introduce
    // non-UTF-8 path components that were never present in git's own
    // (already-validated) reported string. Guard here too, same as
    // `source_root` — the brief names `primary_root` explicitly among the
    // inputs that must be rejected before any store path is built.
    let primary_root = require_utf8("primary_root", canonicalize_or_given(&primary_entry.path))?;

    let Ok(admin_identity_path) = own_git_dir.strip_prefix(&common_dir) else {
        // The linked git-dir isn't nested under the common dir at all — an
        // administrative shape this resolver doesn't understand. Fall back
        // rather than guess.
        return Ok(GitProbe::Unresolvable);
    };
    let admin_identity = admin_identity_path
        .to_str()
        .ok_or_else(|| WorktreeContextError::non_utf8_path("admin-identity", admin_identity_path))?
        .to_owned();

    let stable_id = format!("wt-{}", blake3::hash(admin_identity.as_bytes()).to_hex());

    Ok(GitProbe::Linked {
        primary_root,
        stable_id,
        admin_identity,
    })
}

/// This checkout's own (canonicalized) Git directory, or `None` if `dir`
/// isn't inside a Git working tree (or `git` failed for any other reason —
/// treated identically: unresolvable, never a hard error).
fn git_dir_of(dir: &Path) -> Result<Option<PathBuf>, WorktreeContextError> {
    let Some(raw) = run_git_stdout(dir, &["rev-parse", "--path-format=absolute", "--git-dir"])
    else {
        return Ok(None);
    };
    let decoded = decode_git_line(&raw, "git-dir")?;
    Ok(Some(canonicalize_or_given(&resolve_absolute(
        dir, &decoded,
    ))))
}

/// Run a hardened `git` subcommand in `dir` and return its raw stdout, or
/// `None` on any failure (missing `git`, non-repository, nonzero exit) —
/// every caller treats that identically: fall back, never error.
fn run_git_stdout(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = hardened_git_command(dir).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

/// Strictly decode one line of `git` stdout as UTF-8 (no `from_utf8_lossy` —
/// see the module docs on why this path is a hard error here).
fn decode_git_line(bytes: &[u8], field: &'static str) -> Result<String, WorktreeContextError> {
    std::str::from_utf8(bytes)
        .map(|s| s.trim().to_owned())
        .map_err(|_| WorktreeContextError::non_utf8_bytes(field, bytes))
}

/// Resolve `raw` (as reported by `git`) to an absolute path, joining it onto
/// `base` if `git` reported a relative path (old `git` without full
/// `--path-format=absolute` support).
fn resolve_absolute(base: &Path, raw: &str) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}

/// `fs::canonicalize`, falling back to the given path unchanged when it
/// cannot be resolved on disk (e.g. it does not exist) — resolution must
/// degrade gracefully, not error, when the filesystem can't answer.
fn canonicalize_or_given(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Reject `path` if it is not valid UTF-8, tagging the error with which
/// resolution input it came from. Used for every path this resolver hands
/// back to a caller or feeds into a hash/store-path computation
/// (`source_root`, `primary_root`) — never only checked at the point a
/// value first arrives from `git`, since a later filesystem operation
/// (`canonicalize_or_given`'s symlink resolution) can introduce non-UTF-8
/// bytes that were never in the original, already-validated string.
fn require_utf8(field: &'static str, path: PathBuf) -> Result<PathBuf, WorktreeContextError> {
    if path.to_str().is_none() {
        return Err(WorktreeContextError::non_utf8_path(field, &path));
    }
    Ok(path)
}

/// Strictly decode a `git worktree list --porcelain -z` byte stream into its
/// entries. `-z` NUL-delimits every line; a blank line (a lone NUL) ends one
/// entry and starts the next, exactly like the newline-delimited blank-line
/// separator in the non-`-z` form.
fn parse_worktree_list(raw: &[u8]) -> Result<Vec<WorktreeListEntry>, WorktreeContextError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| WorktreeContextError::non_utf8_bytes("worktree-list", raw))?;

    let mut entries = Vec::new();
    let mut current: Option<WorktreeListEntry> = None;
    for line in text.split('\0') {
        if line.is_empty() {
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeListEntry {
                path: PathBuf::from(path),
                bare: false,
            });
        } else if line == "bare"
            && let Some(entry) = current.as_mut()
        {
            entry.bare = true;
        }
        // HEAD / branch / detached / locked / prunable lines carry no
        // information this resolver needs.
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    Ok(entries)
}

/// Which `loomweave.yaml` precedence rung applies, given the resolved source
/// and primary roots (see [`ConfigOrigin`]).
fn resolve_config_origin(source_root: &Path, primary_root: &Path) -> ConfigOrigin {
    if source_root.join(CONFIG_FILE_NAME).is_file() {
        ConfigOrigin::Source
    } else if primary_root.join(CONFIG_FILE_NAME).is_file() {
        ConfigOrigin::Primary
    } else {
        ConfigOrigin::DefaultTarget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Covers the `primary_root` non-UTF-8 gap directly on the guard
    /// primitives rather than through a `git`-mediated fixture.
    ///
    /// A live `git worktree list --porcelain -z` fixture was tried first
    /// (a UTF-8-named primary reached only through a symlink to a
    /// non-UTF-8-named real directory) and found empirically infeasible:
    /// `git` resolves a worktree's registered path to its real,
    /// symlink-free form *at registration time*
    /// (`git init`/`git worktree add`), so `git worktree list` always
    /// reports the already-resolved path — meaning any non-UTF-8 bytes
    /// there are already caught by `parse_worktree_list`'s strict decode,
    /// never reaching this guard. That leaves this guard as defense against
    /// what `canonicalize_or_given`'s own symlink resolution can introduce
    /// (e.g. a TOCTOU-style swap between when `git` recorded a path and
    /// when this resolver re-canonicalizes it, or platform/filesystem
    /// differences in path resolution) — exercised here directly against
    /// a real symlink to a real non-UTF-8-named directory, proving
    /// `canonicalize_or_given` really does surface non-UTF-8 bytes on this
    /// platform and that `require_utf8` rejects them.
    #[test]
    #[cfg(unix)]
    fn require_utf8_rejects_a_symlink_resolving_to_a_non_utf8_target() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let real_target = dir
            .path()
            .join(std::ffi::OsStr::from_bytes(b"real-\xFF-primary"));
        std::fs::create_dir(&real_target).unwrap();
        let symlink = dir.path().join("utf8-primary-link");
        std::os::unix::fs::symlink(&real_target, &symlink).unwrap();

        let canonical = canonicalize_or_given(&symlink);
        assert!(
            canonical.to_str().is_none(),
            "sanity: canonicalize must actually resolve the symlink to the \
             non-UTF-8 real path on this platform, got {canonical:?}"
        );

        let err = require_utf8("primary_root", canonical).expect_err("must reject");
        match err {
            WorktreeContextError::NonUtf8Path { field, .. } => {
                assert_eq!(field, "primary_root");
            }
        }
    }

    #[test]
    fn require_utf8_accepts_a_valid_utf8_path() {
        let path = PathBuf::from("/store/primary");
        assert_eq!(require_utf8("primary_root", path.clone()).unwrap(), path);
    }
}
