//! Phase 2 — `derives` edges resolved through the symbol table.
//!
//! Drives the edges-aware extraction entry point with a real `Resolver` built
//! over a small in-memory project, asserting that `#[derive(...)]` attribute
//! paths on structs/enums resolve to anchored `derives` edges:
//! - an in-project `#[derive(Pretty)]` → a **Resolved** edge from the deriving
//!   struct/enum entity to the trait id, anchored on the DERIVE PATH's span
//!   (the `Pretty` token inside the attribute, never the whole item),
//! - external derives (`#[derive(Debug, Clone)]`) → **NO** edge (D1: external
//!   dropped at emit by the resolver),
//! - non-`derive` attributes and unparseable derive lists mint nothing.
//!
//! Every assertion checks the emitted `derives` edge SET exactly (kind, from,
//! to, confidence, byte span) — never just presence — because the storage
//! writer's `ON CONFLICT` upsert silently merges duplicate rows.

use loomweave_plugin_rust::extract::{Extracted, extract_file_with_edges};
use loomweave_plugin_rust::resolve::Resolver;
use loomweave_plugin_rust::symbol_table::build_symbol_table;

/// Stage a one-crate project (`c_crate`) whose `lib.rs` IS `src`, build the
/// real symbol table over it, and run the edges-aware extraction of `src` as
/// the crate-root module.
fn extract_crate_root(src: &str) -> Extracted {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), src).unwrap();
    let table = build_symbol_table(root);
    let r = Resolver::new(&table);
    extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap()
}

