//! MCP storage-backed tool tests.

use clarion_mcp::ServerState;
use clarion_storage::{ReaderPool, pragma, schema};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn open_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let project = tempfile::tempdir().expect("temp project");
    let clarion_dir = project.path().join(".clarion");
    std::fs::create_dir(&clarion_dir).expect("create .clarion");
    let db_path = clarion_dir.join("clarion.db");
    let mut conn = Connection::open(&db_path).expect("open sqlite");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    seed_graph(&conn, project.path());
    drop(conn);
    (project, db_path)
}

fn seed_graph(conn: &Connection, project_root: &std::path::Path) {
    let source_path = project_root.join("demo.py");
    std::fs::write(
        &source_path,
        "def entry():\n    return mid()\n\ndef mid():\n    return target()\n\ndef target():\n    return 1\n",
    )
    .expect("write demo source");

    insert_entity(
        conn,
        "python:module:demo",
        "module",
        &source_path,
        Some((1, 8)),
        None,
    );
    insert_entity(
        conn,
        "python:function:demo.entry",
        "function",
        &source_path,
        Some((1, 2)),
        Some("python:module:demo"),
    );
    insert_entity(
        conn,
        "python:function:demo.mid",
        "function",
        &source_path,
        Some((4, 5)),
        Some("python:module:demo"),
    );
    insert_entity(
        conn,
        "python:function:demo.target",
        "function",
        &source_path,
        Some((7, 8)),
        Some("python:module:demo"),
    );
    insert_entity(
        conn,
        "python:function:demo.alt_target",
        "function",
        &source_path,
        Some((7, 8)),
        Some("python:module:demo"),
    );
    insert_edge(
        conn,
        "contains",
        "python:module:demo",
        "python:function:demo.entry",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "contains",
        "python:module:demo",
        "python:function:demo.mid",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "contains",
        "python:module:demo",
        "python:function:demo.target",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "contains",
        "python:module:demo",
        "python:function:demo.alt_target",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "calls",
        "python:function:demo.entry",
        "python:function:demo.mid",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "calls",
        "python:function:demo.mid",
        "python:function:demo.target",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "calls",
        "python:function:demo.entry",
        "python:function:demo.target",
        "ambiguous",
        Some(json!({"candidates": ["python:function:demo.alt_target"]})),
    );
    insert_edge(
        conn,
        "references",
        "python:function:demo.entry",
        "python:function:demo.target",
        "resolved",
        None,
    );
}

fn insert_entity(
    conn: &Connection,
    id: &str,
    kind: &str,
    source_path: &std::path::Path,
    range: Option<(i64, i64)>,
    parent_id: Option<&str>,
) {
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            ?1, 'python', ?2, ?1, ?1, ?3, ?4, ?5, ?6, '{}', ?7,
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        params![
            id,
            kind,
            parent_id,
            source_path.display().to_string(),
            range.map(|(start, _)| start),
            range.map(|(_, end)| end),
            format!("hash-{id}"),
        ],
    )
    .expect("insert entity");
}

fn insert_edge(
    conn: &Connection,
    kind: &str,
    from_id: &str,
    to_id: &str,
    confidence: &str,
    properties: Option<Value>,
) {
    let anchored = matches!(kind, "calls" | "references");
    conn.execute(
        "INSERT INTO edges (
            kind, from_id, to_id, confidence, properties, source_byte_start, source_byte_end
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            kind,
            from_id,
            to_id,
            confidence,
            properties.map(|value| value.to_string()),
            anchored.then_some(10),
            anchored.then_some(20),
        ],
    )
    .expect("insert edge");
}

fn state_for(project_root: &std::path::Path, db_path: &std::path::Path) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool)
}

async fn call_tool(state: &ServerState, name: &str, arguments: Value) -> Value {
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "tool-test",
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], "tool-test");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool content text");
    serde_json::from_str(text).expect("tool envelope JSON")
}

#[tokio::test]
async fn entity_at_returns_innermost_entity_and_empty_match() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let hit = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 1})).await;
    assert_eq!(hit["ok"], true);
    assert_eq!(hit["result"]["entity"]["id"], "python:function:demo.entry");

    let miss = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 99})).await;
    assert_eq!(miss["ok"], true);
    assert!(miss["result"]["entity"].is_null());
}

#[tokio::test]
async fn find_entity_paginates_and_searches_punctuation_heavy_ids() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let first = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "python:function:demo", "limit": 2}),
    )
    .await;
    assert_eq!(first["ok"], true);
    assert_eq!(first["result"]["entities"].as_array().unwrap().len(), 2);
    assert_eq!(first["result"]["next_cursor"], "2");

    let second = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "python:function:demo", "limit": 2, "cursor": "2"}),
    )
    .await;
    assert_eq!(second["ok"], true);
    assert!(!second["result"]["entities"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn callers_of_defaults_to_resolved_and_expands_ambiguous_candidates() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let default_callers = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.alt_target"}),
    )
    .await;
    assert_eq!(default_callers["ok"], true);
    assert_eq!(
        default_callers["result"]["callers"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let ambiguous_callers = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.alt_target", "confidence": "ambiguous"}),
    )
    .await;
    assert_eq!(ambiguous_callers["ok"], true);
    assert_eq!(
        ambiguous_callers["result"]["callers"][0]["entity"]["id"],
        "python:function:demo.entry"
    );
    assert_eq!(
        ambiguous_callers["result"]["callers"][0]["stored_to_id"],
        "python:function:demo.target"
    );
}

#[tokio::test]
async fn execution_paths_from_reports_edge_cap_truncation() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path).with_edge_cap(1);

    let envelope = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry", "max_depth": 3, "confidence": "ambiguous"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["truncation_reason"], "edge-cap");
    assert_eq!(envelope["result"]["edge_count_visited"], 2);
}

#[tokio::test]
async fn neighborhood_returns_one_hop_graph_sections() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.target", "confidence": "resolved"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["callers"][0]["entity"]["id"],
        "python:function:demo.mid"
    );
    assert_eq!(envelope["result"]["container"]["id"], "python:module:demo");
    assert_eq!(envelope["result"]["contained"].as_array().unwrap().len(), 0);
    assert_eq!(
        envelope["result"]["references_in"][0]["entity"]["id"],
        "python:function:demo.entry"
    );
}
