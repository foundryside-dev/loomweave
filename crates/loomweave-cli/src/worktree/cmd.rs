//! Target resolution for `loomweave worktree analyze [--no-incremental] --
//! <name-or-path>`.
//!
//! `<name-or-path>` is either a registered Git worktree's administrative
//! name (its `git worktree list` directory basename) or a filesystem path.
//! [`resolve_target`] turns either spelling into a concrete [`PathBuf`],
//! which the CLI then hands to the ordinary `loomweave analyze` entry point
//! — `loomweave worktree analyze` is not a separate analysis pipeline, only
//! a convenience for naming a linked worktree without typing its full path.
//!
//! A registered worktree name is checked **before** filesystem-path
//! interpretation: a name is an unambiguous Git administrative identity, so
//! a same-named directory that happens to exist must never shadow it.

use std::path::{Path, PathBuf};

use loomweave_core::hardened_git::hardened_git_command;

/// `<name-or-path>` matched neither a registered Git worktree nor an
/// existing filesystem path.
#[derive(Debug, thiserror::Error)]
#[error(
    "{name_or_path:?} is not a registered git worktree name and not an \
     existing path (run `git worktree list` to see registered names)"
)]
pub struct TargetNotFound {
    /// The unresolved argument, for the error message.
    pub name_or_path: String,
}

/// Resolve `name_or_path` (as given to `loomweave worktree analyze -- <name-or-path>`)
/// against `cwd`.
///
/// Tries a registered Git worktree name first (via `git worktree list
/// --porcelain -z`, run from `cwd`); on any failure to positively identify a
/// registered worktree (no `git`, not a repository, no match), falls back to
/// treating `name_or_path` as a filesystem path, resolved relative to `cwd`
/// if not already absolute.
///
/// # Errors
///
/// Returns [`TargetNotFound`] if `name_or_path` matches no registered
/// worktree and does not exist as a path.
pub fn resolve_target(cwd: &Path, name_or_path: &str) -> Result<PathBuf, TargetNotFound> {
    if let Some(path) = find_registered_worktree(cwd, name_or_path) {
        return Ok(path);
    }

    let candidate = if Path::new(name_or_path).is_absolute() {
        PathBuf::from(name_or_path)
    } else {
        cwd.join(name_or_path)
    };
    if candidate.exists() {
        return Ok(candidate);
    }

    Err(TargetNotFound {
        name_or_path: name_or_path.to_owned(),
    })
}

/// Search `git worktree list --porcelain -z` (run from `cwd`) for an entry
/// whose path's final component equals `name` exactly — matching the path
/// the way `git worktree list` itself reports it, which is usually (though
/// not always: Git disambiguates a colliding default name, and `git
/// worktree move` changes the path without renaming the administrative
/// identity) the worktree's own administrative name. This is a display-level
/// convenience for typing a short name instead of a full path, not a claim
/// about administrative identity — once resolved to a path, the analyze
/// pipeline re-derives identity itself via `WorktreeContext::resolve`,
/// exactly as it would for a hand-typed path. `None` on any failure to run
/// or parse `git` output; every such failure falls back to filesystem-path
/// interpretation in [`resolve_target`], never a hard error here.
fn find_registered_worktree(cwd: &Path, name: &str) -> Option<PathBuf> {
    let output = hardened_git_command(cwd)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    for line in text.split('\0') {
        // Every `-z`-delimited line other than the block-opening `worktree
        // <path>` line (`HEAD <sha>`, `branch <ref>`, `bare`, `detached`, or
        // a blank block separator) carries no path and must be skipped, not
        // treated as a parse failure — an early return here on the first
        // non-matching line would silently fail to match every worktree
        // whose entry isn't first in the list.
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        let candidate = Path::new(path);
        if candidate.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::resolve_target;
    use std::path::Path;
    use std::process::Command;

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

    fn init_repo(dir: &Path, branch: &str) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", branch]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "hi\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "init"]);
    }

    #[test]
    fn resolves_a_registered_worktree_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feature", "../by-name"],
        );

        let resolved = resolve_target(&repo, "by-name").expect("resolves by name");
        assert_eq!(
            std::fs::canonicalize(resolved).unwrap(),
            std::fs::canonicalize(tmp.path().join("by-name")).unwrap()
        );
    }

    #[test]
    fn resolves_a_plain_filesystem_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plain-dir");
        std::fs::create_dir_all(&dir).unwrap();

        let resolved = resolve_target(tmp.path(), "plain-dir").expect("resolves by path");
        assert_eq!(resolved, dir);
    }

    #[test]
    fn resolves_an_absolute_path_regardless_of_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("abs-dir");
        std::fs::create_dir_all(&dir).unwrap();

        let elsewhere = tempfile::tempdir().unwrap();
        let resolved = resolve_target(elsewhere.path(), dir.to_str().unwrap()).expect("resolves");
        assert_eq!(resolved, dir);
    }

    #[test]
    fn a_registered_name_takes_priority_over_a_same_named_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feature", "../shadowed"],
        );
        // A decoy directory with the same relative name, reachable from cwd
        // by plain path interpretation, must not win.
        std::fs::create_dir_all(repo.join("shadowed")).unwrap();

        let resolved = resolve_target(&repo, "shadowed").expect("resolves");
        assert_eq!(
            std::fs::canonicalize(resolved).unwrap(),
            std::fs::canonicalize(tmp.path().join("shadowed")).unwrap(),
            "the registered worktree must win over the same-named subdirectory"
        );
    }

    #[test]
    fn neither_a_worktree_nor_a_path_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_target(tmp.path(), "does-not-exist-anywhere").unwrap_err();
        assert!(err.to_string().contains("does-not-exist-anywhere"));
    }
}
