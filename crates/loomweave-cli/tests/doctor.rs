//! `loomweave doctor [--fix]` integration tests.
//!
//! Exercises the exit-code contract (healthy -> 0, any problem -> 1) and the
//! end-to-end `--fix` wiring across the three orientation surfaces. Per-surface
//! detection/merge correctness is unit-tested in the owning modules
//! (`skill_pack`, `hooks_settings`, `mcp_registration`).

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use rusqlite::Connection;
#[cfg(unix)]
use tempfile::TempDir;

fn loomweave_bin() -> Command {
    let mut cmd = Command::cargo_bin("loomweave").expect("loomweave binary");
    cmd.env_remove("WEFT_TOKEN");
    cmd.env_remove("WEFT_IDENTITY_SECRET");
    cmd.env(
        "LOOMWEAVE_CODEX_CONFIG",
        std::env::temp_dir().join(format!(
            "loomweave-test-codex-config-{}.toml",
            std::process::id()
        )),
    );
    cmd
}

fn install(args: &[&str], dir: &Path) {
    loomweave_bin()
        .args(args)
        .arg("--path")
        .arg(dir)
        .assert()
        .success();
}

fn read_yaml(path: &Path) -> serde_json::Value {
    serde_norway::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Materialise a minimal healthy `SQLite` DB at the canonical store path so
/// `check_loomweave_dir` reports healthy (not the absent warning). The DB is
/// **migrated** (`apply_migrations` stamps `PRAGMA user_version`), because the
/// doctor health classifier mirrors the read-open path and refuses an
/// *unmigrated* (`user_version = 0`) file — a freshly-opened `SQLite` file with no
/// schema is not a Loomweave index, and `serve` would reject it (review #8).
///
/// A real `.weft/loomweave/` (created by `install`) also carries the current
/// `.gitignore`, so this completes the store with one too — otherwise the
/// gitignore-drift check (`gitignore.current`) would add a spurious "missing"
/// warning to tests that build the store dir by hand. The canonical bytes are
/// generated from a throwaway real install rather than duplicated here (which
/// would itself drift — the exact failure the new check guards against).
fn write_healthy_db(root: &Path) {
    let store = root.join(".weft/loomweave");
    fs::create_dir_all(&store).unwrap();
    {
        let mut conn = Connection::open(store.join("loomweave.db")).expect("create SQLite DB");
        loomweave_storage::pragma::apply_write_pragmas(&conn).expect("write pragmas");
        loomweave_storage::schema::apply_migrations(&mut conn).expect("migrate");
    }

    let scratch = tempfile::tempdir().unwrap();
    install(&["install", "--all"], scratch.path());
    let canonical = fs::read(scratch.path().join(".weft/loomweave/.gitignore"))
        .expect("install writes a canonical .gitignore");
    fs::write(store.join(".gitignore"), canonical).unwrap();
}

/// A do-nothing language plugin: it completes the handshake and is never asked
/// to analyze anything (the temp projects hold no `.lwdoc` files). Its only job
/// is to make plugin discovery — and therefore `plugin.availability` and the
/// classifier checks — DETERMINISTIC.
///
/// Without it a doctor test that asserts full health silently depends on whether
/// the machine running it happens to have a language plugin installed globally:
/// green on a developer box, red on a CI runner. That environment coupling is
/// what pinned CI red (clarion-40132c951e).
#[cfg(unix)]
const DOCTOR_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/env python3
import json
import sys


def read_frame():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if line in (b"", b"\r\n"):
            break
        name, value = line.decode("ascii").strip().split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))


def write_frame(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n")
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


while True:
    msg = read_frame()
    method = msg.get("method")
    if method == "initialized":
        continue
    if method == "exit":
        raise SystemExit(0)
    ident = msg["id"]
    if method == "initialize":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {
            "name": "loomweave-plugin-lwdoc",
            "version": "0.1.0",
            "ontology_version": "0.1.0",
            "capabilities": {},
        }})
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

#[cfg(unix)]
const DOCTOR_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "loomweave-plugin-lwdoc"
plugin_id = "lwdoc"
version = "0.1.0"
protocol_version = "1.0"
executable = "loomweave-plugin-lwdoc"
language = "lwdoc"
extensions = ["lwdoc"]

[capabilities.runtime]
expected_max_rss_mb = 64
expected_entities_per_file = 10
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "LMWV-DOC-"
ontology_version = "0.1.0"
classifier_tags = ["entry-point"]

[ontology.roles]
file_scope = ["module"]
"#;

/// Materialise the fixture plugin in a fresh dir and return it, plus the `PATH`
/// value that makes it (and nothing else) discoverable.
#[cfg(unix)]
fn discoverable_plugin_dir() -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("plugin dir");
    let script = dir.path().join("loomweave-plugin-lwdoc");
    fs::write(&script, DOCTOR_PLUGIN_SCRIPT).expect("write plugin script");
    let mut perms = fs::metadata(&script).expect("stat plugin").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod plugin");
    fs::write(dir.path().join("plugin.toml"), DOCTOR_PLUGIN_MANIFEST).expect("write manifest");

    let path = format!("{}:/usr/bin:/bin", dir.path().display());
    (dir, path)
}

/// Run `doctor` (optionally with `--fix`) and return `(exit_code, stdout)`.
fn doctor(dir: &Path, fix: bool) -> (i32, String) {
    doctor_with_env(dir, fix, &[], &[])
}

fn doctor_with_env(
    dir: &Path,
    fix: bool,
    env: &[(&str, &str)],
    env_remove: &[&str],
) -> (i32, String) {
    let mut cmd = loomweave_bin();
    for (name, value) in env {
        cmd.env(name, value);
    }
    for name in env_remove {
        cmd.env_remove(name);
    }
    cmd.arg("doctor");
    if fix {
        cmd.arg("--fix");
    }
    let output = cmd.arg("--path").arg(dir).output().expect("run doctor");
    (
        output.status.code().expect("exit code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn doctor_json(dir: &Path, fix: bool) -> (i32, serde_json::Value) {
    doctor_json_with_env(dir, fix, &[], &[])
}

fn doctor_json_with_env(
    dir: &Path,
    fix: bool,
    env: &[(&str, &str)],
    env_remove: &[&str],
) -> (i32, serde_json::Value) {
    let mut cmd = loomweave_bin();
    for (name, value) in env {
        cmd.env(name, value);
    }
    for name in env_remove {
        cmd.env_remove(name);
    }
    cmd.arg("doctor");
    if fix {
        cmd.arg("--fix");
    }
    let output = cmd
        .args(["--format", "json"])
        .arg("--path")
        .arg(dir)
        .output()
        .expect("run doctor json");
    let stdout = String::from_utf8_lossy(&output.stdout);
    (
        output.status.code().expect("exit code"),
        serde_json::from_str(&stdout).unwrap_or_else(|err| {
            panic!("doctor --format json must emit parseable JSON: {err}\nstdout:\n{stdout}")
        }),
    )
}

fn check<'a>(json: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    json["checks"]
        .as_array()
        .expect("doctor checks array")
        .iter()
        .find(|candidate| candidate["id"] == id)
        .unwrap_or_else(|| panic!("doctor check {id:?} missing from {json}"))
}

#[cfg(unix)]
fn fixture_binary_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_loomweave-fixture-plugin") {
        return PathBuf::from(path);
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let target_dir =
        env::var("CARGO_TARGET_DIR").map_or_else(|_| workspace_root.join("target"), PathBuf::from);
    for profile in ["debug", "release"] {
        let candidate = target_dir.join(profile).join("loomweave-fixture-plugin");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!(
        "loomweave-fixture-plugin binary not found under {}",
        target_dir.display()
    );
}

#[cfg(unix)]
fn setup_classifier_plugin_dir() -> TempDir {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = TempDir::new().expect("plugin tempdir");
    std::os::unix::fs::symlink(
        &fixture_bin,
        plugin_dir.path().join("loomweave-plugin-fixture"),
    )
    .expect("symlink fixture plugin");
    assert_ne!(
        fs::metadata(&fixture_bin).unwrap().permissions().mode() & 0o111,
        0,
        "fixture plugin must be executable"
    );

    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("loomweave-core/tests/fixtures/plugin.toml");
    let manifest = fs::read_to_string(manifest_path).unwrap().replace(
        "ontology_version = \"0.1.0\"",
        "ontology_version = \"0.1.0\"\nclassifier_tags = [\"http-route\"]",
    );
    fs::write(plugin_dir.path().join("plugin.toml"), manifest).unwrap();
    plugin_dir
}

fn insert_run_stats(root: &Path, id: &str, run_status: &str, stats_json: &serde_json::Value) {
    insert_run_stats_raw(root, id, run_status, &stats_json.to_string());
}

fn insert_run_stats_raw(root: &Path, id: &str, run_status: &str, stats_json: &str) {
    let db = root.join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(db).expect("open test catalogue");
    let completed_at = (run_status != "running").then_some("2026-07-12T02:00:00Z");
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES (?1, '2026-07-12T01:00:00Z', ?2, '{}', ?3, ?4)",
        rusqlite::params![id, completed_at, stats_json, run_status],
    )
    .expect("insert analysis run");
}

