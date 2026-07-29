//! Cleanup-sweep policy tests (Task 6 of the worktree-index-isolation
//! feature; see
//! `docs/superpowers/specs/2026-07-18-loomweave-worktree-indexes-design.md`,
//! "Cleanup", and the task brief's two review-critical GREEN
//! specifications: report-only under an active `store_dir` override, and
//! deriving a registered worktree's stable ID from the common Git
//! directory's own `worktrees/` admin entries rather than `git worktree
//! list`).
//!
//! These tests call [`sweep_worktree_stores`] (and, once,
//! [`sweep_best_effort`]) directly against real `git` fixtures — no `git
//! worktree list --porcelain` parsing lives in this module or in
//! production code for this feature, per the brief.
//!
//! **Platform scope.** Every test here only ever asserts that a candidate
//! *survives* a sweep, except [`unregistered_store_is_deleted`], which
//! requires an actual deletion to succeed — the confined-deletion
//! primitive's real mechanism (`openat2`) is Linux-only (see
//! `tests/worktree_confine.rs`), so only that one test is
//! `#[cfg(target_os = "linux")]`. Every other test's assertion ("nothing
//! was deleted") holds on every platform, including the non-Linux
//! `UnsupportedPlatform` path.
//!
//! `cargo nextest run -p loomweave-cli worktree_sweep` (the brief's Verify
//! command) does **not** select this file by itself, same caveat as
//! `worktree_confine.rs`: nextest's plain positional filter matches on test
//! name, not integration-test binary id, and none of these test names
//! contain the literal substring `worktree_sweep`. The working equivalent is
//! `cargo nextest run -p loomweave-cli -E 'binary(worktree_sweep)'`.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use loomweave_cli::worktree::store::{StoreOutcome, ensure_isolated_store};
use loomweave_cli::worktree::sweep::{SweepOutcome, sweep_best_effort, sweep_worktree_stores};
use loomweave_core::worktree::{WorktreeContext, WorktreeKind};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?} in {}: {e}", dir.display()));
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    fs::write(dir.join("f.txt"), "hi\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "init"]);
}

fn add_worktree(repo: &Path, linked: &Path, branch: &str) {
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            linked.to_str().expect("utf-8 path"),
        ],
    );
}

/// A grammar-valid `wt-[0-9a-f]{64}` name that does not correspond to any
/// real Git administrative identity — deterministic and distinct across
/// tests via `fill`.
fn fake_candidate(fill: char) -> String {
    assert!(fill.is_ascii_hexdigit() && fill.is_ascii_lowercase());
    format!("wt-{}", fill.to_string().repeat(64))
}

#[test]
#[cfg(target_os = "linux")]
fn unregistered_store_is_deleted() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");

    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let name = fake_candidate('a');
    let candidate = worktrees_dir.join(&name);
    fs::create_dir(&candidate).unwrap();

    let outcome = sweep_worktree_stores(&ctx);

    assert_eq!(
        outcome,
        SweepOutcome::Completed {
            deleted: vec![name],
            preserved: vec![],
        }
    );
    assert!(!candidate.exists(), "an unregistered store must be deleted");
}

