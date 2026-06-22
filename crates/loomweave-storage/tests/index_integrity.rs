//! Index-integrity detection + surgical repair (clarion-abda98c869 recovery via
//! `loomweave doctor --fix`). Reproduces the file→package refactor corruption
//! (`m.py` becomes `m/__init__.py`, same module qualname) that leaves a stale
//! file entity whose `contains` edge trips `LMWV-INFRA-PARENT-CONTAINS-MISMATCH`.

use std::path::Path;

use loomweave_storage::integrity::{check_integrity, repair_integrity};
use loomweave_storage::{pragma, schema};
use rusqlite::{Connection, params};

/// Build a db seeded with the composer-style corruption against an on-disk tree
/// where only the new package (`.../mod/__init__.py`) exists and the old module
/// file (`.../mod.py`) has vanished.
fn seed(project_root: &Path) -> Connection {
    // On-disk: the new package exists; the old file does not.
    std::fs::create_dir_all(project_root.join("src/app/mod")).unwrap();
    std::fs::write(project_root.join("src/app/mod/__init__.py"), b"").unwrap();

    let db_path = project_root.join("test.db");
    let mut conn = Connection::open(&db_path).unwrap();
    pragma::apply_write_pragmas(&conn).unwrap();
    schema::apply_migrations(&mut conn).unwrap();

    let insert_entity = |conn: &Connection,
                         id: &str,
                         kind: &str,
                         parent: Option<&str>,
                         source_file_id: Option<&str>,
                         source_path: &str| {
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, parent_id, \
                source_file_id, source_file_path, properties, created_at, updated_at) \
             VALUES (?1,?2,?3,?1,?1,?4,?5,?6,'{}','t','t')",
            params![
                id,
                if kind == "file" { "core" } else { "python" },
                kind,
                parent,
                source_file_id,
                source_path
            ],
        )
        .unwrap();
    };
    let insert_contains = |conn: &Connection, from: &str, to: &str| {
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence) VALUES ('contains',?1,?2,'resolved')",
            params![from, to],
        )
        .unwrap();
    };

    let old_file = "core:file:src/app/mod.py";
    let new_file = "core:file:src/app/mod/__init__.py";
    let module = "python:module:app.mod";
    let old_fn = "python:function:app.mod.legacy_helper";

    // Both file entities exist in the cumulative index; only new_file is on disk.
    insert_entity(
        &conn,
        old_file,
        "file",
        None,
        Some(old_file),
        "src/app/mod.py",
    );
    insert_entity(
        &conn,
        new_file,
        "file",
        None,
        Some(new_file),
        "src/app/mod/__init__.py",
    );
    // The module now anchors to the new __init__.py file (parent + source).
    insert_entity(
        &conn,
        module,
        "module",
        Some(new_file),
        Some(new_file),
        "src/app/mod/__init__.py",
    );
    // A stale function still anchored to the vanished old file.
    insert_entity(
        &conn,
        old_fn,
        "function",
        Some(module),
        Some(old_file),
        "src/app/mod.py",
    );

    // Two contains edges into the module — the stale one is the invariant breaker.
    insert_contains(&conn, old_file, module); // STALE (old_file vanished)
    insert_contains(&conn, new_file, module); // valid
    insert_contains(&conn, module, old_fn); // stale child

    conn
}

#[test]
fn detects_stale_file_and_parent_contains_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let conn = seed(dir.path());

    let report = check_integrity(&conn, dir.path()).unwrap();

    assert!(!report.is_healthy(), "corruption must be detected");
    // The vanished old file is the one stale file entity.
    assert_eq!(report.stale_file_entities.len(), 1, "{report:?}");
    assert_eq!(report.stale_file_entities[0].id, "core:file:src/app/mod.py");
    // The stale contains edge trips the parent/contains invariant.
    assert!(
        !report.parent_contains_mismatches.is_empty(),
        "parent/contains mismatch must be detected: {report:?}"
    );
}

