//! `loomweave doctor [--fix]` integration tests.
//!
//! Exercises the exit-code contract (healthy -> 0, any problem -> 1) and the
//! end-to-end `--fix` wiring across the three orientation surfaces. Per-surface
//! detection/merge correctness is unit-tested in the owning modules
//! (`skill_pack`, `hooks_settings`, `mcp_registration`).

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use rusqlite::Connection;

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

/// Materialise a minimal healthy `SQLite` DB at the canonical store path so
/// `check_loomweave_dir` reports healthy (not the absent warning). A freshly
/// opened `SQLite` file has `user_version = 0`, which is <= the current schema
/// version and is therefore accepted.
///
/// A real `.weft/loomweave/` (created by `install`) also carries the current
/// `.gitignore`, so this completes the store with one too — otherwise the
/// gitignore-drift check (`gitignore.current`) would add a spurious "missing"
/// warning to tests that build the store dir by hand. The canonical bytes are
/// generated from a throwaway real install rather than duplicated here (which
/// would itself drift — the exact failure the new check guards against).
fn write_healthy_db(root: &Path) {
    let store = root.join(".weft/loomweave");
    fs::create_dir_all(&store).unwrap();
    Connection::open(store.join("loomweave.db")).expect("create minimal SQLite DB");

    let scratch = tempfile::tempdir().unwrap();
    install(&["install", "--all"], scratch.path());
    let canonical = fs::read(scratch.path().join(".weft/loomweave/.gitignore"))
        .expect("install writes a canonical .gitignore");
    fs::write(store.join(".gitignore"), canonical).unwrap();
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
    // Materialise a healthy DB so the index health check reports ok rather than
    // the absent-DB warning, which would prevent "All orientation surfaces healthy."
    write_healthy_db(dir.path());

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
    // Materialise a healthy DB so the index health check reports ok rather than
    // the absent-DB warning; with the DB present, only the integration bindings
    // surface warns, keeping the "1 warning" count stable.
    write_healthy_db(dir.path());

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

    // Wardline reads no URL from any `wardline.yaml`, so --fix writes none.
    assert!(
        !dir.path().join("wardline.yaml").exists(),
        "doctor --fix must not write a dead wardline.yaml that Wardline never reads"
    );

    let expected_port = loomweave_federation::loomweave_port::deterministic_port(
        &dir.path().canonicalize().unwrap(),
    );
    let expected_loomweave_url = format!("http://127.0.0.1:{expected_port}");

    // Loomweave owns only its OWN `--loomweave-url`; it cedes the emit URL
    // (`--filigree-url`) to wardline's installer (weft emit incident 2026-06-10).
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["wardline"]["args"],
        serde_json::json!([
            "mcp",
            "--root",
            ".",
            "--loomweave-url",
            expected_loomweave_url
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

// ---------------------------------------------------------------------------
// Index DB health check tests (.weft/loomweave.schema)
// ---------------------------------------------------------------------------

/// (a) Absent DB → `.weft/loomweave.schema` is a warning (ok=true), gate passes.
///
/// A missing DB is a legitimate intermediate state (install-before-analyze), so
/// it must not fail the gate. The JSON path must set `ok: true`, and the text
/// path must exit 0 (warnings only, no problems).
#[test]
fn doctor_index_health_absent_db_is_warning_gate_passes() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // `check_sei_population_json` opens the DB with `Connection::open` which
    // creates it as a side-effect when absent. Remove any DB that install or a
    // prior doctor run may have materialised so this test exercises the
    // genuine absence path.
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    if db_path.exists() {
        fs::remove_file(&db_path).unwrap();
    }

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(
        code, 0,
        "absent index DB must not fail the gate (install-before-analyze is a \
         legitimate intermediate state): {json}"
    );
    assert_eq!(
        json["ok"], true,
        "absent index DB must leave ok=true: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "warning",
        ".weft/loomweave.schema must be a warning when DB is absent: {check}"
    );
    assert!(
        check["message"]
            .as_str()
            .unwrap_or("")
            .contains("loomweave install"),
        "warning message must suggest loomweave install + analyze: {check}"
    );

    // Text path: warnings-only → exit 0.
    // Re-delete the DB: doctor_json may have recreated it as a side-effect
    // of check_sei_population_json (which uses Connection::open, not read-only).
    if db_path.exists() {
        fs::remove_file(&db_path).unwrap();
    }
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 0,
        "absent index DB must not fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("⚠ no index"),
        "absent DB must surface as a text-path warning: stdout:\n{out}"
    );
}

/// (b) DB file present but not valid `SQLite` → `.weft/loomweave.schema` is a
/// problem (ok=false), gate fails.
///
/// A corrupt or non-`SQLite` file in the DB position must be surfaced as a gate
/// failure, not silently reported as healthy.
#[test]
fn doctor_index_health_corrupt_db_is_problem_gate_fails() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Write a non-SQLite file at the DB path — must NOT be zero-length (a 0-byte
    // file opens as a fresh db with user_version=0 and is healthy).
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    fs::write(&db_path, b"this is not a sqlite database").unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 1, "a corrupt index DB must fail the gate: {json}");
    assert_eq!(
        json["ok"], false,
        "a corrupt index DB must set ok=false: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "problem",
        ".weft/loomweave.schema must be a problem when DB is unreadable: {check}"
    );
    assert!(
        check["message"]
            .as_str()
            .unwrap_or("")
            .contains("unreadable"),
        "problem message must say the index is unreadable: {check}"
    );

    // Text path: problem → exit 1.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a corrupt index DB must fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("✗") && out.contains("unreadable"),
        "corrupt DB must surface as a text-path problem: stdout:\n{out}"
    );
}