#[test]
fn registered_store_is_preserved() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let linked = tmp.path().join("linked");
    add_worktree(&repo, &linked, "feature");

    let linked_ctx = WorktreeContext::resolve(&linked).expect("resolves as Linked");
    let stable_id = linked_ctx
        .stable_id
        .clone()
        .expect("a linked worktree context always carries a stable_id");

    let main_ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
    let worktrees_dir = main_ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let candidate = worktrees_dir.join(&stable_id);
    fs::create_dir(&candidate).unwrap();

    let outcome = sweep_worktree_stores(&main_ctx);

    match outcome {
        SweepOutcome::Completed { deleted, preserved } => {
            assert!(deleted.is_empty(), "nothing should be deleted: {deleted:?}");
            assert_eq!(preserved, vec![stable_id]);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert!(candidate.is_dir(), "a registered store must be preserved");
}

/// Pins the ordering invariant that today holds only by construction: `git
/// worktree add` registers the admin entry before any analyze/store-creation
/// can target it, so a store built by the real Task 3 pipeline
/// (`ensure_isolated_store`) is registered by the time a sweep can ever see
/// it. A refactor that created store directories before registration would
/// silently turn bootstrap into a sweep-then-rebuild loop.
#[test]
fn store_created_for_a_just_registered_worktree_is_preserved() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let linked = tmp.path().join("linked");
    add_worktree(&repo, &linked, "feature");

    let linked_ctx = WorktreeContext::resolve(&linked).expect("resolves as Linked");
    // `ensure_isolated_store`'s own contract: the primary's store must
    // already exist (normally `install`'s job).
    fs::create_dir_all(&linked_ctx.repository_store).unwrap();
    let store_outcome = ensure_isolated_store(&linked_ctx).expect("ensure isolated store");
    assert_eq!(store_outcome, StoreOutcome::Created);

    let main_ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
    let outcome = sweep_worktree_stores(&main_ctx);

    let stable_id = linked_ctx.stable_id.clone().unwrap();
    match outcome {
        SweepOutcome::Completed { deleted, preserved } => {
            assert!(deleted.is_empty(), "nothing should be deleted: {deleted:?}");
            assert_eq!(preserved, vec![stable_id]);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert!(
        linked_ctx.effective_store.join("metadata.json").is_file(),
        "the just-created store must survive the sweep"
    );
}

#[test]
fn git_locked_worktree_is_preserved() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let linked = tmp.path().join("linked");
    add_worktree(&repo, &linked, "feature");
    git(&repo, &["worktree", "lock", linked.to_str().unwrap()]);

    let linked_ctx = WorktreeContext::resolve(&linked).expect("resolves as Linked");
    let stable_id = linked_ctx.stable_id.clone().unwrap();

    let main_ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
    let worktrees_dir = main_ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let candidate = worktrees_dir.join(&stable_id);
    fs::create_dir(&candidate).unwrap();

    let outcome = sweep_worktree_stores(&main_ctx);

    match outcome {
        SweepOutcome::Completed { deleted, preserved } => {
            assert!(deleted.is_empty(), "nothing should be deleted: {deleted:?}");
            assert_eq!(preserved, vec![stable_id]);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert!(
        candidate.is_dir(),
        "a git-locked worktree's store must be preserved (its admin entry \
         is unaffected by the lock marker)"
    );
}

#[test]
fn main_store_is_never_a_candidate() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");

    fs::create_dir_all(&ctx.repository_store).unwrap();
    let main_db = ctx.repository_store.join("loomweave.db");
    fs::write(&main_db, b"main store contents").unwrap();

    // An unregistered candidate coexists under `worktrees/`, so the sweep
    // has real work to do at that level while this test's real assertion is
    // about the sibling main-store file one level up.
    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    fs::create_dir(worktrees_dir.join(fake_candidate('b'))).unwrap();

    let outcome = sweep_worktree_stores(&ctx);

    // Prove the sweep actually reached the deletion stage against a real
    // (unregistered) candidate, not that it happened to no-op — an aborted
    // or short-circuited sweep would trivially leave `main_db` untouched
    // too, which would make this assertion vacuous.
    assert!(
        matches!(outcome, SweepOutcome::Completed { .. }),
        "expected the sweep to run to completion, got {outcome:?}"
    );
    assert_eq!(
        fs::read(&main_db).unwrap(),
        b"main store contents",
        "the main checkout's own store must never be a sweep candidate"
    );
}

#[test]
fn git_enumeration_failure_deletes_nothing() {
    let tmp = TempDir::new().unwrap();
    let non_git_root = tmp.path().join("not-a-repo");
    fs::create_dir_all(&non_git_root).unwrap();

    let ctx = WorktreeContext::resolve(&non_git_root).expect("resolves");
    assert_eq!(
        ctx.kind,
        WorktreeKind::Standalone,
        "sanity: a plain non-Git directory must resolve as Standalone"
    );

    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let candidate = worktrees_dir.join(fake_candidate('c'));
    fs::create_dir(&candidate).unwrap();

    let outcome = sweep_worktree_stores(&ctx);

    assert_eq!(outcome, SweepOutcome::GitCommonDirUnresolvable);
    assert!(
        candidate.is_dir(),
        "a Git enumeration failure must delete nothing"
    );
}

/// True when the filesystem actually enforces directory read permissions
/// for this process. `false` when running as root (permission checks are
/// bypassed), in which case the permission-based failure injection below
/// cannot actually fail and must be skipped rather than misreported as a
/// pass — mirrors `skill_pack.rs`'s `perms_enforced` (clarion-86f4614c0b).
#[cfg(unix)]
fn perms_enforced() -> bool {
    use std::os::unix::fs::PermissionsExt;
    let probe = TempDir::new().unwrap();
    let ro = probe.path().join("ro");
    fs::create_dir(&ro).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o000)).unwrap();
    let blocked = fs::read_dir(&ro).is_err();
    // Restore before `probe` drops, so its own cleanup can recurse in.
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    blocked
}

