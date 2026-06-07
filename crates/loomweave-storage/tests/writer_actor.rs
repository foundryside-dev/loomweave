//! Writer-actor integration tests.
//!
//! Covers: round-trip insert, per-N-batch commit cadence, `FailRun` rollback.

use std::sync::atomic::Ordering;

use rusqlite::Connection;
use tokio::sync::oneshot;

use loomweave_storage::{
    InferredCallEdgeRecord, InferredEdgeCacheEntry, InferredEdgeCacheKey, ReaderPool,
    SummaryCacheEntry, SummaryCacheKey, UnresolvedCallSiteRecord, Writer,
    commands::{EdgeConfidence, EdgeRecord, EntityRecord, FindingRecord, RunStatus, WriterCmd},
    mark_stale_running_runs_failed, pragma, schema,
};

fn prepared_db(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("loomweave.db");
    let mut conn = Connection::open(&path).unwrap();
    pragma::apply_write_pragmas(&conn).unwrap();
    schema::apply_migrations(&mut conn).unwrap();
    path
}

/// Direct-SQL entity seed for tests that need an `entities` row but must
/// bypass the writer's `BeginRun → InsertEntity` protocol (e.g. tests
/// verifying that non-analyze writer commands work without an active run).
fn seed_entity_row(path: &std::path::Path, id: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO entities ( \
            id, plugin_id, kind, name, short_name, properties, \
            content_hash, created_at, updated_at \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            id,
            "python",
            "function",
            id,
            id.rsplit('.').next().unwrap_or(id),
            "{}",
            format!("hash-{id}"),
            now_iso(),
            now_iso(),
        ],
    )
    .expect("seed entity row");
}

fn now_iso() -> String {
    "2026-04-18T00:00:00.000Z".to_owned()
}

fn make_entity(id: &str) -> EntityRecord {
    EntityRecord {
        id: id.to_owned(),
        plugin_id: "python".to_owned(),
        kind: "function".to_owned(),
        name: "demo.hello".to_owned(),
        short_name: "hello".to_owned(),
        parent_id: None,
        source_file_id: None,
        source_file_path: None,
        source_byte_start: None,
        source_byte_end: None,
        source_line_start: None,
        source_line_end: None,
        properties_json: "{}".to_owned(),
        tags: Vec::new(),
        content_hash: None,
        summary_json: None,
        wardline_json: None,
        first_seen_commit: None,
        last_seen_commit: None,
        created_at: now_iso(),
        updated_at: now_iso(),
    }
}

fn make_entity_with_parent(id: &str, parent_id: Option<&str>) -> EntityRecord {
    let mut e = make_entity(id);
    e.parent_id = parent_id.map(str::to_owned);
    e
}

fn make_module_entity(id: &str) -> EntityRecord {
    let mut e = make_entity(id);
    "module".clone_into(&mut e.kind);
    e
}

fn make_file_entity(id: &str) -> EntityRecord {
    let mut e = make_entity(id);
    "core".clone_into(&mut e.plugin_id);
    "file".clone_into(&mut e.kind);
    "demo.py".clone_into(&mut e.name);
    "demo.py".clone_into(&mut e.short_name);
    e.content_hash = Some("hash-core:file:demo.py".to_owned());
    e
}

fn make_file_entity_named(id: &str, path: &str) -> EntityRecord {
    let mut e = make_file_entity(id);
    path.clone_into(&mut e.name);
    path.clone_into(&mut e.short_name);
    e.content_hash = Some(format!("hash-{id}"));
    e
}

fn make_contains_edge(from_id: &str, to_id: &str) -> EdgeRecord {
    EdgeRecord {
        kind: "contains".to_owned(),
        from_id: from_id.to_owned(),
        to_id: to_id.to_owned(),
        confidence: EdgeConfidence::Resolved,
        properties_json: None,
        source_file_id: None,
        source_byte_start: None,
        source_byte_end: None,
    }
}

fn make_structural_edge(
    kind: &str,
    from_id: &str,
    to_id: &str,
    confidence: EdgeConfidence,
) -> EdgeRecord {
    EdgeRecord {
        kind: kind.to_owned(),
        from_id: from_id.to_owned(),
        to_id: to_id.to_owned(),
        confidence,
        properties_json: None,
        source_file_id: None,
        source_byte_start: None,
        source_byte_end: None,
    }
}

fn make_calls_edge(from_id: &str, to_id: &str, confidence: EdgeConfidence) -> EdgeRecord {
    EdgeRecord {
        kind: "calls".to_owned(),
        from_id: from_id.to_owned(),
        to_id: to_id.to_owned(),
        confidence,
        properties_json: None,
        source_file_id: None,
        source_byte_start: Some(10),
        source_byte_end: Some(18),
    }
}

fn make_references_edge(from_id: &str, to_id: &str, confidence: EdgeConfidence) -> EdgeRecord {
    EdgeRecord {
        kind: "references".to_owned(),
        from_id: from_id.to_owned(),
        to_id: to_id.to_owned(),
        confidence,
        properties_json: None,
        source_file_id: None,
        source_byte_start: Some(20),
        source_byte_end: Some(25),
    }
}

