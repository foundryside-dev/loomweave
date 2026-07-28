//! Worktree-store lifecycle helpers for the CLI.
//!
//! Today this holds only [`confine`], the confined-deletion primitive that
//! every deletion in the worktree-index feature must route through — see
//! its module docs for why. Later tasks (bootstrap, explicit `worktree
//! analyze`, the cleanup sweep) add sibling modules here.

pub mod confine;
