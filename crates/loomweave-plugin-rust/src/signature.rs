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
