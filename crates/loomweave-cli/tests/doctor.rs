//! `loomweave doctor [--fix]` integration tests.
//!
//! Exercises the exit-code contract (healthy -> 0, any problem -> 1) and the
//! end-to-end `--fix` wiring across the three orientation surfaces. Per-surface
//! detection/merge correctness is unit-tested in the owning modules
//! (`skill_pack`, `hooks_settings`, `mcp_registration`).

use std::fs;
use std::path::Path;

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

fn install(args: &[&str], dir: &Path) {
    loomweave_bin()
        .args(args)
        .arg("--path")
        .arg(dir)
        .assert()
        .success();
}

fn read_yaml(path: &Path) -> serde_json::Value {
    serde_norway::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Run `doctor` (optionally with `--fix`) and return `(exit_code, stdout)`.
fn doctor(dir: &Path, fix: bool) -> (i32, String) {
    let mut cmd = loomweave_bin();
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

fn doctor_json(dir: &Path, fix: bool) -> (i32, serde_json::Value) {
    let mut cmd = loomweave_bin();
    cmd.arg("doctor");
    if fix {
        cmd.arg("--fix");
    }
    let output = cmd
        .args(["--format", "json"])
        .arg("--path")
        .arg(dir)
        .output()
        .expect("run doctor json");
    let stdout = String::from_utf8_lossy(&output.stdout);
    (
        output.status.code().expect("exit code"),
        serde_json::from_str(&stdout).unwrap_or_else(|err| {
            panic!("doctor --format json must emit parseable JSON: {err}\nstdout:\n{stdout}")
        }),
    )
}

/// A freshly `install --all`ed project has every orientation surface, including
/// Claude Code MCP, so `doctor` must report it healthy.
#[test]
fn doctor_reports_plain_install_healthy() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(code, 0, "plain install should be healthy; stdout:\n{out}");
    assert!(out.contains("skill pack up to date"), "stdout:\n{out}");
    assert!(out.contains("SessionStart hook present"), "stdout:\n{out}");
    assert!(
        out.contains(".mcp.json loomweave serve entry present"),
        "stdout:\n{out}"
    );
}

/// `doctor --fix` registers the MCP entry; a subsequent plain `doctor` is then
/// fully healthy and exits 0. The `.mcp.json` gains a `loomweave` serve entry.
#[test]
fn doctor_fix_registers_mcp_then_reports_healthy() {
    let dir = tempfile::tempdir().unwrap();
    install(
        &["install", "--skills", "--codex-skills", "--hooks"],
        dir.path(),
    );

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "--fix should repair and exit 0; stdout:\n{out}");
    assert!(
        out.contains("All orientation surfaces healthy."),
        "stdout:\n{out}"
    );

    // The entry is now on disk and uses runtime project autodiscovery.
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert!(
        v["mcpServers"]["loomweave"]["command"]
            .as_str()
            .unwrap()
            .ends_with("loomweave")
    );
    assert_eq!(
        v["mcpServers"]["loomweave"]["args"],
        serde_json::json!(["serve"])
    );

    // A plain re-run is now clean.
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "a repaired project must be healthy on re-run");
}

/// `doctor --fix` preserves a sibling MCP server (e.g. filigree) already in
/// `.mcp.json` while adding the loomweave entry.
#[test]
fn doctor_fix_preserves_sibling_mcp_server() {
    let dir = tempfile::tempdir().unwrap();
    install(
        &["install", "--skills", "--codex-skills", "--hooks"],
        dir.path(),
    );
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
    assert!(
        v["mcpServers"]["loomweave"]["command"]
            .as_str()
            .unwrap()
            .ends_with("loomweave")
    );
}

