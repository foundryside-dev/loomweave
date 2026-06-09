//! syn-based extraction of module/struct/function entities (Task 6).
//! ADR-038 SEI signatures (Task 8). Structural `contains` edges (Phase 1a
//! completion — ADR-026 dual-encoding).
//!
//! Parse one file with `syn`, walk top-level + inline-`mod` items, and emit
//! entity JSON `Value`s plus their `contains` edges, matching the wire
//! contract.
//!
//! **Containment (ADR-026 dual-encoding — every `parent_id` MUST have a
//! matching `contains` edge, or the storage writer fails the run):**
//! - `module` entities are `file_scope`: the **core** re-parents them to the
//!   file and emits the `file -> module` contains edge. The plugin must NOT
//!   emit a contains edge for a module.
//! - every non-`module` free child (`struct`, free `function`, leaf kinds) is
//!   parented to its enclosing **module** and the plugin emits the matching
//!   `module -> child` contains edge here.
//! - an `impl` entity is parented to the enclosing **module** (`module -> impl`
//!   contains), and each impl method is re-parented onto the **impl** entity
//!   (`impl -> method` contains), NOT the module (Task 5). Its locator already
//!   carries the impl discriminator, so the re-parent does not churn the id.
//!   Same-`(type, generic-sig, cfg)` inherent impls MERGE to one `impl` entity
//!   (no source-order ordinal — ADR-049 amend, Option b); cfg-twin impls
//!   (inherent OR trait) are split by an `@cfg(...)` suffix.
use serde_json::{Value, json};
use syn::{
    ImplItem, Item, ItemConst, ItemEnum, ItemFn, ItemImpl, ItemMacro, ItemMod, ItemStatic,
    ItemStruct, ItemTrait, ItemType, Meta,
};

use crate::qualname::{
    build_entity_id, cfg_discriminant, free_item_qualname, impl_disc_for, impl_qualname,
    self_ty_name,
};
use crate::signature::{function_signature, impl_signature, struct_signature};
use crate::spans::{SourceRange, source_range_of};

/// Entities and their structural `contains` edges extracted from one file.
pub struct Extracted {
    /// Wire-shaped entity `Value`s: a `file_scope` `module` for the file, then
    /// every top-level / inline-`mod` `struct`, free `function`, leaf item,
    /// `impl` entity, and impl method.
    pub entities: Vec<Value>,
    /// Wire-shaped `contains` edge `Value`s (`module -> non-module-child` and
    /// `impl -> method`). `module` children are excluded — the core emits their
    /// `file -> module` edge (see the module docs).
    pub edges: Vec<Value>,
}

