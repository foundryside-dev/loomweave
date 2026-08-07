//! Managed git-hook blocks that keep the index fresh.
//!
//! `install --hooks` (and `doctor --fix`) merge a Loomweave-managed block into
//! the repository's `post-commit`, `post-checkout`, and `post-merge` hooks.
//! The block runs `loomweave hook git-sync --path .`, which spawns the same
//! single-shot detached background analyze the `SessionStart` hook uses when
//! the index is stale — so index drift born mid-session (commits, branch
//! switches, merges) heals at the moment it happens instead of waiting for the
//! next session start. Fail-soft by construction: `timeout` + `|| true`, so a
//! missing binary or wedged analyze can never block a commit.
//!
//! Merge semantics follow the cede discipline (clarion-3fbb9cdfcd /
//! clarion-c379a8c9ee): foreign hook content — other tools' managed blocks,
//! git-lfs shims, hand-written lines — is preserved byte-for-byte. The block
//! is inserted *before* a trailing `exit` line when one ends the file (e.g.
//! warpline's managed post-commit ends `exit 0`; appending after it would be
//! dead code), replaced in place when a stale Loomweave block exists, and the
//! whole file is left untouched when the current block is already present.
//!
//! Hook files live where git says they live (`git rev-parse --git-path
//! hooks`), which respects `core.hooksPath` and resolves to the shared common
//! dir for linked worktrees. The block passes `--path .` because git runs
//! these hooks from the top of the working tree — so in a linked worktree the
//! sync targets that worktree's isolated store.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const BLOCK_BEGIN: &str = "# BEGIN LOOMWEAVE MANAGED BLOCK";
const BLOCK_END: &str = "# END LOOMWEAVE MANAGED BLOCK";

/// The git hooks that receive the managed block: every event that moves the
/// working tree to a new committed state.
pub const GIT_SYNC_HOOKS: [&str; 3] = ["post-commit", "post-checkout", "post-merge"];

/// The managed block, exactly as installed. `loomweave` is PATH-resolved (same
/// posture as the `SessionStart` hook command and warpline's managed block) and
/// `--path .` binds to whichever working tree fired the hook.
fn managed_block() -> String {
    format!(
        "{BLOCK_BEGIN}\n\
         # Managed by Loomweave. Fail-soft by design: Loomweave must never block git.\n\
         if command -v timeout >/dev/null 2>&1; then _lw_timeout=\"timeout 30\"; else _lw_timeout=\"\"; fi\n\
         $_lw_timeout loomweave hook git-sync --path . >/dev/null 2>&1 || true\n\
         {BLOCK_END}"
    )
}

/// Read-only health of the managed git-sync hooks, for `loomweave doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHookState {
    /// Every git-sync hook file carries the current managed block.
    Present,
    /// At least one Loomweave block exists but the set is stale — an outdated
    /// block body, or only some of the hook files carry one.
    Stale,
    /// No hook file carries a Loomweave block.
    Missing,
    /// Not a git repository (or `git` itself is unavailable) — the hooks have
    /// nowhere to live, which is fine: git-sync is an enrichment.
    NoGitDir,
}

/// Where this repository's hook files live: `git rev-parse --git-path hooks`,
/// which honours `core.hooksPath` and linked-worktree layouts. `None` when the
/// directory cannot be resolved (not a git repo, no `git` binary).
#[must_use]
pub fn hooks_dir(project_root: &Path) -> Option<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.arg("rev-parse").arg("--git-path").arg("hooks");
    cmd.current_dir(project_root);
    // Strip repository-selector env so a caller's GIT_DIR (e.g. when invoked
    // from inside another git hook) cannot repoint the resolution.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&key);
        }
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    Some(if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    })
}

/// Classify one hook file's relationship to the managed block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileState {
    Current,
    StaleBlock,
    NoBlock,
}

fn file_state(path: &Path) -> FileState {
    let Ok(existing) = fs::read_to_string(path) else {
        return FileState::NoBlock;
    };
    if existing.contains(&managed_block()) {
        FileState::Current
    } else if existing.contains(BLOCK_BEGIN) || existing.contains(BLOCK_END) {
        FileState::StaleBlock
    } else {
        FileState::NoBlock
    }
}

/// Classify the managed git-sync hooks without writing anything.
#[must_use]
pub fn git_sync_hook_state(project_root: &Path) -> GitHookState {
    let Some(dir) = hooks_dir(project_root) else {
        return GitHookState::NoGitDir;
    };
    let states: Vec<FileState> = GIT_SYNC_HOOKS
        .iter()
        .map(|name| file_state(&dir.join(name)))
        .collect();
    if states.iter().all(|s| *s == FileState::Current) {
        GitHookState::Present
    } else if states.iter().all(|s| *s == FileState::NoBlock) {
        GitHookState::Missing
    } else {
        GitHookState::Stale
    }
}

