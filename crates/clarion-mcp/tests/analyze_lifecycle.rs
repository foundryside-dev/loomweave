//! MCP analyze lifecycle tools: `analyze_start` / `analyze_status` /
//! `analyze_cancel` (clarion-7e0c21558a).
//!
//! These drive the tools against a stub launcher injected via
//! `with_analyze_command`, so the lifecycle (background start, live status,
//! group-kill cancel) is exercised without a real multi-minute analyze. The
//! stub spawns a grandchild `sleep` and records its pid, letting the cancel
//! test assert the whole process group — not just the launcher — is killed.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clarion_mcp::{McpToolPolicy, ServerState};
use clarion_storage::{ReaderPool, pragma, schema};
use rusqlite::Connection;
use serde_json::{Value, json};

fn open_project() -> (tempfile::TempDir, PathBuf) {
    let project = tempfile::tempdir().expect("temp project");
    let clarion_dir = project.path().join(".clarion");
    std::fs::create_dir(&clarion_dir).expect("create .clarion");
    let db_path = clarion_dir.join("clarion.db");
    let mut conn = Connection::open(&db_path).expect("open sqlite");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    drop(conn);
    (project, db_path)
}

fn state_for(project_root: &Path, db_path: &Path, stub: &Path) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool)
        .with_tool_policy(McpToolPolicy::allow_write_tools())
        .with_analyze_command(stub.to_path_buf())
}

/// Write an executable stub that stands in for `clarion analyze`: it parses
/// `--progress-file`, spawns a grandchild `sleep` (a stand-in for the plugin /
/// pyright subtree) and records its pid, writes one progress snapshot, then
/// blocks on the grandchild until the group is killed.
fn write_stub(dir: &Path) -> PathBuf {
    let path = dir.join("analyze-stub.sh");
    let script = r#"#!/bin/sh
PF=""
while [ $# -gt 0 ]; do
  case "$1" in
    --progress-file) PF="$2"; shift 2;;
    *) shift;;
  esac
done
sleep 600 &
CHILD=$!
echo "$CHILD" > "${PF}.child"
HB=$(date -u +%Y-%m-%dT%H:%M:%S.000Z)
printf '{"run_id":"stub","pid":%d,"phase":"analyzing","current_plugin":"slowfix","processed_files":1,"total_files":3,"current_file":"src/a.py","heartbeat_at":"%s"}' "$$" "$HB" > "$PF"
wait "$CHILD"
"#;
    std::fs::write(&path, script).expect("write stub");
    let mut perms = std::fs::metadata(&path)
        .expect("stub metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod stub");
    path
}

/// A stub that writes one progress snapshot and exits immediately (no
/// grandchild, no blocking) — stands in for an analyze run that finishes on its
/// own, so the reaping path can be exercised.
fn write_quick_stub(dir: &Path) -> PathBuf {
    let path = dir.join("analyze-quick-stub.sh");
    let script = r#"#!/bin/sh
PF=""
while [ $# -gt 0 ]; do
  case "$1" in
    --progress-file) PF="$2"; shift 2;;
    *) shift;;
  esac
done
printf '{"run_id":"stub","phase":"analyzing","heartbeat_at":"2026-01-01T00:00:00.000Z"}' > "$PF"
exit 0
"#;
    std::fs::write(&path, script).expect("write quick stub");
    let mut perms = std::fs::metadata(&path)
        .expect("stub metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod quick stub");
    path
}

async fn call_tool(state: &ServerState, name: &str, arguments: Value) -> Value {
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "tool-test",
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await
        .expect("tools/call returns a response");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool content text");
    serde_json::from_str(text).expect("tool envelope JSON")
}

