//! Hardened `git` invocation for read-only probes against an **untrusted**
//! corpus.
//!
//! Loomweave analyzes and serves repositories whose contents are not trusted (the
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

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
        // Same environment discipline as `hardened_git_command` — which CALLS
        // this, so building the full hardened command here would re-enter the
        // OnceLock. `--version` is a builtin with no repository access;
        // cleared env + the process-spawn essentials is the whole allowlist
        // it needs (clarion-9ea93124aa: this was the one git spawn in the
        // file that inherited the environment).
        let mut command = Command::new("git");
        command.env_clear();
        apply_operator_env_passthrough(&mut command, |name| std::env::var_os(name));
        command.arg("--version");
        run_git_probe(
            command,
            &GitProbeLimits {
                deadline: Duration::from_secs(5),
                max_stdout_bytes: 4096,
            },
        )
        .ok()
        .and_then(|o| o.stdout_utf8().ok().map(str::to_owned))
        .and_then(|s| parse_git_version(&s))
        .is_some_and(|v| v >= (2, 40))
    })
}

/// Re-add the operator-owned variables a cleared-environment git spawn still
/// needs (or deliberately honors) — the passthrough half of the allowlist,
/// with `getenv` injected so tests can drive it without mutating process
/// environment:
///
/// * `PATH` — resolve the `git` binary itself.
/// * `SYSTEMROOT` — Windows refuses to start processes without it.
/// * `GIT_CEILING_DIRECTORIES` — bounds *upward* repository discovery, so a
///   non-repo project directory nested inside another repository is not
///   probed as the OUTER repo. Operator-owned (never repository content),
///   and it can only STOP discovery, never redirect it — passing it through
///   cannot reopen the selector-hijack class the clear defends against
///   (clarion-9ea93124aa).
fn apply_operator_env_passthrough(
    command: &mut Command,
    getenv: impl Fn(&str) -> Option<std::ffi::OsString>,
) {
    if let Some(path) = getenv("PATH") {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    if let Some(root) = getenv("SYSTEMROOT") {
        command.env("SYSTEMROOT", root);
    }
    if let Some(ceiling) = getenv("GIT_CEILING_DIRECTORIES") {
        command.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
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
/// # use loomweave_core::{hardened_git_command, run_git_probe_default};
/// let mut command = hardened_git_command(Path::new("/corpus"));
/// command.args(["rev-parse", "HEAD"]);
/// let out = run_git_probe_default(command);
/// ```
///
/// Spawn the built command with [`run_git_probe`] / [`run_git_probe_default`],
/// never `Command::output()`: a corpus-controlled repository can make git emit
/// unbounded output or block forever, and `output()` has neither a byte cap nor
/// a deadline.
///
/// **Callers must not hash working-tree content** on an untrusted corpus (use
/// `diff --cached`, not `status` or a worktree `diff`) — see the module docs for
/// why `$GIT_DIR/info/attributes` makes that the call site's responsibility.
/// `--attr-source` is added only on Git >= 2.40 (probed once); older Git omits it
/// and is still safe (the `--cached` call sites are the real control).
pub fn hardened_git_command(repo_root: &Path) -> Command {
    let mut command = Command::new("git");
    // Start from an EMPTY environment (clarion-9202f4acec). The repository
    // selectors — GIT_DIR, GIT_WORK_TREE, GIT_COMMON_DIR, GIT_INDEX_FILE,
    // GIT_OBJECT_DIRECTORY, GIT_ALTERNATE_OBJECT_DIRECTORIES, GIT_NAMESPACE,
    // GIT_EXEC_PATH, GIT_CONFIG_* — all OVERRIDE the `-C <repo_root>` below, so
    // a hostile or merely stale environment silently repoints every probe at a
    // different repository. Enumerating them with `env_remove` is a losing game:
    // the list grows with each Git release. Clearing and re-adding only what a
    // read-only local probe needs fails closed on the ones we have not heard of.
    //
    // The explicit `env_remove` calls below are kept deliberately: they document
    // the specific injection vectors this wrapper defends against, and they keep
    // the guarantee if a caller ever hands us a pre-populated Command.
    command.env_clear();
    apply_operator_env_passthrough(&mut command, |name| std::env::var_os(name));
    command
        // Machine-parsed output: pin the locale so messages and any
        // locale-sensitive formatting cannot shift under the parser.
        .env("LC_ALL", "C")
        .env("LANG", "C")
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

/// Bytes of stderr retained (the tail) by [`run_git_probe`]. stderr is
/// diagnostic: overflow drops the oldest bytes and never fails the probe.
pub const GIT_PROBE_STDERR_TAIL_BYTES: usize = 64 * 1024;

/// How long [`run_git_probe`] still spends joining its drain threads once the
/// deadline is already spent. Without it, every error path would detach its
/// readers by construction (the budget is exhausted the moment the deadline
/// fires) — the leak this runner exists to avoid.
const READER_JOIN_GRACE: Duration = Duration::from_secs(1);

/// Poll interval while waiting for the child to exit.
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Poll interval while waiting for a drain thread to finish.
const READER_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Wall-clock and stdout ceilings for one git probe against the corpus.
#[derive(Debug, Clone, Copy)]
pub struct GitProbeLimits {
    /// Kill the process tree and fail the probe when exceeded.
    pub deadline: Duration,
    /// Kill the process tree and fail the probe when stdout exceeds this.
    pub max_stdout_bytes: usize,
}

impl Default for GitProbeLimits {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(30),
            max_stdout_bytes: 32 * 1024 * 1024,
        }
    }
}

/// A completed, bounded probe.
#[derive(Debug)]
pub struct GitProbeOutput {
    /// The child's exit status (always successful — a non-zero exit is
    /// reported as [`GitProbeError::NonZeroExit`] instead).
    pub status: ExitStatus,
    /// Everything the child wrote to stdout, at most
    /// [`GitProbeLimits::max_stdout_bytes`].
    pub stdout: Vec<u8>,
    /// The last [`GIT_PROBE_STDERR_TAIL_BYTES`] bytes of stderr.
    pub stderr_tail: Vec<u8>,
}

impl GitProbeOutput {
    /// Strict UTF-8 view of stdout. Machine-parsed git output is ASCII/UTF-8
    /// under `LC_ALL=C`; anything else is malformed and fails the probe.
    pub fn stdout_utf8(&self) -> Result<&str, GitProbeError> {
        std::str::from_utf8(&self.stdout).map_err(|_| GitProbeError::NonUtf8)
    }

    /// The retained stderr tail, decoded lossily for diagnostics only.
    #[must_use]
    pub fn stderr_tail_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr_tail).into_owned()
    }
}

