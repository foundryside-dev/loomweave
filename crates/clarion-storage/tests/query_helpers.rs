//! Storage query helper tests for the B.6 MCP surface.

use std::path::Path;

use clarion_core::EdgeConfidence;
use clarion_storage::{
    ModuleDependencyEdge, ReferenceDirection, SubsystemMember, call_edges_from,
    call_edges_targeting, child_entity_ids, contained_entity_ids, containing_module_id,
    entity_at_line, entity_briefing_block_reason, entity_by_id, find_entities, findings_for_emit,
    module_dependency_edges, module_reference_rollup, normalize_source_path, pragma,
    reference_edges_for_entity, resolve_file, resolve_file_catalog_entry, schema,
    subsystem_for_member, subsystem_members, subsystem_of_entity,
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
fn module_reference_rollup_aggregates_contained_symbol_edges_excluding_internal() {
    // A `from pkg.contracts import RunStatus` records a `references` edge to the
    // class, not the module — so the module's own edges are empty and the
    // rollup must aggregate contained symbols' edges (clarion-79d0ff6e14).
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    // Module under query, with two contained classes.
    insert_entity(&conn, "python:module:pkg.contracts", "module");
    insert_entity(&conn, "python:class:pkg.contracts.RunStatus", "class");
    insert_entity(&conn, "python:class:pkg.contracts.Helper", "class");
    insert_contains_edge(
        &conn,
        "python:module:pkg.contracts",
        "python:class:pkg.contracts.RunStatus",
    );
    insert_contains_edge(
        &conn,
        "python:module:pkg.contracts",
        "python:class:pkg.contracts.Helper",
    );

    // An external module whose function imports RunStatus (reverse-import In).
    insert_entity(&conn, "python:module:pkg.consumer", "module");
    insert_entity(&conn, "python:function:pkg.consumer.use", "function");
    insert_contains_edge(
        &conn,
        "python:module:pkg.consumer",
        "python:function:pkg.consumer.use",
    );
    insert_references_edge(
        &conn,
        "python:function:pkg.consumer.use",
        "python:class:pkg.contracts.RunStatus",
        EdgeConfidence::Resolved,
        20,
        25,
    );

    // A symbol the module's class references outward (rollup Out).
    insert_entity(&conn, "python:module:pkg.other", "module");
    insert_entity(&conn, "python:class:pkg.other.Thing", "class");
    insert_contains_edge(
        &conn,
        "python:module:pkg.other",
        "python:class:pkg.other.Thing",
    );
    insert_references_edge(
        &conn,
        "python:class:pkg.contracts.RunStatus",
        "python:class:pkg.other.Thing",
        EdgeConfidence::Resolved,
        30,
        40,
    );

    // Intra-module reference (RunStatus -> Helper): internal wiring, must be
    // excluded from BOTH directions of the rollup.
    insert_references_edge(
        &conn,
        "python:class:pkg.contracts.RunStatus",
        "python:class:pkg.contracts.Helper",
        EdgeConfidence::Resolved,
        50,
        55,
    );

    let inbound =
        module_reference_rollup(&conn, "python:module:pkg.contracts", ReferenceDirection::In)
            .expect("rollup inbound");
    assert_eq!(
        inbound.len(),
        1,
        "only the external referencer rolls up: {inbound:?}"
    );
    assert_eq!(inbound[0].neighbor_id, "python:function:pkg.consumer.use");
    assert_eq!(inbound[0].via_id, "python:class:pkg.contracts.RunStatus");
    assert_eq!(inbound[0].confidence, EdgeConfidence::Resolved);
    assert_eq!(inbound[0].source_byte_start, Some(20));

    let outbound = module_reference_rollup(
        &conn,
        "python:module:pkg.contracts",
        ReferenceDirection::Out,
    )
    .expect("rollup outbound");
    assert_eq!(
        outbound.len(),
        1,
        "only the external reference rolls up: {outbound:?}"
    );
    assert_eq!(outbound[0].neighbor_id, "python:class:pkg.other.Thing");
    assert_eq!(outbound[0].via_id, "python:class:pkg.contracts.RunStatus");
}

#[test]
fn module_reference_rollup_returns_empty_for_module_with_no_contained_edges() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:module:pkg.lonely", "module");
    insert_entity(&conn, "python:class:pkg.lonely.Isolated", "class");
    insert_contains_edge(
        &conn,
        "python:module:pkg.lonely",
        "python:class:pkg.lonely.Isolated",
    );

    let inbound =
        module_reference_rollup(&conn, "python:module:pkg.lonely", ReferenceDirection::In)
            .expect("rollup inbound");
    assert!(
        inbound.is_empty(),
        "no references means an empty rollup, not an error"
    );
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

    let fts_results = find_entities(&conn, "TokenManager", 20, 0, None).expect("FTS search");
    assert_eq!(fts_results.len(), 1);
    assert_eq!(fts_results[0].id, "python:function:demo.TokenManager");

    let like_results = find_entities(&conn, "python:function:demo.TokenManager", 20, 0, None)
        .expect("punctuation-heavy ID search");
    assert_eq!(like_results.len(), 1);
    assert_eq!(like_results[0].id, "python:function:demo.TokenManager");
}

