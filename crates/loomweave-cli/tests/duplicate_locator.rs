//! Duplicate entity-locator detection (clarion-b19fe90c3e).
//!
//! The writer absorbs a colliding entity id via `ON CONFLICT(id) DO UPDATE`
//! (last-write-wins) — deliberately, because the absorption is load-bearing
//! for incremental upserts. These tests assert that the host's analyze path
//! nevertheless SURFACES a collision as an entity-anchored
//! `LMWV-DUPLICATE-LOCATOR` ERROR finding (anchored to the colliding entity
//! since clarion-48af930f2a, so the shadow is queryable from the entity read
//! path), and — just as important — that the alarm stays silent on every
//! legitimate-recurrence shape (unchanged re-analysis, genuine moves, the
//! clarion-6ec7317628 module dual-claim).
//!
//! Driven through the fixture plugin's content-driven `gadget <name>` lines
//! (each emits a `fixture:gadget:<name>` entity) and the
//! `LOOMWEAVE_FIXTURE_DUPLICATE_WIDGET` misbehaviour knob.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use std::{env, fs};

use assert_cmd::Command;
use rusqlite::Connection;
use tempfile::TempDir;

const RULE_ID: &str = "LMWV-DUPLICATE-LOCATOR";
const ANALYZE_BACKSTOP: Duration = Duration::from_secs(120);

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

/// Locate the `loomweave-fixture-plugin` binary (same convention as
/// `analyze_hardening.rs`).
fn fixture_binary_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_loomweave-fixture-plugin") {
        return PathBuf::from(path);
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root must exist");

    let target_dir =
        env::var("CARGO_TARGET_DIR").map_or_else(|_| workspace_root.join("target"), PathBuf::from);

    for profile in &["debug", "release"] {
        let candidate = target_dir.join(profile).join("loomweave-fixture-plugin");
        if candidate.exists() {
            return candidate;
        }
    }

    panic!(
        "loomweave-fixture-plugin binary not found. \
         Run `cargo build --workspace` before running this test. \
         Searched: {}",
        target_dir.display()
    );
}

/// Synthetic `$PATH` directory: fixture binary (symlinked under the
/// `loomweave-plugin-*` discovery glob) + its `plugin.toml` (extensions: mt).
fn setup_plugin_dir(fixture_bin: &PathBuf) -> TempDir {
    let plugin_dir = TempDir::new().expect("create plugin tempdir");

    let dest = plugin_dir.path().join("loomweave-plugin-fixture");
    std::os::unix::fs::symlink(fixture_bin, &dest).expect("symlink loomweave-plugin-fixture");

    let meta = fs::metadata(fixture_bin).expect("stat fixture binary");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "fixture binary must be executable"
    );

    let toml_src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("loomweave-core")
        .join("tests")
        .join("fixtures")
        .join("plugin.toml");
    fs::copy(&toml_src, plugin_dir.path().join("plugin.toml")).expect("copy plugin.toml");

    plugin_dir
}

/// Initialise a temp project (no source files yet) and return
/// (project dir, synthetic PATH value).
fn setup_project(plugin_dir: &TempDir) -> (TempDir, std::ffi::OsString) {
    let project_dir = TempDir::new().expect("create project tempdir");
    loomweave_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    let new_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");
    (project_dir, new_path)
}

fn write_source(project_dir: &TempDir, name: &str, content: &str) {
    fs::write(project_dir.path().join(name), content).expect("write source file");
}

/// Run `loomweave analyze` on the project; succeed-or-panic.
fn analyze(project_dir: &TempDir, new_path: &std::ffi::OsString, extra_args: &[&str]) {
    let mut cmd = loomweave_bin();
    cmd.args(["analyze"]).arg(project_dir.path());
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.env("PATH", new_path)
        .timeout(ANALYZE_BACKSTOP)
        .assert()
        .success();
}

fn open_db(project_dir: &TempDir) -> Connection {
    Connection::open(project_dir.path().join(".weft/loomweave/loomweave.db")).expect("open db")
}

fn finding_count(conn: &Connection, rule_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM findings WHERE rule_id = ?1",
        [rule_id],
        |row| row.get(0),
    )
    .expect("query finding count")
}

/// (severity, message, evidence) of the single `rule_id` finding.
fn single_finding(conn: &Connection, rule_id: &str) -> (String, String, String) {
    assert_eq!(
        finding_count(conn, rule_id),
        1,
        "expected exactly one {rule_id} finding"
    );
    conn.query_row(
        "SELECT severity, message, evidence FROM findings WHERE rule_id = ?1",
        [rule_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .expect("query finding")
}

/// The `entity_id` anchor column of the single `rule_id` finding.
fn finding_entity_id(conn: &Connection, rule_id: &str) -> String {
    conn.query_row(
        "SELECT entity_id FROM findings WHERE rule_id = ?1",
        [rule_id],
        |row| row.get(0),
    )
    .expect("query finding entity_id")
}

fn entity_row_count(conn: &Connection, id: &str) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM entities WHERE id = ?1", [id], |row| {
        row.get(0)
    })
    .expect("query entity row count")
}

