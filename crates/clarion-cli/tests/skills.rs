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
    let body =
        fs::read_to_string(dir.path().join(".claude/skills/clarion-workflow/SKILL.md")).unwrap();
    assert!(body.contains("name: clarion-workflow"));
}

#[test]
fn install_hooks_merges_session_start_without_clobbering() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    fs::write(
        claude.join("settings.json"),
        r#"{"model":"opus","hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#,
    )
    .unwrap();

    clarion_bin()
        .args(["install", "--hooks", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let raw = fs::read_to_string(claude.join("settings.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["model"], "opus");
    assert_eq!(
        parsed["hooks"]["Stop"][0]["hooks"][0]["command"],
        "echo bye"
    );
    let cmds: Vec<String> = parsed["hooks"]["SessionStart"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["hooks"].as_array().unwrap())
        .map(|h| h["command"].as_str().unwrap().to_string())
        .collect();
    assert!(
        cmds.iter()
            .any(|c| c.contains("clarion hook session-start"))
    );
    assert!(!dir.path().join(".clarion").exists());
}

#[test]
fn install_all_does_init_skills_and_hooks() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--all", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(dir.path().join(".clarion/clarion.db").exists(), "no db");
    assert!(
        dir.path()
            .join(".claude/skills/clarion-workflow/SKILL.md")
            .exists(),
        "no skill"
    );
    let raw = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(raw.contains("clarion hook session-start"), "no hook: {raw}");
}

#[test]
fn install_all_is_rerunnable_and_preserves_index() {
    let dir = tempfile::tempdir().unwrap();
    // First --all: full setup.
    clarion_bin()
        .args(["install", "--all", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let db = dir.path().join(".clarion/clarion.db");
    assert!(db.exists(), "first --all did not create db");
    // Mark the db so we can prove the second run did NOT recreate it.
    let before = std::fs::metadata(&db).unwrap().modified().unwrap();

    // Second --all: must succeed (not bail), keep the index, re-apply skills/hooks.
    clarion_bin()
        .args(["install", "--all", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    assert!(db.exists(), "second --all destroyed the db");
    let after = std::fs::metadata(&db).unwrap().modified().unwrap();
    assert_eq!(
        before, after,
        "second --all recreated the db (index not preserved)"
    );
    assert!(
        dir.path()
            .join(".claude/skills/clarion-workflow/SKILL.md")
            .exists(),
        "skill missing after rerun"
    );
    let raw = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(
        raw.contains("clarion hook session-start"),
        "hook missing after rerun"
    );
}
