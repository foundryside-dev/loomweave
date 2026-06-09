//! Phase 2 — `calls` edges resolved through the symbol table.
//!
//! Walks function bodies (free `fn` and impl methods) for call expressions and
//! classifies each:
//! - `ExprCall` with a `Path` func resolving to a unique in-project `function`
//!   id → an anchored **Resolved** `calls` edge,
//! - `ExprMethodCall` (`x.foo()`) → NO edge, one `UnresolvedCallSite`,
//! - an `ExprCall` to an external / assoc path (`Foo::new()`) → NO edge, one
//!   `UnresolvedCallSite` (no fabrication).
//!
//! `calls` is anchored (ADR-026 decision 3): it carries the call expression's
//! source byte span and so may be `resolved` or `ambiguous` but NEVER
//! `inferred`. Unresolved sites carry a per-caller sequential `site_ordinal`.

use loomweave_plugin_rust::extract::extract_file_full;
use loomweave_plugin_rust::extract::extract_file_with_edges;
use loomweave_plugin_rust::resolve::Resolver;
use loomweave_plugin_rust::symbol_table::build_symbol_table;
use serde_json::Value;

/// A one-crate project rooted at `c_crate` whose lib.rs is the file under
/// analysis. Returns `(tempdir, project_root)`; callers write `c/src/lib.rs`.
fn project_with_lib(lib_src: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("c/src")).unwrap();
    std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
    std::fs::write(root.join("c/src/lib.rs"), lib_src).unwrap();
    tmp
}

fn calls_edges(edges: &[Value]) -> Vec<&Value> {
    edges.iter().filter(|e| e["kind"] == "calls").collect()
}

#[test]
fn resolved_free_fn_call_emits_calls_edge() {
    // Both fns live at the crate root, so a bare `b()` resolves via the
    // crate-root-relative bare-name fallback to `c_crate.b`.
    let src = "pub fn a() { b(); }\npub fn b() {}\n";
    let tmp = project_with_lib(src);
    let table = build_symbol_table(tmp.path());
    let r = Resolver::new(&table);

    let extracted =
        extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap();
    let calls = calls_edges(&extracted.edges);
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one calls edge, got {calls:#?}"
    );
    let e = calls[0];
    assert_eq!(e["from_id"], "rust:function:c_crate.a");
    assert_eq!(e["to_id"], "rust:function:c_crate.b");
    assert_eq!(e["confidence"], "resolved");
    assert!(
        e["source_byte_start"].as_i64().is_some(),
        "calls edge must carry a non-null source_byte_start: {e:#?}",
    );
    assert!(
        e["source_byte_end"].as_i64().is_some_and(|b| b > 0),
        "calls edge must carry a non-null source_byte_end: {e:#?}",
    );
    assert_ne!(
        e["confidence"], "inferred",
        "an anchored calls edge is never inferred"
    );
    assert!(
        extracted.unresolved_call_sites.is_empty(),
        "no unresolved sites expected"
    );
}

#[test]
fn impl_method_caller_emits_calls_edge_from_method_id() {
    // An impl method body calling a free fn: the caller is the METHOD's id.
    let src = "pub fn b() {}\npub struct Foo;\nimpl Foo { pub fn run(&self) { b(); } }\n";
    let tmp = project_with_lib(src);
    let table = build_symbol_table(tmp.path());
    let r = Resolver::new(&table);

    let extracted =
        extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap();
    let calls = calls_edges(&extracted.edges);
    assert_eq!(
        calls.len(),
        1,
        "expected one calls edge from the method, got {calls:#?}"
    );
    let e = calls[0];
    assert_eq!(e["to_id"], "rust:function:c_crate.b");
    assert_eq!(e["confidence"], "resolved");
    // Caller is the impl method, not the free fn or the struct.
    let from = e["from_id"].as_str().unwrap();
    assert!(
        from.contains("Foo.impl") && from.split('.').next_back() == Some("run"),
        "caller must be the impl method id, got {from}",
    );
}

