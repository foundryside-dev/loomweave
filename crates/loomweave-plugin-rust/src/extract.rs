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
use loomweave_core::plugin::UnresolvedCallSite;
use serde_json::{Value, json};
use syn::{
    ImplItem, Item, ItemConst, ItemEnum, ItemFn, ItemImpl, ItemMacro, ItemMod, ItemStatic,
    ItemStruct, ItemTrait, ItemType, ItemUse, Meta, UseTree,
};

use crate::calls::walk_calls;
use crate::derives::derive_sites;
use crate::edges::{derives_edge, implements_edge, imports_edge, references_edge};
use crate::parse_guard::GuardViolation;
use crate::qualname::{
    build_entity_id, cfg_discriminant, declared_type_params, free_item_qualname, impl_disc_for,
    impl_disc_for_qualified, impl_qualname, self_ty_locator, self_ty_locator_qualified,
    self_ty_path_witness,
};
use crate::references::{
    ReferenceSite, ReferenceStats, block_reference_sites, expr_reference_sites,
    fields_reference_sites, signature_reference_sites, type_reference_sites,
};
use crate::resolve::{Resolution, Resolver};
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
    /// Call sites that produced NO `calls` edge — method calls, assoc/external
    /// path calls, and non-path call forms (Phase 2). Empty when no resolver is
    /// threaded (the [`extract_file_full`] contract), exactly as `edges` carries
    /// no `imports`/`calls`/`implements` without a resolver.
    pub unresolved_call_sites: Vec<UnresolvedCallSite>,
    /// Per-file `references` counters (Phase 2, D4). All-zero when no resolver
    /// is threaded — sites are not even collected without one, exactly as
    /// `unresolved_call_sites` stays empty.
    pub reference_stats: ReferenceStats,
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
    // No resolver → no `imports` edges (entities + structural `contains` only).
    // Keeps identity / uniqueness / symbol-table callers byte-identical.
    extract_file_inner(crate_name, module_path, file_path, src, None)
}

/// Edges-aware extraction (Phase 1b, Task 7): everything [`extract_file_full`]
/// emits, **plus** resolved `imports` edges. Each file-scope `use` leaf path is
/// resolved against the project symbol table through `resolver`:
/// - a unique in-project target → a `resolved` anchored `imports` edge,
/// - a glob / multi-kind candidate → an `ambiguous` anchored `imports` edge,
/// - an external (or unresolvable) path → NOTHING (D1: external dropped).
///
/// `crate_name` is the resolution origin (`from_crate`) — it is NOT the dotted
/// `module_path` (which already bakes the crate in). The `imports` edge's
/// `from_id` is the enclosing **module** entity (a file-scope `use` is a module
/// property); its byte span anchors the `use` statement.
///
/// # Errors
///
/// As [`extract_file_full`].
pub fn extract_file_with_edges(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
    resolver: &Resolver,
) -> Result<Extracted, syn::Error> {
    extract_file_inner(
        crate_name,
        module_path,
        file_path,
        src,
        Some((crate_name, resolver)),
    )
}

/// Shared extraction core. `resolution = None` skips `use`-edge resolution
/// entirely (the [`extract_file_full`] contract); `Some((from_crate, resolver))`
/// resolves each `use` leaf into an anchored `imports` edge.
///
/// Runs the syn parse AND the recursive AST walk on the pinned 16 MiB stack
/// ([`crate::parse_guard::with_pinned_stack`], ADR-050): syn has no recursion
/// limit, and `syn::File` is `!Send`, so the whole parse-and-consume pipeline
/// stays on the dedicated thread whose stack the scan caps were tuned against.
fn extract_file_inner(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
    resolution: Option<(&str, &Resolver)>,
) -> Result<Extracted, syn::Error> {
    crate::parse_guard::with_pinned_stack(|| {
        extract_file_on_pinned_stack(crate_name, module_path, file_path, src, resolution)
    })
}

/// The extraction body proper. MUST only be called from
/// [`extract_file_inner`]'s pinned-stack thread — calling it on an arbitrary
/// thread reintroduces the environment-dependent crash threshold.
fn extract_file_on_pinned_stack(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
    resolution: Option<(&str, &Resolver)>,
) -> Result<Extracted, syn::Error> {
    // `crate_name` is already encoded into `module_path` (Task 2 builds the
    // dotted path crate-rooted). Extraction of entities does not consult it; the
    // resolver path receives the origin crate via `resolution` instead.
    let _ = crate_name;
    let file = syn::parse_file(src)?;
    let mut entities = Vec::new();
    let mut edges = Vec::new();
    let mut acc = Phase2Acc::default();
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
        resolution,
        &mut entities,
        &mut edges,
        &mut acc,
    )?;
    Ok(Extracted {
        entities,
        edges,
        unresolved_call_sites: acc.call_sites,
        reference_stats: acc.ref_stats,
    })
}

