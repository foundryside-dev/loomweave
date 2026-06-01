//! `ShellGitRenameSource` — the v1 concrete `GitRenameSource` (REQ-C-05).
//!
//! The SEI matcher consumes a typed, locator-level git-rename signal
//! (`{old_locator, new_locator}`), never "Clarion's git code". This module is
//! the v1 supplier: it shells `git diff --name-status -M` for *file* renames and
//! translates each into the locator renames it implies, by substituting the
//! renamed file's module prefix across the current run's locator set. `legis`
//! becomes the first external supplier post-v1 by implementing the same trait —
//! with no change to the matcher.
//!
//! ## Scope (v1, honest framing)
//! - It diffs the **working tree against `HEAD`** (`git diff -M HEAD`), so it
//!   detects *uncommitted* renames — the "rename a file, then re-analyze"
//!   flow. A rename that was already committed shows no git rename; the SEI
//!   move case (identical body + signature) still carries it, labelled `moved`
//!   rather than `locator_changed`. Identity is preserved either way.
//! - Path→module translation is Python-shaped (strip the extension, drop a
//!   trailing `/__init__`, map separators to `.`). That matches the v1 Python
//!   plugin; the seam is what keeps this from calcifying.
//! - No git, not a repo, or a git error ⇒ an empty signal (best-effort): the
//!   move case carries the load without git.

use std::path::{Path, PathBuf};
use std::process::Command;

use clarion_storage::{GitRename, GitRenameSource};

/// v1 `GitRenameSource`: shells git, translates file renames to locator renames
/// over the current run's locator set.
pub(crate) struct ShellGitRenameSource {
    repo_root: PathBuf,
    /// Every locator present in the current run. A file rename only yields a
    /// locator rename for current locators whose qualname sits under the new
    /// module (the renamed-to file).
    current_locators: Vec<String>,
}

impl ShellGitRenameSource {
    pub(crate) fn new(repo_root: impl Into<PathBuf>, current_locators: Vec<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            current_locators,
        }
    }

    /// Run `git diff --name-status -M <base>` in the repo and return its stdout,
    /// or `None` on any failure (not a repo, git missing, non-zero exit).
    fn run_git_diff(&self, base: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["diff", "--name-status", "-M"])
            .arg(base)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }
}

impl GitRenameSource for ShellGitRenameSource {
    fn renames_since(&self, base_commit: &str) -> Vec<GitRename> {
        // An empty base means "compare the working tree to HEAD".
        let base = if base_commit.is_empty() {
            "HEAD"
        } else {
            base_commit
        };
        let Some(stdout) = self.run_git_diff(base) else {
            return Vec::new();
        };
        let file_renames = parse_git_rename_lines(&stdout);
        file_renames_to_locator_renames(&file_renames, &self.current_locators)
    }
}

/// Parse `git diff --name-status -M` output, returning `(old_path, new_path)`
/// for every rename (`R<score>\told\tnew`). Non-rename lines are ignored.
fn parse_git_rename_lines(stdout: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let mut cols = line.split('\t');
        let Some(status) = cols.next() else { continue };
        if !status.starts_with('R') {
            continue;
        }
        if let (Some(old), Some(new)) = (cols.next(), cols.next())
            && !old.is_empty()
            && !new.is_empty()
        {
            out.push((old.to_owned(), new.to_owned()));
        }
    }
    out
}

/// Map a project-relative source path to its Python module qualname:
/// `a/b/c.py` → `a.b.c`; `a/b/__init__.py` → `a.b`; non-Python paths → `None`.
fn path_to_module(path: &str) -> Option<String> {
    let path = path.trim_start_matches("./");
    let stem = path
        .strip_suffix(".py")
        .or_else(|| path.strip_suffix(".pyi"))?;
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    if stem.is_empty() {
        return None;
    }
    Some(stem.replace(['/', '\\'], "."))
}

