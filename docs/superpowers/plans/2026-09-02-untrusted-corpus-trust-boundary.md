# Untrusted-Corpus Trust Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make repository-tracked content inert as operator intent: bounded git probes, a tracked-path primitive, a `.venv` rung trust condition, and a `loomweave.yaml` egress gate.

**Architecture:** A bounded git runner in `loomweave-core::hardened_git` replaces every `Command::output()` on the corpus. A `tracked_state` primitive (Rust + Python twin, shared JSON fixture) sits on top of it. Two consumers use the primitive: ADR-058 rung 2 (skip a tracked `.venv/bin/python`) and `McpConfig::load_trusted` (strip egress-capable sections from a tracked config). Surfaces (`doctor`, `config check`, `install`, `project_status_get`) report the verdicts; writers refuse tracked targets.

**Tech Stack:** Rust edition 2024 / MSRV 1.88, `std::process` + `std::thread` (no tokio in core), `tempfile` fixtures with raw `git` for setup; Python 3.11+ plugin (`subprocess`, `select`), pytest.

**Spec:** `docs/superpowers/specs/2026-09-02-untrusted-corpus-trust-boundary-design.md`

## Global Constraints

- Branch `feat/untrusted-corpus-trust-boundary`; merge target is `release/1.6.0` (never literal `main`).
- CI floor (ADR-023) must be green at every commit that claims a task done: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo build --workspace --bins` **before** `cargo nextest run --workspace --all-features` (integration tests exec the built binary), `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`, `cargo deny check`; Python: `plugins/python/.venv/bin/ruff check plugins/python`, `… ruff format --check plugins/python`, `… mypy --strict plugins/python`, `… pytest plugins/python`.
- `unsafe_code = "deny"`; never call `std::env::set_var`/`remove_var` in tests — inject env via closures or child-process env.
- `hardened_git_command(root) -> std::process::Command` keeps returning a bare `Command` (introspection tests depend on `get_args()`/`get_envs()`).
- `hardened_git_command` remains "the ONLY sanctioned way to spawn git against a corpus path"; no new raw `Command::new("git")` in production code.
- Fixture repos in tests are built with raw `git` (`init -q`, repo-local `user.email`/`user.name`, `add -f`, `commit -q -m`), then the production path is exercised. Skip cleanly (early `return`) if `git` is unavailable.
- Failure direction: the two trust consumers fail **closed** — `TrackedState::Unknown` behaves as `Tracked`; `NotAGitWorkTree` behaves as `Untracked`.
- MCP `tools/list` is 13 bytes under a 22 KB CI budget: do **not** change any tool description or input schema. Output JSON fields may be added.
- Log-once idiom: `std::sync::OnceLock` (Rust); a module-level `_announced: set[str]` or bool (Python) with `sys.stderr.write`.
- Remedy text, verbatim wherever the tracked-config remedy is printed: `To own this file: git rm --cached loomweave.yaml && echo loomweave.yaml >> .gitignore`
- New ADR number is **ADR-063**; ADR-058 gets an `## Amendment (2026-09-02) — rung-2 trust condition` section and its Status line becomes `Accepted — amended 2026-09-02 (see Amendment below)`.
- Commit messages: conventional (`feat(core): …`, `test(cli): …`, `docs(adr): …`), ticket ids in the body, trailer `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

---

### Task 1: Bounded git probe runner

**Files:**
- Modify: `crates/loomweave-core/src/hardened_git.rs` (add runner after `hardened_git_command`; replace the `.output()` at `:102-107` inside `attr_source_supported`)
- Modify: `crates/loomweave-core/src/lib.rs:25` (re-export)
- Test: `crates/loomweave-core/src/hardened_git.rs` `mod tests`

**Interfaces:**
- Consumes: `crate::plugin::process_tree::kill_process_tree(&mut Child) -> io::Result<()>` (exists).
- Produces (later tasks rely on these exact names):

```rust
pub struct GitProbeLimits { pub deadline: Duration, pub max_stdout_bytes: usize }
impl Default for GitProbeLimits { /* deadline 30 s, max_stdout_bytes 32 MiB */ }
pub const GIT_PROBE_STDERR_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct GitProbeOutput { pub status: ExitStatus, pub stdout: Vec<u8>, pub stderr_tail: Vec<u8> }
impl GitProbeOutput {
    pub fn stdout_utf8(&self) -> Result<&str, GitProbeError>;          // strict
    pub fn stderr_tail_lossy(&self) -> String;
}

#[derive(Debug, thiserror::Error)]
pub enum GitProbeError {
    #[error("spawn git: {0}")] Spawn(#[source] std::io::Error),
    #[error("git probe exceeded its {after:?} deadline and was killed")] Timeout { after: Duration },
    #[error("git probe stdout exceeded {limit} bytes and was killed")] StdoutOverflow { limit: usize },
    #[error("git exited with {code:?}: {stderr_tail}")] NonZeroExit { code: Option<i32>, stderr_tail: String },
    #[error("git probe stdout is not valid UTF-8")] NonUtf8,
    #[error("git probe I/O: {0}")] Io(#[source] std::io::Error),
}
impl GitProbeError { pub fn exit_code(&self) -> Option<i32>; }   // Some(code) only for NonZeroExit

pub fn run_git_probe(command: Command, limits: &GitProbeLimits) -> Result<GitProbeOutput, GitProbeError>;
pub fn run_git_probe_default(command: Command) -> Result<GitProbeOutput, GitProbeError>;
```

- [ ] **Step 1: Write the failing tests** (append inside `#[cfg(test)] mod tests`, all `#[cfg(unix)]`)

```rust
    fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
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
            &GitProbeLimits { deadline: Duration::from_millis(200), max_stdout_bytes: 1024 },
        )
        .unwrap_err();
        assert!(matches!(err, GitProbeError::Timeout { .. }), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "child was not killed promptly");
    }

    #[test]
    #[cfg(unix)]
    fn probe_stdout_cap_kills_a_flooding_child_and_reports_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "git", "head -c 4000000 /dev/zero; sleep 30");
        let started = std::time::Instant::now();
        let err = run_git_probe(
            Command::new(exe),
            &GitProbeLimits { deadline: Duration::from_secs(20), max_stdout_bytes: 4096 },
        )
        .unwrap_err();
        assert!(matches!(err, GitProbeError::StdoutOverflow { limit: 4096 }), "{err:?}");
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
        let exe = stub(dir.path(), "git", "head -c 300000 /dev/zero | tr '\\0' 'a' >&2; echo END >&2; echo ok");
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
        if Command::new("git").arg("--version").output().is_err() { return; }
        assert!(Command::new("git").args(["init", "-q"]).current_dir(dir.path()).status().unwrap().success());
        let mut cmd = hardened_git_command(dir.path());
        cmd.args(["rev-parse", "--is-inside-work-tree"]);
        let out = run_git_probe_default(cmd).unwrap();
        assert_eq!(out.stdout_utf8().unwrap().trim(), "true");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p loomweave-core probe_`
Expected: compile error — `GitProbeLimits`, `run_git_probe` not defined.

- [ ] **Step 3: Implement the runner** (insert after `hardened_git_command`, before `list_untracked_files`)

```rust
use std::io::Read;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Bytes of stderr retained (the tail) by [`run_git_probe`]. stderr is
/// diagnostic: overflow drops the oldest bytes and never fails the probe.
pub const GIT_PROBE_STDERR_TAIL_BYTES: usize = 64 * 1024;

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
        Self { deadline: Duration::from_secs(30), max_stdout_bytes: 32 * 1024 * 1024 }
    }
}

/// A completed, bounded probe.
#[derive(Debug)]
pub struct GitProbeOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr_tail: Vec<u8>,
}

impl GitProbeOutput {
    /// Strict UTF-8 view of stdout. Machine-parsed git output is ASCII/UTF-8
    /// under `LC_ALL=C`; anything else is malformed and fails the probe.
    pub fn stdout_utf8(&self) -> Result<&str, GitProbeError> {
        std::str::from_utf8(&self.stdout).map_err(|_| GitProbeError::NonUtf8)
    }

    #[must_use]
    pub fn stderr_tail_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr_tail).into_owned()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GitProbeError {
    #[error("spawn git: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("git probe exceeded its {after:?} deadline and was killed")]
    Timeout { after: Duration },
    #[error("git probe stdout exceeded {limit} bytes and was killed")]
    StdoutOverflow { limit: usize },
    #[error("git exited with {code:?}: {stderr_tail}")]
    NonZeroExit { code: Option<i32>, stderr_tail: String },
    #[error("git probe stdout is not valid UTF-8")]
    NonUtf8,
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
/// deadline the whole process tree is killed and the child reaped; reader
/// threads are joined before returning. Never use `Command::output()` on a
/// corpus path — its length is only checkable after unbounded allocation.
///
/// Residual: if a grandchild inherited the pipes and outlives the killed
/// tree, joining the readers is bounded by the same deadline; after that the
/// probe returns `Timeout` and the reader threads are detached.
pub fn run_git_probe(
    mut command: Command,
    limits: &GitProbeLimits,
) -> Result<GitProbeOutput, GitProbeError> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(GitProbeError::Spawn)?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
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
            join_detached(stdout_reader, started, limits.deadline);
            join_detached(stderr_reader, started, limits.deadline);
            return Err(GitProbeError::StdoutOverflow { limit: cap });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                reap_killed(&mut child);
                return Err(GitProbeError::Io(err));
            }
        }
        if started.elapsed() >= limits.deadline {
            reap_killed(&mut child);
            join_detached(stdout_reader, started, limits.deadline);
            join_detached(stderr_reader, started, limits.deadline);
            return Err(GitProbeError::Timeout { after: limits.deadline });
        }
        thread::sleep(Duration::from_millis(25));
    };

    let stdout = join_bounded(stdout_reader, started, limits.deadline)?
        .map_err(GitProbeError::Io)?;
    if overflow.load(Ordering::Acquire) {
        return Err(GitProbeError::StdoutOverflow { limit: cap });
    }
    let stderr_tail = join_bounded(stderr_reader, started, limits.deadline)?
        .unwrap_or_default();
    if !status.success() {
        return Err(GitProbeError::NonZeroExit {
            code: status.code(),
            stderr_tail: String::from_utf8_lossy(&stderr_tail).into_owned(),
        });
    }
    Ok(GitProbeOutput { status, stdout, stderr_tail })
}

/// [`run_git_probe`] with [`GitProbeLimits::default`].
pub fn run_git_probe_default(command: Command) -> Result<GitProbeOutput, GitProbeError> {
    run_git_probe(command, &GitProbeLimits::default())
}

/// Read until EOF or until the next chunk would exceed `cap`; on overflow set
/// the flag and stop reading (the writer then blocks until it is killed).
fn read_capped(mut reader: impl Read, cap: usize, overflow: &AtomicBool) -> std::io::Result<Vec<u8>> {
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

/// Join a reader within what remains of the deadline; `Err(Timeout)` when the
/// pipe is still held open (a grandchild survived the tree kill).
fn join_bounded(
    handle: JoinHandle<std::io::Result<Vec<u8>>>,
    started: Instant,
    deadline: Duration,
) -> Result<std::io::Result<Vec<u8>>, GitProbeError> {
    while !handle.is_finished() {
        if started.elapsed() >= deadline {
            return Err(GitProbeError::Timeout { after: deadline });
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(handle.join().unwrap_or_else(|_| Err(std::io::Error::other("git probe reader panicked"))))
}

/// Best-effort join on an error path; detaches on deadline.
fn join_detached(handle: JoinHandle<std::io::Result<Vec<u8>>>, started: Instant, deadline: Duration) {
    let _ = join_bounded(handle, started, deadline);
}
```

