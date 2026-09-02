//! Cross-language conformance for `tracked_state` (`fixtures/git_tracked_paths.json`).
//! The Python twin is `plugins/python/tests/test_git_trust.py`.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::Command;

use loomweave_core::{TrackedState, tracked_state};

#[derive(serde::Deserialize)]
struct Case {
    description: String,
    #[serde(default)]
    layout: Vec<serde_json::Value>,
    #[serde(default)]
    git: Vec<String>,
    query: String,
    expected: String,
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        // Hermetic: the operator's ~/.gitconfig and /etc/gitconfig must not
        // shape a fixture repository (init.defaultBranch, core.hooksPath,
        // core.excludesFile, a global .gitignore, commit.gpgsign …). Author
        // and committer identity is supplied explicitly below, so nulling the
        // global file cannot break `git commit`.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

fn build(root: &Path, case: &Case) {
    use std::os::unix::fs::PermissionsExt;
    for entry in &case.layout {
        if let Some(rel) = entry.get("file").and_then(|v| v.as_str()) {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = entry
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("#!/bin/sh\nexit 0\n");
            std::fs::write(&path, body).unwrap();
            let mode = u32::from_str_radix(
                entry.get("mode").and_then(|v| v.as_str()).unwrap_or("0644"),
                8,
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        } else if let Some(rel) = entry.get("dir").and_then(|v| v.as_str()) {
            std::fs::create_dir_all(root.join(rel)).unwrap();
        } else if let Some(rel) = entry.get("symlink").and_then(|v| v.as_str()) {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let target = entry["target"].as_str().unwrap();
            let target: PathBuf = if Path::new(target).is_absolute() {
                target.into()
            } else {
                root.join(target)
            };
            std::os::unix::fs::symlink(target, path).unwrap();
        }
    }
    for step in &case.git {
        match step.as_str() {
            "init" => git(root, &["init", "-q"]),
            "commit" => git(root, &["commit", "-q", "--allow-empty", "-m", "fixture"]),
            other => {
                let parts: Vec<&str> = other.split(' ').collect();
                git(root, &parts);
            }
        }
    }
}

#[test]
fn tracked_state_matches_the_shared_conformance_vectors() {
    if Command::new("git").arg("--version").output().is_err() {
        return; // git unavailable — the primitive's contract is untestable here.
    }
    let raw = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/git_tracked_paths.json"),
    )
    .unwrap();
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    assert!(
        !cases.is_empty(),
        "the conformance fixture must not be empty"
    );
    for case in &cases {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), case);
        let state = tracked_state(dir.path(), Path::new(&case.query));
        assert_eq!(state.label(), case.expected, "{}", case.description);
        assert!(
            !matches!(state, TrackedState::Unknown(_)),
            "{}: {state:?}",
            case.description
        );
    }
}