/// Merge the managed block into one hook file's content.
///
/// Returns `None` when the file already carries the current block (no write),
/// otherwise the full new content. Foreign content is preserved byte-for-byte;
/// the only Loomweave-owned bytes are the block itself and, on a fresh file,
/// the shebang line.
///
/// # Errors
///
/// Refuses (rather than guesses) when an existing file has unbalanced managed
/// block markers — that is hand-mangled state a human must resolve.
fn merge_managed_block(existing: Option<&str>) -> Result<Option<String>> {
    let block = managed_block();
    let Some(existing) = existing else {
        return Ok(Some(format!("#!/bin/sh\n{block}\n")));
    };
    if existing.contains(&block) {
        return Ok(None);
    }

    let has_begin = existing.contains(BLOCK_BEGIN);
    let has_end = existing.contains(BLOCK_END);
    if has_begin != has_end {
        bail!(
            "unbalanced Loomweave managed-block markers; refusing to rewrite — \
             remove the stray marker line and re-run"
        );
    }
    if has_begin {
        // Replace the stale block in place: everything from the BEGIN line to
        // the END line inclusive, keeping surrounding bytes untouched.
        let begin = existing.find(BLOCK_BEGIN).expect("begin marker present");
        let end_marker = existing.find(BLOCK_END).expect("end marker present");
        if end_marker < begin {
            bail!(
                "Loomweave managed-block END marker precedes BEGIN; refusing to \
                 rewrite — fix the hook file by hand and re-run"
            );
        }
        let end = end_marker + BLOCK_END.len();
        return Ok(Some(format!(
            "{}{block}{}",
            &existing[..begin],
            &existing[end..]
        )));
    }

    // No Loomweave block yet. If the file ends with an `exit` line (warpline's
    // managed post-commit ends `exit 0`), the block must go before it or it
    // would be dead code; otherwise append at the end.
    let last_line_start = existing
        .trim_end_matches(['\n', '\r'])
        .rfind('\n')
        .map_or(0, |i| i + 1);
    let last_line = existing[last_line_start..].trim();
    if last_line == "exit" || last_line.starts_with("exit ") {
        return Ok(Some(format!(
            "{}{block}\n{}",
            &existing[..last_line_start],
            &existing[last_line_start..]
        )));
    }
    let sep = if existing.ends_with('\n') { "" } else { "\n" };
    Ok(Some(format!("{existing}{sep}{block}\n")))
}

