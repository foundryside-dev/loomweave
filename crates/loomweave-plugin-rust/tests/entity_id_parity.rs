//! Non-vacuous parity gate (review H1): the shared `fixtures/entity_id.json`
//! carries Rust rows that exercise the ADR-049 qualname scheme, and this test
//! asserts the host-side [`entity_id`] assembler reproduces each
//! `expected_entity_id` byte-for-byte. If the fixture and the assembler ever
//! disagree, the cross-artifact identity contract is broken.

use loomweave_core::entity_id;
use serde_json::Value;

#[test]
fn rust_rows_assemble_byte_for_byte() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/entity_id.json"
    ))
    .unwrap();
    let doc: Value = serde_json::from_str(&raw).unwrap();

    let mut rust_rows = 0_usize;
    for row in doc["entities"].as_array().unwrap() {
        if row["plugin_id"] != "rust" {
            continue;
        }
        rust_rows += 1;
        let id = entity_id(
            row["plugin_id"].as_str().unwrap(),
            row["kind"].as_str().unwrap(),
            row["canonical_qualified_name"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            id.as_str(),
            row["expected_entity_id"].as_str().unwrap(),
            "row: {}",
            row["description"]
        );
    }

    // Guard against a vacuous pass: the fixture must actually carry the
    // ADR-049 Rust rows this gate exists to check.
    assert!(
        rust_rows >= 4,
        "expected at least the four ADR-049 Rust rows, found {rust_rows}"
    );
}
