//! ADR-063 acceptance (clarion-dee44f1a66): a committed `loomweave.yaml` naming an
//! attacker endpoint + an arbitrary credential env var causes NO network call
//! under `analyze` or `serve`; the same file untracked works.
//!
//! The three tests share ONE fixture builder and ONE `analyze` invocation, so
//! the negative result ("zero captured requests") is only ever produced by the
//! same setup that the positive control proves is capable of egress. Without
//! that pairing, a fixture that silently never analysed anything would pass the
//! two negative tests for the wrong reason.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use loomweave_core::plugin::{ContentLengthCeiling, Frame, read_frame, write_frame};
use rusqlite::Connection;

/// The credential env var the hostile config names. Deliberately NOT one of
/// Loomweave's documented defaults: the point of the gate is that repository
/// content must not get to choose which of the operator's env vars is sent.
const CANARY_ENV: &str = "LOOMWEAVE_TEST_CANARY";
const CANARY_VALUE: &str = "leak";

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

/// The directory holding the real `git` binary, or `None` when git is absent.
///
/// The analyzed child runs with a NARROWED `PATH` (so plugin discovery finds
/// only this test's fixture plugin), and `hardened_git_command` resolves `git`
/// through that same `PATH`. Omitting git's directory would make every
/// tracked-state probe answer `GitUnavailable` — which is deliberately
/// PERMISSIVE (a missing git is the operator's environment, ADR-062) — and the
/// negative tests would pass in the wrong direction, proving nothing.
fn git_binary_dir() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find(|dir| {
        let candidate = dir.join("git");
        std::fs::metadata(&candidate)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

/// Hermetic git: no global/system config, explicit identity, so a developer's
/// `~/.gitconfig` (hooks, templates, `core.hooksPath`) cannot reach the fixture.
fn git(dir: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?} in {}: {e}", dir.display()));
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Attacker-endpoint mock
// ---------------------------------------------------------------------------

/// A loopback HTTP listener that answers any request with a well-formed
/// OpenAI-compatible embeddings response and records the raw request text.
///
/// Unlike `tests/analyze.rs`'s `spawn_embedding_mock` (which returns after the
/// FIRST connection), this one accepts until the test explicitly stops it. The
/// negative assertion here is "nothing connected for the whole lifetime of the
/// child process", so the listener must outlive the child rather than a fixed
/// deadline — and the positive control must not be decided by which of the
/// three hostile endpoints happens to connect first.
struct MockServer {
    url: String,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Vec<String>>,
}

impl MockServer {
    fn finish(self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.join().expect("mock listener thread")
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let mut header_end = None;
    while header_end.is_none() {
        let Ok(read) = stream.read(&mut chunk) else {
            break;
        };
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
        let Ok(read) = stream.read(&mut chunk) else {
            break;
        };
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

fn spawn_mock() -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind attacker mock");
    let addr = listener.local_addr().expect("mock addr");
    listener.set_nonblocking(true).expect("nonblocking mock");
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        // Safety net only: `finish()` is what normally ends this loop. A panicking
        // test would otherwise leak the thread for the life of the test binary.
        let hard_deadline = Instant::now() + Duration::from_secs(180);
        let mut requests = Vec::new();
        while !thread_stop.load(Ordering::SeqCst) && Instant::now() < hard_deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_http_request(&mut stream);
                    let body = request.split("\r\n\r\n").nth(1).unwrap_or("{}");
                    let payload: serde_json::Value =
                        serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
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
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                        response.len(),
                        response
                    );
                    requests.push(request);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(err) => panic!("attacker mock accept failed: {err}"),
            }
        }
        requests
    });
    MockServer {
        url: format!("http://{addr}"),
        stop,
        handle,
    }
}

// ---------------------------------------------------------------------------
// Fixture plugin (mirrors tests/analyze.rs's categorised plugin)
// ---------------------------------------------------------------------------

const PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
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
                "name": "loomweave-plugin-categorised",
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

const PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "loomweave-plugin-categorised"
plugin_id = "catfixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "loomweave-plugin-categorised"
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
rule_id_prefix = "LMWV-CAT-"
ontology_version = "0.1.0"
classifier_tags = ["entry-point"]

[ontology.roles]
file_scope = ["module"]
callable = ["function"]
"#;

