//! Confined-deletion primitive tests (Task 2 of the worktree-index-isolation
//! feature; see `docs/superpowers/specs/2026-07-18-loomweave-worktree-indexes-design.md`).
//!
//! **Platform scope.** [`refuse_unsupported`] is portable and its test
//! (`unsupported_platform_reports_and_deletes_nothing`) runs on every
//! platform. Real confined deletion only exists on Linux (`openat2`); every
//! other test here exercises that mechanism end to end (grammar checks
//! included, for a coherent single suite) and is
//! `#[cfg(target_os = "linux")]`. CI's nextest leg runs Linux only (the
//! `rust-macos` job is clippy + build, no nextest), so this gating only
//! matters for a local macOS dev running `cargo nextest run` directly —
//! those tests simply won't be compiled in.
//!
//! The privileged bind-mount suite (`bind_mount_beneath_candidate_refuses_deletion`)
//! is additionally `#[ignore]`d; see its own doc comment for how to run it
//! and why it must fail loudly rather than skip when its precondition is
//! unmet.
//!
//! `cargo nextest run -p loomweave-cli worktree_confine` (the brief's Verify
//! command) does **not** select this file by itself: nextest's plain
//! positional filter matches on *test name*, not on the integration-test
//! binary id, and none of these test names contain the literal substring
//! `worktree_confine`. The working equivalent is
//! `cargo nextest run -p loomweave-cli -E 'binary(worktree_confine)'`.

use std::fs;

use loomweave_cli::worktree::confine::{
    DeleteOutcome, RefusalReason, WorktreesRoot, refuse_unsupported,
};
use tempfile::TempDir;

/// A syntactically valid `wt-[0-9a-f]{64}` name, built by repeating one hex
/// digit — deterministic and trivially distinct across tests that need more
/// than one.
fn valid_name(fill: char) -> String {
    assert!(fill.is_ascii_hexdigit() && fill.is_ascii_lowercase() || fill.is_ascii_digit());
    format!("wt-{}", fill.to_string().repeat(64))
}

/// Locate the `loomweave-worktree-confine-probe` binary built alongside
/// this test — same lookup convention as `duplicate_locator.rs`'s
/// `fixture_binary_path` for `loomweave-fixture-plugin`: prefer the
/// `CARGO_BIN_EXE_*` env var Cargo sets for a dev-dependency's bin target,
/// and fall back to scanning the workspace target directory when that
/// isn't set (nextest does not always propagate it).
#[cfg(target_os = "linux")]
fn confine_probe_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_loomweave-worktree-confine-probe") {
        return std::path::PathBuf::from(path);
    }

    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root must exist");

    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map_or_else(|_| workspace_root.join("target"), std::path::PathBuf::from);

    for profile in &["debug", "release"] {
        let candidate = target_dir
            .join(profile)
            .join("loomweave-worktree-confine-probe");
        if candidate.exists() {
            return candidate;
        }
    }

    panic!(
        "loomweave-worktree-confine-probe binary not found. \
         Run `cargo build --workspace` before running this test. \
         Searched: {}",
        target_dir.display()
    );
}