/// (c) DB present, opens, but `user_version` > current → future-schema
/// problem (ok=false), message names the version numbers.
#[test]
fn doctor_index_health_future_schema_is_problem_with_version_in_message() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    let db_path = dir.path().join(".weft/loomweave/loomweave.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    // Create a valid SQLite file with user_version stamped to current+1.
    {
        let conn = Connection::open(&db_path).expect("create DB");
        // user_version is a 32-bit signed integer in SQLite; any value > current
        // triggers the future-schema guard. We avoid hardcoding a literal so the
        // test stays correct when CURRENT_SCHEMA_VERSION is bumped.
        conn.execute_batch("PRAGMA user_version = 99999;")
            .expect("set future user_version");
    }

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 1, "a future-schema DB must fail the gate: {json}");
    assert_eq!(
        json["ok"], false,
        "a future-schema DB must set ok=false: {json}"
    );
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "problem",
        ".weft/loomweave.schema must be a problem for a future-schema DB: {check}"
    );
    let msg = check["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("99999"),
        "problem message must name the found schema version (99999): {check}"
    );
    assert!(
        msg.contains("newer Loomweave build"),
        "problem message must mention 'newer Loomweave build': {check}"
    );

    // Text path: problem → exit 1.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a future-schema DB must fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("99999"),
        "text output must name the schema version (99999): stdout:\n{out}"
    );
}

/// (d) DB present, opens, version <= current → `.weft/loomweave.schema` is ok.
///
/// The check's specific status is verified via the JSON surface so we don't
/// couple to the global "All healthy" summary (which depends on plugin/llm state).
#[test]
fn doctor_index_health_healthy_db_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // A freshly opened SQLite file has user_version=0, which is <= current and
    // therefore accepted by verify_user_version.
    write_healthy_db(dir.path());

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "a healthy index DB must not fail the gate: {json}");
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ".weft/loomweave.schema")
        .expect(".weft/loomweave.schema check must be present");
    assert_eq!(
        check["status"], "ok",
        ".weft/loomweave.schema must be ok for a healthy DB: {check}"
    );
    assert_eq!(
        check["fixed"],
        serde_json::json!(false),
        "a healthy check is never marked fixed: {check}"
    );

    // Text path: no warning or problem for the index check → does not
    // contribute to exit-1.
    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 0,
        "a healthy index DB must not fail the text-path gate: stdout:\n{out}"
    );
    assert!(
        out.contains("✓") && out.contains("index DB present"),
        "healthy DB must surface as a text-path ok line: stdout:\n{out}"
    );
}

fn run_git(repo: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git runs")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A git-tracked runtime DB is a gate-failing problem: it mutates on every
/// analyze/scan, dirtying the work tree and blocking legis signing. `doctor`
/// must exit non-zero; `--fix` untracks it via `git rm --cached` and the project
/// is then healthy (exit 0), with the working-tree file preserved.
#[test]
fn doctor_flags_git_tracked_db_as_problem_and_fix_untracks_it() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    write_healthy_db(dir.path());
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "t@t"]);
    run_git(dir.path(), &["config", "user.name", "t"]);
    // `-f` overrides the installed .gitignore — the real scenario is a db that
    // was committed before ADR-005 was reversed.
    run_git(dir.path(), &["add", "-f", ".weft/loomweave/loomweave.db"]);

    let (code, out) = doctor(dir.path(), false);
    assert_eq!(
        code, 1,
        "a git-tracked db must fail the gate; stdout:\n{out}"
    );
    assert!(
        out.contains("loomweave.db is git-tracked"),
        "the tracked-db problem must be named; stdout:\n{out}"
    );

    let (fix_code, fix_out) = doctor(dir.path(), true);
    assert_eq!(
        fix_code, 0,
        "--fix untracks the db, then the project is healthy; stdout:\n{fix_out}"
    );
    assert!(
        fix_out.contains("git rm --cached"),
        "the --fix line must report the remedy; stdout:\n{fix_out}"
    );
    assert!(
        dir.path().join(".weft/loomweave/loomweave.db").is_file(),
        "git rm --cached must keep the working-tree db file"
    );
}
