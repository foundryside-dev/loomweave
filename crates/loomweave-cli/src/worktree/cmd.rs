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

/// `<name-or-path>` could not be resolved to exactly one worktree path.
#[derive(Debug, thiserror::Error)]
pub enum ResolveTargetError {
    /// Matched neither a registered Git worktree nor an existing filesystem
    /// path.
    #[error(
        "{name_or_path:?} is not a registered git worktree name and not an \
         existing path (run `git worktree list` to see registered names)"
    )]
    NotFound {
        /// The unresolved argument, for the error message.
        name_or_path: String,
    },
    /// Matched the final path component of more than one registered
    /// worktree — `git worktree list` order must not silently decide which
    /// checkout gets a 20–30 minute analyze (clarion-ce6e9347c9). The
    /// operator disambiguates by passing a full or `./`-prefixed path,
    /// which never enters the name rung.
    #[error(
        "{name:?} matches more than one registered git worktree: {}; pass the full path of the \
         one you mean instead",
        .matches
            .iter()
            .map(|path| format!("{}", path.display()))
            .collect::<Vec<_>>()
            .join(", ")
    )]
    Ambiguous {
        /// The ambiguous basename, for the error message.
        name: String,
        /// Every registered worktree path whose final component matched.
        matches: Vec<PathBuf>,
    },
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
/// Returns [`ResolveTargetError::NotFound`] if `name_or_path` matches no
/// registered worktree and does not exist as a path, and
/// [`ResolveTargetError::Ambiguous`] if it matches the basename of more than
/// one registered worktree (no fall-through to path interpretation: an
/// ambiguous name means the operator's intent is unknown, and a same-named
/// local directory silently winning would be a third interpretation, not a
/// resolution).
pub fn resolve_target(cwd: &Path, name_or_path: &str) -> Result<PathBuf, ResolveTargetError> {
    let mut matches = find_registered_worktrees(cwd, name_or_path);
    match matches.len() {
        1 => return Ok(matches.remove(0)),
        n if n > 1 => {
            return Err(ResolveTargetError::Ambiguous {
                name: name_or_path.to_owned(),
                matches,
            });
        }
        _ => {}
    }

    let candidate = if Path::new(name_or_path).is_absolute() {
        PathBuf::from(name_or_path)
    } else {
        cwd.join(name_or_path)
    };
    if candidate.exists() {
        return Ok(candidate);
    }

    Err(ResolveTargetError::NotFound {
        name_or_path: name_or_path.to_owned(),
    })
}

/// Search `git worktree list --porcelain -z` (run from `cwd`) for every
/// entry whose path's final component equals `name` exactly — matching the
/// path the way `git worktree list` itself reports it, which is usually
/// (though not always: Git disambiguates a colliding default name, and `git
/// worktree move` changes the path without renaming the administrative
/// identity) the worktree's own administrative name. This is a display-level
/// convenience for typing a short name instead of a full path, not a claim
/// about administrative identity — once resolved to a path, the analyze
/// pipeline re-derives identity itself via `WorktreeContext::resolve`,
/// exactly as it would for a hand-typed path. Returns ALL matches so the
/// caller can distinguish one hit from an ambiguous basename
/// (clarion-ce6e9347c9); empty on any failure to run or parse `git` output —
/// every such failure falls back to filesystem-path interpretation in
/// [`resolve_target`], never a hard error here.
fn find_registered_worktrees(cwd: &Path, name: &str) -> Vec<PathBuf> {
    let Ok(output) = hardened_git_command(cwd)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(&output.stdout) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
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
            matches.push(candidate.to_path_buf());
        }
    }
    matches
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

    /// clarion-ce6e9347c9: two registered worktrees sharing a basename must
    /// be an explicit ambiguity error naming both paths — not a silent
    /// first-match that spends a 20–30 minute analyze on whichever checkout
    /// `git worktree list` happens to print first.
    #[test]
    fn ambiguous_basename_is_an_error_naming_both_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        std::fs::create_dir_all(tmp.path().join("a")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b")).unwrap();
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feat-a", "../a/feature"],
        );
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feat-b", "../b/feature"],
        );

        let err = resolve_target(&repo, "feature").expect_err("ambiguous name must error");
        let message = err.to_string();
        assert!(
            message.contains("a/feature") && message.contains("b/feature"),
            "the error must name every colliding path: {message}"
        );
        assert!(
            message.contains("full path"),
            "the error must tell the operator how to disambiguate: {message}"
        );
    }

    /// A `./`-prefixed spelling never enters the name rung, so it remains
    /// the documented disambiguator even while the bare name is ambiguous.
    #[test]
    fn dot_slash_spelling_bypasses_the_ambiguous_name_rung() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        std::fs::create_dir_all(tmp.path().join("a")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b")).unwrap();
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feat-a", "../a/feature"],
        );
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feat-b", "../b/feature"],
        );

        let resolved =
            resolve_target(&repo, "../a/feature").expect("a path spelling resolves directly");
        assert_eq!(
            std::fs::canonicalize(resolved).unwrap(),
            std::fs::canonicalize(tmp.path().join("a/feature")).unwrap()
        );
    }
}
