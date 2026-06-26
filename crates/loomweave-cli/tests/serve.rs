use std::fs;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::{LazyLock, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use hmac::{Hmac, Mac};
use loomweave_core::plugin::{ContentLengthCeiling, Frame, read_frame, write_frame};
use loomweave_llm::LEAF_SUMMARY_PROMPT_TEMPLATE_ID;
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const STABLE_INSTANCE_ID: &str = "9bd7234e-6d44-4a38-9ae4-76f912a10221";
static RESERVED_LOOPBACK_BINDS: LazyLock<Mutex<Vec<(String, TcpListener)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

#[derive(Debug)]
struct HttpJsonResponse {
    status_code: u16,
    body: Value,
}

#[derive(Debug)]
struct HttpRawResponse {
    status_code: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpRawResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

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

#[test]
fn serve_help_advertises_mcp_stdio_server() {
    let assert = loomweave_bin().args(["serve", "--help"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("help is utf8");

    assert!(stdout.contains("Run the MCP stdio server"));
    assert!(stdout.contains("--path"));
}

#[test]
fn serve_stdio_initialize_round_trip() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    write_stdio_config(dir.path());

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("loomweave"))
        .args(["serve", "--path"])
        .arg(dir.path())
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
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "test-client", "version": "0.0.0"}
                    }
                }))
                .expect("serialize request"),
            },
        )
        .expect("write initialize frame");
        stdin.flush().expect("flush initialize frame");
    }

    let output = child.wait_with_output().expect("wait for loomweave serve");
    assert!(
        output.status.success(),
        "serve failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut reader = std::io::BufReader::new(std::io::Cursor::new(output.stdout));
    let frame = read_frame(&mut reader, ContentLengthCeiling::new(usize::MAX))
        .expect("read initialize response");
    let response: serde_json::Value =
        serde_json::from_slice(&frame.body).expect("response body is json");

    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(response["result"]["serverInfo"]["name"], "loomweave");
}

#[test]
fn serve_stdio_accepts_mcp_json_line_initialize_without_stdin_eof() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    write_stdio_config(dir.path());

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("loomweave"))
        .args(["serve", "--path"])
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loomweave serve");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let (response_tx, response_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut stdout = std::io::BufReader::new(stdout);
        let mut line = String::new();
        let result = stdout.read_line(&mut line).map(|_| line);
        let _ = response_tx.send(result);
    });

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "codex-rmcp-smoke", "version": "0.0.0"}
        }
    });
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&request).expect("serialize request")
    )
    .expect("write initialize json line");
    stdin.flush().expect("flush initialize json line");

    let line = match response_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result.expect("read initialize response line"),
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("loomweave serve did not answer newline-delimited MCP initialize: {err}");
        }
    };
    let response: Value = serde_json::from_str(line.trim_end()).expect("response line is json");

    drop(stdin);
    let status = child.wait().expect("wait for loomweave serve");
    reader.join().expect("stdout reader thread");

    assert!(
        status.success(),
        "serve failed with status {status}: {}",
        read_child_stderr(&mut child)
    );
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"]["serverInfo"]["name"], "loomweave");
}

fn read_child_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    if let Some(mut stream) = child.stderr.take() {
        let _ = stream.read_to_string(&mut stderr);
    }
    stderr
}

#[test]
fn serve_http_responses_match_federation_fixture_contracts() {
    let files_fixture = load_contract_fixture(
        "get-api-v1-files.demo-python.json",
        include_str!("../../../docs/federation/fixtures/get-api-v1-files.demo-python.json"),
    );
    let capabilities_fixture = load_contract_fixture(
        "get-api-v1-capabilities.json",
        include_str!("../../../docs/federation/fixtures/get-api-v1-capabilities.json"),
    );
    let files_resolve_fixture = load_contract_fixture(
        "post-api-v1-files-resolve.batch.json",
        include_str!("../../../docs/federation/fixtures/post-api-v1-files-resolve.batch.json"),
    );
    let files_batch_fixture = load_contract_fixture(
        "post-api-v1-files-batch.json",
        include_str!("../../../docs/federation/fixtures/post-api-v1-files-batch.json"),
    );
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    fs::write(
        dir.path().join(".weft/loomweave/instance_id"),
        format!("{STABLE_INSTANCE_ID}\n"),
    )
    .expect("seed stable instance ID");
    seed_file_entity(dir.path());
    seed_storage_failure_file_entity(dir.path());
    seed_briefing_blocked_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    validate_fixture_examples(&bind, &files_fixture, "get-api-v1-files.demo-python.json");
    validate_fixture_examples(
        &bind,
        &files_resolve_fixture,
        "post-api-v1-files-resolve.batch.json",
    );
    validate_fixture_examples_matching(
        &bind,
        &files_batch_fixture,
        "post-api-v1-files-batch.json",
        |example_name| example_name != "batch_unauthorized_401",
    );
    validate_fixture_examples(&bind, &capabilities_fixture, "get-api-v1-capabilities.json");
    stop_serve(&mut child);

    let auth_dir = tempfile::tempdir().expect("temp auth project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(auth_dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(auth_dir.path());
    let auth_bind = free_loopback_bind();
    write_http_config_with_token_env(
        auth_dir.path(),
        &auth_bind,
        "LOOMWEAVE_TEST_FIXTURE_BATCH_TOKEN",
    );

    let mut auth_child = spawn_serve_with_env(
        auth_dir.path(),
        &[("LOOMWEAVE_TEST_FIXTURE_BATCH_TOKEN", "fixture-secret")],
    );
    validate_fixture_examples_matching(
        &auth_bind,
        &files_batch_fixture,
        "post-api-v1-files-batch.json",
        |example_name| example_name == "batch_unauthorized_401",
    );
    stop_serve(&mut auth_child);
}

#[test]
fn serve_http_files_endpoint_returns_briefing_blocked_for_blocked_entity() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_briefing_blocked_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let response = wait_for_http_response(&bind, "/api/v1/files?path=blocked.py&language=python");
    stop_serve(&mut child);
    let response = response.expect("HTTP /api/v1/files briefing-blocked response");

    assert_eq!(response.status_code, 403);
    assert_eq!(response.body["code"], "BRIEFING_BLOCKED");
    let error = response.body["error"]
        .as_str()
        .expect("briefing-blocked error has string message");
    assert!(
        error.to_ascii_lowercase().contains("briefing-blocked"),
        "briefing-blocked message must mention the block: {error}"
    );
    assert!(
        response.body.get("entity_id").is_none(),
        "blocked responses must not leak the entity_id: {response:?}"
    );
    assert!(
        response.body.get("content_hash").is_none(),
        "blocked responses must not leak the content hash: {response:?}"
    );
}

#[test]
fn serve_http_files_endpoint_resolves_known_file_on_configured_port() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let (file_id, content_hash, canonical_path) = seed_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let response = wait_for_http_json(&bind, "/api/v1/files?path=demo.py&language=python");
    stop_serve(&mut child);
    let response = response.expect("HTTP /api/v1/files response");
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../docs/federation/fixtures/get-api-v1-files.demo-python.json"
    ))
    .expect("parse files fixture");

    assert_eq!(response["entity_id"], file_id);
    assert_eq!(response["content_hash"], content_hash);
    assert_eq!(response["canonical_path"], canonical_path);
    assert_eq!(response["language"], "python");
    assert_eq!(&response, fixture_example_body(&fixture, "happy_path_200"));
}

#[test]
fn serve_http_files_endpoint_prefers_stored_manifest_language() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_custom_language_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let response = wait_for_http_json(&bind, "/api/v1/files?path=demo.custom&language=requested");
    stop_serve(&mut child);
    let response = response.expect("HTTP /api/v1/files response");

    assert_eq!(response["entity_id"], "core:file:demo.custom");
    assert_eq!(response["language"], "manifestlang");
}

#[test]
fn serve_http_files_etag_round_trip_and_if_none_match_returns_304() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let (_file_id, content_hash, _canonical_path) = seed_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let response =
        wait_for_http_raw_response(&bind, "/api/v1/files?path=demo.py&language=python", &[]);
    let not_modified = wait_for_http_raw_response(
        &bind,
        "/api/v1/files?path=demo.py&language=python",
        &[("If-None-Match", "\"hash-demo-file\"")],
    );
    stop_serve(&mut child);
    let response = response.expect("HTTP /api/v1/files response");
    let not_modified = not_modified.expect("HTTP /api/v1/files conditional response");

    let expected_etag = format!("\"{content_hash}\"");
    assert_eq!(response.status_code, 200);
    assert_eq!(response.header("etag"), Some(expected_etag.as_str()));
    assert_eq!(not_modified.status_code, 304);
    assert_eq!(not_modified.header("etag"), Some(expected_etag.as_str()));
    assert!(
        not_modified.body.is_empty(),
        "304 response must not include a body: {not_modified:?}"
    );
}

#[test]
fn serve_http_files_blank_path_returns_invalid_path_envelope() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let response = wait_for_http_response(&bind, "/api/v1/files?path=&language=python");
    stop_serve(&mut child);
    let response = response.expect("HTTP /api/v1/files error response");

    assert_eq!(response.status_code, 400);
    assert_eq!(response.body["code"], "INVALID_PATH");
    assert!(
        response.body["error"].as_str().is_some(),
        "error envelope must include a string message: {response:?}"
    );
}

#[test]
fn serve_http_identity_resolve_rejects_sei_shaped_input_and_resolves_unknown() {
    // REQ-F-02 (ADR-038 §4) over the wire: an SEI-shaped locator is rejected
    // with 400 INVALID_PATH (never silently mis-resolved); a well-formed but
    // unknown locator resolves to { alive: false }.
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);
    let mut child = spawn_serve(dir.path());

    let sei_body =
        serde_json::json!({ "locator": "loomweave:eid:0123456789abcdef0123456789abcdef" })
            .to_string();
    let rejected = wait_for_http_post_json(&bind, "/api/v1/identity/resolve", &sei_body, &[]);

    let unknown_body = serde_json::json!({ "locator": "python:function:nope.absent" }).to_string();
    let unknown = wait_for_http_post_json(&bind, "/api/v1/identity/resolve", &unknown_body, &[]);

    stop_serve(&mut child);

    let rejected = rejected.expect("identity/resolve rejection response");
    assert_eq!(rejected.status_code, 400, "{rejected:?}");
    assert_eq!(rejected.body["code"], "INVALID_PATH");

    let unknown = unknown.expect("identity/resolve unknown response");
    assert_eq!(unknown.status_code, 200, "{unknown:?}");
    assert_eq!(unknown.body["alive"], false);
}

