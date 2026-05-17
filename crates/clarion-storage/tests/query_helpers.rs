//! Storage query helper tests for the B.6 MCP surface.

use std::path::Path;

use clarion_core::EdgeConfidence;
use clarion_storage::{
    call_edges_from, call_edges_targeting, contained_entity_ids, entity_at_line, entity_by_id,
    find_entities, normalize_source_path, pragma, schema,
};
use rusqlite::{Connection, params};

fn open_fresh(tempdir: &tempfile::TempDir) -> Connection {
    let path = tempdir.path().join("clarion.db");
    let mut conn = Connection::open(path).expect("open sqlite");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    conn
}

fn insert_entity(conn: &Connection, id: &str, kind: &str) {
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, properties, created_at, updated_at
         ) VALUES (
            ?1, 'python', ?2, ?1, ?1, '{}',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        params![id, kind],
    )
    .expect("insert entity");
}

fn insert_entity_with_range(
    conn: &Connection,
    id: &str,
    kind: &str,
    source_path: &Path,
    start_line: i64,
    end_line: i64,
) {
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, created_at, updated_at
         ) VALUES (
            ?1, 'python', ?2, ?1, ?1, ?3, ?4, ?5, '{}',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        params![
            id,
            kind,
            source_path.display().to_string(),
            start_line,
            end_line
        ],
    )
    .expect("insert ranged entity");
}

fn insert_calls_edge(
    conn: &Connection,
    from_id: &str,
    to_id: &str,
    confidence: EdgeConfidence,
    candidates: &[&str],
) {
    let properties = if candidates.is_empty() {
        None
    } else {
        Some(serde_json::json!({ "candidates": candidates }).to_string())
    };
    conn.execute(
        "INSERT INTO edges (
            kind, from_id, to_id, confidence, properties, source_byte_start, source_byte_end
         ) VALUES ('calls', ?1, ?2, ?3, ?4, 10, 20)",
        params![from_id, to_id, confidence.as_str(), properties],
    )
    .expect("insert calls edge");
}

fn insert_contains_edge(conn: &Connection, from_id: &str, to_id: &str) {
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence)
         VALUES ('contains', ?1, ?2, 'resolved')",
        params![from_id, to_id],
    )
    .expect("insert contains edge");
}

#[test]
fn call_edges_targeting_expands_candidate_only_ambiguous_targets() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:function:demo.caller", "function");
    insert_entity(&conn, "python:function:demo.alpha", "function");
    insert_entity(&conn, "python:function:demo.beta", "function");
    insert_calls_edge(
        &conn,
        "python:function:demo.caller",
        "python:function:demo.alpha",
        EdgeConfidence::Ambiguous,
        &["python:function:demo.beta"],
    );

    let matches = call_edges_targeting(
        &conn,
        "python:function:demo.beta",
        EdgeConfidence::Ambiguous,
    )
    .expect("query call edges targeting beta");

    assert_eq!(matches.len(), 1);
    let edge = &matches[0];
    assert_eq!(edge.from_id, "python:function:demo.caller");
    assert_eq!(edge.to_id, "python:function:demo.beta");
    assert_eq!(edge.stored_to_id, "python:function:demo.alpha");
    assert_eq!(edge.confidence, EdgeConfidence::Ambiguous);
}

#[test]
fn call_edges_targeting_dedupes_stored_to_id_also_listed_as_candidate() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:function:demo.caller", "function");
    insert_entity(&conn, "python:function:demo.alpha", "function");
    insert_entity(&conn, "python:function:demo.beta", "function");
    insert_calls_edge(
        &conn,
        "python:function:demo.caller",
        "python:function:demo.alpha",
        EdgeConfidence::Ambiguous,
        &["python:function:demo.alpha", "python:function:demo.beta"],
    );

    let matches = call_edges_targeting(
        &conn,
        "python:function:demo.alpha",
        EdgeConfidence::Ambiguous,
    )
    .expect("query call edges targeting alpha");

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].to_id, "python:function:demo.alpha");
}

#[test]
fn call_edges_from_expands_ambiguous_candidates_once() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:function:demo.caller", "function");
    insert_entity(&conn, "python:function:demo.alpha", "function");
    insert_entity(&conn, "python:function:demo.beta", "function");
    insert_calls_edge(
        &conn,
        "python:function:demo.caller",
        "python:function:demo.alpha",
        EdgeConfidence::Ambiguous,
        &["python:function:demo.alpha", "python:function:demo.beta"],
    );

    let matches = call_edges_from(
        &conn,
        "python:function:demo.caller",
        EdgeConfidence::Ambiguous,
    )
    .expect("query outgoing call edges");
    let targets: Vec<&str> = matches.iter().map(|edge| edge.to_id.as_str()).collect();

    assert_eq!(
        targets,
        vec!["python:function:demo.alpha", "python:function:demo.beta"]
    );
}

