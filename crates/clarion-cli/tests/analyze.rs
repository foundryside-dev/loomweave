//! `clarion analyze` Sprint-1 integration test.

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

[ontology.roles]
file_scope = ["module"]
callable = ["function"]
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

[ontology.roles]
file_scope = ["module"]
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

[ontology.roles]
file_scope = ["module"]
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

#[cfg(unix)]
const CATEGORISED_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
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
                "name": "clarion-plugin-categorised",
                "version": "0.1.0",
                "ontology_version": "0.1.0",
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
                        "id": "catfixture:module:app",
                        "kind": "module",
                        "qualified_name": "app",
                        "source": {
                            "file_path": path,
                            "source_range": {
                                "start_line": 1,
                                "start_col": 0,
                                "end_line": 3,
                                "end_col": 0
                            },
                        },
                    },
                    {
                        "id": "catfixture:function:app.main",
                        "kind": "function",
                        "qualified_name": "app.main",
                        "source": {
                            "file_path": path,
                            "source_range": {
                                "start_line": 1,
                                "start_col": 0,
                                "end_line": 2,
                                "end_col": 8
                            },
                        },
                        "parent_id": "catfixture:module:app",
                        "tags": ["entry-point"],
                        "docstring": "Launches service",
                    },
                ],
                "edges": [
                    {
                        "kind": "contains",
                        "from_id": "catfixture:module:app",
                        "to_id": "catfixture:function:app.main",
                    }
                ],
                "stats": {},
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
"#;

#[cfg(unix)]
const CATEGORISED_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-categorised"
plugin_id = "catfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-categorised"
language = "catfixture"
extensions = ["cat"]

[capabilities.runtime]
expected_max_rss_mb = 128
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module", "function"]
edge_kinds = ["contains"]
rule_id_prefix = "CLA-CAT-"
ontology_version = "0.1.0"

[ontology.roles]
file_scope = ["module"]
callable = ["function"]
"#;

#[cfg(unix)]
fn write_categorised_plugin(plugin_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("clarion-plugin-categorised");
    std::fs::write(&plugin_script, CATEGORISED_PLUGIN_SCRIPT)
        .expect("write categorised plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat categorised plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod categorised plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), CATEGORISED_PLUGIN_MANIFEST)
        .expect("write categorised plugin manifest");
}

#[cfg(unix)]
fn spawn_embedding_mock() -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let mut header_end = None;
        while header_end.is_none() {
            let read = stream.read(&mut chunk).expect("read headers");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            header_end = buffer
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| i + 4);
        }
        let Some(header_end) = header_end else {
            return String::from_utf8_lossy(&buffer).into_owned();
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len().saturating_sub(header_end) < content_length {
            let read = stream.read(&mut chunk).expect("read body");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind embedding mock");
    let addr = listener.local_addr().expect("mock addr");
    listener
        .set_nonblocking(true)
        .expect("nonblocking embedding mock");
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut requests = Vec::new();
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_http_request(&mut stream);
                    let body = request.split("\r\n\r\n").nth(1).unwrap_or("{}");
                    let payload: serde_json::Value =
                        serde_json::from_str(body).expect("embedding request json");
                    let count = payload["input"].as_array().map_or(0, Vec::len);
                    let data: Vec<serde_json::Value> = (0..count)
                        .map(|index| {
                            let first_dim =
                                f64::from(u32::try_from(index + 1).expect("fixture index fits"));
                            serde_json::json!({
                                "object": "embedding",
                                "index": index,
                                "embedding": [first_dim, 1.0],
                            })
                        })
                        .collect();
                    let response = serde_json::json!({
                        "object": "list",
                        "data": data,
                        "model": "test-embed",
                    })
                    .to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                        response.len(),
                        response
                    )
                    .expect("write embedding response");
                    requests.push(request);
                    return requests;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(err) => panic!("embedding mock accept failed: {err}"),
            }
        }
        requests
    });
    (format!("http://{addr}"), handle)
}

#[cfg(unix)]
#[test]
fn analyze_persists_plugin_tags_and_populates_embedding_sidecar() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_categorised_plugin(plugin_dir.path());
    let (embedding_url, embedding_server) = spawn_embedding_mock();

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    std::fs::write(
        project_dir.path().join("app.cat"),
        "def main():\n    pass\n",
    )
    .expect("write categorised fixture source");
    let config_path = project_dir.path().join("clarion.yaml");
    std::fs::write(
        &config_path,
        format!(
            r"
semantic_search:
  enabled: true
  allow_live_provider: true
  endpoint_url: {embedding_url}
  model_id: test-embed
  dimensions: 2
  api_key_env: TEST_EMBEDDING_KEY
  timeout_seconds: 2
  session_token_ceiling: 10000
"
        ),
    )
    .expect("write semantic search config");

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .env("TEST_EMBEDDING_KEY", "test-key")
        .assert()
        .success();

    let requests = embedding_server.join().expect("embedding mock thread");
    assert_eq!(
        requests.len(),
        1,
        "analyze should call the embedding provider"
    );
    assert!(
        requests[0].contains("Launches service"),
        "embedding text should include plugin docstring; request was {}",
        requests[0]
    );

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let tag_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entity_tags \
             WHERE entity_id = 'catfixture:function:app.main' \
               AND plugin_id = 'catfixture' \
               AND tag = 'entry-point'",
            [],
            |row| row.get(0),
        )
        .expect("query persisted tags");
    assert_eq!(tag_count, 1, "plugin-emitted tags must be persisted");

    let sidecar = project_dir.path().join(".clarion/embeddings.db");
    assert!(sidecar.exists(), "analyze should create embeddings sidecar");
    let sidecar_conn = Connection::open(sidecar).unwrap();
    let embedding_count: i64 = sidecar_conn
        .query_row(
            "SELECT COUNT(*) FROM entity_embeddings \
             WHERE entity_id = 'catfixture:function:app.main' \
               AND model_id = 'test-embed'",
            [],
            |row| row.get(0),
        )
        .expect("query sidecar embeddings");
    assert_eq!(
        embedding_count, 1,
        "function embedding should be present after analyze"
    );
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

/// `analyze` self-migrates a stale DB rather than hard-failing (WS9). `install`
/// is the usual migrator, but a binary upgrade that adds a migration the run path
/// writes (`runs.analyzed_at_commit`) must still work if the operator runs
/// `analyze` before re-`install`. Simulate a pre-0007 (v6) DB by dropping the new
/// column and rewinding the migration ledger, then assert `analyze` succeeds and
/// the schema is brought current.
#[test]
fn analyze_migrates_a_stale_db_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();

    let db = dir.path().join(".clarion/clarion.db");
    // Rewind to the pre-0007 (v6) shape: no `analyzed_at_commit`, no v7 ledger
    // row, user_version back to 6 — exactly an upgraded-binary-vs-old-DB state.
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "ALTER TABLE runs DROP COLUMN analyzed_at_commit;\n\
             DELETE FROM schema_migrations WHERE version = 7;\n\
             PRAGMA user_version = 6;",
        )
        .unwrap();
        let uv: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, 6, "precondition: DB rewound to v6");
    }

    clarion_bin()
        .args(["analyze"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();

    // The run path ran (a row exists with the column populated by begin_run) and
    // the schema is current again.
    let conn = Connection::open(&db).unwrap();
    let uv: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        uv,
        i64::from(clarion_storage::schema::CURRENT_SCHEMA_VERSION),
        "analyze must apply all pending migrations"
    );
    let has_column: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('runs') WHERE name = 'analyzed_at_commit'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_column, 1, "analyzed_at_commit must exist after analyze");
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

/// Set up a phase3 project + plugin and run analyze once. Returns BOTH tempdirs
/// (project, plugin) so the caller can keep the plugin on `PATH` and re-run
/// `analyze` after mutating the source tree — `run_phase3_fixture` drops the
/// plugin dir, which a deletion-detection re-run needs alive.
#[cfg(unix)]
fn phase3_project_for_rerun(stems: &[&str]) -> (tempfile::TempDir, tempfile::TempDir, String) {
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
    std::fs::write(&config_path, phase3_config(2)).expect("write phase3 config");
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    run_phase3_analyze(project_dir.path(), &config_path, &plugin_path);
    (project_dir, plugin_dir, config_path.display().to_string())
}

#[cfg(unix)]
fn run_phase3_analyze(
    project_root: &std::path::Path,
    config_path: &std::path::Path,
    plugin_path: &std::ffi::OsStr,
) {
    clarion_bin()
        .args(["analyze", "--config"])
        .arg(config_path)
        .arg(project_root)
        .env("PATH", plugin_path)
        .assert()
        .success();
}

/// A one-shot mock Filigree `/api/v1/scan-results` sink. Captures every POST body
/// via an idle-timeout accept loop, so it tolerates a run emitting one batch or
/// two (Phase 8 + Phase 8c) without a hard-coded connection count, and stops
/// early as soon as a captured body contains `needle`. Returns the bound base
/// URL and the join handle yielding the captured request strings.
#[cfg(unix)]
fn spawn_capturing_filigree_mock(
    needle: &'static str,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock filigree");
    let addr = listener.local_addr().expect("local addr");
    listener
        .set_nonblocking(true)
        .expect("nonblocking mock listener");
    let handle = std::thread::spawn(move || {
        let body = r#"{"files_created":0,"files_updated":0,"findings_created":0,"findings_updated":0,"new_finding_ids":[],"observations_created":0,"observations_failed":0,"warnings":[]}"#;
        let mut requests: Vec<String> = Vec::new();
        let start = Instant::now();
        let mut last = Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                    let mut buf = [0_u8; 16384];
                    let read = stream.read(&mut buf).unwrap_or(0);
                    let captured = String::from_utf8_lossy(&buf[..read]).into_owned();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let hit = captured.contains(needle);
                    requests.push(captured);
                    // Fast path: stop as soon as the awaited batch lands.
                    if hit {
                        break;
                    }
                    last = Instant::now();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Otherwise return once the client has been idle past the
                    // window (a batch landed and no more is coming), or after a
                    // hard cap so a never-connecting client can't hang the test.
                    if (!requests.is_empty() && last.elapsed() > Duration::from_secs(3))
                        || start.elapsed() > Duration::from_secs(30)
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
        requests
    });
    (format!("http://{addr}"), handle)
}