/// File-scoped Phase 2 accumulator threaded through the item walk (one
/// instance per analysed file; the `walk_items` recursion into inline modules
/// shares it).
#[derive(Default)]
struct Phase2Acc {
    /// Call sites that produced NO `calls` edge (see [`Extracted`]).
    call_sites: Vec<UnresolvedCallSite>,
    /// `(from_id, to_id)` pairs already emitted as `references` edges in THIS
    /// file. The edge PK is `(kind, from_id, to_id)`, so duplicate sites would
    /// silently merge at the storage writer's `ON CONFLICT` upsert
    /// (last-write-wins on the span) — deduping here keeps the emitted edge
    /// SET exact and FIRST-span-wins deterministic.
    ref_dedup: std::collections::BTreeSet<(String, String)>,
    /// The three Rust-populated `references` counters (D4).
    ref_stats: ReferenceStats,
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

/// The wire-ready degraded-aware payload shared by the `*_degraded_aware*` /
/// guard-degraded entry points, in order:
/// `(entities, edges, unresolved_call_sites, reference_stats, findings)`.
pub type DegradedAware = (
    Vec<Value>,
    Vec<Value>,
    Vec<UnresolvedCallSite>,
    ReferenceStats,
    Vec<Value>,
);

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
/// Returns a [`DegradedAware`] tuple. The entities-only entry point never
/// resolves anything, so its `unresolved_call_sites` is always empty and its
/// `reference_stats` all-zero (parity with its empty edges).
///
/// [`AnalyzeFileFinding`]: loomweave_core::plugin::AnalyzeFileFinding
#[must_use]
pub fn extract_file_degraded_aware(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
) -> DegradedAware {
    degraded_aware(
        module_path,
        file_path,
        extract_file_full(crate_name, module_path, file_path, src),
    )
}

/// Edges-aware degraded wrapper (Task 7): like [`extract_file_degraded_aware`]
/// but resolves `use` paths into `imports` edges via `resolver` on a clean
/// parse. The degraded fallback is identical — a single `syntax_error` module
/// plus a Warning finding, no edges — because an unparseable file has no `use`
/// tree to resolve.
///
/// Returns a [`DegradedAware`] tuple.
#[must_use]
pub fn extract_file_degraded_aware_with_edges(
    crate_name: &str,
    module_path: &str,
    file_path: &str,
    src: &str,
    resolver: &Resolver,
) -> DegradedAware {
    degraded_aware(
        module_path,
        file_path,
        extract_file_with_edges(crate_name, module_path, file_path, src, resolver),
    )
}

/// Shape an extraction `Result` into the [`DegradedAware`] tuple: a clean
/// parse passes through with no findings; a parse error collapses to a single
/// `syntax_error` module entity plus one Warning finding and no edges / no
/// call sites / zero reference counters.
fn degraded_aware(
    module_path: &str,
    file_path: &str,
    extracted: Result<Extracted, syn::Error>,
) -> DegradedAware {
    match extracted {
        Ok(Extracted {
            entities,
            edges,
            unresolved_call_sites,
            reference_stats,
        }) => (
            entities,
            edges,
            unresolved_call_sites,
            reference_stats,
            Vec::new(),
        ),
        Err(e) => degraded_module_tuple(
            module_path,
            file_path,
            "syntax_error",
            "LMWV-RUST-SYNTAX-ERROR",
            &format!("syn could not parse {file_path}: {e}"),
        ),
    }
}

/// Degraded result for a file REJECTED by the pre-parse guards (ADR-050): the
/// same single-module-plus-one-warning shape as the syntax-error fallback, but
/// with `parse_status` `"depth_limit"` / `"file_too_large"` and subcode
/// `LMWV-RUST-DEPTH-LIMIT` / `LMWV-RUST-FILE-TOO-LARGE`. The message names the
/// measured depth / run / byte count and the cap it exceeded.
///
/// Returns a [`DegradedAware`] tuple — the same wire-ready shape as
/// [`extract_file_degraded_aware`].
#[must_use]
pub fn extract_file_guard_degraded(
    module_path: &str,
    file_path: &str,
    violation: &GuardViolation,
) -> DegradedAware {
    degraded_module_tuple(
        module_path,
        file_path,
        violation.parse_status(),
        violation.subcode(),
        &violation.message(file_path),
    )
}

/// The shared degraded shape: exactly one `module` entity flagged with
/// `parse_status`, no edges, no call sites, zero reference counters, one
/// Warning finding under `subcode`. Used by the syntax-error fallback and the
/// guard rejections.
fn degraded_module_tuple(
    module_path: &str,
    file_path: &str,
    parse_status: &str,
    subcode: &str,
    message: &str,
) -> DegradedAware {
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
        "parse_status": parse_status,
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
        "subcode": subcode,
        "severity": "warning",
        "message": message,
        "metadata": metadata
    });
    (
        vec![entity],
        Vec::new(),
        Vec::new(),
        ReferenceStats::default(),
        vec![finding],
    )
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
    resolution: Option<(&str, &Resolver)>,
    out: &mut Vec<Value>,
    edges: &mut Vec<Value>,
    acc: &mut Phase2Acc,
) -> Result<(), syn::Error> {
    // Impl entities already emitted in THIS item list, by full impl id. A
    // second source block with the same impl id (same type+sig+cfg) does NOT
    // re-emit the entity — it only appends its methods (the merge, ADR-049
    // amend, Option b). Scoped to this item list.
    let mut seen_impl_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // The residual-collision ladder for impl qualnames (ADR-049 Amendments
    // 1/5/6/7): one planning pre-pass per item list deciding, per impl, the
    // @cfg suffix (bare-key cfg twins), the stage-S self-type-path
    // qualification, and the stage-T trait-path qualification. Every consumer
    // — the method cfg-twin counter below and `emit_impl` — reads
    // [`ImplLadder::final_impl_qualname`], so the passes cannot diverge.
    let ladder = ImplLadder::build(items, module_path);
    // cfg-twin counter for METHODS (ADR-049 Amendment 5, clarion-dfeb905f46),
    // keyed on the FINAL impl qualname (post impl-level `@cfg` suffix, post
    // S/T qualification) + method name. Two methods that land on the SAME impl
    // entity with the SAME name are cfg twins: the impl-level `@cfg` cannot
    // split methods WITHIN one impl entity — a single block
    // (`impl Foo { #[cfg(unix)] fn go #[cfg(windows)] fn go }`) or several
    // blocks that MERGE under Option (b) — so each such method must carry its
    // OWN `@cfg(...)` suffix, exactly as free items and impl blocks do. Keying
    // on the FINAL impl_q (not the pre-cfg one) means impl-level cfg-twins —
    // which are already split into distinct impl entities — do NOT collect a
    // redundant method suffix, while a method-twin *inside* a cfg-twin block
    // still does; likewise S/T-split impls (now distinct entities) collect no
    // spurious method twins.
    let mut method_twin_counts: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    for it in impl_items(items) {
        let impl_q = ladder.final_impl_qualname(module_path, it);
        for member in &it.items {
            if let ImplItem::Fn(m) = member {
                *method_twin_counts
                    .entry((impl_q.clone(), m.sig.ident.to_string()))
                    .or_insert(0) += 1;
            }
        }
    }
    let method_is_cfg_twin = |impl_q: &str, name: &str| {
        method_twin_counts
            .get(&(impl_q.to_owned(), name.to_owned()))
            .copied()
            .unwrap_or(0)
            > 1
    };
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
            // BOTH module forms count (ADR-049 Amendment 8, the extract-side
            // symmetric half): an inline `mod n { … }` AND a declaration
            // `mod n;` (file-backed, possibly `#[path]`-mounted). A decl mod
            // never emits an entity in this walk — its file emits its own
            // module — but it must still make an identically-named inline
            // sibling a cfg twin, or the inline emission collides with the
            // file-backed module's id (`#[cfg(a)] mod m;` + `#[cfg(b)] mod
            // m { … }`: the inline must emit `…m@cfg(b)`).
            Item::Mod(ItemMod { ident, .. }) => Some(("module", ident.to_string())),
            Item::Enum(ItemEnum { ident, .. }) => Some(("enum", ident.to_string())),
            Item::Trait(ItemTrait { ident, .. }) => Some(("trait", ident.to_string())),
            Item::Type(ItemType { ident, .. }) => Some(("type_alias", ident.to_string())),
            // `const _` never becomes an entity (ADR-049 Amendment 9, see the
            // emission arm below), so it must not count toward — or trigger —
            // a `("const", name)` twin discriminant. Behaviorally inert (the
            // emission gate skips it before any suffix could apply); this
            // guard just keeps the counter honest.
            Item::Const(ItemConst { ident, .. }) if *ident != "_" => {
                Some(("const", ident.to_string()))
            }
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
            Item::Fn(ItemFn {
                sig, attrs, block, ..
            }) => {
                let name = sig.ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("function", &name)
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    q.push_str(&disc);
                }
                let child = entity(
                    "function",
                    &q,
                    file_path,
                    &source_range_of(item),
                    Some(parent_id),
                    Some(function_signature(sig)),
                )?;
                let fn_id = build_id("function", &q)?;
                push_with_contains(parent_id, child, out, edges);
                // Phase 2: walk the body for call sites, ONLY with a resolver
                // (the edges-aware entry point) — parity with `imports`. The
                // caller is this free fn; closures / nested fns are walked but
                // attributed to it (see `calls` module docs). The same gate
                // covers `references`: param/return type positions plus body
                // expression paths, all from this fn entity (D3).
                if let Some((from_crate, resolver)) = resolution {
                    walk_calls(
                        block,
                        &fn_id,
                        from_crate,
                        resolver,
                        edges,
                        &mut acc.call_sites,
                    );
                    let mut ref_sites = Vec::new();
                    signature_reference_sites(sig, &mut ref_sites);
                    block_reference_sites(block, &mut ref_sites);
                    emit_reference_edges(&ref_sites, &fn_id, from_crate, resolver, acc, edges);
                }
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
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    q.push_str(&disc);
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
                // Phase 2: anchored `derives` edges, ONLY with a resolver (the
                // edges-aware entry point) — parity with `imports`/`implements`.
                // Field TYPES additionally mint `references` sites from this
                // struct entity (D3); the derive list itself never does.
                if let Some((from_crate, resolver)) = resolution {
                    let struct_id = build_id("struct", &q)?;
                    emit_derive_edges(attrs, &struct_id, from_crate, resolver, edges);
                    let mut ref_sites = Vec::new();
                    fields_reference_sites(fields, &mut ref_sites);
                    emit_reference_edges(&ref_sites, &struct_id, from_crate, resolver, acc, edges);
                }
            }
            Item::Impl(it) => {
                emit_impl(
                    it,
                    module_path,
                    parent_id,
                    file_path,
                    &ladder,
                    &method_is_cfg_twin,
                    &mut seen_impl_ids,
                    resolution,
                    out,
                    edges,
                    acc,
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
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    nested.push_str(&disc);
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
                walk_items(
                    inner, &nested, &nested_id, file_path, resolution, out, edges, acc,
                )?;
            }
            // Phase 1b leaf kinds: free items riding the same qualname + entity +
            // contains pattern as `struct`/`function`, with `None` signature (no
            // signature builder yet — trait/impl SEI signatures are a later task).
            // Trait *bodies* are deliberately NOT walked here (matching 1a).
            Item::Enum(ItemEnum {
                ident,
                attrs,
                variants,
                ..
            }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("enum", &name)
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    q.push_str(&disc);
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
                // Phase 2: `derives` edges for enums too (structs + enums are
                // the only derive targets in the walk — no `Item::Union` arm).
                // Variant FIELD types mint `references` sites from the enum
                // entity (D3); variant discriminant expressions do not (out of
                // envelope — D3 lists variant field types only).
                if let Some((from_crate, resolver)) = resolution {
                    let enum_id = build_id("enum", &q)?;
                    emit_derive_edges(attrs, &enum_id, from_crate, resolver, edges);
                    let mut ref_sites = Vec::new();
                    for variant in variants {
                        fields_reference_sites(&variant.fields, &mut ref_sites);
                    }
                    emit_reference_edges(&ref_sites, &enum_id, from_crate, resolver, acc, edges);
                }
            }
            Item::Trait(ItemTrait { ident, attrs, .. }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("trait", &name)
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    q.push_str(&disc);
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
            Item::Type(ItemType {
                ident, attrs, ty, ..
            }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("type_alias", &name)
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    q.push_str(&disc);
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
                // Phase 2: the alias RHS is a type position — `references`
                // sites from the type_alias entity (D3), resolver-gated.
                if let Some((from_crate, resolver)) = resolution {
                    let alias_id = build_id("type_alias", &q)?;
                    let mut ref_sites = Vec::new();
                    type_reference_sites(ty, &mut ref_sites);
                    emit_reference_edges(&ref_sites, &alias_id, from_crate, resolver, acc, edges);
                }
            }
            Item::Const(ItemConst {
                ident,
                attrs,
                ty,
                expr,
                ..
            }) => {
                // An unnamed `const _` is NOT an entity (ADR-049 Amendment 9,
                // clarion-83870dc534): `_` is non-identifying — no cfg/ordinal/
                // content discriminant can rescue a repeated `_` without
                // churning SEI — and un-nameable, so nothing can ever target
                // it. The skip is total: no entity, no `contains` edge, no
                // Phase-2 `references` sites from its declared type or
                // initializer (a finding inside one attributes to the module).
                // Unconditional on the ident — twin-gating would make the
                // emitted set sibling-dependent. Module-level only by
                // construction: rustc rejects assoc-level `const _`, and `syn`
                // rejects `static _` at parse (the existing degrade path).
                if *ident == "_" {
                    continue;
                }
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("const", &name)
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    q.push_str(&disc);
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
                // Phase 2: declared type (type position) + initializer
                // (expression position) both mint `references` sites from the
                // const entity (D3), resolver-gated.
                if let Some((from_crate, resolver)) = resolution {
                    let const_id = build_id("const", &q)?;
                    let mut ref_sites = Vec::new();
                    type_reference_sites(ty, &mut ref_sites);
                    expr_reference_sites(expr, &mut ref_sites);
                    emit_reference_edges(&ref_sites, &const_id, from_crate, resolver, acc, edges);
                }
            }
            Item::Static(ItemStatic {
                ident,
                attrs,
                ty,
                expr,
                ..
            }) => {
                let name = ident.to_string();
                let mut q = free_item_qualname(module_path, &name);
                if is_cfg_twin("static", &name)
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    q.push_str(&disc);
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
                // Phase 2: same channel as `const` — declared type +
                // initializer, from the static entity (D3), resolver-gated.
                if let Some((from_crate, resolver)) = resolution {
                    let static_id = build_id("static", &q)?;
                    let mut ref_sites = Vec::new();
                    type_reference_sites(ty, &mut ref_sites);
                    expr_reference_sites(expr, &mut ref_sites);
                    emit_reference_edges(&ref_sites, &static_id, from_crate, resolver, acc, edges);
                }
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
                    && let Some(disc) = cfg_suffix(attrs)
                {
                    q.push_str(&disc);
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
            // `use` items resolve to anchored `imports` edges (Phase 1b, Task 7)
            // — ONLY when a resolver is threaded (the edges-aware entry point).
            // A `use` at item scope is a property of the enclosing module, so the
            // edge's `from_id` is `parent_id` (the module/file entity, never
            // `core:file:*`). The whole `use` statement's byte span anchors every
            // leaf edge it expands to.
            Item::Use(it) => {
                if let Some((from_crate, resolver)) = resolution {
                    emit_use_edges(it, from_crate, parent_id, resolver, edges);
                }
            }
            _ => {} // macro invocations / extern / etc. unmodelled.
        }
    }
    Ok(())
}

/// Resolve one `use` item into zero or more anchored `imports` edges.
///
/// The `use` tree is expanded to leaf paths (`use a::{b, c::d};` → `a::b`,
/// `a::c::d`; `use a::*;` → `a::*`; `use a::B as C;` → `a::B`, alias dropped),
/// each resolved against the project symbol table:
/// - [`Resolution::Resolved`] → a `resolved` `imports` edge to the unique id,
/// - [`Resolution::Ambiguous`] → an `ambiguous` `imports` edge to the candidate,
/// - [`Resolution::External`] → NOTHING (D1: external targets dropped).
///
/// Every emitted edge is anchored at the whole `use` statement's byte span
/// (`from = module entity`, `to = resolved id`).
fn emit_use_edges(
    it: &ItemUse,
    from_crate: &str,
    from_id: &str,
    resolver: &Resolver,
    edges: &mut Vec<Value>,
) {
    let span = source_range_of(it);
    let mut leaves = Vec::new();
    collect_use_leaves(&it.tree, "", &mut leaves);
    for leaf in leaves {
        let (to_id, confidence) = match resolver.resolve_use_path(from_crate, &leaf) {
            Resolution::Resolved(id) => (id, "resolved"),
            Resolution::Ambiguous(id) => (id, "ambiguous"),
            Resolution::External => continue,
        };
        edges.push(imports_edge(from_id, &to_id, confidence, &span));
    }
}

/// Flatten a [`syn::UseTree`] into `::`-joined leaf paths.
///
/// `prefix` is the accumulated `::`-joined path from the ancestors. `Path`
/// descends one segment; `Name`/`Rename` terminate a leaf (the rename alias is
/// dropped — resolution keys on the REAL imported path, per the resolver
/// contract); `Glob` terminates a `<prefix>::*` leaf (the resolver special-cases
/// the `::*` suffix); `Group` fans out to each branch sharing `prefix`.
///
/// `self` as a group leaf (`use a::b::{self, Display};` — the very common
/// "import the module itself plus some of its items" idiom) terminates the
/// `prefix` path UNCHANGED (`a::b`), not `a::b::self`: `self` here names the
/// enclosing module, and the resolver only special-cases a *leading* `self`.
/// Appending the literal segment would miss the table and silently drop the
/// module edge.
fn collect_use_leaves(tree: &UseTree, prefix: &str, out: &mut Vec<String>) {
    let joined = |seg: &str| {
        if prefix.is_empty() {
            seg.to_owned()
        } else {
            format!("{prefix}::{seg}")
        }
    };
    match tree {
        UseTree::Path(p) => {
            collect_use_leaves(&p.tree, &joined(&p.ident.to_string()), out);
        }
        // A `self` leaf names the enclosing module: emit `prefix` as-is. (Only
        // meaningful inside a `Group`; a bare `use self;` carries an empty
        // prefix and contributes nothing, which is correct.)
        UseTree::Name(n) if n.ident == "self" => {
            if !prefix.is_empty() {
                out.push(prefix.to_owned());
            }
        }
        UseTree::Name(n) => out.push(joined(&n.ident.to_string())),
        // `use a::B as C;` — resolve the REAL path `a::B`, ignore the alias `C`.
        UseTree::Rename(r) => out.push(joined(&r.ident.to_string())),
        // `use a::*;` — pass the `::*`-suffixed path; the resolver handles it.
        UseTree::Glob(_) => out.push(joined("*")),
        UseTree::Group(g) => {
            for branch in &g.items {
                collect_use_leaves(branch, prefix, out);
            }
        }
    }
}

/// The impl items of one item list, in source order (the ladder's extraction
/// unit — file-local by construction, see [`ImplLadder`]).
fn impl_items(items: &[Item]) -> impl Iterator<Item = &ItemImpl> {
    items.iter().filter_map(|item| match item {
        Item::Impl(it) => Some(it),
        _ => None,
    })
}

/// The ADR-049 **residual-collision ladder** for impl qualnames (Amendment 6,
/// normative ordering; Amendment 7 rides stage T), planned once per
/// extraction unit (one `walk_items` item list). An impl's final qualname is
/// decided in stages, each keyed on the PREVIOUS stage's output:
///
/// 1. **@cfg** (Amendments 1/5 machinery, unchanged): cfg-twin-ness is
///    computed on the BARE pre-cfg impl qualname, exactly as before
///    Amendment 6; twins with a `#[cfg]` append the `@cfg(...)` suffix.
///    Running @cfg FIRST is the no-churn invariant: already-@cfg-split twins
///    (including cross-path cfg twins like `#[cfg(unix)] impl T for a::X` /
///    `#[cfg(windows)] impl T for b::X`) land in distinct post-cfg groups,
///    so S/T stay cold and their ids are byte-identical to today's.
/// 2. **S** (Amendment 6, clarion-8ff7f233fa): impls grouped by POST-CFG
///    qualname; a group whose members carry ≥ 2 distinct written
///    self-type-path witnesses ([`self_ty_path_witness`]) re-renders every
///    `Type::Path` member's base fully path-qualified
///    ([`self_ty_locator_qualified`]: `{m}.a%3A%3AX.impl[T]`). Single-segment
///    members render byte-identically, so only multi-segment members move.
/// 3. **T** (Amendment 7, clarion-fa8bcf8731): impls grouped by POST-S
///    qualname; a trait-impl group with ≥ 2 distinct qualified trait
///    renderings ([`impl_disc_for_qualified`]) switches every member's
///    `impl[…]` fragment to the qualified rendering
///    (`impl[tokio%3A%3Aio%3A%3AAsyncRead]`). Inherent impls never fire T.
/// 4. **method-@cfg** (Amendment 5, unchanged mechanics): keyed on the FINAL
///    impl qualname this ladder produces — see `method_twin_counts` in
///    `walk_items`.
///
/// File-local grouping is complete for valid Rust: two impls sharing a bare
/// qualname share a module path, and one module path means one item list
/// (E0761 rejects a doubly-defined file module, E0428 a doubly-defined inline
/// one; `include!` is invisible to syn; `#[path]`-aliased files are the known
/// residual deferred to Amendment 8).
///
/// All BTree-backed for determinism; a source reorder of a twin pair yields
/// the identical fired sets and ids.
struct ImplLadder {
    /// Bare PRE-cfg impl-qualname counts (stage-1 cfg-twin detection input —
    /// the Amendment-1 machinery, unchanged).
    twin_counts: std::collections::BTreeMap<String, usize>,
    /// POST-CFG qualnames whose written self-type-path witness set has ≥ 2
    /// members (stage S fires).
    s_fired: std::collections::BTreeSet<String>,
    /// POST-S qualnames whose qualified-trait rendering set has ≥ 2 members
    /// (stage T fires).
    t_fired: std::collections::BTreeSet<String>,
}

/// The stages [`ImplLadder::qualname_at`] can stop after. `Final` is the
/// emitted qualname; the earlier rungs exist so the planning passes group on
/// EXACTLY the same prefix computation the final rendering uses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LadderStage {
    PostCfg,
    PostS,
    Final,
}

