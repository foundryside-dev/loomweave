//! End-to-end WP1 smoke test — the minimum that must work at WP1 close.
//!
//! Mirrors docs/implementation/sprint-1/README.md §3 demo script for
//! Sprint 1 WP1 scope (no plugin, no entities — those land in WP2 + WP3).

use assert_cmd::Command;
use rusqlite::Connection;

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

#[test]
fn wp1_walking_skeleton_end_to_end() {
    let dir = tempfile::tempdir().unwrap();

    // Scrub PATH on every loomweave invocation. The runner's PATH almost
    // always contains world-writable directories (`/usr/local/bin`,
    // `/opt/pipx_bin`, …) which trip WP2 scrub commit `7c0e396`'s
    // refusal during plugin discovery; an empty PATH guarantees the
    // `skipped_no_plugins` path this test asserts. Same pattern as
    // `tests/analyze.rs::analyze_without_plugins_writes_skipped_run_row`
    // (scrub commit `ad054bd`).

    // Step 1: loomweave install
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();

    let loomweave_dir = dir.path().join(".loomweave");
    assert!(loomweave_dir.join("loomweave.db").exists());
    assert!(loomweave_dir.join("config.json").exists());
    assert!(loomweave_dir.join(".gitignore").exists());
    assert!(dir.path().join("loomweave.yaml").exists());

    // Step 2: loomweave analyze (no plugins yet — WP2 wires them)
    loomweave_bin()
        .args(["analyze"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();

    // Step 3: verify expected shape in the DB.
    let conn = Connection::open(loomweave_dir.join("loomweave.db")).unwrap();

    let migration_version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        migration_version,
        i64::from(loomweave_storage::schema::CURRENT_SCHEMA_VERSION),
        "schema not on the latest migration"
    );

    let runs_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs_count, 1, "expected exactly one run row");

    let run_status: String = conn
        .query_row("SELECT status FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_status, "skipped_no_plugins");

    let entity_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert_eq!(entity_count, 0);

    // WP2+WP3 extend this test to assert a non-zero entity count with the
    // expected 3-segment ID (L2 format `python:function:demo.hello`).
}
