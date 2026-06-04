//! `clarion install` integration tests.

use std::fs;

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    let mut cmd = Command::cargo_bin("clarion").expect("clarion binary");
    cmd.env(
        "CLARION_CODEX_CONFIG",
        std::env::temp_dir().join(format!(
            "clarion-test-codex-config-{}.toml",
            std::process::id()
        )),
    );
    cmd
}

fn read_yaml(path: &std::path::Path) -> serde_json::Value {
    serde_norway::from_str(&fs::read_to_string(path).unwrap()).unwrap()
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
fn install_all_wires_three_way_integration_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let filigree_dir = dir.path().join(".filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8749\n").unwrap();

    clarion_bin()
        .args(["install", "--all", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let clarion_yaml = read_yaml(&dir.path().join("clarion.yaml"));
    assert_eq!(
        clarion_yaml["integrations"]["filigree"]["enabled"],
        serde_json::json!(true)
    );
    assert_eq!(
        clarion_yaml["integrations"]["filigree"]["base_url"],
        "http://127.0.0.1:8749"
    );
    assert_eq!(
        clarion_yaml["serve"]["http"]["enabled"],
        serde_json::json!(true)
    );
    assert_eq!(clarion_yaml["serve"]["http"]["bind"], "127.0.0.1:9111");
    assert_eq!(
        clarion_yaml["serve"]["http"]["wardline_taint_write"],
        serde_json::json!(true)
    );

    let wardline_yaml = read_yaml(&dir.path().join("wardline.yaml"));
    assert_eq!(wardline_yaml["clarion"]["url"], "http://127.0.0.1:9111");
    assert_eq!(
        wardline_yaml["filigree"]["url"],
        "http://127.0.0.1:8749/api/loom/scan-results"
    );

    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["wardline"]["args"],
        serde_json::json!([
            "mcp",
            "--root",
            ".",
            "--clarion-url",
            "http://127.0.0.1:9111",
            "--filigree-url",
            "http://127.0.0.1:8749/api/loom/scan-results"
        ])
    );
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
    assert_eq!(
        count,
        i64::from(clarion_storage::schema::CURRENT_SCHEMA_VERSION)
    );
    let versions: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get(0)).unwrap();
        rows.map(std::result::Result::unwrap).collect()
    };
    let expected: Vec<i64> =
        (1..=i64::from(clarion_storage::schema::CURRENT_SCHEMA_VERSION)).collect();
    assert_eq!(versions, expected);
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
fn install_force_rejects_non_directory_clarion() {
    // The --force overwrite path has its own non-directory guard (distinct from
    // the --all skip-init guard): it can only remove an existing .clarion/
    // *directory*, never a regular file masquerading as one.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".clarion"), "i am a file, not a dir").unwrap();

    let out = clarion_bin()
        .args(["install", "--force", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("can only overwrite an existing .clarion/ directory"),
        "error did not mention the --force non-directory guard: {stderr}"
    );
}

#[test]
fn install_skips_clarion_init_when_dir_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    // Second bare install must succeed: skip .clarion/ init but still apply
    // skills/hooks idempotently and report "already initialised".
    let out = clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("already initialised"),
        "expected 'already initialised' message on second install: {stdout}"
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
    let yaml = read_yaml(&dir.path().join("clarion.yaml"));
    assert_eq!(yaml["custom"], serde_json::json!(true));
    assert_eq!(
        yaml["serve"]["http"]["wardline_taint_write"],
        serde_json::json!(true)
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
fn install_preserves_existing_clarion_yaml_keys_while_wiring_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let yaml_path = dir.path().join("clarion.yaml");
    let user_content = "# user-edited clarion.yaml\nversion: 1\ncustom_key: preserved\n";
    fs::write(&yaml_path, user_content).unwrap();

    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let after = read_yaml(&yaml_path);
    assert_eq!(after["custom_key"], "preserved");
    assert_eq!(
        after["integrations"]["filigree"]["enabled"],
        serde_json::json!(true)
    );
    assert_eq!(
        after["serve"]["http"]["wardline_taint_write"],
        serde_json::json!(true)
    );
}

#[test]
fn install_claude_code_writes_mcp_json_without_initialising_clarion_dir() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--claude-code", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        !dir.path().join(".clarion").exists(),
        "--claude-code should not create .clarion/"
    );
    let raw = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let entry = &parsed["mcpServers"]["clarion"];
    assert_eq!(entry["type"], "stdio");
    assert!(
        entry["command"].as_str().unwrap().ends_with("clarion"),
        "command should point at a clarion executable: {entry:?}"
    );
    assert_eq!(
        entry["args"],
        serde_json::json!(["serve"]),
        "Claude Code MCP should rely on runtime project autodiscovery"
    );
}

#[test]
fn install_codex_writes_requested_config_without_initialising_clarion_dir() {
    let dir = tempfile::tempdir().unwrap();
    let codex_config = dir.path().join("codex-config.toml");

    clarion_bin()
        .args(["install", "--codex", "--codex-config"])
        .arg(&codex_config)
        .args(["--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        !dir.path().join(".clarion").exists(),
        "--codex should not create .clarion/"
    );
    let raw = fs::read_to_string(&codex_config).unwrap();
    assert!(
        raw.contains("[mcp_servers.clarion]"),
        "Codex MCP entry missing: {raw}"
    );
    assert!(
        raw.contains("args = [\"serve\"]"),
        "Codex MCP should rely on runtime project autodiscovery: {raw}"
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
        .env(
            "CLARION_CODEX_CONFIG",
            dir.path().join("isolated-codex-config.toml"),
        )
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
    // Tracing is intentionally written to stderr so MCP stdio stdout stays
    // protocol-clean; this still proves .env was loaded before tracing setup.
    assert!(
        stderr.contains("DEBUG"),
        ".env-supplied RUST_LOG=debug should produce DEBUG-level lines on stderr; \
         stderr was:\n{stderr}"
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
        .env(
            "CLARION_CODEX_CONFIG",
            dir.path().join("isolated-codex-config.toml"),
        )
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
        !stderr.contains("DEBUG"),
        "explicit RUST_LOG=info should beat .env's RUST_LOG=debug; \
         stderr was:\n{stderr}"
    );
}
