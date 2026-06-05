//! Guidance-sheet write-API integration tests (WS6 / REQ-GUIDANCE-01,
//! REQ-GUIDANCE-03).
//!
//! Exercises `upsert_guidance_sheet` / `get_guidance_sheet` /
//! `list_guidance_sheets` / `delete_guidance_sheet` against a fresh schema,
//! plus the `--for-entity` matcher. The headline assertion is the explicit TDD
//! target: sheets written with various `scope_level`s come back ordered by the
//! generated `scope_rank` column (project < subsystem < … < function).

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use loomweave_storage::{
    GuidanceSheetInput, delete_guidance_sheet, get_guidance_sheet, guidance_sheet_matches_entity,
    insert_guidance_sheet, list_guidance_sheets, pragma, schema, upsert_guidance_sheet,
};

fn open_fresh(tempdir: &tempfile::TempDir) -> Connection {
    let path = tempdir.path().join("loomweave.db");
    let mut conn = Connection::open(&path).expect("open");
    pragma::apply_write_pragmas(&conn).expect("pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    conn
}

fn write_sheet(conn: &Connection, slug: &str, props: &Value) {
    let id = format!("core:guidance:{slug}");
    let short = slug.rsplit('.').next().unwrap_or(slug);
    upsert_guidance_sheet(
        conn,
        &GuidanceSheetInput {
            id: &id,
            name: slug,
            short_name: short,
            properties: props,
        },
    )
    .expect("upsert guidance sheet");
}

fn base_props(scope_level: &str, authored_at: &str) -> Value {
    json!({
        "content": format!("guidance for {scope_level}"),
        "scope_level": scope_level,
        "match_rules": [],
        "pinned": false,
        "provenance": "manual",
        "authored_at": authored_at,
    })
}

#[test]
fn upsert_then_get_roundtrips_properties_and_kind() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    write_sheet(
        &conn,
        "demo.module-sheet",
        &base_props("module", "2026-06-01T00:00:00.000Z"),
    );

    let kind: String = conn
        .query_row(
            "SELECT kind FROM entities WHERE id = ?1",
            params!["core:guidance:demo.module-sheet"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kind, "guidance");

    let sheet = get_guidance_sheet(&conn, "core:guidance:demo.module-sheet")
        .unwrap()
        .expect("sheet present");
    assert_eq!(sheet.scope_level.as_deref(), Some("module"));
    assert_eq!(sheet.scope_rank, Some(4)); // module → 4
    assert_eq!(
        sheet.properties.get("content").and_then(Value::as_str),
        Some("guidance for module")
    );
    assert_eq!(
        sheet.properties.get("provenance").and_then(Value::as_str),
        Some("manual")
    );
}

#[test]
fn insert_guidance_sheet_rejects_existing_id_without_overwrite() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let first_props = base_props("module", "2026-06-01T00:00:00.000Z");
    let second_props = json!({
        "content": "second writer must not win",
        "scope_level": "function",
        "match_rules": [],
        "pinned": true,
        "provenance": "manual",
        "authored_at": "2026-06-02T00:00:00.000Z",
    });

    insert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: "core:guidance:race.sheet",
            name: "race.sheet",
            short_name: "sheet",
            properties: &first_props,
        },
    )
    .expect("first insert succeeds");
    let err = insert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: "core:guidance:race.sheet",
            name: "race.sheet",
            short_name: "sheet",
            properties: &second_props,
        },
    )
    .expect_err("second create must fail instead of overwriting");

    assert!(
        err.to_string().contains("already exists"),
        "duplicate create error should name existing sheet; got {err}"
    );
    let sheet = get_guidance_sheet(&conn, "core:guidance:race.sheet")
        .unwrap()
        .expect("sheet present");
    assert_eq!(
        sheet.properties.get("content").and_then(Value::as_str),
        Some("guidance for module")
    );
    assert_eq!(sheet.scope_level.as_deref(), Some("module"));
    assert_eq!(
        sheet.properties.get("pinned").and_then(Value::as_bool),
        Some(false)
    );
}

#[test]
fn get_returns_none_for_absent_or_non_guidance() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    assert!(
        get_guidance_sheet(&conn, "core:guidance:nope")
            .unwrap()
            .is_none()
    );

    // A non-guidance entity with the same id must not be returned as a sheet.
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) VALUES \
         (?1, 'python', 'function', 'x', 'x', '{}', \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params!["python:function:x"],
    )
    .unwrap();
    assert!(
        get_guidance_sheet(&conn, "python:function:x")
            .unwrap()
            .is_none()
    );
}