impl ImplLadder {
    /// Plan the ladder over one item list: count bare keys, then derive the
    /// stage-S witness groups (on post-cfg qualnames) and the stage-T
    /// rendering groups (on post-S qualnames). Each pass consults only the
    /// fields the previous passes filled, via [`Self::qualname_at`] — the
    /// same chain [`Self::final_impl_qualname`] walks.
    fn build(items: &[Item], module_path: &str) -> Self {
        let mut ladder = ImplLadder {
            twin_counts: std::collections::BTreeMap::new(),
            s_fired: std::collections::BTreeSet::new(),
            t_fired: std::collections::BTreeSet::new(),
        };
        for it in impl_items(items) {
            *ladder
                .twin_counts
                .entry(Self::bare_qualname(module_path, it))
                .or_insert(0) += 1;
        }
        // Stage-S planning: post-cfg groups → distinct written self-type
        // paths. Witnesses carry NO generic args, so Amendment-3-normalized
        // twins (`a::X<T>` vs `a::X<U>`) share one witness and stay merged.
        let mut witnesses: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for it in impl_items(items) {
            witnesses
                .entry(ladder.qualname_at(module_path, it, LadderStage::PostCfg))
                .or_default()
                .insert(self_ty_path_witness(&it.self_ty));
        }
        ladder.s_fired = fired_groups(witnesses);
        // Stage-T planning: post-S groups → distinct qualified trait
        // renderings. Inherent impls contribute none (they never fire T, and
        // an inherent `impl#<…>` qualname can never group with a trait
        // `impl[…]` one).
        let mut renderings: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for it in impl_items(items) {
            if it.trait_.is_none() {
                continue;
            }
            renderings
                .entry(ladder.qualname_at(module_path, it, LadderStage::PostS))
                .or_default()
                .insert(impl_disc_for_qualified(it).key());
        }
        ladder.t_fired = fired_groups(renderings);
        ladder
    }

