//! ADR-038 SEI signatures (Task 8).
//!
//! Per the manifest schemas: `function` → `{v:1, params, return_ann,
//! generics}`, `struct` → `{v:1, fields}`. The core stores these verbatim and
//! compares by string equality, so they must be deterministic (stable field
//! order, canonical rendering). Task 6 needs the builders so extraction can
//! attach signatures; Task 8 expands and pins their exact rendering.
use crate::qualname::{path_textual, self_ty_name};
use quote::ToTokens;
use serde_json::{Value, json};
use syn::{Fields, ItemImpl, ItemTrait, Signature, TypeParamBound};

/// Canonicalise `proc-macro2`'s spaced token rendering so the stored signature
/// is stable under ADR-038's string-equality comparison. `to_token_stream()`
/// renders `x : i32` / `a , b`; collapse the punctuation spacing to the
/// conventional `x: i32` / `a, b`. Applied to every rendered surface (params,
/// return type, struct fields) so the whole signature is canonical, not just
/// one field.
fn tidy(rendered: &str) -> String {
    rendered.replace(" : ", ": ").replace(" , ", ", ")
}

/// Deterministic SEI signature object for a `function` entity.
#[must_use]
pub fn function_signature(sig: &Signature) -> Value {
    let params: Vec<String> = sig
        .inputs
        .iter()
        .map(|a| tidy(&a.to_token_stream().to_string()))
        .collect();
    let return_ann = match &sig.output {
        syn::ReturnType::Default => String::new(),
        syn::ReturnType::Type(_, ty) => tidy(&ty.to_token_stream().to_string()),
    };
    let generics: Vec<String> = sig
        .generics
        .params
        .iter()
        .map(|p| match p {
            syn::GenericParam::Type(t) => t.ident.to_string(),
            syn::GenericParam::Lifetime(l) => format!("'{}", l.lifetime.ident),
            syn::GenericParam::Const(c) => c.ident.to_string(),
        })
        .collect();
    json!({ "v": 1, "params": params, "return_ann": return_ann, "generics": generics })
}

/// Deterministic SEI signature object for a `struct` entity.
#[must_use]
pub fn struct_signature(fields: &Fields) -> Value {
    let rendered: Vec<String> = match fields {
        Fields::Named(n) => n
            .named
            .iter()
            .map(|f| tidy(&f.to_token_stream().to_string()))
            .collect(),
        Fields::Unnamed(u) => u
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{i}: {}", tidy(&f.ty.to_token_stream().to_string())))
            .collect(),
        Fields::Unit => Vec::new(),
    };
    json!({ "v": 1, "fields": rendered })
}

/// Deterministic SEI signature object for a `trait` entity (spec §4.4).
///
/// `supertraits` is the trait-bound paths of `it.supertraits`, each rendered
/// via [`path_textual`] (whitespace-stripped, matching the rest of the crate's
/// path normalisation — `tidy()` only collapses `: ` / `, ` and would leave
/// `std :: fmt :: Debug` unjoined). Lifetime and other non-trait bounds are
/// skipped (supertraits are trait bounds). Sorted for determinism under
/// ADR-038's string-equality comparison.
#[must_use]
pub fn trait_signature(it: &ItemTrait) -> Value {
    let mut supertraits: Vec<String> = it
        .supertraits
        .iter()
        .filter_map(|b| match b {
            TypeParamBound::Trait(t) => Some(path_textual(&t.path)),
            _ => None,
        })
        .collect();
    supertraits.sort();
    json!({ "v": 1, "supertraits": supertraits })
}

/// Deterministic SEI signature object for an `impl` entity (spec §4.4).
///
/// `target` is the self type's locator name via [`self_ty_name`]; `trait` is
/// the implemented trait path rendered via [`path_textual`], or JSON `null`
/// for an inherent impl.
#[must_use]
pub fn impl_signature(it: &ItemImpl) -> Value {
    let target = self_ty_name(&it.self_ty);
    let trait_ = match &it.trait_ {
        Some((_, path, _)) => Value::String(path_textual(path)),
        None => Value::Null,
    };
    json!({ "v": 1, "target": target, "trait": trait_ })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn function_signature_captures_params_return_and_generics() {
        let f: syn::ItemFn = parse_quote!(
            fn g<T>(x: i32, y: T) -> bool {
                true
            }
        );
        let s = function_signature(&f.sig);
        assert_eq!(s["v"], 1);
        assert_eq!(s["params"][0], "x: i32");
        assert_eq!(s["return_ann"], "bool");
        assert_eq!(s["generics"][0], "T");
    }

    #[test]
    fn struct_signature_captures_named_fields() {
        let st: syn::ItemStruct = parse_quote!(
            struct W {
                a: i32,
                b: String,
            }
        );
        let s = struct_signature(&st.fields);
        assert_eq!(s["v"], 1);
        assert_eq!(s["fields"][0], "a: i32");
    }
}
