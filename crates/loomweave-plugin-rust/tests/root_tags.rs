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

#[test]
fn unsafe_no_mangle_ffi_export_is_entry_point_and_exported_api() {
    // Edition 2024 makes bare `#[no_mangle]` a hard error; real FFI code writes
    // `#[unsafe(no_mangle)]`, which syn parses as `Meta::List { path: "unsafe",
    // tokens: "no_mangle" }`. The export ident lives one level in — it must
    // still root, or every edition-2024 FFI export reads as dead (the
    // under-rooting failure ADR-054 fights).
    let m = tags_by_id(
        "k",
        "k.m",
        "#[unsafe(no_mangle)]\npub extern \"C\" fn ffi() {}\n",
    );
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

#[test]
fn macro_export_in_private_mod_is_still_exported_api() {
    // `#[macro_export]` lifts a macro to the crate root regardless of module
    // privacy — chain-INDEPENDENT, unlike `pub` visibility. The common idiom is
    // `mod macros { #[macro_export] macro_rules! ... }`; without this the macro
    // (real external API) reads as dead — the under-rooting failure ADR-054 fights.
    let m = tags_by_id(
        "k",
        "k.m",
        "mod internal { #[macro_export] macro_rules! mac { () => {}; } }\n",
    );
    let id = m
        .keys()
        .find(|k| k.starts_with("rust:macro:"))
        .expect("macro emitted")
        .clone();
    assert_eq!(
        tags(&m, &id),
        ["exported-api"],
        "#[macro_export] is chain-independent"
    );
}

// ---- entry-point: export_name FFI (distinct from no_mangle) ----------------

#[test]
fn export_name_ffi_export_is_entry_point() {
    let m = tags_by_id(
        "k",
        "k.m",
        "#[export_name = \"my_export\"]\npub extern \"C\" fn exported() {}\n",
    );
    assert_eq!(
        tags(&m, "rust:function:k.m.exported"),
        ["entry-point", "exported-api"]
    );
}

#[test]
fn unsafe_export_name_ffi_export_is_entry_point() {
    // Edition-2024 wrapped form (see `unsafe_no_mangle_…`): `#[unsafe(export_name
    // = "…")]` parses as `unsafe(<NameValue>)`, so the inner `export_name` ident
    // must be reached through the wrapper.
    let m = tags_by_id(
        "k",
        "k.m",
        "#[unsafe(export_name = \"my_export\")]\npub extern \"C\" fn exported() {}\n",
    );
    assert_eq!(
        tags(&m, "rust:function:k.m.exported"),
        ["entry-point", "exported-api"]
    );
}

// ---- regression guard: serde/typetag must NOT be mistaken for a root -------

#[test]
fn serde_attribute_is_not_a_root() {
    // The typetag::serde catastrophe class: a bare last-segment match on `serde`
    // would tag every `#[serde(...)]`. This pins that a real serde attribute (and
    // a non-framework derive) produces NO reachability-root tag.
    let serded = tags_by_id(
        "k",
        "k.m",
        "#[serde(rename_all = \"camelCase\")]\nstruct P { v: i32 }\n",
    );
    let derived = tags_by_id(
        "k",
        "k.m",
        "#[derive(Clone, Debug, Serialize)]\nstruct Q { v: i32 }\n",
    );
    assert!(tags(&serded, "rust:struct:k.m.P").is_empty());
    assert!(tags(&derived, "rust:struct:k.m.Q").is_empty());
}

// ---- impl-method rooting (increment 2: pub methods of pub types) -----------

/// Find the single emitted method entity whose id ends in `.<name>` (robust to
/// the exact `…impl[…]` qualname rendering).
fn method_tags(map: &BTreeMap<String, Vec<String>>, name: &str) -> Vec<String> {
    let id = map
        .keys()
        .find(|k| k.starts_with("rust:function:") && k.ends_with(&format!(".{name}")))
        .unwrap_or_else(|| panic!("method {name} not emitted; ids: {:?}", map.keys()));
    map.get(id).cloned().unwrap_or_default()
}

#[test]
fn pub_inherent_method_is_exported_api() {
    let src = "pub struct S;\nimpl S { pub fn doit(&self) {} fn helper(&self) {} }\n";
    let m = tags_by_id("k", "k.m", src);
    assert_eq!(
        method_tags(&m, "doit"),
        ["exported-api"],
        "a pub inherent method is external API"
    );
    assert!(
        method_tags(&m, "helper").is_empty(),
        "a private inherent method is not rooted"
    );
}

#[test]
fn trait_impl_method_is_not_exported_api() {
    // Trait methods carry inherited visibility (no `pub`), so the pub rule does
    // not root them — their dispatch-reachability is a deferred follow-up.
    let src = "pub struct S;\n\
               impl std::fmt::Display for S {\n\
                   fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) }\n\
               }\n";
    let m = tags_by_id("k", "k.m", src);
    assert!(
        method_tags(&m, "fmt").is_empty(),
        "a trait-impl method is not rooted by the pub rule"
    );
}

#[test]
fn pub_method_in_bin_target_is_not_exported_api() {
    let src = "pub struct S;\nimpl S { pub fn doit(&self) {} }\n";
    let m = tags_by_id("k", "k@bin(k)", src);
    assert!(
        method_tags(&m, "doit").is_empty(),
        "a bin target's pub method is internal, not external API"
    );
}

#[test]
fn pub_method_under_private_mod_is_not_exported_api() {
    // The impl's enclosing module chain must be pub (same rule as free items).
    let src = "mod internal { pub struct S; impl S { pub fn doit(&self) {} } }\n";
    let m = tags_by_id("k", "k.m", src);
    assert!(
        method_tags(&m, "doit").is_empty(),
        "a pub method under a private mod is not external surface"
    );
}

// ---- framework-attribute handlers (increment 2) ---------------------------
// http-route / cli-command emit `framework-handler` as a companion (mirroring
// the Python plugin); FFI host exports + proc-macros map to `entry-point` (a
// real root — their callees are traversed). framework-handler is an
// excluded-tag, never a standalone root.

#[test]
fn actix_route_attr_is_http_route() {
    let m = tags_by_id("k", "k.m", "#[get(\"/\")]\nasync fn index() {}\n");
    assert_eq!(
        tags(&m, "rust:function:k.m.index"),
        ["framework-handler", "http-route"]
    );
}

#[test]
fn post_and_generic_route_attrs_are_http_route() {
    let src = "#[post(\"/x\")]\nfn create() {}\n#[route(\"/y\")]\nfn multi() {}\n";
    let m = tags_by_id("k", "k.m", src);
    assert_eq!(
        tags(&m, "rust:function:k.m.create"),
        ["framework-handler", "http-route"]
    );
    assert_eq!(
        tags(&m, "rust:function:k.m.multi"),
        ["framework-handler", "http-route"]
    );
}

#[test]
fn clap_parser_derive_is_cli_command() {
    let m = tags_by_id("k", "k.m", "#[derive(Parser)]\nstruct Cli { v: i32 }\n");
    assert_eq!(
        tags(&m, "rust:struct:k.m.Cli"),
        ["cli-command", "framework-handler"]
    );
}

#[test]
fn clap_subcommand_enum_is_cli_command() {
    let m = tags_by_id("k", "k.m", "#[derive(Subcommand)]\nenum Cmd { A, B }\n");
    assert_eq!(
        tags(&m, "rust:enum:k.m.Cmd"),
        ["cli-command", "framework-handler"]
    );
}

#[test]
fn pyo3_pyfunction_is_entry_point() {
    let m = tags_by_id(
        "k",
        "k.m",
        "#[pyfunction]\nfn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    assert_eq!(tags(&m, "rust:function:k.m.add"), ["entry-point"]);
}

#[test]
fn pyo3_pyclass_is_entry_point() {
    let m = tags_by_id("k", "k.m", "#[pyclass]\nstruct PyThing { v: i32 }\n");
    assert_eq!(tags(&m, "rust:struct:k.m.PyThing"), ["entry-point"]);
}

#[test]
fn proc_macro_is_entry_point() {
    let m = tags_by_id(
        "k",
        "k.m",
        "#[proc_macro]\npub fn my_macro(input: TokenStream) -> TokenStream { input }\n",
    );
    assert_eq!(
        tags(&m, "rust:function:k.m.my_macro"),
        ["entry-point", "exported-api"]
    );
}

#[test]
fn rstest_attr_is_test() {
    let m = tags_by_id("k", "k.m", "#[rstest]\nfn checks() {}\n");
    assert_eq!(tags(&m, "rust:function:k.m.checks"), ["test"]);
}

#[test]
fn serde_and_plain_derives_are_not_roots() {
    // Guard against the typetag::serde catastrophic-collision class: neither a
    // bare `#[serde(...)]` nor a non-framework derive may produce a root tag.
    let plain = tags_by_id(
        "k",
        "k.m",
        "#[derive(Clone, Debug)]\nstruct Plain { v: i32 }\n",
    );
    let serded = tags_by_id("k", "k.m", "struct P { v: i32 }\n");
    assert!(tags(&plain, "rust:struct:k.m.Plain").is_empty());
    assert!(tags(&serded, "rust:struct:k.m.P").is_empty());
}