fn classifier_coverage(
    source_walk_complete: bool,
    source_walk_skipped_entries: u64,
    plugins: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "classifier_coverage": {
            "schema": "loomweave.classifier-coverage.v1",
            "source_walk_complete": source_walk_complete,
            "source_walk_skipped_entries": source_walk_skipped_entries,
            "plugin_discovery_complete": true,
            "plugin_discovery_errors": 0,
            "plugin_discovery_error_samples": [],
            "plugins": plugins,
        }
    })
}

fn plugin_coverage(
    id: &str,
    status: &str,
    matched: u64,
    analyzed: u64,
    degraded: u64,
    tags: &[&str],
) -> serde_json::Value {
    serde_json::json!({
        "plugin_id": id,
        "plugin_version": "1.0.0",
        "ontology_version": "1.0.0",
        "matched_files": matched,
        "analyzed_files": analyzed,
        "retained_files": 0,
        "degraded_files": degraded,
        "status": status,
        "classifier_tags": tags,
    })
}

fn spawn_one_shot_health_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind health server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept health probe");
        let mut buf = [0_u8; 512];
        let _ = stream.read(&mut buf);
        let body = r#"{"ok":true}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write health response");
    });
    (port, handle)
}

/// Like [`spawn_one_shot_health_server`] but answers 200 ONLY on
/// `expected_path`, 404 otherwise — the real read API's contract.
///
/// The path-agnostic server above answers any request, so it cannot tell a
/// correct probe from one aimed at a route that does not exist. That blind spot
/// is why the liveness probe shipped pointed at `/health`, which the read API
/// never registered. Returns the request line the probe actually sent.
fn spawn_one_shot_routed_server(
    expected_path: &'static str,
) -> (u16, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind routed server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept probe");
        let mut buf = [0_u8; 512];
        let read = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..read]).into_owned();
        let request_line = request.lines().next().unwrap_or_default().to_owned();
        let hit = request_line
            .split_whitespace()
            .nth(1)
            .is_some_and(|path| path == expected_path);
        if hit {
            let body = r#"{"ok":true}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
        } else {
            let _ = write!(
                stream,
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n"
            );
        }
        request_line
    });
    (port, handle)
}

/// A freshly `install --all`ed project has every orientation surface, including
/// Claude Code MCP, so `doctor` must report it healthy.
#[test]
fn doctor_reports_plain_install_healthy() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(code, 0, "plain install should be healthy; stdout:\n{out}");
    assert!(out.contains("skill pack up to date"), "stdout:\n{out}");
    assert!(out.contains("SessionStart hook present"), "stdout:\n{out}");
    assert!(
        out.contains(".mcp.json loomweave serve entry present"),
        "stdout:\n{out}"
    );
}

/// Classifier evidence is produced by LANGUAGE PLUGINS. With none installed
/// there is nothing to classify and nothing `--fix` can do about it: running
/// `analyze` yields `skipped_no_plugins`, so reporting the classifier checks as
/// repairable `problem`s makes `doctor --fix` exit non-zero forever on any
/// plugin-less machine — which is every CI runner that does not install a
/// language plugin, and every fresh install before the operator adds one.
///
/// `plugin.availability` already warns about the missing plugin; the classifier
/// checks must degrade to the same warning rather than claim a failed repair.
#[test]
fn doctor_fix_exits_zero_when_no_language_plugin_is_installed() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--skills", "--hooks"], dir.path());
    write_healthy_db(dir.path());

    // A PATH with no `loomweave-plugin-*` on it — the CI runner's shape.
    let (code, doc) = doctor_json_with_env(dir.path(), true, &[("PATH", "/usr/bin:/bin")], &[]);

    let status = |id: &str| -> String {
        doc["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("check {id} missing from {doc:#}"))["status"]
            .as_str()
            .expect("status string")
            .to_owned()
    };

    assert_eq!(
        code, 0,
        "a plugin-less project has nothing to classify, so --fix must not report an \
         unrepairable problem; doctor:\n{doc:#}"
    );
    // Not applicable, not broken — and the missing plugin is still surfaced once,
    // by the check that owns it.
    assert_eq!(status("classifier.enumeration"), "warning");
    assert_eq!(status("classifier.tags"), "warning");
    assert_eq!(status("plugin.availability"), "warning");
}

/// `doctor --fix` registers the MCP entry; a subsequent plain `doctor` is then
/// fully healthy and exits 0. The `.mcp.json` gains a `loomweave` serve entry.
#[cfg(unix)]
#[test]
fn doctor_fix_registers_mcp_then_reports_healthy() {
    let dir = tempfile::tempdir().unwrap();
    install(
        &["install", "--skills", "--codex-skills", "--hooks"],
        dir.path(),
    );
    // Materialise a healthy DB so the index health check reports ok rather than
    // the absent-DB warning, which would prevent "All orientation surfaces healthy."
    write_healthy_db(dir.path());
    // ...and pin plugin discovery, for the same reason: a machine with no
    // language plugin warns on `plugin.availability` + the classifier checks,
    // which also prevents the healthy summary line. Asserting FULL health means
    // owning every input to it, not inheriting the developer's global installs.
    let (_plugin_dir, path) = discoverable_plugin_dir();

    let (code, out) = doctor_with_env(dir.path(), true, &[("PATH", path.as_str())], &[]);
    assert_eq!(code, 0, "--fix should repair and exit 0; stdout:\n{out}");
    assert!(
        out.contains("All orientation surfaces healthy."),
        "stdout:\n{out}"
    );

    // The entry is now on disk and uses runtime project autodiscovery.
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert!(
        v["mcpServers"]["loomweave"]["command"]
            .as_str()
            .unwrap()
            .ends_with("loomweave")
    );
    assert_eq!(
        v["mcpServers"]["loomweave"]["args"],
        serde_json::json!(["serve"])
    );

    // A plain re-run is now clean.
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "a repaired project must be healthy on re-run");
}

/// `doctor --fix` preserves a sibling MCP server (e.g. filigree) already in
/// `.mcp.json` while adding the loomweave entry.
#[test]
fn doctor_fix_preserves_sibling_mcp_server() {
    let dir = tempfile::tempdir().unwrap();
    install(
        &["install", "--skills", "--codex-skills", "--hooks"],
        dir.path(),
    );
    fs::write(
        dir.path().join(".mcp.json"),
        r#"{"mcpServers":{"filigree":{"type":"stdio","command":"/opt/filigree-mcp","args":[]}}}"#,
    )
    .unwrap();

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "stdout:\n{out}");

    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        v["mcpServers"]["filigree"]["command"], "/opt/filigree-mcp",
        "sibling server must be preserved"
    );
    assert!(
        v["mcpServers"]["loomweave"]["command"]
            .as_str()
            .unwrap()
            .ends_with("loomweave")
    );
}