fn write_fixture_plugin(plugin_dir: &Path) {
    let script = plugin_dir.join("loomweave-plugin-categorised");
    std::fs::write(&script, PLUGIN_SCRIPT).expect("write fixture plugin script");
    let mut perms = std::fs::metadata(&script)
        .expect("stat fixture plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod fixture plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), PLUGIN_MANIFEST)
        .expect("write fixture plugin manifest");
}

// ---------------------------------------------------------------------------
// The hostile fixture
// ---------------------------------------------------------------------------

/// A `loomweave.yaml` a hostile repository could commit: three egress sections,
/// all pointed at the test's own listener, all naming an env var of the
/// attacker's choosing as the credential to send.
///
/// `serve.http` is deliberately left at its default so the stripped-section
/// list is exactly the three that were set — a positive witness that the gate
/// reports what it actually did, not a fixed string.
fn hostile_yaml(mock_url: &str) -> String {
    format!(
        "version: 1
analysis:
  min_subsystem_size: 2
semantic_search:
  enabled: true
  provider: api
  allow_live_provider: true
  endpoint_url: {mock_url}
  api_key_env: {CANARY_ENV}
  model_id: test-embed
  dimensions: 2
  timeout_seconds: 5
llm_policy:
  enabled: true
  provider: openrouter
  allow_live_provider: true
  openrouter:
    endpoint_url: {mock_url}
    api_key_env: {CANARY_ENV}
    timeout_seconds: 5
integrations:
  filigree:
    enabled: true
    base_url: {mock_url}
    token_env: {CANARY_ENV}
    actor: t
"
    )
}

/// `install` + one analysable source file + the hostile config, then a real git
/// repository with that config COMMITTED.
///
/// Every test starts here; the untracked control differs only by a subsequent
/// `git rm --cached`.
fn hostile_fixture(project_dir: &Path, plugin_dir: &Path, mock_url: &str) {
    write_fixture_plugin(plugin_dir);
    loomweave_bin()
        .args(["install", "--path"])
        .arg(project_dir)
        .assert()
        .success();
    std::fs::write(project_dir.join("app.cat"), "def main():\n    pass\n")
        .expect("write fixture source");
    std::fs::write(project_dir.join("loomweave.yaml"), hostile_yaml(mock_url))
        .expect("write hostile config");

    git(project_dir, &["init", "-q", "-b", "main"]);
    git(project_dir, &["add", "-f", "--", "loomweave.yaml"]);
    git(project_dir, &["commit", "-qm", "commit the hostile config"]);
}

/// `PATH` for the analysed child: the fixture plugin's directory (so discovery
/// finds exactly one plugin) plus git's own directory (so the trust probe can
/// actually run — see [`git_binary_dir`]).
fn child_path(plugin_dir: &Path, git_dir: &Path) -> std::ffi::OsString {
    std::env::join_paths([plugin_dir.to_path_buf(), git_dir.to_path_buf()]).expect("join PATH")
}

fn run_analyze(
    project_dir: &Path,
    plugin_dir: &Path,
    git_dir: &Path,
) -> assert_cmd::assert::Assert {
    loomweave_bin()
        .args(["analyze"])
        .arg(project_dir)
        .env("PATH", child_path(plugin_dir, git_dir))
        .env("RUST_LOG", "info")
        .env(CANARY_ENV, CANARY_VALUE)
        .assert()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn a_committed_config_never_reaches_the_network_under_analyze() {
    let Some(git_dir) = git_binary_dir() else {
        eprintln!("skipping: no git on PATH");
        return;
    };
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let mock = spawn_mock();
    hostile_fixture(project_dir.path(), plugin_dir.path(), &mock.url);

    let assert = run_analyze(project_dir.path(), plugin_dir.path(), &git_dir).success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is utf8");

    // Second witness: the run must SAY it downgraded the config, so "no
    // requests" can never be mistaken for "the fixture never ran".
    assert!(
        stderr.contains("tracked by the repository"),
        "analyze must announce the ADR-063 downgrade on stderr; stderr was:\n{stderr}"
    );

    // The entity the fixture plugin emits proves analyze really walked and
    // extracted — the positive control for the negative assertion below.
    let conn = Connection::open(project_dir.path().join(".weft/loomweave/loomweave.db")).unwrap();
    let entities: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE id = 'catfixture:function:app.main'",
            [],
            |row| row.get(0),
        )
        .expect("query entities");
    assert_eq!(
        entities, 1,
        "the fixture plugin must have produced entities"
    );
    drop(conn);

    let requests = mock.finish();
    assert!(
        requests.is_empty(),
        "a repository-tracked loomweave.yaml must not reach the network; captured {requests:#?}"
    );
}

#[test]
fn a_committed_config_never_reaches_the_network_under_serve() {
    let Some(git_dir) = git_binary_dir() else {
        eprintln!("skipping: no git on PATH");
        return;
    };
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let mock = spawn_mock();
    hostile_fixture(project_dir.path(), plugin_dir.path(), &mock.url);

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("loomweave"))
        .args(["serve", "--path"])
        .arg(project_dir.path())
        .env("PATH", child_path(plugin_dir.path(), &git_dir))
        .env("RUST_LOG", "info")
        .env(CANARY_ENV, CANARY_VALUE)
        .env(
            "LOOMWEAVE_CODEX_CONFIG",
            std::env::temp_dir().join(format!(
                "loomweave-test-codex-config-{}.toml",
                std::process::id()
            )),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loomweave serve");
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        write_frame(
            &mut stdin,
            &Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {"name": "llm_config_get", "arguments": {}}
                }))
                .expect("serialize request"),
            },
        )
        .expect("write llm_config_get frame");
        stdin.flush().expect("flush llm_config_get frame");
    }
    let output = child.wait_with_output().expect("wait for loomweave serve");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "serve failed: {stderr}");

    let mut reader = std::io::BufReader::new(std::io::Cursor::new(output.stdout));
    let frame =
        read_frame(&mut reader, ContentLengthCeiling::new(usize::MAX)).expect("read response");
    let response: serde_json::Value =
        serde_json::from_slice(&frame.body).expect("response body is json");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool text missing in {response}"));
    let envelope: serde_json::Value = serde_json::from_str(text).expect("tool envelope");
    assert_eq!(envelope["ok"], true, "{envelope}");

    let trust = &envelope["result"]["config_trust"];
    assert_eq!(trust["state"], "repository_tracked", "{envelope}");
    assert_eq!(
        trust["stripped"],
        serde_json::json!(["llm_policy", "semantic_search", "integrations"]),
        "the gate must report exactly the egress sections the hostile file set: {envelope}"
    );
    assert_eq!(
        trust["remedy"],
        loomweave_federation::config::CONFIG_TRACKED_REMEDY,
        "the verdict must carry the verbatim remedy: {envelope}"
    );
    assert_eq!(
        envelope["result"]["llm"]["enabled"], false,
        "a tracked llm_policy must not enable the provider: {envelope}"
    );
    assert_eq!(
        envelope["result"]["llm"]["live"], false,
        "a tracked llm_policy must not go live: {envelope}"
    );

    let requests = mock.finish();
    assert!(
        requests.is_empty(),
        "a repository-tracked loomweave.yaml must not reach the network under serve; \
         captured {requests:#?}"
    );
}

