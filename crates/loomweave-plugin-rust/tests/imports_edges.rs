//! Task 7 (Phase 1b) — `imports` edges resolved through the symbol table.
//!
//! Drives the edges-aware extraction entry point with a real `Resolver` built
//! over a small in-memory project, asserting that `use` statements resolve to
//! anchored `imports` edges:
//! - an in-project `use c_crate::a::S;` → a **Resolved** edge to the struct id,
//! - a glob `use c_crate::a::*;` → an **Ambiguous** edge to the module id,
//! - an external `use serde::Serialize;` → **NO** edge (D1: external dropped).
//!
//! The `imports` edges are anchored: they MUST carry non-null byte offsets
//! (the `use` statement's source span) and MUST NOT be `inferred` confidence.

use loomweave_plugin_rust::extract::extract_file_with_edges;
use loomweave_plugin_rust::resolve::Resolver;
use loomweave_plugin_rust::symbol_table::build_symbol_table;
use serde_json::Value;

#[test]
fn imports_resolve_drop_external_and_anchor_bytes() {
    // A one-crate project: `c_crate` with a sub-module `a` declaring `struct S`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub mod a;\n").unwrap();
    std::fs::write(root.join("c/src/a.rs"), "pub struct S;\n").unwrap();

    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // The file under analysis: three `use` statements, one of each outcome.
    let src = "use c_crate::a::S;\nuse c_crate::a::*;\nuse serde::Serialize;\n";
    let extracted = extract_file_with_edges(
        "c_crate",
        "c_crate.consumer",
        "/p/c/src/consumer.rs",
        src,
        &r,
    )
    .unwrap();

    let imports: Vec<&Value> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "imports")
        .collect();

    // Exactly two imports edges: the Resolved struct and the Ambiguous glob.
    // The external `serde::Serialize` is dropped entirely (D1).
    assert_eq!(
        imports.len(),
        2,
        "expected exactly 2 imports edges (Resolved + Ambiguous), got {imports:#?}",
    );

    // Resolved: `use c_crate::a::S;` -> the struct id, confidence "resolved".
    let resolved = imports
        .iter()
        .find(|e| e["to_id"] == "rust:struct:c_crate.a.S")
        .expect("a Resolved imports edge to rust:struct:c_crate.a.S");
    assert_eq!(resolved["confidence"], "resolved");

    // Ambiguous: `use c_crate::a::*;` -> the module id, confidence "ambiguous".
    let ambiguous = imports
        .iter()
        .find(|e| e["to_id"] == "rust:module:c_crate.a")
        .expect("an Ambiguous imports edge to rust:module:c_crate.a");
    assert_eq!(ambiguous["confidence"], "ambiguous");

    // No edge points anywhere serde-related (external dropped).
    assert!(
        !imports
            .iter()
            .any(|e| e["to_id"].as_str().is_some_and(|t| t.contains("serde"))),
        "external serde::Serialize must NOT yield an imports edge",
    );

    // Both imports edges are anchored: from the enclosing module, with non-null
    // byte offsets, and NOT `inferred` confidence.
    for e in &imports {
        assert_eq!(e["from_id"], "rust:module:c_crate.consumer");
        assert!(
            e["source_byte_start"].as_i64().is_some(),
            "imports edge must carry a non-null source_byte_start: {e:#?}",
        );
        assert!(
            e["source_byte_end"].as_i64().is_some_and(|b| b > 0),
            "imports edge must carry a non-null source_byte_end: {e:#?}",
        );
        assert_ne!(
            e["confidence"], "inferred",
            "an anchored imports edge must never be `inferred`",
        );
    }
}

/// `use a::b::{self, Item};` — the "import the module itself plus an item"
/// idiom. The `self` leaf must resolve to the module (`a::b`), NOT `a::b::self`
/// (which would miss the table and silently drop the module edge). The aliased
/// re-export resolves on its REAL path; the group fans out to both.
#[test]
fn group_self_resolves_the_module_and_rename_uses_the_real_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub mod a;\n").unwrap();
    std::fs::write(root.join("c/src/a.rs"), "pub struct S;\n").unwrap();

    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // `{self, S as Renamed}`: `self` -> the module `c_crate.a`; `S as Renamed`
    // -> resolve the real `S`, alias dropped.
    let src = "use c_crate::a::{self, S as Renamed};\n";
    let extracted = extract_file_with_edges(
        "c_crate",
        "c_crate.consumer",
        "/p/c/src/consumer.rs",
        src,
        &r,
    )
    .unwrap();

    let to_ids: Vec<&str> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "imports")
        .filter_map(|e| e["to_id"].as_str())
        .collect();

    assert!(
        to_ids.contains(&"rust:module:c_crate.a"),
        "`{{self}}` must resolve to the module id, got {to_ids:?}",
    );
    assert!(
        to_ids.contains(&"rust:struct:c_crate.a.S"),
        "`S as Renamed` must resolve on the real path to the struct id, got {to_ids:?}",
    );
}

/// `use a::{b, c::d};` — a NESTED group: a bare leaf (`b`) AND a `Path`-inside-
/// `Group` (`c::d`). Both leaves must fan out to their own `imports` edge. The
/// expansion logic handles a `Path` branch inside a `Group`, but no prior test
/// exercised that branch (the existing group test only has bare/`self`/renamed
/// leaves). Here both `a::b` and `a::c::d` resolve in-project so both land as
/// Resolved edges.
#[test]
fn nested_group_use_expands_both_leaves() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src/a/c")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub mod a;\n").unwrap();
    // `a` is a module dir: `a/mod.rs` declares `struct b` and `pub mod c`,
    // `a/c.rs` declares `struct d`. So `a::b` -> struct, `a::c::d` -> struct.
    std::fs::write(root.join("c/src/a/mod.rs"), "pub struct b;\npub mod c;\n").unwrap();
    std::fs::write(root.join("c/src/a/c.rs"), "pub struct d;\n").unwrap();

    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // The nested group: a bare leaf `b` and a `Path`-inside-`Group` leaf `c::d`.
    let src = "use c_crate::a::{b, c::d};\n";
    let extracted = extract_file_with_edges(
        "c_crate",
        "c_crate.consumer",
        "/p/c/src/consumer.rs",
        src,
        &r,
    )
    .unwrap();

    let to_ids: Vec<&str> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "imports")
        .filter_map(|e| e["to_id"].as_str())
        .collect();

    assert!(
        to_ids.contains(&"rust:struct:c_crate.a.b"),
        "the bare leaf `b` must expand to `a::b`, got {to_ids:?}",
    );
    assert!(
        to_ids.contains(&"rust:struct:c_crate.a.c.d"),
        "the `Path`-inside-`Group` leaf `c::d` must expand to `a::c::d`, got {to_ids:?}",
    );
}