/// Extract entities **and** their `contains` edges from one file's source.
///
/// `module_path` is the file-level dotted module (Task 2 output).
///
/// # Errors
///
/// Returns the [`syn::Error`] from [`syn::parse_file`] when `src` is not valid
/// Rust (the degraded-parse fallback wrapping this is Task 9). Also surfaces an
/// [`syn::Error`] if an assembled qualname fails [`build_entity_id`] validation.
pub fn extract_file_full(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
) -> Result<Extracted, syn::Error> {
    // `crate_name` is already encoded into `module_path` (Task 2 builds the
    // dotted path crate-rooted). It stays in the public signature for Phase 1b
    // cross-crate edge resolution; extraction itself does not consult it.
    let _ = crate_name;
    let file = syn::parse_file(src)?;
    let mut entities = Vec::new();
    let mut edges = Vec::new();
    // File-level module entity (file_scope; core emits its file->module edge).
    entities.push(entity(
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
    walk_items(
        &file.items,
        module_path,
        &module_id,
        file_path,
        &mut entities,
        &mut edges,
    )?;
    Ok(Extracted { entities, edges })
}

/// Entities-only extraction, for identity / uniqueness / symbol-table callers
/// that do not need edges. See [`extract_file_full`].
///
/// # Errors
///
/// As [`extract_file_full`].
pub fn extract_file(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
) -> Result<Vec<Value>, syn::Error> {
    extract_file_full(crate_name, module_path, file_path, src).map(|x| x.entities)
}

/// Extraction wrapper with degraded-parse fallback (review M3).
///
/// On a successful parse, returns the extracted entities, their `contains`
/// edges, and an empty finding list. On `syn::parse_file` failure (or a
/// qualname/id-validation error from [`extract_file_full`]), returns **exactly
/// one** `module` entity flagged `parse_status = "syntax_error"`, **no** edges,
/// plus a single Warning finding — never an empty entity list, never a panic.
/// The manifest declares the `syntax_degraded_module` role on `module`.
///
/// The returned finding `Value` carries the real [`AnalyzeFileFinding`] field
/// names (`subcode`/`severity`/`message`/`metadata`) so `main.rs` can
/// `serde_json::from_value` each one into the wire struct without remapping.
///
/// Returns `(entities, edges, findings)`.
///
/// [`AnalyzeFileFinding`]: loomweave_core::plugin::AnalyzeFileFinding
#[must_use]
pub fn extract_file_degraded_aware(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    match extract_file_full(crate_name, module_path, file_path, src) {
        Ok(Extracted { entities, edges }) => (entities, edges, Vec::new()),
        Err(e) => {
            // Best-effort id; if the module path itself is unrepresentable the
            // entity still carries the (empty) id and the qualified_name, which
            // is enough for the host to record a degraded module.
            let id = build_entity_id("module", module_path)
                .map(|i| i.as_str().to_owned())
                .unwrap_or_default();
            let entity = json!({
                "id": id,
                "kind": "module",
                "qualified_name": module_path,
                "parse_status": "syntax_error",
                "source": {
                    "file_path": file_path,
                    "source_byte_start": 0,
                    "source_byte_end": 0,
                    "source_range": { "start_line": 1, "end_line": 1 }
                }
            });
            let mut metadata = serde_json::Map::new();
            if !id.is_empty() {
                metadata.insert("entity_id".to_owned(), json!(id));
            }
            let finding = json!({
                "subcode": "LMWV-RUST-SYNTAX-ERROR",
                "severity": "warning",
                "message": format!("syn could not parse {file_path}: {e}"),
                "metadata": metadata
            });
            (vec![entity], Vec::new(), vec![finding])
        }
    }
}

// Length is arm count, not branching complexity: each leaf kind is one flat,
// near-identical dispatch arm over the item enum. Splitting it would obscure the
// one-arm-per-syn-Item structure the reader relies on.
#[allow(clippy::too_many_lines)]
fn walk_items(
    items: &[Item],
    module_path: &str,
    parent_id: &str,
    file_path: &str,
    out: &mut Vec<Value>,
    edges: &mut Vec<Value>,
) -> Result<(), syn::Error> {
    // Impl entities already emitted in THIS item list, by full impl id. A
    // second source block with the same impl id (same type+sig+cfg) does NOT
    // re-emit the entity — it only appends its methods (the merge, ADR-049
    // amend, Option b). Scoped to this item list.
    let mut seen_impl_ids: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    // cfg-twin counter for impls: keyed on the FULL pre-cfg impl qualname
    // (incl. the trait `[Trait]` fragment), so it splits cfg-twin inherent AND
    // trait impls (e.g. `#[cfg(unix)] impl Display for Foo` /
    // `#[cfg(windows)] impl Display for Foo` both render `Foo.impl[Display]`
    // and would dedup to one entity, silently dropping one `fmt`). `twin_counts`
    // above is keyed `(kind, name)` and cannot see impl qualnames, so impls need
    // their own pre-pass. Counting genuinely-same `(type, generic-sig)` blocks
    // >1 marks a twin; the `@cfg` suffix then re-splits the cfg variants.
    let mut impl_twin_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for item in items {
        if let Item::Impl(it) = item {
            let type_q = format!("{module_path}.{}", self_ty_name(&it.self_ty));
            let impl_q = impl_qualname(&type_q, &impl_disc_for(it));
            *impl_twin_counts.entry(impl_q).or_insert(0) += 1;
        }
    }
    let impl_is_cfg_twin =
        |impl_q: &str| impl_twin_counts.get(impl_q).copied().unwrap_or(0) > 1;
    // Named items sharing one (kind, name) in this item list are cfg twins
    // (`#[cfg(unix)] fn f` / `#[cfg(windows)] fn f`, and the same for a `struct`
    // or an inline `mod`): all cfg variants are visible (spec §5), so a bare path
    // collides — silent intra-run data loss at the writer's
    // `ON CONFLICT(id) DO UPDATE` (ADR-049 Context). Such siblings get a
    // normalised `@cfg(...)` discriminant (ADR-049 §3). Counting is per-kind
    // because the entity id's `kind` segment already separates `fn Foo` from
    // `struct Foo`; a unique (kind, name) keeps the bare path, so the common case
    // is undisturbed.
    let mut twin_counts: std::collections::BTreeMap<(&'static str, String), usize> =
        std::collections::BTreeMap::new();
    for item in items {
        let key = match item {
            Item::Fn(ItemFn { sig, .. }) => Some(("function", sig.ident.to_string())),
            Item::Struct(ItemStruct { ident, .. }) => Some(("struct", ident.to_string())),
            Item::Mod(ItemMod {
                ident,
                content: Some(_),
                ..
            }) => Some(("module", ident.to_string())),
            Item::Enum(ItemEnum { ident, .. }) => Some(("enum", ident.to_string())),
            Item::Trait(ItemTrait { ident, .. }) => Some(("trait", ident.to_string())),
            Item::Type(ItemType { ident, .. }) => Some(("type_alias", ident.to_string())),
            Item::Const(ItemConst { ident, .. }) => Some(("const", ident.to_string())),
            Item::Static(ItemStatic { ident, .. }) => Some(("static", ident.to_string())),
            Item::Macro(ItemMacro {
                ident: Some(ident), ..
            }) => Some(("macro", ident.to_string())),
            _ => None,
        };
        if let Some(k) = key {
            *twin_counts.entry(k).or_insert(0) += 1;
        }
    }
    // True when a (kind, name) is shared by a cfg-gated sibling in this list.
    let is_cfg_twin = |kind: &'static str, name: &str| {
        twin_counts
            .get(&(kind, name.to_owned()))
            .copied()
            .unwrap_or(0)
            > 1
    };
    for item in items {
        match item {
            Item::Fn(ItemFn { sig, attrs, .. }) => {
                let name = sig.ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("function", &name)
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    q.push_str(&cfg_discriminant(&pred));
                }
                let child = entity(
                    "function",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    Some(function_signature(sig)),
                )?;
                push_with_contains(parent_id, child, out, edges);
            }
            Item::Struct(ItemStruct {
                ident,
                fields,
                attrs,
                ..
            }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("struct", &name)
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    q.push_str(&cfg_discriminant(&pred));
                }
                let child = entity(
                    "struct",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    Some(struct_signature(fields)),
                )?;
                push_with_contains(parent_id, child, out, edges);
            }
            Item::Impl(it) => {
                emit_impl(
                    it,
                    module_path,
                    parent_id,
                    file_path,
                    &impl_is_cfg_twin,
                    &mut seen_impl_ids,
                    out,
                    edges,
                )?;
            }
            Item::Mod(ItemMod {
                ident,
                content: Some((_, inner)),
                attrs,
                ..
            }) => {
                // A nested `module` is `file_scope`: the core re-parents it to
                // the file and emits the `file -> module` contains edge, so the
                // plugin emits neither a parent_id nor a contains edge for it.
                let mut nested = format!("{module_path}.{ident}");
                if is_cfg_twin("module", &ident.to_string())
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    nested.push_str(&cfg_discriminant(&pred));
                }
                out.push(entity(
                    "module",
                    &nested,
                    file_path,
                    &source_range_of(item),
                    None,
                    None,
                )?);
                let nested_id = build_id("module", &nested)?;
                walk_items(inner, &nested, &nested_id, file_path, out, edges)?;
            }
            // Phase 1b leaf kinds: free items riding the same qualname + entity +
            // contains pattern as `struct`/`function`, with `None` signature (no
            // signature builder yet — trait/impl SEI signatures are a later task).
            // Trait *bodies* are deliberately NOT walked here (matching 1a).
            Item::Enum(ItemEnum { ident, attrs, .. }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("enum", &name)
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    q.push_str(&cfg_discriminant(&pred));
                }
                let child = entity(
                    "enum",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    None,
                )?;
                push_with_contains(parent_id, child, out, edges);
            }
            Item::Trait(ItemTrait { ident, attrs, .. }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("trait", &name)
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    q.push_str(&cfg_discriminant(&pred));
                }
                let child = entity(
                    "trait",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    None,
                )?;
                push_with_contains(parent_id, child, out, edges);
            }
            Item::Type(ItemType { ident, attrs, .. }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("type_alias", &name)
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    q.push_str(&cfg_discriminant(&pred));
                }
                let child = entity(
                    "type_alias",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    None,
                )?;
                push_with_contains(parent_id, child, out, edges);
            }
            Item::Const(ItemConst { ident, attrs, .. }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("const", &name)
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    q.push_str(&cfg_discriminant(&pred));
                }
                let child = entity(
                    "const",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    None,
                )?;
                push_with_contains(parent_id, child, out, edges);
            }
            Item::Static(ItemStatic { ident, attrs, .. }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("static", &name)
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    q.push_str(&cfg_discriminant(&pred));
                }
                let child = entity(
                    "static",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    None,
                )?;
                push_with_contains(parent_id, child, out, edges);
            }
            Item::Macro(ItemMacro {
                ident: Some(ident),
                attrs,
                ..
            }) => {
                // Only `macro_rules! name { .. }` (named) — bare macro
                // *invocations* (`foo!();`) carry `ident: None` and fall through.
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("macro", &name)
                    && let Some(pred) = cfg_predicate(attrs)
                {
                    q.push_str(&cfg_discriminant(&pred));
                }
                let child = entity(
                    "macro",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    None,
                )?;
                push_with_contains(parent_id, child, out, edges);
            }
            _ => {} // `impl` entity is Task 5; macro invocations / use / extern etc. unmodelled.
        }
    }
    Ok(())
}