/// Why a bounded git probe produced no usable output.
#[derive(Debug, thiserror::Error)]
pub enum GitProbeError {
    /// `git` could not be started (not on `PATH`, not executable, …).
    #[error("spawn git: {0}")]
    Spawn(#[source] std::io::Error),
    /// The child outlived its deadline; its process tree was killed.
    #[error("git probe exceeded its {after:?} deadline and was killed")]
    Timeout {
        /// The deadline that was exceeded.
        after: Duration,
    },
    /// The child wrote more than the stdout cap; its process tree was killed.
    #[error("git probe stdout exceeded {limit} bytes and was killed")]
    StdoutOverflow {
        /// The cap that was exceeded.
        limit: usize,
    },
    /// The child ran to completion but exited non-zero.
    #[error("git exited with {code:?}: {stderr_tail}")]
    NonZeroExit {
        /// The child's exit code, `None` when it died from a signal.
        code: Option<i32>,
        /// The retained stderr tail, decoded lossily.
        stderr_tail: String,
    },
    /// stdout was not valid UTF-8 (see [`GitProbeOutput::stdout_utf8`]).
    #[error("git probe stdout is not valid UTF-8")]
    NonUtf8,
    /// Reading a pipe, waiting on the child, or joining a drain thread failed.
    #[error("git probe I/O: {0}")]
    Io(#[source] std::io::Error),
}

impl GitProbeError {
    /// The child's exit code, only for [`GitProbeError::NonZeroExit`].
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Self::NonZeroExit { code, .. } => *code,
            _ => None,
        }
    }
}

