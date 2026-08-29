//! Integration tests for `WorktreeContext::resolve`, built against real
//! `git worktree` fixtures in tempdirs (the established pattern used by
//! `hardened_git.rs`'s own tests, `loomweave-cli`'s `doctor.rs`, and
//! `sei_git.rs`) rather than mocks — the whole point of this resolver is its
//! interaction with real Git administrative state.

use std::path::Path;
use std::process::Command;

use loomweave_core::store::store_dir;
use loomweave_core::worktree::{ConfigOrigin, WorktreeContext, WorktreeContextError, WorktreeKind};

/// Run `git -C <dir> <args>`, panicking on failure — test setup only.
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
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

/// `git init -b <branch>` plus repo-local identity (`hardened_git` nulls
/// global/system config, so no `safe.directory` setup is needed) and one
/// commit, so the fixture has a real `HEAD`.
fn init_repo(dir: &Path, branch: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", branch]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("f.txt"), "hi\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "init"]);
}

// ---- standalone / main -----------------------------------------------------

#[test]
fn standalone_project_keeps_current_store_path() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = WorktreeContext::resolve(dir.path()).expect("resolves");

    assert_eq!(ctx.kind, WorktreeKind::Standalone);
    assert_eq!(ctx.primary_root, ctx.source_root);
    assert_eq!(ctx.repository_store, ctx.effective_store);
    assert_eq!(ctx.repository_store, store_dir(&ctx.source_root));
    assert!(ctx.stable_id.is_none());
}

#[test]
fn main_worktree_keeps_current_store_path() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    // A linked worktree exists elsewhere — proves `kind == Main` fires
    // because this checkout IS the primary, not merely because Git's
    // worktree machinery has never been touched.
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );

    let ctx = WorktreeContext::resolve(&repo).expect("resolves");

    assert_eq!(ctx.kind, WorktreeKind::Main);
    assert_eq!(ctx.primary_root, ctx.source_root);
    assert_eq!(ctx.repository_store, ctx.effective_store);
    assert_eq!(ctx.repository_store, store_dir(&ctx.source_root));
    assert!(ctx.stable_id.is_none());
}

// ---- linked -----------------------------------------------------------------

#[test]
fn linked_worktree_resolves_under_primary_store() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );
    let linked = dir.path().join("linked");

    // Composition: an operator `[loomweave].store_dir` override at the
    // PRIMARY must be honored, since `repository_store = store_dir(primary_root)`.
    std::fs::write(
        repo.join("weft.toml"),
        "[loomweave]\nstore_dir = \"custom-store\"\n",
    )
    .unwrap();

    let ctx = WorktreeContext::resolve(&linked).expect("resolves");

    assert_eq!(ctx.kind, WorktreeKind::Linked);
    let canonical_repo = std::fs::canonicalize(&repo).unwrap();
    assert_eq!(ctx.primary_root, canonical_repo);
    assert_eq!(ctx.repository_store, canonical_repo.join("custom-store"));
    let stable_id = ctx
        .stable_id
        .clone()
        .expect("linked worktree has a stable id");
    assert_eq!(
        ctx.effective_store,
        ctx.repository_store.join("worktrees").join(&stable_id)
    );
    assert_eq!(ctx.store_paths.db, ctx.effective_store.join("loomweave.db"));
    assert!(stable_id.starts_with("wt-"));
    assert_eq!(stable_id.len(), "wt-".len() + 64);
    assert!(
        stable_id["wt-".len()..]
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    );
}

#[test]
fn linked_nested_project_preserves_its_path_under_the_primary_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    let primary_project = repo.join("services/api");
    std::fs::create_dir_all(&primary_project).unwrap();
    std::fs::write(primary_project.join("api.py"), "pass\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "add nested project"]);
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );
    let linked_project = dir.path().join("linked/services/api");

    let ctx = WorktreeContext::resolve(&linked_project).expect("resolves nested project");

    assert_eq!(ctx.kind, WorktreeKind::Linked);
    assert_eq!(
        ctx.primary_root,
        std::fs::canonicalize(&primary_project).unwrap(),
        "the corresponding nested project path in the primary checkout must be retained"
    );
    assert_eq!(ctx.repository_store, store_dir(&primary_project));
}

