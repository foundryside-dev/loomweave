//! Schema-apply integration tests.
//!
//! Verifies that migration 0001 produces every table, index, trigger,
//! generated column, and view from detailed-design.md §3, and that
//! applying migrations a second time is a no-op.

use rusqlite::{Connection, params};

use loomweave_storage::{Writer, error::StorageError, pragma, schema};

fn open_fresh(tempdir: &tempfile::TempDir) -> Connection {
    let path = tempdir.path().join("loomweave.db");
    let mut conn = Connection::open(&path).expect("open");
    pragma::apply_write_pragmas(&conn).expect("pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    conn
}

fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect()
}

fn trigger_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect()
}

fn view_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='view' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect()
}

fn index_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect()
}

fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect()
}

fn primary_key_columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let mut columns = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
        })
        .unwrap()
        .map(std::result::Result::unwrap)
        .filter(|(_, pk_position)| *pk_position > 0)
        .collect::<Vec<_>>();
    columns.sort_by_key(|(_, pk_position)| *pk_position);
    columns.into_iter().map(|(name, _)| name).collect()
}

#[test]
fn migration_0001_creates_every_expected_table() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let tables = table_names(&conn);
    for expected in &[
        "edges",
        "entities",
        "entity_tags",
        "findings",
        "inferred_edge_cache",
        "entity_unresolved_call_sites",
        "runs",
        "schema_migrations",
        "summary_cache",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected} in {tables:?}"
        );
    }
}

#[test]
fn entity_tags_primary_key_includes_tag_owner_plugin() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    assert_eq!(
        table_columns(&conn, "entity_tags"),
        ["entity_id", "plugin_id", "tag"]
    );
    assert_eq!(
        primary_key_columns(&conn, "entity_tags"),
        ["entity_id", "plugin_id", "tag"]
    );
}

#[test]
fn runs_table_records_owner_pid_and_heartbeat() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let columns = table_columns(&conn, "runs");

    for expected in ["owner_pid", "heartbeat_at"] {
        assert!(
            columns.iter().any(|column| column == expected),
            "missing runs.{expected} in {columns:?}"
        );
    }

    let indexes = index_names(&conn);
    assert!(
        indexes.iter().any(|idx| idx == "ix_runs_running_heartbeat"),
        "missing running-heartbeat index in {indexes:?}"
    );
}

#[test]
fn migration_0001_creates_entity_fts_virtual_table() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='entity_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(sql.contains("CREATE VIRTUAL TABLE"), "sql was: {sql}");
    conn.execute_batch("SELECT entity_id, name FROM entity_fts LIMIT 0")
        .expect("entity_fts queryable");
}

#[test]
fn migration_0001_creates_entity_source_file_path_column_and_index() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    conn.execute_batch("SELECT source_file_path FROM entities LIMIT 0")
        .expect("entities.source_file_path is queryable");

    let indexes = index_names(&conn);
    assert!(
        indexes
            .iter()
            .any(|idx| idx == "ix_entities_source_file_path"),
        "missing source-file path index in {indexes:?}"
    );
}

#[test]
fn schema_accepts_open_entity_and_edge_kinds() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, properties, created_at, updated_at
         ) VALUES (
            'custom:widget:left', 'custom', 'widget', 'left', 'left', '{}',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert custom source entity kind");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, properties, created_at, updated_at
         ) VALUES (
            'custom:gadget:right', 'custom', 'gadget', 'right', 'right', '{}',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert custom target entity kind");
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence)
         VALUES ('custom_relation', 'custom:widget:left', 'custom:gadget:right', 'resolved')",
        [],
    )
    .expect("insert custom edge kind");
}

