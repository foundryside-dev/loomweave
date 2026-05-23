//! Integration tests for the gap-register STO-01 cross-process advisory lock.
//!
//! Two scenarios:
//!
//! 1. `clarion analyze` refuses to run when another holder already owns the
//!    project lockfile. We simulate the "other holder" by acquiring the
//!    `fs2` exclusive lock from the test process itself, then spawning a
//!    `clarion analyze` subprocess against the same project root and
//!    asserting the subprocess exits non-zero with the expected error.
//!    This is deterministic — the test process retains the lock for the
//!    full lifetime of the spawned subprocess, so there is no race
//!    window.
//!
//! 2. Two concurrent `clarion analyze` subprocesses against the same
//!    project root: exactly one succeeds and exactly one fails with the
//!    "another clarion analyze or serve is in progress" message. This
//!    test is best-effort — if both processes happen to serialise
//!    completely (the first finishes before the second tries to
//!    acquire), the test re-tries up to a small bounded number of times.
//!    The first scenario is the authoritative contract test; this one is
//!    the realistic-workload smoke check.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command as StdCommand, Stdio};
use std::thread;

use assert_cmd::Command;
use fs2::FileExt;
use tempfile::TempDir;

/// Locate the `clarion-plugin-fixture` binary. Mirrors `wp2_e2e.rs`.
fn fixture_binary_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_clarion-plugin-fixture") {
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
        let candidate = target_dir.join(profile).join("clarion-plugin-fixture");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!(
        "clarion-plugin-fixture binary not found. \
         Run `cargo build --workspace` before running this test. \
         Searched: {}",
        target_dir.display()
    );
}

/// Set up a synthetic `$PATH` dir with the fixture plugin + manifest.
/// Mirrors `wp2_e2e.rs::setup_plugin_dir`.
fn setup_plugin_dir(fixture_bin: &PathBuf) -> TempDir {
    let plugin_dir = TempDir::new().expect("create plugin tempdir");
    let dest = plugin_dir.path().join("clarion-plugin-fixture");
    std::os::unix::fs::symlink(fixture_bin, &dest).expect("symlink clarion-plugin-fixture");
    let meta = fs::metadata(fixture_bin).expect("stat fixture binary");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "fixture binary must be executable"
    );
    let toml_src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("clarion-core")
        .join("tests")
        .join("fixtures")
        .join("plugin.toml");
    let toml_dest = plugin_dir.path().join("plugin.toml");
    fs::copy(&toml_src, &toml_dest).expect("copy plugin.toml");
    plugin_dir
}

/// Initialise a project directory with `clarion install` and write a source
/// file the fixture plugin will claim.
fn make_installed_project(plugin_path: &std::path::Path) -> TempDir {
    let project_dir = TempDir::new().expect("create project tempdir");
    Command::cargo_bin("clarion")
        .expect("clarion binary")
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();
    fs::write(
        project_dir.path().join("demo.mt"),
        b"widget demo.sample {}\n",
    )
    .expect("write demo.mt");
    let _ = plugin_path; // silence unused warning if test list changes
    project_dir
}

/// Scenario 1: while the test process holds the lock, `clarion analyze`
/// must fail fast with the expected error message and a non-zero exit.
#[test]
fn analyze_refuses_when_lock_already_held() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let project_dir = make_installed_project(plugin_dir.path());

    // Acquire the lock from the test process.
    let lock_path = project_dir.path().join(".clarion").join("clarion.lock");
    let lock_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open lockfile");
    FileExt::try_lock_exclusive(&lock_file).expect("acquire lock in test process");

    // While the lock is held, attempt `clarion analyze`.
    let new_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");
    let output = Command::cargo_bin("clarion")
        .expect("clarion binary")
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &new_path)
        .output()
        .expect("spawn clarion analyze");

    assert!(
        !output.status.success(),
        "clarion analyze must fail when lock is held; status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("another clarion analyze or serve is in progress"),
        "expected lock-conflict message in stderr; got: {stderr}"
    );
    assert!(
        stderr.contains("clarion.lock"),
        "stderr must name the lockfile path: {stderr}"
    );

    // Drop the lock; a subsequent analyze succeeds.
    FileExt::unlock(&lock_file).expect("release lock");
    drop(lock_file);

    Command::cargo_bin("clarion")
        .expect("clarion binary")
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &new_path)
        .assert()
        .success();
}

