# Worktree Indexes Part 1: Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Loomweave resolve linked Git worktrees into owned, isolated
central stores and support explicit analysis without a checkout-local install.

**Architecture:** Capture separate trusted-Git and sanitized full process
environments before repository `.env` loading, then resolve one typed
`WorktreeContext` and `StorePaths` boundary.
Linked stores live under the primary checkout's repository store and are
initialized crash-consistently with an owner marker and metadata. All manual
analysis and non-service runtime consumers receive explicit paths.

**Tech Stack:** Rust 1.88 workspace, Clap, Serde/JSON, BLAKE3, getrandom,
RFC 3339 `time` Serde adapters, fs2 file locks, `syn` call-site auditing,
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
- Create `crates/loomweave-core/src/worktree/record.rs` for the shared canonical
  `Checksummed<T>` codec and RFC 3339 adapters used by every durable schema.
- Create `crates/loomweave-core/src/worktree/store.rs` for owned namespace and
  crash-consistent store open/create.
- Create `crates/loomweave-core/src/process_environment.rs` for the full
  pre-dotenv child environment, kept separate from trusted Git's allowlist.
- Create `crates/loomweave-cli/src/worktree.rs` for the explicit worktree CLI.
- Create focused core and CLI integration tests rather than expanding large
  existing test modules.

### Task 1: Capture trusted Git and bound every worktree probe

**Files:**

- Modify: `Cargo.toml`
- Modify: `crates/loomweave-core/Cargo.toml`
- Modify: `crates/loomweave-core/src/hardened_git.rs`
- Create: `crates/loomweave-core/src/process_environment.rs`
- Modify: `crates/loomweave-core/src/plugin/host.rs`
- Modify: `crates/loomweave-core/src/lib.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/config.rs`
- Modify: `crates/loomweave-cli/src/serve.rs`
- Modify: `crates/loomweave-cli/src/sei_git.rs`
- Modify: `crates/loomweave-mcp/src/index_diff.rs`
- Modify: `crates/loomweave-mcp/src/analyze_runs.rs`
- Create: `crates/loomweave-core/tests/hardened_git.rs`
- Create: `crates/loomweave-cli/tests/dotenv_policy.rs`

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
fn silent_rev_parse_timeout_kills_group_and_reaps_git() {}

#[test]
fn silent_worktree_list_timeout_kills_group_and_reaps_git() {}

#[test]
fn inherited_helper_group_cancellation_reaps_git_without_nested_group() {}

#[test]
fn nonzero_exit_fails_the_whole_probe() {}

#[test]
fn plugin_environment_preserves_operator_values_but_strips_git_overrides() {}

#[test]
fn explicit_git_constructor_never_resolves_path() {}

#[test]
fn process_environment_applicator_clears_before_restore() {}

```

In `dotenv_policy.rs`, add `top_level_analyze_skips_repository_dotenv` against
the real binary/CLI policy. Later tasks extend this same target only after the
nested and hidden commands they exercise exist.

The selector test must seed `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`,
`GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
`GIT_NAMESPACE`, `GIT_EXEC_PATH`, and `GIT_CONFIG_COUNT`, then assert none is
inherited. The overflow and silent-timeout fixtures run indefinitely and record
their PID plus a process-group child; each test asserts the whole owned group is
gone and the direct child was reaped after the typed error.

- [ ] **Step 2: Run the tests and verify RED**

```bash
cargo test -p loomweave-core --test hardened_git
cargo test -p loomweave-cli --test dotenv_policy
```

Expected: compilation fails because `TrustedGitContext`, `GitOutputLimits`, and
`run_hardened_git` do not exist.

- [ ] **Step 3: Implement the trusted runner and preserve the old safe helper**

Enable the core crate's target-Unix `nix` `signal` feature in this task; do not
rely on later CLI feature unification. Normal Unix probes use
`CommandExt::process_group(0)` plus `killpg`. Windows/non-Unix normal probes use
the platform direct-child terminate-and-wait boundary; plan 3 never launches a
lifecycle cleanup helper on unsupported non-Unix targets. The tests compile and
exercise each cfg explicitly.

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
    pub deadline: std::time::Duration,
}

