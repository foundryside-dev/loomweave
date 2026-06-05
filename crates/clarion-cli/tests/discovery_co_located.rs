//! Proves the production discovery path finds a plugin co-located in the same
//! directory as the `clarion` binary even when that directory is NOT on $PATH —
//! the PyPI/venv install scenario (spec 2026-06-05-clarion-pypi-distribution).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn co_located_plugin_discovered_off_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();

    // Copy the built clarion binary into the staged bin/.
    let staged = bin.join("clarion");
    fs::copy(env!("CARGO_BIN_EXE_clarion"), &staged).unwrap();
    set_exec(&staged);

    // Sibling plugin executable + install-prefix manifest.
    let plugin_exe = bin.join("clarion-plugin-mocktest");
    fs::write(&plugin_exe, b"#!/bin/sh\nexit 0\n").unwrap();
    set_exec(&plugin_exe);
    let share = tmp.path().join("share/clarion/plugins/mocktest");
    fs::create_dir_all(&share).unwrap();
    fs::write(share.join("plugin.toml"), MOCK_MANIFEST).unwrap();

    // Run the staged binary with an EMPTY PATH so discovery can ONLY succeed via
    // the current_exe() level. Use a project dir so doctor has somewhere to look.
    let proj = tmp.path().join("proj");
    fs::create_dir_all(&proj).unwrap();
    let output = std::process::Command::new(&staged)
        .args(["doctor", "--format", "json", "--path"])
        .arg(&proj)
        .current_dir(&proj)
        .env("PATH", "")
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the doctor JSON and assert the plugin.availability check is "ok"
    // and names the discovered mock plugin.
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --format json not parseable: {e}\nstdout:\n{stdout}"));
    let checks = report["checks"].as_array().expect("checks array");
    let plugin_check = checks
        .iter()
        .find(|c| c["id"] == "plugin.availability")
        .expect("plugin.availability check present");
    assert_eq!(
        plugin_check["status"], "ok",
        "co-located plugin should be discovered off-PATH; report:\n{stdout}"
    );
    assert!(
        plugin_check["message"]
            .as_str()
            .unwrap_or("")
            .contains("mocktest"),
        "plugin.availability message should name the discovered plugin; got: {plugin_check}"
    );
}

fn set_exec(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

const MOCK_MANIFEST: &str = r#"[plugin]
name = "clarion-plugin-mocktest"
plugin_id = "mocktest"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-mocktest"
language = "mocktest"
extensions = ["mt"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["function"]
edge_kinds = ["calls"]
rule_id_prefix = "CLA-MT-"
ontology_version = "0.1.0"
"#;