#[test]
fn moved_linked_worktree_without_repair_still_classifies_linked() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );
    let linked = dir.path().join("linked");

    let before = WorktreeContext::resolve(&linked).expect("resolves before move");
    assert_eq!(before.kind, WorktreeKind::Linked);

    // Move the worktree directory WITHOUT `git worktree repair`: the
    // registered path in `git worktree list` is now stale (it names the old
    // location), but git commands run inside the moved checkout still work —
    // its `.git` file points at the (unmoved) common dir's administrative
    // entry. Git's own evidence still proves this is a linked worktree.
    let moved = dir.path().join("linked-moved");
    std::fs::rename(&linked, &moved).unwrap();

    let after = WorktreeContext::resolve(&moved).expect("resolves after move");

    // Silently degrading to Standalone here would let `loomweave analyze`
    // build a decoy store at <worktree>/.weft/loomweave/ — the exact outcome
    // worktree isolation exists to forbid.
    assert_eq!(after.kind, WorktreeKind::Linked);
    assert_eq!(after.primary_root, std::fs::canonicalize(&repo).unwrap());
    // The stable ID is keyed by the Git administrative identity, which a
    // plain directory move does not change.
    assert_eq!(after.stable_id, before.stable_id);
    assert_eq!(after.effective_store, before.effective_store);
}

#[test]
fn branch_only_nested_project_errors_instead_of_pointing_at_a_nonexistent_primary() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );
    let linked = dir.path().join("linked");

    // A nested project that exists ONLY on the worktree's branch — the
    // primary checkout (on `main`) has no services/newsvc at all.
    let linked_project = linked.join("services/newsvc");
    std::fs::create_dir_all(&linked_project).unwrap();
    std::fs::write(linked_project.join("svc.py"), "pass\n").unwrap();
    git(&linked, &["add", "."]);
    git(
        &linked,
        &["commit", "-qm", "add branch-only nested project"],
    );

    let err = WorktreeContext::resolve(&linked_project)
        .expect_err("a primary-side project directory that does not exist must fail loud");

    let canonical_repo = std::fs::canonicalize(&repo).unwrap();
    let message = err.to_string();
    match err {
        WorktreeContextError::NestedProjectMissingFromPrimary {
            primary_project,
            primary_checkout,
            ..
        } => {
            assert_eq!(primary_checkout, canonical_repo);
            assert_eq!(primary_project, canonical_repo.join("services/newsvc"));
        }
        other => panic!("expected NestedProjectMissingFromPrimary, got {other:?}"),
    }
    // The message must be truthful and actionable: it names the missing
    // primary-side directory rather than routing the caller to a
    // `loomweave install` hint against a path that does not exist.
    assert!(
        message.contains("does not exist"),
        "message must say the primary-side project directory does not exist: {message}"
    );
    assert!(
        message.contains(canonical_repo.join("services/newsvc").to_str().unwrap()),
        "message must name the missing primary-side path: {message}"
    );
}

#[test]
fn shared_absolute_store_override_namespaces_identical_admin_names_by_project() {
    let dir = tempfile::tempdir().unwrap();
    let shared_store = dir.path().join("shared-store");
    let mut contexts = Vec::new();

    for parent in ["a", "b"] {
        let repo = dir.path().join(parent).join("repo");
        init_repo(&repo, "main");
        std::fs::write(
            repo.join("weft.toml"),
            format!(
                "[loomweave]\nstore_dir = {:?}\n",
                shared_store.to_str().unwrap()
            ),
        )
        .unwrap();
        git(&repo, &["add", "weft.toml"]);
        git(&repo, &["commit", "-qm", "configure shared store"]);
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feature", "../linked"],
        );
        contexts.push(
            WorktreeContext::resolve(&dir.path().join(parent).join("linked"))
                .expect("resolve linked worktree"),
        );
    }

    assert!(contexts.iter().all(|ctx| ctx.store_dir_overridden));
    assert_eq!(contexts[0].repository_store, shared_store);
    assert_eq!(contexts[1].repository_store, shared_store);
    assert_ne!(
        contexts[0].stable_id, contexts[1].stable_id,
        "unrelated projects sharing an override must not route the same Git admin name to one store"
    );
    assert_ne!(contexts[0].effective_store, contexts[1].effective_store);
}

