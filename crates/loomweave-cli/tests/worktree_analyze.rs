//! CLI-level tests for Task 3 of the worktree-index-isolation feature: bare
//! `loomweave analyze <linked-worktree-path>` and the explicit `loomweave
//! worktree analyze` subcommand both routing storage through the isolated
//! per-worktree store (see
//! `docs/superpowers/specs/2026-07-18-loomweave-worktree-indexes-design.md`).
//!
//! `plain_analyze_on_linked_worktree_writes_isolated_store` is the
//! review-critical case: the `SessionStart` hook and `serve`'s bootstrap both
//! spawn plain `loomweave analyze <source-root>`, so if that spelling ever
//! stops routing through `WorktreeContext`, a linked worktree silently gets
//! a 20-30 minute index written to a location `serve` never polls.
//!
//! Every fixture plugin here is a fake `python3` script that speaks the
//! plugin JSON-RPC wire protocol directly over stdio — never a real
//! Python/Pyright pass — matching the established pattern in
//! `tests/analyze.rs`.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use fs2::FileExt as _;
use loomweave_core::worktree::WorktreeContext;
use rusqlite::Connection;

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

/// `PATH` for a spawned `loomweave` process: the fixture plugin directory,
/// prepended to the REAL inherited `PATH` — unlike `tests/analyze.rs`'s
/// plugin-dir-only convention, these tests exercise real `git worktree`
/// repositories, so the spawned process's own internal `git` calls
/// (`WorktreeContext::resolve`, `sei_git`) need a real `git` on `PATH` too.
fn path_with_plugin(plugin_dir: &Path) -> std::ffi::OsString {
    let mut entries = vec![plugin_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(entries).expect("join PATH entries")
}

fn git(dir: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
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

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
}

/// A fake plugin (`.wta` extension) that mints one module entity per file,
/// named after the file's own (trimmed) content — so two worktrees with
/// different working-tree content produce distinguishable graph rows,
/// proving each worktree's isolated store holds ITS OWN analysis, not a
/// shared or cross-contaminated one.
#[cfg(unix)]
const WTA_PLUGIN_SCRIPT: &str = r#"#!/usr/bin/python3
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
                "name": "loomweave-plugin-wta",
                "version": "0.1.0",
                "ontology_version": "0.1.0",
                "capabilities": {},
            },
        })
    elif method == "analyze_file":
        path = msg["params"]["file_path"]
        with open(path, encoding="utf-8") as f:
            content = f.read().strip()
        write_frame({
            "jsonrpc": "2.0",
            "id": ident,
            "result": {
                "entities": [
                    {
                        "id": f"wtafixture:module:{content}",
                        "kind": "module",
                        "qualified_name": content,
                        "source": {"file_path": path},
                    },
                ],
                "edges": [],
                "stats": {},
            },
        })
    elif method == "shutdown":
        write_frame({"jsonrpc": "2.0", "id": ident, "result": {}})
"#;

#[cfg(unix)]
const WTA_PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "loomweave-plugin-wta"
plugin_id = "wtafixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "loomweave-plugin-wta"
language = "wtafixture"
extensions = ["wta"]

[capabilities.runtime]
expected_max_rss_mb = 128
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "LMWV-WTA-"
ontology_version = "0.1.0"

[ontology.roles]
file_scope = ["module"]
"#;

#[cfg(unix)]
fn write_wta_plugin(plugin_dir: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let plugin_script = plugin_dir.join("loomweave-plugin-wta");
    std::fs::write(&plugin_script, WTA_PLUGIN_SCRIPT).expect("write wta plugin script");
    let mut perms = std::fs::metadata(&plugin_script)
        .expect("stat wta plugin")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin_script, perms).expect("chmod wta plugin");

    std::fs::write(plugin_dir.join("plugin.toml"), WTA_PLUGIN_MANIFEST)
        .expect("write wta plugin manifest");
}

/// Build a primary repo (`install`ed, one committed `.wta` source file) plus
/// one linked worktree named `worktree_name`, whose own `.wta` file content
/// is `module_name` (an uncommitted working-tree edit, distinguishing it
/// from the primary's committed content).
#[cfg(unix)]
fn setup_primary_with_linked_worktree(
    root: &Path,
    plugin_dir: &Path,
    worktree_name: &str,
    branch: &str,
    module_name: &str,
) -> (PathBuf, PathBuf) {
    write_wta_plugin(plugin_dir);

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    loomweave_bin()
        .args(["install", "--path"])
        .arg(&repo)
        .assert()
        .success();
    std::fs::write(repo.join("app.wta"), "primary_module\n").unwrap();

    init_repo(&repo);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            &format!("../{worktree_name}"),
        ],
    );

    let linked = root.join(worktree_name);
    std::fs::write(linked.join("app.wta"), format!("{module_name}\n")).unwrap();

    (repo, linked)
}

