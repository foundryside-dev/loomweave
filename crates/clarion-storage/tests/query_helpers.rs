//! Storage query helper tests for the B.6 MCP surface.

use std::path::Path;

use clarion_core::EdgeConfidence;
use clarion_storage::{
    ModuleDependencyEdge, ReferenceDirection, SubsystemMember, call_edges_from,
    call_edges_targeting, child_entity_ids, contained_entity_ids, entity_at_line, entity_by_id,
    find_entities, module_dependency_edges, normalize_source_path, pragma,
    reference_edges_for_entity, schema, subsystem_for_member, subsystem_members,
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
    insert_named_entity(conn, id, kind, id, id, None);
}

fn insert_named_entity(
    conn: &Connection,
    id: &str,
    kind: &str,
    name: &str,
    short_name: &str,
    source_file_path: Option<&str>,
) {
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path, properties, created_at,
            updated_at
         ) VALUES (
            ?1, 'python', ?2, ?3, ?4, ?5, '{}',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        params![id, kind, name, short_name, source_file_path],
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

fn insert_references_edge(
    conn: &Connection,
    from_id: &str,
    to_id: &str,
    confidence: EdgeConfidence,
    start: i64,
    end: i64,
) {
    conn.execute(
        "INSERT INTO edges (
            kind, from_id, to_id, confidence, source_byte_start, source_byte_end
         ) VALUES ('references', ?1, ?2, ?3, ?4, ?5)",
        params![from_id, to_id, confidence.as_str(), start, end],
    )
    .expect("insert references edge");
}

fn insert_imports_edge(conn: &Connection, from_id: &str, to_id: &str) {
    conn.execute(
        "INSERT INTO edges (
            kind, from_id, to_id, confidence, source_byte_start, source_byte_end
         ) VALUES ('imports', ?1, ?2, 'resolved', 30, 40)",
        params![from_id, to_id],
    )
    .expect("insert imports edge");
}

fn insert_in_subsystem_edge(conn: &Connection, module_id: &str, subsystem_id: &str) {
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence)
         VALUES ('in_subsystem', ?1, ?2, 'resolved')",
        params![module_id, subsystem_id],
    )
    .expect("insert in_subsystem edge");
}

#[test]
fn module_dependency_edges_include_imports() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:module:pkg.alpha", "module");
    insert_entity(&conn, "python:module:pkg.beta", "module");
    insert_imports_edge(&conn, "python:module:pkg.alpha", "python:module:pkg.beta");

    let edges = module_dependency_edges(&conn, &["imports"]).expect("module dependency edges");

    assert_eq!(
        edges,
        vec![ModuleDependencyEdge {
            from_module_id: "python:module:pkg.alpha".to_owned(),
            to_module_id: "python:module:pkg.beta".to_owned(),
            reference_count: 1,
            edge_kinds: vec!["imports".to_owned()],
        }],
    );
}

#[test]
fn module_dependency_edges_roll_up_function_calls_to_parent_modules() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    for id in ["python:module:pkg.alpha", "python:module:pkg.beta"] {
        insert_entity(&conn, id, "module");
    }
    for id in [
        "python:function:pkg.alpha.source",
        "python:function:pkg.beta.target",
    ] {
        insert_entity(&conn, id, "function");
    }
    insert_contains_edge(
        &conn,
        "python:module:pkg.alpha",
        "python:function:pkg.alpha.source",
    );
    insert_contains_edge(
        &conn,
        "python:module:pkg.beta",
        "python:function:pkg.beta.target",
    );
    insert_calls_edge(
        &conn,
        "python:function:pkg.alpha.source",
        "python:function:pkg.beta.target",
        EdgeConfidence::Resolved,
        &[],
    );

    let edges = module_dependency_edges(&conn, &["calls"]).expect("module dependency edges");

    assert_eq!(
        edges,
        vec![ModuleDependencyEdge {
            from_module_id: "python:module:pkg.alpha".to_owned(),
            to_module_id: "python:module:pkg.beta".to_owned(),
            reference_count: 1,
            edge_kinds: vec!["calls".to_owned()],
        }],
    );
}

