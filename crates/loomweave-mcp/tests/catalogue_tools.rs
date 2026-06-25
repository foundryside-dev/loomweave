//! WS5 stateless catalogue — inspection reads (Task 1): `guidance_for`,
//! `findings_for`, `wardline_for`. Exercises the SEI-join contract,
//! honest-empty behaviour, and the bounded/pagination contract over the public
//! JSON-RPC surface.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use loomweave_core::{EmbeddingRecording, RecordingEmbeddingProvider};
use loomweave_mcp::config::SemanticSearchConfig;
use loomweave_mcp::filigree::{
    EntityAssociationsResponse, FiligreeClientError, FiligreeLookup, WardlineFinding,
};
use loomweave_mcp::{ServerState, list_tools};
use loomweave_storage::{EmbeddingKey, EmbeddingStore, ReaderPool, pragma, schema};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn open_project() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
    let project = tempfile::tempdir().expect("temp project");
    let loomweave_dir = project.path().join(".weft/loomweave");
    std::fs::create_dir_all(&loomweave_dir).expect("create .loomweave");
    let db_path = loomweave_dir.join("loomweave.db");
    let mut conn = Connection::open(&db_path).expect("open sqlite");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    (project, db_path, conn)
}

fn state_for(project_root: &std::path::Path, db_path: &std::path::Path) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool)
        .with_clock(|| "2026-06-02T00:00:00.000Z".to_owned())
}

fn state_for_filigree(
    project_root: &std::path::Path,
    db_path: &std::path::Path,
    client: Arc<dyn FiligreeLookup>,
) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool)
        .with_clock(|| "2026-06-02T00:00:00.000Z".to_owned())
        .with_filigree_client(client)
}

#[derive(Debug, Default)]
struct WardlineFindingClient {
    findings_by_path: Mutex<HashMap<String, Vec<WardlineFinding>>>,
    path_calls: Mutex<Vec<String>>,
    // When set, `wardline_findings_for_path` returns a transport error (503),
    // modelling a Filigree outage / paginated-hop truncation (L4).
    fail: bool,
}

impl WardlineFindingClient {
    fn with_findings_for_path(self, path: &str, findings: Vec<WardlineFinding>) -> Self {
        self.findings_by_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.to_owned(), findings);
        self
    }

    fn failing() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }

    fn path_calls(&self) -> Vec<String> {
        self.path_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl FiligreeLookup for WardlineFindingClient {
    fn associations_for(
        &self,
        _entity_id: &str,
    ) -> Result<EntityAssociationsResponse, FiligreeClientError> {
        Ok(EntityAssociationsResponse {
            associations: Vec::new(),
        })
    }

    fn wardline_findings_for_path(
        &self,
        path: &str,
    ) -> Result<Vec<WardlineFinding>, FiligreeClientError> {
        self.path_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(path.to_owned());
        if self.fail {
            return Err(FiligreeClientError::HttpStatus {
                status: 503,
                body: "down".to_owned(),
            });
        }
        Ok(self
            .findings_by_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
            .cloned()
            .unwrap_or_default())
    }
}

fn wardline_finding(qualname: &str, rule_id: &str) -> WardlineFinding {
    WardlineFinding {
        rule_id: rule_id.to_owned(),
        message: "tainted sink".to_owned(),
        severity: Some("high".to_owned()),
        status: Some("open".to_owned()),
        line_start: Some(10),
        line_end: Some(10),
        fingerprint: Some(format!("fp-{rule_id}")),
        file_id: Some("file-demo".to_owned()),
        metadata: json!({"wardline": {"qualname": qualname}}),
    }
}

fn insert_entity(
    conn: &Connection,
    id: &str,
    kind: &str,
    source_path: &str,
    range: Option<(i64, i64)>,
) {
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path, \
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at) \
         VALUES (?1,'python',?2,?1,?1,?3,?4,?5,'{}','hash','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        params![id, kind, source_path, range.map(|(s, _)| s), range.map(|(_, e)| e)],
    )
    .expect("insert entity");
}

fn insert_guidance(conn: &Connection, id: &str, properties_json: &str) {
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
         VALUES (?1,'core','guidance',?1,?1,?2,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        params![id, properties_json],
    )
    .expect("insert guidance");
}

fn insert_finding(
    conn: &Connection,
    id: &str,
    entity_id: &str,
    kind: &str,
    severity: &str,
    status: &str,
) {
    // A run row is required by the findings.run_id FK.
    conn.execute(
        "INSERT OR IGNORE INTO runs (id, started_at, config, stats, status) \
         VALUES ('run-1','2026-01-01T00:00:00.000Z','{}','{}','completed')",
        [],
    )
    .expect("insert run");
    conn.execute(
        "INSERT INTO findings (id, tool, tool_version, run_id, rule_id, kind, severity, entity_id, \
            related_entities, message, evidence, properties, supports, supported_by, status, created_at, updated_at) \
         VALUES (?1,'loomweave','1.0','run-1','R1',?3,?4,?2,'[]','m','{}','{}','[]','[]',?5, \
                 '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        params![id, entity_id, kind, severity, status],
    )
    .expect("insert finding");
}

fn insert_taint_fact(conn: &Connection, entity_id: &str, wardline_json: &str) {
    conn.execute(
        "INSERT INTO wardline_taint_facts (entity_id, wardline_json, updated_at) \
         VALUES (?1, ?2, '2026-01-01T00:00:00.000Z')",
        params![entity_id, wardline_json],
    )
    .expect("insert taint fact");
}

fn insert_alive_sei(conn: &Connection, sei: &str, locator: &str) {
    conn.execute(
        "INSERT INTO sei_bindings (sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at) \
         VALUES (?1, ?2, 'bh', NULL, 'alive', 'run-1', 'run-1', '2026-01-01T00:00:00.000Z')",
        params![sei, locator],
    )
    .expect("insert sei binding");
}

fn insert_tag(conn: &Connection, entity_id: &str, tag: &str) {
    conn.execute(
        "INSERT INTO entity_tags (entity_id, plugin_id, tag) VALUES (?1, 'python', ?2)",
        params![entity_id, tag],
    )
    .expect("insert tag");
}

fn insert_tag_with_plugin(conn: &Connection, entity_id: &str, plugin_id: &str, tag: &str) {
    conn.execute(
        "INSERT INTO entity_tags (entity_id, plugin_id, tag) VALUES (?1, ?2, ?3)",
        params![entity_id, plugin_id, tag],
    )
    .expect("insert tag");
}

fn insert_contains_edge(conn: &Connection, parent: &str, child: &str) {
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence) VALUES ('contains', ?1, ?2, 'resolved')",
        params![parent, child],
    )
    .expect("insert contains edge");
}

async fn call_tool(state: &ServerState, name: &str, arguments: Value) -> Value {
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "t",
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await
        .expect("tools/call returns a response");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool content text");
    serde_json::from_str(text).expect("tool envelope JSON")
}

#[test]
fn tools_list_includes_ws5_inspection_tools() {
    let names: Vec<&str> = list_tools().iter().map(|t| t.name).collect();
    for expected in [
        "entity_guidance_list",
        "entity_finding_list",
        "entity_wardline_get",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
}

// ---- wardline_for -------------------------------------------------------

#[tokio::test]
async fn wardline_for_returns_verbatim_blob_when_present() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_taint_fact(
        &conn,
        "python:function:m.f",
        r#"{"taint":"tainted","sources":["request.body"]}"#,
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "wardline_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["result_kind"], "present");
    assert_eq!(env["result"]["wardline"]["taint"], "tainted");
}

#[tokio::test]
async fn wardline_for_is_honest_empty_when_no_fact() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "wardline_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["result_kind"], "no_facts");
    assert_eq!(env["result"]["wardline"], Value::Null);
    assert_eq!(env["result"]["signal"]["available"], false);
}

#[tokio::test]
async fn wardline_for_unknown_entity_errors() {
    let (project, db, _conn) = open_project();
    let state = state_for(project.path(), &db);
    let env = call_tool(
        &state,
        "wardline_for",
        json!({"id": "python:function:nope"}),
    )
    .await;
    assert_eq!(env["ok"], false, "{env}");
    assert_eq!(env["error"]["code"], "entity-not-found");
}

// ---- SEI-join contract (ADR-038) ---------------------------------------

#[tokio::test]
async fn entity_sei_is_null_without_binding_and_populated_with_one() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    // Pre-Wave-1: no sei_bindings row -> sei is null (graceful degrade).
    let env = call_tool(&state, "wardline_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["result"]["entity"]["sei"], Value::Null, "{env}");

    // Bind an alive SEI -> the read-time join populates it.
    let conn = Connection::open(&db).unwrap();
    insert_alive_sei(&conn, "loomweave:eid:deadbeef", "python:function:m.f");
    drop(conn);
    let env = call_tool(&state, "wardline_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(
        env["result"]["entity"]["sei"], "loomweave:eid:deadbeef",
        "{env}"
    );
}

#[tokio::test]
async fn wardline_for_accepts_sei_and_resolves_to_same_entity_as_locator() {
    // Item 1 (clarion-d76e7f7267): wardline_for accepts a SEI and resolves it to
    // the same entity as the locator.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_taint_fact(
        &conn,
        "python:function:m.f",
        r#"{"taint":"tainted","sources":["request.body"]}"#,
    );
    insert_alive_sei(&conn, "loomweave:eid:mf", "python:function:m.f");
    drop(conn);
    let state = state_for(project.path(), &db);

    let by_locator = call_tool(&state, "wardline_for", json!({"id": "python:function:m.f"})).await;
    let by_sei = call_tool(&state, "wardline_for", json!({"id": "loomweave:eid:mf"})).await;

    assert_eq!(by_locator["ok"], true, "{by_locator}");
    assert_eq!(by_sei["ok"], true, "SEI must resolve: {by_sei}");
    assert_eq!(by_sei["result"]["result_kind"], "present");
    assert_eq!(by_sei["result"], by_locator["result"]);
}

// ---- findings_for -------------------------------------------------------