/// Run `command` with stdin null, stdout and stderr piped and drained
/// concurrently, a hard stdout byte cap, and a wall-clock deadline. On cap or
/// deadline the whole process tree is killed and the child reaped; the drain
/// threads are then joined before returning. Never use `Command::output()` on a
/// corpus path — its length is only checkable after unbounded allocation, and
/// it waits forever.
///
/// stderr is diagnostic, not data: only its last
/// [`GIT_PROBE_STDERR_TAIL_BYTES`] bytes are kept, and its volume never fails
/// the probe.
///
/// Residual: a grandchild that inherited the pipes can outlive the killed tree
/// and hold them open. Joining the drain threads is therefore bounded too — by
/// whatever is left of the deadline, but never less than a one-second grace, so
/// an already-expired probe still reaps its readers. Only if that bound is
/// also exhausted does the probe return [`GitProbeError::Timeout`] and detach
/// the threads (they exit on their own at EOF).
pub fn run_git_probe(
    mut command: Command,
    limits: &GitProbeLimits,
) -> Result<GitProbeOutput, GitProbeError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(GitProbeError::Spawn)?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        // Unreachable: both were just piped. Fail rather than panic — this is a
        // hardening primitive on the untrusted-corpus path.
        reap_killed(&mut child);
        return Err(GitProbeError::Io(std::io::Error::other(
            "git probe pipes were not created",
        )));
    };
    let overflow = Arc::new(AtomicBool::new(false));
    let cap = limits.max_stdout_bytes;
    let stdout_reader = {
        let overflow = Arc::clone(&overflow);
        thread::spawn(move || read_capped(stdout, cap, &overflow))
    };
    let stderr_reader = thread::spawn(move || read_tail(stderr, GIT_PROBE_STDERR_TAIL_BYTES));

    let started = Instant::now();
    let status = loop {
        if overflow.load(Ordering::Acquire) {
            reap_killed(&mut child);
            reap_readers(stdout_reader, stderr_reader, started, limits.deadline);
            return Err(GitProbeError::StdoutOverflow { limit: cap });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                reap_killed(&mut child);
                reap_readers(stdout_reader, stderr_reader, started, limits.deadline);
                return Err(GitProbeError::Io(err));
            }
        }
        if started.elapsed() >= limits.deadline {
            tracing::debug!(
                deadline = ?limits.deadline,
                "git probe exceeded its deadline; killing the process tree"
            );
            reap_killed(&mut child);
            reap_readers(stdout_reader, stderr_reader, started, limits.deadline);
            return Err(GitProbeError::Timeout {
                after: limits.deadline,
            });
        }
        thread::sleep(CHILD_POLL_INTERVAL);
    };

    let until = reader_join_deadline(started, limits.deadline);
    let stdout = join_within(stdout_reader, until).ok_or(GitProbeError::Timeout {
        after: limits.deadline,
    })?;
    // The child can also exit on its own after overflowing a cap smaller than
    // the pipe buffer, so the flag is re-checked on the success path — before
    // the reader's own error, because a cap breach outranks a read error.
    if overflow.load(Ordering::Acquire) {
        let _ = join_within(stderr_reader, until);
        return Err(GitProbeError::StdoutOverflow { limit: cap });
    }
    let stdout = stdout.map_err(GitProbeError::Io)?;
    let stderr_tail = join_within(stderr_reader, until)
        .ok_or(GitProbeError::Timeout {
            after: limits.deadline,
        })?
        .unwrap_or_default();
    if !status.success() {
        return Err(GitProbeError::NonZeroExit {
            code: status.code(),
            stderr_tail: String::from_utf8_lossy(&stderr_tail).into_owned(),
        });
    }
    Ok(GitProbeOutput {
        status,
        stdout,
        stderr_tail,
    })
}

/// [`run_git_probe`] with [`GitProbeLimits::default`] (30 s, 32 MiB).
pub fn run_git_probe_default(command: Command) -> Result<GitProbeOutput, GitProbeError> {
    run_git_probe(command, &GitProbeLimits::default())
}

