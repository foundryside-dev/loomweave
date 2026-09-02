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
//! (`wt-<BLAKE3 hex>`) is normally derived from the Git *administrative* identity —
//! the path fragment under the common Git directory (e.g.
//! `worktrees/federation-seam-followups`) — never from the branch, `HEAD`,
//! dirty state, or absolute filesystem path. That is what lets the ID
//! survive a branch switch ([`resolve`]'s own admin identity is keyed by
//! worktree *directory* name, which `git worktree add`/`move` controls, not
//! `git checkout`) and, once `git worktree repair` has restored the
//! administrative links, a repository relocation. When an explicit
//! `store_dir` override can be shared by unrelated projects, the corresponding
//! primary project root is also domain-separated into the hash so identical
//! Git administrative names cannot route two repositories to one directory.
//!
//! [`resolve`]: WorktreeContext::resolve

use std::path::{Path, PathBuf};

use crate::hardened_git::{hardened_git_command, run_git_probe_default};
use crate::store::store_dir_resolution;
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

/// A failure while resolving a [`WorktreeContext`].
///
/// Every variant is a *fail-loud* condition: once Git's own evidence proves
/// `source_root` is a linked worktree, the resolver must never silently
/// degrade to [`WorktreeKind::Standalone`] — that fallback would let
/// `loomweave analyze` build a decoy store at the worktree's own
/// `.weft/loomweave/`, exactly the outcome worktree isolation forbids.
/// Conditions that fail to *prove* the linked relationship in the first
/// place (no Git repository, a bare primary) still fall back to
/// `Standalone` without error; conditions that arise *after* the linked
/// relationship is proven error out here instead.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeContextError {
    /// Non-UTF-8 input encountered during resolution.
    ///
    /// Loomweave persists worktree metadata as JSON and hashes the Git
    /// administrative identity as a UTF-8 string, so a non-UTF-8 source
    /// root, primary root, Git administrative directory, or derived
    /// admin-identity fragment is rejected here, before any store directory
    /// is created or any git output is trusted further. It is the same strict
    /// decode [`crate::hardened_git::GitProbeOutput::stdout_utf8`] applies to
    /// every corpus git probe: the decoded value becomes a filesystem path and
    /// a hash input, so silent mangling could route two different worktrees to
    /// the same (or a bogus) store.
    ///
    /// `field` identifies which resolution input failed to decode as UTF-8:
    /// `"source_root"`, `"git-dir"`, `"git-common-dir"`, `"worktree-list"`,
    /// `"toplevel"`, or `"admin-identity"`.
    #[error("{field} is not valid UTF-8 (lossy: {lossy:?})")]
    NonUtf8Path {
        /// Which resolution input failed to decode.
        field: &'static str,
        /// A best-effort lossy rendering of the offending bytes, for
        /// diagnostics only — never used to build a path or a hash input.
        lossy: String,
    },
    /// Git proved `source_root` is a linked worktree, but the checkout's own
    /// root could not be established, so the isolated store cannot be
    /// routed. Refusing the standalone fallback is deliberate: it would
    /// build a decoy store inside the worktree.
    #[error(
        "{source_root:?} is a linked Git worktree, but its own checkout root could not be \
         resolved ({detail}); refusing to fall back to a worktree-local store. If this worktree \
         was moved or its administrative links are damaged, run `git worktree repair` from the \
         primary checkout, then retry"
    )]
    LinkedWorktreeUnresolvable {
        /// The canonicalized source root that was being resolved.
        source_root: PathBuf,
        /// What specifically could not be established.
        detail: &'static str,
    },
    /// `source_root` is a project directory nested inside a linked worktree,
    /// but the corresponding directory does not exist in the primary
    /// checkout — e.g. the project was added on this worktree's branch and
    /// has not reached the primary's checked-out branch yet. Loomweave
    /// anchors a linked worktree's store at the corresponding project
    /// directory in the primary checkout, so there is no store location to
    /// route to until that directory exists.
    #[error(
        "{source_root:?} is inside a linked Git worktree, but its corresponding project \
         directory {primary_project:?} does not exist in the primary checkout \
         {primary_checkout:?} (the project likely exists only on this worktree's branch). \
         Loomweave anchors a linked worktree's store at that primary-side directory, so this \
         project cannot be resolved from the worktree yet — check out or merge a branch \
         containing it in the primary checkout first, or run Loomweave against a project \
         directory that exists in both"
    )]
    NestedProjectMissingFromPrimary {
        /// The canonicalized source root that was being resolved.
        source_root: PathBuf,
        /// The primary-side project directory that does not exist.
        primary_project: PathBuf,
        /// The primary checkout root the project directory was expected
        /// under.
        primary_checkout: PathBuf,
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
    /// The canonicalized corresponding project path in the main worktree.
    /// Equals `source_root` unless `kind` is [`WorktreeKind::Linked`]. For a
    /// nested project resolved at `services/api`, this is the primary
    /// checkout's `services/api`, not the Git checkout root.
    pub primary_root: PathBuf,
    /// `store_dir(primary_root)` — the primary checkout's store directory. A
    /// `[loomweave].store_dir` override in `weft.toml` DOES apply here: it
    /// composes correctly precisely because this is always anchored at
    /// `primary_root`, so every worktree of the same repository resolves to
    /// the same `repository_store` regardless of which checkout initiated
    /// the resolve.
    pub repository_store: PathBuf,
    /// Whether `repository_store` came from a `[loomweave].store_dir`
    /// override in `weft.toml` (`true`) or the built-in `.weft/loomweave/`
    /// default (`false`) — recorded at resolve time. Safety decisions keyed
    /// on the override (the cleanup sweep's report-only mode: an absolute
    /// override can be shared between unrelated repositories) must consult
    /// this field, never re-read `weft.toml` at decision time: `analyze`
    /// resolves its context at start but sweeps at the end of a long run,
    /// and an override removed in between must not flip the sweep into
    /// delete mode against paths that were resolved under the override
    /// (clarion-306ed41ce3).
    pub store_dir_overridden: bool,
    /// The store this checkout actually reads and writes:
    /// `repository_store` unless `kind` is [`WorktreeKind::Linked`], in
    /// which case `repository_store/worktrees/<stable_id>`.
    pub effective_store: PathBuf,
    /// Explicit leaf paths under `effective_store`.
    pub store_paths: StorePaths,
    /// Which `loomweave.yaml` precedence rung applies.
    pub config_origin: ConfigOrigin,
    /// `wt-<BLAKE3 hex>` of the Git administrative identity (plus the primary
    /// project identity under a shared `store_dir` override), present only
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
    /// [`WorktreeKind::Standalone`] rather than guessing. Once the linked
    /// relationship IS proven, later failures error out instead of degrading
    /// (see [`WorktreeContextError`]): a proven-linked checkout must never
    /// silently receive a worktree-local (decoy) store.
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
        let (repository_store, store_dir_overridden) = store_dir_resolution(&source_root);
        let config_origin = resolve_config_origin(&source_root, &source_root);
        Self {
            kind,
            primary_root: source_root.clone(),
            effective_store: repository_store.clone(),
            store_paths: StorePaths::under(&repository_store),
            repository_store,
            store_dir_overridden,
            config_origin,
            stable_id: None,
            git_admin_identity: None,
            source_root,
        }
    }

    /// The effective `loomweave.yaml` path per [`Self::config_origin`]: the
    /// file `loomweave config check`/`llm status`/`semantic status` should
    /// treat as active, and the file `llm set`/`semantic set` should write,
    /// whenever the caller has no explicit `--config` override of its own.
    ///
    /// A caller that *does* have an explicit `--config` path should use that
    /// path directly and never call this method — [`ConfigOrigin::Explicit`]
    /// has no associated path on this struct (nothing here ever learns the
    /// explicit path a caller was given), so that arm falls back to the same
    /// primary-root write target as [`ConfigOrigin::DefaultTarget`] rather
    /// than panicking. [`WorktreeContext::resolve`] never produces
    /// `Explicit` itself (see the enum's docs), so in practice this method
    /// only ever returns the `Source` or `Primary`/`DefaultTarget` path.
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        match self.config_origin {
            ConfigOrigin::Source => self.source_root.join(CONFIG_FILE_NAME),
            ConfigOrigin::Primary | ConfigOrigin::DefaultTarget | ConfigOrigin::Explicit => {
                self.primary_root.join(CONFIG_FILE_NAME)
            }
        }
    }

    /// Build a context for a positively identified linked worktree.
    fn linked_store(
        source_root: PathBuf,
        primary_root: PathBuf,
        mut stable_id: String,
        admin_identity: String,
    ) -> Self {
        let (repository_store, store_dir_overridden) = store_dir_resolution(&primary_root);
        if store_dir_overridden {
            stable_id = stable_id_for_shared_store_project(&primary_root, &admin_identity);
        }
        let effective_store = repository_store.join("worktrees").join(&stable_id);
        let config_origin = resolve_config_origin(&source_root, &primary_root);
        Self {
            kind: WorktreeKind::Linked,
            source_root,
            primary_root,
            store_paths: StorePaths::under(&effective_store),
            repository_store,
            store_dir_overridden,
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

    // From here on, git's own evidence has PROVEN this checkout is a linked
    // worktree of a non-bare primary, so no failure below may silently
    // degrade to `Unresolvable`/`Standalone`: that fallback would let
    // `analyze` build a decoy store at the worktree's own
    // `.weft/loomweave/`, exactly what isolation forbids. Anything that
    // cannot be resolved is a hard, actionable error instead.
    //
    // The checkout's own root comes from `git rev-parse --show-toplevel`
    // run at `source`, never from this checkout's `git worktree list`
    // entry: the list reports the *registered* path, which goes stale when
    // a linked worktree is moved without `git worktree repair` (the
    // administrative `gitdir` file still names the old location) — while
    // git commands run inside the moved checkout keep working, because its
    // `.git` file points at the unmoved common dir. Keying on the stale
    // entry would misroute exactly that moved-worktree case.
    let Some(toplevel_raw) = run_git_stdout(source, &["rev-parse", "--show-toplevel"]) else {
        return Err(WorktreeContextError::LinkedWorktreeUnresolvable {
            source_root: source.to_path_buf(),
            detail: "`git rev-parse --show-toplevel` failed at the source root",
        });
    };
    let toplevel_str = decode_git_line(&toplevel_raw, "toplevel")?;
    let own_checkout_root = canonicalize_or_given(&resolve_absolute(source, &toplevel_str));
    let Ok(project_suffix) = source.strip_prefix(&own_checkout_root) else {
        return Err(WorktreeContextError::LinkedWorktreeUnresolvable {
            source_root: source.to_path_buf(),
            detail: "the source root is not under the checkout root git reported",
        });
    };
    let primary_checkout_root = canonicalize_or_given(&primary_entry.path);
    let primary_project = primary_checkout_root.join(project_suffix);
    // A nested project may exist only on this worktree's branch; its
    // primary-side counterpart then does not exist, and no store can be
    // anchored there. Detect that here, with a truthful error, instead of
    // letting callers bail against a store path under a directory that does
    // not exist (an unsatisfiable "run `loomweave install`" hint).
    if !project_suffix.as_os_str().is_empty() && !primary_project.is_dir() {
        return Err(WorktreeContextError::NestedProjectMissingFromPrimary {
            source_root: source.to_path_buf(),
            primary_project,
            primary_checkout: primary_checkout_root,
        });
    }
    // `canonicalize_or_given` resolves symlinks; that can introduce
    // non-UTF-8 path components that were never present in git's own
    // (already-validated) reported string. Guard here too, same as
    // `source_root` — the brief names `primary_root` explicitly among the
    // inputs that must be rejected before any store path is built.
    let primary_root = require_utf8("primary_root", canonicalize_or_given(&primary_project))?;

    let Ok(admin_identity_path) = own_git_dir.strip_prefix(&common_dir) else {
        // The linked git-dir isn't nested under the common dir at all — an
        // administrative shape this resolver doesn't understand. Fail loud
        // rather than guess: this checkout is proven linked, so the
        // standalone fallback would build a decoy store.
        return Err(WorktreeContextError::LinkedWorktreeUnresolvable {
            source_root: source.to_path_buf(),
            detail: "the linked worktree's git dir is not nested under the common git dir",
        });
    };
    let admin_identity = admin_identity_path
        .to_str()
        .ok_or_else(|| WorktreeContextError::non_utf8_path("admin-identity", admin_identity_path))?
        .to_owned();
    // Windows: `Path::strip_prefix` yields `worktrees\<name>`, but the
    // stable-ID contract (and the sweep, which hashes `"worktrees/" +
    // <readdir name>`) pins the forward-slash form — without this the
    // resolver and the sweep would hash DIVERGENT identities and the sweep
    // would read every live store as unregistered (clarion-4cd5b0b3b9).
    // Unix stays byte-exact: `\` is a legal filename byte there, and
    // rewriting it would desynchronize this identity from the sweep's raw
    // `readdir` name in the opposite direction.
    #[cfg(windows)]
    let admin_identity = admin_identity.replace('\\', "/");

    let stable_id = stable_id_for_admin_identity(&admin_identity);

    Ok(GitProbe::Linked {
        primary_root,
        stable_id,
        admin_identity,
    })
}

/// Compute a linked worktree's stable ID from its Git administrative
/// identity — the path fragment under the common Git directory (e.g.
/// `worktrees/federation-seam-followups`, see the module docs): `wt-`
/// followed by the `BLAKE3` hex digest of `admin_identity`'s UTF-8 bytes.
///
/// This is the single source of truth for that hash construction.
/// [`WorktreeContext::resolve`] calls it (via this module's private `git`
/// probe) while resolving a live worktree relationship from `git`'s own
/// output; the cleanup sweep
/// (`loomweave-cli`'s `worktree::sweep`, worktree-index Task 6) calls it
/// while re-deriving every currently-*registered* worktree's stable ID
/// directly from the common Git directory's own `worktrees/` administrative
/// entries (never from `git worktree list`, which does not expose an
/// entry's administrative directory name). Both call sites must produce
/// byte-identical IDs for the same admin identity, so this is written once
/// here and imported, never duplicated.
#[must_use]
pub fn stable_id_for_admin_identity(admin_identity: &str) -> String {
    format!("wt-{}", blake3::hash(admin_identity.as_bytes()).to_hex())
}

/// Derive the project-qualified stable ID used when multiple repositories may
/// share one configured `store_dir`. Cleanup enumeration must use this same
/// construction or every live override-backed store appears unregistered.
///
/// # Panics
///
/// Panics when `primary_root` is not valid UTF-8. Production callers obtain
/// this path from [`WorktreeContext`], whose resolver rejects non-UTF-8 roots.
#[must_use]
pub fn stable_id_for_shared_store_project(primary_root: &Path, admin_identity: &str) -> String {
    let primary_root = primary_root
        .to_str()
        .expect("WorktreeContext rejects non-UTF-8 primary roots before store routing");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"loomweave.shared-store-project.v1\0");
    hasher.update(primary_root.as_bytes());
    hasher.update(b"\0");
    hasher.update(admin_identity.as_bytes());
    format!("wt-{}", hasher.finalize().to_hex())
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

/// Run a hardened, BOUNDED `git` subcommand in `dir` and return its raw stdout,
/// or `None` on any failure (missing `git`, non-repository, nonzero exit, or —
/// since clarion-9202f4acec — the probe's deadline or stdout cap) — every
/// caller treats that identically: fall back, never error.
///
/// The strict-UTF-8 decision stays with the caller, in [`decode_git_line`]:
/// resolution output becomes a filesystem path and a store-routing hash input,
/// so non-UTF-8 there is a hard [`WorktreeContextError::NonUtf8Path`], not a
/// silent fallback. That is why this returns bytes rather than
/// `GitProbeOutput::stdout_utf8`'s `&str`.
fn run_git_stdout(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = hardened_git_command(dir);
    command.args(args);
    Some(run_git_probe_default(command).ok()?.stdout)
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
            other => panic!("expected NonUtf8Path, got {other:?}"),
        }
    }

    #[test]
    fn require_utf8_accepts_a_valid_utf8_path() {
        let path = PathBuf::from("/store/primary");
        assert_eq!(require_utf8("primary_root", path.clone()).unwrap(), path);
    }

    #[test]
    fn config_path_follows_origin() {
        let source_root = PathBuf::from("/worktrees/feature");
        let primary_root = PathBuf::from("/primary");
        let base = WorktreeContext::linked_store(
            source_root.clone(),
            primary_root.clone(),
            "wt-deadbeef".to_owned(),
            "worktrees/feature".to_owned(),
        );

        let source = WorktreeContext {
            config_origin: ConfigOrigin::Source,
            ..base.clone()
        };
        assert_eq!(source.config_path(), source_root.join("loomweave.yaml"));

        let primary = WorktreeContext {
            config_origin: ConfigOrigin::Primary,
            ..base.clone()
        };
        assert_eq!(primary.config_path(), primary_root.join("loomweave.yaml"));

        let default_target = WorktreeContext {
            config_origin: ConfigOrigin::DefaultTarget,
            ..base
        };
        assert_eq!(
            default_target.config_path(),
            primary_root.join("loomweave.yaml")
        );
    }
}
