#![cfg(unix)]

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

const PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
import json
import os
import pathlib
import re
import sys


def read_frame():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if line in (b"", b"\r\n"):
            break
        name, value = line.decode("ascii").strip().split(":", 1)
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    return json.loads(sys.stdin.buffer.read(length))


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
        write_frame({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "name": "clarion-plugin-secretfixture",
                "version": "0.1.0",
                "ontology_version": "0.1.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        if (
            os.environ.get("SECRETFIXTURE_ASSERT_ENV_ABSENT")
            and os.environ.get("CLARION_DOTENV_SENTINEL") is not None
        ):
            raise SystemExit(42)
        path = msg["params"]["file_path"]
        source_path = os.environ.get("SECRETFIXTURE_SOURCE_OVERRIDE", path)
        name = "file_" + re.sub(r"[^A-Za-z0-9_]", "_", pathlib.Path(path).name)
        write_frame({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "entities": [
                    {
                        "id": "secretfixture:module:" + name,
                        "kind": "module",
                        "qualified_name": name,
                        "source": {"file_path": source_path},
                    }
                ],
                "edges": [],
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

const PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-secretfixture"
plugin_id = "secretfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-secretfixture"
language = "secretfixture"
extensions = ["sec"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "CLA-SECRET-FIXTURE-"
ontology_version = "0.1.0"
"#;

fn write_secret_fixture_plugin(plugin_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("clarion-plugin-secretfixture");
    std::fs::write(&plugin_script, PLUGIN_SCRIPT).expect("write plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), PLUGIN_MANIFEST).expect("write plugin manifest");
}

fn install_project(project: &std::path::Path) {
    clarion_bin()
        .args(["install", "--path"])
        .arg(project)
        .assert()
        .success();
}

fn plugin_path(plugin_dir: &std::path::Path) -> std::ffi::OsString {
    std::env::join_paths(std::iter::once(plugin_dir.to_path_buf())).unwrap()
}

fn conn(project: &std::path::Path) -> Connection {
    Connection::open(project.join(".clarion/clarion.db")).expect("open clarion db")
}

#[test]
fn clean_project_has_no_secret_findings() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(project.path().join("clean.sec"), b"nothing to see\n").unwrap();

    clarion_bin()
        .arg("analyze")
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .success();

    let count: i64 = conn(project.path())
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id LIKE 'CLA-SEC-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn secret_file_persists_finding_and_briefing_block() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(
        project.path().join("leaky.sec"),
        b"aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n",
    )
    .unwrap();

    clarion_bin()
        .arg("analyze")
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .success();

    let db = conn(project.path());
    let blocked: String = db
        .query_row(
            "SELECT json_extract(properties, '$.briefing_blocked') FROM entities",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(blocked, "secret_present");
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-SEC-SECRET-DETECTED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn dotenv_sidecar_persists_finding_with_core_file_anchor() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(project.path().join("clean.sec"), b"nothing to see\n").unwrap();
    std::fs::write(
        project.path().join(".env"),
        b"aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n",
    )
    .unwrap();

    clarion_bin()
        .arg("analyze")
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .success();

    let db = conn(project.path());
    let finding_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-SEC-SECRET-DETECTED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let anchor_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM entities \
             WHERE plugin_id = 'core' \
               AND kind = 'file' \
               AND source_file_path LIKE '%.env' \
               AND json_extract(properties, '$.briefing_blocked') = 'secret_present'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(finding_count, 1);
    assert_eq!(anchor_count, 1);
}

#[test]
fn analyze_does_not_load_dotenv_into_plugin_environment() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(project.path().join("clean.sec"), b"nothing to see\n").unwrap();
    std::fs::write(
        project.path().join(".env"),
        b"CLARION_DOTENV_SENTINEL=ordinaryvalue\n",
    )
    .unwrap();

    clarion_bin()
        .arg("analyze")
        .arg(".")
        .current_dir(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .env("SECRETFIXTURE_ASSERT_ENV_ABSENT", "1")
        .assert()
        .success();
}

#[test]
fn plugin_entity_for_unscanned_source_is_briefing_blocked() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(project.path().join("clean.sec"), b"nothing to see\n").unwrap();
    let unscanned = project.path().join("notes.txt");
    std::fs::write(&unscanned, b"ordinary notes\n").unwrap();

    clarion_bin()
        .arg("analyze")
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .env("SECRETFIXTURE_SOURCE_OVERRIDE", &unscanned)
        .assert()
        .success();

    let blocked: String = conn(project.path())
        .query_row(
            "SELECT json_extract(properties, '$.briefing_blocked') FROM entities",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(blocked, "unscanned_source");
}

#[test]
fn baseline_suppresses_secret_and_emits_audit_match() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(
        project.path().join("leaky.sec"),
        b"aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join(".clarion/secrets-baseline.yaml"),
        r#"
version: "1.0"
results:
  "leaky.sec":
    - type: "AWS Access Key"
      hashed_secret: "25910f981e85ca04baf359199dd0bd4a3ae738b6"
      line_number: 1
      is_secret: false
      justification: "AWS documentation example key."
"#,
    )
    .unwrap();

    clarion_bin()
        .arg("analyze")
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .success();

    let db = conn(project.path());
    let secret_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-SEC-SECRET-DETECTED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let match_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-INFRA-SECRET-BASELINE-MATCH'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let blocked_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE json_extract(properties, '$.briefing_blocked') IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(secret_count, 0);
    assert_eq!(match_count, 1);
    assert_eq!(blocked_count, 0);
}

#[test]
fn missing_baseline_justification_degrades_to_finding() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(
        project.path().join("leaky.sec"),
        b"aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join(".clarion/secrets-baseline.yaml"),
        r#"
version: "1.0"
results:
  "leaky.sec":
    - type: "AWS Access Key"
      hashed_secret: "25910f981e85ca04baf359199dd0bd4a3ae738b6"
      line_number: 1
      is_secret: false
  "stale.sec":
    - type: "AWS Access Key"
      hashed_secret: "25910f981e85ca04baf359199dd0bd4a3ae738b6"
      line_number: 9
      is_secret: false
"#,
    )
    .unwrap();

    clarion_bin()
        .arg("analyze")
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .success();

    let count: i64 = conn(project.path())
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn non_tty_override_confirmed_allows_briefing_and_records_stats() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(
        project.path().join("leaky.sec"),
        b"aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n",
    )
    .unwrap();

    clarion_bin()
        .args([
            "analyze",
            "--allow-unredacted-secrets",
            "--confirm-allow-unredacted-secrets=yes-i-understand",
        ])
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .success();

    let db = conn(project.path());
    let blocked_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE json_extract(properties, '$.briefing_blocked') IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let override_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-SEC-UNREDACTED-SECRETS-ALLOWED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let override_used: i64 = db
        .query_row(
            "SELECT json_extract(stats, '$.secret_override_used') FROM runs",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(blocked_count, 0);
    assert_eq!(override_count, 1);
    assert_eq!(override_used, 1);
}

#[test]
fn non_tty_override_without_confirmation_exits_78_before_run_start() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(
        project.path().join("leaky.sec"),
        b"aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n",
    )
    .unwrap();

    let assert = clarion_bin()
        .args(["analyze", "--allow-unredacted-secrets"])
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .code(78);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains("CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED"));
    let run_count: i64 = conn(project.path())
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_count, 0);
}