    /// The bare (last-segment-base) self-type qualname prefix shared by
    /// [`Self::bare_qualname`] and the stage-S else-branch of
    /// [`Self::qualname_at`].
    fn bare_type_qualname(module_path: &str, it: &ItemImpl) -> String {
        format!(
            "{module_path}.{}",
            self_ty_locator(&it.self_ty, &declared_type_params(it))
        )
    }

    /// The bare pre-cfg impl qualname (the Amendment-1 twin key): self type
    /// INCLUDING its concrete generic args (ADR-049 §2 self-type-args
    /// amendment — `impl Foo<i32>` ≠ `impl Foo<u32>`), last-segment base,
    /// last-segment trait fragment, no `@cfg`.
    fn bare_qualname(module_path: &str, it: &ItemImpl) -> String {
        impl_qualname(
            &Self::bare_type_qualname(module_path, it),
            &impl_disc_for(it),
        )
    }

    /// The stage-1 `@cfg(...)` suffix, decided on the BARE pre-cfg key
    /// exactly as before Amendment 6. Applies to ANY cfg-gated twin impl,
    /// trait OR inherent (`#[cfg(unix)] impl Display for Foo` /
    /// `#[cfg(windows)] impl Display for Foo` share `Foo.impl[Display]`) —
    /// do NOT gate on `it.trait_.is_none()`.
    fn cfg_suffix_for(&self, bare: &str, it: &ItemImpl) -> Option<String> {
        if self.twin_counts.get(bare).copied().unwrap_or(0) > 1 {
            cfg_suffix(&it.attrs)
        } else {
            None
        }
    }