/// Seed a coherent POST-RENAME snapshot for the WS4 dossier-participation e2e: a
/// function renamed `mod.process` → `mod.process_v2` whose SEI was CARRIED across
/// the rename (a `locator_changed` lineage event), still carrying its
/// caller/callee linkages, in a file with a content hash. This is the state the
/// Wave 1 matcher produces (proven by the SEI conformance oracle + the
/// production orphan test); the test below proves the Wardline dossier assembler
/// can read every slice it needs over Loomweave's HTTP surface alone. Returns the
/// carried SEI.
fn seed_renamed_function_dossier(project_root: &Path) -> String {
    let source_path = project_root.join("mod.py");
    fs::write(&source_path, "def process_v2():\n    return 1\n").expect("write source");
    let canonical_path = source_path
        .canonicalize()
        .expect("canonical source path")
        .display()
        .to_string();
    let sei = "loomweave:eid:0123456789abcdef0123456789abcdef".to_owned();
    let new_locator = "python:function:mod.process_v2";
    let old_locator = "python:function:mod.process";
    let ts = "2026-06-02T00:00:00.000Z";
    let db_path = project_root.join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open sqlite");

    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at)
         VALUES ('core:file:mod.py','core','file','mod.py','mod.py',?1,1,2,'{}','hash-mod-file',?2,?2)",
        params![canonical_path, ts],
    )
    .expect("insert file entity");
    for (id, name) in [
        (new_locator, "process_v2"),
        ("python:function:mod.caller", "caller"),
        ("python:function:mod.callee", "callee"),
    ] {
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path,
                source_line_start, source_line_end, properties, content_hash, created_at, updated_at)
             VALUES (?1,'python','function',?2,?2,?3,1,2,'{}','hash-fn',?4,?4)",
            params![id, name, canonical_path, ts],
        )
        .expect("insert function entity");
    }
    // Linkages: caller → process_v2 → callee.
    conn.execute(
        "INSERT INTO edges (kind, from_id, to_id, confidence) VALUES
            ('calls','python:function:mod.caller',?1,'resolved'),
            ('calls',?1,'python:function:mod.callee','resolved')",
        params![new_locator],
    )
    .expect("insert call edges");
    // Identity: minted at the OLD locator (run-1), CARRIED to the new locator
    // (run-2, locator_changed). Alive — never orphaned.
    conn.execute(
        "INSERT INTO sei_bindings
            (sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at)
         VALUES (?1, ?2, 'hash-fn', NULL, 'alive', 'run-1', 'run-2', ?3)",
        params![sei, new_locator, ts],
    )
    .expect("insert sei binding");
    conn.execute(
        "INSERT INTO sei_lineage (sei, event, old_locator, new_locator, run_id, recorded_at) VALUES
            (?1,'born',NULL,?2,'run-1',?4),
            (?1,'locator_changed',?2,?3,'run-2',?4)",
        params![sei, old_locator, new_locator, ts],
    )
    .expect("insert sei lineage");
    sei
}

#[test]
fn serve_http_dossier_participation_surface_serves_a_renamed_function() {
    // WS4 core-paradise e2e (Wave 2 DoD): the Wardline dossier assembler can build
    // a complete, freshness-stamped, SEI-keyed view of a RENAMED function using
    // ONLY Loomweave's HTTP surface — SEI carried, facts not orphaned, freshness
    // stamped. Exercises every slice the participation spec pins.
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let sei = seed_renamed_function_dossier(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);
    let mut child = spawn_serve(dir.path());

    let new_locator = "python:function:mod.process_v2";
    let old_locator = "python:function:mod.process";

    // Content axis + identity join: resolve(new) → carried SEI + content_hash.
    let resolve_new = wait_for_http_post_json(
        &bind,
        "/api/v1/identity/resolve",
        &serde_json::json!({ "locator": new_locator }).to_string(),
        &[],
    );
    // The renamed-away OLD address no longer resolves — but identity persists.
    let resolve_old = wait_for_http_post_json(
        &bind,
        "/api/v1/identity/resolve",
        &serde_json::json!({ "locator": old_locator }).to_string(),
        &[],
    );
    // Identity axis: resolve_sei(sei) alive at the new locator + lineage rename.
    let sei_lookup = wait_for_http_json(&bind, &format!("/api/v1/identity/sei/{sei}"));
    let lineage = wait_for_http_json(&bind, &format!("/api/v1/identity/lineage/{sei}"));
    // Linkages over HTTP, keyed on the new locator.
    let inbound = wait_for_http_json(&bind, &format!("/api/v1/entities/{new_locator}/callers"));
    let outbound = wait_for_http_json(&bind, &format!("/api/v1/entities/{new_locator}/callees"));
    // File context.
    let file = wait_for_http_json(&bind, "/api/v1/files?path=mod.py&language=python");
    stop_serve(&mut child);

    let resolve_new = resolve_new.expect("resolve(new) response");
    assert_eq!(resolve_new.status_code, 200, "{resolve_new:?}");
    assert_eq!(resolve_new.body["alive"], true);
    assert_eq!(
        resolve_new.body["sei"], sei,
        "resolve(new) must carry the SEI"
    );
    assert_eq!(
        resolve_new.body["content_hash"], "hash-fn",
        "resolve(new) must stamp the content axis"
    );

    let resolve_old = resolve_old.expect("resolve(old) response");
    assert_eq!(resolve_old.status_code, 200, "{resolve_old:?}");
    assert_eq!(
        resolve_old.body["alive"], false,
        "the renamed-away locator must no longer resolve"
    );

    let sei_lookup = sei_lookup.expect("resolve_sei response");
    assert_eq!(
        sei_lookup["alive"], true,
        "the carried identity must be alive (not orphaned) after the rename"
    );
    assert_eq!(sei_lookup["current_locator"], new_locator);

    let lineage = lineage.expect("lineage response");
    let events = lineage["lineage"].as_array().expect("lineage array");
    assert!(
        events.iter().any(|e| e["event"] == "locator_changed"
            && e["old_locator"] == old_locator
            && e["new_locator"] == new_locator),
        "lineage must record the rename carry: {lineage}"
    );

    let inbound = inbound.expect("callers response");
    let inbound_ids: Vec<&str> = inbound["callers"]
        .as_array()
        .expect("callers array")
        .iter()
        .map(|c| c["entity_id"].as_str().unwrap())
        .collect();
    assert!(
        inbound_ids.contains(&"python:function:mod.caller"),
        "callers must resolve at the new locator: {inbound}"
    );

    let outbound = outbound.expect("callees response");
    let outbound_ids: Vec<&str> = outbound["callees"]
        .as_array()
        .expect("callees array")
        .iter()
        .map(|c| c["entity_id"].as_str().unwrap())
        .collect();
    assert!(
        outbound_ids.contains(&"python:function:mod.callee"),
        "callees must resolve at the new locator: {outbound}"
    );

    let file = file.expect("file context response");
    assert_eq!(file["entity_id"], "core:file:mod.py");
    assert_eq!(
        file["content_hash"], "hash-mod-file",
        "file context must be reachable + content-stamped"
    );
}

#[test]
fn serve_http_files_rejects_unknown_query_fields() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let response = wait_for_http_response(
        &bind,
        "/api/v1/files?path=demo.py&language=python&surprise=1",
    );
    stop_serve(&mut child);
    let response = response.expect("HTTP /api/v1/files query rejection");

    assert_eq!(response.status_code, 400);
    assert_eq!(response.body["code"], "INVALID_PATH");
    assert!(
        response.body["error"].as_str().is_some(),
        "error envelope must include a string message: {response:?}"
    );
}

#[test]
fn serve_http_rejects_oversized_get_body_before_handler() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let status_code =
        wait_for_http_get_with_body_status(&bind, "/api/v1/_capabilities", 16 * 1024 + 1);
    stop_serve(&mut child);
    let status_code = status_code.expect("HTTP response to oversized body");

    assert_eq!(status_code, 413);
}

#[test]
fn serve_http_files_path_traversal_returns_outside_project_envelope() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let response =
        wait_for_http_response(&bind, "/api/v1/files?path=../outside.py&language=python");
    stop_serve(&mut child);
    let response = response.expect("HTTP /api/v1/files error response");

    assert_eq!(response.status_code, 400);
    assert_eq!(response.body["code"], "PATH_OUTSIDE_PROJECT");
    assert!(
        response.body["error"].as_str().is_some(),
        "error envelope must include a string message: {response:?}"
    );
}

#[test]
fn serve_http_files_unknown_catalog_file_returns_not_found_envelope() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let response = wait_for_http_response(&bind, "/api/v1/files?path=missing.py&language=python");
    stop_serve(&mut child);
    let response = response.expect("HTTP /api/v1/files error response");

    assert_eq!(response.status_code, 404);
    assert_eq!(response.body["code"], "NOT_FOUND");
    assert!(
        response.body["error"].as_str().is_some(),
        "error envelope must include a string message: {response:?}"
    );
}

#[test]
fn serve_http_files_storage_failure_returns_closed_error_without_raw_detail() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);
    let source_path = dir.path().join("missing-on-disk.py");
    fs::write(&source_path, "def missing():\n    return 1\n").expect("write source");
    let canonical_path = source_path
        .canonicalize()
        .expect("canonical source path")
        .display()
        .to_string();
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, created_at, updated_at
         ) VALUES (
            'core:file:missing-on-disk.py', 'core', 'file',
            'missing-on-disk.py', 'missing-on-disk.py', ?1,
            1, 2, '{}',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical_path],
    )
    .expect("insert file entity without cached hash");
    drop(conn);
    fs::remove_file(&source_path).expect("remove cataloged file to force storage failure");

    let mut child = spawn_serve(dir.path());
    let capabilities = wait_for_http_response(&bind, "/api/v1/_capabilities");
    let response = wait_for_http_response(
        &bind,
        "/api/v1/files?path=missing-on-disk.py&language=python",
    );
    stop_serve(&mut child);
    let capabilities = capabilities.expect("HTTP /api/v1/_capabilities response");
    assert_eq!(capabilities.status_code, 200);
    let response = response.expect("HTTP /api/v1/files storage error response");

    assert!(
        response.status_code == 500 || response.status_code == 503,
        "storage failures must be 500-class: {response:?}"
    );
    // The fixture pins this code: the deleted-file-on-disk path runs
    // through `StorageError::Io` in `into_resolved_file`, which the HTTP
    // surface classifies as `STORAGE_ERROR`. The historical `|| INTERNAL`
    // fallback masked silent drift in the classifier; tighten it so any
    // future re-categorisation surfaces here rather than at a federation
    // consumer.
    assert_eq!(
        response.body["code"], "STORAGE_ERROR",
        "unexpected storage failure code: {response:?}"
    );
    let body = response.body.to_string();
    assert!(!body.to_ascii_lowercase().contains("sqlite"));
    assert!(!body.contains("not a database"));
    assert!(!body.contains("No such file"));
    assert!(!body.contains("no such column"));
    assert!(!body.contains("no such table"));
    assert!(!body.contains(&dir.path().display().to_string()));
}