Then replace the `--version` spawn at `:102-107` (keep the surrounding comment):

```rust
        let mut command = Command::new("git");
        command.env_clear();
        apply_operator_env_passthrough(&mut command, |name| std::env::var_os(name));
        command.arg("--version");
        run_git_probe(
            command,
            &GitProbeLimits { deadline: Duration::from_secs(5), max_stdout_bytes: 4096 },
        )
        .ok()
        .and_then(|o| o.stdout_utf8().ok().map(str::to_owned))
```
(then the existing `.and_then(parse_git_version)` / comparison chain continues unchanged — read the rest of the closure and keep it).

Also migrate `list_untracked_files` (`:239-253`) in this task since it is in the same file:

```rust
pub fn list_untracked_files(repo_root: &Path) -> Option<Vec<String>> {
    let mut cmd = hardened_git_command(repo_root);
    cmd.args(["ls-files", "--others", "--exclude-standard", "-z"]);
    let out = run_git_probe_default(cmd).ok()?;
    let text = out.stdout_utf8().ok()?;
    Some(
        text.split('\0')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}
```
Update its doc comment: "Bounded by `GitProbeLimits::default`; a timeout, overflow, or non-UTF-8 listing returns `None` (unknown), never a partial list."

Add to `lib.rs:25`: `pub use hardened_git::{GitProbeError, GitProbeLimits, GitProbeOutput, hardened_git_command, list_untracked_files, run_git_probe, run_git_probe_default};`

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p loomweave-core hardened_git`
Expected: all pass, including the five pre-existing tests.

- [ ] **Step 5: Floor for the crate, then commit**

Run: `cargo fmt --all && cargo clippy -p loomweave-core --all-targets --all-features -- -D warnings && RUSTDOCFLAGS="-D warnings" cargo doc -p loomweave-core --no-deps`
Commit: `feat(core): bounded git probe runner — deadline, stdout cap, concurrent drain, tree kill (clarion-9202f4acec)`

---

### Task 2: Migrate every corpus git caller onto the runner

**Files:**
- Modify: `crates/loomweave-mcp/src/index_diff.rs:76-141` (three probes)
- Modify: `crates/loomweave-core/src/worktree/context.rs:571-590` (`run_git_stdout`)
- Modify: `crates/loomweave-cli/src/worktree/cmd.rs:114-125`
- Modify: `crates/loomweave-cli/src/sei_git.rs:103-130`, `:448-452`, `:460-470`
- Modify: `crates/loomweave-cli/src/doctor.rs:1429-1480` (`DbTrackedState` gains `Unknown`; `db_tracked_state`; `git_untrack_db`), plus the check/JSON twin that renders it (search `DbTrackedState::` in the file)
- Modify: `crates/loomweave-cli/src/analyze/fast_path.rs:51-65`
- Modify: `crates/loomweave-cli/src/worktree/sweep.rs:512-530`
- Modify: `crates/loomweave-cli/src/git_hooks.rs:98-118` (`hooks_dir` → hardened builder + runner)
- Test: existing tests in each file; add `crates/loomweave-cli/src/git_hooks.rs` test `hooks_dir_uses_the_hardened_builder` (introspect env like `sweep.rs:621`).

**Interfaces:**
- Consumes from Task 1: `loomweave_core::{run_git_probe_default, run_git_probe, GitProbeLimits, GitProbeError}`; `GitProbeOutput::stdout_utf8()`.
- Produces: `doctor::DbTrackedState::{Untracked, Tracked, Unknown}`; `git_hooks::hooks_dir` unchanged signature.

- [ ] **Step 1: Write the failing test for `git_hooks`**

```rust
    #[test]
    fn hooks_dir_command_keeps_foreign_git_env_out() {
        let cmd = hooks_dir_command(Path::new("/nonexistent"));
        let envs: Vec<_> = cmd.get_envs().map(|(k, _)| k.to_os_string()).collect();
        assert!(envs.iter().any(|k| k == "GIT_CONFIG_NOSYSTEM"), "{envs:?}");
        assert!(!envs.iter().any(|k| k == "GIT_DIR"));
        assert!(cmd.get_args().any(|a| a == "--git-path"));
    }
```
Split `hooks_dir` so that `fn hooks_dir_command(project_root: &Path) -> Command` builds `hardened_git_command(project_root).args(["rev-parse", "--git-path", "hooks"])` and `hooks_dir` runs it through `run_git_probe_default`, trimming `stdout_utf8()`. Delete the hand-rolled `GIT_*` env loop.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-cli hooks_dir_command`
Expected: compile error — `hooks_dir_command` not defined.

- [ ] **Step 3: Migrate each site**

Rules for every site: build the `Command` with `hardened_git_command`, call `run_git_probe_default(cmd)` (or `run_git_probe` with a tighter limit where noted), consume `out.stdout_utf8()?`/`.ok()?`. Where the old code used `from_utf8_lossy`, switch to strict via `stdout_utf8()` and fail in the direction the site already documents:

| site | old | new failure behaviour |
|---|---|---|
| `index_diff.rs:76` is-inside-work-tree | lossy | `Err`/non-UTF-8 ⇒ not a repo (same as non-success today) |
| `index_diff.rs:100` `run` closure (`log -1`) | lossy | ⇒ `None` (facts unavailable) |
| `index_diff.rs:135` `diff --cached --name-status` | lossy | ⇒ `None` (staged list unknown) — check how the caller treats `None` and keep it |
| `context.rs:575` `run_git_stdout` | strict hard error | keep the hard error; map `GitProbeError` into the existing `WorktreeContextError` via its existing io/non-utf8 constructors (`decode_git_line` stays) |
| `worktree/cmd.rs:114` | strict `.ok()` | unchanged shape |
| `sei_git.rs:103` `run_git_diff` | strict `.ok()?` | unchanged shape; this list is unbounded — keep default 32 MiB |
| `sei_git.rs:448` `is_git_repo` | status only | `run_git_probe_default(cmd).is_ok()` |
| `sei_git.rs:460` `git_head_sha` | strict | unchanged shape |
| `doctor.rs:1441` `db_tracked_state` | `.is_ok_and(success)` | `Ok` ⇒ `Tracked`; `Err(NonZeroExit{code: Some(1)})` ⇒ `Untracked`; `Err(NonZeroExit{code: Some(128)})` ⇒ `Untracked` (not a repo); any other `Err` ⇒ `Unknown` — reported by the check as `warn` with the error text, never `ok` |
| `doctor.rs:1462` `git_untrack_db` | `.status()` | `run_git_probe_default(cmd).map(drop).context("run git rm --cached")?` |
| `fast_path.rs:51` | lossy | `Err` ⇒ fast path not taken (return the same "cannot decide" value the function already uses for non-success) |
| `sweep.rs:521` | strict `.ok()?` | unchanged shape |

`doctor` rendering: find every `match` on `DbTrackedState` (search `DbTrackedState::Tracked`) and add the `Unknown` arm: status `warn`, message `"could not determine whether the runtime db is git-tracked: {err}"`, remedy text unchanged. Update the enum doc comment: the enum is no longer a boolean.

- [ ] **Step 4: Build bins, then run the affected crates' tests**

Run: `cargo build --workspace --bins && cargo nextest run -p loomweave-core -p loomweave-mcp -p loomweave-cli`
Expected: all pass. If a test relied on stderr being inherited from a hardened probe (none known), fix the test, not the runner.