    /// One impl's qualname up to `stage` — the single computation every
    /// planning pass and every emission consumes, so they cannot diverge.
    fn qualname_at(&self, module_path: &str, it: &ItemImpl, stage: LadderStage) -> String {
        let declared = declared_type_params(it);
        let bare_type_q = Self::bare_type_qualname(module_path, it);
        let bare = Self::bare_qualname(module_path, it);
        let cfg = self.cfg_suffix_for(&bare, it);
        let post_cfg = with_suffix(&bare, cfg.as_deref());
        if stage == LadderStage::PostCfg {
            return post_cfg;
        }
        // Stage S: re-render the self-type base fully path-qualified. The
        // @cfg suffix decided above is kept verbatim — S changes the base
        // only.
        let type_q = if self.s_fired.contains(&post_cfg) {
            format!(
                "{module_path}.{}",
                self_ty_locator_qualified(&it.self_ty, &declared)
            )
        } else {
            bare_type_q
        };
        let post_s = with_suffix(&impl_qualname(&type_q, &impl_disc_for(it)), cfg.as_deref());
        if stage == LadderStage::PostS {
            return post_s;
        }
        // Stage T: switch the impl[…] fragment to the qualified trait
        // rendering. `impl_disc_for_qualified` is byte-identical to the base
        // form for inherent impls and single-segment trait paths.
        if self.t_fired.contains(&post_s) {
            with_suffix(
                &impl_qualname(&type_q, &impl_disc_for_qualified(it)),
                cfg.as_deref(),
            )
        } else {
            post_s
        }
    }

