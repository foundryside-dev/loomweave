# Worktree Indexes Part 1: Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Loomweave resolve linked Git worktrees into owned, isolated
central stores and support explicit analysis without a checkout-local install.

**Architecture:** Capture a trusted Git executable before repository `.env`
loading, then resolve one typed `WorktreeContext` and `StorePaths` boundary.
Linked stores live under the primary checkout's repository store and are
initialized crash-consistently with an owner marker and metadata. All manual
analysis and non-service runtime consumers receive explicit paths.

**Tech Stack:** Rust 1.88 workspace, Clap, Serde/JSON, BLAKE3, fs2 file locks,
bounded subprocess I/O, NUL-delimited Git worktree porcelain output, Cargo
Nextest.

**Design:**
[`2026-07-18-loomweave-worktree-indexes-design.md`](../specs/2026-07-18-loomweave-worktree-indexes-design.md)

**Sequence:** This is plan 1 of 3. Complete it before
`2026-07-18-worktree-indexes-2-bootstrap.md`, then execute
`2026-07-18-worktree-indexes-3-cleanup.md`.

---

## Execution preflight

- [ ] **Step 1: Create the isolated implementation worktree**

Use `superpowers:using-git-worktrees` from the clean main checkout:

```bash
cd /home/john/loomweave
test -z "$(git status --porcelain)"
git check-ignore -q .worktrees
git worktree add .worktrees/worktree-indexes \
  -b feat/worktree-indexes main
cd .worktrees/worktree-indexes
git status --short --branch
```

Expected: branch `feat/worktree-indexes` in a clean ignored worktree.

- [ ] **Step 2: Confirm the tracker and focused baseline**

```bash
filigree show clarion-c297efc752
cargo nextest run -p loomweave-core -p loomweave-federation
```

Expected: the feature is assigned and in `building`; both crate suites pass.

## File structure

- Create `crates/loomweave-core/src/worktree/mod.rs` as the public module
  boundary.
- Create `crates/loomweave-core/src/worktree/context.rs` for Git worktree
  resolution, stable IDs, configuration origin, and name selection.
- Create `crates/loomweave-core/src/worktree/paths.rs` for
  `RepositoryStorePaths` and `StorePaths`.
- Create `crates/loomweave-core/src/worktree/locks.rs` for namespace, activity,
  metadata, intent, and writer lock guards.
- Create `crates/loomweave-core/src/worktree/metadata.rs` for owner,
  initialization, and worktree metadata records.
- Create `crates/loomweave-core/src/worktree/store.rs` for owned namespace and
  crash-consistent store open/create.
- Create `crates/loomweave-cli/src/worktree.rs` for the explicit worktree CLI.
- Create focused core and CLI integration tests rather than expanding large
  existing test modules.

### Task 1: Capture trusted Git and bound every worktree probe

**Files:**

- Modify: `Cargo.toml`
- Modify: `crates/loomweave-core/Cargo.toml`
- Modify: `crates/loomweave-core/src/hardened_git.rs`
- Modify: `crates/loomweave-core/src/lib.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/sei_git.rs`
- Modify: `crates/loomweave-mcp/src/index_diff.rs`
- Create: `crates/loomweave-core/tests/hardened_git.rs`

- [ ] **Step 1: Add failing trusted-environment and bounded-I/O tests**

Add tests named exactly:

```rust
#[test]
fn trusted_git_is_resolved_before_path_changes() {}

#[test]
fn context_bound_command_clears_repository_selectors() {}

#[test]
fn context_bound_version_probe_uses_captured_executable() {}

#[test]
fn runner_drains_stdout_and_stderr_concurrently() {}

#[test]
fn stdout_overflow_kills_and_reaps_git() {}

#[test]
fn stderr_overflow_kills_and_reaps_git() {}

#[test]
fn nonzero_exit_fails_the_whole_probe() {}
```