/// Poll `analyze_status` until `status` matches `want` or the deadline elapses.
async fn poll_until_status(state: &ServerState, run_id: &str, want: &str) -> Value {
    for _ in 0..100 {
        let resp = call_tool(state, "analyze_status", json!({"run_id": run_id})).await;
        if resp["result"]["status"] == want {
            return resp;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("analyze_status never reached {want} for {run_id}");
}

fn pid_alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    // Signal 0 probes existence without delivering a signal.
    kill(Pid::from_raw(pid), None).is_ok()
}

#[tokio::test]
async fn analyze_start_runs_in_background_status_reports_progress_then_cancel_kills_group() {
    let (project, db_path) = open_project();
    let stub = write_stub(project.path());
    let state = state_for(project.path(), &db_path, &stub);

    // Start returns a handle immediately without blocking on the run.
    let started = call_tool(&state, "analyze_start", json!({})).await;
    assert_eq!(started["ok"], true, "{started:?}");
    let run_id = started["result"]["run_id"]
        .as_str()
        .expect("run_id")
        .to_owned();
    let progress_file = started["result"]["progress_file"]
        .as_str()
        .expect("progress_file")
        .to_owned();
    assert_eq!(started["result"]["status"], "started");

    // Status flips to running and exposes structured progress (no log scraping).
    let running = poll_until_status(&state, &run_id, "running").await;
    let result = &running["result"];
    assert_eq!(result["phase"], "analyzing");
    assert_eq!(result["current_plugin"], "slowfix");
    assert_eq!(result["processed_files"], 1);
    assert_eq!(result["total_files"], 3);
    assert_eq!(result["current_file"], "src/a.py");
    assert!(result["heartbeat_at"].as_str().is_some());
    assert_eq!(result["progress_observed"], true, "{running:?}");

    // The stub recorded its grandchild (stand-in for plugin/pyright) pid.
    let child_pid: i32 = std::fs::read_to_string(format!("{progress_file}.child"))
        .expect("child pid file")
        .trim()
        .parse()
        .expect("child pid");
    assert!(pid_alive(child_pid), "grandchild should be alive mid-run");

    // Cancel marks the run cancelled and group-kills the subtree.
    let cancelled = call_tool(&state, "analyze_cancel", json!({"run_id": &run_id})).await;
    assert_eq!(cancelled["ok"], true, "{cancelled:?}");
    assert_eq!(cancelled["result"]["status"], "cancelled");

    // The grandchild is terminated by the process-group kill.
    let mut gone = false;
    for _ in 0..100 {
        if !pid_alive(child_pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        gone,
        "cancel must terminate the grandchild process (pid {child_pid})"
    );

    // Status after cancel is terminal-cancelled.
    let after = call_tool(&state, "analyze_status", json!({"run_id": &run_id})).await;
    assert_eq!(after["result"]["status"], "cancelled", "{after:?}");
}

#[tokio::test]
async fn analyze_start_rejects_a_second_concurrent_run() {
    let (project, db_path) = open_project();
    let stub = write_stub(project.path());
    let state = state_for(project.path(), &db_path, &stub);

    let first = call_tool(&state, "analyze_start", json!({})).await;
    let run_id = first["result"]["run_id"].as_str().unwrap().to_owned();
    poll_until_status(&state, &run_id, "running").await;

    let second = call_tool(&state, "analyze_start", json!({})).await;
    assert_eq!(second["ok"], false, "{second:?}");
    assert_eq!(second["error"]["code"], "analyze-already-running");

    // Clean up the background run.
    call_tool(&state, "analyze_cancel", json!({"run_id": &run_id})).await;
}

/// Seed a `runs` row directly, bypassing the registry, so `analyze_status`
/// takes the `Absent → read_run_row → terminal_status_envelope` path.
fn seed_run(db_path: &Path, id: &str, run_status: &str, stats_json: &str) {
    let conn = Connection::open(db_path).expect("open db");
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES (?1, '2026-01-01T00:00:00.000Z', '2026-01-01T00:01:00.000Z', '{}', ?2, ?3)",
        rusqlite::params![id, stats_json, run_status],
    )
    .expect("insert runs row");
}

#[tokio::test]
async fn analyze_status_maps_terminal_run_states_from_the_runs_table() {
    let (project, db_path) = open_project();
    let stub = write_stub(project.path());
    let state = state_for(project.path(), &db_path, &stub);

    seed_run(&db_path, "r-done", "completed", "{}");
    seed_run(&db_path, "r-fail", "failed", "{}");
    seed_run(&db_path, "r-skip", "skipped_no_plugins", "{}");
    // A cancelled run is recorded as `failed` + stats.terminal_reason; the
    // status tool must surface it as cancelled, not failed (decomposition B).
    seed_run(
        &db_path,
        "r-cancel",
        "failed",
        r#"{"terminal_reason":"cancelled"}"#,
    );

    for (id, expected) in [
        ("r-done", "completed"),
        ("r-fail", "failed"),
        ("r-skip", "skipped_no_plugins"),
        ("r-cancel", "cancelled"),
    ] {
        let resp = call_tool(&state, "analyze_status", json!({"run_id": id})).await;
        assert_eq!(resp["ok"], true, "{resp:?}");
        assert_eq!(resp["result"]["status"], expected, "run {id}: {resp:?}");
    }
}

#[tokio::test]
async fn analyze_status_does_not_mutate_stale_running_run() {
    let (project, db_path) = open_project();
    let stub = write_stub(project.path());
    let state = state_for(project.path(), &db_path, &stub);

    seed_stale_running_run(&db_path, "r-stale");

    let resp = call_tool(&state, "analyze_status", json!({"run_id": "r-stale"})).await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert_eq!(resp["result"]["status"], "failed");

    let conn = Connection::open(&db_path).expect("open db");
    let (run_status, run_owner_pid, stats_json): (String, Option<i64>, String) = conn
        .query_row(
            "SELECT status, owner_pid, stats FROM runs WHERE id = 'r-stale'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read run");
    assert_eq!(run_status, "running");
    assert_eq!(run_owner_pid, Some(999_999));
    let repair_stats: Value = serde_json::from_str(&stats_json).expect("stats json");
    assert_eq!(repair_stats, json!({}));
}

#[tokio::test]
async fn analyze_start_reaps_finished_runs_and_their_progress_files() {
    let (project, db_path) = open_project();
    let stub = write_quick_stub(project.path());
    let state = state_for(project.path(), &db_path, &stub);

    // First run finishes on its own (quick stub exits 0).
    let first = call_tool(&state, "analyze_start", json!({})).await;
    let first_id = first["result"]["run_id"].as_str().unwrap().to_owned();
    let first_progress = PathBuf::from(first["result"]["progress_file"].as_str().unwrap());
    assert_eq!(
        state.tracked_analyze_runs(),
        1,
        "handle tracked after start"
    );

    // Wait for it to reach a terminal status (it writes no runs row, so the
    // terminal mapping is `failed` — what matters here is that it exited).
    poll_until_status(&state, &first_id, "failed").await;
    assert_eq!(
        state.tracked_analyze_runs(),
        1,
        "status read does not evict — eviction is swept on the next start"
    );

    // A second start sweeps the finished first run out of the registry and
    // reaps its progress file before spawning.
    let second = call_tool(&state, "analyze_start", json!({})).await;
    assert_eq!(second["ok"], true, "{second:?}");
    let second_id = second["result"]["run_id"].as_str().unwrap().to_owned();
    assert_ne!(first_id, second_id);

    assert_eq!(
        state.tracked_analyze_runs(),
        1,
        "only the second run remains; the finished first was evicted"
    );
    assert!(
        !first_progress.exists(),
        "the finished run's progress file must be reaped on the next start"
    );

    poll_until_status(&state, &second_id, "failed").await;
}

#[tokio::test]
async fn analyze_status_for_unknown_run_is_not_found() {
    let (project, db_path) = open_project();
    let stub = write_stub(project.path());
    let state = state_for(project.path(), &db_path, &stub);

    let resp = call_tool(&state, "analyze_status", json!({"run_id": "no-such-run"})).await;
    assert_eq!(resp["ok"], false, "{resp:?}");
    assert_eq!(resp["error"]["code"], "run-not-found");
}
