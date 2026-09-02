//! `install --hooks` must write into the operator's GLOBAL `core.hooksPath`.
//!
//! Routing the hook-directory probe onto the hardened git builder (ADR-063)
//! nulls `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`, so `git rev-parse --git-path
//! hooks` answers `.git/hooks` even when the operator's `~/.gitconfig` sets
//! `core.hooksPath`. `install --hooks` then wrote hooks git would never run,
//! and `doctor` reported them present. The operator's global git config is
//! operator intent under ADR-063's own definition, so it is read by a second,
//! deliberately unhardened probe.
//!
//! Driven end-to-end through the binary rather than in-process: this workspace
//! denies `unsafe_code` and `std::env::set_var` is `unsafe`, so the only way to
//! pin an environment-shaped contract without mutating the test process is to
//! set it on a child.

use std::fs;
use std::path::Path;

use assert_cmd::Command;

const BLOCK_BEGIN: &str = "# BEGIN LOOMWEAVE MANAGED BLOCK";

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A hermetic `git` for fixture setup: the operator's own global/system config
/// must not shape the fixture repository.
fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

fn loomweave_install_hooks(project_root: &Path, gitconfig: &Path) -> assert_cmd::assert::Assert {
    let mut cmd = Command::cargo_bin("loomweave").expect("loomweave binary");
    cmd.args(["install", "--hooks", "--path"])
        .arg(project_root)
        // The operator's global config, redirected at a file this test owns.
        // `--global` never reads repository config, so nothing corpus-borne
        // enters through it.
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "LOOMWEAVE_CODEX_CONFIG",
            std::env::temp_dir().join(format!(
                "loomweave-test-codex-config-{}.toml",
                std::process::id()
            )),
        );
    cmd.assert()
}

/// An ABSOLUTE global `core.hooksPath` is where the hooks land — not
/// `.git/hooks`, which git would never consult.
#[test]
fn install_hooks_honours_an_absolute_operator_global_hooks_path() {
    if !git_available() {
        eprintln!("skipping: no git available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let global_hooks = dir.path().join("xyz-hooks");
    fs::create_dir_all(&project).unwrap();
    git(&project, &["init", "-q"]);

    let gitconfig = dir.path().join("operator-gitconfig");
    fs::write(
        &gitconfig,
        format!("[core]\n\thooksPath = {}\n", global_hooks.display()),
    )
    .unwrap();

    loomweave_install_hooks(&project, &gitconfig).success();

    for hook in ["post-merge", "post-checkout"] {
        let installed = global_hooks.join(hook);
        let body = fs::read_to_string(&installed)
            .unwrap_or_else(|err| panic!("{} not written: {err}", installed.display()));
        assert!(
            body.contains(BLOCK_BEGIN),
            "{} carries no managed block:\n{body}",
            installed.display()
        );
        assert!(
            !project.join(".git/hooks").join(hook).exists(),
            "{hook} must not be written to .git/hooks, which git will never run"
        );
    }
}

/// A RELATIVE global `core.hooksPath` resolves against the worktree TOP LEVEL
/// — which is what git itself does when it runs a hook — not against whatever
/// subdirectory the operator pointed `--path` at.
#[test]
fn a_relative_global_hooks_path_resolves_against_the_worktree_top_level() {
    if !git_available() {
        eprintln!("skipping: no git available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let nested = project.join("sub");
    fs::create_dir_all(&nested).unwrap();
    git(&project, &["init", "-q"]);

    let gitconfig = dir.path().join("operator-gitconfig");
    fs::write(&gitconfig, "[core]\n\thooksPath = teamhooks\n").unwrap();

    loomweave_install_hooks(&nested, &gitconfig).success();

    let at_top = project.join("teamhooks/post-merge");
    assert!(
        at_top.exists(),
        "a relative global hooksPath must resolve against the worktree top level ({})",
        at_top.display()
    );
    assert!(
        !nested.join("teamhooks/post-merge").exists(),
        "resolving against --path instead of the worktree top puts the hooks where git will \
         never look"
    );
}

/// git's own precedence: a `core.hooksPath` in the repository's `.git/config`
/// beats the operator's global one. `.git/config` is not committed content —
/// it is operator/tool state, exactly like `~/.gitconfig` — so honouring it
/// keeps Loomweave writing where git will actually look.
#[test]
fn a_repository_local_hooks_path_beats_the_operator_global_one() {
    if !git_available() {
        eprintln!("skipping: no git available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let local_hooks = dir.path().join("local-hooks");
    let global_hooks = dir.path().join("global-hooks");
    fs::create_dir_all(&project).unwrap();
    git(&project, &["init", "-q"]);
    // Raw git, straight into `.git/config`.
    git(
        &project,
        &["config", "core.hooksPath", local_hooks.to_str().unwrap()],
    );

    let gitconfig = dir.path().join("operator-gitconfig");
    fs::write(
        &gitconfig,
        format!("[core]\n\thooksPath = {}\n", global_hooks.display()),
    )
    .unwrap();

    loomweave_install_hooks(&project, &gitconfig).success();

    assert!(
        local_hooks.join("post-merge").exists(),
        "the repository's own .git/config must win, as it does for git itself ({})",
        local_hooks.display()
    );
    assert!(
        !global_hooks.exists(),
        "the operator's global hooks dir must not be touched when .git/config sets the path"
    );
    assert!(
        !project.join(".git/hooks/post-merge").exists(),
        "neither should the default dir git will not consult"
    );
}

/// The global path is honoured only INSIDE a work tree. Without this gate, a
/// `--path` that is not a repository would merge Loomweave's managed block
/// into the operator's real, global hooks directory.
#[test]
fn a_non_repository_never_reaches_the_operators_global_hooks_dir() {
    if !git_available() {
        eprintln!("skipping: no git available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("not-a-repo");
    let global_hooks = dir.path().join("xyz-hooks");
    fs::create_dir_all(&project).unwrap();

    let gitconfig = dir.path().join("operator-gitconfig");
    fs::write(
        &gitconfig,
        format!("[core]\n\thooksPath = {}\n", global_hooks.display()),
    )
    .unwrap();

    loomweave_install_hooks(&project, &gitconfig).success();

    assert!(
        !global_hooks.exists(),
        "a non-repository must not have hooks installed into the operator's global hooks dir"
    );
}