// `impl_is_cfg_twin` is a borrowed closure (one per item list); `seen_impl_ids`
// is threaded so a second same-id block merges (entity emitted once, methods
// appended). Both are inherent to the merge contract.
#[allow(clippy::too_many_arguments)]
fn emit_impl(
    it: &ItemImpl,
    module_path: &str,
    module_id: &str,
    file_path: &str,
    impl_is_cfg_twin: &dyn Fn(&str) -> bool,
    seen_impl_ids: &mut std::collections::BTreeSet<String>,
    out: &mut Vec<Value>,
    edges: &mut Vec<Value>,
) -> Result<(), syn::Error> {
    // Type qualname for the impl's self type (simple path types in 1a; exotic
    // self types fall back to a textual rendering in `self_ty_name`).
    let type_q = format!("{module_path}.{}", self_ty_name(&it.self_ty));
    let disc = impl_disc_for(it); // ordinal-free (ADR-049 amend, Option b)
    let mut impl_q = impl_qualname(&type_q, &disc);
    // cfg-twin discriminant applies to ANY cfg-gated twin impl, trait OR
    // inherent. `#[cfg(unix)] impl Display for Foo` and
    // `#[cfg(windows)] impl Display for Foo` share `Foo.impl[Display]` and would
    // dedup to one entity, silently dropping one `fmt`. `impl_is_cfg_twin` keys
    // on the FULL pre-cfg `impl_q` (which includes `[Display]`), so it is
    // correct for trait impls too — do NOT gate on `it.trait_.is_none()`.
    if impl_is_cfg_twin(&impl_q)
        && let Some(pred) = cfg_predicate(&it.attrs)
    {
        impl_q.push_str(&cfg_discriminant(&pred));
    }
    let impl_id = build_id("impl", &impl_q)?;
    // First block with this id → emit the entity + the module->impl edge. A
    // second same-id block (the merge) skips this and only appends methods.
    if seen_impl_ids.insert(impl_id.clone()) {
        let e = entity(
            "impl",
            &impl_q,
            file_path,
            &source_range_of(it),
            Some(module_id),
            Some(impl_signature(it)),
        )?;
        edges.push(contains_edge(module_id, &impl_id)); // module -> impl
        out.push(e);
    }
    // Methods re-parent onto the impl entity (impl -> method), NOT the module.
    // The method qualname is built from the cfg-AUGMENTED `impl_q` (not from
    // `disc`, which no longer carries the cfg discriminant under Option (b)):
    // for a cfg-twin inherent impl, `disc.key()` is identical across twins, so
    // building from `disc` would collide both `go` methods on one locator. The
    // `@cfg` suffix lives in `impl_q`, so the method must inherit it from there.
    for member in &it.items {
        if let ImplItem::Fn(m) = member {
            let q = format!("{impl_q}.{}", m.sig.ident);
            let child = entity(
                "function",
                &q,
                file_path,
                &source_range_of(member),
                Some(&impl_id),
                Some(function_signature(&m.sig)),
            )?;
            push_with_contains(&impl_id, child, out, edges); // impl -> method
        }
    }
    Ok(())
}