#[test]
fn migration_0001_extends_summary_cache_for_mcp_staleness_tracking() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let columns = table_columns(&conn, "summary_cache");

    for expected in &[
        "last_accessed_at",
        "caller_count",
        "fan_out",
        "stale_semantic",
    ] {
        assert!(
            columns.iter().any(|column| column == expected),
            "missing summary_cache.{expected} in {columns:?}"
        );
    }

    // summary_cache.entity_id has an FK to entities(id) per V11-STO-03;
    // seed the parent row first so the INSERT below isn't FK-rejected.
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            "python:function:demo.hello",
            "python",
            "function",
            "demo.hello",
            "hello",
            "{}",
            "2026-05-17T00:00:00.000Z",
            "2026-05-17T00:00:00.000Z",
        ],
    )
    .expect("seed summary_cache parent entity");

    conn.execute(
        "INSERT INTO summary_cache ( \
            entity_id, content_hash, prompt_template_id, model_tier, \
            guidance_fingerprint, summary_json, cost_usd, tokens_input, \
            tokens_output, created_at, last_accessed_at, caller_count, \
            fan_out, stale_semantic \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            "python:function:demo.hello",
            "hash-a",
            "leaf-v1",
            "claude-haiku-4-5",
            "guidance-empty",
            r#"{"purpose":"demo"}"#,
            0.001_f64,
            100_i64,
            20_i64,
            "2026-05-17T00:00:00.000Z",
            "2026-05-17T00:00:01.000Z",
            2_i64,
            1_i64,
            0_i64,
        ],
    )
    .expect("summary_cache should accept ADR-007 MCP freshness columns");

    let stale_semantic: i64 = conn
        .query_row(
            "SELECT stale_semantic FROM summary_cache WHERE entity_id = ?1",
            params!["python:function:demo.hello"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale_semantic, 0);
}

#[test]
fn migration_0001_creates_inferred_edge_cache_with_four_part_key() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let columns = table_columns(&conn, "inferred_edge_cache");

    for expected in &[
        "caller_entity_id",
        "caller_content_hash",
        "model_id",
        "prompt_version",
        "result_json",
        "last_accessed_at",
    ] {
        assert!(
            columns.iter().any(|column| column == expected),
            "missing inferred_edge_cache.{expected} in {columns:?}"
        );
    }

    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            "python:function:demo.caller",
            "python",
            "function",
            "demo.caller",
            "caller",
            "{}",
            "2026-05-17T00:00:00.000Z",
            "2026-05-17T00:00:00.000Z",
        ],
    )
    .expect("seed caller entity for inferred-edge cache FK");

    conn.execute(
        "INSERT INTO inferred_edge_cache ( \
            caller_entity_id, caller_content_hash, model_id, prompt_version, \
            result_json, cost_usd, token_count, created_at, last_accessed_at \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            "python:function:demo.caller",
            "hash-caller",
            "claude-haiku-4-5",
            "inferred-calls-v1",
            r#"{"edges":[]}"#,
            0.002_f64,
            80_i64,
            "2026-05-17T00:00:00.000Z",
            "2026-05-17T00:00:01.000Z",
        ],
    )
    .expect("inferred_edge_cache should accept D5 cache rows");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM inferred_edge_cache \
             WHERE caller_entity_id = ?1 AND caller_content_hash = ?2 \
               AND model_id = ?3 AND prompt_version = ?4",
            params![
                "python:function:demo.caller",
                "hash-caller",
                "claude-haiku-4-5",
                "inferred-calls-v1",
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn migration_0001_creates_unresolved_call_sites_table_and_indexes() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let columns = table_columns(&conn, "entity_unresolved_call_sites");

    for expected in &[
        "caller_entity_id",
        "caller_content_hash",
        "site_key",
        "site_ordinal",
        "source_file_id",
        "source_byte_start",
        "source_byte_end",
        "callee_expr",
        "created_at",
    ] {
        assert!(
            columns.iter().any(|column| column == expected),
            "missing entity_unresolved_call_sites.{expected} in {columns:?}"
        );
    }

    let indexes = index_names(&conn);
    for expected in &[
        "ix_unresolved_call_sites_caller",
        "ix_unresolved_call_sites_expr",
    ] {
        assert!(
            indexes.iter().any(|index| index == expected),
            "missing unresolved-call-site index {expected} in {indexes:?}"
        );
    }
}

#[test]
fn migration_0001_creates_all_three_fts_triggers() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let triggers = trigger_names(&conn);
    for expected in &["entities_ad", "entities_ai", "entities_au"] {
        assert!(
            triggers.iter().any(|t| t == expected),
            "missing trigger {expected} in {triggers:?}"
        );
    }
}

#[test]
fn migration_0001_creates_guidance_sheets_view() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let views = view_names(&conn);
    assert!(
        views.iter().any(|v| v == "guidance_sheets"),
        "views: {views:?}"
    );
    conn.execute_batch(
        "SELECT id, name, scope_level, scope_rank, pinned, provenance \
         FROM guidance_sheets LIMIT 0",
    )
    .expect("guidance_sheets queryable");
}

#[test]
fn migration_0001_creates_partial_indexes() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let indexes = index_names(&conn);
    for expected in &[
        "ix_entities_churn",
        "ix_entities_scope_rank",
        "ix_entities_briefing_blocked",
    ] {
        assert!(
            indexes.iter().any(|i| i == expected),
            "missing index {expected} in {indexes:?}"
        );
    }
}

