//! `loomweave config example|check` integration tests, plus the `doctor` LLM
//! check. These cover the agent-first-feedback §2.1/§2.3/§2.4 fixes: the schema
//! is discoverable from the binary, a misconfigured `loomweave.yaml` fails loud
//! (naming the bad key), and a configured-but-disabled provider is surfaced.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use loomweave_federation::config::{LlmProviderKind, McpConfig, SemanticProviderKind};

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
    assert!(stdout.contains("semantic_search:"), "stub: {stdout}");
    assert!(stdout.contains("provider: local_openai"), "stub: {stdout}");
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
fn config_example_accepts_sidecar_provider_aliases() {
    let cases = [
        ("openrouter_api", "openrouter"),
        ("codex_sidecar", "codex_cli"),
        ("claude_sidecar", "claude_cli"),
    ];

    for (alias, canonical) in cases {
        let (code, stdout, _) = config(Path::new("."), &["example", "--provider", alias]);
        assert_eq!(code, 0, "alias {alias} should be accepted");
        assert!(
            stdout.contains(&format!("\n  provider: {canonical}")),
            "alias {alias} should select canonical provider {canonical}. stub: {stdout}"
        );
    }
}

#[test]
fn config_example_accepts_every_llm_provider_parse_alias() {
    // L9: `--provider` routes through the same `LlmProviderKind::parse` the
    // YAML/serde path and the MCP schema accept, so the aliases the old
    // hand-maintained set rejected (open_router / codex / claude_code /
    // recording) are now accepted and normalised to their canonical line.
    let cases = [
        ("open_router", "openrouter"),
        ("codex", "codex_cli"),
        ("claude_code", "claude_cli"),
        ("recording", "recording"),
    ];

    for (alias, canonical) in cases {
        let (code, stdout, stderr) = config(Path::new("."), &["example", "--provider", alias]);
        assert_eq!(
            code, 0,
            "alias {alias} should be accepted; stderr: {stderr}"
        );
        assert!(
            stdout.contains(&format!("\n  provider: {canonical}")),
            "alias {alias} should select canonical provider {canonical}. stub: {stdout}"
        );
    }
}

#[test]
fn config_example_rejects_unknown_provider() {
    let (code, _, stderr) = config(Path::new("."), &["example", "--provider", "bogus"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("bogus"), "stderr: {stderr}");
}

#[test]
fn config_check_reports_defaults_when_file_absent() {
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, _) = config(dir.path(), &["check"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("absent"), "out: {stdout}");
    assert!(stdout.contains("cache-only"), "out: {stdout}");
    assert!(
        stdout.contains("MCP write tools:       true"),
        "out: {stdout}"
    );
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

#[test]
fn config_llm_set_enables_codex_and_mcp_write_tools() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "version: 1\nanalysis:\n  clustering:\n    enabled: false\n",
    )
    .unwrap();

    let (code, stdout, stderr) = config(
        dir.path(),
        &[
            "llm",
            "set",
            "--enable",
            "--allow-live",
            "--provider",
            "codex_sidecar",
            "--codex-model",
            "gpt-5-codex",
            "--enable-write-tools",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("MCP write tools:       true"), "{stdout}");

    let config = McpConfig::from_path(&dir.path().join("loomweave.yaml")).unwrap();
    assert!(config.llm.enabled);
    assert!(config.llm.allow_live_provider);
    assert_eq!(config.llm.provider, LlmProviderKind::CodexCli);
    assert_eq!(config.llm.codex_cli.model.as_deref(), Some("gpt-5-codex"));
    assert!(config.serve.mcp.enable_write_tools);
    assert_eq!(
        config.analysis["clustering"]["enabled"],
        serde_norway::Value::Bool(false),
        "unrelated analysis section should survive the edit"
    );
}

#[test]
fn config_llm_set_rejects_empty_patch() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = config(dir.path(), &["llm", "set"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("no LLM config changes requested"),
        "stderr: {stderr}"
    );
}

#[test]
fn config_semantic_set_enables_local_openai_without_key() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "version: 1\nllm_policy:\n  provider: codex_cli\n",
    )
    .unwrap();

    let (code, stdout, stderr) = config(
        dir.path(),
        &[
            "semantic",
            "set",
            "--enable",
            "--provider",
            "local_openai",
            "--endpoint-url",
            "http://127.0.0.1:11434/v1",
            "--model-id",
            "nomic-embed-text",
            "--dimensions",
            "768",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("Semantic provider:      local_openai"),
        "{stdout}"
    );
    assert!(stdout.contains("Provider available:     true"), "{stdout}");

    let config = McpConfig::from_path(&dir.path().join("loomweave.yaml")).unwrap();
    assert!(config.semantic_search.enabled);
    assert_eq!(
        config.semantic_search.provider,
        SemanticProviderKind::LocalOpenAi
    );
    assert_eq!(config.semantic_search.dimensions, 768);
    assert_eq!(config.llm.provider, LlmProviderKind::CodexCli);
}

