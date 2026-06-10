use loomweave_plugin_rust::extract::extract_file;
use std::collections::BTreeSet;

fn id_set(src: &str) -> BTreeSet<String> {
    extract_file("k", "k.m", "/p/src/m.rs", src)
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn reordering_impl_blocks_does_not_change_method_ids() {
    let a = "struct Foo;\nimpl A for Foo { fn x(&self){} }\nimpl B for Foo { fn y(&self){} }\ntrait A { fn x(&self); }\ntrait B { fn y(&self); }\n";
    let b = "struct Foo;\nimpl B for Foo { fn y(&self){} }\nimpl A for Foo { fn x(&self){} }\ntrait A { fn x(&self); }\ntrait B { fn y(&self); }\n";
    assert_eq!(id_set(a), id_set(b));
}

#[test]
fn mutating_one_impls_method_set_does_not_churn_other_ids() {
    // ADR-049 §4.3 benign-edit stability: adding/removing a method in one impl
    // must not perturb any *other* entity's locator. `b` adds `fn n` alongside
    // `m` in the same inherent block; every id from `a` must survive verbatim,
    // and exactly one new id (the added method) appears.
    let a = "struct Foo;\nimpl Foo { fn m(&self){} }\n";
    let b = "struct Foo;\nimpl Foo { fn m(&self){} fn n(&self){} }\n";
    let before = id_set(a);
    let after = id_set(b);
    assert!(
        before.is_subset(&after),
        "adding `fn n` churned a pre-existing id: {:?}",
        &before - &after
    );
    assert_eq!(
        (&after - &before).len(),
        1,
        "expected exactly the new method's id to be added, got {:?}",
        &after - &before
    );
}

#[test]
fn renaming_a_generic_param_is_a_noop_for_inherent_impl_ids() {
    let t = "struct Foo<X>(X);\nimpl<T> Foo<T> { fn m(&self){} }\n";
    let u = "struct Foo<X>(X);\nimpl<U> Foo<U> { fn m(&self){} }\n";
    // the method id (which carries the inherent-impl positional signature) is unchanged
    let mt: BTreeSet<_> = id_set(t).into_iter().filter(|i| i.contains(".m")).collect();
    let mu: BTreeSet<_> = id_set(u).into_iter().filter(|i| i.contains(".m")).collect();
    assert_eq!(mt, mu);
}

// --- ADR-049 Amendments 6/7 negative controls (byte-pins). The residual-
// collision ladder (@cfg on bare keys → S on post-cfg groups → T on post-S
// groups → method-@cfg on final keys) qualifies a written path ONLY inside a
// fired group; everything below pins the OFF states byte-for-byte.

#[test]
fn lone_path_qualified_self_type_keeps_last_segment_base() {
    // Stage-S gate off: a LONE `impl T for a::X` (no same-key twin) keeps the
    // bare last-segment self-type base — Amendment 6 must not churn the
    // ubiquitous lone path-qualified impl (e.g. loomweave-cli guidance.rs
    // `impl StorageResultExt for loomweave_storage::Result`).
    let ids = id_set("pub trait T {}\nmod a { pub struct X; }\nimpl T for a::X {}\n");
    assert!(
        ids.contains("rust:impl:k.m.X.impl[T]"),
        "lone path-qualified self-type impl id moved: {ids:?}"
    );
}

#[test]
fn lone_multi_segment_trait_path_keeps_last_segment_fragment() {
    // Stage-T gate off: a LONE `impl std::fmt::Display for Foo` keeps the
    // bare `impl[Display]` fragment (reinforces the corpus `trait_method`
    // row) — Amendment 7 must not churn the ubiquitous std-trait impl.
    let ids = id_set(
        "struct Foo;\nimpl std::fmt::Display for Foo { fn fmt(&self, _f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n",
    );
    assert!(
        ids.contains("rust:impl:k.m.Foo.impl[Display]"),
        "lone multi-segment trait-path impl id moved: {ids:?}"
    );
    assert!(ids.contains("rust:function:k.m.Foo.impl[Display].fmt"));
}

#[test]
fn same_path_cfg_twins_keep_cfg_only_ids() {
    // Stage 1 (@cfg) splits same-path cfg twins exactly as before Amendments
    // 6/7; S sees two singleton post-cfg groups with one witness each and
    // stays cold — the ids carry @cfg only, no path qualification.
    let ids = id_set(
        "pub trait T {}\nmod a { pub struct X; }\n#[cfg(unix)] impl T for a::X {}\n#[cfg(windows)] impl T for a::X {}\n",
    );
    assert!(
        ids.contains("rust:impl:k.m.X.impl[T]@cfg(unix)")
            && ids.contains("rust:impl:k.m.X.impl[T]@cfg(windows)"),
        "same-path cfg-twin impl ids changed: {ids:?}"
    );
}

#[test]
fn cross_path_cfg_twins_keep_todays_cfg_ids() {
    // THE ladder-ordering pin (the no-churn invariant the @cfg-before-S
    // ordering exists for): cfg-twins across DIFFERENT self-type paths
    // (`#[cfg(unix)] impl T for a::X` / `#[cfg(windows)] impl T for b::X`)
    // are split by stage 1 on the BARE key, so S groups are singletons and
    // stay cold — the ids are byte-identical to the pre-Amendment-6 @cfg ids
    // (no `%3A%3A` anywhere).
    let ids = id_set(
        "pub trait T {}\nmod a { pub struct X; }\nmod b { pub struct X; }\n#[cfg(unix)] impl T for a::X {}\n#[cfg(windows)] impl T for b::X {}\n",
    );
    assert!(
        ids.contains("rust:impl:k.m.X.impl[T]@cfg(unix)")
            && ids.contains("rust:impl:k.m.X.impl[T]@cfg(windows)"),
        "cross-path cfg-twin impl ids changed from the @cfg-only form: {ids:?}"
    );
    assert!(
        ids.iter().all(|id| !id.contains("%3A")),
        "cfg-split cross-path twins must NOT be path-qualified: {ids:?}"
    );
}

#[test]
fn same_path_inherent_blocks_still_merge_to_one_impl_with_both_methods() {
    // Merge fidelity: two `impl a::X { … }` blocks share one witness, so S is
    // cold and the Option-(b) merge is untouched — ONE impl entity, both
    // methods hang off it.
    let entities = extract_file(
        "k",
        "k.m",
        "/p/src/m.rs",
        "mod a { pub struct X; }\nimpl a::X { fn f(&self){} }\nimpl a::X { fn g(&self){} }\n",
    )
    .unwrap();
    let impl_ids: Vec<&str> = entities
        .iter()
        .filter(|e| e["kind"] == "impl")
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        impl_ids,
        vec!["rust:impl:k.m.X.impl#<>"],
        "same-path inherent blocks must still merge to ONE bare-base impl"
    );
    let ids = id_set(
        "mod a { pub struct X; }\nimpl a::X { fn f(&self){} }\nimpl a::X { fn g(&self){} }\n",
    );
    assert!(ids.contains("rust:function:k.m.X.impl#<>.f"));
    assert!(ids.contains("rust:function:k.m.X.impl#<>.g"));
}

