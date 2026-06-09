//! Plugin-host hardening integration tests (lifecycle deadlines + exit
//! classification — plan 2026-06-10, Tasks 4-6; ticket clarion-7bc08e05c0).
//!
//! Drives the env-triggered misbehaviors of `loomweave-plugin-fixture`
//! (`LOOMWEAVE_FIXTURE_HANG_AT_INITIALIZE` / `HANG_AT_SHUTDOWN` /
//! `SPIN_AT_ANALYZE` / `ABORT_AT_ANALYZE`) through a real `loomweave analyze`
//! subprocess and asserts the run record, the persisted findings, and the
//! absence of leaked plugin children.
//!
//! Linux-gated: leak detection scans `/proc/<pid>/environ` for a per-test
//! marker variable the plugin child inherits from the analyze process.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use std::{env, fs};

use assert_cmd::Command;
use rusqlite::Connection;
use tempfile::TempDir;

/// Backstop for invocations that are EXPECTED to terminate quickly because a
/// host-side deadline (set to 500 ms via env) fires. Far below the fixture's
/// 3600 s hang, so a red (no-deadline) implementation fails the test fast
/// instead of wedging the suite.
const ANALYZE_BACKSTOP: Duration = Duration::from_secs(60);

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

/// Locate the `loomweave-fixture-plugin` binary (same convention as
/// `wp2_e2e.rs`: cargo artifact env first, `target/{debug,release}` fallback).
fn fixture_binary_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_loomweave-fixture-plugin") {
        return PathBuf::from(path);
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root must exist");

    let target_dir =
        env::var("CARGO_TARGET_DIR").map_or_else(|_| workspace_root.join("target"), PathBuf::from);

    for profile in &["debug", "release"] {
        let candidate = target_dir.join(profile).join("loomweave-fixture-plugin");
        if candidate.exists() {
            return candidate;
        }
    }

    panic!(
        "loomweave-fixture-plugin binary not found. \
         Run `cargo build --workspace` before running this test. \
         Searched: {}",
        target_dir.display()
    );
}

/// Synthetic `$PATH` directory: fixture binary (symlinked under the
/// `loomweave-plugin-*` discovery glob) + its `plugin.toml` (extensions: mt).
fn setup_plugin_dir(fixture_bin: &PathBuf) -> TempDir {
    let plugin_dir = TempDir::new().expect("create plugin tempdir");

    let dest = plugin_dir.path().join("loomweave-plugin-fixture");
    std::os::unix::fs::symlink(fixture_bin, &dest).expect("symlink loomweave-plugin-fixture");

    let meta = fs::metadata(fixture_bin).expect("stat fixture binary");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "fixture binary must be executable"
    );

    let toml_src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("loomweave-core")
        .join("tests")
        .join("fixtures")
        .join("plugin.toml");
    fs::copy(&toml_src, plugin_dir.path().join("plugin.toml")).expect("copy plugin.toml");

    plugin_dir
}

/// Initialise a temp project containing one fixture-claimed source file and
/// return (project dir, synthetic PATH value).
fn setup_project(plugin_dir: &TempDir) -> (TempDir, std::ffi::OsString) {
    let project_dir = TempDir::new().expect("create project tempdir");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    fs::write(
        project_dir.path().join("demo.mt"),
        b"widget demo.sample {}\n",
    )
    .expect("write demo.mt");
    let new_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");
    (project_dir, new_path)
}

fn open_db(project_dir: &TempDir) -> Connection {
    Connection::open(project_dir.path().join(".weft/loomweave/loomweave.db")).expect("open db")
}

/// (run-row count, status, failure_reason-from-stats) for the single run.
fn run_record(conn: &Connection) -> (i64, String, String) {
    let (count, run_status, stats_raw): (i64, String, String) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(status), ''), COALESCE(MAX(stats), '{}') FROM runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query runs");
    let parsed_stats: serde_json::Value =
        serde_json::from_str(&stats_raw).expect("runs.stats must be valid JSON");
    let failure_reason = parsed_stats["failure_reason"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    (count, run_status, failure_reason)
}

fn finding_count(conn: &Connection, rule_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM findings WHERE rule_id = ?1",
        [rule_id],
        |row| row.get(0),
    )
    .expect("query finding count")
}