#[cfg(unix)]
#[test]
fn admin_dir_enumeration_failure_deletes_nothing() {
    use std::os::unix::fs::PermissionsExt;

    if !perms_enforced() {
        eprintln!("skipping: directory permissions not enforced (running as root?)");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let linked = tmp.path().join("linked");
    add_worktree(&repo, &linked, "feature");

    let ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let candidate = worktrees_dir.join(fake_candidate('d'));
    fs::create_dir(&candidate).unwrap();

    let admin_worktrees_dir = repo.join(".git").join("worktrees");
    assert!(
        admin_worktrees_dir.is_dir(),
        "sanity: `git worktree add` creates .git/worktrees/"
    );
    fs::set_permissions(&admin_worktrees_dir, fs::Permissions::from_mode(0o000)).unwrap();

    let outcome = sweep_worktree_stores(&ctx);

    // Restore before any assertion below can panic and skip the tempdir's
    // own recursive cleanup.
    fs::set_permissions(&admin_worktrees_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(outcome, SweepOutcome::AdminDirUnreadable);
    assert!(
        candidate.is_dir(),
        "an unreadable admin directory must delete nothing"
    );
}

/// The review-critical case (task brief): an absolute `[loomweave].store_dir`
/// override is not scoped to this repository, so two repositories sharing
/// one override resolve to the same `<repository-store>` — this
/// repository's registered-worktree set must never authorize deleting
/// another repository's `wt-*` stores sitting in that shared namespace.
#[test]
fn override_store_dir_makes_sweep_report_only() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);

    let override_dir = tmp.path().join("shared-override-store");
    fs::write(
        repo.join("weft.toml"),
        format!("[loomweave]\nstore_dir = \"{}\"\n", override_dir.display()),
    )
    .unwrap();

    let ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
    assert_eq!(
        ctx.repository_store, override_dir,
        "sanity: the override must actually apply"
    );

    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let name = fake_candidate('e');
    let candidate = worktrees_dir.join(&name);
    fs::create_dir(&candidate).unwrap();

    let outcome = sweep_worktree_stores(&ctx);

    assert_eq!(
        outcome,
        SweepOutcome::ReportOnly {
            would_delete: vec![name],
        }
    );
    assert!(
        candidate.is_dir(),
        "report-only mode under an active override must delete nothing"
    );
}

#[test]
fn override_sweep_recognizes_this_repositorys_registered_store() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let override_dir = tmp.path().join("shared-override-store");
    fs::write(
        repo.join("weft.toml"),
        format!("[loomweave]\nstore_dir = \"{}\"\n", override_dir.display()),
    )
    .unwrap();
    let linked = tmp.path().join("linked");
    add_worktree(&repo, &linked, "feature");

    let linked_ctx = WorktreeContext::resolve(&linked).expect("resolves as Linked");
    fs::create_dir_all(&linked_ctx.repository_store).unwrap();
    ensure_isolated_store(&linked_ctx).expect("create qualified isolated store");
    let stable_id = linked_ctx.stable_id.clone().unwrap();

    let main_ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
    let outcome = sweep_worktree_stores(&main_ctx);

    assert_eq!(
        outcome,
        SweepOutcome::ReportOnly {
            would_delete: vec![],
        },
        "a live override-backed store must use the same project-qualified ID in both context resolution and registration enumeration"
    );
    assert!(
        override_dir.join("worktrees").join(stable_id).is_dir(),
        "report-only sweep must preserve the live store"
    );
}