/// Push a non-`module` child entity and its matching `module -> child`
/// `contains` edge (ADR-026 dual-encoding: a `parent_id` without a `contains`
/// edge fails the storage writer's consistency check). `child` MUST already
/// carry `parent_id == from_id`.
fn push_with_contains(from_id: &str, child: Value, out: &mut Vec<Value>, edges: &mut Vec<Value>) {
    if let Some(to_id) = child.get("id").and_then(Value::as_str) {
        edges.push(contains_edge(from_id, to_id));
    }
    out.push(child);
}

/// A structural `contains` edge. Per ADR-026 decision 3 a structural edge
/// carries NULL byte offsets (omitted here → wire default `None`); confidence
/// is `resolved` (the relationship is syntactically certain).
fn contains_edge(from_id: &str, to_id: &str) -> Value {
    json!({
        "kind": "contains",
        "from_id": from_id,
        "to_id": to_id,
        "confidence": "resolved"
    })
}

/// Extract the predicate of the first `#[cfg(...)]` attribute on an item, if
/// any. Returns the raw token text of the predicate (e.g. `"unix"`,
/// `"any(unix, windows)"`); normalisation into a stable suffix is
/// [`cfg_discriminant`]'s job. `#[cfg_attr(...)]` and other attributes are
/// ignored — only a literal `cfg` list disambiguates a path-sharing twin.
fn cfg_predicate(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if let Meta::List(list) = &attr.meta
            && list.path.is_ident("cfg")
        {
            Some(list.tokens.to_string())
        } else {
            None
        }
    })
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

    #[test]
    fn malformed_file_yields_one_degraded_module_and_a_warning() {
        let src = "fn broken( {{{ this is not rust";
        let (entities, edges, findings) =
            extract_file_degraded_aware("k", "k.m", "/p/src/m.rs", src);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0]["kind"], "module");
        assert_eq!(entities[0]["id"], "rust:module:k.m");
        assert_eq!(entities[0]["parse_status"], "syntax_error");
        assert!(edges.is_empty());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["severity"], "warning");
    }

    #[test]
    fn valid_file_yields_entities_and_no_findings() {
        let src = "pub fn a() {}\n";
        let (entities, _edges, findings) =
            extract_file_degraded_aware("k", "k.m", "/p/src/m.rs", src);
        assert!(findings.is_empty());
        assert!(entities.iter().any(|e| e["kind"] == "function"));
    }

    /// ADR-026 dual-encoding, mirroring the storage writer's two-direction
    /// `parent_contains_mismatch` check (`writer.rs:1252`): emitting a
    /// `parent_id` without a matching `contains` edge — the bug this fix closes
    /// — would `FailRun`. Every non-`module` entity with a `parent_id` must have a
    /// `contains` edge `(parent_id -> id)`, and every `contains` edge must have a
    /// child whose `parent_id` equals its `from_id`. `module` entities are
    /// excluded: they are `file_scope`, so the core supplies their
    /// `file -> module` edge, not the plugin.
    #[test]
    fn parent_contains_dual_encoding_holds() {
        let src = "pub struct Foo { a: i32 }\n\
                   pub fn free() {}\n\
                   impl Foo { pub fn make() -> Foo { Foo { a: 0 } } }\n\
                   impl std::fmt::Display for Foo { fn fmt(&self, _f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n\
                   pub mod inner { pub struct Bar; }\n";
        let Extracted { entities, edges } =
            extract_file_full("k", "k.m", "/p/src/m.rs", src).unwrap();

        // Index the contains edges by (from, to).
        let contains: std::collections::BTreeSet<(String, String)> = edges
            .iter()
            .filter(|e| e["kind"] == "contains")
            .map(|e| {
                (
                    e["from_id"].as_str().unwrap().to_owned(),
                    e["to_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let id_to_parent: std::collections::BTreeMap<String, Option<String>> = entities
            .iter()
            .map(|e| {
                (
                    e["id"].as_str().unwrap().to_owned(),
                    e.get("parent_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                )
            })
            .collect();

        // Direction 1: every non-module entity with a parent_id has a contains.
        for e in &entities {
            if e["kind"] == "module" {
                continue;
            }
            if let Some(parent) = e.get("parent_id").and_then(Value::as_str) {
                let id = e["id"].as_str().unwrap();
                assert!(
                    contains.contains(&(parent.to_owned(), id.to_owned())),
                    "entity {id} has parent_id={parent} but no matching contains edge",
                );
            }
        }
        // Direction 2: every contains edge has a child whose parent_id == from.
        for (from, to) in &contains {
            assert_eq!(
                id_to_parent.get(to).and_then(Option::as_deref),
                Some(from.as_str()),
                "contains ({from} -> {to}) has no matching child parent_id",
            );
        }
        // And the fix is non-vacuous: the impl method is present and parented.
        assert!(
            entities.iter().any(|e| e["id"]
                .as_str()
                .is_some_and(|id| id.contains("Foo.impl") && id.ends_with("make"))),
            "expected the impl method entity",
        );
    }
}