#[test]
fn entity_generated_columns_extract_from_properties_json() {
    // Round-trips a guidance entity's scope_level / scope_rank / git_churn_count
    // generated columns. scope_level (TEXT) carries the enum value verbatim;
    // scope_rank (INTEGER) is CASE-mapped per ADR-024 so that ORDER BY
    // scope_rank produces the documented composition order
    // project→subsystem→package→module→class→function (1..6).
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let props = r#"{"scope_level": "subsystem", "git_churn_count": 42}"#;
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![
            "core:guidance:demo.subsystem-sheet",
            "core",
            "guidance",
            "demo.subsystem-sheet",
            "subsystem-sheet",
            props
        ],
    )
    .unwrap();
    let (scope_level, scope_rank, churn): (Option<String>, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT scope_level, scope_rank, git_churn_count FROM entities WHERE id = ?1",
            params!["core:guidance:demo.subsystem-sheet"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(scope_level.as_deref(), Some("subsystem"));
    assert_eq!(scope_rank, Some(2));
    assert_eq!(churn, Some(42));
}

#[test]
fn briefing_blocked_generated_column_reflects_property_and_partial_index() {
    // The briefing_blocked generated column extracts $.briefing_blocked, is NULL
    // when the property is absent (so the partial index stays small), and the
    // partial index query counts exactly the blocked entities (clarion-bdabfd6bca).
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    let insert = |id: &str, props: &str| {
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
             created_at, updated_at) \
             VALUES (?1, 'python', 'function', ?1, ?1, ?2, \
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            params![id, props],
        )
        .unwrap();
    };
    insert(
        "python:function:demo.blocked",
        r#"{"briefing_blocked": "secret_detected"}"#,
    );
    insert("python:function:demo.clear", "{}");

    let blocked: Option<String> = conn
        .query_row(
            "SELECT briefing_blocked FROM entities WHERE id = ?1",
            params!["python:function:demo.blocked"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(blocked.as_deref(), Some("secret_detected"));

    let clear: Option<String> = conn
        .query_row(
            "SELECT briefing_blocked FROM entities WHERE id = ?1",
            params!["python:function:demo.clear"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(clear, None);

    // The partial index serves "how many entities are withheld" in SQL.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE briefing_blocked IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn scope_rank_case_mapping_covers_all_six_levels() {
    // Asserts the full CASE table in 0001_initial_schema.sql:
    // project=1, subsystem=2, package=3, module=4, class=5, function=6.
    // ORDER BY scope_rank ASC is the canonical guidance-composition order
    // (outer→inner, project outermost / function innermost; ADR-024).
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let cases: &[(&str, i64)] = &[
        ("project", 1),
        ("subsystem", 2),
        ("package", 3),
        ("module", 4),
        ("class", 5),
        ("function", 6),
    ];
    for (level, expected_rank) in cases {
        let id = format!("core:guidance:demo.level-{level}");
        let props = format!(r#"{{"scope_level": "{level}"}}"#);
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
             created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            params![&id, "core", "guidance", &id, level, &props],
        )
        .unwrap();
        let rank: Option<i64> = conn
            .query_row(
                "SELECT scope_rank FROM entities WHERE id = ?1",
                params![&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            rank,
            Some(*expected_rank),
            "scope_level {level:?} should map to scope_rank {expected_rank}",
        );
    }

    // An unknown enum value yields NULL (CASE has no ELSE branch); the
    // partial index `ix_entities_scope_rank ... WHERE scope_rank IS NOT NULL`
    // excludes such rows from the ordered index.
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![
            "core:guidance:demo.level-bogus",
            "core",
            "guidance",
            "demo.level-bogus",
            "level-bogus",
            r#"{"scope_level": "bogus"}"#,
        ],
    )
    .unwrap();
    let rank: Option<i64> = conn
        .query_row(
            "SELECT scope_rank FROM entities WHERE id = ?1",
            params!["core:guidance:demo.level-bogus"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rank, None, "unknown scope_level should produce NULL rank");
}

#[test]
fn fts_trigger_populates_entity_fts_on_insert() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let summary_json = r#"{"briefing": {"purpose": "refresh session tokens"}}"#;
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, summary, \
         created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, '{}', ?6, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![
            "python:function:auth.refresh",
            "python",
            "function",
            "auth.refresh",
            "refresh",
            summary_json,
        ],
    )
    .unwrap();

    // MATCH against the FTS5 virtual table; the entities_ai trigger should have
    // populated the summary_text field from summary.briefing.purpose.
    let matched_id: String = conn
        .query_row(
            "SELECT entity_id FROM entity_fts WHERE entity_fts MATCH 'refresh'",
            [],
            |row| row.get(0),
        )
        .expect("entity_fts row should exist after INSERT trigger fires");
    assert_eq!(matched_id, "python:function:auth.refresh");
}

#[test]
fn migration_0009_drops_dead_fts_content_text_column() {
    // V11-STO-06 / clarion-716449c371: the never-populated, never-read
    // content_text column is gone after 0009, and search via the recreated
    // virtual table + triggers still works.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='entity_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !sql.contains("content_text"),
        "entity_fts must not declare content_text after 0009; sql was: {sql}"
    );

    let summary_json = r#"{"briefing": {"purpose": "rotate signing keys"}}"#;
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, summary, \
         created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, '{}', ?6, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![
            "python:function:auth.rotate",
            "python",
            "function",
            "auth.rotate",
            "rotate",
            summary_json,
        ],
    )
    .unwrap();

    let matched_id: String = conn
        .query_row(
            "SELECT entity_id FROM entity_fts WHERE entity_fts MATCH 'rotate'",
            [],
            |row| row.get(0),
        )
        .expect("FTS search still works after content_text drop");
    assert_eq!(matched_id, "python:function:auth.rotate");
}

#[test]
fn edges_table_has_no_id_column() {
    // ADR-026 decision 4: drop synthetic `id` PK from edges. Natural key
    // `(kind, from_id, to_id)` is the only identity.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('edges')")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect();
    assert!(
        !columns.iter().any(|c| c == "id"),
        "edges should not have an id column post-ADR-026; columns: {columns:?}"
    );
}

#[test]
fn edges_table_primary_key_is_kind_from_to() {
    // ADR-026 decision 4: PK is the natural composite `(kind, from_id, to_id)`.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let mut pk_cols: Vec<(i64, String)> = conn
        .prepare("SELECT pk, name FROM pragma_table_info('edges') WHERE pk > 0 ORDER BY pk")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect();
    pk_cols.sort_by_key(|(rank, _)| *rank);
    let names: Vec<String> = pk_cols.into_iter().map(|(_, n)| n).collect();
    assert_eq!(
        names,
        vec![
            "kind".to_string(),
            "from_id".to_string(),
            "to_id".to_string()
        ],
        "edges PK must be (kind, from_id, to_id)"
    );
}

#[test]
fn edges_table_is_without_rowid() {
    // ADR-026 decision 4 / Q4 panel reconciliation: WITHOUT ROWID clause
    // optimises storage now that the natural PK obviates the rowid.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='edges'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let normalised = sql.to_ascii_uppercase();
    assert!(
        normalised.contains("WITHOUT ROWID"),
        "edges should be WITHOUT ROWID; sql was: {sql}"
    );
}

#[test]
fn edges_confidence_column_rejects_unknown_tier() {
    // ADR-028 decision 1: every edge row carries a confidence tier, constrained
    // to resolved / ambiguous / inferred so traversal filters are trustworthy.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    for id in ["python:function:demo.a", "python:function:demo.b"] {
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
             created_at, updated_at) \
             VALUES (?1, 'python', 'function', ?1, ?1, '{}', \
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            params![id],
        )
        .unwrap();
    }
    let err = conn
        .execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence) \
             VALUES ('contains', 'python:function:demo.a', 'python:function:demo.b', 'garbage')",
            [],
        )
        .expect_err("confidence CHECK should reject unknown edge tiers");
    assert!(
        err.to_string().contains("CHECK constraint failed"),
        "unexpected error for invalid confidence tier: {err}"
    );
}

