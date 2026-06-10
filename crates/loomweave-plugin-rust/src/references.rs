//! Phase 2 — `references` sites: type-position paths + expression paths.
//!
//! Two [`syn::visit::Visit`] walkers enumerate every in-envelope path mention
//! (D3) as a [`ReferenceSite`]; resolution + emission live in the caller
//! (`extract.rs`), kind-unfiltered through `Resolver::resolve_use_path`.
//!
//! **IN — type positions** (the type walker): struct/enum-variant field types,
//! fn param + return types, type-alias RHS, const/static declared types. The
//! walker recurses into nested generic args: `Vec<MyType>` mints a site for
//! `MyType` AND one for `Vec` (the container resolves External and is
//! dropped + counted by the caller).
//!
//! **IN — expression positions** (the expression walker, over fn bodies and
//! const/static initializers): `Expr::Path` NOT in call-callee position, and
//! `Expr::Struct` literal paths. Method-call receivers are walked normally
//! (`CONFIG.get()` mints a site for `CONFIG`); a call's ARGS are walked even
//! when its callee is skipped.
//!
//! **OUT:** `use` statements (the imports channel owns them), call-callee
//! paths (the calls channel owns them), derive lists (derives), impl-header
//! trait path + self type (implements / the impl entity), generic
//! params/bounds/where-clauses, trait item bodies (never walked — same as
//! calls), macro bodies/arguments (spec §5 — `visit_macro` is a no-op),
//! `Self`/`self` keyword paths, and qself paths (`<Foo>::Out` names an
//! ASSOCIATED item — minting its bare post-qself segments would fabricate
//! wrong crate-root edges, H5; the qself TYPE `Foo` is still collected in
//! type position via descent).
//!
//! **Counter divergence from the Python plugin (D4):** syn is a parser, not a
//! type checker — unlike pyright it cannot distinguish "resolves to an
//! external crate" from "resolves to nothing", so
//! `references_skipped_external_total` absorbs BOTH outcomes. There is no
//! per-file site cap (pyright needs one because reference enumeration costs
//! LSP round-trips; a syn walk is already bounded by the parse), so
//! `references_skipped_cap_total` and `unresolved_reference_sites_total` stay
//! 0 for Rust.
use syn::visit::Visit;
use syn::{Block, Expr, ExprCall, ExprPath, ExprStruct, Fields, FnArg, Macro, ReturnType, Type};

use crate::extract::trait_path_for_lookup;
use crate::spans::{SourceRange, source_range_of};

/// A single reference site: the path rendered for resolver lookup (the same
/// `::`-joined, generic-stripped form the `implements`/`derives` resolution
/// uses) plus the span of that path token (ADR-026: the token IS the edge).
pub struct ReferenceSite {
    /// The `::`-joined lookup string for [`Resolver::resolve_use_path`].
    ///
    /// [`Resolver::resolve_use_path`]: crate::resolve::Resolver::resolve_use_path
    pub path: String,
    /// The path token's source span (NOT the enclosing item/expression).
    pub span: SourceRange,
}

/// The three Rust-populated per-file `references` counters (D4). The two
/// Python-only counters (`unresolved_reference_sites_total`,
/// `references_skipped_cap_total`) stay 0 — see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReferenceStats {
    /// Every collected site, pre-resolution.
    pub sites_total: u64,
    /// Sites whose resolution returned Resolved OR Ambiguous (≥1 in-project
    /// candidate) — including sites later dropped by the self-edge guard or
    /// the per-file dedup (counting is site-side, emission is edge-side).
    pub resolved_total: u64,
    /// Every `Resolution::External` outcome. Absorbs BOTH external-crate
    /// paths and no-match paths — syn cannot distinguish (module docs).
    pub skipped_external_total: u64,
}

/// Collect every type-position path inside `ty`, recursing into nested
/// generic args (`Vec<MyType>` → sites for `Vec` and `MyType`).
pub fn type_reference_sites(ty: &Type, out: &mut Vec<ReferenceSite>) {
    TypeRefVisitor { out }.visit_type(ty);
}

/// Collect the type-position sites of a fn signature: every typed param plus
/// the return type. Receivers (`&self`, `self: Box<Self>`) are skipped —
/// `Self` paths are out of envelope. Generic params/bounds/where-clauses are
/// deliberately NOT walked (D3 OUT).
pub fn signature_reference_sites(sig: &syn::Signature, out: &mut Vec<ReferenceSite>) {
    for input in &sig.inputs {
        if let FnArg::Typed(pat_ty) = input {
            type_reference_sites(&pat_ty.ty, out);
        }
    }
    if let ReturnType::Type(_, ty) = &sig.output {
        type_reference_sites(ty, out);
    }
}

