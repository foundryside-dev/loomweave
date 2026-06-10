use loomweave_plugin_rust::extract::{extract_file, extract_file_full};

fn ids(src: &str) -> Vec<String> {
    extract_file("k", "k.m", "/p/src/m.rs", src)
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn each_leaf_kind_is_emitted_with_its_kind_segment() {
    let src = "\
        pub enum E { A, B }\n\
        pub trait T { fn m(&self); }\n\
        pub type Alias = i32;\n\
        pub const C: i32 = 1;\n\
        pub static S: i32 = 1;\n\
        macro_rules! mac { () => {}; }\n";
    let got = ids(src);
    for want in [
        "rust:enum:k.m.E",
        "rust:trait:k.m.T",
        "rust:type_alias:k.m.Alias",
        "rust:const:k.m.C",
        "rust:static:k.m.S",
        "rust:macro:k.m.mac",
    ] {
        assert!(
            got.contains(&want.to_owned()),
            "missing {want}; got {got:?}"
        );
    }
}

#[test]
fn unnamed_const_is_not_an_entity() {
    // ADR-049 Amendment 9 (clarion-83870dc534): `const _` is non-identifying —
    // it is skipped entirely (no entity, no contains edge), so only the NAMED
    // const emits and repeated `_` twins cannot collide.
    let src = "pub const LIMIT: u32 = 10;\nconst _: () = ();\nconst _: () = ();\n";
    let extracted = extract_file_full("k", "k.m", "/p/src/m.rs", src).unwrap();
    let got: Vec<String> = extracted
        .entities
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    let consts: Vec<_> = got
        .iter()
        .filter(|i| i.starts_with("rust:const:"))
        .collect();
    assert_eq!(
        consts,
        vec!["rust:const:k.m.LIMIT"],
        "exactly the named const emits; got {got:?}"
    );
    assert!(
        got.iter().all(|i| !i.ends_with("._")),
        "no qualname may end `._`: {got:?}"
    );
    assert!(
        extracted
            .edges
            .iter()
            .all(|e| e["to_id"].as_str().unwrap() != "rust:const:k.m._"),
        "no contains edge may target a skipped `_` const"
    );
    let mut sorted = got.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), got.len(), "duplicate ids emitted: {got:?}");
}

#[test]
fn same_cfg_unnamed_const_twins_emit_nothing() {
    // The hard repro behind Amendment 9: byte-identical cfg attrs hand both
    // twins the SAME @cfg suffix, so no discriminant can split them — the
    // unconditional skip is the only duplicate-free answer.
    let src = "#[cfg(target_pointer_width = \"64\")]\nconst _: () = ();\n\
               #[cfg(target_pointer_width = \"64\")]\nconst _: () = ();\n";
    let got = ids(src);
    assert!(
        got.iter().all(|i| !i.starts_with("rust:const:")),
        "no const entity may emit for `const _` twins: {got:?}"
    );
    let mut sorted = got.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), got.len(), "duplicate ids emitted: {got:?}");
}

#[test]
fn leading_underscore_named_const_is_kept() {
    // The skip is `ident == "_"` exactly — a NAMED const that merely starts
    // with an underscore is a normal entity.
    let got = ids("const _FOO: u32 = 1;\n");
    assert!(
        got.contains(&"rust:const:k.m._FOO".to_owned()),
        "named `_FOO` const must still emit; got {got:?}"
    );
}

#[test]
fn cfg_twin_discriminant_is_item_general_for_leaf_kinds() {
    // Two cfg-gated enums of the same name must not collide.
    let got = ids("#[cfg(unix)] enum E {}\n#[cfg(windows)] enum E {}\n");
    let es: Vec<_> = got.iter().filter(|i| i.starts_with("rust:enum:")).collect();
    assert_eq!(es.len(), 2, "both cfg-twin enums emitted: {es:?}");
    assert_ne!(es[0], es[1], "cfg discriminant must separate them");
}
