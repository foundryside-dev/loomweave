//! WS5 stateless catalogue — inspection reads (Task 1): `guidance_for`,
//! `findings_for`, `wardline_for`. Exercises the SEI-join contract,
//! honest-empty behaviour, and the bounded/pagination contract over the public
//! JSON-RPC surface.

use clarion_mcp::{ServerState, list_tools};
use clarion_storage::{ReaderPool, pragma, schema};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn open_project() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
    let project = tempfile::tempdir().expect("temp project");
    let clarion_dir = project.path().join(".clarion");
    std::fs::create_dir(&clarion_dir).expect("create .clarion");
    let db_path = clarion_dir.join("clarion.db");
    let mut conn = Connection::open(&db_path).expect("open sqlite");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    (project, db_path, conn)
}

fn state_for(project_root: &std::path::Path, db_path: &std::path::Path) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool).with_clock(|| "2026-06-02T00:00:00.000Z".to_owned())
}

fn insert_entity(conn: &Connection, id: &str, kind: &str, source_path: &str, range: Option<(i64, i64)>) {
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

fn insert_finding(conn: &Connection, id: &str, entity_id: &str, kind: &str, severity: &str, status: &str) {
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
         VALUES (?1,'clarion','1.0','run-1','R1',?3,?4,?2,'[]','m','{}','{}','[]','[]',?5, \
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
    for expected in ["guidance_for", "findings_for", "wardline_for"] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
}

// ---- wardline_for -------------------------------------------------------

#[tokio::test]
async fn wardline_for_returns_verbatim_blob_when_present() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
    insert_taint_fact(&conn, "python:function:m.f", r#"{"taint":"tainted","sources":["request.body"]}"#);
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
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
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
    let env = call_tool(&state, "wardline_for", json!({"id": "python:function:nope"})).await;
    assert_eq!(env["ok"], false, "{env}");
    assert_eq!(env["error"]["code"], "entity-not-found");
}

// ---- SEI-join contract (ADR-038) ---------------------------------------

#[tokio::test]
async fn entity_sei_is_null_without_binding_and_populated_with_one() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);

    // Pre-Wave-1: no sei_bindings row -> sei is null (graceful degrade).
    let env = call_tool(&state, "wardline_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["result"]["entity"]["sei"], Value::Null, "{env}");

    // Bind an alive SEI -> the read-time join populates it.
    let conn = Connection::open(&db).unwrap();
    insert_alive_sei(&conn, "clarion:eid:deadbeef", "python:function:m.f");
    drop(conn);
    let env = call_tool(&state, "wardline_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["result"]["entity"]["sei"], "clarion:eid:deadbeef", "{env}");
}

// ---- findings_for -------------------------------------------------------

#[tokio::test]
async fn findings_for_returns_anchored_findings_and_filters() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
    insert_finding(&conn, "f-open", "python:function:m.f", "defect", "WARN", "open");
    insert_finding(&conn, "f-supp", "python:function:m.f", "defect", "ERROR", "suppressed");
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
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
    for i in 0..5 {
        insert_finding(&conn, &format!("f-{i}"), "python:function:m.f", "defect", "WARN", "open");
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
async fn findings_for_empty_entity_is_not_an_error() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
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
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
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
    assert_eq!(env["result"]["guidance"].as_array().unwrap().len(), 0, "{env}");
}

#[tokio::test]
async fn guidance_for_honest_empty_when_no_sheet_matches() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
    drop(conn);
    let state = state_for(project.path(), &db);
    let env = call_tool(&state, "guidance_for", json!({"id": "python:function:m.f"})).await;
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["page"]["total"], 0);
}

#[tokio::test]
async fn guidance_for_reports_unevaluable_wardline_group_rule() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:m.f", "function", "m.py", Some((1, 2)));
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
    assert_eq!(env["result"]["guidance"].as_array().unwrap().len(), 0, "{env}");
    assert_eq!(env["result"]["notes"][0]["signal"], "wardline_group");
}