/// The `GET /api/v1/_capabilities` federation golden, embedded BYTE-IDENTICAL
/// from the on-disk normative fixture. `include_str!` captures the exact bytes
/// (meta + `shape_decl` + examples), so this const IS the wire authority the
/// producer-recheck tests (`serve_http_responses_match_federation_fixture_contracts`
/// and `serve_http_capabilities_and_mcp_stdio_coexist`) compare the live
/// `get_capabilities` handler against.
const CAPABILITIES_GOLDEN: &str =
    include_str!("../../../docs/federation/fixtures/get-api-v1-capabilities.json");

/// Layer-1 byte-pin: lowercase-hex `blake3` over the EXACT bytes of the
/// capabilities golden. Pins the whole fixture file so a silent reformat,
/// field reorder, or re-vendor reds here even when the semantic shape the
/// producer-recheck validates is unchanged.
///
/// Tamper proof: perturbing one byte of the fixture (or one hex char of this
/// const) makes `capabilities_golden_bytes_match_layer1_pin` fail with a
/// `left != right` mismatch; `capabilities_golden_byte_pin_rejects_a_mutated_byte`
/// additionally proves a single flipped input byte yields a DIFFERENT digest.
const CAPABILITIES_GOLDEN_BLAKE3: &str =
    "74d5fd1230a62f7a279e54d2a798ce85b3ae8b962f593d1d6bd8c0e2db70dbf7";

#[test]
fn capabilities_golden_bytes_match_layer1_pin() {
    let actual = blake3::hash(CAPABILITIES_GOLDEN.as_bytes())
        .to_hex()
        .to_string();
    assert_eq!(
        actual, CAPABILITIES_GOLDEN_BLAKE3,
        "vendored get-api-v1-capabilities golden drifted from its byte-pin; \
         re-vendor BYTE-IDENTICAL and update CAPABILITIES_GOLDEN_BLAKE3"
    );
}

#[test]
fn capabilities_golden_byte_pin_rejects_a_mutated_byte() {
    // Tamper proof: flipping one byte of the golden produces a digest that
    // does NOT match the pin, demonstrating the byte-pin is load-bearing.
    let mut tampered = CAPABILITIES_GOLDEN.as_bytes().to_vec();
    tampered[0] ^= 0x01;
    let mutated = blake3::hash(&tampered).to_hex().to_string();
    assert_ne!(
        mutated, CAPABILITIES_GOLDEN_BLAKE3,
        "a single mutated byte must NOT collide with the pinned digest"
    );
}

#[test]
fn serve_http_capabilities_and_mcp_stdio_coexist() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    fs::write(
        dir.path().join(".weft/loomweave/instance_id"),
        format!("{STABLE_INSTANCE_ID}\n"),
    )
    .expect("seed stable instance ID");
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let capabilities = wait_for_http_json(&bind, "/api/v1/_capabilities")
        .expect("HTTP /api/v1/_capabilities response");

    assert_eq!(capabilities["registry_backend"], true);
    assert_eq!(capabilities["file_registry"], true);
    assert_eq!(capabilities["api_version"], 1);
    assert!(capabilities.get("version").is_none());
    let instance_id = capabilities["instance_id"]
        .as_str()
        .expect("instance_id is a string");
    Uuid::parse_str(instance_id).expect("instance_id is a UUID");
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../docs/federation/fixtures/get-api-v1-capabilities.json"
    ))
    .expect("parse capabilities fixture");
    assert_eq!(
        &capabilities,
        fixture_example_body(&fixture, "capabilities_200")
    );

    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        write_frame(
            stdin,
            &Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 42,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "test-client", "version": "0.0.0"}
                    }
                }))
                .expect("serialize request"),
            },
        )
        .expect("write initialize frame");
        stdin.flush().expect("flush initialize frame");
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for loomweave serve");

    assert!(
        output.status.success(),
        "serve failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(output.stdout));
    let frame = read_frame(&mut reader, ContentLengthCeiling::new(usize::MAX))
        .expect("read initialize response");
    let response: serde_json::Value =
        serde_json::from_slice(&frame.body).expect("response body is json");

    assert_eq!(response["id"], 42);
    assert_eq!(response["result"]["serverInfo"]["name"], "loomweave");
}

#[test]
fn serve_http_capabilities_reuses_persisted_instance_id_across_restarts() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let instance_id_path = dir.path().join(".weft/loomweave/instance_id");

    let first_bind = free_loopback_bind();
    write_http_config(dir.path(), &first_bind);
    let mut first_child = spawn_serve(dir.path());
    let first = wait_for_http_json(&first_bind, "/api/v1/_capabilities")
        .expect("first capabilities response");
    stop_serve(&mut first_child);
    let first_instance_id = first["instance_id"]
        .as_str()
        .expect("first instance_id")
        .to_owned();
    assert_eq!(
        fs::read_to_string(&instance_id_path)
            .expect("read first persisted instance_id")
            .trim(),
        first_instance_id
    );

    let second_bind = free_loopback_bind();
    write_http_config(dir.path(), &second_bind);
    let mut second_child = spawn_serve(dir.path());
    let second = wait_for_http_json(&second_bind, "/api/v1/_capabilities")
        .expect("second capabilities response");
    stop_serve(&mut second_child);

    assert_eq!(second["instance_id"], first["instance_id"]);
    assert_eq!(
        fs::read_to_string(&instance_id_path)
            .expect("read second persisted instance_id")
            .trim(),
        first_instance_id
    );
}

/// C12 rotation positive case. The sibling
/// `_reuses_persisted_instance_id_across_restarts` test proves *stability*
/// (the same persisted file produces the same `instance_id`), which silently
/// passes a regression that ignores the file and re-mints on every boot.
/// This test proves the *rotation* direction: overwriting the persisted
/// file between restarts causes `/api/v1/_capabilities` to surface the new
/// UUID. Without it, a future refactor could hard-code a per-process UUID
/// and pass every existing capabilities test.
#[test]
fn serve_http_capabilities_returns_new_instance_id_after_rotation() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let instance_id_path = dir.path().join(".weft/loomweave/instance_id");

    let first_bind = free_loopback_bind();
    write_http_config(dir.path(), &first_bind);
    let mut first_child = spawn_serve(dir.path());
    let first = wait_for_http_json(&first_bind, "/api/v1/_capabilities")
        .expect("first capabilities response");
    stop_serve(&mut first_child);
    let first_instance_id = first["instance_id"]
        .as_str()
        .expect("first instance_id")
        .to_owned();

    // Rotate: overwrite the persisted file with a different valid UUID.
    let rotated_uuid = Uuid::new_v4().to_string();
    assert_ne!(
        rotated_uuid, first_instance_id,
        "rotated UUID must differ from the first to make the test meaningful"
    );
    fs::write(&instance_id_path, format!("{rotated_uuid}\n")).expect("rotate instance_id file");

    let second_bind = free_loopback_bind();
    write_http_config(dir.path(), &second_bind);
    let mut second_child = spawn_serve(dir.path());
    let second = wait_for_http_json(&second_bind, "/api/v1/_capabilities")
        .expect("second capabilities response");
    stop_serve(&mut second_child);

    assert_eq!(
        second["instance_id"], rotated_uuid,
        "after rotation, /_capabilities must surface the new persisted UUID"
    );
    assert_ne!(
        second["instance_id"], first["instance_id"],
        "rotated response must differ from the pre-rotation response"
    );
    assert_eq!(
        fs::read_to_string(&instance_id_path)
            .expect("read post-rotation persisted instance_id")
            .trim(),
        rotated_uuid,
        "post-rotation file content must remain the rotated UUID"
    );
}

#[test]
fn serve_http_capabilities_creates_instance_id_with_private_unix_mode() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let capabilities = wait_for_http_json(&bind, "/api/v1/_capabilities")
        .expect("HTTP /api/v1/_capabilities response");
    stop_serve(&mut child);

    let instance_id_path = dir.path().join(".weft/loomweave/instance_id");
    assert_eq!(
        fs::read_to_string(&instance_id_path)
            .expect("read persisted instance_id")
            .trim(),
        capabilities["instance_id"].as_str().expect("instance_id")
    );
    let mode = fs::metadata(instance_id_path)
        .expect("instance_id metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn serve_http_capabilities_repairs_existing_instance_id_mode() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let instance_id_path = dir.path().join(".weft/loomweave/instance_id");
    let seeded_id = "9bd7234e-6d44-4a38-9ae4-76f912a10221";
    fs::write(&instance_id_path, format!("{seeded_id}\n")).expect("seed instance ID");
    fs::set_permissions(&instance_id_path, fs::Permissions::from_mode(0o644))
        .expect("seed permissive instance ID mode");
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let capabilities = wait_for_http_json(&bind, "/api/v1/_capabilities");
    stop_serve(&mut child);
    let capabilities = capabilities.expect("HTTP /api/v1/_capabilities response");

    assert_eq!(capabilities["instance_id"], seeded_id);
    let mode = fs::metadata(instance_id_path)
        .expect("instance_id metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn serve_rejects_invalid_instance_id_before_serving_http() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    fs::write(
        dir.path().join(".weft/loomweave/instance_id"),
        "not-a-uuid\n",
    )
    .expect("write invalid instance ID");
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let child = spawn_serve(dir.path());
    let output = wait_for_child_exit(child, Duration::from_secs(2))
        .expect("serve should fail before accepting HTTP requests");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid Loomweave instance ID"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn serve_http_batch_endpoint_resolves_mixed_paths() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(dir.path());
    seed_briefing_blocked_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let body = serde_json::json!({
        "queries": [
            {"path": "demo.py", "language": "python"},
            {"path": "missing.py", "language": ""},
            {"path": "blocked.py", "language": "python"},
            {"path": "../escapes.py", "language": "python"},
            {"path": "  ", "language": ""}
        ]
    })
    .to_string();
    let response = wait_for_http_post_json(&bind, "/api/v1/files/batch", &body, &[]);
    stop_serve(&mut child);
    let response = response.expect("batch response");

    assert_eq!(response.status_code, 200);
    let resolved = response.body["resolved"]
        .as_array()
        .expect("resolved array");
    assert_eq!(resolved.len(), 1, "{response:?}");
    assert_eq!(resolved[0]["requested_path"], "demo.py");
    assert_eq!(resolved[0]["entity_id"], "core:file:demo.py");
    assert_eq!(resolved[0]["content_hash"], "hash-demo-file");
    assert_eq!(resolved[0]["canonical_path"], "demo.py");
    assert_eq!(resolved[0]["language"], "python");

    let not_found = response.body["not_found"].as_array().expect("not_found");
    assert_eq!(not_found.len(), 1);
    assert_eq!(not_found[0], "missing.py");

    let blocked = response.body["briefing_blocked"]
        .as_array()
        .expect("briefing_blocked");
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0], "blocked.py");

    let errors = response.body["errors"].as_array().expect("errors");
    assert_eq!(errors.len(), 2);
    let by_path: std::collections::HashMap<&str, &Value> = errors
        .iter()
        .filter_map(|err| err["requested_path"].as_str().map(|p| (p, err)))
        .collect();
    let outside = by_path
        .get("../escapes.py")
        .expect("outside-root error entry");
    assert_eq!(outside["code"], "PATH_OUTSIDE_PROJECT");
    let blank = by_path.get("  ").expect("blank-path error entry");
    assert_eq!(blank["code"], "INVALID_PATH");
}

