//! Worktree-isolated storage: resolve which store a checkout uses before any
//! runtime path is derived.
//!
//! See [`WorktreeContext::resolve`] for the resolver and [`StorePaths`] for
//! the explicit per-store leaf paths it hands back.

mod context;
mod paths;

pub use context::{
    ConfigOrigin, WorktreeContext, WorktreeContextError, WorktreeKind, stable_id_for_admin_identity,
};
pub use paths::StorePaths;