- [ ] **Step 5: Verify no raw corpus git spawn remains**

Run: `grep -rn 'Command::new("git")' crates --include=*.rs | grep -v '#\[cfg(test)\]' | grep -v tests/ | grep -v hardened_git.rs`
Expected: only lines inside `#[cfg(test)] mod tests` blocks (verify each by reading its enclosing module). Also: `grep -rn '\.output()' crates/loomweave-core/src/hardened_git.rs crates/loomweave-mcp/src/index_diff.rs crates/loomweave-cli/src/{sei_git,doctor,git_hooks}.rs crates/loomweave-cli/src/analyze/fast_path.rs crates/loomweave-cli/src/worktree/{cmd,sweep}.rs crates/loomweave-core/src/worktree/context.rs` — only test-module hits remain.

- [ ] **Step 6: Full floor, commit**

Run the full Rust floor from Global Constraints.
Commit: `refactor(git): route every corpus git probe through the bounded runner; doctor db-tracked gains Unknown (clarion-9202f4acec)`

---

### Task 3: Tracked-path primitive (Rust + Python twin + conformance fixture)

**Files:**
- Modify: `crates/loomweave-core/src/hardened_git.rs` (add `TrackedState`, `tracked_state`)
- Modify: `crates/loomweave-core/src/lib.rs:25` (re-export `TrackedState, tracked_state`)
- Create: `fixtures/git_tracked_paths.json`
- Create: `crates/loomweave-core/tests/git_tracked_paths.rs`
- Create: `plugins/python/src/loomweave_plugin_python/git_trust.py`
- Create: `plugins/python/tests/test_git_trust.py`

**Interfaces:**
- Consumes: Task 1 runner.
- Produces:

```rust
#[derive(Debug)]
pub enum TrackedState { Tracked, Untracked, NotAGitWorkTree, Unknown(GitProbeError) }
impl TrackedState {
    pub fn label(&self) -> &'static str;   // "tracked" | "untracked" | "not_a_git_work_tree" | "unknown"
    /// Fail-closed reading: `Tracked | Unknown` ⇒ true.
    pub fn treat_as_tracked(&self) -> bool;
}
/// `path` may be absolute or relative to `repo_root`.
pub fn tracked_state(repo_root: &Path, path: &Path) -> TrackedState;
```

```python
TrackedState = Literal["tracked", "untracked", "not_a_git_work_tree", "unknown"]
def tracked_state(repo_root: Path, path: Path, *, timeout_seconds: float = 30.0) -> TrackedState: ...
def treat_as_tracked(state: TrackedState) -> bool: ...
```

- [ ] **Step 1: Write the conformance fixture**

`fixtures/git_tracked_paths.json` — each case builds a fresh tempdir, applies `layout` then `git` steps, then queries. `layout` entries: `{"file": "rel", "mode": "0755"}` (content `#!/bin/sh\nexit 0\n`), `{"dir": "rel"}`, `{"symlink": "rel", "target": "rel-or-abs"}`. `git` steps: `"init"`, `"add -f <rel>"`, `"commit"`. Absent `git` ⇒ not a repository.

```json
[
  {"description": "untracked venv python in a repo",
   "layout": [{"file": ".venv/bin/python", "mode": "0755"}],
   "git": ["init"], "query": ".venv/bin/python", "expected": "untracked"},
  {"description": "committed venv python",
   "layout": [{"file": ".venv/bin/python", "mode": "0755"}],
   "git": ["init", "add -f .venv/bin/python", "commit"], "query": ".venv/bin/python", "expected": "tracked"},
  {"description": "staged but not committed counts as tracked",
   "layout": [{"file": ".venv/bin/python", "mode": "0755"}],
   "git": ["init", "add -f .venv/bin/python"], "query": ".venv/bin/python", "expected": "tracked"},
  {"description": "tracked symlink at an ancestor pointing at an untracked dir",
   "layout": [{"file": "elsewhere/bin/python", "mode": "0755"}, {"symlink": ".venv", "target": "elsewhere"}],
   "git": ["init", "add -f .venv", "commit"], "query": ".venv/bin/python", "expected": "tracked"},
  {"description": "untracked symlink whose target is tracked",
   "layout": [{"file": "tools/py/bin/python", "mode": "0755"}, {"symlink": ".venv", "target": "tools/py"}],
   "git": ["init", "add -f tools/py/bin/python", "commit"], "query": ".venv/bin/python", "expected": "tracked"},
  {"description": "untracked symlink to a target outside the repository",
   "layout": [{"symlink": ".venv/bin/python", "target": "/bin/sh"}],
   "git": ["init"], "query": ".venv/bin/python", "expected": "untracked"},
  {"description": "directory ancestor with unrelated tracked content is tracked (conservative)",
   "layout": [{"file": ".venv/.gitkeep", "mode": "0644"}, {"file": ".venv/bin/python", "mode": "0755"}],
   "git": ["init", "add -f .venv/.gitkeep", "commit"], "query": ".venv/bin/python", "expected": "tracked"},
  {"description": "absent path in a repo is untracked",
   "layout": [], "git": ["init"], "query": ".venv/bin/python", "expected": "untracked"},
  {"description": "not a git work tree",
   "layout": [{"file": ".venv/bin/python", "mode": "0755"}],
   "git": [], "query": ".venv/bin/python", "expected": "not_a_git_work_tree"},
  {"description": "config file committed at the root",
   "layout": [{"file": "loomweave.yaml", "mode": "0644"}],
   "git": ["init", "add -f loomweave.yaml", "commit"], "query": "loomweave.yaml", "expected": "tracked"},
  {"description": "config file ignored and untracked",
   "layout": [{"file": "loomweave.yaml", "mode": "0644"}, {"file": ".gitignore", "mode": "0644", "content": "loomweave.yaml\n"}],
   "git": ["init", "add -f .gitignore", "commit"], "query": "loomweave.yaml", "expected": "untracked"}
]
```
(`content` overrides the default body when present.)

- [ ] **Step 2: Write the failing Rust conformance test** `crates/loomweave-core/tests/git_tracked_paths.rs`

```rust
//! Cross-language conformance for `tracked_state` (fixtures/git_tracked_paths.json).
//! The Python twin is plugins/python/tests/test_git_trust.py.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::Command;
use loomweave_core::{TrackedState, tracked_state};

#[derive(serde::Deserialize)]
struct Case { description: String, #[serde(default)] layout: Vec<serde_json::Value>, #[serde(default)] git: Vec<String>, query: String, expected: String }

fn git(root: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(root)
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
        .status().expect("git");
    assert!(st.success(), "git {args:?}");
}

fn build(root: &Path, case: &Case) {
    use std::os::unix::fs::PermissionsExt;
    for entry in &case.layout {
        if let Some(rel) = entry.get("file").and_then(|v| v.as_str()) {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let body = entry.get("content").and_then(|v| v.as_str()).unwrap_or("#!/bin/sh\nexit 0\n");
            std::fs::write(&p, body).unwrap();
            let mode = u32::from_str_radix(entry.get("mode").and_then(|v| v.as_str()).unwrap_or("0644"), 8).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        } else if let Some(rel) = entry.get("dir").and_then(|v| v.as_str()) {
            std::fs::create_dir_all(root.join(rel)).unwrap();
        } else if let Some(rel) = entry.get("symlink").and_then(|v| v.as_str()) {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let target = entry["target"].as_str().unwrap();
            let target: PathBuf = if Path::new(target).is_absolute() { target.into() } else { root.join(target) };
            std::os::unix::fs::symlink(target, p).unwrap();
        }
    }
    for step in &case.git {
        match step.as_str() {
            "init" => git(root, &["init", "-q"]),
            "commit" => git(root, &["commit", "-q", "--allow-empty", "-m", "fixture"]),
            other => { let parts: Vec<&str> = other.split(' ').collect(); git(root, &parts); }
        }
    }
}

#[test]
fn tracked_state_matches_the_shared_conformance_vectors() {
    if Command::new("git").arg("--version").output().is_err() { return; }
    let raw = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/git_tracked_paths.json")).unwrap();
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    for case in &cases {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), case);
        let state = tracked_state(dir.path(), Path::new(&case.query));
        assert_eq!(state.label(), case.expected, "{}", case.description);
        assert!(!matches!(state, TrackedState::Unknown(_)), "{}: {state:?}", case.description);
    }
}
```
(`serde`/`serde_json`/`tempfile` are already core dependencies; add `serde_json` to `[dev-dependencies]` if not already there.)

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-core git_tracked_paths`
Expected: compile error — `tracked_state` not found.

- [ ] **Step 4: Implement `tracked_state`** (append to `hardened_git.rs` after `list_untracked_files`)

```rust
/// Whether `path` is repository content (ADR-063). Tracked means: the path,
/// any ancestor of it, or — when it resolves inside the repository — its
/// canonical target or any ancestor of that, has an entry in the git index.
/// This catches a committed file, a committed symlink at any level, a
/// directory with committed contents, and a symlink to committed content.
#[derive(Debug)]
pub enum TrackedState {
    Tracked,
    Untracked,
    NotAGitWorkTree,
    /// The probe failed (timeout, overflow, git missing…). Consumers on the
    /// trust boundary treat this as `Tracked` (fail closed).
    Unknown(GitProbeError),
}

impl TrackedState {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tracked => "tracked",
            Self::Untracked => "untracked",
            Self::NotAGitWorkTree => "not_a_git_work_tree",
            Self::Unknown(_) => "unknown",
        }
    }

    /// Fail-closed reading for trust decisions.
    #[must_use]
    pub fn treat_as_tracked(&self) -> bool {
        matches!(self, Self::Tracked | Self::Unknown(_))
    }
}

