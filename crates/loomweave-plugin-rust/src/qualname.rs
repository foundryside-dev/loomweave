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
/// This is the `impl[...]` / `impl#<...>` *fragment only* — it discriminates the
/// trait (with its concrete generics) and the inherent-impl declared-generic
/// signature. The impl's **self-type concrete generic arguments** (what makes
/// `Foo<i32>` and `Foo<u32>` distinct) are NOT here; they live in the type-name
/// prefix composed by [`self_ty_locator`] (`Foo<i32>.impl#<>` vs
/// `Foo<u32>.impl#<>`). The full impl locator is therefore
/// `<self_ty_locator>.<ImplDisc::key>[@cfg(...)]`.
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
    ImplDisc::inherent(&declared_type_params(it))
}

/// The impl's declared generic type-parameter names in source order
/// (`impl<T, U> Foo<…>` → `["T", "U"]`). Lifetimes and const generics are
/// excluded. Used both for the inherent `#<…>` signature and to recognise which
/// self-type generic arguments are the impl's own parameters (rendered
/// positionally by [`self_ty_locator`]) versus concrete instantiations.
#[must_use]
pub fn declared_type_params(it: &ItemImpl) -> Vec<String> {
    it.generics
        .type_params()
        .map(|p| p.ident.to_string())
        .collect()
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
            GenericArgument::Type(ty) => Some(render_concrete_arg(ty)),
            GenericArgument::Const(expr) => Some(render_concrete_arg(expr)),
            _ => None,
        })
        .collect()
}

/// The locator-relevant *bare* name of an impl's self type: the last path
/// segment for a simple path type (`Foo` in `crate::m::Foo`), else a
/// whitespace-stripped textual rendering that is still deterministic for exotic
/// self types. This drops any self-type generic arguments — used for the SEI
/// signature `target` field, NOT for the impl locator (see [`self_ty_locator`]).
#[must_use]
pub fn self_ty_name(ty: &Type) -> String {
    if let Type::Path(p) = ty
        && let Some(last) = p.path.segments.last()
    {
        return last.ident.to_string();
    }
    type_textual(ty)
}

/// The locator-relevant name of an impl's self type **including its concrete
/// generic arguments** (ADR-049 §2, self-type-generic-args amendment). This is
/// what makes `impl Foo<i32>` and `impl Foo<u32>` distinct impl entities:
/// without the self-type args both render bare `Foo`, so their impl keys
/// (`Foo.impl#<>` / `Foo.impl[Display]`) collide and `seen_impl_ids` would
/// spuriously merge them, silently overwriting a like-named method at the writer.
///
/// `declared_params` are the impl's own declared generic-parameter names
/// (`impl<T> Foo<T>` → `["T"]`). A self-type arg that is exactly one of those is
/// the impl's *own* type parameter, not a concrete instantiation, so it is
/// rendered **positionally** (`$N`, matching the De Bruijn scheme of the inherent
/// `#<…>` signature) to stay rename-stable: `impl<T> Foo<T>` and `impl<U> Foo<U>`
/// both render `Foo<$0>`. Concrete args (`i32`, `String`, a const, a nested
/// generic) render via `type_textual` (whitespace-free), so spacing matches
/// the rest of the dialect. Lifetimes and associated-type bindings are dropped
/// (not part of the locator). A non-`Type::Path` self type, or a path with no
/// angle-bracketed args, renders exactly as [`self_ty_name`] (bare).
#[must_use]
pub fn self_ty_locator(ty: &Type, declared_params: &[String]) -> String {
    let Type::Path(p) = ty else {
        return type_textual(ty);
    };
    let Some(last) = p.path.segments.last() else {
        return type_textual(ty);
    };
    let base = last.ident.to_string();
    let PathArguments::AngleBracketed(ab) = &last.arguments else {
        return base;
    };
    let rendered: Vec<String> = ab
        .args
        .iter()
        .filter_map(|a| match a {
            GenericArgument::Type(arg_ty) => Some(self_ty_arg(arg_ty, declared_params)),
            GenericArgument::Const(expr) => Some(render_concrete_arg(expr)),
            _ => None,
        })
        .collect();
    if rendered.is_empty() {
        base
    } else {
        format!("{base}<{}>", rendered.join(","))
    }
}