/// clarion-ef8f64d5fd: the post-`CommitRun` deletion finding
/// (`CLA-FACT-ENTITY-DELETED`) must reach Filigree in the SAME run, not only the
/// store. Phase-8 emission runs *before* `CommitRun`, while the SEI mint pass
/// persists deletion findings *after* it, so without a second emission pass the
/// finding is stranded store-only even with `emit_findings=true`. End-to-end:
/// run 1 establishes the prior index (no Filigree); run 2 (file removed, emit
/// enabled) must POST a body containing the deletion finding to a mock Filigree.
/// The deletion finding anchors to the deleted entity's own never-pruned,
/// path-bearing row, so it survives the `findings_for_emit` JOIN and the
/// `wire_finding` path filter.
#[cfg(unix)]
#[test]
fn analyze_emits_post_commit_deletion_finding_to_filigree() {
    // Run 1: establish the prior index with a plain (no-Filigree) config so the
    // mock only sees run 2's POSTs.
    let (project_dir, plugin_dir, config_path) =
        phase3_project_for_rerun(&["auth_a", "auth_b", "billing_a", "billing_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    let (base_url, server) = spawn_capturing_filigree_mock("CLA-FACT-ENTITY-DELETED");

    // Run 2: rewrite the config to enable emission against the mock, delete a
    // source file, and re-run.
    std::fs::write(&config_path, phase3_config_with_filigree(2, &base_url))
        .expect("rewrite config with filigree emission enabled");
    std::fs::remove_file(project_dir.path().join("billing_a.p3")).expect("delete a source file");
    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let requests = server.join().expect("mock server thread");
    let combined = requests.join("\n---POST BOUNDARY---\n");
    assert!(
        combined.contains("CLA-FACT-ENTITY-DELETED"),
        "the post-commit deletion finding must reach Filigree in the same run; \
         captured {} POST(s): {combined}",
        requests.len(),
    );
}

/// clarion-ef8f64d5fd (tier half): post-`CommitRun` tier findings reach Filigree
/// in the same run too. They anchor to a synthetic subsystem entity with no
/// `source_file_path`, so the Phase-8c pass posts them against the project-root
/// fallback path (mirroring the `core:project:*` anchor) and flags them
/// `synthetic_anchor`. Run 1 builds the subsystems; tiers are seeded between runs
/// (analyze never writes them — the enrich-only axiom); run 2 (emit enabled)
/// computes the tier finding post-commit and Phase 8c POSTs it.
#[cfg(unix)]
#[test]
fn analyze_emits_post_commit_tier_finding_to_filigree_at_project_anchor() {
    let (project_dir, plugin_dir, config_path) =
        phase3_project_for_rerun(&["auth_a", "auth_b", "billing_a", "billing_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    {
        let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
        // Two subsystems → two tier findings, both anchored to the project root.
        // auth disagrees (MIXING); billing agrees (UNANIMOUS). They share
        // (rule-family, path, null line) but carry subsystem-distinct messages —
        // Filigree's intake is content-keyed (includes the message), so both
        // persist distinctly rather than collapsing onto the shared path.
        seed_wardline_tier(&conn, "phase3fixture:module:auth_a", "public");
        seed_wardline_tier(&conn, "phase3fixture:module:auth_b", "internal");
        seed_wardline_tier(&conn, "phase3fixture:module:billing_a", "trusted");
        seed_wardline_tier(&conn, "phase3fixture:module:billing_b", "trusted");
    }

    let (base_url, server) = spawn_capturing_filigree_mock("CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS");

    std::fs::write(&config_path, phase3_config_with_filigree(2, &base_url))
        .expect("rewrite config with filigree emission enabled");
    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let requests = server.join().expect("mock server thread");
    let posted = requests
        .iter()
        .find(|r| r.contains("CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS"))
        .unwrap_or_else(|| {
            panic!(
                "the post-commit tier finding must reach Filigree; captured {} POST(s): {}",
                requests.len(),
                requests.join("\n---\n")
            )
        });
    // Both subsystems' tier findings ride the one Phase-8c batch...
    assert!(
        posted.contains("CLA-FACT-TIER-SUBSYSTEM-MIXING")
            && posted.contains("CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS"),
        "both tier findings reach Filigree in one batch: {posted}"
    );
    // ...anchored to the project root and flagged synthetic (non-file) so a
    // consumer never reads the shared path as a real location...
    assert!(
        posted.contains("\"synthetic_anchor\":true"),
        "tier findings are flagged as synthetic anchors: {posted}"
    );
    assert!(
        posted.contains(&project_dir.path().display().to_string()),
        "tier findings are anchored to the project root path: {posted}"
    );
    // ...and carry subsystem-distinct messages (≥2 distinct `Subsystem <id>`
    // anchors), which is what keeps them distinct under Filigree's content key.
    let subsystem_mentions: std::collections::BTreeSet<&str> = posted
        .match_indices("core:subsystem:")
        .map(|(i, _)| &posted[i..(i + "core:subsystem:".len() + 8).min(posted.len())])
        .collect();
    assert!(
        subsystem_mentions.len() >= 2,
        "two distinct subsystem anchors keep the findings content-distinct: {subsystem_mentions:?} in {posted}"
    );
}

/// REQ-ANALYZE-04 verification (verbatim): run analyze, delete a file, re-run;
/// assert a `CLA-FACT-ENTITY-DELETED` finding per previously-extracted entity in
/// the deleted file — and no false positives for entities still present.
#[cfg(unix)]
#[test]
fn analyze_emits_entity_deleted_finding_when_file_removed() {
    let (project_dir, plugin_dir, config_path) =
        phase3_project_for_rerun(&["auth_a", "auth_b", "billing_a", "billing_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    std::fs::remove_file(project_dir.path().join("billing_a.p3")).expect("delete a source file");
    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    // The plugin's `module` entity carries the canonical finding shape.
    let (kind, severity, status): (String, String, String) = conn
        .query_row(
            "SELECT kind, severity, status FROM findings \
             WHERE rule_id = 'CLA-FACT-ENTITY-DELETED' \
               AND entity_id = 'phase3fixture:module:billing_a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("entity-deleted finding for the deleted module");
    assert_eq!(kind, "fact");
    assert_eq!(severity, "INFO");
    assert_eq!(status, "open");

    // Deleting one source file orphans exactly its two previously-extracted
    // entities — the core-minted `core:file:*` and the plugin `module` — and
    // nothing belonging to the surviving files.
    let deleted: std::collections::BTreeSet<String> = conn
        .prepare("SELECT entity_id FROM findings WHERE rule_id = 'CLA-FACT-ENTITY-DELETED'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        deleted,
        std::collections::BTreeSet::from([
            "core:file:billing_a.p3".to_owned(),
            "phase3fixture:module:billing_a".to_owned(),
        ]),
        "only the deleted file's entities should be flagged"
    );
}

/// REQ-ANALYZE-04: a guidance sheet whose `guides` edge targets a deleted entity
/// produces `CLA-FACT-GUIDANCE-ORPHAN`, and the deleted entity's cached summaries
/// are invalidated. Both halves are injected between runs (the fixture plugin
/// emits neither guidance sheets nor summaries), then a file is deleted + re-run.
#[cfg(unix)]
#[test]
fn analyze_emits_guidance_orphan_and_invalidates_summary_cache_on_deletion() {
    let (project_dir, plugin_dir, config_path) =
        phase3_project_for_rerun(&["auth_a", "auth_b", "billing_a", "billing_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");
    let target = "phase3fixture:module:billing_a";

    // Inject a guidance sheet that `guides` the soon-to-be-deleted entity, plus a
    // cached summary for it. Entities/edges are never pruned, so these survive the
    // re-run; the deletion path must orphan the guidance and clear the summary.
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
             VALUES ('core:guidance:g1', 'core', 'guidance', 'g1', 'g1', '{}', \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence) \
             VALUES ('guides', 'core:guidance:g1', ?1, 'resolved')",
            [target],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO summary_cache \
             (entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint, \
              summary_json, cost_usd, tokens_input, tokens_output, created_at, last_accessed_at, \
              caller_count, fan_out) \
             VALUES (?1, 'h', 'tmpl', 'tier', 'fp', '{}', 0.0, 0, 0, \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, 0)",
            [target],
        )
        .unwrap();
    }

    std::fs::remove_file(project_dir.path().join("billing_a.p3")).expect("delete a source file");
    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    let (rule_id, severity, anchor, related): (String, String, String, String) = conn
        .query_row(
            "SELECT rule_id, severity, entity_id, related_entities \
             FROM findings WHERE rule_id = 'CLA-FACT-GUIDANCE-ORPHAN'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query guidance-orphan finding");
    assert_eq!(rule_id, "CLA-FACT-GUIDANCE-ORPHAN");
    assert_eq!(severity, "WARN");
    assert_eq!(anchor, "core:guidance:g1");
    let related: serde_json::Value = serde_json::from_str(&related).unwrap();
    assert_eq!(related, serde_json::json!([target]));

    let cached: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM summary_cache WHERE entity_id = ?1",
            [target],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        cached, 0,
        "deleted entity's summary cache must be invalidated"
    );
}

/// T4a (WS6): a guidance sheet whose `match_rules` carries `{"type":"entity","id":X}`
/// pointing at a deleted entity also produces `CLA-FACT-GUIDANCE-ORPHAN`. When the
/// SAME deleted target is reachable via BOTH a `guides` edge and a `match_rule`, only
/// one finding is emitted for that (sheet, target) pair (idempotent run-scoped id).
#[cfg(unix)]
#[test]
fn analyze_emits_guidance_orphan_for_match_rule_entity_and_dedupes() {
    let (project_dir, plugin_dir, config_path) =
        phase3_project_for_rerun(&["auth_a", "auth_b", "billing_a", "billing_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");
    let target = "phase3fixture:module:billing_a";

    {
        let conn = Connection::open(&db_path).unwrap();
        // g_match: orphans `target` via a match_rule {type:entity, id:target} only.
        let props = serde_json::json!({
            "match_rules": [{ "type": "entity", "id": target }],
            "authored_at": "2026-01-01T00:00:00.000Z",
        })
        .to_string();
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
             VALUES ('core:guidance:g_match', 'core', 'guidance', 'g_match', 'g_match', ?1, \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [&props],
        )
        .unwrap();
        // g_both: orphans `target` via a `guides` edge AND a match_rule → one finding.
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
             VALUES ('core:guidance:g_both', 'core', 'guidance', 'g_both', 'g_both', ?1, \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [&props],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence) \
             VALUES ('guides', 'core:guidance:g_both', ?1, 'resolved')",
            [target],
        )
        .unwrap();
    }

    std::fs::remove_file(project_dir.path().join("billing_a.p3")).expect("delete a source file");
    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    // g_match emits exactly one orphan finding for target.
    let match_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM findings \
             WHERE rule_id = 'CLA-FACT-GUIDANCE-ORPHAN' AND entity_id = 'core:guidance:g_match'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(match_count, 1, "match_rule entity orphan should emit");

    // g_both: guides-edge + match_rule to the same target ⇒ exactly one finding.
    let both_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM findings \
             WHERE rule_id = 'CLA-FACT-GUIDANCE-ORPHAN' AND entity_id = 'core:guidance:g_both'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        both_count, 1,
        "guides-edge + match_rule to same target must dedupe to one finding"
    );
}

/// T4a (WS6): `CLA-FACT-GUIDANCE-EXPIRED` fires for a sheet whose `expires` is in
/// the past, and does NOT fire for a future `expires` or a sheet with no `expires`.
/// Runs on every analyze (independent of deletions), so this re-runs with no source
/// change. Severity INFO, confidence 1.0.
#[cfg(unix)]
#[test]
fn analyze_emits_guidance_expired_for_past_expiry_only() {
    let (project_dir, plugin_dir, config_path) = phase3_project_for_rerun(&["auth_a", "auth_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");

    {
        let conn = Connection::open(&db_path).unwrap();
        let insert = |conn: &Connection, slug: &str, expires: Option<&str>| {
            let mut props = serde_json::json!({ "authored_at": "2026-01-01T00:00:00.000Z" });
            if let Some(e) = expires {
                props["expires"] = serde_json::Value::String(e.to_owned());
            }
            conn.execute(
                "INSERT INTO entities \
                 (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
                 VALUES (?1, 'core', 'guidance', ?2, ?2, ?3, \
                         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                rusqlite::params![format!("core:guidance:{slug}"), slug, props.to_string()],
            )
            .unwrap();
        };
        insert(&conn, "g_past", Some("2020-01-01T00:00:00.000Z"));
        insert(&conn, "g_future", Some("2999-01-01T00:00:00.000Z"));
        insert(&conn, "g_none", None);
    }

    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    let anchors: Vec<String> = conn
        .prepare("SELECT entity_id FROM findings WHERE rule_id = 'CLA-FACT-GUIDANCE-EXPIRED'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(anchors, vec!["core:guidance:g_past".to_owned()]);

    let (severity, confidence): (String, f64) = conn
        .query_row(
            "SELECT severity, confidence FROM findings \
             WHERE rule_id = 'CLA-FACT-GUIDANCE-EXPIRED'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(severity, "INFO");
    assert!((confidence - 1.0).abs() < f64::EPSILON);
}

/// T4a (WS6): EXPIRED fires even under `--no-sei` — the guidance-staleness pass is
/// independent of the SEI mint pass (deletion detection is SEI-gated; staleness is
/// not). Guards the load-bearing placement decision.
#[cfg(unix)]
#[test]
fn analyze_emits_guidance_expired_under_no_sei() {
    let (project_dir, plugin_dir, config_path) = phase3_project_for_rerun(&["auth_a", "auth_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
             VALUES ('core:guidance:g_past', 'core', 'guidance', 'g_past', 'g_past', ?1, \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [&serde_json::json!({
                "authored_at": "2026-01-01T00:00:00.000Z",
                "expires": "2020-01-01T00:00:00.000Z",
            })
            .to_string()],
        )
        .unwrap();
    }

    clarion_bin()
        .args(["analyze", "--config"])
        .arg(std::path::Path::new(&config_path))
        .arg("--no-sei")
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-FACT-GUIDANCE-EXPIRED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "EXPIRED must fire under --no-sei");
}

/// T4a (WS6): `CLA-FACT-GUIDANCE-CHURN-STALE` asymmetric threshold. A pinned sheet
/// matching entities whose aggregate `git_churn_count` is in [20, 49] fires; an
/// identical non-pinned sheet at the same churn does not. Below 20 neither fires;
/// at/above 50 both fire. With churn unpopulated (production), nothing fires.
#[cfg(unix)]
#[test]
fn analyze_emits_guidance_churn_stale_with_asymmetric_pinned_threshold() {
    let (project_dir, plugin_dir, config_path) = phase3_project_for_rerun(&["auth_a", "auth_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");

    // Seed git_churn_count on the matched module via properties JSON (the analyze
    // pipeline does not populate it). A `kind:module` match_rule selects both
    // auth modules; we control the aggregate by choosing the per-module value.
    let seed_run = |churn_each: i64, pinned: bool, slug: &str| {
        let conn = Connection::open(&db_path).unwrap();
        // Set churn on auth_a + auth_b (both kind=module) via properties merge.
        for stem in ["auth_a", "auth_b"] {
            conn.execute(
                "UPDATE entities SET properties = json_set(properties, '$.git_churn_count', ?2) \
                 WHERE id = ?1",
                rusqlite::params![format!("phase3fixture:module:{stem}"), churn_each],
            )
            .unwrap();
        }
        let props = serde_json::json!({
            "match_rules": [{ "type": "kind", "value": "module" }],
            "authored_at": "2026-01-01T00:00:00.000Z",
            "pinned": pinned,
        })
        .to_string();
        conn.execute(
            "INSERT OR REPLACE INTO entities \
             (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
             VALUES (?1, 'core', 'guidance', ?2, ?2, ?3, \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![format!("core:guidance:{slug}"), slug, props],
        )
        .unwrap();
        drop(conn);
        run_phase3_analyze(
            project_dir.path(),
            std::path::Path::new(&config_path),
            &plugin_path,
        );
        let conn = Connection::open(&db_path).unwrap();
        let fired: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM findings \
                 WHERE rule_id = 'CLA-FACT-GUIDANCE-CHURN-STALE' AND entity_id = ?1",
                rusqlite::params![format!("core:guidance:{slug}")],
                |row| row.get(0),
            )
            .unwrap();
        fired > 0
    };

    // Aggregate 30 (15 each): pinned fires (>=20), non-pinned does not (<50).
    assert!(seed_run(15, true, "g_pinned_30"), "pinned@30 should fire");
    assert!(
        !seed_run(15, false, "g_plain_30"),
        "non-pinned@30 should NOT fire"
    );
    // Aggregate 10 (5 each): neither fires (<20).
    assert!(
        !seed_run(5, true, "g_pinned_10"),
        "pinned@10 should NOT fire"
    );
    // Aggregate 60 (30 each): both fire (>=50).
    assert!(seed_run(30, true, "g_pinned_60"), "pinned@60 should fire");
    assert!(
        seed_run(30, false, "g_plain_60"),
        "non-pinned@60 should fire"
    );

    let conn = Connection::open(&db_path).unwrap();
    let (severity, confidence): (String, f64) = conn
        .query_row(
            "SELECT severity, confidence FROM findings \
             WHERE rule_id = 'CLA-FACT-GUIDANCE-CHURN-STALE' LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(severity, "WARN");
    assert!((confidence - 0.7).abs() < 1e-9);
}

/// T4a (WS6): honest-empty churn. With `git_churn_count` unpopulated (the
/// production reality — analyze never writes it), CHURN-STALE does not fire even
/// for a sheet that matches many entities.
#[cfg(unix)]
#[test]
fn analyze_guidance_churn_stale_is_honest_empty_without_churn() {
    let (project_dir, plugin_dir, config_path) = phase3_project_for_rerun(&["auth_a", "auth_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");

    {
        let conn = Connection::open(&db_path).unwrap();
        let props = serde_json::json!({
            "match_rules": [{ "type": "kind", "value": "module" }],
            "authored_at": "2026-01-01T00:00:00.000Z",
            "pinned": true,
        })
        .to_string();
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
             VALUES ('core:guidance:g_inert', 'core', 'guidance', 'g_inert', 'g_inert', ?1, \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [&props],
        )
        .unwrap();
    }

    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-FACT-GUIDANCE-CHURN-STALE'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "no churn populated ⇒ CHURN-STALE inert");
}

#[cfg(unix)]
fn write_wardline_manifest(project_root: &std::path::Path, tier_content: &str) {
    std::fs::write(
        project_root.join("wardline.yaml"),
        format!(
            r#"version: 1
tiers:
  integral:
    paths:
      - auth_a.p3
    content: "{tier_content}"
boundaries:
  payment_api:
    paths:
      - billing_a.p3
annotation_groups:
  secrets:
    paths:
      - auth_b.p3
"#
        ),
    )
    .expect("write wardline.yaml");
}

#[cfg(unix)]
fn write_real_wardline_output_fixture(project_root: &std::path::Path) {
    std::fs::create_dir_all(project_root.join("src/payments"))
        .expect("create Wardline overlay dir");
    std::fs::write(
        project_root.join("wardline.yaml"),
        r#"tiers:
  - id: AUDIT_TRAIL
    tier: 1
    description: "Fully audited code"
  - id: EXTERNAL_RAW
    tier: 4
    description: "Unvetted external input"

module_tiers:
  - path: "src/core"
    default_taint: "AUDIT_TRAIL"
  - path: "src/integrations"
    default_taint: "EXTERNAL_RAW"
"#,
    )
    .expect("write real wardline.yaml");
    std::fs::write(
        project_root.join("wardline.fingerprint.json"),
        r#"{
  "python_version": "3.12",
  "generated_at": "2026-03-01T00:00:00Z",
  "coverage": {
    "annotated": 2,
    "total": 3,
    "ratio": 0.66,
    "tier1_annotated": 1,
    "tier1_total": 1
  },
  "fingerprints": [
    {
      "qualified_name": "core.auth.validate_token",
      "module": "core.auth",
      "decorators": ["wardline.tier"],
      "annotation_hash": "sha256:aaa111",
      "tier_context": 1,
      "artefact_class": "policy"
    },
    {
      "qualified_name": "integrations.handler.process",
      "module": "integrations.handler",
      "decorators": ["wardline.tier", "wardline.external_boundary"],
      "annotation_hash": "sha256:bbb222",
      "tier_context": 4,
      "boundary_transition": "shape_validation",
      "artefact_class": "enforcement"
    }
  ]
}
"#,
    )
    .expect("write real wardline.fingerprint.json");
    std::fs::write(
        project_root.join("wardline.exceptions.json"),
        r#"{
  "exceptions": [
    {
      "id": "EXC-001",
      "rule": "PY-WL-001",
      "taint_state": "EXTERNAL_RAW",
      "location": "src/integrations/handler.py::process",
      "exceptionability": "STANDARD",
      "severity_at_grant": "ERROR",
      "rationale": "Legacy integration pending migration",
      "reviewer": "j.smith",
      "expires": "2027-12-01"
    }
  ]
}
"#,
    )
    .expect("write real wardline.exceptions.json");
    std::fs::write(
        project_root.join("src/payments/wardline.overlay.yaml"),
        r#"overlay_for: "src/payments"

boundaries:
  - function: "process_payment"
    transition: "construction"
    from_tier: 1
    to_tier: 3
  - function: "validate_receipt"
    transition: "shape_validation"
    from_tier: 3
    to_tier: 2
"#,
    )
    .expect("write real wardline.overlay.yaml");
}

#[cfg(unix)]
#[test]
fn analyze_generates_pinned_wardline_derived_guidance() {
    let (project_dir, plugin_dir, config_path) =
        phase3_project_for_rerun(&["auth_a", "auth_b", "billing_a"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");
    write_wardline_manifest(project_dir.path(), "Keep integral code isolated.");

    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    let rows: Vec<(String, serde_json::Value)> = conn
        .prepare(
            "SELECT id, properties FROM entities \
             WHERE kind = 'guidance' AND id LIKE 'core:guidance:wardline-%' \
             ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let raw: String = row.get(1)?;
            Ok((id, serde_json::from_str(&raw).unwrap()))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(
        ids,
        vec![
            "core:guidance:wardline-annotation-group-secrets".to_owned(),
            "core:guidance:wardline-boundary-payment_api".to_owned(),
            "core:guidance:wardline-tier-integral".to_owned(),
        ]
    );

    let tier = rows
        .iter()
        .find(|(id, _)| id == "core:guidance:wardline-tier-integral")
        .expect("tier guidance")
        .1
        .clone();
    assert_eq!(tier["provenance"], "wardline_derived");
    assert_eq!(tier["pinned"], true);
    assert_eq!(tier["wardline_kind"], "tier");
    assert_eq!(tier["wardline_key"], "integral");
    assert_eq!(tier["content"], "Keep integral code isolated.");
    assert!(
        tier["wardline_manifest_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );
    assert_eq!(
        tier["match_rules"][0],
        serde_json::json!({"type":"path","pattern":"auth_a.p3"})
    );
}

#[cfg(unix)]
#[test]
fn analyze_accepts_real_wardline_output_bundle() {
    let (project_dir, plugin_dir, config_path) = phase3_project_for_rerun(&["seed"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");
    write_real_wardline_output_fixture(project_dir.path());

    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    let rows: Vec<(String, serde_json::Value)> = conn
        .prepare(
            "SELECT id, properties FROM entities \
             WHERE kind = 'guidance' AND id LIKE 'core:guidance:wardline-%' \
             ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let raw: String = row.get(1)?;
            Ok((id, serde_json::from_str(&raw).unwrap()))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
    assert!(ids.contains(&"core:guidance:wardline-tier-src-core-AUDIT_TRAIL".to_owned()));
    assert!(ids.contains(&"core:guidance:wardline-tier-src-integrations-EXTERNAL_RAW".to_owned()));
    assert!(
        ids.contains(&"core:guidance:wardline-boundary-src-payments-process_payment".to_owned())
    );
    assert!(
        ids.contains(&"core:guidance:wardline-boundary-src-payments-validate_receipt".to_owned())
    );
    assert!(ids.contains(&"core:guidance:wardline-annotation-group-wardline.tier".to_owned()));
    assert!(ids.contains(
        &"core:guidance:wardline-annotation-group-wardline.external_boundary".to_owned()
    ));

    let tier = rows
        .iter()
        .find(|(id, _)| id == "core:guidance:wardline-tier-src-core-AUDIT_TRAIL")
        .expect("module-tier guidance")
        .1
        .clone();
    assert_eq!(tier["provenance"], "wardline_derived");
    assert_eq!(tier["pinned"], true);
    assert_eq!(tier["wardline_kind"], "tier");
    assert_eq!(tier["wardline_key"], "src/core-AUDIT_TRAIL");
    assert_eq!(
        tier["match_rules"][0],
        serde_json::json!({"type":"path","pattern":"src/core/**"})
    );
    assert_eq!(tier["wardline_fingerprint_count"], 2);
    assert_eq!(tier["wardline_exception_count"], 1);
    assert!(
        tier["wardline_fingerprint_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );
    assert!(
        tier["wardline_exceptions_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );

    let boundary = rows
        .iter()
        .find(|(id, _)| id == "core:guidance:wardline-boundary-src-payments-process_payment")
        .expect("overlay boundary guidance")
        .1
        .clone();
    assert_eq!(boundary["scope_level"], "subsystem");
    assert!(
        boundary["content"]
            .as_str()
            .unwrap()
            .contains("construction")
    );
    assert_eq!(
        boundary["match_rules"][0],
        serde_json::json!({"type":"path","pattern":"src/payments/**"})
    );

    let group = rows
        .iter()
        .find(|(id, _)| id == "core:guidance:wardline-annotation-group-wardline.tier")
        .expect("fingerprint annotation-group guidance")
        .1
        .clone();
    assert_eq!(group["scope_level"], "project");
    assert_eq!(
        group["match_rules"][0],
        serde_json::json!({"type":"wardline_group","name":"wardline.tier"})
    );
}

#[cfg(unix)]
#[test]
fn analyze_preserves_wardline_override_and_emits_guidance_stale() {
    let (project_dir, plugin_dir, config_path) =
        phase3_project_for_rerun(&["auth_a", "auth_b", "billing_a"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");
    write_wardline_manifest(project_dir.path(), "Initial Wardline guidance.");
    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    {
        let conn = Connection::open(&db_path).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT properties FROM entities WHERE id = 'core:guidance:wardline-tier-integral'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut props: serde_json::Value = serde_json::from_str(&raw).unwrap();
        props["content"] = serde_json::Value::String("Operator override text.".to_owned());
        conn.execute(
            "UPDATE entities SET properties = ?1 WHERE id = 'core:guidance:wardline-tier-integral'",
            [props.to_string()],
        )
        .unwrap();
    }

    write_wardline_manifest(project_dir.path(), "Updated Wardline guidance.");
    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT properties FROM entities WHERE id = 'core:guidance:wardline-tier-integral'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let props: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(props["content"], "Operator override text.");
    assert_eq!(props["provenance"], "wardline_derived_overridden");

    let (severity, confidence, evidence): (String, f64, String) = conn
        .query_row(
            "SELECT severity, confidence, evidence FROM findings \
             WHERE rule_id = 'CLA-FACT-GUIDANCE-STALE' \
               AND entity_id = 'core:guidance:wardline-tier-integral'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("guidance-stale finding");
    assert_eq!(severity, "WARN");
    assert!((confidence - 1.0).abs() < f64::EPSILON);
    let evidence: serde_json::Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(
        evidence["guidance_id"],
        "core:guidance:wardline-tier-integral"
    );
    assert!(
        evidence["stored_manifest_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );
    assert!(
        evidence["current_manifest_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );
}

#[cfg(unix)]
fn seed_wardline_tier(conn: &Connection, entity_id: &str, tier: &str) {
    conn.execute(
        "INSERT INTO wardline_taint_facts (entity_id, wardline_json, updated_at) \
         VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
        rusqlite::params![entity_id, format!("{{\"tier\":\"{tier}\"}}")],
    )
    .unwrap();
}

#[cfg(unix)]
fn findings_by_rule(conn: &Connection, rule_id: &str) -> Vec<(String, String, String)> {
    // (entity_id anchor, related_entities, evidence)
    conn.prepare(
        "SELECT entity_id, related_entities, evidence FROM findings \
         WHERE rule_id = ?1 ORDER BY entity_id",
    )
    .unwrap()
    .query_map([rule_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// REQ-ANALYZE-05 verification (verbatim): a fixture with mixed Wardline tiers in
/// a subsystem produces `CLA-FACT-TIER-SUBSYSTEM-MIXING`; a uniform-tier subsystem
/// produces `CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS`. Tier facts are seeded between
/// runs (analyze never writes them — the enrich-only axiom), so run 1 builds the
/// subsystems and run 2 emits the findings against the seeded tiers.
#[cfg(unix)]
#[test]
fn analyze_emits_tier_mixing_and_unanimous_findings() {
    let (project_dir, plugin_dir, config_path) =
        phase3_project_for_rerun(&["auth_a", "auth_b", "billing_a", "billing_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");

    {
        let conn = Connection::open(&db_path).unwrap();
        // auth subsystem: two disagreeing tiers -> MIXING.
        seed_wardline_tier(&conn, "phase3fixture:module:auth_a", "public");
        seed_wardline_tier(&conn, "phase3fixture:module:auth_b", "internal");
        // billing subsystem: two agreeing tiers -> UNANIMOUS.
        seed_wardline_tier(&conn, "phase3fixture:module:billing_a", "trusted");
        seed_wardline_tier(&conn, "phase3fixture:module:billing_b", "trusted");
    }

    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    let mixing = findings_by_rule(&conn, "CLA-FACT-TIER-SUBSYSTEM-MIXING");
    assert_eq!(mixing.len(), 1, "exactly the auth subsystem mixes tiers");
    let related: serde_json::Value = serde_json::from_str(&mixing[0].1).unwrap();
    assert_eq!(
        related,
        serde_json::json!(["phase3fixture:module:auth_a", "phase3fixture:module:auth_b"])
    );
    let evidence: serde_json::Value = serde_json::from_str(&mixing[0].2).unwrap();
    assert_eq!(evidence["tier_distribution"]["public"], 1);
    assert_eq!(evidence["tier_distribution"]["internal"], 1);

    let unanimous = findings_by_rule(&conn, "CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS");
    assert_eq!(
        unanimous.len(),
        1,
        "exactly the billing subsystem is unanimous"
    );
    let related: serde_json::Value = serde_json::from_str(&unanimous[0].1).unwrap();
    assert_eq!(
        related,
        serde_json::json!([
            "phase3fixture:module:billing_a",
            "phase3fixture:module:billing_b"
        ])
    );
    let evidence: serde_json::Value = serde_json::from_str(&unanimous[0].2).unwrap();
    assert_eq!(evidence["tier"], "trusted");
    assert_eq!(evidence["member_count"], 2);
}

/// REQ-ANALYZE-05: tiers land on functions, not modules, so the production path
/// resolves a tier-bearing function up its `contains` chain to the subsystem.
/// This seeds a tier on a `python:function:*` entity contained in a
/// subsystem-member module (depth-1 walk) and asserts it contributes to the
/// subsystem's consensus alongside a module-level member.
#[cfg(unix)]
#[test]
fn analyze_resolves_function_tier_through_contains_chain_to_subsystem() {
    let (project_dir, plugin_dir, config_path) = phase3_project_for_rerun(&["auth_a", "auth_b"]);
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let db_path = project_dir.path().join(".clarion/clarion.db");
    let func = "phase3fixture:function:auth_a.handler";

    {
        let conn = Connection::open(&db_path).unwrap();
        // A function contained in the auth_a module (which is in the auth
        // subsystem). The auth_a module itself carries NO tier — the only way the
        // auth_a side contributes is via this contained function (depth-1 walk).
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, parent_id, properties, created_at, updated_at) \
             VALUES (?1, 'phase3fixture', 'function', 'handler', 'handler', \
                     'phase3fixture:module:auth_a', '{}', \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [func],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence) \
             VALUES ('contains', 'phase3fixture:module:auth_a', ?1, 'resolved')",
            [func],
        )
        .unwrap();
        seed_wardline_tier(&conn, func, "secret");
        seed_wardline_tier(&conn, "phase3fixture:module:auth_b", "secret");
    }

    run_phase3_analyze(
        project_dir.path(),
        std::path::Path::new(&config_path),
        &plugin_path,
    );

    let conn = Connection::open(&db_path).unwrap();
    let unanimous = findings_by_rule(&conn, "CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS");
    assert_eq!(
        unanimous.len(),
        1,
        "the auth subsystem is unanimous via the function tier"
    );
    let related: serde_json::Value = serde_json::from_str(&unanimous[0].1).unwrap();
    // The contained FUNCTION (not its module) is the auth_a-side member, proving
    // the function -> module -> subsystem resolution fired.
    assert_eq!(
        related,
        serde_json::json!([func, "phase3fixture:module:auth_b"])
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
fn analyze_mints_core_file_entity_for_registry_resolution() {
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
    let resolved = clarion_storage::resolve_file(&conn, project_dir.path(), "demo.call", "")
        .expect("resolve_file should not error")
        .expect("analyzed ordinary source file should resolve as a core file entity");

    assert_eq!(resolved.entity_id, "core:file:demo.call");
    assert_eq!(resolved.canonical_path.as_str(), "demo.call");
    assert_eq!(
        resolved.language, "callsfixture",
        "HTTP file resolution must use the plugin manifest language, not a hardcoded extension fallback"
    );
    assert_eq!(
        resolved.content_hash,
        blake3::hash(b"caller callee\n").to_hex().to_string()
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

    // Scrub the ambient PATH — build the child's PATH from ONLY the
    // broken-plugin dir. If we inherited the parent's PATH, a real
    // `clarion-plugin-*` binary installed on the developer's machine
    // (e.g. `clarion-plugin-python` under ~/.local/bin) would be
    // discovered, the run would complete cleanly, and this FailRun test
    // would fail with "Unexpected success". The sibling tests
    // (`analyze_resume_*`, `analyze_prune_unseen_*`) build their PATH the
    // same single-dir way.
    let new_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");

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

#[cfg(unix)]
fn phase3_config_with_filigree(min_cluster_size: u64, base_url: &str) -> String {
    phase3_config_with_filigree_emit(min_cluster_size, base_url, true)
}

#[cfg(unix)]
fn phase3_config_with_filigree_emit(
    min_cluster_size: u64,
    base_url: &str,
    emit_findings: bool,
) -> String {
    format!(
        r"
analysis:
  clustering:
    min_cluster_size: {min_cluster_size}
integrations:
  filigree:
    enabled: true
    emit_findings: {emit_findings}
    base_url: {base_url}
    timeout_seconds: 1
"
    )
}

/// WP9-B: emission is best-effort. With Filigree enabled but unreachable, the
/// analyze run must still complete (exit 0, run row `completed`) and record the
/// failure in `stats.json` as `CLA-INFRA-FILIGREE-UNREACHABLE` — the enrich-only
/// federation contract: a sibling being down never changes Clarion's outcome.
#[cfg(unix)]
#[test]
fn analyze_finding_emission_is_best_effort_when_filigree_unreachable() {
    // Port 1 is not listening: connection refused, fast.
    let project_dir = run_phase3_fixture(
        &["weak_a", "weak_b"],
        &phase3_config_with_filigree(2, "http://127.0.0.1:1"),
    );

    // run_phase3_fixture already asserted the analyze invocation `.success()`;
    // confirm the run row landed `completed` despite the emission failure.
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query run status");
    assert_eq!(
        status, "completed",
        "Filigree being unreachable must not fail the analyze run",
    );

    let run_stats = latest_run_stats(project_dir.path());
    let emission = &run_stats["filigree_emission"];
    assert_eq!(
        emission["status"].as_str(),
        Some("unreachable"),
        "emission recorded as unreachable: {run_stats}",
    );
    assert_eq!(
        emission["rule_id"].as_str(),
        Some("CLA-INFRA-FILIGREE-UNREACHABLE"),
    );
    assert!(
        emission["endpoint"]
            .as_str()
            .unwrap_or_default()
            .contains("127.0.0.1:1"),
        "endpoint records the target: {emission}",
    );
    // The weak-modularity finding anchors to a subsystem (no source path), so
    // it is skipped, not emitted.
    assert_eq!(
        emission["skipped_no_path"].as_u64(),
        Some(1),
        "path-less finding skipped: {emission}",
    );
    assert_eq!(emission["emitted_attempted"].as_u64(), Some(0));
}

/// WP9-B: the happy path — analyze actually POSTs to a listening Filigree and
/// records `status: "emitted"` with the parsed response counts in `stats.json`.
/// A one-shot mock server stands in for Filigree's `/api/v1/scan-results`.
#[cfg(unix)]
#[test]
fn analyze_finding_emission_posts_and_records_emitted_on_success() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock filigree");
    let addr = listener.local_addr().expect("local addr");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept scan-results POST");
        let mut buf = [0_u8; 8192];
        let read = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..read]).into_owned();
        let body = r#"{"files_created":0,"files_updated":0,"findings_created":0,"findings_updated":0,"new_finding_ids":[],"observations_created":0,"observations_failed":0,"warnings":[]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write response");
        request
    });

    let project_dir = run_phase3_fixture(
        &["weak_a", "weak_b"],
        &phase3_config_with_filigree(2, &format!("http://{addr}")),
    );

    let request = server.join().expect("mock server thread");
    assert!(
        request.contains("POST /api/v1/scan-results HTTP/1.1"),
        "analyze POSTed to the scan-results route: {request}",
    );
    assert!(
        request.contains("\"scan_source\":\"clarion\""),
        "request body carries scan_source: {request}",
    );

    let stats = latest_run_stats(project_dir.path());
    let emission = &stats["filigree_emission"];
    assert_eq!(
        emission["status"].as_str(),
        Some("emitted"),
        "emission succeeded: {stats}",
    );
    assert_eq!(emission["findings_created"].as_u64(), Some(0));
    // The only persisted finding (weak-modularity) is path-less → skipped.
    assert_eq!(emission["skipped_no_path"].as_u64(), Some(1), "{emission}");
    assert_eq!(emission["emitted"].as_u64(), Some(0));
}

/// REQ-FINDING-05 `--resume`: re-running with `--resume RUN_ID` reuses the
/// prior run's row (one `runs` row, not two) and emits with `mark_unseen=false`
/// so the re-emit does not flip the prior run's findings to `unseen_in_latest`
/// on the peer. A fresh run emits `mark_unseen=true`. End-to-end through a mock
/// Filigree that captures both POST bodies.
#[cfg(unix)]
#[test]
fn analyze_resume_reuses_run_row_and_emits_mark_unseen_false() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock filigree");
    let addr = listener.local_addr().expect("local addr");
    // Accept exactly two POSTs — the fresh run, then the resume — and capture
    // each request body.
    let server = std::thread::spawn(move || {
        let body = r#"{"files_created":0,"files_updated":0,"findings_created":0,"findings_updated":0,"new_finding_ids":[],"observations_created":0,"observations_failed":0,"warnings":[]}"#;
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept scan-results POST");
            let mut buf = [0_u8; 8192];
            let read = stream.read(&mut buf).expect("read request");
            requests.push(String::from_utf8_lossy(&buf[..read]).into_owned());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        }
        requests
    });

    // Build a project + plugin and run a fresh analyze (POST 1).
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    for stem in ["weak_a", "weak_b"] {
        std::fs::write(project_dir.path().join(format!("{stem}.p3")), b"module\n")
            .expect("write phase3 fixture file");
    }
    let config_path = project_dir.path().join("phase3-clarion.yaml");
    std::fs::write(
        &config_path,
        phase3_config_with_filigree(2, &format!("http://{addr}")),
    )
    .expect("write phase3 config");
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    // Capture the fresh run's id, then resume it (POST 2).
    let run_id: String = {
        let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
        conn.query_row(
            "SELECT id FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("read fresh run id")
    };
    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .args(["--resume", &run_id])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let requests = server.join().expect("mock server thread");
    assert!(
        requests[0].contains("\"mark_unseen\":true"),
        "fresh run marks old-position findings unseen: {}",
        requests[0],
    );
    assert!(
        requests[1].contains("\"mark_unseen\":false"),
        "resume must NOT mark the prior run's findings unseen: {}",
        requests[1],
    );

    // Resume reused the run row — exactly one row in `runs`, finalized.
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let run_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        run_rows, 1,
        "resume reuses the run row — no second `runs` row inserted",
    );
    let run_status: String = conn
        .query_row("SELECT status FROM runs WHERE id = ?1", [&run_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        run_status, "completed",
        "the resumed run finalizes to completed"
    );
    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["filigree_emission"]["mark_unseen"].as_bool(),
        Some(false),
        "stats.json records the resume emit ran with mark_unseen=false: {stats}",
    );
}

/// REQ-FINDING-06 `--prune-unseen`: after emission, analyze POSTs a retention
/// sweep to Filigree's loom `clean-stale` route, scoped to `scan_source=clarion`,
/// and records the soft-archive count in `stats.json`. End-to-end through a mock
/// Filigree that accepts both the emission POST and the prune POST.
#[cfg(unix)]
#[test]
fn analyze_prune_unseen_posts_clean_stale_after_emission() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock filigree");
    let addr = listener.local_addr().expect("local addr");
    // One body satisfies both parsers (serde(default) ignores the other's
    // fields): scan-results counts + clean-stale counts.
    let server = std::thread::spawn(move || {
        let body = r#"{"files_created":0,"files_updated":0,"findings_created":0,"findings_updated":0,"new_finding_ids":[],"observations_created":0,"observations_failed":0,"warnings":[],"findings_fixed":2,"scan_source":"clarion","older_than_days":30}"#;
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept POST");
            let mut buf = [0_u8; 8192];
            let read = stream.read(&mut buf).expect("read request");
            requests.push(String::from_utf8_lossy(&buf[..read]).into_owned());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        }
        requests
    });

    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    for stem in ["weak_a", "weak_b"] {
        std::fs::write(project_dir.path().join(format!("{stem}.p3")), b"module\n")
            .expect("write phase3 fixture file");
    }
    let config_path = project_dir.path().join("phase3-clarion.yaml");
    std::fs::write(
        &config_path,
        phase3_config_with_filigree(2, &format!("http://{addr}")),
    )
    .expect("write phase3 config");
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg("--prune-unseen")
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let requests = server.join().expect("mock server thread");
    assert!(
        requests[0].contains("POST /api/v1/scan-results HTTP/1.1"),
        "first POST is the emission intake: {}",
        requests[0],
    );
    assert!(
        requests[1].contains("POST /api/loom/findings/clean-stale HTTP/1.1"),
        "second POST is the loom clean-stale sweep: {}",
        requests[1],
    );
    assert!(
        requests[1].contains("\"scan_source\":\"clarion\""),
        "prune is scoped to scan_source=clarion: {}",
        requests[1],
    );
    // Guard the wire field name: the live Filigree clean-stale route silently
    // ignores a `days` field — only `older_than_days` takes effect. Assert the
    // request carries the correct key (default 30) so a serde rename can't
    // regress the retention window to a no-op.
    assert!(
        requests[1].contains("\"older_than_days\":30"),
        "prune sends older_than_days (not `days`): {}",
        requests[1],
    );

    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["filigree_prune"]["status"].as_str(),
        Some("pruned"),
        "stats.json records the prune sweep: {stats}",
    );
    assert_eq!(
        stats["filigree_prune"]["findings_fixed"].as_u64(),
        Some(2),
        "prune records Filigree's soft-archive count: {stats}",
    );
}

/// REQ-FINDING-06 `--prune-unseen` is enrich-only: with Filigree unreachable the
/// analyze run still completes and the sweep failure is recorded in `stats.json`
/// as `CLA-INFRA-FILIGREE-UNREACHABLE` — never failing the run.
#[cfg(unix)]
#[test]
fn analyze_prune_unseen_is_best_effort_when_filigree_unreachable() {
    let project_dir = run_phase3_fixture(
        &["weak_a", "weak_b"],
        &phase3_config_with_filigree(2, "http://127.0.0.1:1"),
    );

    // run_phase3_fixture does not pass --prune-unseen; re-run analyze with it.
    // (A second run is fine — analyze is idempotent.) Filigree is unreachable
    // (port 1), so both emission and prune fail soft.
    let plugin = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin.path());
    let config_path = project_dir.path().join("phase3-clarion.yaml");
    let plugin_path = std::env::join_paths(std::iter::once(plugin.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg("--prune-unseen")
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
        .unwrap();
    assert_eq!(
        run_status, "completed",
        "Filigree being unreachable must not fail the prune run",
    );
    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["filigree_prune"]["status"].as_str(),
        Some("unreachable"),
        "prune failure recorded, not propagated: {stats}",
    );
    assert_eq!(
        stats["filigree_prune"]["rule_id"].as_str(),
        Some("CLA-INFRA-FILIGREE-UNREACHABLE"),
    );
}

/// WP9-B: with Filigree `enabled: true` but `emit_findings: false`, analyze
/// makes ZERO scan-results POST. The emission gate short-circuits before any
/// network I/O (no finding flush, no client build), so a listening mock must
/// see no connection at all. `stats.json` carries no `filigree_emission` blob
/// (the emit helper returns null, which is not folded in).
#[cfg(unix)]
#[test]
fn analyze_does_not_emit_when_emit_findings_false() {
    use std::net::TcpListener;

    // Bind a listener but never accept on a thread — analyze must not connect.
    // Set non-blocking so the post-run `accept()` returns `WouldBlock`
    // immediately rather than hanging, and inspect the accept queue directly.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock filigree");
    listener
        .set_nonblocking(true)
        .expect("set listener non-blocking");
    let addr = listener.local_addr().expect("local addr");

    let project_dir = run_phase3_fixture(
        &["weak_a", "weak_b"],
        &phase3_config_with_filigree_emit(2, &format!("http://{addr}"), false),
    );

    // No client ever connected — a completed connection would sit in the accept
    // queue even if closed, so `WouldBlock` proves zero POSTs were made.
    match listener.accept() {
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("emit_findings=false must make no POST, but got: {other:?}"),
    }

    // The emission helper returns null when `emit_findings` is off, and a null
    // emission is never folded into `stats.json`.
    let stats = latest_run_stats(project_dir.path());
    assert!(
        stats["filigree_emission"].is_null(),
        "no emission blob recorded when emit_findings=false: {stats}",
    );
}

/// REQ-FINDING-06 `--prune-unseen` is enrich-only against a non-2xx response,
/// not just connection refusal: when Filigree answers the clean-stale POST with
/// HTTP 500, analyze still exits 0 with the run row `completed`, and the sweep
/// failure is recorded in `stats.json` as `CLA-INFRA-FILIGREE-UNREACHABLE`. The
/// 500 is well-formed (content-length present) so it exercises the client's
/// `!status.is_success()` branch rather than a torn-connection error.
#[cfg(unix)]
#[test]
fn analyze_prune_unseen_is_best_effort_on_non_2xx() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock filigree");
    let addr = listener.local_addr().expect("local addr");
    // Accept both POSTs: 200 to the emission intake (POST 1), then 500 to the
    // clean-stale sweep (POST 2).
    let server = std::thread::spawn(move || {
        let ok_body = r#"{"files_created":0,"files_updated":0,"findings_created":0,"findings_updated":0,"new_finding_ids":[],"observations_created":0,"observations_failed":0,"warnings":[]}"#;
        let err_body = r#"{"error":"boom"}"#;
        for i in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept POST");
            let mut buf = [0_u8; 8192];
            let _ = stream.read(&mut buf).expect("read request");
            if i == 0 {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    ok_body.len(),
                    ok_body
                )
                .expect("write emission response");
            } else {
                write!(
                    stream,
                    "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    err_body.len(),
                    err_body
                )
                .expect("write clean-stale 500");
            }
        }
    });

    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    for stem in ["weak_a", "weak_b"] {
        std::fs::write(project_dir.path().join(format!("{stem}.p3")), b"module\n")
            .expect("write phase3 fixture file");
    }
    let config_path = project_dir.path().join("phase3-clarion.yaml");
    std::fs::write(
        &config_path,
        phase3_config_with_filigree(2, &format!("http://{addr}")),
    )
    .expect("write phase3 config");
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg("--prune-unseen")
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    server.join().expect("mock server thread");

    // A non-2xx clean-stale response must never fail the run.
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let run_status: String = conn
        .query_row(
            "SELECT status FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        run_status, "completed",
        "a 500 from the clean-stale route must not fail the prune run",
    );
    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["filigree_prune"]["status"].as_str(),
        Some("unreachable"),
        "non-2xx prune failure recorded, not propagated: {stats}",
    );
    assert_eq!(
        stats["filigree_prune"]["rule_id"].as_str(),
        Some("CLA-INFRA-FILIGREE-UNREACHABLE"),
    );
}

/// `--prune-unseen` with the Filigree integration disabled is a logged no-op,
/// not an error: the run completes and `stats.json` records the skip.
#[cfg(unix)]
#[test]
fn analyze_prune_unseen_noops_when_filigree_disabled() {
    // phase3_config (no `integrations.filigree`) leaves the integration
    // disabled by default.
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    for stem in ["weak_a", "weak_b"] {
        std::fs::write(project_dir.path().join(format!("{stem}.p3")), b"module\n")
            .expect("write phase3 fixture file");
    }
    let config_path = project_dir.path().join("phase3-clarion.yaml");
    std::fs::write(&config_path, phase3_config(2)).expect("write phase3 config");
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    clarion_bin()
        .args(["analyze", "--config"])
        .arg(&config_path)
        .arg("--prune-unseen")
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["filigree_prune"]["status"].as_str(),
        Some("skipped"),
        "prune is a no-op when Filigree is disabled: {stats}",
    );
    assert_eq!(
        stats["filigree_prune"]["reason"].as_str(),
        Some("filigree_disabled"),
    );
}

/// Wave 0 / WS3 (plan T1.4): after a successful `clarion analyze`, the
/// `sei_prior_index` snapshot must equal EXACTLY that run's entity set — stale
/// rows from the prior run removed. Two back-to-back runs on the same project
/// where the second drops a file prove the full-snapshot replace: the dropped
/// entity's row must not survive. `entities` is cumulative, so a snapshot built
/// by querying it would wrongly retain the vanished entity; this guards the
/// accumulate-and-replace path that avoids that.
#[cfg(unix)]
#[test]
fn analyze_rewrites_prior_index_to_current_run_entity_set() {
    use std::collections::BTreeSet;

    fn prior_index_locators(project_root: &std::path::Path) -> BTreeSet<String> {
        let conn = Connection::open(project_root.join(".clarion/clarion.db")).unwrap();
        conn.prepare("SELECT locator FROM sei_prior_index")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();

    // Stems deliberately absent from the plugin's TARGETS map, so each file
    // yields one module entity and no import edges (clustering skips cleanly).
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let analyze = |dir: &std::path::Path| {
        clarion_bin()
            .args(["analyze"])
            .arg(dir)
            .env("PATH", &plugin_path)
            .assert()
            .success();
    };

    // Run 1: two source files. Each yields a core-minted `core:file:*` entity
    // (whole-file hash) plus the plugin's `module` entity — all four carry a
    // body hash, so all four enter the snapshot.
    std::fs::write(project_dir.path().join("pidx_alpha.p3"), b"module\n").unwrap();
    std::fs::write(project_dir.path().join("pidx_beta.p3"), b"module\n").unwrap();
    analyze(project_dir.path());
    assert_eq!(
        prior_index_locators(project_dir.path()),
        BTreeSet::from([
            "core:file:pidx_alpha.p3".to_owned(),
            "core:file:pidx_beta.p3".to_owned(),
            "phase3fixture:module:pidx_alpha".to_owned(),
            "phase3fixture:module:pidx_beta".to_owned(),
        ]),
        "prior index after run 1 must equal run 1's entity set"
    );

    // Run 2: beta removed → the snapshot must drop BOTH stale beta rows (the
    // core file entity and the plugin module) even though those rows still live
    // in the cumulative `entities` table.
    std::fs::remove_file(project_dir.path().join("pidx_beta.p3")).unwrap();
    analyze(project_dir.path());
    assert_eq!(
        prior_index_locators(project_dir.path()),
        BTreeSet::from([
            "core:file:pidx_alpha.p3".to_owned(),
            "phase3fixture:module:pidx_alpha".to_owned(),
        ]),
        "prior index after run 2 must equal run 2's entity set (stale beta rows removed)"
    );

    // Column contract: body_hash populated (NOT NULL), recorded_at stamped, and
    // signature still NULL in Wave 0 (the WS1 matcher fills it later).
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let (body_hash, recorded_at, signature): (String, String, Option<String>) = conn
        .query_row(
            "SELECT body_hash, recorded_at, signature FROM sei_prior_index WHERE locator = ?1",
            ["phase3fixture:module:pidx_alpha"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("prior-index row for the surviving module");
    assert!(!body_hash.is_empty(), "body_hash must be populated");
    assert!(!recorded_at.is_empty(), "recorded_at must be populated");
    assert_eq!(
        signature, None,
        "the phase3 fixture declares no signature, so module rows stay NULL"
    );
}

/// Map of `current_locator -> sei` for every ALIVE binding (Wave 1 / WS1).
fn alive_sei_bindings(
    project_root: &std::path::Path,
) -> std::collections::BTreeMap<String, String> {
    let conn = Connection::open(project_root.join(".clarion/clarion.db")).unwrap();
    conn.prepare(
        "SELECT current_locator, sei FROM sei_bindings \
         WHERE status = 'alive' AND current_locator IS NOT NULL",
    )
    .unwrap()
    .query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn all_entity_ids(project_root: &std::path::Path) -> std::collections::BTreeSet<String> {
    let conn = Connection::open(project_root.join(".clarion/clarion.db")).unwrap();
    conn.prepare("SELECT id FROM entities")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
#[cfg_attr(not(unix), ignore = "fixture plugin script is a unix shebang")]
fn analyze_mints_alive_sei_binding_for_every_entity() {
    // DoD: every alive entity has an alive `sei_bindings` row after analysis,
    // and every SEI carries the reserved `clarion:eid:` prefix (ADR-038).
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    std::fs::write(project_dir.path().join("sei_alpha.p3"), b"module\n").unwrap();
    std::fs::write(project_dir.path().join("sei_beta.p3"), b"module\n").unwrap();
    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let entities = all_entity_ids(project_dir.path());
    let bindings = alive_sei_bindings(project_dir.path());
    // On a from-scratch run every entity is current, so the alive binding set
    // must equal the entity set exactly.
    assert_eq!(
        bindings
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        entities,
        "every entity must have exactly one alive SEI binding after analysis"
    );
    assert!(!bindings.is_empty(), "expected at least one minted SEI");
    for (locator, sei) in &bindings {
        assert!(
            sei.starts_with("clarion:eid:"),
            "SEI for {locator} must carry the reserved prefix: {sei}"
        );
    }
}

#[test]
#[cfg_attr(not(unix), ignore = "fixture plugin script is a unix shebang")]
fn analyze_carries_sei_on_unchanged_rerun() {
    // DoD + ADR-038 determinity boundary: a back-to-back unchanged re-run must
    // CARRY (never re-mint) every SEI. Run 2 uses a different run_id, so a
    // re-mint would change every token — identical tokens prove the carry.
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let analyze = || {
        clarion_bin()
            .args(["analyze"])
            .arg(project_dir.path())
            .env("PATH", &plugin_path)
            .assert()
            .success();
    };

    std::fs::write(project_dir.path().join("sei_gamma.p3"), b"module\n").unwrap();
    analyze();
    let after_run1 = alive_sei_bindings(project_dir.path());
    assert!(!after_run1.is_empty());

    analyze();
    let after_run2 = alive_sei_bindings(project_dir.path());

    assert_eq!(
        after_run1, after_run2,
        "an unchanged re-run must carry every SEI (identical token per locator), not re-mint"
    );
}

#[test]
#[cfg_attr(not(unix), ignore = "fixture plugin script is a unix shebang")]
fn analyze_no_sei_flag_skips_minting() {
    // The --no-sei escape hatch leaves sei_bindings empty.
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();

    std::fs::write(project_dir.path().join("sei_delta.p3"), b"module\n").unwrap();
    clarion_bin()
        .args(["analyze", "--no-sei"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    assert!(
        alive_sei_bindings(project_dir.path()).is_empty(),
        "--no-sei must skip the mint pass entirely"
    );
}

#[test]
#[cfg_attr(not(unix), ignore = "fixture plugin script is a unix shebang")]
fn analyze_orphans_deleted_entity_bindings_through_the_real_pipeline() {
    // Drives the PRODUCTION `run_sei_mint_pass` orphan-first path + lineage
    // end-to-end (not the oracle's re-implementation): an entity present in run
    // 1 but absent in run 2 must have its binding flipped to `orphaned` with an
    // `orphaned` lineage event, while the surviving entity stays `alive`.
    // (Phase3 fixture entities are module-only with null signatures, so a
    // vanished locator with no git signal correctly fails closed to orphan.)
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    let analyze = || {
        clarion_bin()
            .args(["analyze"])
            .arg(project_dir.path())
            .env("PATH", &plugin_path)
            .assert()
            .success();
    };

    std::fs::write(project_dir.path().join("sei_keep.p3"), b"module\n").unwrap();
    std::fs::write(project_dir.path().join("sei_drop.p3"), b"module\n").unwrap();
    analyze();
    let run1 = alive_sei_bindings(project_dir.path());
    let drop_locator = "phase3fixture:module:sei_drop";
    let keep_locator = "phase3fixture:module:sei_keep";
    let dropped_sei = run1
        .get(drop_locator)
        .expect("dropped module must have an alive binding after run 1")
        .clone();

    // Run 2: remove sei_drop.p3 → its module entity vanishes.
    std::fs::remove_file(project_dir.path().join("sei_drop.p3")).unwrap();
    analyze();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    // The dropped entity's binding is now orphaned (by SEI — its row persists).
    let dropped_status: String = conn
        .query_row(
            "SELECT status FROM sei_bindings WHERE sei = ?1",
            [&dropped_sei],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        dropped_status, "orphaned",
        "a deleted entity's binding must flip to orphaned on the real pipeline"
    );
    // An `orphaned` lineage event was appended for it.
    let orphaned_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sei_lineage WHERE sei = ?1 AND event = 'orphaned'",
            [&dropped_sei],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        orphaned_events >= 1,
        "delete must record an orphaned lineage event"
    );
    // The dropped locator no longer has an alive binding; the survivor does.
    let after = alive_sei_bindings(project_dir.path());
    assert!(
        !after.contains_key(drop_locator),
        "dropped locator must not be alive"
    );
    assert!(
        after.contains_key(keep_locator),
        "kept locator must stay alive"
    );
}

// ── Wave 2 / T3.1: incremental analysis (skip unchanged files) + orphan guard ──

/// Install Clarion + the phase3 fixture plugin into a fresh project. Returns the
/// project dir, the plugin dir (kept alive so the script stays on disk), and the
/// `PATH` value that exposes the plugin to `clarion analyze`.
#[cfg(unix)]
fn phase3_env() -> (tempfile::TempDir, tempfile::TempDir, std::ffi::OsString) {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_phase3_plugin(plugin_dir.path());
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    (project_dir, plugin_dir, plugin_path)
}

#[cfg(unix)]
fn run_git(project_root: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed with {status}");
}

#[cfg(unix)]
fn git_stdout(project_root: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout)
        .expect("git stdout is utf8")
        .trim()
        .to_owned()
}

#[test]
#[cfg_attr(not(unix), ignore = "fixture plugin script is a unix shebang")]
fn analyze_stamps_entities_with_git_head_commit() {
    let (project_dir, _plugin_dir, plugin_path) = phase3_env();
    let mut analyze_paths: Vec<std::path::PathBuf> = std::env::split_paths(&plugin_path).collect();
    if let Some(system_path) = std::env::var_os("PATH") {
        analyze_paths.extend(std::env::split_paths(&system_path));
    }
    let analyze_path = std::env::join_paths(analyze_paths).expect("join analyze PATH");
    std::fs::write(project_dir.path().join("demo.p3"), b"module\n").expect("write fixture file");
    run_git(project_dir.path(), &["init", "-q"]);
    run_git(project_dir.path(), &["config", "user.email", "t@t"]);
    run_git(project_dir.path(), &["config", "user.name", "t"]);
    run_git(project_dir.path(), &["add", "demo.p3"]);
    run_git(project_dir.path(), &["commit", "-qm", "initial"]);
    let head = git_stdout(project_dir.path(), &["rev-parse", "HEAD"]);

    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &analyze_path)
        .assert()
        .success();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    for entity_id in ["core:file:demo.p3", "phase3fixture:module:demo"] {
        let (first_seen, last_seen): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT first_seen_commit, last_seen_commit FROM entities WHERE id = ?1",
                [entity_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|err| panic!("query provenance for {entity_id}: {err}"));
        assert_eq!(
            first_seen.as_deref(),
            Some(head.as_str()),
            "{entity_id} first_seen_commit"
        );
        assert_eq!(
            last_seen.as_deref(),
            Some(head.as_str()),
            "{entity_id} last_seen_commit"
        );
    }
}

#[test]
#[cfg_attr(not(unix), ignore = "fixture plugin script is a unix shebang")]
fn analyze_incremental_skip_does_not_orphan_unchanged_file_entities() {
    // THE correctness crux of Wave 2 (T3.1 Step 4). With incremental skip on, a
    // re-run that changes ONE file must skip the OTHER — and a skipped file's
    // still-present entities must keep their SEI and NOT be orphaned. This is
    // load-bearing: without the matcher's current-locator union including skipped
    // entities, every entity in every unchanged file would be falsely orphaned.
    let (project_dir, _plugin_dir, plugin_path) = phase3_env();
    let analyze = || {
        clarion_bin()
            .args(["analyze"])
            .arg(project_dir.path())
            .env("PATH", &plugin_path)
            .assert()
            .success();
    };

    std::fs::write(project_dir.path().join("inc_stable.p3"), b"module\n").unwrap();
    std::fs::write(project_dir.path().join("inc_churn.p3"), b"module\n").unwrap();
    analyze();
    let run1 = alive_sei_bindings(project_dir.path());
    // The unchanged file contributes two entities: its core `file` entity and the
    // fixture module. Both must survive run 2 with identical SEIs.
    let stable_module = "phase3fixture:module:inc_stable";
    let stable_sei = run1
        .get(stable_module)
        .expect("stable module has an alive binding after run 1")
        .clone();
    let stable_file_locator = run1
        .keys()
        .find(|k| k.starts_with("core:file:") && k.contains("inc_stable"))
        .expect("stable file entity has an alive binding after run 1")
        .clone();
    let stable_file_sei = run1[&stable_file_locator].clone();

    // Run 2: change ONLY inc_churn.p3 (its whole-file hash changes → re-analyzed);
    // inc_stable.p3 is byte-identical → skipped.
    std::fs::write(
        project_dir.path().join("inc_churn.p3"),
        b"module\n# changed\n",
    )
    .unwrap();
    analyze();

    let stats = latest_run_stats(project_dir.path());
    assert_eq!(
        stats["skipped_files"].as_u64(),
        Some(1),
        "exactly the unchanged file must be skipped: {stats}"
    );

    let run2 = alive_sei_bindings(project_dir.path());
    assert_eq!(
        run2.get(stable_module),
        Some(&stable_sei),
        "the skipped file's module must keep its SEI alive (not orphaned, not re-minted)"
    );
    assert_eq!(
        run2.get(&stable_file_locator),
        Some(&stable_file_sei),
        "the skipped file's core file entity must keep its SEI alive"
    );
    // And the binding's status is literally alive (belt-and-braces: alive_sei_bindings
    // already filters status='alive', but assert no orphaned lineage was recorded).
    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let orphaned_for_stable: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sei_lineage WHERE sei = ?1 AND event = 'orphaned'",
            [&stable_sei],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        orphaned_for_stable, 0,
        "no orphaned lineage event may be recorded for a skipped-unchanged entity"
    );
}

#[test]
#[cfg_attr(not(unix), ignore = "fixture plugin script is a unix shebang")]
fn analyze_incremental_repeated_unchanged_runs_keep_skipping() {
    // Guards the prior-index WRITE side: a skipped file's entries must be re-fed
    // into the rebuilt prior index, or the snapshot blanks them out and the skip
    // decays (run 2 skips all, run 3 skips nothing). Three identical runs must
    // each skip BOTH files after the first.
    let (project_dir, _plugin_dir, plugin_path) = phase3_env();
    let analyze = || {
        clarion_bin()
            .args(["analyze"])
            .arg(project_dir.path())
            .env("PATH", &plugin_path)
            .assert()
            .success();
    };

    std::fs::write(project_dir.path().join("decay_a.p3"), b"module\n").unwrap();
    std::fs::write(project_dir.path().join("decay_b.p3"), b"module\n").unwrap();

    analyze(); // run 1: from-scratch, nothing to skip.
    assert_eq!(
        latest_run_stats(project_dir.path())["skipped_files"].as_u64(),
        Some(0),
        "first run has no prior index, so it skips nothing"
    );

    analyze(); // run 2: both unchanged → both skipped.
    assert_eq!(
        latest_run_stats(project_dir.path())["skipped_files"].as_u64(),
        Some(2),
        "second run must skip both unchanged files"
    );

    analyze(); // run 3: the snapshot must NOT have decayed.
    assert_eq!(
        latest_run_stats(project_dir.path())["skipped_files"].as_u64(),
        Some(2),
        "third run must still skip both — the prior index must not decay after a skip"
    );
}

#[test]
#[cfg_attr(not(unix), ignore = "fixture plugin script is a unix shebang")]
fn analyze_no_incremental_forces_full_reanalysis() {
    // The --no-incremental escape hatch disables the skip entirely: an unchanged
    // re-run re-analyses every file (skipped_files = 0).
    let (project_dir, _plugin_dir, plugin_path) = phase3_env();
    std::fs::write(project_dir.path().join("full_a.p3"), b"module\n").unwrap();
    std::fs::write(project_dir.path().join("full_b.p3"), b"module\n").unwrap();

    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();
    clarion_bin()
        .args(["analyze", "--no-incremental"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    assert_eq!(
        latest_run_stats(project_dir.path())["skipped_files"].as_u64(),
        Some(0),
        "--no-incremental must re-analyse everything"
    );
    // Identity is still stable across the forced full re-run (carried, not re-minted).
    assert!(
        !alive_sei_bindings(project_dir.path()).is_empty(),
        "a forced full re-run still carries SEIs"
    );
}

// ── REQ-ANALYZE-06: parse-failure findings are persisted, not just logged ────

/// Mirrors the real Python plugin: every file yields one `module` entity, and a
/// file whose stem starts with `broken` carries the top-level
/// `parse_status="syntax_error"` flag the plugin sets on `ast.parse` failure.
/// The flag rides into `properties_json` via the host's `extra` handling, where
/// the core's `syntax_error_finding` reads it.
#[cfg(unix)]
const SYNTAX_ERROR_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
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
                "name": "clarion-plugin-syn",
                "version": "0.1.0",
                "ontology_version": "0.6.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        stem = pathlib.Path(path).stem
        entity = {
            "id": f"synfixture:module:{stem}",
            "kind": "module",
            "qualified_name": stem,
            "source": {"file_path": path},
            "parse_status": "syntax_error" if stem.startswith("broken") else "ok",
        }
        write_frame({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {"entities": [entity], "edges": [], "stats": {}},
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

#[cfg(unix)]
const SYNTAX_ERROR_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-syn"
plugin_id = "synfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-syn"
language = "synfixture"
extensions = ["syn"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "CLA-SYN-"
ontology_version = "0.6.0"

[ontology.roles]
file_scope = ["module"]
syntax_degraded_module = ["module"]
"#;

#[cfg(unix)]
fn write_syntax_error_plugin(plugin_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("clarion-plugin-syn");
    std::fs::write(&plugin_script, SYNTAX_ERROR_PLUGIN_SCRIPT).expect("write syn plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat syn plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod syn plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), SYNTAX_ERROR_PLUGIN_MANIFEST)
        .expect("write syn plugin manifest");
}

/// REQ-ANALYZE-06 verification (in part): a file that fails to parse produces a
/// `CLA-PY-SYNTAX-ERROR` finding **persisted to the store**, anchored to the
/// degraded module entity — not merely logged. A cleanly-parsed file produces
/// no such finding.
#[cfg(unix)]
#[test]
fn analyze_persists_syntax_error_finding_for_unparseable_file() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_syntax_error_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    std::fs::write(project_dir.path().join("broken_mod.syn"), b"def (\n").unwrap();
    std::fs::write(project_dir.path().join("clean_mod.syn"), b"ok\n").unwrap();

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .success();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let (count, anchor): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MIN(entity_id), '') FROM findings \
             WHERE rule_id = 'CLA-PY-SYNTAX-ERROR'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query syntax-error findings");
    assert_eq!(
        count, 1,
        "exactly one CLA-PY-SYNTAX-ERROR finding persisted"
    );
    assert_eq!(
        anchor, "synfixture:module:broken_mod",
        "finding anchors to the degraded module entity"
    );

    // The anchor row exists (FK integrity) and the clean file produced no finding.
    let anchor_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE id = 'synfixture:module:broken_mod'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(anchor_exists, 1, "finding anchor entity is present");
}

/// A plugin that crashes mid-`analyze_file`. Initializes cleanly, then exits
/// non-zero on the first analyze request — exercising the host's crash path.
#[cfg(unix)]
const CRASHING_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
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
                "name": "clarion-plugin-crash",
                "version": "0.1.0",
                "ontology_version": "0.6.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        # Crash mid-run: exit non-zero so the host's supervisor sees a crash.
        raise SystemExit(7)
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

#[cfg(unix)]
const CRASHING_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-crash"
plugin_id = "crashfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-crash"
language = "crashfixture"
extensions = ["crx"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "CLA-CRASH-"
ontology_version = "0.6.0"

[ontology.roles]
file_scope = ["module"]
"#;

#[cfg(unix)]
fn write_crashing_plugin(plugin_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("clarion-plugin-crash");
    std::fs::write(&plugin_script, CRASHING_PLUGIN_SCRIPT).expect("write crash plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat crash plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod crash plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), CRASHING_PLUGIN_MANIFEST)
        .expect("write crash plugin manifest");
}

/// REQ-ANALYZE-06 verification (in part): a plugin that crashes mid-run produces
/// a `CLA-INFRA-PLUGIN-CRASH` finding **persisted to the store**, anchored to the
/// synthetic `core:project:{name}` entity — not merely logged.
#[cfg(unix)]
#[test]
fn analyze_persists_crash_finding_anchored_to_project() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_crashing_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    std::fs::write(project_dir.path().join("a.crx"), b"x\n").unwrap();

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    // A crashed plugin yields a non-zero exit (SoftFailed → CommitRun(Failed));
    // the persisted finding is what we assert on.
    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .assert()
        .failure();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let (count, anchor): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MIN(entity_id), '') FROM findings \
             WHERE rule_id = 'CLA-INFRA-PLUGIN-CRASH'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query crash findings");
    assert!(count >= 1, "a CLA-INFRA-PLUGIN-CRASH finding is persisted");
    let project_name = project_dir
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap();
    assert_eq!(
        anchor,
        format!("core:project:{project_name}"),
        "crash finding anchors to the synthetic project entity"
    );
    // The synthetic anchor entity exists (FK integrity).
    let anchor_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE id = ?1 AND kind = 'project'",
            [&anchor],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(anchor_exists, 1, "project anchor entity is present");

    // REQ-ANALYZE-06: failure findings are also visible in runs.stats.
    let stats_raw: String = conn
        .query_row(
            "SELECT stats FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query run stats");
    let stats: serde_json::Value = serde_json::from_str(&stats_raw).expect("stats JSON");
    assert!(
        stats["failure_findings"].as_u64().unwrap_or(0) >= 1,
        "stats.json reports the persisted failure-finding count; got: {stats_raw}"
    );
}

/// A plugin that hangs inside `analyze_file` (sleeps far longer than the
/// configured per-file timeout). Initializes cleanly, then blocks forever on the
/// first analyze request.
#[cfg(unix)]
const HANGING_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
import json
import sys
import time


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
                "name": "clarion-plugin-hang",
                "version": "0.1.0",
                "ontology_version": "0.6.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        # Hang: never respond. The host watchdog must kill us.
        time.sleep(3600)
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
    else:
        raise SystemExit(1)
"#;

#[cfg(unix)]
const HANGING_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "clarion-plugin-hang"
plugin_id = "hangfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-hang"
language = "hangfixture"
extensions = ["hng"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "CLA-HANG-"
ontology_version = "0.6.0"

[ontology.roles]
file_scope = ["module"]
"#;

#[cfg(unix)]
fn write_hanging_plugin(plugin_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("clarion-plugin-hang");
    std::fs::write(&plugin_script, HANGING_PLUGIN_SCRIPT).expect("write hang plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat hang plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod hang plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), HANGING_PLUGIN_MANIFEST)
        .expect("write hang plugin manifest");
}

/// REQ-ANALYZE-06 verification (in part): a plugin that hangs on a file is killed
/// by the per-file analysis-timeout watchdog and produces a persisted
/// `CLA-PY-TIMEOUT` finding (and not a redundant `CLA-INFRA-PLUGIN-CRASH`). The
/// timeout is set low via `CLARION_PLUGIN_FILE_TIMEOUT_MS` on the spawned process.
#[cfg(unix)]
#[test]
fn analyze_persists_timeout_finding_for_hanging_plugin() {
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_hanging_plugin(plugin_dir.path());

    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    std::fs::write(project_dir.path().join("slow.hng"), b"x\n").unwrap();

    let plugin_path =
        std::env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).unwrap();
    clarion_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &plugin_path)
        .env("CLARION_PLUGIN_FILE_TIMEOUT_MS", "500")
        .assert()
        .failure();

    let conn = Connection::open(project_dir.path().join(".clarion/clarion.db")).unwrap();
    let (timeout_count, anchor): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MIN(entity_id), '') FROM findings \
             WHERE rule_id = 'CLA-PY-TIMEOUT'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query timeout findings");
    assert!(timeout_count >= 1, "a CLA-PY-TIMEOUT finding is persisted");
    let project_name = project_dir
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap();
    assert_eq!(
        anchor,
        format!("core:project:{project_name}"),
        "timeout finding anchors to the synthetic project entity"
    );

    // The generic crash finding is suppressed when a timeout is the root cause.
    let crash_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM findings WHERE rule_id = 'CLA-INFRA-PLUGIN-CRASH'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        crash_count, 0,
        "no redundant CLA-INFRA-PLUGIN-CRASH when the cause is a timeout"
    );
}
