//! Task 8 (Phase 1b) — `implements` edges resolved through the symbol table.
//!
//! Drives the edges-aware extraction entry point with a real `Resolver` built
//! over a small in-memory project, asserting that `impl Trait for Type` blocks
//! resolve to anchored `implements` edges:
//! - an in-project `impl Tr for Foo` → a **Resolved** edge from the impl entity
//!   to the trait id, anchored on the TRAIT PATH's source span,
//! - an external `impl std::fmt::Display for Foo` → **NO** edge (D1: external
//!   dropped at emit by the resolver).
//!
//! The `implements` edge is anchored: it MUST carry non-null byte offsets (the
//! implemented-trait path's source span) and MUST NOT be `inferred` confidence.

use loomweave_plugin_rust::extract::extract_file_with_edges;
use loomweave_plugin_rust::resolve::Resolver;
use loomweave_plugin_rust::symbol_table::build_symbol_table;
use serde_json::Value;

#[test]
fn implements_resolve_in_project_and_drop_external_trait() {
    // A one-crate project: `c_crate` declares `trait Tr` and `struct Foo` at the
    // crate root, plus an `impl Tr for Foo` and an external
    // `impl std::fmt::Display for Foo`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(
        root.join("c/src/lib.rs"),
        "pub trait Tr { fn go(&self); }\npub struct Foo;\n",
    )
    .unwrap();

    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // The file under analysis declares the two impls. `Tr` is a bare crate-root
    // name (crate-root-relative); `std::fmt::Display` is genuinely external.
    let src = "use std::fmt;\n\
               pub trait Tr { fn go(&self); }\n\
               pub struct Foo;\n\
               impl Tr for Foo { fn go(&self) {} }\n\
               impl std::fmt::Display for Foo {\n\
               fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { Ok(()) }\n\
               }\n";
    let extracted = extract_file_with_edges(
        "c_crate",
        "c_crate", // crate-root module (lib.rs)
        "/p/c/src/lib.rs",
        src,
        &r,
    )
    .unwrap();

    let implements: Vec<&Value> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "implements")
        .collect();

    // Exactly one implements edge: the in-project `impl Tr for Foo`. The
    // external `impl std::fmt::Display for Foo` is dropped entirely (D1).
    assert_eq!(
        implements.len(),
        1,
        "expected exactly 1 implements edge (the in-project trait impl), got {implements:#?}",
    );

    let edge = implements[0];

    // `to` is the in-project trait id, `from` is the impl entity id, Resolved.
    assert_eq!(
        edge["to_id"], "rust:trait:c_crate.Tr",
        "implements edge must point at the in-project trait id",
    );
    assert_eq!(
        edge["from_id"], "rust:impl:c_crate.Foo.impl[Tr]",
        "implements edge must originate from the trait-impl entity id",
    );
    assert_eq!(edge["confidence"], "resolved");

    // No edge points anywhere Display-related (external dropped).
    assert!(
        !implements
            .iter()
            .any(|e| e["to_id"].as_str().is_some_and(|t| t.contains("Display"))),
        "external std::fmt::Display must NOT yield an implements edge",
    );

    // The implements edge is anchored on the TRAIT PATH's span: non-null byte
    // offsets, and NOT `inferred` confidence.
    assert!(
        edge["source_byte_start"].as_i64().is_some(),
        "implements edge must carry a non-null source_byte_start: {edge:#?}",
    );
    assert!(
        edge["source_byte_end"].as_i64().is_some_and(|b| b > 0),
        "implements edge must carry a non-null source_byte_end: {edge:#?}",
    );
    assert_ne!(
        edge["confidence"], "inferred",
        "an anchored implements edge must never be `inferred`",
    );

    // The span is the TRAIT PATH (`Tr`), not the whole impl block: it must fall
    // strictly inside the impl entity's own span.
    let impl_entity = extracted
        .entities
        .iter()
        .find(|e| e["id"] == "rust:impl:c_crate.Foo.impl[Tr]")
        .expect("the trait-impl entity must be emitted");
    let impl_start = impl_entity["source"]["source_byte_start"]
        .as_i64()
        .expect("impl entity has a start span");
    let impl_end = impl_entity["source"]["source_byte_end"]
        .as_i64()
        .expect("impl entity has an end span");
    let trait_start = edge["source_byte_start"].as_i64().unwrap();
    let trait_end = edge["source_byte_end"].as_i64().unwrap();
    assert!(
        trait_start >= impl_start && trait_end <= impl_end && trait_end > trait_start,
        "trait-path span [{trait_start},{trait_end}) must fall inside the impl \
         entity span [{impl_start},{impl_end})",
    );
}

