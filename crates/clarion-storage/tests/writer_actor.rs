//! Writer-actor integration tests.
//!
//! Covers: round-trip insert, per-N-batch commit cadence, `FailRun` rollback.

use std::sync::atomic::Ordering;

use rusqlite::Connection;
use tokio::sync::oneshot;

use clarion_storage::{
    ReaderPool, Writer,
    commands::{EdgeConfidence, EdgeRecord, EntityRecord, RunStatus, WriterCmd},
    pragma, schema,
};

fn prepared_db(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("clarion.db");
    let mut conn = Connection::open(&path).unwrap();
    pragma::apply_write_pragmas(&conn).unwrap();
    schema::apply_migrations(&mut conn).unwrap();
    path
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
        source_byte_start: None,
        source_byte_end: None,
        source_line_start: None,
        source_line_end: None,
        properties_json: "{}".to_owned(),
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

fn make_contains_edge(from_id: &str, to_id: &str) -> EdgeRecord {
    EdgeRecord {
        kind: "contains".to_owned(),
        from_id: from_id.to_owned(),
        to_id: to_id.to_owned(),
        confidence: EdgeConfidence::Resolved,
        properties_json: None,
        source_file_id: Some(from_id.to_owned()),
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
        source_file_id: Some(from_id.to_owned()),
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
        source_file_id: Some("python:module:demo".to_owned()),
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
        source_file_id: Some("python:module:demo".to_owned()),
        source_byte_start: Some(20),
        source_byte_end: Some(25),
    }
}

async fn begin_demo_run(tx: &tokio::sync::mpsc::Sender<WriterCmd>, run_id: &str) {
    send::<()>(tx, |ack| WriterCmd::BeginRun {
        run_id: run_id.into(),
        config_json: "{}".into(),
        started_at: now_iso(),
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
    build: impl FnOnce(oneshot::Sender<Result<T, clarion_storage::StorageError>>) -> WriterCmd,
) -> Result<T, clarion_storage::StorageError> {
    let (ack_tx, ack_rx) = oneshot::channel();
    tx.send(build(ack_tx)).await.unwrap();
    ack_rx.await.unwrap()
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
async fn batch_size_fifty_commits_every_fifty_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-1".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
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
async fn fail_run_rolls_back_pending_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-fail".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
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
        matches!(err, clarion_storage::StorageError::WriterProtocol(_)),
        "expected WriterProtocol, got {err:?}"
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
        ack,
    })
    .await
    .unwrap();

    let result = send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-b".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        ack,
    })
    .await;

    let err = result.expect_err("second BeginRun should fail");
    assert!(
        matches!(err, clarion_storage::StorageError::WriterProtocol(_)),
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
    // Writer rejects with CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-c".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
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
        msg.contains("CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT"),
        "expected CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT in error; got: {msg}"
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
        source_file_id: Some("python:module:demo".to_owned()),
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
        format!("{err:?}").contains("CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT"),
        "expected CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT in error; got {err:?}"
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
        source_file_id: Some("python:module:demo".to_owned()),
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
        format!("{err:?}").contains("CLA-INFRA-EDGE-UNKNOWN-KIND"),
        "expected CLA-INFRA-EDGE-UNKNOWN-KIND in error; got {err:?}"
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
async fn duplicate_contains_edge_is_deduped_and_counter_increments() {
    // B.3 §6 / ADR-026: idempotent re-analyze means UNIQUE-conflicting edges
    // are silently deduped and counted on dropped_edges_total.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-d".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
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

    assert_eq!(writer.dropped_edges_total.load(Ordering::Relaxed), 1);

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
async fn parent_id_without_matching_contains_edge_rejects_run() {
    // B.3 §3 Q2 / §5: parent_id and contains edges are dual encodings of
    // the same fact. Mismatch at CommitRun time rejects the run with
    // CLA-INFRA-PARENT-CONTAINS-MISMATCH and rolls back the transaction.
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-m".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
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
        format!("{err:?}").contains("CLA-INFRA-PARENT-CONTAINS-MISMATCH"),
        "expected CLA-INFRA-PARENT-CONTAINS-MISMATCH in error; got {err:?}"
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
        format!("{err:?}").contains("CLA-INFRA-PARENT-CONTAINS-MISMATCH"),
        "expected CLA-INFRA-PARENT-CONTAINS-MISMATCH; got {err:?}"
    );

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();
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
        "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT",
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
        "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT",
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
        "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT",
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
        "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT",
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
            msg.contains("CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT"),
            "{label}: expected CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT in error; got {msg}"
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
        "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT",
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
    let (observed_status, observed_reason, entity_count): (String, String, i64) = pool
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
        observed_status, "failed",
        "self-heal must mark abandoned run as failed"
    );
    assert!(
        observed_reason.contains("writer channel closed unexpectedly"),
        "failure_reason must cite channel close; got stats = {observed_reason}"
    );
    assert_eq!(
        entity_count, 0,
        "pending insert must be rolled back when channel closes"
    );
}