#[test]
fn module_dependency_edges_weight_by_reference_count() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    for id in ["python:module:pkg.alpha", "python:module:pkg.beta"] {
        insert_entity(&conn, id, "module");
    }
    for id in [
        "python:function:pkg.alpha.first",
        "python:function:pkg.alpha.second",
        "python:function:pkg.beta.target",
    ] {
        insert_entity(&conn, id, "function");
    }
    insert_contains_edge(
        &conn,
        "python:module:pkg.alpha",
        "python:function:pkg.alpha.first",
    );
    insert_contains_edge(
        &conn,
        "python:module:pkg.alpha",
        "python:function:pkg.alpha.second",
    );
    insert_contains_edge(
        &conn,
        "python:module:pkg.beta",
        "python:function:pkg.beta.target",
    );
    insert_calls_edge(
        &conn,
        "python:function:pkg.alpha.first",
        "python:function:pkg.beta.target",
        EdgeConfidence::Resolved,
        &[],
    );
    insert_calls_edge(
        &conn,
        "python:function:pkg.alpha.second",
        "python:function:pkg.beta.target",
        EdgeConfidence::Resolved,
        &[],
    );
    insert_imports_edge(&conn, "python:module:pkg.alpha", "python:module:pkg.beta");

    let edges =
        module_dependency_edges(&conn, &["imports", "calls"]).expect("module dependency edges");

    assert_eq!(
        edges,
        vec![ModuleDependencyEdge {
            from_module_id: "python:module:pkg.alpha".to_owned(),
            to_module_id: "python:module:pkg.beta".to_owned(),
            reference_count: 3,
            edge_kinds: vec!["calls".to_owned(), "imports".to_owned()],
        }],
    );
}

#[test]
fn module_dependency_edges_skip_self_edges() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:module:pkg.alpha", "module");
    insert_entity(&conn, "python:function:pkg.alpha.first", "function");
    insert_entity(&conn, "python:function:pkg.alpha.second", "function");
    insert_contains_edge(
        &conn,
        "python:module:pkg.alpha",
        "python:function:pkg.alpha.first",
    );
    insert_contains_edge(
        &conn,
        "python:module:pkg.alpha",
        "python:function:pkg.alpha.second",
    );
    insert_calls_edge(
        &conn,
        "python:function:pkg.alpha.first",
        "python:function:pkg.alpha.second",
        EdgeConfidence::Resolved,
        &[],
    );
    insert_imports_edge(&conn, "python:module:pkg.alpha", "python:module:pkg.alpha");

    let edges =
        module_dependency_edges(&conn, &["imports", "calls"]).expect("module dependency edges");

    assert!(edges.is_empty());
}

#[test]
fn module_dependency_edges_exclude_inferred_calls() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    for id in ["python:module:pkg.alpha", "python:module:pkg.beta"] {
        insert_entity(&conn, id, "module");
    }
    for id in [
        "python:function:pkg.alpha.source",
        "python:function:pkg.beta.target",
    ] {
        insert_entity(&conn, id, "function");
    }
    insert_contains_edge(
        &conn,
        "python:module:pkg.alpha",
        "python:function:pkg.alpha.source",
    );
    insert_contains_edge(
        &conn,
        "python:module:pkg.beta",
        "python:function:pkg.beta.target",
    );
    insert_calls_edge(
        &conn,
        "python:function:pkg.alpha.source",
        "python:function:pkg.beta.target",
        EdgeConfidence::Inferred,
        &[],
    );

    let edges = module_dependency_edges(&conn, &["calls"]).expect("module dependency edges");

    assert!(
        edges.is_empty(),
        "query-time inferred calls must not contaminate Phase 3 clustering input"
    );
}

#[test]
fn module_dependency_edges_expands_ambiguous_call_candidates() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    for id in [
        "python:module:pkg.alpha",
        "python:module:pkg.beta",
        "python:module:pkg.gamma",
    ] {
        insert_entity(&conn, id, "module");
    }
    for id in [
        "python:function:pkg.alpha.source",
        "python:function:pkg.beta.first",
        "python:function:pkg.gamma.second",
    ] {
        insert_entity(&conn, id, "function");
    }
    insert_contains_edge(
        &conn,
        "python:module:pkg.alpha",
        "python:function:pkg.alpha.source",
    );
    insert_contains_edge(
        &conn,
        "python:module:pkg.beta",
        "python:function:pkg.beta.first",
    );
    insert_contains_edge(
        &conn,
        "python:module:pkg.gamma",
        "python:function:pkg.gamma.second",
    );
    insert_calls_edge(
        &conn,
        "python:function:pkg.alpha.source",
        "python:function:pkg.beta.first",
        EdgeConfidence::Ambiguous,
        &[
            "python:function:pkg.beta.first",
            "python:function:pkg.gamma.second",
        ],
    );

    let edges = module_dependency_edges(&conn, &["calls"]).expect("module dependency edges");

    assert_eq!(
        edges,
        vec![
            ModuleDependencyEdge {
                from_module_id: "python:module:pkg.alpha".to_owned(),
                to_module_id: "python:module:pkg.beta".to_owned(),
                reference_count: 1,
                edge_kinds: vec!["calls".to_owned()],
            },
            ModuleDependencyEdge {
                from_module_id: "python:module:pkg.alpha".to_owned(),
                to_module_id: "python:module:pkg.gamma".to_owned(),
                reference_count: 1,
                edge_kinds: vec!["calls".to_owned()],
            },
        ],
    );
}

