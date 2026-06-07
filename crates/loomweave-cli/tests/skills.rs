//! `loomweave install --skills/--codex-skills/--hooks/--all` integration tests.

use std::fs;

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
fn install_skills_writes_claude_pack_without_initialising_loomweave_dir() {
    let dir = tempfile::tempdir().unwrap();
    loomweave_bin()
        .args(["install", "--skills", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        dir.path()
            .join(".claude/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "skill not installed under .claude"
    );
    assert!(
        !dir.path()
            .join(".agents/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "--skills should not install Codex skills under .agents"
    );
    // --skills MUST NOT init .weft/loomweave/.
    assert!(
        !dir.path().join(".weft/loomweave").exists(),
        "--skills should not create .weft/loomweave/"
    );
}

#[test]
fn install_codex_skills_writes_agents_pack_without_initialising_loomweave_dir() {
    let dir = tempfile::tempdir().unwrap();
    loomweave_bin()
        .args(["install", "--codex-skills", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        !dir.path()
            .join(".claude/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "--codex-skills should not install Claude skills under .claude"
    );
    assert!(
        dir.path()
            .join(".agents/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "Codex skill not installed under .agents"
    );
    assert!(
        !dir.path().join(".weft/loomweave").exists(),
        "--codex-skills should not create .weft/loomweave/"
    );
}

#[test]
fn install_skills_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        loomweave_bin()
            .args(["install", "--skills", "--path"])
            .arg(dir.path())
            .assert()
            .success();
    }
    let body = fs::read_to_string(
        dir.path()
            .join(".claude/skills/loomweave-workflow/SKILL.md"),
    )
    .unwrap();
    assert!(body.contains("name: loomweave-workflow"));
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

    loomweave_bin()
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
            .any(|c| c.contains("loomweave hook session-start"))
    );
    assert!(!dir.path().join(".weft/loomweave").exists());
}

#[test]
fn install_all_does_init_skills_and_hooks() {
    let dir = tempfile::tempdir().unwrap();
    loomweave_bin()
        .args(["install", "--all", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        dir.path().join(".weft/loomweave/loomweave.db").exists(),
        "no db"
    );
    assert!(
        dir.path()
            .join(".claude/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "no Claude skill"
    );
    assert!(
        dir.path()
            .join(".agents/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "no Codex skill"
    );
    let raw = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(
        raw.contains("loomweave hook session-start"),
        "no hook: {raw}"
    );
    let mcp_raw = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    assert!(
        mcp_raw.contains("\"loomweave\""),
        "no Claude Code MCP entry: {mcp_raw}"
    );
}

#[test]
fn install_all_is_rerunnable_and_preserves_index() {
    let dir = tempfile::tempdir().unwrap();
    // First --all: full setup.
    loomweave_bin()
        .args(["install", "--all", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let db = dir.path().join(".weft/loomweave/loomweave.db");
    assert!(db.exists(), "first --all did not create db");
    // Mark the db so we can prove the second run did NOT recreate it.
    let before = std::fs::metadata(&db).unwrap().modified().unwrap();

    // Second --all: must succeed (not bail), keep the index, re-apply skills/hooks.
    loomweave_bin()
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
            .join(".claude/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "skill missing after rerun"
    );
    let raw = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(
        raw.contains("loomweave hook session-start"),
        "hook missing after rerun"
    );
}
