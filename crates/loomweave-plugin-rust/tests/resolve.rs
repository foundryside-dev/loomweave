use loomweave_plugin_rust::resolve::{Resolution, Resolver};
use loomweave_plugin_rust::symbol_table::build_symbol_table;

#[test]
fn resolves_unique_inproject_path_else_ambiguous_or_external() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub mod a;\npub trait Tr {}\n").unwrap();
    std::fs::write(root.join("c/src/a.rs"), "pub struct S;\n").unwrap();
    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // unique in-project struct -> Resolved
    assert_eq!(
        r.resolve_use_path("c_crate", "c_crate::a::S"),
        Resolution::Resolved("rust:struct:c_crate.a.S".to_owned())
    );
    // in-project trait -> Resolved (implements/imports share this)
    assert_eq!(
        r.resolve_trait_path("c_crate", "Tr"),
        Resolution::Resolved("rust:trait:c_crate.Tr".to_owned())
    );
    // glob -> Ambiguous carrying the in-project module id (never faked Resolved,
    // H5; never null — edges.to_id is NOT NULL, so Ambiguous MUST supply a real id)
    assert_eq!(
        r.resolve_use_path("c_crate", "c_crate::a::*"),
        Resolution::Ambiguous("rust:module:c_crate.a".to_owned())
    );
    // external -> External (later tasks drop it per D1)
    assert_eq!(
        r.resolve_use_path("c_crate", "serde::Serialize"),
        Resolution::External
    );
    // glob of an EXTERNAL module -> External (no in-project candidate to point at)
    assert_eq!(
        r.resolve_use_path("c_crate", "serde::*"),
        Resolution::External
    );
}

#[test]
fn super_prefixed_path_never_resolves_to_a_wrong_entity() {
    // `super::` full resolution needs the defining module path of the `use`;
    // 1b-minimal handles it conservatively (treated as crate-root, segment
    // dropped). The H5 invariant is that this MUST NEVER produce a WRONG
    // Resolved — only External or Ambiguous, or a correct Resolved.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub mod a;\npub struct Top;\n").unwrap();
    std::fs::write(root.join("c/src/a.rs"), "pub struct S;\n").unwrap();
    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // `super::a::S` from crate c_crate: conservatively drops `super`, yielding
    // qualname `c_crate.a.S` which happens to be correct here -> Resolved(correct).
    // The key guarantee: whatever it returns, it must not be a WRONG id.
    match r.resolve_use_path("c_crate", "super::a::S") {
        Resolution::Resolved(id) => assert_eq!(id, "rust:struct:c_crate.a.S"),
        Resolution::Ambiguous(_) | Resolution::External => {}
    }

    // `super::NoSuchThing` -> must be External (a miss), never a wrong Resolved.
    assert_eq!(
        r.resolve_use_path("c_crate", "super::NoSuchThing"),
        Resolution::External
    );
}
