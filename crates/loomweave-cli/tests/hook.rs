//! `loomweave hook session-start` integration tests.

use assert_cmd::Command;

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
fn hook_session_start_exits_zero_without_loomweave_db() {
    // Fail-soft: no .loomweave/ at all must still exit 0 and nudge.
    let dir = tempfile::tempdir().unwrap();
    let assert = loomweave_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        out.contains("loomweave analyze"),
        "missing analyze nudge in: {out}"
    );
    // Installing a skill into a project that never had one is `loomweave install
    // --skills`'s job, NOT the hook's. The hook only re-syncs a skill that is
    // already present, so a bare project must come away with no skill roots
    // created (clarion-ac0fc3bd86).
    assert!(
        !dir.path().join(".claude/skills").exists(),
        "hook must not create .claude/skills when no skill is present"
    );
    assert!(
        !dir.path().join(".agents/skills").exists(),
        "hook must not create .agents/skills when no skill is present"
    );
}

#[test]
fn hook_session_start_prints_counts_for_installed_project() {
    let dir = tempfile::tempdir().unwrap();
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let assert = loomweave_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(out.contains("entities"), "missing entity count line: {out}");
    assert!(out.contains("loomweave analyze"), "missing nudge: {out}");
}

#[test]
fn hook_session_start_exits_zero_with_corrupt_db() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".loomweave")).unwrap();
    // Garbage where loomweave.db should be — not a valid SQLite file.
    std::fs::write(
        dir.path().join(".loomweave/loomweave.db"),
        b"NOT A SQLITE DB",
    )
    .unwrap();
    let assert = loomweave_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success(); // fail-soft: must exit 0
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        out.contains("could not be opened") || out.contains("corrupt"),
        "expected a present-but-unreadable nudge, got: {out}"
    );
}

#[test]
fn hook_session_start_resyncs_skill_when_present_and_drifted() {
    let dir = tempfile::tempdir().unwrap();
    loomweave_bin()
        .args(["install", "--skills", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let skill = dir
        .path()
        .join(".claude/skills/loomweave-workflow/SKILL.md");
    std::fs::write(&skill, "STALE").unwrap();

    loomweave_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let body = std::fs::read_to_string(&skill).unwrap();
    assert!(
        body.contains("name: loomweave-workflow"),
        "hook did not repair drifted skill: {body}"
    );
}
