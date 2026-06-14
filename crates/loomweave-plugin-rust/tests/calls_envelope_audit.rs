//! Phase 2 — the `calls` resolution ENVELOPE, pinned boundary by boundary
//! (plan D7, ticket clarion-bfb3e2be49).
//!
//! `tests/calls_edges.rs` covers the happy paths (bare crate-root call
//! resolves, method call → site, assoc/external → sites, per-caller ordinal
//! reset, no-resolver parity). This file pins every EDGE of the envelope —
//! the behaviors that were previously documented only in `src/calls.rs`
//! comments — so a regression in any boundary fails a named test:
//!
//! - `envelope_multi_segment_*` — which path shapes resolve vs under-resolve,
//! - `envelope_ambiguous_*` — why the Ambiguous arm is unreachable for calls,
//! - `envelope_external_*` / `envelope_method_*` / `envelope_assoc_*` /
//!   `envelope_ufcs_*` / `envelope_non_path_*` — the unresolved-site family,
//! - `envelope_closure_*` / `envelope_nested_fn_*` / `envelope_trait_*` —
//!   body-walk attribution,
//! - `envelope_ordinals_*` / `envelope_accounting_*` — the counting contract.

use loomweave_plugin_rust::extract::{Extracted, extract_file_with_edges};
use loomweave_plugin_rust::resolve::Resolver;
use loomweave_plugin_rust::symbol_table::build_symbol_table;
use serde_json::Value;

/// A one-crate project rooted at `c_crate` whose lib.rs is the file under
/// analysis (the `calls_edges.rs` harness). Returns the extraction result.
fn extract(lib_src: &str) -> Extracted {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), lib_src).unwrap();
    let table = build_symbol_table(root);
    let resolver = Resolver::new(&table);
    extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", lib_src, &resolver).unwrap()
}

fn calls_edges(extracted: &Extracted) -> Vec<&Value> {
    extracted
        .edges
        .iter()
        .filter(|e| e["kind"] == "calls")
        .collect()
}