#[test]
fn doctor_fix_repairs_missing_three_way_integration_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let filigree_dir = dir.path().join(".weft").join("filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8749\n").unwrap();

    install(
        &[
            "install",
            "--skills",
            "--codex-skills",
            "--hooks",
            "--claude-code",
            "--instructions",
        ],
        dir.path(),
    );

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 0,
        "missing enrich-only integration bindings must NOT fail the gate (federation axiom: \
         Wardline is enrich-only, a Loomweave-solo/Filigree-only project is first-class):\n{out}"
    );
    assert!(
        out.contains("⚠ three-way integration bindings missing or stale"),
        "missing bindings should surface as a warning, not a problem:\n{out}"
    );
    assert!(
        out.contains("1 warning; no problems"),
        "summary should report the warning without claiming a problem:\n{out}"
    );

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "--fix should repair and exit 0; stdout:\n{out}");
    assert!(
        out.contains("three-way integration bindings missing or stale — fixed"),
        "stdout:\n{out}"
    );

    let loomweave_yaml = read_yaml(&dir.path().join("loomweave.yaml"));
    assert_eq!(
        loomweave_yaml["integrations"]["filigree"]["base_url"],
        "http://127.0.0.1:8749"
    );
    assert_eq!(
        loomweave_yaml["serve"]["http"]["wardline_taint_write"],
        serde_json::json!(true)
    );

    let wardline_yaml = read_yaml(&dir.path().join("wardline.yaml"));
    assert_eq!(
        wardline_yaml["filigree"]["url"],
        "http://127.0.0.1:8749/api/weft/scan-results"
    );

    let expected_port = loomweave_federation::loomweave_port::deterministic_port(
        &dir.path().canonicalize().unwrap(),
    );
    let expected_loomweave_url = format!("http://127.0.0.1:{expected_port}");

    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["wardline"]["args"],
        serde_json::json!([
            "mcp",
            "--root",
            ".",
            "--loomweave-url",
            expected_loomweave_url,
            "--filigree-url",
            "http://127.0.0.1:8749/api/weft/scan-results"
        ])
    );

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(code, 0, "repaired project should be healthy:\n{out}");
}

#[test]
fn doctor_json_reports_stable_check_shape_for_healthy_install() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "healthy install should exit 0: {json}");
    assert_eq!(json["ok"], true);
    assert!(json["checks"].as_array().unwrap().iter().any(|check| {
        check["id"] == "mcp.registration"
            && check["status"] == "ok"
            && check["fixed"] == serde_json::json!(false)
    }));
    assert!(json["checks"].as_array().unwrap().iter().any(|check| {
        check["id"] == "integration.bindings"
            && check["status"] == "ok"
            && check["fixed"] == serde_json::json!(false)
    }));
    assert!(
        json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| { check["id"] == "index.freshness" && check["status"].is_string() })
    );
    assert!(
        json["next_actions"].is_array(),
        "next_actions must always be an array: {json}"
    );
}

#[test]
fn doctor_fix_json_reports_fixed_config_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let filigree_dir = dir.path().join(".weft").join("filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8749\n").unwrap();
    install(
        &[
            "install",
            "--skills",
            "--codex-skills",
            "--hooks",
            "--claude-code",
        ],
        dir.path(),
    );

    let (code, json) = doctor_json(dir.path(), true);
    assert_eq!(code, 0, "--fix json should repair and exit 0: {json}");
    assert_eq!(json["ok"], true);
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "integration.bindings")
        .expect("integration.bindings check");
    assert_eq!(check["status"], "fixed");
    assert_eq!(check["fixed"], serde_json::json!(true));

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "repaired project should be healthy: {json}");
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "integration.bindings")
        .expect("integration.bindings check");
    assert_eq!(check["status"], "ok");
    assert_eq!(check["fixed"], serde_json::json!(false));
}

/// With only the skill installed (no hook, no mcp, no integration bindings),
/// `doctor` exits 1 on the genuine problems (missing hook + mcp) while the
/// enrich-only integration bindings surface only as a warning; the index
/// snapshot block is still printed.
#[test]
fn doctor_reports_missing_hook_and_mcp_and_prints_index_block() {
    let dir = tempfile::tempdir().unwrap();
    // Skill flags install ONLY the skill packs (no .weft/loomweave/, no hook, no mcp).
    install(&["install", "--skills", "--codex-skills"], dir.path());

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(code, 1, "stdout:\n{out}");
    assert!(out.contains("SessionStart hook missing"), "stdout:\n{out}");
    assert!(
        out.contains(".mcp.json has no loomweave serve entry"),
        "stdout:\n{out}"
    );
    assert!(
        out.contains("⚠ three-way integration bindings missing or stale"),
        "enrich-only bindings should be a warning, not a problem:\n{out}"
    );
    assert!(out.contains("--- index ---"), "stdout:\n{out}");
    // Only the hook and mcp surfaces are genuine problems; bindings is a warning.
    assert!(out.contains("2 problems found"), "stdout:\n{out}");
}

