//! Managed git-hook blocks that keep the index fresh.
//!
//! `install --hooks` (and `doctor --fix`) merge a Loomweave-managed block into
//! the repository's `post-merge` and `post-checkout` hooks. The block runs
//! `loomweave hook git-sync --path .`, which spawns the same single-shot
//! detached background analyze the `SessionStart` hook uses when the index is
//! stale — so index drift born from a merge or a branch switch heals at the
//! moment it happens instead of waiting for the next session start. Fail-soft
//! by construction: `timeout` + `|| true`, so a missing binary or wedged
//! analyze can never block git.
//!
//! `post-commit` is deliberately NOT in the set (clarion-78d75e45c9). A commit
//! changes no file content the index has not already seen through the
//! file-drift channel; it only moves the commit clock, which the analyze
//! no-indexed-changes fast path settles in about a second on demand. Firing a
//! full refresh per commit produced 12–148 runs a day on a shared checkout,
//! most of them re-scanning a tree that had not structurally changed. The
//! `post-checkout` block is gated on git's branch-switch flag (`$3 = 1`): a
//! file checkout (`git checkout -- path`) does not fire it. An install over a
//! pre-1.6 layout removes the retired `post-commit` block (only Loomweave's
//! own bytes; foreign content stays byte-for-byte).
//!
//! Merge semantics follow the cede discipline (clarion-3fbb9cdfcd /
//! clarion-c379a8c9ee): foreign hook content — other tools' managed blocks,
//! git-lfs shims, hand-written lines — is preserved byte-for-byte. The block
//! is inserted *before* a trailing `exit` line when one ends the file (e.g.
//! warpline's managed post-commit ends `exit 0`; appending after it would be
//! dead code), replaced in place when a stale Loomweave block exists, and the
//! whole file is left untouched when the current block is already present.
//!
//! Hook files live where git says they live, which takes two probes rather
//! than one (ADR-063). The hardened `git rev-parse --git-path hooks` answers
//! for the repository — a repo-local `core.hooksPath`, and the shared common
//! dir for linked worktrees — but the hardening nulls `GIT_CONFIG_GLOBAL`, so
//! it is blind to the operator's own `~/.gitconfig`. An operator-global
//! `core.hooksPath` is therefore read by a second, deliberately unhardened
//! probe (`loomweave_core::operator_global_git_config_command`) and wins when
//! set; see [`hooks_dir`] for the precedence and its one deliberate departure
//! from git's own. The block passes `--path .` because git runs these hooks
//! from the top of the working tree — so in a linked worktree the sync targets
//! that worktree's isolated store.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use loomweave_core::{
    hardened_git_command, operator_global_git_config_command, run_git_probe_default,
};

const BLOCK_BEGIN: &str = "# BEGIN LOOMWEAVE MANAGED BLOCK";
const BLOCK_END: &str = "# END LOOMWEAVE MANAGED BLOCK";

/// The git hooks that receive the managed block: the events that move the
/// working tree to a materially different committed state in bulk.
pub const GIT_SYNC_HOOKS: [&str; 2] = ["post-checkout", "post-merge"];

/// Hooks that carried the block in earlier releases and no longer should. An
/// install removes Loomweave's block from these; `doctor` reports a lingering
/// one as stale.
pub const RETIRED_GIT_SYNC_HOOKS: [&str; 1] = ["post-commit"];