    /// The emitted impl qualname: bare key → @cfg → S → T.
    fn final_impl_qualname(&self, module_path: &str, it: &ItemImpl) -> String {
        self.qualname_at(module_path, it, LadderStage::Final)
    }
}

/// The group keys whose member set has ≥ 2 distinct entries — a fired ladder
/// stage. `BTree` in, `BTree` out: deterministic regardless of source order.
fn fired_groups(
    groups: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> std::collections::BTreeSet<String> {
    groups
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(key, _)| key)
        .collect()
}

/// Append an optional already-rendered suffix (the `@cfg(...)` discriminant).
fn with_suffix(q: &str, suffix: Option<&str>) -> String {
    match suffix {
        Some(s) => format!("{q}{s}"),
        None => q.to_owned(),
    }
}

// `ladder` is the per-item-list impl-qualname plan; `seen_impl_ids` is
// threaded so a second same-id block merges (entity emitted once, methods
// appended). Both are inherent to the merge contract.
#[allow(clippy::too_many_arguments)]
fn emit_impl(
    it: &ItemImpl,
    module_path: &str,
    module_id: &str,
    file_path: &str,
    ladder: &ImplLadder,
    method_is_cfg_twin: &dyn Fn(&str, &str) -> bool,
    seen_impl_ids: &mut std::collections::BTreeSet<String>,
    resolution: Option<(&str, &Resolver)>,
    out: &mut Vec<Value>,
    edges: &mut Vec<Value>,
    acc: &mut Phase2Acc,
) -> Result<(), syn::Error> {
    // The full ADR-049 impl qualname — bare key → @cfg → stage S → stage T —
    // comes from the per-item-list ladder plan, the same computation the
    // method cfg-twin pre-pass consumed, so the two cannot diverge. See
    // [`ImplLadder`] for the stage semantics.
    let impl_q = ladder.final_impl_qualname(module_path, it);
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
        // Anchored `implements` edge for a TRAIT impl (`impl Tr for Foo`), ONLY
        // when a resolver is threaded (the edges-aware entry point) and the
        // implemented trait resolves in-project. The edge anchors on the
        // implemented-TRAIT-PATH's span (the `Tr`), not the whole `impl` block.
        // `External` traits (`impl std::fmt::Display for Foo`) yield no edge —
        // dropped here at emit, the resolver's first line of defence; the host
        // seen-entity-set gate (Task 8) is the second. Emitted once per impl
        // entity (inside the seen-id guard): a merge twin shares the trait, so a
        // second edge would only redundantly upsert the same natural-PK row.
        //
        // A NEGATIVE impl (`impl !Trait for Foo`) asserts NON-implementation, so
        // it must NOT emit a (positive) `implements` edge. `it.trait_` is
        // `Some((Option<Bang>, Path, For))`; the `Bang` is `Some` for a negative
        // impl. Guard on `bang.is_none()` (the impl ENTITY + module->impl
        // `contains` edge above are still emitted; only the `implements` edge is
        // suppressed).
        if let (Some((from_crate, resolver)), Some((bang, trait_path, _))) =
            (resolution, it.trait_.as_ref())
            && bang.is_none()
            && let Some((to_id, confidence)) =
                match resolver.resolve_trait_path(from_crate, &trait_path_for_lookup(trait_path)) {
                    Resolution::Resolved(id) => Some((id, "resolved")),
                    Resolution::Ambiguous(id) => Some((id, "ambiguous")),
                    Resolution::External => None,
                }
        {
            let span = source_range_of(trait_path);
            edges.push(implements_edge(&impl_id, &to_id, confidence, &span));
        }
    }
    // Methods re-parent onto the impl entity (impl -> method), NOT the module.
    // The method qualname is built from the cfg-AUGMENTED `impl_q` (not from
    // `disc`, which no longer carries the cfg discriminant under Option (b)):
    // for a cfg-twin inherent impl, `disc.key()` is identical across twins, so
    // building from `disc` would collide both `go` methods on one locator. The
    // `@cfg` suffix lives in `impl_q`, so the method must inherit it from there.
    for member in &it.items {
        if let ImplItem::Fn(m) = member {
            // A cfg-gated twin method (same final impl entity, same name) carries
            // its own `@cfg(...)` suffix AFTER the method name — `…go@cfg(unix)` —
            // mirroring the free-item rule. Composes on top of any impl-level cfg
            // already in `impl_q` (ADR-049 Amendment 5, clarion-dfeb905f46).
            let mut q = format!("{impl_q}.{}", m.sig.ident);
            if method_is_cfg_twin(&impl_q, &m.sig.ident.to_string())
                && let Some(disc) = cfg_suffix(&m.attrs)
            {
                q.push_str(&disc);
            }
            let child = entity(
                "function",
                &q,
                file_path,
                &source_range_of(member),
                Some(&impl_id),
                Some(function_signature(&m.sig)),
            )?;
            let method_id = build_id("function", &q)?;
            push_with_contains(&impl_id, child, out, edges); // impl -> method
            // Phase 2: walk the method body for call sites, ONLY with a resolver.
            // The caller is this impl method id (NOT the impl or the module).
            // `references` ride the same gate and the same origin: the method's
            // param/return types + body expression paths all originate from the
            // METHOD entity id (D3). The impl header (trait path + self type)
            // is deliberately NOT walked — `implements` / the impl entity own it.
            if let Some((from_crate, resolver)) = resolution {
                walk_calls(
                    &m.block,
                    &method_id,
                    from_crate,
                    resolver,
                    edges,
                    &mut acc.call_sites,
                );
                let mut ref_sites = Vec::new();
                signature_reference_sites(&m.sig, &mut ref_sites);
                block_reference_sites(&m.block, &mut ref_sites);
                emit_reference_edges(&ref_sites, &method_id, from_crate, resolver, acc, edges);
            }
        }
    }
    Ok(())
}

