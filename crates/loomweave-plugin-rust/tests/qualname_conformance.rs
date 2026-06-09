//! Cross-engine qualname conformance gate (Loomweave <-> Wardline).
//!
//! Drives the shared corpus `fixtures/qualnames_rust.json` through the actual
//! Rust extractor and asserts every emitted `(qualified_name, kind)` matches
//! the corpus `expected` BYTE-FOR-BYTE, and that `module_path_for` reproduces
//! each route. This is the Rust analogue of Wardline's
//! `tests/conformance/test_qualname_conformance.py`: Loomweave is the
//! authoritative producer for the Rust dialect (ADR-049), so the corpus
//! `expected` values are generated from THIS extractor, and Wardline vendors a
//! copy and reproduces them from its tree-sitter frontend. A divergence here
//! means the dialect drifted — fix the extractor or the fixture, never silently.

use loomweave_plugin_rust::extract::extract_file;
use loomweave_plugin_rust::module_path::module_path_for;
use serde_json::Value;
use std::path::Path;

fn corpus() -> Value {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/qualnames_rust.json"
    ))
    .expect("read fixtures/qualnames_rust.json");
    serde_json::from_str(&raw).expect("parse qualnames_rust.json")
}

/// The corpus `expected` shape: the full emission `[{qualname, kind}, ...]` in
/// source order, including the file-scope and nested `module` entities.
fn emitted_pairs(entities: &[Value]) -> Vec<(String, String)> {
    entities
        .iter()
        .map(|e| {
            (
                e["qualified_name"].as_str().unwrap().to_owned(),
                e["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn expected_pairs(case: &Value) -> Vec<(String, String)> {
    case["expected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["qualname"].as_str().unwrap().to_owned(),
                e["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn module_routes_match_byte_for_byte() {
    let doc = corpus();
    let routes = doc["module_route"].as_array().unwrap();
    assert!(
        routes.len() >= 6,
        "corpus must carry the route cases incl. lib/main/mod.rs + #[path] gap"
    );
    for route in routes {
        let got = module_path_for(
            route["crate"].as_str().unwrap(),
            Path::new(route["src_root"].as_str().unwrap()),
            Path::new(route["file"].as_str().unwrap()),
        );
        assert_eq!(
            got,
            route["expected_module"].as_str().unwrap(),
            "module_route case: {}",
            route["name"]
        );
    }
}

#[test]
fn entities_match_byte_for_byte() {
    let doc = corpus();
    let cases = doc["entities"].as_array().unwrap();
    assert!(
        cases.len() >= 14,
        "corpus must carry every ADR-049 dialect family (found {})",
        cases.len()
    );
    for case in cases {
        let entities = extract_file(
            case["crate"].as_str().unwrap(),
            case["module_path"].as_str().unwrap(),
            case["rel_path"].as_str().unwrap(),
            case["source"].as_str().unwrap(),
        )
        .unwrap_or_else(|e| panic!("extract_file failed for {}: {e}", case["name"]));
        assert_eq!(
            emitted_pairs(&entities),
            expected_pairs(case),
            "entities case: {}",
            case["name"]
        );
    }
}

/// Guard against a resync silently introducing an entity kind the contract has
/// not vetted (mirrors the Python parity test's kind guard).
#[test]
fn corpus_kinds_are_known() {
    let doc = corpus();
    let mut kinds = std::collections::BTreeSet::new();
    for case in doc["entities"].as_array().unwrap() {
        for e in case["expected"].as_array().unwrap() {
            kinds.insert(e["kind"].as_str().unwrap().to_owned());
        }
    }
    // The contract-vetted kind set: Phase 1a (module/struct/function) plus the
    // Phase 1b leaf kinds and the `impl` entity (Task 5, ADR-027 MINOR bump).
    // Mirrors plugin.toml `entity_kinds` exactly. An eleventh, unvetted kind
    // still trips the guard.
    let known: std::collections::BTreeSet<String> = [
        "module",
        "struct",
        "function",
        "enum",
        "trait",
        "type_alias",
        "const",
        "static",
        "macro",
        "impl",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert!(
        kinds.is_subset(&known),
        "unhandled entity kinds in corpus: {:?}",
        &kinds - &known
    );
}
