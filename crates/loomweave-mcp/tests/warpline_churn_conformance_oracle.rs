//! Warpline -> Loomweave GV-LW-2 churn-count conformance oracle.
//!
//! Loomweave consumes Warpline's `warpline_entity_churn_count_get`
//! (`warpline.entity_churn_count.v1`) to power `entity_high_churn_list` and
//! `entity_recent_change_list`. The companion `warpline_churn_consumer` test
//! proves Loomweave's user-facing behaviour. This oracle pins the producer side:
//! the Warpline vector index and tool inventory are vendored byte-for-byte, the
//! GV-LW-2 full envelope is parsed by Loomweave's real consumer parser, and a
//! sibling-repo recheck compares the vendored authority files back to Warpline
//! when that repo is available.

use std::path::{Path, PathBuf};
use std::process::Command;

use loomweave_mcp::warpline::{
    WARPLINE_CHURN_SCHEMA, WARPLINE_CHURN_TOOL, parse_churn_count_response,
};
use serde_json::Value;

const GOLDEN_VECTORS: &str =
    include_str!("../../../docs/federation/fixtures/warpline-golden-vectors.json");
const MCP_TOOL_INVENTORY: &str =
    include_str!("../../../docs/federation/fixtures/warpline-mcp-tool-inventory.json");
const GV_LW_2_ENVELOPE: &str =
    include_str!("../../../docs/federation/fixtures/warpline-gv-lw-2-churn-envelope.json");

const GOLDEN_VECTORS_BLAKE3: &str =
    "15d191a3f6322c21d7412f079a5a0991eb721f913ee1a34cef08e798a3cdf67b";
const MCP_TOOL_INVENTORY_BLAKE3: &str =
    "3c043163444aacfcbdc750c14bb4251ba2c909027187c3de296f09205c37d538";
const GV_LW_2_ENVELOPE_BLAKE3: &str =
    "eb6c59a6912bdcbfff1eb9116de3cf30cd64b2037a2c3148e57f278357a9b953";

const DRIFT_REQUIRED_ENV: &str = "LOOMWEAVE_DRIFT_REQUIRED";
const WARPLINE_REPO_ENV: &str = "WARPLINE_REPO";

#[derive(Debug, PartialEq, Eq)]
enum DriftCheck {
    Compare,
    SkipClean,
    FailRequired,
}

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn fixture(value: &str) -> Value {
    serde_json::from_str(value).expect("vendored Warpline fixture parses")
}

fn drift_check_action(required: bool, authority_exists: bool) -> DriftCheck {
    match (authority_exists, required) {
        (true, _) => DriftCheck::Compare,
        (false, false) => DriftCheck::SkipClean,
        (false, true) => DriftCheck::FailRequired,
    }
}