/// In-run, same-file shape: one file emits the same gadget id twice (three
/// times, in fact — proving one finding per id per run, not per occurrence).
/// The run still completes (the alarm detects; it does not block).
#[test]
fn in_run_same_file_duplicate_emits_single_error_finding() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    write_source(
        &project_dir,
        "demo.mt",
        "widget demo.sample {}\ngadget dup.item\ngadget dup.item\ngadget dup.item\n",
    );

    analyze(&project_dir, &new_path, &[]);

    let conn = open_db(&project_dir);
    let (severity, message, evidence) = single_finding(&conn, RULE_ID);
    assert_eq!(severity, "ERROR", "a duplicate locator is silent data loss");
    assert!(
        message.contains("fixture:gadget:dup.item"),
        "message must name the colliding id; got {message:?}"
    );
    assert!(
        evidence.contains("in_run_same_file"),
        "evidence must carry the same-file shape; got {evidence}"
    );
    assert!(
        evidence.contains("anchor_entity_id"),
        "finding must anchor to the colliding entity (clarion-48af930f2a) so it \
         is queryable from the entity read path AND still reaches Filigree's \
         emit with a real path via the entity's own source_file_path; got {evidence}"
    );

    // The run committed: the entity row exists despite the collision.
    let run_status: String = conn
        .query_row("SELECT status FROM runs", [], |row| row.get(0))
        .expect("query run status");
    assert_eq!(run_status, "completed", "the alarm must not fail the run");
}

/// In-run, cross-file shape: two files emit the same gadget id; the single
/// finding names both source paths.
#[test]
fn in_run_cross_file_duplicate_names_both_paths() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    write_source(&project_dir, "alpha.mt", "gadget shared.item\n");
    write_source(&project_dir, "beta.mt", "gadget shared.item\n");

    analyze(&project_dir, &new_path, &[]);

    let conn = open_db(&project_dir);
    let (severity, message, evidence) = single_finding(&conn, RULE_ID);
    assert_eq!(severity, "ERROR");
    assert!(
        message.contains("fixture:gadget:shared.item"),
        "message must name the colliding id; got {message:?}"
    );
    assert!(
        message.contains("alpha.mt") && message.contains("beta.mt"),
        "message must name both colliding source paths; got {message:?}"
    );
    assert!(
        evidence.contains("in_run_cross_file"),
        "evidence must carry the cross-file shape; got {evidence}"
    );
}

/// Cross-run shape: run A seeds the gadget from `alpha.mt`; run B (incremental)
/// analyzes ONLY the new `beta.mt`, which emits the same id while `alpha.mt`
/// — unchanged, skipped — still claims it in the store. That is a genuine
/// collision, not a move.
#[test]
fn cross_run_duplicate_against_unchanged_file_is_flagged() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    write_source(&project_dir, "alpha.mt", "gadget shared.item\n");
    analyze(&project_dir, &new_path, &[]);
    {
        let conn = open_db(&project_dir);
        assert_eq!(finding_count(&conn, RULE_ID), 0, "run A must be clean");
    }

    write_source(&project_dir, "beta.mt", "gadget shared.item\n");
    analyze(&project_dir, &new_path, &[]);

    let conn = open_db(&project_dir);
    let (severity, message, evidence) = single_finding(&conn, RULE_ID);
    assert_eq!(severity, "ERROR");
    assert!(
        message.contains("fixture:gadget:shared.item"),
        "message must name the colliding id; got {message:?}"
    );
    assert!(
        message.contains("alpha.mt") && message.contains("beta.mt"),
        "message must name the unchanged prior owner and the new emitter; got {message:?}"
    );
    assert!(
        evidence.contains("cross_run_unchanged_file"),
        "evidence must carry the cross-run shape; got {evidence}"
    );
}

/// Negative control: the SAME id legitimately recurs across runs — an
/// incremental re-analyze (everything skipped) and a forced full re-analyze
/// (everything re-emitted from its own unchanged file) must both stay silent.
#[test]
fn unchanged_reanalysis_emits_no_findings() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    write_source(&project_dir, "alpha.mt", "gadget alpha.item\n");
    write_source(&project_dir, "beta.mt", "gadget beta.item\n");

    analyze(&project_dir, &new_path, &[]);
    analyze(&project_dir, &new_path, &[]); // incremental: all files skipped
    analyze(&project_dir, &new_path, &["--no-incremental"]); // full re-emit

    let conn = open_db(&project_dir);
    assert_eq!(
        finding_count(&conn, RULE_ID),
        0,
        "legitimate recurrence across runs must never trip the alarm"
    );
}

/// Negative control: a genuine move. Run B re-analyzes the old file too (it
/// changed and no longer emits the id) while the new file picks it up — the
/// old claim dies this run, so the alarm stays silent.
#[test]
fn genuine_move_emits_no_findings() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    write_source(&project_dir, "alpha.mt", "gadget moved.item\n");
    analyze(&project_dir, &new_path, &[]);

    write_source(&project_dir, "alpha.mt", "gadget kept.item\n");
    write_source(&project_dir, "beta.mt", "gadget moved.item\n");
    analyze(&project_dir, &new_path, &[]);

    let conn = open_db(&project_dir);
    assert_eq!(
        finding_count(&conn, RULE_ID),
        0,
        "a genuine move (old file re-analyzed, id relocated) must not trip the alarm"
    );
}