/// Scenario 2: race two concurrent `clarion analyze` subprocesses; exactly
/// one wins.
///
/// We use raw `std::process::Command` rather than `assert_cmd::Command` for
/// the spawn step because we need `spawn()` + `wait_with_output()` to run
/// them concurrently. The losing process MUST emit the lock-conflict
/// message on stderr; the winning process MUST exit 0.
#[test]
fn concurrent_analyze_processes_exactly_one_wins() {
    const RACE_ATTEMPTS: usize = 8;
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_plugin_dir(&fixture_bin);
    let project_dir = make_installed_project(plugin_dir.path());

    let new_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");

    // Resolve the clarion binary via assert_cmd so we use the same path
    // the other tests use (avoids cargo-target-dir resolution drift).
    let clarion_bin = assert_cmd::cargo::cargo_bin("clarion");
    assert!(
        clarion_bin.exists(),
        "clarion binary missing at {}",
        clarion_bin.display()
    );

    // Spawn two processes back-to-back from worker threads. The
    // back-to-back spawn ordering does not guarantee contention on its
    // own — analyze could finish before the second starts. We retry the
    // whole race up to RACE_ATTEMPTS times until at least one observed
    // case has one winner + one lock-conflict loser. With the fixture
    // plugin, the first analyze takes tens of milliseconds; the spawn
    // overhead of the second process is comparable, so contention is
    // typical on a warm cache.
    let mut saw_contended_race = false;
    let mut last_diagnostic = String::new();

    for attempt in 0..RACE_ATTEMPTS {
        // Clean up runs from any previous attempt so the test stays
        // idempotent. We leave `.clarion/` itself in place (the lock
        // path stays valid) and drop the database file. `clarion
        // install` re-creates the DB on next analyze? No — install is
        // separate. Instead, just leave the DB alone and let runs
        // accumulate; the test only cares about exit codes + stderr,
        // not run-row count.
        let _ = attempt;

        let project_for_a = project_dir.path().to_path_buf();
        let project_for_b = project_dir.path().to_path_buf();
        let bin_a = clarion_bin.clone();
        let bin_b = clarion_bin.clone();
        let path_a = new_path.clone();
        let path_b = new_path.clone();

        let handle_a = thread::spawn(move || {
            StdCommand::new(&bin_a)
                .args(["analyze"])
                .arg(&project_for_a)
                .env("PATH", &path_a)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn analyze A")
                .wait_with_output()
                .expect("wait analyze A")
        });
        // No deliberate delay between the two spawns; we want the
        // contention window to be as tight as the OS schedules.
        let handle_b = thread::spawn(move || {
            StdCommand::new(&bin_b)
                .args(["analyze"])
                .arg(&project_for_b)
                .env("PATH", &path_b)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn analyze B")
                .wait_with_output()
                .expect("wait analyze B")
        });
        let out_a = handle_a.join().expect("join A");
        let out_b = handle_b.join().expect("join B");

        let stderr_a = String::from_utf8_lossy(&out_a.stderr).into_owned();
        let stderr_b = String::from_utf8_lossy(&out_b.stderr).into_owned();
        let conflict_msg = "another clarion analyze or serve is in progress";
        let a_lost = !out_a.status.success() && stderr_a.contains(conflict_msg);
        let b_lost = !out_b.status.success() && stderr_b.contains(conflict_msg);
        let a_won = out_a.status.success();
        let b_won = out_b.status.success();

        last_diagnostic = format!(
            "attempt={attempt} a_status={:?} b_status={:?} \n--- stderr A ---\n{stderr_a}\n--- stderr B ---\n{stderr_b}",
            out_a.status, out_b.status
        );

        if (a_won && b_lost) || (b_won && a_lost) {
            saw_contended_race = true;
            break;
        }
        // Both succeeded (they fully serialised) — try again to get an
        // actual contention window. Crucially we never want to see
        // "both failed" or "one failed for a non-lock reason"; assert
        // that now so misconfiguration surfaces immediately.
        assert!(
            a_won || a_lost,
            "process A failed for a non-lock reason: {last_diagnostic}"
        );
        assert!(
            b_won || b_lost,
            "process B failed for a non-lock reason: {last_diagnostic}"
        );
    }

    assert!(
        saw_contended_race,
        "after {RACE_ATTEMPTS} attempts no contention was observed. \
         The lock is the load-bearing guarantee; if the race never \
         materialises here the test cannot prove the lock fired. \
         Last diagnostic: {last_diagnostic}"
    );
}