#[test]
fn subsystem_of_entity_resolves_module_and_nested_entities() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "core:subsystem:abc", "subsystem");
    insert_entity(&conn, "python:module:pkg.mod", "module");
    insert_entity(&conn, "python:class:pkg.mod.Cls", "class");
    insert_entity(&conn, "python:function:pkg.mod.Cls.method", "function");
    insert_in_subsystem_edge(&conn, "python:module:pkg.mod", "core:subsystem:abc");
    insert_contains_edge(&conn, "python:module:pkg.mod", "python:class:pkg.mod.Cls");
    insert_contains_edge(
        &conn,
        "python:class:pkg.mod.Cls",
        "python:function:pkg.mod.Cls.method",
    );

    // A module resolves directly (depth 0).
    let from_module = subsystem_of_entity(&conn, "python:module:pkg.mod")
        .unwrap()
        .expect("module should resolve");
    assert_eq!(from_module.subsystem_id, "core:subsystem:abc");
    assert_eq!(from_module.via_module_id, "python:module:pkg.mod");

    // A method nested module -> class -> function resolves via its module
    // ancestor (exercises the recursive walk past a non-module container).
    let from_method = subsystem_of_entity(&conn, "python:function:pkg.mod.Cls.method")
        .unwrap()
        .expect("nested method should resolve");
    assert_eq!(from_method.subsystem_id, "core:subsystem:abc");
    assert_eq!(from_method.via_module_id, "python:module:pkg.mod");

    // A module not assigned to any subsystem -> None.
    insert_entity(&conn, "python:module:orphan", "module");
    assert!(
        subsystem_of_entity(&conn, "python:module:orphan")
            .unwrap()
            .is_none()
    );

    // An unknown entity id -> None (no error).
    assert!(
        subsystem_of_entity(&conn, "python:function:does.not.exist")
            .unwrap()
            .is_none()
    );
}

#[test]
fn find_entities_kind_filter_constrains_results() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    // Two entities sharing a search term but differing in kind, plus a subsystem
    // named after the same package (the realistic "find the subsystem" case).
    insert_entity(&conn, "python:module:demo", "module");
    insert_entity(&conn, "python:function:demo.run", "function");
    insert_entity(&conn, "core:subsystem:demo", "subsystem");

    // Unfiltered: all three "demo" entities match (FTS or LIKE path).
    let all = find_entities(&conn, "demo", 20, 0, None).expect("unfiltered search");
    assert_eq!(all.len(), 3, "{all:?}");

    // kind=subsystem returns only the subsystem entity.
    let subs = find_entities(&conn, "demo", 20, 0, Some("subsystem")).expect("kind=subsystem");
    assert_eq!(subs.len(), 1, "{subs:?}");
    assert_eq!(subs[0].id, "core:subsystem:demo");
    assert_eq!(subs[0].kind, "subsystem");

    // kind=function returns only the function.
    let funcs = find_entities(&conn, "demo", 20, 0, Some("function")).expect("kind=function");
    assert_eq!(funcs.len(), 1, "{funcs:?}");
    assert_eq!(funcs[0].id, "python:function:demo.run");

    // An unknown (but well-formed) kind simply matches nothing.
    let none = find_entities(&conn, "demo", 20, 0, Some("nonesuch")).expect("unknown kind");
    assert!(none.is_empty(), "{none:?}");

    // A blank kind is rejected as a malformed request.
    assert!(find_entities(&conn, "demo", 20, 0, Some("  ")).is_err());
}

