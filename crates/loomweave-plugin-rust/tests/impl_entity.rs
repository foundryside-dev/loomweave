use loomweave_plugin_rust::extract::extract_file_full;

#[test]
fn impl_entity_emitted_and_methods_reparent_to_it() {
    let x = extract_file_full(
        "k",
        "k.m",
        "/p/src/m.rs",
        "struct Foo;\nimpl Foo { pub fn a(&self){} }\n",
    )
    .unwrap();
    let impl_id = "rust:impl:k.m.Foo.impl#<>";
    let method_id = "rust:function:k.m.Foo.impl#<>.a";
    let ids: Vec<_> = x
        .entities
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&impl_id), "impl entity present: {ids:?}");
    let m = x.entities.iter().find(|e| e["id"] == method_id).unwrap();
    assert_eq!(m["parent_id"], impl_id); // method parents to impl, not module
    let edges: Vec<(&str, &str)> = x
        .edges
        .iter()
        .filter(|e| e["kind"] == "contains")
        .map(|e| (e["from_id"].as_str().unwrap(), e["to_id"].as_str().unwrap()))
        .collect();
    assert!(edges.contains(&("rust:module:k.m", impl_id))); // module -> impl
    assert!(edges.contains(&(impl_id, method_id))); // impl -> method
    assert!(!edges.contains(&("rust:module:k.m", method_id))); // NO leftover module -> method
}

#[test]
fn two_no_cfg_inherent_impls_merge_to_one_entity_and_are_reorder_stable() {
    let a = extract_file_full(
        "k",
        "k.m",
        "/p/m.rs",
        "struct Foo;\nimpl Foo { fn a(&self){} }\nimpl Foo { fn b(&self){} }\n",
    )
    .unwrap();
    let b = extract_file_full(
        "k",
        "k.m",
        "/p/m.rs",
        "struct Foo;\nimpl Foo { fn b(&self){} }\nimpl Foo { fn a(&self){} }\n",
    )
    .unwrap(); // reordered
    let impl_ids = |x: &loomweave_plugin_rust::extract::Extracted| {
        let mut v: Vec<String> = x
            .entities
            .iter()
            .filter(|e| e["kind"] == "impl")
            .map(|e| e["id"].as_str().unwrap().to_owned())
            .collect();
        v.sort();
        v
    };
    assert_eq!(impl_ids(&a), vec!["rust:impl:k.m.Foo.impl#<>".to_owned()]); // ONE entity, no #0/#1
    assert_eq!(
        impl_ids(&a),
        impl_ids(&b),
        "reordering source must not churn the impl id"
    );
    let mids: std::collections::BTreeSet<_> = a
        .entities
        .iter()
        .filter(|e| e["kind"] == "function")
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(mids.contains("rust:function:k.m.Foo.impl#<>.a"));
    assert!(mids.contains("rust:function:k.m.Foo.impl#<>.b"));

    // The merge's SECOND-block path (entity already emitted; only the method +
    // its `impl -> method` edge are appended) must still honour ADR-026
    // dual-encoding: every method parents to the ONE impl entity, with a
    // matching `impl -> method` contains edge and NO leftover `module -> method`.
    let impl_id = "rust:impl:k.m.Foo.impl#<>";
    for mid in [
        "rust:function:k.m.Foo.impl#<>.a",
        "rust:function:k.m.Foo.impl#<>.b",
    ] {
        let m = a.entities.iter().find(|e| e["id"] == mid).unwrap();
        assert_eq!(m["parent_id"], impl_id, "method {mid} must parent to impl");
    }
    let edges: std::collections::BTreeSet<(&str, &str)> = a
        .edges
        .iter()
        .filter(|e| e["kind"] == "contains")
        .map(|e| (e["from_id"].as_str().unwrap(), e["to_id"].as_str().unwrap()))
        .collect();
    assert!(edges.contains(&("rust:module:k.m", impl_id))); // module -> impl (emitted once)
    assert!(edges.contains(&(impl_id, "rust:function:k.m.Foo.impl#<>.a")));
    assert!(edges.contains(&(impl_id, "rust:function:k.m.Foo.impl#<>.b")));
    assert!(!edges.contains(&("rust:module:k.m", "rust:function:k.m.Foo.impl#<>.a")));
    assert!(!edges.contains(&("rust:module:k.m", "rust:function:k.m.Foo.impl#<>.b")));
    // Exactly ONE module -> impl edge despite two source blocks (the merge).
    let module_to_impl = a
        .edges
        .iter()
        .filter(|e| {
            e["kind"] == "contains" && e["from_id"] == "rust:module:k.m" && e["to_id"] == impl_id
        })
        .count();
    assert_eq!(
        module_to_impl, 1,
        "module -> impl edge must be emitted once"
    );
}