/// An in-project GENERIC trait (`impl GenTrait<i32> for Foo`) must still resolve
/// to a Resolved `implements` edge: the resolver lookup keys on the trait's bare
/// qualname (`c_crate.GenTrait`), so the generic argument `<i32>` MUST be stripped
/// before lookup. A naive textual render (`GenTrait<i32>`) misses the table and
/// silently drops the edge — the bug this test guards.
#[test]
fn implements_resolves_in_project_generic_trait_by_stripping_args() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(
        root.join("c/src/lib.rs"),
        "pub trait GenTrait<T> { fn take(&self, t: T); }\npub struct Foo;\n",
    )
    .unwrap();

    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    let src = "pub trait GenTrait<T> { fn take(&self, t: T); }\n\
               pub struct Foo;\n\
               impl GenTrait<i32> for Foo { fn take(&self, _t: i32) {} }\n";
    let extracted =
        extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap();

    let implements: Vec<&Value> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "implements")
        .collect();

    assert_eq!(
        implements.len(),
        1,
        "an in-project generic trait impl must yield exactly one Resolved edge \
         (generic args stripped before lookup), got {implements:#?}",
    );
    assert_eq!(
        implements[0]["to_id"], "rust:trait:c_crate.GenTrait",
        "the edge must point at the trait's bare qualname, args stripped",
    );
    assert_eq!(implements[0]["confidence"], "resolved");
}

/// A NEGATIVE impl (`impl !MyTrait for Foo`) asserts NON-implementation, so it
/// must NOT emit a (positive) `implements` edge — even though the trait resolves
/// in-project. The `it.trait_` destructure carries an `Option<Bang>`; without the
/// `bang.is_none()` guard a negative impl would emit a WRONG positive edge.
///
/// The trait MUST be in-project (`MyTrait`, not e.g. `Send`): an external trait
/// resolves `External -> None` and yields no edge regardless of the guard, so
/// the test would pass vacuously. With an in-project trait the test
/// discriminates: no guard -> 1 edge (wrong), guard -> 0 edges (correct).
#[test]
fn negative_impl_emits_no_implements_edge() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(
        root.join("c/src/lib.rs"),
        "pub trait MyTrait {}\npub struct Foo;\n",
    )
    .unwrap();

    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // `impl !MyTrait for Foo {}` — nightly-gated syntax, but syn parses it.
    let src = "pub trait MyTrait {}\n\
               pub struct Foo;\n\
               impl !MyTrait for Foo {}\n";
    let extracted =
        extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap();

    let implements: Vec<&Value> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "implements")
        .collect();

    assert!(
        implements.is_empty(),
        "a negative impl (`impl !MyTrait for Foo`) must NOT emit a positive \
         implements edge; got {implements:#?}",
    );

    // The impl ENTITY itself is still emitted (only the edge is suppressed).
    assert!(
        extracted
            .entities
            .iter()
            .any(|e| e["id"] == "rust:impl:c_crate.Foo.impl[MyTrait]"),
        "the negative-impl entity must still be emitted (only the implements \
         edge is suppressed); got {:#?}",
        extracted.entities,
    );
}

/// A FIRED stage-T group (ADR-049 Amendment 7) must still resolve its
/// `implements` edges: the T qualification rewrites only the impl LOCATOR's
/// `impl[…]` fragment; the resolver lookup goes through
/// `trait_path_for_lookup`, which already uses the full written path and is
/// unaffected. Two same-named in-project traits (`a::Tr` / `b::Tr`)
/// implemented for one type fire T — and each impl still gets a Resolved
/// edge to ITS trait id, originating from the T-qualified impl entity id.
#[test]
fn fired_t_group_still_resolves_implements_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(
        root.join("c/src/lib.rs"),
        "pub mod a;\npub mod b;\npub struct Foo;\n",
    )
    .unwrap();
    std::fs::write(root.join("c/src/a.rs"), "pub trait Tr { fn go(&self); }\n").unwrap();
    std::fs::write(root.join("c/src/b.rs"), "pub trait Tr { fn go(&self); }\n").unwrap();

    let table = build_symbol_table(root);
    let r = Resolver::new(&table);

    // `crate::`-rooted spellings: a multi-segment RELATIVE path (`a::Tr`)
    // deliberately resolves External (the H5 shadowing gate in resolve.rs),
    // so the resolvable in-project spelling is `crate::a::Tr` — which is
    // also multi-segment and distinct per member, so T fires.
    let src = "pub mod a;\npub mod b;\npub struct Foo;\n\
               impl crate::a::Tr for Foo { fn go(&self) {} }\n\
               impl crate::b::Tr for Foo { fn go(&self) {} }\n";
    let extracted =
        extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap();

    let mut implements: Vec<(String, String, String)> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "implements")
        .map(|e| {
            (
                e["from_id"].as_str().unwrap().to_owned(),
                e["to_id"].as_str().unwrap().to_owned(),
                e["confidence"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    implements.sort();

    assert_eq!(
        implements,
        vec![
            (
                "rust:impl:c_crate.Foo.impl[crate%3A%3Aa%3A%3ATr]".to_owned(),
                "rust:trait:c_crate.a.Tr".to_owned(),
                "resolved".to_owned(),
            ),
            (
                "rust:impl:c_crate.Foo.impl[crate%3A%3Ab%3A%3ATr]".to_owned(),
                "rust:trait:c_crate.b.Tr".to_owned(),
                "resolved".to_owned(),
            ),
        ],
        "a fired T group must keep BOTH Resolved implements edges, each from \
         its T-qualified impl entity to ITS trait id; got {implements:#?}",
    );
}