/// Resolve every `#[derive(...)]` path on a struct/enum into an anchored
/// `derives` edge (Phase 2), exactly mirroring how the `Item::Impl` arm
/// consumes [`Resolver::resolve_trait_path`]: a unique in-project trait →
/// `resolved`, a multi-kind candidate → `ambiguous`, an `External` derive
/// (`Debug`, `serde::Serialize`, …) → NOTHING (D1: dropped at emit, no
/// counter — matching `implements`). Each edge anchors on ITS derive path
/// token's span (the `Pretty` in `#[derive(Debug, Pretty)]`), never the whole
/// attribute or item.
fn emit_derive_edges(
    attrs: &[syn::Attribute],
    from_id: &str,
    from_crate: &str,
    resolver: &Resolver,
    edges: &mut Vec<Value>,
) {
    for site in derive_sites(attrs) {
        let (to_id, confidence) = match resolver.resolve_trait_path(from_crate, &site.path) {
            Resolution::Resolved(id) => (id, "resolved"),
            Resolution::Ambiguous(id) => (id, "ambiguous"),
            Resolution::External => continue,
        };
        edges.push(derives_edge(from_id, &to_id, confidence, &site.span));
    }
}

/// Resolve the collected `references` sites of ONE entity (`from_id`) into
/// anchored `references` edges (D3/D4/D5), kind-unfiltered through
/// [`Resolver::resolve_use_path`]:
/// - [`Resolution::Resolved`] → a `resolved` edge,
/// - [`Resolution::Ambiguous`] → an `ambiguous` edge (never faked resolved),
/// - [`Resolution::External`] → NO edge, counted in
///   `skipped_external_total` — this absorbs both external-crate paths AND
///   no-match paths (syn cannot distinguish; see the `references` module docs).
///
/// Two emission-side drops (the site still COUNTS in the stats):
/// - **self-edge guard** — `to_id == from_id` (a type naming itself in its own
///   fields) is noise,
/// - **per-file dedup** — the edge PK is `(kind, from_id, to_id)`, so a repeat
///   `(from, to)` pair would silently merge at the writer's `ON CONFLICT`
///   upsert; dropping it here keeps FIRST-span-wins deterministic.
fn emit_reference_edges(
    sites: &[ReferenceSite],
    from_id: &str,
    from_crate: &str,
    resolver: &Resolver,
    acc: &mut Phase2Acc,
    edges: &mut Vec<Value>,
) {
    for site in sites {
        acc.ref_stats.sites_total += 1;
        let (to_id, confidence) = match resolver.resolve_use_path(from_crate, &site.path) {
            Resolution::Resolved(id) => (id, "resolved"),
            Resolution::Ambiguous(id) => (id, "ambiguous"),
            Resolution::External => {
                acc.ref_stats.skipped_external_total += 1;
                continue;
            }
        };
        acc.ref_stats.resolved_total += 1;
        if to_id == from_id {
            continue; // self-edge guard
        }
        if !acc.ref_dedup.insert((from_id.to_owned(), to_id.clone())) {
            continue; // per-file dedup, first-span-wins
        }
        edges.push(references_edge(from_id, &to_id, confidence, &site.span));
    }
}