#[tokio::test]
async fn findings_for_returns_anchored_findings_and_filters() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_finding(
        &conn,
        "f-open",
        "python:function:m.f",
        "defect",
        "WARN",
        "open",
    );
    insert_finding(
        &conn,
        "f-supp",
        "python:function:m.f",
        "defect",
        "ERROR",
        "suppressed",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "findings_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 2);

    let env = call_tool(
        &state,
        "findings_for",
        json!({"id": "python:function:m.f", "filter": {"status": "open"}}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["findings"][0]["id"], "f-open");
}

#[tokio::test]
async fn findings_for_paginates_with_total_and_truncated() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    for i in 0..5 {
        insert_finding(
            &conn,
            &format!("f-{i}"),
            "python:function:m.f",
            "defect",
            "WARN",
            "open",
        );
    }
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "findings_for",
        json!({"id": "python:function:m.f", "limit": 2, "offset": 0}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 5, "{env}");
    assert_eq!(env["result"]["page"]["returned"], 2);
    assert_eq!(env["result"]["page"]["truncated"], true);
    assert_eq!(env["result"]["findings"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn findings_for_applies_filter_before_large_result_cap() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    for i in 0..5000 {
        insert_finding(
            &conn,
            &format!("f-{i:04}"),
            "python:function:m.f",
            "defect",
            "WARN",
            "open",
        );
    }
    insert_finding(
        &conn,
        "z-critical",
        "python:function:m.f",
        "defect",
        "CRITICAL",
        "open",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "findings_for",
        json!({"id": "python:function:m.f", "filter": {"severity": "CRITICAL"}}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["findings"][0]["id"], "z-critical");
    assert_eq!(env["result"]["scan_truncated"], false, "{env}");
}

#[tokio::test]
async fn findings_for_empty_entity_is_not_an_error() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "findings_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
    assert!(env["result"]["findings"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn findings_for_rejects_unknown_filter_values_with_vocabulary() {
    // clarion-c137d73ebf: kind/severity/status are closed sets (ADR-031 CHECK
    // constraints), so a typo'd value can never match a row — silently
    // returning an empty page is indistinguishable from a clean entity. An
    // unknown value is a caller bug: JSON-RPC param error (-32602) naming the
    // valid vocabulary, mirroring the unknown-argument-KEY precedent.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    for (field, bad, vocab_member) in [
        ("severity", "eror", "CRITICAL"),
        ("kind", "defct", "classification"),
        ("status", "opne", "promoted_to_issue"),
    ] {
        let response = state
            .handle_json_rpc(&json!({
                "jsonrpc": "2.0",
                "id": "bad-filter",
                "method": "tools/call",
                "params": {"name": "entity_finding_list", "arguments": {
                    "id": "python:function:m.f",
                    "filter": {field: bad}
                }}
            }))
            .await
            .expect("response");
        assert_eq!(
            response["error"]["code"], -32602,
            "filter.{field}={bad} must be a param error: {response}"
        );
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains(vocab_member),
            "filter.{field} error must list the valid vocabulary: {message}"
        );
        assert!(
            message.contains(bad),
            "filter.{field} error must echo the rejected value: {message}"
        );
    }
}

#[tokio::test]
async fn findings_for_filter_values_canonicalize_case() {
    // The canonical vocabulary mixes cases (severity uppercase, kind/status
    // lowercase) and agents reliably type the other one — `severity: "error"`
    // appeared in our own skill example. Case-insensitive input canonicalises
    // instead of rejecting, and the echoed filter shows the canonical value.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_finding(
        &conn,
        "f-err",
        "python:function:m.f",
        "defect",
        "ERROR",
        "open",
    );
    insert_finding(
        &conn,
        "f-info",
        "python:function:m.f",
        "fact",
        "INFO",
        "open",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_finding_list",
        json!({"id": "python:function:m.f", "filter": {"severity": "error", "kind": "DEFECT", "status": "Open"}}),
    )
    .await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["findings"][0]["id"], "f-err", "{env}");
    assert_eq!(env["result"]["filter"]["severity"], "ERROR", "{env}");
    assert_eq!(env["result"]["filter"]["kind"], "defect", "{env}");
    assert_eq!(env["result"]["filter"]["status"], "open", "{env}");
}

// ---- project_finding_list (L1: whole-project finding browser) -----------

#[tokio::test]
async fn project_finding_list_total_reconciles_with_project_status_finding_count() {
    // The L1 acceptance: an agent must be able to go from project_status's
    // `findings: N` straight to the N findings — so the project-wide list's
    // page.total must reconcile with project_status_get's finding count.
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    insert_entity(&conn, "python:function:b", "function", "b.py", Some((3, 4)));
    insert_finding(&conn, "f-1", "python:function:a", "defect", "WARN", "open");
    insert_finding(&conn, "f-2", "python:function:a", "defect", "ERROR", "open");
    insert_finding(&conn, "f-3", "python:function:b", "fact", "INFO", "open");
    drop(conn);
    let state = state_for(project.path(), &db);

    let status = call_tool(&state, "project_status", json!({})).await;
    let count = status["result"]["counts"]["findings"].as_i64().unwrap();
    assert_eq!(count, 3, "{status}");

    let env = call_tool(&state, "project_finding_list", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(
        env["result"]["page"]["total"].as_i64().unwrap(),
        count,
        "project_finding_list total must reconcile with project_status finding count: {env}"
    );
    assert_eq!(
        env["result"]["findings"].as_array().unwrap().len(),
        3,
        "{env}"
    );
}

#[tokio::test]
async fn project_finding_list_honest_empty_when_no_findings() {
    // Honest-empty: a project with 0 findings returns an empty list, not an error.
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "project_finding_list", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
    assert!(
        env["result"]["findings"].as_array().unwrap().is_empty(),
        "{env}"
    );
}

#[tokio::test]
async fn project_finding_list_rows_carry_entity_sei_file_line_severity_rule() {
    // Each finding carries its anchoring entity SEI + file:line + severity/rule —
    // with no entity id supplied by the caller.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((4, 9)),
    );
    insert_alive_sei(&conn, "loomweave:eid:abc123", "python:function:m.f");
    insert_finding(
        &conn,
        "f-1",
        "python:function:m.f",
        "defect",
        "WARN",
        "open",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "project_finding_list", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let row = &env["result"]["findings"][0];
    assert_eq!(row["rule_id"], "R1", "{env}");
    assert_eq!(row["severity"], "WARN", "{env}");
    assert_eq!(row["entity"]["id"], "python:function:m.f", "{env}");
    assert_eq!(row["entity"]["sei"], "loomweave:eid:abc123", "{env}");
    assert_eq!(row["entity"]["file"], "m.py", "{env}");
    assert_eq!(row["entity"]["line"], 4, "{env}");
}

#[tokio::test]
async fn project_finding_list_rejects_unknown_filter_value() {
    // Same closed-set discipline as entity_finding_list — both routes share
    // FindingFilter::parse, but the contract is asserted per registered tool.
    let (project, db, _conn) = open_project();
    let state = state_for(project.path(), &db);

    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "bad-filter",
            "method": "tools/call",
            "params": {"name": "project_finding_list", "arguments": {
                "filter": {"severity": "eror"}
            }}
        }))
        .await
        .expect("response");
    assert_eq!(
        response["error"]["code"], -32602,
        "typo'd severity must be a param error: {response}"
    );
    let message = response["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("CRITICAL") && message.contains("eror"),
        "error must list the vocabulary and echo the rejected value: {message}"
    );
}

#[tokio::test]
async fn project_finding_list_filters_and_paginates() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    for i in 0..5 {
        insert_finding(
            &conn,
            &format!("f-{i}"),
            "python:function:a",
            "defect",
            "WARN",
            "open",
        );
    }
    insert_finding(
        &conn,
        "z-crit",
        "python:function:a",
        "defect",
        "CRITICAL",
        "open",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    // Filter: only the CRITICAL one.
    let env = call_tool(
        &state,
        "project_finding_list",
        json!({"filter": {"severity": "CRITICAL"}}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["findings"][0]["id"], "z-crit", "{env}");

    // Paginate over the full set (6 findings).
    let env = call_tool(
        &state,
        "project_finding_list",
        json!({"limit": 2, "offset": 0}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 6, "{env}");
    assert_eq!(env["result"]["page"]["returned"], 2, "{env}");
    assert_eq!(env["result"]["page"]["truncated"], true, "{env}");
}

// ---- guidance_for -------------------------------------------------------

#[tokio::test]
async fn guidance_for_composes_path_matched_sheets_ranked() {
    let (project, db, conn) = open_project();
    let src = project.path().join("src/auth/tokens.py");
    insert_entity(
        &conn,
        "python:function:src.auth.tokens.refresh",
        "function",
        src.to_str().unwrap(),
        Some((1, 2)),
    );
    // project-scope sheet (rank 1) and a module-scope sheet (rank 4); both match
    // by path, so ranking puts the project sheet first.
    insert_guidance(
        &conn,
        "core:guidance:proj",
        r#"{"scope_level":"project","scope_rank":1,"content":"P","authored_at":"2026-01-01",
            "match_rules":[{"type":"path","pattern":"src/**"}]}"#,
    );
    insert_guidance(
        &conn,
        "core:guidance:mod",
        r#"{"scope_level":"module","scope_rank":4,"content":"M","authored_at":"2026-01-01",
            "match_rules":[{"type":"path","pattern":"src/auth/**"}]}"#,
    );
    // a non-matching sheet
    insert_guidance(
        &conn,
        "core:guidance:other",
        r#"{"scope_level":"module","scope_rank":4,"content":"X","authored_at":"2026-01-01",
            "match_rules":[{"type":"path","pattern":"src/billing/**"}]}"#,
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "guidance_for",
        json!({"id": "python:function:src.auth.tokens.refresh"}),
    )
    .await;
    assert_eq!(env["ok"], true, "{env}");
    let sheets = env["result"]["guidance"].as_array().unwrap();
    assert_eq!(sheets.len(), 2, "{env}");
    assert_eq!(sheets[0]["id"], "core:guidance:proj"); // rank 1 first
    assert_eq!(sheets[1]["id"], "core:guidance:mod");
    assert_eq!(env["result"]["page"]["total"], 2);
}

#[tokio::test]
async fn guidance_for_excludes_expired_sheets() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_guidance(
        &conn,
        "core:guidance:stale",
        r#"{"scope_level":"project","scope_rank":1,"content":"S","authored_at":"2025-01-01",
            "expires":"2025-12-31T00:00:00.000Z","match_rules":[{"type":"kind","value":"function"}]}"#,
    );
    drop(conn);
    // clock is 2026-06-02, after the expiry
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "guidance_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(
        env["result"]["guidance"].as_array().unwrap().len(),
        0,
        "{env}"
    );
}

#[tokio::test]
async fn guidance_for_honors_unix_clock_for_expiry() {
    // Regression for clarion-3153e74f0b: production `serve` uses the default
    // `unix:<seconds>` clock (never `.with_clock(...)`). A raw lexical compare
    // against an ISO `expires` (which starts with '2' < 'u') wrongly classified
    // EVERY sheet with any `expires` as expired. This exercises the production
    // clock path: a far-future sheet must survive; a far-past one must be dropped.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_guidance(
        &conn,
        "core:guidance:future",
        r#"{"scope_level":"project","scope_rank":1,"content":"F","authored_at":"2026-01-01",
            "expires":"2999-12-31T00:00:00.000Z","match_rules":[{"type":"kind","value":"function"}]}"#,
    );
    insert_guidance(
        &conn,
        "core:guidance:past",
        r#"{"scope_level":"project","scope_rank":1,"content":"P","authored_at":"2026-01-01",
            "expires":"2000-01-01T00:00:00.000Z","match_rules":[{"type":"kind","value":"function"}]}"#,
    );
    drop(conn);
    // Production-style clock: `unix:<seconds>` (here a fixed mid-2025 instant),
    // matching `default_now_string`'s form — between the past (2000) and
    // future (2999) expiries.
    let pool = ReaderPool::open(&db, 2).expect("reader pool");
    let state = ServerState::new(project.path().to_path_buf(), pool)
        .with_clock(|| "unix:1748822400".to_owned());

    let env = call_tool(&state, "guidance_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["ok"], true, "{env}");
    let sheets = env["result"]["guidance"].as_array().unwrap();
    let ids: Vec<&str> = sheets.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert!(
        ids.contains(&"core:guidance:future"),
        "far-future sheet must survive under the unix: clock, got {ids:?} in {env}"
    );
    assert!(
        !ids.contains(&"core:guidance:past"),
        "far-past sheet must be excluded, got {ids:?} in {env}"
    );
}

#[tokio::test]
async fn guidance_for_honest_empty_when_no_sheet_matches() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "guidance_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
}

#[tokio::test]
async fn guidance_for_reports_unevaluable_wardline_group_rule() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_guidance(
        &conn,
        "core:guidance:wl",
        r#"{"scope_level":"project","scope_rank":1,"content":"W","authored_at":"2026-01-01",
            "match_rules":[{"type":"wardline_group","value":2}]}"#,
    );
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "guidance_for", json!({"id": "python:function:m.f"})).await;
    // The wardline_group rule cannot match here -> sheet not applied, note surfaced.
    assert_eq!(
        env["result"]["guidance"].as_array().unwrap().len(),
        0,
        "{env}"
    );
    assert_eq!(env["result"]["notes"][0]["signal"], "wardline_group");
}

// ---- faceted search -----------------------------------------------------

