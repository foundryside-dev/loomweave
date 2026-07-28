//! Library surface for `loomweave-cli`.
//!
//! `main.rs` remains this crate's sole binary entry point and does not
//! depend on this lib target. This lib exists only so integration tests
//! under `tests/` can exercise crate-internal modules ahead of any CLI
//! subcommand wiring them up — starting with the confined-deletion
//! primitive in [`worktree::confine`], which lands (deliberately) before
//! the worktree-index callers that will use it, so no interim
//! string-path deletion ever exists in this feature's history.
//!
//! The pattern mirrors `loomweave-plugin-rust`, whose thin `main.rs` calls
//! straight into its own lib crate (`loomweave_plugin_rust::serve::run()`):
//! Cargo automatically links a package's bin target against its own lib
//! target, so a future CLI caller here can reach this module as
//! `loomweave_cli::worktree::confine::...` without any further `Cargo.toml`
//! wiring.

pub mod worktree;
