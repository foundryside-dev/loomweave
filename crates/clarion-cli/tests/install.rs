//! `clarion install` integration tests.

use std::fs;

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[test]
fn install_creates_clarion_dir_with_expected_contents() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let clarion = dir.path().join(".clarion");
    assert!(clarion.join("clarion.db").exists(), "clarion.db missing");
    assert!(clarion.join("config.json").exists(), "config.json missing");
    assert!(clarion.join(".gitignore").exists(), ".gitignore missing");
    assert!(
        dir.path().join("clarion.yaml").exists(),
        "clarion.yaml not at project root"
    );

    let config = fs::read_to_string(clarion.join("config.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert_eq!(parsed["schema_version"], 1);
    assert!(parsed["last_run_id"].is_null());

    let gitignore = fs::read_to_string(clarion.join(".gitignore")).unwrap();
    for rule in &[
        "*.shadow.db",
        "tmp/",
        "logs/",
        "runs/*/log.jsonl",
        "*-wal",
        "*-shm",
    ] {
        assert!(
            gitignore.contains(rule),
            ".gitignore missing rule {rule}: {gitignore}"
        );
    }
}

#[test]
fn install_applies_migration_0001_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let conn = Connection::open(dir.path().join(".clarion/clarion.db")).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let version: i64 = conn
        .query_row("SELECT version FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 1);
}

#[test]
fn install_refuses_to_overwrite_existing_clarion_dir() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    // Second install must fail with a clear message.
    let out = clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("already exists"),
        "error did not mention existing dir: {stderr}"
    );
    assert!(
        stderr.contains("--force"),
        "error did not mention --force escape hatch: {stderr}"
    );
}

#[test]
fn install_force_returns_unimplemented_in_sprint_one() {
    let dir = tempfile::tempdir().unwrap();
    let out = clarion_bin()
        .args(["install", "--force", "--path"])
        .arg(dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("not implemented in Sprint 1"),
        "expected Sprint 1 --force stub message: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn install_cleans_up_clarion_dir_when_post_mkdir_step_fails() {
    // Bug clarion-ed5017139f: `clarion install` left .clarion/ partially
    // populated on failure, blocking re-install without manual rm -rf.
    //
    // Reproducer: pre-create clarion.yaml as a *broken symlink* whose target
    // sits under a non-existent parent dir. Install's `yaml_path.exists()`
    // check follows symlinks → returns false → install attempts `fs::write`,
    // which follows the symlink → tries to open a path under a non-existent
    // dir → ENOENT. By that point .clarion/ has been mkdir'd and populated;
    // the bug was leaving it on disk.
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let yaml = dir.path().join("clarion.yaml");
    symlink(
        "/clarion-test-nonexistent-by-construction/never/exists/cannot-write",
        &yaml,
    )
    .unwrap();

    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .failure();

    let clarion = dir.path().join(".clarion");
    assert!(
        !clarion.exists(),
        ".clarion/ should have been cleaned up after install failed, \
         but it still exists at {}",
        clarion.display()
    );
}

#[test]
fn install_leaves_existing_clarion_yaml_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let yaml_path = dir.path().join("clarion.yaml");
    let user_content = "# user-edited clarion.yaml\nversion: 1\ncustom_key: preserved\n";
    fs::write(&yaml_path, user_content).unwrap();

    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let after = fs::read_to_string(&yaml_path).unwrap();
    assert_eq!(
        after, user_content,
        "clarion.yaml was overwritten; user content lost"
    );
}
