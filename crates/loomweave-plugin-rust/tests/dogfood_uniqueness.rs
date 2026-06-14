//! Phase 1a GATE (Task 14, spec §6): zero colliding locators over Loomweave's
//! own multi-crate workspace.
//!
//! This is the exit gate the spec names — the ADR-049 qualname scheme must emit
//! a unique locator for every entity declared across this repo's `crates/`. A
//! duplicate here is a real collision bug in the qualname scheme, not a test
//! defect: do NOT weaken the assertion to make it pass.
use loomweave_plugin_rust::symbol_table::build_symbol_table;
use std::path::PathBuf;

/// The workspace root = two parents up from this crate's manifest dir
/// (`crates/loomweave-plugin-rust` -> `crates` -> repo root).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn rust_plugin_emits_zero_colliding_locators_over_this_workspace() {
    let table = build_symbol_table(&workspace_root().join("crates"));
    let dups = table.duplicate_ids();
    assert!(
        table.len() > 100,
        "expected a substantial entity set, got {}",
        table.len()
    );
    assert_eq!(
        dups,
        Vec::<String>::new(),
        "PHASE 1a GATE FAILED — colliding locators: {dups:?}"
    );
}
