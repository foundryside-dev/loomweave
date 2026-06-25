//! ADR-054 reachability-root tagging. Pure derivation of the
//! `exported-api` / `entry-point` / `test` / `allow-dead-code` root tags from a
//! `syn` item's visibility + attributes and the threaded module context. No
//! I/O, no resolver, no cross-file resolution (increment 1, clarion-05fdd0490e).
//!
//! Provenance lives in the tag value, mirroring ADR-053: `exported-api` is a
//! declared `pub` surface; `entry-point` / `test` are structural; the
//! lowest-confidence `allow-dead-code` is an explicit `#[allow(dead_code)]`
//! suppression. The engine (`loomweave-mcp`) unions them all into the dead-code
//! root set.

use std::collections::BTreeSet;

use syn::{Attribute, Meta, Visibility};

/// Module context threaded down the recursive item walk for tag derivation.
/// Four independent lexical facts about the current position — a context bag,
/// not a state machine; two-variant enums would obscure rather than clarify.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
pub struct TagCtx {
    /// Every enclosing module back to the crate root is `pub` — a precondition
    /// for `exported-api` (the visibility chain must reach the external surface).
    ancestors_all_pub: bool,
    /// An enclosing module carries `#[cfg(test)]` → everything inside is `test`.
    under_cfg_test: bool,
    /// The file routes to a `<crate>@bin(<name>)` target (ADR-049 / scope.rs):
    /// its `pub` items are internal, so `exported-api` is suppressed.
    in_bin_target: bool,
    /// The item is a direct child of the file root (where a bare `fn main` is
    /// the program entry; a nested `fn main` is just a function).
    at_file_top: bool,
}

impl TagCtx {
    /// The root context for a freshly-parsed file. A `@bin(` segment in the
    /// file's root `module_path` marks a binary target (ADR-049 / scope.rs).
    #[must_use]
    pub fn for_file(module_path: &str) -> Self {
        Self {
            ancestors_all_pub: true, // the crate root is the public boundary
            under_cfg_test: false,
            in_bin_target: module_path.contains("@bin("),
            at_file_top: true,
        }
    }

    /// The context for the body of an inline `mod` nested in this one.
    #[must_use]
    pub fn descend_into_mod(self, vis: &Visibility, attrs: &[Attribute]) -> Self {
        Self {
            ancestors_all_pub: self.ancestors_all_pub && is_unrestricted_pub(vis),
            under_cfg_test: self.under_cfg_test || has_cfg_test(attrs),
            in_bin_target: self.in_bin_target,
            at_file_top: false,
        }
    }
}

/// Reachability-root tags for a walked item, sorted + deduplicated (ADR-054).
///
/// * `is_public` — the item exposes external visibility: unrestricted `pub` for
///   value/type items, `#[macro_export]` for `macro_rules!` (macros carry no
///   [`Visibility`]).
/// * `is_fn` — `entry-point` applies only to functions.
/// * `name` — the item identifier (for the bare `fn main` entry rule).
#[must_use]
pub fn root_tags(
    name: &str,
    is_public: bool,
    is_fn: bool,
    attrs: &[Attribute],
    ctx: TagCtx,
) -> Vec<String> {
    let mut tags: BTreeSet<&'static str> = BTreeSet::new();
    if is_public && ctx.ancestors_all_pub && !ctx.in_bin_target {
        tags.insert("exported-api");
    }
    if ctx.under_cfg_test || has_test_attr(attrs) {
        tags.insert("test");
    }
    if is_fn && is_entry_point(name, attrs, ctx) {
        tags.insert("entry-point");
    }
    if has_allow_dead_code(attrs) {
        tags.insert("allow-dead-code");
    }
    tags.into_iter().map(str::to_owned).collect()
}

/// [`Visibility::Public`] only — `pub(crate)` / `pub(super)` / `pub(in ..)` are
/// [`Visibility::Restricted`] (intra-crate, not external API).
#[must_use]
pub fn is_unrestricted_pub(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

/// `#[macro_export]` — the only export marker for a `macro_rules!` item (macros
/// have no [`Visibility`]).
#[must_use]
pub fn has_macro_export(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("macro_export"))
}

