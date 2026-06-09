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
    // `super::` full resolution needs the defining-module path of the `use` site,
    // which 1b does not thread through. It is a DELIBERATE deferral: a leading
    // `super` deterministically misses the table -> External (H5-safe
    // under-resolution). It MUST NOT collapse to a crate-root-relative path, which
    // would WRONG-resolve a same-tail crate-root entity.
    //
    // This table deliberately contains a crate-root entity `c_crate.S` with the
    // SAME TAIL NAME as the `super::...::S` target, so a naive crate-root collapse
    // would wrong-resolve `super::a::S` (and `super::S`) to `rust:struct:c_crate.S`.
    // The assertions below prove that does NOT happen.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    // crate root defines `S` (the wrong-resolve trap) AND module `a` (also `S`).
    std::fs::write(root.join("c/src/lib.rs"), "pub mod a;\npub struct S;\n").unwrap();
    std::fs::write(root.join("c/src/a.rs"), "pub struct S;\n").unwrap();
    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // Sanity: the crate-root trap entity really exists (proves the assert_ne
    // below is LIVE, not vacuously true on a missing entity). A `crate::S` path
    // resolves to the same id a naive super-collapse would wrong-resolve to.
    assert_eq!(
        r.resolve_use_path("c_crate", "crate::S"),
        Resolution::Resolved("rust:struct:c_crate.S".to_owned())
    );

    // `super::S` from c_crate: a crate-root collapse would wrong-resolve to the
    // crate-root `c_crate.S`. It MUST be External, and specifically NOT that id.
    let got = r.resolve_use_path("c_crate", "super::S");
    assert_eq!(got, Resolution::External);
    assert_ne!(
        got,
        Resolution::Resolved("rust:struct:c_crate.S".to_owned())
    );

    // `super::a::S` from c_crate: a crate-root collapse (drop `super`) would
    // wrong-resolve to `c_crate.a.S`. It MUST be External.
    assert_eq!(
        r.resolve_use_path("c_crate", "super::a::S"),
        Resolution::External
    );

    // `super::NoSuchThing` -> must be External (a miss), never a wrong Resolved.
    assert_eq!(
        r.resolve_use_path("c_crate", "super::NoSuchThing"),
        Resolution::External
    );
}

#[test]
fn external_crate_shadowed_by_inproject_module_stays_external() {
    // FINDING #2 (H5): an in-project module that SHADOWS an external crate name
    // must not let a multi-segment `use external::Item` wrong-resolve to the
    // in-project entity. Here an in-project `mod serde` defines `Serialize`, so
    // the crate-root fallback would form `c_crate.serde.Serialize` and match —
    // UNLESS the fallback is gated to bare single-segment paths. `serde::Serialize`
    // has a `::`, so it stays External (the external crate is meant).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub mod serde;\n").unwrap();
    std::fs::write(root.join("c/src/serde.rs"), "pub struct Serialize;\n").unwrap();
    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // Sanity: the in-project shadow entity really exists (proves the trap is live).
    assert_eq!(
        r.resolve_use_path("c_crate", "crate::serde::Serialize"),
        Resolution::Resolved("rust:struct:c_crate.serde.Serialize".to_owned())
    );

    // The H5 guard: `serde::Serialize` means the EXTERNAL crate -> External,
    // NEVER the in-project shadow.
    let got = r.resolve_use_path("c_crate", "serde::Serialize");
    assert_eq!(got, Resolution::External);
    assert_ne!(
        got,
        Resolution::Resolved("rust:struct:c_crate.serde.Serialize".to_owned())
    );
}

#[test]
fn bare_trait_name_still_resolves_via_gated_fallback() {
    // FINDING #2 composes with the bare-name path: a single-segment crate-root
    // name (`Tr` in `impl Tr for Foo`) has no `::`, so the gated fallback still
    // fires and resolves it. crate::/self:: still resolve via prefix handling.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), "pub mod a;\npub trait Tr {}\n").unwrap();
    std::fs::write(root.join("c/src/a.rs"), "pub struct S;\n").unwrap();
    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // bare single-segment trait name -> resolves via the gated fallback.
    assert_eq!(
        r.resolve_trait_path("c_crate", "Tr"),
        Resolution::Resolved("rust:trait:c_crate.Tr".to_owned())
    );
    // crate:: prefix still resolves at attempt 1 (not via fallback).
    assert_eq!(
        r.resolve_use_path("c_crate", "crate::a::S"),
        Resolution::Resolved("rust:struct:c_crate.a.S".to_owned())
    );
    // self:: prefix still resolves at attempt 1.
    assert_eq!(
        r.resolve_use_path("c_crate", "self::a::S"),
        Resolution::Resolved("rust:struct:c_crate.a.S".to_owned())
    );
}