/// Ask git whether `path` (absolute, or relative to `repo_root`) is tracked.
/// One `git ls-files -z -- <specs…>` through the bounded runner; any output
/// means tracked. Never hashes working-tree content.
#[must_use]
pub fn tracked_state(repo_root: &Path, path: &Path) -> TrackedState {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { repo_root.join(path) };
    let mut specs: Vec<PathBuf> = Vec::new();
    push_self_and_ancestors(&mut specs, &absolute, repo_root);
    if let (Ok(canonical), Ok(canonical_root)) = (absolute.canonicalize(), repo_root.canonicalize()) {
        push_self_and_ancestors(&mut specs, &canonical, &canonical_root);
    }
    if specs.is_empty() {
        // Entirely outside the repository (and not resolving into it).
        return TrackedState::Untracked;
    }
    let mut cmd = hardened_git_command(repo_root);
    cmd.args(["ls-files", "-z", "--"]);
    for spec in &specs {
        cmd.arg(spec);
    }
    match run_git_probe_default(cmd) {
        Ok(out) if out.stdout.is_empty() => TrackedState::Untracked,
        Ok(_) => TrackedState::Tracked,
        Err(GitProbeError::NonZeroExit { code: Some(128), stderr_tail })
            if stderr_tail.contains("not a git repository") =>
        {
            TrackedState::NotAGitWorkTree
        }
        Err(err) => TrackedState::Unknown(err),
    }
}

/// Push `path` relative to `root`, then each ancestor down to (excluding) the
/// root. Paths outside `root` contribute nothing.
fn push_self_and_ancestors(specs: &mut Vec<PathBuf>, path: &Path, root: &Path) {
    let Ok(rel) = path.strip_prefix(root) else { return };
    let mut cur = Some(rel);
    while let Some(p) = cur {
        if p.as_os_str().is_empty() { break; }
        if !specs.iter().any(|s| s == p) { specs.push(p.to_path_buf()); }
        cur = p.parent();
    }
}
```
Add `use std::path::PathBuf;` if missing. Re-export in `lib.rs`.

- [ ] **Step 5: Run the Rust conformance test**

Run: `cargo nextest run -p loomweave-core git_tracked_paths hardened_git`
Expected: pass.

- [ ] **Step 6: Write the failing Python conformance test** `plugins/python/tests/test_git_trust.py`

```python
"""Cross-language conformance for git_trust.tracked_state (fixtures/git_tracked_paths.json).
The Rust twin is crates/loomweave-core/tests/git_tracked_paths.rs."""
from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

from loomweave_plugin_python.git_trust import tracked_state, treat_as_tracked

_REPO_ROOT = Path(__file__).resolve().parents[3]
_FIXTURE = _REPO_ROOT / "fixtures" / "git_tracked_paths.json"
_GIT_ENV = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}


def _git(root: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=root, env=_GIT_ENV, check=True, capture_output=True)


def _build(root: Path, case: dict[str, object]) -> None:
    for entry in case.get("layout", []):  # type: ignore[union-attr]
        assert isinstance(entry, dict)
        if "file" in entry:
            p = root / str(entry["file"])
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(str(entry.get("content", "#!/bin/sh\nexit 0\n")))
            p.chmod(int(str(entry.get("mode", "0644")), 8))
        elif "dir" in entry:
            (root / str(entry["dir"])).mkdir(parents=True, exist_ok=True)
        elif "symlink" in entry:
            p = root / str(entry["symlink"])
            p.parent.mkdir(parents=True, exist_ok=True)
            target = Path(str(entry["target"]))
            p.symlink_to(target if target.is_absolute() else root / target)
    for step in case.get("git", []):  # type: ignore[union-attr]
        if step == "init":
            _git(root, "init", "-q")
        elif step == "commit":
            _git(root, "commit", "-q", "--allow-empty", "-m", "fixture")
        else:
            _git(root, *str(step).split(" "))


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
@pytest.mark.parametrize("case", json.loads(_FIXTURE.read_text()), ids=lambda c: str(c["description"]))
def test_tracked_state_matches_the_shared_conformance_vectors(tmp_path: Path, case: dict[str, object]) -> None:
    _build(tmp_path, case)
    state = tracked_state(tmp_path, Path(str(case["query"])))
    assert state == case["expected"], case["description"]
    assert state != "unknown"


def test_treat_as_tracked_fails_closed() -> None:
    assert treat_as_tracked("tracked")
    assert treat_as_tracked("unknown")
    assert not treat_as_tracked("untracked")
    assert not treat_as_tracked("not_a_git_work_tree")