#[test]
fn config_semantic_set_rejects_non_loopback_local_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = config(
        dir.path(),
        &[
            "semantic",
            "set",
            "--enable",
            "--provider",
            "local_openai",
            "--endpoint-url",
            "https://api.openai.com/v1",
        ],
    );
    assert_ne!(code, 0);
    assert!(
        stderr.contains("LMWV-CONFIG-SEMANTIC-NON-LOOPBACK"),
        "stderr: {stderr}"
    );
}

#[test]
fn config_semantic_set_disable_succeeds_despite_non_loopback_local_endpoint() {
    // weft-ac59e8e730: a disabled semantic block must not be held to the
    // loopback-trust gate — otherwise `config semantic set --disable` is
    // itself rejected (the edit path re-parses the file before writing) and
    // the operator has no in-tool recovery from a stale endpoint.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "semantic_search:\n  enabled: false\n  provider: local_openai\n  endpoint_url: http://192.168.1.50:11434/v1\n",
    )
    .unwrap();

    // The config must load (status must not hard-fail)...
    let (code, stdout, stderr) = config(dir.path(), &["semantic", "status"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("Semantic enabled:       false"), "{stdout}");

    // ...and explicitly writing the disabled state must succeed.
    let (code, stdout, stderr) = config(dir.path(), &["semantic", "set", "--disable"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let config = McpConfig::from_path(&dir.path().join("loomweave.yaml")).unwrap();
    assert!(!config.semantic_search.enabled);
}

#[test]
fn config_semantic_status_reports_sidecar_absent_without_secret() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "semantic_search:\n  enabled: true\n  provider: local_openai\n  endpoint_url: http://localhost:11434/v1\n",
    )
    .unwrap();
    let (code, stdout, stderr) = config(dir.path(), &["semantic", "status"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("API key env:"), "{stdout}");
    assert!(
        stdout.contains("Sidecar vectors:        absent"),
        "{stdout}"
    );
    assert!(
        stdout.contains("start the local embeddings server"),
        "{stdout}"
    );
}

/// ADR-063: a `loomweave.yaml` the repository tracks is corpus content.
/// `config check` names the verdict and the remedy before anything else, and
/// `config llm set` refuses to write into it at all.
#[test]
fn config_check_reports_a_tracked_config_and_llm_set_refuses_it() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("loomweave.yaml"),
        "version: 1\nllm_policy:\n  enabled: true\n  provider: codex_cli\n  allow_live_provider: true\n",
    )
    .unwrap();
    for args in [vec!["init", "-q"], vec!["add", "-f", "loomweave.yaml"]] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    let (code, stdout, stderr) = config(dir.path(), &["check"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("config trust: repository_tracked"),
        "{stdout}"
    );
    assert!(stdout.contains("llm_policy"), "{stdout}");
    assert!(
        stdout.contains("git rm --cached loomweave.yaml"),
        "{stdout}"
    );
    // The stripped llm_policy must actually take effect, not just be announced.
    assert!(stdout.contains("LLM enabled:           false"), "{stdout}");

    let (set_code, set_stdout, set_stderr) = config(dir.path(), &["llm", "set", "--enable"]);
    assert_ne!(set_code, 0, "stdout: {set_stdout}");
    assert!(
        set_stderr.contains("tracked by the repository"),
        "stderr: {set_stderr}"
    );
    // The refusal must leave the file byte-identical.
    assert_eq!(
        fs::read_to_string(dir.path().join("loomweave.yaml")).unwrap(),
        "version: 1\nllm_policy:\n  enabled: true\n  provider: codex_cli\n  allow_live_provider: true\n",
    );
}

/// Regression: a BARE RELATIVE `--config` path (`--config loomweave.yaml`) has
/// `Path::parent() == Some("")`, which names no directory. Before the fix the
/// trust probe short-circuited to a permissive verdict there, so
/// `serve`/`analyze`/`config … --config loomweave.yaml` run from inside a
/// corpus repository treated a COMMITTED config as operator-owned: its
/// `llm_policy` and `integrations` reached live clients and both writer gates
/// opened. The probe is now rooted at the process cwd.
///
/// Deliberately an integration test: the cwd is the subprocess's, so nothing
/// here mutates this process's state the way `std::env::set_current_dir` would.
#[test]
fn a_bare_relative_config_path_is_probed_against_the_cwd_repository() {
    const ORIGINAL: &str = "version: 1\nllm_policy:\n  enabled: true\n  provider: codex_cli\n  allow_live_provider: true\n";

    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("loomweave.yaml"), ORIGINAL).unwrap();
    for args in [vec!["init", "-q"], vec!["add", "-f", "loomweave.yaml"]] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    // The reader: a bare relative --config still sees repository ownership.
    let (code, stdout, stderr) = config(dir.path(), &["check", "--config", "loomweave.yaml"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("config trust: repository_tracked"),
        "a bare relative --config must not read as trusted: {stdout}"
    );

    // The writer: same path, same cwd, refused — and the file untouched.
    let (set_code, set_stdout, set_stderr) = config(
        dir.path(),
        &["llm", "set", "--config", "loomweave.yaml", "--enable"],
    );
    assert_ne!(set_code, 0, "stdout: {set_stdout}");
    assert!(
        set_stderr.contains("tracked by the repository"),
        "stderr: {set_stderr}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("loomweave.yaml")).unwrap(),
        ORIGINAL,
        "a refused write must leave the file byte-identical"
    );
}
