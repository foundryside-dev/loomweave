//! ADR-049 qualname canonicalization (Tasks 3-5).
//!
//! The qualname is the SEI *locator*; it must be unique-within-a-run and
//! stable across benign edits.

use loomweave_core::{EntityId, EntityIdError, entity_id};
use syn::{GenericArgument, ItemImpl, PathArguments, Type};

/// The plugin id that prefixes every entity id this plugin emits (ADR-049).
pub const PLUGIN_ID: &str = "rust";

/// `<module-path>.<item-name>` for a free item (function, struct, enum, …).
#[must_use]
pub fn free_item_qualname(module_path: &str, item_name: &str) -> String {
    format!("{module_path}.{item_name}")
}

/// Assemble the three-segment entity id. The qualname must already be canonical.
///
/// # Errors
///
/// Propagates [`EntityIdError`] from [`entity_id()`] when `kind` violates the
/// ADR-022 grammar or `qualname` is empty / contains a reserved `:`.
pub fn build_entity_id(kind: &str, qualname: &str) -> Result<EntityId, EntityIdError> {
    entity_id(PLUGIN_ID, kind, qualname)
}

/// An impl block's stable discriminator (ADR-049 §2).
///
/// Trait impls key by `impl[<TraitPath-with-concrete-generics>]`; inherent
/// impls key by `impl#<positional-De-Bruijn-generic-signature>`. There is NO
/// source-order ordinal (ADR-049 amend, Option b): same-`(type, generic-sig,
/// cfg)` inherent impls share one key and are MERGED into one `impl` entity at
/// the extraction layer, so the discriminator is genuinely
/// source-order-independent (reorder-stable). cfg-twin inherent impls (same
/// type+sig, mutually-exclusive cfgs) are split by an `@cfg(...)` suffix
/// appended at the extraction layer, not by the discriminator.
pub enum ImplDisc {
    /// `impl[<trait-with-generics>]`
    Trait {
        /// The rendered trait path with any concrete generic arguments.
        rendered: String,
    },
    /// `impl#<positional-generics>` (no ordinal — merged at the entity layer).
    Inherent {
        /// Positional, De Bruijn-style rendering of the impl's generic params.
        positional_generics: String,
    },
}

impl ImplDisc {
    /// A trait impl discriminator. `generic_args` are the trait's *concrete*
    /// generic arguments (e.g. `["i32"]` for `impl From<i32>`); they are part
    /// of the key so `From<i32>` and `From<u32>` are distinct.
    #[must_use]
    pub fn trait_impl(trait_name: &str, generic_args: &[String]) -> Self {
        let rendered = if generic_args.is_empty() {
            trait_name.to_owned()
        } else {
            format!("{trait_name}<{}>", generic_args.join(","))
        };
        ImplDisc::Trait { rendered }
    }

    /// An inherent impl discriminator. `generic_param_names` are the impl's
    /// declared generic parameter names; they are rendered positionally
    /// (De Bruijn) so a rename (`<T>` → `<U>`) does not churn the key. There is
    /// no ordinal — same-signature inherent impls share this key and merge to
    /// one entity (ADR-049 amend, Option b).
    #[must_use]
    pub fn inherent(generic_param_names: &[String]) -> Self {
        let positional: Vec<String> = (0..generic_param_names.len())
            .map(|i| format!("${i}"))
            .collect();
        ImplDisc::Inherent {
            positional_generics: positional.join(","),
        }
    }

    /// The `impl[...]` / `impl#...` fragment (no leading type).
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            ImplDisc::Trait { rendered } => format!("impl[{rendered}]"),
            ImplDisc::Inherent {
                positional_generics,
            } => {
                format!("impl#<{positional_generics}>")
            }
        }
    }

    /// Test alias making the positional intent explicit.
    #[must_use]
    pub fn key_with_positional(&self) -> String {
        self.key()
    }
}

/// `<type-qualname>.<impl-disc>` — the impl entity's own qualname.
#[must_use]
pub fn impl_qualname(type_qualname: &str, disc: &ImplDisc) -> String {
    format!("{type_qualname}.{}", disc.key())
}

/// `<type-qualname>.<impl-disc>.<method>` — a method carries its impl's disc.
///
/// This is what makes `Display::fmt` and `Debug::fmt` on the same type
/// distinct locators (ADR-049 §2).
#[must_use]
pub fn method_qualname(type_qualname: &str, disc: &ImplDisc, method: &str) -> String {
    format!("{type_qualname}.{}.{method}", disc.key())
}

