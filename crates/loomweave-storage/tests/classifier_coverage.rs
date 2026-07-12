use loomweave_storage::{
    MAX_CLASSIFIER_STATS_BYTES, known_entity_tags, latest_classifier_coverage, pragma, schema,
};
use rusqlite::{Connection, params};
use serde_json::json;
use sha2::{Digest, Sha256};

const PRODUCER_GOLDEN: &[u8] = include_bytes!("fixtures/classifier-coverage-v1.golden.json");
const PRODUCER_GOLDEN_SHA256: &str =
    include_str!("fixtures/classifier-coverage-v1.golden.json.sha256");
const AUTHORITY_NOTE: &str =
    include_str!("../../../docs/federation/2026-07-12-federation-seam-golden-authority.md");

fn database() -> Connection {
    let mut conn = Connection::open_in_memory().expect("open database");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    conn
}

#[test]
fn producer_golden_is_strict_valid_5_file_python_coverage_and_byte_pinned() {
    let coverage: loomweave_core::ClassifierCoverage =
        serde_json::from_slice(PRODUCER_GOLDEN).expect("strictly parse producer golden");
    let plugin = &coverage.plugins()[0];
    assert_eq!(coverage.schema(), "loomweave.classifier-coverage.v1");
    assert!(coverage.source_walk_complete());
    assert!(coverage.plugin_discovery_complete());
    assert_eq!(plugin.plugin_id(), "python");
    assert_eq!(plugin.matched_files(), 5);
    assert_eq!(plugin.analyzed_files(), 5);
    assert_eq!(plugin.retained_files(), 0);
    assert_eq!(plugin.degraded_files(), 0);
    assert_eq!(
        plugin.status(),
        loomweave_core::PluginCoverageStatus::Complete
    );
    for tag in ["cli-command", "entry-point", "http-route", "exported-api"] {
        assert!(
            plugin
                .classifier_tags()
                .iter()
                .any(|declared| declared == tag)
        );
    }

    let expected = PRODUCER_GOLDEN_SHA256
        .split_whitespace()
        .next()
        .expect("digest sidecar");
    let actual = format!("{:x}", Sha256::digest(PRODUCER_GOLDEN));
    assert_eq!(actual, expected);

    let mut mutated = PRODUCER_GOLDEN.to_vec();
    mutated[0] ^= 1;
    assert_ne!(format!("{:x}", Sha256::digest(mutated)), expected);
}

#[test]
fn authority_note_hash_table_is_locked_to_every_fixture_and_sidecar() {
    let fixtures: &[(&str, &[u8], &str)] = &[
        (
            "crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json",
            PRODUCER_GOLDEN,
            PRODUCER_GOLDEN_SHA256,
        ),
        (
            "docs/federation/fixtures/classification.python.json",
            include_bytes!("../../../docs/federation/fixtures/classification.python.json"),
            include_str!("../../../docs/federation/fixtures/classification.python.json.sha256"),
        ),
        (
            "docs/federation/fixtures/get-api-v1-capabilities.json",
            include_bytes!("../../../docs/federation/fixtures/get-api-v1-capabilities.json"),
            include_str!("../../../docs/federation/fixtures/get-api-v1-capabilities.json.sha256"),
        ),
        (
            "docs/federation/fixtures/loomweave-http-auth-v1.golden.json",
            include_bytes!("../../../docs/federation/fixtures/loomweave-http-auth-v1.golden.json"),
            include_str!(
                "../../../docs/federation/fixtures/loomweave-http-auth-v1.golden.json.sha256"
            ),
        ),
        (
            "docs/federation/fixtures/external-sqlite-compatibility-v1.json",
            include_bytes!(
                "../../../docs/federation/fixtures/external-sqlite-compatibility-v1.json"
            ),
            include_str!(
                "../../../docs/federation/fixtures/external-sqlite-compatibility-v1.json.sha256"
            ),
        ),
        (
            "docs/federation/fixtures/identity-ownership-v1.golden.json",
            include_bytes!("../../../docs/federation/fixtures/identity-ownership-v1.golden.json"),
            include_str!(
                "../../../docs/federation/fixtures/identity-ownership-v1.golden.json.sha256"
            ),
        ),
    ];
    for (path, bytes, sidecar) in fixtures {
        let actual = format!("{:x}", Sha256::digest(bytes));
        let pinned = sidecar.split_whitespace().next().expect("sidecar digest");
        assert_eq!(&actual, pinned, "sidecar drift for {path}");
        let expected_row = format!("| `{path}` | `{actual}` |");
        assert!(
            AUTHORITY_NOTE.lines().any(|line| line == expected_row),
            "authority note hash table drift for {path}: expected {expected_row}"
        );
    }
}