/// Find the sole `wt-[0-9a-f]{64}` child of `<repo>/.weft/loomweave/worktrees/`.
#[cfg(unix)]
fn find_single_worktree_store(repo: &Path) -> PathBuf {
    let worktrees_dir = repo.join(".weft/loomweave/worktrees");
    let candidates: Vec<PathBuf> = std::fs::read_dir(&worktrees_dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", worktrees_dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "expected exactly one worktree store under {}, found {candidates:?}",
        worktrees_dir.display()
    );
    candidates.into_iter().next().unwrap()
}

#[cfg(unix)]
fn module_entity_exists(db_path: &Path, module_name: &str) -> bool {
    let conn = Connection::open(db_path).unwrap();
    let id = format!("wtafixture:module:{module_name}");
    conn.query_row("SELECT 1 FROM entities WHERE id = ?1", [&id], |_row| Ok(()))
        .is_ok()
}

#[cfg(unix)]
fn latest_run_stats(db_path: &Path) -> serde_json::Value {
    let conn = Connection::open(db_path).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT stats FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query latest runs.stats");
    serde_json::from_str(&raw).expect("runs.stats JSON")
}

/// Pick out the `wtafixture` plugin's own `classifier_coverage` entry by
/// `plugin_id` — other plugins on `PATH` (e.g. a dev machine's real
/// `python`/`rust` plugins, discovered because `path_with_plugin` prepends
/// rather than replaces `PATH`) also appear in `classifier_coverage.plugins`,
/// so indexing by position is not stable.
#[cfg(unix)]
fn wta_plugin_coverage(stats: &serde_json::Value) -> serde_json::Value {
    stats["classifier_coverage"]["plugins"]
        .as_array()
        .expect("plugins array")
        .iter()
        .find(|p| p["plugin_id"] == "wtafixture")
        .cloned()
        .expect("wtafixture plugin coverage present")
}

#[cfg(unix)]
#[test]
fn worktree_analyze_builds_an_index_by_path() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        plugin_dir.path(),
        "by-path",
        "feature-path",
        "path_module",
    );

    loomweave_bin()
        .args(["worktree", "analyze", "--"])
        .arg(&linked)
        .env("PATH", path_with_plugin(plugin_dir.path()))
        .assert()
        .success();

    let store = find_single_worktree_store(&repo);
    let db_path = store.join("loomweave.db");
    assert!(db_path.is_file(), "isolated store must have a loomweave.db");
    assert!(
        module_entity_exists(&db_path, "path_module"),
        "the linked worktree's own content must be reflected in the isolated store"
    );
    assert!(
        !linked.join(".weft/loomweave/loomweave.db").exists(),
        "the worktree's own local .weft/loomweave/ must never get a loomweave.db"
    );
}

#[cfg(unix)]
#[test]
fn worktree_analyze_acquires_the_stable_lock_before_initializing_its_store() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let (_repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        plugin_dir.path(),
        "lock-before-init",
        "feature-lock-before-init",
        "locked_module",
    );
    let ctx = WorktreeContext::resolve(&linked).expect("resolve linked worktree");
    let stable_id = ctx.stable_id.as_deref().expect("linked stable id");
    let worktrees_dir = ctx.repository_store.join("worktrees");
    std::fs::create_dir_all(&worktrees_dir).expect("create worktrees namespace");
    let lock_path = worktrees_dir.join(format!("{stable_id}.lock"));
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open stable analyze lock");
    lock_file
        .lock_exclusive()
        .expect("hold stable analyze lock");

    loomweave_bin()
        .args(["worktree", "analyze", "--"])
        .arg(&linked)
        .env("PATH", path_with_plugin(plugin_dir.path()))
        .assert()
        .failure();

    assert!(
        !ctx.effective_store.exists(),
        "a process that cannot acquire the analyze lock must not initialize or rebuild the store"
    );
}

#[cfg(unix)]
#[test]
fn worktree_analyze_builds_an_index_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let (repo, _linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        plugin_dir.path(),
        "by-name",
        "feature-name",
        "name_module",
    );

    loomweave_bin()
        .args(["worktree", "analyze", "--", "by-name"])
        .current_dir(&repo)
        .env("PATH", path_with_plugin(plugin_dir.path()))
        .assert()
        .success();

    let store = find_single_worktree_store(&repo);
    let db_path = store.join("loomweave.db");
    assert!(
        module_entity_exists(&db_path, "name_module"),
        "resolving by registered worktree name must analyze the right worktree"
    );
}