#[test]
fn the_same_config_untracked_populates_embeddings_through_the_mock() {
    let Some(git_dir) = git_binary_dir() else {
        eprintln!("skipping: no git on PATH");
        return;
    };
    let project_dir = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let mock = spawn_mock();
    hostile_fixture(project_dir.path(), plugin_dir.path(), &mock.url);
    // The ONLY difference from the two tests above: the operator takes the file
    // back. Byte-identical config, same plugin, same PATH, same child env.
    git(
        project_dir.path(),
        &["rm", "--cached", "-q", "--", "loomweave.yaml"],
    );

    run_analyze(project_dir.path(), plugin_dir.path(), &git_dir).success();

    let requests = mock.finish();
    assert!(
        requests
            .iter()
            .any(|request| request.contains("/embeddings")
                && request.contains(&format!("Bearer {CANARY_VALUE}"))),
        "an operator-owned config must reach its configured endpoint; captured {requests:#?}"
    );

    let sidecar = project_dir.path().join(".weft/loomweave/embeddings.db");
    assert!(
        sidecar.exists(),
        "analyze should create the embeddings sidecar"
    );
    let conn = Connection::open(sidecar).unwrap();
    let embeddings: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entity_embeddings \
             WHERE entity_id = 'catfixture:function:app.main' AND model_id = 'test-embed'",
            [],
            |row| row.get(0),
        )
        .expect("query sidecar embeddings");
    assert_eq!(embeddings, 1, "the function embedding should be persisted");
}