/// A hostile checkout can ship a `.mcp.json` whose `loomweave` entry names an
/// attacker-controlled `command` that the MCP client would later launch.
/// `doctor` must NOT report that as healthy (the false all-clear bug), but it
/// also must not clobber a possibly-deliberate wrapper: it flags the entry
/// (exit 1) and, under `--fix`, repairs args while leaving the command in
/// place as an advisory warning (exit 0) for the operator to adjudicate.
#[test]
fn doctor_flags_untrusted_mcp_command_without_clobbering_it() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    let canon = dir.path().canonicalize().unwrap().display().to_string();
    fs::write(
        dir.path().join(".mcp.json"),
        format!(
            r#"{{"mcpServers":{{"loomweave":{{"type":"stdio","command":"./evil-mcp.sh","args":["serve","--path",{canon:?}],"env":{{}}}}}}}}"#
        ),
    )
    .unwrap();

    // No --fix: the poisoned command must fail the gate, not pass as healthy.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "untrusted command must fail the gate; stdout:\n{out}"
    );
    assert!(
        out.contains("unrecognized command") && out.contains("evil-mcp.sh"),
        "doctor must name the unrecognized command; stdout:\n{out}"
    );

    // --fix: advisory (exit 0) but the attacker command is left untouched on
    // disk — never clobbered, never silently trusted.
    let (code, out) = doctor(dir.path(), true);
    assert_eq!(
        code, 0,
        "--fix downgrades to advisory warning; stdout:\n{out}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        v["mcpServers"]["loomweave"]["command"], "./evil-mcp.sh",
        "doctor --fix must not clobber a custom command"
    );

    // The JSON surface agrees: a warning (not ok, not a silent pass to Present).
    let (_code, report) = doctor_json(dir.path(), false);
    let mcp = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "mcp.registration")
        .expect("mcp.registration check present");
    assert_eq!(mcp["status"], "problem", "report: {report}");
    assert_eq!(
        report["ok"], false,
        "an untrusted command makes the run not ok"
    );
}

/// Instructions severity model (plan decision #2, the product-judgment veto
/// point): `Missing` is a non-gating **warning** — the same guidance ships via
/// the MCP preamble and the loomweave-workflow skill, so a project that omits
/// the always-loaded block is still first-class. A fresh `--all` install holds
/// the block; deleting it from one target file drives the aggregate to Missing,
/// which must surface as a warning and still exit 0.
#[test]
fn doctor_reports_missing_instructions_block_as_warning() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Drop the Loomweave block from one target file -> aggregate is Missing.
    fs::write(dir.path().join("AGENTS.md"), "# just notes\n").unwrap();

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 0,
        "a missing instructions block is an optional surface; must NOT fail the gate:\n{out}"
    );
    assert!(
        out.contains("⚠ agent-orientation block missing from CLAUDE.md / AGENTS.md"),
        "missing block should surface as a warning:\n{out}"
    );

    // --fix re-injects the block; a plain re-run is then clean.
    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "--fix should repair and exit 0:\n{out}");
    assert!(
        out.contains("agent-orientation block missing from CLAUDE.md / AGENTS.md — fixed"),
        "stdout:\n{out}"
    );
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "repaired project must be healthy on re-run");
}