/// Read until EOF or until the next chunk would exceed `cap`; on overflow set
/// the flag and stop reading (the writer then blocks until it is killed).
fn read_capped(
    mut reader: impl Read,
    cap: usize,
    overflow: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(buf),
            Ok(n) => {
                if buf.len() + n > cap {
                    overflow.store(true, Ordering::Release);
                    return Ok(buf);
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// Ring-buffer drain: keep only the last `tail` bytes. Mirrors the plugin
/// host's `drain_stderr_into_ring` (host.rs) semantics.
fn read_tail(mut reader: impl Read, tail: usize) -> std::io::Result<Vec<u8>> {
    let mut ring = std::collections::VecDeque::with_capacity(tail.min(8192));
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(ring.into_iter().collect()),
            Ok(n) => {
                ring.extend(chunk[..n].iter().copied());
                while ring.len() > tail {
                    ring.pop_front();
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// Kill the whole tree and reap the child (a dropped `Child` is never waited on
/// on Unix — see host.rs — so an early return would leave a zombie).
fn reap_killed(child: &mut Child) {
    let _ = crate::plugin::process_tree::kill_process_tree(child);
    let _ = child.wait();
}

/// The instant by which the drain threads must have finished: whatever is left
/// of the deadline, but never less than [`READER_JOIN_GRACE`] from now.
fn reader_join_deadline(started: Instant, deadline: Duration) -> Instant {
    let grace = Instant::now() + READER_JOIN_GRACE;
    started
        .checked_add(deadline)
        .map_or(grace, |by| by.max(grace))
}

/// Join a drain thread, or give up at `until` and detach it (something still
/// holds the pipe open — see the residual in [`run_git_probe`]).
fn join_within(
    handle: JoinHandle<std::io::Result<Vec<u8>>>,
    until: Instant,
) -> Option<std::io::Result<Vec<u8>>> {
    while !handle.is_finished() {
        if Instant::now() >= until {
            tracing::debug!("git probe drain thread outlived the killed process tree; detaching");
            return None;
        }
        thread::sleep(READER_POLL_INTERVAL);
    }
    Some(
        handle
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("git probe reader panicked"))),
    )
}

/// Best-effort join of both drain threads on an error path; their output is
/// already known to be unusable, so only the reaping matters.
fn reap_readers(
    stdout_reader: JoinHandle<std::io::Result<Vec<u8>>>,
    stderr_reader: JoinHandle<std::io::Result<Vec<u8>>>,
    started: Instant,
    deadline: Duration,
) {
    let until = reader_join_deadline(started, deadline);
    let _ = join_within(stdout_reader, until);
    let _ = join_within(stderr_reader, until);
}

/// List untracked, non-ignored files in `repo_root`, hardened for an untrusted
/// corpus (clarion-d9cf8bcfa9; ADR-045).
///
/// Uses `git ls-files --others --exclude-standard -z`: it enumerates worktree
/// paths Git is not tracking and that `.gitignore`/exclude rules do not cover,
/// **without hashing working-tree content**. That distinction is load-bearing —
/// `git status` must hash to report modifications, which runs a repo-controlled
/// `filter.<drv>.clean` (the one residual the module docs describe, via
/// `$GIT_DIR/info/attributes`); listing untracked paths never hashes, so that
/// filter is never invoked. Verified by the
/// `ls_files_others_does_not_run_clean_filter` test in this module.
///
/// `-z` is NUL-delimited, so paths containing newlines or other special bytes
/// are unambiguous (no C-quoting to decode). Fail-soft like the crate's other
/// corpus git probes: returns `None` when git is unavailable, `repo_root` is not
/// a work tree, or the command fails — never an error. An empty `Vec` means "a
/// git repo with no untracked files".
///
/// Bounded by [`GitProbeLimits::default`]; a timeout, overflow, or non-UTF-8
/// listing returns `None` (unknown), never a partial list.
#[must_use]
pub fn list_untracked_files(repo_root: &Path) -> Option<Vec<String>> {
    let mut command = hardened_git_command(repo_root);
    command.args(["ls-files", "--others", "--exclude-standard", "-z"]);
    let out = run_git_probe_default(command).ok()?;
    let text = out.stdout_utf8().ok()?;
    Some(
        text.split('\0')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect(),
    )
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
        // Machine-parsed output needs a pinned locale.
        assert!(envs.contains(&("LC_ALL".to_owned(), Some("C".to_owned()))));
        assert!(envs.contains(&("LANG".to_owned(), Some("C".to_owned()))));

        // The environment is CLEARED and rebuilt, so the child sees exactly the
        // keys below and nothing else — including selectors no one has enumerated
        // yet. Asserting the whole key set (not just a deny-list) is what makes
        // that closed: a new inherited variable cannot slip in unnoticed.
        //
        // NOTE: after `env_clear()`, `Command::env_remove` drops the key instead
        // of recording a `(key, None)` entry, so removed vars correctly do NOT
        // appear here. `clarion-9202f4acec`'s behavioural proof — that an
        // inherited `GIT_DIR` no longer redirects a probe — lives in
        // `loomweave-cli`'s `doctor_git_probes_ignore_a_hijacked_git_dir_in_the_environment`.
        let mut keys: Vec<&str> = envs.iter().map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        let mut expected = vec![
            "GIT_ATTR_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_SYSTEM",
            "GIT_OPTIONAL_LOCKS",
            "LANG",
            "LC_ALL",
        ];
        if std::env::var_os("PATH").is_some() {
            expected.push("PATH");
        }
        #[cfg(windows)]
        if std::env::var_os("SYSTEMROOT").is_some() {
            expected.push("SYSTEMROOT");
        }
        // Operator-owned upward-discovery bound; passed through when the
        // ambient environment carries one (clarion-9ea93124aa). The
        // deterministic passthrough proof is
        // `ceiling_directories_pass_through_the_clear` below.
        if std::env::var_os("GIT_CEILING_DIRECTORIES").is_some() {
            expected.push("GIT_CEILING_DIRECTORIES");
        }
        expected.sort_unstable();
        assert_eq!(
            keys, expected,
            "the child environment must be exactly the read-only-probe allowlist"
        );
        for selector in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_EXEC_PATH",
            "GIT_CONFIG_COUNT",
            "GIT_EXTERNAL_DIFF",
            "GIT_ATTR_SOURCE",
        ] {
            assert!(
                !keys.contains(&selector),
                "{selector} must never reach the child environment"
            );
        }
    }

    /// clarion-9ea93124aa: an operator-set `GIT_CEILING_DIRECTORIES` survives
    /// the clear — it bounds upward discovery (the same safety direction as
    /// the clear itself) and repository content can never set it. Driven
    /// through the injected getenv so no process environment is mutated.
    #[test]
    fn ceiling_directories_pass_through_the_clear() {
        let mut command = Command::new("git");
        command.env_clear();
        apply_operator_env_passthrough(&mut command, |name| match name {
            "GIT_CEILING_DIRECTORIES" => Some(std::ffi::OsString::from("/srv/checkouts")),
            _ => None,
        });
        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(
            envs.contains(&(
                "GIT_CEILING_DIRECTORIES".to_owned(),
                Some("/srv/checkouts".to_owned())
            )),
            "the operator ceiling must reach the child: {envs:?}"
        );
    }

    /// The allowlist must be sufficient, not merely safe: a cleared environment
    /// that cannot find or run `git` would turn every probe into a silent
    /// "untracked / not a repo" answer, which several call sites treat as a
    /// benign negative rather than an error.
    #[test]
    fn hardened_command_can_still_run_git() {
        let repo = tempfile::tempdir().expect("tempdir");
        assert!(
            hardened_git_command(repo.path())
                .args(["init", "-q"])
                .status()
                .is_ok_and(|s| s.success()),
            "the allowlist must leave git runnable"
        );
        let out = hardened_git_command(repo.path())
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .expect("run git rev-parse");
        assert!(out.status.success(), "rev-parse failed: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true");
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

    #[test]
    fn ls_files_others_does_not_run_clean_filter() {
        // The one corpus-controlled code-exec vector hardened_git CANNOT disable by
        // config is `$GIT_DIR/info/attributes` naming a `filter`, whose `.clean`
        // runs only when git HASHES working-tree content. `list_untracked_files`
        // uses `ls-files --others`, which lists paths and never hashes — so the
        // filter must never fire. Prove it empirically (ADR-045, clarion-d9cf8bcfa9):
        // a booby-trapped repo whose clean filter would create a marker must leave
        // NO marker after the call, while still returning the untracked file.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();

        // Skip cleanly if git is unavailable on the test host.
        let Ok(init) = Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo)
            .status()
        else {
            return;
        };
        if !init.success() {
            return;
        }
        // git refuses commands without an identity in some environments; not needed
        // here (no commit), but set repo-local config defensively.
        let _ = Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(repo)
            .status();

        // Booby-trap: an in-`.git` attribute selects a clean filter (the residual
        // source --attr-source cannot neutralize), and a repo-local config defines
        // that filter to create PWNED if ever invoked. Repo-local config + in-git
        // attributes are exactly what an untrusted corpus controls.
        std::fs::create_dir_all(repo.join(".git/info")).unwrap();
        std::fs::write(repo.join(".git/info/attributes"), "* filter=pwn\n").unwrap();
        let marker = repo.join("PWNED");
        Command::new("git")
            .args([
                "config",
                "filter.pwn.clean",
                &format!("sh -c 'touch \"{}\"'", marker.display()),
            ])
            .current_dir(repo)
            .status()
            .unwrap();

        // An untracked file matching the `*` filter attribute. If anything hashed
        // it, the clean filter would run and create the marker.
        std::fs::write(repo.join("evil.py"), "x = 1\n").unwrap();

        let untracked = list_untracked_files(repo).expect("ls-files must succeed in a git repo");
        assert!(
            untracked.iter().any(|p| p == "evil.py"),
            "the untracked file must be listed: {untracked:?}"
        );
        assert!(
            !marker.exists(),
            "ls-files --others must NOT hash working-tree content, so the corpus \
             clean filter must never run (no PWNED marker)"
        );
    }

    /// Write an executable `#!/bin/sh` stub named `name` in `dir`, so a probe
    /// can be driven against a child whose behaviour (hang, flood, exit code)
    /// is chosen by the test rather than by the host's real `git`.
    #[cfg(unix)]
    fn stub(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    #[cfg(unix)]
    fn probe_deadline_kills_a_hung_child_and_reports_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "git", "sleep 30");
        let started = std::time::Instant::now();
        let err = run_git_probe(
            Command::new(exe),
            &GitProbeLimits {
                deadline: Duration::from_millis(200),
                max_stdout_bytes: 1024,
            },
        )
        .unwrap_err();
        assert!(matches!(err, GitProbeError::Timeout { .. }), "{err:?}");
        // Returning at all proves the kill landed: `reap_killed` waits on the
        // child *before* the error is returned, so a `sleep 30` that survived
        // the signal would hold this call for the full 30 s.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "child was not killed promptly"
        );
    }

    #[test]
    #[cfg(unix)]
    fn probe_stdout_cap_kills_a_flooding_child_and_reports_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "git", "head -c 4000000 /dev/zero; sleep 30");
        let started = std::time::Instant::now();
        let err = run_git_probe(
            Command::new(exe),
            &GitProbeLimits {
                deadline: Duration::from_secs(20),
                max_stdout_bytes: 4096,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, GitProbeError::StdoutOverflow { limit: 4096 }),
            "{err:?}"
        );
        // Same proof as the deadline test: the trailing `sleep 30` would hold
        // `reap_killed`'s wait for 30 s if the tree kill had not landed.
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    #[cfg(unix)]
    fn probe_reports_non_zero_exit_with_the_stderr_tail() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "git", "echo boom >&2; exit 3");
        let err = run_git_probe_default(Command::new(exe)).unwrap_err();
        match err {
            GitProbeError::NonZeroExit { code, stderr_tail } => {
                assert_eq!(code, Some(3));
                assert!(stderr_tail.contains("boom"), "{stderr_tail}");
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn probe_keeps_only_the_stderr_tail_and_never_fails_on_stderr_volume() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(
            dir.path(),
            "git",
            "head -c 300000 /dev/zero | tr '\\0' 'a' >&2; echo END >&2; echo ok",
        );
        let out = run_git_probe_default(Command::new(exe)).unwrap();
        assert_eq!(out.stdout_utf8().unwrap().trim(), "ok");
        assert!(out.stderr_tail.len() <= GIT_PROBE_STDERR_TAIL_BYTES);
        assert!(out.stderr_tail_lossy().ends_with("END\n"));
    }

    #[test]
    #[cfg(unix)]
    fn probe_stdout_utf8_is_strict() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "git", "printf '\\377\\376'");
        let out = run_git_probe_default(Command::new(exe)).unwrap();
        assert!(matches!(out.stdout_utf8(), Err(GitProbeError::NonUtf8)));
        assert_eq!(out.stdout, vec![0xff, 0xfe]);
    }

    #[test]
    fn probe_runs_real_git_through_the_hardened_builder() {
        let dir = tempfile::tempdir().unwrap();
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        let mut cmd = hardened_git_command(dir.path());
        cmd.args(["rev-parse", "--is-inside-work-tree"]);
        let out = run_git_probe_default(cmd).unwrap();
        assert_eq!(out.stdout_utf8().unwrap().trim(), "true");
    }

    /// The strict decode is a deliberate behaviour change (this task): a
    /// corpus path that is not UTF-8 now reads as "unknown" rather than as a
    /// lossily-mangled entry in an otherwise trusted list.
    #[test]
    #[cfg(unix)]
    fn list_untracked_files_reports_unknown_for_a_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let Ok(init) = Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo)
            .status()
        else {
            return;
        };
        if !init.success() {
            return;
        }
        std::fs::write(
            repo.join(std::ffi::OsStr::from_bytes(b"bad\xff.py")),
            "x = 1\n",
        )
        .unwrap();
        assert_eq!(
            list_untracked_files(repo),
            None,
            "a non-UTF-8 untracked path must read as unknown, never a partial list"
        );
    }
}