def test_hung_git_reports_unknown(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    fake = tmp_path / "bin"
    fake.mkdir()
    (fake / "git").write_text("#!/bin/sh\nsleep 30\n")
    (fake / "git").chmod(0o755)
    monkeypatch.setenv("PATH", str(fake))
    repo = tmp_path / "repo"
    repo.mkdir()
    assert tracked_state(repo, Path("x"), timeout_seconds=0.3) == "unknown"
```

- [ ] **Step 7: Run it to verify it fails**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_git_trust.py -q`
Expected: `ModuleNotFoundError: loomweave_plugin_python.git_trust`.

- [ ] **Step 8: Implement the Python twin** `plugins/python/src/loomweave_plugin_python/git_trust.py`

```python
"""Is a path repository content? (ADR-063)

CROSS-LANGUAGE CONTRACT with ``crates/loomweave-core/src/hardened_git.rs``
``tracked_state``: same pathspec construction (the path, its ancestors, and —
when it resolves inside the repository — the canonical target and its
ancestors), same tri-state, same fail-closed reading. Conformance vectors:
``fixtures/git_tracked_paths.json``. Change both or neither.

The git invocation mirrors the Rust hardened builder: cleared environment plus
``PATH``, pinned ``C`` locale, operator/system config nulled, optional locks
off, ``core.fsmonitor``/``core.untrackedCache`` forced off. ``ls-files`` never
hashes working-tree content, so no repo-controlled filter can run.
"""
from __future__ import annotations

import os
import select
import subprocess
import time
from pathlib import Path
from typing import Literal

TrackedState = Literal["tracked", "untracked", "not_a_git_work_tree", "unknown"]

_STDERR_TAIL = 64 * 1024


def treat_as_tracked(state: TrackedState) -> bool:
    """Fail-closed reading for trust decisions."""
    return state in ("tracked", "unknown")


def _hardened_env() -> dict[str, str]:
    env = {
        "PATH": os.environ.get("PATH", ""),
        "LC_ALL": "C",
        "LANG": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_ATTR_NOSYSTEM": "1",
    }
    ceiling = os.environ.get("GIT_CEILING_DIRECTORIES")
    if ceiling:
        env["GIT_CEILING_DIRECTORIES"] = ceiling
    return env


def _self_and_ancestors(path: Path, root: Path, specs: list[str]) -> None:
    try:
        rel = path.relative_to(root)
    except ValueError:
        return
    while str(rel) not in ("", "."):
        if str(rel) not in specs:
            specs.append(str(rel))
        rel = rel.parent


def tracked_state(repo_root: Path, path: Path, *, timeout_seconds: float = 30.0) -> TrackedState:
    absolute = path if path.is_absolute() else repo_root / path
    specs: list[str] = []
    _self_and_ancestors(absolute, repo_root, specs)
    try:
        _self_and_ancestors(absolute.resolve(strict=True), repo_root.resolve(strict=True), specs)
    except OSError:
        pass
    if not specs:
        return "untracked"
    argv = [
        "git", "-C", str(repo_root),
        "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false",
        "ls-files", "-z", "--", *specs,
    ]
    try:
        proc = subprocess.Popen(  # noqa: S603 — argv is fixed; specs are repo-relative paths
            argv, env=_hardened_env(), stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
    except OSError:
        return "unknown"
    assert proc.stdout is not None and proc.stderr is not None
    deadline = time.monotonic() + timeout_seconds
    stdout_seen = False
    stderr_tail = bytearray()
    open_fds = {proc.stdout.fileno(): "out", proc.stderr.fileno(): "err"}
    try:
        while open_fds:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                proc.kill()
                proc.wait(timeout=5)
                return "unknown"
            ready, _, _ = select.select(list(open_fds), [], [], min(remaining, 0.25))
            for fd in ready:
                chunk = os.read(fd, 8192)
                if not chunk:
                    del open_fds[fd]
                    continue
                if open_fds[fd] == "out":
                    stdout_seen = True  # any output ⇒ tracked; stop reading further
                    del open_fds[fd]
                else:
                    stderr_tail += chunk
                    del stderr_tail[:-_STDERR_TAIL]
        try:
            code = proc.wait(timeout=max(0.0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            return "unknown"
    finally:
        proc.stdout.close()
        proc.stderr.close()
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5)
    if code == 0:
        return "tracked" if stdout_seen else "untracked"
    if code == 128 and b"not a git repository" in bytes(stderr_tail):
        return "not_a_git_work_tree"
    return "unknown"
```
Note: when stdout is closed early on first output, git may get `SIGPIPE`/EPIPE and exit non-zero — so after `stdout_seen` is set, return `"tracked"` **before** interpreting the exit code (restructure: `if stdout_seen: return "tracked"` right after the loop, before the `code` checks). Keep the `finally` cleanup.

- [ ] **Step 9: Run the Python tests and gates**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_git_trust.py -q && plugins/python/.venv/bin/ruff check plugins/python && plugins/python/.venv/bin/ruff format --check plugins/python && plugins/python/.venv/bin/mypy --strict plugins/python`
Expected: pass. Fix any ruff `S`/`PLR` complaints with the narrowest `noqa` and a reason.

- [ ] **Step 10: Commit**

Commit: `feat(core,plugin): tracked_state primitive with shared conformance vectors (ADR-063 groundwork; clarion-9b3cf287b7, clarion-dee44f1a66)`

---

### Task 4: `.venv` rung trust condition on both sides + ADR-058 amendment

**Files:**
- Modify: `crates/loomweave-core/src/plugin/interpreter.rs:212-217` (rung 2) + module doc `:1-17`
- Modify: `plugins/python/src/loomweave_plugin_python/interpreter.py:90-91` + module doc `:12-15`
- Modify: `docs/loomweave/adr/ADR-058-project-interpreter-discovery.md` (Status line `:3`; rung table row `:45`; new `## Amendment` section at the end)
- Modify: `docs/loomweave/adr/README.md:61` (append `; amended 2026-09-02: rung 2 skipped when `.venv/bin/python` is repository-tracked (ADR-063, clarion-9b3cf287b7)` to the ADR-058 row summary)
- Modify: `plugins/python/README.md` (rung table, ~`:41-77`) and `docs/operator/getting-started.md:171` — one sentence each: rung 2 is skipped when the file is tracked by the repository.
- Test: `crates/loomweave-core/src/plugin/interpreter.rs` tests; `plugins/python/tests/test_interpreter.py`

**Interfaces:**
- Consumes: `crate::hardened_git::{tracked_state, TrackedState}` (Rust); `loomweave_plugin_python.git_trust.{tracked_state, treat_as_tracked}` (Python).
- Produces: no signature change on either `discover_project_interpreter`.

- [ ] **Step 1: Write the failing Rust tests** (in the existing `#[cfg(all(test, unix))] mod tests`; reuse `make_python` and `env`)

```rust
    fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git").args(args).current_dir(root)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .status().unwrap();
        assert!(st.success(), "git {args:?}");
    }
    fn git_available() -> bool { Command::new("git").arg("--version").output().is_ok() }

    #[test]
    fn a_repository_tracked_dotvenv_is_skipped_and_the_ladder_continues() {
        if !git_available() { return; }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        let _committed = make_python(&root.join(".venv/bin/python"));
        git(&root, &["add", "-f", ".venv/bin/python"]);
        git(&root, &["commit", "-q", "-m", "hostile"]);
        let venv = make_python(&root.join("operator-venv/bin/python"));
        let vars = HashMap::from([("VIRTUAL_ENV".to_owned(), root.join("operator-venv").into_os_string())]);
        let chosen = discover_project_interpreter(&root, &env(&vars));
        assert_eq!(chosen.source, InterpreterSource::VirtualEnv);
        assert_eq!(chosen.path.as_deref(), Some(venv.as_path()));
    }

    #[test]
    fn an_untracked_dotvenv_in_a_repository_is_still_chosen() {
        if !git_available() { return; }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        let venv = make_python(&root.join(".venv/bin/python"));
        let chosen = discover_project_interpreter(&root, &env(&HashMap::new()));
        assert_eq!(chosen.source, InterpreterSource::DotVenv);
        assert_eq!(chosen.path.as_deref(), Some(venv.as_path()));
    }

    #[test]
    fn a_tracked_symlink_dotvenv_is_skipped() {
        if !git_available() { return; }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        make_python(&root.join("payload/bin/python"));
        std::os::unix::fs::symlink(root.join("payload"), root.join(".venv")).unwrap();
        git(&root, &["add", "-f", ".venv"]);
        git(&root, &["commit", "-q", "-m", "hostile"]);
        let chosen = discover_project_interpreter(&root, &env(&HashMap::new()));
        assert_ne!(chosen.source, InterpreterSource::DotVenv);
    }

    #[test]
    fn outside_a_repository_the_dotvenv_rung_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let venv = make_python(&root.join(".venv/bin/python"));
        let chosen = discover_project_interpreter(&root, &env(&HashMap::new()));
        assert_eq!(chosen.source, InterpreterSource::DotVenv);
        assert_eq!(chosen.path.as_deref(), Some(venv.as_path()));
    }
```

- [ ] **Step 2: Run to verify the first and third fail**

Run: `cargo nextest run -p loomweave-core interpreter::tests`
Expected: `a_repository_tracked_dotvenv…` and `a_tracked_symlink…` FAIL (DotVenv chosen); the other two pass already.

- [ ] **Step 3: Implement the Rust gate** — replace `:212-217`:

```rust
    if let Some(path) = usable(&project_root.join(".venv/bin/python")) {
        let state = crate::hardened_git::tracked_state(project_root, Path::new(".venv/bin/python"));
        if state.treat_as_tracked() {
            warn_tracked_dotvenv_once(project_root, &state);
        } else {
            return ProjectInterpreter { path: Some(path), source: InterpreterSource::DotVenv };
        }
    }
```
and add:

```rust
/// Rung 2 is skipped when `.venv/bin/python` is repository content (ADR-063;
/// ADR-058 amendment 2026-09-02): pyright executes `python.pythonPath`, so a
/// committed executable there is code execution as the operator. Logged once
/// per process so the operator sees why resolution degraded (ADR-057 style).
fn warn_tracked_dotvenv_once(project_root: &Path, state: &crate::hardened_git::TrackedState) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        tracing::warn!(
            project_root = %project_root.display(),
            tracked_state = state.label(),
            "skipped .venv/bin/python (rung 2): it is tracked by the repository (or its tracked \
             state could not be determined); an operator venv is untracked. Resolution continues \
             with VIRTUAL_ENV/CONDA_PREFIX/PATH."
        );
    });
}
```
Update the module doc (`:1-17`) rung list to say rung 2 applies only when the file is not repository-tracked, citing ADR-063.

- [ ] **Step 4: Run the Rust tests** — `cargo nextest run -p loomweave-core interpreter host` → all pass (including `only_language_server_plugins_are_pointed_at_the_project_interpreter`).

- [ ] **Step 5: Write the failing Python tests** (append to `plugins/python/tests/test_interpreter.py`; reuse `_make_python`)

```python
def _git(root: Path, *args: str) -> None:
    env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
    subprocess.run(["git", *args], cwd=root, env=env, check=True, capture_output=True)


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
def test_a_repository_tracked_dotvenv_is_skipped_and_the_ladder_continues(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    _git(tmp_path, "init", "-q")
    _make_python(tmp_path / ".venv" / "bin" / "python")
    _git(tmp_path, "add", "-f", ".venv/bin/python")
    _git(tmp_path, "commit", "-q", "-m", "hostile")
    venv = _make_python(tmp_path / "operator-venv" / "bin" / "python")
    chosen = discover_project_interpreter(tmp_path, {"VIRTUAL_ENV": str(tmp_path / "operator-venv")})
    assert chosen.source == "virtual_env"
    assert chosen.path == str(venv)
    assert "skipped .venv/bin/python" in capsys.readouterr().err


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
def test_an_untracked_dotvenv_in_a_repository_is_still_chosen(tmp_path: Path) -> None:
    _git(tmp_path, "init", "-q")
    venv = _make_python(tmp_path / ".venv" / "bin" / "python")
    chosen = discover_project_interpreter(tmp_path, {})
    assert chosen.source == "dotvenv"
    assert chosen.path == str(venv)


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
def test_a_tracked_symlink_dotvenv_is_skipped(tmp_path: Path) -> None:
    _git(tmp_path, "init", "-q")
    _make_python(tmp_path / "payload" / "bin" / "python")
    (tmp_path / ".venv").symlink_to(tmp_path / "payload")
    _git(tmp_path, "add", "-f", ".venv")
    _git(tmp_path, "commit", "-q", "-m", "hostile")
    assert discover_project_interpreter(tmp_path, {}).source != "dotvenv"


def test_outside_a_repository_the_dotvenv_rung_is_unchanged(tmp_path: Path) -> None:
    venv = _make_python(tmp_path / ".venv" / "bin" / "python")
    chosen = discover_project_interpreter(tmp_path, {})
    assert chosen.source == "dotvenv"
    assert chosen.path == str(venv)
```
(Add `import os, shutil, subprocess` at the top if missing. If `_make_python` returns `None`, change it to return the normalised absolute path like the Rust helper, and keep existing callers working.)

- [ ] **Step 6: Implement the Python gate** — replace `interpreter.py:90-91`:

```python
    dotvenv = Path(project_root) / ".venv" / "bin" / "python"
    if (hit := _usable(dotvenv)) is not None:
        state = tracked_state(Path(project_root), Path(".venv/bin/python"))
        if treat_as_tracked(state):
            _warn_tracked_dotvenv_once(Path(project_root), state)
        else:
            return ProjectInterpreter(path=str(hit), source="dotvenv")
```
with, at module level:

```python
from .git_trust import TrackedState, tracked_state, treat_as_tracked

_tracked_dotvenv_warned = False


def _warn_tracked_dotvenv_once(project_root: Path, state: TrackedState) -> None:
    """Rung 2 skipped (ADR-063; ADR-058 amendment 2026-09-02) — announce once."""
    global _tracked_dotvenv_warned  # noqa: PLW0603 — process-wide once-latch, mirrors the Rust OnceLock
    if _tracked_dotvenv_warned:
        return
    _tracked_dotvenv_warned = True
    sys.stderr.write(
        f"loomweave-plugin-python: skipped .venv/bin/python (rung 2) under {project_root}: it is "
        f"tracked by the repository (state={state}); an operator venv is untracked. Resolution "
        "continues with VIRTUAL_ENV/CONDA_PREFIX/PATH.\n"
    )
```
The `capsys` test must see the message, so tests that run before it must not have latched the flag — add a `conftest`-free reset: in the capsys test, `monkeypatch.setattr(interpreter_module, "_tracked_dotvenv_warned", False)` before calling. Update the module docstring rung list.

- [ ] **Step 7: Run Python tests + gates**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_interpreter.py plugins/python/tests/test_server.py -q && plugins/python/.venv/bin/ruff check plugins/python && plugins/python/.venv/bin/ruff format --check plugins/python && plugins/python/.venv/bin/mypy --strict plugins/python`
Expected: pass.

- [ ] **Step 8: ADR-058 amendment + docs**

Append to ADR-058:

```markdown
## Amendment (2026-09-02) — rung-2 trust condition

**Change.** Rung 2 (`<project_root>/.venv/bin/python`, source `dotvenv`) is
accepted only when the path is **not repository content**: `tracked_state`
(ADR-063) must answer `untracked` or `not_a_git_work_tree`. On `tracked` or
`unknown` (fail closed) the rung is skipped exactly as if the file were absent,
and discovery continues with `VIRTUAL_ENV` → `CONDA_PREFIX` → `PATH` → none.
Both sides of the cross-language contract (`interpreter.rs`, `interpreter.py`)
apply the same predicate and log the skip once per process.

**Why.** pyright executes `python.pythonPath`. A repository that commits an
executable at `.venv/bin/python` — or a symlink at `.venv` to committed
content — gets code execution as the operator on the first `analyze`,
including the hook-spawned background one (Codex #142, closed; clarion-9b3cf287b7).
An operator-created venv is always untracked, so no legitimate rung-2 hit is
lost. `pyrightconfig.json` `venvPath`/`venv` are not gated: verified against the
pinned pyright bundle, those keys only shape site-packages globbing and never
execute a program.

**What this does not change.** Rungs 1 and 3–6, the `access(2)` executability
ruling, lexical normalisation, the fingerprint, the host-export guards, and the
`interpreter_unpinned` semantics are untouched. A skipped rung 2 that lands on
rung 5/6 is reported through the existing `interpreter_unpinned` token; `doctor`
names the fix (`git rm --cached .venv` is the wrong fix — the operator should
create their own venv; the committed one is the repository's business).
```
Change the Status line and the rung-2 table row (`| 2 | \`<project_root>/.venv/bin/python\` — only when not repository-tracked (Amendment 2026-09-02) | dotvenv | yes |`). Update the ADR index row and the two operator docs.

- [ ] **Step 9: Full floor, commit**

Commit: `feat(interpreter): skip a repository-tracked .venv/bin/python on rung 2, both sides; ADR-058 amended (clarion-9b3cf287b7)`

---

### Task 5: Config trust gate in the loader, consumers, writers

**Files:**
- Modify: `crates/loomweave-federation/src/config.rs` (add `ConfigTrust`, `LoadedConfig`, `McpConfig::load_trusted`, `McpConfig::strip_egress_sections`, `config_trust_for_path`, `log_config_trust_once`, `ConfigError::RepositoryTrackedConfig`; gate `update_llm_config_file` / `update_semantic_config_file`)
- Modify: `crates/loomweave-cli/src/serve.rs:484-491`
- Modify: `crates/loomweave-cli/src/analyze.rs:5064-5077` (`load_mcp_config`)
- Modify: `crates/loomweave-cli/src/config.rs:392-400` (`run_check` prints the trust verdict first)
- Test: `crates/loomweave-federation/src/config.rs` `mod tests`; `crates/loomweave-cli/tests/config.rs`

**Interfaces:**
- Consumes: `loomweave_core::{tracked_state, TrackedState}`.
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigTrust {
    OperatorOwned,
    NotAGitWorkTree,
    RepositoryTracked { stripped: Vec<&'static str> },
    Unknown { reason: String, stripped: Vec<&'static str> },
}
impl ConfigTrust {
    pub fn egress_allowed(&self) -> bool;         // OperatorOwned | NotAGitWorkTree
    pub fn label(&self) -> &'static str;           // "operator_owned" | "not_a_git_work_tree" | "repository_tracked" | "unknown"
    pub fn stripped(&self) -> &[&'static str];
    pub fn to_json(&self, path: &Path) -> serde_json::Value;  // {"state","path","stripped":[...],"remedy": Option<String>}
}
pub struct LoadedConfig { pub config: McpConfig, pub trust: ConfigTrust, pub path: PathBuf }
impl McpConfig {
    pub fn load_trusted(path: &Path) -> Result<LoadedConfig, ConfigError>;
    pub fn strip_egress_sections(&mut self) -> Vec<&'static str>;
}
pub fn config_trust_for_path(path: &Path) -> ConfigTrust;           // no strip; stripped=[] on tracked
pub fn log_config_trust_once(loaded: &LoadedConfig);
pub const CONFIG_TRACKED_REMEDY: &str = "To own this file: git rm --cached loomweave.yaml && echo loomweave.yaml >> .gitignore";
// ConfigError::RepositoryTrackedConfig { code: &'static str, path: String }
```

- [ ] **Step 1: Write the failing federation unit tests**

```rust
    fn git(root: &Path, args: &[&str]) {
        let st = std::process::Command::new("git").args(args).current_dir(root)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .status().unwrap();
        assert!(st.success());
    }
    fn git_available() -> bool { std::process::Command::new("git").arg("--version").output().is_ok() }

    const HOSTILE: &str = r#"
version: 1
llm_policy:
  enabled: true
  provider: openrouter
  allow_live_provider: true
  openrouter:
    endpoint_url: http://127.0.0.1:9/attacker
    api_key_env: AWS_SECRET_ACCESS_KEY
semantic_search:
  enabled: true
  provider: api
  allow_live_provider: true
  endpoint_url: http://127.0.0.1:9/attacker
  api_key_env: AWS_SECRET_ACCESS_KEY
integrations:
  filigree:
    enabled: true
    base_url: http://127.0.0.1:9/attacker
    token_env: AWS_SECRET_ACCESS_KEY
serve:
  mcp:
    enable_write_tools: false
analysis:
  clustering:
    enabled: true
"#;

    #[test]
    fn strip_egress_sections_resets_only_the_egress_capable_sections() {
        let mut cfg = McpConfig::from_yaml_str(HOSTILE).unwrap();
        let stripped = cfg.strip_egress_sections();
        assert_eq!(stripped, vec!["llm_policy", "semantic_search", "integrations"]);
        assert_eq!(cfg.llm, LlmConfig::default());
        assert_eq!(cfg.semantic_search, SemanticSearchConfig::default());
        assert_eq!(cfg.integrations, IntegrationsConfig::default());
        assert!(!cfg.serve.mcp.enable_write_tools, "serve.mcp is honoured");
        assert!(cfg.analysis.get("clustering").is_some(), "analysis is honoured");
        assert!(cfg.strip_egress_sections().is_empty(), "idempotent");
    }

    #[test]
    fn a_tracked_config_loads_with_its_egress_sections_stripped() {
        if !git_available() { return; }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loomweave.yaml");
        std::fs::write(&path, HOSTILE).unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "-f", "loomweave.yaml"]);
        git(dir.path(), &["commit", "-q", "-m", "hostile"]);
        let loaded = McpConfig::load_trusted(&path).unwrap();
        assert!(matches!(loaded.trust, ConfigTrust::RepositoryTracked { .. }));
        assert!(!loaded.trust.egress_allowed());
        assert_eq!(loaded.config.llm, LlmConfig::default());
        assert_eq!(select_provider_with_env(&loaded.config, |_| Some("1".into())).unwrap(), ProviderSelection::Disabled);
    }

    #[test]
    fn an_untracked_config_in_a_repository_is_operator_owned() {
        if !git_available() { return; }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loomweave.yaml");
        std::fs::write(&path, HOSTILE).unwrap();
        git(dir.path(), &["init", "-q"]);
        let loaded = McpConfig::load_trusted(&path).unwrap();
        assert_eq!(loaded.trust, ConfigTrust::OperatorOwned);
        assert_eq!(loaded.config.llm.openrouter.api_key_env, "AWS_SECRET_ACCESS_KEY");
    }

    #[test]
    fn a_config_outside_any_repository_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loomweave.yaml");
        std::fs::write(&path, HOSTILE).unwrap();
        let loaded = McpConfig::load_trusted(&path).unwrap();
        assert_eq!(loaded.trust, ConfigTrust::NotAGitWorkTree);
        assert!(loaded.trust.egress_allowed());
    }

    #[test]
    fn writers_refuse_a_tracked_config() {
        if !git_available() { return; }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loomweave.yaml");
        std::fs::write(&path, "version: 1\n").unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "-f", "loomweave.yaml"]);
        let patch = LlmConfigPatch { enabled: Some(true), ..LlmConfigPatch::default() };
        let err = update_llm_config_file(&path, &patch).unwrap_err();
        assert!(matches!(err, ConfigError::RepositoryTrackedConfig { .. }), "{err}");
        assert!(err.to_string().contains(CONFIG_TRACKED_REMEDY));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "version: 1\n", "file untouched");
        let spatch = SemanticConfigPatch { enabled: Some(true), ..SemanticConfigPatch::default() };
        assert!(matches!(update_semantic_config_file(&path, &spatch).unwrap_err(), ConfigError::RepositoryTrackedConfig { .. }));
    }

    #[test]
    fn a_tracked_config_with_an_invalid_egress_section_still_loads_after_stripping() {
        if !git_available() { return; }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loomweave.yaml");
        std::fs::write(&path, "version: 1\nsemantic_search:\n  enabled: true\n  provider: local_openai\n  endpoint_url: http://10.0.0.1:11434/v1\n").unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "-f", "loomweave.yaml"]);
        assert!(McpConfig::from_path(&path).is_err(), "non-loopback local endpoint is rejected by validate()");
        let loaded = McpConfig::load_trusted(&path).unwrap();
        assert!(matches!(loaded.trust, ConfigTrust::RepositoryTracked { .. }));
    }