#[test]
fn doctor_fix_repairs_missing_three_way_integration_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let filigree_dir = dir.path().join(".weft").join("filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8749\n").unwrap();

    install(
        &[
            "install",
            "--skills",
            "--codex-skills",
            "--hooks",
            "--claude-code",
            "--instructions",
        ],
        dir.path(),
    );
    // Materialise a healthy DB so the index health check reports ok rather than
    // the absent-DB warning. A never-analysed DB and its missing identity are
    // now reported independently from the integration binding warning.
    write_healthy_db(dir.path());

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 0,
        "missing enrich-only integration bindings must NOT fail the gate (federation axiom: \
         Wardline is enrich-only, a Loomweave-solo/Filigree-only project is first-class):\n{out}"
    );
    assert!(
        out.contains("⚠ three-way integration bindings missing or stale"),
        "missing bindings should surface as a warning, not a problem:\n{out}"
    );
    assert!(
        out.contains("warnings; no problems"),
        "summary should report warnings without claiming a problem:\n{out}"
    );

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "--fix should repair and exit 0; stdout:\n{out}");
    assert!(
        out.contains("three-way integration bindings missing or stale — fixed"),
        "stdout:\n{out}"
    );

    let loomweave_yaml = read_yaml(&dir.path().join("loomweave.yaml"));
    assert_eq!(
        loomweave_yaml["integrations"]["filigree"]["base_url"],
        "http://127.0.0.1:8749"
    );
    assert_eq!(
        loomweave_yaml["serve"]["http"]["wardline_taint_write"],
        serde_json::json!(true)
    );

    // Wardline reads no URL from any `wardline.yaml`, so --fix writes none.
    assert!(
        !dir.path().join("wardline.yaml").exists(),
        "doctor --fix must not write a dead wardline.yaml that Wardline never reads"
    );

    let expected_port = loomweave_federation::loomweave_port::deterministic_port(
        &dir.path().canonicalize().unwrap(),
    );
    let expected_loomweave_url = format!("http://127.0.0.1:{expected_port}");

    // Loomweave owns only its OWN `--loomweave-url`; it cedes the emit URL
    // (`--filigree-url`) to wardline's installer (weft emit incident 2026-06-10).
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["wardline"]["args"],
        serde_json::json!([
            "mcp",
            "--root",
            ".",
            "--loomweave-url",
            expected_loomweave_url
        ])
    );

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(code, 0, "repaired project should be healthy:\n{out}");
}

#[test]
fn doctor_json_reports_stable_check_shape_for_healthy_install() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "healthy install should exit 0: {json}");
    assert_eq!(json["ok"], true);
    assert!(json["checks"].as_array().unwrap().iter().any(|check| {
        check["id"] == "mcp.registration"
            && check["status"] == "ok"
            && check["fixed"] == serde_json::json!(false)
    }));
    let legacy_shape = check(&json, "mcp.registration")
        .as_object()
        .expect("legacy doctor check object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        legacy_shape,
        ["fixed", "id", "message", "status"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "machine-readable details must be additive only on new checks"
    );
    assert!(json["checks"].as_array().unwrap().iter().any(|check| {
        check["id"] == "integration.bindings"
            && check["status"] == "ok"
            && check["fixed"] == serde_json::json!(false)
    }));
    assert!(
        json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| { check["id"] == "index.freshness" && check["status"].is_string() })
    );
    assert!(
        json["next_actions"].is_array(),
        "next_actions must always be an array: {json}"
    );
}

#[test]
fn doctor_fix_json_reports_fixed_config_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let filigree_dir = dir.path().join(".weft").join("filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8749\n").unwrap();
    install(
        &[
            "install",
            "--skills",
            "--codex-skills",
            "--hooks",
            "--claude-code",
        ],
        dir.path(),
    );

    let (code, json) = doctor_json(dir.path(), true);
    assert_eq!(code, 0, "--fix json should repair and exit 0: {json}");
    assert_eq!(json["ok"], true);
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "integration.bindings")
        .expect("integration.bindings check");
    assert_eq!(check["status"], "fixed");
    assert_eq!(check["fixed"], serde_json::json!(true));

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "repaired project should be healthy: {json}");
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "integration.bindings")
        .expect("integration.bindings check");
    assert_eq!(check["status"], "ok");
    assert_eq!(check["fixed"], serde_json::json!(false));
}

/// With only the skill installed (no hook, no mcp, no integration bindings),
/// `doctor` exits 1 on the genuine problems (missing hook + mcp) while the
/// enrich-only integration bindings surface only as a warning; the index
/// snapshot block is still printed.
#[test]
fn doctor_reports_missing_hook_and_mcp_and_prints_index_block() {
    let dir = tempfile::tempdir().unwrap();
    // Skill flags install ONLY the skill packs (no .weft/loomweave/, no hook, no mcp).
    install(&["install", "--skills", "--codex-skills"], dir.path());

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(code, 1, "stdout:\n{out}");
    assert!(out.contains("SessionStart hook missing"), "stdout:\n{out}");
    assert!(
        out.contains(".mcp.json has no loomweave serve entry"),
        "stdout:\n{out}"
    );
    assert!(
        out.contains("⚠ three-way integration bindings missing or stale"),
        "enrich-only bindings should be a warning, not a problem:\n{out}"
    );
    assert!(out.contains("--- index ---"), "stdout:\n{out}");
    // Only the hook and mcp surfaces are genuine problems; bindings is a warning.
    assert!(out.contains("2 problems found"), "stdout:\n{out}");
}

/// A hostile checkout can ship a `.mcp.json` whose `loomweave` entry names an
/// attacker-controlled `command` that the MCP client would later launch.
/// `doctor` must NOT report that as healthy (the false all-clear bug), but it
/// also must not clobber a possibly-deliberate wrapper: it flags the entry
/// (exit 1) and, under `--fix`, repairs args while leaving the command in
/// place as an advisory warning (exit 0) for the operator to adjudicate.
#[test]
fn doctor_flags_untrusted_mcp_command_without_clobbering_it() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    let canon = dir.path().canonicalize().unwrap().display().to_string();
    fs::write(
        dir.path().join(".mcp.json"),
        format!(
            r#"{{"mcpServers":{{"loomweave":{{"type":"stdio","command":"./evil-mcp.sh","args":["serve","--path",{canon:?}],"env":{{}}}}}}}}"#
        ),
    )
    .unwrap();

    // No --fix: the poisoned command must fail the gate, not pass as healthy.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "untrusted command must fail the gate; stdout:\n{out}"
    );
    assert!(
        out.contains("unrecognized command") && out.contains("evil-mcp.sh"),
        "doctor must name the unrecognized command; stdout:\n{out}"
    );

    // --fix: advisory (exit 0) but the attacker command is left untouched on
    // disk — never clobbered, never silently trusted.
    let (code, out) = doctor(dir.path(), true);
    assert_eq!(
        code, 0,
        "--fix downgrades to advisory warning; stdout:\n{out}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        v["mcpServers"]["loomweave"]["command"], "./evil-mcp.sh",
        "doctor --fix must not clobber a custom command"
    );

    // The JSON surface agrees: a warning (not ok, not a silent pass to Present).
    let (_code, report) = doctor_json(dir.path(), false);
    let mcp = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "mcp.registration")
        .expect("mcp.registration check present");
    assert_eq!(mcp["status"], "problem", "report: {report}");
    assert_eq!(
        report["ok"], false,
        "an untrusted command makes the run not ok"
    );
}

/// Instructions severity model (plan decision #2, the product-judgment veto
/// point): `Missing` is a non-gating **warning** — the same guidance ships via
/// the MCP preamble and the loomweave-workflow skill, so a project that omits
/// the always-loaded block is still first-class. A fresh `--all` install holds
/// the block; deleting it from one target file drives the aggregate to Missing,
/// which must surface as a warning and still exit 0.
#[test]
fn doctor_reports_missing_instructions_block_as_warning() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Drop the Loomweave block from one target file -> aggregate is Missing.
    fs::write(dir.path().join("AGENTS.md"), "# just notes\n").unwrap();

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 0,
        "a missing instructions block is an optional surface; must NOT fail the gate:\n{out}"
    );
    assert!(
        out.contains("⚠ agent-orientation block missing from CLAUDE.md / AGENTS.md"),
        "missing block should surface as a warning:\n{out}"
    );

    // --fix re-injects the block; a plain re-run is then clean.
    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "--fix should repair and exit 0:\n{out}");
    assert!(
        out.contains("agent-orientation block missing from CLAUDE.md / AGENTS.md — fixed"),
        "stdout:\n{out}"
    );
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "repaired project must be healthy on re-run");
}

