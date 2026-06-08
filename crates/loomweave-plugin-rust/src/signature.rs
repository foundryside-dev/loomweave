//! ADR-038 SEI signatures (Task 8).
//!
//! Per the manifest schemas: `function` → `{v:1, params, return_ann,
//! generics}`, `struct` → `{v:1, fields}`. The core stores these verbatim and
//! compares by string equality, so they must be deterministic (stable field
//! order, canonical rendering). Task 6 needs the builders so extraction can
//! attach signatures; Task 8 expands and pins their exact rendering.
use quote::ToTokens;
use serde_json::{Value, json};
use syn::{Fields, Signature};

/// Deterministic SEI signature object for a `function` entity.
#[must_use]
pub fn function_signature(sig: &Signature) -> Value {
    let params: Vec<String> = sig
        .inputs
        .iter()
        .map(|a| a.to_token_stream().to_string())
        .collect();
    let return_ann = match &sig.output {
        syn::ReturnType::Default => String::new(),
        syn::ReturnType::Type(_, ty) => ty.to_token_stream().to_string(),
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
            .map(|f| f.to_token_stream().to_string())
            .collect(),
        Fields::Unnamed(u) => u
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{i}: {}", f.ty.to_token_stream()))
            .collect(),
        Fields::Unit => Vec::new(),
    };
    json!({ "v": 1, "fields": rendered })
}
