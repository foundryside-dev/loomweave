# Untrusted-corpus trust boundary — design

**Date**: 2026-09-02
**Tickets**: clarion-dee44f1a66 (P1, config egress gate), clarion-9b3cf287b7 (P2, `.venv` rung trust), clarion-9202f4acec (P2, bounded git I/O)
**Branch**: `feat/untrusted-corpus-trust-boundary` → `release/1.6.0`
**ADRs**: new ADR-063 (repository-tracked content is not operator intent); amendment to ADR-058

## Problem

Loomweave analyzes repositories it does not trust (ADR-045 posture), yet three
inputs it reads from *inside* the analyzed tree are treated as if the operator
wrote them:

1. `<project_root>/loomweave.yaml` names network endpoints (`llm_policy`,
   `semantic_search`, `integrations.filigree`) and the **name** of the env var
   whose value is sent as a bearer token. A committed config can exfiltrate any
   operator env var plus source-derived text on the first `analyze` — including
   the hook-spawned background one. `allow_live_provider` lives in the same
   file, so it is not a gate.
2. `<project_root>/.venv/bin/python` (ADR-058 rung 2) is executed by pyright.
   A committed executable there is code execution as the operator.
3. Every git probe against the corpus runs with unbounded output and no
   deadline (`Command::output()`), so a hostile or pathological repository can
   wedge `serve`, the hook, or `analyze` indefinitely, or allocate without
   limit. Env stripping (the other half of clarion-9202f4acec) already landed.

All three share one principle and one primitive.

**Principle (ADR-063):** *content tracked by the analyzed repository is
repository content, never operator intent.* Operator intent is expressed by
files the operator owns — untracked files in the checkout, the real
environment, CLI flags.

**Primitive:** "is this path tracked by git?", answered through the hardened,
bounded git probe.

## Non-goals

- An operator-level config location outside the tree (XDG). Rejected for now:
  no such location exists today, every operator already has an untracked
  `loomweave.yaml` path available, and adding a second precedence ladder is
  larger than the threat needs. Recorded as a considered alternative in ADR-063.
- Allowlisting credential env-var names or endpoint hosts. Rejected: an
  operator's untracked config is trusted; allowlists would break legitimate
  local providers (Ollama on a LAN host) and add nothing once tracked config
  is inert.
- `weft.toml` `[loomweave]` keys and the `.env` sidecar (ADR-062). Both are
  repository-adjacent files with their own decisions; their residual exposure
  is recorded in ADR-063, not fixed here.
- Bounding the non-git `current_exe analyze` spawn in `doctor --fix`.

## Part A — bounded git probe (clarion-9202f4acec)

### Contract

`hardened_git_command(root) -> Command` keeps returning a bare `Command`; the
`get_args()`/`get_envs()` introspection tests depend on it. A new runner sits
beside it:

```rust
pub struct GitProbeLimits { pub deadline: Duration, pub max_stdout_bytes: usize }
impl Default for GitProbeLimits  // 30 s, 32 MiB

pub struct GitProbeOutput { pub stdout: Vec<u8>, pub stderr_tail: Vec<u8>, pub status: ExitStatus }
impl GitProbeOutput {
    pub fn stdout_utf8(&self) -> Result<&str, GitProbeError>;   // strict; NonUtf8 on failure
}

pub enum GitProbeError {
    Spawn(io::Error), Timeout { after: Duration }, StdoutOverflow { limit: usize },
    NonZeroExit { code: Option<i32>, stderr_tail: String }, NonUtf8, Io(io::Error),
}

pub fn run_git_probe(command: Command, limits: &GitProbeLimits) -> Result<GitProbeOutput, GitProbeError>;
pub fn run_git_probe_default(command: Command) -> Result<GitProbeOutput, GitProbeError>;
```

Semantics:

- stdin null; stdout and stderr piped; both drained on dedicated threads
  started before `wait`.
- stdout is read with a hard cap: `limit + 1` bytes; exceeding it kills the
  process tree (`plugin::process_tree::kill_process_tree`) and returns
  `StdoutOverflow`. stderr is a 64 KiB ring (oldest dropped); it is diagnostic
  and never fails the probe.
- Wall-clock deadline via `try_wait` polling (25 ms); on expiry kill the
  process tree, join the readers, return `Timeout`.
- Non-zero exit → `NonZeroExit` with the stderr tail (callers that need the
  exit code for a tri-state, e.g. `ls-files --error-unmatch`, read it from the
  error).
- Reader threads are always joined before returning (no leaked threads); the
  child is always reaped (no zombie) — this is the correction to the
  `llm_provider.rs` prior art.