/// The managed block for one hook, exactly as installed. `loomweave` is
/// PATH-resolved (same posture as the `SessionStart` hook command and
/// warpline's managed block) and `--path .` binds to whichever working tree
/// fired the hook. `post-checkout` receives git's `(prev, new, flag)` arguments
/// and runs only for a branch checkout (`flag = 1`); the block body is
/// otherwise identical across hooks.
fn managed_block(hook: &str) -> String {
    let body = "# Managed by Loomweave. Fail-soft by design: Loomweave must never block git.\n\
         if command -v timeout >/dev/null 2>&1; then _lw_timeout=\"timeout 30\"; else _lw_timeout=\"\"; fi\n\
         $_lw_timeout loomweave hook git-sync --path . >/dev/null 2>&1 || true";
    if hook == "post-checkout" {
        format!(
            "{BLOCK_BEGIN}\n\
             # Branch checkouts only: git passes flag=1 for a branch switch, 0 for a file checkout.\n\
             if [ \"${{3:-1}}\" = \"1\" ]; then\n\
             {body}\n\
             fi\n\
             {BLOCK_END}"
        )
    } else {
        format!("{BLOCK_BEGIN}\n{body}\n{BLOCK_END}")
    }
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
    /// [`hooks_dir`] could not answer: not a git repository, `git` itself is
    /// unavailable, or the bounded probe failed (deadline, output cap,
    /// non-UTF-8). The hooks have nowhere to install, which is fine: git-sync
    /// is an enrichment, so every one of those folds to the same graceful
    /// no-op rather than an error.
    NoGitDir,
}

/// The hardened `git rev-parse --git-path hooks` probe for `project_root`.
///
/// Split out from [`hooks_dir`] so a unit test can introspect the built
/// command without spawning git. The hardened builder is what strips the
/// repository-selector environment: it calls `env_clear()` and rebuilds the
/// child environment from an explicit allow-list, which is strictly stronger
/// than the `GIT_*`-prefix loop this replaced (that loop missed every
/// non-`GIT_`-prefixed vector and had to be kept in sync with each git
/// release). It matters here because git exports `GIT_DIR` into every hook it
/// runs, and `install --hooks` can be reached from inside one.
fn hooks_dir_command(project_root: &Path) -> Command {
    let mut cmd = hardened_git_command(project_root);
    cmd.args(["rev-parse", "--git-path", "hooks"]);
    cmd
}

/// The operator's GLOBAL `core.hooksPath`, or `None` when they have not set
/// one (or it cannot be read).
///
/// This is the half [`hooks_dir_command`] structurally cannot see: the hardened
/// builder nulls `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`, so `rev-parse
/// --git-path hooks` answers `.git/hooks` even when `~/.gitconfig` says
/// otherwise — and `install --hooks` then writes hooks git will never run,
/// while `git_sync_hook_state` cheerfully reports them current. Under ADR-063
/// the operator's global git config is operator intent, so it is read through
/// the one sanctioned non-corpus git spawn
/// (`loomweave_core::operator_global_git_config_command`, which never consults
/// repository config).
///
/// `--path` makes git expand a leading `~` against the forwarded `HOME`. A
/// relative value is resolved against the WORKTREE TOP LEVEL, which is where
/// git runs hooks from and therefore what git itself resolves it against —
/// verified empirically, and NOT the same as `project_root`, which may be any
/// subdirectory the operator pointed a command at. If that probe cannot answer,
/// `project_root` is the fallback (the common case, where they are equal).
fn operator_global_hooks_path(project_root: &Path) -> Option<PathBuf> {
    let mut command = operator_global_git_config_command();
    command.args(["--path", "core.hooksPath"]);
    // An unset key exits 1 with no output — not an error, just "not set".
    let out = run_git_probe_default(command).ok()?;
    let trimmed = out.stdout_utf8().ok()?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        return Some(path);
    }
    Some(
        worktree_top_level(project_root)
            .unwrap_or_else(|| project_root.to_path_buf())
            .join(path),
    )
}

