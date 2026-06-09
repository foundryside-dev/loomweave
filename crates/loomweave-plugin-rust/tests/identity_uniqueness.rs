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
        // The same cfg-twin collision for a `struct` and an inline `mod` — the
        // `@cfg` discriminant is item-general (ADR-049 §3), not functions-only.
        // Without it both `S`/`inner` variants emit one bare locator and the
        // writer's ON CONFLICT silently drops one (the exact Phase-1a data-loss
        // family the zero-collision gate exists to catch).
        (
            "k",
            "k.m",
            "#[cfg(unix)] struct S;\n#[cfg(windows)] struct S;\n",
        ),
        (
            "k",
            "k.m",
            "#[cfg(unix)] mod inner { pub fn g(){} }\n#[cfg(windows)] mod inner { pub fn g(){} }\n",
        ),
        // Two cfg-gated inherent impls of the SAME type defining the SAME
        // method name. With all cfg variants visible (spec §5) both `go`
        // methods are extracted; under Option (b) the source-order ordinal is
        // GONE, so they stay distinct ONLY because the cfg-twin `@cfg`
        // discriminant splits the two impl entities (`impl#<>@cfg(unix)` vs
        // `impl#<>@cfg(windows)`) and the methods inherit it. Drop the cfg
        // discriminant and both impls + both `go` methods collapse to one
        // locator — so this entry makes the cfg discriminant a non-vacuous
        // regression for cfg-twin INHERENT impls (the `fn a`/`fn b` entry above
        // does not, since the names differ).
        (
            "k",
            "k.m",
            "struct Foo;\n#[cfg(unix)] impl Foo { fn go(&self){} }\n#[cfg(windows)] impl Foo { fn go(&self){} }\n",
        ),
        // A cfg-gated TRAIT-impl twin: same trait, same type, mutually-exclusive
        // cfgs. Both `fmt` methods AND both `impl` entities share
        // `Foo.impl[Display]` pre-cfg; the cfg discriminant must apply to TRAIT
        // impls too (extract.rs does NOT gate the suffix on `it.trait_.is_none()`),
        // else the two `impl[Display]` entities dedup to one and one `fmt` is
        // silently dropped. Proves the dropped guard.
        (
            "k",
            "k.m",
            "struct Foo;\n#[cfg(unix)] impl std::fmt::Display for Foo { fn fmt(&self,_:&mut std::fmt::Formatter)->std::fmt::Result{Ok(())} }\n#[cfg(windows)] impl std::fmt::Display for Foo { fn fmt(&self,_:&mut std::fmt::Formatter)->std::fmt::Result{Ok(())} }\n",
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
fn cfg_discriminant_is_load_bearing_for_cfg_twin_inherent_impls() {
    // Two cfg-gated inherent impls of `Foo`, each defining `go`. Both methods
    // are extracted (all-cfg-visible). Under Option (b) the source-order ordinal
    // is GONE; the `@cfg` discriminant on the impl entity (and inherited by the
    // method) is now the only thing keeping their locators apart
    // (`impl#<>@cfg(unix)` vs `impl#<>@cfg(windows)`). Assert there are exactly
    // two `go` methods and that they are distinct — a regression that goes red
    // if the cfg discriminant is ever dropped for cfg-twin inherent impls.
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
        "cfg discriminant collapsed two distinct methods to one locator"
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
