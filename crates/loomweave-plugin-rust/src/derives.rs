//! `#[derive(...)]` invocation sites (Phase 2). Captures the *invocation*,
//! never the macro-generated impl body (spec §5) — resolution mirrors
//! `implements` (trait-filtered, externals dropped per D1).
use syn::punctuated::Punctuated;
use syn::{Attribute, Path, Token};

use crate::extract::trait_path_for_lookup;
use crate::spans::{SourceRange, source_range_of};

/// A single derive-path site: the path rendered for resolver lookup (the same
/// `::`-joined, generic-stripped form `implements` resolution uses) plus the
/// span of that path token inside the attribute list (ADR-026: the token IS
/// the edge).
pub struct DeriveSite {
    /// The `::`-joined lookup string for [`Resolver::resolve_trait_path`].
    ///
    /// [`Resolver::resolve_trait_path`]: crate::resolve::Resolver::resolve_trait_path
    pub path: String,
    /// The derive path token's source span (NOT the whole attribute/item).
    pub span: SourceRange,
}

/// Extract derive paths from an item's attributes. Non-`derive` attributes
/// and unparseable derive lists yield nothing (degrade silently — the file
/// already parsed; a malformed derive is the macro's problem, not ours).
#[must_use]
pub fn derive_sites(attrs: &[Attribute]) -> Vec<DeriveSite> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("derive") {
            continue;
        }
        let Ok(paths) = attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
        else {
            continue;
        };
        for p in &paths {
            out.push(DeriveSite {
                path: trait_path_for_lookup(p),
                span: source_range_of(p),
            });
        }
    }
    out
}