#[test]
fn migration_0001_creates_edge_confidence_index() {
    // B.4* Q5: B.6's confidence-filtered traversals must not degrade to a full
    // scan; this index is the storage-side dispatch primitive.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let indexes = index_names(&conn);
    assert!(
        indexes.iter().any(|i| i == "ix_edges_kind_confidence"),
        "missing ix_edges_kind_confidence in {indexes:?}"
    );
}

#[test]
fn edge_confidence_filter_uses_dispatch_index() {
    // B.4* Q5: the B.6 traversal default filters by kind+confidence; assert
    // SQLite chooses the purpose-built index rather than a table scan.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) \
         VALUES ('python:module:demo', 'python', 'module', 'demo', 'demo', '{}', \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        [],
    )
    .unwrap();
    for i in 0..200 {
        let id = format!("python:function:demo.f{i:03}");
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, parent_id, \
             properties, created_at, updated_at) \
             VALUES (?1, 'python', 'function', ?1, ?1, 'python:module:demo', '{}', \
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            params![id],
        )
        .unwrap();
    }
    for i in 0..199 {
        let confidence = if i % 2 == 0 { "resolved" } else { "ambiguous" };
        let from_id = format!("python:function:demo.f{i:03}");
        let to_id = format!("python:function:demo.f{:03}", i + 1);
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence, source_byte_start, source_byte_end) \
             VALUES ('calls', ?1, ?2, ?3, 1, 2)",
            params![from_id, to_id, confidence],
        )
        .unwrap();
    }
    conn.execute_batch("ANALYZE").unwrap();
    let details: Vec<String> = conn
        .prepare("EXPLAIN QUERY PLAN SELECT * FROM edges WHERE kind = ?1 AND confidence = ?2")
        .unwrap()
        .query_map(params!["calls", "resolved"], |row| row.get::<_, String>(3))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("ix_edges_kind_confidence")),
        "expected ix_edges_kind_confidence in query plan; got {details:?}"
    );
}