#[derive(Debug)]
pub struct GitOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub enum GitTerminationDomain {
    FreshOwned,
    InheritedOwnedGroup {
        pgid: i32,
        global_deadline: std::time::Instant,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
}

pub const REV_PARSE_LIMITS: GitOutputLimits = GitOutputLimits {
    stdout_bytes: 64 * 1024,
    stderr_bytes: 64 * 1024,
    deadline: std::time::Duration::from_secs(30),
};

pub const WORKTREE_LIST_LIMITS: GitOutputLimits = GitOutputLimits {
    stdout_bytes: 8 * 1024 * 1024,
    stderr_bytes: 64 * 1024,
    deadline: std::time::Duration::from_secs(60),
};

impl TrustedGitContext {
    pub fn capture() -> Result<Self, HardenedGitError>;
    pub fn from_explicit_executable(
        executable: std::path::PathBuf,
        process_env: &PreDotenvProcessEnvironment,
    ) -> Result<Self, HardenedGitError>;
    pub fn executable(&self) -> &std::path::Path;
    pub fn command(&self, repo_root: &std::path::Path) -> std::process::Command;
}

impl PreDotenvProcessEnvironment {
    pub fn capture() -> Self;
    pub fn apply_to_command(&self, command: &mut std::process::Command);
}

pub fn run_hardened_git(
    trusted: &TrustedGitContext,
    repo_root: &std::path::Path,
    args: &[std::ffi::OsString],
    limits: GitOutputLimits,
    termination: GitTerminationDomain,
) -> Result<GitOutput, HardenedGitError>;
```

`TrustedGitContext::capture` uses `which::which_in` against the pre-dotenv
`PATH`, canonicalizes the executable, and computes Git attribute support with
that executable. `from_explicit_executable` requires an absolute path,
canonicalizes and validates a regular executable, derives Git's minimal launch
allowlist from the supplied captured process environment, and runs all version
and attribute probes through that exact canonical path without consulting
`PATH`. This is the only supported reconstruction path for the later hidden
cleanup helper. `command` starts with `env_clear`, restores only the captured
platform-specific launch allowlist, sets `LC_ALL=C` and `LANG=C`, and applies
the existing hostile-config flags. Document and test the Linux, macOS, and
Windows allowlists independently. `run_hardened_git` pipes both streams and
drains them on two threads. `FreshOwned` owns Git in a fresh process group. A
cleanup worker instead supplies its already fresh `InheritedOwnedGroup`; Git
inherits that PGID, its effective deadline is the minimum of the local probe
limit and global remaining time, and no nested group can escape worker
shutdown. Local cap/deadline and cooperative TERM/cancellation terminate and
mandatorily reap the direct child before a typed return. Plan 3 places a Linux
child-subreaper supervisor outside this worker/Git group; forced group KILL
returns no worker result, and that surviving supervisor waits the worker plus
every adopted Git descendant. The runner itself claims only cooperative reaps.
Non-zero exit is also rejected. Add silent-child deadline/cancellation tests as
well as stream-overflow tests.

`PreDotenvProcessEnvironment::capture()` retains the full operator environment
needed by analysis/plugin children but removes Git selectors, `GIT_CONFIG_*`,
execution overrides, and unsafe values named by the hardened boundary.
`apply_to_command` always calls `env_clear()` before restoring that sanitized
full environment, so service, analysis, plugin, and cleanup-child launchers do
not open-code environment reconstruction. Do not derive it from
`TrustedGitContext::launch_env`.

Keep `hardened_git_command(repo_root)` for existing callers, but extend it to
remove all repository selectors. Migrate `sei_git.rs` and `index_diff.rs`
without changing their output contract.

- [ ] **Step 4: Capture Git before `.env` and run GREEN checks**

Capture `Arc<TrustedGitContext>` and `Arc<PreDotenvProcessEnvironment>`
immediately after `Cli::parse()` and before dotenv loading. Thread the Git
context through every resolver, SEI/status/rename/untracked probe, and cleanup
path. Thread the full process environment through analysis options, plugin
launch, service/MCP launchers, and snapshots. Do not reconstruct either inside
`serve`. Update `should_load_dotenv` for top-level analysis now. Plan 1 Task 5,
plan 2 Task 1, and plan 3 Task 5 extend the policy for the nested worktree
command, hidden analysis child, and hidden cleanup helper respectively, after
each command exists.

```bash
cargo test -p loomweave-core --test hardened_git
cargo test -p loomweave-core hardened_git::tests
cargo test -p loomweave-cli --test dotenv_policy
cargo check -p loomweave-mcp
```

Expected: all tests and checks pass; existing clean-filter hardening remains
green.

- [ ] **Step 5: Commit the trusted Git boundary**

```bash
git add Cargo.toml Cargo.lock crates/loomweave-core \
  crates/loomweave-cli/src/main.rs crates/loomweave-cli/src/analyze.rs \
  crates/loomweave-cli/src/config.rs crates/loomweave-cli/src/serve.rs \
  crates/loomweave-cli/src/sei_git.rs \
  crates/loomweave-cli/tests/dotenv_policy.rs \
  crates/loomweave-mcp/src/index_diff.rs \
  crates/loomweave-mcp/src/analyze_runs.rs
git commit -m "feat(core): capture and bound hardened git probes"
test -z "$(git status --porcelain)"
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
#[test]
fn present_entries_resolve_administrative_identity_with_bounded_probes() {}
#[test]
fn inventory_over_4096_entries_fails_before_store_creation() {}
#[test]
fn prunable_missing_entry_uses_validated_metadata_not_missing_path_probe() {}
#[test]
fn repository_paths_include_typed_cleanup_diagnostic_directory_lock_and_leaf() {}
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GcDisabledReason {
    ConfiguredStoreOverride,
    NonCanonicalDefaultStore,
    SymlinkedStorePath,
    UnconfinedStorePath,
    MissingOwner,
    InvalidOwner,
    OwnerRepositoryMismatch,
    UnsupportedPlatform,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryStorePaths {
    pub repository_store: std::path::PathBuf,
    pub worktrees_dir: std::path::PathBuf,
    pub owner_marker: std::path::PathBuf,
    pub gc_lock: std::path::PathBuf,
    pub gc_state: std::path::PathBuf,
    pub cleanup_diagnostics_dir: std::path::PathBuf,
    pub cleanup_diagnostics_lock: std::path::PathBuf,
    pub cleanup_diagnostic: std::path::PathBuf,
    pub relocation_dir: std::path::PathBuf,
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
    pub metadata_update_pending: std::path::PathBuf,
    pub metadata_update_journal: std::path::PathBuf,
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
    pub gc_preflight: GcCapability,
    pub kind: WorktreeKind,
    pub git_common_dir: Option<std::path::PathBuf>,
    pub git_admin_identity: Option<String>,
    pub stable_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryAuthority {
    pub repository_paths: RepositoryStorePaths,
    pub gc_capability: GcCapability,
    pub owner_id: Option<String>,
}

pub fn resolve_worktree_context(
    target: &std::path::Path,
    explicit_config: Option<&std::path::Path>,
    trusted_git: &TrustedGitContext,
) -> Result<WorktreeContext, WorktreeContextError>;
```

`WorktreeContext.gc_preflight` is provisional diagnostic input only. A fresh
canonical namespace legitimately resolves as `MissingOwner`; no caller may use
that preflight value for status, scheduling, helper identity, or mutation after
repository open. Namespace open/initialization and owner rebind return a fresh
`RepositoryAuthority`, and every downstream cleanup/status surface requires it.

`GcDisabledReason` serializes at public boundaries as the closed kebab-case
values `configured-store-override`, `noncanonical-default-store`,
`symlinked-store-path`, `unconfined-store-path`, `missing-owner`,
`invalid-owner`, `owner-repository-mismatch`, and `unsupported-platform`.
Unknown values are rejected; callers never construct a free-form reason.

Add `blake3.workspace = true` to core. Parse `worktree list --porcelain -z`
strictly. Because porcelain omits the administrative directory, perform one
bounded context-bound `rev-parse --absolute-git-dir` per present entry and cap
the inventory/probe count at 4,096. A probe failure fails the whole operation.
Missing/prunable entries are matched only through validated managed metadata,
never by invoking Git through a missing source path. Compute
`wt-<full BLAKE3>`, reject required non-UTF-8 paths, and call
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
test -z "$(git status --porcelain)"
```

### Task 3: Initialize owned stores crash-consistently

**Files:**

- Create: `crates/loomweave-core/src/worktree/locks.rs`
- Create: `crates/loomweave-core/src/worktree/metadata.rs`
- Create: `crates/loomweave-core/src/worktree/record.rs`
- Create: `crates/loomweave-core/src/worktree/store.rs`
- Create: `crates/loomweave-core/tests/worktree_store.rs`
- Modify: `crates/loomweave-core/src/worktree/mod.rs`
- Modify: `crates/loomweave-core/Cargo.toml`
- Modify: `Cargo.toml`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/analyze_lock.rs`
- Create: `crates/loomweave-cli/tests/worktree_analyze.rs`

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
#[test]
fn owner_and_nonce_width_alphabet_and_entropy_failure_are_exact() {}
#[test]
fn durable_records_use_rfc3339_and_canonical_checksum_bytes() {}
#[test]
fn durable_record_exact_1mib_limit_is_accepted_and_plus_one_rejected() {}
#[test]
fn owner_leaf_symlink_hardlink_and_special_file_create_no_authority() {}
#[test]
fn metadata_and_initializing_authority_leaves_are_no_follow_regular_only() {}
#[test]
fn durable_record_identity_or_size_change_during_read_fails_closed() {}
#[test]
fn reappearance_clear_killed_before_metadata_parent_fsync_recovers_clear() {}
#[test]
fn reappearance_clear_killed_after_metadata_parent_fsync_keeps_clear() {}
#[test]
fn metadata_update_journal_mismatch_disables_store_and_gc() {}
#[test]
fn gc_disabled_reason_vocabulary_is_closed() {}
#[test]
fn owner_rebind_after_repository_move_is_confined_and_atomic() {}
#[test]
fn copied_store_cannot_reuse_gc_authority() {}
#[test]
fn fresh_default_namespace_returns_enabled_post_open_authority() {}
#[test]
fn pre_open_missing_owner_is_never_published_after_open() {}
#[test]
fn owner_rebind_refreshes_authority_without_restart() {}
#[test]
fn existing_authority_probe_never_creates_or_rebinds_namespace() {}
#[test]
fn owner_election_rw_open_preserves_nofollow_single_link_rules() {}
#[test]
fn metadata_journal_worst_case_envelope_fits_composed_caps() {}
#[test]
fn metadata_update_journal_golden_schema_checksum_and_exact_cap_are_pinned() {}
#[test]
fn kill_after_metadata_journal_rename_recovers_without_two_link_state() {}
#[test]
fn partial_metadata_journal_scratch_with_pending_resets_orphan_evidence() {}
#[test]
fn pending_with_malformed_published_journal_fails_closed() {}
#[test]
fn fixed_scratch_rename_requires_pre_and_post_inode_match() {}
#[test]
fn ordinary_scratch_kill_points_discard_or_fail_closed() {}
#[test]
fn forged_valid_unjournaled_scratch_is_never_promoted_after_restart() {}
```

- [ ] **Step 2: Run the store test and verify RED**

```bash
cargo test -p loomweave-core --test worktree_store
```

Expected: compilation fails because owner, metadata, and lock types are absent.

- [ ] **Step 3: Implement fixed-schema records and ordered guards**

Add `fs2.workspace`, `getrandom = "0.3.4"`, `time.workspace`,
`uuid.workspace`, and workspace `rustix = { version = "1.1.4", features =
["fs"] }` to core now. Descriptor-relative no-follow open, identity,
no-replace rename, unlink, and parent-fsync tests must compile in plan 1; plan 3
later adds only the Linux `mount` feature. Enable `serde` and
`serde-well-known` on the workspace `time` dependency. Use `getrandom::fill`
for 32-byte owner IDs and independent
16-byte initialization/lifecycle nonces; encode lowercase hex exactly. UUID v4
remains reserved for run IDs. Entropy failure publishes nothing. Annotate
required timestamps with `time::serde::rfc3339` and optional timestamps with
`time::serde::rfc3339::option`, normalizing UTC. Implement:

```rust
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Checksummed<T> {
    #[serde(flatten)]
    payload: T,
    checksum: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct OwnerMarkerPayload {
    schema: String,
    owner_id: String,
    primary_root: std::path::PathBuf,
    git_common_dir: std::path::PathBuf,
    created_at: time::OffsetDateTime,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorktreeMetadataPayload {
    schema: String,
    owner_id: String,
    stable_id: String,
    git_admin_identity: String,
    source_root: std::path::PathBuf,
    primary_root: std::path::PathBuf,
    created_at: time::OffsetDateTime,
    last_seen_at: time::OffsetDateTime,
    last_analyzed_commit: Option<String>,
    last_completed_run_id: Option<String>,
    orphan_candidate_since: Option<time::OffsetDateTime>,
    absence_confirmations: u32,
}

pub(crate) type OwnerMarker = Checksummed<OwnerMarkerPayload>;
pub(crate) type WorktreeMetadata = Checksummed<WorktreeMetadataPayload>;

pub struct LinkedStoreGuard {
    authority: RepositoryAuthority,
    owner: OwnerMarker,
    metadata: WorktreeMetadata,
    activity: SharedActivityGuard,
}

impl LinkedStoreGuard {
    pub fn repository_authority(&self) -> &RepositoryAuthority;
}

pub fn open_or_initialize_linked_store(
    context: &WorktreeContext,
) -> Result<LinkedStoreGuard, WorktreeStoreError>;

pub fn open_repository_authority(
    context: &WorktreeContext,
) -> Result<RepositoryAuthority, WorktreeStoreError>;

pub fn probe_existing_repository_authority(
    context: &WorktreeContext,
) -> Result<RepositoryAuthority, WorktreeStoreError>;

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

Owner initialization or rebind computes `RepositoryAuthority` only after the
winner is flushed, re-read, and validated under the namespace lock. The linked
guard retains that value; main/standalone entry points use
`open_repository_authority`. No post-open API accepts only the provisional
`WorktreeContext.gc_preflight`.
`probe_existing_repository_authority` is the cleanup-helper gate: it never
creates `worktrees/` or `owner.json`, never completes an owner, and never
rebinds roots. The caller must first compare the non-opening resolved canonical
repository-store path with the expected path; only then may it probe that exact
existing namespace.

Implement one core `DurableRecordFile` boundary now and require every quiescent
owner, initialization, metadata, intent, GC, relocation, quarantine, tombstone,
and metadata-update-journal reader to use it. Plan 3 may add only its explicitly
bounded `TransientRecordFile` for the in-flight same-inode/two-link publication
pair, which must normalize before commit. `open_read` works relative to an
already pinned no-follow parent directory, opens the exact direct child with
`O_RDONLY|O_CLOEXEC|O_NOFOLLOW|O_NONBLOCK`, requires a regular file with link
count one, and captures device/inode/size. The narrowly scoped
`open_election_rw` adds `O_RDWR` with otherwise identical checks and is used
only for owner election/completion and lock leaves. No general authority-record
writer receives that API. Reject a declared or observed size over the selected
schema limit; the universal ceiling is exactly 1,048,576 bytes. Read at most
one byte beyond the selected limit, reject overflow before JSON
allocation/parsing, then `fstat` again and require unchanged identity and size.
Symlinks, hardlinks, FIFOs, sockets, devices, directories, identity changes,
and size changes fail closed. An invalid `owner.json` or owner lock returns
before creating `gc.lock`, any managed directory, or any store.

Add `open_read_expected(parent, name, expected_identity, schema_limit)`. It
opens/fstats the final once, compares that descriptor to the retained scratch
identity, and only then reads. Every scratch publication and plan-3 transient
normalization uses it; never split pathname revalidation from an unchecked
second open.

Schema-specific limits compose rather than merely inherit the ceiling:
`metadata.json` is capped at 524,288 bytes and `metadata-update.json` at 786,432
bytes. The intended next envelope is embedded as a JSON object. The writer
serializes and checks both sizes before publishing either record, and a
worst-case field-bound test proves the maximum legal metadata record produces a
legal journal below the shared ceiling. Every other schema names its tighter
limit beside its codec.

All ordinary authority-record replacement uses one schema-specific fixed sibling
scratch opened create-new/no-follow, complete canonical bytes plus file
`sync_all`, a retained handle/identity, pre-rename scratch-name revalidation,
atomic rename, post-rename `open_read_expected` against that handle, and
parent-directory fsync before releasing its lock.
Metadata updates add a write-ahead `metadata-update.json` so a power loss cannot
resurrect stale absence evidence. Before writing its scratch, create-new the
exact empty direct-regular single-link `metadata-update.pending` and parent-fsync
it. Publish the journal only through `.metadata-update.json.tmp` with the
retained-handle identity barriers above and no-replace rename. Replace
`metadata.json` through `.metadata.json.tmp` with the same barriers, fsync the
parent, remove the journal and pending sentinel, and fsync again. This ordinary
journal path never uses hard links.

Under the corresponding exclusive lock, a read-only structural/budget preflight
recognizes only exact direct-regular single-link scratches: the two metadata
names above,
`.owner.json.tmp` for rebind, `.gc-state.json.tmp` for later GC state, and the
explicit fixed intent/initialization scratch named by its record module. It
reserves all repair/final-validation bytes before mutation. Recovery may
never promote an ordinary unjournaled scratch after restart, even when its
unkeyed checksum is valid. It may discard an exact direct-regular scratch only
when the authoritative final/surrounding state proves discard cannot increase
deletion or execution authority; otherwise it preserves the artifact and fails
closed. Post-crash completion requires a durable precursor or retained two-link
anchor. After discard, fsync the parent and require the authoritative final to
pass `DurableRecordFile`.
Wrong types, extra scratch-shaped names, oversize, unknown content, or
final/scratch conflict fail closed. Top-level managed inventory includes this
preflight and accepts no unreconciled scratch before candidate decisions.

On open under metadata lock, pending plus an absent final journal and at most
the one exact unpublished direct-regular scratch conservatively rewrites the
current valid metadata with orphan evidence reset to zero/null, then removes
that scratch/sentinel after fsync. A present final journal that fails file,
schema, or checksum validation is preserved and fails closed. Old metadata plus
a valid
matching journal applies and fsyncs the intended next envelope; already-next
metadata removes the journal/sentinel; any other combination fails closed. GC
calls the same reconciliation before evaluating absence and refuses relocation
while a pending sentinel, journal, or scratch is unresolved. A reappearance
clear therefore survives every kill point around sentinel creation, scratch
writes, identity barriers, journal rename, metadata rename, and directory fsync,
and old absence evidence is never accepted while an update marker remains.

For each durable record, define a separate schema-ordered canonical payload
struct without `checksum` plus a persisted `Checksummed<T>` envelope. Serialize
the payload as compact
UTF-8 JSON, hash those bytes with BLAKE3, and store lowercase hex in the
envelope. Golden fixtures pin the
exact JSON, RFC 3339 string shape, checksum bytes, malformed-width rejection,
and corruption rejection. Under `gc.lock`, a moved repository revalidates
owner checksum/confinement and atomically rebinds its audit roots before it may
quarantine stale-root indexes. A copied/unconfined store remains GC-disabled.

Put the shared encode/decode API in `record.rs` as `Checksummed<T>` plus a
`DurablePayload` validation trait. Owner, initialization, metadata, intent, GC,
relocation, quarantine, and tombstone code must call this codec; no caller may
deserialize an authority-bearing payload directly. Do not derive unchecked
`Deserialize` for the envelope. A custom raw-object visitor tracks keys and
rejects every unknown, duplicate (including duplicate checksum), or missing key
before typed payload deserialization and checksum validation; exact known keys
may arrive reordered. Keep envelope fields private. Add generic codec tests for
unknown/duplicate/missing/reordered keys and one inheritance/round-trip test for
every authority-bearing schema.
Exercise the shared file boundary at exactly 1,048,576 and 1,048,577 bytes, the
metadata/journal composed limits, both read-only and election-RW modes, and
leaf/type/identity swaps; codec tests alone do not satisfy this boundary.

- [ ] **Step 4: Add the linked-analysis tracer test and verify RED**

Add
`analyze_linked_path_creates_only_central_store` to
`worktree_analyze.rs`, then run it before changing the analysis path:

```bash
cargo test -p loomweave-cli --test worktree_analyze \
  analyze_linked_path_creates_only_central_store
```

Expected: it fails because linked analysis still derives the checkout-local
store.

- [ ] **Step 5: Implement and prove the first linked-analysis tracer bullet**

Before broad config/federation migration, route the existing top-level
`loomweave analyze <linked-path>` through `WorktreeContext`, `StorePaths`, and
`LinkedStoreGuard`. Carry explicit database, runs, writer-lock, embeddings,
baseline, and diagnostics leaves. Add one real-repository test that analyzes a
single linked worktree without a local install and asserts its central DB and
graph differ from main. The richer `worktree analyze` selector follows in Task
5, but this slice must compile and run now.

```bash
cargo test -p loomweave-cli --test worktree_analyze \
  analyze_linked_path_creates_only_central_store
```

- [ ] **Step 6: Run GREEN and commit owned-store initialization plus tracer**

```bash
cargo test -p loomweave-core --test worktree_store
cargo test -p loomweave-cli --test worktree_analyze \
  analyze_linked_path_creates_only_central_store
git add Cargo.toml Cargo.lock crates/loomweave-core crates/loomweave-cli
git commit -m "feat(core): initialize owned worktree stores safely"
test -z "$(git status --porcelain)"
```

Expected: all creation, collision, crash, and metadata tests pass.

### Task 4: Route configuration and sibling discovery through context

**Files:**

- Modify: `crates/loomweave-cli/src/config.rs`
- Modify: `crates/loomweave-cli/src/install.rs`
- Modify: `crates/loomweave-cli/src/serve.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/http_read.rs`
- Modify: `crates/loomweave-cli/src/integration_bindings.rs`
- Modify: `crates/loomweave-federation/src/filigree_url.rs`
- Modify: `crates/loomweave-federation/src/filigree.rs`
- Modify: `crates/loomweave-federation/src/loomweave_port.rs`
- Modify: `crates/loomweave-federation/src/loomweave_url.rs`
- Modify: `crates/loomweave-cli/tests/config.rs`
- Modify: `crates/loomweave-mcp/tests/storage_tools.rs`
- Modify all other federation and MCP tests that call the migrated public
  helpers, as revealed by compiler errors and call-site search.

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
cargo test -p loomweave-cli --test config
cargo test -p loomweave-federation
cargo test -p loomweave-mcp --test storage_tools
```

Expected: the new assertions fail because current APIs accept one project root.

- [ ] **Step 3: Add explicit APIs, migrate every caller, then retire wrappers**

Introduce additive signatures first; do not replace a public root-taking API in
the same step that creates its successor:

```rust
pub fn resolve_filigree_url_from_roots(
    config: &FiligreeConfig,
    sibling_lookup_roots: &[std::path::PathBuf],
    getenv: impl Fn(&str) -> Option<String>,
) -> FiligreeUrlResolution;

pub fn read_filigree_ephemeral_port_from_roots(
    sibling_lookup_roots: &[std::path::PathBuf],
) -> Option<u16>;

pub fn publish_port_at_path(path: &std::path::Path, port: u16) -> std::io::Result<()>;
pub fn read_published_port_at_path(path: &std::path::Path) -> Option<u16>;
pub fn remove_published_port_at_path_if_matches(
    path: &std::path::Path,
    port: u16,
);
```

Make Filigree token lookup use the same ordered roots. Make config setters write
`ConfigOrigin.path` exactly. Keep environment and explicit URL precedence above
sidecars. Migrate every CLI serve/analyze/HTTP, integration-binding,
federation, MCP, and test caller, running checks after each caller group. Only
then deprecate or remove legacy wrappers. Every intermediate commit must
compile all affected crates.

- [ ] **Step 4: Run GREEN and commit configuration routing**

```bash
cargo test -p loomweave-cli --test config
cargo test -p loomweave-federation filigree_url::tests
cargo test -p loomweave-federation filigree::tests
cargo test -p loomweave-federation loomweave_port::tests
cargo test -p loomweave-federation loomweave_url::tests
cargo test -p loomweave-mcp --test storage_tools
cargo check -p loomweave-federation -p loomweave-mcp -p loomweave-cli
git add crates/loomweave-cli crates/loomweave-federation \
  crates/loomweave-mcp Cargo.lock
git commit -m "refactor(config): route worktree config and sibling state"
test -z "$(git status --porcelain)"
```

### Task 5: Add explicit worktree analysis

**Files:**

- Modify: `crates/loomweave-cli/src/cli.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/analyze_lock.rs`
- Create: `crates/loomweave-cli/src/worktree.rs`
- Modify: `crates/loomweave-cli/tests/worktree_analyze.rs`
- Modify: `crates/loomweave-cli/tests/dotenv_policy.rs`

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
analyze_flags_preserve_top_level_and_nested_help
hidden_flags_remain_hidden_but_parse_for_internal_children
nested_worktree_analyze_skips_repository_dotenv
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
    pub(crate) config: Option<std::path::PathBuf>,
    #[arg(long)]
    pub(crate) allow_unredacted_secrets: bool,
    #[arg(long, value_name = "TOKEN", requires = "allow_unredacted_secrets")]
    pub(crate) confirm_allow_unredacted_secrets: Option<String>,
    #[arg(long, hide = true)]
    pub(crate) run_id: Option<String>,
    #[arg(long, value_name = "RUN_ID", conflicts_with = "run_id")]
    pub(crate) resume: Option<String>,
    #[arg(long)]
    pub(crate) prune_unseen: bool,
    #[arg(long, hide = true)]
    pub(crate) progress_file: Option<std::path::PathBuf>,
    #[arg(long)]
    pub(crate) no_sei: bool,
    #[arg(long)]
    pub(crate) no_incremental: bool,
    #[arg(long)]
    pub(crate) legis_url: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
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

Pin Clap parser and help snapshots for both command forms, hidden flags,
`--`, and exit behavior. Update `should_load_dotenv` for the nested command and
extend `dotenv_policy.rs` with its real-binary regression. The hidden analysis
child is added and covered in plan 2. A linked analysis retains its shared
activity guard through the last SQLite/filesystem access.

- [ ] **Step 4: Run GREEN, pin help, and commit manual analysis**

```bash
cargo test -p loomweave-cli --test worktree_analyze
cargo test -p loomweave-cli --test dotenv_policy
cargo test -p loomweave-cli analyze_lock::tests
cargo run -q -p loomweave-cli -- worktree analyze --help
git add crates/loomweave-cli
git commit -m "feat(cli): analyze registered worktrees explicitly"
test -z "$(git status --porcelain)"
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
- Modify: `crates/loomweave-cli/src/config.rs`
- Modify: `crates/loomweave-cli/Cargo.toml`
- Create: `crates/loomweave-cli/tests/runtime_path_callsite_audit.rs`
- Modify focused tests for each command above.

- [ ] **Step 1: Add a failing syntax-aware production call-site audit**

Add `syn = { version = "2", features = ["full", "visit"] }` as a CLI
dev-dependency and commit the lockfile. Parse every `crates/*/src/**/*.rs` file
with `syn::parse_file` and a `syn::visit::Visit` implementation; walk call
expressions, method calls, and string/path join expressions
independent of formatting or argument variable names. Inventory and reject all
root-taking runtime callees and wrappers: DB/store, embeddings/open-in-store,
LLM traffic, instance, port, baseline, diagnostics, runs, hooks/status,
install/setup, and federation helpers, plus literal `.weft` + `loomweave`
joins. Include `config.rs`. Allow only repository-store derivation in the
resolver and explicit test fixtures. The temporary plan-2 service allowlist is
file-exact and removed in plan 2; new files cannot match by directory wildcard.
Self-tests feed differently formatted calls, renamed arguments, method calls,
split component joins, and an allowed resolver fixture to prove the visitor
detects syntax rather than substrings.

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
Every command that opens or mutates store state retains a shared activity guard
for the entire operation, including DB backup/checkpoint, guidance, hooks and
status, doctor database inspection, install, and setup. Add barriers that prove
cleanup cannot acquire exclusive activity mid-operation.

Replace the current `install --force` whole-store deletion. Refuse `--force`
from a linked context and refuse it from main whenever the managed
`worktrees/` namespace exists, including `.trash/`, `.quarantine/`, an active
server, or an override store. V1 has no bypass. Direct operators to
`worktree analyze --no-incremental` for a linked rebuild. Tests cover active
activity, trash/quarantine, override, partial failure, and concurrent install;
no production path calls `remove_dir_all(repository_store)`.

- [ ] **Step 4: Run command tests and commit the explicit-path migration**

```bash
cargo test -p loomweave-cli --test runtime_path_callsite_audit
cargo test -p loomweave-cli --test db --test hook --test guidance
cargo test -p loomweave-cli --test install --test doctor
cargo check -p loomweave-storage -p loomweave-cli
git add Cargo.lock crates/loomweave-storage crates/loomweave-cli
git commit -m "refactor(runtime): make worktree store paths explicit"
test -z "$(git status --porcelain)"
```

## Part 1 verification

- [ ] **Step 1: Run the focused foundation suites**

```bash
cargo nextest run -p loomweave-core -p loomweave-federation
cargo nextest run -p loomweave-cli \
  --test dotenv_policy \
  --test worktree_analyze \
  --test runtime_path_callsite_audit \
  --test config \
  --test db \
  --test hook \
  --test guidance \
  --test install \
  --test doctor
cargo fmt --all -- --check
git status --short
```

Expected: all focused suites pass and only intentional commits exist.

- [ ] **Step 2: Continue directly to plan 2**

Do not merge or release this intermediate slice. Continue in the same feature
worktree with `2026-07-18-worktree-indexes-2-bootstrap.md`; automatic bootstrap
and the final no-allowlist path audit are acceptance requirements.