#[test]
fn find_entities_kind_filter_applies_on_punctuation_like_path() {
    // The punctuation-heavy ID search takes the LIKE branch (not FTS); the kind
    // filter must apply there too, and bind correctly against the OR-group.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_entity(&conn, "python:module:pkg.svc", "module");
    insert_entity(&conn, "python:function:pkg.svc", "function");

    let like_all = find_entities(&conn, "python:module:pkg.svc", 20, 0, None).expect("like search");
    assert_eq!(like_all.len(), 1);

    let like_module = find_entities(&conn, "pkg.svc", 20, 0, Some("module")).expect("like+kind");
    assert!(
        like_module.iter().all(|e| e.kind == "module"),
        "{like_module:?}"
    );
    assert!(
        like_module.iter().any(|e| e.id == "python:module:pkg.svc"),
        "{like_module:?}"
    );
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

#[test]
fn resolve_file_surfaces_briefing_blocked_reason_from_properties() {
    let tempdir = tempfile::tempdir().expect("temp project root");
    let project_root = tempdir.path();
    let source_path = project_root.join("secret.env");
    std::fs::write(&source_path, "TOKEN=AKIAIOSFODNN7EXAMPLE\n").expect("write source");
    let canonical = source_path.canonicalize().expect("canonical source");

    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'core:file:hash-secret@secret.env', 'core', 'file', 'secret.env', 'secret.env', ?1,
            1, 1, '{\"briefing_blocked\":\"secret_present\"}', 'hash-secret-file',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical.display().to_string()],
    )
    .expect("insert briefing-blocked entity");

    let resolved = resolve_file(&conn, project_root, "secret.env", "env")
        .expect("resolve_file")
        .expect("entity is known");

    assert_eq!(
        resolved.briefing_blocked.as_deref(),
        Some("secret_present"),
        "resolve_file must surface briefing_blocked reason so federation read \
         surfaces (HTTP /api/v1/files) can refuse to expose blocked entities"
    );
}

#[test]
fn resolve_file_returns_none_when_no_file_kind_entity_exists() {
    let tempdir = tempfile::tempdir().expect("temp project root");
    let project_root = tempdir.path();
    let source_path = project_root.join("src").join("demo.py");
    std::fs::create_dir_all(source_path.parent().unwrap()).expect("create source dir");
    std::fs::write(&source_path, "def entry():\n    return 1\n").expect("write source");
    let canonical = source_path.canonicalize().expect("canonical source");

    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'python:module:demo', 'python', 'module', 'demo', 'demo', ?1,
            1, 2, '{}', 'hash-demo-module',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical.display().to_string()],
    )
    .expect("insert module entity");

    let resolved =
        resolve_file(&conn, project_root, "src/demo.py", "python").expect("resolve_file");

    assert!(
        resolved.is_none(),
        "resolve_file must fail closed instead of synthesizing a file identity from a module row"
    );
}

#[test]
fn resolve_file_returns_none_briefing_blocked_for_clean_entity() {
    let tempdir = tempfile::tempdir().expect("temp project root");
    let project_root = tempdir.path();
    let source_path = project_root.join("src").join("demo.py");
    std::fs::create_dir_all(source_path.parent().unwrap()).expect("create source dir");
    std::fs::write(&source_path, "def entry():\n    return 1\n").expect("write source");
    let canonical = source_path.canonicalize().expect("canonical source");

    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'python:file:demo', 'python', 'file', 'demo.py', 'demo.py', ?1,
            1, 2, '{}', 'hash-demo',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical.display().to_string()],
    )
    .expect("insert clean entity");

    let resolved = resolve_file(&conn, project_root, "src/demo.py", "python")
        .expect("resolve_file")
        .expect("entity is known");

    assert_eq!(resolved.canonical_path.as_str(), "src/demo.py");
    assert!(
        !resolved.canonical_path.as_str().starts_with('/')
            && !resolved.canonical_path.as_str().starts_with("./")
            && !resolved.canonical_path.as_str().starts_with("../"),
        "canonical path must be project-relative POSIX: {:?}",
        resolved.canonical_path
    );
    assert!(
        resolved.briefing_blocked.is_none(),
        "clean entity must not surface a briefing_blocked reason; got {:?}",
        resolved.briefing_blocked
    );
}