#[test]
fn list_orders_by_scope_rank_then_authored_at_then_id() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    // Insert deliberately out of scope-rank order.
    write_sheet(
        &conn,
        "s.function",
        &base_props("function", "2026-01-01T00:00:00.000Z"),
    );
    write_sheet(
        &conn,
        "s.project",
        &base_props("project", "2026-01-01T00:00:00.000Z"),
    );
    write_sheet(
        &conn,
        "s.class",
        &base_props("class", "2026-01-01T00:00:00.000Z"),
    );
    write_sheet(
        &conn,
        "s.subsystem",
        &base_props("subsystem", "2026-01-01T00:00:00.000Z"),
    );
    write_sheet(
        &conn,
        "s.package",
        &base_props("package", "2026-01-01T00:00:00.000Z"),
    );
    write_sheet(
        &conn,
        "s.module",
        &base_props("module", "2026-01-01T00:00:00.000Z"),
    );

    let listed = list_guidance_sheets(&conn).unwrap();
    let ranks: Vec<i64> = listed.iter().map(|s| s.scope_rank.unwrap()).collect();
    assert_eq!(ranks, vec![1, 2, 3, 4, 5, 6], "ordered project→function");
    let levels: Vec<&str> = listed
        .iter()
        .map(|s| s.scope_level.as_deref().unwrap())
        .collect();
    assert_eq!(
        levels,
        vec![
            "project",
            "subsystem",
            "package",
            "module",
            "class",
            "function"
        ]
    );
}

#[test]
fn list_ties_break_by_authored_at_then_id() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    // Same scope_level (same rank): order by authored_at ASC, then id ASC.
    write_sheet(
        &conn,
        "a.later",
        &base_props("module", "2026-06-02T00:00:00.000Z"),
    );
    write_sheet(
        &conn,
        "a.earlier",
        &base_props("module", "2026-06-01T00:00:00.000Z"),
    );
    // Same authored_at as a.earlier — tie-broken by id (z.same > a.earlier).
    write_sheet(
        &conn,
        "z.same",
        &base_props("module", "2026-06-01T00:00:00.000Z"),
    );

    let ids: Vec<String> = list_guidance_sheets(&conn)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(
        ids,
        vec![
            "core:guidance:a.earlier".to_owned(),
            "core:guidance:z.same".to_owned(),
            "core:guidance:a.later".to_owned(),
        ]
    );
}

#[test]
fn upsert_updates_in_place_preserving_created_at() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    write_sheet(
        &conn,
        "demo.sheet",
        &base_props("module", "2026-06-01T00:00:00.000Z"),
    );
    let before = get_guidance_sheet(&conn, "core:guidance:demo.sheet")
        .unwrap()
        .unwrap();

    // Re-upsert with changed scope_level + content.
    write_sheet(
        &conn,
        "demo.sheet",
        &base_props("class", "2026-06-01T00:00:00.000Z"),
    );
    let after = get_guidance_sheet(&conn, "core:guidance:demo.sheet")
        .unwrap()
        .unwrap();

    assert_eq!(after.created_at, before.created_at, "created_at preserved");
    assert_eq!(after.scope_rank, Some(5), "class → 5");

    // Exactly one row — upsert, not duplicate insert.
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM entities WHERE kind = 'guidance'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn delete_removes_only_guidance_sheet() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    write_sheet(
        &conn,
        "demo.sheet",
        &base_props("module", "2026-06-01T00:00:00.000Z"),
    );

    assert!(delete_guidance_sheet(&conn, "core:guidance:demo.sheet").unwrap());
    assert!(
        get_guidance_sheet(&conn, "core:guidance:demo.sheet")
            .unwrap()
            .is_none()
    );
    // Second delete is a no-op (returns false).
    assert!(!delete_guidance_sheet(&conn, "core:guidance:demo.sheet").unwrap());
}

