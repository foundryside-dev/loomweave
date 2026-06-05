//! LLM cache helper integration tests for the B.6 MCP surface.

use rusqlite::Connection;

use loomweave_storage::{
    InferredEdgeCacheEntry, InferredEdgeCacheKey, SummaryCacheEntry, SummaryCacheKey,
    inferred_edge_cache_lookup, pragma, schema, summary_cache_lookup, touch_inferred_edge_cache,
    touch_summary_cache, upsert_inferred_edge_cache, upsert_summary_cache,
};

fn open_fresh(tempdir: &tempfile::TempDir) -> Connection {
    let path = tempdir.path().join("loomweave.db");
    let mut conn = Connection::open(&path).expect("open");
    pragma::apply_write_pragmas(&conn).expect("pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    conn
}

fn seed_entity(conn: &Connection, entity_id: &str, content_hash: &str) {
    conn.execute(
        "INSERT INTO entities ( \
            id, plugin_id, kind, name, short_name, properties, content_hash, \
            created_at, updated_at \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            entity_id,
            "python",
            "function",
            entity_id,
            entity_id.rsplit('.').next().unwrap_or(entity_id),
            "{}",
            content_hash,
            "2026-05-17T00:00:00.000Z",
            "2026-05-17T00:00:00.000Z",
        ],
    )
    .expect("seed entity");
}

#[test]
fn summary_cache_upsert_lookup_and_touch_round_trip() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    seed_entity(&conn, "python:function:demo.hello", "hash-a");

    let key = SummaryCacheKey {
        entity_id: "python:function:demo.hello".to_owned(),
        content_hash: "hash-a".to_owned(),
        prompt_template_id: "leaf-v1".to_owned(),
        model_tier: "claude-haiku-4-5".to_owned(),
        guidance_fingerprint: "guidance-empty".to_owned(),
    };
    let entry = SummaryCacheEntry {
        key: key.clone(),
        summary_json: r#"{"purpose":"demo"}"#.to_owned(),
        cost_usd: 0.001,
        tokens_input: 100,
        tokens_output: 20,
        caller_count: 2,
        fan_out: 1,
        stale_semantic: false,
        created_at: "2026-05-17T00:00:00.000Z".to_owned(),
        last_accessed_at: "2026-05-17T00:00:01.000Z".to_owned(),
    };

    upsert_summary_cache(&conn, &entry).expect("insert summary cache row");
    let found = summary_cache_lookup(&conn, &key)
        .expect("lookup summary cache")
        .expect("summary cache row exists");
    assert_eq!(found, entry);

    assert!(touch_summary_cache(&conn, &key, "2026-05-17T00:00:02.000Z").unwrap());
    let touched = summary_cache_lookup(&conn, &key)
        .unwrap()
        .expect("summary cache row still exists");
    assert_eq!(touched.last_accessed_at, "2026-05-17T00:00:02.000Z");

    let updated = SummaryCacheEntry {
        summary_json: r#"{"purpose":"updated"}"#.to_owned(),
        cost_usd: 0.002,
        caller_count: 3,
        fan_out: 4,
        stale_semantic: true,
        last_accessed_at: "2026-05-17T00:00:03.000Z".to_owned(),
        ..touched
    };
    upsert_summary_cache(&conn, &updated).expect("upsert summary cache row");

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM summary_cache", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "same 5-tuple key should replace in place");
    let found = summary_cache_lookup(&conn, &key).unwrap().unwrap();
    assert_eq!(found, updated);
}

#[test]
fn inferred_edge_cache_upsert_lookup_and_touch_round_trip() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    seed_entity(&conn, "python:function:demo.caller", "hash-caller");

    let key = InferredEdgeCacheKey {
        caller_entity_id: "python:function:demo.caller".to_owned(),
        caller_content_hash: "hash-caller".to_owned(),
        model_id: "claude-haiku-4-5".to_owned(),
        prompt_version: "inferred-calls-v1".to_owned(),
    };
    let entry = InferredEdgeCacheEntry {
        key: key.clone(),
        result_json: r#"{"edges":[]}"#.to_owned(),
        cost_usd: 0.002,
        token_count: 80,
        created_at: "2026-05-17T00:00:00.000Z".to_owned(),
        last_accessed_at: "2026-05-17T00:00:01.000Z".to_owned(),
    };

    upsert_inferred_edge_cache(&conn, &entry).expect("insert inferred-edge cache row");
    let found = inferred_edge_cache_lookup(&conn, &key)
        .expect("lookup inferred-edge cache")
        .expect("inferred-edge cache row exists");
    assert_eq!(found, entry);

    assert!(touch_inferred_edge_cache(&conn, &key, "2026-05-17T00:00:02.000Z").unwrap());
    let touched = inferred_edge_cache_lookup(&conn, &key).unwrap().unwrap();
    assert_eq!(touched.last_accessed_at, "2026-05-17T00:00:02.000Z");

    let updated = InferredEdgeCacheEntry {
        result_json: r#"{"edges":[{"to_id":"python:function:demo.callee"}]}"#.to_owned(),
        cost_usd: 0.003,
        token_count: 120,
        last_accessed_at: "2026-05-17T00:00:03.000Z".to_owned(),
        ..touched
    };
    upsert_inferred_edge_cache(&conn, &updated).expect("upsert inferred-edge cache row");

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM inferred_edge_cache", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1, "same 4-tuple key should replace in place");
    let found = inferred_edge_cache_lookup(&conn, &key).unwrap().unwrap();
    assert_eq!(found, updated);
}