#[test]
fn migrations_are_idempotent() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut conn = open_fresh(&tempdir);
    schema::apply_migrations(&mut conn).expect("second apply should be a no-op");
    assert_eq!(schema::applied_count(&conn).unwrap(), 10);
    let tables_after = table_names(&conn);
    assert!(tables_after.contains(&"entities".to_owned()));
}

#[test]
fn schema_migrations_records_each_applied_migration() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 10);
    let names: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM schema_migrations ORDER BY version")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get(0)).unwrap();
        rows.map(std::result::Result::unwrap).collect()
    };
    assert_eq!(
        names,
        vec![
            "0001_initial_schema",
            "0002_briefing_blocked",
            "0003_wardline_taint_facts",
            "0004_sei_prior_index",
            "0005_sei",
            "0006_wardline_taint_sei",
            "0007_run_analyzed_commit",
            "0008_run_owner_heartbeat",
            "0009_drop_fts_content_text",
            "0010_dedupe_findings_drop_run_scoped_ids",
        ]
    );
}

// ----------------------------------------------------------------------------
// Migration 0005 — SEI identity store (Wave 1 / WS1, ADR-038). The identity
// store lives in `sei_bindings` (NOT a column on the cumulative `entities`
// table); `entities` gains only a plain `signature TEXT`. These tests pin the
// shape the matcher + resolution surface depend on.
// ----------------------------------------------------------------------------

#[test]
fn migration_0005_creates_sei_tables_and_signature_column() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    let tables = table_names(&conn);
    for expected in &["sei_bindings", "sei_lineage"] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected} in {tables:?}"
        );
    }

    // entities gains a plain `signature` column; there is NO `sei` column
    // (identity lives in sei_bindings — entities is cumulative/never-pruned).
    let entity_cols = table_columns(&conn, "entities");
    assert!(
        entity_cols.iter().any(|c| c == "signature"),
        "entities.signature missing in {entity_cols:?}"
    );
    assert!(
        !entity_cols.iter().any(|c| c == "sei"),
        "entities must NOT have a `sei` column (ADR-038): {entity_cols:?}"
    );

    // sei_bindings is keyed by the opaque SEI.
    assert_eq!(primary_key_columns(&conn, "sei_bindings"), vec!["sei"]);
}

#[test]
fn migration_0005_partial_unique_index_allows_one_alive_binding_per_locator() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    let insert = |sei: &str, locator: &str, status: &str| {
        conn.execute(
            "INSERT INTO sei_bindings \
             (sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at) \
             VALUES (?1, ?2, 'h', NULL, ?3, 'r0', 'r0', 't0')",
            params![sei, locator, status],
        )
    };

    // One alive binding for a locator is fine.
    insert("loomweave:eid:aaaa", "python:function:m.f", "alive").expect("first alive");
    // A SECOND alive binding for the same locator violates the partial unique index.
    insert("loomweave:eid:bbbb", "python:function:m.f", "alive")
        .expect_err("second alive binding on the same locator must be rejected");
    // An orphaned binding may share the former locator (audit history retained).
    insert("loomweave:eid:cccc", "python:function:m.f", "orphaned")
        .expect("orphaned may share locator");
    // Two alive bindings with NULL locator do not collide (partial index excludes NULLs).
    insert("loomweave:eid:dddd", "", "alive").expect("setup distinct locator");
    conn.execute(
        "INSERT INTO sei_bindings \
         (sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at) \
         VALUES ('loomweave:eid:eeee', NULL, 'h', NULL, 'alive', 'r0', 'r0', 't0')",
        [],
    )
    .expect("null locator alive #1");
    conn.execute(
        "INSERT INTO sei_bindings \
         (sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at) \
         VALUES ('loomweave:eid:ffff', NULL, 'h', NULL, 'alive', 'r0', 'r0', 't0')",
        [],
    )
    .expect("null locator alive #2 must not collide");
}