/// Translate file renames into locator renames over the current locator set.
///
/// For a rename `old_path → new_path` with modules `old_mod`/`new_mod`, any
/// current locator `{plugin}:{kind}:{qualname}` whose qualname is `new_mod` or
/// is prefixed by `new_mod.` maps to the candidate old locator with `new_mod`
/// rewritten to `old_mod`. The matcher then confirms the carry only if the old
/// binding's body hash is byte-identical (the rename predicate), so an
/// over-broad candidate here cannot cause a false carry — it is a *hint*, fail-
/// closed at the matcher.
fn file_renames_to_locator_renames(
    file_renames: &[(String, String)],
    current_locators: &[String],
) -> Vec<GitRename> {
    let mut out = Vec::new();
    for (old_path, new_path) in file_renames {
        let (Some(old_mod), Some(new_mod)) =
            (path_to_module(old_path), path_to_module(new_path))
        else {
            continue;
        };
        if old_mod == new_mod {
            continue;
        }
        for locator in current_locators {
            let mut segs = locator.splitn(3, ':');
            let (Some(plugin), Some(kind), Some(qualname)) =
                (segs.next(), segs.next(), segs.next())
            else {
                continue;
            };
            let new_qualname = if qualname == new_mod {
                Some(old_mod.clone())
            } else {
                qualname
                    .strip_prefix(&format!("{new_mod}."))
                    .map(|rest| format!("{old_mod}.{rest}"))
            };
            if let Some(old_qualname) = new_qualname {
                out.push(GitRename {
                    old_locator: format!("{plugin}:{kind}:{old_qualname}"),
                    new_locator: locator.clone(),
                });
            }
        }
    }
    out
}

/// True if `path` is inside a git work tree (used to skip the git probe
/// entirely on non-repo corpora, avoiding a spurious subprocess per run).
pub(crate) fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rename_lines_and_ignores_others() {
        let out = "M\tsrc/keep.py\n\
                   R096\tsrc/auth.py\tsrc/authn.py\n\
                   A\tsrc/new.py\n\
                   R100\tpkg/a/old.py\tpkg/a/new.py\n";
        assert_eq!(
            parse_git_rename_lines(out),
            vec![
                ("src/auth.py".to_owned(), "src/authn.py".to_owned()),
                ("pkg/a/old.py".to_owned(), "pkg/a/new.py".to_owned()),
            ]
        );
    }

    #[test]
    fn path_to_module_handles_python_shapes() {
        assert_eq!(path_to_module("a/b/c.py").as_deref(), Some("a.b.c"));
        assert_eq!(path_to_module("./a/b.py").as_deref(), Some("a.b"));
        assert_eq!(path_to_module("a/b/__init__.py").as_deref(), Some("a.b"));
        assert_eq!(path_to_module("a/b/c.pyi").as_deref(), Some("a.b.c"));
        assert_eq!(path_to_module("README.md"), None);
    }

    #[test]
    fn translates_file_rename_to_locator_renames_for_module_members() {
        let renames = vec![("auth.py".to_owned(), "authn.py".to_owned())];
        let current = vec![
            "python:function:authn.login".to_owned(),
            "python:class:authn.Session".to_owned(),
            "python:module:authn".to_owned(),
            "python:function:other.unrelated".to_owned(),
        ];
        let got = file_renames_to_locator_renames(&renames, &current);
        assert!(got.contains(&GitRename {
            old_locator: "python:function:auth.login".to_owned(),
            new_locator: "python:function:authn.login".to_owned(),
        }));
        assert!(got.contains(&GitRename {
            old_locator: "python:class:auth.Session".to_owned(),
            new_locator: "python:class:authn.Session".to_owned(),
        }));
        // The module entity itself (qualname == new module) maps too.
        assert!(got.contains(&GitRename {
            old_locator: "python:module:auth".to_owned(),
            new_locator: "python:module:authn".to_owned(),
        }));
        // Unrelated module is untouched.
        assert!(!got.iter().any(|r| r.new_locator.contains("other")));
    }

    #[test]
    fn no_locator_rename_when_module_unchanged() {
        // A rename whose path maps to the same module (e.g. case-only on a
        // case-insensitive checkout) yields nothing.
        let renames = vec![("a/b.py".to_owned(), "a/b.py".to_owned())];
        let current = vec!["python:function:a.b.f".to_owned()];
        assert!(file_renames_to_locator_renames(&renames, &current).is_empty());
    }
}