fn valid_coverage() -> serde_json::Value {
    json!({
        "schema": "loomweave.classifier-coverage.v1",
        "source_walk_complete": true,
        "source_walk_skipped_entries": 0,
        "plugin_discovery_complete": true,
        "plugin_discovery_errors": 0,
        "plugin_discovery_error_samples": [],
        "plugins": [{
            "plugin_id": "python",
            "plugin_version": "0.12.0",
            "ontology_version": "0.12.0",
            "matched_files": 1,
            "analyzed_files": 1,
            "retained_files": 0,
            "degraded_files": 0,
            "status": "complete",
            "classifier_tags": ["entry-point", "test"]
        }]
    })
}

fn insert_run(
    conn: &Connection,
    id: &str,
    started_at: &str,
    completed_at: Option<&str>,
    status: &str,
    stats_json: &str,
) {
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES (?1, ?2, ?3, '{}', ?4, ?5)",
        params![id, started_at, completed_at, stats_json, status],
    )
    .expect("insert run");
}

#[test]
fn no_run_is_unavailable() {
    let conn = database();
    let latest = latest_classifier_coverage(&conn).expect("read coverage");
    assert!(!latest.is_available());
    assert_eq!(latest.run_id(), None);
    assert_eq!(latest.reason(), Some("no analysis run exists"));
}

#[test]
fn completed_latest_run_returns_strictly_validated_coverage() {
    let conn = database();
    let stats = json!({"classifier_coverage": valid_coverage()}).to_string();
    insert_run(
        &conn,
        "run-ok",
        "2026-07-12T00:00:00Z",
        Some("2026-07-12T00:01:00Z"),
        "completed",
        &stats,
    );

    let latest = latest_classifier_coverage(&conn).expect("read coverage");
    assert!(latest.is_available());
    assert_eq!(latest.run_id(), Some("run-ok"));
    assert_eq!(latest.run_status(), Some("completed"));
    assert_eq!(
        latest.coverage().unwrap().plugins()[0].plugin_id(),
        "python"
    );
    assert_eq!(latest.reason(), None);
}

#[test]
fn newest_run_never_falls_back_and_ties_break_by_id() {
    let conn = database();
    let stats = json!({"classifier_coverage": valid_coverage()}).to_string();
    insert_run(
        &conn,
        "run-old",
        "2026-07-11T00:00:00Z",
        Some("2026-07-11T00:01:00Z"),
        "completed",
        &stats,
    );
    insert_run(
        &conn,
        "run-a",
        "2026-07-12T00:00:00Z",
        Some("2026-07-12T00:01:00Z"),
        "completed",
        &stats,
    );
    insert_run(
        &conn,
        "run-z",
        "2026-07-12T00:00:00Z",
        None,
        "running",
        &stats,
    );

    let latest = latest_classifier_coverage(&conn).expect("read coverage");
    assert!(!latest.is_available());
    assert_eq!(latest.run_id(), Some("run-z"));
    assert_eq!(latest.run_status(), Some("running"));
}

#[test]
fn every_non_completed_latest_state_is_unavailable_even_with_coverage() {
    for status in ["running", "failed", "skipped_no_plugins"] {
        let conn = database();
        let stats = json!({"classifier_coverage": valid_coverage()}).to_string();
        insert_run(
            &conn,
            "run-state",
            "2026-07-12T00:00:00Z",
            Some("2026-07-12T00:01:00Z"),
            status,
            &stats,
        );
        let latest = latest_classifier_coverage(&conn).expect("read coverage");
        assert!(!latest.is_available(), "status {status}");
        assert_eq!(latest.run_status(), Some(status));
    }
}