/// The full `derives` edge SET as comparable `(from, to, confidence, start,
/// end)` tuples, sorted — exact-set assertions compare against this.
fn derives_set(extracted: &Extracted) -> Vec<(String, String, String, i64, i64)> {
    let mut out: Vec<_> = extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "derives")
        .map(|e| {
            (
                e["from_id"].as_str().unwrap().to_owned(),
                e["to_id"].as_str().unwrap().to_owned(),
                e["confidence"].as_str().unwrap().to_owned(),
                e["source_byte_start"].as_i64().unwrap(),
                e["source_byte_end"].as_i64().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

/// Byte range of the `nth` (0-based) occurrence of `needle` in `src`.
fn nth_occurrence(src: &str, needle: &str, nth: usize) -> (i64, i64) {
    let start = src
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("{needle:?} occurrence {nth} not found"))
        .0;
    (
        i64::try_from(start).unwrap(),
        i64::try_from(start + needle.len()).unwrap(),
    )
}

#[test]
fn in_project_derive_resolves_with_span_on_the_path_token() {
    let src = "pub trait Pretty {}\n\
               #[derive(Pretty)]\n\
               pub struct Foo;\n";
    let extracted = extract_crate_root(src);

    // The span is the `Pretty` token INSIDE the attribute (the second
    // occurrence — the first is the trait declaration).
    let (start, end) = nth_occurrence(src, "Pretty", 1);
    assert_eq!(
        derives_set(&extracted),
        vec![(
            "rust:struct:c_crate.Foo".to_owned(),
            "rust:trait:c_crate.Pretty".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "exactly one Resolved derives edge Foo→Pretty, anchored on the derive \
         path token",
    );

    // Belt-and-braces: an anchored edge may never be `inferred`.
    assert!(
        !extracted
            .edges
            .iter()
            .any(|e| e["kind"] == "derives" && e["confidence"] == "inferred"),
        "an anchored derives edge must never be `inferred`",
    );
}

#[test]
fn external_derives_are_dropped_entirely() {
    // `Debug`/`Clone` are std derives — out of project, so D1 drops them at
    // emit: no edge, nothing fabricated.
    let src = "#[derive(Debug, Clone)]\npub struct Bar;\n";
    let extracted = extract_crate_root(src);
    assert_eq!(
        derives_set(&extracted),
        Vec::new(),
        "external derives must yield ZERO derives edges",
    );
}

#[test]
fn mixed_list_keeps_only_the_in_project_path_with_its_own_span() {
    let src = "pub trait Pretty {}\n\
               #[derive(Debug, Pretty)]\n\
               pub struct Foo;\n";
    let extracted = extract_crate_root(src);

    // One edge for `Pretty` (the SECOND path in the list), anchored on ITS
    // token — not the list, not `Debug`, not the attribute.
    let (start, end) = nth_occurrence(src, "Pretty", 1);
    assert_eq!(
        derives_set(&extracted),
        vec![(
            "rust:struct:c_crate.Foo".to_owned(),
            "rust:trait:c_crate.Pretty".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "a mixed derive list must mint exactly the in-project edge, span on \
         the `Pretty` token",
    );
}

#[test]
fn enum_target_derives_from_the_enum_entity() {
    let src = "pub trait Pretty {}\n\
               #[derive(Pretty)]\n\
               pub enum E { A }\n";
    let extracted = extract_crate_root(src);

    let (start, end) = nth_occurrence(src, "Pretty", 1);
    assert_eq!(
        derives_set(&extracted),
        vec![(
            "rust:enum:c_crate.E".to_owned(),
            "rust:trait:c_crate.Pretty".to_owned(),
            "resolved".to_owned(),
            start,
            end,
        )],
        "a derive on an enum must originate from the enum entity id",
    );
}

#[test]
fn non_derive_attributes_mint_nothing() {
    // `#[cfg(test)]` and a `#[serde(...)]`-shaped helper attribute are NOT
    // derive lists — they must be skipped without minting any site, even
    // though the in-project `Pretty` trait exists and is named in scope.
    let src = "pub trait Pretty {}\n\
               #[cfg(test)]\n\
               #[serde(rename_all = \"lowercase\")]\n\
               pub struct Baz;\n";
    let extracted = extract_crate_root(src);
    assert_eq!(
        derives_set(&extracted),
        Vec::new(),
        "non-derive attributes must yield ZERO derives edges",
    );
}

#[test]
fn unparseable_derive_list_degrades_silently() {
    // `#[derive(123)]` parses as a FILE (attribute tokens are free-form) but
    // its argument list is not a comma-separated path list — the site parser
    // must yield nothing rather than erroring the extraction.
    let src = "pub trait Pretty {}\n\
               #[derive(123)]\n\
               pub struct Qux;\n";
    let extracted = extract_crate_root(src);
    assert_eq!(
        derives_set(&extracted),
        Vec::new(),
        "an unparseable derive list must yield ZERO derives edges",
    );
    // The struct entity itself is unharmed.
    assert!(
        extracted
            .entities
            .iter()
            .any(|e| e["id"] == "rust:struct:c_crate.Qux"),
        "the deriving struct entity must still be emitted",
    );
}

/// The Ambiguous arm for `derives` is architecturally UNREACHABLE through the
/// real resolver, for the same reason documented on `resolve_call_path`'s
/// tests (resolve.rs): `resolve_trait_path` keeps only `rust:trait:` ids, a
/// trait id IS `rust:trait:<qualname>`, and the symbol table's reverse index
/// holds each id once — so one qualname can never map to TWO trait ids (two
/// same-qualname `trait` declarations collide to one id, recorded in
/// `duplicate_ids`; a multi-KIND collision like `struct Pretty` + `trait
/// Pretty` collapses to ≤1 id under the trait filter → Resolved, never
/// Ambiguous). Two same-NAME traits in different modules have different
/// qualnames and a bare derive path matches neither. So instead of a faked
/// ambiguity, pin the External path that staging produces: the bare
/// `#[derive(Pretty)]` at crate root resolves against `c_crate.Pretty`,
/// misses both module-scoped traits, and lands External → no edge.
#[test]
fn same_name_traits_in_two_modules_stay_external_for_a_bare_derive() {
    let src = "pub mod m1 { pub trait Pretty {} }\n\
               pub mod m2 { pub trait Pretty {} }\n\
               #[derive(Pretty)]\n\
               pub struct Foo;\n";
    let extracted = extract_crate_root(src);
    assert_eq!(
        derives_set(&extracted),
        Vec::new(),
        "a bare derive path matching no crate-root trait must resolve \
         External and mint NO edge (never a guessed Resolved/Ambiguous)",
    );
}