/// Render the discriminator for a syn impl block (ADR-049 §2).
///
/// Trait impls become [`ImplDisc::trait_impl`] keyed by the trait path's last
/// segment plus its concrete generic arguments (so `From<i32>` and `From<u32>`
/// differ); inherent impls become [`ImplDisc::inherent`] keyed by the impl's
/// declared generic-parameter count (positional, rename-stable). There is no
/// ordinal (ADR-049 amend, Option b): same-signature inherent blocks share the
/// key and merge to one entity; cfg-twins are split by an `@cfg(...)` suffix
/// appended by the caller (extract.rs), not here.
#[must_use]
pub fn impl_disc_for(it: &ItemImpl) -> ImplDisc {
    if let Some((_, trait_path, _)) = &it.trait_
        && let Some(last) = trait_path.segments.last()
    {
        let generic_args = trait_generic_args(&last.arguments);
        return ImplDisc::trait_impl(&last.ident.to_string(), &generic_args);
    }
    let param_names: Vec<String> = it
        .generics
        .type_params()
        .map(|p| p.ident.to_string())
        .collect();
    ImplDisc::inherent(&param_names)
}

/// Concrete type/const generic arguments of a trait path's final segment,
/// rendered textually. Lifetimes are skipped (not part of the locator).
fn trait_generic_args(args: &PathArguments) -> Vec<String> {
    let PathArguments::AngleBracketed(ab) = args else {
        return Vec::new();
    };
    ab.args
        .iter()
        .filter_map(|a| match a {
            GenericArgument::Type(ty) => Some(type_textual(ty)),
            GenericArgument::Const(expr) => {
                Some(quote::ToTokens::to_token_stream(expr).to_string())
            }
            _ => None,
        })
        .collect()
}

/// The locator-relevant name of an impl's self type: the last path segment for
/// a simple path type (`Foo` in `crate::m::Foo`), else a whitespace-stripped
/// textual rendering that is still deterministic for exotic self types.
#[must_use]
pub fn self_ty_name(ty: &Type) -> String {
    if let Type::Path(p) = ty
        && let Some(last) = p.path.segments.last()
    {
        return last.ident.to_string();
    }
    type_textual(ty)
}