/// Collect the field-type sites of a struct or one enum variant.
pub fn fields_reference_sites(fields: &Fields, out: &mut Vec<ReferenceSite>) {
    for field in fields {
        type_reference_sites(&field.ty, out);
    }
}

/// Collect the expression-path sites of one fn body block.
pub fn block_reference_sites(block: &Block, out: &mut Vec<ReferenceSite>) {
    ExprRefVisitor { out }.visit_block(block);
}

/// Collect the expression-path sites of one const/static initializer.
pub fn expr_reference_sites(expr: &Expr, out: &mut Vec<ReferenceSite>) {
    ExprRefVisitor { out }.visit_expr(expr);
}

/// Push one site for `path` unless it is a `Self`/`self` keyword path (D3
/// OUT: a `Self` mention inside an impl is the impl's own type — noise, and
/// unresolvable without the impl context anyway).
fn push_path_site(path: &syn::Path, out: &mut Vec<ReferenceSite>) {
    if path
        .segments
        .first()
        .is_some_and(|s| s.ident == "Self" || s.ident == "self")
    {
        return;
    }
    out.push(ReferenceSite {
        path: trait_path_for_lookup(path),
        span: source_range_of(path),
    });
}

/// Type-position walker: collects every nested [`syn::TypePath`]'s path.
struct TypeRefVisitor<'a> {
    out: &'a mut Vec<ReferenceSite>,
}

impl<'ast> Visit<'ast> for TypeRefVisitor<'_> {
    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        // A qself path (`<Foo>::Out`, `<Foo as Tr>::Out`) names an ASSOCIATED
        // type, never a free path: resolving its bare post-qself segments
        // would fabricate an edge to an unrelated same-named crate-root
        // entity via the bare-name fallback (H5). Skip the path; the descent
        // below still reaches the qself TYPE (`Foo`), which is a legitimate
        // type-position mention.
        if node.qself.is_none() {
            push_path_site(&node.path, self.out);
        }
        // Default descent reaches nested generic args (`Vec<MyType>` →
        // `MyType`) and any qself type (`<Foo as Tr>::Out` → `Foo`).
        syn::visit::visit_type_path(self, node);
    }

    fn visit_macro(&mut self, _node: &'ast Macro) {
        // Spec §5: macro bodies/arguments are opaque tokens — never walked.
    }
}

/// Expression walker for fn bodies and const/static initializers. Collects
/// `Expr::Path` (with the call-callee carve-out) and `Expr::Struct` literal
/// paths. Nested closures / nested `fn` items are descended WITHOUT becoming
/// new origins — every site inside the body is attributed to the one named
/// entity this walk was seeded with, exactly like the calls walk.
struct ExprRefVisitor<'a> {
    out: &'a mut Vec<ReferenceSite>,
}

impl<'ast> Visit<'ast> for ExprRefVisitor<'_> {
    fn visit_expr_path(&mut self, node: &'ast ExprPath) {
        // qself guard, as in the type walker: `<Foo>::LIMIT` names an
        // associated item — never mint its bare post-qself path (H5). The
        // qself TYPE is not collected here either (this walker only collects
        // whole expression paths; type mentions belong to the type walker).
        if node.qself.is_none() {
            push_path_site(&node.path, self.out);
        }
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        // The calls channel owns a `Path`-func callee — do NOT mint a
        // references site for it; the call's ARGS are still walked. Any other
        // func form (`(get_fn())(x)`, a field func, …) is walked normally —
        // its inner paths are plain expression mentions.
        if matches!(node.func.as_ref(), Expr::Path(_)) {
            for arg in &node.args {
                self.visit_expr(arg);
            }
        } else {
            syn::visit::visit_expr_call(self, node);
        }
    }

    fn visit_expr_struct(&mut self, node: &'ast ExprStruct) {
        // A struct literal's path (`Foo { a: 1 }`) is an explicit mention.
        push_path_site(&node.path, self.out);
        // Default descent walks the field values and the `..rest` expression
        // (the path is re-visited only as a bare `Path`, never re-collected).
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_macro(&mut self, _node: &'ast Macro) {
        // Spec §5: macro bodies/arguments are opaque tokens — never walked.
    }
}