#[tokio::test]
async fn find_by_kind_returns_matching_entities_with_sei_field() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    insert_entity(&conn, "python:function:b", "function", "b.py", Some((1, 2)));
    insert_entity(&conn, "python:class:C", "class", "c.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_by_kind", json!({"kind": "function"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 2);
    let ents = env["result"]["entities"].as_array().unwrap();
    assert_eq!(ents.len(), 2);
    assert!(
        ents[0].get("sei").is_some(),
        "entity rows must carry sei: {env}"
    );
    assert!(
        env["result"].get("known_kinds").is_none(),
        "known_kinds is an unknown-kind hint, not a constant payload: {env}"
    );
}

#[tokio::test]
async fn find_by_kind_unknown_kind_is_empty_with_known_kinds_hint() {
    // clarion-c137d73ebf: kinds are plugin-owned (an OPEN set, unlike finding
    // filters) so an unknown kind cannot be rejected up front — but the empty
    // result must be distinguishable from "kind exists, no matches in scope".
    // When the kind matches zero entities project-wide the result carries the
    // kinds the index actually holds.
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    insert_entity(&conn, "python:class:C", "class", "c.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "find_by_kind", json!({"kind": "nonesuch"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
    assert_eq!(
        env["result"]["known_kinds"],
        json!(["class", "function"]),
        "empty-by-unknown-kind must list the kinds the index holds: {env}"
    );
}

#[tokio::test]
async fn find_by_kind_paginates_with_total_and_truncated() {
    let (project, db, conn) = open_project();
    for i in 0..5 {
        insert_entity(
            &conn,
            &format!("python:function:f{i}"),
            "function",
            "m.py",
            Some((1, 2)),
        );
    }
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(
        &state,
        "find_by_kind",
        json!({"kind": "function", "limit": 2}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 5, "{env}");
    assert_eq!(env["result"]["page"]["returned"], 2);
    assert_eq!(env["result"]["page"]["truncated"], true);
}

#[tokio::test]
async fn find_by_tag_is_honest_empty_when_no_tag_emitted() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "find_by_tag", json!({"tag": "entry-point"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
    assert_eq!(env["result"]["signal"]["available"], false);
}

#[tokio::test]
async fn find_by_tag_empty_reason_is_derived_from_reality_not_hand_maintained() {
    // weft-7256739b31 (dogfood-4 B10b): the honest-empty reason claimed "the
    // Python plugin emits none today" while test/data-model/entry-point tags
    // were demonstrably populated in the same index. The reason must be derived
    // from the store: name the tags that ARE present, and never assert that
    // plugins emit no tags when they visibly do.
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    insert_entity(&conn, "python:function:b", "function", "b.py", Some((1, 2)));
    insert_tag(&conn, "python:function:a", "test");
    insert_tag(&conn, "python:function:b", "entry-point");
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_by_tag", json!({"tag": "no-such-tag"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
    assert_eq!(env["result"]["signal"]["available"], false);
    let reason = env["result"]["signal"]["reason"]
        .as_str()
        .expect("missing-signal reason is a string");
    assert!(
        !reason.contains("emits none"),
        "the reason must not claim plugins emit no tags when tags are populated: {reason}"
    );
    // The truthful, reality-derived hint: the tags this index actually holds.
    let known = env["result"]["known_tags"]
        .as_array()
        .unwrap_or_else(|| panic!("known_tags must list the populated tags: {env}"));
    let known: Vec<&str> = known.iter().filter_map(Value::as_str).collect();
    assert_eq!(known, ["entry-point", "test"], "{env}");
}

#[tokio::test]
async fn find_by_tag_empty_reason_on_tagless_index_says_so() {
    // The other truthful branch: an index with NO tags at all says exactly
    // that, with an empty known_tags list.
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_by_tag", json!({"tag": "test"})).await;
    assert_eq!(env["result"]["page"]["total"], 0);
    assert_eq!(env["result"]["signal"]["available"], false);
    assert_eq!(env["result"]["known_tags"], json!([]), "{env}");
    let reason = env["result"]["signal"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("no categorisation tags"),
        "a tagless index must say no tags are populated at all: {reason}"
    );
}

#[tokio::test]
async fn find_by_tag_returns_tagged_entities() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    insert_entity(&conn, "python:function:b", "function", "b.py", Some((1, 2)));
    insert_tag(&conn, "python:function:a", "integral_writer");
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "find_by_tag", json!({"tag": "integral_writer"})).await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["entities"][0]["id"], "python:function:a");
}

#[tokio::test]
async fn find_by_kind_path_glob_scope_filters_by_source_path() {
    let (project, db, conn) = open_project();
    let auth = project.path().join("src/auth/tokens.py");
    let billing = project.path().join("src/billing/ledger.py");
    insert_entity(
        &conn,
        "python:function:auth.f",
        "function",
        auth.to_str().unwrap(),
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:billing.f",
        "function",
        billing.to_str().unwrap(),
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(
        &state,
        "find_by_kind",
        json!({"kind": "function", "scope": "src/auth/**"}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["entities"][0]["id"], "python:function:auth.f");
}

#[tokio::test]
async fn find_by_kind_entity_scope_filters_to_descendants() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:module:m", "module", "m.py", Some((1, 20)));
    insert_entity(
        &conn,
        "python:function:m.inner",
        "function",
        "m.py",
        Some((2, 3)),
    );
    insert_entity(
        &conn,
        "python:function:other",
        "function",
        "o.py",
        Some((1, 2)),
    );
    insert_contains_edge(&conn, "python:module:m", "python:function:m.inner");
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(
        &state,
        "find_by_kind",
        json!({"kind": "function", "scope": "python:module:m"}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(
        env["result"]["entities"][0]["id"],
        "python:function:m.inner"
    );
}

#[tokio::test]
async fn find_by_wardline_filters_by_tier_best_effort() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    insert_entity(&conn, "python:function:b", "function", "b.py", Some((1, 2)));
    insert_taint_fact(&conn, "python:function:a", r#"{"tier":"exact","group":2}"#);
    insert_taint_fact(
        &conn,
        "python:function:b",
        r#"{"tier":"heuristic","group":1}"#,
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_by_wardline", json!({})).await;
    assert_eq!(env["result"]["page"]["total"], 2, "{env}");

    let env = call_tool(&state, "find_by_wardline", json!({"tier": "exact"})).await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["entities"][0]["id"], "python:function:a");
    assert_eq!(env["result"]["entities"][0]["wardline"]["tier"], "exact");

    let env = call_tool(&state, "find_by_wardline", json!({"group": 1})).await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["entities"][0]["id"], "python:function:b");
}

#[tokio::test]
async fn find_by_wardline_honest_empty_when_no_facts() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "find_by_wardline", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
    assert_eq!(env["result"]["signal"]["available"], false);
}

#[tokio::test]
async fn find_by_wardline_has_findings_filter_restricts_to_fact_carrying_entities() {
    // L1 complement: page only the wardline entities that actually carry
    // findings, instead of every taint-fact-bearing entity.
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    insert_entity(&conn, "python:function:b", "function", "b.py", Some((1, 2)));
    insert_taint_fact(&conn, "python:function:a", r#"{"tier":"exact"}"#);
    insert_taint_fact(&conn, "python:function:b", r#"{"tier":"exact"}"#);
    // Only `a` carries a finding.
    insert_finding(&conn, "f-1", "python:function:a", "defect", "WARN", "open");
    drop(conn);
    let state = state_for(project.path(), &db);

    // Unfiltered: both taint-fact entities.
    let env = call_tool(&state, "find_by_wardline", json!({})).await;
    assert_eq!(env["result"]["page"]["total"], 2, "{env}");

    // has_findings: true → only `a`.
    let env = call_tool(&state, "find_by_wardline", json!({"has_findings": true})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(
        env["result"]["entities"][0]["id"], "python:function:a",
        "{env}"
    );
    assert_eq!(env["result"]["facet"]["has_findings"], true, "{env}");
}

#[tokio::test]
async fn find_by_wardline_has_findings_uses_filigree_wardline_enrichment() {
    // Lacuna dogfood regression: targeted enrichment (`entity_issue_list`) can
    // hydrate Wardline findings from Filigree even when the local Loomweave
    // findings table is empty. The browse/facet path must agree.
    let (project, db, conn) = open_project();
    std::fs::create_dir_all(project.path().join("src")).expect("create src dir");
    std::fs::write(
        project.path().join("src/demo.py"),
        "def hello():\n    pass\n",
    )
    .expect("write source");
    insert_entity(
        &conn,
        "python:function:demo.hello",
        "function",
        "src/demo.py",
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:demo.other",
        "function",
        "src/demo.py",
        Some((4, 5)),
    );
    insert_taint_fact(&conn, "python:function:demo.hello", r#"{"tier":"exact"}"#);
    insert_taint_fact(&conn, "python:function:demo.other", r#"{"tier":"exact"}"#);
    drop(conn);

    let client = Arc::new(WardlineFindingClient::default().with_findings_for_path(
        "src/demo.py",
        vec![wardline_finding("demo.hello", "WLN-TAINT-001")],
    ));
    let state = state_for_filigree(project.path(), &db, client.clone());

    let targeted = call_tool(
        &state,
        "entity_issue_list",
        json!({"id": "python:function:demo.hello", "include_contained": false}),
    )
    .await;
    assert_eq!(targeted["ok"], true, "{targeted}");
    assert_eq!(
        targeted["result"]["wardline_findings"]["result_kind"], "matched",
        "{targeted}"
    );

    let listed = call_tool(
        &state,
        "entity_wardline_list",
        json!({"has_findings": true}),
    )
    .await;
    assert_eq!(listed["ok"], true, "{listed}");
    assert_eq!(listed["result"]["page"]["total"], 1, "{listed}");
    assert_eq!(
        listed["result"]["entities"][0]["id"], "python:function:demo.hello",
        "{listed}"
    );
    assert_eq!(
        client.path_calls(),
        vec!["src/demo.py".to_owned(), "src/demo.py".to_owned()],
        "targeted lookup plus cached browse lookup should each resolve the project-relative path"
    );
}

#[tokio::test]
async fn find_by_wardline_has_findings_emits_degrade_marker_on_filigree_outage() {
    // L4: during a Filigree outage the has_findings filter cannot evaluate the
    // Filigree-only-finding entities (no local anchor row), so they are dropped.
    // The response must carry an in-band `wardline` degrade marker rather than
    // an affirmative "no entity matches" missing-signal — "couldn't check" must
    // never be conflated with "confirmed none".
    let (project, db, conn) = open_project();
    std::fs::create_dir_all(project.path().join("src")).expect("create src dir");
    std::fs::write(
        project.path().join("src/demo.py"),
        "def hello():\n    pass\n",
    )
    .expect("write source");
    insert_entity(
        &conn,
        "python:function:demo.hello",
        "function",
        "src/demo.py",
        Some((1, 2)),
    );
    insert_taint_fact(&conn, "python:function:demo.hello", r#"{"tier":"exact"}"#);
    // No local findings row: the only path to a positive has_findings answer is
    // the Filigree Wardline lookup — which is down.
    drop(conn);

    let client = Arc::new(WardlineFindingClient::failing());
    let state = state_for_filigree(project.path(), &db, client.clone());

    let listed = call_tool(
        &state,
        "entity_wardline_list",
        json!({"has_findings": true}),
    )
    .await;
    assert_eq!(listed["ok"], true, "{listed}");
    // The entity was dropped (enrich-only: the outage never breaks the facet)...
    assert_eq!(listed["result"]["page"]["total"], 0, "{listed}");
    // ...but the result is explicitly marked degraded, NOT a confirmed empty.
    assert_eq!(
        listed["result"]["wardline"]["result_kind"], "unavailable",
        "a Filigree outage must surface an in-band wardline degrade marker: {listed}"
    );
    assert!(
        listed["result"]["signal"].is_null(),
        "the affirmative 'no entity matches' missing-signal must NOT appear on a degraded read: {listed}"
    );
}

#[test]
fn entity_wardline_list_schema_declares_has_findings() {
    // additionalProperties:false on the advertised schema would reject an
    // undeclared param, so has_findings must be declared for clients to send it.
    let tools = list_tools();
    let tool = tools
        .iter()
        .find(|t| t.name == "entity_wardline_list")
        .expect("entity_wardline_list tool definition");
    assert_eq!(
        tool.input_schema["properties"]["has_findings"],
        json!({"type": "boolean"}),
        "{:#}",
        tool.input_schema
    );
}

// ---- graph shortcuts ----------------------------------------------------

fn insert_edge(conn: &Connection, kind: &str, from: &str, to: &str, confidence: &str) {
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence) VALUES (?1, ?2, ?3, ?4)",
        params![kind, from, to, confidence],
    )
    .expect("insert edge");
}

fn insert_edge_with_properties(
    conn: &Connection,
    kind: &str,
    from: &str,
    to: &str,
    confidence: &str,
    properties: &Value,
) {
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence, properties) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![kind, from, to, confidence, properties.to_string()],
    )
    .expect("insert edge with properties");
}

#[tokio::test]
async fn find_circular_imports_detects_a_cycle() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:module:a", "module", "a.py", Some((1, 5)));
    insert_entity(&conn, "python:module:b", "module", "b.py", Some((1, 5)));
    insert_entity(&conn, "python:module:c", "module", "c.py", Some((1, 5)));
    insert_edge(
        &conn,
        "imports",
        "python:module:a",
        "python:module:b",
        "resolved",
    );
    insert_edge(
        &conn,
        "imports",
        "python:module:b",
        "python:module:a",
        "resolved",
    );
    insert_edge(
        &conn,
        "imports",
        "python:module:b",
        "python:module:c",
        "resolved",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_circular_imports", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["cycles"][0]["length"], 2);
    assert_eq!(env["result"]["confidence"], "resolved");
    // members carry sei
    assert!(
        env["result"]["cycles"][0]["members"][0]
            .get("sei")
            .is_some()
    );
}

#[tokio::test]
async fn find_circular_imports_empty_on_a_dag() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:module:a", "module", "a.py", Some((1, 5)));
    insert_entity(&conn, "python:module:b", "module", "b.py", Some((1, 5)));
    insert_edge(
        &conn,
        "imports",
        "python:module:a",
        "python:module:b",
        "resolved",
    );
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "find_circular_imports", json!({})).await;
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
}

#[tokio::test]
async fn find_circular_imports_default_confidence_excludes_inferred() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:module:a", "module", "a.py", Some((1, 5)));
    insert_entity(&conn, "python:module:b", "module", "b.py", Some((1, 5)));
    insert_edge(
        &conn,
        "imports",
        "python:module:a",
        "python:module:b",
        "resolved",
    );
    insert_edge(
        &conn,
        "imports",
        "python:module:b",
        "python:module:a",
        "inferred",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    // resolved-only: the inferred back-edge is excluded, so no cycle.
    let env = call_tool(&state, "find_circular_imports", json!({})).await;
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");

    // requesting inferred includes it -> cycle appears.
    let env = call_tool(
        &state,
        "find_circular_imports",
        json!({"confidence": "inferred"}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(env["result"]["confidence"], "inferred");
}

#[tokio::test]
async fn find_circular_imports_ignores_type_only_and_function_local_imports() {
    let (project, db, conn) = open_project();
    for id in ["python:module:a", "python:module:b", "python:module:c"] {
        insert_entity(&conn, id, "module", "a.py", Some((1, 5)));
    }
    insert_edge(
        &conn,
        "imports",
        "python:module:a",
        "python:module:b",
        "resolved",
    );
    insert_edge_with_properties(
        &conn,
        "imports",
        "python:module:b",
        "python:module:a",
        "resolved",
        &json!({"type_only": true}),
    );
    insert_edge(
        &conn,
        "imports",
        "python:module:b",
        "python:module:c",
        "resolved",
    );
    insert_edge_with_properties(
        &conn,
        "imports",
        "python:module:c",
        "python:module:b",
        "resolved",
        &json!({"scope": "function"}),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_circular_imports", json!({})).await;

    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
}

#[tokio::test]
async fn find_coupling_hotspots_ranks_by_fan_in_plus_out() {
    let (project, db, conn) = open_project();
    for id in ["hub", "a", "b", "c"] {
        insert_entity(
            &conn,
            &format!("python:function:{id}"),
            "function",
            "m.py",
            Some((1, 2)),
        );
    }
    // hub is called by a, b, c and calls a -> fan_in 3, fan_out 1, coupling 4.
    insert_edge(
        &conn,
        "calls",
        "python:function:a",
        "python:function:hub",
        "resolved",
    );
    insert_edge(
        &conn,
        "calls",
        "python:function:b",
        "python:function:hub",
        "resolved",
    );
    insert_edge(
        &conn,
        "calls",
        "python:function:c",
        "python:function:hub",
        "resolved",
    );
    insert_edge(
        &conn,
        "calls",
        "python:function:hub",
        "python:function:a",
        "resolved",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_coupling_hotspots", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let top = &env["result"]["hotspots"][0];
    assert_eq!(top["entity"]["id"], "python:function:hub", "{env}");
    assert_eq!(top["fan_in"], 3);
    assert_eq!(top["fan_out"], 1);
    assert_eq!(top["coupling"], 4);
    assert!(top["entity"].get("sei").is_some());
}

#[tokio::test]
async fn find_coupling_hotspots_respects_limit_and_scope() {
    let (project, db, conn) = open_project();
    let auth = project.path().join("src/auth/a.py");
    let other = project.path().join("src/other/b.py");
    insert_entity(
        &conn,
        "python:function:auth.a",
        "function",
        auth.to_str().unwrap(),
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:other.b",
        "function",
        other.to_str().unwrap(),
        Some((1, 2)),
    );
    insert_edge(
        &conn,
        "calls",
        "python:function:auth.a",
        "python:function:other.b",
        "resolved",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    // Scope to src/auth/** -> only auth.a is in scope (fan_out 1).
    let env = call_tool(
        &state,
        "find_coupling_hotspots",
        json!({"scope": "src/auth/**"}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(
        env["result"]["hotspots"][0]["entity"]["id"],
        "python:function:auth.a"
    );
}

// ---- categorisation / churn shortcuts (honest-empty) --------------------

#[tokio::test]
async fn categorisation_shortcuts_are_honest_empty() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);

    for tool in [
        "find_entry_points",
        "find_http_routes",
        "find_data_models",
        "find_tests",
        "find_deprecations",
        "find_todos",
        "high_churn",
    ] {
        let env = call_tool(&state, tool, json!({})).await;
        assert_eq!(env["ok"], true, "{tool}: {env}");
        assert_eq!(env["result"]["page"]["total"], 0, "{tool}: {env}");
        assert_eq!(env["result"]["signal"]["available"], false, "{tool}: {env}");
    }
}

#[tokio::test]
async fn find_tests_lights_up_when_test_tag_is_present() {
    // The query is real: if a plugin ever emits the `test` tag, the tool returns
    // results (this proves it is not a hardcoded empty).
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:test_login",
        "function",
        "t.py",
        Some((1, 2)),
    );
    insert_tag(&conn, "python:function:test_login", "test");
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "find_tests", json!({})).await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(
        env["result"]["entities"][0]["id"],
        "python:function:test_login"
    );
}

#[tokio::test]
async fn what_tests_this_is_honest_empty_without_test_tags() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:target",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:caller",
        "function",
        "m.py",
        Some((3, 4)),
    );
    insert_edge(
        &conn,
        "calls",
        "python:function:caller",
        "python:function:target",
        "resolved",
    );
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(
        &state,
        "what_tests_this",
        json!({"id": "python:function:target"}),
    )
    .await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
    assert_eq!(env["result"]["signal"]["available"], false);
}

#[tokio::test]
async fn what_tests_this_returns_test_tagged_callers() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:target",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:test_target",
        "function",
        "t.py",
        Some((1, 2)),
    );
    insert_edge(
        &conn,
        "calls",
        "python:function:test_target",
        "python:function:target",
        "resolved",
    );
    insert_tag(&conn, "python:function:test_target", "test");
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(
        &state,
        "what_tests_this",
        json!({"id": "python:function:target"}),
    )
    .await;
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");
    assert_eq!(
        env["result"]["tests"][0]["id"],
        "python:function:test_target"
    );
}

#[tokio::test]
async fn recently_changed_is_honest_noop() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(
        &state,
        "recently_changed",
        json!({"since": "2026-01-01T00:00:00Z"}),
    )
    .await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
    assert_eq!(env["result"]["signal"]["signal"], "git_change_time");
}