```
(If `LlmConfigPatch`/`SemanticConfigPatch` do not derive `Default`, add `#[derive(Default)]`.)

- [ ] **Step 2: Run to verify they fail** — `cargo nextest run -p loomweave-federation config::tests` → compile errors.

- [ ] **Step 3: Implement**

```rust
/// Verbatim remedy printed wherever a tracked config is reported.
pub const CONFIG_TRACKED_REMEDY: &str =
    "To own this file: git rm --cached loomweave.yaml && echo loomweave.yaml >> .gitignore";

const EGRESS_SECTIONS: [&str; 4] = ["llm_policy", "semantic_search", "integrations", "serve.http"];

/// Who owns the effective `loomweave.yaml` (ADR-063). Repository-tracked
/// content may shape analysis; it may not name a network endpoint, a
/// credential env var, or a listen interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigTrust {
    OperatorOwned,
    NotAGitWorkTree,
    RepositoryTracked { stripped: Vec<&'static str> },
    /// The tracked-state probe failed; treated as tracked (fail closed).
    Unknown { reason: String, stripped: Vec<&'static str> },
}

impl ConfigTrust {
    #[must_use] pub fn egress_allowed(&self) -> bool { matches!(self, Self::OperatorOwned | Self::NotAGitWorkTree) }
    #[must_use] pub fn label(&self) -> &'static str { match self { Self::OperatorOwned => "operator_owned", Self::NotAGitWorkTree => "not_a_git_work_tree", Self::RepositoryTracked { .. } => "repository_tracked", Self::Unknown { .. } => "unknown" } }
    #[must_use] pub fn stripped(&self) -> &[&'static str] { match self { Self::RepositoryTracked { stripped } | Self::Unknown { stripped, .. } => stripped, _ => &[] } }
    #[must_use] pub fn to_json(&self, path: &Path) -> serde_json::Value {
        serde_json::json!({
            "state": self.label(),
            "path": path.display().to_string(),
            "stripped": self.stripped(),
            "remedy": if self.egress_allowed() { serde_json::Value::Null } else { serde_json::Value::String(CONFIG_TRACKED_REMEDY.to_owned()) },
        })
    }
}

#[derive(Debug, Clone)]
pub struct LoadedConfig { pub config: McpConfig, pub trust: ConfigTrust, pub path: PathBuf }

/// Trust verdict for a config file path (no stripping). The repository
/// consulted is the one containing the file's directory.
#[must_use]
pub fn config_trust_for_path(path: &Path) -> ConfigTrust {
    let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) else { return ConfigTrust::NotAGitWorkTree };
    let Some(name) = path.file_name() else { return ConfigTrust::NotAGitWorkTree };
    match loomweave_core::tracked_state(dir, Path::new(name)) {
        loomweave_core::TrackedState::Untracked => ConfigTrust::OperatorOwned,
        loomweave_core::TrackedState::NotAGitWorkTree => ConfigTrust::NotAGitWorkTree,
        loomweave_core::TrackedState::Tracked => ConfigTrust::RepositoryTracked { stripped: Vec::new() },
        loomweave_core::TrackedState::Unknown(err) => ConfigTrust::Unknown { reason: err.to_string(), stripped: Vec::new() },
    }
}

impl McpConfig {
    /// Parse `path`, decide ownership, strip egress sections from repository
    /// content, THEN validate — so a hostile-but-invalid egress section cannot
    /// turn the gate into a startup failure.
    pub fn load_trusted(path: &Path) -> Result<LoadedConfig, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io { path: path.display().to_string(), source })?;
        let mut config = Self::parse_unvalidated(&raw)?;      // factor out of from_yaml_str: deserialize + alias-collision check, no validate()
        let mut trust = config_trust_for_path(path);
        if !trust.egress_allowed() {
            let stripped = config.strip_egress_sections();
            match &mut trust {
                ConfigTrust::RepositoryTracked { stripped: s } | ConfigTrust::Unknown { stripped: s, .. } => *s = stripped,
                _ => unreachable!(),
            }
        }
        config.validate()?;
        Ok(LoadedConfig { config, trust, path: path.to_path_buf() })
    }

    /// Reset every egress-capable section to its default; returns the names of
    /// the sections that were non-default (in `EGRESS_SECTIONS` order).
    pub fn strip_egress_sections(&mut self) -> Vec<&'static str> {
        let mut stripped = Vec::new();
        if self.llm != LlmConfig::default() { self.llm = LlmConfig::default(); stripped.push(EGRESS_SECTIONS[0]); }
        if self.semantic_search != SemanticSearchConfig::default() { self.semantic_search = SemanticSearchConfig::default(); stripped.push(EGRESS_SECTIONS[1]); }
        if self.integrations != IntegrationsConfig::default() { self.integrations = IntegrationsConfig::default(); stripped.push(EGRESS_SECTIONS[2]); }
        if self.serve.http != HttpReadConfig::default() { self.serve.http = HttpReadConfig::default(); stripped.push(EGRESS_SECTIONS[3]); }
        stripped
    }
}

/// Once per process: `warn` when sections were stripped, `info` otherwise.
pub fn log_config_trust_once(loaded: &LoadedConfig) {
    static LOGGED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    LOGGED.get_or_init(|| {
        if loaded.trust.egress_allowed() {
            tracing::info!(path = %loaded.path.display(), trust = loaded.trust.label(), "loomweave.yaml is operator-owned; egress settings honoured");
        } else {
            tracing::warn!(
                path = %loaded.path.display(), trust = loaded.trust.label(), stripped = ?loaded.trust.stripped(),
                "loomweave.yaml is tracked by the repository; ignoring its llm_policy, semantic_search, integrations and serve.http sections (ADR-063). {CONFIG_TRACKED_REMEDY}"
            );
        }
    });
}
```
Add the error variant (follow the existing `code` constant naming convention in this enum — read the neighbours and pick the matching style):