#[test]
fn resolve_file_deleted_on_disk_but_cataloged_row_resolves() {
    let tempdir = tempfile::tempdir().expect("temp project root");
    let project_root = tempdir.path();
    let source_path = project_root.join("src").join("deleted.py");
    std::fs::create_dir_all(source_path.parent().unwrap()).expect("create source dir");
    std::fs::write(&source_path, "def gone():\n    return 1\n").expect("write source");
    let canonical = source_path.canonicalize().expect("canonical source");

    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'python:file:deleted', 'python', 'file', 'deleted.py', 'deleted.py', ?1,
            1, 2, '{}', 'hash-deleted',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical.display().to_string()],
    )
    .expect("insert deleted entity");
    std::fs::remove_file(&source_path).expect("delete source after cataloging");

    let resolved = resolve_file(&conn, project_root, "src/deleted.py", "python")
        .expect("resolve_file should use catalog row without requiring disk file")
        .expect("entity is known");

    assert_eq!(resolved.entity_id, "python:file:deleted");
    assert_eq!(resolved.content_hash, "hash-deleted");
    assert_eq!(resolved.canonical_path.as_str(), "src/deleted.py");
    assert!(
        !resolved.canonical_path.as_str().starts_with('/')
            && !resolved.canonical_path.as_str().starts_with("./")
            && !resolved.canonical_path.as_str().starts_with("../"),
        "canonical path must be project-relative POSIX: {:?}",
        resolved.canonical_path
    );
}

#[test]
fn resolve_file_catalog_entry_returns_missing_hash_without_reading_disk() {
    let tempdir = tempfile::tempdir().expect("temp project root");
    let project_root = tempdir.path();
    let source_path = project_root.join("src").join("missing-hash.py");
    std::fs::create_dir_all(source_path.parent().unwrap()).expect("create source dir");
    std::fs::write(&source_path, "def missing_hash():\n    return 1\n").expect("write source");
    let canonical = source_path.canonicalize().expect("canonical source");

    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'python:file:missing_hash', 'python', 'file', 'missing-hash.py', 'missing-hash.py', ?1,
            1, 2, '{}', NULL,
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical.display().to_string()],
    )
    .expect("insert file entity without cached hash");
    std::fs::remove_file(&source_path).expect("delete source after cataloging");

    let entry = resolve_file_catalog_entry(&conn, project_root, "src/missing-hash.py", "python")
        .expect("catalog lookup should not read deleted source")
        .expect("entity is known");

    assert_eq!(entry.entity_id, "python:file:missing_hash");
    assert_eq!(entry.content_hash, None);
    assert_eq!(entry.canonical_path.as_str(), "src/missing-hash.py");
    assert_eq!(entry.language, "python");
}

#[test]
#[cfg(unix)]
fn resolve_file_unreadable_hash_failure_propagates() {
    use std::os::unix::fs::PermissionsExt;

    let tempdir = tempfile::tempdir().expect("temp project root");
    let project_root = tempdir.path();
    let source_path = project_root.join("src").join("unreadable.py");
    std::fs::create_dir_all(source_path.parent().unwrap()).expect("create source dir");
    std::fs::write(&source_path, "def unreadable():\n    return 1\n").expect("write source");
    let canonical = source_path.canonicalize().expect("canonical source");

    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'python:file:unreadable', 'python', 'file', 'unreadable.py', 'unreadable.py', ?1,
            1, 2, '{}', NULL,
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical.display().to_string()],
    )
    .expect("insert unreadable entity");
    let original_permissions = std::fs::metadata(&source_path)
        .expect("source metadata")
        .permissions();
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o000))
        .expect("make source unreadable");

    if std::fs::read(&source_path).is_ok() {
        std::fs::set_permissions(&source_path, original_permissions).expect("restore source perms");
        eprintln!("skipping unreadable-file assertion because this runner can read 0o000 files");
        return;
    }

    let result = resolve_file(&conn, project_root, "src/unreadable.py", "python");

    std::fs::set_permissions(&source_path, original_permissions).expect("restore source perms");
    let error = result.expect_err("missing catalog hash must propagate hash fallback read failure");
    assert!(
        error.to_string().contains("io error"),
        "unexpected error: {error}"
    );
}

#[test]
fn resolve_file_does_not_echo_invalid_requested_language_over_catalog_inference() {
    let tempdir = tempfile::tempdir().expect("temp project root");
    let project_root = tempdir.path();
    let source_path = project_root.join("src").join("demo.py");
    std::fs::create_dir_all(source_path.parent().unwrap()).expect("create source dir");
    std::fs::write(&source_path, "def entry():\n    return 1\n").expect("write source");
    let canonical = source_path.canonicalize().expect("canonical source");

    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'python:file:demo-language', 'python', 'file', 'demo.py', 'demo.py', ?1,
            1, 2, '{}', 'hash-demo-language',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical.display().to_string()],
    )
    .expect("insert python entity");

    let resolved = resolve_file(&conn, project_root, "src/demo.py", "javascript")
        .expect("resolve_file")
        .expect("entity is known");

    assert_eq!(resolved.language, "python");
}