/// `(from_id, to_id, confidence)` triples of every calls edge — SET-exact
/// assertions, never just presence.
fn calls_set(extracted: &Extracted) -> Vec<(String, String, String)> {
    let mut v: Vec<(String, String, String)> = calls_edges(extracted)
        .iter()
        .map(|e| {
            (
                e["from_id"].as_str().unwrap().to_owned(),
                e["to_id"].as_str().unwrap().to_owned(),
                e["confidence"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    v.sort();
    v
}

/// `(ordinal, callee_expr)` pairs in ordinal order for one caller.
fn sites_of<'a>(
    extracted: &'a Extracted,
    caller: &str,
) -> Vec<&'a loomweave_core::plugin::UnresolvedCallSite> {
    let mut v: Vec<_> = extracted
        .unresolved_call_sites
        .iter()
        .filter(|s| s.caller_entity_id == caller)
        .collect();
    v.sort_by_key(|s| s.site_ordinal);
    v
}

// ---------------------------------------------------------------------------
// Boundary 1 — path-call resolution shapes. The bare crate-root call
// (`b()` next to `fn b`) is already pinned by
// `calls_edges::resolved_free_fn_call_emits_calls_edge`; here we pin the
// multi-segment shapes it does NOT cover.
// ---------------------------------------------------------------------------

/// A `crate::`-qualified multi-segment path call resolves: `normalize_path`
/// maps the leading `crate` to the origin crate, so `crate::sub::helper()`
/// looks up `c_crate.sub.helper` and lands a `resolved` edge.
#[test]
fn envelope_multi_segment_crate_qualified_call_resolves() {
    let src = "pub mod sub { pub fn helper() {} }\n\
               pub fn a() { crate::sub::helper(); }\n";
    let extracted = extract(src);
    assert_eq!(
        calls_set(&extracted),
        vec![(
            "rust:function:c_crate.a".to_owned(),
            "rust:function:c_crate.sub.helper".to_owned(),
            "resolved".to_owned(),
        )],
    );
    assert!(extracted.unresolved_call_sites.is_empty());
}

/// A BARE multi-segment relative path (`sub::helper()`, no `crate::` prefix)
/// deliberately UNDER-resolves to an unresolved site: the resolver's
/// crate-root fallback is gated to single-segment paths (the H5 guard in
/// `resolve.rs::resolve_non_glob` — re-prefixing a multi-segment miss would
/// wrong-resolve external-crate paths shadowed by in-project modules).
/// Intentional, documented under-resolution — NOT a defect.
#[test]
fn envelope_bare_multi_segment_relative_call_underresolves_to_site() {
    let src = "pub mod sub { pub fn helper() {} }\n\
               pub fn a() { sub::helper(); }\n";
    let extracted = extract(src);
    assert!(
        calls_edges(&extracted).is_empty(),
        "bare multi-segment relative paths stay External (H5 guard): {:#?}",
        calls_edges(&extracted),
    );
    let sites = sites_of(&extracted, "rust:function:c_crate.a");
    assert_eq!(sites.len(), 1, "got {sites:#?}");
    assert_eq!(sites[0].callee_expr, "sub::helper");
}

/// The bare-name fallback is CRATE-ROOT-relative, not module-relative: a bare
/// `sibling()` call inside `mod sub` looks up `c_crate.sibling`, NOT
/// `c_crate.sub.sibling`, so a module-local sibling call under-resolves to a
/// site. This is the documented 1b approximation ("a bare name is assumed
/// crate-root-or-unique-in-project", `resolve.rs` — module context is not
/// threaded into the call walk; deepening it is the resolution-depth
/// follow-up). Intentional under-resolution, pinned as-is. The dual hazard
/// (a crate-root fn of the same name shadowing the module-local one would
/// wrong-resolve) is the same documented approximation.
#[test]
fn envelope_bare_name_fallback_is_crate_root_relative_not_module_relative() {
    let src = "pub mod sub {\n\
                 pub fn sibling() {}\n\
                 pub fn a() { sibling(); }\n\
               }\n";
    let extracted = extract(src);
    assert!(
        calls_edges(&extracted).is_empty(),
        "module-local bare call must not resolve without module context: {:#?}",
        calls_edges(&extracted),
    );
    let sites = sites_of(&extracted, "rust:function:c_crate.sub.a");
    assert_eq!(sites.len(), 1, "got {sites:#?}");
    assert_eq!(sites[0].callee_expr, "sibling");
}

// ---------------------------------------------------------------------------
// Boundary 2 — Ambiguous. For `calls` the Ambiguous arm is ARCHITECTURALLY
// UNREACHABLE through a real symbol table: a function id IS its qualname
// (`rust:function:<qualname>`), so two function ids can never share a
// qualname (same-name collisions collapse to one id + a `duplicate_ids`
// record, cfg twins get distinct `@cfg(...)` qualnames), and a multi-KIND
// collision is collapsed by the `rust:function:` filter to exactly one id →
// Resolved. The arm itself is pinned at unit level by
// `resolve.rs::tests::resolve_ids_collapses_multiple_function_ids_to_ambiguous_first_sorted`.
// Here we pin the closest stageable surfaces.
// ---------------------------------------------------------------------------

/// A multi-KIND qualname collision (`struct S` + `fn S`) does NOT land
/// Ambiguous for a call: the `rust:function:` kind filter keeps exactly one
/// candidate → a `resolved` edge to the function id.
#[test]
fn envelope_ambiguous_multi_kind_collision_collapses_to_resolved() {
    // Legal to parse (syn never name-checks); the value-namespace clash is
    // irrelevant to extraction.
    let src = "pub struct S;\npub fn S() {}\npub fn a() { S(); }\n";
    let extracted = extract(src);
    assert_eq!(
        calls_set(&extracted),
        vec![(
            "rust:function:c_crate.a".to_owned(),
            "rust:function:c_crate.S".to_owned(),
            "resolved".to_owned(),
        )],
        "the kind filter must collapse the struct+fn collision to ONE \
         function candidate — Resolved, never Ambiguous",
    );
    assert!(extracted.unresolved_call_sites.is_empty());
}

/// The other side of the kind filter: a path that names ONLY a non-function
/// entity (`struct Only`) survives the filter with ZERO candidates → External
/// → an unresolved site, never a fabricated edge to the struct.
#[test]
fn envelope_call_to_non_function_entity_is_external_site() {
    let src = "pub struct Only;\npub fn a() { Only(); }\n";
    let extracted = extract(src);
    assert!(
        calls_edges(&extracted).is_empty(),
        "a calls edge may only target rust:function: ids: {:#?}",
        calls_edges(&extracted),
    );
    let sites = sites_of(&extracted, "rust:function:c_crate.a");
    assert_eq!(sites.len(), 1, "got {sites:#?}");
    assert_eq!(sites[0].callee_expr, "Only");
}

// ---------------------------------------------------------------------------
// Boundaries 3/4/5 — the unresolved-site family.
// ---------------------------------------------------------------------------

/// External path calls (std or third-party) → one site each, `callee_expr` =
/// the `::`-joined path as written, NO edge.
#[test]
fn envelope_external_path_call_is_site_with_path_callee_expr() {
    let src = "pub fn a(x: i32) { std::mem::drop(x); external_crate::f(); }\n";
    let extracted = extract(src);
    assert!(calls_edges(&extracted).is_empty());
    let sites = sites_of(&extracted, "rust:function:c_crate.a");
    let exprs: Vec<&str> = sites.iter().map(|s| s.callee_expr.as_str()).collect();
    assert_eq!(exprs, vec!["std::mem::drop", "external_crate::f"]);
}

/// A method call `x.foo()` → one site with `callee_expr` EXACTLY `.foo` (the
/// dotted form marks it as a method, distinct from a bare path `foo`), NO
/// edge. `calls_edges.rs` pins the no-edge half loosely (`contains("foo")`);
/// this pins the exact rendering.
#[test]
fn envelope_method_call_site_callee_expr_is_dotted_name() {
    let src = "pub struct X;\npub fn a(x: X) { x.foo(); }\n";
    let extracted = extract(src);
    assert!(calls_edges(&extracted).is_empty());
    let sites = sites_of(&extracted, "rust:function:c_crate.a");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].callee_expr, ".foo");
}