#[test]
fn primary_is_identified_by_git_dir_not_branch_name() {
    let dir = tempfile::tempdir().unwrap();
    // The primary lives in a directory that is NOT named "main" or
    // "primary" in any way a naive heuristic would key on.
    let primary = dir.path().join("checkout-a");
    init_repo(&primary, "trunk");
    // The LINKED worktree is deliberately named "main" — a naive
    // name-based check would misidentify this as the primary.
    git(
        &primary,
        &["worktree", "add", "-q", "-b", "other", "../main"],
    );
    let misleadingly_named_linked = dir.path().join("main");

    let linked_ctx = WorktreeContext::resolve(&misleadingly_named_linked).expect("resolves");
    let canonical_primary = std::fs::canonicalize(&primary).unwrap();
    assert_eq!(linked_ctx.kind, WorktreeKind::Linked);
    assert_eq!(linked_ctx.primary_root, canonical_primary);
    assert_ne!(
        linked_ctx.primary_root,
        std::fs::canonicalize(&misleadingly_named_linked).unwrap()
    );

    let primary_ctx = WorktreeContext::resolve(&primary).expect("resolves");
    assert_eq!(primary_ctx.kind, WorktreeKind::Main);
    assert_eq!(primary_ctx.primary_root, canonical_primary);
}

#[test]
fn bare_primary_classifies_as_standalone() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    init_repo(&src, "main");

    let bare = dir.path().join("bare.git");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            "--bare",
            src.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(
        &bare,
        &["worktree", "add", "-q", "../linked-from-bare", "main"],
    );
    let linked = dir.path().join("linked-from-bare");

    let ctx = WorktreeContext::resolve(&linked).expect("resolves");

    // A bare primary has no working tree to isolate under: fall back to
    // this checkout's own local store, not `Linked`.
    assert_eq!(ctx.kind, WorktreeKind::Standalone);
    assert_eq!(ctx.primary_root, ctx.source_root);
    assert_eq!(ctx.repository_store, ctx.effective_store);
    assert_eq!(ctx.repository_store, store_dir(&ctx.source_root));
    assert!(ctx.stable_id.is_none());
}

// ---- identity stability ------------------------------------------------------

#[test]
fn symlinked_source_root_resolves_to_same_identity_as_target() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );
    let linked = dir.path().join("linked");

    let symlink = dir.path().join("linked-symlink");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&linked, &symlink).unwrap();

    let via_target = WorktreeContext::resolve(&linked).expect("resolves");
    let via_symlink = WorktreeContext::resolve(&symlink).expect("resolves");

    assert_eq!(via_target.source_root, via_symlink.source_root);
    assert_eq!(via_target.kind, via_symlink.kind);
    assert_eq!(via_target.primary_root, via_symlink.primary_root);
    assert_eq!(via_target.stable_id, via_symlink.stable_id);
    assert_eq!(via_target.effective_store, via_symlink.effective_store);
}

#[test]
fn stable_id_survives_branch_switch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );
    let linked = dir.path().join("linked");

    let before = WorktreeContext::resolve(&linked).expect("resolves");
    assert_eq!(before.kind, WorktreeKind::Linked);
    assert!(before.stable_id.is_some());

    git(&linked, &["checkout", "-q", "-b", "other-branch"]);

    let after = WorktreeContext::resolve(&linked).expect("resolves");
    assert_eq!(after.kind, WorktreeKind::Linked);
    assert_eq!(before.stable_id, after.stable_id);
}

