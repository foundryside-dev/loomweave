//! `clarion analyze` Sprint-1 integration test.

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[cfg(unix)]
const AMBIGUOUS_CALLS_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
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
                "name": "clarion-plugin-calls",
                "version": "0.1.0",
                "ontology_version": "0.4.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        write_frame({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "entities": [
                    {
                        "id": "callsfixture:module:demo",
                        "kind": "module",
                        "qualified_name": "demo",
                        "source": {"file_path": path},
                    },
                    {
                        "id": "callsfixture:function:demo.caller",
                        "kind": "function",
                        "qualified_name": "demo.caller",
                        "source": {
                            "file_path": path,
                            "source_range": {
                                "start_line": 1,
                                "start_col": 0,
                                "end_line": 1,
                                "end_col": 13
                            },
                        },
                        "parent_id": "callsfixture:module:demo",
                    },
                    {
                        "id": "callsfixture:function:demo.callee",
                        "kind": "function",
                        "qualified_name": "demo.callee",
                        "source": {"file_path": path},
                        "parent_id": "callsfixture:module:demo",
                    },
                ],
                "edges": [
                    {
                        "kind": "contains",
                        "from_id": "callsfixture:module:demo",
                        "to_id": "callsfixture:function:demo.caller",
                    },
                    {
                        "kind": "contains",
                        "from_id": "callsfixture:module:demo",
                        "to_id": "callsfixture:function:demo.callee",
                    },
                    {
                        "kind": "calls",
                        "from_id": "callsfixture:function:demo.caller",
                        "to_id": "callsfixture:function:demo.callee",
                        "source_byte_start": 12,
                        "source_byte_end": 18,
                        "confidence": "ambiguous",
                    },
                ],
                "stats": {
                    "unresolved_call_sites_total": 2,
                    "unresolved_call_sites": [
                        {
                            "caller_entity_id": "callsfixture:function:demo.caller",
                            "site_ordinal": 0,
                            "source_byte_start": 0,
                            "source_byte_end": 6,
                            "callee_expr": "dynamic_target",
                        },
                    ],
                    "reference_sites_total": 3,
                    "references_resolved_total": 4,
                    "references_skipped_external_total": 5,
                    "references_skipped_cap_total": 6,
                    "unresolved_reference_sites_total": 7,
                    "pyright_query_latency_ms": list(range(10, 1010, 10)),
                    "pyright_index_parse_latency_ms": [4, 8, 12],
                    "extractor_parse_latency_ms": 6,
                },
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