```rust
    #[error("{code}: {path} is tracked by the repository, so Loomweave ignores its egress sections and refuses to edit it (ADR-063). {CONFIG_TRACKED_REMEDY}")]
    RepositoryTrackedConfig { code: &'static str, path: String },
```
At the top of `update_llm_config_file` and `update_semantic_config_file`:

```rust
    if path.exists() && !config_trust_for_path(path).egress_allowed() {
        return Err(ConfigError::RepositoryTrackedConfig { code: CONFIG_REPOSITORY_TRACKED_CODE, path: path.display().to_string() });
    }
```
Check whether `HttpReadConfig`, `IntegrationsConfig`, `LlmConfig`, `SemanticSearchConfig` implement `PartialEq + Default` (they derive `PartialEq`; add `Default` where missing by moving the existing manual defaults).

Consumers:
- `serve.rs:486-491` → `let loaded = McpConfig::load_trusted(config_path)…?; log_config_trust_once(&loaded); let config = loaded.config;` and keep `loaded.trust` in a local `config_trust` for Task 6 (pass it along to wherever the MCP server state is built — leave a `// Task 6 wires config_trust into the server state` marker is NOT allowed; instead thread it now into the existing builder as a new field `config_trust: ConfigTrust` on the MCP server state, unused until Task 6 reads it). If no file exists: `ConfigTrust::NotAGitWorkTree` is wrong — use `ConfigTrust::OperatorOwned` for "absent, defaults in effect".
- `analyze.rs::load_mcp_config` → same, `load_trusted` + `log_config_trust_once`; on error keep the existing warn+default.
- `config.rs::run_check` → `load_trusted`; print a first line `config trust: <label> (<path>)` and, when stripped, the list and the remedy, before the existing output.

- [ ] **Step 4: Run tests** — `cargo build --workspace --bins && cargo nextest run -p loomweave-federation -p loomweave-cli config` → pass.

- [ ] **Step 5: CLI integration test** (append to `crates/loomweave-cli/tests/config.rs`)

```rust
#[test]
fn config_check_reports_a_tracked_config_and_llm_set_refuses_it() {
    if std::process::Command::new("git").arg("--version").output().is_err() { return; }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("loomweave.yaml"), "version: 1\nllm_policy:\n  enabled: true\n  provider: codex_cli\n  allow_live_provider: true\n").unwrap();
    for args in [vec!["init", "-q"], vec!["add", "-f", "loomweave.yaml"]] {
        assert!(std::process::Command::new("git").args(&args).current_dir(dir.path()).status().unwrap().success());
    }
    let out = config(dir.path(), &["check"]);   // existing helper; adapt to its signature
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("config trust: repository_tracked"), "{text}");
    assert!(text.contains("llm_policy"), "{text}");
    assert!(text.contains("git rm --cached loomweave.yaml"), "{text}");
    let set = config(dir.path(), &["llm", "set", "--enable"]);
    assert!(!set.status.success());
    assert!(String::from_utf8_lossy(&set.stderr).contains("tracked by the repository"));
}
```

- [ ] **Step 6: Full floor, commit**

Commit: `feat(config): repository-tracked loomweave.yaml loses its egress sections; writers refuse tracked targets (clarion-dee44f1a66, ADR-063)`

---

