//! WS5 stateless catalogue — inspection reads (Task 1): `guidance_for`,
//! `findings_for`, `wardline_for`. Exercises the SEI-join contract,
//! honest-empty behaviour, and the bounded/pagination contract over the public
//! JSON-RPC surface.

use std::sync::Arc;

use loomweave_core::{EmbeddingRecording, RecordingEmbeddingProvider};
use loomweave_mcp::config::SemanticSearchConfig;
use loomweave_mcp::{ServerState, list_tools};
use loomweave_storage::{EmbeddingKey, EmbeddingStore, ReaderPool, pragma, schema};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn open_project() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
    let project = tempfile::tempdir().expect("temp project");
    let loomweave_dir = project.path().join(".loomweave");
    std::fs::create_dir(&loomweave_dir).expect("create .loomweave");
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
}

#[tokio::test]
async fn find_by_kind_unknown_kind_is_empty_not_error() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:a", "function", "a.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "find_by_kind", json!({"kind": "nonesuch"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
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

    let candidate = &env["result"]["dead_code"][0];
    assert_eq!(
        candidate["rule_id"], "LMWV-FACT-DEAD-CODE-CANDIDATE",
        "{env}"
    );
    assert_eq!(candidate["kind"], "fact", "{env}");
    assert_eq!(candidate["confidence_basis"], "heuristic", "{env}");
    assert!(
        candidate["confidence"].as_f64().unwrap() < 1.0,
        "heuristic confidence must be < 1: {env}"
    );
    assert!(
        candidate["entity"]["sei"].is_null() || candidate["entity"]["sei"].is_string(),
        "candidate carries an sei field: {env}"
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
    let store = EmbeddingStore::open_in_loomweave_dir(project.path()).expect("open sidecar");
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
