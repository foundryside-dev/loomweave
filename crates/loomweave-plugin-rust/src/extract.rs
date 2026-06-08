//! syn-based extraction of module/struct/function entities (Task 6).
//! ADR-038 SEI signatures (Task 8).
//!
//! Parse one file with `syn`, walk top-level + inline-`mod` items, and emit
//! entity JSON `Value`s matching the wire contract. The file-level `module`
//! entity is `file_scope` (the core auto-emits its `contains` edge).
//! Inherent/trait impl methods use the Task 4 qualnames; the impl block itself
//! becomes an `impl` entity in Phase 1b (Phase 1a emits `module`/`struct`/
//! `function`, where `function` includes methods).
use serde_json::{Value, json};
use syn::{ImplItem, Item, ItemFn, ItemImpl, ItemMod, ItemStruct};

use crate::qualname::{
    build_entity_id, free_item_qualname, impl_disc_for, impl_qualname, method_qualname,
    self_ty_name,
};
use crate::signature::{function_signature, struct_signature};
use crate::spans::{SourceRange, source_range_of};

/// Extract entities from one file's source.
///
/// `module_path` is the file-level dotted module (Task 2 output). Returns
/// wire-shaped entity `Value`s: a `file_scope` `module` for the file itself,
/// then every top-level / inline-`mod` `struct`, free `function`, and impl
/// method.
///
/// # Errors
///
/// Returns the [`syn::Error`] from [`syn::parse_file`] when `src` is not valid
/// Rust (the degraded-parse fallback wrapping this is Task 9). Also surfaces an
/// [`syn::Error`] if an assembled qualname fails [`build_entity_id`] validation.
pub fn extract_file(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
) -> Result<Vec<Value>, syn::Error> {
    // `crate_name` is already encoded into `module_path` (Task 2 builds the
    // dotted path crate-rooted). It stays in the public signature for Phase 1b
    // cross-crate edge resolution; extraction itself does not consult it.
    let _ = crate_name;
    let file = syn::parse_file(src)?;
    let mut out = Vec::new();
    // File-level module entity (file_scope; core emits its contains edge).
    out.push(entity(
        "module",
        module_path,
        file_path,
        &SourceRange {
            byte_start: 0,
            byte_end: i64::try_from(src.len()).unwrap_or(0),
            start_line: 1,
            end_line: i64::try_from(src.lines().count()).unwrap_or(1),
        },
        None,
        None,
    )?);
    let module_id = build_id("module", module_path)?;
    walk_items(&file.items, module_path, &module_id, file_path, &mut out)?;
    Ok(out)
}

fn walk_items(
    items: &[Item],
    module_path: &str,
    parent_id: &str,
    file_path: &str,
    out: &mut Vec<Value>,
) -> Result<(), syn::Error> {
    // Source-order ordinal for inherent impls of the same self-type, so
    // multiple inherent blocks get distinct keys without perturbing trait
    // impls (which carry no ordinal). Scoped to this item list.
    let mut inherent_ordinals: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for item in items {
        match item {
            Item::Fn(ItemFn { sig, .. }) => {
                let name = sig.ident.to_string();
                let q = free_item_qualname(module_path, &name);
                out.push(entity(
                    "function",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    Some(function_signature(sig)),
                )?);
            }
            Item::Struct(ItemStruct { ident, fields, .. }) => {
                let q = free_item_qualname(module_path, &ident.to_string());
                out.push(entity(
                    "struct",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    Some(struct_signature(fields)),
                )?);
            }
            Item::Impl(it) => {
                emit_impl_methods(it, module_path, file_path, &mut inherent_ordinals, out)?;
            }
            Item::Mod(ItemMod {
                ident,
                content: Some((_, inner)),
                ..
            }) => {
                let nested = format!("{module_path}.{ident}");
                out.push(entity(
                    "module",
                    &nested,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    None,
                )?);
                let nested_id = build_id("module", &nested)?;
                walk_items(inner, &nested, &nested_id, file_path, out)?;
            }
            _ => {} // const/static/enum/trait/etc. are Phase 1b
        }
    }
    Ok(())
}

