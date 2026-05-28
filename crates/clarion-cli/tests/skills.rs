//! `clarion install --skills/--hooks/--all` integration tests.

use std::fs;

use assert_cmd::Command;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[test]
fn install_skills_writes_pack_without_initialising_clarion_dir() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--skills", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        dir.path()
            .join(".claude/skills/clarion-workflow/SKILL.md")
            .exists(),
        "skill not installed under .claude"
    );
    assert!(
        dir.path()
            .join(".agents/skills/clarion-workflow/SKILL.md")
            .exists(),
        "skill not installed under .agents"
    );
    // --skills MUST NOT init .clarion/.
    assert!(
        !dir.path().join(".clarion").exists(),
        "--skills should not create .clarion/"
    );
}

#[test]
fn install_skills_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        clarion_bin()
            .args(["install", "--skills", "--path"])
            .arg(dir.path())
            .assert()
            .success();
    }
    let body = fs::read_to_string(
        dir.path().join(".claude/skills/clarion-workflow/SKILL.md"),
    )
    .unwrap();
    assert!(body.contains("name: clarion-workflow"));
}