/// `Drifted` -> **problem**: a stale block body fails the gate without `--fix`
/// and is auto-repaired with `--fix`. This pins the one branch that actually
/// gates the doctor exit code; a refactor flipping Drifted to a warning would
/// otherwise pass the suite undetected.
#[test]
fn doctor_reports_drifted_instructions_block_as_gating_problem() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Hand-edit the body inside the Loomweave span -> Drifted.
    let claude = dir.path().join("CLAUDE.md");
    let content = fs::read_to_string(&claude).unwrap();
    let drifted = content.replace("code archaeology", "DRIFTED HEADER");
    assert_ne!(drifted, content, "test setup: substitution must apply");
    fs::write(&claude, &drifted).unwrap();

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a drifted instructions block must FAIL the doctor gate without --fix:\n{out}"
    );
    assert!(
        out.contains("agent-orientation block drifted from the bundled copy"),
        "stdout:\n{out}"
    );

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "--fix should repair drift and exit 0:\n{out}");
    assert!(
        out.contains("agent-orientation block drifted from the bundled copy — fixed"),
        "stdout:\n{out}"
    );
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "repaired project must be healthy on re-run");
}

/// `Malformed` -> **problem**: a dangling Loomweave start marker (no following
/// end marker) fails the gate without `--fix`, and `--fix` repairs it without
/// truncating to EOF.
#[test]
fn doctor_reports_malformed_instructions_block_as_gating_problem() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Replace one target file's block with a dangling start marker.
    fs::write(
        dir.path().join("CLAUDE.md"),
        "# notes\n<!-- loomweave:instructions:v0:deadbeef -->\norphan body, no end marker\n",
    )
    .unwrap();

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a malformed instructions block must FAIL the doctor gate without --fix:\n{out}"
    );
    assert!(
        out.contains("agent-orientation block malformed (dangling loomweave marker)"),
        "stdout:\n{out}"
    );

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(
        code, 0,
        "--fix should repair the malformed block and exit 0:\n{out}"
    );
    let fixed = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
    assert!(
        fixed.contains("# notes"),
        "leading content must survive the repair:\n{fixed}"
    );
    assert!(
        fixed.contains("orphan body, no end marker"),
        "orphaned body must survive as loose prose:\n{fixed}"
    );
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "repaired project must be healthy on re-run");
}

/// JSON surface: pin the `instructions.block` check shape. Healthy install ->
/// status `ok`, `fixed: false`; a drifted block -> status `problem` and the run
/// aggregates to `ok: false`. The healthy-install json shape test omits this
/// check, leaving the status string and `fixed` flag unverified.
#[test]
fn doctor_json_reports_instructions_block_check_shape() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());

    // Healthy: instructions.block is ok, not fixed.
    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "healthy install should exit 0: {json}");
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "instructions.block")
        .expect("instructions.block check present");
    assert_eq!(check["status"], "ok");
    assert_eq!(check["fixed"], serde_json::json!(false));

    // Drift the block -> the json check becomes a problem and ok aggregates to false.
    let claude = dir.path().join("CLAUDE.md");
    let content = fs::read_to_string(&claude).unwrap();
    fs::write(
        &claude,
        content.replace("code archaeology", "DRIFTED HEADER"),
    )
    .unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 1, "a drifted block must fail the json gate: {json}");
    assert_eq!(
        json["ok"], false,
        "an instructions-driven problem must make the run not ok: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "instructions.block")
        .expect("instructions.block check present");
    assert_eq!(check["status"], "problem");

    // --fix repairs it: status becomes fixed.
    let (code, json) = doctor_json(dir.path(), true);
    assert_eq!(code, 0, "--fix json should repair and exit 0: {json}");
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "instructions.block")
        .expect("instructions.block check present");
    assert_eq!(check["status"], "fixed");
    assert_eq!(check["fixed"], serde_json::json!(true));
}

#[test]
fn doctor_reports_published_ephemeral_port() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    let (port, handle) = spawn_one_shot_health_server();
    let loomweave_dir = dir.path().join(".weft/loomweave");
    std::fs::create_dir_all(&loomweave_dir).unwrap();
    std::fs::write(loomweave_dir.join("ephemeral.port"), format!("{port}\n")).unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    handle.join().expect("health server joins");
    assert_eq!(code, 0, "{json}");
    let http = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "http.config")
        .expect("http.config check present");
    assert_eq!(http["status"], "ok");
    assert!(
        http["message"]
            .as_str()
            .unwrap_or("")
            .contains(&port.to_string()),
        "http.config should report the published live port: {http}"
    );
}

/// The liveness probe must aim at a route the read API actually serves.
///
/// `/api/v1/_capabilities` is the one deliberately unauthenticated route
/// (siblings probe it pre-auth), which makes it the only sound liveness target:
/// every other route can legitimately answer 401/403 on a perfectly healthy
/// server. `/health` was never registered at all, so doctor reported a LIVE
/// server as "stale port metadata … not reachable" for every operator with the
/// HTTP read API enabled (clarion-7ad374bac4).
#[test]
fn doctor_probes_a_route_the_read_api_actually_serves() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    let (port, handle) = spawn_one_shot_routed_server("/api/v1/_capabilities");
    let loomweave_dir = dir.path().join(".weft/loomweave");
    std::fs::create_dir_all(&loomweave_dir).unwrap();
    std::fs::write(loomweave_dir.join("ephemeral.port"), format!("{port}\n")).unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    let request_line = handle.join().expect("routed server joins");
    assert_eq!(code, 0, "{json}");
    let http = check(&json, "http.config");
    assert_eq!(
        http["status"], "ok",
        "a live server must not be reported unreachable; probe sent {request_line:?}: {http}"
    );
}

#[test]
fn doctor_warns_when_published_ephemeral_port_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve unused port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    let loomweave_dir = dir.path().join(".weft/loomweave");
    std::fs::create_dir_all(&loomweave_dir).unwrap();
    std::fs::write(loomweave_dir.join("ephemeral.port"), format!("{port}\n")).unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(
        code, 0,
        "stale HTTP metadata is advisory, not a gate failure: {json}"
    );
    let http = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "http.config")
        .expect("http.config check present");
    assert_eq!(http["status"], "warning", "{http}");
    let message = http["message"].as_str().unwrap_or("");
    assert!(
        message.contains("stale HTTP read-API port metadata"),
        "{http}"
    );
    // The message names the route that was actually probed, so an operator can
    // reproduce the check by hand. That must stay in step with the probe itself.
    assert!(
        message.contains(&format!("127.0.0.1:{port}/api/v1/_capabilities")),
        "{http}"
    );
    assert!(
        message.contains(".mcp.json launches the stdio runtime"),
        "{http}"
    );
}

// ---------------------------------------------------------------------------
// Index DB health check tests (.weft/loomweave.schema)
// ---------------------------------------------------------------------------