#[test]
fn migration_0005_check_constraints_reject_bad_vocab() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    conn.execute(
        "INSERT INTO sei_bindings \
         (sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at) \
         VALUES ('loomweave:eid:aaaa', 'l', 'h', NULL, 'bogus', 'r0', 'r0', 't0')",
        [],
    )
    .expect_err("sei_bindings.status must reject out-of-vocabulary values");

    conn.execute(
        "INSERT INTO sei_lineage (sei, event, old_locator, new_locator, run_id, recorded_at) \
         VALUES ('loomweave:eid:aaaa', 'bogus_event', NULL, NULL, 'r0', 't0')",
        [],
    )
    .expect_err("sei_lineage.event must reject out-of-vocabulary values");
}

// ----------------------------------------------------------------------------
// ADR-031: schema-validation policy. Each CHECK-constrained enum-shaped TEXT
// column must reject out-of-vocabulary inserts at the SQL layer. These tests
// mirror `edges_confidence_column_rejects_unknown_tier` (line 538) and serve
// as the executable record of the closed-vocabulary policy.
// ----------------------------------------------------------------------------

fn insert_anchor_entity(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) \
         VALUES (?1, 'python', 'function', ?1, ?1, '{}', \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![id],
    )
    .unwrap();
}

fn insert_run(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO runs (id, started_at, config, stats, status) \
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), '{}', '{}', 'running')",
        params![id],
    )
    .unwrap();
}

fn insert_finding(
    conn: &Connection,
    entity_id: &str,
    kind: &str,
    severity: &str,
    status: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO findings (id, tool, tool_version, run_id, rule_id, kind, severity, \
         entity_id, related_entities, message, evidence, properties, supports, \
         supported_by, status, created_at, updated_at) \
         VALUES ('f1', 'loomweave', '0.1', 'r1', 'LMWV-FACT-TODO', ?1, ?2, ?3, '[]', \
         'm', '{}', '{}', '[]', '[]', ?4, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![kind, severity, entity_id, status],
    )
}

#[test]
fn findings_run_id_rejects_missing_run() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_anchor_entity(&conn, "python:function:demo.a");
    let err = insert_finding(&conn, "python:function:demo.a", "fact", "INFO", "open")
        .expect_err("findings.run_id should reference an existing run");
    assert!(
        err.to_string().contains("FOREIGN KEY constraint failed"),
        "unexpected error for missing run_id: {err}"
    );
}

#[test]
fn findings_kind_check_rejects_unknown_value() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_run(&conn, "r1");
    insert_anchor_entity(&conn, "python:function:demo.a");
    let err = insert_finding(&conn, "python:function:demo.a", "bogus", "INFO", "open")
        .expect_err("findings.kind CHECK should reject unknown values");
    assert!(
        err.to_string().contains("CHECK constraint failed"),
        "unexpected error for invalid kind: {err}"
    );
}

#[test]
fn findings_kind_check_accepts_all_documented_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_run(&conn, "r1");
    insert_anchor_entity(&conn, "python:function:demo.a");
    // Insert each documented kind in turn; deletion between iterations keeps
    // the PRIMARY KEY available.
    for kind in ["defect", "fact", "classification", "metric", "suggestion"] {
        insert_finding(&conn, "python:function:demo.a", kind, "INFO", "open")
            .unwrap_or_else(|err| panic!("kind={kind} rejected unexpectedly: {err}"));
        conn.execute("DELETE FROM findings", []).unwrap();
    }
}

#[test]
fn findings_severity_check_rejects_unknown_value() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_run(&conn, "r1");
    insert_anchor_entity(&conn, "python:function:demo.a");
    let err = insert_finding(&conn, "python:function:demo.a", "fact", "info", "open")
        .expect_err("findings.severity CHECK should reject lowercase 'info'");
    assert!(
        err.to_string().contains("CHECK constraint failed"),
        "unexpected error for invalid severity: {err}"
    );
}

