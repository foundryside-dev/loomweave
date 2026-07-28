//! Serve bootstrap for a linked worktree's isolated index (Task 5,
//! worktree-indexes design, "Bootstrap" section).
//!
//! Drives [`ServerState`] directly over in-process `handle_json_rpc` (no TCP,
//! no real `loomweave serve` subprocess) against a *real* Git repository +
//! `git worktree add` fixture, mirroring the pattern in
//! `crates/loomweave-cli/src/worktree/store.rs`'s and
//! `crates/loomweave-core/src/worktree/context.rs`'s own tests. Task 3's
//! store-creation step (`ensure_isolated_store`, `loomweave-cli`-private) is
//! not called here — these tests replicate its one load-bearing effect (an
//! eagerly schema-initialized, empty `loomweave.db`) directly via
//! `loomweave-storage`, since that is the only thing this gate depends on.
//!
//! `second_serve_does_not_spawn_a_duplicate_analyze` deliberately does not
//! reach for the real cross-process `analyze_lock.rs` (it lives in
//! `loomweave-cli`, a downstream crate this one cannot depend on, and is
//! already covered by its own unit tests) — it proves the property that
//! matters at this layer: two concurrent bootstrap spawns never corrupt or
//! duplicate the outcome, using a lock-file stand-in built from the same
//! atomic-mkdir primitive.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use loomweave_core::worktree::WorktreeContext;
use loomweave_mcp::{McpToolPolicy, ServerState};
use loomweave_storage::{ReaderPool, pragma, schema};
use rusqlite::Connection;
use serde_json::{Value, json};

// ---------------------------------------------------------------------
// Git / WorktreeContext fixtures — mirrors
// `loomweave-cli/src/worktree/store.rs`'s and
// `loomweave-core/src/worktree/context.rs`'s own test helpers.
// ---------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?} in {}: {e}", dir.display()));
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(dir: &Path, branch: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", branch]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("f.txt"), "hi\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "init"]);
}

/// A real primary repo with one linked worktree, resolved.
fn linked_context(root: &Path) -> WorktreeContext {
    let repo = root.join("repo");
    init_repo(&repo, "main");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );
    let linked = root.join("linked");
    let ctx = WorktreeContext::resolve(&linked).expect("resolves as Linked");
    assert_eq!(
        ctx.kind,
        loomweave_core::worktree::WorktreeKind::Linked,
        "fixture must actually resolve as Linked"
    );
    ctx
}

/// Replicates the one load-bearing effect of Task 3's `ensure_isolated_store`
/// this gate depends on: the effective store directory exists and
/// `loomweave.db` is schema-initialized (migrations applied, zero rows) —
/// i.e. exactly the state a fresh linked worktree is in the instant `serve`
/// bootstraps it, before any analyze run has written a `runs` row.
fn init_effective_store(ctx: &WorktreeContext) {
    std::fs::create_dir_all(&ctx.effective_store).expect("create effective store dir");
    let mut conn = Connection::open(&ctx.store_paths.db).expect("open sqlite");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
}

/// A `ServerState` wired exactly as `serve.rs::run_mcp_stdio` wires one for a
/// linked worktree: `readers` opened against the *effective* store, gate
/// active via `with_worktree_gate`.
fn linked_state(ctx: &WorktreeContext) -> ServerState {
    let pool = ReaderPool::open(&ctx.store_paths.db, 4).expect("reader pool");
    ServerState::new(ctx.source_root.clone(), pool)
        .with_tool_policy(McpToolPolicy::allow_write_tools())
        .with_worktree_gate(ctx.store_paths.db.clone(), &ctx.source_root)
}

fn seed_run(db_path: &Path, id: &str, started_at: &str, status: &str) {
    let conn = Connection::open(db_path).expect("open db");
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES (?1, ?2, '2026-01-01T00:01:00.000Z', '{}', '{}', ?3)",
        rusqlite::params![id, started_at, status],
    )
    .expect("insert runs row");
}

fn seed_completed_run(db_path: &Path, id: &str) {
    seed_run(db_path, id, "2026-01-01T00:00:00.000Z", "completed");
}

