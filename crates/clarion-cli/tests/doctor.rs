//! `clarion doctor [--fix]` integration tests.
//!
//! Exercises the exit-code contract (healthy -> 0, any problem -> 1) and the
//! end-to-end `--fix` wiring across the three orientation surfaces. Per-surface
//! detection/merge correctness is unit-tested in the owning modules
//! (`skill_pack`, `hooks_settings`, `mcp_registration`).

use std::fs;
use std::path::Path;

use assert_cmd::Command;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

fn install(args: &[&str], dir: &Path) {
    clarion_bin()
        .args(args)
        .arg("--path")
        .arg(dir)
        .assert()
        .success();
}

/// Run `doctor` (optionally with `--fix`) and return `(exit_code, stdout)`.
fn doctor(dir: &Path, fix: bool) -> (i32, String) {
    let mut cmd = clarion_bin();
    cmd.arg("doctor");
    if fix {
        cmd.arg("--fix");
    }
    let output = cmd.arg("--path").arg(dir).output().expect("run doctor");
    (
        output.status.code().expect("exit code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// A freshly `install --all`ed project has the skill + hook, but `install`
/// never registers `.mcp.json`, so `doctor` must flag the missing MCP entry and
/// exit non-zero.
#[test]
fn doctor_flags_missing_mcp_entry_after_plain_install() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(code, 1, "missing mcp entry must fail; stdout:\n{out}");
    assert!(out.contains("skill pack up to date"), "stdout:\n{out}");
    assert!(out.contains("SessionStart hook present"), "stdout:\n{out}");
    assert!(
        out.contains(".mcp.json has no clarion serve entry"),
        "stdout:\n{out}"
    );
}

/// `doctor --fix` registers the MCP entry; a subsequent plain `doctor` is then
/// fully healthy and exits 0. The `.mcp.json` gains a `clarion` serve entry.
#[test]
fn doctor_fix_registers_mcp_then_reports_healthy() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "--fix should repair and exit 0; stdout:\n{out}");
    assert!(
        out.contains("All orientation surfaces healthy."),
        "stdout:\n{out}"
    );

    // The entry is now on disk, pinned to this project with the bare command.
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["clarion"]["command"], "clarion");
    let canon = dir.path().canonicalize().unwrap().display().to_string();
    assert_eq!(
        v["mcpServers"]["clarion"]["args"],
        serde_json::json!(["serve", "--path", canon])
    );

    // A plain re-run is now clean.
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "a repaired project must be healthy on re-run");
}

/// `doctor --fix` preserves a sibling MCP server (e.g. filigree) already in
/// `.mcp.json` while adding the clarion entry.
#[test]
fn doctor_fix_preserves_sibling_mcp_server() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    fs::write(
        dir.path().join(".mcp.json"),
        r#"{"mcpServers":{"filigree":{"type":"stdio","command":"/opt/filigree-mcp","args":[]}}}"#,
    )
    .unwrap();

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "stdout:\n{out}");

    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        v["mcpServers"]["filigree"]["command"], "/opt/filigree-mcp",
        "sibling server must be preserved"
    );
    assert_eq!(v["mcpServers"]["clarion"]["command"], "clarion");
}

/// With only the skill installed (no hook, no mcp), `doctor` reports two
/// problems and exits 1; the index snapshot block is still printed.
#[test]
fn doctor_reports_missing_hook_and_mcp_and_prints_index_block() {
    let dir = tempfile::tempdir().unwrap();
    // --skills installs ONLY the skill pack (no .clarion/, no hook, no mcp).
    install(&["install", "--skills"], dir.path());

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(code, 1, "stdout:\n{out}");
    assert!(out.contains("SessionStart hook missing"), "stdout:\n{out}");
    assert!(
        out.contains(".mcp.json has no clarion serve entry"),
        "stdout:\n{out}"
    );
    assert!(out.contains("--- index ---"), "stdout:\n{out}");
    assert!(out.contains("2 problems found"), "stdout:\n{out}");
}