#[test]
fn repair_removes_stale_rows_and_restores_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = seed(dir.path());

    let repair = repair_integrity(&mut conn, dir.path()).unwrap();

    assert_eq!(repair.removed_file_entities, 1, "{repair:?}");
    // old file + its stale child function removed.
    assert_eq!(repair.removed_entities_total, 2, "{repair:?}");
    assert!(
        repair.residual.is_healthy(),
        "residual: {:?}",
        repair.residual
    );

    // The surviving module + new file are intact; the stale rows are gone.
    let exists = |id: &str| -> bool {
        conn.query_row("SELECT 1 FROM entities WHERE id = ?1", params![id], |_| {
            Ok(())
        })
        .is_ok()
    };
    assert!(exists("python:module:app.mod"), "module must survive");
    assert!(
        exists("core:file:src/app/mod/__init__.py"),
        "new file must survive"
    );
    assert!(
        !exists("core:file:src/app/mod.py"),
        "stale file must be gone"
    );
    assert!(
        !exists("python:function:app.mod.legacy_helper"),
        "stale fn must be gone"
    );

    // The stale contains edge cascaded away; the valid one remains.
    let contains_into_module: i64 = conn
        .query_row(
            "SELECT count(*) FROM edges WHERE kind='contains' AND to_id='python:module:app.mod'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        contains_into_module, 1,
        "only the valid contains edge survives"
    );

    // Re-checking a now-clean index reports healthy.
    assert!(check_integrity(&conn, dir.path()).unwrap().is_healthy());
}

#[test]
fn repair_nulls_dangling_edge_provenance_into_vanished_file() {
    // The elspeth failure mode: an edge between two SURVIVING entities whose
    // `source_file_id` points at a vanished file. `edges.source_file_id` is a
    // NO-ACTION FK (no cascade), so naive deletion fails the FK check at commit.
    // Repair must null the dangling provenance and keep the edge.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/app")).unwrap();
    std::fs::write(dir.path().join("src/app/live.py"), b"").unwrap();
    let mut conn = Connection::open(dir.path().join("test.db")).unwrap();
    pragma::apply_write_pragmas(&conn).unwrap();
    schema::apply_migrations(&mut conn).unwrap();

    let ent = |conn: &Connection, id: &str, kind: &str, sfi: &str| {
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_id, \
                source_file_path, properties, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?1, ?1, ?4, 'p', '{}', 't', 't')",
            params![
                id,
                if kind == "file" { "core" } else { "python" },
                kind,
                sfi
            ],
        )
        .unwrap();
    };
    let live_file = "core:file:src/app/live.py";
    let gone_file = "core:file:src/app/gone.py";
    ent(&conn, live_file, "file", live_file);
    ent(&conn, gone_file, "file", gone_file);
    // Two surviving functions in the live file…
    ent(&conn, "python:function:app.live.a", "function", live_file);
    ent(&conn, "python:function:app.live.b", "function", live_file);
    // …with a calls edge whose provenance points at the vanished file.
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, source_file_id, confidence) \
         VALUES ('calls','python:function:app.live.a','python:function:app.live.b',?1,'resolved')",
        params![gone_file],
    )
    .unwrap();

    let repair = repair_integrity(&mut conn, dir.path()).unwrap();
    assert_eq!(
        repair.removed_file_entities, 1,
        "only the vanished file is removed"
    );
    assert!(repair.residual.is_healthy(), "{:?}", repair.residual);

    // The edge survives (relationship preserved) with provenance nulled.
    let (cnt, sfi): (i64, Option<String>) = conn
        .query_row(
            "SELECT count(*), max(source_file_id) FROM edges WHERE kind='calls' \
                AND from_id='python:function:app.live.a'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cnt, 1, "the cross-file edge must be preserved");
    assert_eq!(sfi, None, "its dangling provenance must be nulled");
}

#[test]
fn repair_is_a_noop_on_a_healthy_index() {
    let dir = tempfile::tempdir().unwrap();
    // Healthy: a single on-disk file + its module, consistent parent/contains.
    std::fs::create_dir_all(dir.path().join("src/app")).unwrap();
    std::fs::write(dir.path().join("src/app/clean.py"), b"").unwrap();
    let db_path = dir.path().join("test.db");
    let mut conn = Connection::open(&db_path).unwrap();
    pragma::apply_write_pragmas(&conn).unwrap();
    schema::apply_migrations(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_id, \
            source_file_path, properties, created_at, updated_at) \
         VALUES ('core:file:src/app/clean.py','core','file','f','f','core:file:src/app/clean.py',\
            'src/app/clean.py','{}','t','t')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, parent_id, source_file_id, \
            source_file_path, properties, created_at, updated_at) \
         VALUES ('python:module:app.clean','python','module','m','m','core:file:src/app/clean.py',\
            'core:file:src/app/clean.py','src/app/clean.py','{}','t','t')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence) \
         VALUES ('contains','core:file:src/app/clean.py','python:module:app.clean','resolved')",
        [],
    )
    .unwrap();

    assert!(check_integrity(&conn, dir.path()).unwrap().is_healthy());
    let repair = repair_integrity(&mut conn, dir.path()).unwrap();
    assert_eq!(repair.removed_file_entities, 0);
    assert_eq!(repair.removed_entities_total, 0);
    assert!(repair.residual.is_healthy());
}