/// An associated-fn call `Foo::new()` whose target IS in-project lands an
/// unresolved site TODAY: the method's real qualname carries the `.impl`
/// discriminator (`c_crate.Foo.impl….new`) that the call syntax lacks, so the
/// exact-qualname lookup misses → External. Deliberate MVP behavior
/// (`resolve.rs::resolve_call_path` docs name assoc resolution a
/// fast-follow); the important half is that NO edge is fabricated.
#[test]
fn envelope_assoc_fn_call_is_unresolved_site_today() {
    let src = "pub struct Foo;\nimpl Foo { pub fn new() -> Foo { Foo } }\n\
               pub fn a() { Foo::new(); }\n";
    let extracted = extract(src);
    // The impl method entity exists (the .impl discriminator is what the
    // call path cannot spell) …
    assert!(
        extracted.entities.iter().any(|e| e["id"]
            .as_str()
            .is_some_and(|id| id.contains("Foo.impl") && id.rsplit('.').next() == Some("new"))),
        "expected the impl-method entity",
    );
    // … but the call lands a site, not an edge.
    assert!(calls_edges(&extracted).is_empty());
    let sites = sites_of(&extracted, "rust:function:c_crate.a");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].callee_expr, "Foo::new");
}

// ---------------------------------------------------------------------------
// Boundary 6 — UFCS / qself. A qself path names an ASSOCIATED item by
// definition, never a free function, so it must never resolve through the
// free-fn table.
// ---------------------------------------------------------------------------

/// `<Foo as Trait>::f(x)` → unresolved site, no edge. The recorded
/// `callee_expr` is the path segments as written after the qself
/// (`Pretty::pretty` — the `<Foo as …>` qualifier is dropped from the
/// rendering; the path alone could never have resolved anyway because trait
/// items are not entities).
#[test]
fn envelope_ufcs_qself_trait_call_is_site_not_edge() {
    let src = "pub trait Pretty { fn pretty(&self); }\npub struct Foo;\n\
               pub fn a(f: Foo) { <Foo as Pretty>::pretty(&f); }\n";
    let extracted = extract(src);
    assert!(
        calls_edges(&extracted).is_empty(),
        "UFCS calls never resolve in the MVP: {:#?}",
        calls_edges(&extracted),
    );
    let sites = sites_of(&extracted, "rust:function:c_crate.a");
    assert_eq!(sites.len(), 1, "got {sites:#?}");
    assert_eq!(sites[0].callee_expr, "Pretty::pretty");
}