/// (a) Absent DB → `.weft/loomweave.schema` is a warning (ok=true), gate passes.
///
/// A missing DB is a legitimate intermediate state (install-before-analyze), so
/// it must not fail the gate. The JSON path must set `ok: true`, and the text
/// path must exit 0 (warnings only, no problems).
#[test]
fn doctor_index_health_absent_db_is_warning_gate_passes() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Install materialises an index; remove it once so both doctor surfaces
    // must preserve the genuine absent state.
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    if db_path.exists() {
        fs::remove_file(&db_path).unwrap();
    }

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(
        code, 0,
        "absent index DB must not fail the gate (install-before-analyze is a \
         legitimate intermediate state): {json}"
    );
    assert_eq!(
        json["ok"], true,
        "absent index DB must leave ok=true: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "warning",
        ".weft/loomweave.schema must be a warning when DB is absent: {check}"
    );
    assert!(
        check["message"]
            .as_str()
            .unwrap_or("")
            .contains("loomweave install"),
        "warning message must suggest loomweave install + analyze: {check}"
    );
    assert!(
        !db_path.exists(),
        "doctor --format json must inspect an absent index read-only and never create loomweave.db"
    );

    // Text path: warnings-only → exit 0.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 0,
        "absent index DB must not fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("⚠ no index"),
        "absent DB must surface as a text-path warning: stdout:\n{out}"
    );
    assert!(
        !db_path.exists(),
        "doctor must inspect an absent index read-only and never create loomweave.db"
    );
}

#[test]
fn doctor_reports_external_sqlite_current_legacy_and_older_states() {
    let current = tempfile::tempdir().unwrap();
    install(&["install", "--all"], current.path());
    write_healthy_db(current.path());
    let (_, json) = doctor_json(current.path(), false);
    let current_check = check(&json, "federation.sqlite_compatibility");
    assert_eq!(current_check["status"], "ok", "{current_check}");
    assert_eq!(current_check["details"]["compatibility"], "compatible");
    assert_eq!(current_check["details"]["user_version"], 12);
    let (_, text) = doctor(current.path(), false);
    assert!(
        text.contains("federation.sqlite_compatibility")
            && text.contains("compatible at user_version=12"),
        "text output must carry the same current compatibility verdict:\n{text}"
    );

    let legacy = tempfile::tempdir().unwrap();
    install(&["install", "--all"], legacy.path());
    write_healthy_db(legacy.path());
    Connection::open(legacy.path().join(".weft/loomweave/loomweave.db"))
        .unwrap()
        .execute_batch("PRAGMA application_id = 0;")
        .unwrap();
    let (_, json) = doctor_json(legacy.path(), false);
    let legacy_check = check(&json, "federation.sqlite_compatibility");
    assert_eq!(legacy_check["status"], "warning", "{legacy_check}");
    assert_eq!(legacy_check["details"]["legacy_application_id"], true);
    let (_, text) = doctor(legacy.path(), false);
    assert!(
        text.contains("federation.sqlite_compatibility")
            && text.contains("legacy application_id=0"),
        "text output must report accepted legacy compatibility:\n{text}"
    );

    let older = tempfile::tempdir().unwrap();
    install(&["install", "--all"], older.path());
    write_healthy_db(older.path());
    Connection::open(older.path().join(".weft/loomweave/loomweave.db"))
        .unwrap()
        .execute_batch("PRAGMA user_version = 11;")
        .unwrap();
    let (_, json) = doctor_json(older.path(), false);
    let older_check = check(&json, "federation.sqlite_compatibility");
    assert_eq!(older_check["status"], "warning", "{older_check}");
    assert_eq!(older_check["details"]["compatibility"], "older_supported");
    assert_eq!(older_check["details"]["user_version"], 11);
    let (_, text) = doctor(older.path(), false);
    assert!(
        text.contains("federation.sqlite_compatibility")
            && text.contains("user_version=11 is older but supported"),
        "text output must report the actual older-supported version:\n{text}"
    );
}

#[cfg(unix)]
#[test]
fn doctor_warns_for_an_unanalysed_catalogue_and_missing_instance_identity() {
    let project = tempfile::tempdir().unwrap();
    install(&["install", "--all"], project.path());
    write_healthy_db(project.path());
    fs::write(project.path().join("sample.mt"), "gadget sample\n").unwrap();
    let plugin_dir = setup_classifier_plugin_dir();
    let plugin_path = env::join_paths([plugin_dir.path()]).unwrap();

    let (code, json) = doctor_json_with_env(
        project.path(),
        false,
        &[("PATH", plugin_path.to_str().unwrap())],
        &[],
    );

    assert_eq!(code, 0, "warnings remain advisory: {json}");
    for id in [
        "classifier.enumeration",
        "classifier.tags",
        "index.freshness",
        "http.instance_id",
    ] {
        assert_eq!(
            check(&json, id)["status"],
            "warning",
            "{id} must detect the uninitialised federation state: {json}"
        );
    }
    assert_eq!(
        check(&json, "http.instance_id")["details"]["present"],
        false
    );
    assert!(
        !project.path().join(".weft/loomweave/instance_id").exists(),
        "read-only doctor must not materialise identity"
    );
}