- The `git --version` probe inside `attr_source_supported` uses the same
  runner with a short deadline (5 s) so a wedged git cannot poison the
  `OnceLock` forever.

### Callers migrated

All 14 production sites enumerated in the exploration (index_diff ×3,
worktree/context `run_git_stdout`, worktree/cmd, sei_git ×3, doctor ×2,
analyze/fast_path, worktree/sweep, `list_untracked_files`, `--version`) plus
the one raw `Command::new("git")` in `git_hooks.rs::hooks_dir`, which moves onto
the hardened builder.

UTF-8: sites that were lossy become strict through `stdout_utf8()`; a probe
that yields non-UTF-8 fails **in the direction each caller already documents
as fail-soft** (fast path → not taken → full analyze; `index_diff` git facts →
unavailable; `list_untracked_files` → `None`). No fail-soft site turns a probe
error into a *positive* verdict: `doctor::db_tracked_state` gains an
`Unknown` state and reports it rather than folding a timeout into "untracked".

### Tests

- Unit (core): deadline fires on `git` replaced by a sleeping stub via a
  `PATH` shim; stdout overflow kills and returns `StdoutOverflow`; stderr
  ring keeps the tail; non-zero exit surfaces code + tail; strict UTF-8.
- Existing hardened_git/introspection tests unchanged.
- Every migrated caller's existing tests pass unchanged.

## Part B — tracked-path primitive

```rust
pub enum TrackedState { Tracked, Untracked, NotAGitWorkTree, Unknown(GitProbeError) }
pub fn tracked_state(repo_root: &Path, path: &Path) -> TrackedState;
```

Implementation: one `git ls-files -z -- <rel> <rel ancestors…> [<canonical rel> …]`
through the runner; any output line ⇒ `Tracked`. The pathspec set is the
literal relative path plus each ancestor up to (not including) the root, plus
— when `canonicalize(path)` resolves inside the canonical root — the canonical
relative path and its ancestors. This catches: a tracked file, a tracked
symlink at the path or at any ancestor, a directory with tracked contents at
any ancestor, and a symlink whose target is tracked. A path resolving outside
the root (e.g. `/usr/bin/python3`) contributes nothing. Exit 128 with
"not a git repository" ⇒ `NotAGitWorkTree`; other failures ⇒ `Unknown`.

Failure direction is decided by the caller, and both callers below fail
**closed**: `Unknown` is treated as `Tracked`. `NotAGitWorkTree` is treated as
`Untracked` (no git to consult; the operator owns the tree), per both tickets.

