//! ADR-054 reachability-root tagging: `exported-api` / `entry-point` / `test` /
//! `allow-dead-code`. Drives the extractor's tag emission (clarion-05fdd0490e).
//!
//! Tests pass the file's root `module_path` directly so a `@bin(...)` root can
//! be simulated without a real Cargo layout (the bin discriminator is the
//! ADR-049 module-path segment, see `scope.rs`).

use std::collections::BTreeMap;

use loomweave_plugin_rust::extract::extract_file;
use serde_json::Value;

/// Every emitted entity id → its `tags` array (empty when none). Tags are
/// emitted sorted, so equality against a sorted literal is order-stable.
fn tags_by_id(crate_name: &str, module_path: &str, src: &str) -> BTreeMap<String, Vec<String>> {
    extract_file(crate_name, module_path, "/p/src/lib.rs", src)
        .unwrap()
        .iter()
        .map(|e| {
            let id = e["id"].as_str().unwrap().to_owned();
            let tags = e
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|t| t.as_str().unwrap().to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (id, tags)
        })
        .collect()
}

fn tags(map: &BTreeMap<String, Vec<String>>, id: &str) -> Vec<String> {
    map.get(id)
        .cloned()
        .unwrap_or_else(|| panic!("entity {id} not emitted; ids: {:?}", map.keys()))
}

// ---- exported-api (visibility chain → external surface) --------------------

#[test]
fn pub_lib_fn_is_exported_api() {
    let m = tags_by_id("k", "k.m", "pub fn helper() {}\n");
    assert_eq!(tags(&m, "rust:function:k.m.helper"), ["exported-api"]);
}

#[test]
fn private_fn_has_no_tags() {
    let m = tags_by_id("k", "k.m", "fn helper() {}\n");
    assert!(tags(&m, "rust:function:k.m.helper").is_empty());
}

#[test]
fn pub_crate_fn_is_not_exported_api() {
    // pub(crate)/pub(super)/pub(in ..) are intra-crate, statically analysable —
    // not the external API surface (ADR-054 §1).
    let m = tags_by_id("k", "k.m", "pub(crate) fn helper() {}\n");
    assert!(
        tags(&m, "rust:function:k.m.helper").is_empty(),
        "pub(crate) is not exported-api"
    );
}

#[test]
fn pub_fn_in_private_mod_is_not_exported_api() {
    // The visibility chain is broken by the private `mod internal`.
    let m = tags_by_id("k", "k.m", "mod internal { pub fn helper() {} }\n");
    assert!(
        tags(&m, "rust:function:k.m.internal.helper").is_empty(),
        "pub item under a private mod is not external surface"
    );
}

#[test]
fn pub_fn_in_pub_mod_is_exported_api() {
    let m = tags_by_id("k", "k.m", "pub mod api { pub fn helper() {} }\n");
    assert_eq!(tags(&m, "rust:function:k.m.api.helper"), ["exported-api"]);
}

#[test]
fn pub_leaf_kinds_are_exported_api() {
    let src = "pub struct S;\npub enum E { A }\npub trait T {}\n\
               pub type A = i32;\npub const C: i32 = 1;\npub static ST: i32 = 1;\n";
    let m = tags_by_id("k", "k.m", src);
    assert_eq!(tags(&m, "rust:struct:k.m.S"), ["exported-api"]);
    assert_eq!(tags(&m, "rust:enum:k.m.E"), ["exported-api"]);
    assert_eq!(tags(&m, "rust:trait:k.m.T"), ["exported-api"]);
    assert_eq!(tags(&m, "rust:type_alias:k.m.A"), ["exported-api"]);
    assert_eq!(tags(&m, "rust:const:k.m.C"), ["exported-api"]);
    assert_eq!(tags(&m, "rust:static:k.m.ST"), ["exported-api"]);
}

// ---- entry-point ----------------------------------------------------------

#[test]
fn fn_main_is_entry_point() {
    let m = tags_by_id("k", "k", "fn main() {}\n");
    assert_eq!(tags(&m, "rust:function:k.main"), ["entry-point"]);
}

#[test]
fn tokio_main_attr_is_entry_point() {
    let m = tags_by_id("k", "k", "#[tokio::main]\nasync fn run() {}\n");
    assert_eq!(tags(&m, "rust:function:k.run"), ["entry-point"]);
}

#[test]
fn no_mangle_ffi_export_is_entry_point_and_exported_api() {
    // FFI export in a lib file: an entry from outside the Rust graph AND pub.
    let m = tags_by_id("k", "k.m", "#[no_mangle]\npub extern \"C\" fn ffi() {}\n");
    assert_eq!(
        tags(&m, "rust:function:k.m.ffi"),
        ["entry-point", "exported-api"]
    );
}

// ---- test -----------------------------------------------------------------

#[test]
fn test_attr_fn_is_test() {
    let m = tags_by_id("k", "k.m", "#[test]\nfn it_works() {}\n");
    assert_eq!(tags(&m, "rust:function:k.m.it_works"), ["test"]);
}

#[test]
fn bench_attr_fn_is_test() {
    let m = tags_by_id("k", "k.m", "#[bench]\nfn bench_it(b: &mut Bencher) {}\n");
    assert_eq!(tags(&m, "rust:function:k.m.bench_it"), ["test"]);
}

#[test]
fn items_under_cfg_test_mod_are_test() {
    let src = "#[cfg(test)]\nmod tests {\n    fn helper() {}\n    struct Fixture;\n}\n";
    let m = tags_by_id("k", "k.m", src);
    assert_eq!(tags(&m, "rust:function:k.m.tests.helper"), ["test"]);
    assert_eq!(tags(&m, "rust:struct:k.m.tests.Fixture"), ["test"]);
}

// ---- allow-dead-code (explicit author keep-signal) ------------------------

#[test]
fn allow_dead_code_is_root() {
    let m = tags_by_id("k", "k.m", "#[allow(dead_code)]\nfn kept() {}\n");
    assert_eq!(tags(&m, "rust:function:k.m.kept"), ["allow-dead-code"]);
}

#[test]
fn allow_dead_code_combines_with_exported_api() {
    let m = tags_by_id("k", "k.m", "#[allow(dead_code)]\npub fn kept() {}\n");
    assert_eq!(
        tags(&m, "rust:function:k.m.kept"),
        ["allow-dead-code", "exported-api"]
    );
}

// ---- bin targets (pub is internal; main is the entry) ---------------------

#[test]
fn bin_target_pub_fn_is_not_exported_api() {
    let m = tags_by_id("k", "k@bin(k)", "pub fn helper() {}\nfn main() {}\n");
    assert!(
        tags(&m, "rust:function:k@bin(k).helper").is_empty(),
        "a bin target's pub item is internal, not external API"
    );
    assert_eq!(tags(&m, "rust:function:k@bin(k).main"), ["entry-point"]);
}

// ---- macros (exported via #[macro_export], not `pub`) ---------------------

#[test]
fn macro_export_is_exported_api() {
    let m = tags_by_id(
        "k",
        "k.m",
        "#[macro_export]\nmacro_rules! mac { () => {}; }\n",
    );
    assert_eq!(tags(&m, "rust:macro:k.m.mac"), ["exported-api"]);
}

#[test]
fn non_exported_macro_has_no_tags() {
    let m = tags_by_id("k", "k.m", "macro_rules! mac { () => {}; }\n");
    assert!(tags(&m, "rust:macro:k.m.mac").is_empty());
}