#[cfg(unix)]
const AMBIGUOUS_CALLS_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-calls"
plugin_id = "callsfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-calls"
language = "callsfixture"
extensions = ["call"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module", "function"]
edge_kinds = ["contains", "calls"]
rule_id_prefix = "CLA-CALLS-"
ontology_version = "0.4.0"
"#;

#[cfg(unix)]
fn write_ambiguous_calls_plugin(plugin_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("clarion-plugin-calls");
    std::fs::write(&plugin_script, AMBIGUOUS_CALLS_PLUGIN_SCRIPT)
        .expect("write calls plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat calls plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod calls plugin");

    std::fs::write(
        plugin_dir.join("plugin.toml"),
        AMBIGUOUS_CALLS_PLUGIN_MANIFEST,
    )
    .expect("write calls plugin manifest");
}

#[test]
fn analyze_without_plugins_writes_skipped_run_row() {
    let dir = tempfile::tempdir().unwrap();

    // Scrub PATH — if the developer or CI image has any clarion-plugin-*
    // binary installed (including the project's own fixture), discovery
    // will find it and the run transitions out of `skipped_no_plugins`.
    // The sibling test `analyze_failrun_exits_nonzero_with_run_row_marked_failed`
    // uses the same pattern.
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();

    clarion_bin()
        .args(["analyze"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();

    let conn = Connection::open(dir.path().join(".clarion/clarion.db")).unwrap();
    let (count, status): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(status), '') FROM runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(status, "skipped_no_plugins");

    let entity_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert_eq!(entity_count, 0);
}

#[test]
fn analyze_fails_cleanly_if_clarion_dir_missing() {
    let dir = tempfile::tempdir().unwrap();
    let out = clarion_bin()
        .args(["analyze"])
        .arg(dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("clarion install"),
        "error did not point operator at install: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn analyze_stats_reports_ambiguous_edges_total() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_ambiguous_calls_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    std::fs::write(project_dir.path().join("demo.call"), b"caller callee\n")
        .expect("write demo.call");

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let stats_raw: String = conn
        .query_row("SELECT stats FROM runs LIMIT 1", [], |row| row.get(0))
        .expect("query runs.stats");
    let stats: serde_json::Value = serde_json::from_str(&stats_raw).expect("stats JSON");
    assert!(
        stats["ambiguous_edges_total"].as_u64().unwrap_or_default() > 0,
        "ambiguous_edges_total should be > 0 after ambiguous calls edge; got {stats_raw}"
    );
    assert_eq!(
        stats["unresolved_call_sites_total"].as_u64(),
        Some(2),
        "unresolved_call_sites_total should aggregate plugin stats; got {stats_raw}"
    );
    assert_eq!(
        stats["reference_sites_total"].as_u64(),
        Some(3),
        "reference_sites_total should aggregate plugin stats; got {stats_raw}"
    );
    assert_eq!(
        stats["references_resolved_total"].as_u64(),
        Some(4),
        "references_resolved_total should aggregate plugin stats; got {stats_raw}"
    );
    assert_eq!(
        stats["references_skipped_external_total"].as_u64(),
        Some(5),
        "references_skipped_external_total should aggregate plugin stats; got {stats_raw}"
    );
    assert_eq!(
        stats["references_skipped_cap_total"].as_u64(),
        Some(6),
        "references_skipped_cap_total should aggregate plugin stats; got {stats_raw}"
    );
    assert_eq!(
        stats["unresolved_reference_sites_total"].as_u64(),
        Some(7),
        "unresolved_reference_sites_total should aggregate plugin stats; got {stats_raw}"
    );
    assert_eq!(
        stats["pyright_query_latency_p95_ms"].as_u64(),
        Some(950),
        "pyright_query_latency_p95_ms should be the deterministic p95; got {stats_raw}"
    );
    assert_eq!(
        stats["pyright_index_parse_latency_p95_ms"].as_u64(),
        Some(12),
        "pyright_index_parse_latency_p95_ms should be aggregated; got {stats_raw}"
    );
    assert_eq!(
        stats["extractor_parse_latency_p95_ms"].as_u64(),
        Some(6),
        "extractor_parse_latency_p95_ms should be aggregated; got {stats_raw}"
    );
    let unresolved_row: (String, String, i64, i64) = conn
        .query_row(
            "SELECT caller_entity_id, callee_expr, source_byte_start, source_byte_end \
             FROM entity_unresolved_call_sites",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query unresolved call site row");
    assert_eq!(
        unresolved_row,
        (
            "callsfixture:function:demo.caller".to_owned(),
            "dynamic_target".to_owned(),
            0,
            6,
        )
    );
}

/// Regression for wp2 review-2 (clarion-f56dc6ee43): `FailRun` must exit
/// non-zero so `clarion analyze && next` chains and CI gating work.
///
/// Triggers the discovery-errors `FailRun` branch by placing a
/// `clarion-plugin-*` executable on `$PATH` next to a malformed
/// `plugin.toml`. Before the fix, this exited 0; after, it exits non-zero
/// AND the `runs.status` column still reads `failed` (the run row is
/// marked before the bail).
#[cfg(unix)]
#[test]
fn analyze_failrun_exits_nonzero_with_run_row_marked_failed() {
    use std::os::unix::fs::symlink;

    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();

    // Put a `clarion-plugin-broken` on the synthetic PATH alongside a
    // malformed plugin.toml. Discovery will try to parse the toml and
    // collect the error; with no compliant plugins, FailRun fires.
    let plugin_bin = plugin_dir.path().join("clarion-plugin-broken");
    symlink("/bin/true", &plugin_bin).expect("symlink /bin/true");
    std::fs::write(
        plugin_dir.path().join("plugin.toml"),
        b"this is {not = valid toml @@@",
    )
    .expect("write malformed plugin.toml");

    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let new_path = std::env::join_paths(
        std::iter::once(plugin_dir.path().to_path_buf())
            .chain(std::env::split_paths(&current_path)),
    )
    .expect("join_paths");

    let out = clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &new_path)
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("failed"),
        "stderr should mention failure; got: {stderr}"
    );

    // The run row must still be marked `failed` — the FailRun WriterCmd
    // runs before the bail, so the DB state is consistent with the exit
    // code.
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query latest run status");
    assert_eq!(
        status, "failed",
        "run row must be marked 'failed' to stay consistent with exit code"
    );
}