#[test]
fn matcher_evaluates_kind_tag_and_entity_rules() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let project_root = tempdir.path();

    // A code entity with a tag.
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         source_file_path, created_at, updated_at) VALUES \
         (?1, 'python', 'function', 'pkg.mod.f', 'f', '{}', ?2, \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![
            "python:function:pkg.mod.f",
            project_root.join("src/pkg/mod.py").to_str().unwrap()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entity_tags (entity_id, plugin_id, tag) VALUES (?1, 'python', 'auth')",
        params!["python:function:pkg.mod.f"],
    )
    .unwrap();

    let kind_sheet = sheet_with_rules(&conn, "k", &json!([{"type":"kind","value":"function"}]));
    let tag_sheet = sheet_with_rules(&conn, "t", &json!([{"type":"tag","value":"auth"}]));
    let entity_sheet = sheet_with_rules(
        &conn,
        "e",
        &json!([{"type":"entity","id":"python:function:pkg.mod.f"}]),
    );
    let path_sheet = sheet_with_rules(&conn, "p", &json!([{"type":"path","pattern":"src/**"}]));
    let nomatch = sheet_with_rules(&conn, "n", &json!([{"type":"kind","value":"class"}]));
    let wardline = sheet_with_rules(&conn, "w", &json!([{"type":"wardline_group","group":"x"}]));

    let m = |s: &loomweave_storage::GuidanceSheet| {
        guidance_sheet_matches_entity(&conn, s, "python:function:pkg.mod.f", project_root).unwrap()
    };
    assert!(m(&kind_sheet));
    assert!(m(&tag_sheet));
    assert!(m(&entity_sheet));
    assert!(m(&path_sheet));
    assert!(!m(&nomatch));
    assert!(!m(&wardline), "wardline_group not evaluable here");
}

fn sheet_with_rules(
    conn: &Connection,
    slug: &str,
    rules: &Value,
) -> loomweave_storage::GuidanceSheet {
    let props = json!({
        "content": "x",
        "scope_level": "module",
        "match_rules": rules,
        "provenance": "manual",
        "authored_at": "2026-06-01T00:00:00.000Z",
    });
    write_sheet(conn, slug, &props);
    get_guidance_sheet(conn, &format!("core:guidance:{slug}"))
        .unwrap()
        .unwrap()
}

fn seed_cache_row(conn: &Connection, entity_id: &str) {
    conn.execute(
        "INSERT INTO summary_cache \
         (entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint, \
          summary_json, cost_usd, tokens_input, tokens_output, created_at, last_accessed_at, \
          caller_count, fan_out) \
         VALUES (?1, 'h', 'tmpl', 'tier', 'fp', '{}', 0.0, 0, 0, \
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, 0)",
        params![entity_id],
    )
    .unwrap();
}

fn cache_row_count(conn: &Connection, entity_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM summary_cache WHERE entity_id = ?1",
        params![entity_id],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn invalidate_summaries_drops_matched_and_keeps_unmatched() {
    use loomweave_storage::invalidate_summaries_for_sheet;

    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let project_root = tempdir.path();

    // A `function` entity (the sheet's `kind:function` rule will match) and a
    // `class` entity (it will not). Both have a cached summary.
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) VALUES \
         (?1, 'python', 'function', 'pkg.mod.f', 'f', '{}', \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params!["python:function:pkg.mod.f"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) VALUES \
         (?1, 'python', 'class', 'pkg.mod.C', 'C', '{}', \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params!["python:class:pkg.mod.C"],
    )
    .unwrap();
    seed_cache_row(&conn, "python:function:pkg.mod.f");
    seed_cache_row(&conn, "python:class:pkg.mod.C");

    let sheet = sheet_with_rules(&conn, "k", &json!([{"type":"kind","value":"function"}]));
    let removed = invalidate_summaries_for_sheet(&conn, &sheet, project_root).unwrap();

    assert_eq!(removed, 1, "exactly one matched entity's cache invalidated");
    assert_eq!(
        cache_row_count(&conn, "python:function:pkg.mod.f"),
        0,
        "matched entity's cache row gone"
    );
    assert_eq!(
        cache_row_count(&conn, "python:class:pkg.mod.C"),
        1,
        "non-matching entity's cache row survives"
    );
}

#[test]
fn upsert_rejects_non_guidance_id_and_leaves_code_entity_intact() {
    // FINDING 1: a sheet id that is NOT `core:guidance:` (e.g. a hand-edited /
    // malicious import naming a code entity) must be rejected by
    // `upsert_guidance_sheet`, and must NOT overwrite the existing code entity.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    // A pre-existing code entity with distinctive name/properties.
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) VALUES \
         (?1, 'python', 'function', 'pkg.mod.foo', 'foo', '{\"k\":\"v\"}', \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params!["python:function:foo"],
    )
    .unwrap();

    let before: (String, String, String, String) = conn
        .query_row(
            "SELECT name, kind, plugin_id, properties FROM entities WHERE id = ?1",
            params!["python:function:foo"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();

    // Attempt to upsert a "guidance" sheet whose id collides with the code entity.
    let props = base_props("module", "2026-06-01T00:00:00.000Z");
    let err = upsert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: "python:function:foo",
            name: "evil",
            short_name: "evil",
            properties: &props,
        },
    );
    assert!(err.is_err(), "non-guidance id must be rejected");

    let after: (String, String, String, String) = conn
        .query_row(
            "SELECT name, kind, plugin_id, properties FROM entities WHERE id = ?1",
            params!["python:function:foo"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        after, before,
        "code entity must be byte-identical after a rejected upsert"
    );
}

#[test]
fn upsert_accepts_valid_guidance_id() {
    // FINDING 1: the canonical `core:guidance:` id still upserts fine.
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    write_sheet(
        &conn,
        "valid.sheet",
        &base_props("module", "2026-06-01T00:00:00.000Z"),
    );
    assert!(
        get_guidance_sheet(&conn, "core:guidance:valid.sheet")
            .unwrap()
            .is_some()
    );
}

#[test]
fn invalidate_summaries_for_no_rule_sheet_is_noop() {
    use loomweave_storage::invalidate_summaries_for_sheet;

    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) VALUES \
         (?1, 'python', 'function', 'pkg.mod.f', 'f', '{}', \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params!["python:function:pkg.mod.f"],
    )
    .unwrap();
    seed_cache_row(&conn, "python:function:pkg.mod.f");

    let sheet = sheet_with_rules(&conn, "empty", &json!([]));
    let removed = invalidate_summaries_for_sheet(&conn, &sheet, tempdir.path()).unwrap();
    assert_eq!(removed, 0, "a no-rule sheet invalidates nothing");
    assert_eq!(cache_row_count(&conn, "python:function:pkg.mod.f"), 1);
}