The selector test must seed `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`,
`GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
`GIT_NAMESPACE`, `GIT_EXEC_PATH`, and `GIT_CONFIG_COUNT`, then assert none is
inherited. The overflow fixtures write indefinitely and record their PID; the
test asserts the child no longer exists after the error.

- [ ] **Step 2: Run the tests and verify RED**

```bash
cargo test -p loomweave-core --test hardened_git
```

Expected: compilation fails because `TrustedGitContext`, `GitOutputLimits`, and
`run_hardened_git` do not exist.

- [ ] **Step 3: Implement the trusted runner and preserve the old safe helper**

Add this public surface in `hardened_git.rs`:

```rust
#[derive(Clone, Debug)]
pub struct TrustedGitContext {
    executable: std::path::PathBuf,
    launch_env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    attr_source_supported: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct GitOutputLimits {
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
}

#[derive(Debug)]
pub struct GitOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub const REV_PARSE_LIMITS: GitOutputLimits = GitOutputLimits {
    stdout_bytes: 64 * 1024,
    stderr_bytes: 64 * 1024,
};

pub const WORKTREE_LIST_LIMITS: GitOutputLimits = GitOutputLimits {
    stdout_bytes: 8 * 1024 * 1024,
    stderr_bytes: 64 * 1024,
};

impl TrustedGitContext {
    pub fn capture() -> Result<Self, HardenedGitError>;
    pub fn executable(&self) -> &std::path::Path;
    pub fn command(&self, repo_root: &std::path::Path) -> std::process::Command;
}

pub fn run_hardened_git(
    trusted: &TrustedGitContext,
    repo_root: &std::path::Path,
    args: &[std::ffi::OsString],
    limits: GitOutputLimits,
) -> Result<GitOutput, HardenedGitError>;
```

`TrustedGitContext::capture` uses `which::which_in` against the pre-dotenv
`PATH`, canonicalizes the executable, and computes Git attribute support with
that executable. `command` starts with `env_clear`, restores only the captured
platform launch allowlist, sets `LC_ALL=C` and `LANG=C`, and applies the existing
hostile-config flags. `run_hardened_git` pipes both streams, drains them on two
threads, kills on the first cap breach, always reaps, and rejects non-zero exit.

Keep `hardened_git_command(repo_root)` for existing callers, but extend it to
remove all repository selectors. Migrate `sei_git.rs` and `index_diff.rs`
without changing their output contract.

- [ ] **Step 4: Capture Git before `.env` and run GREEN checks**

Capture `TrustedGitContext` immediately after `Cli::parse()` and pass it into
commands that resolve a worktree. Do not reconstruct it inside `serve`.

```bash
cargo test -p loomweave-core --test hardened_git
cargo test -p loomweave-core hardened_git::tests
cargo check -p loomweave-cli -p loomweave-mcp
```

Expected: all tests and checks pass; existing clean-filter hardening remains
green.

- [ ] **Step 5: Commit the trusted Git boundary**

```bash
git add Cargo.toml Cargo.lock crates/loomweave-core \
  crates/loomweave-cli/src/main.rs crates/loomweave-cli/src/sei_git.rs \
  crates/loomweave-mcp/src/index_diff.rs
git commit -m "feat(core): capture and bound hardened git probes"
```

### Task 2: Resolve typed worktree context and runtime paths

**Files:**

- Create: `crates/loomweave-core/src/worktree/mod.rs`
- Create: `crates/loomweave-core/src/worktree/context.rs`
- Create: `crates/loomweave-core/src/worktree/paths.rs`
- Create: `crates/loomweave-core/tests/worktree_context.rs`
- Create: `crates/loomweave-core/tests/support/mod.rs`
- Modify: `crates/loomweave-core/src/lib.rs`
- Modify: `crates/loomweave-core/src/store.rs`
- Modify: `crates/loomweave-core/Cargo.toml`

- [ ] **Step 1: Add real-repository resolver tests**

Create temporary Git repositories and linked worktrees. Pin these cases:

```rust
#[test]
fn standalone_context_preserves_existing_store_path() {}
#[test]
fn main_context_preserves_existing_store_path() {}
#[test]
fn linked_context_uses_primary_repository_store() {}
#[test]
fn linked_context_honors_only_primary_weft_override() {}
#[test]
fn stable_id_ignores_branch_head_and_dirty_state() {}
#[test]
fn stable_id_survives_whole_repository_relocation() {}
#[test]
fn primary_is_selected_by_git_directory() {}
#[test]
fn linked_paths_with_spaces_and_newlines_round_trip() {}
#[cfg(unix)]
#[test]
fn non_utf8_required_path_fails_before_store_creation() {}
#[test]
fn malformed_porcelain_is_rejected_without_partial_context() {}
#[test]
fn linked_config_precedence_and_default_target_are_exact() {}
#[test]
fn sibling_roots_are_source_then_primary_and_deduplicated() {}
#[test]
fn exact_worktree_selection_rejects_ambiguity_and_main() {}
```

- [ ] **Step 2: Run the resolver test and verify RED**

```bash
cargo test -p loomweave-core --test worktree_context
```

Expected: compilation fails because the worktree module and types are absent.

- [ ] **Step 3: Define the typed boundary**

Implement these types without public root-based convenience constructors:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeKind { Standalone, Main, Linked }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigOriginKind { Explicit, Source, Primary, DefaultTarget }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigOrigin {
    pub path: std::path::PathBuf,
    pub kind: ConfigOriginKind,
    pub existed_at_resolution: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GcCapability {
    EnabledOwnedDefault,
    Disabled { reason: GcDisabledReason },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryStorePaths {
    pub repository_store: std::path::PathBuf,
    pub worktrees_dir: std::path::PathBuf,
    pub owner_marker: std::path::PathBuf,
    pub gc_lock: std::path::PathBuf,
    pub gc_state: std::path::PathBuf,
    pub trash_dir: std::path::PathBuf,
    pub quarantine_dir: std::path::PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorePaths {
    pub store_dir: std::path::PathBuf,
    pub database: std::path::PathBuf,
    pub embeddings: std::path::PathBuf,
    pub instance_id: std::path::PathBuf,
    pub ephemeral_port: std::path::PathBuf,
    pub runs_dir: std::path::PathBuf,
    pub diagnostics_dir: std::path::PathBuf,
    pub secret_baseline: std::path::PathBuf,
    pub writer_lock: std::path::PathBuf,
    pub activity_lock: std::path::PathBuf,
    pub analysis_intent_lock: std::path::PathBuf,
    pub analysis_intent: std::path::PathBuf,
    pub metadata_lock: std::path::PathBuf,
    pub metadata: std::path::PathBuf,
    pub initializing: std::path::PathBuf,
}

#[derive(Clone, Debug)]
pub struct WorktreeContext {
    pub source_root: std::path::PathBuf,
    pub primary_root: std::path::PathBuf,
    pub repository_paths: RepositoryStorePaths,
    pub store_paths: StorePaths,
    pub config_origin: ConfigOrigin,
    pub sibling_lookup_roots: Vec<std::path::PathBuf>,
    pub gc_capability: GcCapability,
    pub kind: WorktreeKind,
    pub git_common_dir: Option<std::path::PathBuf>,
    pub git_admin_identity: Option<String>,
    pub stable_id: Option<String>,
}

pub fn resolve_worktree_context(
    target: &std::path::Path,
    explicit_config: Option<&std::path::Path>,
    trusted_git: &TrustedGitContext,
) -> Result<WorktreeContext, WorktreeContextError>;
```

Add `blake3.workspace = true` to core. Parse `worktree list --porcelain -z`
strictly, compute `wt-<full BLAKE3>`, reject required non-UTF-8 paths, and call
`store_dir(primary_root)` exactly once to derive the repository store.

- [ ] **Step 4: Implement exact registered-worktree selection and run GREEN**

Expose:

```rust
pub fn registered_worktrees(
    context: &WorktreeContext,
    trusted_git: &TrustedGitContext,
) -> Result<Vec<RegisteredWorktree>, WorktreeContextError>;

pub fn select_registered_linked_worktree<'a>(
    entries: &'a [RegisteredWorktree],
    name_or_path: &std::ffi::OsStr,
) -> Result<&'a RegisteredWorktree, WorktreeSelectionError>;
```

Only exact canonical paths, exact path basenames, and exact administrative
basenames match. Return all registered choices on zero or ambiguous matches.

```bash
cargo test -p loomweave-core --test worktree_context
```

Expected: all resolver, malformed-output, config-origin, and selection tests
pass.

- [ ] **Step 5: Commit context and paths**

```bash
git add crates/loomweave-core Cargo.toml Cargo.lock
git commit -m "feat(core): resolve worktree-scoped runtime context"
```

### Task 3: Initialize owned stores crash-consistently

**Files:**

- Create: `crates/loomweave-core/src/worktree/locks.rs`
- Create: `crates/loomweave-core/src/worktree/metadata.rs`
- Create: `crates/loomweave-core/src/worktree/store.rs`
- Create: `crates/loomweave-core/tests/worktree_store.rs`
- Modify: `crates/loomweave-core/src/worktree/mod.rs`
- Modify: `crates/loomweave-core/Cargo.toml`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add owner-election and partial-store recovery tests**

Add barrier-driven tests named:

```rust
#[test]
fn empty_namespace_creates_owner_before_any_other_child() {}
#[test]
fn two_initializers_publish_one_owner_id() {}
#[test]
fn incomplete_owner_is_recovered_only_when_sole_child() {}
#[test]
fn missing_owner_beside_content_is_refused() {}
#[test]
fn configured_override_is_isolated_but_gc_disabled() {}
#[test]
fn empty_stable_directory_is_recoverable() {}
#[test]
fn matching_initializing_record_is_recoverable() {}
#[test]
fn valid_metadata_with_initializing_record_is_finalized() {}
#[test]
fn mismatched_or_unknown_partial_content_is_preserved() {}
#[test]
fn activity_lock_is_held_before_gc_lock_is_released() {}
#[test]
fn metadata_update_is_atomic_and_serialized() {}
```

- [ ] **Step 2: Run the store test and verify RED**

```bash
cargo test -p loomweave-core --test worktree_store
```

Expected: compilation fails because owner, metadata, and lock types are absent.

- [ ] **Step 3: Implement fixed-schema records and ordered guards**

Add `fs2.workspace`, `time.workspace`, and `uuid.workspace` to core. Enable
the `serde` feature on the workspace `time` dependency. Use UUID v4 values for
initialization nonces and owner IDs. Implement:

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OwnerMarker {
    pub schema: String,
    pub owner_id: String,
    pub primary_root: std::path::PathBuf,
    pub git_common_dir: std::path::PathBuf,
    pub created_at: time::OffsetDateTime,
    pub checksum: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WorktreeMetadata {
    pub schema: String,
    pub owner_id: String,
    pub stable_id: String,
    pub git_admin_identity: String,
    pub source_root: std::path::PathBuf,
    pub primary_root: std::path::PathBuf,
    pub created_at: time::OffsetDateTime,
    pub last_seen_at: time::OffsetDateTime,
    pub last_analyzed_commit: Option<String>,
    pub last_completed_run_id: Option<String>,
    pub orphan_candidate_since: Option<time::OffsetDateTime>,
    pub absence_confirmations: u32,
}

pub struct LinkedStoreGuard {
    pub owner: OwnerMarker,
    pub metadata: WorktreeMetadata,
    pub activity: SharedActivityGuard,
}

pub fn open_or_initialize_linked_store(
    context: &WorktreeContext,
) -> Result<LinkedStoreGuard, WorktreeStoreError>;

pub fn record_completed_analysis(
    guard: &mut LinkedStoreGuard,
    commit: Option<&str>,
    run_id: &str,
) -> Result<(), WorktreeStoreError>;
```

Owner initialization uses `create_new` plus an exclusive owner-file lock. A
stable directory is recoverable only when empty or when its matching
`initializing.json` accompanies only known zero-length locks and a same-nonce
metadata temp file. Unknown content is never removed.

- [ ] **Step 4: Run GREEN and commit owned-store initialization**

```bash
cargo test -p loomweave-core --test worktree_store
git add Cargo.toml Cargo.lock crates/loomweave-core
git commit -m "feat(core): initialize owned worktree stores safely"
```

Expected: all creation, collision, crash, and metadata tests pass.

### Task 4: Route configuration and sibling discovery through context

**Files:**

- Modify: `crates/loomweave-cli/src/config.rs`
- Modify: `crates/loomweave-cli/src/install.rs`
- Modify: `crates/loomweave-cli/src/integration_bindings.rs`
- Modify: `crates/loomweave-federation/src/filigree_url.rs`
- Modify: `crates/loomweave-federation/src/filigree.rs`
- Modify: `crates/loomweave-federation/src/loomweave_port.rs`
- Modify: `crates/loomweave-federation/src/loomweave_url.rs`
- Modify: `crates/loomweave-cli/tests/config.rs`

- [ ] **Step 1: Add failing config-origin and sibling-root tests**

Pin source-before-primary reads, exact-origin writes, primary default creation,
explicit-config preservation, Filigree port/token fallback, and explicit
Loomweave port leaves with these names:

```text
linked_config_set_updates_selected_source_file_only
linked_config_set_updates_selected_primary_file_only
linked_config_set_without_file_creates_primary_target
explicit_config_never_creates_a_shadow_file
filigree_port_falls_back_to_primary_root
filigree_token_falls_back_to_primary_root
source_sibling_state_wins_over_primary
loomweave_port_helpers_use_explicit_leaf
```

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test -p loomweave-cli --test config linked_config_
cargo test -p loomweave-federation filigree_port_falls_back_to_primary_root
```

Expected: the new assertions fail because current APIs accept one project root.

- [ ] **Step 3: Change APIs to explicit origins, roots, and leaves**

Use these signatures:

```rust
pub fn resolve_filigree_url(
    config: &FiligreeConfig,
    sibling_lookup_roots: &[std::path::PathBuf],
    getenv: impl Fn(&str) -> Option<String>,
) -> FiligreeUrlResolution;

pub fn read_filigree_ephemeral_port(
    sibling_lookup_roots: &[std::path::PathBuf],
) -> Option<u16>;

pub fn publish_port(path: &std::path::Path, port: u16) -> std::io::Result<()>;
pub fn read_published_port(path: &std::path::Path) -> Option<u16>;
pub fn remove_published_port_if_matches(path: &std::path::Path, port: u16);
```

Make Filigree token lookup use the same ordered roots. Make config setters write
`ConfigOrigin.path` exactly. Keep environment and explicit URL precedence above
sidecars.

- [ ] **Step 4: Run GREEN and commit configuration routing**

```bash
cargo test -p loomweave-cli --test config
cargo test -p loomweave-federation filigree_url::tests
cargo test -p loomweave-federation filigree::tests
cargo test -p loomweave-federation loomweave_port::tests
cargo test -p loomweave-federation loomweave_url::tests
git add crates/loomweave-cli/src/config.rs \
  crates/loomweave-cli/src/install.rs \
  crates/loomweave-cli/src/integration_bindings.rs \
  crates/loomweave-cli/tests/config.rs crates/loomweave-federation
git commit -m "refactor(config): route worktree config and sibling state"
```

### Task 5: Add explicit worktree analysis

**Files:**

- Modify: `crates/loomweave-cli/src/cli.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/analyze_lock.rs`
- Create: `crates/loomweave-cli/src/worktree.rs`
- Create: `crates/loomweave-cli/tests/worktree_analyze.rs`

- [ ] **Step 1: Add failing CLI and divergent-graph tests**

Cover both command forms and exact selection:

```text
analyze_linked_path_creates_only_central_store
analyze_linked_path_needs_no_local_install
analyze_main_path_preserves_existing_store
worktree_analyze_selects_exact_path_basename
worktree_analyze_selects_exact_admin_basename
worktree_analyze_rejects_main_and_unregistered_paths
worktree_analyze_reports_every_ambiguous_choice
worktree_analyze_double_dash_accepts_dash_prefixed_path
linked_analysis_uses_primary_store_override
linked_analysis_records_completed_metadata
divergent_main_and_linked_worktrees_produce_distinct_graphs
```

- [ ] **Step 2: Run the integration test and verify RED**

```bash
cargo test -p loomweave-cli --test worktree_analyze
```

Expected: Clap rejects `worktree analyze` and linked `analyze` still requires a
local install.

- [ ] **Step 3: Share analysis arguments and pass resolved context**

Refactor Clap and analysis entry points to:

```rust
#[derive(clap::Args)]
pub struct AnalyzeFlags {
    #[arg(long)]
    config: Option<std::path::PathBuf>,
    #[arg(long)]
    allow_unredacted_secrets: bool,
    #[arg(long, value_name = "TOKEN", requires = "allow_unredacted_secrets")]
    confirm_allow_unredacted_secrets: Option<String>,
    #[arg(long, hide = true)]
    run_id: Option<String>,
    #[arg(long, value_name = "RUN_ID", conflicts_with = "run_id")]
    resume: Option<String>,
    #[arg(long)]
    prune_unseen: bool,
    #[arg(long, hide = true)]
    progress_file: Option<std::path::PathBuf>,
    #[arg(long)]
    no_sei: bool,
    #[arg(long)]
    no_incremental: bool,
    #[arg(long)]
    legis_url: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(clap::Subcommand)]
pub enum Command {
    Analyze {
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
        #[command(flatten)]
        flags: AnalyzeFlags,
    },
}

#[derive(clap::Subcommand)]
pub enum WorktreeCommand {
    Analyze {
        name_or_path: std::path::PathBuf,
        #[command(flatten)]
        flags: AnalyzeFlags,
    },
}

pub(crate) async fn run_with_options(
    context: WorktreeContext,
    linked_store: Option<LinkedStoreGuard>,
    options: AnalyzeOptions,
) -> anyhow::Result<()>;
```

Move the existing help text with each field and preserve every current option
name. The only positional argument remains outside `AnalyzeFlags`: `path` for
`loomweave analyze`, or `name_or_path` for `loomweave worktree analyze`.
Resolve context before store checks. Change `acquire_analyze_lock` to accept
`&StorePaths`. Pass explicit database, embeddings, baseline, diagnostics, and
progress paths through the analysis pipeline. Update linked metadata only
after the durable completed run commit.

- [ ] **Step 4: Run GREEN, pin help, and commit manual analysis**

```bash
cargo test -p loomweave-cli --test worktree_analyze
cargo test -p loomweave-cli analyze_lock::tests
cargo run -q -p loomweave-cli -- worktree analyze --help
git add crates/loomweave-cli
git commit -m "feat(cli): analyze registered worktrees explicitly"
```

Expected: both command forms use one central linked store; main behavior stays
unchanged. The durable analysis-intent coordinator is added in plan 2 before
automatic bootstrap is enabled.

### Task 6: Migrate non-service runtime path consumers and install an audit

**Files:**

- Modify: `crates/loomweave-storage/src/embeddings.rs`
- Modify: `crates/loomweave-cli/src/instance.rs`
- Modify: `crates/loomweave-cli/src/secret_scan/baseline.rs`
- Modify: `crates/loomweave-cli/src/hook.rs`
- Modify: `crates/loomweave-cli/src/db.rs`
- Modify: `crates/loomweave-cli/src/guidance.rs`
- Modify: `crates/loomweave-cli/src/doctor.rs`
- Modify: `crates/loomweave-cli/src/install.rs`
- Create: `crates/loomweave-cli/tests/runtime_path_callsite_audit.rs`
- Modify focused tests for each command above.

- [ ] **Step 1: Add a failing production call-site audit**

The audit recursively reads `crates/*/src/**/*.rs` and rejects:

```rust
const FORBIDDEN: &[&str] = &[
    "store_dir(&self.project_root)",
    "db_path(&self.project_root)",
    "EmbeddingStore::open_in_store_dir",
    "embeddings_db_path(project_root)",
    "llm_traffic_log_path(project_root)",
    ".join(\".weft/loomweave\")",
];
```

Allow only repository-store calculation in `loomweave-core/src/store.rs` and
`worktree/context.rs`. Temporarily allow the exact service files migrated in
plan 2: `loomweave-cli/src/serve.rs`, `loomweave-cli/src/http_read.rs`, and
`loomweave-mcp/src/**`. Any additional match fails.

- [ ] **Step 2: Run the audit and verify RED**

```bash
cargo test -p loomweave-cli --test runtime_path_callsite_audit
```

Expected: failures identify the non-service helpers listed in this task.

- [ ] **Step 3: Replace root arguments with explicit paths**

Use leaf-path APIs:

```rust
pub fn load_or_create(path: &std::path::Path) -> anyhow::Result<InstanceId>;
pub fn load_secret_baseline(path: &std::path::Path) -> Result<Baseline, Error>;
pub fn embeddings_db_path(paths: &StorePaths) -> &std::path::Path;
```

Route `db`, `guidance`, `doctor`, hooks, install store setup, diagnostics, and
semantic sidecar status through `WorktreeContext` or `StorePaths`. Assets still
target `source_root`; only runtime store creation targets `effective_store`.

- [ ] **Step 4: Run command tests and commit the explicit-path migration**

```bash
cargo test -p loomweave-cli --test runtime_path_callsite_audit
cargo test -p loomweave-cli --test db --test hook --test guidance
cargo test -p loomweave-cli --test install --test doctor
cargo check -p loomweave-storage -p loomweave-cli
git add crates/loomweave-storage crates/loomweave-cli
git commit -m "refactor(runtime): make worktree store paths explicit"
```

## Part 1 verification

- [ ] **Step 1: Run the focused foundation suites**

```bash
cargo nextest run -p loomweave-core -p loomweave-federation
cargo nextest run -p loomweave-cli \
  --test worktree_analyze \
  --test runtime_path_callsite_audit \
  --test config \
  --test db \
  --test hook
cargo fmt --all -- --check
git status --short
```

Expected: all focused suites pass and only intentional commits exist.

- [ ] **Step 2: Continue directly to plan 2**

Do not merge or release this intermediate slice. Continue in the same feature
worktree with `2026-07-18-worktree-indexes-2-bootstrap.md`; automatic bootstrap
and the final no-allowlist path audit are acceptance requirements.
