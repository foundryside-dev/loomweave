use loomweave_plugin_rust::extract::extract_file;

/// One source string per ADR-049 collision family, plus the cross-crate case.
fn corpus() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        // (crate, module, src)
        (
            "k",
            "k.m",
            "struct Foo;\nimpl std::fmt::Display for Foo { fn fmt(&self,_:&mut std::fmt::Formatter)->std::fmt::Result{Ok(())} }\nimpl std::fmt::Debug for Foo { fn fmt(&self,_:&mut std::fmt::Formatter)->std::fmt::Result{Ok(())} }\n",
        ),
        (
            "k",
            "k.m",
            "struct Foo;\nimpl From<i32> for Foo { fn from(_:i32)->Foo{Foo} }\nimpl From<u32> for Foo { fn from(_:u32)->Foo{Foo} }\n",
        ),
        (
            "k",
            "k.m",
            "struct Foo;\nimpl Foo { fn a(&self){} }\nimpl Foo { fn b(&self){} }\n",
        ),
        (
            "k",
            "k.m",
            "#[cfg(unix)] fn f(){}\n#[cfg(windows)] fn f(){}\n",
        ),
        // Two cfg-gated inherent impls of the SAME type defining the SAME
        // method name. With all cfg variants visible (spec §5) both `go`
        // methods are extracted; they stay distinct ONLY because the
        // inherent-impl ordinal (`impl#<>#0` vs `impl#<>#1`) is in the key.
        // Drop the ordinal and these two collapse to one locator — so this
        // entry makes the ordinal discriminant a non-vacuous regression
        // (the `fn a`/`fn b` entry above does not, since the names differ).
        (
            "k",
            "k.m",
            "struct Foo;\n#[cfg(unix)] impl Foo { fn go(&self){} }\n#[cfg(windows)] impl Foo { fn go(&self){} }\n",
        ),
    ]
}

#[test]
fn no_duplicate_ids_across_every_collision_family() {
    // Each corpus entry is its own `extract_file` extraction. Because every
    // extraction legitimately re-emits the file-level `module` (and several
    // families reuse `struct Foo`), uniqueness is an *intra-extraction*
    // invariant: within a single file's entities, no two ids may collide.
    // (Aggregating ids across the four calls into one flat list would
    // double-count those legitimate per-call repeats and mask which collisions
    // are real — the cfg twin being the only genuine one.)
    for (c, m, src) in corpus() {
        let all: Vec<String> = extract_file(c, m, "/p/src/m.rs", src)
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_owned())
            .collect();
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            all.len(),
            "duplicate locator(s) emitted for ({c}, {m}): {:?}",
            duplicates(&all)
        );
    }
}

fn duplicates(ids: &[String]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    ids.iter()
        .filter(|i| !seen.insert((*i).clone()))
        .cloned()
        .collect()
}

#[test]
fn inherent_impl_ordinal_is_load_bearing() {
    // Two cfg-gated inherent impls of `Foo`, each defining `go`. Both methods
    // are extracted (all-cfg-visible); the ordinal in `impl#<>#N` is the only
    // thing keeping their locators apart. Assert there are exactly two `go`
    // methods and that they are distinct — a regression that goes red if the
    // ordinal discriminant is ever removed.
    let src = "struct Foo;\n#[cfg(unix)] impl Foo { fn go(&self){} }\n#[cfg(windows)] impl Foo { fn go(&self){} }\n";
    let go_ids: Vec<String> = extract_file("k", "k.m", "/p/src/m.rs", src)
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_str().map(ToOwned::to_owned))
        .filter(|id| id.rsplit('.').next() == Some("go"))
        .collect();
    assert_eq!(
        go_ids.len(),
        2,
        "expected both cfg-gated `go` methods, got {go_ids:?}"
    );
    assert_ne!(
        go_ids[0], go_ids[1],
        "ordinal discriminant collapsed two distinct methods to one locator"
    );
}

#[test]
fn cross_crate_same_item_distinct() {
    let a: Vec<String> = extract_file(
        "loomweave_core",
        "loomweave_core.config",
        "/p/a.rs",
        "pub struct X;\n",
    )
    .unwrap()
    .iter()
    .map(|e| e["id"].as_str().unwrap().to_owned())
    .collect();
    let b: Vec<String> = extract_file(
        "loomweave_cli",
        "loomweave_cli.config",
        "/p/b.rs",
        "pub struct X;\n",
    )
    .unwrap()
    .iter()
    .map(|e| e["id"].as_str().unwrap().to_owned())
    .collect();
    assert!(a.iter().all(|id| !b.contains(id)));
}