#[test]
fn invalidate_summaries_includes_guides_edge_targets() {
    // FINDING 3: a sheet that applies SOLELY via a `guides` edge (NO match_rules)
    // must still invalidate the guided entity's cached summary. The `guidance_for`
    // read path composes match_rules OR guides edges, so invalidation must too.
    use loomweave_storage::invalidate_summaries_for_sheet;

    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    // A code entity that will be the `guides`-edge target (it must exist first,
    // for the edge's FK).
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) VALUES \
         (?1, 'python', 'function', 'pkg.mod.g', 'g', '{}', \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params!["python:function:pkg.mod.g"],
    )
    .unwrap();
    seed_cache_row(&conn, "python:function:pkg.mod.g");

    // A sheet with NO match_rules — so any invalidation can ONLY come from the
    // guides edge, not a rule.
    let sheet = sheet_with_rules(&conn, "guides-only", &json!([]));
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence) VALUES \
         ('guides', ?1, ?2, 'resolved')",
        params!["core:guidance:guides-only", "python:function:pkg.mod.g"],
    )
    .unwrap();

    let removed = invalidate_summaries_for_sheet(&conn, &sheet, tempdir.path()).unwrap();
    assert_eq!(
        removed, 1,
        "the guides-edge target's cache row is invalidated"
    );
    assert_eq!(
        cache_row_count(&conn, "python:function:pkg.mod.g"),
        0,
        "guided entity's summary row must be gone"
    );
}

#[test]
fn invalidate_summaries_dedups_rule_and_guides_match() {
    // FINDING 3: an entity matched by BOTH a match_rule AND a guides edge is
    // invalidated exactly once (count is 1, not 2).
    use loomweave_storage::invalidate_summaries_for_sheet;

    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);

    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) VALUES \
         (?1, 'python', 'function', 'pkg.mod.h', 'h', '{}', \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params!["python:function:pkg.mod.h"],
    )
    .unwrap();
    seed_cache_row(&conn, "python:function:pkg.mod.h");

    // A sheet whose `kind:function` rule matches the entity AND a guides edge to
    // the same entity → it must count once.
    let sheet = sheet_with_rules(&conn, "both", &json!([{"type":"kind","value":"function"}]));
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence) VALUES \
         ('guides', ?1, ?2, 'resolved')",
        params!["core:guidance:both", "python:function:pkg.mod.h"],
    )
    .unwrap();

    let removed = invalidate_summaries_for_sheet(&conn, &sheet, tempdir.path()).unwrap();
    assert_eq!(
        removed, 1,
        "matched by both rule and guides edge → invalidated once"
    );
}
