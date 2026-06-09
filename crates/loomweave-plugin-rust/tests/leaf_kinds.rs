use loomweave_plugin_rust::extract::extract_file;

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
        assert!(got.contains(&want.to_owned()), "missing {want}; got {got:?}");
    }
}

#[test]
fn cfg_twin_discriminant_is_item_general_for_leaf_kinds() {
    // Two cfg-gated enums of the same name must not collide.
    let got = ids("#[cfg(unix)] enum E {}\n#[cfg(windows)] enum E {}\n");
    let es: Vec<_> = got.iter().filter(|i| i.starts_with("rust:enum:")).collect();
    assert_eq!(es.len(), 2, "both cfg-twin enums emitted: {es:?}");
    assert_ne!(es[0], es[1], "cfg discriminant must separate them");
}
