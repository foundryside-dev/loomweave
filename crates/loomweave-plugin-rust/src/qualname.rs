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