#[test]
fn unresolved_method_call_records_a_site_not_an_edge() {
    // `x.foo()` has no receiver type without inference → never an edge.
    let src = "pub struct X;\npub fn a(x: X) { x.foo(); }\n";
    let tmp = project_with_lib(src);
    let table = build_symbol_table(tmp.path());
    let r = Resolver::new(&table);

    let extracted =
        extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap();
    assert!(
        calls_edges(&extracted.edges).is_empty(),
        "method call is not an edge"
    );
    let sites = &extracted.unresolved_call_sites;
    assert_eq!(
        sites.len(),
        1,
        "expected one unresolved method-call site, got {sites:#?}"
    );
    assert_eq!(sites[0].caller_entity_id, "rust:function:c_crate.a");
    assert!(
        sites[0].callee_expr.contains("foo"),
        "callee_expr should mention the method name, got {}",
        sites[0].callee_expr,
    );
    assert!(sites[0].source_byte_end > sites[0].source_byte_start);
}

#[test]
fn unresolved_assoc_and_external_paths_are_sites_not_edges() {
    // `Foo::new()` is an assoc call whose real qualname carries an `.impl#<>`
    // segment the call syntax lacks (deliberate fast-follow, not this MVP);
    // an out-of-project path resolves External. Both are sites, NOT edges —
    // proving no fabrication.
    let src = "pub struct Foo;\nimpl Foo { pub fn new() -> Foo { Foo } }\n\
               pub fn a() { Foo::new(); serde::de(); }\n";
    let tmp = project_with_lib(src);
    let table = build_symbol_table(tmp.path());
    let r = Resolver::new(&table);

    let extracted =
        extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap();
    assert!(
        calls_edges(&extracted.edges).is_empty(),
        "neither Foo::new() nor serde::de() may fabricate a calls edge: {:#?}",
        calls_edges(&extracted.edges),
    );
    let a_sites: Vec<_> = extracted
        .unresolved_call_sites
        .iter()
        .filter(|s| s.caller_entity_id == "rust:function:c_crate.a")
        .collect();
    assert_eq!(
        a_sites.len(),
        2,
        "Foo::new() and serde::de() are both sites, got {a_sites:#?}"
    );
}

#[test]
fn no_resolver_path_emits_no_calls_edges_and_no_sites() {
    // `extract_file_full` (no resolver) must behave exactly like imports: it
    // emits entities + contains only — zero calls edges, zero unresolved sites.
    let src = "pub fn a() { b(); }\npub fn b() {}\n";
    let extracted = extract_file_full("c_crate", "c_crate", "/p/c/src/lib.rs", src).unwrap();
    assert!(
        calls_edges(&extracted.edges).is_empty(),
        "no-resolver path must emit zero calls edges",
    );
    assert!(
        extracted.unresolved_call_sites.is_empty(),
        "no-resolver path must emit zero unresolved call sites",
    );
    // Non-vacuous: entities + contains are still present.
    assert!(extracted.entities.iter().any(|e| e["kind"] == "function"));
    assert!(extracted.edges.iter().any(|e| e["kind"] == "contains"));
}

#[test]
fn site_ordinal_is_per_caller_sequential_and_deterministic() {
    // Two callers, each with two unresolved method calls. Each caller's ordinals
    // start at 0 and increase in source order; ordinals do NOT leak across
    // callers.
    let src = "pub struct X;\n\
               pub fn a(x: X) { x.one(); x.two(); }\n\
               pub fn b(x: X) { x.three(); x.four(); }\n";
    let tmp = project_with_lib(src);
    let table = build_symbol_table(tmp.path());
    let r = Resolver::new(&table);

    let extracted =
        extract_file_with_edges("c_crate", "c_crate", "/p/c/src/lib.rs", src, &r).unwrap();
    let sites = &extracted.unresolved_call_sites;
    assert_eq!(
        sites.len(),
        4,
        "four method-call sites expected, got {sites:#?}"
    );

    let for_caller = |id: &str| -> Vec<(i64, String)> {
        let mut v: Vec<(i64, String)> = sites
            .iter()
            .filter(|s| s.caller_entity_id == id)
            .map(|s| (s.site_ordinal, s.callee_expr.clone()))
            .collect();
        v.sort_by_key(|(o, _)| *o);
        v
    };

    let a = for_caller("rust:function:c_crate.a");
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].0, 0, "first site of caller a is ordinal 0");
    assert_eq!(a[1].0, 1, "second site of caller a is ordinal 1");
    assert!(
        a[0].1.contains("one") && a[1].1.contains("two"),
        "source order: {a:?}"
    );

    let b = for_caller("rust:function:c_crate.b");
    assert_eq!(b.len(), 2);
    assert_eq!(b[0].0, 0, "caller b's ordinal resets to 0");
    assert_eq!(b[1].0, 1);
    assert!(
        b[0].1.contains("three") && b[1].1.contains("four"),
        "source order: {b:?}"
    );
}
