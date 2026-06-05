//! Core consult-mode tool implementations, split out of `lib.rs` (V11-ARCH-04).
//!
//! Each submodule attaches `tool_*` methods (and their private helpers) to
//! [`crate::ServerState`] via an inherent `impl` block. The crate root retains
//! the shared free-function helpers, the tool catalogue (`list_tools`), and the
//! `tools/call` JSON-RPC dispatch that fans out to these methods. This mirrors
//! the existing `catalogue/` split for the WS5 stateless tools.

mod analyze;
mod graph;
mod orientation;
mod status;
mod summary;
