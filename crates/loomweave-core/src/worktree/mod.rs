//! Worktree-isolated storage: resolve which store a checkout uses before any
//! runtime path is derived.
//!
//! See [`WorktreeContext::resolve`] for the resolver and [`StorePaths`] for
//! the explicit per-store leaf paths it hands back.

mod context;
mod paths;

pub use context::{ConfigOrigin, WorktreeContext, WorktreeContextError, WorktreeKind};
pub use paths::StorePaths;