#[test]
fn serve_http_files_resolve_endpoint_returns_per_path_results() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(dir.path());
    seed_briefing_blocked_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let mut child = spawn_serve(dir.path());
    let body = serde_json::json!({
        "paths": [
            {"path": "demo.py", "language": "python"},
            {"path": "missing.py"},
            {"path": "blocked.py", "language": "python"},
            {"path": "../escapes.py", "language": "python"},
            {"path": "  "}
        ]
    })
    .to_string();
    let response = wait_for_http_post_json(&bind, "/api/v1/files:resolve", &body, &[]);
    stop_serve(&mut child);
    let response = response.expect("files:resolve response");

    assert_eq!(response.status_code, 200);
    let results = response.body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 5, "{response:?}");

    assert_eq!(results[0]["path"], "demo.py");
    assert_eq!(results[0]["response"]["status"], "resolved");
    assert_eq!(
        results[0]["response"]["body"]["entity_id"],
        "core:file:demo.py"
    );
    assert_eq!(
        results[0]["response"]["body"]["content_hash"],
        "hash-demo-file"
    );
    assert_eq!(results[0]["response"]["body"]["canonical_path"], "demo.py");
    assert_eq!(results[0]["response"]["body"]["language"], "python");

    assert_eq!(results[1]["path"], "missing.py");
    assert_eq!(results[1]["response"]["status"], "not_found");
    assert_eq!(results[1]["response"]["body"]["code"], "NOT_FOUND");

    assert_eq!(results[2]["path"], "blocked.py");
    assert_eq!(results[2]["response"]["status"], "blocked");
    assert_eq!(results[2]["response"]["body"]["code"], "BRIEFING_BLOCKED");

    assert_eq!(results[3]["path"], "../escapes.py");
    assert_eq!(results[3]["response"]["status"], "error");
    assert_eq!(
        results[3]["response"]["body"]["code"],
        "PATH_OUTSIDE_PROJECT"
    );

    assert_eq!(results[4]["path"], "  ");
    assert_eq!(results[4]["response"]["status"], "error");
    assert_eq!(results[4]["response"]["body"]["code"], "INVALID_PATH");
}

#[test]
fn serve_http_files_resolve_rejects_over_1000_paths() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let paths: Vec<Value> = (0..1001).map(|_| serde_json::json!({"path": ""})).collect();
    let body = serde_json::json!({"paths": paths}).to_string();
    assert!(
        body.len() <= 16 * 1024,
        "test body should fit under the transport cap: {} bytes",
        body.len()
    );

    let mut child = spawn_serve(dir.path());
    let response = wait_for_http_post_json(&bind, "/api/v1/files:resolve", &body, &[]);
    stop_serve(&mut child);
    let response = response.expect("files:resolve over-limit response");

    assert_eq!(response.status_code, 400);
    assert_eq!(response.body["code"], "INVALID_PATH");
    assert!(
        response.body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("1000")),
        "error message should cite the 1000-path ceiling: {response:?}"
    );
}

#[test]
fn serve_http_batch_rejects_over_256_queries() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config(dir.path(), &bind);

    let queries: Vec<Value> = (0..257)
        .map(|i| serde_json::json!({"path": format!("p{i}.py"), "language": ""}))
        .collect();
    let body = serde_json::json!({"queries": queries}).to_string();
    assert!(
        body.len() <= 16 * 1024,
        "test body should fit under the 16 KB body cap to exercise the AFTER-parse 256 limit: {} bytes",
        body.len()
    );

    let mut child = spawn_serve(dir.path());
    let response = wait_for_http_post_json(&bind, "/api/v1/files/batch", &body, &[]);
    stop_serve(&mut child);
    let response = response.expect("batch over-limit response");

    assert_eq!(response.status_code, 400);
    assert_eq!(response.body["code"], "BATCH_TOO_LARGE");
    assert!(
        response.body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("256")),
        "error message should cite the 256-query ceiling: {response:?}"
    );
}

#[test]
fn serve_http_batch_requires_auth_when_configured() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config_with_token_env(dir.path(), &bind, "WEFT_TEST_TOKEN_BATCH");

    let mut child = spawn_serve_with_env(dir.path(), &[("WEFT_TEST_TOKEN_BATCH", "batch-secret")]);
    let body = serde_json::json!({
        "queries": [{"path": "demo.py", "language": "python"}]
    })
    .to_string();
    let unauthenticated = wait_for_http_post_json(&bind, "/api/v1/files/batch", &body, &[]);
    let authenticated = wait_for_http_post_json(
        &bind,
        "/api/v1/files/batch",
        &body,
        &[("Authorization", "Bearer batch-secret")],
    );
    stop_serve(&mut child);
    let unauthenticated = unauthenticated.expect("unauthenticated batch");
    let authenticated = authenticated.expect("authenticated batch");

    assert_eq!(unauthenticated.status_code, 401);
    assert_eq!(unauthenticated.body["code"], "UNAUTHENTICATED");
    assert_eq!(authenticated.status_code, 200);
    let resolved = authenticated.body["resolved"]
        .as_array()
        .expect("authenticated batch resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0]["entity_id"], "core:file:demo.py");
}

#[test]
fn serve_http_files_endpoint_requires_bearer_token_when_configured() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config_with_token_env(dir.path(), &bind, "WEFT_TEST_TOKEN_REQ");

    let mut child =
        spawn_serve_with_env(dir.path(), &[("WEFT_TEST_TOKEN_REQ", "shh-its-a-secret")]);
    let unauthenticated =
        wait_for_http_response(&bind, "/api/v1/files?path=demo.py&language=python");
    let authenticated = wait_for_http_raw_response(
        &bind,
        "/api/v1/files?path=demo.py&language=python",
        &[("Authorization", "Bearer shh-its-a-secret")],
    );
    stop_serve(&mut child);
    let unauthenticated = unauthenticated.expect("unauthenticated probe response");
    let authenticated = authenticated.expect("authenticated probe response");

    assert_eq!(unauthenticated.status_code, 401);
    assert_eq!(unauthenticated.body["code"], "UNAUTHENTICATED");
    assert_eq!(authenticated.status_code, 200);
    let body: Value =
        serde_json::from_str(&authenticated.body).expect("authenticated body is JSON");
    assert_eq!(body["entity_id"], "core:file:demo.py");
}

#[test]
fn serve_http_files_endpoint_rejects_wrong_token() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config_with_token_env(dir.path(), &bind, "WEFT_TEST_TOKEN_WRONG");

    let mut child = spawn_serve_with_env(dir.path(), &[("WEFT_TEST_TOKEN_WRONG", "correct-horse")]);
    let wrong = wait_for_http_raw_response(
        &bind,
        "/api/v1/files?path=demo.py&language=python",
        &[("Authorization", "Bearer battery-staple")],
    );
    let blank = wait_for_http_raw_response(
        &bind,
        "/api/v1/files?path=demo.py&language=python",
        &[("Authorization", "Bearer ")],
    );
    let wrong_scheme = wait_for_http_raw_response(
        &bind,
        "/api/v1/files?path=demo.py&language=python",
        &[("Authorization", "Basic correct-horse")],
    );
    stop_serve(&mut child);
    let wrong = wrong.expect("wrong-token response");
    let blank = blank.expect("blank-token response");
    let wrong_scheme = wrong_scheme.expect("wrong-scheme response");

    for (name, response) in [
        ("wrong", &wrong),
        ("blank", &blank),
        ("wrong-scheme", &wrong_scheme),
    ] {
        assert_eq!(response.status_code, 401, "{name}: {response:?}");
        let body: Value = serde_json::from_str(&response.body)
            .unwrap_or_else(|err| panic!("{name} body parse: {err}; raw={:?}", response.body));
        assert_eq!(body["code"], "UNAUTHENTICATED", "{name}");
    }
}

#[test]
fn serve_http_files_endpoint_requires_hmac_identity_when_configured() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config_with_identity_token_env(dir.path(), &bind, "WEFT_TEST_IDENTITY_REQ");

    let mut child =
        spawn_serve_with_env(dir.path(), &[("WEFT_TEST_IDENTITY_REQ", "shared-secret")]);
    let path = "/api/v1/files?path=demo.py&language=python";
    let missing = wait_for_http_raw_response(&bind, path, &[]);
    let (signed_header, signed_timestamp, signed_nonce) =
        hmac_component_headers("shared-secret", "GET", path, b"");
    let signed = wait_for_http_raw_response(
        &bind,
        path,
        &[
            ("X-Weft-Component", &signed_header),
            ("X-Weft-Timestamp", &signed_timestamp),
            ("X-Weft-Nonce", &signed_nonce),
        ],
    );
    stop_serve(&mut child);
    let missing = missing.expect("missing identity response");
    let signed = signed.expect("signed identity response");

    assert_eq!(missing.status_code, 401);
    let missing_body: Value =
        serde_json::from_str(&missing.body).expect("missing identity body is JSON");
    assert_eq!(missing_body["code"], "UNAUTHENTICATED");
    assert_eq!(signed.status_code, 200);
    let signed_body: Value = serde_json::from_str(&signed.body).expect("signed body is JSON");
    assert_eq!(signed_body["entity_id"], "core:file:demo.py");
}

#[test]
fn serve_http_files_endpoint_rejects_wrong_hmac_identity() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_file_entity(dir.path());
    let bind = free_loopback_bind();
    write_http_config_with_identity_token_env(dir.path(), &bind, "WEFT_TEST_IDENTITY_WRONG");

    let mut child =
        spawn_serve_with_env(dir.path(), &[("WEFT_TEST_IDENTITY_WRONG", "shared-secret")]);
    let path = "/api/v1/files?path=demo.py&language=python";
    let (wrong_header, wrong_timestamp, wrong_nonce) =
        hmac_component_headers("other-secret", "GET", path, b"");
    let response = wait_for_http_raw_response(
        &bind,
        path,
        &[
            ("X-Weft-Component", &wrong_header),
            ("X-Weft-Timestamp", &wrong_timestamp),
            ("X-Weft-Nonce", &wrong_nonce),
        ],
    );
    stop_serve(&mut child);
    let response = response.expect("wrong identity response");
    let body: Value = serde_json::from_str(&response.body).expect("wrong identity body is JSON");

    assert_eq!(response.status_code, 401);
    assert_eq!(body["code"], "UNAUTHENTICATED");
}