/// `#[test]` / `#[bench]`, including last-segment variants like `#[tokio::test]`.
fn has_test_attr(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| last_segment_is(a, "test") || last_segment_is(a, "bench"))
}

/// A module-level `fn main`, an async-runtime entry attribute (`#[tokio::main]`
/// / `#[actix_web::main]` / `#[async_std::main]`, matched on the `main` last
/// segment), or an FFI export (`#[no_mangle]` / `#[export_name]`).
fn is_entry_point(name: &str, attrs: &[Attribute], ctx: TagCtx) -> bool {
    (ctx.at_file_top && name == "main")
        || attrs.iter().any(|a| {
            last_segment_is(a, "main")
                || a.path().is_ident("no_mangle")
                || a.path().is_ident("export_name")
        })
}

/// `#[allow(dead_code)]` or `#[expect(dead_code)]` — an explicit author keep
/// signal that suppresses rustc's own dead-code lint.
fn has_allow_dead_code(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        if let Meta::List(list) = &a.meta {
            (list.path.is_ident("allow") || list.path.is_ident("expect"))
                && list
                    .tokens
                    .to_string()
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|t| t == "dead_code")
        } else {
            false
        }
    })
}

/// `#[cfg(test)]` exactly (a bare `test` predicate). Compound forms like
/// `cfg(all(test, ..))` are out of increment-1 scope (fail-toward-live: a missed
/// cfg-test item is merely surveyed, never mis-rooted).
fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        if let Meta::List(list) = &a.meta {
            list.path.is_ident("cfg") && list.tokens.to_string().trim() == "test"
        } else {
            false
        }
    })
}

/// The attribute's final path segment equals `name` (so `#[test]` and
/// `#[tokio::test]` both match `"test"`).
fn last_segment_is(attr: &Attribute, name: &str) -> bool {
    attr.path().segments.last().is_some_and(|s| s.ident == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(src: &str) -> Vec<Attribute> {
        // Parse the attributes off a throwaway item.
        let item: syn::ItemFn = syn::parse_str(&format!("{src}\nfn f() {{}}")).unwrap();
        item.attrs
    }

    fn lib_ctx() -> TagCtx {
        TagCtx::for_file("k.m")
    }

    #[test]
    fn unrestricted_pub_only() {
        let pub_vis: Visibility = syn::parse_str("pub").unwrap();
        let crate_vis: Visibility = syn::parse_str("pub(crate)").unwrap();
        let inherited = Visibility::Inherited;
        assert!(is_unrestricted_pub(&pub_vis));
        assert!(!is_unrestricted_pub(&crate_vis));
        assert!(!is_unrestricted_pub(&inherited));
    }

    #[test]
    fn for_file_reads_bin_segment() {
        let lib = TagCtx::for_file("k.m");
        let bin = TagCtx::for_file("k@bin(k)");
        assert!(!lib.in_bin_target);
        assert!(bin.in_bin_target);
    }

    #[test]
    fn private_mod_breaks_pub_chain() {
        let descended = lib_ctx().descend_into_mod(&Visibility::Inherited, &[]);
        assert!(!descended.ancestors_all_pub);
        // a pub item under a private mod is NOT exported-api
        assert!(root_tags("x", true, false, &[], descended).is_empty());
    }

    #[test]
    fn pub_mod_preserves_pub_chain() {
        let pub_vis: Visibility = syn::parse_str("pub").unwrap();
        let descended = lib_ctx().descend_into_mod(&pub_vis, &[]);
        assert!(descended.ancestors_all_pub);
        assert_eq!(
            root_tags("x", true, false, &[], descended),
            ["exported-api"]
        );
    }

    #[test]
    fn allow_dead_code_detected_in_list() {
        assert!(has_allow_dead_code(&attrs("#[allow(unused, dead_code)]")));
        assert!(has_allow_dead_code(&attrs("#[expect(dead_code)]")));
        assert!(!has_allow_dead_code(&attrs("#[allow(unused)]")));
    }

    #[test]
    fn cfg_test_only_matches_bare_test() {
        assert!(has_cfg_test(&attrs("#[cfg(test)]")));
        assert!(!has_cfg_test(&attrs("#[cfg(feature = \"x\")]")));
    }
}