/// Deterministic, whitespace-free textual rendering of any token-bearing node.
/// `to_token_stream` renders paths/types spaced (`std :: fmt :: Debug`); stripping
/// whitespace yields the conventional `std::fmt::Debug`. This is the crate's one
/// path/type normaliser (the `signature::tidy` helper only handles `param: type`
/// surfaces, not `::`).
fn strip_ws<T: quote::ToTokens>(t: &T) -> String {
    t.to_token_stream()
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Deterministic, whitespace-free textual rendering of a type.
fn type_textual(ty: &Type) -> String {
    strip_ws(ty)
}

/// Deterministic, whitespace-free textual rendering of a path (e.g. a supertrait
/// or implemented-trait path for SEI signatures).
#[must_use]
pub fn path_textual(path: &syn::Path) -> String {
    strip_ws(path)
}

/// Normalise a `#[cfg(<predicate>)]` predicate to a stable `@cfg(...)` suffix:
/// whitespace stripped, nested predicate arguments sorted. Applied only to an
/// item that shares a path with a sibling (extract.rs decides applicability),
/// closing the otherwise-guaranteed cfg-twin collision (ADR-049 §3).
#[must_use]
pub fn cfg_discriminant(predicate: &str) -> String {
    format!("@cfg({})", normalise_pred(predicate))
}

fn normalise_pred(p: &str) -> String {
    let s: String = p.chars().filter(|c| !c.is_whitespace()).collect();
    // Sort the args of any single `any(...)`/`all(...)` wrapper (1-level; the
    // common twin case). Deeper nesting falls back to the stripped string,
    // which is still deterministic.
    if let Some(inner) = s.strip_prefix("any(").and_then(|r| r.strip_suffix(')')) {
        let mut parts: Vec<&str> = inner.split(',').collect();
        parts.sort_unstable();
        return format!("any({})", parts.join(","));
    }
    if let Some(inner) = s.strip_prefix("all(").and_then(|r| r.strip_suffix(')')) {
        let mut parts: Vec<&str> = inner.split(',').collect();
        parts.sort_unstable();
        return format!("all({})", parts.join(","));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_function_and_struct_qualnames() {
        assert_eq!(
            free_item_qualname("loomweave_core.config", "helper"),
            "loomweave_core.config.helper"
        );
        assert_eq!(
            free_item_qualname("loomweave_core.config", "Widget"),
            "loomweave_core.config.Widget"
        );
    }

    #[test]
    fn cross_crate_same_module_item_are_distinct() {
        let a = free_item_qualname("loomweave_core.config", "X");
        let b = free_item_qualname("loomweave_cli.config", "X");
        assert_ne!(a, b);
    }

    #[test]
    fn builds_a_valid_entity_id() {
        let id = build_entity_id(
            "struct",
            &free_item_qualname("loomweave_core.config", "Widget"),
        )
        .unwrap();
        assert_eq!(id.as_str(), "rust:struct:loomweave_core.config.Widget");
    }
}

#[cfg(test)]
mod impl_tests {
    use super::*;

    #[test]
    fn trait_impl_methods_on_same_type_are_distinct() {
        let display_fmt = method_qualname("m.Foo", &ImplDisc::trait_impl("Display", &[]), "fmt");
        let debug_fmt = method_qualname("m.Foo", &ImplDisc::trait_impl("Debug", &[]), "fmt");
        assert_ne!(display_fmt, debug_fmt);
        assert_eq!(display_fmt, "m.Foo.impl[Display].fmt");
    }

    #[test]
    fn trait_generic_args_are_part_of_the_key() {
        let from_signed = ImplDisc::trait_impl("From", &["i32".to_owned()]).key();
        let from_unsigned = ImplDisc::trait_impl("From", &["u32".to_owned()]).key();
        assert_ne!(from_signed, from_unsigned);
        assert_eq!(from_signed, "impl[From<i32>]");
    }

    #[test]
    fn inherent_generic_param_rename_does_not_churn() {
        // impl<T> Foo<T> and impl<U> Foo<U> render identically (positional).
        let t = ImplDisc::inherent(&["T".to_owned()]).key_with_positional();
        let u = ImplDisc::inherent(&["U".to_owned()]).key_with_positional();
        assert_eq!(t, u);
    }

    #[test]
    fn same_signature_inherent_impls_share_one_key() {
        // ADR-049 amend (Option b): no ordinal — two non-generic inherent
        // blocks render the SAME key and are merged into one `impl` entity at
        // the extraction layer (see tests/impl_entity.rs).
        let a = ImplDisc::inherent(&[]).key_with_positional();
        let b = ImplDisc::inherent(&[]).key_with_positional();
        assert_eq!(a, b);
        assert_eq!(a, "impl#<>");
    }
}

#[cfg(test)]
mod cfg_tests {
    use super::*;

    #[test]
    fn normalises_a_cfg_predicate_deterministically() {
        assert_eq!(cfg_discriminant("unix"), "@cfg(unix)");
        // whitespace-stripped, args sorted
        assert_eq!(
            cfg_discriminant("any( windows , unix )"),
            "@cfg(any(unix,windows))"
        );
    }

    #[test]
    fn cfg_twins_get_distinct_qualnames() {
        let unix = format!("{}{}", "m.f", cfg_discriminant("unix"));
        let win = format!("{}{}", "m.f", cfg_discriminant("windows"));
        assert_ne!(unix, win);
    }
}

#[cfg(test)]
mod syn_disc_tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn self_ty_name_takes_the_last_path_segment() {
        let it: syn::ItemImpl = parse_quote!(impl crate::m::Foo { fn x(&self) {} });
        assert_eq!(self_ty_name(&it.self_ty), "Foo");
    }

    #[test]
    fn impl_disc_for_inherent_renders_positional_generics_no_ordinal() {
        let it: syn::ItemImpl = parse_quote!(
            impl<T> Foo<T> {
                fn m(&self) {}
            }
        );
        let disc = impl_disc_for(&it);
        assert_eq!(disc.key(), "impl#<$0>");
    }

    #[test]
    fn impl_disc_for_inherent_rename_is_stable_and_same_signature_blocks_share_one_key() {
        let a: syn::ItemImpl = parse_quote!(
            impl<T> Foo<T> {
                fn m(&self) {}
            }
        );
        let b: syn::ItemImpl = parse_quote!(
            impl<U> Foo<U> {
                fn m(&self) {}
            }
        );
        // Rename T -> U is a no-op (positional). Under Option (b) there is no
        // ordinal, so two same-signature inherent blocks share ONE key and are
        // merged at the entity layer (see tests/impl_entity.rs).
        assert_eq!(impl_disc_for(&a).key(), impl_disc_for(&b).key());
        assert_eq!(impl_disc_for(&a).key(), "impl#<$0>");
    }

    #[test]
    fn impl_disc_for_trait_keeps_trait_name_and_concrete_generic_args() {
        let display: syn::ItemImpl =
            parse_quote!(impl std::fmt::Display for Foo { fn fmt(&self) {} });
        assert_eq!(impl_disc_for(&display).key(), "impl[Display]");

        let from_signed: syn::ItemImpl = parse_quote!(impl From<i32> for Foo {});
        assert_eq!(impl_disc_for(&from_signed).key(), "impl[From<i32>]");
        let from_unsigned: syn::ItemImpl = parse_quote!(impl From<u32> for Foo {});
        assert_ne!(
            impl_disc_for(&from_signed).key(),
            impl_disc_for(&from_unsigned).key()
        );
    }
}