#[test]
fn completed_without_completion_timestamp_is_unavailable() {
    let conn = database();
    let stats = json!({"classifier_coverage": valid_coverage()}).to_string();
    insert_run(
        &conn,
        "run-inconsistent",
        "2026-07-12T00:00:00Z",
        None,
        "completed",
        &stats,
    );
    let latest = latest_classifier_coverage(&conn).expect("read coverage");
    assert!(!latest.is_available());
    assert!(latest.reason().unwrap().contains("completed_at"));
}

#[test]
fn malformed_missing_wrong_schema_and_invalid_invariants_fail_closed() {
    let cases = [
        ("not-json".to_owned(), "malformed"),
        (json!({}).to_string(), "missing"),
        (
            json!({"classifier_coverage": {"schema": "future"}}).to_string(),
            "invalid",
        ),
        (
            json!({"classifier_coverage": {
                "schema": "loomweave.classifier-coverage.v1",
                "source_walk_complete": true,
                "source_walk_skipped_entries": 1,
                "plugin_discovery_complete": true,
                "plugin_discovery_errors": 0,
                "plugin_discovery_error_samples": [],
                "plugins": []
            }})
            .to_string(),
            "invalid",
        ),
    ];

    for (index, (stats, reason_fragment)) in cases.into_iter().enumerate() {
        let conn = database();
        insert_run(
            &conn,
            &format!("run-{index}"),
            "2026-07-12T00:00:00Z",
            Some("2026-07-12T00:01:00Z"),
            "completed",
            &stats,
        );
        let latest = latest_classifier_coverage(&conn).expect("read coverage");
        assert!(!latest.is_available(), "case {index}");
        assert!(
            latest.reason().unwrap().contains(reason_fragment),
            "case {index}: {:?}",
            latest.reason()
        );
    }
}

#[test]
fn persisted_coverage_rejects_duplicate_and_unbounded_plugin_evidence() {
    let base = valid_coverage();
    let mut duplicate_plugins = base.clone();
    let plugin = duplicate_plugins["plugins"][0].clone();
    duplicate_plugins["plugins"] = json!([plugin.clone(), plugin]);

    let mut duplicate_tags = base.clone();
    duplicate_tags["plugins"][0]["classifier_tags"] = json!(["test", "test"]);

    let mut impossible_counts = base.clone();
    impossible_counts["plugins"][0]["matched_files"] = json!(2);

    let mut too_many_plugins = base.clone();
    let template = too_many_plugins["plugins"][0].clone();
    let plugins: Vec<_> = (0..257)
        .map(|index| {
            let mut plugin = template.clone();
            plugin["plugin_id"] = json!(format!("plugin_{index}"));
            plugin
        })
        .collect();
    too_many_plugins["plugins"] = json!(plugins);

    let mut oversized_string = base.clone();
    oversized_string["plugins"][0]["plugin_id"] = json!(format!("p{}", "x".repeat(128)));

    for (index, invalid) in [
        duplicate_plugins,
        duplicate_tags,
        impossible_counts,
        too_many_plugins,
        oversized_string,
    ]
    .into_iter()
    .enumerate()
    {
        let conn = database();
        let stats = json!({"classifier_coverage": invalid}).to_string();
        insert_run(
            &conn,
            &format!("run-invalid-{index}"),
            "2026-07-12T00:00:00Z",
            Some("2026-07-12T00:01:00Z"),
            "completed",
            &stats,
        );
        let latest = latest_classifier_coverage(&conn).expect("read coverage");
        assert!(!latest.is_available(), "case {index}");
        assert!(latest.reason().unwrap().contains("invalid"), "case {index}");
    }
}

