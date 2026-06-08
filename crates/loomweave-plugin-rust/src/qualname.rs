//! ADR-049 qualname canonicalization (Tasks 3-5).
//!
//! The qualname is the SEI *locator*; it must be unique-within-a-run and
//! stable across benign edits.

use loomweave_core::{EntityId, EntityIdError, entity_id};

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
/// Propagates [`EntityIdError`] from [`entity_id`] when `kind` violates the
/// ADR-022 grammar or `qualname` is empty / contains a reserved `:`.
pub fn build_entity_id(kind: &str, qualname: &str) -> Result<EntityId, EntityIdError> {
    entity_id(PLUGIN_ID, kind, qualname)
}

/// An impl block's stable discriminator (ADR-049 §2).
///
/// Trait impls key by `impl[<TraitPath-with-concrete-generics>]`; inherent
/// impls key by `impl#<positional-De-Bruijn-generic-signature>` plus a stable
/// ordinal that disambiguates multiple inherent blocks for the same type.
pub enum ImplDisc {
    /// `impl[<trait-with-generics>]`
    Trait {
        /// The rendered trait path with any concrete generic arguments.
        rendered: String,
    },
    /// `impl#<positional-generics>` with a stable ordinal for ties.
    Inherent {
        /// Positional, De Bruijn-style rendering of the impl's generic params.
        positional_generics: String,
        /// Source-order ordinal disambiguating same-signature inherent blocks.
        ordinal: usize,
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
    /// (De Bruijn) so a rename (`<T>` → `<U>`) does not churn the key.
    #[must_use]
    pub fn inherent(generic_param_names: &[String], ordinal: usize) -> Self {
        let positional: Vec<String> = (0..generic_param_names.len())
            .map(|i| format!("${i}"))
            .collect();
        ImplDisc::Inherent {
            positional_generics: positional.join(","),
            ordinal,
        }
    }

    /// The `impl[...]` / `impl#...` fragment (no leading type).
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            ImplDisc::Trait { rendered } => format!("impl[{rendered}]"),
            ImplDisc::Inherent {
                positional_generics,
                ordinal,
            } => {
                format!("impl#<{positional_generics}>#{ordinal}")
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
        let t = ImplDisc::inherent(&["T".to_owned()], /*ordinal*/ 0).key_with_positional();
        let u = ImplDisc::inherent(&["U".to_owned()], 0).key_with_positional();
        assert_eq!(t, u);
    }

    #[test]
    fn multiple_inherent_impls_get_distinct_ordinals() {
        let a = ImplDisc::inherent(&[], 0).key_with_positional();
        let b = ImplDisc::inherent(&[], 1).key_with_positional();
        assert_ne!(a, b);
    }
}