#[test]
fn serve_http_capabilities_does_not_require_token() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config_with_token_env(dir.path(), &bind, "WEFT_TEST_TOKEN_CAPS");

    let mut child =
        spawn_serve_with_env(dir.path(), &[("WEFT_TEST_TOKEN_CAPS", "any-token-value")]);
    let response = wait_for_http_response(&bind, "/api/v1/_capabilities");
    stop_serve(&mut child);
    let response = response.expect("capabilities probe response");

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body["registry_backend"], true);
    assert_eq!(response.body["api_version"], 1);
}

#[test]
fn serve_http_refuses_startup_on_non_loopback_without_token() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    // Non-loopback bind + allow_non_loopback opt-in + token_env unset
    // should refuse to start with LMWV-CONFIG-HTTP-NO-AUTH.
    fs::write(
        dir.path().join("loomweave.yaml"),
        "version: 1\nserve:\n  http:\n    enabled: true\n    bind: \"0.0.0.0:0\"\n    \
         allow_non_loopback: true\n    token_env: \"WEFT_TEST_TOKEN_REFUSE\"\n",
    )
    .expect("write non-loopback HTTP config without token env");

    let child = spawn_serve_with_env(dir.path(), &[]);
    let output = wait_for_child_exit(child, Duration::from_secs(2))
        .expect("serve should refuse to start without auth on non-loopback");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LMWV-CONFIG-HTTP-NO-AUTH"),
        "error should cite LMWV-CONFIG-HTTP-NO-AUTH: {stderr}"
    );
    assert!(
        stderr.contains("WEFT_TEST_TOKEN_REFUSE"),
        "error should name the configured token_env: {stderr}"
    );
}

#[test]
fn serve_http_refuses_startup_when_identity_env_is_missing() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let bind = free_loopback_bind();
    write_http_config_with_identity_token_env(dir.path(), &bind, "WEFT_TEST_IDENTITY_MISSING");

    let child = spawn_serve_with_env(dir.path(), &[]);
    let output = wait_for_child_exit(child, Duration::from_secs(2))
        .expect("serve should refuse to start when identity env is missing");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LMWV-CONFIG-HTTP-IDENTITY-MISSING"),
        "error should cite LMWV-CONFIG-HTTP-IDENTITY-MISSING: {stderr}"
    );
    assert!(
        stderr.contains("WEFT_TEST_IDENTITY_MISSING"),
        "error should name the configured identity_token_env: {stderr}"
    );
}

#[test]
fn serve_rejects_non_loopback_http_bind_before_binding_without_opt_in() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "version: 1\nserve:\n  http:\n    enabled: true\n    bind: \"0.0.0.0:0\"\n",
    )
    .expect("write non-loopback HTTP config");

    let child = spawn_serve(dir.path());
    let output = wait_for_child_exit(child, Duration::from_secs(2))
        .expect("serve should reject non-loopback HTTP bind before binding");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unauthenticated non-loopback"),
        "error should explain non-loopback unauthenticated risk: {stderr}"
    );
    assert!(
        stderr.contains("allow_non_loopback"),
        "error should name the explicit opt-in: {stderr}"
    );
}

#[test]
fn serve_rejects_invalid_project_config() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "llm: [not valid for this schema]\n",
    )
    .expect("write invalid config");

    let assert = loomweave_bin()
        .args(["serve", "--path"])
        .arg(dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is utf8");

    assert!(stderr.contains("invalid MCP config"));
}

#[test]
fn serve_wires_recording_llm_provider_and_writer_for_cached_summary_touches() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let source_path = dir.path().join("demo.py");
    fs::write(&source_path, "def entry():\n    return 1\n").expect("write source");
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'python:function:demo.entry', 'python', 'function',
            'python:function:demo.entry', 'entry', ?1,
            1, 2, '{}', 'hash-entry',
            '2026-05-17T00:00:00.000Z', '2026-05-17T00:00:00.000Z'
         )",
        params![source_path.display().to_string()],
    )
    .expect("insert entity");
    conn.execute(
        "INSERT INTO summary_cache (
            entity_id, content_hash, prompt_template_id, model_tier,
            guidance_fingerprint, summary_json, cost_usd, tokens_input,
            tokens_output, created_at, last_accessed_at, caller_count,
            fan_out, stale_semantic
         ) VALUES (
            'python:function:demo.entry', 'hash-entry', ?1, 'anthropic/claude-sonnet-4.6',
            'guidance-empty', '{\"purpose\":\"cached\"}', 0.001, 10,
            5, '2026-05-17T00:00:00.000Z', 'old-touch', 0, 0, 0
         )",
        params![LEAF_SUMMARY_PROMPT_TEMPLATE_ID],
    )
    .expect("insert summary cache");
    drop(conn);
    fs::write(
        dir.path().join("loomweave.yaml"),
        "llm:\n  enabled: true\n  provider: recording\nserve:\n  mcp:\n    enable_write_tools: true\n",
    )
    .expect("write config");

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("loomweave"))
        .args(["serve", "--path"])
        .arg(dir.path())
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
                    "id": 7,
                    "method": "tools/call",
                    "params": {
                        "name": "summary",
                        "arguments": {"id": "python:function:demo.entry"}
                    }
                }))
                .expect("serialize request"),
            },
        )
        .expect("write summary frame");
        stdin.flush().expect("flush summary frame");
    }

    let output = child.wait_with_output().expect("wait for loomweave serve");
    assert!(
        output.status.success(),
        "serve failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(output.stdout));
    let frame =
        read_frame(&mut reader, ContentLengthCeiling::new(usize::MAX)).expect("read response");
    let response: serde_json::Value =
        serde_json::from_slice(&frame.body).expect("response body is json");
    let tool_text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    let envelope: serde_json::Value = serde_json::from_str(tool_text).expect("tool envelope");

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["result"]["cache"]["hit"], true);

    let conn = Connection::open(&db_path).expect("reopen sqlite");
    let touched: String = conn
        .query_row(
            "SELECT last_accessed_at FROM summary_cache WHERE entity_id = ?1",
            params!["python:function:demo.entry"],
            |row| row.get(0),
        )
        .expect("read touched cache row");
    assert_ne!(touched, "old-touch");
}

#[test]
fn serve_routes_summary_miss_through_codex_cli_provider() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_summary_entity(dir.path());

    let fake_codex = dir.path().join("fake-codex");
    let prompt_log = dir.path().join("codex-prompt.log");
    fs::write(
        &fake_codex,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
out=""
schema=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    exec|--json|-)
      shift
      ;;
    --output-last-message)
      out="$2"
      shift 2
      ;;
    --output-schema)
      schema="$2"
      shift 2
      ;;
    --sandbox|--cd|-c|--model|--profile)
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
stdin_prompt="$(cat)"
printf '%s' "$stdin_prompt" > "{prompt_log}"
case "$stdin_prompt" in
  *"Prompt contract: loomweave-agent-provider-v1"*"Do not inspect additional files"*"Source excerpt:"*) ;;
  *) echo "missing Loomweave agent prompt contract" >&2; exit 32 ;;
esac
grep -q '"purpose"' "$schema"
printf '%s\n' '{{"usage":{{"input_tokens":31,"cached_input_tokens":9,"output_tokens":7,"total_tokens":38}}}}'
printf '%s' '{{"purpose":"via codex serve","behavior":"served through fake Codex CLI","relationships":"","risks":""}}' > "$out"
"#,
            prompt_log = prompt_log.display()
        ),
    )
    .expect("write fake codex");
    make_executable(&fake_codex);
    write_provider_config(
        dir.path(),
        "codex_cli",
        r#"
  codex_cli:
    executable: "__EXECUTABLE__"
    model: null
    profile: null
    sandbox: read-only
    timeout_seconds: 5
"#,
        &fake_codex,
    );

    let envelope = call_summary_through_serve(dir.path());

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["result"]["cache"]["hit"], false);
    assert_eq!(envelope["result"]["summary"]["purpose"], "via codex serve");
    assert_eq!(envelope["result"]["usage"]["tokens_cached_input"], 9);
    assert_eq!(envelope["stats_delta"]["summary_tokens_cached_input"], 9);
    assert!(
        fs::read_to_string(prompt_log)
            .expect("read Codex prompt log")
            .contains("Prompt contract: loomweave-agent-provider-v1")
    );
    assert_llm_traffic_log_metadata_only(dir.path(), "codex_cli", 31, 9);
}

#[test]
fn serve_routes_summary_miss_through_claude_cli_provider() {
    let dir = tempfile::tempdir().expect("temp project");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    seed_summary_entity(dir.path());

    let fake_claude = dir.path().join("fake-claude");
    let prompt_log = dir.path().join("claude-prompt.log");
    fs::write(
        &fake_claude,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
schema=""
print_prompt=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -p|--print)
      print_prompt="$2"
      shift 2
      ;;
    --json-schema)
      schema="$2"
      shift 2
      ;;
    --output-format|--permission-mode|--max-turns|--model|--tools)
      shift 2
      ;;
    --no-session-persistence|--exclude-dynamic-system-prompt-sections)
      shift
      ;;
    *)
      shift
      ;;
  esac
done
stdin_prompt="$(cat)"
printf '%s\n%s' "$print_prompt" "$stdin_prompt" > "{prompt_log}"
case "$print_prompt" in
  *"Loomweave's local Claude Code LLM provider"*) ;;
  *) echo "missing Claude print prompt" >&2; exit 41 ;;
esac
case "$stdin_prompt" in
  *"Prompt contract: loomweave-agent-provider-v1"*"Do not inspect additional files"*"Source excerpt:"*) ;;
  *) echo "missing Loomweave agent prompt contract" >&2; exit 42 ;;
esac
case "$schema" in
  *'"purpose"'*'"behavior"'*) ;;
  *) echo "schema missing summary fields" >&2; exit 43 ;;
esac
printf '%s\n' '{{"type":"result","subtype":"success","structured_output":{{"purpose":"via claude serve","behavior":"served through fake Claude CLI","relationships":"","risks":""}},"usage":{{"input_tokens":33,"cached_input_tokens":12,"output_tokens":8,"total_tokens":41}},"total_cost_usd":0.0}}'
"#,
            prompt_log = prompt_log.display()
        ),
    )
    .expect("write fake claude");
    make_executable(&fake_claude);
    write_provider_config(
        dir.path(),
        "claude_cli",
        r#"
  claude_cli:
    executable: "__EXECUTABLE__"
    model: null
    permission_mode: plan
    tools: []
    timeout_seconds: 5
    max_turns: 2
    no_session_persistence: true
    exclude_dynamic_system_prompt_sections: true
"#,
        &fake_claude,
    );

    let envelope = call_summary_through_serve(dir.path());

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["cache"]["hit"], false);
    assert_eq!(envelope["result"]["summary"]["purpose"], "via claude serve");
    assert_eq!(envelope["result"]["usage"]["tokens_cached_input"], 12);
    assert_eq!(envelope["stats_delta"]["summary_tokens_cached_input"], 12);
    assert!(
        fs::read_to_string(prompt_log)
            .expect("read Claude prompt log")
            .contains("Loomweave's local Claude Code LLM provider")
    );
    assert_llm_traffic_log_metadata_only(dir.path(), "claude_cli", 33, 12);
}