#[tokio::test]
async fn find_coupling_hotspots_ignores_structural_edges() {
    // Regression (code review): coupling must rank on dependency edges
    // (calls/imports/references), not structural ones (contains/in_subsystem),
    // which would otherwise make containers dominate purely by membership.
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:module:m", "module", "m.py", Some((1, 20)));
    for i in 0..5 {
        let f = format!("python:function:m.f{i}");
        insert_entity(&conn, &f, "function", "m.py", Some((i + 1, i + 2)));
        insert_edge(&conn, "contains", "python:module:m", &f, "resolved");
    }
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "find_coupling_hotspots", json!({})).await;
    // Only structural `contains` edges exist -> no dependency coupling at all.
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
}

#[tokio::test]
async fn guidance_sheet_carries_its_own_sei_not_the_queried_entitys() {
    // Regression (code review): each composed sheet's `sei` is the sheet's own
    // identity, not the queried entity's.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:m.f",
        "function",
        "m.py",
        Some((1, 2)),
    );
    insert_guidance(
        &conn,
        "core:guidance:g",
        r#"{"scope_level":"project","scope_rank":1,"content":"G","authored_at":"2026-01-01",
            "match_rules":[{"type":"kind","value":"function"}]}"#,
    );
    insert_alive_sei(&conn, "loomweave:eid:entitysei", "python:function:m.f");
    insert_alive_sei(&conn, "loomweave:eid:sheetsei", "core:guidance:g");
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "guidance_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(
        env["result"]["entity"]["sei"], "loomweave:eid:entitysei",
        "{env}"
    );
    assert_eq!(
        env["result"]["guidance"][0]["sei"], "loomweave:eid:sheetsei",
        "{env}"
    );
}

// ---- find_dead_code -----------------------------------------------------

fn insert_calls_edge(conn: &Connection, from: &str, to: &str, confidence: &str) {
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence) VALUES ('calls', ?1, ?2, ?3)",
        params![from, to, confidence],
    )
    .expect("insert calls edge");
}

fn insert_ambiguous_calls_edge(conn: &Connection, from: &str, to: &str, candidates: &[&str]) {
    let properties = json!({ "candidates": candidates }).to_string();
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence, properties) \
         VALUES ('calls', ?1, ?2, 'ambiguous', ?3)",
        params![from, to, properties],
    )
    .expect("insert ambiguous calls edge");
}

/// Like [`insert_entity`] but with an explicit `short_name` (the terminal
/// identifier). The dead-code unresolved-call-site shield matches a candidate's
/// `short_name` against unresolved callee leaves, so realistic leaf names
/// (`do_work`, not the full id) are required to exercise it.
fn insert_entity_named(
    conn: &Connection,
    id: &str,
    kind: &str,
    source_path: &str,
    range: Option<(i64, i64)>,
    short_name: &str,
) {
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path, \
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at) \
         VALUES (?1,'rust',?2,?1,?6,?3,?4,?5,'{}','hash','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        params![id, kind, source_path, range.map(|(s, _)| s), range.map(|(_, e)| e), short_name],
    )
    .expect("insert named entity");
}

/// Record an unresolved call site whose caller is content-current (hash `hash`,
/// matching [`insert_entity`] / [`insert_entity_named`]), so the dead-code
/// staleness join keeps it. `callee_expr` is the recorded form (`.foo` for a
/// method call, `Type::assoc` for an associated call).
fn insert_unresolved_site(conn: &Connection, caller_id: &str, site_key: &str, callee_expr: &str) {
    conn.execute(
        "INSERT INTO entity_unresolved_call_sites ( \
            caller_entity_id, caller_content_hash, site_key, site_ordinal, \
            source_byte_start, source_byte_end, callee_expr, created_at \
         ) VALUES (?1, 'hash', ?2, 0, 10, 20, ?3, '2026-01-01T00:00:00.000Z')",
        params![caller_id, site_key, callee_expr],
    )
    .expect("insert unresolved site");
}

// clarion-… consumer honesty: a function reached ONLY via an unresolved call
// site (a method `x.do_work()` or associated `Svc::make()` call the Rust
// resolver could not bind — no `calls` edge) has no incoming edge, so pure
// static reachability would false-flag it dead. The dead-code tool must spare
// it (fail toward live) and disclose the suppression count.
#[tokio::test]
async fn find_dead_code_spares_fn_reached_only_via_unresolved_call_site() {
    let (project, db, conn) = open_project();
    // Root so the reachability root set is non-empty (else signal-unavailable).
    // Inserted as a RUST-plugin entity (matching the candidates below) so the
    // rust plugin has root coverage and its entities are surveyed — the
    // per-plugin honest-exclusion path (weft-3fb0f5dfc7) is exercised by its
    // own test.
    insert_entity_named(
        &conn,
        "rust:function:app.main",
        "function",
        "app.rs",
        Some((1, 5)),
        "main",
    );
    insert_tag(&conn, "rust:function:app.main", "entry-point");
    // Reached ONLY via an unresolved method call `.do_work` — no edge.
    insert_entity_named(
        &conn,
        "rust:function:app.Svc.impl.do_work",
        "function",
        "app.rs",
        Some((6, 9)),
        "do_work",
    );
    insert_unresolved_site(&conn, "rust:function:app.main", "s0", ".do_work");
    // Reached ONLY via an unresolved associated call `Svc::make` — no edge.
    insert_entity_named(
        &conn,
        "rust:function:app.Svc.impl.make",
        "function",
        "app.rs",
        Some((10, 12)),
        "make",
    );
    insert_unresolved_site(&conn, "rust:function:app.main", "s1", "Svc::make");
    // Genuinely dead — no edge and no unresolved site names it.
    insert_entity_named(
        &conn,
        "rust:function:app.orphan",
        "function",
        "app.rs",
        Some((13, 15)),
        "orphan",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        dead,
        vec!["rust:function:app.orphan".to_owned()],
        "method/assoc-only-called fns must be spared; only the true orphan is dead: {env}"
    );
    assert_eq!(
        env["result"]["unresolved_call_site_suppressed"], 2,
        "the two unresolved-call-site shields must be disclosed: {env}"
    );
}

#[test]
fn tools_list_includes_find_dead_code() {
    let names: Vec<&str> = list_tools().iter().map(|t| t.name).collect();
    assert!(
        names.contains(&"entity_dead_list"),
        "missing entity_dead_list"
    );
}

// Safety case (and the catastrophe guard): with no reachability roots emitted,
// the tool must NOT flag every entity as dead — it returns an honest
// signal-unavailable with zero candidates.
#[tokio::test]
async fn find_dead_code_signal_unavailable_when_no_roots() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((1, 5)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["signal"]["available"], false, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
    assert!(
        env["result"]["dead_code"].as_array().unwrap().is_empty(),
        "{env}"
    );
}

