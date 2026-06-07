//! `loomweave db backup` integration tests (clarion-6d433b61ba / STO-04).

use assert_cmd::Command;
use rusqlite::{Connection, OpenFlags};

fn loomweave_bin() -> Command {
    let mut cmd = Command::cargo_bin("loomweave").expect("loomweave binary");
    cmd.env(
        "LOOMWEAVE_CODEX_CONFIG",
        std::env::temp_dir().join(format!(
            "loomweave-test-codex-config-{}.toml",
            std::process::id()
        )),
    );
    cmd
}

/// Seed a real `.weft/loomweave/loomweave.db` under `root` with one identifiable row,
/// left in WAL mode (the state a live analyze leaves behind).
fn seed_db(root: &std::path::Path) {
    let loomweave_dir = root.join(".weft/loomweave");
    std::fs::create_dir_all(&loomweave_dir).expect("mkdir .loomweave");
    let db_path = loomweave_dir.join("loomweave.db");
    let mut conn = Connection::open(&db_path).expect("open db");
    loomweave_storage::pragma::apply_write_pragmas(&conn).expect("write pragmas");
    loomweave_storage::schema::apply_migrations(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), NULL, '{}', '{}', 'running')",
        rusqlite::params!["run-backup-test"],
    )
    .expect("seed runs row");
}

/// A backup of a seeded DB is a standalone, single-file copy that opens
/// read-only and contains the source rows (no WAL sidecar required).
#[test]
fn backup_produces_a_readable_standalone_copy() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    let output = dir.path().join("snapshot.db");

    loomweave_bin()
        .args(["db", "backup"])
        .arg(&output)
        .arg("--path")
        .arg(dir.path())
        .assert()
        .success();

    assert!(output.exists(), "backup file was not created");
    // No WAL sidecar should be shipped alongside the standalone copy.
    assert!(
        !output.with_extension("db-wal").exists(),
        "backup should be a single-file copy with no -wal sidecar"
    );

    // Open the copy read-only and confirm the seeded row survived.
    let copy = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open backup read-only");
    let status: String = copy
        .query_row(
            "SELECT status FROM runs WHERE id = 'run-backup-test'",
            [],
            |row| row.get(0),
        )
        .expect("seeded row present in backup");
    assert_eq!(status, "running");
    drop(copy);

    // integrity_check on the FTS5 tables needs to write scratch state, so it
    // must run on a read-write handle (the backup command runs the same check
    // internally before promoting the file).
    let rw = Connection::open(&output).expect("open backup read-write");
    let integrity: String = rw
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity_check");
    assert_eq!(integrity, "ok");
}

/// An existing output is protected: refused without --force, overwritten with.
#[test]
fn backup_refuses_to_clobber_without_force() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    let output = dir.path().join("snapshot.db");
    std::fs::write(&output, b"pre-existing precious file").unwrap();

    loomweave_bin()
        .args(["db", "backup"])
        .arg(&output)
        .arg("--path")
        .arg(dir.path())
        .assert()
        .failure();

    // The pre-existing file must be untouched by the refused run.
    assert_eq!(
        std::fs::read(&output).unwrap(),
        b"pre-existing precious file"
    );

    // --force replaces it with a real backup.
    loomweave_bin()
        .args(["db", "backup"])
        .arg(&output)
        .arg("--path")
        .arg(dir.path())
        .arg("--force")
        .assert()
        .success();
    let copy = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open overwritten backup");
    let n: i64 = copy
        .query_row("SELECT count(*) FROM runs", [], |row| row.get(0))
        .expect("count rows");
    assert_eq!(n, 1);
}

/// A missing source database is rejected with a clear error and leaves no
/// debris (no output file, no staging temp).
#[test]
fn backup_rejects_missing_source_db() {
    let dir = tempfile::tempdir().unwrap();
    // No seed_db: .weft/loomweave/loomweave.db does not exist.
    let output = dir.path().join("snapshot.db");

    loomweave_bin()
        .args(["db", "backup"])
        .arg(&output)
        .arg("--path")
        .arg(dir.path())
        .assert()
        .failure();

    assert!(!output.exists(), "no output should be written on failure");
}