fn assert_llm_traffic_log_metadata_only(
    project_root: &Path,
    provider: &str,
    input_tokens: u64,
    cached_input_tokens: u64,
) {
    // C-9: the diagnostics log lives under Loomweave's own .weft/loomweave/
    // store subtree, never a legacy .loomweave/ root (weft-ac59e8e730).
    let log_path = project_root.join(".weft/loomweave/diagnostics/llm-traffic.jsonl");
    assert!(
        !project_root.join(".loomweave").exists(),
        "serve must not create a legacy .loomweave/ store dir"
    );
    let log = fs::read_to_string(&log_path)
        .unwrap_or_else(|err| panic!("read LLM traffic log {}: {err}", log_path.display()));
    assert!(
        !log.contains("Prompt contract:")
            && !log.contains("Source excerpt:")
            && !log.contains("via codex serve")
            && !log.contains("via claude serve"),
        "LLM traffic log must contain lookup metadata only, not prompt/output contents: {log}"
    );
    let event: serde_json::Value = serde_json::from_str(log.trim()).expect("traffic log JSON");
    assert_eq!(event["schema"], "loomweave.llm.lookup.v1");
    assert_eq!(event["provider"], provider);
    assert_eq!(event["purpose"], "Summary");
    assert_eq!(event["prompt_id"], "leaf-v1");
    assert_eq!(event["outcome"], "success");
    assert_eq!(event["usage"]["input_tokens"], input_tokens);
    assert_eq!(event["usage"]["cached_input_tokens"], cached_input_tokens);
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod executable");
}

fn seed_summary_entity(project_root: &Path) {
    let source = "def entry():\n    return 1\n";
    let source_path = project_root.join("demo.py");
    fs::write(&source_path, source).expect("write source");
    let content_hash = line_range_content_hash(source, 1, 2);
    let db_path = project_root.join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'python:function:demo.entry', 'python', 'function',
            'python:function:demo.entry', 'entry', ?1,
            1, 2, '{}', ?2,
            '2026-05-17T00:00:00.000Z', '2026-05-17T00:00:00.000Z'
         )",
        params![source_path.display().to_string(), content_hash],
    )
    .expect("insert summary entity");
}

fn line_range_content_hash(source: &str, start_line: usize, end_line: usize) -> String {
    let lines = source.lines().collect::<Vec<_>>();
    let start = start_line.saturating_sub(1);
    let end = end_line.min(lines.len());
    blake3::hash(lines[start..end].join("\n").as_bytes())
        .to_hex()
        .to_string()
}

fn write_provider_config(
    project_root: &Path,
    provider: &str,
    provider_block: &str,
    executable: &Path,
) {
    let executable_yaml =
        serde_json::to_string(&executable.display().to_string()).expect("quote executable path");
    let provider_block = provider_block
        .trim_start_matches('\n')
        .replace("\"__EXECUTABLE__\"", &executable_yaml);
    fs::write(
        project_root.join("loomweave.yaml"),
        format!(
            concat!(
                "version: 1\n",
                "llm:\n",
                "  enabled: true\n",
                "  provider: {provider}\n",
                "  allow_live_provider: true\n",
                "{provider_block}",
                "  model_id: {provider}-test\n",
                "  session_token_ceiling: 1000000\n",
                "  max_inferred_edges_per_caller: 8\n",
                "  cache_max_age_days: 180\n",
                "serve:\n",
                "  mcp:\n",
                "    enable_write_tools: true\n",
            ),
            provider = provider,
            provider_block = provider_block,
        ),
    )
    .expect("write provider config");
}

fn call_summary_through_serve(project_root: &Path) -> Value {
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("loomweave"))
        .args(["serve", "--path"])
        .arg(project_root)
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
                    "id": "summary",
                    "method": "tools/call",
                    "params": {
                        "name": "summary",
                        "arguments": {"id": "python:function:demo.entry"}
                    }
                }))
                .expect("serialize request"),
            },
        )
        .expect("write summary frame");
        stdin.flush().expect("flush summary frame");
    }

    let output = child.wait_with_output().expect("wait for loomweave serve");
    let config_debug = fs::read_to_string(project_root.join("loomweave.yaml"))
        .unwrap_or_else(|err| format!("failed to read loomweave.yaml: {err}"));
    assert!(
        output.status.success(),
        "serve failed: {}\nloomweave.yaml:\n{}",
        String::from_utf8_lossy(&output.stderr),
        config_debug
    );
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(output.stdout));
    let frame =
        read_frame(&mut reader, ContentLengthCeiling::new(usize::MAX)).expect("read response");
    let response: serde_json::Value =
        serde_json::from_slice(&frame.body).expect("response body is json");
    let tool_text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    serde_json::from_str(tool_text).expect("tool envelope")
}

fn load_contract_fixture(fixture_name: &str, source: &str) -> Value {
    let fixture: Value = serde_json::from_str(source).expect("parse contract fixture");
    assert!(
        fixture.get("_meta").and_then(Value::as_object).is_some(),
        "{fixture_name} missing top-level _meta object"
    );
    for field in [
        "contract",
        "stability",
        "authority",
        "verification",
        "updated",
    ] {
        assert!(
            fixture.pointer(&format!("/_meta/{field}")).is_some(),
            "{fixture_name} missing required _meta.{field}"
        );
    }
    assert!(
        fixture
            .pointer("/shape_decl/shapes")
            .and_then(Value::as_object)
            .is_some(),
        "{fixture_name} missing top-level shape_decl.shapes object"
    );
    let examples = fixture
        .get("examples")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{fixture_name} missing top-level examples array"));
    assert!(
        !examples.is_empty(),
        "{fixture_name} must declare at least one example"
    );
    fixture
}

fn fixture_example_body<'a>(fixture: &'a Value, example_name: &str) -> &'a Value {
    let examples = fixture
        .get("examples")
        .and_then(Value::as_array)
        .expect("examples array");
    examples
        .iter()
        .find(|example| example.get("name").and_then(Value::as_str) == Some(example_name))
        .and_then(|example| example.pointer("/response/body"))
        .unwrap_or_else(|| panic!("missing fixture example body {example_name}"))
}

fn validate_fixture_examples(bind: &str, fixture: &Value, fixture_name: &str) {
    validate_fixture_examples_matching(bind, fixture, fixture_name, |_| true);
}

fn validate_fixture_examples_matching(
    bind: &str,
    fixture: &Value,
    fixture_name: &str,
    should_validate: impl Fn(&str) -> bool,
) {
    let shapes = fixture
        .pointer("/shape_decl/shapes")
        .and_then(Value::as_object)
        .expect("shape_decl.shapes object");
    let examples = fixture
        .get("examples")
        .and_then(Value::as_array)
        .expect("examples array");
    for example in examples {
        let example_name = example
            .get("name")
            .and_then(Value::as_str)
            .expect("example name");
        if !should_validate(example_name) {
            continue;
        }
        let method = example
            .pointer("/request/method")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{fixture_name}:{example_name} missing request.method"));
        let path = example
            .pointer("/request/path")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{fixture_name}:{example_name} missing request.path"));
        let expected_status = example
            .pointer("/response/status")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{fixture_name}:{example_name} missing response.status"));
        let expected_body = example
            .pointer("/response/body")
            .unwrap_or_else(|| panic!("{fixture_name}:{example_name} missing response.body"));
        let shape_name = example
            .pointer("/response/shape")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{fixture_name}:{example_name} missing response.shape"));

        let response = match method {
            "GET" => wait_for_http_response(bind, path).unwrap_or_else(|err| {
                panic!("{fixture_name}:{example_name} HTTP request failed: {err}")
            }),
            "POST" => {
                let body = fixture_request_body(example, fixture_name, example_name);
                wait_for_http_post_json(bind, path, &body, &[]).unwrap_or_else(|err| {
                    panic!("{fixture_name}:{example_name} HTTP POST request failed: {err}")
                })
            }
            other => panic!("{fixture_name}:{example_name} unsupported method {other}"),
        };

        assert_eq!(
            u64::from(response.status_code),
            expected_status,
            "{fixture_name}:{example_name} status mismatch"
        );
        let shape = shapes
            .get(shape_name)
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{fixture_name}:{example_name} missing shape {shape_name}"));
        assert_status_allowed(shape, response.status_code, fixture_name, example_name);
        assert_body_matches_shape(
            shape,
            &response.body,
            fixture_name,
            example_name,
            shape_name,
        );
        assert_normative_example_fields(
            &response.body,
            expected_body,
            shape_name,
            fixture_name,
            example_name,
        );
    }
}

fn fixture_request_body(example: &Value, fixture_name: &str, example_name: &str) -> String {
    let body = example
        .pointer("/request/body")
        .unwrap_or_else(|| panic!("{fixture_name}:{example_name} missing request.body"));
    if fixture_name == "post-api-v1-files-batch.json" && example_name == "batch_too_large_400" {
        let queries: Vec<Value> = (0..257)
            .map(|index| serde_json::json!({"path": format!("p{index}.py"), "language": ""}))
            .collect();
        return serde_json::json!({"queries": queries}).to_string();
    }
    body.to_string()
}

fn assert_normative_example_fields(
    actual: &Value,
    expected: &Value,
    shape_name: &str,
    fixture_name: &str,
    example_name: &str,
) {
    if shape_name == "error_envelope" {
        assert_eq!(
            actual.get("code"),
            expected.get("code"),
            "{fixture_name}:{example_name} error code mismatch"
        );
        return;
    }
    assert_eq!(
        actual, expected,
        "{fixture_name}:{example_name} body mismatch"
    );
}

fn assert_status_allowed(
    shape: &serde_json::Map<String, Value>,
    status_code: u16,
    fixture_name: &str,
    example_name: &str,
) {
    if let Some(status) = shape.get("status").and_then(Value::as_u64) {
        assert_eq!(
            status,
            u64::from(status_code),
            "{fixture_name}:{example_name} status is not allowed by shape"
        );
        return;
    }
    let allowed = shape
        .get("status_any")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{fixture_name}:{example_name} shape missing status/status_any"));
    assert!(
        allowed
            .iter()
            .any(|candidate| candidate.as_u64() == Some(u64::from(status_code))),
        "{fixture_name}:{example_name} status {status_code} is not in status_any {allowed:?}"
    );
}