#[test]
fn subsystem_members_returns_modules_ordered_by_name() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_named_entity(
        &conn,
        "core:subsystem:abc123def456",
        "subsystem",
        "Auth subsystem",
        "Auth subsystem",
        None,
    );
    insert_named_entity(
        &conn,
        "python:module:pkg.beta",
        "module",
        "pkg.beta",
        "beta",
        Some("/tmp/pkg/beta.py"),
    );
    insert_named_entity(
        &conn,
        "python:module:pkg.alpha",
        "module",
        "pkg.alpha",
        "alpha",
        Some("/tmp/pkg/alpha.py"),
    );
    insert_in_subsystem_edge(
        &conn,
        "python:module:pkg.beta",
        "core:subsystem:abc123def456",
    );
    insert_in_subsystem_edge(
        &conn,
        "python:module:pkg.alpha",
        "core:subsystem:abc123def456",
    );

    let members =
        subsystem_members(&conn, "core:subsystem:abc123def456").expect("subsystem members");
    let subsystem =
        subsystem_for_member(&conn, "python:module:pkg.alpha").expect("subsystem for member");

    assert_eq!(
        members,
        vec![
            SubsystemMember {
                id: "python:module:pkg.alpha".to_owned(),
                name: "pkg.alpha".to_owned(),
                source_file_path: Some("/tmp/pkg/alpha.py".to_owned()),
            },
            SubsystemMember {
                id: "python:module:pkg.beta".to_owned(),
                name: "pkg.beta".to_owned(),
                source_file_path: Some("/tmp/pkg/beta.py".to_owned()),
            },
        ],
    );
    assert_eq!(subsystem, Some("core:subsystem:abc123def456".to_owned()));
    assert_eq!(
        subsystem_for_member(&conn, "python:module:pkg.gamma").expect("unknown member"),
        None,
    );
}

#[test]
fn reference_edges_for_entity_returns_directional_neighbors() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:function:demo.source", "function");
    insert_entity(&conn, "python:function:demo.target", "function");
    insert_entity(&conn, "python:function:demo.outbound", "function");
    insert_references_edge(
        &conn,
        "python:function:demo.source",
        "python:function:demo.target",
        EdgeConfidence::Resolved,
        20,
        25,
    );
    insert_references_edge(
        &conn,
        "python:function:demo.target",
        "python:function:demo.outbound",
        EdgeConfidence::Ambiguous,
        30,
        39,
    );

    let inbound =
        reference_edges_for_entity(&conn, "python:function:demo.target", ReferenceDirection::In)
            .expect("query inbound references");
    let outbound = reference_edges_for_entity(
        &conn,
        "python:function:demo.target",
        ReferenceDirection::Out,
    )
    .expect("query outbound references");

    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].neighbor_id, "python:function:demo.source");
    assert_eq!(inbound[0].confidence, EdgeConfidence::Resolved);
    assert_eq!(inbound[0].source_byte_start, Some(20));
    assert_eq!(inbound[0].source_byte_end, Some(25));

    assert_eq!(outbound.len(), 1);
    assert_eq!(outbound[0].neighbor_id, "python:function:demo.outbound");
    assert_eq!(outbound[0].confidence, EdgeConfidence::Ambiguous);
    assert_eq!(outbound[0].source_byte_start, Some(30));
    assert_eq!(outbound[0].source_byte_end, Some(39));
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
fn child_entity_ids_returns_only_direct_contains_children() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    for id in [
        "python:module:demo",
        "python:class:demo.Demo",
        "python:function:demo.Demo.method",
    ] {
        insert_entity(&conn, id, "function");
    }
    insert_contains_edge(&conn, "python:module:demo", "python:class:demo.Demo");
    insert_contains_edge(
        &conn,
        "python:class:demo.Demo",
        "python:function:demo.Demo.method",
    );

    let children = child_entity_ids(&conn, "python:module:demo").expect("direct children");

    assert_eq!(children, vec!["python:class:demo.Demo".to_owned()]);
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