/// `Drifted` -> **problem**: a stale block body fails the gate without `--fix`
/// and is auto-repaired with `--fix`. This pins the one branch that actually
/// gates the doctor exit code; a refactor flipping Drifted to a warning would
/// otherwise pass the suite undetected.
#[test]
fn doctor_reports_drifted_instructions_block_as_gating_problem() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Hand-edit the body inside the Loomweave span -> Drifted.
    let claude = dir.path().join("CLAUDE.md");
    let content = fs::read_to_string(&claude).unwrap();
    let drifted = content.replace("code archaeology", "DRIFTED HEADER");
    assert_ne!(drifted, content, "test setup: substitution must apply");
    fs::write(&claude, &drifted).unwrap();

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a drifted instructions block must FAIL the doctor gate without --fix:\n{out}"
    );
    assert!(
        out.contains("agent-orientation block drifted from the bundled copy"),
        "stdout:\n{out}"
    );

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(code, 0, "--fix should repair drift and exit 0:\n{out}");
    assert!(
        out.contains("agent-orientation block drifted from the bundled copy — fixed"),
        "stdout:\n{out}"
    );
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "repaired project must be healthy on re-run");
}

/// `Malformed` -> **problem**: a dangling Loomweave start marker (no following
/// end marker) fails the gate without `--fix`, and `--fix` repairs it without
/// truncating to EOF.
#[test]
fn doctor_reports_malformed_instructions_block_as_gating_problem() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Replace one target file's block with a dangling start marker.
    fs::write(
        dir.path().join("CLAUDE.md"),
        "# notes\n<!-- loomweave:instructions:v0:deadbeef -->\norphan body, no end marker\n",
    )
    .unwrap();

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a malformed instructions block must FAIL the doctor gate without --fix:\n{out}"
    );
    assert!(
        out.contains("agent-orientation block malformed (dangling loomweave marker)"),
        "stdout:\n{out}"
    );

    let (code, out) = doctor(dir.path(), true);
    assert_eq!(
        code, 0,
        "--fix should repair the malformed block and exit 0:\n{out}"
    );
    let fixed = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
    assert!(
        fixed.contains("# notes"),
        "leading content must survive the repair:\n{fixed}"
    );
    assert!(
        fixed.contains("orphan body, no end marker"),
        "orphaned body must survive as loose prose:\n{fixed}"
    );
    let (code, _) = doctor(dir.path(), false);
    assert_eq!(code, 0, "repaired project must be healthy on re-run");
}

/// JSON surface: pin the `instructions.block` check shape. Healthy install ->
/// status `ok`, `fixed: false`; a drifted block -> status `problem` and the run
/// aggregates to `ok: false`. The healthy-install json shape test omits this
/// check, leaving the status string and `fixed` flag unverified.
#[test]
fn doctor_json_reports_instructions_block_check_shape() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());

    // Healthy: instructions.block is ok, not fixed.
    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "healthy install should exit 0: {json}");
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "instructions.block")
        .expect("instructions.block check present");
    assert_eq!(check["status"], "ok");
    assert_eq!(check["fixed"], serde_json::json!(false));

    // Drift the block -> the json check becomes a problem and ok aggregates to false.
    let claude = dir.path().join("CLAUDE.md");
    let content = fs::read_to_string(&claude).unwrap();
    fs::write(
        &claude,
        content.replace("code archaeology", "DRIFTED HEADER"),
    )
    .unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 1, "a drifted block must fail the json gate: {json}");
    assert_eq!(
        json["ok"], false,
        "an instructions-driven problem must make the run not ok: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "instructions.block")
        .expect("instructions.block check present");
    assert_eq!(check["status"], "problem");

    // --fix repairs it: status becomes fixed.
    let (code, json) = doctor_json(dir.path(), true);
    assert_eq!(code, 0, "--fix json should repair and exit 0: {json}");
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "instructions.block")
        .expect("instructions.block check present");
    assert_eq!(check["status"], "fixed");
    assert_eq!(check["fixed"], serde_json::json!(true));
}

#[test]
fn doctor_reports_published_ephemeral_port() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Simulate a live serve having published its port.
    let loomweave_dir = dir.path().join(".weft/loomweave");
    std::fs::create_dir_all(&loomweave_dir).unwrap();
    std::fs::write(loomweave_dir.join("ephemeral.port"), "9876\n").unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "{json}");
    let http = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "http.config")
        .expect("http.config check present");
    assert_eq!(http["status"], "ok");
    assert!(
        http["message"].as_str().unwrap_or("").contains("9876"),
        "http.config should report the published live port: {http}"
    );
}