fn fallback_command_json(ctx: &WorktreeContext) -> Value {
    json!([
        "loomweave",
        "worktree",
        "analyze",
        "--",
        ctx.source_root.display().to_string(),
    ])
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

/// Poll `condition` until it is true or `deadline` elapses, panicking on
/// timeout. Used only where a real spawned child's completion must be
/// observed (`second_serve_does_not_spawn_a_duplicate_analyze`) — every other
/// test in this file is a direct SQL seed + immediate call, no waiting
/// needed.
fn wait_until(deadline: Duration, mut condition: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    loop {
        if condition() {
            return;
        }
        assert!(
            start.elapsed() <= deadline,
            "condition not met within {deadline:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[tokio::test]
async fn serve_on_unbuilt_worktree_starts_and_reports_building() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = linked_context(tmp.path());
    init_effective_store(&ctx);
    let state = linked_state(&ctx);

    let graph = call_tool(&state, "entity_find", json!({"pattern": "anything"})).await;
    assert_eq!(graph["ok"], false, "{graph:?}");
    assert_eq!(graph["error"]["code"], "index-building", "{graph:?}");
    assert_eq!(graph["error"]["retryable"], true, "{graph:?}");

    // The session started and answered in-process — no exit-1, no dropped
    // connection — and status stays reachable in the very same session.
    let status = call_tool(&state, "project_status_get", json!({})).await;
    assert_eq!(status["ok"], true, "{status:?}");
}

#[tokio::test]
async fn serve_gate_routes_linked_worktree_to_building_not_no_index() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = linked_context(tmp.path());
    init_effective_store(&ctx);
    let state = linked_state(&ctx);

    // `serve_stdio_no_index`'s degraded loop answers every `tools/call` with
    // `isError: true` and a plain-text "run analyze ... then reconnect" chirp
    // — not a structured `{"ok":false,"error":{"code":...}}` envelope. Assert
    // the response IS the structured envelope, proving pinned decision 1's
    // routing: a linked worktree with Task 3's eagerly-created (but
    // unanalyzed) store must reach the full `ServerState` path with a
    // `building` readiness gate, never the no-index degradation.
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "entity_find", "arguments": {"pattern": "x"}}
        }))
        .await
        .expect("tools/call returns a response");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    assert!(
        !text.contains("then reconnect this MCP server"),
        "that is `no_index_message`'s exact chirp text; the bootstrap gate must never produce \
         it: {text}"
    );
    let envelope: Value = serde_json::from_str(text)
        .expect("a gated response is a structured JSON envelope, not the no-index chirp");
    assert_eq!(envelope["ok"], false, "{envelope:?}");
    assert_eq!(envelope["error"]["code"], "index-building", "{envelope:?}");
}

#[tokio::test]
async fn graph_tool_returns_retryable_index_building_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = linked_context(tmp.path());
    init_effective_store(&ctx);
    let state = linked_state(&ctx);

    let resp = call_tool(&state, "entity_at", json!({"file": "src/a.py", "line": 1})).await;
    assert_eq!(resp["ok"], false, "{resp:?}");
    assert_eq!(resp["error"]["code"], "index-building", "{resp:?}");
    assert_eq!(resp["error"]["retryable"], true, "{resp:?}");
    let diagnostics = resp["diagnostics"].as_array().expect("diagnostics array");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(
        diagnostics[0]["run_id"],
        Value::Null,
        "no run row exists yet: {diagnostics:?}"
    );
    assert_eq!(
        diagnostics[0]["fallback_command"],
        fallback_command_json(&ctx),
        "{diagnostics:?}"
    );
}

#[tokio::test]
async fn session_becomes_usable_after_run_completes_without_reconnect() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = linked_context(tmp.path());
    init_effective_store(&ctx);
    let state = linked_state(&ctx);

    let building = call_tool(&state, "entity_find", json!({"pattern": "x"})).await;
    assert_eq!(building["error"]["code"], "index-building", "{building:?}");

    // Complete the run directly — no spawn, no sleep, no polling loop: the
    // readiness consult reads the `runs` table fresh on every call (pinned
    // decision 2), so writing the terminal row is enough to make the very
    // next tool call, against the SAME `ServerState`, see it. No reconnect.
    seed_completed_run(&ctx.store_paths.db, "r1");

    let ready = call_tool(&state, "entity_find", json!({"pattern": "x"})).await;
    assert_eq!(ready["ok"], true, "{ready:?}");
    assert!(ready["error"].is_null(), "{ready:?}");
}

