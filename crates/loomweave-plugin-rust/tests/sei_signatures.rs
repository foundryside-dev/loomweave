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

#[test]
fn impl_signature_target_stays_last_segment_for_a_fired_group_member() {
    // ADR-049 Amendments 6/7 explicitly leave the ADR-038 SEI signature
    // alone: the S/T qualification rewrites only the impl LOCATOR; the
    // signature `target` stays the self type's bare LAST SEGMENT ("changing
    // it would be mass churn for zero identity value"). `impl a::Tr for
    // c::X` is the canonical fired-group member shape (its locator renders
    // `c%3A%3AX.impl[a%3A%3ATr]` inside a fired group) — the signature is
    // per-item and group-independent, so these bytes hold fired or not.
    let it: syn::ItemImpl = syn::parse_str("impl a::Tr for c::X {}").unwrap();
    assert_eq!(
        impl_signature(&it),
        serde_json::json!({"v":1,"target":"X","trait":"a::Tr"})
    );
}
