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
        assert_eq!(m.ontology.ontology_version, "0.9.0");
        assert_eq!(
            m.ontology.classifier_tags,
            vec![
                "allow-dead-code",
                "cli-command",
                "entry-point",
                "exported-api",
                "framework-handler",
                "http-route",
                "test",
            ]
        );
    }

    #[test]
    fn packaged_manifest_is_byte_identical_to_the_canonical_manifest() {
        let canonical = include_bytes!("../plugin.toml");
        let packaged = include_bytes!(
            "../../../packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml"
        );

        assert_eq!(canonical, packaged);
    }
}
