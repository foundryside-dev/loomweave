//! `loomweave config example|check` integration tests, plus the `doctor` LLM
//! check. These cover the agent-first-feedback §2.1/§2.3/§2.4 fixes: the schema
//! is discoverable from the binary, a misconfigured `loomweave.yaml` fails loud
//! (naming the bad key), and a configured-but-disabled provider is surfaced.

use std::fs;
use std::path::Path;

use assert_cmd::Command;

fn loomweave_bin() -> Command {
    Command::cargo_bin("loomweave").expect("loomweave binary")
}

/// Run `loomweave config <args>` in `dir` and return `(exit_code, stdout, stderr)`.
fn config(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let output = loomweave_bin()
        .arg("config")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run config");
    (
        output.status.code().expect("exit code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn config_example_emits_parseable_annotated_stub() {
    let (code, stdout, _) = config(Path::new("."), &["example"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("llm_policy:"), "stub: {stdout}");
    assert!(stdout.contains("provider: openrouter"), "stub: {stdout}");
    // The annotated stub must round-trip as a generic YAML document.
    serde_norway::from_str::<serde_norway::Value>(&stdout)
        .expect("config example output must be valid YAML");
}

#[test]
fn config_example_provider_flag_swaps_active_provider() {
    let (code, stdout, _) = config(Path::new("."), &["example", "--provider", "claude_cli"]);
    assert_eq!(code, 0);
    // Check the active config line (indented), not the comment that mentions
    // "provider: openrouter" as the default.
    assert!(
        stdout.contains("\n  provider: claude_cli"),
        "stub: {stdout}"
    );
    assert!(
        !stdout.contains("\n  provider: openrouter"),
        "stub: {stdout}"
    );
}

#[test]
fn config_example_rejects_unknown_provider() {
    let (code, _, stderr) = config(Path::new("."), &["example", "--provider", "bogus"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("bogus"), "stderr: {stderr}");
}

#[test]
fn config_check_reports_disabled_default_when_file_absent() {
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, _) = config(dir.path(), &["check"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("absent"), "out: {stdout}");
    assert!(stdout.contains("cache-only"), "out: {stdout}");
}

#[test]
fn config_check_warns_on_configured_but_disabled_provider() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "llm_policy:\n  provider: claude_cli\n  allow_live_provider: true\n",
    )
    .unwrap();
    let (code, stdout, _) = config(dir.path(), &["check"]);
    // A configured-but-disabled provider loads (exit 0) but must warn loudly.
    assert_eq!(code, 0, "out: {stdout}");
    assert!(stdout.contains("Warnings:"), "out: {stdout}");
    assert!(stdout.contains("enabled=false"), "out: {stdout}");
}

#[test]
fn config_check_fails_loud_on_unknown_nested_key() {
    // The exact dogfood bug: model_id placed under claude_cli (field is `model`).
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "llm_policy:\n  enabled: true\n  provider: claude_cli\n  claude_cli:\n    model_id: x\n",
    )
    .unwrap();
    let (code, _, stderr) = config(dir.path(), &["check"]);
    assert_ne!(code, 0, "a misplaced key must fail config check");
    assert!(
        stderr.contains("model_id"),
        "stderr should name the key: {stderr}"
    );
}