#[cfg(unix)]
#[test]
fn worktree_analyze_no_incremental_rebuilds() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        plugin_dir.path(),
        "no-incremental",
        "feature-noinc",
        "noinc_module",
    );
    let path_value = path_with_plugin(plugin_dir.path());

    loomweave_bin()
        .args(["worktree", "analyze", "--"])
        .arg(&linked)
        .env("PATH", &path_value)
        .assert()
        .success();
    let store = find_single_worktree_store(&repo);
    let db_path = store.join("loomweave.db");
    let first = latest_run_stats(&db_path);
    assert_eq!(
        wta_plugin_coverage(&first)["analyzed_files"],
        1,
        "first run must actually dispatch the file to the plugin"
    );

    // Unchanged content, plain incremental re-run: the file must be
    // retained, not re-dispatched.
    loomweave_bin()
        .args(["worktree", "analyze", "--"])
        .arg(&linked)
        .env("PATH", &path_value)
        .assert()
        .success();
    let incremental = latest_run_stats(&db_path);
    let incremental_coverage = wta_plugin_coverage(&incremental);
    assert_eq!(
        incremental_coverage["analyzed_files"], 0,
        "an unchanged incremental re-run must retain, not re-dispatch, the file"
    );
    assert_eq!(incremental_coverage["retained_files"], 1);

    // `--no-incremental`: forces the unchanged file to be re-dispatched.
    loomweave_bin()
        .args(["worktree", "analyze", "--no-incremental", "--"])
        .arg(&linked)
        .env("PATH", &path_value)
        .assert()
        .success();
    let forced = latest_run_stats(&db_path);
    assert_eq!(
        wta_plugin_coverage(&forced)["analyzed_files"],
        1,
        "--no-incremental must force re-dispatch of an unchanged file"
    );
}

/// The review-critical case: bare `loomweave analyze <linked-worktree-path>`
/// — the exact argv the `SessionStart` hook spawns — must land in the
/// isolated store, and must NOT create a `loomweave.db`/`loomweave.lock`
/// under the worktree's own local `.weft/loomweave/`.
#[cfg(unix)]
#[test]
fn plain_analyze_on_linked_worktree_writes_isolated_store() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        plugin_dir.path(),
        "plain-analyze",
        "feature-plain",
        "plain_module",
    );

    loomweave_bin()
        .args(["analyze"])
        .arg(&linked)
        .env("PATH", path_with_plugin(plugin_dir.path()))
        .assert()
        .success();

    let store = find_single_worktree_store(&repo);
    let db_path = store.join("loomweave.db");
    assert!(
        module_entity_exists(&db_path, "plain_module"),
        "bare `loomweave analyze <linked-worktree-path>` must land in the isolated store"
    );

    assert!(
        !linked.join(".weft/loomweave/loomweave.db").exists(),
        "plain analyze on a linked worktree must NEVER create a loomweave.db under \
         the worktree's own local .weft/loomweave/ — that store is never polled by serve"
    );
    assert!(
        !linked.join(".weft/loomweave/loomweave.lock").exists(),
        "the analyze advisory lock must be taken on the EFFECTIVE (isolated) store, \
         never on the linked worktree's own local .weft/loomweave/"
    );

    // The primary's own store is untouched by analyzing a linked worktree.
    // `loomweave install` already eagerly schema-initializes the primary's
    // own loomweave.db (empty, no runs) — so the meaningful assertion isn't
    // "the file doesn't exist", it's "no run was ever recorded there".
    let primary_db_path = repo.join(".weft/loomweave/loomweave.db");
    assert!(
        primary_db_path.is_file(),
        "sanity: `install` eagerly initializes the primary's own loomweave.db"
    );
    let primary_conn = Connection::open(&primary_db_path).unwrap();
    let primary_run_count: i64 = primary_conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        primary_run_count, 0,
        "analyzing a linked worktree must never write a run into the primary's own store"
    );
}

