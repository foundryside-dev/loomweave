//! Hardened `git` invocation for read-only probes against an **untrusted**
//! corpus.
//!
//! Clarion analyzes and serves repositories whose contents are not trusted (the
//! same posture that motivates the plugin jail, ADR-021, and the pre-ingest
//! secret scanner). Running `git` inside such a repo is a command-execution
//! hazard: repo-local configuration and Git *attributes* can name programs that
//! Git executes during ordinary *read* commands. The known config/attribute
//! vectors that turn a read into code execution are:
//!
//! - `core.fsmonitor=<program>` — run on index refresh (fires on a fresh clone);
//! - `diff.external` / `GIT_EXTERNAL_DIFF`, `diff.<drv>.textconv` — content diff;
//! - `core.pager` — paged output;
//! - `filter.<driver>.clean` / `.smudge` / `.process`, **selected by a `filter`
//!   attribute** — run whenever Git hashes working-tree content (status, a
//!   worktree diff, rename-similarity scoring).
//!
//! [`hardened_git_command`] is the ONLY sanctioned way to spawn `git` against a
//! corpus path. It neutralizes the config vectors and every attribute source it
//! *can* reach, at the config/argument level (no sandboxing, no new dependency,
//! no change to the read *output*):
//!
//! - operator/global/system config is ignored (`GIT_CONFIG_NOSYSTEM`,
//!   `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` → null device), and env-borne config/
//!   exec injection is stripped (`GIT_CONFIG_COUNT`, `GIT_EXTERNAL_DIFF`,
//!   `GIT_DIFF_OPTS`, `GIT_ATTR_SOURCE`, `GIT_PAGER`);
//! - the remaining (still-untrusted) repo-local config is overridden where it can
//!   name a program, via highest-precedence `-c` flags (`core.fsmonitor=false`,
//!   `diff.external=`, `core.pager=cat`, `core.untrackedCache=false`,
//!   `core.attributesFile=` → null device);
//! - the **attribute sources** that select a `filter`/diff/textconv driver are
//!   neutralized: the per-directory in-tree `.gitattributes` via `--attr-source`
//!   (read from the empty tree → no path gets an attribute), the system
//!   attributes file via `GIT_ATTR_NOSYSTEM`, and `core.attributesFile` via the
//!   `-c` override above.
//!
//! ## The one source config cannot reach: `$GIT_DIR/info/attributes`
//! Git always consults `$GIT_DIR/info/attributes`, and **no config key or
//! environment variable disables it** (`--attr-source` only redirects the
//! *worktree* `.gitattributes`; `GIT_ATTR_NOSYSTEM` only affects the *system*
//! file). An attacker who ships a crafted `.git` directory can therefore still
//! place `* filter=evil` there. The filter only *executes* when Git hashes
//! working-tree content, so the residual is closed not in this helper but at the
//! **call site**, by never hashing the working tree on an untrusted corpus:
//!
//! - the SEI rename diff uses `git diff --cached` (index vs HEAD — no worktree
//!   hash; still sees staged `git mv` renames);
//! - the index-freshness probe avoids `git status` (which must hash the worktree)
//!   in favour of `git diff --cached` plus the stat-based per-file drift check.
//!
//! Read commands that never hash working-tree content (`rev-parse`, `log`,
//! `diff --cached`) are safe through this helper regardless of
//! `info/attributes`. See `clarion-4b5a8aff54`.
//!
//! `--attr-source` requires Git >= 2.40, so it is added only when a one-time
//! `git --version` probe confirms support (see `attr_source_supported`); older
//! Git omits it and stays safe, because `--cached` — not `--attr-source` — is the
//! control that closes the vuln. This avoids silently raising the minimum Git or
//! blanking the (best-effort) signal on Debian/Ubuntu-LTS Git. SHA-256
//! repositories (whose empty tree OID differs from the SHA-1 constant below) make
//! the `--attr-source` resolve fail; the read then fails soft to empty (secure),
//! and the in-tree-attribute belt-and-suspenders is simply inactive — again, the
//! `--cached` call sites carry the actual safety.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// The well-known empty tree object (SHA-1). Reading gitattributes from this
/// tree assigns no attribute to any path, so no `filter`/diff/textconv driver is
/// selected from the in-tree `.gitattributes`.
const EMPTY_TREE_OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Parse `(major, minor)` from `git --version` output (e.g. "git version 2.43.0"
/// or "git version 2.39.3 (Apple Git-145)").
fn parse_git_version(out: &str) -> Option<(u32, u32)> {
    let token = out
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Whether the local `git` supports `--attr-source` (added in Git 2.40). Probed
/// once via `git --version`. When false, the flag is omitted — which is safe
/// regardless: the corpus call sites never hash working-tree content (the only
/// trigger for an attribute-selected filter), so `--attr-source` is in-tree
/// `.gitattributes` defense-in-depth, not the primary control. Omitting it on old
/// Git therefore keeps the probe BOTH safe AND functional, rather than failing
/// the whole git signal (passing an unknown flag to git < 2.40 errors out).
fn attr_source_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        Command::new("git")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| parse_git_version(&s))
            .is_some_and(|v| v >= (2, 40))
    })
}