fn drift_required() -> bool {
    matches!(
        std::env::var(DRIFT_REQUIRED_ENV)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn warpline_repo() -> PathBuf {
    PathBuf::from(std::env::var(WARPLINE_REPO_ENV).unwrap_or_else(|_| "/home/john/warpline".into()))
}

fn origin_main_bytes(repo: &Path, relative_path: &str) -> Option<Vec<u8>> {
    let spec = format!("origin/main:{relative_path}");
    let output = Command::new("git")
        .args(["show", &spec])
        .current_dir(repo)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn authority_bytes(repo: &Path, relative_path: &str) -> Option<Vec<u8>> {
    origin_main_bytes(repo, relative_path).or_else(|| std::fs::read(repo.join(relative_path)).ok())
}

#[test]
fn fixture_bytes_match_layer1_pins() {
    assert_eq!(
        digest(GOLDEN_VECTORS.as_bytes()),
        GOLDEN_VECTORS_BLAKE3,
        "vendored Warpline golden-vectors index drifted from its byte pin"
    );
    assert_eq!(
        digest(MCP_TOOL_INVENTORY.as_bytes()),
        MCP_TOOL_INVENTORY_BLAKE3,
        "vendored Warpline MCP tool inventory drifted from its byte pin"
    );
    assert_eq!(
        digest(GV_LW_2_ENVELOPE.as_bytes()),
        GV_LW_2_ENVELOPE_BLAKE3,
        "vendored Warpline GV-LW-2 churn envelope drifted from its byte pin"
    );
}

#[test]
fn fixture_pins_reject_mutated_bytes() {
    for (name, bytes, pinned) in [
        (
            "golden-vectors",
            GOLDEN_VECTORS.as_bytes(),
            GOLDEN_VECTORS_BLAKE3,
        ),
        (
            "mcp-tool-inventory",
            MCP_TOOL_INVENTORY.as_bytes(),
            MCP_TOOL_INVENTORY_BLAKE3,
        ),
        (
            "gv-lw-2-envelope",
            GV_LW_2_ENVELOPE.as_bytes(),
            GV_LW_2_ENVELOPE_BLAKE3,
        ),
    ] {
        let mut tampered = bytes.to_vec();
        tampered[0] ^= 0x01;
        assert_ne!(
            digest(&tampered),
            pinned,
            "a single mutated byte must not pass the {name} pin"
        );
    }
}

#[test]
fn vector_index_declares_gv_lw_2_churn_contract() {
    let fixture = fixture(GOLDEN_VECTORS);
    assert_eq!(fixture["schema"], "warpline.golden_vectors.v1");
    assert_eq!(fixture["producer"], "warpline");
    assert_eq!(
        fixture
            .pointer("/executable/module")
            .and_then(Value::as_str),
        Some("tests.contracts.test_golden_vectors")
    );

    let gv_lw_2 = fixture["vectors"]
        .as_array()
        .expect("vectors array")
        .iter()
        .find(|vector| vector["id"] == "GV-LW-2")
        .expect("GV-LW-2 vector exists");
    assert_eq!(gv_lw_2["seam"], "loomweave");
    assert_eq!(gv_lw_2["tool"], WARPLINE_CHURN_TOOL);
    let assertion = gv_lw_2["assert"].as_str().expect("assertion text");
    assert!(assertion.contains("3 SEIs"));
    assert!(assertion.contains("observed >=1"));
    assert!(assertion.contains("unobserved churn_count 0"));
    assert!(assertion.contains("not omitted, not error"));
}

#[test]
fn tool_inventory_declares_churn_read_only_local_contract() {
    let fixture = fixture(MCP_TOOL_INVENTORY);
    assert_eq!(fixture["schema"], "warpline.mcp_tool_inventory.v1");

    let churn_tools: Vec<&Value> = fixture["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter(|tool| tool["schema"] == WARPLINE_CHURN_SCHEMA)
        .collect();
    assert_eq!(
        churn_tools.len(),
        2,
        "inventory carries both short and endorsed churn tool entries"
    );

    for tool in churn_tools {
        assert_eq!(tool["endorsed_name"], WARPLINE_CHURN_TOOL);
        assert_eq!(tool["mutates"], false);
        assert_eq!(tool["read_only"], true);
        assert_eq!(tool["writes_local_state"], true);
        assert_eq!(tool["local_only"], true);
        assert_eq!(
            tool["peer_side_effects"]
                .as_array()
                .expect("peer_side_effects array")
                .len(),
            0
        );
        assert!(
            tool["authority_boundary"]
                .as_str()
                .unwrap_or_default()
                .contains("never-observed entity is churn_count 0")
        );
    }
}

#[test]
fn real_parser_accepts_producer_generated_gv_lw_2_envelope() {
    let envelope = fixture(GV_LW_2_ENVELOPE);
    assert_eq!(envelope["schema"], WARPLINE_CHURN_SCHEMA);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["query"]["tool"], WARPLINE_CHURN_TOOL);
    assert_eq!(envelope["data"]["overflow"]["reason_class"], "clean");
    assert_eq!(envelope["data"]["page"]["reason_class"], "clean");
    assert_eq!(envelope["enrichment"]["sei"], "present");
    assert_eq!(
        envelope["enrichment_reasons"]["sei"]["reason_class"],
        "clean"
    );
    assert_eq!(
        envelope["enrichment_reasons"]["requirements"]["reason_class"],
        "disabled"
    );
    assert_eq!(envelope["meta"]["local_only"], true);
    assert_eq!(
        envelope["meta"]["peer_side_effects"]
            .as_array()
            .expect("peer_side_effects array")
            .len(),
        0
    );

    let parsed = parse_churn_count_response(GV_LW_2_ENVELOPE)
        .expect("Loomweave must parse Warpline's producer-generated GV-LW-2 envelope");
    assert_eq!(parsed.schema.as_deref(), Some(WARPLINE_CHURN_SCHEMA));
    assert_eq!(parsed.ok, Some(true));
    assert_eq!(
        parsed.data.items.len(),
        3,
        "all three requested refs are echoed"
    );

    let observed = parsed
        .data
        .items
        .iter()
        .filter(|item| item.churn_count >= 1)
        .count();
    assert_eq!(observed, 2, "two refs carry observed churn");

    let unobserved = parsed
        .data
        .items
        .iter()
        .find(|item| item.entity.sei.as_deref() == Some("loomweave:eid:never-observed"))
        .expect("unobserved ref is present, not omitted");
    assert_eq!(
        unobserved.churn_count, 0,
        "unobserved ref is a 0, not an error"
    );
}

#[test]
fn vendored_authority_artifacts_match_warpline_origin_main() {
    let repo = warpline_repo();
    let required = drift_required();
    let authority_exists = repo.exists();
    match drift_check_action(required, authority_exists) {
        DriftCheck::SkipClean => {
            eprintln!(
                "Warpline repo not found at {} — skipping drift recheck \
                 (set {WARPLINE_REPO_ENV} to enable, or {DRIFT_REQUIRED_ENV}=1 to require it)",
                repo.display()
            );
        }
        DriftCheck::FailRequired => {
            panic!(
                "Warpline repo not found at {} but {DRIFT_REQUIRED_ENV} is set",
                repo.display()
            );
        }
        DriftCheck::Compare => {
            for (relative_path, vendored) in [
                (
                    "tests/fixtures/contracts/warpline/golden-vectors.json",
                    GOLDEN_VECTORS.as_bytes(),
                ),
                (
                    "tests/fixtures/contracts/warpline/mcp-tool-inventory.json",
                    MCP_TOOL_INVENTORY.as_bytes(),
                ),
            ] {
                let authority = authority_bytes(&repo, relative_path)
                    .unwrap_or_else(|| panic!("read Warpline authority artifact {relative_path}"));
                assert_eq!(
                    authority,
                    vendored,
                    "vendored Warpline artifact drifted from {} at {} \
                     (git origin/main is preferred; worktree file is fallback)",
                    relative_path,
                    repo.display()
                );
            }
        }
    }
}

#[test]
fn warpline_executable_gv_lw_2_source_oracle_passes_when_repo_available() {
    let repo = warpline_repo();
    let required = drift_required();
    let test_path = repo.join("tests/contracts/test_golden_vectors.py");
    match drift_check_action(required, test_path.exists()) {
        DriftCheck::SkipClean => {
            eprintln!(
                "Warpline executable oracle not found at {} — skipping producer-source recheck \
                 (set {WARPLINE_REPO_ENV} to enable, or {DRIFT_REQUIRED_ENV}=1 to require it)",
                test_path.display()
            );
        }
        DriftCheck::FailRequired => {
            panic!(
                "Warpline executable oracle not found at {} but {DRIFT_REQUIRED_ENV} is set",
                test_path.display()
            );
        }
        DriftCheck::Compare => {
            let output = Command::new("uv")
                .args([
                    "run",
                    "pytest",
                    "tests/contracts/test_golden_vectors.py",
                    "-q",
                    "-k",
                    "test_gv_lw_2_churn_count_includes_unobserved_as_zero",
                ])
                .current_dir(&repo)
                .output()
                .expect("run Warpline GV-LW-2 executable oracle through uv");
            assert!(
                output.status.success(),
                "Warpline GV-LW-2 executable oracle failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn drift_check_action_covers_required_and_skip_postures() {
    assert_eq!(drift_check_action(false, true), DriftCheck::Compare);
    assert_eq!(drift_check_action(true, true), DriftCheck::Compare);
    assert_eq!(drift_check_action(false, false), DriftCheck::SkipClean);
    assert_eq!(drift_check_action(true, false), DriftCheck::FailRequired);
}