#[test]
fn resolved_confidence_excludes_ambiguous_candidate_expansion() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:function:demo.caller", "function");
    insert_entity(&conn, "python:function:demo.alpha", "function");
    insert_entity(&conn, "python:function:demo.beta", "function");
    insert_calls_edge(
        &conn,
        "python:function:demo.caller",
        "python:function:demo.alpha",
        EdgeConfidence::Ambiguous,
        &["python:function:demo.beta"],
    );

    let matches =
        call_edges_targeting(&conn, "python:function:demo.beta", EdgeConfidence::Resolved)
            .expect("query resolved-only callers");

    assert!(matches.is_empty());
}

#[test]
fn entity_at_line_uses_innermost_range_then_kind_precedence() {
    let tempdir = tempfile::tempdir().unwrap();
    let source_path = tempdir.path().join("demo.py");
    std::fs::write(
        &source_path,
        "class Demo:\n    def method(self):\n        return 1\n",
    )
    .unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity_with_range(&conn, "python:module:demo", "module", &source_path, 1, 3);
    insert_entity_with_range(&conn, "python:class:demo.Demo", "class", &source_path, 1, 3);
    insert_entity_with_range(
        &conn,
        "python:function:demo.Demo.method",
        "function",
        &source_path,
        2,
        3,
    );

    let entity = entity_at_line(&conn, source_path.to_str().unwrap(), 2)
        .expect("entity_at query")
        .expect("line should match an entity");

    assert_eq!(entity.id, "python:function:demo.Demo.method");
}

#[test]
fn entity_lookup_and_search_cover_id_and_fts_paths() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:module:demo", "module");
    insert_entity(&conn, "python:function:demo.TokenManager", "function");

    let entity = entity_by_id(&conn, "python:function:demo.TokenManager")
        .expect("lookup by id")
        .expect("entity should exist");
    assert_eq!(entity.kind, "function");

    let fts_results = find_entities(&conn, "TokenManager", 20, 0).expect("FTS search");
    assert_eq!(fts_results.len(), 1);
    assert_eq!(fts_results[0].id, "python:function:demo.TokenManager");

    let like_results = find_entities(&conn, "python:function:demo.TokenManager", 20, 0)
        .expect("punctuation-heavy ID search");
    assert_eq!(like_results.len(), 1);
    assert_eq!(like_results[0].id, "python:function:demo.TokenManager");
}

#[test]
fn contained_entity_ids_is_depth_first_cycle_safe_and_capped() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    for id in [
        "python:module:demo",
        "python:class:demo.Demo",
        "python:function:demo.Demo.method",
        "python:function:demo.helper",
    ] {
        insert_entity(&conn, id, "function");
    }
    insert_contains_edge(&conn, "python:module:demo", "python:class:demo.Demo");
    insert_contains_edge(
        &conn,
        "python:class:demo.Demo",
        "python:function:demo.Demo.method",
    );
    insert_contains_edge(&conn, "python:module:demo", "python:function:demo.helper");
    insert_contains_edge(
        &conn,
        "python:function:demo.Demo.method",
        "python:module:demo",
    );

    let traversal = contained_entity_ids(&conn, "python:module:demo", 2)
        .expect("contains traversal should complete");

    assert_eq!(
        traversal.entity_ids,
        vec![
            "python:class:demo.Demo".to_owned(),
            "python:function:demo.Demo.method".to_owned(),
        ]
    );
    assert!(traversal.truncated);
}

#[test]
fn normalize_source_path_accepts_project_relative_paths_and_rejects_escape() {
    let tempdir = tempfile::tempdir().unwrap();
    let source_path = tempdir.path().join("src").join("demo.py");
    std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    std::fs::write(&source_path, "def demo():\n    pass\n").unwrap();

    let normalized =
        normalize_source_path(tempdir.path(), "src/demo.py").expect("relative source path");

    assert_eq!(
        normalized,
        source_path.canonicalize().unwrap().to_str().unwrap()
    );
    let escaped = normalize_source_path(tempdir.path(), "../outside.py")
        .expect_err("path escape should be rejected");
    assert!(
        escaped.to_string().contains("invalid source path"),
        "unexpected error: {escaped}"
    );
}