/// Two `loomweave serve` starts racing the same linked worktree each fire
/// their own bootstrap spawn unconditionally (pinned decision 3: no
/// pre-spawn lock check in this crate — that would be the forbidden "run
/// authority probe"). This proves the OUTCOME property double-spawn
/// tolerance depends on: two concurrent spawns racing an exclusive lock
/// never produce two winners. The real guard (`analyze_lock.rs`'s fs2
/// exclusive lock, acquired by the spawned `loomweave worktree analyze`
/// before it writes any `runs` row) is `loomweave-cli`-private and already
/// covered by its own tests; this test's stub stands in for it with the same
/// atomic-mkdir primitive, driven through the real, public
/// `spawn_detached_worktree_analyze` entry point `serve.rs` calls.
#[test]
#[cfg(unix)]
fn second_serve_does_not_spawn_a_duplicate_analyze() {
    let dir = tempfile::tempdir().unwrap();
    let coord = dir.path().to_path_buf();
    let script = write_lock_race_stub(&coord);
    let target = dir.path().join("target-worktree");
    std::fs::create_dir_all(&target).unwrap();

    loomweave_mcp::worktree_bootstrap::spawn_detached_worktree_analyze(&script, &target);
    loomweave_mcp::worktree_bootstrap::spawn_detached_worktree_analyze(&script, &target);

    wait_until(Duration::from_secs(5), || {
        count_matching(&coord, "done-") == 2
    });

    assert_eq!(
        count_matching(&coord, "won-"),
        1,
        "exactly one of the two racing bootstrap spawns may win the lock; a second serve must \
         not cause a duplicate analyze to actually run"
    );
}

#[cfg(unix)]
fn write_lock_race_stub(coord: &Path) -> PathBuf {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let script = coord.join("race-stub.sh");
    let mut file = std::fs::File::create(&script).unwrap();
    // Atomic exclusive `mkdir` stands in for the real fs2 lock; whichever of
    // the two concurrent invocations wins writes a `won-<pid>` marker, and
    // both always write a `done-<pid>` marker so the test can observe
    // completion without depending on the win/lose split.
    writeln!(
        file,
        "#!/bin/sh\nmkdir \"{coord}/lock\" 2>/dev/null && : > \"{coord}/won-$$\"\n: > \"{coord}/done-$$\"\n",
        coord = coord.display()
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn count_matching(dir: &Path, prefix: &str) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(prefix))
        })
        .count()
}

#[tokio::test]
async fn failed_build_returns_index_build_failed_with_fallback_command() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = linked_context(tmp.path());
    init_effective_store(&ctx);
    seed_run(
        &ctx.store_paths.db,
        "r-fail",
        "2026-01-01T00:00:00.000Z",
        "failed",
    );
    let state = linked_state(&ctx);

    let resp = call_tool(&state, "entity_find", json!({"pattern": "x"})).await;
    assert_eq!(resp["ok"], false, "{resp:?}");
    assert_eq!(resp["error"]["code"], "index-build-failed", "{resp:?}");
    assert_eq!(resp["error"]["retryable"], false, "{resp:?}");
    let diagnostics = resp["diagnostics"].as_array().expect("diagnostics array");
    assert_eq!(diagnostics[0]["run_id"], "r-fail", "{diagnostics:?}");
    assert_eq!(
        diagnostics[0]["fallback_command"],
        fallback_command_json(&ctx),
        "{diagnostics:?}"
    );

    // project_status_get remains available and reports the failure, per
    // pinned decision 4's "remain available while building" carve-out.
    let status = call_tool(&state, "project_status_get", json!({})).await;
    assert_eq!(status["ok"], true, "{status:?}");
    assert_eq!(status["result"]["latest_run"]["status"], "failed");
}

#[tokio::test]
async fn status_counts_are_null_not_zero_while_building() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = linked_context(tmp.path());
    init_effective_store(&ctx);
    let state = linked_state(&ctx);

    let status = call_tool(&state, "project_status_get", json!({})).await;
    assert_eq!(status["ok"], true, "{status:?}");
    let result = &status["result"];

    // Corpus-derived fields: null, never a fabricated zero.
    for key in [
        "entities",
        "subsystems",
        "edges",
        "findings",
        "briefing_blocked",
    ] {
        assert!(
            result["counts"][key].is_null(),
            "counts.{key} must be null while building, got {:?}",
            result["counts"][key]
        );
    }
    assert!(result["staleness"].is_null(), "{result:?}");
    assert!(result["staleness_note"].is_null(), "{result:?}");
    assert!(result["plugins"].is_null(), "{result:?}");
    assert!(result["sei"]["populated"].is_null(), "{result:?}");
    assert!(result["last_analyzed_at"].is_null(), "{result:?}");
    assert!(result["git_sha"].is_null(), "{result:?}");

    // Store-*file* facts are real and honest, never nulled: Task 3's eager
    // schema-init means the effective store genuinely is present.
    assert_eq!(result["db_present"], true, "{result:?}");
    assert_eq!(
        result["db_path"],
        ctx.store_paths.db.display().to_string(),
        "{result:?}"
    );
    assert!(
        !result["db_identity"]["db_size_bytes"].is_null(),
        "{result:?}"
    );
}