/// Render one self-type generic argument: a bare path matching a declared impl
/// type-parameter becomes its positional `$N` token (rename-stable); anything
/// else is rendered concretely and whitespace-free.
fn self_ty_arg(arg_ty: &Type, declared_params: &[String]) -> String {
    if let Type::Path(p) = arg_ty
        && p.qself.is_none()
        && p.path.segments.len() == 1
        && matches!(p.path.segments[0].arguments, PathArguments::None)
        && let Some(pos) = declared_params
            .iter()
            .position(|d| p.path.segments[0].ident == d.as_str())
    {
        return format!("${pos}");
    }
    render_concrete_arg(arg_ty)
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

/// Render one **concrete** generic argument (type or const) for an impl locator
/// through the shared ADR-049 Amendment-4 pipeline: `escape_reserved(strip_ws(arg))`.
///
/// Strip first (so const args lose proc-macro2 token spacing — `{ 1 + 2 }` →
/// `{1+2}`), then apply the same injective reserved-char escape used on the cfg
/// path (`%`→`%25` then `:`→`%3A`). Without the escape a `::`-path arg
/// (`From<std::io::Error>`, ubiquitous in real Rust) leaks a literal `:` into the
/// qualname; `build_entity_id` then rejects the id and the extractor collapses the
/// whole cleanly-parsed file to one `syntax_error` module (clarion-8245039f6b).
/// Distinct paths stay distinct (injective), and the cfg path already pins the
/// escape bytes, so a second producer reproduces this byte-for-byte.
fn render_concrete_arg<T: quote::ToTokens>(t: &T) -> String {
    escape_reserved(&strip_ws(t))
}

/// Deterministic, whitespace-free textual rendering of a path (e.g. a supertrait
/// or implemented-trait path for SEI signatures).
#[must_use]
pub fn path_textual(path: &syn::Path) -> String {
    strip_ws(path)
}

/// Normalise the FULL set of `#[cfg(<predicate>)]` attributes on an item into a
/// single stable `@cfg(...)` suffix: each predicate is individually
/// whitespace-stripped, its nested arguments sorted, and EVERY reserved
/// entity-id character escaped; the normalised predicates are then sorted and
/// joined so the suffix is order-independent (ADR-049 §3).
///
/// Two reasons this folds *all* cfgs rather than just the first:
///
/// - **Stacked-cfg twins** like `#[cfg(unix)] #[cfg(feature="a")]` vs
///   `#[cfg(unix)] #[cfg(feature="b")]` legally coexist; folding only the first
///   `#[cfg]` would hand both the same `@cfg(unix)` discriminant and collide one
///   away at the writer's `ON CONFLICT`. Folding every cfg keeps them distinct.
/// - **Reserved-char safety**: a predicate such as `feature = "a:b"` carries the
///   reserved `:` separator; flowed verbatim into a qualname it makes
///   `build_entity_id` reject the id and collapse the whole clean-parse file to a
///   single `syntax_error` module. `escape_reserved` guarantees the suffix can
///   never contain a reserved entity-id char.
///
/// Applied only to an item that shares a path with a sibling (extract.rs decides
/// applicability), closing the otherwise-guaranteed cfg-twin collision.
#[must_use]
pub fn cfg_discriminant(predicates: &[String]) -> String {
    let mut norm: Vec<String> = predicates.iter().map(|p| normalise_pred(p)).collect();
    norm.sort_unstable();
    format!("@cfg({})", norm.join("&"))
}

/// Escape every reserved entity-id character so a cfg predicate can never make
/// `build_entity_id` reject the assembled id (ADR-022 reserves exactly `:` in the
/// canonical-qualified-name segment; see `loomweave_core::entity_id`).
///
/// The escape is injective so distinct predicates stay distinct: `%` is the
/// escape introducer and is encoded FIRST (`%` → `%25`), then each reserved char
/// (`:` → `%3A`). Without the leading `%` pass a literal `%3A` in source would
/// alias a real escaped `:`; with it, `feature="a:b"` and the literal
/// `feature="a%3Ab"` produce distinct discriminants.
fn escape_reserved(s: &str) -> String {
    // Order matters: encode the introducer before any char it could introduce.
    s.replace('%', "%25").replace(':', "%3A")
}

fn normalise_pred(p: &str) -> String {
    let stripped: String = p.chars().filter(|c| !c.is_whitespace()).collect();
    let s = escape_reserved(&stripped);
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

    fn disc(preds: &[&str]) -> String {
        let owned: Vec<String> = preds.iter().map(|p| (*p).to_owned()).collect();
        cfg_discriminant(&owned)
    }

    #[test]
    fn normalises_a_cfg_predicate_deterministically() {
        assert_eq!(disc(&["unix"]), "@cfg(unix)");
        // whitespace-stripped, args sorted
        assert_eq!(disc(&["any( windows , unix )"]), "@cfg(any(unix,windows))");
    }

    #[test]
    fn cfg_twins_get_distinct_qualnames() {
        let unix = format!("{}{}", "m.f", disc(&["unix"]));
        let win = format!("{}{}", "m.f", disc(&["windows"]));
        assert_ne!(unix, win);
    }

    #[test]
    fn stacked_cfgs_are_folded_order_independently() {
        // FINDING #5: a stacked-cfg pair sharing a leading `#[cfg(unix)]` but
        // differing on the second cfg must NOT collide. The whole set is folded,
        // so the two discriminants differ.
        let a = disc(&["unix", "feature=\"a\""]);
        let b = disc(&["unix", "feature=\"b\""]);
        assert_ne!(a, b);
        // Order-independent: a source reorder yields the same discriminant.
        assert_eq!(
            disc(&["unix", "feature=\"a\""]),
            disc(&["feature=\"a\"", "unix"])
        );
    }

    #[test]
    fn reserved_char_in_predicate_is_escaped_and_injective() {
        // FINDING #6: a `:` in the predicate must be escaped so it never reaches
        // build_entity_id as a reserved char.
        let d = disc(&["feature=\"a:b\""]);
        assert!(!d.contains(':'), "discriminant must not contain a raw ':'");
        // Injective: `a:b` and a literal `a%3Ab` stay distinct (the escape
        // introducer `%` is itself encoded, so no aliasing).
        assert_ne!(disc(&["feature=\"a:b\""]), disc(&["feature=\"a%3Ab\""]));
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
    fn self_ty_locator_folds_concrete_args_and_distinguishes_instantiations() {
        let signed: syn::ItemImpl = parse_quote!(impl Foo<i32> { fn get(&self) {} });
        let unsigned: syn::ItemImpl = parse_quote!(impl Foo<u32> { fn get(&self) {} });
        let s = self_ty_locator(&signed.self_ty, &declared_type_params(&signed));
        let u = self_ty_locator(&unsigned.self_ty, &declared_type_params(&unsigned));
        assert_eq!(s, "Foo<i32>");
        assert_eq!(u, "Foo<u32>");
        assert_ne!(
            s, u,
            "distinct concrete self-type args must produce distinct keys"
        );
    }

    #[test]
    fn self_ty_locator_renders_declared_param_positionally_and_rename_stable() {
        // The self-type arg here is the impl's OWN declared param `T`/`U`, so it
        // renders positionally ($0) and a rename does not churn.
        let t: syn::ItemImpl = parse_quote!(
            impl<T> Foo<T> {
                fn get(&self) {}
            }
        );
        let u: syn::ItemImpl = parse_quote!(
            impl<U> Foo<U> {
                fn get(&self) {}
            }
        );
        let lt = self_ty_locator(&t.self_ty, &declared_type_params(&t));
        let lu = self_ty_locator(&u.self_ty, &declared_type_params(&u));
        assert_eq!(lt, "Foo<$0>");
        assert_eq!(
            lt, lu,
            "a generic-param rename must not churn the self-type locator"
        );
    }

    #[test]
    fn self_ty_locator_non_generic_self_renders_bare() {
        let it: syn::ItemImpl = parse_quote!(impl crate::m::Foo { fn x(&self) {} });
        assert_eq!(
            self_ty_locator(&it.self_ty, &declared_type_params(&it)),
            "Foo"
        );
    }

    #[test]
    fn self_ty_locator_mixes_declared_and_concrete_positionally() {
        // `impl<T> Foo<T, i32>`: declared `T` -> $0, concrete `i32` kept; the
        // positional rendering prevents collision with `impl Foo<i32>`.
        let it: syn::ItemImpl = parse_quote!(
            impl<T> Foo<T, i32> {
                fn get(&self) {}
            }
        );
        assert_eq!(
            self_ty_locator(&it.self_ty, &declared_type_params(&it)),
            "Foo<$0,i32>"
        );
    }

    #[test]
    fn self_ty_locator_renders_nested_declared_param_literally_not_positionally() {
        // A declared param NESTED inside another type arg (`Vec<T>`) is NOT
        // positionally substituted: `self_ty_arg`'s bare-path guard requires
        // `PathArguments::None`, which `Vec<T>` fails, so the whole arg falls to
        // `type_textual` (literal `T`). The prefix is `Foo<Vec<T>>`, never the
        // recursive `Foo<Vec<$0>>`. Pins the F2 nested-param rule (ADR-049 §2
        // rename-stability limitation) at the rendering layer; the end-to-end
        // cross-tool trip-wire is the `generic_self_nested_param` corpus row.
        let it: syn::ItemImpl = parse_quote!(
            impl<T> Foo<Vec<T>> {
                fn get(&self) {}
            }
        );
        assert_eq!(
            self_ty_locator(&it.self_ty, &declared_type_params(&it)),
            "Foo<Vec<T>>"
        );
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

    // --- ADR-049 Amendment 4: escape_reserved(strip_ws(arg)) for concrete generic
    // args (clarion-8245039f6b). A `::`-path arg leaking a literal `:` into the
    // qualname makes build_entity_id reject the id and degrade the whole file.

    #[test]
    fn path_typed_trait_generic_arg_escapes_reserved_colon() {
        let it: syn::ItemImpl = parse_quote!(impl From<std::io::Error> for Foo {});
        assert_eq!(
            impl_disc_for(&it).key(),
            "impl[From<std%3A%3Aio%3A%3AError>]"
        );
    }

    #[test]
    fn path_typed_self_ty_generic_arg_escapes_reserved_colon() {
        let it: syn::ItemImpl = parse_quote!(impl Foo<std::io::Error> { fn x(&self) {} });
        assert_eq!(
            self_ty_locator(&it.self_ty, &declared_type_params(&it)),
            "Foo<std%3A%3Aio%3A%3AError>"
        );
    }

    #[test]
    fn const_generic_arg_is_whitespace_stripped() {
        // GenericArgument::Const was the dialect's one whitespace-bearing render
        // (proc-macro2 token spacing). Amendment 4 routes it through strip_ws too.
        let it: syn::ItemImpl = parse_quote!(impl Foo<{ 1 + 2 }> { fn x(&self) {} });
        assert_eq!(
            self_ty_locator(&it.self_ty, &declared_type_params(&it)),
            "Foo<{1+2}>"
        );
    }

    #[test]
    fn const_generic_arg_path_const_escapes_reserved_colon() {
        // A path-typed const arg (`{ usize::MAX }`) must both strip and escape.
        let it: syn::ItemImpl = parse_quote!(impl Foo<{ usize::MAX }> { fn x(&self) {} });
        assert_eq!(
            self_ty_locator(&it.self_ty, &declared_type_params(&it)),
            "Foo<{usize%3A%3AMAX}>"
        );
    }
}