### Task 6: Surfaces, acceptance test, ADR-063, docs, changelog

**Files:**
- Modify: `crates/loomweave-mcp/src/lib.rs` (server state carries `ConfigTrust`; `llm_config_get` and `tools/status.rs` `project_status_get` output gain `"config_trust": {...}`; `tool_llm_config_set`/`tool_semantic_config_set` already refuse via Task 5 — map `ConfigError::RepositoryTrackedConfig` to `McpErrorCode::InvalidParams`-class (not StorageError) with the remedy in the message)
- Modify: `crates/loomweave-cli/src/doctor.rs` (new check `config.trust` + `--fix`)
- Modify: `crates/loomweave-cli/src/install.rs:604-613` (ownership advisory)
- Create: `crates/loomweave-cli/tests/config_trust.rs` (acceptance)
- Create: `docs/loomweave/adr/ADR-063-repository-content-is-not-operator-intent.md`
- Modify: `docs/loomweave/adr/README.md` (new row after ADR-062), `docs/operator/getting-started.md` (config ownership paragraph), `docs/operator/language-support.md` only if it mentions rung 2 (grep), `CHANGELOG.md` (`Unreleased` → three bullets)
- Test: `crates/loomweave-cli/tests/doctor.rs`, `crates/loomweave-mcp/src/lib.rs` tests

**Interfaces:**
- Consumes: Task 5 `ConfigTrust`, `LoadedConfig`, `CONFIG_TRACKED_REMEDY`, `config_trust_for_path`; Task 3 `tracked_state`.

- [ ] **Step 1: Acceptance test** `crates/loomweave-cli/tests/config_trust.rs` — copy `spawn_embedding_mock` from `tests/analyze.rs:1360-1455` into this file (or move it to a shared `tests/common/mod.rs` if one exists) and write:

```rust
//! ADR-063 acceptance (clarion-dee44f1a66): a committed loomweave.yaml naming an
//! attacker endpoint + an arbitrary credential env var causes NO network call
//! under `analyze` or `serve`; the same file untracked works.
#![cfg(unix)]
// helpers: install(dir) runs `loomweave install <dir>`; git(dir, args); write_hostile(dir, mock_url)
// hostile yaml: semantic_search {enabled, provider: api, allow_live_provider: true, endpoint_url: <mock>, api_key_env: LOOMWEAVE_TEST_CANARY}
//               llm_policy {enabled, provider: openrouter, allow_live_provider: true, openrouter: {endpoint_url: <mock>, api_key_env: LOOMWEAVE_TEST_CANARY}}
//               integrations.filigree {enabled: true, base_url: <mock>, token_env: LOOMWEAVE_TEST_CANARY, actor: t}

#[test]
fn a_committed_config_never_reaches_the_network_under_analyze() {
    // install → write hostile yaml → git init/add -f/commit → run `loomweave analyze <dir>` with env LOOMWEAVE_TEST_CANARY=leak
    // assert exit success; join the mock → captured requests is EMPTY
    // assert stderr contains "tracked by the repository"
}

#[test]
fn a_committed_config_never_reaches_the_network_under_serve() {
    // same fixture; spawn `loomweave serve --path <dir>` over stdio, send initialize + tools/call llm_config_get (pattern: tests/serve.rs)
    // assert result.config_trust.state == "repository_tracked", provider disabled; kill serve; captured requests EMPTY
}

#[test]
fn the_same_config_untracked_populates_embeddings_through_the_mock() {
    // same fixture but `git rm --cached loomweave.yaml` after commit (file stays); run analyze
    // assert the mock captured ≥1 request containing "/embeddings" and "Bearer leak"
    // (this is the existing analyze_persists_plugin_tags_and_populates_embedding_sidecar shape — mirror its mock response body)
}
```
Write the three bodies fully, following `tests/analyze.rs:1459-1520` for the analyze invocation and `tests/serve.rs` for the stdio handshake.

- [ ] **Step 2: Run to verify** — the first two FAIL if Task 5 missed a path (they should pass already for `analyze`; `serve` fails until `config_trust` is in `llm_config_get`). The third passes.

- [ ] **Step 3: MCP surfaces** — add `config_trust: ConfigTrust` to the server state (constructed in `serve.rs` from `loaded.trust`, default `OperatorOwned` in test builders); in `llm_config_get`'s JSON and `project_status_get`'s JSON add `"config_trust": trust.to_json(&path)`. Map `ConfigError::RepositoryTrackedConfig` in `tool_llm_config_set`/`tool_semantic_config_set` to an invalid-params-class error with the remedy. Add an MCP unit test beside `llm_config_set_bootstraps_provider_and_write_tools_under_read_only_policy:7759` that a tracked config target returns that error and the file is untouched. Verify the tools/list byte budget test still passes (no schema/description changes).

- [ ] **Step 4: `doctor`** — add check id `config.trust` next to the db-tracked check: `ok` (operator_owned / not_a_git_work_tree / absent), `problem` (repository_tracked / unknown) with message `loomweave.yaml is tracked by the repository; its llm_policy, semantic_search, integrations and serve.http sections are ignored (ADR-063)` and remedy `CONFIG_TRACKED_REMEDY`. `--fix`: `git rm --cached -q -- loomweave.yaml` through the runner, then append `loomweave.yaml\n` to `<root>/.gitignore` **only if** `tracked_state(root, ".gitignore")` is `Untracked`/`NotAGitWorkTree` or the file is absent; otherwise report "fixed index; add `loomweave.yaml` to your tracked .gitignore yourself" (cede discipline). Tests in `tests/doctor.rs`: problem reported; `--fix` untracks and (untracked .gitignore case) appends; tracked `.gitignore` is not modified.

- [ ] **Step 5: `install` advisory** — after writing/leaving the stub at `install.rs:604-613`: if `tracked_state(project_root, "loomweave.yaml")` is `Tracked`, print `warning: loomweave.yaml is tracked by this repository; Loomweave ignores its egress sections (ADR-063). {CONFIG_TRACKED_REMEDY}`; else if the root is a git work tree and `.gitignore` does not contain a line equal to `loomweave.yaml`, print `note: loomweave.yaml is operator-owned; add it to .gitignore so it is never committed (ADR-063).` Test in `tests/install.rs` (or wherever install tests live): note printed in a fresh repo; nothing printed outside a repo.

- [ ] **Step 6: ADR-063** — write `docs/loomweave/adr/ADR-063-repository-content-is-not-operator-intent.md` in the ADR-062 shape (Status Accepted, Date 2026-09-02, Deciders john@foundryside.dev, Context, Summary, Decision (numbered: 1 principle; 2 tracked-path primitive + fail-closed; 3 config gate table; 4 writers refuse; 5 rung-2 via ADR-058 amendment; 6 bounded git probes), Consequences (positive: the class closes for config + interpreter; negative: teams that committed `loomweave.yaml` lose egress until they untrack it — surfaced by doctor/config check/install/serve log; `analysis` still honoured), Alternatives considered (XDG operator config — deferred, allowlists — rejected, closing #142/#147 as written — rejected), Residuals (`weft.toml` sibling URLs; committed `.env` under ADR-062; grandchild pipe-holder after tree kill; `doctor --fix` `current_exe analyze` spawn unbounded), Related (ADR-013/045/058/062; tickets). Glossary verdict: **no clash** for `config_trust` (check `docs/suite/glossary.md`; add the term). Add the README row after ADR-062 in the same one-line style.

- [ ] **Step 7: Docs + changelog** — `docs/operator/getting-started.md`: a short "Who owns loomweave.yaml" paragraph (operator-local; add to .gitignore; what a tracked file can and cannot do; the remedy). `CHANGELOG.md` `Unreleased` (create the heading if absent, matching the file's style) with three bullets: security — tracked `loomweave.yaml` egress sections ignored (clarion-dee44f1a66, ADR-063); security — tracked `.venv/bin/python` skipped (clarion-9b3cf287b7, ADR-058 amendment); hardening — bounded git probes, `doctor` db-tracked `Unknown` (clarion-9202f4acec). Note the operator-visible behaviour change in one line each.

- [ ] **Step 8: Full floor (Rust + Python + e2e smoke), commit**

Run everything in Global Constraints plus `bash tests/e2e/sprint_1_walking_skeleton.sh`.
Commit: `feat(doctor,install,mcp): surface config trust; ADR-063; acceptance test for the tracked-config egress gate (clarion-dee44f1a66)`

---

## Self-review

- **Spec coverage:** Part A → Tasks 1–2 (runner, `--version`, all 14 sites + `git_hooks`, strict UTF-8 directions, doctor `Unknown`). Part B → Task 3 (Rust, Python, fixture, fail-closed). Part C → Task 4 (both sides, log once, ADR-058 amendment, docs, no `venvPath` gating). Part D → Tasks 5–6 (rule table, `load_trusted`, three consumers, writers refuse, log once, `config check`, `doctor` + `--fix` with cede rule, `install` advisory, `project_status_get`, acceptance test, ADR-063, changelog).
- **Placeholders:** Task 6 Step 1 gives the three test shapes with concrete fixtures and assertions rather than full bodies because they mirror existing files line-for-line; the implementer is pointed at the exact templates. Everything else carries code.
- **Type consistency:** `TrackedState::{label, treat_as_tracked}` used identically in Tasks 3–6; `ConfigTrust::{egress_allowed, label, stripped, to_json}` in Tasks 5–6; `CONFIG_TRACKED_REMEDY` verbatim everywhere; `run_git_probe_default` / `GitProbeError::NonZeroExit { code, stderr_tail }` in Tasks 1–3.
