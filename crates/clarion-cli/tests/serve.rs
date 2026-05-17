use std::fs;
use std::io::Write;
use std::process::{Command as StdCommand, Stdio};

use assert_cmd::Command;
use clarion_core::{
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID,
    plugin::{ContentLengthCeiling, Frame, read_frame, write_frame},
};
use rusqlite::{Connection, params};

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[test]
fn serve_help_advertises_mcp_stdio_server() {
    let assert = clarion_bin().args(["serve", "--help"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("help is utf8");

    assert!(stdout.contains("Run the MCP stdio server"));
    assert!(stdout.contains("--path"));
}

#[test]
fn serve_stdio_initialize_round_trip() {
    let dir = tempfile::tempdir().expect("temp project");
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("clarion"))
        .args(["serve", "--path"])
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn clarion serve");

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

    let output = child.wait_with_output().expect("wait for clarion serve");
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
    assert_eq!(response["result"]["serverInfo"]["name"], "clarion");
}

#[test]
fn serve_rejects_invalid_project_config() {
    let dir = tempfile::tempdir().expect("temp project");
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    fs::write(
        dir.path().join("clarion.yaml"),
        "llm: [not valid for this schema]\n",
    )
    .expect("write invalid config");

    let assert = clarion_bin()
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
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .env("PATH", "")
        .assert()
        .success();
    let source_path = dir.path().join("demo.py");
    fs::write(&source_path, "def entry():\n    return 1\n").expect("write source");
    let db_path = dir.path().join(".clarion/clarion.db");
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
            'python:function:demo.entry', 'hash-entry', ?1, 'claude-haiku-4-5',
            'guidance-empty', '{\"purpose\":\"cached\"}', 0.001, 10,
            5, '2026-05-17T00:00:00.000Z', 'old-touch', 0, 0, 0
         )",
        params![LEAF_SUMMARY_PROMPT_TEMPLATE_ID],
    )
    .expect("insert summary cache");
    drop(conn);
    fs::write(
        dir.path().join("clarion.yaml"),
        "llm:\n  enabled: true\n  provider: recording\n",
    )
    .expect("write config");

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("clarion"))
        .args(["serve", "--path"])
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn clarion serve");
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

    let output = child.wait_with_output().expect("wait for clarion serve");
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

    assert_eq!(envelope["ok"], true);
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