async fn begin_demo_run(tx: &tokio::sync::mpsc::Sender<WriterCmd>, run_id: &str) {
    send::<()>(tx, |ack| WriterCmd::BeginRun {
        run_id: run_id.into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
}

async fn seed_module_and_functions(tx: &tokio::sync::mpsc::Sender<WriterCmd>) {
    send::<()>(tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    for id in ["python:function:demo.caller", "python:function:demo.callee"] {
        send::<()>(tx, |ack| WriterCmd::InsertEntity {
            entity: Box::new(make_entity_with_parent(id, Some("python:module:demo"))),
            ack,
        })
        .await
        .unwrap();
    }
}

async fn seed_contains_edges_for_demo_functions(tx: &tokio::sync::mpsc::Sender<WriterCmd>) {
    for id in ["python:function:demo.caller", "python:function:demo.callee"] {
        send::<()>(tx, |ack| WriterCmd::InsertEdge {
            edge: Box::new(make_contains_edge("python:module:demo", id)),
            ack,
        })
        .await
        .unwrap();
    }
}

fn summary_cache_entry() -> SummaryCacheEntry {
    SummaryCacheEntry {
        key: SummaryCacheKey {
            entity_id: "python:function:demo.hello".to_owned(),
            content_hash: "hash-python:function:demo.hello".to_owned(),
            prompt_template_id: "leaf-v1".to_owned(),
            model_tier: "claude-haiku-4-5".to_owned(),
            guidance_fingerprint: "guidance-empty".to_owned(),
        },
        summary_json: r#"{"purpose":"demo"}"#.to_owned(),
        cost_usd: 0.001,
        tokens_input: 100,
        tokens_output: 20,
        caller_count: 1,
        fan_out: 2,
        stale_semantic: false,
        created_at: now_iso(),
        last_accessed_at: now_iso(),
    }
}

fn unresolved_site(callee_expr: &str, ordinal: i64) -> UnresolvedCallSiteRecord {
    UnresolvedCallSiteRecord {
        caller_entity_id: "python:function:demo.caller".to_owned(),
        caller_content_hash: "hash-python:function:demo.caller".to_owned(),
        site_key: format!("site-{ordinal}"),
        site_ordinal: ordinal,
        source_file_id: None,
        source_byte_start: ordinal * 10,
        source_byte_end: ordinal * 10 + 4,
        callee_expr: callee_expr.to_owned(),
        created_at: now_iso(),
    }
}

fn inferred_cache_entry() -> InferredEdgeCacheEntry {
    InferredEdgeCacheEntry {
        key: InferredEdgeCacheKey {
            caller_entity_id: "python:function:demo.caller".to_owned(),
            caller_content_hash: "hash-python:function:demo.caller".to_owned(),
            model_id: "claude-haiku-4-5".to_owned(),
            prompt_version: "inferred-calls-v1".to_owned(),
        },
        result_json: r#"{"edges":[{"target_id":"python:function:demo.inferred"}]}"#.to_owned(),
        cost_usd: 0.002,
        token_count: 42,
        created_at: now_iso(),
        last_accessed_at: now_iso(),
    }
}

fn inferred_record(to_id: &str, start: i64) -> InferredCallEdgeRecord {
    InferredCallEdgeRecord {
        from_id: "python:function:demo.caller".to_owned(),
        to_id: to_id.to_owned(),
        source_file_id: None,
        source_byte_start: start,
        source_byte_end: start + 8,
        properties_json: r#"{"inference_cache_key":"cache-a"}"#.to_owned(),
    }
}

async fn assert_edge_rejected_with_counter(
    writer: &Writer,
    tx: &tokio::sync::mpsc::Sender<WriterCmd>,
    edge: EdgeRecord,
    expected_code: &str,
) {
    let result = send::<()>(tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(edge),
        ack,
    })
    .await;
    let err = result.expect_err("edge should be rejected by writer contract");
    let msg = format!("{err:?}");
    assert!(
        msg.contains(expected_code),
        "expected {expected_code} in error; got: {msg}"
    );
    assert_eq!(
        writer.dropped_edges_total.load(Ordering::Relaxed),
        1,
        "contract rejection should increment dropped_edges_total"
    );
}

async fn send<T>(
    tx: &tokio::sync::mpsc::Sender<WriterCmd>,
    build: impl FnOnce(oneshot::Sender<Result<T, loomweave_storage::StorageError>>) -> WriterCmd,
) -> Result<T, loomweave_storage::StorageError> {
    let (ack_tx, ack_rx) = oneshot::channel();
    tx.send(build(ack_tx)).await.unwrap();
    ack_rx.await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_cache_writer_commands_do_not_require_active_analyze_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    // summary_cache.entity_id has an FK to entities(id) (V11-STO-03). Seed the
    // referenced entity directly so the FK is satisfied without going through
    // the writer's BeginRun → InsertEntity protocol — the whole point of this
    // test is that summary_cache writer commands work *without* an active
    // analyze run.
    seed_entity_row(&path, "python:function:demo.hello");
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::UpsertSummaryCache {
        entry: Box::new(summary_cache_entry()),
        ack,
    })
    .await
    .unwrap();

    send::<bool>(&tx, |ack| WriterCmd::TouchSummaryCache {
        key: summary_cache_entry().key,
        last_accessed_at: "2026-04-18T00:00:01.000Z".to_owned(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let conn = Connection::open(path).unwrap();
    let (summary_json, last_accessed_at): (String, String) = conn
        .query_row(
            "SELECT summary_json, last_accessed_at FROM summary_cache \
             WHERE entity_id = ?1",
            rusqlite::params!["python:function:demo.hello"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(summary_json, r#"{"purpose":"demo"}"#);
    assert_eq!(last_accessed_at, "2026-04-18T00:00:01.000Z");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upsert_wardline_taint_fact_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    // wardline_taint_facts.entity_id has an FK to entities(id) (ADR-036). Seed
    // the referenced entity directly so the FK is satisfied — this is a
    // query-time write that must work without an active analyze run.
    seed_entity_row(&path, "python:function:a.b.c");
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::UpsertWardlineTaintFact {
        fact: Box::new(loomweave_storage::TaintFact {
            entity_id: "python:function:a.b.c".to_owned(),
            wardline_json: r#"{"v":1}"#.to_owned(),
            scan_id: Some("scan-1".to_owned()),
            content_hash_at_compute: Some("hash".to_owned()),
            updated_at: now_iso(),
            sei: None,
        }),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let conn = Connection::open(path).unwrap();
    let json: String = conn
        .query_row(
            "SELECT wardline_json FROM wardline_taint_facts WHERE entity_id = ?1",
            rusqlite::params!["python:function:a.b.c"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(json, r#"{"v":1}"#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_unresolved_call_sites_replaces_current_and_old_hash_rows_for_caller() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-unresolved").await;
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;

    send::<()>(&tx, |ack| WriterCmd::ReplaceUnresolvedCallSitesForCaller {
        caller_entity_id: "python:function:demo.caller".to_owned(),
        caller_content_hash: "old-hash".to_owned(),
        sites: vec![UnresolvedCallSiteRecord {
            caller_content_hash: "old-hash".to_owned(),
            site_key: "old-site".to_owned(),
            ..unresolved_site("old_target", 1)
        }],
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::ReplaceUnresolvedCallSitesForCaller {
        caller_entity_id: "python:function:demo.caller".to_owned(),
        caller_content_hash: "hash-python:function:demo.caller".to_owned(),
        sites: vec![
            unresolved_site("dynamic_target", 1),
            unresolved_site("fallback", 2),
        ],
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-unresolved".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let conn = Connection::open(path).unwrap();
    let rows: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT caller_content_hash, callee_expr \
                 FROM entity_unresolved_call_sites \
                 WHERE caller_entity_id = ?1 \
                 ORDER BY site_ordinal",
            )
            .unwrap();
        stmt.query_map(rusqlite::params!["python:function:demo.caller"], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    assert_eq!(
        rows,
        vec![
            (
                "hash-python:function:demo.caller".to_owned(),
                "dynamic_target".to_owned()
            ),
            (
                "hash-python:function:demo.caller".to_owned(),
                "fallback".to_owned()
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_inferred_edges_materializes_and_skips_static_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-inferred").await;
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.inferred",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_contains_edge(
            "python:module:demo",
            "python:function:demo.inferred",
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_calls_edge(
            "python:function:demo.caller",
            "python:function:demo.callee",
            EdgeConfidence::Resolved,
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-inferred".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    let stats = send::<loomweave_storage::InferredEdgeWriteStats>(&tx, |ack| {
        WriterCmd::InsertInferredEdges {
            cache_entry: Box::new(inferred_cache_entry()),
            edges: vec![
                inferred_record("python:function:demo.callee", 10),
                inferred_record("python:function:demo.inferred", 20),
            ],
            ack,
        }
    })
    .await
    .unwrap();

    assert_eq!(stats.inserted_edges, 1);
    assert_eq!(stats.skipped_static_duplicates, 1);

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let conn = Connection::open(path).unwrap();
    let rows: Vec<(String, String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT from_id, to_id, confidence \
                 FROM edges \
                 WHERE kind = 'calls' AND from_id = ?1 \
                 ORDER BY to_id",
            )
            .unwrap();
        stmt.query_map(rusqlite::params!["python:function:demo.caller"], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    assert!(rows.contains(&(
        "python:function:demo.caller".to_owned(),
        "python:function:demo.callee".to_owned(),
        "resolved".to_owned(),
    )));
    assert!(rows.contains(&(
        "python:function:demo.caller".to_owned(),
        "python:function:demo.inferred".to_owned(),
        "inferred".to_owned(),
    )));

    let cached: String = conn
        .query_row(
            "SELECT result_json FROM inferred_edge_cache WHERE caller_entity_id = ?1",
            rusqlite::params!["python:function:demo.caller"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(cached.contains("python:function:demo.inferred"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round_trip_insert_persists_entity() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-1".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity("python:function:demo.hello")),
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-1".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);

    let kind: String = pool
        .with_reader(|conn| {
            let k: String = conn.query_row(
                "SELECT kind FROM entities WHERE id = ?1",
                rusqlite::params!["python:function:demo.hello"],
                |row| row.get(0),
            )?;
            Ok(k)
        })
        .await
        .unwrap();
    assert_eq!(kind, "function");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_entity_replaces_entity_tags_for_same_plugin_entity() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-tags").await;
    let mut first = make_entity("python:function:demo.hello");
    first.tags = vec!["entry-point".to_owned(), "test".to_owned()];
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(first),
        ack,
    })
    .await
    .unwrap();

    let mut second = make_entity("python:function:demo.hello");
    second.tags = vec!["http-route".to_owned()];
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(second),
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-tags".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let conn = Connection::open(path).unwrap();
    let tags: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT tag FROM entity_tags \
                 WHERE entity_id = ?1 AND plugin_id = ?2 \
                 ORDER BY tag",
            )
            .unwrap();
        stmt.query_map(
            rusqlite::params!["python:function:demo.hello", "python"],
            |row| row.get(0),
        )
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    assert_eq!(tags, vec!["http-route".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_entity_is_idempotent_across_runs() {
    // Regression: `loomweave analyze` re-runs against an unchanged corpus
    // must not crash with `UNIQUE constraint failed: entities.id`. The
    // insert path UPSERTs on `id`, preserving `created_at`/`first_seen_commit`
    // and updating the rest from the new run. WS-D smoke gate.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    let entity_id = "python:function:demo.hello";
    let first_created = "2026-05-01T00:00:00Z".to_owned();
    let first_updated = "2026-05-01T00:00:00Z".to_owned();
    let second_updated = "2026-05-02T00:00:00Z".to_owned();

    // Run 1: insert.
    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-1".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    let mut e = make_entity(entity_id);
    e.created_at = first_created.clone();
    e.updated_at = first_updated.clone();
    e.first_seen_commit = Some("commit-abc".to_owned());
    e.last_seen_commit = Some("commit-abc".to_owned());
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(e),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-1".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    // Run 2: re-insert same id with refreshed fields. Must not raise UNIQUE.
    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-2".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    let mut e2 = make_entity(entity_id);
    e2.short_name = "hello-v2".to_owned();
    e2.created_at = "2026-12-31T00:00:00Z".to_owned();
    e2.updated_at = second_updated.clone();
    e2.first_seen_commit = Some("commit-NEW-should-be-ignored".to_owned());
    e2.last_seen_commit = Some("commit-xyz".to_owned());
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(e2),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-2".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let (count, short_name, created_at, updated_at, first_commit, last_commit): (
        i64,
        String,
        String,
        String,
        String,
        String,
    ) = pool
        .with_reader(move |conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            let row = conn.query_row(
                "SELECT short_name, created_at, updated_at, first_seen_commit, last_seen_commit \
                 FROM entities WHERE id = ?1",
                rusqlite::params![entity_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )?;
            Ok((n, row.0, row.1, row.2, row.3, row.4))
        })
        .await
        .unwrap();
    assert_eq!(count, 1, "second InsertEntity must not duplicate the row");
    assert_eq!(
        short_name, "hello-v2",
        "mutable fields refresh on re-insert"
    );
    assert_eq!(
        created_at, first_created,
        "created_at is preserved across re-insert"
    );
    assert_eq!(
        updated_at, second_updated,
        "updated_at refreshes to latest run's value"
    );
    assert_eq!(
        first_commit, "commit-abc",
        "first_seen_commit is preserved across re-insert"
    );
    assert_eq!(
        last_commit, "commit-xyz",
        "last_seen_commit refreshes to latest run's value"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_run_reopens_existing_row_without_pk_conflict() {
    // REQ-FINDING-05 `--resume`: reopening a prior run must reuse its `runs`
    // row, not `INSERT` a second one (which would fail on the run PK — the
    // documented blocker that made `--resume` more than flag-wiring). After a
    // completed run, `ResumeRun` flips the row back to `running` / clears
    // `completed_at`, a re-walk upserts entities, and a second `CommitRun`
    // finalizes the *same* row.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    // Original run: begin → insert → complete.
    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-1".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity("python:function:demo.hello")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-1".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    // Resume the same run id: must NOT raise `UNIQUE constraint failed: runs.id`.
    send::<()>(&tx, |ack| WriterCmd::ResumeRun {
        run_id: "run-1".into(),
        ack,
    })
    .await
    .expect("ResumeRun must reuse the existing run row, not insert a duplicate");
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity("python:function:demo.world")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-1".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let (run_rows, status, entity_count): (i64, String, i64) = pool
        .with_reader(|conn| {
            let run_rows: i64 =
                conn.query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))?;
            let status: String =
                conn.query_row("SELECT status FROM runs WHERE id = 'run-1'", [], |row| {
                    row.get(0)
                })?;
            let entity_count: i64 =
                conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok((run_rows, status, entity_count))
        })
        .await
        .unwrap();
    assert_eq!(
        run_rows, 1,
        "resume reuses the run row — no second `runs` row"
    );
    assert_eq!(
        status, "completed",
        "the resumed run finalizes to completed"
    );
    assert_eq!(
        entity_count, 2,
        "both walks' entities persist under one run"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_run_errors_when_run_id_unknown() {
    // Resuming a run id that was never begun is a caller error, not a silent
    // no-op or an accidental insert.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    let err = send::<()>(&tx, |ack| WriterCmd::ResumeRun {
        run_id: "never-begun".into(),
        ack,
    })
    .await
    .expect_err("resuming an unknown run id must error");
    assert!(
        matches!(err, loomweave_storage::StorageError::WriterProtocol(ref m) if m.contains("never-begun")),
        "error names the missing run id: {err}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_lifecycle_records_owner_pid_and_heartbeat_until_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();
    let expected_pid = i64::from(std::process::id());

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-heartbeat".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();

    {
        let conn = Connection::open(&path).unwrap();
        let (status, owner_pid, heartbeat_at): (String, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT status, owner_pid, heartbeat_at FROM runs WHERE id = 'run-heartbeat'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "running");
        assert_eq!(owner_pid, Some(expected_pid));
        assert_eq!(heartbeat_at.as_deref(), Some(now_iso().as_str()));
    }

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-heartbeat".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    {
        let conn = Connection::open(&path).unwrap();
        let owner_pid: Option<i64> = conn
            .query_row(
                "SELECT owner_pid FROM runs WHERE id = 'run-heartbeat'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner_pid, None, "terminal runs must release pid ownership");
    }

    send::<()>(&tx, |ack| WriterCmd::ResumeRun {
        run_id: "run-heartbeat".into(),
        ack,
    })
    .await
    .unwrap();

    {
        let conn = Connection::open(&path).unwrap();
        let (status, completed_at, owner_pid, heartbeat_at): (
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT status, completed_at, owner_pid, heartbeat_at \
                 FROM runs WHERE id = 'run-heartbeat'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(status, "running");
        assert_eq!(completed_at, None);
        assert_eq!(owner_pid, Some(expected_pid));
        assert!(
            heartbeat_at
                .as_deref()
                .is_some_and(|value| value.ends_with('Z')),
            "resume should refresh heartbeat_at: {heartbeat_at:?}"
        );
    }

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-heartbeat".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[test]
fn stale_running_repair_fails_pre_migration_rows_with_null_heartbeat() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO runs ( \
            id, started_at, completed_at, config, stats, status, owner_pid, heartbeat_at \
         ) VALUES ( \
            'run-null-heartbeat', '2026-02-04T00:00:00.000Z', NULL, '{}', '{}', \
            'running', 999999, NULL \
         )",
        [],
    )
    .expect("insert upgraded pre-heartbeat running row");

    let changed = mark_stale_running_runs_failed(&conn).expect("repair stale runs");
    assert_eq!(changed, 1);

    let (status, owner_pid, completed_at, stats_json): (
        String,
        Option<i64>,
        Option<String>,
        String,
    ) = conn
        .query_row(
            "SELECT status, owner_pid, completed_at, stats \
             FROM runs WHERE id = 'run-null-heartbeat'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read repaired run");
    assert_eq!(status, "failed");
    assert_eq!(owner_pid, None);
    assert!(
        completed_at
            .as_deref()
            .is_some_and(|value| value.ends_with('Z')),
        "repair should stamp completed_at: {completed_at:?}"
    );
    let repair_stats: serde_json::Value = serde_json::from_str(&stats_json).expect("stats json");
    assert_eq!(
        repair_stats["failure_reason"],
        "analyze run abandoned: stale heartbeat"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_core_plugin_cannot_insert_reserved_entity_kind() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-reserved-kind".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();

    let mut reserved = make_entity("python:subsystem:demo");
    reserved.kind = "subsystem".to_owned();

    let result = send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(reserved),
        ack,
    })
    .await;

    if result.is_ok() {
        send::<()>(&tx, |ack| WriterCmd::FailRun {
            run_id: "run-reserved-kind".into(),
            completed_at: now_iso(),
            reason: "test cleanup".into(),
            ack,
        })
        .await
        .unwrap();
    }

    let err = result.expect_err("reserved entity kind from non-core plugin must fail");
    assert!(
        matches!(err, loomweave_storage::StorageError::WriterProtocol(_)),
        "expected WriterProtocol, got {err:?}"
    );
    assert!(
        err.to_string().contains("LMWV-INFRA-RESERVED-ENTITY-KIND"),
        "error should carry reserved-kind code; got {err:#}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let entity_count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(entity_count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_inserts_fact_findings() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-finding").await;
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::InsertFinding {
        finding: Box::new(FindingRecord {
            id: "finding-1".to_owned(),
            tool: "loomweave".to_owned(),
            tool_version: "0.1.0".to_owned(),
            run_id: "run-finding".to_owned(),
            rule_id: "LMWV-FACT-CLUSTERING-WEAK-MODULARITY".to_owned(),
            kind: "fact".to_owned(),
            severity: "INFO".to_owned(),
            confidence: Some(0.9),
            confidence_basis: Some("deterministic modularity calculation".to_owned()),
            entity_id: "python:module:demo".to_owned(),
            related_entities_json: r#"["python:module:demo"]"#.to_owned(),
            message: "Module graph has weak subsystem modularity".to_owned(),
            evidence_json: r#"{"modularity_score":0.0}"#.to_owned(),
            properties_json: r#"{"threshold":0.25}"#.to_owned(),
            supports_json: "[]".to_owned(),
            supported_by_json: "[]".to_owned(),
            created_at: now_iso(),
            updated_at: now_iso(),
        }),
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-finding".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let conn = Connection::open(path).unwrap();
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT rule_id, kind, severity, status, related_entities, evidence, properties, \
             supports, supported_by FROM findings WHERE id = ?1",
            rusqlite::params!["finding-1"],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "LMWV-FACT-CLUSTERING-WEAK-MODULARITY".to_owned(),
            "fact".to_owned(),
            "INFO".to_owned(),
            "open".to_owned(),
            r#"["python:module:demo"]"#.to_owned(),
            r#"{"modularity_score":0.0}"#.to_owned(),
            r#"{"threshold":0.25}"#.to_owned(),
            "[]".to_owned(),
            "[]".to_owned(),
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_finding_is_idempotent_on_resume() {
    // REQ-FINDING-05 `--resume`: a finding id embeds its run_id, so a resume
    // re-walk regenerates the same id. `InsertFinding` must UPSERT (refresh
    // analysis-derived fields, preserve `created_at` and lifecycle) rather than
    // fail on `UNIQUE constraint: findings.id`.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    let finding = |message: &str, created_at: &str, updated_at: &str| FindingRecord {
        id: "core:finding:run-resume:weak-modularity".to_owned(),
        tool: "loomweave".to_owned(),
        tool_version: "1.0.0".to_owned(),
        run_id: "run-resume".to_owned(),
        rule_id: "LMWV-FACT-CLUSTERING-WEAK-MODULARITY".to_owned(),
        kind: "fact".to_owned(),
        severity: "INFO".to_owned(),
        confidence: Some(0.9),
        confidence_basis: Some("deterministic".to_owned()),
        entity_id: "python:module:demo".to_owned(),
        related_entities_json: r#"["python:module:demo"]"#.to_owned(),
        message: message.to_owned(),
        evidence_json: r#"{"modularity_score":0.0}"#.to_owned(),
        properties_json: r#"{"threshold":0.25}"#.to_owned(),
        supports_json: "[]".to_owned(),
        supported_by_json: "[]".to_owned(),
        created_at: created_at.to_owned(),
        updated_at: updated_at.to_owned(),
    };

    // Original run: insert the finding.
    begin_demo_run(&tx, "run-resume").await;
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertFinding {
        finding: Box::new(finding(
            "first message",
            "2026-05-01T00:00:00Z",
            "2026-05-01T00:00:00Z",
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-resume".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    // Resume: re-insert the same finding id with a refreshed message. Must not
    // raise UNIQUE; must refresh `message`/`updated_at`, preserve `created_at`.
    send::<()>(&tx, |ack| WriterCmd::ResumeRun {
        run_id: "run-resume".into(),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertFinding {
        finding: Box::new(finding(
            "refreshed message",
            "2099-01-01T00:00:00Z",
            "2026-05-02T00:00:00Z",
        )),
        ack,
    })
    .await
    .expect("re-inserting a run-scoped finding id on resume must upsert, not error");
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-resume".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let conn = Connection::open(path).unwrap();
    let (count, message, status, created_at, updated_at): (i64, String, String, String, String) =
        conn.query_row(
            "SELECT COUNT(*), MAX(message), MAX(status), MAX(created_at), MAX(updated_at) \
             FROM findings WHERE id = 'core:finding:run-resume:weak-modularity'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(count, 1, "resume upserts the finding — no duplicate row");
    assert_eq!(
        message, "refreshed message",
        "analysis-derived fields refresh"
    );
    assert_eq!(
        status, "open",
        "lifecycle status is preserved across resume"
    );
    assert_eq!(
        created_at, "2026-05-01T00:00:00Z",
        "created_at (first-seen) is preserved across resume",
    );
    assert_eq!(
        updated_at, "2026-05-02T00:00:00Z",
        "updated_at refreshes to the resume walk's value",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entity_source_file_id_rejects_non_source_anchor_entity() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-source-anchor").await;
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.source_like_but_not_file",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();

    let mut bad = make_entity_with_parent(
        "python:function:demo.bad_source_anchor",
        Some("python:module:demo"),
    );
    bad.source_file_id = Some("python:function:demo.source_like_but_not_file".to_owned());

    let result = send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(bad),
        ack,
    })
    .await;
    let err = result.expect_err("source_file_id must point at a source-anchor entity");
    assert!(
        format!("{err:?}").contains("LMWV-INFRA-SOURCE-FILE-KIND-CONTRACT"),
        "expected LMWV-INFRA-SOURCE-FILE-KIND-CONTRACT in error; got {err:?}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entity_source_file_id_accepts_core_file_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-source-file-anchor").await;
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_file_entity("core:file:demo.py")),
        ack,
    })
    .await
    .unwrap();

    let mut module = make_module_entity("python:module:demo");
    module.parent_id = Some("core:file:demo.py".to_owned());
    module.source_file_id = Some("core:file:demo.py".to_owned());
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(module),
        ack,
    })
    .await
    .expect("module source anchor may reference core file entity");

    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_contains_edge(
            "core:file:demo.py",
            "python:module:demo",
        )),
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-source-file-anchor".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_entity_rejected_as_source_file_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-source-module-reject").await;
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();

    let mut module = make_module_entity("python:module:demo");
    module.source_file_id = Some("python:module:demo".to_owned());

    let result = send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(module),
        ack,
    })
    .await
    .expect_err("module entity must not be accepted as a source_file_id anchor");
    assert!(
        format!("{result:?}").contains("LMWV-INFRA-SOURCE-FILE-KIND-CONTRACT"),
        "expected LMWV-INFRA-SOURCE-FILE-KIND-CONTRACT in error; got {result:?}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[test]
fn python_plugin_edge_kinds_are_accepted_by_writer_contract() {
    let manifest =
        loomweave_core::parse_manifest(include_bytes!("../../../plugins/python/plugin.toml"))
            .expect("production Python plugin manifest should parse");
    let writer_kinds: std::collections::BTreeSet<&'static str> =
        loomweave_storage::known_scan_time_edge_kinds().collect();
    let missing: Vec<&str> = manifest
        .ontology
        .edge_kinds
        .iter()
        .map(String::as_str)
        .filter(|kind| !writer_kinds.contains(kind))
        .collect();

    assert!(
        missing.is_empty(),
        "Python plugin declares edge kind(s) the writer rejects: {missing:?}; \
         writer accepts {writer_kinds:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_size_fifty_commits_every_fifty_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-1".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();

    for i in 0..150 {
        let id = format!("python:function:demo.f{i:03}");
        send::<()>(&tx, |ack| WriterCmd::InsertEntity {
            entity: Box::new(make_entity(&id)),
            ack,
        })
        .await
        .unwrap();
    }

    assert_eq!(writer.commits_observed.load(Ordering::Relaxed), 3);

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-1".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    assert_eq!(writer.commits_observed.load(Ordering::Relaxed), 4);

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(count, 150);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloned_senders_accept_concurrent_entity_producers() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-concurrent-producers").await;

    let tx_a = tx.clone();
    let producer_a = tokio::spawn(async move {
        for i in 0..25 {
            let id = format!("python:function:demo.producer_a_{i:02}");
            send::<()>(&tx_a, |ack| WriterCmd::InsertEntity {
                entity: Box::new(make_entity(&id)),
                ack,
            })
            .await
            .unwrap();
        }
    });
    let tx_b = tx.clone();
    let producer_b = tokio::spawn(async move {
        for i in 0..25 {
            let id = format!("python:function:demo.producer_b_{i:02}");
            send::<()>(&tx_b, |ack| WriterCmd::InsertEntity {
                entity: Box::new(make_entity(&id)),
                ack,
            })
            .await
            .unwrap();
        }
    });
    producer_a.await.unwrap();
    producer_b.await.unwrap();

    assert_eq!(writer.commits_observed.load(Ordering::Relaxed), 1);

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-concurrent-producers".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(count, 50);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fail_run_rolls_back_pending_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-fail".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();

    for i in 0..10 {
        let id = format!("python:function:demo.g{i:03}");
        send::<()>(&tx, |ack| WriterCmd::InsertEntity {
            entity: Box::new(make_entity(&id)),
            ack,
        })
        .await
        .unwrap();
    }

    send::<()>(&tx, |ack| WriterCmd::FailRun {
        run_id: "run-fail".into(),
        reason: "deliberate test failure".into(),
        completed_at: now_iso(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let entity_count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(entity_count, 0, "FailRun did not roll back inserts");

    let status: String = pool
        .with_reader(|conn| {
            let s: String =
                conn.query_row("SELECT status FROM runs WHERE id = 'run-fail'", [], |row| {
                    row.get(0)
                })?;
            Ok(s)
        })
        .await
        .unwrap();
    assert_eq!(status, "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fail_run_after_prior_batch_commit_rolls_back_only_pending_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-fail-after-batch".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();

    for i in 0..60 {
        let id = format!("python:function:demo.batch_fail_{i:03}");
        send::<()>(&tx, |ack| WriterCmd::InsertEntity {
            entity: Box::new(make_entity(&id)),
            ack,
        })
        .await
        .unwrap();
    }

    assert_eq!(writer.commits_observed.load(Ordering::Relaxed), 1);

    send::<()>(&tx, |ack| WriterCmd::FailRun {
        run_id: "run-fail-after-batch".into(),
        reason: "deliberate test failure".into(),
        completed_at: now_iso(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let (entity_count, status): (i64, String) = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            let s: String = conn.query_row(
                "SELECT status FROM runs WHERE id = 'run-fail-after-batch'",
                [],
                |row| row.get(0),
            )?;
            Ok((n, s))
        })
        .await
        .unwrap();

    assert_eq!(
        entity_count, 50,
        "FailRun must preserve prior committed batches and roll back only the open batch"
    );
    assert_eq!(status, "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_entity_without_begin_run_is_protocol_violation() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    let result = send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity("python:function:demo.early")),
        ack,
    })
    .await;

    let err = result.expect_err("InsertEntity without BeginRun should fail");
    assert!(
        matches!(err, loomweave_storage::StorageError::WriterProtocol(_)),
        "expected WriterProtocol, got {err:?}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_run_without_begin_run_is_protocol_violation() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    let result = send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-cold".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await;

    let err = result.expect_err("CommitRun without BeginRun should fail");
    assert!(
        matches!(err, loomweave_storage::StorageError::WriterProtocol(_)),
        "expected WriterProtocol, got {err:?}"
    );
    assert!(
        err.to_string().contains("without a preceding BeginRun"),
        "error should explain missing BeginRun: {err}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fail_run_without_begin_run_is_protocol_violation() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    let result = send::<()>(&tx, |ack| WriterCmd::FailRun {
        run_id: "run-cold".into(),
        reason: "test".into(),
        completed_at: now_iso(),
        ack,
    })
    .await;

    let err = result.expect_err("FailRun without BeginRun should fail");
    assert!(
        matches!(err, loomweave_storage::StorageError::WriterProtocol(_)),
        "expected WriterProtocol, got {err:?}"
    );
    assert!(
        err.to_string().contains("without a preceding BeginRun"),
        "error should explain missing BeginRun: {err}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_run_with_stale_run_id_is_protocol_violation() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-active").await;

    let result = send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-stale".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await;

    let err = result.expect_err("stale CommitRun run_id should fail");
    assert!(
        matches!(err, loomweave_storage::StorageError::WriterProtocol(_)),
        "expected WriterProtocol, got {err:?}"
    );
    assert!(
        err.to_string().contains("run-stale"),
        "error should name stale run id: {err}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fail_run_with_stale_run_id_is_protocol_violation() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-active").await;

    let result = send::<()>(&tx, |ack| WriterCmd::FailRun {
        run_id: "run-stale".into(),
        reason: "test".into(),
        completed_at: now_iso(),
        ack,
    })
    .await;

    let err = result.expect_err("stale FailRun run_id should fail");
    assert!(
        matches!(err, loomweave_storage::StorageError::WriterProtocol(_)),
        "expected WriterProtocol, got {err:?}"
    );
    assert!(
        err.to_string().contains("run-stale"),
        "error should name stale run id: {err}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_begin_run_is_protocol_violation() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-a".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();

    let result = send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-b".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await;

    let err = result.expect_err("second BeginRun should fail");
    assert!(
        matches!(err, loomweave_storage::StorageError::WriterProtocol(_)),
        "expected WriterProtocol, got {err:?}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round_trip_insert_persists_contains_edge() {
    // B.3: round-trip a (module, function) pair with a contains edge.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-1".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.hello",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_contains_edge(
            "python:module:demo",
            "python:function:demo.hello",
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-1".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let (kind, from_id, to_id): (String, String, String) = pool
        .with_reader(|conn| {
            let row = conn.query_row("SELECT kind, from_id, to_id FROM edges", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            Ok(row)
        })
        .await
        .unwrap();
    assert_eq!(kind, "contains");
    assert_eq!(from_id, "python:module:demo");
    assert_eq!(to_id, "python:function:demo.hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contains_edge_with_byte_offsets_rejected_by_per_kind_contract() {
    // ADR-026 decision 3 / B.3 Q5: contains edges MUST have NULL source range.
    // Writer rejects with LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-c".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.hello",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();

    let mut bad = make_contains_edge("python:module:demo", "python:function:demo.hello");
    bad.source_byte_start = Some(0);
    bad.source_byte_end = Some(42);

    let result = send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(bad),
        ack,
    })
    .await;
    let err = result.expect_err("contains edge with byte offsets should be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT"),
        "expected LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT in error; got: {msg}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calls_edge_without_byte_offsets_rejected_by_per_kind_contract() {
    // Dead-code dispatch test: B.3 emits no `calls` edges, but the per-kind
    // contract dispatch must be uniform across all 8 known kinds.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-k".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.caller",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.callee",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();

    let bad = EdgeRecord {
        kind: "calls".to_owned(),
        from_id: "python:function:demo.caller".to_owned(),
        to_id: "python:function:demo.callee".to_owned(),
        confidence: EdgeConfidence::Resolved,
        properties_json: None,
        source_file_id: None,
        source_byte_start: None,
        source_byte_end: None,
    };
    let result = send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(bad),
        ack,
    })
    .await;
    let err = result.expect_err("calls edge without byte offsets should be rejected");
    assert!(
        format!("{err:?}").contains("LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT"),
        "expected LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT in error; got {err:?}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_edge_kind_rejected_strictly() {
    // Per advisor + ADR-026: 8 known kinds form the ontology; unknown kinds
    // reaching the writer are a manifest/wire drift bug. Reject strictly.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-u".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.f",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();

    let bad = EdgeRecord {
        kind: "smells_like".to_owned(),
        from_id: "python:module:demo".to_owned(),
        to_id: "python:function:demo.f".to_owned(),
        confidence: EdgeConfidence::Resolved,
        properties_json: None,
        source_file_id: None,
        source_byte_start: None,
        source_byte_end: None,
    };
    let result = send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(bad),
        ack,
    })
    .await;
    let err = result.expect_err("unknown edge kind should be rejected");
    assert!(
        format!("{err:?}").contains("LMWV-INFRA-EDGE-UNKNOWN-KIND"),
        "expected LMWV-INFRA-EDGE-UNKNOWN-KIND in error; got {err:?}"
    );
    assert_eq!(
        writer.dropped_edges_total.load(Ordering::Relaxed),
        1,
        "unknown-kind rejection should increment dropped_edges_total"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_contains_edge_upsert_keeps_one_row_without_drop_counter() {
    // H-01: idempotent re-analyze refreshes existing edge rows via upsert.
    // Duplicate triples still collapse to one row, but they are accepted writes
    // rather than silent drops.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-d".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.hello",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();
    let edge = make_contains_edge("python:module:demo", "python:function:demo.hello");
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(edge.clone()),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(edge),
        ack,
    })
    .await
    .unwrap();

    assert_eq!(writer.dropped_edges_total.load(Ordering::Relaxed), 0);

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-d".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 1).unwrap();
    let count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(count, 1, "duplicate contains edge should be deduped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_anchored_edge_updates_source_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-edge-upsert").await;
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_file_entity("core:file:demo.py")),
        ack,
    })
    .await
    .unwrap();
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;

    let mut first = make_calls_edge(
        "python:function:demo.caller",
        "python:function:demo.callee",
        EdgeConfidence::Resolved,
    );
    first.source_file_id = Some("core:file:demo.py".to_owned());
    first.source_byte_start = Some(10);
    first.source_byte_end = Some(18);
    first.properties_json = Some(r#"{"site":"old"}"#.to_owned());
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(first),
        ack,
    })
    .await
    .unwrap();

    let mut updated = make_calls_edge(
        "python:function:demo.caller",
        "python:function:demo.callee",
        EdgeConfidence::Ambiguous,
    );
    updated.source_file_id = Some("core:file:demo.py".to_owned());
    updated.source_byte_start = Some(30);
    updated.source_byte_end = Some(42);
    updated.properties_json = Some(r#"{"site":"new"}"#.to_owned());
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(updated),
        ack,
    })
    .await
    .unwrap();

    assert_eq!(
        writer.dropped_edges_total.load(Ordering::Relaxed),
        0,
        "metadata refreshes are accepted edge writes, not dropped dedupes"
    );

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-edge-upsert".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 1).unwrap();
    let row: (i64, i64, String, String) = pool
        .with_reader(|conn| {
            conn.query_row(
                "SELECT source_byte_start, source_byte_end, confidence, properties \
                 FROM edges WHERE kind = 'calls'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(
        row,
        (
            30,
            42,
            "ambiguous".to_owned(),
            r#"{"site":"new"}"#.to_owned()
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_edges_for_source_file_removes_only_stale_anchored_edges() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-edge-replace").await;
    for entity in [
        make_file_entity_named("core:file:demo.py", "demo.py"),
        make_file_entity_named("core:file:other.py", "other.py"),
    ] {
        send::<()>(&tx, |ack| WriterCmd::InsertEntity {
            entity: Box::new(entity),
            ack,
        })
        .await
        .unwrap();
    }
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;

    let mut stale_call = make_calls_edge(
        "python:function:demo.caller",
        "python:function:demo.callee",
        EdgeConfidence::Resolved,
    );
    stale_call.source_file_id = Some("core:file:demo.py".to_owned());
    let mut stale_ref = make_references_edge(
        "python:function:demo.caller",
        "python:function:demo.callee",
        EdgeConfidence::Resolved,
    );
    stale_ref.source_file_id = Some("core:file:demo.py".to_owned());
    let mut other_file_call = make_calls_edge(
        "python:function:demo.callee",
        "python:function:demo.caller",
        EdgeConfidence::Resolved,
    );
    other_file_call.source_file_id = Some("core:file:other.py".to_owned());
    for edge in [stale_call, stale_ref, other_file_call] {
        send::<()>(&tx, |ack| WriterCmd::InsertEdge {
            edge: Box::new(edge),
            ack,
        })
        .await
        .unwrap();
    }

    send::<()>(&tx, |ack| WriterCmd::ReplaceAnchoredEdgesForSourceFile {
        source_file_id: "core:file:demo.py".to_owned(),
        ack,
    })
    .await
    .unwrap();

    let mut fresh_call = make_calls_edge(
        "python:function:demo.caller",
        "python:function:demo.callee",
        EdgeConfidence::Resolved,
    );
    fresh_call.source_file_id = Some("core:file:demo.py".to_owned());
    fresh_call.source_byte_start = Some(100);
    fresh_call.source_byte_end = Some(108);
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(fresh_call),
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-edge-replace".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 1).unwrap();
    let rows: Vec<(String, Option<String>, String, String)> = pool
        .with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT kind, source_file_id, from_id, to_id \
                 FROM edges ORDER BY kind, source_file_id, from_id, to_id",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert!(
        rows.iter()
            .any(|(kind, source_file_id, _, _)| { kind == "contains" && source_file_id.is_none() })
    );
    assert!(rows.iter().any(|(kind, source_file_id, from_id, _)| {
        kind == "calls"
            && source_file_id.as_deref() == Some("core:file:demo.py")
            && from_id == "python:function:demo.caller"
    }));
    assert!(rows.iter().any(|(kind, source_file_id, from_id, _)| {
        kind == "calls"
            && source_file_id.as_deref() == Some("core:file:other.py")
            && from_id == "python:function:demo.callee"
    }));
    assert!(!rows.iter().any(|(kind, source_file_id, _, _)| {
        kind == "references" && source_file_id.as_deref() == Some("core:file:demo.py")
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_id_without_matching_contains_edge_rejects_run() {
    // B.3 §3 Q2 / §5: parent_id and contains edges are dual encodings of
    // the same fact. Mismatch at CommitRun time rejects the run with
    // LMWV-INFRA-PARENT-CONTAINS-MISMATCH and rolls back the transaction.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-m".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    // Child claims parent_id but no contains edge emitted.
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.lonely",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();

    let result = send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-m".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await;
    let err = result.expect_err("CommitRun should reject parent-id mismatch");
    assert!(
        format!("{err:?}").contains("LMWV-INFRA-PARENT-CONTAINS-MISMATCH"),
        "expected LMWV-INFRA-PARENT-CONTAINS-MISMATCH in error; got {err:?}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    // Transaction rolled back; run row marked failed.
    let pool = ReaderPool::open(&path, 1).unwrap();
    let (status, entity_count): (String, i64) = pool
        .with_reader(|conn| {
            let s: String =
                conn.query_row("SELECT status FROM runs WHERE id = 'run-m'", [], |row| {
                    row.get(0)
                })?;
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok((s, n))
        })
        .await
        .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(entity_count, 0, "rejection must roll back entity inserts");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphan_contains_edge_with_no_matching_parent_id_rejects_run() {
    // Inverse direction of parent-id consistency: a contains edge exists but
    // the child entity's parent_id does not match (or is NULL).
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-o".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    // Child has no parent_id, but we'll emit a contains edge anyway.
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent("python:function:demo.orphan", None)),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_contains_edge(
            "python:module:demo",
            "python:function:demo.orphan",
        )),
        ack,
    })
    .await
    .unwrap();

    let result = send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-o".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await;
    let err = result.expect_err("CommitRun should reject orphan contains edge");
    assert!(
        format!("{err:?}").contains("LMWV-INFRA-PARENT-CONTAINS-MISMATCH"),
        "expected LMWV-INFRA-PARENT-CONTAINS-MISMATCH; got {err:?}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flush_run_batch_rejects_parent_contains_mismatch_before_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-flush-mismatch".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.lonely",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();

    let result = send::<()>(&tx, |ack| WriterCmd::FlushRunBatch { ack }).await;
    let err = result.expect_err("FlushRunBatch should reject parent mismatch");
    assert!(
        format!("{err:?}").contains("LMWV-INFRA-PARENT-CONTAINS-MISMATCH"),
        "expected LMWV-INFRA-PARENT-CONTAINS-MISMATCH in error; got {err:?}"
    );

    send::<()>(&tx, |ack| WriterCmd::FailRun {
        run_id: "run-flush-mismatch".into(),
        reason: "phase3 clustering failed: parent mismatch".into(),
        completed_at: now_iso(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 1).unwrap();
    let (status, entity_count): (String, i64) = pool
        .with_reader(|conn| {
            let s: String = conn.query_row(
                "SELECT status FROM runs WHERE id = 'run-flush-mismatch'",
                [],
                |row| row.get(0),
            )?;
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok((s, n))
        })
        .await
        .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(
        entity_count, 0,
        "flush-boundary rejection must roll back pending plugin rows"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_in_batch_counts_entities_and_edges_uniformly() {
    // Q2 / Task 2: rename inserts_in_batch -> writes_in_batch and increment
    // on both InsertEntity and InsertEdge. With batch_size=4, a mix of 2
    // entities + 2 edges should trigger one mid-run commit.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 4, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-b".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_module_entity("python:module:demo")),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.a",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_contains_edge(
            "python:module:demo",
            "python:function:demo.a",
        )),
        ack,
    })
    .await
    .unwrap();
    // Pre-fourth write: no batch commit yet.
    assert_eq!(writer.commits_observed.load(Ordering::Relaxed), 0);
    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity_with_parent(
            "python:function:demo.b",
            Some("python:module:demo"),
        )),
        ack,
    })
    .await
    .unwrap();
    // Fourth write crosses the boundary.
    assert_eq!(writer.commits_observed.load(Ordering::Relaxed), 1);

    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_contains_edge(
            "python:module:demo",
            "python:function:demo.b",
        )),
        ack,
    })
    .await
    .unwrap();
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-b".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structural_contains_ambiguous_confidence_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-contains-ambiguous").await;
    seed_module_and_functions(&tx).await;

    assert_edge_rejected_with_counter(
        &writer,
        &tx,
        make_structural_edge(
            "contains",
            "python:module:demo",
            "python:function:demo.caller",
            EdgeConfidence::Ambiguous,
        ),
        "LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT",
    )
    .await;

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structural_contains_inferred_confidence_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-contains-inferred").await;
    seed_module_and_functions(&tx).await;

    assert_edge_rejected_with_counter(
        &writer,
        &tx,
        make_structural_edge(
            "contains",
            "python:module:demo",
            "python:function:demo.caller",
            EdgeConfidence::Inferred,
        ),
        "LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT",
    )
    .await;

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_structural_kind_inferred_confidence_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-subsystem-inferred").await;
    seed_module_and_functions(&tx).await;

    assert_edge_rejected_with_counter(
        &writer,
        &tx,
        make_structural_edge(
            "in_subsystem",
            "python:module:demo",
            "python:function:demo.caller",
            EdgeConfidence::Inferred,
        ),
        "LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT",
    )
    .await;

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchored_calls_inferred_confidence_rejected_at_scan_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-calls-inferred").await;
    seed_module_and_functions(&tx).await;

    assert_edge_rejected_with_counter(
        &writer,
        &tx,
        make_calls_edge(
            "python:function:demo.caller",
            "python:function:demo.callee",
            EdgeConfidence::Inferred,
        ),
        "LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT",
    )
    .await;

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchored_references_missing_or_partial_byte_offsets_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-references-range-contract").await;
    seed_module_and_functions(&tx).await;

    let cases = [
        ("missing", None, None),
        ("start-only", Some(20), None),
        ("end-only", None, Some(25)),
    ];
    for (idx, (label, start, end)) in cases.into_iter().enumerate() {
        let mut edge = make_references_edge(
            "python:function:demo.caller",
            "python:function:demo.callee",
            EdgeConfidence::Resolved,
        );
        edge.source_byte_start = start;
        edge.source_byte_end = end;
        let result = send::<()>(&tx, |ack| WriterCmd::InsertEdge {
            edge: Box::new(edge),
            ack,
        })
        .await;
        let err = result.expect_err("references edge with incomplete range should be rejected");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT"),
            "{label}: expected LMWV-INFRA-EDGE-SOURCE-RANGE-CONTRACT in error; got {msg}"
        );
        assert_eq!(
            writer.dropped_edges_total.load(Ordering::Relaxed),
            idx + 1,
            "{label}: rejection should increment dropped_edges_total"
        );
    }

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchored_references_inferred_confidence_rejected_at_scan_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-references-inferred").await;
    seed_module_and_functions(&tx).await;

    assert_edge_rejected_with_counter(
        &writer,
        &tx,
        make_references_edge(
            "python:function:demo.caller",
            "python:function:demo.callee",
            EdgeConfidence::Inferred,
        ),
        "LMWV-INFRA-EDGE-CONFIDENCE-CONTRACT",
    )
    .await;

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchored_references_ambiguous_confidence_is_accepted_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-references-ambiguous").await;
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;

    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_references_edge(
            "python:function:demo.caller",
            "python:function:demo.callee",
            EdgeConfidence::Ambiguous,
        )),
        ack,
    })
    .await
    .unwrap();
    assert_eq!(writer.dropped_edges_total.load(Ordering::Relaxed), 0);
    assert_eq!(writer.ambiguous_edges_total.load(Ordering::Relaxed), 1);

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-confidence-references-ambiguous".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 1).unwrap();
    let (count, confidence): (i64, String) = pool
        .with_reader(|conn| {
            let row = conn.query_row(
                "SELECT COUNT(*), max(confidence) FROM edges WHERE kind = 'references'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok(row)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(confidence, "ambiguous");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchored_references_resolved_confidence_is_accepted_without_counters() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-references-resolved").await;
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;

    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_references_edge(
            "python:function:demo.caller",
            "python:function:demo.callee",
            EdgeConfidence::Resolved,
        )),
        ack,
    })
    .await
    .unwrap();
    assert_eq!(writer.dropped_edges_total.load(Ordering::Relaxed), 0);
    assert_eq!(writer.ambiguous_edges_total.load(Ordering::Relaxed), 0);

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-confidence-references-resolved".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 1).unwrap();
    let (count, confidence): (i64, String) = pool
        .with_reader(|conn| {
            let row = conn.query_row(
                "SELECT COUNT(*), max(confidence) FROM edges WHERE kind = 'references'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok(row)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(confidence, "resolved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchored_calls_ambiguous_confidence_is_accepted_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-calls-ambiguous").await;
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;

    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_calls_edge(
            "python:function:demo.caller",
            "python:function:demo.callee",
            EdgeConfidence::Ambiguous,
        )),
        ack,
    })
    .await
    .unwrap();
    assert_eq!(writer.dropped_edges_total.load(Ordering::Relaxed), 0);
    assert_eq!(writer.ambiguous_edges_total.load(Ordering::Relaxed), 1);

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-confidence-calls-ambiguous".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 1).unwrap();
    let (count, confidence): (i64, String) = pool
        .with_reader(|conn| {
            let row = conn.query_row(
                "SELECT COUNT(*), max(confidence) FROM edges WHERE kind = 'calls'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok(row)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(confidence, "ambiguous");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchored_calls_resolved_confidence_is_accepted_without_counters() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    begin_demo_run(&tx, "run-confidence-calls-resolved").await;
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;

    send::<()>(&tx, |ack| WriterCmd::InsertEdge {
        edge: Box::new(make_calls_edge(
            "python:function:demo.caller",
            "python:function:demo.callee",
            EdgeConfidence::Resolved,
        )),
        ack,
    })
    .await
    .unwrap();
    assert_eq!(writer.dropped_edges_total.load(Ordering::Relaxed), 0);
    assert_eq!(writer.ambiguous_edges_total.load(Ordering::Relaxed), 0);

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-confidence-calls-resolved".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 1).unwrap();
    let (count, confidence): (i64, String) = pool
        .with_reader(|conn| {
            let row = conn.query_row(
                "SELECT COUNT(*), max(confidence) FROM edges WHERE kind = 'calls'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok(row)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(confidence, "resolved");
}

/// Regression for review finding #8: if the channel closes while a run is
/// still open (e.g. the Writer is dropped before CommitRun/FailRun is sent),
/// the actor must update the `runs` row to `status='failed'` rather than
/// leaving it stuck at `'running'`. Without this, every crashed analyze
/// accumulates an orphaned row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_close_with_open_run_self_heals_to_failed() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-abandoned".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        head_commit: None,
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: Box::new(make_entity("python:function:demo.hello")),
        ack,
    })
    .await
    .unwrap();

    // Caller disappears mid-run — no CommitRun / FailRun sent.
    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    // The run row must have been self-healed to 'failed'. The pending insert
    // is rolled back.
    let pool = ReaderPool::open(&path, 1).expect("pool");
    let (run_status, stats_json, entity_count): (String, String, i64) = pool
        .with_reader(|conn| {
            let (s, st): (String, String) = conn.query_row(
                "SELECT status, stats FROM runs WHERE id = 'run-abandoned'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok((s, st, n))
        })
        .await
        .expect("reader query");

    assert_eq!(
        run_status, "failed",
        "self-heal must mark abandoned run as failed"
    );
    let stats: serde_json::Value =
        serde_json::from_str(&stats_json).expect("stats must be valid JSON");
    assert_eq!(
        stats["failure_reason"], "writer channel closed unexpectedly",
        "failure_reason must cite channel close; got stats = {stats_json}"
    );
    assert_eq!(
        entity_count, 0,
        "pending insert must be rolled back when channel closes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_run_truncates_wal_while_writer_still_alive() {
    // clarion-cdee445ed8: ADR-005 commits `.weft/loomweave/loomweave.db`, so a finished
    // analyze must leave the on-disk file a whole, committable snapshot WITHOUT
    // waiting for the process to exit. `CommitRun` now issues an explicit
    // `wal_checkpoint(TRUNCATE)`. We assert the WAL is reset to 0 bytes with the
    // writer STILL ALIVE — proving it is the post-commit checkpoint, not SQLite's
    // last-connection-close cleanup (which would only fire after the drop below).
    //
    // Scope note: `CommitRun` is reached only at the end of an analyze run, never
    // by serve's summary-write path, so there is no per-write checkpoint cost. And
    // while a long-lived serve holds reader connections open the TRUNCATE is
    // best-effort (a reader can hold it back, harmlessly); `loomweave db backup`
    // remains the way to capture a consistent committable copy mid-serve.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let wal_path = dir.path().join("loomweave.db-wal");

    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();
    begin_demo_run(&tx, "run-wal").await;
    seed_module_and_functions(&tx).await;
    seed_contains_edges_for_demo_functions(&tx).await;
    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-wal".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    // Writer is STILL ALIVE here (tx/writer not dropped): the only thing that
    // could have emptied the WAL is the explicit post-CommitRun checkpoint.
    let wal_after_commit = std::fs::metadata(&wal_path).map_or(0, |m| m.len());
    assert_eq!(
        wal_after_commit, 0,
        "CommitRun must TRUNCATE-checkpoint the WAL to 0 bytes while the writer is \
         still alive, so the committed loomweave.db is whole on disk; got {wal_after_commit}"
    );

    // Clean shutdown still succeeds (and the actor task joins without error).
    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
    let wal_after_shutdown = std::fs::metadata(&wal_path).map_or(0, |m| m.len());
    assert_eq!(
        wal_after_shutdown, 0,
        "WAL must remain truncated after shutdown; got {wal_after_shutdown}"
    );
}
