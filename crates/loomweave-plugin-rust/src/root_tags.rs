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

/// actix-web / ntex / rocket route attribute macros (last-segment match). All
/// cross-crate collisions are benign — every match means an HTTP route — and
/// over-rooting is fail-toward-live, so a generic last-segment match is safe.
const HTTP_ROUTE_ATTRS: &[&str] = &[
    "get", "post", "put", "patch", "delete", "head", "options", "trace", "connect", "route",
];
/// clap (v3/v4) + structopt CLI command/arg derive macros (derive-list match).
/// Distinctive, derive-position-unambiguous names (collision-safe per the
/// framework-taxonomy survey, ADR-054 increment 2).
const CLI_COMMAND_DERIVES: &[&str] = &["Parser", "Subcommand", "Args", "ValueEnum", "StructOpt"];
/// pyo3 FFI host-export attributes (last-segment) — callable from a Python host,
/// so a genuine entry point from outside the Rust call graph. pyo3-unique names,
/// zero collision.
const PYO3_ENTRY_ATTRS: &[&str] = &["pyfunction", "pyfn", "pyclass", "pymodule"];
/// proc-macro entry points (bare single ident, never path-qualified) —
/// compiler-invoked, so reachability roots.
const PROC_MACRO_ATTRS: &[&str] = &["proc_macro", "proc_macro_derive", "proc_macro_attribute"];
/// std-replacement test runners beyond `#[test]`/`#[bench]` (last-segment).
const TEST_RUNNER_ATTRS: &[&str] = &["rstest", "test_case", "quickcheck"];

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

    /// The context for an `impl` block's methods. The pub-chain and cfg(test)
    /// ancestry are inherited from the enclosing module unchanged (an `impl`
    /// adds no visibility of its own); only `at_file_top` clears, so a method
    /// named `main` is never mistaken for the program entry (ADR-054 increment 2).
    #[must_use]
    pub fn descend_into_impl(self) -> Self {
        Self {
            at_file_top: false,
            ..self
        }
    }

    /// The export chain is satisfied independently of the lexical `pub` chain.
    /// `#[macro_export]` lifts a macro to the crate root regardless of how deeply
    /// it is nested in private modules, so its `exported-api` status must not be
    /// gated by `ancestors_all_pub` (the standard `mod macros { #[macro_export]
    /// … }` idiom would otherwise read as dead — under-rooting).
    #[must_use]
    pub fn with_export_chain_satisfied(self) -> Self {
        Self {
            ancestors_all_pub: true,
            ..self
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
    // entry-point: a bare module-level `fn main` (fns only), OR an entry
    // attribute (runtime entry / FFI host export / proc-macro) on any item.
    if (is_fn && ctx.at_file_top && name == "main") || has_entry_attr(attrs) {
        tags.insert("entry-point");
    }
    // Framework-dispatched handlers — reached by the framework, not by a static
    // caller, so roots regardless of visibility/crate-type. `framework-handler`
    // rides as the excluded-tag companion (mirroring the Python plugin); the
    // ROOT is `http-route` / `cli-command` (ADR-054 increment 2).
    if attr_last_seg_in(attrs, HTTP_ROUTE_ATTRS) {
        tags.insert("http-route");
        tags.insert("framework-handler");
    }
    if derive_last_seg_in(attrs, CLI_COMMAND_DERIVES) {
        tags.insert("cli-command");
        tags.insert("framework-handler");
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

/// `#[test]` / `#[bench]` (incl. last-segment variants like `#[tokio::test]`),
/// and the std-replacement test-runner attributes (`#[rstest]`, etc.).
fn has_test_attr(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| last_segment_is(a, "test") || last_segment_is(a, "bench"))
        || attr_last_seg_in(attrs, TEST_RUNNER_ATTRS)
}

/// An attribute that reaches an item from OUTSIDE the Rust call graph: an
/// async-runtime entry (`#[tokio::main]` / `#[actix_web::main]`, matched on the
/// `main` last segment), an FFI host export (pyo3, `#[no_mangle]` /
/// `#[export_name]`), or a compiler-invoked proc-macro entry point.
fn has_entry_attr(attrs: &[Attribute]) -> bool {
    attr_last_seg_in(attrs, &["main"])
        || attr_last_seg_in(attrs, PYO3_ENTRY_ATTRS)
        || attr_is_ident_in(attrs, &["no_mangle", "export_name"])
        || attr_is_ident_in(attrs, PROC_MACRO_ATTRS)
}

/// Any attribute whose final path segment is one of `names` (so `#[get]` and
/// `#[actix_web::get]` both match `"get"`).
fn attr_last_seg_in(attrs: &[Attribute], names: &[&str]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| names.iter().any(|n| s.ident == n))
    })
}

/// Any attribute whose path ident — after peeling a single edition-2024
/// `#[unsafe(<inner>)]` wrapper — is one of `names`. Covers both a bare
/// single-ident attribute (`#[proc_macro]`, never path-qualified) and the
/// unsafe-wrapped FFI exports: edition 2024 makes bare `#[no_mangle]` /
/// `#[export_name = "…"]` a hard error, so real code writes
/// `#[unsafe(no_mangle)]` / `#[unsafe(export_name = "…")]`, which syn parses as
/// `Meta::List { path: "unsafe", tokens: <inner attr> }` — the export ident
/// lives one level in. The inner may be a bare path (`no_mangle`) or a
/// name-value (`export_name = "…"`), so it is parsed as a full [`Meta`]. Missing
/// the wrapped form would under-root every edition-2024 FFI export (read dead).
fn attr_is_ident_in(attrs: &[Attribute], names: &[&str]) -> bool {
    attrs.iter().any(|a| {
        if names.iter().any(|n| a.path().is_ident(n)) {
            return true;
        }
        let Meta::List(list) = &a.meta else {
            return false;
        };
        if !list.path.is_ident("unsafe") {
            return false;
        }
        let Ok(inner) = syn::parse2::<Meta>(list.tokens.clone()) else {
            return false;
        };
        names.iter().any(|n| inner.path().is_ident(n))
    })
}

/// Any `#[derive(...)]` whose derive list contains a path with a final segment
/// (or any segment) in `names` — catches `#[derive(Parser)]` and
/// `#[derive(clap::Parser)]`. The `names` are distinctive enough that a
/// path-prefix token can never be one of them.
fn derive_last_seg_in(attrs: &[Attribute], names: &[&str]) -> bool {
    attrs.iter().any(|a| {
        if let Meta::List(list) = &a.meta {
            list.path.is_ident("derive")
                && list
                    .tokens
                    .to_string()
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|t| names.contains(&t))
        } else {
            false
        }
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