/// Merge the managed block into every git-sync hook file. Returns `Ok(None)`
/// when there is no git hooks dir to install into, otherwise whether any file
/// changed.
///
/// # Errors
///
/// Returns an error when a hook file cannot be read/written or carries
/// unbalanced managed-block markers.
pub fn install_git_sync_hooks(project_root: &Path) -> Result<Option<bool>> {
    let Some(dir) = hooks_dir(project_root) else {
        return Ok(None);
    };
    fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let mut changed = false;
    for name in GIT_SYNC_HOOKS {
        let path = dir.join(name);
        let existing = match fs::read_to_string(&path) {
            Ok(content) => Some(content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(err).with_context(|| format!("read {}", path.display()));
            }
        };
        let Some(merged) = merge_managed_block(existing.as_deref())
            .with_context(|| format!("merge managed block into {}", path.display()))?
        else {
            continue;
        };
        fs::write(&path, merged).with_context(|| format!("write {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .with_context(|| format!("chmod {}", path.display()))?;
        }
        changed = true;
    }
    Ok(Some(changed))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        GIT_SYNC_HOOKS, GitHookState, git_sync_hook_state, install_git_sync_hooks, managed_block,
    };

    fn git_init(dir: &Path) {
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .expect("run git init");
        assert!(status.success(), "git init failed");
    }

    #[test]
    fn install_creates_executable_hooks_with_block() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        assert_eq!(git_sync_hook_state(dir.path()), GitHookState::Missing);

        let changed = install_git_sync_hooks(dir.path()).unwrap();
        assert_eq!(changed, Some(true));
        assert_eq!(git_sync_hook_state(dir.path()), GitHookState::Present);

        for name in GIT_SYNC_HOOKS {
            let path = dir.path().join(".git/hooks").join(name);
            let content = fs::read_to_string(&path).unwrap();
            assert!(content.starts_with("#!/bin/sh\n"), "{name}: {content}");
            assert!(content.contains(&managed_block()), "{name} missing block");
            assert!(
                content.contains("hook git-sync --path ."),
                "{name} must run git-sync"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "{name} must be executable");
            }
        }
    }

    #[test]
    fn install_inserts_before_trailing_exit_preserving_foreign_block() {
        // The elspeth layout: warpline's managed post-commit ends `exit 0`.
        // Appending after it would be dead code; the block must land before it
        // and warpline's bytes must survive verbatim.
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let warpline = "#!/bin/sh\n\
            # BEGIN WARPLINE MANAGED BLOCK\n\
            $_wl_timeout warpline ingest-commit HEAD >/dev/null 2>&1 || true\n\
            # END WARPLINE MANAGED BLOCK\n\
            exit 0\n";
        let hook = dir.path().join(".git/hooks/post-commit");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, warpline).unwrap();

        install_git_sync_hooks(dir.path()).unwrap();

        let content = fs::read_to_string(&hook).unwrap();
        assert!(
            content.contains("# BEGIN WARPLINE MANAGED BLOCK"),
            "warpline block must survive: {content}"
        );
        let loomweave_at = content.find("# BEGIN LOOMWEAVE MANAGED BLOCK").unwrap();
        let exit_at = content.rfind("exit 0").unwrap();
        assert!(
            loomweave_at < exit_at,
            "loomweave block must run before the trailing exit: {content}"
        );
        // Everything except the inserted block is byte-identical.
        let stripped = content.replace(&format!("{}\n", managed_block()), "");
        assert_eq!(stripped, warpline, "foreign bytes must be preserved");
    }

    #[test]
    fn install_appends_when_no_trailing_exit() {
        // The git-lfs post-checkout layout: last line invokes lfs, no trailing
        // exit — append at the end.
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let lfs = "#!/bin/sh\ngit lfs post-checkout \"$@\"\n";
        let hook = dir.path().join(".git/hooks/post-checkout");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, lfs).unwrap();

        install_git_sync_hooks(dir.path()).unwrap();

        let content = fs::read_to_string(&hook).unwrap();
        assert!(
            content.starts_with(lfs),
            "foreign prefix preserved: {content}"
        );
        assert!(
            content
                .trim_end()
                .ends_with("# END LOOMWEAVE MANAGED BLOCK")
        );
    }

    #[test]
    fn reinstall_is_byte_for_byte_noop() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        assert_eq!(install_git_sync_hooks(dir.path()).unwrap(), Some(true));
        let before: Vec<String> = GIT_SYNC_HOOKS
            .iter()
            .map(|n| fs::read_to_string(dir.path().join(".git/hooks").join(n)).unwrap())
            .collect();
        assert_eq!(
            install_git_sync_hooks(dir.path()).unwrap(),
            Some(false),
            "reinstall over current hooks must be a no-op"
        );
        let after: Vec<String> = GIT_SYNC_HOOKS
            .iter()
            .map(|n| fs::read_to_string(dir.path().join(".git/hooks").join(n)).unwrap())
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn stale_block_is_replaced_in_place() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let stale = "#!/bin/sh\n\
            # keep me above\n\
            # BEGIN LOOMWEAVE MANAGED BLOCK\n\
            old-loomweave-command\n\
            # END LOOMWEAVE MANAGED BLOCK\n\
            # keep me below\n";
        let hook = dir.path().join(".git/hooks/post-merge");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, stale).unwrap();
        assert_eq!(git_sync_hook_state(dir.path()), GitHookState::Stale);

        install_git_sync_hooks(dir.path()).unwrap();

        let content = fs::read_to_string(&hook).unwrap();
        assert!(!content.contains("old-loomweave-command"), "{content}");
        assert!(content.contains(&managed_block()));
        assert!(content.contains("# keep me above\n"));
        assert!(content.contains("# keep me below\n"));
        assert_eq!(
            content.matches("# BEGIN LOOMWEAVE MANAGED BLOCK").count(),
            1,
            "exactly one block after repair: {content}"
        );
        assert_eq!(git_sync_hook_state(dir.path()), GitHookState::Present);
    }

    #[test]
    fn unbalanced_markers_refuse_to_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let mangled = "#!/bin/sh\n# BEGIN LOOMWEAVE MANAGED BLOCK\nhalf a block\n";
        let hook = dir.path().join(".git/hooks/post-commit");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, mangled).unwrap();

        let result = install_git_sync_hooks(dir.path());
        assert!(result.is_err(), "must refuse on unbalanced markers");
        assert_eq!(
            fs::read_to_string(&hook).unwrap(),
            mangled,
            "mangled file must be left untouched"
        );
    }

    #[test]
    fn non_git_directory_is_a_graceful_noop() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_sync_hook_state(dir.path()), GitHookState::NoGitDir);
        assert_eq!(install_git_sync_hooks(dir.path()).unwrap(), None);
    }
}