/// SILENT-DATA-LOSS trip-wire (ADR-049 §2 self-type-args amendment): two impls
/// on DISTINCT concrete instantiations of one generic self type must produce
/// distinct impl entities AND distinct like-named methods. Before the fix,
/// `impl Foo<i32>` and `impl Foo<u32>` both rendered the bare-`Foo` key
/// (`Foo.impl#<>` / `Foo.impl[Display]`); `seen_impl_ids` merged the second into
/// the first, re-parenting both `get`/`fmt` methods under one impl id, and the
/// second method silently overwrote the first at the writer's
/// `ON CONFLICT(id) DO UPDATE`.
#[test]
fn distinct_concrete_self_type_args_do_not_merge_inherent() {
    let x = extract_file_full(
        "demo",
        "demo.m",
        "/p/src/m.rs",
        "struct Foo<T>(T);\nimpl Foo<i32> { fn get(&self) {} }\nimpl Foo<u32> { fn get(&self) {} }\n",
    )
    .unwrap();
    let impl_ids: std::collections::BTreeSet<&str> = x
        .entities
        .iter()
        .filter(|e| e["kind"] == "impl")
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(impl_ids.contains("rust:impl:demo.m.Foo<i32>.impl#<>"));
    assert!(impl_ids.contains("rust:impl:demo.m.Foo<u32>.impl#<>"));
    assert_eq!(
        impl_ids.len(),
        2,
        "two concrete impls must NOT merge: {impl_ids:?}"
    );

    let method_ids: std::collections::BTreeSet<&str> = x
        .entities
        .iter()
        .filter(|e| e["kind"] == "function")
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    // The two `get` methods get DISTINCT ids — neither is overwritten.
    assert!(method_ids.contains("rust:function:demo.m.Foo<i32>.impl#<>.get"));
    assert!(method_ids.contains("rust:function:demo.m.Foo<u32>.impl#<>.get"));
}

/// Trait-arm twin: `impl Display for Foo<i32>` and `impl Display for Foo<u32>`
/// share the `impl[Display]` fragment; only the concrete self-type arg in the
/// prefix keeps them (and their `fmt` methods) distinct.
#[test]
fn distinct_concrete_self_type_args_do_not_merge_trait() {
    let x = extract_file_full(
        "demo",
        "demo.m",
        "/p/src/m.rs",
        "struct Foo<T>(T);\n\
         impl std::fmt::Display for Foo<i32> { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n\
         impl std::fmt::Display for Foo<u32> { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n",
    )
    .unwrap();
    let method_ids: std::collections::BTreeSet<&str> = x
        .entities
        .iter()
        .filter(|e| e["kind"] == "function")
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(method_ids.contains("rust:function:demo.m.Foo<i32>.impl[Display].fmt"));
    assert!(method_ids.contains("rust:function:demo.m.Foo<u32>.impl[Display].fmt"));
    assert_eq!(
        x.entities.iter().filter(|e| e["kind"] == "impl").count(),
        2,
        "two concrete trait impls must NOT merge",
    );
}

/// PRESERVES THE LEGITIMATE MERGE: two inherent blocks on the SAME concrete
/// instantiation (`Foo<i32>`) that split a type's methods across blocks must
/// STILL merge into ONE impl entity carrying both methods (same self-type-args +
/// generic-sig + cfg). Only DIFFERENT concrete args stop merging.
#[test]
fn same_concrete_self_type_args_still_merge_across_blocks() {
    let x = extract_file_full(
        "demo",
        "demo.m",
        "/p/src/m.rs",
        "struct Foo<T>(T);\nimpl Foo<i32> { fn a(&self) {} }\nimpl Foo<i32> { fn b(&self) {} }\n",
    )
    .unwrap();
    let impl_ids: Vec<&str> = x
        .entities
        .iter()
        .filter(|e| e["kind"] == "impl")
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        impl_ids,
        vec!["rust:impl:demo.m.Foo<i32>.impl#<>"],
        "two same-instantiation blocks must merge to ONE impl entity",
    );
    let impl_id = "rust:impl:demo.m.Foo<i32>.impl#<>";
    let method_ids: std::collections::BTreeSet<&str> = x
        .entities
        .iter()
        .filter(|e| e["kind"] == "function")
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(method_ids.contains("rust:function:demo.m.Foo<i32>.impl#<>.a"));
    assert!(method_ids.contains("rust:function:demo.m.Foo<i32>.impl#<>.b"));
    // Both methods parent to the one merged impl entity (ADR-026 dual-encoding).
    for mid in [
        "rust:function:demo.m.Foo<i32>.impl#<>.a",
        "rust:function:demo.m.Foo<i32>.impl#<>.b",
    ] {
        let m = x.entities.iter().find(|e| e["id"] == mid).unwrap();
        assert_eq!(m["parent_id"], impl_id);
    }
    let edges: std::collections::BTreeSet<(&str, &str)> = x
        .edges
        .iter()
        .filter(|e| e["kind"] == "contains")
        .map(|e| (e["from_id"].as_str().unwrap(), e["to_id"].as_str().unwrap()))
        .collect();
    assert!(edges.contains(&(impl_id, "rust:function:demo.m.Foo<i32>.impl#<>.a")));
    assert!(edges.contains(&(impl_id, "rust:function:demo.m.Foo<i32>.impl#<>.b")));
}
