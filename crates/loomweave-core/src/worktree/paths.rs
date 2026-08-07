//! Explicit runtime-state leaf paths for a single Loomweave store directory.
//!
//! [`StorePaths`] names every file a store directory holds, so callers stop
//! re-deriving `db_path`, the embeddings sidecar, the instance-ID file, the
//! ephemeral-port file, the runs directory, and the advisory lock from a
//! project root by hand. One store root (the primary checkout's
//! `.weft/loomweave/`, or a linked worktree's
//! `.weft/loomweave/worktrees/<stable-id>/`) always yields the same leaf
//! names, so a single constructor covers both.

use std::path::{Path, PathBuf};

/// The structural-graph database leaf name.
const DB_FILE: &str = "loomweave.db";

/// The embeddings sidecar database leaf name.
const EMBEDDINGS_FILE: &str = "embeddings.db";

/// The per-store instance-ID leaf name.
const INSTANCE_ID_FILE: &str = "instance_id";

/// The published ephemeral-port leaf name (ADR-044's convention).
const PORT_FILE: &str = "ephemeral.port";

/// The per-run progress/metadata directory name.
const RUNS_DIR: &str = "runs";

/// The advisory-lock leaf name (`analyze_lock.rs`'s convention).
const LOCK_FILE: &str = "loomweave.lock";

/// The directory under a repository store that namespaces every linked
/// worktree's isolated store and its analyze lock:
/// `<repository-store>/worktrees/`. Shared by the CLI's store bootstrap and
/// cleanup sweep and by [`linked_worktree_analyze_lock_path`] — one name, one
/// definition, so the layout and the lock contract can never drift apart.
pub const WORKTREES_DIR_NAME: &str = "worktrees";

/// The linked-worktree analyze lock path:
/// `<repository-store>/worktrees/<stable-id>.lock`.
///
/// This is the single encoding of the lock-path contract. The lock file is a
/// stable *sibling* of the replaceable store directory
/// (`<repository-store>/worktrees/<stable-id>/`), not a leaf inside it, so
/// deleting and re-creating the store cannot orphan a held lock. A live
/// `loomweave worktree analyze` holds an exclusive `fs2` lock on this file
/// for its whole run — acquired before it writes any `runs` row and released
/// only after its final transaction lands — which is what lets other
/// processes use the lock as a builder-liveness probe. Producers
/// (`loomweave-cli`'s `analyze_lock.rs`) and probers (`loomweave-mcp`'s
/// `worktree_bootstrap.rs`, via [`linked_worktree_analyze_lock_path_for_store`])
/// must both route through this module rather than re-deriving the path.
#[must_use]
pub fn linked_worktree_analyze_lock_path(repository_store: &Path, stable_id: &str) -> PathBuf {
    repository_store
        .join(WORKTREES_DIR_NAME)
        .join(format!("{stable_id}.lock"))
}

/// Recover the analyze lock path from a linked worktree's *effective store*
/// directory (`<repository-store>/worktrees/<stable-id>/`) — the inverse of
/// [`linked_worktree_analyze_lock_path`], for callers that hold only the
/// store's paths (e.g. an MCP gate configured with [`StorePaths`]) and not
/// the full resolved context.
///
/// Returns `None` when `effective_store` is not shaped like a linked
/// worktree's isolated store (its parent directory is not named
/// [`WORKTREES_DIR_NAME`], or the path is too shallow to inspect) — e.g. a
/// primary or standalone store, whose analyze lock lives at
/// `<store>/loomweave.lock` instead and is not this contract's concern.
#[must_use]
pub fn linked_worktree_analyze_lock_path_for_store(effective_store: &Path) -> Option<PathBuf> {
    // Stable IDs are always `wt-<hex>` (UTF-8 by construction); a non-UTF-8
    // name here is not a linked store, and lossy-decoding it could route the
    // probe to a lock nobody actually holds.
    let stable_id = effective_store.file_name()?.to_str()?;
    let worktrees_dir = effective_store.parent()?;
    if worktrees_dir.file_name()? != std::ffi::OsStr::new(WORKTREES_DIR_NAME) {
        return None;
    }
    let repository_store = worktrees_dir.parent()?;
    Some(linked_worktree_analyze_lock_path(
        repository_store,
        stable_id,
    ))
}

/// Explicit leaf paths under one store root directory.
///
/// Every command and service that reads or writes Loomweave's runtime state
/// should receive a `StorePaths` (or one of its fields) rather than
/// re-deriving a leaf path from a project root — that re-derivation is
/// exactly what breaks worktree isolation, since a re-derived path always
/// lands under the *current* checkout instead of the resolved store root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePaths {
    /// `<root>/loomweave.db` — the structural-graph store.
    pub db: PathBuf,
    /// `<root>/embeddings.db` — the embeddings sidecar.
    pub embeddings: PathBuf,
    /// `<root>/instance_id` — this store's stable instance identifier.
    pub instance_id: PathBuf,
    /// `<root>/ephemeral.port` — the published `serve` port.
    pub port: PathBuf,
    /// `<root>/runs/` — per-run progress and metadata.
    pub runs: PathBuf,
    /// `<root>/loomweave.lock` — the analyze advisory lock.
    pub lock: PathBuf,
}

impl StorePaths {
    /// Build the explicit leaf paths rooted at `root` (a store directory —
    /// either the primary checkout's store, or a linked worktree's isolated
    /// store subdirectory).
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            db: root.join(DB_FILE),
            embeddings: root.join(EMBEDDINGS_FILE),
            instance_id: root.join(INSTANCE_ID_FILE),
            port: root.join(PORT_FILE),
            runs: root.join(RUNS_DIR),
            lock: root.join(LOCK_FILE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_builds_every_explicit_leaf() {
        let root = Path::new("/store/root");
        let paths = StorePaths::under(root);
        assert_eq!(paths.db, root.join("loomweave.db"));
        assert_eq!(paths.embeddings, root.join("embeddings.db"));
        assert_eq!(paths.instance_id, root.join("instance_id"));
        assert_eq!(paths.port, root.join("ephemeral.port"));
        assert_eq!(paths.runs, root.join("runs"));
        assert_eq!(paths.lock, root.join("loomweave.lock"));
    }

    #[test]
    fn linked_lock_path_is_a_stable_sibling_of_the_store() {
        assert_eq!(
            linked_worktree_analyze_lock_path(Path::new("/repo/.weft/loomweave"), "wt-abc"),
            Path::new("/repo/.weft/loomweave/worktrees/wt-abc.lock")
        );
    }

    #[test]
    fn lock_path_for_store_inverts_the_forward_derivation() {
        let repository_store = Path::new("/repo/.weft/loomweave");
        let forward = linked_worktree_analyze_lock_path(repository_store, "wt-abc");
        let store = repository_store.join(WORKTREES_DIR_NAME).join("wt-abc");
        assert_eq!(
            linked_worktree_analyze_lock_path_for_store(&store),
            Some(forward)
        );
    }

    #[test]
    fn lock_path_for_store_rejects_non_worktree_store_shapes() {
        // A primary/standalone store: parent is not `worktrees/`.
        assert_eq!(
            linked_worktree_analyze_lock_path_for_store(Path::new("/repo/.weft/loomweave")),
            None
        );
        // Too shallow to inspect.
        assert_eq!(
            linked_worktree_analyze_lock_path_for_store(Path::new("/")),
            None
        );
    }
}
