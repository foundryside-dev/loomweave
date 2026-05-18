use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};

use assert_cmd::Command;
use clarion_core::{
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID,
    plugin::{ContentLengthCeiling, Frame, read_frame, write_frame},
};
use rusqlite::{Connection, params};
use serde_json::Value;

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
            'python:function:demo.entry', 'hash-entry', ?1, 'anthropic/claude-sonnet-4.6',
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

#[test]
fn serve_routes_summary_miss_through_codex_cli_provider() {
    let dir = tempfile::tempdir().expect("temp project");
    clarion_bin()
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
  *"Prompt contract: clarion-agent-provider-v1"*"Do not inspect additional files"*"Source excerpt:"*) ;;
  *) echo "missing Clarion agent prompt contract" >&2; exit 32 ;;
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

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["cache"]["hit"], false);
    assert_eq!(envelope["result"]["summary"]["purpose"], "via codex serve");
    assert_eq!(envelope["result"]["usage"]["tokens_cached_input"], 9);
    assert_eq!(envelope["stats_delta"]["summary_tokens_cached_input"], 9);
    assert!(
        fs::read_to_string(prompt_log)
            .expect("read Codex prompt log")
            .contains("Prompt contract: clarion-agent-provider-v1")
    );
}

#[test]
fn serve_routes_summary_miss_through_claude_cli_provider() {
    let dir = tempfile::tempdir().expect("temp project");
    clarion_bin()
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
  *"Clarion's local Claude Code LLM provider"*) ;;
  *) echo "missing Claude print prompt" >&2; exit 41 ;;
esac
case "$stdin_prompt" in
  *"Prompt contract: clarion-agent-provider-v1"*"Do not inspect additional files"*"Source excerpt:"*) ;;
  *) echo "missing Clarion agent prompt contract" >&2; exit 42 ;;
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
            .contains("Clarion's local Claude Code LLM provider")
    );
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
    let db_path = project_root.join(".clarion/clarion.db");
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
        project_root.join("clarion.yaml"),
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
            ),
            provider = provider,
            provider_block = provider_block,
        ),
    )
    .expect("write provider config");
}

fn call_summary_through_serve(project_root: &Path) -> Value {
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("clarion"))
        .args(["serve", "--path"])
        .arg(project_root)
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

    let output = child.wait_with_output().expect("wait for clarion serve");
    let config_debug = fs::read_to_string(project_root.join("clarion.yaml"))
        .unwrap_or_else(|err| format!("failed to read clarion.yaml: {err}"));
    assert!(
        output.status.success(),
        "serve failed: {}\nclarion.yaml:\n{}",
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
