//! `clarion hook session-start` integration tests.

use assert_cmd::Command;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[test]
fn hook_session_start_exits_zero_without_clarion_db() {
    // Fail-soft: no .clarion/ at all must still exit 0 and nudge.
    let dir = tempfile::tempdir().unwrap();
    let assert = clarion_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        out.contains("clarion analyze"),
        "missing analyze nudge in: {out}"
    );
}

#[test]
fn hook_session_start_prints_counts_for_installed_project() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let assert = clarion_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(out.contains("entities"), "missing entity count line: {out}");
    assert!(out.contains("clarion analyze"), "missing nudge: {out}");
}

#[test]
fn hook_session_start_resyncs_skill_when_present_and_drifted() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--skills", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let skill = dir.path().join(".claude/skills/clarion-workflow/SKILL.md");
    std::fs::write(&skill, "STALE").unwrap();

    clarion_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let body = std::fs::read_to_string(&skill).unwrap();
    assert!(
        body.contains("name: clarion-workflow"),
        "hook did not repair drifted skill: {body}"
    );
}