/// The short qself form `<Foo>::create()` carries the BARE path segment
/// `create`, which (before the qself guard) hit the crate-root bare-name
/// fallback and FABRICATED a resolved edge to an unrelated free
/// `fn create` — an H5 violation (fixed in this audit: `calls.rs` now treats
/// every qself path as unresolvable). Pinned: site, never an edge, even with
/// a same-named free fn at crate root.
#[test]
fn envelope_ufcs_qself_short_form_never_fabricates_an_edge() {
    let src = "pub fn create() {}\npub struct Foo;\n\
               impl Foo { pub fn create() -> Foo { Foo } }\n\
               pub fn a() { <Foo>::create(); }\n";
    let extracted = extract(src);
    assert!(
        calls_edges(&extracted).is_empty(),
        "<Foo>::create() must NOT resolve to the free fn `create` (H5): {:#?}",
        calls_edges(&extracted),
    );
    let sites = sites_of(&extracted, "rust:function:c_crate.a");
    assert_eq!(sites.len(), 1, "got {sites:#?}");
    assert_eq!(sites[0].callee_expr, "create");
}

// ---------------------------------------------------------------------------
// Boundary 7 — non-path callee.
// ---------------------------------------------------------------------------

/// A call whose func is not a plain path (`(get_fn())(x)`) records the
/// placeholder `<expr>()` site; the descent still walks the inner call
/// (`get_fn()` → its own site, here external) and the args.
#[test]
fn envelope_non_path_callee_records_expr_placeholder_site() {
    let src = "pub fn a(x: i32) { (external_crate::get_fn())(x); }\n";
    let extracted = extract(src);
    assert!(calls_edges(&extracted).is_empty());
    let sites = sites_of(&extracted, "rust:function:c_crate.a");
    let exprs: Vec<&str> = sites.iter().map(|s| s.callee_expr.as_str()).collect();
    // Pre-order: the outer non-path call is recorded BEFORE the descent
    // reaches the inner `get_fn()` call.
    assert_eq!(exprs, vec!["<expr>()", "external_crate::get_fn"]);
}

// ---------------------------------------------------------------------------
// Boundaries 8/9/10 — body-walk attribution.
// ---------------------------------------------------------------------------

/// A call inside a closure body attributes to the enclosing NAMED function:
/// the visitor does not treat `|| …` as a new caller.
#[test]
fn envelope_closure_body_call_attributes_to_enclosing_named_fn() {
    let src = "pub fn helper() {}\npub fn outer() { let _f = || helper(); }\n";
    let extracted = extract(src);
    assert_eq!(
        calls_set(&extracted),
        vec![(
            "rust:function:c_crate.outer".to_owned(),
            "rust:function:c_crate.helper".to_owned(),
            "resolved".to_owned(),
        )],
    );
    assert!(extracted.unresolved_call_sites.is_empty());
}

/// A nested `fn inner` inside a fn body is NOT an entity (the item walk only
/// covers top-level + inline-`mod` items, so body-local items are invisible
/// to the entity surface), and a call inside its body attributes to the
/// enclosing named fn `outer` — "nearest enclosing NAMED function" in the
/// module docs means nearest EXTRACTED function. A call TO the nested fn
/// (`inner()`) under-resolves to a site (its qualname is never in the table).
#[test]
fn envelope_nested_fn_is_not_an_entity_and_attributes_to_outer() {
    let src = "pub fn helper() {}\n\
               pub fn outer() { fn inner() { helper(); } inner(); }\n";
    let extracted = extract(src);
    // inner is not an entity …
    assert!(
        !extracted.entities.iter().any(|e| e["qualified_name"]
            .as_str()
            .is_some_and(|q| q.contains("inner"))),
        "a body-local fn must not be extracted as an entity",
    );
    // … its body's call attributes to outer …
    assert_eq!(
        calls_set(&extracted),
        vec![(
            "rust:function:c_crate.outer".to_owned(),
            "rust:function:c_crate.helper".to_owned(),
            "resolved".to_owned(),
        )],
    );
    // … and the call TO it is an unresolved site on outer.
    let sites = sites_of(&extracted, "rust:function:c_crate.outer");
    assert_eq!(sites.len(), 1, "got {sites:#?}");
    assert_eq!(sites[0].callee_expr, "inner");
}

