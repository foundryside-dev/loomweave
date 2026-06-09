//! SEI signature builders for `trait` and `impl` (spec §4.4).
//!
//! Mirrors the inline `function_signature`/`struct_signature` cases in
//! `signature.rs`; these live in an external integration test because they
//! pin the *public* SEI shape the core stores and string-compares.

use loomweave_plugin_rust::signature::{impl_signature, trait_signature};

#[test]
fn trait_signature_lists_supertraits_sorted() {
    let it: syn::ItemTrait = syn::parse_str("trait T: Clone + std::fmt::Debug {}").unwrap();
    assert_eq!(
        trait_signature(&it),
        serde_json::json!({"v":1,"supertraits":["Clone","std::fmt::Debug"]})
    );
}

#[test]
fn impl_signature_carries_target_and_trait() {
    let it: syn::ItemImpl = syn::parse_str("impl std::fmt::Display for Foo {}").unwrap();
    assert_eq!(
        impl_signature(&it),
        serde_json::json!({"v":1,"target":"Foo","trait":"std::fmt::Display"})
    );
    let inh: syn::ItemImpl = syn::parse_str("impl Foo {}").unwrap();
    assert_eq!(
        impl_signature(&inh),
        serde_json::json!({"v":1,"target":"Foo","trait":serde_json::Value::Null})
    );
}
