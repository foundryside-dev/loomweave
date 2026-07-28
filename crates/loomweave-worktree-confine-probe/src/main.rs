//! Privileged helper for `bind_mount_beneath_candidate_refuses_deletion`
//! (`loomweave-cli/tests/worktree_confine.rs`).
//!
//! `unshare(CLONE_NEWUSER)` fails with `EINVAL` on a multithreaded process,
//! and every `#[test]`-harness process is multithreaded by the time a test
//! body runs (the harness itself spawns the worker thread that calls it) —
//! confirmed empirically while building this suite, not merely suspected.
//! A freshly started, ordinary `fn main()` binary has exactly one thread,
//! so this binary — not the test process itself — does the unprivileged
//! user+mount namespace setup, the bind mount, and the real confined-delete
//! check, then reports the result on stdout for the test to parse and
//! assert on.
//!
//! Usage: `loomweave-worktree-confine-probe <scratch-dir>`, where
//! `<scratch-dir>` is an empty directory the caller owns (normally a
//! `tempfile::TempDir`). Output is one `KEY=VALUE` pair per line:
//!
//! - On failure to acquire or configure the namespace:
//!   `NAMESPACE_ERROR=<EINVAL|EPERM|step-name: message>`, exit code 1. Every
//!   step from `unshare` itself through the final `mount_change` is folded
//!   into this one failure mode — on a real runner without full
//!   unprivileged-userns support, the failure can surface at any of them
//!   (empirically, this codebase's own dev environment fails one step later
//!   than a bare `unshare()` call would predict: `AppArmor`'s unprivileged
//!   user-namespace restriction — present by default on Ubuntu 23.10+ — lets
//!   `unshare()` itself succeed but then rejects the `/proc/self/setgroups`
//!   write).
//! - On success: `NAME=<candidate name>`, `OUTCOME=<Debug of DeleteOutcome>`,
//!   `MARKER_INTACT=<true|false>`, exit code 0. Exit code 0 means only "ran
//!   to completion" — the caller still inspects `OUTCOME` and
//!   `MARKER_INTACT` to decide pass/fail.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use loomweave_cli::worktree::confine::WorktreesRoot;
use rustix::io::Errno;
use rustix::mount::{MountPropagationFlags, UnmountFlags, mount_bind, mount_change, unmount};
use rustix::thread::UnshareFlags;

/// `rustix::thread::unshare` is deprecated in favor of `unshare_unsafe`
/// because `UnshareFlags::FILES` can leave threads observing inconsistent
/// file-descriptor tables. This binary passes only `NEWUSER | NEWNS`,
/// neither of which touches file-descriptor-table sharing, and — being a
/// single-threaded `fn main()` with no other threads ever spawned — has no
/// other thread that could observe such an inconsistency regardless. Using
/// the deprecated *safe* wrapper here, rather than `unshare_unsafe`, keeps
/// this workspace's `unsafe_code = "deny"` policy at its single documented
/// exception (`plugin/host.rs`'s `pre_exec`).
#[allow(deprecated)]
fn unshare_user_and_mount_namespace(flags: UnshareFlags) -> Result<(), Errno> {
    rustix::thread::unshare(flags)
}

/// Every step of acquiring and configuring the namespace, folded into one
/// fallible sequence: `unshare` itself, the three `/proc/self/*` writes,
/// and the final `mount_change`. On some runners the *first* failure isn't
/// `unshare()` — see the module docs — so every step here is reported
/// identically, tagged with which one failed.
fn setup_namespace() -> Result<(), (&'static str, String)> {
    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();

    unshare_user_and_mount_namespace(UnshareFlags::NEWUSER | UnshareFlags::NEWNS)
        .map_err(|e| ("unshare(CLONE_NEWUSER|CLONE_NEWNS)", classify_errno(e)))?;
    fs::write("/proc/self/setgroups", b"deny")
        .map_err(|e| ("write /proc/self/setgroups", classify_io_error(&e)))?;
    fs::write("/proc/self/uid_map", format!("0 {uid} 1").as_bytes())
        .map_err(|e| ("write /proc/self/uid_map", classify_io_error(&e)))?;
    fs::write("/proc/self/gid_map", format!("0 {gid} 1").as_bytes())
        .map_err(|e| ("write /proc/self/gid_map", classify_io_error(&e)))?;
    // Make this mount namespace's root private+recursive before mounting
    // anything, so the bind mount below never propagates back to the
    // original namespace.
    mount_change(
        "/",
        MountPropagationFlags::REC | MountPropagationFlags::PRIVATE,
    )
    .map_err(|e| ("mount_change(/, private+rec)", classify_errno(e)))?;
    Ok(())
}

fn classify_errno(errno: Errno) -> String {
    match errno {
        Errno::INVAL => "EINVAL".to_owned(),
        Errno::PERM | Errno::ACCESS => "EPERM".to_owned(),
        other => format!("OTHER:{other}"),
    }
}

/// Same classification as [`classify_errno`], for the `std::io::Error` the
/// `/proc/self/*` writes return (`fs::write`, not a raw `rustix` call).
/// `EACCES` is folded into the same `"EPERM"` tag as `EPERM` itself: both
/// mean "this process lacks a capability it needs," which is exactly the
/// distinction the caller's diagnostic cares about — not which specific
/// errno the kernel or an LSM (e.g. `AppArmor`'s unprivileged-userns
/// restriction) happened to return.
fn classify_io_error(err: &std::io::Error) -> String {
    match err.raw_os_error() {
        Some(raw) => classify_errno(Errno::from_raw_os_error(raw)),
        None => format!("OTHER:{err}"),
    }
}

fn main() -> ExitCode {
    let Some(scratch_arg) = std::env::args().nth(1) else {
        eprintln!("usage: loomweave-worktree-confine-probe <scratch-dir>");
        return ExitCode::FAILURE;
    };
    let scratch = PathBuf::from(scratch_arg);

    if let Err((step, detail)) = setup_namespace() {
        println!("NAMESPACE_ERROR={step}: {detail}");
        return ExitCode::FAILURE;
    }

    let worktrees_dir = scratch.join("worktrees");
    fs::create_dir(&worktrees_dir).expect("mkdir worktrees");
    let name = format!("wt-{}", "f".repeat(64));
    let candidate = worktrees_dir.join(&name);
    fs::create_dir(&candidate).expect("mkdir candidate");
    let nested = candidate.join("nested");
    fs::create_dir(&nested).expect("mkdir nested");

    let bind_source = scratch.join("bind-source");
    fs::create_dir(&bind_source).expect("mkdir bind_source");
    fs::write(bind_source.join("marker"), b"do not touch").expect("write bind_source marker");

    mount_bind(&bind_source, &nested).expect("bind mount beneath candidate");

    let root = WorktreesRoot::open(&worktrees_dir).expect("open worktrees/");
    let outcome = root.delete_worktree_store(&name, "test");

    let _ = unmount(&nested, UnmountFlags::DETACH);

    let marker_intact =
        fs::read(bind_source.join("marker")).is_ok_and(|contents| contents == b"do not touch");

    println!("NAME={name}");
    println!("OUTCOME={outcome:?}");
    println!("MARKER_INTACT={marker_intact}");
    ExitCode::SUCCESS
}