#[test]
fn non_tty_override_with_wrong_confirmation_exits_78_before_run_start() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(
        project.path().join("leaky.sec"),
        b"aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n",
    )
    .unwrap();

    let assert = clarion_bin()
        .args([
            "analyze",
            "--allow-unredacted-secrets",
            "--confirm-allow-unredacted-secrets=oops",
        ])
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .code(78);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains("CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED"));
    let run_count: i64 = conn(project.path())
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_count, 0);
}

#[test]
#[ignore = "TTY override confirmation needs an interactive terminal; WS-D owns the manual smoke."]
fn tty_override_confirmation_manual_smoke() {}

#[test]
fn override_flag_is_noop_without_detections() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(project.path().join("clean.sec"), b"nothing to see\n").unwrap();

    clarion_bin()
        .args(["analyze", "--allow-unredacted-secrets"])
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .success();

    let db = conn(project.path());
    let override_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-SEC-UNREDACTED-SECRETS-ALLOWED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stats_value: Option<i64> = db
        .query_row(
            "SELECT json_extract(stats, '$.secret_override_used') FROM runs",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(override_count, 0);
    assert_eq!(stats_value, None);
}

#[test]
fn only_secret_bearing_file_is_blocked_in_multi_file_project() {
    let project = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    write_secret_fixture_plugin(plugin.path());
    install_project(project.path());
    std::fs::write(project.path().join("clean_a.sec"), b"nothing to see\n").unwrap();
    std::fs::write(
        project.path().join("leaky.sec"),
        b"aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'\n",
    )
    .unwrap();
    std::fs::write(project.path().join("clean_b.sec"), b"still clean\n").unwrap();

    clarion_bin()
        .arg("analyze")
        .arg(project.path())
        .env("PATH", plugin_path(plugin.path()))
        .assert()
        .success();

    let blocked_count: i64 = conn(project.path())
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE json_extract(properties, '$.briefing_blocked') = 'secret_present'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(blocked_count, 1);
}
