//! External-corpus qualname-collision sweep (Scale-QA plan, D7).
//!
//! Usage: `cargo run -p loomweave-plugin-rust --example qualname_check -- <dir>`
//!
//! Builds the init-time symbol table over `<dir>` — the exact walk the plugin
//! runs inside `initialize` (crate-root discovery, src/-only scope discipline,
//! ADR-050 pre-parse guards) — and reports every duplicate entity id. Exits 0
//! with a one-line summary when the table is collision-free, 1 listing each
//! duplicate id otherwise, 2 on usage errors.

use std::path::PathBuf;
use std::process::ExitCode;

use loomweave_plugin_rust::symbol_table::build_symbol_table;

fn main() -> ExitCode {
    let Some(root) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: qualname_check <project-root>");
        return ExitCode::from(2);
    };
    if !root.is_dir() {
        eprintln!("qualname_check: not a directory: {}", root.display());
        return ExitCode::from(2);
    }

    let table = build_symbol_table(&root);
    let duplicates = table.duplicate_ids();
    if duplicates.is_empty() {
        println!(
            "qualname_check OK: {} entity ids, 0 duplicates ({})",
            table.len(),
            root.display()
        );
        return ExitCode::SUCCESS;
    }

    println!(
        "qualname_check FAIL: {} entity ids, {} duplicate id(s) ({})",
        table.len(),
        duplicates.len(),
        root.display()
    );
    for id in &duplicates {
        println!("duplicate: {id}");
    }
    ExitCode::FAILURE
}