/// Concatenated evidence JSON of every finding with `rule_id` (empty string
/// when none exist).
fn finding_evidence(conn: &Connection, rule_id: &str) -> String {
    conn.query_row(
        "SELECT COALESCE(GROUP_CONCAT(evidence, '\n'), '') FROM findings WHERE rule_id = ?1",
        [rule_id],
        |row| row.get(0),
    )
    .expect("query finding evidence")
}

/// Assert no live process carries `marker` ("KEY=VALUE") in its environment.
///
/// The marker is set on the spawned `loomweave analyze` process only; the
/// plugin child inherits it. After analyze exits, any process still carrying
/// the marker is a leaked (un-killed) plugin child. A true zombie cannot be
/// observed from here — once analyze exits, its zombie children are reparented
/// and reaped by init — so this asserts the leak that CAN outlive the run: a
/// live, hung plugin process.
fn assert_no_leaked_child(marker: &str) {
    let marker_bytes = marker.as_bytes();
    let mut leaked: Vec<String> = Vec::new();
    for entry in fs::read_dir("/proc").expect("read /proc") {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // Processes may vanish mid-scan; permission errors mean not ours.
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environ
            .windows(marker_bytes.len())
            .any(|w| w == marker_bytes)
        {
            let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
            leaked.push(format!(
                "pid {name}: {}",
                String::from_utf8_lossy(&cmdline).replace('\0', " ")
            ));
        }
    }
    assert!(
        leaked.is_empty(),
        "plugin child leaked past analyze exit (marker {marker}): {leaked:?}"
    );
}

fn unique_marker(test_name: &str) -> (String, String, String) {
    let key = "LOOMWEAVE_HARDENING_TEST_MARKER".to_owned();
    let value = format!("{test_name}-{}", std::process::id());
    let pair = format!("{key}={value}");
    (key, value, pair)
}

/// Task 4 (plan step 4.1): a plugin that hangs inside `initialize` is killed
/// by the handshake deadline; the run resolves terminal (`failed`, reason
/// names the handshake timeout), a phase-tagged `LMWV-PY-TIMEOUT` finding is
/// persisted, and no plugin child outlives the run.
#[test]
fn handshake_hang_times_out_run_terminal_child_reaped() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    let (marker_key, marker_value, marker_pair) = unique_marker("handshake-hang");

    loomweave_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &new_path)
        .env(&marker_key, &marker_value)
        .env("LOOMWEAVE_FIXTURE_HANG_AT_INITIALIZE", "1")
        .env("LOOMWEAVE_PLUGIN_HANDSHAKE_TIMEOUT_MS", "500")
        .timeout(ANALYZE_BACKSTOP)
        .assert()
        .failure();

    let conn = open_db(&project_dir);

    // (b) Run record is terminal: failed, reason names the handshake timeout.
    let (run_count, run_status, failure_reason) = run_record(&conn);
    assert_eq!(run_count, 1, "exactly one run row");
    assert_eq!(run_status, "failed", "run must resolve terminal-failed");
    assert!(
        failure_reason.contains("handshake timeout"),
        "failure_reason must name the handshake timeout; got {failure_reason:?}"
    );

    // (c) Phase-tagged timeout finding persisted.
    assert_eq!(
        finding_count(&conn, "LMWV-PY-TIMEOUT"),
        1,
        "exactly one LMWV-PY-TIMEOUT finding"
    );
    let evidence = finding_evidence(&conn, "LMWV-PY-TIMEOUT");
    assert!(
        evidence.contains("\"phase\":\"handshake\""),
        "timeout finding must carry phase=handshake metadata; got {evidence}"
    );

    // The timeout is the root cause: no redundant generic crash finding.
    assert_eq!(
        finding_count(&conn, "LMWV-INFRA-PLUGIN-CRASH"),
        0,
        "no redundant LMWV-INFRA-PLUGIN-CRASH when the cause is a timeout"
    );

    // (e) The watchdog's own SIGKILL must not be misreported as an OOM kill.
    // Enabled by Task 6 (timed_out gate); red until then.
    // assert_eq!(finding_count(&conn, "LMWV-INFRA-PLUGIN-OOM-KILLED"), 0);

    // (d) No plugin child outlives the run.
    assert_no_leaked_child(&marker_pair);
}