// Conservative reachability: a genuinely dead leaf is flagged; an
// ambiguous-edge target is spared (all tiers count as reachable); a
// reflection/dynamic-dispatch barrier-tagged entity is spared.
#[tokio::test]
async fn find_dead_code_flags_unreachable_and_spares_live() {
    let (project, db, conn) = open_project();
    // Root (entry point) — seeded live.
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    // Reachable from the root over a resolved call edge.
    insert_entity(
        &conn,
        "python:function:helper",
        "function",
        "app.py",
        Some((6, 10)),
    );
    insert_calls_edge(
        &conn,
        "python:function:main",
        "python:function:helper",
        "resolved",
    );
    // Reachable only via an AMBIGUOUS edge — must NOT be flagged (fail toward live).
    insert_entity(
        &conn,
        "python:function:maybe",
        "function",
        "app.py",
        Some((11, 15)),
    );
    insert_entity(
        &conn,
        "python:function:maybe_other",
        "function",
        "app.py",
        Some((16, 17)),
    );
    insert_ambiguous_calls_edge(
        &conn,
        "python:function:helper",
        "python:function:maybe",
        &["python:function:maybe", "python:function:maybe_other"],
    );
    // Reflectively reached: no static edge, but barrier-tagged → live.
    insert_entity(
        &conn,
        "python:function:reflected",
        "function",
        "app.py",
        Some((18, 20)),
    );
    insert_tag(&conn, "python:function:reflected", "dynamic-dispatch");
    // Genuinely dead leaf.
    insert_entity(
        &conn,
        "python:function:unused",
        "function",
        "app.py",
        Some((21, 25)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(dead, vec!["python:function:unused".to_owned()], "{env}");
    assert_eq!(env["result"]["page"]["total"], 1, "{env}");

    // The candidate facets are hoisted to the top-level `finding` block
    // (weft-3fb0f5dfc7 — they used to repeat verbatim on every row).
    let finding = &env["result"]["finding"];
    assert_eq!(finding["rule_id"], "LMWV-FACT-DEAD-CODE-CANDIDATE", "{env}");
    assert_eq!(finding["kind"], "fact", "{env}");
    assert_eq!(finding["confidence_basis"], "heuristic", "{env}");
    assert!(
        finding["confidence"].as_f64().unwrap() < 1.0,
        "heuristic confidence must be < 1: {env}"
    );
    let candidate = &env["result"]["dead_code"][0];
    assert!(
        candidate["entity"]["sei"].is_null() || candidate["entity"]["sei"].is_string(),
        "candidate carries an sei field: {env}"
    );
}

// clarion-bf496d55d1 §4.2: a Wardline-derived trust-boundary tag
// (`wardline:external_boundary` / `wardline:trusted`, emitted by the Python
// plugin from the on-disk Wardline vocabulary descriptor) acts as a reachability
// root, so a statically-unreached but human-annotated trust boundary is NOT
// flagged dead, while an untagged unreached entity still is.
#[tokio::test]
async fn find_dead_code_treats_wardline_trust_boundaries_as_roots() {
    let (project, db, conn) = open_project();
    // An externally-invoked boundary: unreached over static edges, but annotated
    // @external_boundary -> wardline:external_boundary. Must be a root, not dead.
    insert_entity(
        &conn,
        "python:function:webhook",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(
        &conn,
        "python:function:webhook",
        "wardline:external_boundary",
    );
    // A trusted producer: @trusted -> wardline:trusted. Also a root.
    insert_entity(
        &conn,
        "python:function:mint_token",
        "function",
        "app.py",
        Some((6, 10)),
    );
    insert_tag(&conn, "python:function:mint_token", "wardline:trusted");
    // Genuinely dead leaf — unreached and untagged.
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((11, 15)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        dead,
        vec!["python:function:orphan".to_owned()],
        "only the untagged unreached entity is dead; the Wardline trust \
         boundaries are roots: {env}"
    );
}

// Framework-magic entities (decorated handlers, plugin hooks) are excluded from
// candidacy even when statically unreached.
#[tokio::test]
async fn find_dead_code_excludes_framework_magic() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    // Unreached, but a framework handler — excluded from candidacy.
    insert_entity(
        &conn,
        "python:function:on_event",
        "function",
        "app.py",
        Some((6, 10)),
    );
    insert_tag(&conn, "python:function:on_event", "framework-handler");
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
}

// ---- search_semantic ----------------------------------------------------

#[test]
fn tools_list_includes_search_semantic() {
    let names: Vec<&str> = list_tools().iter().map(|t| t.name).collect();
    assert!(
        names.contains(&"entity_semantic_search_list"),
        "missing entity_semantic_search_list"
    );
}

// Off by default: honest "not enabled", never a fabricated result.
#[tokio::test]
async fn search_semantic_disabled_returns_not_enabled() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:f", "function", "f.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "search_semantic", json!({"query": "auth"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["result_kind"], "not_enabled", "{env}");
    assert_eq!(env["result"]["signal"]["available"], false, "{env}");
    assert!(
        env["result"]["results"].as_array().unwrap().is_empty(),
        "{env}"
    );
}

// Enabled: ranks entities by cosine similarity to the query embedding, using
// only sidecar vectors whose content_hash matches the entity's current hash.
#[tokio::test]
async fn search_semantic_ranks_by_cosine_similarity() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:login",
        "function",
        "auth.py",
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:add",
        "function",
        "math.py",
        Some((3, 4)),
    );
    // A stale embedding (content_hash mismatch) must be ignored.
    insert_entity(
        &conn,
        "python:function:stale",
        "function",
        "old.py",
        Some((5, 6)),
    );
    drop(conn);

    let now = "2026-01-01T00:00:00.000Z";
    let store = EmbeddingStore::open_in_store_dir(project.path()).expect("open sidecar");
    let mk = |id: &str, hash: &str| EmbeddingKey {
        entity_id: id.to_owned(),
        content_hash: hash.to_owned(),
        model_id: "rec-model".to_owned(),
    };
    // insert_entity sets content_hash = "hash".
    store
        .upsert(
            &mk("python:function:login", "hash"),
            &[1.0, 0.0],
            0.0,
            1,
            now,
        )
        .expect("u login");
    store
        .upsert(&mk("python:function:add", "hash"), &[0.0, 1.0], 0.0, 1, now)
        .expect("u add");
    store
        .upsert(
            &mk("python:function:stale", "STALE"),
            &[1.0, 0.0],
            0.0,
            1,
            now,
        )
        .expect("u stale");
    drop(store);

    let provider = Arc::new(RecordingEmbeddingProvider::from_recordings(
        "rec-model",
        2,
        vec![EmbeddingRecording {
            text: "authenticate user".to_owned(),
            vector: vec![0.9, 0.1],
        }],
    ));
    let config = SemanticSearchConfig {
        enabled: true,
        model_id: "rec-model".to_owned(),
        dimensions: 2,
        ..SemanticSearchConfig::default()
    };
    let state = state_for(project.path(), &db).with_semantic_search(config, provider);

    let env = call_tool(
        &state,
        "search_semantic",
        json!({"query": "authenticate user"}),
    )
    .await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["result_kind"], "ranked", "{env}");
    let results = env["result"]["results"].as_array().unwrap();
    // stale (content_hash mismatch) is excluded; login + add remain.
    assert_eq!(results.len(), 2, "{env}");
    assert_eq!(results[0]["entity"]["id"], "python:function:login", "{env}");
    assert!(
        results[0]["score"].as_f64().unwrap() > results[1]["score"].as_f64().unwrap(),
        "login should outrank add: {env}"
    );
}

// ---- briefing_blocked identity gate (clarion-307668e2be) ----------------

/// Like [`insert_entity`] but marks the row briefing-blocked. `content_hash` is
/// kept identical to [`insert_entity`] ("hash") so embedding keys still match.
fn insert_blocked_entity(
    conn: &Connection,
    id: &str,
    kind: &str,
    source_path: &str,
    range: Option<(i64, i64)>,
    reason: &str,
) {
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path, \
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at) \
         VALUES (?1,'python',?2,?1,?1,?3,?4,?5,?6,'hash','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        params![
            id,
            kind,
            source_path,
            range.map(|(s, _)| s),
            range.map(|(_, e)| e),
            json!({"briefing_blocked": reason}).to_string(),
        ],
    )
    .expect("insert blocked entity");
}

/// Assert a briefing-blocked entity projection keeps its navigable identity
/// (clarion-719e7320f5, A3): `id`, `kind`, `name`, `short_name`,
/// `source_file_path`, the line span and `content_hash` are PRESENT alongside
/// the `briefing_blocked` flag, so the entity stays navigable; only the secret
/// content is withheld, and the cross-tool SEI binding key stays null. The id is
/// the qualname-bearing locator the caller pasted, so it appears verbatim.
fn assert_blocked_identity_present(entity: &Value, reason: &str) {
    assert_eq!(entity["briefing_blocked"], reason, "block reason: {entity}");
    for field in [
        "id",
        "kind",
        "name",
        "short_name",
        "source_file_path",
        "source_line_start",
        "source_line_end",
        "content_hash",
    ] {
        assert!(
            entity.get(field).is_some_and(|v| !v.is_null()),
            "identity field `{field}` must be PRESENT for a blocked entity: {entity}"
        );
    }
    assert!(
        entity["sei"].is_null(),
        "SEI must stay null for a blocked entity (ADR-034): {entity}"
    );
    // The secret *content* never appears in the entity projection.
    for leaked in ["summary", "source", "docstring"] {
        assert!(
            entity.get(leaked).is_none_or(Value::is_null),
            "content field `{leaked}` must not appear in a blocked entity projection: {entity}"
        );
    }
}

#[tokio::test]
async fn find_by_kind_redacts_briefing_blocked_identity() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:visible",
        "function",
        "a.py",
        Some((1, 2)),
    );
    insert_blocked_entity(
        &conn,
        "python:function:leaky",
        "function",
        "b.py",
        Some((3, 4)),
        "secret_present",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_by_kind", json!({"kind": "function"})).await;
    assert_eq!(env["ok"], true, "{env}");
    // Both functions counted; the blocked one is a stub, not omitted.
    assert_eq!(env["result"]["page"]["total"], 2, "{env}");
    let blocked = env["result"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["briefing_blocked"] == "secret_present")
        .expect("blocked entity stub present");
    assert_blocked_identity_present(blocked, "secret_present");
    // The navigable locator IS exposed now (A3): the identity is not the secret.
    assert_eq!(blocked["id"], "python:function:leaky", "{env}");
}

#[tokio::test]
async fn search_semantic_redacts_briefing_blocked_identity() {
    let (project, db, conn) = open_project();
    insert_blocked_entity(
        &conn,
        "python:function:login",
        "function",
        "auth.py",
        Some((1, 2)),
        "secret_present",
    );
    drop(conn);

    let now = "2026-01-01T00:00:00.000Z";
    let store = EmbeddingStore::open_in_store_dir(project.path()).expect("open sidecar");
    let key = EmbeddingKey {
        entity_id: "python:function:login".to_owned(),
        content_hash: "hash".to_owned(),
        model_id: "rec-model".to_owned(),
    };
    store
        .upsert(&key, &[1.0, 0.0], 0.0, 1, now)
        .expect("upsert login embedding");
    drop(store);

    let provider = Arc::new(RecordingEmbeddingProvider::from_recordings(
        "rec-model",
        2,
        vec![EmbeddingRecording {
            text: "authenticate user".to_owned(),
            vector: vec![0.9, 0.1],
        }],
    ));
    let config = SemanticSearchConfig {
        enabled: true,
        model_id: "rec-model".to_owned(),
        dimensions: 2,
        ..SemanticSearchConfig::default()
    };
    let state = state_for(project.path(), &db).with_semantic_search(config, provider);

    let env = call_tool(
        &state,
        "search_semantic",
        json!({"query": "authenticate user"}),
    )
    .await;
    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{env}");
    assert_blocked_identity_present(&results[0]["entity"], "secret_present");
    assert_eq!(results[0]["entity"]["id"], "python:function:login", "{env}");
}

#[tokio::test]
async fn find_by_wardline_redacts_blocked_entity_and_withholds_blob() {
    let (project, db, conn) = open_project();
    insert_blocked_entity(
        &conn,
        "python:function:tainted",
        "function",
        "t.py",
        Some((1, 2)),
        "secret_present",
    );
    // The Wardline taint blob embeds a qualname — which would survive the
    // identity stub if attached.
    insert_taint_fact(
        &conn,
        "python:function:tainted",
        &json!({"qualname": "app.secrets.tainted", "tier": "high"}).to_string(),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_by_wardline", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let blocked = env["result"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["briefing_blocked"] == "secret_present")
        .expect("blocked entity stub present");
    assert_blocked_identity_present(blocked, "secret_present");
    // The entity's own navigable locator IS exposed now (A3).
    assert_eq!(blocked["id"], "python:function:tainted", "{env}");
    // …but the Wardline taint blob is source-derived *content*, so it stays
    // withheld: the blob's embedded qualname must never leak.
    assert!(
        blocked["wardline"].is_null(),
        "wardline blob must stay withheld for a blocked entity: {blocked}"
    );
    assert!(
        !env.to_string().contains("app.secrets.tainted"),
        "qualname leaked via the wardline blob: {env}"
    );
}

#[tokio::test]
async fn coupling_hotspots_blocked_entity_keeps_navigable_identity() {
    // clarion-719e7320f5 (A3): a briefing-blocked entity ranked into the
    // coupling hotspots keeps its navigable identity (id/name/path/lines/hash)
    // alongside the `briefing_blocked` flag — `project_finding_list` already
    // prints those same paths, so the identity is not the secret.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:caller",
        "function",
        "a.py",
        Some((1, 2)),
    );
    insert_blocked_entity(
        &conn,
        "python:function:hub",
        "function",
        "hub.py",
        Some((3, 9)),
        "secret_present",
    );
    insert_edge(
        &conn,
        "calls",
        "python:function:caller",
        "python:function:hub",
        "resolved",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_coupling_hotspots", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let blocked = env["result"]["hotspots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| &h["entity"])
        .find(|e| e["briefing_blocked"] == "secret_present")
        .expect("blocked hotspot present with identity");
    assert_blocked_identity_present(blocked, "secret_present");
    assert_eq!(blocked["id"], "python:function:hub", "{env}");
    assert_eq!(blocked["source_file_path"], "hub.py", "{env}");
    assert_eq!(blocked["source_line_start"], 3, "{env}");
    assert_eq!(blocked["source_line_end"], 9, "{env}");
}

#[tokio::test]
async fn briefing_blocked_high_entropy_name_field_is_re_withheld() {
    // The A3 guard: in the rare case where the name/id is ITSELF a high-entropy
    // token (a generated symbol embedding a secret), that one field is
    // re-withheld while the rest of the identity still rides along.
    let (project, db, conn) = open_project();
    // A long base64/hex-like high-entropy qualname (>= the entropy threshold).
    let secret_name = "fn_aGVsbG8gd29ybGQgc2VjcmV0IGtleSBhYmMxMjP8x9z";
    let id = format!("python:function:{secret_name}");
    insert_blocked_entity(
        &conn,
        &id,
        "function",
        "g.py",
        Some((1, 2)),
        "secret_present",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_by_kind", json!({"kind": "function"})).await;
    assert_eq!(env["ok"], true, "{env}");
    let blocked = env["result"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["briefing_blocked"] == "secret_present")
        .expect("blocked entity present");
    // The high-entropy id/name/short_name are re-withheld…
    assert!(
        blocked["id"].is_null(),
        "high-entropy id must be re-withheld: {env}"
    );
    assert!(blocked["name"].is_null(), "{env}");
    assert!(blocked["short_name"].is_null(), "{env}");
    // …but the non-secret structural identity still rides along.
    assert_eq!(blocked["kind"], "function", "{env}");
    assert_eq!(blocked["source_file_path"], "g.py", "{env}");
    assert_eq!(blocked["source_line_start"], 1, "{env}");
    // The secret-bearing qualname must not leak anywhere.
    assert!(
        !env.to_string().contains(secret_name),
        "high-entropy secret name leaked: {env}"
    );
}

// ---- entity_resolve (Item 2, clarion-d76e7f7267) ------------------------

#[tokio::test]
async fn entity_resolve_resolves_qualname_to_id_and_sei() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:demo.entry",
        "function",
        "demo.py",
        Some((1, 2)),
    );
    insert_alive_sei(
        &conn,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["demo.entry"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["qualname"], "demo.entry");
    assert_eq!(results[0]["result_kind"], "resolved");
    let candidates = results[0]["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["id"], "python:function:demo.entry");
    assert_eq!(candidates[0]["sei"], "loomweave:eid:demo-entry");
    assert_eq!(candidates[0]["kind"], "function");
}

// ---- A2 inline tags + arg aliases (clarion-057ff2b330) -----------------

#[tokio::test]
async fn entity_resolve_candidate_carries_inline_tags_sorted() {
    // A2: the shared entity-row projection inlines the entity's own tags so an
    // agent sees them without a reverse-index `entity_tag_list` round-trip. Tags
    // arrive deduplicated and sorted for determinism.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:demo.entry",
        "function",
        "demo.py",
        Some((1, 2)),
    );
    insert_tag(&conn, "python:function:demo.entry", "test");
    insert_tag(&conn, "python:function:demo.entry", "entry-point");
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["demo.entry"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let candidates = env["result"]["results"][0]["candidates"]
        .as_array()
        .expect("candidates");
    assert_eq!(
        candidates[0]["tags"],
        json!(["entry-point", "test"]),
        "tags must be inline, deduplicated, and sorted: {env}"
    );
}

#[tokio::test]
async fn entity_row_tags_default_to_empty_array_when_untagged() {
    // An untagged entity carries `tags: []`, not a missing field, so a client
    // can read the key unconditionally.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:demo.entry",
        "function",
        "demo.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["demo.entry"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let candidates = env["result"]["results"][0]["candidates"]
        .as_array()
        .expect("candidates");
    assert_eq!(candidates[0]["tags"], json!([]), "{env}");
}

#[tokio::test]
async fn entity_resolve_accepts_identifiers_alias() {
    // A2: `identifiers` is a pure synonym for `qualnames`.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:demo.entry",
        "function",
        "demo.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let via_alias = call_tool(
        &state,
        "entity_resolve",
        json!({"identifiers": ["demo.entry"]}),
    )
    .await;
    assert_eq!(via_alias["ok"], true, "{via_alias}");
    assert_eq!(
        via_alias["result"]["results"][0]["candidates"][0]["id"],
        "python:function:demo.entry"
    );
}

