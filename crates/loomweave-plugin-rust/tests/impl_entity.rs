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
        .map(|e| {
            (
                e["from_id"].as_str().unwrap(),
                e["to_id"].as_str().unwrap(),
            )
        })
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
        .map(|e| {
            (
                e["from_id"].as_str().unwrap(),
                e["to_id"].as_str().unwrap(),
            )
        })
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
        .filter(|e| e["kind"] == "contains" && e["from_id"] == "rust:module:k.m" && e["to_id"] == impl_id)
        .count();
    assert_eq!(module_to_impl, 1, "module -> impl edge must be emitted once");
}