#[test]
fn mixed_bare_and_qualified_group_moves_only_the_multi_segment_member() {
    // A fired S group containing a BARE-path member: the single-segment
    // witness renders byte-identically to today, so only the multi-segment
    // member moves (`b%3A%3AX`); the bare member's id is unchanged.
    let ids = id_set(
        "pub trait T {}\nstruct X;\nmod b { pub struct X; }\nimpl T for X {}\nimpl T for b::X {}\n",
    );
    assert!(
        ids.contains("rust:impl:k.m.X.impl[T]"),
        "bare member of a mixed S group must keep its id: {ids:?}"
    );
    assert!(
        ids.contains("rust:impl:k.m.b%3A%3AX.impl[T]"),
        "qualified member of a mixed S group must carry its written path: {ids:?}"
    );
}

#[test]
fn reordering_a_self_type_path_twin_pair_is_id_stable() {
    // The S/T witness sets are BTree-collected, so a source reorder of the
    // twin pair yields the identical id set.
    let a = "pub trait T { fn go(&self); }\nmod a { pub struct X; }\nmod b { pub struct X; }\nimpl T for a::X { fn go(&self){} }\nimpl T for b::X { fn go(&self){} }\n";
    let b = "pub trait T { fn go(&self); }\nmod a { pub struct X; }\nmod b { pub struct X; }\nimpl T for b::X { fn go(&self){} }\nimpl T for a::X { fn go(&self){} }\n";
    assert_eq!(id_set(a), id_set(b));
}