#[tokio::test]
async fn entity_resolve_qualnames_wins_when_both_present() {
    // `qualnames` takes precedence over `identifiers` for backward compatibility.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:demo.entry",
        "function",
        "demo.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({
            "qualnames": ["demo.entry"],
            "identifiers": ["no.such.thing"],
        }),
    )
    .await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["results"][0]["qualname"], "demo.entry");
    assert_eq!(env["result"]["results"][0]["result_kind"], "resolved");
}

#[tokio::test]
async fn entity_resolve_full_locator_input_resolves_to_sei_gv_lw_5() {
    // GV-LW-5 (warpline interface-lock 2026-06-13, HX1): warpline resolves SEIs by
    // passing a fully-formed Loomweave locator (`python:function:m.f`), not the
    // bare qualname. It must resolve to the real `loomweave:eid:` SEI so warpline
    // stores `sei` + `enrichment.sei: present` instead of `sei: null`.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:demo.entry",
        "function",
        "demo.py",
        Some((1, 2)),
    );
    insert_alive_sei(
        &conn,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["python:function:demo.entry"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["qualname"], "python:function:demo.entry");
    assert_eq!(results[0]["result_kind"], "resolved");
    let candidates = results[0]["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["id"], "python:function:demo.entry");
    assert_eq!(candidates[0]["sei"], "loomweave:eid:demo-entry");
}

#[tokio::test]
async fn entity_resolve_unknown_qualname_is_unresolved_not_error() {
    let (project, db, _conn) = open_project();
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["no.such.thing"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "honest-empty, not an error: {env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["result_kind"], "unresolved");
    assert_eq!(
        results[0]["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        0
    );
}

#[tokio::test]
async fn entity_resolve_preserves_input_order_across_batch() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:a.one",
        "function",
        "a.py",
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:b.two",
        "function",
        "b.py",
        Some((1, 2)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    // Order: known, unknown, known — must echo back in exactly this order.
    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["b.two", "missing.x", "a.one"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["qualname"], "b.two");
    assert_eq!(results[0]["result_kind"], "resolved");
    assert_eq!(results[1]["qualname"], "missing.x");
    assert_eq!(results[1]["result_kind"], "unresolved");
    assert_eq!(results[2]["qualname"], "a.one");
    assert_eq!(results[2]["candidates"][0]["id"], "python:function:a.one");
}

#[tokio::test]
async fn entity_resolve_over_cap_is_param_error() {
    let (project, db, _conn) = open_project();
    let state = state_for(project.path(), &db);

    // A ParamError surfaces as a JSON-RPC error (-32602), not a tool envelope,
    // so drive handle_json_rpc directly.
    let qualnames: Vec<String> = (0..2001).map(|i| format!("q.{i}")).collect();
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "over-cap",
            "method": "tools/call",
            "params": {"name": "entity_resolve", "arguments": {"qualnames": qualnames}}
        }))
        .await
        .expect("response");

    assert_eq!(
        response["error"]["code"], -32602,
        "over-cap must be a JSON-RPC param error: {response}"
    );
}

#[tokio::test]
async fn entity_resolve_collapses_briefing_blocked_candidate_to_stub() {
    // Landmine #9: a blocked entity's candidate must collapse to the stub —
    // routing through entity_json — so the reverse-map never discloses a
    // secret-scan-blocked locator.
    let (project, db, conn) = open_project();
    insert_blocked_entity(
        &conn,
        "python:function:secret.handler",
        "function",
        "secret.py",
        Some((1, 2)),
        "secret_in_source",
    );
    insert_alive_sei(
        &conn,
        "loomweave:eid:secret",
        "python:function:secret.handler",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["secret.handler"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "resolved");
    let candidate = &results[0]["candidates"][0];
    assert_blocked_identity_present(candidate, "secret_in_source");
    // The navigable locator IS exposed now (A3); the cross-tool SEI stays null.
    assert_eq!(candidate["id"], "python:function:secret.handler", "{env}");
    assert!(
        !env.to_string().contains("loomweave:eid:secret"),
        "blocked SEI leaked via entity_resolve: {env}"
    );
}

#[tokio::test]
async fn entity_resolve_resolves_rust_qualname_to_id_and_sei() {
    // clarion-69db8b2739: per-plugin candidate minting resolves a Rust
    // qualname to its `rust:function:` id (no longer python-only).
    let (project, db, conn) = open_project();
    insert_entity_named(
        &conn,
        "rust:function:mcp_fixture.ops.entry",
        "function",
        "ops.rs",
        Some((1, 2)),
        "entry",
    );
    insert_alive_sei(
        &conn,
        "loomweave:eid:rust-entry",
        "rust:function:mcp_fixture.ops.entry",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["mcp_fixture.ops.entry"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["result_kind"], "resolved");
    let candidates = results[0]["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["id"], "rust:function:mcp_fixture.ops.entry");
    assert_eq!(candidates[0]["sei"], "loomweave:eid:rust-entry");
    assert_eq!(candidates[0]["kind"], "function");
}

#[tokio::test]
async fn entity_resolve_same_qualname_under_two_plugins_is_ambiguous() {
    // The same dotted qualname under both plugins surfaces as result_kind
    // "ambiguous" with BOTH candidates (sorted: python < rust).
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:dual.target",
        "function",
        "dual.py",
        Some((1, 2)),
    );
    insert_entity_named(
        &conn,
        "rust:function:dual.target",
        "function",
        "dual.rs",
        Some((1, 2)),
        "target",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["dual.target"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["result_kind"], "ambiguous");
    let candidates = results[0]["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 2);
    let ids: Vec<&str> = candidates
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["python:function:dual.target", "rust:function:dual.target"],
        "both candidates, sorted: {env}"
    );
}

#[tokio::test]
async fn entity_resolve_collapses_briefing_blocked_rust_candidate_to_stub() {
    // The stub-collapse non-disclosure property applies to RUST candidates too:
    // a briefing-blocked rust:function entity must come back as the redacted
    // stub, never leaking its locator or SEI.
    let (project, db, conn) = open_project();
    insert_blocked_entity(
        &conn,
        "rust:function:secret.rust_handler",
        "function",
        "secret.rs",
        Some((1, 2)),
        "secret_in_source",
    );
    // Mark plugin_id=rust so per-plugin candidate minting enumerates it.
    conn.execute(
        "UPDATE entities SET plugin_id = 'rust' WHERE id = ?1",
        params!["rust:function:secret.rust_handler"],
    )
    .expect("set plugin_id");
    insert_alive_sei(
        &conn,
        "loomweave:eid:rust-secret",
        "rust:function:secret.rust_handler",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["secret.rust_handler"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "resolved");
    let candidate = &results[0]["candidates"][0];
    assert_blocked_identity_present(candidate, "secret_in_source");
    // The navigable locator IS exposed now (A3); the cross-tool SEI stays null.
    assert_eq!(
        candidate["id"], "rust:function:secret.rust_handler",
        "{env}"
    );
    assert!(
        !env.to_string().contains("loomweave:eid:rust-secret"),
        "blocked SEI leaked via entity_resolve: {env}"
    );
}

#[tokio::test]
async fn entity_resolve_rejects_blank_kind_and_blank_plugin() {
    // clarion-c2bb394f46: `kind` and `plugin` are free-form constraints, but a
    // BLANK value is a caller bug — JSON-RPC param error (-32602), mirroring
    // the HTTP layer's blank-rejection adjudication (ADR-036 plugin hint).
    let (project, db, _conn) = open_project();
    let state = state_for(project.path(), &db);

    for (param, value) in [("kind", "  "), ("plugin", "")] {
        let response = state
            .handle_json_rpc(&json!({
                "jsonrpc": "2.0",
                "id": "blank-param",
                "method": "tools/call",
                "params": {
                    "name": "entity_resolve",
                    "arguments": {"qualnames": ["demo.entry"], param: value}
                }
            }))
            .await
            .expect("response");
        assert_eq!(
            response["error"]["code"], -32602,
            "blank {param} must be a JSON-RPC param error: {response}"
        );
    }
}

// ---- entity_resolve all-kinds + SEI + plugin hint (clarion-c2bb394f46) ----

#[tokio::test]
async fn entity_resolve_resolves_class_qualname_to_id_and_sei() {
    // The reverse-map is no longer function-only: a class qualname resolves to
    // its identity row.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:class:demo.Widget",
        "class",
        "demo.py",
        Some((1, 9)),
    );
    insert_alive_sei(
        &conn,
        "loomweave:eid:demo-widget",
        "python:class:demo.Widget",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["demo.Widget"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "resolved");
    let candidate = &results[0]["candidates"][0];
    assert_eq!(candidate["id"], "python:class:demo.Widget");
    assert_eq!(candidate["sei"], "loomweave:eid:demo-widget");
    assert_eq!(candidate["kind"], "class");
}

#[tokio::test]
async fn entity_resolve_resolves_rust_struct_qualname() {
    let (project, db, conn) = open_project();
    insert_entity_named(
        &conn,
        "rust:struct:mcp_fixture.ops.Widget",
        "struct",
        "ops.rs",
        Some((1, 9)),
        "Widget",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["mcp_fixture.ops.Widget"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "resolved");
    assert_eq!(
        results[0]["candidates"][0]["id"],
        "rust:struct:mcp_fixture.ops.Widget"
    );
}

#[tokio::test]
async fn entity_resolve_cross_kind_collision_is_ambiguous_and_kind_constrains() {
    // The same qualname as a python function AND class: honest ambiguous by
    // default (sorted class < function); kind="class" collapses it; an unknown
    // kind is a constraint nothing satisfies (unresolved, not an error).
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:demo.thing",
        "function",
        "demo.py",
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:class:demo.thing",
        "class",
        "demo.py",
        Some((4, 9)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["demo.thing"]}),
    )
    .await;
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "ambiguous", "{env}");
    let ids: Vec<&str> = results[0]["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["python:class:demo.thing", "python:function:demo.thing"],
        "both kinds, sorted: {env}"
    );

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["demo.thing"], "kind": "class"}),
    )
    .await;
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "resolved", "{env}");
    assert_eq!(results[0]["candidates"][0]["id"], "python:class:demo.thing");

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["demo.thing"], "kind": "nosuch"}),
    )
    .await;
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(
        results[0]["result_kind"], "unresolved",
        "unknown kind is honest-empty, not an error: {env}"
    );
}

#[tokio::test]
async fn entity_resolve_plugin_hint_constrains_cross_plugin_collision() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:dual.target",
        "function",
        "dual.py",
        Some((1, 2)),
    );
    insert_entity_named(
        &conn,
        "rust:function:dual.target",
        "function",
        "dual.rs",
        Some((1, 2)),
        "target",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["dual.target"], "plugin": "python"}),
    )
    .await;
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "resolved", "{env}");
    assert_eq!(
        results[0]["candidates"][0]["id"],
        "python:function:dual.target"
    );

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["dual.target"], "plugin": "cobol"}),
    )
    .await;
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(
        results[0]["result_kind"], "unresolved",
        "unknown plugin is a constraint nothing satisfies: {env}"
    );
}

#[tokio::test]
async fn entity_resolve_accepts_sei_entry_ignoring_constraints() {
    // An SEI token in the batch is an exact identity lookup: it resolves to
    // its alive entity row, and kind/plugin constraints do NOT apply (an SEI
    // is already exact — constraining it can only manufacture a false miss).
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:demo.entry",
        "function",
        "demo.py",
        Some((1, 2)),
    );
    insert_alive_sei(
        &conn,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["loomweave:eid:demo-entry"], "kind": "class"}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(
        results[0]["qualname"], "loomweave:eid:demo-entry",
        "echoes the input as given: {env}"
    );
    assert_eq!(results[0]["result_kind"], "resolved", "{env}");
    let candidate = &results[0]["candidates"][0];
    assert_eq!(candidate["id"], "python:function:demo.entry");
    assert_eq!(candidate["sei"], "loomweave:eid:demo-entry");
}

