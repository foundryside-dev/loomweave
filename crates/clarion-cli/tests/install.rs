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
fn install_applies_each_migration_exactly_once() {
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
    assert_eq!(count, 2);
    let versions: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get(0)).unwrap();
        rows.map(std::result::Result::unwrap).collect()
    };
    assert_eq!(versions, vec![1, 2]);
}

#[test]
fn install_all_rejects_non_directory_clarion() {
    // Bug (PR#21 review #6): when `.clarion` already exists as a regular file
    // and `--all` (a non-bare init) is run without `--force`, install treated
    // it as "already initialised" and skipped DB creation, then proceeded to
    // install skills/hooks atop a project with no usable `.clarion/clarion.db`.
    // It must instead refuse with a clear non-directory error.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".clarion"), "i am a file, not a dir").unwrap();

    let out = clarion_bin()
        .args(["install", "--all", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("non-directory"),
        "error did not mention the non-directory .clarion: {stderr}"
    );
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
fn install_force_replaces_existing_clarion_dir_without_overwriting_yaml() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let clarion = dir.path().join(".clarion");
    fs::write(clarion.join("stale.tmp"), "stale").unwrap();
    fs::write(
        dir.path().join("clarion.yaml"),
        "version: 1\ncustom: true\n",
    )
    .unwrap();

    clarion_bin()
        .args(["install", "--force", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        !clarion.join("stale.tmp").exists(),
        "--force should remove stale .clarion/ contents"
    );
    assert!(clarion.join("clarion.db").exists(), "clarion.db missing");
    assert_eq!(
        fs::read_to_string(dir.path().join("clarion.yaml")).unwrap(),
        "version: 1\ncustom: true\n"
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

#[test]
fn dotenv_in_cwd_is_loaded_before_tracing_setup() {
    // Proves the dotenvy hook in `main()` runs before `init_tracing()`: a
    // `.env`-supplied RUST_LOG=debug enables the debug-level log line in
    // `install` (the "clarion.yaml already exists; leaving untouched"
    // branch in install.rs) that the default `info` filter would
    // otherwise suppress. Pre-creating `clarion.yaml` puts us on the
    // branch that emits debug.
    //
    // Uses raw std::process::Command rather than assert_cmd::Command so the
    // child env is exactly what we set — assert_cmd's wrappers were observed
    // to drop the env_remove/env_clear effect on RUST_LOG under nextest,
    // producing an empty stderr regardless of .env content.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".env"), "RUST_LOG=debug\n").unwrap();
    fs::write(dir.path().join("clarion.yaml"), "version: 1\n").unwrap();

    let bin = assert_cmd::cargo::cargo_bin("clarion");
    let path = std::env::var("PATH").unwrap_or_default();
    let out = std::process::Command::new(&bin)
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", path)
        .args(["install", "--path"])
        .arg(dir.path())
        .output()
        .expect("clarion install");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "install failed; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // tracing_subscriber::fmt defaults to stdout, so the DEBUG line lands there.
    assert!(
        stdout.contains("DEBUG"),
        ".env-supplied RUST_LOG=debug should produce DEBUG-level lines on stdout; \
         stdout was:\n{stdout}"
    );
}

#[test]
fn explicit_env_var_wins_over_dotenv() {
    // dotenvy's default semantics: existing process env vars are NOT clobbered
    // by .env entries. An explicit `RUST_LOG=info` in the process env should
    // suppress the debug line even when .env tries to bump verbosity.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".env"), "RUST_LOG=debug\n").unwrap();
    fs::write(dir.path().join("clarion.yaml"), "version: 1\n").unwrap();

    let bin = assert_cmd::cargo::cargo_bin("clarion");
    let path = std::env::var("PATH").unwrap_or_default();
    let out = std::process::Command::new(&bin)
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", path)
        .env("RUST_LOG", "info")
        .args(["install", "--path"])
        .arg(dir.path())
        .output()
        .expect("clarion install");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "install failed; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("DEBUG"),
        "explicit RUST_LOG=info should beat .env's RUST_LOG=debug; \
         stdout was:\n{stdout}"
    );
}