#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

/// Build a `git` [`Command`] hardened for read-only probes against an untrusted
/// repository at `repo_root` (sets `git -C <repo_root>`). The caller appends the
/// subcommand and its arguments, e.g.:
///
/// ```no_run
/// # use std::path::Path;
/// # use clarion_core::hardened_git_command;
/// let out = hardened_git_command(Path::new("/corpus"))
///     .args(["rev-parse", "HEAD"])
///     .output();
/// ```
///
/// **Callers must not hash working-tree content** on an untrusted corpus (use
/// `diff --cached`, not `status` or a worktree `diff`) — see the module docs for
/// why `$GIT_DIR/info/attributes` makes that the call site's responsibility.
/// `--attr-source` is added only on Git >= 2.40 (probed once); older Git omits it
/// and is still safe (the `--cached` call sites are the real control).
pub fn hardened_git_command(repo_root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
        .env("GIT_CONFIG_SYSTEM", NULL_DEVICE)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Ignore the system gitattributes file (the worktree and core.attributesFile
        // sources are handled by --attr-source and the -c override below).
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIFF_OPTS")
        .env_remove("GIT_ATTR_SOURCE")
        .env_remove("GIT_PAGER")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.untrackedCache=false")
        .arg("-c")
        .arg("diff.external=")
        .arg("-c")
        .arg("core.pager=cat")
        .arg("-c")
        .arg(format!("core.attributesFile={NULL_DEVICE}"));
    // Belt-and-suspenders for the in-tree `.gitattributes` source, but only on
    // Git >= 2.40 (older Git rejects the flag, which would blank the whole
    // signal). Safe to omit otherwise — see `attr_source_supported`.
    if attr_source_supported() {
        command.arg(format!("--attr-source={EMPTY_TREE_OID}"));
    }
    command.arg("-C").arg(repo_root);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardened_command_overrides_repo_controlled_helpers() {
        let command = hardened_git_command(Path::new("/corpus"));
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        // `-c` overrides for the program-naming repo-local config keys.
        assert!(args.windows(2).any(|w| w == ["-c", "core.fsmonitor=false"]));
        assert!(args.windows(2).any(|w| w == ["-c", "diff.external="]));
        assert!(
            args.windows(2)
                .any(|w| w == ["-c", &format!("core.attributesFile={NULL_DEVICE}")]),
            "core.attributesFile must be overridden to the null device"
        );
        // Attributes read from the empty tree → no in-tree filter is selected.
        // Present iff the local git supports the flag (>= 2.40); the test machine
        // determines which branch applies, so gate the assertion on the probe.
        let has_attr_source = args
            .iter()
            .any(|a| a == &format!("--attr-source={EMPTY_TREE_OID}"));
        assert_eq!(
            has_attr_source,
            attr_source_supported(),
            "--attr-source must be present iff git >= 2.40"
        );
        // Operates against the given corpus path.
        assert!(args.windows(2).any(|w| w == ["-C", "/corpus"]));

        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&("GIT_CONFIG_NOSYSTEM".to_owned(), Some("1".to_owned()))));
        assert!(envs.contains(&("GIT_CONFIG_GLOBAL".to_owned(), Some(NULL_DEVICE.to_owned()))));
        assert!(envs.contains(&("GIT_ATTR_NOSYSTEM".to_owned(), Some("1".to_owned()))));
        // Inherited env-based config/exec injection is stripped.
        for removed in ["GIT_CONFIG_COUNT", "GIT_EXTERNAL_DIFF", "GIT_ATTR_SOURCE"] {
            assert!(
                envs.iter().any(|(k, v)| k == removed && v.is_none()),
                "{removed} must be removed from the child environment"
            );
        }
    }

    #[test]
    fn parse_git_version_extracts_major_minor() {
        assert_eq!(parse_git_version("git version 2.43.0"), Some((2, 43)));
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-145)"),
            Some((2, 39))
        );
        assert_eq!(
            parse_git_version("git version 2.40.1.windows.1"),
            Some((2, 40))
        );
        assert_eq!(parse_git_version("garbage"), None);
    }
}