/// A trait DEFAULT method body is not walked at all (the `Item::Trait` arm
/// emits only the trait entity — intentional, matching Phase 1a: trait items
/// are not entities, so there is no caller id to attribute to): no edge, no
/// site, even for a resolvable call inside it.
#[test]
fn envelope_trait_default_method_body_is_not_walked() {
    let src = "pub fn helper() {}\n\
               pub trait T { fn d(&self) { helper(); } }\n";
    let extracted = extract(src);
    assert!(
        calls_edges(&extracted).is_empty(),
        "trait default bodies must mint no calls edges: {:#?}",
        calls_edges(&extracted),
    );
    assert!(
        extracted.unresolved_call_sites.is_empty(),
        "…and no unresolved sites: {:#?}",
        extracted.unresolved_call_sites,
    );
    // Non-vacuous: the trait entity itself IS extracted, its method is not.
    assert!(
        extracted
            .entities
            .iter()
            .any(|e| e["id"] == "rust:trait:c_crate.T"),
    );
    assert!(
        !extracted.entities.iter().any(|e| e["qualified_name"]
            .as_str()
            .is_some_and(|q| q.rsplit('.').next() == Some("d"))),
        "trait items are not entities",
    );
}

// ---------------------------------------------------------------------------
// Boundaries 11/12 — ordinals + accounting.
// ---------------------------------------------------------------------------

/// One per-caller ordinal counter advances at EVERY call site in source
/// order — edge-minting calls consume ordinals too (an unresolved site's
/// ordinal is its position among ALL sites, so query-time inference can
/// interleave them with the edges). Resolved at 0 and 3, sites at 1, 2, 4.
#[test]
fn envelope_ordinals_count_both_edges_and_sites_in_source_order() {
    let src = "pub fn helper() {}\npub fn helper2() {}\npub struct X;\n\
               pub fn caller(x: X) {\n\
                 helper();\n\
                 x.m();\n\
                 std::mem::drop(0);\n\
                 helper2();\n\
                 x.n();\n\
               }\n";
    let extracted = extract(src);
    assert_eq!(calls_edges(&extracted).len(), 2, "two resolved edges");
    let sites = sites_of(&extracted, "rust:function:c_crate.caller");
    let got: Vec<(i64, &str)> = sites
        .iter()
        .map(|s| (s.site_ordinal, s.callee_expr.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![(1, ".m"), (2, "std::mem::drop"), (4, ".n")],
        "ordinals 0 and 3 are consumed by the two edge-minting calls",
    );
    // Strictly increasing in source order — ordinal order IS byte order.
    for w in sites.windows(2) {
        assert!(
            w[0].site_ordinal < w[1].site_ordinal
                && w[0].source_byte_start < w[1].source_byte_start,
            "ordinals/spans must both increase in source order: {sites:#?}",
        );
    }
}

/// Accounting at the extraction surface: the unresolved-site VEC is the
/// single source of truth for a mixed file (`unresolved_call_sites_total` is
/// derived from it in exactly one place — `serve.rs` sets
/// `unresolved_call_sites_total: unresolved_call_sites.len()`; the serve-loop
/// equality is asserted end-to-end through the real subprocess in
/// `tests/host_integration.rs`). Here: a mixed file's site set is exact, with
/// no double-count from the resolved calls.
#[test]
fn envelope_accounting_site_vec_is_exact_for_mixed_file() {
    let src = "pub fn helper() {}\npub struct X;\n\
               pub fn a(x: X) { helper(); x.m(); std::mem::drop(0); }\n\
               pub fn b(x: X) { x.n(); helper(); }\n";
    let extracted = extract(src);
    assert_eq!(calls_edges(&extracted).len(), 2);
    let mut got: Vec<(String, String)> = extracted
        .unresolved_call_sites
        .iter()
        .map(|s| (s.caller_entity_id.clone(), s.callee_expr.clone()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("rust:function:c_crate.a".to_owned(), ".m".to_owned()),
            (
                "rust:function:c_crate.a".to_owned(),
                "std::mem::drop".to_owned()
            ),
            ("rust:function:c_crate.b".to_owned(), ".n".to_owned()),
        ],
        "the site vec must hold exactly the unresolved sites, nothing else",
    );
}