#[test]
fn findings_severity_check_accepts_all_documented_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_run(&conn, "r1");
    insert_anchor_entity(&conn, "python:function:demo.a");
    for severity in ["INFO", "WARN", "ERROR", "CRITICAL", "NONE"] {
        insert_finding(&conn, "python:function:demo.a", "fact", severity, "open")
            .unwrap_or_else(|err| panic!("severity={severity} rejected unexpectedly: {err}"));
        conn.execute("DELETE FROM findings", []).unwrap();
    }
}

#[test]
fn findings_status_check_rejects_unknown_value() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_run(&conn, "r1");
    insert_anchor_entity(&conn, "python:function:demo.a");
    let err = insert_finding(&conn, "python:function:demo.a", "fact", "INFO", "closed")
        .expect_err("findings.status CHECK should reject 'closed'");
    assert!(
        err.to_string().contains("CHECK constraint failed"),
        "unexpected error for invalid status: {err}"
    );
}

#[test]
fn findings_status_check_accepts_all_documented_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_run(&conn, "r1");
    insert_anchor_entity(&conn, "python:function:demo.a");
    for status in ["open", "acknowledged", "suppressed", "promoted_to_issue"] {
        insert_finding(&conn, "python:function:demo.a", "fact", "INFO", status)
            .unwrap_or_else(|err| panic!("status={status} rejected unexpectedly: {err}"));
        conn.execute("DELETE FROM findings", []).unwrap();
    }
}

#[test]
fn migration_0010_preserves_operator_lifecycle_findings() {
    // The dedupe migration drops regenerable open+unlinked findings, but must
    // keep operator-owned lifecycle rows — Filigree-linked or triaged out of
    // `open` (Codex P1: a blanket DELETE reopened already-triaged issues). This
    // re-runs the 0010 body over seeded rows and asserts the surviving set.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    insert_run(&conn, "r1");
    insert_anchor_entity(&conn, "python:function:demo.a");

    let put = |id: &str, status: &str, issue: Option<&str>| {
        conn.execute(
            "INSERT INTO findings (id, tool, tool_version, run_id, rule_id, kind, severity, \
             entity_id, related_entities, message, evidence, properties, supports, \
             supported_by, status, filigree_issue_id, created_at, updated_at) \
             VALUES (?1, 'loomweave', '0.1', 'r1', 'LMWV-FACT-TODO', 'fact', 'INFO', \
             'python:function:demo.a', '[]', 'm', '{}', '{}', '[]', '[]', ?2, ?3, \
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            params![id, status, issue],
        )
        .unwrap();
    };
    put("core:finding:r1:open-unlinked", "open", None);
    put("core:finding:r1:linked", "open", Some("clarion-123"));
    put("core:finding:r1:acked", "acknowledged", None);

    conn.execute_batch(include_str!(
        "../migrations/0010_dedupe_findings_drop_run_scoped_ids.sql"
    ))
    .unwrap();

    let surviving: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM findings ORDER BY id").unwrap();
        let rows = stmt.query_map([], |row| row.get(0)).unwrap();
        rows.map(std::result::Result::unwrap).collect()
    };
    assert_eq!(
        surviving,
        vec![
            "core:finding:r1:acked".to_string(),
            "core:finding:r1:linked".to_string(),
        ],
        "0010 must drop only open+unlinked findings, preserving linked/triaged rows"
    );
}

#[test]
fn runs_status_check_rejects_unknown_value() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let err = conn
        .execute(
            "INSERT INTO runs (id, started_at, config, stats, status) \
             VALUES ('r1', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), '{}', '{}', 'runing')",
            [],
        )
        .expect_err("runs.status CHECK should reject 'runing' typo");
    assert!(
        err.to_string().contains("CHECK constraint failed"),
        "unexpected error for invalid runs.status: {err}"
    );
}

#[test]
fn runs_status_check_accepts_all_documented_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    for (i, status) in ["running", "skipped_no_plugins", "completed", "failed"]
        .iter()
        .enumerate()
    {
        let id = format!("r{i}");
        conn.execute(
            "INSERT INTO runs (id, started_at, config, stats, status) \
             VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), '{}', '{}', ?2)",
            params![id, status],
        )
        .unwrap_or_else(|err| panic!("runs.status={status} rejected unexpectedly: {err}"));
    }
}

// ----------------------------------------------------------------------------
// STO-02 (gap-register.md): the writer must self-identify Loomweave databases
// via SQLite's `application_id` header and refuse forward-incompatible
// `user_version` values. These tests pin the open-time contract.
// ----------------------------------------------------------------------------