fn assert_body_matches_shape(
    shape: &serde_json::Map<String, Value>,
    body: &Value,
    fixture_name: &str,
    example_name: &str,
    shape_name: &str,
) {
    let body = body
        .as_object()
        .unwrap_or_else(|| panic!("{fixture_name}:{example_name} body is not an object"));
    let required_fields = shape
        .get("required_fields")
        .and_then(Value::as_object)
        .unwrap_or_else(|| {
            panic!("{fixture_name}:{example_name} shape {shape_name} missing required_fields")
        });
    for (field, field_decl) in required_fields {
        let value = body
            .get(field)
            .unwrap_or_else(|| panic!("{fixture_name}:{example_name} missing field {field}"));
        assert_value_matches_decl(value, field_decl, fixture_name, example_name, field);
    }
    if let Some(forbidden_fields) = shape.get("forbidden_fields").and_then(Value::as_array) {
        for field in forbidden_fields {
            let field = field.as_str().unwrap_or_else(|| {
                panic!("{fixture_name}:{example_name} forbidden field entry is not a string")
            });
            assert!(
                !body.contains_key(field),
                "{fixture_name}:{example_name} field {field} is forbidden by {shape_name}"
            );
        }
    }
    let allow_extra_fields = shape
        .get("allow_extra_fields")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !allow_extra_fields {
        for field in body.keys() {
            assert!(
                required_fields.contains_key(field),
                "{fixture_name}:{example_name} unexpected field {field}"
            );
        }
    }
}

fn assert_value_matches_decl(
    value: &Value,
    field_decl: &Value,
    fixture_name: &str,
    example_name: &str,
    field: &str,
) {
    if let Some(expected) = field_decl.get("const") {
        assert_eq!(
            value, expected,
            "{fixture_name}:{example_name} field {field} const mismatch"
        );
    }
    if let Some(allowed) = field_decl.get("enum").and_then(Value::as_array) {
        assert!(
            allowed.iter().any(|candidate| candidate == value),
            "{fixture_name}:{example_name} field {field} value {value:?} not in {allowed:?}"
        );
    }
    let type_name = field_decl
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{fixture_name}:{example_name} field {field} missing type"));
    match type_name {
        "boolean" => {
            assert!(
                value.as_bool().is_some(),
                "{fixture_name}:{example_name} field {field} is not a boolean"
            );
        }
        "integer" => {
            assert!(
                value.as_i64().is_some() || value.as_u64().is_some(),
                "{fixture_name}:{example_name} field {field} is not an integer"
            );
        }
        "array" => {
            assert!(
                value.as_array().is_some(),
                "{fixture_name}:{example_name} field {field} is not an array"
            );
        }
        "array_of_strings" => {
            let values = value.as_array().unwrap_or_else(|| {
                panic!("{fixture_name}:{example_name} field {field} is not an array")
            });
            for item in values {
                assert!(
                    item.as_str().is_some(),
                    "{fixture_name}:{example_name} field {field} contains a non-string item"
                );
            }
        }
        "array_of_resolved_items" => {
            let values = value.as_array().unwrap_or_else(|| {
                panic!("{fixture_name}:{example_name} field {field} is not an array")
            });
            for item in values {
                assert_batch_resolved_item(item, fixture_name, example_name, field);
            }
        }
        "array_of_error_items" => {
            let values = value.as_array().unwrap_or_else(|| {
                panic!("{fixture_name}:{example_name} field {field} is not an array")
            });
            for item in values {
                assert_batch_error_item(item, fixture_name, example_name, field);
            }
        }
        "non_empty_string" => {
            let value = value.as_str().unwrap_or_else(|| {
                panic!("{fixture_name}:{example_name} field {field} is not a string")
            });
            assert!(
                !value.is_empty(),
                "{fixture_name}:{example_name} field {field} is empty"
            );
        }
        "uuid" => {
            let value = value.as_str().unwrap_or_else(|| {
                panic!("{fixture_name}:{example_name} field {field} is not a string")
            });
            Uuid::parse_str(value)
                .unwrap_or_else(|err| panic!("{fixture_name}:{example_name} invalid UUID: {err}"));
        }
        "adr003_file_entity_id" => {
            let value = value.as_str().unwrap_or_else(|| {
                panic!("{fixture_name}:{example_name} field {field} is not a string")
            });
            assert!(
                value
                    .strip_prefix("core:file:")
                    .is_some_and(|qualified_name| {
                        !qualified_name.is_empty()
                            && !qualified_name.contains('@')
                            && !qualified_name.contains('\\')
                    }),
                "{fixture_name}:{example_name} field {field} is not an ADR-003 file ID"
            );
        }
        "project_relative_path" => {
            let value = value.as_str().unwrap_or_else(|| {
                panic!("{fixture_name}:{example_name} field {field} is not a string")
            });
            assert!(
                !value.is_empty()
                    && !value.starts_with('/')
                    && !value.starts_with("./")
                    && !value.contains('\\'),
                "{fixture_name}:{example_name} field {field} is not a project-relative path"
            );
        }
        "object" => {
            assert!(
                value.is_object(),
                "{fixture_name}:{example_name} field {field} is not an object"
            );
        }
        other => panic!("{fixture_name}:{example_name} unknown field type {other} for {field}"),
    }
}

fn assert_batch_resolved_item(item: &Value, fixture_name: &str, example_name: &str, field: &str) {
    let item = item.as_object().unwrap_or_else(|| {
        panic!("{fixture_name}:{example_name} field {field} contains a non-object item")
    });
    for required in [
        "requested_path",
        "entity_id",
        "content_hash",
        "canonical_path",
        "language",
    ] {
        let value = item.get(required).unwrap_or_else(|| {
            panic!("{fixture_name}:{example_name} field {field} item missing {required}")
        });
        assert!(
            value.as_str().is_some_and(|value| !value.is_empty()),
            "{fixture_name}:{example_name} field {field} item {required} is not a non-empty string"
        );
    }
    assert!(
        item.keys().all(|key| [
            "requested_path",
            "entity_id",
            "content_hash",
            "canonical_path",
            "language"
        ]
        .contains(&key.as_str())),
        "{fixture_name}:{example_name} field {field} resolved item has unexpected keys: {item:?}"
    );
}

fn assert_batch_error_item(item: &Value, fixture_name: &str, example_name: &str, field: &str) {
    let item = item.as_object().unwrap_or_else(|| {
        panic!("{fixture_name}:{example_name} field {field} contains a non-object item")
    });
    for required in ["requested_path", "code", "message"] {
        let value = item.get(required).unwrap_or_else(|| {
            panic!("{fixture_name}:{example_name} field {field} item missing {required}")
        });
        assert!(
            value.as_str().is_some_and(|value| !value.is_empty()),
            "{fixture_name}:{example_name} field {field} item {required} is not a non-empty string"
        );
    }
    assert!(
        item.keys()
            .all(|key| ["requested_path", "code", "message"].contains(&key.as_str())),
        "{fixture_name}:{example_name} field {field} error item has unexpected keys: {item:?}"
    );
}

fn seed_file_entity(project_root: &Path) -> (String, String, String) {
    let source_path = project_root.join("demo.py");
    fs::write(&source_path, "def entry():\n    return 1\n").expect("write source");
    let canonical_path = source_path
        .canonicalize()
        .expect("canonical source path")
        .display()
        .to_string();
    let content_hash = "hash-demo-file".to_owned();
    let file_id = "core:file:demo.py".to_owned();
    let db_path = project_root.join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            ?1, 'core', 'file', 'demo.py', 'demo.py', ?2,
            1, 2, '{}', ?3,
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![file_id, canonical_path, content_hash],
    )
    .expect("insert file entity");
    (file_id, content_hash, "demo.py".to_owned())
}

fn seed_custom_language_file_entity(project_root: &Path) {
    let source_path = project_root.join("demo.custom");
    fs::write(&source_path, "custom source\n").expect("write custom source");
    let canonical_path = source_path
        .canonicalize()
        .expect("canonical source path")
        .display()
        .to_string();
    let db_path = project_root.join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'core:file:demo.custom', 'core', 'file', 'demo.custom', 'demo.custom', ?1,
            1, 1, '{\"language\":\"manifestlang\"}', 'hash-demo-custom',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical_path],
    )
    .expect("insert custom language file entity");
}

fn seed_briefing_blocked_file_entity(project_root: &Path) {
    let source_path = project_root.join("blocked.py");
    fs::write(&source_path, "secret = \"redacted\"\n").expect("write blocked source");
    let canonical_path = source_path
        .canonicalize()
        .expect("canonical blocked path")
        .display()
        .to_string();
    let db_path = project_root.join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            'core:file:blocked.py', 'core', 'file',
            'blocked.py', 'blocked.py', ?1,
            1, 2,
            '{\"briefing_blocked\":\"pre-ingest secret scan flagged this file\"}',
            'hash-blocked',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical_path],
    )
    .expect("insert briefing-blocked file entity");
}

fn seed_storage_failure_file_entity(project_root: &Path) {
    let source_path = project_root.join("missing-on-disk.py");
    fs::write(&source_path, "def missing():\n    return 1\n").expect("write source");
    let canonical_path = source_path
        .canonicalize()
        .expect("canonical source path")
        .display()
        .to_string();
    let db_path = project_root.join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path,
            source_line_start, source_line_end, properties, created_at, updated_at
         ) VALUES (
            'core:file:missing-on-disk.py', 'core', 'file',
            'missing-on-disk.py', 'missing-on-disk.py', ?1,
            1, 2, '{}',
            '2026-05-19T00:00:00.000Z', '2026-05-19T00:00:00.000Z'
         )",
        params![canonical_path],
    )
    .expect("insert file entity without cached hash");
    drop(conn);
    fs::remove_file(&source_path).expect("remove cataloged file to force storage failure");
}

