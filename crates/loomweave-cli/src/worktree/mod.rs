//! Worktree-store lifecycle helpers for the CLI.
//!
//! - [`confine`] — the confined-deletion primitive that every deletion in
//!   the worktree-index feature must route through.
//! - [`store`] — creates, validates, and (via `confine`) rebuilds an
//!   isolated worktree store on disk.
//! - [`cmd`] — resolves the `<name-or-path>` argument of `loomweave worktree
//!   analyze` to a concrete filesystem path.
//!
//! Later tasks (bootstrap, the cleanup sweep) add sibling modules here.

pub mod cmd;
pub mod confine;
pub mod store;
