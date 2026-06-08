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