/// Spawn a writer, immediately shut it down, and return whatever Result the
/// blocking task produced. Used to assert refuse-on-open behaviour without
/// leaking the spawned task.
fn spawn_writer_and_drain(path: std::path::PathBuf) -> Result<(), StorageError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let (writer, handle) = Writer::spawn(path, 50, 256)?;
        // Close the command channel so the actor (if it reached the loop)
        // exits cleanly; the join then surfaces the open-time error if any.
        drop(writer);
        handle.await.expect("writer task did not panic")
    })
}

#[test]
fn open_refuses_db_with_foreign_application_id() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("foreign.db");
    {
        let conn = Connection::open(&path).unwrap();
        // `PRAGMA application_id` only writes the header page to disk once
        // the database file has been materialised by some other write. Touch
        // a temporary table to force the file out of zero-bytes state.
        conn.execute_batch(
            "PRAGMA application_id = 0x7AFEBABE; \
             CREATE TABLE _touch (x INTEGER); DROP TABLE _touch;",
        )
        .expect("set foreign application_id");
    }
    let err = spawn_writer_and_drain(path).expect_err(
        "Writer::spawn must refuse a SQLite file carrying a non-Loomweave application_id",
    );
    assert!(
        matches!(
            err,
            StorageError::ForeignDatabase {
                application_id: 0x7AFE_BABE,
            }
        ),
        "expected ForeignDatabase {{ application_id: 0x7AFEBABE }}, got {err:?}"
    );
}

#[test]
fn open_refuses_db_from_future_user_version() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("future.db");
    // Open the file via the normal writer path first so it carries the
    // Loomweave application_id and the v1 schema (the migration runner sets
    // user_version=1 on apply). Then bump user_version past current and
    // re-open via the writer — must refuse.
    {
        let mut conn = Connection::open(&path).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
    }
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA user_version = {};",
            schema::CURRENT_SCHEMA_VERSION + 1
        ))
        .expect("bump user_version");
    }

    let err = spawn_writer_and_drain(path)
        .expect_err("Writer::spawn must refuse a future-versioned database");
    let expected_found = schema::CURRENT_SCHEMA_VERSION + 1;
    let expected_current = schema::CURRENT_SCHEMA_VERSION;
    assert!(
        matches!(
            err,
            StorageError::FutureUserVersion { found, current }
                if found == expected_found && current == expected_current
        ),
        "expected FutureUserVersion {{ found: {expected_found}, current: \
         {expected_current} }}, got {err:?}"
    );
}

#[test]
fn writer_spawn_refuses_future_user_version_before_returning_sender() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("future-sync.db");
    {
        let mut conn = Connection::open(&path).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
    }
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA user_version = {};",
            schema::CURRENT_SCHEMA_VERSION + 1
        ))
        .expect("bump user_version");
    }

    let expected_found = schema::CURRENT_SCHEMA_VERSION + 1;
    let expected_current = schema::CURRENT_SCHEMA_VERSION;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        match Writer::spawn(path, 50, 256) {
            Err(StorageError::FutureUserVersion { found, current }) => {
                assert_eq!(found, expected_found);
                assert_eq!(current, expected_current);
            }
            Err(err) => panic!("expected FutureUserVersion, got {err:?}"),
            Ok((writer, handle)) => {
                drop(writer);
                handle.abort();
                panic!("Writer::spawn returned a sender for a future-versioned database");
            }
        }
    });
}

#[test]
fn open_sets_application_id_on_legacy_db() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("legacy.db");
    // Touch a SQLite file with no application_id set (default 0). Open as a
    // raw connection so we leave the header at its zero default — no
    // `apply_write_pragmas` here.
    {
        let conn = Connection::open(&path).unwrap();
        let raw: i64 = conn
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        assert_eq!(raw, 0, "fresh SQLite files start at application_id=0");
        // Force the file to materialise on disk by writing a table; this also
        // guarantees the header page exists.
        conn.execute_batch("CREATE TABLE _touch (x INTEGER); DROP TABLE _touch;")
            .unwrap();
    }

    // First open via the writer should set the Loomweave application_id.
    spawn_writer_and_drain(path.clone())
        .expect("Writer::spawn must accept a legacy (application_id=0) file");
    {
        let conn = Connection::open(&path).unwrap();
        let raw: i64 = conn
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let observed = raw as u32;
        assert_eq!(
            observed,
            pragma::LOOMWEAVE_APPLICATION_ID,
            "writer must stamp Loomweave application_id on a legacy DB"
        );
    }

    // Re-open: must not refuse, application_id is now recognised.
    spawn_writer_and_drain(path)
        .expect("Writer::spawn must accept a database it has already stamped");
}