#[tokio::test]
async fn entity_resolve_mixed_sei_and_qualname_batch_preserves_order() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:a.one",
        "function",
        "a.py",
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:b.two",
        "function",
        "b.py",
        Some((1, 2)),
    );
    insert_alive_sei(&conn, "loomweave:eid:a-one", "python:function:a.one");
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["b.two", "loomweave:eid:a-one", "missing.x"]}),
    )
    .await;

    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["qualname"], "b.two");
    assert_eq!(results[0]["candidates"][0]["id"], "python:function:b.two");
    assert_eq!(results[1]["qualname"], "loomweave:eid:a-one");
    assert_eq!(results[1]["candidates"][0]["id"], "python:function:a.one");
    assert_eq!(results[2]["qualname"], "missing.x");
    assert_eq!(results[2]["result_kind"], "unresolved");
}

#[tokio::test]
async fn entity_resolve_unknown_sei_is_unresolved_not_error() {
    let (project, db, _conn) = open_project();
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["loomweave:eid:never-minted"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "honest-empty, not an error: {env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "unresolved");
    assert_eq!(
        results[0]["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        0
    );
}

#[tokio::test]
async fn entity_resolve_blocked_sei_entry_collapses_to_stub() {
    // The non-disclosure property holds on the SEI path too: an SEI whose
    // entity is secret-scan-blocked resolves to the redacted stub, never
    // leaking the locator.
    let (project, db, conn) = open_project();
    insert_blocked_entity(
        &conn,
        "python:function:secret.handler",
        "function",
        "secret.py",
        Some((1, 2)),
        "secret_in_source",
    );
    insert_alive_sei(
        &conn,
        "loomweave:eid:secret",
        "python:function:secret.handler",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["loomweave:eid:secret"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results[0]["result_kind"], "resolved");
    let candidate = &results[0]["candidates"][0];
    assert_blocked_identity_present(candidate, "secret_in_source");
    // The navigable locator IS exposed now (A3); the resolving SEI was the
    // caller's own input, so it may echo, but the candidate row's SEI is null.
    assert_eq!(candidate["id"], "python:function:secret.handler", "{env}");
    assert!(
        candidate["sei"].is_null(),
        "candidate SEI must be null: {env}"
    );
}

#[tokio::test]
async fn entity_resolve_normalizes_rust_path_separator() {
    // MCP-audit F6 acceptance criterion: a pasted Rust `::` path (stack trace,
    // compiler error, rustdoc) resolves — `::` normalizes to `.` for
    // resolution while the result echoes the input as given. Storage stays
    // byte-exact; normalization is this tool's input courtesy only.
    let (project, db, conn) = open_project();
    insert_entity_named(
        &conn,
        "rust:function:mcp_fixture.ops.entry",
        "function",
        "ops.rs",
        Some((1, 2)),
        "entry",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["mcp_fixture::ops::entry"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(
        results[0]["qualname"], "mcp_fixture::ops::entry",
        "echoes the :: form as pasted: {env}"
    );
    assert_eq!(results[0]["result_kind"], "resolved", "{env}");
    assert_eq!(
        results[0]["candidates"][0]["id"],
        "rust:function:mcp_fixture.ops.entry"
    );
}

#[tokio::test]
async fn entity_resolve_ambiguous_with_blocked_candidate_redacts_only_that_candidate() {
    // Composition: a dual-plugin qualname where ONE candidate is
    // briefing-blocked. result_kind stays "ambiguous" (both rows survive), the
    // normal candidate keeps its identity, and the blocked one collapses to
    // the redacted stub — its locator and SEI never appear anywhere in the
    // envelope.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:dual.secret",
        "function",
        "dual.py",
        Some((1, 2)),
    );
    insert_blocked_entity(
        &conn,
        "rust:function:dual.secret",
        "function",
        "secret.rs",
        Some((1, 2)),
        "secret_in_source",
    );
    // Mark plugin_id=rust so per-plugin candidate minting enumerates it.
    conn.execute(
        "UPDATE entities SET plugin_id = 'rust' WHERE id = ?1",
        params!["rust:function:dual.secret"],
    )
    .expect("set plugin_id");
    insert_alive_sei(
        &conn,
        "loomweave:eid:dual-secret",
        "rust:function:dual.secret",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["dual.secret"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["result_kind"], "ambiguous", "{env}");
    let candidates = results[0]["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 2, "both candidates kept: {env}");
    // Sorted python < rust: the python candidate comes first, identity intact…
    assert_eq!(candidates[0]["id"], "python:function:dual.secret");
    // …and the rust one keeps its navigable identity but stays content-redacted.
    assert_blocked_identity_present(&candidates[1], "secret_in_source");
    assert_eq!(candidates[1]["id"], "rust:function:dual.secret", "{env}");
    assert!(
        !env.to_string().contains("loomweave:eid:dual-secret"),
        "blocked SEI leaked via ambiguous entity_resolve: {env}"
    );
}

#[tokio::test]
async fn entity_resolve_mixed_batch_ambiguous_unresolved_resolved_in_order() {
    // One batch carrying all three result kinds: results echo back in input
    // order and the dual qualname's rust row must not cross-contaminate the
    // python-only resolved entry.
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:a.one",
        "function",
        "a.py",
        Some((1, 2)),
    );
    insert_entity(
        &conn,
        "python:function:dual.target",
        "function",
        "dual.py",
        Some((1, 2)),
    );
    insert_entity_named(
        &conn,
        "rust:function:dual.target",
        "function",
        "dual.rs",
        Some((1, 2)),
        "target",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(
        &state,
        "entity_resolve",
        json!({"qualnames": ["dual.target", "missing.x", "a.one"]}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let results = env["result"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["qualname"], "dual.target");
    assert_eq!(results[0]["result_kind"], "ambiguous");
    assert_eq!(
        results[0]["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        2,
        "{env}"
    );
    assert_eq!(results[1]["qualname"], "missing.x");
    assert_eq!(results[1]["result_kind"], "unresolved");
    assert_eq!(
        results[1]["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        0,
        "{env}"
    );
    assert_eq!(results[2]["qualname"], "a.one");
    assert_eq!(results[2]["result_kind"], "resolved");
    let resolved = results[2]["candidates"].as_array().expect("candidates");
    assert_eq!(resolved.len(), 1, "no cross-contamination: {env}");
    assert_eq!(resolved[0]["id"], "python:function:a.one");
}

// ── B2 (weft-3fb0f5dfc7): entity_dead_list usable as a survey ─────────────────

/// Insert an entity with an explicit plugin id (the shared helper hardcodes
/// `python`); used by the dead-list survey tests below.
fn insert_entity_with_plugin(
    conn: &Connection,
    id: &str,
    plugin_id: &str,
    kind: &str,
    source_path: &str,
    properties: &str,
) {
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path, \
            properties, content_hash, created_at, updated_at) \
         VALUES (?1,?2,?3,?1,?1,?4,?5,'hash','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        params![id, plugin_id, kind, source_path, properties],
    )
    .expect("insert entity");
}

/// B2(1) failing-first: non-code entities — core `file` anchors (the dogfooded
/// `.env.example`), the project anchor, subsystems, guidance — must never be
/// "dead CODE" candidates.
#[tokio::test]
async fn find_dead_code_excludes_non_code_entities() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    // A genuinely dead python function — the only legitimate candidate.
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((6, 9)),
    );
    // Non-code rows, all unreachable by construction.
    insert_entity_with_plugin(
        &conn,
        "core:file:.env.example",
        "core",
        "file",
        ".env.example",
        "{}",
    );
    insert_entity_with_plugin(&conn, "core:project:proj", "core", "project", "/proj", "{}");
    insert_entity_with_plugin(&conn, "core:subsystem:abc", "core", "subsystem", "x", "{}");
    insert_guidance(&conn, "core:guidance:g1", "{}");
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        dead,
        vec!["python:function:orphan".to_owned()],
        "config files / anchors / subsystems / guidance are not dead CODE: {env}"
    );
}

/// B2(2): entities owned by a plugin that emitted NO reachability root tags must
/// be EXCLUDED with an in-band marker, never false-flagged dead. A wrong answer
/// is worse than an honest scope statement. (Since ADR-054 the Rust plugin DOES
/// emit roots — see `find_dead_code_surveys_rust_once_it_emits_roots` — so the
/// untagged `rust` entity here stands in for any hypothetical rootless plugin;
/// the exclusion mechanism is plugin-name-agnostic, keyed on emitted tags.)
#[tokio::test]
async fn find_dead_code_excludes_plugins_without_root_coverage_with_marker() {
    let (project, db, conn) = open_project();
    // One plugin emits roots (python); the other emits none — its entities are
    // withheld rather than false-flagged dead.
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((6, 9)),
    );
    // The dogfooded false positive: specimen-rs/src/main.rs, unreachable only
    // because no rust root tags exist.
    insert_entity_with_plugin(
        &conn,
        "rust:function:specimen_rs.main",
        "rust",
        "function",
        "specimen-rs/src/main.rs",
        "{}",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        dead,
        vec!["python:function:orphan".to_owned()],
        "a rootless plugin's entities must not be false-flagged dead: {env}"
    );
    // The honest in-band scope statement.
    let excluded = env["result"]["excluded"]["plugins_without_roots"]
        .as_array()
        .unwrap_or_else(|| panic!("missing plugins_without_roots marker: {env}"));
    assert_eq!(excluded.len(), 1, "{env}");
    assert_eq!(excluded[0]["plugin"], "rust", "{env}");
    assert_eq!(excluded[0]["entities_excluded"], 1, "{env}");
    assert!(
        excluded[0]["reason"].as_str().unwrap().contains("root"),
        "the marker must explain the missing root coverage: {env}"
    );
}

/// B2(3) failing-first (revised per PM ruling on weft-3fb0f5dfc7): a
/// briefing-blocked entity appears in the survey NEITHER as an all-null row
/// (unactionable noise — the dogfooded failure) NOR as an identity-bearing row
/// (the stub-collapse non-disclosure invariant stays absolute). It is EXCLUDED
/// from the row set, and the exclusion is reported once, in-band, at the top
/// level: count + reason + the standard recovery path.
#[tokio::test]
async fn find_dead_code_withholds_blocked_entities_with_aggregate_marker() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((6, 9)),
    );
    insert_entity_with_plugin(
        &conn,
        "python:function:leaky.helper",
        "python",
        "function",
        "leaky.py",
        r#"{"briefing_blocked": "secret_present"}"#,
    );
    insert_entity_with_plugin(
        &conn,
        "python:function:leaky.other",
        "python",
        "function",
        "leaky.py",
        r#"{"briefing_blocked": "secret_present"}"#,
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    // No blocked row in the page — neither nulls nor identity.
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap_or("<null>").to_owned())
        .collect();
    assert_eq!(
        dead,
        vec!["python:function:orphan".to_owned()],
        "blocked entities must be excluded from the row set: {env}"
    );
    assert!(
        !env.to_string().contains("python:function:leaky"),
        "blocked identity must not be disclosed anywhere in the envelope: {env}"
    );
    // The single aggregate in-band marker.
    let withheld = &env["result"]["withheld"];
    assert_eq!(withheld["count"], 2, "{env}");
    assert_eq!(withheld["reasons"][0], "secret_present", "{env}");
    assert!(
        withheld["recovery"]
            .as_str()
            .unwrap()
            .contains("secrets-baseline"),
        "the marker must carry the standard briefing-block recovery path: {env}"
    );
}

/// B2(4) failing-first: the constant five-line `reason` (and rule/confidence
/// facets) must be hoisted to ONE top-level block, not repeated verbatim on
/// every row of every page (the C12-class repeated-degrade-block problem).
#[tokio::test]
async fn find_dead_code_hoists_constant_facets_to_top_level() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    insert_entity(
        &conn,
        "python:function:orphan_a",
        "function",
        "app.py",
        Some((6, 9)),
    );
    insert_entity(
        &conn,
        "python:function:orphan_b",
        "function",
        "app.py",
        Some((10, 13)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let finding = &env["result"]["finding"];
    assert_eq!(finding["rule_id"], "LMWV-FACT-DEAD-CODE-CANDIDATE", "{env}");
    assert_eq!(finding["kind"], "fact", "{env}");
    assert!(finding["confidence"].is_number(), "{env}");
    assert!(
        finding["reason"].as_str().unwrap().contains("unreachable"),
        "{env}"
    );
    for row in env["result"]["dead_code"].as_array().unwrap() {
        for hoisted in [
            "reason",
            "rule_id",
            "kind",
            "confidence",
            "confidence_basis",
        ] {
            assert!(
                row.get(hoisted).is_none(),
                "constant facet '{hoisted}' must be hoisted, not repeated per row: {env}"
            );
        }
    }
}

// ---- A5: app-scoped reachability roots + app_only filter (clarion-663aca16aa)

/// The default mode is `explicit`: existing consumers see the same output, and
/// the summary now declares `roots_mode: "explicit"` with no `roots_confidence`
/// (roots are taken verbatim from emitted tags, not derived).
#[tokio::test]
async fn find_dead_code_explicit_roots_mode_is_default_and_reported() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((6, 9)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    // Default == explicit; behaviour unchanged.
    assert_eq!(env["result"]["summary"]["roots_mode"], "explicit", "{env}");
    assert!(
        env["result"]["summary"]["roots_confidence"].is_null(),
        "explicit mode must not claim derived roots: {env}"
    );
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(dead, vec!["python:function:orphan".to_owned()], "{env}");
}

/// Explicit mode preserves the honest-empty signal when no root tags exist —
/// the regression surface the ticket flags. Passing `roots: "explicit"`
/// explicitly is identical to the default.
#[tokio::test]
async fn find_dead_code_explicit_roots_preserves_honest_empty() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((1, 5)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({"roots": "explicit"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["signal"]["available"], false, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
    assert!(
        env["result"]["dead_code"].as_array().unwrap().is_empty(),
        "{env}"
    );
}

/// Auto mode derives roots from the same emitted tags Loomweave already has and
/// declares the lower confidence (`roots_mode: "auto"`,
/// `roots_confidence: "derived"`). It also relaxes the per-plugin missing-root
/// exclusion: a plugin with no root tags of its own is still surveyed against
/// the auto-derived global root set rather than excluded.
#[tokio::test]
async fn find_dead_code_auto_roots_seeds_from_tags_and_surveys_rootless_plugins() {
    let (project, db, conn) = open_project();
    // Python emits a root tag.
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((6, 9)),
    );
    // A rootless plugin's dead leaf: excluded in explicit mode, surveyed in auto.
    insert_entity_with_plugin(
        &conn,
        "rust:function:specimen_rs.dead",
        "rust",
        "function",
        "specimen-rs/src/lib.rs",
        "{}",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({"roots": "auto"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["summary"]["roots_mode"], "auto", "{env}");
    assert_eq!(
        env["result"]["summary"]["roots_confidence"], "derived",
        "auto mode must declare derived-confidence roots: {env}"
    );
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    // Both the python orphan and the rootless-plugin dead leaf are surveyed.
    assert!(
        dead.contains(&"python:function:orphan".to_owned()),
        "auto mode surveys the python orphan: {env}"
    );
    assert!(
        dead.contains(&"rust:function:specimen_rs.dead".to_owned()),
        "auto mode surveys a rootless plugin against the derived roots: {env}"
    );
    // No plugins are excluded for missing root coverage in auto mode.
    assert!(
        env["result"]["excluded"]["plugins_without_roots"]
            .as_array()
            .unwrap()
            .is_empty(),
        "auto mode relaxes the per-plugin missing-root exclusion: {env}"
    );
}

/// Auto mode with NO root tags anywhere still cannot fabricate roots: it stays
/// honest-empty rather than flagging the whole corpus dead.
#[tokio::test]
async fn find_dead_code_auto_roots_honest_empty_when_no_tags() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((1, 5)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({"roots": "auto"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["signal"]["available"], false, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
}

/// `app_only: true` excludes test-tagged entities (and core-plugin entities)
/// from the dead-code candidate set. A purely additive, opt-in read filter.
#[tokio::test]
async fn find_dead_code_app_only_excludes_tests() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    // A genuinely dead app function.
    insert_entity(
        &conn,
        "python:function:orphan",
        "function",
        "app.py",
        Some((6, 9)),
    );
    // A dead, test-tagged helper (e.g. tour.*-style or a test fixture).
    insert_entity(
        &conn,
        "python:function:dead_test_helper",
        "function",
        "tests/helpers.py",
        Some((10, 13)),
    );
    insert_tag(&conn, "python:function:dead_test_helper", "test");
    drop(conn);
    let state = state_for(project.path(), &db);

    // Default: both dead functions appear.
    let env = call_tool(&state, "find_dead_code", json!({})).await;
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(dead.contains(&"python:function:orphan".to_owned()), "{env}");

    // app_only: the test-tagged dead helper is filtered out.
    let env = call_tool(&state, "find_dead_code", json!({"app_only": true})).await;
    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        dead,
        vec!["python:function:orphan".to_owned()],
        "app_only must exclude the test-tagged dead helper: {env}"
    );
    assert_eq!(env["result"]["app_only"], true, "{env}");
}

#[tokio::test]
async fn find_dead_code_app_only_does_not_treat_test_roots_as_live_reachability() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    insert_entity(
        &conn,
        "python:function:test_root",
        "function",
        "tests/test_app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:test_root", "test");
    insert_entity(
        &conn,
        "python:function:app_helper",
        "function",
        "app.py",
        Some((6, 9)),
    );
    insert_calls_edge(
        &conn,
        "python:function:test_root",
        "python:function:app_helper",
        "resolved",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({"app_only": true})).await;

    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        dead,
        vec!["python:function:app_helper".to_owned()],
        "app_only reachability must not let test roots suppress app dead-code candidates: {env}"
    );
}

/// clarion-4ec50f3d92: a module that declares no `__all__` whose public surface
/// is reached only through paths static analysis cannot follow here (a test, but
/// equally framework dispatch / DI / a CLI in a real app). The Python plugin's
/// no-`__all__` fallback tags public module-level defs/classes `public-surface`;
/// that tag is a reachability root, so in `app_only` mode (tests excluded as
/// roots) the public surface and its transitive internals stay live instead of
/// reading as mostly dead. Genuinely-unused private code is still flagged.
#[tokio::test]
async fn find_dead_code_public_surface_root_rescues_library_api_in_app_only() {
    let (project, db, conn) = open_project();
    // Public library surface from a no-__all__ module → public-surface root.
    insert_entity(
        &conn,
        "python:function:lib.public_api",
        "function",
        "lib.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:lib.public_api", "public-surface");
    // Internal implementations reachable only from the public API (not from any
    // test root) — kept live transitively by the public-surface root. A real
    // library has many internals per genuine orphan, so the post-fix dead share
    // is plausible rather than implausibly high.
    for (idx, line) in [(6i64, 10i64), (11, 15), (16, 20)].into_iter().enumerate() {
        let id = format!("python:function:lib.internal_impl_{idx}");
        insert_entity(&conn, &id, "function", "lib.py", Some(line));
        insert_calls_edge(&conn, "python:function:lib.public_api", &id, "resolved");
    }
    // The only caller of the public API is a test — in app_only this root is
    // excluded, so without the public-surface root both lib functions would read
    // as dead (the implausible over-report this ticket fixes).
    insert_entity(
        &conn,
        "python:function:tests.test_api",
        "function",
        "tests/test_lib.py",
        Some((1, 4)),
    );
    insert_tag(&conn, "python:function:tests.test_api", "test");
    insert_calls_edge(
        &conn,
        "python:function:tests.test_api",
        "python:function:lib.public_api",
        "resolved",
    );
    // Genuinely dead: a private internal nothing calls.
    insert_entity(
        &conn,
        "python:function:lib._orphan",
        "function",
        "lib.py",
        Some((21, 24)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({"app_only": true})).await;
    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    // Only the genuine orphan is dead; the public API and its internal callee are
    // kept live by the public-surface root despite tests being excluded.
    assert_eq!(
        dead,
        vec!["python:function:lib._orphan".to_owned()],
        "public-surface must root the library API in app_only mode: {env}"
    );
    // The dead share is now plausible (1 dead of 5 analysed = 20%; test_api is in
    // the DB and counts toward `analysed` even though its `test` tag is not a root
    // in app_only), so the verdict is no longer the implausible LOW-confidence
    // band (threshold > 25%).
    assert_eq!(env["result"]["summary"]["confidence"], "moderate", "{env}");
    assert!(env["result"]["summary"]["advisory"].is_null(), "{env}");
}

/// ADR-054 acceptance: once the Rust plugin emits reachability roots, a Rust-only
/// index is SURVEYED (not withheld by the no-roots exclusion); a genuinely-unused
/// private fn is flagged dead; and a `pub` lib fn reached only via a test stays
/// live through its `exported-api` root even in `app_only` (where tests are
/// excluded). The exclusion lift is automatic — `plugins_without_roots` drops to
/// zero the moment Rust owns a root-tagged entity.
#[tokio::test]
async fn find_dead_code_surveys_rust_once_it_emits_roots() {
    let (project, db, conn) = open_project();
    // A pub lib fn → `exported-api` root.
    insert_entity_with_plugin(
        &conn,
        "rust:function:lib.public_api",
        "rust",
        "function",
        "src/lib.rs",
        "{}",
    );
    insert_tag_with_plugin(
        &conn,
        "rust:function:lib.public_api",
        "rust",
        "exported-api",
    );
    // A private internal reached only from the public API — kept live transitively.
    insert_entity_with_plugin(
        &conn,
        "rust:function:lib.internal_used",
        "rust",
        "function",
        "src/lib.rs",
        "{}",
    );
    insert_calls_edge(
        &conn,
        "rust:function:lib.public_api",
        "rust:function:lib.internal_used",
        "resolved",
    );
    // A pub lib fn whose ONLY caller is a test — its `exported-api` root must keep
    // it live in app_only, where the test root is excluded.
    insert_entity_with_plugin(
        &conn,
        "rust:function:lib.tested_only",
        "rust",
        "function",
        "src/lib.rs",
        "{}",
    );
    insert_tag_with_plugin(
        &conn,
        "rust:function:lib.tested_only",
        "rust",
        "exported-api",
    );
    insert_entity_with_plugin(
        &conn,
        "rust:function:tests.it_works",
        "rust",
        "function",
        "src/lib.rs",
        "{}",
    );
    insert_tag_with_plugin(&conn, "rust:function:tests.it_works", "rust", "test");
    insert_calls_edge(
        &conn,
        "rust:function:tests.it_works",
        "rust:function:lib.tested_only",
        "resolved",
    );
    // Genuinely dead: a private fn nothing calls.
    insert_entity_with_plugin(
        &conn,
        "rust:function:lib.dead_helper",
        "rust",
        "function",
        "src/lib.rs",
        "{}",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    for app_only in [false, true] {
        let env = call_tool(&state, "find_dead_code", json!({ "app_only": app_only })).await;
        assert_eq!(env["ok"], true, "{env}");
        let dead: Vec<String> = env["result"]["dead_code"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            dead,
            vec!["rust:function:lib.dead_helper".to_owned()],
            "only the genuine orphan is dead (app_only={app_only}); the exported-api root \
             keeps the test-only-reached pub fn live: {env}"
        );
        // The exclusion has lifted: Rust is surveyed, not withheld.
        assert_eq!(
            env["result"]["summary"]["not_analysed"]["plugins_without_roots"], 0,
            "rust emits roots → no plugin is withheld (app_only={app_only}): {env}"
        );
    }
}

/// ADR-054: `module` and `impl` are containment-spine containers rooted at the
/// always-live crate root, not removable code — so they are never dead-code
/// candidates, even with no inbound call/import edge. (Rust modules/impls
/// systematically lack module-targeting import edges; without this exclusion
/// every Rust module and impl block would read as dead and dominate the
/// candidate set.)
#[tokio::test]
async fn find_dead_code_excludes_module_containers() {
    let (project, db, conn) = open_project();
    insert_entity(
        &conn,
        "python:function:main",
        "function",
        "app.py",
        Some((1, 5)),
    );
    insert_tag(&conn, "python:function:main", "entry-point");
    // Container entities with no inbound call/import edge — never "dead code".
    insert_entity(
        &conn,
        "python:module:orphan_mod",
        "module",
        "orphan.py",
        Some((1, 10)),
    );
    insert_entity_with_plugin(
        &conn,
        "rust:impl:orphan.Widget.impl#<>",
        "rust",
        "impl",
        "src/orphan.rs",
        "{}",
    );
    // A genuinely dead function — the only legitimate candidate.
    insert_entity(
        &conn,
        "python:function:dead",
        "function",
        "app.py",
        Some((6, 9)),
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    let env = call_tool(&state, "find_dead_code", json!({})).await;
    assert_eq!(env["ok"], true, "{env}");
    let dead: Vec<String> = env["result"]["dead_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        dead,
        vec!["python:function:dead".to_owned()],
        "module / impl containers are never dead-code candidates: {env}"
    );
}

/// `app_only: true` on coupling excludes test-tagged callers from the ranking so
/// a hub's coupling drops to reflect only first-party app fan-in/out.
#[tokio::test]
async fn find_coupling_hotspots_app_only_excludes_test_tagged() {
    let (project, db, conn) = open_project();
    for id in ["hub", "app_caller", "test_caller"] {
        insert_entity(
            &conn,
            &format!("python:function:{id}"),
            "function",
            "m.py",
            Some((1, 2)),
        );
    }
    insert_tag(&conn, "python:function:test_caller", "test");
    insert_edge(
        &conn,
        "calls",
        "python:function:app_caller",
        "python:function:hub",
        "resolved",
    );
    insert_edge(
        &conn,
        "calls",
        "python:function:test_caller",
        "python:function:hub",
        "resolved",
    );
    drop(conn);
    let state = state_for(project.path(), &db);

    // Default: hub fan_in counts both callers.
    let env = call_tool(&state, "find_coupling_hotspots", json!({})).await;
    let hub = env["result"]["hotspots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["entity"]["id"] == "python:function:hub")
        .unwrap();
    assert_eq!(hub["fan_in"], 2, "{env}");

    // app_only: the test-tagged caller and the test entity itself are excluded.
    let env = call_tool(&state, "find_coupling_hotspots", json!({"app_only": true})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["app_only"], true, "{env}");
    let hub = env["result"]["hotspots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["entity"]["id"] == "python:function:hub")
        .unwrap();
    assert_eq!(
        hub["fan_in"], 1,
        "app_only must drop the test-tagged caller from fan-in: {env}"
    );
    // The test entity must not appear as a hotspot row.
    assert!(
        !env["result"]["hotspots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["entity"]["id"] == "python:function:test_caller"),
        "app_only must exclude the test entity from the ranking: {env}"
    );
}