#[test]
fn stable_id_survives_repository_move() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    // Nested so a single rename of the repo directory moves the linked
    // worktree along with it, matching a real "the whole checkout moved"
    // relocation (renaming a project folder, unpacking to a new CI path).
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "linked-sub"],
    );
    let linked = repo.join("linked-sub");

    let before = WorktreeContext::resolve(&linked).expect("resolves");
    assert_eq!(before.kind, WorktreeKind::Linked);
    assert!(before.stable_id.is_some());

    let moved_repo = dir.path().join("repo-moved");
    std::fs::rename(&repo, &moved_repo).unwrap();
    // A repository move invalidates Git's absolute administrative links
    // (`.git` files, `worktrees/<name>/gitdir`) exactly as it would for a
    // real developer relocating a checkout; `git worktree repair` is Git's
    // own documented remediation, run here as the fixture's setup step
    // (not something this resolver performs itself).
    git(&moved_repo, &["worktree", "repair", "linked-sub"]);
    let moved_linked = moved_repo.join("linked-sub");

    let after = WorktreeContext::resolve(&moved_linked).expect("resolves");
    assert_eq!(after.kind, WorktreeKind::Linked);
    assert!(after.stable_id.is_some());
    assert_eq!(before.stable_id, after.stable_id);
}

#[test]
fn distinct_worktrees_get_distinct_stable_ids() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature-a", "../linked-a"],
    );
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature-b", "../linked-b"],
    );

    let ctx_a = WorktreeContext::resolve(&dir.path().join("linked-a")).expect("resolves");
    let ctx_b = WorktreeContext::resolve(&dir.path().join("linked-b")).expect("resolves");

    assert_eq!(ctx_a.kind, WorktreeKind::Linked);
    assert_eq!(ctx_b.kind, WorktreeKind::Linked);
    assert_eq!(ctx_a.primary_root, ctx_b.primary_root);
    assert_ne!(ctx_a.stable_id, ctx_b.stable_id);
}

// ---- error / fallback paths --------------------------------------------------

#[test]
#[cfg(unix)]
fn non_utf8_worktree_path_is_rejected_with_typed_error() {
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    // A directory component containing an invalid UTF-8 byte sequence.
    let bad_name = std::ffi::OsStr::from_bytes(b"bad-\xFF-name");
    let bad_path = dir.path().join(bad_name);
    std::fs::create_dir(&bad_path).unwrap();

    let err = WorktreeContext::resolve(&bad_path).expect_err("non-UTF-8 path must be rejected");
    match err {
        WorktreeContextError::NonUtf8Path { field, .. } => {
            assert_eq!(field, "source_root");
        }
        other => panic!("expected NonUtf8Path, got {other:?}"),
    }
}

#[test]
fn unresolvable_git_context_falls_back_to_local_store() {
    let dir = tempfile::tempdir().unwrap();
    let broken = dir.path().join("broken-worktree");
    std::fs::create_dir_all(&broken).unwrap();
    // A worktree `.git` file pointing at an administrative directory that
    // does not exist — e.g. the primary was deleted, or the link was never
    // repaired after a move. `git rev-parse` fails cleanly (nonzero exit);
    // the resolver must fall back, not panic or misclassify as `Linked`.
    std::fs::write(broken.join(".git"), "gitdir: /nonexistent/gitdir\n").unwrap();

    let ctx = WorktreeContext::resolve(&broken).expect("resolves without error");

    assert_eq!(ctx.kind, WorktreeKind::Standalone);
    assert_eq!(ctx.primary_root, ctx.source_root);
    assert_eq!(ctx.repository_store, ctx.effective_store);
    assert_eq!(ctx.repository_store, store_dir(&ctx.source_root));
    assert!(ctx.stable_id.is_none());
}

// ---- config_origin (struct shape, not independently listed in the RED list) -

#[test]
fn config_origin_prefers_source_then_primary_then_default() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo, "main");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../linked"],
    );
    let linked = dir.path().join("linked");

    // Neither source nor primary has a config file yet.
    let ctx = WorktreeContext::resolve(&linked).expect("resolves");
    assert_eq!(ctx.config_origin, ConfigOrigin::DefaultTarget);

    // Primary has one: selected.
    std::fs::write(repo.join("loomweave.yaml"), "# primary config\n").unwrap();
    let ctx = WorktreeContext::resolve(&linked).expect("resolves");
    assert_eq!(ctx.config_origin, ConfigOrigin::Primary);

    // Source (the linked worktree itself, e.g. a branch-tracked config)
    // takes precedence over the primary's.
    std::fs::write(linked.join("loomweave.yaml"), "# source config\n").unwrap();
    let ctx = WorktreeContext::resolve(&linked).expect("resolves");
    assert_eq!(ctx.config_origin, ConfigOrigin::Source);
}
