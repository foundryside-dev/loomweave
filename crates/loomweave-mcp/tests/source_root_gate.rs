//! Unit tests for the source-root-missing gate decision (clarion review
//! finding): a transient stat error on the worktree source root must NOT be
//! reported as a permanently-gone root. Only a confirmed `Ok(false)` from
//! `Path::try_exists` (a NotFound-style resolution) counts as missing —
//! anything else lets the request proceed, since the gate is per-request and
//! self-heals once the stat hiccup clears.

use loomweave_mcp::source_root_confirmed_missing;

#[test]
fn present_root_is_not_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(!source_root_confirmed_missing(dir.path()));
}

#[test]
fn absent_root_is_confirmed_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gone = dir.path().join("removed-worktree");
    assert!(source_root_confirmed_missing(&gone));
}

/// A stat error (EACCES via an unreadable parent, standing in for any
/// transient failure such as ELOOP or an automount hiccup) must not be
/// treated as "the root is gone".
#[cfg(unix)]
#[test]
fn stat_error_is_not_missing() {
    use std::os::unix::fs::PermissionsExt;

    // Root bypasses permission checks (e.g. some CI containers), so probe
    // whether the 0o000 parent actually denies the stat and skip if not —
    // std has no geteuid, so the probe doubles as the privilege check.
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = dir.path().join("locked");
    let root = parent.join("worktree");
    std::fs::create_dir_all(&root).expect("create nested dirs");

    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000))
        .expect("chmod 000 parent");

    // If we can still stat through the 0o000 parent we are privileged
    // (root); the simulation cannot produce EACCES, so skip.
    let denied = root.try_exists().is_err();

    let result = source_root_confirmed_missing(&root);

    // Restore permissions so the tempdir can be cleaned up.
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
        .expect("restore parent perms");

    if !denied {
        eprintln!("skipping: running privileged, cannot simulate EACCES");
        return;
    }
    assert!(
        !result,
        "a stat error must not be reported as source-root-missing"
    );
}