/// clarion-a93b43923e: a store path that reaches through a symlink is the
/// symlink analogue of the shared `store_dir` override — two repositories
/// whose `.weft/loomweave` link to one shared directory would let repo A's
/// registered set authorize deleting repo B's live stores, and unlike the
/// `weft.toml` route there is no override signal to key on. Such a store
/// sweeps report-only.
#[test]
fn symlinked_store_prefix_makes_sweep_report_only() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);

    // Relocate the store behind a symlink: `.weft/loomweave -> ../shared`.
    let shared = tmp.path().join("shared-store");
    fs::create_dir_all(&shared).unwrap();
    let weft = repo.join(".weft");
    fs::create_dir_all(&weft).unwrap();
    std::os::unix::fs::symlink(&shared, weft.join("loomweave")).unwrap();

    let ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
    assert!(
        ctx.repository_store.starts_with(&repo),
        "sanity: the literal store path stays under the repo: {}",
        ctx.repository_store.display()
    );

    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let name = fake_candidate('c');
    let candidate = worktrees_dir.join(&name);
    fs::create_dir(&candidate).unwrap();

    let outcome = sweep_worktree_stores(&ctx);

    assert_eq!(
        outcome,
        SweepOutcome::ReportOnly {
            would_delete: vec![name],
        }
    );
    assert!(
        candidate.is_dir(),
        "a symlink-reached store must never be delete-swept"
    );
}

/// The override decision must be the state the context was *resolved* under,
/// not a fresh config read at sweep time: `analyze` resolves its context at
/// start but sweeps at the end of a 20–30 minute run, and an override
/// removed from `weft.toml` inside that window must not flip the sweep from
/// report-only into delete mode against a store that may still be shared
/// with another repository (clarion-306ed41ce3).
#[test]
fn override_removed_after_resolve_stays_report_only() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);

    let override_dir = tmp.path().join("shared-override-store");
    let weft_toml = repo.join("weft.toml");
    fs::write(
        &weft_toml,
        format!("[loomweave]\nstore_dir = \"{}\"\n", override_dir.display()),
    )
    .unwrap();

    let ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
    assert_eq!(
        ctx.repository_store, override_dir,
        "sanity: the override must actually apply"
    );

    // The operator removes the override while the analyze that resolved
    // `ctx` is still in flight.
    fs::remove_file(&weft_toml).unwrap();

    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let name = fake_candidate('b');
    let candidate = worktrees_dir.join(&name);
    fs::create_dir(&candidate).unwrap();

    let outcome = sweep_worktree_stores(&ctx);

    assert_eq!(
        outcome,
        SweepOutcome::ReportOnly {
            would_delete: vec![name],
        }
    );
    assert!(
        candidate.is_dir(),
        "the sweep must honor the override state its context was resolved under"
    );
}

#[test]
fn held_gc_lock_skips_the_sweep() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");

    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let candidate = worktrees_dir.join(fake_candidate('f'));
    fs::create_dir(&candidate).unwrap();

    let lock_path = worktrees_dir.join("gc.lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open gc.lock");
    fs2::FileExt::try_lock_exclusive(&lock_file).expect("acquire gc.lock in the test");

    let outcome = sweep_worktree_stores(&ctx);

    assert_eq!(outcome, SweepOutcome::GcLockHeld);
    assert!(
        candidate.is_dir(),
        "the sweep must delete nothing while gc.lock is held"
    );

    drop(lock_file);
}

/// `sweep_best_effort` — the exact entry point `serve.rs`/`analyze.rs`
/// call — returns `()`, so there is nothing for either caller to
/// propagate as its own failure even when the sweep itself aborts
/// internally (here: no Git repository at all).
#[test]
fn sweep_failure_does_not_fail_serve_or_analyze() {
    let tmp = TempDir::new().unwrap();
    let non_git_root = tmp.path().join("not-a-repo");
    fs::create_dir_all(&non_git_root).unwrap();
    let ctx = WorktreeContext::resolve(&non_git_root).expect("resolves");

    let worktrees_dir = ctx.repository_store.join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();
    let candidate = worktrees_dir.join(fake_candidate('a'));
    fs::create_dir(&candidate).unwrap();

    sweep_best_effort(&ctx);

    assert!(
        candidate.is_dir(),
        "an aborted sweep reached through the best-effort entry point must \
         still delete nothing"
    );
}