#[test]
fn resolve_file_prefers_core_extension_inference_over_requested_language() {
    let tempdir = tempfile::tempdir().expect("temp project root");
    let project_root = tempdir.path();
    let source_path = project_root.join("src").join("demo.py");
    std::fs::create_dir_all(source_path.parent().unwrap()).expect("create source dir");
    std::fs::write(&source_path, "def entry():\n    return 1\n").expect("write source");
    let canonical = source_path.canonicalize().expect("canonical source");

    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'core:file:src/demo.py', 'core', 'file', 'demo.py', 'demo.py', ?1,
            1, 2, '{}', 'hash-core-demo',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical.display().to_string()],
    )
    .expect("insert core file entity");

    let resolved = resolve_file(&conn, project_root, "src/demo.py", "javascript")
        .expect("resolve_file")
        .expect("entity is known");

    assert_eq!(resolved.language, "python");
}

#[test]
fn entity_briefing_block_reason_parses_property_and_tolerates_garbage() {
    assert_eq!(
        entity_briefing_block_reason(r#"{"briefing_blocked":"secret_present"}"#),
        Some("secret_present".to_owned()),
    );
    assert_eq!(
        entity_briefing_block_reason(r#"{"briefing_blocked":"unscanned_source"}"#),
        Some("unscanned_source".to_owned()),
    );
    // No key.
    assert_eq!(entity_briefing_block_reason("{}"), None);
    assert_eq!(entity_briefing_block_reason(r#"{"other":"x"}"#), None);
    // Wrong type — key present but not a string.
    assert_eq!(
        entity_briefing_block_reason(r#"{"briefing_blocked":42}"#),
        None,
    );
}

#[test]
fn entity_briefing_block_reason_fails_closed_on_malformed_json() {
    // Fail-closed contract (SEC-01): a plugin emitting malformed
    // properties JSON must not be able to silently unblock the entity
    // through the federation read paths. Any parse failure returns
    // Some("malformed_properties_json") so callers treat the row as
    // briefing-blocked.
    let expected = Some("malformed_properties_json".to_owned());
    assert_eq!(entity_briefing_block_reason(""), expected);
    assert_eq!(entity_briefing_block_reason("not json"), expected);
    assert_eq!(entity_briefing_block_reason(r#"{"unterminated"#), expected);
    assert_eq!(entity_briefing_block_reason(r#"{"x":}"#), expected);
    // Valid JSON whose root is not an object still parses and follows the
    // non-malformed path (no `briefing_blocked` key on a JSON `null`/array,
    // so the reason is `None`).
    assert_eq!(entity_briefing_block_reason("null"), None);
    assert_eq!(entity_briefing_block_reason("[]"), None);
}

fn insert_run(conn: &Connection, run_id: &str) {
    conn.execute(
        "INSERT INTO runs (id, started_at, config, stats, status) \
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), '{}', '{}', 'running')",
        params![run_id],
    )
    .expect("insert run");
}

#[allow(clippy::too_many_arguments)]
fn insert_finding(
    conn: &Connection,
    id: &str,
    run_id: &str,
    rule_id: &str,
    kind: &str,
    severity: &str,
    entity_id: &str,
    related_entities: &str,
) {
    conn.execute(
        "INSERT INTO findings (
            id, tool, tool_version, run_id, rule_id, kind, severity, confidence,
            confidence_basis, entity_id, related_entities, message, evidence,
            properties, supports, supported_by, status, created_at, updated_at
         ) VALUES (
            ?1, 'clarion', '1.0.0', ?2, ?3, ?4, ?5, 0.9,
            'ast_match', ?6, ?7, 'msg', '{}',
            '{}', '[]', '[]', 'open',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        params![
            id,
            run_id,
            rule_id,
            kind,
            severity,
            entity_id,
            related_entities
        ],
    )
    .expect("insert finding");
}

#[test]
fn findings_for_emit_joins_entity_path_and_preserves_nullable_location() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let conn = open_fresh(&tempdir);

    insert_run(&conn, "run-1");
    // A defect anchored to a function with a source location.
    insert_entity_with_range(
        &conn,
        "python:function:auth.tokens.refresh",
        "function",
        Path::new("src/auth/tokens.py"),
        12,
        20,
    );
    // A fact anchored to a subsystem entity — no source_file_path.
    insert_named_entity(
        &conn,
        "core:subsystem:abcd",
        "subsystem",
        "abcd",
        "abcd",
        None,
    );

    insert_finding(
        &conn,
        "core:finding:run-1:defect",
        "run-1",
        "CLA-PY-STRUCTURE-001",
        "defect",
        "WARN",
        "python:function:auth.tokens.refresh",
        r#"["python:class:auth.sessions::SessionStore"]"#,
    );
    insert_finding(
        &conn,
        "core:finding:run-1:weak-modularity",
        "run-1",
        "CLA-FACT-CLUSTERING-WEAK-MODULARITY",
        "fact",
        "INFO",
        "core:subsystem:abcd",
        "[]",
    );

    let rows = findings_for_emit(&conn, "run-1").expect("findings_for_emit");
    assert_eq!(rows.len(), 2, "both findings returned: {rows:?}");

    // Ordered by finding id: "defect" sorts before "weak-modularity".
    let defect = &rows[0];
    assert_eq!(defect.id, "core:finding:run-1:defect");
    assert_eq!(defect.rule_id, "CLA-PY-STRUCTURE-001");
    assert_eq!(defect.kind, "defect");
    assert_eq!(defect.severity, "WARN");
    assert_eq!(defect.entity_id, "python:function:auth.tokens.refresh");
    assert_eq!(
        defect.source_file_path.as_deref(),
        Some("src/auth/tokens.py")
    );
    assert_eq!(defect.source_line_start, Some(12));
    assert_eq!(defect.source_line_end, Some(20));
    assert_eq!(defect.confidence, Some(0.9));
    assert_eq!(
        defect.related_entities_json,
        r#"["python:class:auth.sessions::SessionStore"]"#
    );

    // The subsystem-anchored fact has no source path — the emitter will skip it.
    let fact = &rows[1];
    assert_eq!(fact.id, "core:finding:run-1:weak-modularity");
    assert_eq!(fact.entity_id, "core:subsystem:abcd");
    assert_eq!(fact.source_file_path, None);
    assert_eq!(fact.source_line_start, None);
}

#[test]
fn findings_for_emit_scopes_to_run_id() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let conn = open_fresh(&tempdir);

    insert_run(&conn, "run-1");
    insert_run(&conn, "run-2");
    insert_entity_with_range(
        &conn,
        "python:function:demo.f",
        "function",
        Path::new("demo.py"),
        1,
        2,
    );
    insert_finding(
        &conn,
        "f-run1",
        "run-1",
        "CLA-PY-X",
        "defect",
        "WARN",
        "python:function:demo.f",
        "[]",
    );
    insert_finding(
        &conn,
        "f-run2",
        "run-2",
        "CLA-PY-X",
        "defect",
        "WARN",
        "python:function:demo.f",
        "[]",
    );

    let rows = findings_for_emit(&conn, "run-1").expect("findings_for_emit");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "f-run1");
}

#[test]
fn containing_module_id_walks_up_to_the_nearest_module() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    // module -> class -> method nesting via `contains`.
    insert_entity(&conn, "python:module:pkg.mod", "module");
    insert_entity(&conn, "python:class:pkg.mod.Cls", "class");
    insert_entity(&conn, "python:function:pkg.mod.Cls.method", "function");
    insert_contains_edge(&conn, "python:module:pkg.mod", "python:class:pkg.mod.Cls");
    insert_contains_edge(
        &conn,
        "python:class:pkg.mod.Cls",
        "python:function:pkg.mod.Cls.method",
    );

    // A nested method resolves up through its class to the module.
    assert_eq!(
        containing_module_id(&conn, "python:function:pkg.mod.Cls.method")
            .expect("query")
            .as_deref(),
        Some("python:module:pkg.mod"),
    );
    // A module resolves to itself (depth 0).
    assert_eq!(
        containing_module_id(&conn, "python:module:pkg.mod")
            .expect("query")
            .as_deref(),
        Some("python:module:pkg.mod"),
    );
    // A symbol with no module ancestor returns None.
    insert_entity(&conn, "python:function:orphan", "function");
    assert_eq!(
        containing_module_id(&conn, "python:function:orphan").expect("query"),
        None,
    );
}