/// The `::`-joined trait-path string the resolver looks up, with generic
/// arguments STRIPPED. The resolver lookup keys on the trait's qualname
/// (`crate.module.MyTrait`), and an in-project `trait MyTrait<T>` entity is keyed
/// on its bare ident (`impl_disc_for` takes `last.ident` and handles generic args
/// separately) — so `impl MyTrait<i32> for Foo` MUST resolve as `MyTrait`, not
/// `MyTrait<i32>` (which `normalize_path` would never match, silently dropping the
/// edge for every in-project generic trait). Joining the segment idents drops the
/// `<…>` arguments while preserving the `a::b::Tr` path shape. Leading
/// `crate`/`self`/`super` segments are kept verbatim for `normalize_path` to map.
/// Also reused by [`crate::derives::derive_sites`] to render derive paths.
pub(crate) fn trait_path_for_lookup(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
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

/// Extract the predicate of EVERY `#[cfg(...)]` attribute on an item, in source
/// order. Returns the raw token text of each predicate (e.g. `"unix"`,
/// `"any(unix, windows)"`); normalisation + reserved-char escaping + folding
/// into one stable suffix is [`cfg_discriminant`]'s job. `#[cfg_attr(...)]` and
/// other attributes are ignored — only literal `cfg` lists disambiguate a
/// path-sharing twin.
///
/// All cfgs are collected (not just the first): stacked twins like
/// `#[cfg(unix)] #[cfg(feature="a")]` vs `#[cfg(unix)] #[cfg(feature="b")]`
/// legally coexist and must get DISTINCT discriminants, so the whole set feeds
/// the discriminant (FINDING #5).
fn cfg_predicates(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter_map(|attr| {
            if let Meta::List(list) = &attr.meta
                && list.path.is_ident("cfg")
            {
                Some(list.tokens.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// The folded `@cfg(...)` discriminant suffix for an item, or `None` when the
/// item carries no `#[cfg(...)]`. Folds EVERY cfg (FINDING #5) and escapes
/// reserved entity-id chars (FINDING #6) via [`cfg_discriminant`]. Shared
/// with `mounts` (Amendment 8): a twin `#[path]` mount appends exactly this
/// suffix, so the mounted file's module id and an inline twin's id use one
/// rendering.
pub(crate) fn cfg_suffix(attrs: &[syn::Attribute]) -> Option<String> {
    let preds = cfg_predicates(attrs);
    if preds.is_empty() {
        None
    } else {
        Some(cfg_discriminant(&preds))
    }
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

    /// ADR-049 Amendment 8 (the extract-side symmetric half): an inline mod
    /// sharing its name with a file-backed `mod n;` declaration is a cfg twin
    /// — the decl form now counts toward module twin-ness, so the inline
    /// sibling carries its own `@cfg(...)` suffix. (The mounted FILE's
    /// `@cfg(a)` path is the mounts overlay's job; in tokio this arm changes
    /// nothing since the mount name `imp` ≠ the facade name `unix`.)
    #[test]
    fn inline_mod_twinned_with_a_decl_mod_gets_its_cfg_suffix() {
        let src = "#[path = \"x.rs\"]\n#[cfg(a)]\nmod m;\n\
                   #[cfg(b)]\nmod m {\n    pub fn f() {}\n}\n";
        let out = extract_file("k", "k.host", "/p/src/host.rs", src).unwrap();
        let got = ids(&out);
        assert!(
            got.contains(&"rust:module:k.host.m@cfg(b)".to_owned()),
            "inline twin must carry its @cfg suffix: {got:?}"
        );
        assert!(
            !got.contains(&"rust:module:k.host.m".to_owned()),
            "the bare path would collide with the file-backed twin: {got:?}"
        );
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
        let (entities, edges, sites, ref_stats, findings) =
            extract_file_degraded_aware("k", "k.m", "/p/src/m.rs", src);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0]["kind"], "module");
        assert_eq!(entities[0]["id"], "rust:module:k.m");
        assert_eq!(entities[0]["parse_status"], "syntax_error");
        assert!(edges.is_empty());
        assert!(sites.is_empty());
        assert_eq!(ref_stats, ReferenceStats::default());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["severity"], "warning");
    }

    #[test]
    fn valid_file_yields_entities_and_no_findings() {
        let src = "pub fn a() {}\n";
        let (entities, _edges, _sites, _ref_stats, findings) =
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
        let Extracted {
            entities, edges, ..
        } = extract_file_full("k", "k.m", "/p/src/m.rs", src).unwrap();

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