#[test]
fn oversized_stats_fail_closed_before_json_materialization() {
    let conn = database();
    let oversized = format!(
        "{{\"padding\":\"{}\"}}",
        "x".repeat(MAX_CLASSIFIER_STATS_BYTES)
    );
    assert!(oversized.len() > MAX_CLASSIFIER_STATS_BYTES);
    insert_run(
        &conn,
        "run-oversized",
        "2026-07-12T00:00:00Z",
        Some("2026-07-12T00:01:00Z"),
        "completed",
        &oversized,
    );

    let latest = latest_classifier_coverage(&conn).expect("read coverage");
    assert!(!latest.is_available());
    assert!(latest.reason().unwrap().contains("byte limit"));
}

#[test]
fn malformed_sqlite_storage_classes_are_unavailable_not_reader_errors() {
    let conn = database();
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES ('run-blob', '2026-07-12T00:00:00Z', '2026-07-12T00:01:00Z', \
                 '{}', x'00ff', 'completed')",
        [],
    )
    .expect("insert blob stats");
    let latest = latest_classifier_coverage(&conn).expect("read malformed coverage fail-closed");
    assert!(!latest.is_available());
    assert_eq!(latest.run_id(), Some("run-blob"));
    assert!(latest.reason().unwrap().contains("non-text stats"));

    conn.execute("PRAGMA ignore_check_constraints = ON", [])
        .expect("allow corrupt status fixture");
    conn.execute("UPDATE runs SET status = x'00ff' WHERE id = 'run-blob'", [])
        .expect("corrupt status storage class");
    let latest = latest_classifier_coverage(&conn).expect("read malformed status fail-closed");
    assert!(!latest.is_available());
    assert_eq!(latest.run_id(), Some("run-blob"));
    assert_eq!(latest.run_status(), None);
    assert!(latest.reason().unwrap().contains("non-text status"));

    conn.execute(
        "UPDATE runs SET status = 'completed', started_at = x'00ff' WHERE id = 'run-blob'",
        [],
    )
    .expect("corrupt started_at storage class");
    let latest = latest_classifier_coverage(&conn).expect("read malformed started_at fail-closed");
    assert!(!latest.is_available());
    assert_eq!(latest.run_id(), Some("run-blob"));
    assert!(latest.reason().unwrap().contains("started_at"));

    conn.execute(
        "UPDATE runs SET started_at = '2026-07-12T00:00:00Z', id = x'00ff' \
         WHERE id = 'run-blob'",
        [],
    )
    .expect("corrupt id storage class");
    let latest = latest_classifier_coverage(&conn).expect("read malformed id fail-closed");
    assert!(!latest.is_available());
    assert_eq!(latest.run_id(), None);
    assert!(latest.reason().unwrap().contains("id"));
}

#[test]
fn explicit_read_transaction_pins_entity_and_coverage_generation() {
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("loomweave.db");
    let mut writer = Connection::open(&path).expect("open writer");
    pragma::apply_write_pragmas(&writer).expect("writer pragmas");
    schema::apply_migrations(&mut writer).expect("migrate database");
    let stats = json!({"classifier_coverage": valid_coverage()}).to_string();
    insert_run(
        &writer,
        "run-old",
        "2026-07-12T00:00:00Z",
        Some("2026-07-12T00:01:00Z"),
        "completed",
        &stats,
    );

    let reader = Connection::open(&path).expect("open reader");
    let snapshot = reader.unchecked_transaction().expect("begin read snapshot");
    // This is the entity/tag SELECT that establishes the same WAL snapshot the
    // MCP tag facet now holds until after reading classifier coverage.
    assert!(known_entity_tags(&snapshot).unwrap().is_empty());

    insert_run(
        &writer,
        "run-new",
        "2026-07-12T01:00:00Z",
        None,
        "running",
        &stats,
    );

    let pinned = latest_classifier_coverage(&snapshot).expect("read pinned coverage");
    assert!(pinned.is_available());
    assert_eq!(pinned.run_id(), Some("run-old"));
    snapshot.commit().expect("end read snapshot");

    let current = latest_classifier_coverage(&reader).expect("read current coverage");
    assert!(!current.is_available());
    assert_eq!(current.run_id(), Some("run-new"));
}
