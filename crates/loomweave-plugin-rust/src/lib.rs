//! Rust language plugin — Phase 1a: identity foundation.
pub mod calls;
pub mod crate_roots;
pub mod derives;
pub mod edges;
pub mod extract;
pub mod module_path;
pub mod mounts;
pub mod parse_guard;
pub mod qualname;
pub mod references;
pub mod resolve;
pub mod root_tags;
pub mod scope;
pub mod serve;
pub mod signature;
pub mod spans;
pub mod symbol_table;

#[cfg(test)]
mod manifest_tests {
    #[test]
    fn manifest_parses_and_declares_rust_plugin() {
        let bytes = include_bytes!("../plugin.toml");
        let m = loomweave_core::plugin::parse_manifest(bytes).expect("manifest parses");
        assert_eq!(m.plugin.plugin_id, "rust");
        assert_eq!(m.plugin.language, "rust");
        assert!(m.ontology.entity_kinds.contains(&"struct".to_owned()));
    }
}