#[cfg(unix)]
#[test]
fn doctor_fix_materialises_identity_and_authoritative_classifier_metadata() {
    let project = tempfile::tempdir().unwrap();
    install(&["install", "--all"], project.path());
    write_healthy_db(project.path());
    fs::write(project.path().join("sample.mt"), "gadget sample\n").unwrap();
    let plugin_dir = setup_classifier_plugin_dir();
    let plugin_path = env::join_paths([plugin_dir.path()]).unwrap();
    let env = [("PATH", plugin_path.to_str().unwrap())];

    let (code, json) = doctor_json_with_env(project.path(), true, &env, &[]);

    assert_eq!(code, 0, "--fix must converge: {json}");
    for id in [
        "classifier.enumeration",
        "classifier.tags",
        "http.instance_id",
    ] {
        assert_eq!(check(&json, id)["status"], "fixed", "{id}: {json}");
        assert_eq!(check(&json, id)["fixed"], true, "{id}: {json}");
    }
    assert_eq!(check(&json, "index.freshness")["status"], "ok", "{json}");
    assert_eq!(check(&json, "sei.population")["status"], "ok", "{json}");

    let instance_path = project.path().join(".weft/loomweave/instance_id");
    let instance = fs::read_to_string(&instance_path).unwrap();
    uuid::Uuid::parse_str(instance.trim()).expect("doctor --fix writes a UUID");
    assert_eq!(
        fs::metadata(&instance_path).unwrap().permissions().mode() & 0o777,
        0o600,
        "instance identity must remain private"
    );

    let conn = Connection::open(project.path().join(".weft/loomweave/loomweave.db")).unwrap();
    let (status, coverage_schema): (String, String) = conn
        .query_row(
            "SELECT status, json_extract(stats, '$.classifier_coverage.schema') \
             FROM runs ORDER BY started_at DESC, id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "completed");
    assert_eq!(coverage_schema, "loomweave.classifier-coverage.v1");

    let (rerun_code, rerun) = doctor_json_with_env(project.path(), false, &env, &[]);
    assert_eq!(rerun_code, 0, "{rerun}");
    for id in [
        "classifier.enumeration",
        "classifier.tags",
        "index.freshness",
        "http.instance_id",
        "sei.population",
    ] {
        assert_eq!(check(&rerun, id)["status"], "ok", "{id}: {rerun}");
    }
}

#[test]
fn doctor_rejects_foreign_and_too_new_external_sqlite_before_catalogue_queries() {
    let foreign = tempfile::tempdir().unwrap();
    install(&["install", "--all"], foreign.path());
    write_healthy_db(foreign.path());
    Connection::open(foreign.path().join(".weft/loomweave/loomweave.db"))
        .unwrap()
        .execute_batch("PRAGMA application_id = 1234;")
        .unwrap();
    let (code, json) = doctor_json(foreign.path(), false);
    let foreign_check = check(&json, "federation.sqlite_compatibility");
    assert_eq!(code, 1, "{json}");
    assert_eq!(foreign_check["status"], "problem", "{foreign_check}");
    assert_eq!(foreign_check["details"]["reason"], "foreign_database");
    assert_eq!(
        check(&json, "classifier.enumeration")["status"],
        "problem",
        "classifier rows must not be interpreted from a foreign catalogue: {json}"
    );
    assert!(
        check(&json, "classifier.enumeration")["message"]
            .as_str()
            .unwrap_or_default()
            .contains("external SQLite catalogue is incompatible"),
        "{json}"
    );
    assert_eq!(
        check(&json, "classifier.tags")["status"],
        "problem",
        "{json}"
    );
    assert_eq!(
        check(&json, "sei.population")["status"],
        "problem",
        "SEI rows must not be interpreted from a foreign catalogue: {json}"
    );
    let (_, text) = doctor(foreign.path(), false);
    assert!(
        text.contains("federation.sqlite_compatibility")
            && text.contains("incompatible")
            && text.contains("ForeignDatabase"),
        "text output must report a foreign catalogue:\n{text}"
    );

    let too_new = tempfile::tempdir().unwrap();
    install(&["install", "--all"], too_new.path());
    write_healthy_db(too_new.path());
    Connection::open(too_new.path().join(".weft/loomweave/loomweave.db"))
        .unwrap()
        .execute_batch("PRAGMA user_version = 13;")
        .unwrap();
    let (code, json) = doctor_json(too_new.path(), false);
    let incompatible = check(&json, "federation.sqlite_compatibility");
    assert_eq!(code, 1, "{json}");
    assert_eq!(incompatible["status"], "problem", "{incompatible}");
    assert_eq!(incompatible["details"]["reason"], "too_new");
    assert_eq!(
        check(&json, "classifier.enumeration")["status"],
        "problem",
        "classifier rows must not be interpreted from a future catalogue: {json}"
    );
    assert!(
        check(&json, "classifier.enumeration")["message"]
            .as_str()
            .unwrap_or_default()
            .contains("external SQLite catalogue is incompatible"),
        "{json}"
    );
    assert_eq!(
        check(&json, "classifier.tags")["status"],
        "problem",
        "{json}"
    );
    assert_eq!(
        check(&json, "sei.population")["status"],
        "problem",
        "SEI rows must not be interpreted from a future catalogue: {json}"
    );
    let (_, text) = doctor(too_new.path(), false);
    assert!(
        text.contains("federation.sqlite_compatibility")
            && text.contains("incompatible")
            && text.contains("TooNew"),
        "text output must report a non-foreign incompatible catalogue:\n{text}"
    );
}

#[test]
fn doctor_classifier_latest_run_states_fail_closed() {
    for (status, stats, expected_status, expected_reason) in [
        ("running", "{}", "warning", "not completed"),
        ("failed", "{}", "problem", "not completed"),
        ("completed", "{}", "problem", "missing classifier_coverage"),
        ("completed", "{broken", "problem", "malformed stats JSON"),
        (
            "completed",
            r#"{"classifier_coverage":{"schema":"wrong","source_walk_complete":true,"source_walk_skipped_entries":0,"plugin_discovery_complete":true,"plugin_discovery_errors":0,"plugin_discovery_error_samples":[],"plugins":[]}}"#,
            "problem",
            "invalid classifier_coverage metadata",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        install(&["install", "--all"], dir.path());
        write_healthy_db(dir.path());
        insert_run_stats_raw(dir.path(), "latest", status, stats);

        let (_, json) = doctor_json(dir.path(), false);
        let enumeration = check(&json, "classifier.enumeration");
        let tags = check(&json, "classifier.tags");
        assert_eq!(enumeration["status"], expected_status, "{enumeration}");
        assert!(
            enumeration["message"]
                .as_str()
                .unwrap_or_default()
                .contains(expected_reason),
            "{enumeration}"
        );
        assert_eq!(enumeration["details"]["run_id"], "latest");
        assert_eq!(tags["status"], expected_status, "{tags}");

        let (_, text) = doctor(dir.path(), false);
        assert!(
            text.contains("classifier.enumeration") && text.contains(expected_reason),
            "text output must preserve the latest-run verdict:\n{text}"
        );
    }
}

#[test]
fn doctor_separates_classifier_enumeration_from_active_tag_support() {
    let healthy = tempfile::tempdir().unwrap();
    install(&["install", "--all"], healthy.path());
    write_healthy_db(healthy.path());
    insert_run_stats(
        healthy.path(),
        "healthy",
        "completed",
        &classifier_coverage(
            true,
            0,
            &serde_json::json!([
                plugin_coverage(
                    "python",
                    "complete",
                    2,
                    2,
                    0,
                    &["cli-command", "http-route"]
                ),
                plugin_coverage("rust", "not-applicable", 0, 0, 0, &["cli-command"]),
            ]),
        ),
    );
    let (_, json) = doctor_json(healthy.path(), false);
    let enumeration = check(&json, "classifier.enumeration");
    let tags = check(&json, "classifier.tags");
    assert_eq!(enumeration["status"], "ok", "{enumeration}");
    assert_eq!(enumeration["details"]["source_walk_complete"], true);
    assert_eq!(tags["status"], "ok", "{tags}");
    assert_eq!(
        tags["details"]["active_plugins"].as_array().unwrap().len(),
        1
    );
    assert_eq!(tags["details"]["active_plugins"][0]["plugin_id"], "python");
    assert_eq!(
        tags["details"]["active_plugins"][0]["classifier_tags"],
        serde_json::json!(["cli-command", "http-route"])
    );
    assert_eq!(
        tags["details"]["not_applicable_plugins"],
        serde_json::json!(["rust"])
    );
    let (_, text) = doctor(healthy.path(), false);
    assert!(
        text.contains("classifier.enumeration: classifier enumeration is complete")
            && text.contains("classifier.tags: active classifier tags: python")
            && !text.contains("classifier.tags: active classifier tags: rust"),
        "text output must report complete enumeration and active tags only:\n{text}"
    );

    let not_applicable = tempfile::tempdir().unwrap();
    install(&["install", "--all"], not_applicable.path());
    write_healthy_db(not_applicable.path());
    insert_run_stats(
        not_applicable.path(),
        "not-applicable",
        "completed",
        &classifier_coverage(
            true,
            0,
            &serde_json::json!([plugin_coverage(
                "rust",
                "not-applicable",
                0,
                0,
                0,
                &["cli-command"]
            )]),
        ),
    );
    let (_, json) = doctor_json(not_applicable.path(), false);
    assert_eq!(
        check(&json, "classifier.tags")["details"]["active_plugins"],
        serde_json::json!([])
    );
    let (_, text) = doctor(not_applicable.path(), false);
    assert!(
        text.contains("no active plugins; all discovered plugins were not applicable"),
        "text output must not infer support from a not-applicable plugin:\n{text}"
    );

    let incomplete = tempfile::tempdir().unwrap();
    install(&["install", "--all"], incomplete.path());
    write_healthy_db(incomplete.path());
    insert_run_stats(
        incomplete.path(),
        "incomplete",
        "completed",
        &classifier_coverage(
            false,
            1,
            &serde_json::json!([plugin_coverage(
                "python",
                "complete",
                1,
                1,
                0,
                &["cli-command"]
            )]),
        ),
    );
    let (code, json) = doctor_json(incomplete.path(), false);
    assert_eq!(code, 1, "{json}");
    assert_eq!(check(&json, "classifier.enumeration")["status"], "problem");
    assert_eq!(check(&json, "classifier.tags")["status"], "ok");
    let (_, text) = doctor(incomplete.path(), false);
    assert!(
        text.contains("classifier.enumeration: classifier enumeration is incomplete")
            && text.contains("source_walk_complete=false")
            && text.contains("classifier.tags: active classifier tags: python"),
        "text output must separate incomplete enumeration from tag support:\n{text}"
    );
}

#[test]
fn doctor_reports_degraded_failed_and_empty_classifier_declarations() {
    for (plugin_status, analyzed, degraded, tags, expected, expected_text) in [
        (
            "degraded",
            1,
            1,
            &["cli-command"][..],
            "warning",
            "active classifier plugin degraded",
        ),
        (
            "failed",
            0,
            0,
            &["cli-command"][..],
            "problem",
            "active classifier plugin failed",
        ),
        (
            "complete",
            1,
            0,
            &[][..],
            "warning",
            "active plugin declares no classifier tags",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        install(&["install", "--all"], dir.path());
        write_healthy_db(dir.path());
        insert_run_stats(
            dir.path(),
            "coverage",
            "completed",
            &classifier_coverage(
                true,
                0,
                &serde_json::json!([plugin_coverage(
                    "python",
                    plugin_status,
                    1,
                    analyzed,
                    degraded,
                    tags
                )]),
            ),
        );
        let (_, json) = doctor_json(dir.path(), false);
        let tags_check = check(&json, "classifier.tags");
        assert_eq!(tags_check["status"], expected, "{tags_check}");
        assert_eq!(
            tags_check["details"]["active_plugins"][0]["status"],
            plugin_status
        );
        let (_, text) = doctor(dir.path(), false);
        assert!(
            text.contains("classifier.tags") && text.contains(expected_text),
            "text output must preserve the active-plugin verdict:\n{text}"
        );
    }
}

#[test]
fn doctor_reports_authentication_modes_and_missing_configured_secret() {
    for (yaml, env, expected_mode, expected_status, expected_text) in [
        (
            "version: 1\nserve:\n  http:\n    enabled: true\n",
            None,
            "none",
            "ok",
            "enabled on loopback without authentication",
        ),
        (
            "version: 1\nserve:\n  http:\n    enabled: true\n    token_env: DOCTOR_BEARER\n",
            Some(("DOCTOR_BEARER", "secret")),
            "bearer",
            "ok",
            "protected routes use bearer authentication",
        ),
        (
            "version: 1\nserve:\n  http:\n    enabled: true\n    identity_token_env: DOCTOR_HMAC\n",
            Some(("DOCTOR_HMAC", "secret")),
            "hmac",
            "ok",
            "protected routes use HMAC authentication",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        install(&["install", "--all"], dir.path());
        fs::write(dir.path().join("loomweave.yaml"), yaml).unwrap();
        let env_pairs = env.map_or_else(Vec::new, |pair| vec![pair]);
        let (_, json) = doctor_json_with_env(dir.path(), false, &env_pairs, &[]);
        let auth = check(&json, "http.authentication");
        assert_eq!(auth["status"], expected_status, "{auth}");
        assert_eq!(auth["details"]["protected_routes"], expected_mode);
        let (_, text) = doctor_with_env(dir.path(), false, &env_pairs, &[]);
        assert!(
            text.contains("http.authentication") && text.contains(expected_text),
            "text output must report the effective {expected_mode} posture:\n{text}"
        );
    }

    let missing = tempfile::tempdir().unwrap();
    install(&["install", "--all"], missing.path());
    fs::write(
        missing.path().join("loomweave.yaml"),
        "version: 1\nserve:\n  http:\n    enabled: true\n    identity_token_env: DOCTOR_MISSING_HMAC\n",
    )
    .unwrap();
    let (code, json) = doctor_json_with_env(missing.path(), false, &[], &["DOCTOR_MISSING_HMAC"]);
    let auth = check(&json, "http.authentication");
    assert_eq!(code, 1, "{json}");
    assert_eq!(auth["status"], "problem", "{auth}");
    assert_eq!(auth["details"]["secret_present"], false);
    assert!(
        json["next_actions"].as_array().unwrap().iter().any(|action| {
            action
                == "Set $DOCTOR_MISSING_HMAC to a non-empty HMAC secret, then run `loomweave doctor` again."
        }),
        "missing-secret remediation must name the configured pointer without exposing its value: {json}"
    );
    let (_, text) = doctor_with_env(missing.path(), false, &[], &["DOCTOR_MISSING_HMAC"]);
    assert!(
        text.contains("http.authentication")
            && text.contains("configured but unusable")
            && text.contains("DOCTOR_MISSING_HMAC"),
        "text output must report a configured-but-missing secret without its value:\n{text}"
    );
}

#[test]
fn doctor_reports_malformed_config_and_instance_identity() {
    let malformed_yaml = tempfile::tempdir().unwrap();
    install(&["install", "--all"], malformed_yaml.path());
    fs::write(
        malformed_yaml.path().join("loomweave.yaml"),
        "serve: [broken\n",
    )
    .unwrap();
    let (code, json) = doctor_json(malformed_yaml.path(), false);
    assert_eq!(code, 1, "{json}");
    assert_eq!(check(&json, "http.authentication")["status"], "problem");
    assert!(
        json["next_actions"].as_array().unwrap().iter().any(|action| {
            action
                == "Repair `loomweave.yaml` syntax and validation errors, then run `loomweave doctor` again."
        }),
        "malformed config needs a config-specific next action: {json}"
    );
    let (_, text) = doctor(malformed_yaml.path(), false);
    assert!(
        text.contains("http.authentication") && text.contains("cannot parse loomweave.yaml"),
        "text output must surface malformed auth discovery:\n{text}"
    );

    let missing_instance = tempfile::tempdir().unwrap();
    install(&["install", "--all"], missing_instance.path());
    let (_, json) = doctor_json(missing_instance.path(), false);
    let instance = check(&json, "http.instance_id");
    assert_eq!(instance["status"], "warning", "{instance}");
    assert_eq!(instance["details"]["present"], false);
    let (_, text) = doctor(missing_instance.path(), false);
    assert!(
        text.contains("http.instance_id") && text.contains("not materialised yet"),
        "text output must report an absent instance ID:\n{text}"
    );

    let malformed_instance = tempfile::tempdir().unwrap();
    install(&["install", "--all"], malformed_instance.path());
    fs::write(
        malformed_instance
            .path()
            .join(".weft/loomweave/instance_id"),
        "not-a-uuid\n",
    )
    .unwrap();
    let (code, json) = doctor_json(malformed_instance.path(), false);
    let instance = check(&json, "http.instance_id");
    assert_eq!(code, 1, "{json}");
    assert_eq!(instance["status"], "problem", "{instance}");
    assert_eq!(instance["details"]["present"], true);
    assert!(
        json["next_actions"].as_array().unwrap().iter().any(|action| {
            action
                == "Remove the malformed `.weft/loomweave/instance_id`; `loomweave serve` will create a valid replacement."
        }),
        "malformed UUID needs a safe replacement action: {json}"
    );
    let (_, text) = doctor(malformed_instance.path(), false);
    assert!(
        text.contains("http.instance_id") && text.contains("malformed; expected a UUID"),
        "text output must report a malformed instance ID:\n{text}"
    );

    let valid_instance = tempfile::tempdir().unwrap();
    install(&["install", "--all"], valid_instance.path());
    let uuid = "00000000-0000-4000-8000-000000000007";
    fs::write(
        valid_instance.path().join(".weft/loomweave/instance_id"),
        format!("{uuid}\n"),
    )
    .unwrap();
    let (_, json) = doctor_json(valid_instance.path(), false);
    let instance = check(&json, "http.instance_id");
    assert_eq!(instance["status"], "ok", "{instance}");
    assert_eq!(instance["details"]["instance_id"], uuid);
    let (_, text) = doctor(valid_instance.path(), false);
    assert!(
        text.contains("http.instance_id") && text.contains(uuid),
        "text output must report the valid serving identity:\n{text}"
    );

    let unreadable_instance = tempfile::tempdir().unwrap();
    install(&["install", "--all"], unreadable_instance.path());
    fs::create_dir(
        unreadable_instance
            .path()
            .join(".weft/loomweave/instance_id"),
    )
    .unwrap();
    let (code, json) = doctor_json(unreadable_instance.path(), false);
    assert_eq!(code, 1, "{json}");
    assert_eq!(check(&json, "http.instance_id")["status"], "problem");
    assert!(
        json["next_actions"].as_array().unwrap().iter().any(|action| {
            action
                == "Restore read access to `.weft/loomweave/instance_id` and inspect it before replacing any data."
        }),
        "an unreadable identity needs a distinct non-destructive remedy: {json}"
    );
}

/// (b) DB file present but not valid `SQLite` → `.weft/loomweave.schema` is a
/// problem (ok=false), gate fails.
///
/// A corrupt or non-`SQLite` file in the DB position must be surfaced as a gate
/// failure, not silently reported as healthy.
#[test]
fn doctor_index_health_corrupt_db_is_problem_gate_fails() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Write a non-SQLite file at the DB path. (A 0-byte file would open as a
    // fresh db with user_version=0, which now classifies Unmigrated — a problem
    // — see `doctor_index_health_unmigrated_db_is_problem`; here we want the
    // distinct *unreadable* classification.)
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    fs::write(&db_path, b"this is not a sqlite database").unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 1, "a corrupt index DB must fail the gate: {json}");
    assert_eq!(
        json["ok"], false,
        "a corrupt index DB must set ok=false: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "problem",
        ".weft/loomweave.schema must be a problem when DB is unreadable: {check}"
    );
    assert!(
        check["message"]
            .as_str()
            .unwrap_or("")
            .contains("unreadable"),
        "problem message must say the index is unreadable: {check}"
    );

    // Text path: problem → exit 1.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a corrupt index DB must fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("✗") && out.contains("unreadable"),
        "corrupt DB must surface as a text-path problem: stdout:\n{out}"
    );
}

/// (c0) DB present, opens, header-valid, but `user_version = 0` (never
/// migrated) → problem (ok=false). The read-open path refuses such a file, so
/// `serve` would too; doctor must not green-light a DB `serve` rejects (review
/// #8 / read-vs-doctor parity). This is the regression for the prior false
/// positive where an empty/external `SQLite` file was reported Healthy.
#[test]
fn doctor_index_health_unmigrated_db_is_problem() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    // `install --all` may seed a migrated DB; remove it and drop a header-valid
    // SQLite file with NO schema applied, so user_version stays 0 (the
    // empty/external-file case the read path refuses).
    let _ = fs::remove_file(&db_path);
    {
        let conn = Connection::open(&db_path).expect("create empty SQLite DB");
        // Touch the file so the SQLite header is actually written to disk
        // (a bare open of a new path is lazy until first write).
        conn.execute_batch("PRAGMA user_version = 0;")
            .expect("stamp user_version 0");
    }

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 1, "an unmigrated index DB must fail the gate: {json}");
    assert_eq!(
        json["ok"], false,
        "an unmigrated index DB must set ok=false: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "problem",
        ".weft/loomweave.schema must be a problem for an unmigrated DB: {check}"
    );
    assert!(
        check["message"]
            .as_str()
            .unwrap_or("")
            .contains("unmigrated"),
        "problem message must say the index is unmigrated: {check}"
    );

    // Text path: problem → exit 1.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "an unmigrated index DB must fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("✗") && out.contains("unmigrated"),
        "unmigrated DB must surface as a text-path problem: stdout:\n{out}"
    );
}

