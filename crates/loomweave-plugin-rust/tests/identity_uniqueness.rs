use loomweave_plugin_rust::extract::{extract_file, extract_file_degraded_aware};

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
        // ADR-049 Amendment 5 (clarion-dfeb905f46): two cfg-gated twin METHODS
        // inside ONE impl block. The impl-level `@cfg` discriminant cannot help
        // (one block → one impl key); without a METHOD-level `@cfg` suffix both
        // `go` methods render `Foo.impl#<>.go` and the writer's ON CONFLICT
        // silently keeps one. The fix applies `cfg_suffix(&m.attrs)` to twin
        // methods just as it does to free items / impl blocks.
        (
            "k",
            "k.m",
            "struct Foo;\nimpl Foo { #[cfg(unix)] fn go(&self){} #[cfg(windows)] fn go(&self){} }\n",
        ),
        // ADR-049 Amendment 5, cross-merged-block variant: two SEPARATE impl
        // blocks on the same `(type, sig, cfg)` MERGE into one impl entity
        // (Option b), each contributing a `go`. The method-twin count must span
        // all blocks sharing the pre-cfg impl key, not just one block, or the
        // merged `go` methods collide.
        (
            "k",
            "k.m",
            "struct Foo;\nimpl Foo { #[cfg(unix)] fn go(&self){} }\nimpl Foo { #[cfg(windows)] fn go(&self){} }\n",
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
fn cfg_discriminant_is_load_bearing_for_cfg_twin_methods_in_one_block() {
    // ADR-049 Amendment 5 (clarion-dfeb905f46): two cfg-gated `go` methods in
    // ONE impl block. The impl-level @cfg cannot split them (one impl key); a
    // method-level @cfg suffix must. Assert exactly two `go` methods, distinct.
    let src = "struct Foo;\nimpl Foo { #[cfg(unix)] fn go(&self){} #[cfg(windows)] fn go(&self){} }\n";
    // The method-level @cfg suffix lands AFTER the name: `…go@cfg(unix)`, so the
    // final `.`-segment starts with `go` rather than equalling it.
    let go_ids: Vec<String> = extract_file("k", "k.m", "/p/src/m.rs", src)
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_str().map(ToOwned::to_owned))
        .filter(|id| id.rsplit('.').next().is_some_and(|s| s.starts_with("go")))
        .collect();
    assert_eq!(
        go_ids.len(),
        2,
        "expected both cfg-gated `go` methods, got {go_ids:?}"
    );
    assert_ne!(
        go_ids[0], go_ids[1],
        "method-level cfg discriminant collapsed two distinct methods to one locator"
    );
    assert!(
        go_ids.iter().any(|id| id.ends_with("go@cfg(unix)"))
            && go_ids.iter().any(|id| id.ends_with("go@cfg(windows)")),
        "expected method-level @cfg suffixes, got {go_ids:?}"
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

#[test]
fn stacked_cfg_twins_get_distinct_ids() {
    // FINDING #5: legally-coexisting stacked-cfg twins share a leading
    // `#[cfg(unix)]` but differ on a second `#[cfg(feature=...)]`. Folding only
    // the FIRST cfg would hand both an identical `@cfg(unix)` discriminant and
    // collide one away at the writer's ON CONFLICT. Folding ALL cfgs keeps the
    // two `f` functions distinct.
    let src = "#[cfg(unix)] #[cfg(feature=\"a\")] fn f(){}\n\
               #[cfg(unix)] #[cfg(feature=\"b\")] fn f(){}\n";
    let f_ids: Vec<String> = extract_file("k", "k.m", "/p/src/m.rs", src)
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_str().map(ToOwned::to_owned))
        .filter(|id| id.contains(":function:"))
        .collect();
    assert_eq!(
        f_ids.len(),
        2,
        "expected both stacked-cfg-twin `f` functions, got {f_ids:?}"
    );
    assert_ne!(
        f_ids[0], f_ids[1],
        "stacked-cfg twins collapsed to one locator (only the first cfg folded)"
    );
}

#[test]
fn reserved_char_in_cfg_predicate_does_not_collapse_the_file() {
    // FINDING #6: a cfg predicate carrying a reserved entity-id char (`:` in
    // `feature = "a:b"`) must NOT flow verbatim into the qualname — that would
    // make build_entity_id reject the id and, on the real ingest path, degrade
    // the ENTIRE clean-parse file to a single `syntax_error` module, discarding
    // every real entity. The cfg discriminant escapes the reserved char, so the
    // file parses cleanly and the twins extract.
    let src = "#[cfg(feature=\"a:b\")] fn f(){}\n\
               #[cfg(feature=\"c:d\")] fn f(){}\n\
               struct Keeper;\n";

    // The degraded-aware path is the real ingest path that collapses on an
    // id-validation error. Post-fix it must report NO findings and NOT collapse.
    let (entities, _edges, _sites, _ref_stats, findings) =
        extract_file_degraded_aware("k", "k.m", "/p/src/m.rs", src);
    assert!(
        findings.is_empty(),
        "exotic-but-legal cfg predicate produced a degraded-parse finding: {findings:?}"
    );
    let kinds: Vec<&str> = entities.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        !kinds.contains(&"syntax_error"),
        "exotic-but-legal cfg predicate collapsed the file to syntax_error: {kinds:?}"
    );
    // The unrelated real entity survived — the file did not collapse.
    assert!(
        entities
            .iter()
            .any(|e| e["id"].as_str().is_some_and(|id| id.ends_with(".Keeper"))),
        "clean entity `Keeper` was discarded — file collapsed"
    );
    // Every emitted id is a valid 3-segment entity id (no raw `:` leaked into a
    // qualname segment).
    for e in &entities {
        let id = e["id"].as_str().unwrap();
        assert_eq!(
            id.matches(':').count(),
            2,
            "id has a reserved-char leak in its qualname segment: {id}"
        );
    }
    // The two reserved-char twins remain distinct.
    let f_ids: Vec<String> = entities
        .iter()
        .filter_map(|e| e["id"].as_str().map(ToOwned::to_owned))
        .filter(|id| id.contains(":function:"))
        .collect();
    assert_eq!(f_ids.len(), 2, "expected both `f` twins, got {f_ids:?}");
    assert_ne!(
        f_ids[0], f_ids[1],
        "reserved-char cfg twins collapsed to one locator"
    );
}