#[tokio::test]
async fn removed_source_root_surfaces_source_root_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = linked_context(tmp.path());
    init_effective_store(&ctx);
    // A completed index — so the failure below is unambiguously about the
    // removed source root, not a leftover `building` state.
    seed_completed_run(&ctx.store_paths.db, "r1");
    let state = linked_state(&ctx);

    // Simulate `git worktree remove` (or a manual `rm -rf`) racing a live
    // `serve` — the design's accepted race.
    std::fs::remove_dir_all(&ctx.source_root).unwrap();

    let graph = call_tool(&state, "entity_find", json!({"pattern": "x"})).await;
    assert_eq!(graph["ok"], false, "{graph:?}");
    assert_eq!(graph["error"]["code"], "source-root-missing", "{graph:?}");

    // Unlike `building`, `source-root-missing` is not carved out for status
    // tools: once the tree itself is gone there is no progress left to
    // report, and answering from the last-known state would be exactly the
    // stale answer this code exists to prevent.
    let status = call_tool(&state, "project_status_get", json!({})).await;
    assert_eq!(status["ok"], false, "{status:?}");
    assert_eq!(status["error"]["code"], "source-root-missing", "{status:?}");
}

#[tokio::test]
async fn main_and_standalone_keep_existing_no_index_behavior() {
    // A `ServerState` built the way `serve.rs` builds one for the main
    // worktree or a standalone checkout — no `.with_worktree_gate` call —
    // must be completely unaffected by the bootstrap gate, even with zero
    // `runs` rows (which would read as "still building" if the gate were
    // mistakenly active for every `ServerState`). This is the regression
    // guard for pinned decision 1's "main/standalone keep today's behavior
    // exactly" at the layer this crate's own tests can reach directly; the
    // CLI-level routing (`serve.rs`'s unchanged `db_path(path).exists()`
    // gate before any `ServerState` is even constructed) is exercised by the
    // `tests/e2e/*.sh` smokes instead, since it is not reachable from an
    // `loomweave-mcp` integration test.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("loomweave.db");
    let mut conn = Connection::open(&db_path).unwrap();
    pragma::apply_write_pragmas(&conn).unwrap();
    schema::apply_migrations(&mut conn).unwrap();
    drop(conn);

    let pool = ReaderPool::open(&db_path, 2).unwrap();
    let state = ServerState::new(tmp.path().to_path_buf(), pool)
        .with_tool_policy(McpToolPolicy::allow_write_tools());

    let graph = call_tool(&state, "entity_find", json!({"pattern": "x"})).await;
    assert_eq!(graph["ok"], true, "{graph:?}");
    assert!(
        graph["error"].is_null(),
        "no worktree gate configured => no index-building gate, ever: {graph:?}"
    );

    let status = call_tool(&state, "project_status_get", json!({})).await;
    assert_eq!(status["ok"], true, "{status:?}");
    // Real, non-null zero counts (not nulled — there is no gate active).
    assert_eq!(status["result"]["counts"]["entities"], 0, "{status:?}");
}

/// `loomweave://context` is a second door into the same store the tool-call
/// gate protects (CLAUDE.md points agents at it directly: "live counts:
/// `loomweave://context`"). While building it must report the same
/// `degraded: true` shape `unreadable_db_snapshot()` already uses for a
/// broken read — never `entity_count: 0` presented as a real, trustworthy
/// count — and once a run completes it must report real counts again, in the
/// same session, no reconnect.
#[tokio::test]
async fn context_resource_reports_degraded_while_building_then_real_counts_after_completion() {
    async fn read_context(state: &ServerState) -> Value {
        let response = state
            .handle_json_rpc(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "resources/read",
                "params": {"uri": "loomweave://context"}
            }))
            .await
            .expect("resources/read returns a response");
        let text = response["result"]["contents"][0]["text"]
            .as_str()
            .expect("resource content text");
        serde_json::from_str(text).expect("snapshot JSON")
    }

    let tmp = tempfile::tempdir().unwrap();
    let ctx = linked_context(tmp.path());
    init_effective_store(&ctx);
    let state = linked_state(&ctx);

    let building = read_context(&state).await;
    assert_eq!(building["degraded"], true, "{building:?}");
    assert_eq!(building["reason"], "index-building", "{building:?}");
    // The zero counts are the same shape `unreadable_db_snapshot()` already
    // produces for a broken read — present, but disclaimed by `degraded`.
    assert_eq!(building["entity_count"], 0, "{building:?}");
    assert_eq!(building["db_present"], true, "{building:?}");

    seed_completed_run(&ctx.store_paths.db, "r1");

    let ready = read_context(&state).await;
    assert_eq!(ready["degraded"], false, "{ready:?}");
    assert!(
        ready.get("reason").is_none(),
        "an ordinary (non-gated) snapshot must not carry a `reason` key: {ready:?}"
    );
}