/// Module dual-claim carve-out (clarion-6ec7317628): the fixture's `file_scope`
/// widget carries the SAME id from every file, and the first-claim-wins
/// machinery reconciles that deliberately — across files AND across runs
/// (unchanged claim owner + new emitter). No finding for either shape.
#[test]
fn file_scope_dual_claim_across_files_stays_silent() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    write_source(&project_dir, "alpha.mt", "widget demo.sample {}\n");
    write_source(&project_dir, "beta.mt", "widget demo.sample {}\n");
    analyze(&project_dir, &new_path, &[]);

    // Cross-run flavour: a NEW file emits the widget id while its unchanged
    // claim owner is skipped.
    write_source(&project_dir, "gamma.mt", "widget demo.sample {}\n");
    analyze(&project_dir, &new_path, &[]);

    let conn = open_db(&project_dir);
    assert_eq!(
        finding_count(&conn, RULE_ID),
        0,
        "the reconciled module dual-claim must not be double-reported"
    );
}

/// A `file_scope` id emitted twice from ONE file is not a dual declaration —
/// it is a plugin bug, and the carve-out does not cover it.
#[test]
fn file_scope_same_file_duplicate_is_flagged() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    write_source(&project_dir, "demo.mt", "widget demo.sample {}\n");

    let mut cmd = loomweave_bin();
    cmd.args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &new_path)
        .env("LOOMWEAVE_FIXTURE_DUPLICATE_WIDGET", "1")
        .timeout(ANALYZE_BACKSTOP)
        .assert()
        .success();

    let conn = open_db(&project_dir);
    let (severity, message, evidence) = single_finding(&conn, RULE_ID);
    assert_eq!(severity, "ERROR");
    assert!(
        message.contains("fixture:widget:demo.sample"),
        "message must name the colliding id; got {message:?}"
    );
    assert!(
        evidence.contains("in_run_same_file"),
        "evidence must carry the same-file shape; got {evidence}"
    );
}

/// clarion-48af930f2a: the duplicate-locator finding must anchor to the
/// COLLIDING ENTITY (the survivor row), not the file — so an entity-scoped read
/// (`entity_finding_list` / the `collision` projection in `entity_json`) on the
/// shadowed declaration surfaces the chimera instead of returning a clean row.
/// And the disclosure must follow the standing finding lifecycle: it survives a
/// no-op incremental run (loomweave's normal mode) and clears only on a clean
/// full pass that re-walks the files and no longer reproduces the collision.
#[test]
fn duplicate_locator_finding_anchors_to_entity_and_follows_lifecycle() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let (project_dir, new_path) = setup_project(&plugin_dir);
    write_source(&project_dir, "alpha.mt", "gadget shared.item\n");
    write_source(&project_dir, "beta.mt", "gadget shared.item\n");

    // Run 1 (full): the cross-file collision fires.
    analyze(&project_dir, &new_path, &[]);
    {
        let conn = open_db(&project_dir);
        assert_eq!(
            finding_count(&conn, RULE_ID),
            1,
            "run 1 must surface the collision exactly once"
        );
        // The anchor is the colliding ENTITY, not a `core:file:*` row. This is
        // what makes the shadow queryable from the entity read path.
        assert_eq!(
            finding_entity_id(&conn, RULE_ID),
            "fixture:gadget:shared.item",
            "the finding must anchor to the colliding entity id, not the file"
        );
        // The survivor row exists under that id (one row per id, by design).
        assert_eq!(entity_row_count(&conn, "fixture:gadget:shared.item"), 1);
    }

    // Run 2 (incremental no-op): nothing changed, so neither file is dispatched
    // and the collision is not re-detected. The disclosure must NOT be swept —
    // the general stale-finding sweep is gated to a clean full pass.
    analyze(&project_dir, &new_path, &[]);
    {
        let conn = open_db(&project_dir);
        assert_eq!(
            finding_count(&conn, RULE_ID),
            1,
            "a no-op incremental run must not retire the still-valid collision"
        );
        assert_eq!(
            finding_entity_id(&conn, RULE_ID),
            "fixture:gadget:shared.item",
            "the anchor must remain the colliding entity across an incremental no-op"
        );
    }

    // Run 3 (resolution): remove one declaration and force a full re-pass. The
    // collision no longer reproduces, so the stale disclosure is retired.
    fs::remove_file(project_dir.path().join("beta.mt")).expect("remove beta.mt");
    analyze(&project_dir, &new_path, &["--no-incremental"]);
    {
        let conn = open_db(&project_dir);
        assert_eq!(
            finding_count(&conn, RULE_ID),
            0,
            "a clean full pass that no longer reproduces the collision must clear it"
        );
        // The surviving declaration is still in the graph, now uncontested.
        assert_eq!(entity_row_count(&conn, "fixture:gadget:shared.item"), 1);
    }
}