#[cfg(unix)]
#[test]
fn two_worktrees_analyze_concurrently_without_contention() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    write_wta_plugin(plugin_dir.path());

    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    loomweave_bin()
        .args(["install", "--path"])
        .arg(&repo)
        .assert()
        .success();
    std::fs::write(repo.join("app.wta"), "primary_module\n").unwrap();
    init_repo(&repo);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature-a", "../linked-a"],
    );
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature-b", "../linked-b"],
    );

    let linked_a = tmp.path().join("linked-a");
    let linked_b = tmp.path().join("linked-b");
    std::fs::write(linked_a.join("app.wta"), "alpha_module\n").unwrap();
    std::fs::write(linked_b.join("app.wta"), "beta_module\n").unwrap();

    let path_value = path_with_plugin(plugin_dir.path());

    // `assert_cmd::Command::spawn` is private (it wants to own
    // output-capture), so genuinely concurrent spawning goes through a
    // plain `std::process::Command` against the same binary — Cargo sets
    // `CARGO_BIN_EXE_loomweave` for every integration test in a crate that
    // has a `[[bin]] name = "loomweave"` target.
    let bin = env!("CARGO_BIN_EXE_loomweave");
    let mut child_a = StdCommand::new(bin)
        .args(["analyze"])
        .arg(&linked_a)
        .env("PATH", &path_value)
        .spawn()
        .expect("spawn analyze A");
    let mut child_b = StdCommand::new(bin)
        .args(["analyze"])
        .arg(&linked_b)
        .env("PATH", &path_value)
        .spawn()
        .expect("spawn analyze B");

    let status_a = child_a.wait().expect("wait A");
    let status_b = child_b.wait().expect("wait B");
    assert!(
        status_a.success(),
        "concurrent analyze of worktree A must succeed"
    );
    assert!(
        status_b.success(),
        "concurrent analyze of worktree B must succeed"
    );

    let worktrees_dir = repo.join(".weft/loomweave/worktrees");
    let stores: Vec<PathBuf> = std::fs::read_dir(&worktrees_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(
        stores.len(),
        2,
        "each linked worktree must get its OWN isolated store: {stores:?}"
    );

    let mut found_alpha = false;
    let mut found_beta = false;
    for store in &stores {
        let db_path = store.join("loomweave.db");
        if module_entity_exists(&db_path, "alpha_module") {
            found_alpha = true;
        }
        if module_entity_exists(&db_path, "beta_module") {
            found_beta = true;
        }
    }
    assert!(
        found_alpha,
        "worktree A's own content must appear in exactly one isolated store"
    );
    assert!(
        found_beta,
        "worktree B's own content must appear in exactly one isolated store"
    );
}

#[cfg(unix)]
fn latest_run_config(db_path: &Path) -> serde_json::Value {
    let conn = Connection::open(db_path).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT config FROM runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query latest runs.config");
    serde_json::from_str(&raw).expect("runs.config JSON")
}

/// clarion-c39b92b868: with no explicit `--config`, a linked-worktree
/// analyze resolves configuration through the worktree ladder — the
/// worktree's own `loomweave.yaml` first, falling back to the PRIMARY
/// checkout's — matching what `serve` reports for the same checkout. Before
/// the fix, the run silently used built-in defaults whenever the worktree
/// itself had no config file.
#[cfg(unix)]
#[test]
fn worktree_analyze_uses_primary_config_when_worktree_has_none() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        plugin_dir.path(),
        "ladder-config",
        "feature-ladder",
        "ladder_module",
    );
    // Primary-only config: the design's stated common case — an ignored
    // local configuration at the primary, worked from a worktree with none.
    // `install` writes a default loomweave.yaml that the fixture commits, so
    // the linked checkout starts with a copy; drop it to model the
    // ignored/local-only configuration.
    std::fs::write(
        repo.join("loomweave.yaml"),
        "analysis:\n  clustering:\n    seed: 4242\n",
    )
    .unwrap();
    std::fs::remove_file(linked.join("loomweave.yaml")).expect("drop the worktree's copy");
    assert!(
        !linked.join("loomweave.yaml").exists(),
        "fixture: the worktree itself must have no config"
    );

    loomweave_bin()
        .args(["worktree", "analyze", "--"])
        .arg(&linked)
        .env("PATH", path_with_plugin(plugin_dir.path()))
        .assert()
        .success();

    let store = find_single_worktree_store(&repo);
    let config = latest_run_config(&store.join("loomweave.db"));
    assert_eq!(
        config["analysis"]["clustering"]["seed"], 4242,
        "the primary's loomweave.yaml must govern a config-less linked worktree's \
         analyze: {config}"
    );
}

/// The `--config` flag on `worktree analyze` wins over the ladder — parity
/// with plain `analyze` (clarion-c39b92b868).
#[cfg(unix)]
#[test]
fn worktree_analyze_explicit_config_wins_over_ladder() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        plugin_dir.path(),
        "explicit-config",
        "feature-explicit",
        "explicit_module",
    );
    std::fs::write(
        repo.join("loomweave.yaml"),
        "analysis:\n  clustering:\n    seed: 4242\n",
    )
    .unwrap();
    let explicit = tmp.path().join("elsewhere.yaml");
    std::fs::write(&explicit, "analysis:\n  clustering:\n    seed: 7777\n").unwrap();

    loomweave_bin()
        .args(["worktree", "analyze", "--config"])
        .arg(&explicit)
        .arg("--")
        .arg(&linked)
        .env("PATH", path_with_plugin(plugin_dir.path()))
        .assert()
        .success();

    let store = find_single_worktree_store(&repo);
    let config = latest_run_config(&store.join("loomweave.db"));
    assert_eq!(
        config["analysis"]["clustering"]["seed"], 7777,
        "an explicit --config must win over the ladder: {config}"
    );
}