Python twin: `plugins/python/src/loomweave_plugin_python/git_trust.py` with the
same pathspec construction and the same tri-state, shelling out with a minimal
env (`PATH`, `LC_ALL=C`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL`/`SYSTEM`
→ `/dev/null`, `GIT_OPTIONAL_LOCKS=0`), `-c core.fsmonitor=false`,
`subprocess.run(timeout=…)`, bounded read. A shared JSON conformance fixture
(`fixtures/git_tracked_paths.json`: on-disk layout + git actions → expected
state) is consumed by both test suites. Malformed / hung git ⇒ `unknown`.

## Part C — `.venv` rung trust (clarion-9b3cf287b7)

Rung 2 becomes: accept `<project_root>/.venv/bin/python` only when
`tracked_state(root, ".venv/bin/python") ∈ {Untracked, NotAGitWorkTree}`. On
`Tracked` or `Unknown`, skip the rung and fall through to rung 3 exactly as if
the file were absent. Both sides implement the same predicate; both sides log
once, in the existing idiom (`tracing::warn!` once via `OnceLock` on the Rust
side; `sys.stderr.write` once on the Python side):

```
loomweave: skipped .venv/bin/python (rung 2): it is tracked by the repository; an
operator venv is untracked. Resolution continues with VIRTUAL_ENV/CONDA_PREFIX/PATH.
```

No new `InterpreterSource` variant: the outcome is whatever the next rung
yields, and `interpreter_unpinned` already explains the degradation on the
plugin side (ADR-057). `pyrightconfig.json` `venvPath`/`venv` is *not* gated —
verified against the pinned pyright bundle: those keys only shape search-path
globbing and never execute a program.

ADR-058 gets an `## Amendment (2026-09-02) — rung-2 trust condition` section
and its rung table gains the condition; Status line points at the amendment.
Docs: `plugins/python/README.md` rung table, `docs/operator/getting-started.md`.

Tests: Rust — committed `.venv/bin/python` (added via raw `git add -f`) is
skipped and the ladder yields the `VIRTUAL_ENV` candidate; an untracked one is
still chosen; a symlink `.venv` → tracked dir is skipped; non-git tempdir keeps
current behaviour. Python — the same four. Existing tests unchanged.

## Part D — config trust gate (clarion-dee44f1a66)

### Rule

When the effective `loomweave.yaml` (whichever the worktree ladder resolves,
or `--config`) is **tracked** by the repository that contains it — or its
tracked state is `Unknown` — the loader replaces the egress-capable sections
with their defaults before anything consumes them:

| section | tracked ⇒ |
|---|---|
| `llm_policy` | `LlmConfig::default()` (disabled) |
| `semantic_search` | `SemanticSearchConfig::default()` (disabled) |
| `integrations` | `IntegrationsConfig::default()` (no Filigree/Wardline endpoints or `token_env`) |
| `serve.http` | `HttpConfig::default()` (loopback, no `identity_token_env`) |
| `version`, `analysis`, `serve.mcp` | honoured |

One sentence for the ADR: *a tracked config may shape analysis; it may not
name a network endpoint, a credential, or a listen interface.*

### Shape

In `loomweave-federation`:

```rust
pub enum ConfigTrust { OperatorOwned, RepositoryTracked { stripped: Vec<&'static str> }, Unknown { .. } , NotAGitWorkTree }
pub struct LoadedConfig { pub config: McpConfig, pub trust: ConfigTrust, pub path: PathBuf }
impl McpConfig {
    pub fn load_trusted(path: &Path) -> Result<LoadedConfig, ConfigError>;   // from_path + tracked_state + strip
    pub fn strip_egress_sections(&mut self) -> Vec<&'static str>;             // pure; returns names of sections that were non-default
}
```

`load_trusted` is the **only** entry the three consumers use: `serve.rs:484`,
`analyze.rs::load_mcp_config`, and `loomweave-mcp`'s `config_file_path`
consumers. `from_path` stays for tests and for `config check`'s "what does the
file say" rendering.

Log once per process, `warn` when stripped, `info` otherwise:

```
loomweave: loomweave.yaml at <path> is tracked by the repository; ignoring its
llm_policy, semantic_search, integrations and serve.http sections (ADR-063).
To own this file: git rm --cached loomweave.yaml && echo loomweave.yaml >> .gitignore
```

### Writers refuse

`llm_config_set` / `semantic_config_set` (MCP), `loomweave config llm set` /
`config semantic set` (CLI) refuse to edit a tracked target with the same
remedy text, so a bootstrap from a read-only session cannot leave the operator
with a config that is silently inert.

### Surfaces

- `loomweave config check` prints the trust verdict first.
- `loomweave doctor` gains `config.trust` (`ok` / `problem` with the remedy);
  `doctor --fix` performs `git rm --cached` + appends `loomweave.yaml` to the
  root `.gitignore` **only if** that file is untracked or absent; if
  `.gitignore` is tracked it prints the remedy instead (cede discipline).
- `loomweave install` keeps writing the stub at the root; it now also prints
  the ownership advisory when the root is a git work tree and `.gitignore`
  does not already cover `loomweave.yaml`. It does not edit `.gitignore`.
- `project_status_get` reports `config.trust` so an agent can see why
  summaries are off.

### Acceptance test (from the ticket)

CLI integration test, `#[cfg(unix)]`, using the existing `spawn_embedding_mock`
listener pattern: tempdir repo with a **committed** `loomweave.yaml` whose
`semantic_search.endpoint_url` and `llm_policy.openrouter.endpoint_url` point
at the mock, `api_key_env: LOOMWEAVE_TEST_CANARY`, `enabled: true`,
`allow_live_provider: true`; env carries `LOOMWEAVE_TEST_CANARY=leak`.
`analyze` completes with zero requests captured; `serve` starts, answers
`llm_config_get` showing provider `disabled` with trust `repository_tracked`,
zero requests captured. Second half: the same file untracked (`git rm --cached`)
→ `analyze` populates embeddings via the mock (the existing
`analyze_persists_plugin_tags_and_populates_embedding_sidecar` shape).

## Order of work

1. Part A (runner + caller migration) — CI floor green, commit.
2. Part B (Rust + Python primitive + conformance fixture) — commit.
3. Part C — commit, ADR-058 amendment.
4. Part D — commit, ADR-063, docs, changelog `Unreleased`.
5. PR → `release/1.6.0`, CI, admin-merge, close the three tickets with the
   merge SHA, file follow-ups for the recorded residuals.
