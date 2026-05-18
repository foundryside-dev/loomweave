//! `clarion analyze` Sprint-1 integration test.

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

fn latest_run_config(project_root: &std::path::Path) -> serde_json::Value {
    let conn = Connection::open(project_root.join(".clarion/clarion.db")).unwrap();
    let config_raw: String = conn
        .query_row(
            "SELECT config FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query latest runs.config");
    serde_json::from_str(&config_raw).expect("runs.config JSON")
}

fn latest_run_stats(project_root: &std::path::Path) -> serde_json::Value {
    let conn = Connection::open(project_root.join(".clarion/clarion.db")).unwrap();
    let stats_raw: String = conn
        .query_row(
            "SELECT stats FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query latest runs.stats");
    serde_json::from_str(&stats_raw).expect("runs.stats JSON")
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
const IMPORTS_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
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
                "name": "clarion-plugin-imports",
                "version": "0.1.0",
                "ontology_version": "0.6.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        stem = pathlib.Path(path).stem
        module_id = f"importsfixture:module:{stem}"
        edges = []
        if stem == "consumer":
            edges = [
                {
                    "kind": "imports",
                    "from_id": module_id,
                    "to_id": "importsfixture:module:internal",
                    "source_byte_start": 0,
                    "source_byte_end": 15,
                    "confidence": "resolved",
                    "properties": {"imported_name": "internal"},
                },
                {
                    "kind": "imports",
                    "from_id": module_id,
                    "to_id": "importsfixture:module:external",
                    "source_byte_start": 16,
                    "source_byte_end": 31,
                    "confidence": "resolved",
                    "properties": {"imported_name": "external"},
                },
            ]
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
                "edges": edges,
                "stats": {},
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

#[cfg(unix)]
const IMPORTS_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-imports"
plugin_id = "importsfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-imports"
language = "importsfixture"
extensions = ["imp"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = ["imports"]
rule_id_prefix = "CLA-IMPORTS-"
ontology_version = "0.6.0"
"#;

#[cfg(unix)]
const PHASE3_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
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


TARGETS = {
    "auth_a": ["auth_b"],
    "auth_b": ["auth_a"],
    "billing_a": ["billing_b"],
    "billing_b": ["billing_a"],
    "weak_a": ["weak_b"],
}


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
                "name": "clarion-plugin-phase3",
                "version": "0.1.0",
                "ontology_version": "0.6.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        stem = pathlib.Path(path).stem
        module_id = f"phase3fixture:module:{stem}"
        edges = [
            {
                "kind": "imports",
                "from_id": module_id,
                "to_id": f"phase3fixture:module:{target}",
                "source_byte_start": 0,
                "source_byte_end": 10,
                "confidence": "resolved",
                "properties": {"imported_name": target},
            }
            for target in TARGETS.get(stem, [])
        ]
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
                "edges": edges,
                "stats": {},
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

#[cfg(unix)]
const PHASE3_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-phase3"
plugin_id = "phase3fixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-phase3"
language = "phase3fixture"
extensions = ["p3"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = ["imports"]
rule_id_prefix = "CLA-PHASE3-"
ontology_version = "0.6.0"
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

#[cfg(unix)]
fn write_imports_plugin(plugin_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("clarion-plugin-imports");
    std::fs::write(&plugin_script, IMPORTS_PLUGIN_SCRIPT).expect("write imports plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat imports plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod imports plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), IMPORTS_PLUGIN_MANIFEST)
        .expect("write imports plugin manifest");
}

#[cfg(unix)]
fn write_phase3_plugin(plugin_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("clarion-plugin-phase3");
    std::fs::write(&plugin_script, PHASE3_PLUGIN_SCRIPT).expect("write phase3 plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat phase3 plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod phase3 plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), PHASE3_PLUGIN_MANIFEST)
        .expect("write phase3 plugin manifest");
}

#[cfg(unix)]
fn run_phase3_fixture(stems: &[&str], config_yaml: &str) -> tempfile::TempDir {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    for stem in stems {
        std::fs::write(project_dir.path().join(format!("{stem}.p3")), b"module\n")
            .expect("write phase3 fixture file");
    }
    let config_path = project_dir.path().join("phase3-clarion.yaml");
    std::fs::write(&config_path, config_yaml).expect("write phase3 config");

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    project_dir
}

#[cfg(unix)]
fn phase3_config(min_cluster_size: u64) -> String {
    format!(
        r"
analysis:
  clustering:
    min_cluster_size: {min_cluster_size}
"
    )
}

#[cfg(unix)]
fn phase3_weighted_components_config(min_cluster_size: u64) -> String {
    format!(
        r"
analysis:
  clustering:
    algorithm: weighted_components
    min_cluster_size: {min_cluster_size}
"
    )
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
fn analyze_default_config_records_clustering_defaults() {
    let dir = tempfile::tempdir().unwrap();

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

    let config = latest_run_config(dir.path());
    let clustering = &config["analysis"]["clustering"];
    assert_eq!(clustering["enabled"].as_bool(), Some(true));
    assert_eq!(clustering["algorithm"].as_str(), Some("leiden"));
    assert_eq!(clustering["seed"].as_u64(), Some(42));
    assert_eq!(clustering["resolution"].as_f64(), Some(1.0));
    assert_eq!(clustering["max_iterations"].as_u64(), Some(100));
    assert_eq!(clustering["min_cluster_size"].as_u64(), Some(3));
    assert_eq!(
        clustering["edge_types"],
        serde_json::json!(["imports", "calls"])
    );
    assert_eq!(clustering["weight_by"].as_str(), Some("reference_count"));
    assert_eq!(clustering["weak_modularity_threshold"].as_f64(), Some(0.3));
}

#[test]
fn analyze_config_file_overrides_clustering_seed_and_algorithm() {
    let dir = tempfile::tempdir().unwrap();

    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let config_path = dir.path().join("custom-clarion.yaml");
    std::fs::write(
        &config_path,
        r"
analysis:
  clustering:
    algorithm: weighted_components
    seed: 99
    weak_modularity_threshold: 0.0
",
    )
    .expect("write analyze config");

    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();

    let config = latest_run_config(dir.path());
    let clustering = &config["analysis"]["clustering"];
    assert_eq!(
        clustering["algorithm"].as_str(),
        Some("weighted_components")
    );
    assert_eq!(clustering["seed"].as_u64(), Some(99));
    assert_eq!(clustering["enabled"].as_bool(), Some(true));
    assert_eq!(clustering["max_iterations"].as_u64(), Some(100));
    assert_eq!(clustering["weak_modularity_threshold"].as_f64(), Some(0.0));
}

#[test]
fn analyze_rejects_invalid_clustering_algorithm() {
    let dir = tempfile::tempdir().unwrap();

    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let config_path = dir.path().join("bad-clarion.yaml");
    std::fs::write(
        &config_path,
        r"
analysis:
  clustering:
    algorithm: spectral
",
    )
    .expect("write invalid analyze config");

    let out = clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("invalid analyze config") && stderr.contains("algorithm"),
        "stderr should identify invalid clustering algorithm; got: {stderr}"
    );

    let conn = Connection::open(dir.path().join(".clarion/clarion.db")).unwrap();
    let run_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .expect("query run count");
    assert_eq!(run_count, 0, "invalid config must fail before BeginRun");
}

#[cfg(unix)]
#[test]
fn analyze_phase3_emits_subsystem_entities_and_edges() {
    let project_dir = run_phase3_fixture(
        &["auth_a", "auth_b", "billing_a", "billing_b"],
        &phase3_config(2),
    );
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();

    let subsystem_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE kind = 'subsystem'",
            [],
            |row| row.get(0),
        )
        .expect("query subsystem count");
    assert!(
        subsystem_count >= 2,
        "expected at least two subsystem entities, got {subsystem_count}"
    );

    let in_subsystem_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE kind = 'in_subsystem'",
            [],
            |row| row.get(0),
        )
        .expect("query in_subsystem edge count");
    assert_eq!(in_subsystem_edges, 4);

    let stats = latest_run_stats(project_dir.path());
    let clustering = &stats["clustering"];
    assert_eq!(clustering["status"].as_str(), Some("completed"));
    assert_eq!(clustering["subsystems_inserted"].as_u64(), Some(2));
    assert_eq!(clustering["in_subsystem_edges_inserted"].as_u64(), Some(4));
    assert_eq!(clustering["module_count"].as_u64(), Some(4));
    assert_eq!(clustering["module_edge_count"].as_u64(), Some(4));
    assert_eq!(clustering["configured_algorithm"].as_str(), Some("leiden"));
    assert_eq!(
        clustering["algorithm"].as_str(),
        Some("weighted_components")
    );
    assert!(clustering["modularity_score"].is_number());
}

#[cfg(unix)]
#[test]
fn analyze_phase3_is_deterministic_across_two_runs() {
    fn signature(project_root: &std::path::Path) -> Vec<(String, String)> {
        let conn = Connection::open(project_root.join(".clarion/clarion.db")).unwrap();
        conn.prepare("SELECT id, properties FROM entities WHERE kind = 'subsystem' ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    let config = phase3_config(2);
    let first = run_phase3_fixture(&["auth_a", "auth_b", "billing_a", "billing_b"], &config);
    let second = run_phase3_fixture(&["auth_a", "auth_b", "billing_a", "billing_b"], &config);

    assert_eq!(signature(first.path()), signature(second.path()));
}

#[cfg(unix)]
#[test]
fn analyze_phase3_skips_empty_graph_with_stats() {
    let project_dir = run_phase3_fixture(&["solo"], &phase3_config(2));
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let subsystem_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE kind = 'subsystem'",
            [],
            |row| row.get(0),
        )
        .expect("query subsystem count");
    assert_eq!(subsystem_count, 0);

    let stats = latest_run_stats(project_dir.path());
    let clustering = &stats["clustering"];
    assert_eq!(clustering["status"].as_str(), Some("skipped"));
    assert_eq!(
        clustering["skipped_reason"].as_str(),
        Some("no_module_dependency_edges")
    );
    assert_eq!(clustering["module_count"].as_u64(), Some(1));
    assert_eq!(clustering["module_edge_count"].as_u64(), Some(0));
    assert_eq!(clustering["subsystems_inserted"].as_u64(), Some(0));
    assert_eq!(clustering["in_subsystem_edges_inserted"].as_u64(), Some(0));
    assert!(clustering["modularity_score"].is_null());
}

#[cfg(unix)]
#[test]
fn analyze_phase3_emits_weak_modularity_fact_when_below_threshold() {
    let project_dir = run_phase3_fixture(&["weak_a", "weak_b"], &phase3_config(2));
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let row: (String, String, String, String, String) = conn
        .query_row(
            "SELECT rule_id, kind, severity, status, properties \
             FROM findings WHERE rule_id = 'CLA-FACT-CLUSTERING-WEAK-MODULARITY'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("query weak modularity finding");
    assert_eq!(row.0, "CLA-FACT-CLUSTERING-WEAK-MODULARITY");
    assert_eq!(row.1, "fact");
    assert_eq!(row.2, "INFO");
    assert_eq!(row.3, "open");
    let properties: serde_json::Value = serde_json::from_str(&row.4).expect("finding properties");
    assert_eq!(properties["threshold"].as_f64(), Some(0.3));
    assert_eq!(properties["algorithm"].as_str(), Some("leiden"));
    assert!(properties["modularity_score"].as_f64().unwrap_or(1.0) < 0.3);

    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["clustering"]["weak_modularity_finding_emitted"].as_bool(),
        Some(true)
    );
}

#[cfg(unix)]
#[test]
fn analyze_phase3_weak_modularity_threshold_zero_disables_fact() {
    let project_dir = run_phase3_fixture(
        &["weak_a", "weak_b"],
        r"
analysis:
  clustering:
    min_cluster_size: 2
    weak_modularity_threshold: 0.0
",
    );
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let finding_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM findings \
             WHERE rule_id = 'CLA-FACT-CLUSTERING-WEAK-MODULARITY'",
            [],
            |row| row.get(0),
        )
        .expect("query weak modularity finding count");
    assert_eq!(finding_count, 0);

    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["clustering"]["weak_modularity_threshold"].as_f64(),
        Some(0.0)
    );
    assert_eq!(
        stats["clustering"]["weak_modularity_finding_emitted"].as_bool(),
        Some(false)
    );
}

#[cfg(unix)]
#[test]
fn analyze_phase3_does_not_emit_weak_modularity_fact_when_threshold_is_met() {
    let project_dir = run_phase3_fixture(
        &["auth_a", "auth_b", "billing_a", "billing_b"],
        &phase3_config(2),
    );
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let finding_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM findings \
             WHERE rule_id = 'CLA-FACT-CLUSTERING-WEAK-MODULARITY'",
            [],
            |row| row.get(0),
        )
        .expect("query weak modularity finding count");
    assert_eq!(finding_count, 0);

    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["clustering"]["weak_modularity_finding_emitted"].as_bool(),
        Some(false)
    );
    assert!(
        stats["clustering"]["modularity_score"]
            .as_f64()
            .unwrap_or_default()
            >= 0.3
    );
}

