//! Phase 2 — call-site extraction over a named function's body.
//!
//! Walks the body expression tree of ONE enclosing named function (a free
//! `Item::Fn` or an impl `ImplItem::Fn`) with a [`syn::visit::Visit`] visitor,
//! classifying every call expression:
//!
//! 1. `ExprCall` whose `func` is an `Expr::Path` → resolve the dotted path via
//!    [`Resolver::resolve_call_path`]. `Resolved`/`Ambiguous` → a `calls` edge;
//!    `External` (incl. assoc `Foo::new()` and out-of-project paths) → an
//!    [`UnresolvedCallSite`] (no fabrication).
//! 2. `ExprMethodCall` (`x.foo()`) → never an edge (no receiver type without
//!    inference) → an [`UnresolvedCallSite`] (`callee_expr = ".foo"`).
//! 3. Any other call form (non-path `ExprCall` func) → an [`UnresolvedCallSite`].
//!
//! **Attribution (deliberate, matching the dialect's "closures & nested fns are
//! not entities"):** the visitor descends through nested closures and nested
//! `fn` items WITHOUT treating them as new callers — every call site textually
//! inside the named function's body is attributed to that one named function.
//! We do NOT override `visit_item_fn` / `visit_expr_closure`, so the default
//! `syn::visit` descent walks them, and their inner calls land on the nearest
//! enclosing NAMED function (the one this visitor was seeded with).
//!
//! `site_ordinal` is a single per-CALLER monotonic counter incremented at every
//! call site in source (visit) order — both edge-emitting and unresolved sites
//! advance it; an unresolved site records its position. This keeps ordinals
//! deterministic and per-caller (the counter is fresh per visitor instance).
use loomweave_core::plugin::UnresolvedCallSite;
use serde_json::Value;
use syn::visit::Visit;
use syn::{Block, Expr, ExprCall, ExprMethodCall};

use crate::edges::calls_edge;
use crate::resolve::{Resolution, Resolver};
use crate::spans::{SourceRange, source_range_of};

/// Bound on a recorded `callee_expr` string so a pathological call expression
/// cannot blow the unresolved-site payload (mirrors the host's field caps).
const CALLEE_EXPR_MAX: usize = 256;

/// Walk one named function's `block`, attributing every call site to
/// `caller_id`. Appends resolved/ambiguous `calls` edges to `edges` and every
/// unresolved call site to `sites`. `from_crate` is the resolution origin.
pub fn walk_calls(
    block: &Block,
    caller_id: &str,
    from_crate: &str,
    resolver: &Resolver,
    edges: &mut Vec<Value>,
    sites: &mut Vec<UnresolvedCallSite>,
) {
    let mut v = CallVisitor {
        caller_id,
        from_crate,
        resolver,
        edges,
        sites,
        next_ordinal: 0,
    };
    v.visit_block(block);
}

struct CallVisitor<'a> {
    caller_id: &'a str,
    from_crate: &'a str,
    resolver: &'a Resolver<'a>,
    edges: &'a mut Vec<Value>,
    sites: &'a mut Vec<UnresolvedCallSite>,
    /// Per-caller monotonic site counter (see module docs).
    next_ordinal: i64,
}

impl CallVisitor<'_> {
    /// Take and advance the per-caller ordinal.
    fn take_ordinal(&mut self) -> i64 {
        let o = self.next_ordinal;
        self.next_ordinal += 1;
        o
    }

    /// Record an unresolved call site at `span` with a bounded `callee_expr`.
    fn record_site(&mut self, callee_expr: String, span: &SourceRange) {
        let ordinal = self.take_ordinal();
        let mut callee_expr = callee_expr;
        if callee_expr.len() > CALLEE_EXPR_MAX {
            // Truncate on a CHAR boundary: `String::truncate` panics on a byte
            // index mid-codepoint, and a Unicode identifier/path can straddle
            // byte CALLEE_EXPR_MAX. Walk back to the nearest boundary.
            let mut end = CALLEE_EXPR_MAX;
            while !callee_expr.is_char_boundary(end) {
                end -= 1;
            }
            callee_expr.truncate(end);
        }
        self.sites.push(UnresolvedCallSite {
            caller_entity_id: self.caller_id.to_owned(),
            site_ordinal: ordinal,
            source_byte_start: span.byte_start,
            source_byte_end: span.byte_end,
            callee_expr,
        });
    }
}

impl<'ast> Visit<'ast> for CallVisitor<'_> {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        let span = source_range_of(node);
        // A `Path`-func call is the only resolvable form. Everything else
        // (`(get_fn())(x)`, a field/index/paren func, etc.) is unresolvable.
        if let Expr::Path(p) = node.func.as_ref() {
            let lookup = path_lookup_string(&p.path);
            match self.resolver.resolve_call_path(self.from_crate, &lookup) {
                Resolution::Resolved(id) => {
                    let _ = self.take_ordinal();
                    self.edges
                        .push(calls_edge(self.caller_id, &id, "resolved", &span));
                }
                Resolution::Ambiguous(id) => {
                    let _ = self.take_ordinal();
                    self.edges
                        .push(calls_edge(self.caller_id, &id, "ambiguous", &span));
                }
                Resolution::External => {
                    let printed = path_display_string(&p.path);
                    self.record_site(printed, &span);
                }
            }
        } else {
            self.record_site("<expr>()".to_owned(), &span);
        }
        // Descend into args (and into the func expr) so nested calls — incl.
        // `f(g())` and calls inside closure/nested-fn bodies — are all counted
        // and attributed to THIS named caller.
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        // No receiver type without inference → never an edge. Record the method
        // name (dotted, e.g. `.foo`) as the callee_expr.
        let span = source_range_of(node);
        self.record_site(format!(".{}", node.method), &span);
        // Descend into the receiver + args so chained / nested calls count too.
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// The `::`-joined lookup string the resolver normalises, with generic
/// arguments STRIPPED — mirrors `extract::trait_path_for_lookup`. Joining the
/// segment idents drops `<…>` turbofish while preserving the `a::b::f` shape;
/// leading `crate`/`self`/`super` segments are kept verbatim for
/// `normalize_path` to map.
fn path_lookup_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// A short human-readable printed form of a call path for `callee_expr`
/// (the lookup string is reused — it is already the dotted-ident path).
fn path_display_string(path: &syn::Path) -> String {
    path_lookup_string(path)
}