#[test]
fn unsupported_platform_reports_and_deletes_nothing() {
    // Exercises `refuse_unsupported` directly rather than
    // `WorktreesRoot::delete_worktree_store`: the latter's non-Linux arm is
    // behind `#[cfg(not(target_os = "linux"))]` and literally cannot be
    // compiled into a test binary built on this (Linux) machine. What *is*
    // portable, and what this test proves on every platform including this
    // one, is that the fallback function that arm delegates to reports
    // `UnsupportedPlatform` and leaves the filesystem untouched.
    let tmp = TempDir::new().expect("tempdir");
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir(&worktrees_dir).expect("mkdir worktrees");
    let name = valid_name('a');
    let candidate = worktrees_dir.join(&name);
    fs::create_dir(&candidate).expect("mkdir candidate");
    fs::write(candidate.join("loomweave.db"), b"stub").expect("write stub db");

    let outcome = refuse_unsupported(&name, "manual-test");

    assert_eq!(outcome, DeleteOutcome::UnsupportedPlatform);
    assert!(
        candidate.is_dir(),
        "the unsupported-platform path must delete nothing"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn symlinked_worktrees_component_refuses_deletion() {
    let tmp = TempDir::new().expect("tempdir");
    let real_dir = tmp.path().join("elsewhere");
    fs::create_dir(&real_dir).expect("mkdir real_dir");
    let candidate = real_dir.join(valid_name('a'));
    fs::create_dir(&candidate).expect("mkdir candidate");

    let worktrees_symlink = tmp.path().join("worktrees");
    std::os::unix::fs::symlink(&real_dir, &worktrees_symlink).expect("symlink worktrees/");

    let opened = WorktreesRoot::open(&worktrees_symlink);

    assert!(
        opened.is_err(),
        "opening a symlinked worktrees/ component must refuse, not follow it"
    );
    assert!(
        candidate.is_dir(),
        "the real directory reached through the symlink must be untouched"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn symlink_inside_candidate_refuses_deletion() {
    let tmp = TempDir::new().expect("tempdir");
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir(&worktrees_dir).expect("mkdir worktrees");
    let name = valid_name('b');
    let candidate = worktrees_dir.join(&name);
    fs::create_dir(&candidate).expect("mkdir candidate");
    fs::write(candidate.join("loomweave.db"), b"stub").expect("write stub db");
    let target = tmp.path().join("outside-target");
    fs::write(&target, b"do not touch").expect("write target");
    std::os::unix::fs::symlink(&target, candidate.join("evil-link")).expect("symlink inside");

    let root = WorktreesRoot::open(&worktrees_dir).expect("open worktrees/");
    let outcome = root.delete_worktree_store(&name, "test");

    assert_eq!(
        outcome,
        DeleteOutcome::Refused(RefusalReason::SymlinkEncountered {
            relative_path: format!("{name}/evil-link"),
        })
    );
    assert!(
        candidate.is_dir(),
        "the candidate must survive a refused deletion"
    );
    assert!(
        fs::read(&target).expect("read target") == b"do not touch",
        "the symlink target must be untouched"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn non_matching_directory_name_is_never_deletable() {
    let tmp = TempDir::new().expect("tempdir");
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir(&worktrees_dir).expect("mkdir worktrees");

    let bad_names: &[&str] = &[
        "wt-short",
        "not-a-worktree-store",
        "wt",
        "wt-",
        "WT-0000000000000000000000000000000000000000000000000000000000000",
        // Path-traversal-shaped names: the most obvious escape attempt
        // against a sweep scoped to `worktrees/`. Refused by the grammar
        // today (no `/`, and `..` isn't `[0-9a-f]{64}` under any prefix) —
        // asserted explicitly, not just implied, so a future grammar
        // loosening can't silently reopen this.
        "..",
        "../filigree",
        "../../filigree",
        "wt-/../../filigree",
    ];
    let too_short = format!("wt-{}", "a".repeat(63));
    let too_long = format!("wt-{}", "a".repeat(65));
    let uppercase_hex = format!("wt-{}A", "a".repeat(63));
    let non_hex = format!("wt-{}g", "a".repeat(63));
    let traversal_suffix = format!("wt-{}/../../filigree", "a".repeat(64));

    let mut all_bad: Vec<String> = bad_names.iter().map(|s| (*s).to_owned()).collect();
    all_bad.extend([
        too_short,
        too_long,
        uppercase_hex,
        non_hex,
        traversal_suffix,
    ]);

    let root = WorktreesRoot::open(&worktrees_dir).expect("open worktrees/");

    for bad in &all_bad {
        // Some of these are on-disk (below); others aren't, but the
        // primitive must refuse purely on the name, before ever touching
        // the filesystem.
        let outcome = root.delete_worktree_store(bad, "test");
        assert_eq!(
            outcome,
            DeleteOutcome::Refused(RefusalReason::NameDoesNotMatchGrammar),
            "expected a grammar refusal for {bad:?}, got {outcome:?}"
        );
    }

    // Prove it end to end: a malformed name that genuinely exists on disk
    // (created directly, bypassing the primitive) still survives.
    let existing_bad = worktrees_dir.join(&all_bad[0]);
    fs::create_dir(&existing_bad).expect("mkdir bad candidate");
    let outcome = root.delete_worktree_store(&all_bad[0], "test");
    assert_eq!(
        outcome,
        DeleteOutcome::Refused(RefusalReason::NameDoesNotMatchGrammar)
    );
    assert!(
        existing_bad.is_dir(),
        "a malformed-name directory must survive"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn sibling_weft_directories_are_unreachable() {
    let tmp = TempDir::new().expect("tempdir");
    let weft = tmp.path().join(".weft");
    let filigree = weft.join("filigree");
    let wardline = weft.join("wardline");
    fs::create_dir_all(&filigree).expect("mkdir filigree");
    fs::create_dir_all(&wardline).expect("mkdir wardline");
    fs::write(
        filigree.join("issues.db"),
        b"irreplaceable issue tracker state",
    )
    .expect("write filigree marker");
    fs::write(
        wardline.join("baselines.db"),
        b"irreplaceable trust baselines",
    )
    .expect("write wardline marker");

    let worktrees_dir = weft.join("loomweave").join("worktrees");
    fs::create_dir_all(&worktrees_dir).expect("mkdir worktrees");
    let name = valid_name('c');
    // A grammar-valid candidate name that is actually a symlink into a
    // sibling, non-regenerable store — exactly the shape a bug or a
    // malicious actor would need to exploit to reach `.weft/filigree` from
    // a sweep scoped to `worktrees/`.
    std::os::unix::fs::symlink(&filigree, worktrees_dir.join(&name))
        .expect("symlink candidate into filigree");

    let root = WorktreesRoot::open(&worktrees_dir).expect("open worktrees/");
    let outcome = root.delete_worktree_store(&name, "test");

    assert_eq!(
        outcome,
        DeleteOutcome::Refused(RefusalReason::SymlinkEncountered {
            relative_path: name.clone(),
        })
    );
    assert_eq!(
        fs::read(filigree.join("issues.db")).expect("read filigree marker"),
        b"irreplaceable issue tracker state"
    );
    assert_eq!(
        fs::read(wardline.join("baselines.db")).expect("read wardline marker"),
        b"irreplaceable trust baselines"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn deletion_is_rooted_at_pinned_handle_not_resolved_path() {
    let tmp = TempDir::new().expect("tempdir");
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir(&worktrees_dir).expect("mkdir worktrees");
    let name = valid_name('d');
    fs::create_dir(worktrees_dir.join(&name)).expect("mkdir candidate");

    let root = WorktreesRoot::open(&worktrees_dir).expect("open worktrees/ (pins the handle)");

    // After pinning, swap the string path out from under the handle: move
    // the real directory aside, then put a decoy in its place.
    let moved_aside = tmp.path().join("worktrees-moved-aside");
    fs::rename(&worktrees_dir, &moved_aside).expect("rename worktrees/ aside");
    fs::create_dir(&worktrees_dir).expect("mkdir decoy at the original path");
    let decoy_marker = worktrees_dir.join("decoy-marker");
    fs::write(&decoy_marker, b"should never be touched").expect("write decoy marker");

    let outcome = root.delete_worktree_store(&name, "test");

    assert_eq!(
        outcome,
        DeleteOutcome::Deleted,
        "deletion must follow the pinned handle, not the re-resolved path"
    );
    assert!(
        !moved_aside.join(&name).exists(),
        "the candidate under the ORIGINAL (moved-aside) directory must be gone"
    );
    assert!(
        decoy_marker.is_file(),
        "the decoy at the re-resolved string path must be untouched"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn deletion_logs_stable_id_and_reason() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("lock log buffer")
                .extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedBuf {
        type Writer = SharedBuf;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let buf = SharedBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();

    let tmp = TempDir::new().expect("tempdir");
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir(&worktrees_dir).expect("mkdir worktrees");
    let name = valid_name('e');
    fs::create_dir(worktrees_dir.join(&name)).expect("mkdir candidate");
    let root = WorktreesRoot::open(&worktrees_dir).expect("open worktrees/");

    let outcome = tracing::subscriber::with_default(subscriber, || {
        root.delete_worktree_store(&name, "worktree no longer registered")
    });

    assert_eq!(outcome, DeleteOutcome::Deleted);
    let log =
        String::from_utf8(buf.0.lock().expect("lock log buffer").clone()).expect("log is UTF-8");
    assert!(
        log.contains(&name),
        "log line must include the stable ID {name:?}: {log}"
    );
    assert!(
        log.contains("worktree no longer registered"),
        "log line must include the reason: {log}"
    );
}

/// Requires an unprivileged user + mount namespace
/// (`unshare(CLONE_NEWUSER | CLONE_NEWNS)`), available to unprivileged
/// processes on a standard `ubuntu-latest` runner but not universally (some
/// hardened environments disable it — see the `NAMESPACE_ERROR` handling
/// below). This is `#[ignore]`d for ordinary runs and driven explicitly
/// from CI's privileged-suite step:
///
/// ```sh
/// cargo nextest run -p loomweave-cli -p loomweave-worktree-confine-probe \
///     --run-ignored ignored-only -E 'test(bind_mount)'
/// ```
///
/// Pass **both** `-p` flags, not just `loomweave-cli`: scoping to
/// `loomweave-cli` alone was observed, while building this suite, to reuse
/// a stale `loomweave-worktree-confine-probe` binary after an edit to only
/// the probe crate — `CARGO_BIN_EXE_loomweave-worktree-confine-probe`
/// pointed at a real file, so the test didn't fail to *find* the probe, it
/// silently ran an *outdated* one. In CI this is a non-issue in practice
/// (the preceding `--workspace` nextest run already rebuilds everything),
/// but the exact command above is also what a local run reaches for, so it
/// is written to not have that footgun.
///
/// If namespace creation fails, this test **fails with a diagnostic naming
/// the missing capability** rather than skipping — a silently-skipped
/// confinement test is exactly the failure mode this whole suite exists to
/// rule out.
///
/// **Namespace strategy: a freshly-spawned helper binary, not in-process or
/// a re-exec'd copy of this test binary.** `unshare(CLONE_NEWUSER)` fails
/// with `EINVAL` on a multithreaded process, and empirically (verified
/// while building this suite — see the loomweave worktree-index Task 2
/// report) a `#[test]`-harness process is *always* multithreaded by the
/// time a test body runs: the harness itself spawns the worker thread that
/// calls it, and this holds even for a single filtered test run directly
/// via `cargo test`/`nextest` or the compiled binary's own CLI — re-exec'ing
/// *this same* test binary does not change that, since the re-exec'd copy
/// re-enters the identical harness. A freshly started, ordinary `fn main()`
/// binary has exactly one thread, so `loomweave-worktree-confine-probe`
/// (built as a `loomweave-cli` dev-dependency; see its crate docs) is what
/// actually calls `unshare`, sets up the bind mount, and runs the real
/// [`WorktreesRoot::delete_worktree_store`] check — this test only spawns
/// it, parses its output, and asserts.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "privileged: unprivileged user+mount namespaces; see doc comment for the CI invocation"]
fn bind_mount_beneath_candidate_refuses_deletion() {
    let tmp = TempDir::new().expect("tempdir");

    let output = std::process::Command::new(confine_probe_path())
        .arg(tmp.path())
        .output()
        .expect("spawn loomweave-worktree-confine-probe");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if let Some(error_line) = stdout
        .lines()
        .find_map(|l| l.strip_prefix("NAMESPACE_ERROR="))
    {
        let capability_hint = if error_line.contains("EPERM") || error_line.contains("EACCES") {
            "\n\nThis looks like a missing capability rather than a bug: check \
             `/proc/sys/kernel/unprivileged_userns_clone` (must be 1, if present), \
             `/proc/sys/user/max_user_namespaces` (must be > 0), and — on Ubuntu 23.10+ — \
             AppArmor's unprivileged-user-namespace restriction (`unshare(CLONE_NEWUSER)` can \
             succeed while a later `/proc/self/setgroups`/`uid_map`/`gid_map` write still fails \
             with EACCES under that restriction; this was observed empirically while building \
             this suite). This test fails loudly here rather than skipping, by design."
        } else if error_line.starts_with("unshare") && error_line.contains("EINVAL") {
            "\n\nEINVAL from unshare() almost always means the probe process was unexpectedly \
             multithreaded at the point of the call — that should not happen for a freshly \
             started `fn main()` binary; investigate loomweave-worktree-confine-probe."
        } else {
            ""
        };
        panic!(
            "namespace setup failed in the probe: {error_line}{capability_hint}\nstderr: {stderr}"
        );
    }

    assert!(
        output.status.success(),
        "probe exited with {:?}; stdout={stdout:?} stderr={stderr:?}",
        output.status
    );

    let field = |key: &str| -> String {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or_else(|| panic!("probe stdout missing {key}: {stdout:?}"))
            .to_owned()
    };
    let name = field("NAME=");
    let outcome = field("OUTCOME=");
    let marker_intact = field("MARKER_INTACT=");

    assert_eq!(
        outcome,
        format!(
            "Refused(MountBoundaryCrossed {{ relative_path: {:?} }})",
            format!("{name}/nested")
        ),
        "expected a mount-boundary refusal; probe stdout={stdout:?}"
    );
    assert_eq!(
        marker_intact, "true",
        "bind-mount source content must be untouched; probe stdout={stdout:?}"
    );
}
