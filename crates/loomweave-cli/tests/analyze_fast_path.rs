//! The no-indexed-changes fast path and hook-side coalescing
//! (clarion-78d75e45c9), driven through a real `loomweave analyze` subprocess
//! against a real git repository with the fixture plugin (extensions: `mt`).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::{env, fs};

use assert_cmd::Command;
use rusqlite::Connection;
use tempfile::TempDir;

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

fn fixture_binary_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_loomweave-fixture-plugin") {
        return PathBuf::from(path);
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root must exist");
    let target_dir =
        env::var("CARGO_TARGET_DIR").map_or_else(|_| workspace_root.join("target"), PathBuf::from);
    for profile in &["debug", "release"] {
        let candidate = target_dir.join(profile).join("loomweave-fixture-plugin");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!("loomweave-fixture-plugin binary not found; run `cargo build --workspace` first");
}

fn setup_plugin_dir(fixture_bin: &Path) -> TempDir {
    let plugin_dir = TempDir::new().expect("create plugin tempdir");
    let dest = plugin_dir.path().join("loomweave-plugin-fixture");
    std::os::unix::fs::symlink(fixture_bin, &dest).expect("symlink loomweave-plugin-fixture");
    assert!(fs::metadata(fixture_bin).unwrap().permissions().mode() & 0o111 != 0);
    let toml_src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("loomweave-core/tests/fixtures/plugin.toml");
    fs::copy(&toml_src, plugin_dir.path().join("plugin.toml")).expect("copy plugin.toml");
    plugin_dir
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn head(dir: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A git project with one fixture-claimed source file, installed and committed.
fn setup_project(plugin_dir: &TempDir) -> (TempDir, std::ffi::OsString) {
    let project = TempDir::new().expect("create project tempdir");
    git(project.path(), &["init", "-q"]);
    fs::write(project.path().join("demo.mt"), b"widget demo.sample {}\n").unwrap();
    fs::write(project.path().join("README.md"), b"# demo\n").unwrap();
    loomweave_bin()
        .args(["install", "--path"])
        .arg(project.path())
        .assert()
        .success();
    git(project.path(), &["add", "-A"]);
    git(project.path(), &["commit", "-qm", "init"]);
    // Plugin dir first, then the real PATH: the hardened git wrapper clears
    // the environment and needs `git` resolvable.
    let inherited = env::var_os("PATH").unwrap_or_default();
    let path = env::join_paths(
        std::iter::once(plugin_dir.path().to_path_buf()).chain(env::split_paths(&inherited)),
    )
    .unwrap();
    (project, path)
}

fn analyze(project: &TempDir, path: &std::ffi::OsString) -> String {
    let out = loomweave_bin()
        .arg("analyze")
        .arg(project.path())
        .env("PATH", path)
        .assert()
        .success();
    String::from_utf8_lossy(&out.get_output().stdout).into_owned()
}

fn runs(project: &TempDir) -> Vec<(String, String, serde_json::Value)> {
    let conn =
        Connection::open(project.path().join(".weft/loomweave/loomweave.db")).expect("open db");
    let mut stmt = conn
        .prepare(
            "SELECT status, COALESCE(analyzed_at_commit, ''), stats FROM runs \
             ORDER BY started_at ASC, id ASC",
        )
        .unwrap();
    stmt.query_map([], |row| {
        let stats: String = row.get(2)?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            serde_json::from_str(&stats).unwrap_or(serde_json::Value::Null),
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn docs_only_commit_settles_on_the_fast_path_and_source_commit_does_not() {
    let plugin_dir = setup_plugin_dir(&fixture_binary_path());
    let (project, path) = setup_project(&plugin_dir);

    // Run 1: the real pipeline.
    let out = analyze(&project, &path);
    assert!(!out.contains("fast path"), "first run must walk: {out}");
    let first = runs(&project);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, "completed");
    assert_eq!(first[0].1, head(project.path()));
    assert!(first[0].2["fast_path"].is_null());

    // A docs-only commit: only the commit clock moved.
    fs::write(project.path().join("README.md"), b"# demo\n\nmore words\n").unwrap();
    git(project.path(), &["commit", "-qam", "docs"]);
    let docs_head = head(project.path());
    let out = analyze(&project, &path);
    assert!(out.contains("fast path: no indexed changes"), "{out}");
    let second = runs(&project);
    assert_eq!(second.len(), 2, "the fast path still records a run");
    let (recorded_status, at_commit, recorded) = &second[1];
    assert_eq!(recorded_status, "completed");
    assert_eq!(
        at_commit, &docs_head,
        "the run row is stamped at the new HEAD"
    );
    assert_eq!(recorded["fast_path"]["reason"], "no_indexed_changes");
    assert_eq!(recorded["fast_path"]["paths_changed"], 1);
    assert_eq!(recorded["entities_inserted"], 0);
    assert_eq!(
        recorded["classifier_coverage"], first[0].2["classifier_coverage"],
        "coverage is carried forward from the base run"
    );

    // A source commit: the fast path must stand aside.
    fs::write(
        project.path().join("demo.mt"),
        b"widget demo.sample {}\nwidget demo.other {}\n",
    )
    .unwrap();
    git(project.path(), &["commit", "-qam", "source"]);
    let out = analyze(&project, &path);
    assert!(
        !out.contains("fast path"),
        "a source change must walk: {out}"
    );
    let third = runs(&project);
    assert_eq!(third.len(), 3);
    assert!(third[2].2["fast_path"].is_null());
    assert_eq!(third[2].1, head(project.path()));

    // `--no-incremental` bypasses the fast path even for a docs-only commit.
    fs::write(project.path().join("README.md"), b"# demo again\n").unwrap();
    git(project.path(), &["commit", "-qam", "docs 2"]);
    let out = loomweave_bin()
        .args(["analyze", "--no-incremental"])
        .arg(project.path())
        .env("PATH", &path)
        .assert()
        .success();
    let out = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(!out.contains("fast path"), "{out}");
}

#[test]
fn config_sentinel_and_working_tree_edits_disable_the_fast_path() {
    let plugin_dir = setup_plugin_dir(&fixture_binary_path());
    let (project, path) = setup_project(&plugin_dir);
    analyze(&project, &path);

    // loomweave.yaml is an analyzer input, not source — still indexed scope.
    fs::write(project.path().join("loomweave.yaml"), b"# touched\n").unwrap();
    git(project.path(), &["add", "-A"]);
    git(project.path(), &["commit", "-qm", "config"]);
    let out = analyze(&project, &path);
    assert!(
        !out.contains("fast path"),
        "a config change must walk: {out}"
    );

    // Docs commit AND an unstaged edit to an indexed file: file drift wins.
    fs::write(project.path().join("README.md"), b"# edited\n").unwrap();
    git(project.path(), &["commit", "-qam", "docs"]);
    fs::write(
        project.path().join("demo.mt"),
        b"widget demo.sample {}\n// edited\n",
    )
    .unwrap();
    let out = analyze(&project, &path);
    assert!(
        !out.contains("fast path"),
        "in-place drift must walk: {out}"
    );
}

#[test]
fn a_pending_marker_left_for_this_run_is_consumed_not_drained() {
    let plugin_dir = setup_plugin_dir(&fixture_binary_path());
    let (project, path) = setup_project(&plugin_dir);
    // A request queued before we start is satisfied BY this run: exactly one
    // run, and the marker is gone afterwards.
    let marker = project.path().join(".weft/loomweave/loomweave.pending");
    fs::write(&marker, b"").unwrap();
    analyze(&project, &path);
    assert!(
        !marker.exists(),
        "the run consumes the queued request on lock acquire"
    );
    assert_eq!(runs(&project).len(), 1);
}