/// The top of the working tree containing `project_root`, through the hardened
/// probe. Only consulted to resolve a relative operator-global
/// `core.hooksPath`, which is rare.
fn worktree_top_level(project_root: &Path) -> Option<PathBuf> {
    let mut command = hardened_git_command(project_root);
    command.args(["rev-parse", "--show-toplevel"]);
    let out = run_git_probe_default(command).ok()?;
    let trimmed = out.stdout_utf8().ok()?.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// Where this repository's hook files live. `None` when the directory cannot be
/// resolved (not a git repo, no `git` binary, or the probe hit its deadline /
/// output cap / produced non-UTF-8) — hooks are an enrichment, so every failure
/// folds to "nowhere to install", never an error.
///
/// Two sources. `git rev-parse --git-path hooks` through the hardened builder
/// is asked first — it honours a **repository-local** `core.hooksPath` and
/// linked-worktree layouts, and its failure is what tells us there is no
/// repository here at all. When it answers, the operator's **global**
/// `core.hooksPath` ([`operator_global_hooks_path`]) overrides it if set:
/// operator intent under ADR-063, and structurally invisible to the hardened
/// probe, which nulls `GIT_CONFIG_GLOBAL`.
///
/// The operator's setting is preferred over the repository's, which inverts
/// git's own precedence in the one case where both are set. That is deliberate
/// under ADR-063 — a repo-local `core.hooksPath` is corpus content, and
/// Loomweave should not merge its managed block into a directory the corpus
/// chose. The cost is that in that (rare) case git runs the repository's hooks
/// dir while Loomweave writes into the operator's, so the managed block does
/// not fire; nothing is written into a corpus-chosen path, which is the safer
/// half of the trade.
#[must_use]
pub fn hooks_dir(project_root: &Path) -> Option<PathBuf> {
    // The hardened probe runs FIRST and its failure is still the whole answer.
    // It is what establishes that `project_root` is a git work tree at all —
    // and without that gate, an operator with a global `core.hooksPath` would
    // have Loomweave merge its managed block into their real hooks directory
    // when pointed at a directory that is not a repository.
    let out = run_git_probe_default(hooks_dir_command(project_root)).ok()?;
    let trimmed = out.stdout_utf8().ok()?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(global) = operator_global_hooks_path(project_root) {
        return Some(global);
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

fn file_state(path: &Path, hook: &str) -> FileState {
    let Ok(existing) = fs::read_to_string(path) else {
        return FileState::NoBlock;
    };
    if existing.contains(&managed_block(hook)) {
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
        .map(|name| file_state(&dir.join(name), name))
        .collect();
    // A block lingering in a retired hook is stale even when the live set is
    // current: it still fires a refresh per commit until removed.
    let retired_block_present = RETIRED_GIT_SYNC_HOOKS
        .iter()
        .any(|name| file_state(&dir.join(name), name) != FileState::NoBlock);
    if states.iter().all(|s| *s == FileState::Current) && !retired_block_present {
        GitHookState::Present
    } else if states.iter().all(|s| *s == FileState::NoBlock) && !retired_block_present {
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
fn merge_managed_block(existing: Option<&str>, hook: &str) -> Result<Option<String>> {
    let block = managed_block(hook);
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

/// Strip the Loomweave managed block from a retired hook file's content.
///
/// Only the block's own bytes (plus the newline that terminated it) are
/// removed; everything foreign is preserved byte-for-byte. A file that
/// consisted of nothing but the shebang and our block is deleted rather than
/// left as an empty executable.
///
/// # Errors
///
/// Refuses on unbalanced markers, like [`merge_managed_block`].
fn remove_managed_block(existing: &str) -> Result<RetiredHookEdit> {
    let has_begin = existing.contains(BLOCK_BEGIN);
    let has_end = existing.contains(BLOCK_END);
    if has_begin != has_end {
        bail!(
            "unbalanced Loomweave managed-block markers; refusing to rewrite — \
             remove the stray marker line and re-run"
        );
    }
    if !has_begin {
        return Ok(RetiredHookEdit::Unchanged);
    }
    let begin = existing.find(BLOCK_BEGIN).expect("begin marker present");
    let end_marker = existing.find(BLOCK_END).expect("end marker present");
    if end_marker < begin {
        bail!(
            "Loomweave managed-block END marker precedes BEGIN; refusing to \
             rewrite — fix the hook file by hand and re-run"
        );
    }
    let mut end = end_marker + BLOCK_END.len();
    if existing[end..].starts_with('\n') {
        end += 1;
    }
    let remaining = format!("{}{}", &existing[..begin], &existing[end..]);
    let only_shebang = remaining
        .lines()
        .all(|line| line.trim().is_empty() || line.starts_with("#!"));
    Ok(if only_shebang {
        RetiredHookEdit::Delete
    } else {
        RetiredHookEdit::Rewrite(remaining)
    })
}

/// What removing the block from a retired hook file amounts to.
enum RetiredHookEdit {
    /// No Loomweave block in the file.
    Unchanged,
    /// Foreign content remains; write this back.
    Rewrite(String),
    /// Nothing but the shebang would remain; delete the file.
    Delete,
}

/// Merge the managed block into every git-sync hook file and remove it from
/// every retired one. Returns `Ok(None)`
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
        let Some(merged) = merge_managed_block(existing.as_deref(), name)
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
    for name in RETIRED_GIT_SYNC_HOOKS {
        let path = dir.join(name);
        let existing = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("read {}", path.display()));
            }
        };
        match remove_managed_block(&existing)
            .with_context(|| format!("remove managed block from {}", path.display()))?
        {
            RetiredHookEdit::Unchanged => continue,
            RetiredHookEdit::Rewrite(content) => {
                fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
            }
            RetiredHookEdit::Delete => {
                fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            }
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
        GIT_SYNC_HOOKS, GitHookState, git_sync_hook_state, hooks_dir_command,
        install_git_sync_hooks, managed_block,
    };

    /// The hook-directory probe must run on the hardened builder, not a
    /// hand-rolled `GIT_*` strip. The builder clears the environment and
    /// rebuilds it from an explicit allow-list, so a repository-selector
    /// variable inherited from the caller (git itself exports `GIT_DIR` into
    /// every hook it runs, which is exactly how `install --hooks` can be
    /// reached) can never repoint `rev-parse --git-path hooks` at a foreign
    /// repository.
    ///
    /// This is a unit test introspecting the built `Command` rather than an
    /// integration test mutating the real process environment: this workspace
    /// denies `unsafe_code`, and `std::env::set_var`/`remove_var` are `unsafe
    /// fn` on this toolchain. Same technique, and same reasoning, as
    /// `worktree::sweep::git_common_dir_command_keeps_foreign_git_env_out`.
    #[test]
    fn hooks_dir_command_keeps_foreign_git_env_out() {
        let cmd = hooks_dir_command(Path::new("/nonexistent"));
        let envs: Vec<_> = cmd.get_envs().map(|(k, _)| k.to_os_string()).collect();
        assert!(envs.iter().any(|k| k == "GIT_CONFIG_NOSYSTEM"), "{envs:?}");
        assert!(!envs.iter().any(|k| k == "GIT_DIR"));
        assert!(cmd.get_args().any(|a| a == "--git-path"));
    }

    fn hook_path(dir: &Path, name: &str) -> std::path::PathBuf {
        dir.join(".git/hooks").join(name)
    }

    /// Skip when the developer running the suite has a GLOBAL
    /// `core.hooksPath`. These tests write real hook files through
    /// [`install_git_sync_hooks`], and `hooks_dir` now (correctly) prefers the
    /// operator's global hooks directory — which is OUTSIDE the fixture
    /// tempdir, so an unguarded run would both fail the `.git/hooks`
    /// assertions and merge Loomweave's managed block into the operator's real
    /// hooks. The environment cannot be neutralised in-process here (this
    /// workspace denies `unsafe_code`, and `set_var` is `unsafe`), so the
    /// global arm is covered by the hermetic CLI integration test
    /// `tests/git_hooks_global_path.rs` instead, which sets the child's
    /// `GIT_CONFIG_GLOBAL`.
    fn operator_global_hooks_path_would_redirect() -> bool {
        if super::operator_global_hooks_path(Path::new(".")).is_some() {
            eprintln!("skipping: this machine's global core.hooksPath redirects hooks_dir");
            return true;
        }
        false
    }

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
        if operator_global_hooks_path_would_redirect() {
            return;
        }
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
            assert!(
                content.contains(&managed_block(name)),
                "{name} missing block"
            );
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
    fn post_checkout_block_is_gated_on_the_branch_switch_flag() {
        if operator_global_hooks_path_would_redirect() {
            return;
        }
        // clarion-78d75e45c9: `git checkout -- file` (flag 0) must not fire a
        // refresh; only a branch switch (flag 1) does. post-merge has no such
        // flag and runs unconditionally.
        let checkout = managed_block("post-checkout");
        assert!(
            checkout.contains("if [ \"${3:-1}\" = \"1\" ]; then"),
            "{checkout}"
        );
        assert!(checkout.contains("hook git-sync --path ."), "{checkout}");
        let merge = managed_block("post-merge");
        assert!(!merge.contains("${3"), "{merge}");
        assert!(merge.contains("hook git-sync --path ."), "{merge}");
        assert!(!GIT_SYNC_HOOKS.contains(&"post-commit"));
    }

    #[test]
    fn install_removes_retired_post_commit_block_preserving_foreign_bytes() {
        if operator_global_hooks_path_would_redirect() {
            return;
        }
        // The pre-1.6 elspeth layout: our block sits before warpline's trailing
        // `exit 0`. Only Loomweave's bytes go; warpline's block and the exit
        // line survive verbatim, and doctor reads the lingering block as stale
        // until the install runs.
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let warpline = "#!/bin/sh\n\
            # BEGIN WARPLINE MANAGED BLOCK\n\
            $_wl_timeout warpline ingest-commit HEAD >/dev/null 2>&1 || true\n\
            # END WARPLINE MANAGED BLOCK\n";
        let legacy = format!("{warpline}{}\nexit 0\n", managed_block("post-merge"));
        let hook = hook_path(dir.path(), "post-commit");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, &legacy).unwrap();
        for name in GIT_SYNC_HOOKS {
            fs::write(
                hook_path(dir.path(), name),
                format!("#!/bin/sh\n{}\n", managed_block(name)),
            )
            .unwrap();
        }
        assert_eq!(
            git_sync_hook_state(dir.path()),
            GitHookState::Stale,
            "a retired-hook block is stale even when the live set is current"
        );

        assert_eq!(install_git_sync_hooks(dir.path()).unwrap(), Some(true));

        assert_eq!(
            fs::read_to_string(&hook).unwrap(),
            format!("{warpline}exit 0\n"),
            "foreign bytes must be preserved and only our block removed"
        );
        assert_eq!(git_sync_hook_state(dir.path()), GitHookState::Present);
    }

    #[test]
    fn install_deletes_a_retired_hook_that_held_only_our_block() {
        if operator_global_hooks_path_would_redirect() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let hook = hook_path(dir.path(), "post-commit");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(
            &hook,
            format!("#!/bin/sh\n{}\n", managed_block("post-merge")),
        )
        .unwrap();

        install_git_sync_hooks(dir.path()).unwrap();

        assert!(
            !hook.exists(),
            "a shebang-only leftover is removed, not left executable"
        );
        assert_eq!(git_sync_hook_state(dir.path()), GitHookState::Present);
    }

    #[test]
    fn install_inserts_before_trailing_exit_preserving_foreign_block() {
        if operator_global_hooks_path_would_redirect() {
            return;
        }
        // The elspeth layout: warpline's managed hook ends `exit 0`.
        // Appending after it would be dead code; the block must land before it
        // and warpline's bytes must survive verbatim.
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let warpline = "#!/bin/sh\n\
            # BEGIN WARPLINE MANAGED BLOCK\n\
            $_wl_timeout warpline ingest-commit HEAD >/dev/null 2>&1 || true\n\
            # END WARPLINE MANAGED BLOCK\n\
            exit 0\n";
        let hook = dir.path().join(".git/hooks/post-merge");
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
        let stripped = content.replace(&format!("{}\n", managed_block("post-merge")), "");
        assert_eq!(stripped, warpline, "foreign bytes must be preserved");
    }

    #[test]
    fn install_appends_when_no_trailing_exit() {
        if operator_global_hooks_path_would_redirect() {
            return;
        }
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
        if operator_global_hooks_path_would_redirect() {
            return;
        }
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
        if operator_global_hooks_path_would_redirect() {
            return;
        }
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
        assert!(content.contains(&managed_block("post-merge")));
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
        if operator_global_hooks_path_would_redirect() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let mangled = "#!/bin/sh\n# BEGIN LOOMWEAVE MANAGED BLOCK\nhalf a block\n";
        let hook = dir.path().join(".git/hooks/post-merge");
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
