//! Failure-mode integration tests for `clarion analyze`.
//!
//! These exercise the run-outcome promotion logic in
//! `crates/clarion-cli/src/analyze.rs::run_with_options` — specifically the
//! branch where the writer-actor rejects an `InsertEdge` mid-run and
//! `RunOutcome` must be set to `HardFailed` (→ `FailRun`) rather than
//! `SoftFailed` (→ `CommitRun(Failed)` with the full stats blob).
//!
//! The two paths differ in what the writer persists into `runs.stats`:
//!
//! - `HardFailed` (`FailRun`): stats is `{"failure_reason": "..."}` only —
//!   no `entities_inserted` / `edges_inserted` keys.
//! - `SoftFailed` (`CommitRun(Failed)`): stats carries the full schema
//!   (`entities_inserted`, `edges_inserted`, clustering, ...) plus a
//!   `failure_reason` naming the plugin crash.
//!
//! Both end with `runs.status = 'failed'`, so the discriminator is the
//! *shape* of the stats blob, not the status column.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

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

/// Tiny Python fixture plugin that declares an edge kind which the writer
/// does not know about (`bogus_edge`). The host accepts it (it appears in
/// `[ontology].edge_kinds`); the writer's `enforce_edge_contract` rejects
/// it with `CLA-INFRA-EDGE-UNKNOWN-KIND` — a `StorageError::WriterProtocol`
/// surfaced from `Writer::send_wait` on the first `InsertEdge`.
///
/// This is the most realistic deterministic trigger for a mid-run
/// writer-actor failure: passes core's validation, fails at the writer.
const BOGUS_EDGE_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
import json
import pathlib
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
                "name": "clarion-plugin-bogus",
                "version": "0.1.0",
                "ontology_version": "0.6.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        stem = pathlib.Path(path).stem
        module_id = f"bogusfixture:module:{stem}"
        other_id = f"bogusfixture:module:{stem}_partner"
        write_frame({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "entities": [
                    {
                        "id": module_id,
                        "kind": "module",
                        "qualified_name": stem,
                        "source": {"file_path": path},
                    },
                    {
                        "id": other_id,
                        "kind": "module",
                        "qualified_name": f"{stem}_partner",
                        "source": {"file_path": path},
                    },
                ],
                # `bogus_edge` is declared in the manifest's ontology, so
                # `PluginHost::process_edges` accepts it; the writer's
                # `enforce_edge_contract` rejects it with
                # `CLA-INFRA-EDGE-UNKNOWN-KIND`. That is the mid-run
                # writer-actor failure under test.
                "edges": [
                    {
                        "kind": "bogus_edge",
                        "from_id": module_id,
                        "to_id": other_id,
                        "source_byte_start": 0,
                        "source_byte_end": 4,
                        "confidence": "resolved",
                    },
                ],
                "stats": {},
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

const BOGUS_EDGE_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-bogus"
plugin_id = "bogusfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-bogus"
language = "bogusfixture"
extensions = ["bog"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = ["bogus_edge"]
rule_id_prefix = "CLA-BOGUS-"
ontology_version = "0.6.0"

[ontology.roles]
file_scope = ["module"]
"#;

/// Fixture plugin that successfully emits one module, then exits without
/// replying to the next `analyze_file` request. The test below pins the H5
/// contract: already completed file output is durable even when a later file
/// crashes the plugin.
const PARTIAL_CRASH_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
import json
import pathlib
import sys


seen_files = 0


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
                "name": "clarion-plugin-partial",
                "version": "0.1.0",
                "ontology_version": "0.6.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        seen_files += 1
        if seen_files > 1:
            raise SystemExit(7)
        path = msg["params"]["file_path"]
        stem = pathlib.Path(path).stem
        module_id = f"partialfixture:module:{stem}"
        write_frame({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "entities": [
                    {
                        "id": module_id,
                        "kind": "module",
                        "qualified_name": stem,
                        "source": {"file_path": path},
                    },
                ],
                "edges": [],
                "stats": {},
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

const PARTIAL_CRASH_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-partial"
plugin_id = "partialfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-partial"
language = "partialfixture"
extensions = ["part"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "CLA-PARTIAL-"
ontology_version = "0.6.0"

[ontology.roles]
file_scope = ["module"]
"#;

/// Fixture plugin that emits a cross-file call edge before the callee entity is
/// emitted by a later file. This pins the streaming writer ordering contract:
/// file entities may be streamed immediately, but edges must wait until both
/// endpoints exist in storage.
const CROSS_FILE_EDGE_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
import json
import pathlib
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
                "name": "clarion-plugin-cross-file",
                "version": "0.1.0",
                "ontology_version": "0.6.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        stem = pathlib.Path(path).stem
        module_id = f"crossfixture:module:{stem}"
        entities = [
            {
                "id": module_id,
                "kind": "module",
                "qualified_name": stem,
                "source": {"file_path": path},
            },
        ]
        edges = []
        if stem == "00_caller":
            caller_id = "crossfixture:function:00_caller.preview"
            entities.append({
                "id": caller_id,
                "kind": "function",
                "qualified_name": "00_caller.preview",
                "source": {
                    "file_path": path,
                    "line_start": 1,
                    "line_end": 1,
                    "byte_start": 0,
                    "byte_end": 7,
                },
            })
            edges.append({
                "kind": "calls",
                "from_id": caller_id,
                "to_id": "crossfixture:function:99_callee.record",
                "source_byte_start": 0,
                "source_byte_end": 7,
                "confidence": "resolved",
            })
        else:
            entities.append({
                "id": "crossfixture:function:99_callee.record",
                "kind": "function",
                "qualified_name": "99_callee.record",
                "source": {
                    "file_path": path,
                    "line_start": 1,
                    "line_end": 1,
                    "byte_start": 0,
                    "byte_end": 6,
                },
            })
        write_frame({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "entities": entities,
                "edges": edges,
                "stats": {},
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

const CROSS_FILE_EDGE_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-cross-file"
plugin_id = "crossfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-cross-file"
language = "crossfixture"
extensions = ["cross"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module", "function"]
edge_kinds = ["calls"]
rule_id_prefix = "CLA-CROSS-"
ontology_version = "0.6.0"

[ontology.roles]
file_scope = ["module"]
callable = ["function"]
"#;

fn write_bogus_edge_plugin(plugin_dir: &std::path::Path) {
    let plugin_script = plugin_dir.join("clarion-plugin-bogus");
    std::fs::write(&plugin_script, BOGUS_EDGE_PLUGIN_SCRIPT)
        .expect("write bogus edge plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat bogus edge plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod bogus edge plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), BOGUS_EDGE_PLUGIN_MANIFEST)
        .expect("write bogus edge plugin manifest");
}

fn write_partial_crash_plugin(plugin_dir: &std::path::Path) {
    let plugin_script = plugin_dir.join("clarion-plugin-partial");
    std::fs::write(&plugin_script, PARTIAL_CRASH_PLUGIN_SCRIPT)
        .expect("write partial crash plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat partial crash plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod partial crash plugin");

    std::fs::write(
        plugin_dir.join("plugin.toml"),
        PARTIAL_CRASH_PLUGIN_MANIFEST,
    )
    .expect("write partial crash plugin manifest");
}

fn write_cross_file_edge_plugin(plugin_dir: &std::path::Path) {
    let plugin_script = plugin_dir.join("clarion-plugin-cross-file");
    std::fs::write(&plugin_script, CROSS_FILE_EDGE_PLUGIN_SCRIPT)
        .expect("write cross-file edge plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat cross-file edge plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod cross-file edge plugin");

    std::fs::write(
        plugin_dir.join("plugin.toml"),
        CROSS_FILE_EDGE_PLUGIN_MANIFEST,
    )
    .expect("write cross-file edge plugin manifest");
}

#[test]
fn analyze_defers_cross_file_edges_until_target_entity_batch_arrives() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_cross_file_edge_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .env("PATH", "")
        .assert()
        .success();
    std::fs::write(project_dir.path().join("00_caller.cross"), b"preview\n")
        .expect("write caller file");
    std::fs::write(project_dir.path().join("99_callee.cross"), b"record\n")
        .expect("write callee file");

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let run_status: String = conn
        .query_row(
            "SELECT status FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query latest run status");
    assert_eq!(run_status, "completed");

    let cross_file_calls: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges \
             WHERE kind = 'calls' \
               AND from_id = 'crossfixture:function:00_caller.preview' \
               AND to_id = 'crossfixture:function:99_callee.record'",
            [],
            |row| row.get(0),
        )
        .expect("query cross-file calls edge count");
    assert_eq!(
        cross_file_calls, 1,
        "cross-file edge should be persisted after the target entity batch arrives"
    );
}

/// Seam test for the `SoftFailed` vs `HardFailed` branch in
/// `run_with_options` (analyze.rs ~lines 426-475, 519-601).
///
/// A writer-actor `InsertEdge` rejection mid-run must promote the run to
/// `HardFailed` → `FailRun`. The run row must:
///
/// 1. End with `status = 'failed'`.
/// 2. Carry a `stats.failure_reason` naming the writer-actor failure
///    (`CLA-INFRA-EDGE-UNKNOWN-KIND` from `enforce_edge_contract`), not a
///    plugin-crash reason.
/// 3. Have the minimal `FailRun` stats shape — no `entities_inserted`
///    key, distinguishing this from the `SoftFailed` path which writes the
///    full `CommitRun(Failed)` stats blob.
/// 4. Have NO `findings` rows tagged with the crash-loop `rule_id` — the
///    failure here is writer-side, not plugin-side.
///
/// The process must exit non-zero so `analyze && next` chains and CI
/// gating work (regression for the same surface as
/// `analyze_failrun_exits_nonzero_with_run_row_marked_failed`).
#[test]
fn analyze_promotes_run_to_hard_failed_when_writer_actor_fails_mid_run() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_bogus_edge_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .env("PATH", "")
        .assert()
        .success();
    std::fs::write(project_dir.path().join("demo.bog"), b"module\n").expect("write demo.bog");

    // Scrub PATH so only the bogus plugin is discovered. A second
    // discovered plugin would muddy the assertion: a healthy plugin
    // *after* the bogus one cannot run (writer-actor failure breaks the
    // 'plugins loop), but a healthy plugin *before* would insert
    // entities and edges first, changing the stats-shape discriminator.
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let out = clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("failed"),
        "stderr should mention failure; got: {stderr}"
    );

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();

    // (1) Run row marked failed.
    let (run_status, run_stats_raw): (String, String) = conn
        .query_row(
            "SELECT status, stats FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query latest run row");
    assert_eq!(
        run_status, "failed",
        "writer-actor mid-run failure must mark the run row failed"
    );

    let stats: serde_json::Value =
        serde_json::from_str(&run_stats_raw).expect("parse runs.stats JSON");

    // (2) failure_reason names the writer-actor rejection, not a plugin crash.
    let failure_reason = stats["failure_reason"]
        .as_str()
        .expect("HardFailed stats must contain failure_reason");
    assert!(
        failure_reason.contains("CLA-INFRA-EDGE-UNKNOWN-KIND"),
        "failure_reason should cite the writer's edge-contract code; \
         got: {failure_reason}"
    );
    assert!(
        failure_reason.contains("InsertEdge"),
        "failure_reason should name the InsertEdge surface; got: {failure_reason}"
    );
    assert!(
        !failure_reason.contains("plugin(s) crashed") && !failure_reason.contains("panicked"),
        "writer-actor failure must not be reported as a plugin crash; \
         got: {failure_reason}"
    );

    // (3) Stats shape is the minimal FailRun blob — no SoftFailed keys.
    // SoftFailed's `CommitRun(Failed)` writes the full stats schema with
    // `entities_inserted`, `edges_inserted`, clustering, etc.; FailRun
    // writes only `{"failure_reason": ...}`. Asserting the absence of
    // SoftFailed-only keys is the load-bearing discriminator between the
    // two branches.
    assert!(
        stats.get("entities_inserted").is_none(),
        "FailRun stats must not contain entities_inserted (SoftFailed key); \
         got: {run_stats_raw}"
    );
    assert!(
        stats.get("edges_inserted").is_none(),
        "FailRun stats must not contain edges_inserted (SoftFailed key); \
         got: {run_stats_raw}"
    );
    assert!(
        stats.get("clustering").is_none(),
        "FailRun stats must not contain clustering (SoftFailed key); \
         got: {run_stats_raw}"
    );

    // (4) No crash-loop finding rows — the failure here is writer-side.
    // The crash-loop breaker only ticks on plugin crashes; it must not
    // have tripped, and no row tagged with the crash-loop `rule_id` may
    // appear.
    let crash_loop_findings: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM findings \
             WHERE rule_id = 'CLA-INFRA-PLUGIN-DISABLED-CRASH-LOOP'",
            [],
            |row| row.get(0),
        )
        .expect("query crash-loop findings count");
    assert_eq!(
        crash_loop_findings, 0,
        "no FINDING_DISABLED_CRASH_LOOP rows should exist; \
         writer-actor failure must not tick the crash-loop breaker"
    );
}

#[test]
fn analyze_persists_completed_file_batches_when_plugin_later_crashes() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_partial_crash_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .env("PATH", "")
        .assert()
        .success();
    std::fs::write(project_dir.path().join("first.part"), b"first\n").expect("write first.part");
    std::fs::write(project_dir.path().join("second.part"), b"second\n").expect("write second.part");

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .failure();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let (run_status, run_stats_raw): (String, String) = conn
        .query_row(
            "SELECT status, stats FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query latest run row");
    assert_eq!(run_status, "failed");
    let stats: serde_json::Value =
        serde_json::from_str(&run_stats_raw).expect("parse runs.stats JSON");
    let failure_reason = stats["failure_reason"]
        .as_str()
        .expect("failed plugin run should record a failure_reason");
    assert!(
        failure_reason.contains("partialfixture"),
        "failure_reason should identify the crashing plugin; got: {failure_reason}"
    );

    let persisted_modules: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities \
             WHERE plugin_id = 'partialfixture' \
               AND kind = 'module'",
            [],
            |row| row.get(0),
        )
        .expect("query persisted partialfixture module count");
    assert_eq!(
        persisted_modules, 1,
        "the completed file's module must remain durable after the next file crashes"
    );
}