fn free_loopback_bind() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free loopback port");
    let bind = listener.local_addr().expect("local addr").to_string();
    RESERVED_LOOPBACK_BINDS
        .lock()
        .expect("reserved loopback bind lock")
        .push((bind.clone(), listener));
    bind
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ServeTestConfig {
    serve: ServeTestSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ServeTestSection {
    http: ServeHttpTestSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ServeHttpTestSection {
    bind: Option<String>,
}

fn release_reserved_loopback_bind(project_root: &Path) {
    let Some(bind) = configured_http_bind(project_root) else {
        return;
    };
    let mut reserved = RESERVED_LOOPBACK_BINDS
        .lock()
        .expect("reserved loopback bind lock");
    if let Some(index) = reserved
        .iter()
        .position(|(reserved_bind, _)| reserved_bind == &bind)
    {
        reserved.swap_remove(index);
    }
}

fn configured_http_bind(project_root: &Path) -> Option<String> {
    let raw = fs::read_to_string(project_root.join("loomweave.yaml")).ok()?;
    let parsed: ServeTestConfig = serde_norway::from_str(&raw).ok()?;
    parsed.serve.http.bind
}

fn write_stdio_config(project_root: &Path) {
    fs::write(
        project_root.join("loomweave.yaml"),
        "version: 1\nserve:\n  http:\n    enabled: false\n",
    )
    .expect("write MCP stdio-only serve config");
}

fn write_http_config(project_root: &Path, bind: &str) {
    fs::write(
        project_root.join("loomweave.yaml"),
        format!("version: 1\nserve:\n  http:\n    enabled: true\n    bind: \"{bind}\"\n"),
    )
    .expect("write HTTP serve config");
}

fn write_http_config_with_token_env(project_root: &Path, bind: &str, token_env: &str) {
    fs::write(
        project_root.join("loomweave.yaml"),
        format!(
            "version: 1\nserve:\n  http:\n    enabled: true\n    bind: \"{bind}\"\n    token_env: \"{token_env}\"\n"
        ),
    )
    .expect("write HTTP serve config with token_env");
}

fn write_http_config_with_identity_token_env(project_root: &Path, bind: &str, token_env: &str) {
    fs::write(
        project_root.join("loomweave.yaml"),
        format!(
            "version: 1\nserve:\n  http:\n    enabled: true\n    bind: \"{bind}\"\n    identity_token_env: \"{token_env}\"\n"
        ),
    )
    .expect("write HTTP serve config with identity_token_env");
}

fn hmac_component_headers(
    secret: &str,
    method: &str,
    path_and_query: &str,
    body: &[u8],
) -> (String, String, String) {
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let nonce = Uuid::new_v4().to_string();
    let component = hmac_component_header_with_freshness(
        secret,
        method,
        path_and_query,
        body,
        timestamp,
        &nonce,
    );
    (component, timestamp.to_string(), nonce)
}

fn hmac_component_header_with_freshness(
    secret: &str,
    method: &str,
    path_and_query: &str,
    body: &[u8],
    timestamp: i64,
    nonce: &str,
) -> String {
    format!(
        "loomweave:{}",
        hmac_sha256_hex(
            secret.as_bytes(),
            canonical_hmac_message(method, path_and_query, body, timestamp, nonce).as_bytes()
        )
    )
}

fn canonical_hmac_message(
    method: &str,
    path_and_query: &str,
    body: &[u8],
    timestamp: i64,
    nonce: &str,
) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        method,
        path_and_query,
        hex_lower(&Sha256::digest(body)),
        timestamp,
        nonce
    )
}

fn hmac_sha256_hex(secret: &[u8], message: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts keys of any size");
    mac.update(message);
    hex_lower(&mac.finalize().into_bytes())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

struct ServeChild {
    child: Option<Child>,
}

impl ServeChild {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            stop_child(&mut child);
        }
    }

    fn wait_with_output(mut self) -> std::io::Result<std::process::Output> {
        self.child
            .take()
            .expect("serve child was already stopped")
            .wait_with_output()
    }
}

impl std::ops::Deref for ServeChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        self.child.as_ref().expect("serve child was stopped")
    }
}

impl std::ops::DerefMut for ServeChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut().expect("serve child was stopped")
    }
}

impl Drop for ServeChild {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_serve(project_root: &Path) -> ServeChild {
    spawn_serve_with_env(project_root, &[])
}

fn spawn_serve_with_env(project_root: &Path, env: &[(&str, &str)]) -> ServeChild {
    release_reserved_loopback_bind(project_root);
    let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("loomweave"));
    command
        .args(["serve", "--path"])
        .arg(project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    ServeChild::new(command.spawn().expect("spawn loomweave serve"))
}

fn stop_serve(child: &mut ServeChild) {
    child.stop();
}

fn stop_child(child: &mut Child) {
    drop(child.stdin.take());
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_for_child_exit(mut child: ServeChild, timeout: Duration) -> Option<std::process::Output> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().expect("poll child").is_some() {
            return Some(child.wait_with_output().expect("collect child output"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    child.stop();
    None
}

fn wait_for_http_json(bind: &str, path: &str) -> Result<Value, String> {
    let response = wait_for_http_response(bind, path)?;
    if response.status_code != 200 {
        return Err(format!(
            "unexpected HTTP status {}; body: {}",
            response.status_code, response.body
        ));
    }
    Ok(response.body)
}

fn wait_for_http_response(bind: &str, path: &str) -> Result<HttpJsonResponse, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match http_get_response(bind, path) {
            Ok(response) => return Ok(response),
            Err(err) => last_error = err,
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(last_error)
}

fn wait_for_http_raw_response(
    bind: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<HttpRawResponse, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match http_raw_response(bind, path, headers) {
            Ok(response) => return Ok(response),
            Err(err) => last_error = err,
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(last_error)
}

fn wait_for_http_post_json(
    bind: &str,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> Result<HttpJsonResponse, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match http_post_json(bind, path, body, headers) {
            Ok(response) => return Ok(response),
            Err(err) => last_error = err,
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(last_error)
}

fn http_post_json(
    bind: &str,
    path: &str,
    body: &str,
    request_headers: &[(&str, &str)],
) -> Result<HttpJsonResponse, String> {
    let addr = bind
        .parse()
        .map_err(|err| format!("parse bind address {bind}: {err}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(100))
        .map_err(|err| format!("connect to {bind}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| format!("set read timeout: {err}"))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {bind}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    )
    .map_err(|err| format!("write request head: {err}"))?;
    for (name, value) in request_headers {
        write!(stream, "{name}: {value}\r\n")
            .map_err(|err| format!("write request header {name}: {err}"))?;
    }
    write!(stream, "Connection: close\r\n\r\n")
        .map_err(|err| format!("write request terminator: {err}"))?;
    stream
        .write_all(body.as_bytes())
        .map_err(|err| format!("write request body: {err}"))?;
    let mut reader = std::io::BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|err| format!("read status line: {err}"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed HTTP status line: {status_line}"))?
        .parse::<u16>()
        .map_err(|err| format!("parse HTTP status from {status_line:?}: {err}"))?;
    let mut content_length = None;
    let mut header = String::new();
    loop {
        header.clear();
        reader
            .read_line(&mut header)
            .map_err(|err| format!("read header: {err}"))?;
        if header == "\r\n" || header == "\n" || header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|err| format!("parse content-length from {header:?}: {err}"))?,
            );
        }
    }
    let mut body = String::new();
    if let Some(content_length) = content_length {
        let mut bytes = vec![0_u8; content_length];
        reader
            .read_exact(&mut bytes)
            .map_err(|err| format!("read response body: {err}"))?;
        body = String::from_utf8(bytes).map_err(|err| format!("response body is utf8: {err}"))?;
    } else {
        reader
            .read_to_string(&mut body)
            .map_err(|err| format!("read response body: {err}"))?;
    }
    let body =
        serde_json::from_str(&body).map_err(|err| format!("parse json body {body:?}: {err}"))?;
    Ok(HttpJsonResponse { status_code, body })
}

fn wait_for_http_get_with_body_status(
    bind: &str,
    path: &str,
    body_len: usize,
) -> Result<u16, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let body = vec![b'x'; body_len];
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match http_get_with_body_status(bind, path, &body) {
            Ok(status_code) => return Ok(status_code),
            Err(err) => last_error = err,
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(last_error)
}

fn http_get_with_body_status(bind: &str, path: &str, body: &[u8]) -> Result<u16, String> {
    let addr = bind
        .parse()
        .map_err(|err| format!("parse bind address {bind}: {err}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(100))
        .map_err(|err| format!("connect to {bind}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| format!("set read timeout: {err}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {bind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .map_err(|err| format!("write request head: {err}"))?;
    stream
        .write_all(body)
        .map_err(|err| format!("write request body: {err}"))?;
    let mut reader = std::io::BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|err| format!("read status line: {err}"))?;
    status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed HTTP status line: {status_line}"))?
        .parse::<u16>()
        .map_err(|err| format!("parse HTTP status from {status_line:?}: {err}"))
}

fn http_raw_response(
    bind: &str,
    path: &str,
    request_headers: &[(&str, &str)],
) -> Result<HttpRawResponse, String> {
    let addr = bind
        .parse()
        .map_err(|err| format!("parse bind address {bind}: {err}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(100))
        .map_err(|err| format!("connect to {bind}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| format!("set read timeout: {err}"))?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {bind}\r\n")
        .map_err(|err| format!("write request line: {err}"))?;
    for (name, value) in request_headers {
        write!(stream, "{name}: {value}\r\n")
            .map_err(|err| format!("write request header {name}: {err}"))?;
    }
    write!(stream, "Connection: close\r\n\r\n")
        .map_err(|err| format!("write request terminator: {err}"))?;

    let mut reader = std::io::BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|err| format!("read status line: {err}"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed HTTP status line: {status_line}"))?
        .parse::<u16>()
        .map_err(|err| format!("parse HTTP status from {status_line:?}: {err}"))?;
    let mut content_length = None;
    let mut response_headers = Vec::new();
    let mut header = String::new();
    loop {
        header.clear();
        reader
            .read_line(&mut header)
            .map_err(|err| format!("read header: {err}"))?;
        if header == "\r\n" || header == "\n" || header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let name = name.trim().to_owned();
            let value = value.trim().to_owned();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|err| format!("parse content-length from {header:?}: {err}"))?,
                );
            }
            response_headers.push((name, value));
        }
    }
    let mut body = String::new();
    if let Some(content_length) = content_length {
        let mut bytes = vec![0_u8; content_length];
        reader
            .read_exact(&mut bytes)
            .map_err(|err| format!("read response body: {err}"))?;
        body = String::from_utf8(bytes).map_err(|err| format!("response body is utf8: {err}"))?;
    } else {
        reader
            .read_to_string(&mut body)
            .map_err(|err| format!("read response body: {err}"))?;
    }
    Ok(HttpRawResponse {
        status_code,
        headers: response_headers,
        body,
    })
}

fn http_get_response(bind: &str, path: &str) -> Result<HttpJsonResponse, String> {
    let addr = bind
        .parse()
        .map_err(|err| format!("parse bind address {bind}: {err}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(100))
        .map_err(|err| format!("connect to {bind}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| format!("set read timeout: {err}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|err| format!("write request: {err}"))?;
    let mut reader = std::io::BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|err| format!("read status line: {err}"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed HTTP status line: {status_line}"))?
        .parse::<u16>()
        .map_err(|err| format!("parse HTTP status from {status_line:?}: {err}"))?;
    let mut content_length = None;
    let mut header = String::new();
    loop {
        header.clear();
        reader
            .read_line(&mut header)
            .map_err(|err| format!("read header: {err}"))?;
        if header == "\r\n" || header == "\n" || header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|err| format!("parse content-length from {header:?}: {err}"))?,
            );
        }
    }
    let mut body = String::new();
    if let Some(content_length) = content_length {
        let mut bytes = vec![0_u8; content_length];
        reader
            .read_exact(&mut bytes)
            .map_err(|err| format!("read response body: {err}"))?;
        body = String::from_utf8(bytes).map_err(|err| format!("response body is utf8: {err}"))?;
    } else {
        reader
            .read_to_string(&mut body)
            .map_err(|err| format!("read response body: {err}"))?;
    }
    let body =
        serde_json::from_str(&body).map_err(|err| format!("parse json body {body:?}: {err}"))?;
    Ok(HttpJsonResponse { status_code, body })
}
