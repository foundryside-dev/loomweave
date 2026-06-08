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