fn emit_impl_methods(
    it: &ItemImpl,
    module_path: &str,
    file_path: &str,
    inherent_ordinals: &mut std::collections::BTreeMap<String, usize>,
    out: &mut Vec<Value>,
) -> Result<(), syn::Error> {
    // Type qualname for the impl's self type (simple path types in 1a; exotic
    // self types fall back to a textual rendering in `self_ty_name`).
    let type_q = format!("{module_path}.{}", self_ty_name(&it.self_ty));
    // Inherent impls take a per-self-type source-order ordinal; trait impls do
    // not consume an ordinal (their trait path already disambiguates them), so
    // reordering trait impls cannot perturb a later inherent ordinal.
    let ordinal = if it.trait_.is_none() {
        let slot = inherent_ordinals.entry(type_q.clone()).or_insert(0);
        let current = *slot;
        *slot += 1;
        current
    } else {
        0
    };
    let disc = impl_disc_for(it, ordinal);
    // The impl entity itself is Phase 1b; here it only parents its methods.
    let impl_id = build_id("function", &impl_qualname(&type_q, &disc))?;
    for member in &it.items {
        if let ImplItem::Fn(m) = member {
            let q = method_qualname(&type_q, &disc, &m.sig.ident.to_string());
            out.push(entity(
                "function",
                &q,
                file_path,
                &source_range_of(member),
                Some(impl_id.as_str()),
                Some(function_signature(&m.sig)),
            )?);
        }
    }
    Ok(())
}

/// Build an entity id string, mapping the [`EntityIdError`] into a
/// [`syn::Error`] so the extraction path has a single error type.
///
/// [`EntityIdError`]: loomweave_core::EntityIdError
fn build_id(kind: &str, qualname: &str) -> Result<String, syn::Error> {
    build_entity_id(kind, qualname)
        .map(|id| id.as_str().to_owned())
        .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e.to_string()))
}

fn entity(
    kind: &str,
    qualname: &str,
    file_path: &str,
    range: &SourceRange,
    parent_id: Option<&str>,
    signature: Option<Value>,
) -> Result<Value, syn::Error> {
    let id = build_id(kind, qualname)?;
    let mut e = json!({
        "id": id.as_str(),
        "kind": kind,
        "qualified_name": qualname,
        "source": {
            "file_path": file_path,
            "source_byte_start": range.byte_start,
            "source_byte_end": range.byte_end,
            "source_range": { "start_line": range.start_line, "end_line": range.end_line }
        }
    });
    if let Some(p) = parent_id {
        e["parent_id"] = json!(p);
    }
    if let Some(s) = signature {
        e["signature"] = s;
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(entities: &[Value]) -> Vec<String> {
        entities
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn extracts_module_struct_and_free_function() {
        let src = "pub struct Widget { a: i32 }\npub fn helper(x: i32) -> bool { x > 0 }\n";
        let out = extract_file(
            "loomweave_core",
            "loomweave_core.config",
            "/p/src/config.rs",
            src,
        )
        .unwrap();
        let got = ids(&out);
        assert!(got.contains(&"rust:module:loomweave_core.config".to_owned()));
        assert!(got.contains(&"rust:struct:loomweave_core.config.Widget".to_owned()));
        assert!(got.contains(&"rust:function:loomweave_core.config.helper".to_owned()));
    }

    #[test]
    fn trait_and_inherent_methods_are_distinct_functions() {
        let src = "struct Foo;\nimpl std::fmt::Display for Foo { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\nimpl std::fmt::Debug for Foo { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n";
        let out = extract_file("k", "k.m", "/p/src/m.rs", src).unwrap();
        let got = ids(&out);
        assert!(got.iter().any(|id| id.contains("Foo.impl[Display].fmt")));
        assert!(got.iter().any(|id| id.contains("Foo.impl[Debug].fmt")));
    }

    #[test]
    fn every_entity_carries_file_path_and_byte_range() {
        let src = "pub fn a() {}\n";
        let out = extract_file("k", "k.m", "/p/src/m.rs", src).unwrap();
        let f = out.iter().find(|e| e["kind"] == "function").unwrap();
        assert_eq!(f["source"]["file_path"], "/p/src/m.rs");
        assert!(f["source"]["source_byte_start"].as_i64().is_some());
        assert!(f["source"]["source_byte_end"].as_i64().unwrap() > 0);
    }
}