/// (c) DB present, opens, but `user_version` > current → future-schema
/// problem (ok=false), message names the version numbers.
#[test]
fn doctor_index_health_future_schema_is_problem_with_version_in_message() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    // Create a valid SQLite file with user_version stamped to current+1.
    {
        let conn = Connection::open(&db_path).expect("create DB");
        // user_version is a 32-bit signed integer in SQLite; any value > current
        // triggers the future-schema guard. We avoid hardcoding a literal so the
        // test stays correct when CURRENT_SCHEMA_VERSION is bumped.
        conn.execute_batch("PRAGMA user_version = 99999;")
            .expect("set future user_version");
    }

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 1, "a future-schema DB must fail the gate: {json}");
    assert_eq!(
        json["ok"], false,
        "a future-schema DB must set ok=false: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "problem",
        ".weft/loomweave.schema must be a problem for a future-schema DB: {check}"
    );
    let msg = check["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("99999"),
        "problem message must name the found schema version (99999): {check}"
    );
    assert!(
        msg.contains("newer Loomweave build"),
        "problem message must mention 'newer Loomweave build': {check}"
    );

    // Text path: problem → exit 1.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a future-schema DB must fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("99999"),
        "text output must name the schema version (99999): stdout:\n{out}"
    );
}

/// (d) DB present, opens, version <= current → `.weft/loomweave.schema` is ok.
///
/// The check's specific status is verified via the JSON surface so we don't
/// couple to the global "All healthy" summary (which depends on plugin/llm state).
#[test]
fn doctor_index_health_healthy_db_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // A migrated DB carries a non-zero user_version, which the classifier reports
    // Healthy (the read-open path accepts it).
    write_healthy_db(dir.path());

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "a healthy index DB must not fail the gate: {json}");
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "ok",
        ".weft/loomweave.schema must be ok for a healthy DB: {check}"
    );
    assert_eq!(
        check["fixed"],
        serde_json::json!(false),
        "a healthy check is never marked fixed: {check}"
    );

    // Text path: no warning or problem for the index check → does not
    // contribute to exit-1.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 0,
        "a healthy index DB must not fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("✓") && out.contains("index DB present"),
        "healthy DB must surface as a text-path ok line: stdout:\n{out}"
    );
}

