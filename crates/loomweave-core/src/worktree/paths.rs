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
}