#[cfg(unix)]
#[test]
fn analyze_phase3_min_cluster_size_drops_undersized_weighted_components() {
    let project_dir = run_phase3_fixture(
        &["auth_a", "auth_b", "billing_a", "billing_b"],
        &phase3_weighted_components_config(3),
    );
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let subsystem_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE kind = 'subsystem'",
            [],
            |row| row.get(0),
        )
        .expect("query subsystem count");
    assert_eq!(subsystem_count, 0);

    let in_subsystem_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE kind = 'in_subsystem'",
            [],
            |row| row.get(0),
        )
        .expect("query in_subsystem edge count");
    assert_eq!(in_subsystem_edges, 0);

    let stats = latest_run_stats(project_dir.path());
    let clustering = &stats["clustering"];
    assert_eq!(clustering["status"].as_str(), Some("skipped"));
    assert_eq!(
        clustering["skipped_reason"].as_str(),
        Some("no_clusters_emitted")
    );
    assert_eq!(clustering["subsystems_inserted"].as_u64(), Some(0));
    assert_eq!(clustering["in_subsystem_edges_inserted"].as_u64(), Some(0));
}

#[cfg(unix)]
#[test]
fn analyze_phase3_persists_weighted_components_algorithm_when_selected() {
    let project_dir = run_phase3_fixture(
        &["auth_a", "auth_b", "billing_a", "billing_b"],
        &phase3_weighted_components_config(2),
    );
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let properties_json: String = conn
        .query_row(
            "SELECT properties FROM entities \
             WHERE kind = 'subsystem' ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query subsystem properties");
    let properties: serde_json::Value =
        serde_json::from_str(&properties_json).expect("subsystem properties JSON");
    assert_eq!(
        properties["algorithm"].as_str(),
        Some("weighted_components")
    );

    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["clustering"]["algorithm"].as_str(),
        Some("weighted_components")
    );
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

#[cfg(unix)]
#[test]
fn analyze_filters_external_import_edges_before_writer_insert() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_imports_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    std::fs::write(
        project_dir.path().join("consumer.imp"),
        b"import internal\n",
    )
    .expect("write consumer.imp");
    std::fs::write(project_dir.path().join("internal.imp"), b"# internal\n")
        .expect("write internal.imp");

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let import_edges: Vec<(String, String)> = conn
        .prepare("SELECT from_id, to_id FROM edges WHERE kind = 'imports' ORDER BY from_id, to_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        import_edges,
        vec![(
            "importsfixture:module:consumer".to_owned(),
            "importsfixture:module:internal".to_owned(),
        )],
    );

    let stats_raw: String = conn
        .query_row("SELECT stats FROM runs LIMIT 1", [], |row| row.get(0))
        .expect("query runs.stats");
    let stats: serde_json::Value = serde_json::from_str(&stats_raw).expect("stats JSON");
    assert_eq!(
        stats["imports_skipped_external_total"].as_u64(),
        Some(1),
        "host should count the filtered external import; got {stats_raw}"
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