fn run_git(repo: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git runs")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A git-tracked runtime DB is a gate-failing problem: it mutates on every
/// analyze/scan, dirtying the work tree and blocking legis signing. `doctor`
/// must exit non-zero; `--fix` untracks it via `git rm --cached` and the project
/// is then healthy (exit 0), with the working-tree file preserved.
#[test]
fn doctor_flags_git_tracked_db_as_problem_and_fix_untracks_it() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    write_healthy_db(dir.path());
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "t@t"]);
    run_git(dir.path(), &["config", "user.name", "t"]);
    // `-f` overrides the installed .gitignore — the real scenario is a db that
    // was committed before ADR-005 was reversed.
    run_git(dir.path(), &["add", "-f", ".weft/loomweave/loomweave.db"]);

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a git-tracked db must fail the gate; stdout:\n{out}"
    );
    assert!(
        out.contains("loomweave.db is git-tracked"),
        "the tracked-db problem must be named; stdout:\n{out}"
    );

    let (fix_code, fix_out) = doctor(dir.path(), true);
    assert_eq!(
        fix_code, 0,
        "--fix untracks the db, then the project is healthy; stdout:\n{fix_out}"
    );
    assert!(
        fix_out.contains("git rm --cached"),
        "the --fix line must report the remedy; stdout:\n{fix_out}"
    );
    assert!(
        dir.path().join(".weft/loomweave/loomweave.db").is_file(),
        "git rm --cached must keep the working-tree db file"
    );
}

/// `doctor --fix`'s index-integrity repair deletes entity rows, so it must not
/// interleave with a concurrent `loomweave analyze` re-linking those same rows.
/// It takes the same advisory lock `analyze` does (STO-01) and reports busy
/// instead of racing.
///
/// Deliberately NOT held across all of `--fix`: `repair_classifier_analysis`
/// spawns `loomweave analyze`, which acquires this lock itself, so a
/// doctor-wide lock would deadlock against doctor's own child. The rest of the
/// run must still complete while the lock is held elsewhere.
#[test]
fn doctor_fix_reports_busy_when_analyze_holds_the_advisory_lock() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install"], dir.path());
    write_healthy_db(dir.path());

    // Baseline: with the lock free, the integrity check is not lock-blocked.
    let (_, free) = doctor_json(dir.path(), true);
    let free_status = check(&free, "index.integrity")["status"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(
        free_status, "problem",
        "baseline should not be lock-blocked"
    );

    // Now hold the lock the way a running `analyze` would.
    let lock_path = dir.path().join(".weft/loomweave/loomweave.lock");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open analyze lock");
    held.try_lock().expect("acquire analyze lock in test");

    let (_, busy) = doctor_json(dir.path(), true);
    let integrity = check(&busy, "index.integrity");
    assert_eq!(
        integrity["status"], "problem",
        "repair must refuse while analyze holds the lock; got {integrity}"
    );
    let message = integrity["message"].as_str().unwrap_or_default();
    let detail = integrity.to_string();
    assert!(
        message.contains("already in progress") || detail.contains("already in progress"),
        "operator needs to be told analyze holds the lock; got {integrity}"
    );

    drop(held);

    // Releasing the lock restores the repair path — the refusal is transient,
    // not a latched failure.
    let (_, recovered) = doctor_json(dir.path(), true);
    assert_eq!(
        check(&recovered, "index.integrity")["status"],
        serde_json::Value::from(free_status),
        "repair must recover once the lock is released"
    );
}
