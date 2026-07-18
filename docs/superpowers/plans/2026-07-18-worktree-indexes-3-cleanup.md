# Worktree Indexes Part 3: Lifecycle Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Periodically detect removed Git worktrees and reclaim only validated,
inactive Loomweave stores through a recoverable tombstone lifecycle.

**Architecture:** A fail-soft GC engine consumes one complete hardened Git
inventory, records repeated absence evidence, and uses non-blocking ordered
locks before an atomic handle-relative rename. Recursive deletion occurs only
from the owned `.trash` root after another 24-hour window and is enabled only
where the platform can guarantee no symlink or mount-boundary escape.

**Tech Stack:** Rust 1.88, fs2 locks, Serde/JSON, BLAKE3, rustix descriptor APIs,
Linux `openat2`, Tokio scheduling, fake clocks/filesystems, Cargo Nextest,
Wardline.

**Design:**
[`2026-07-18-loomweave-worktree-indexes-design.md`](../specs/2026-07-18-loomweave-worktree-indexes-design.md)

**Prerequisite:** Complete parts 1 and 2 on the same
`feat/worktree-indexes` branch.

---

## Execution preflight

- [ ] **Step 1: Verify bootstrap work and a clean branch**

```bash
cd /home/john/loomweave/.worktrees/worktree-indexes
test "$(git branch --show-current)" = "feat/worktree-indexes"
test -z "$(git status --porcelain)"
cargo test -p loomweave-mcp --test index_readiness
cargo test -p loomweave-cli --test worktree_serve_bootstrap
```

Expected: automatic bootstrap and readiness suites pass before lifecycle work.

## Portability decision

- Unix uses no-follow directory handles and descriptor-relative rename for
  quarantine/tombstones.
- Linux enables recursive deletion with `openat2` using `RESOLVE_BENEATH`,
  `RESOLVE_NO_SYMLINKS`, and `RESOLVE_NO_XDEV` plus revalidation.
- Non-Linux Unix, Windows, and other targets report
  `recursive_delete_supported=false` and retain tombstones. Do not introduce a
  canonicalize/prefix or `remove_dir_all` fallback.

## File structure

- Extend `crates/loomweave-core/src/worktree/locks.rs` with cleanup lock sets.
- Extend `crates/loomweave-core/src/worktree/metadata.rs` with GC, quarantine,
  and tombstone records.
- Create `crates/loomweave-core/src/worktree/enumeration.rs` for strict worktree
  inventory used by cleanup.
- Create `crates/loomweave-core/src/worktree/gc.rs` for pure candidate and pass
  orchestration.
- Create `crates/loomweave-core/src/worktree/lifecycle_fs.rs` and platform
  implementations under `worktree/lifecycle_fs/`.
- Create `crates/loomweave-cli/src/worktree_cleanup.rs` for helper process and
  scheduler integration.
- Keep destructive filesystem code out of CLI, MCP, and HTTP modules.

### Task 1: Parse complete worktree inventory and conservative GC state

**Files:**

- Create: `crates/loomweave-core/src/worktree/enumeration.rs`
- Create: `crates/loomweave-core/src/worktree/gc.rs`
- Modify: `crates/loomweave-core/src/worktree/metadata.rs`
- Modify: `crates/loomweave-core/src/worktree/mod.rs`
- Create: `crates/loomweave-core/tests/worktree_gc.rs`

- [ ] **Step 1: Add failing inventory, cadence, and presence tests**

```text
porcelain_z_accepts_spaces_and_newlines
porcelain_z_rejects_unterminated_records
porcelain_z_rejects_duplicate_singleton_fields
enumeration_error_returns_no_partial_inventory
absent_malformed_and_future_gc_state_are_due
startup_is_not_due_before_six_hours
analysis_trigger_ignores_six_hour_throttle
present_worktree_clears_orphan_state
git_locked_worktree_is_present_and_protected
first_absence_records_one_confirmation
early_recheck_keeps_one_confirmation
second_absence_after_twenty_four_hours_is_tombstone_eligible
reappearance_during_grace_clears_orphan_state
main_and_quarantine_are_never_candidates
disabled_gc_reports_without_mutation
```

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test -p loomweave-core --test worktree_gc presence_
cargo test -p loomweave-core --test worktree_gc gc_state_
```

Expected: GC state, inventory, and candidate decisions do not exist.

- [ ] **Step 3: Implement strict inventory and records**

```rust
pub struct WorktreeInventory {
    pub entries: Vec<RegisteredWorktree>,
    pub observed_at: time::OffsetDateTime,
}

pub fn enumerate_worktrees(
    git: &TrustedGitContext,
    primary_root: &std::path::Path,
    observed_at: time::OffsetDateTime,
) -> Result<WorktreeInventory, WorktreeContextError>;

pub fn parse_worktree_porcelain_z(
    bytes: &[u8],
    observed_at: time::OffsetDateTime,
) -> Result<WorktreeInventory, WorktreeContextError>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GcState {
    pub schema: String,
    pub last_attempt_at: Option<time::OffsetDateTime>,
    pub last_success_at: Option<time::OffsetDateTime>,
    pub last_error: Option<GcDiagnostic>,
}

pub enum GcTrigger { AnalysisComplete, ServeStartup, Periodic }

pub enum CandidateDecision {
    Refreshed,
    Protected,
    FirstAbsence,
    GracePending,
    TombstoneEligible,
}
```

Malformed/future GC state means `CheckDue`, never deletion authority. A Git
enumeration error returns no inventory and permits no metadata update.

- [ ] **Step 4: Implement pure absence evidence and run GREEN**

Use constants of six hours for startup cadence, 24 hours for orphan grace, and
24 hours for tombstone recovery. Analysis-triggered checks bypass only the
six-hour throttle, never absence windows.

```bash
cargo test -p loomweave-core --test worktree_gc
```

Expected: candidate decisions are deterministic under a fake clock.

- [ ] **Step 5: Commit inventory and evidence**

```bash
git add crates/loomweave-core
git commit -m "feat(core): track conservative worktree absence evidence"
```

### Task 2: Complete the non-blocking lifecycle lock topology

**Files:**

- Modify: `crates/loomweave-core/src/worktree/locks.rs`
- Modify: `crates/loomweave-core/src/worktree/store.rs`
- Modify: `crates/loomweave-core/src/worktree/analysis_intent.rs`
- Modify: `crates/loomweave-cli/src/analyze_lock.rs`
- Create: `crates/loomweave-core/tests/worktree_locks.rs`

- [ ] **Step 1: Add failing lock-order and contention tests**

```text
server_and_analysis_share_activity
shared_activity_blocks_cleanup_exclusive
cleanup_skips_held_intent_writer_or_metadata
cleanup_partial_acquisition_releases_prior_guards
store_open_holds_gc_until_activity_is_acquired
analysis_cannot_acquire_intent_without_activity
analysis_finalization_releases_writer_before_terminal_intent
```

- [ ] **Step 2: Run lock tests and verify RED**

```bash
cargo test -p loomweave-core --test worktree_locks
```

Expected: cleanup lock-set APIs are missing and lock-order assertions fail.

- [ ] **Step 3: Implement typed guards and one non-blocking cleanup set**

```rust
pub struct RepositoryGcGuard { file: std::fs::File }
pub struct SharedActivityGuard { file: std::fs::File }
pub struct ExclusiveActivityGuard { file: std::fs::File }
pub struct IntentLockGuard { file: std::fs::File }
pub struct WriterLockGuard { file: std::fs::File }
pub struct MetadataLockGuard { file: std::fs::File }

pub struct CleanupLockSet {
    activity: ExclusiveActivityGuard,
    intent: IntentLockGuard,
    writer: WriterLockGuard,
    metadata: MetadataLockGuard,
}

pub fn try_acquire_cleanup_locks(
    gc: &RepositoryGcGuard,
    paths: &StorePaths,
) -> Result<Option<CleanupLockSet>, WorktreeLifecycleError>;
```

Acquire cleanup locks in activity, intent, writer, metadata order. Every step
uses try-lock and drops prior guards immediately on contention. Analysis and
servers never request `gc.lock` while holding per-store locks.

- [ ] **Step 4: Run GREEN and commit locks**

```bash
cargo test -p loomweave-core --test worktree_locks
git add crates/loomweave-core crates/loomweave-cli/src/analyze_lock.rs
git commit -m "refactor(core): enforce worktree lifecycle lock order"
```

### Task 3: Add capability-safe quarantine and tombstone relocation

**Files:**

- Modify: `Cargo.toml`
- Modify: `crates/loomweave-core/Cargo.toml`
- Create: `crates/loomweave-core/src/worktree/lifecycle_fs.rs`
- Create: `crates/loomweave-core/src/worktree/lifecycle_fs/unix.rs`
- Create: `crates/loomweave-core/src/worktree/lifecycle_fs/unsupported.rs`
- Modify: `crates/loomweave-core/src/worktree/gc.rs`
- Modify: `crates/loomweave-core/src/worktree/metadata.rs`
- Modify: `crates/loomweave-core/src/worktree/store.rs`
- Modify: `crates/loomweave-core/tests/worktree_gc.rs`

- [ ] **Step 1: Add failing relocation and reappearance tests**

```text
owned_default_store_quarantines_root_mismatch
override_store_never_quarantines
active_store_prevents_quarantine
eligible_candidate_moves_atomically_to_trash
tombstone_name_record_owner_and_identity_match
identity_or_metadata_change_before_rename_preserves_store
git_reappearance_at_final_enumeration_preserves_store
post_rename_identity_mismatch_restores_or_preserves_store
tombstone_record_failure_restores_when_safe
reappeared_identity_restores_matching_tombstone
new_active_store_preserves_old_tombstone
symlinked_candidate_is_never_renamed
quarantine_is_never_a_gc_candidate
```

- [ ] **Step 2: Run relocation tests and verify RED**

```bash
cargo test -p loomweave-core --test worktree_gc tombstone_
cargo test -p loomweave-core --test worktree_gc quarantine_
```

Expected: lifecycle filesystem and relocation records are missing.

- [ ] **Step 3: Add direct rustix support and the lifecycle abstraction**

Add:

```toml
[workspace.dependencies]
rustix = { version = "1.1.4", features = ["fs"] }
```

Use it only under `cfg(unix)` in core. Define:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemIdentity { pub device: u64, pub inode: u64 }

pub trait LifecycleFilesystem: Send + Sync {
    fn supports_atomic_relocation(&self) -> bool;
    fn supports_recursive_delete(&self) -> bool;
    fn inspect_active_store(
        &self,
        namespace: &std::path::Path,
        stable_id: &str,
    ) -> Result<PinnedStore, WorktreeLifecycleError>;
    fn relocate_active_store(
        &self,
        store: PinnedStore,
        destination: RelocationDestination,
    ) -> Result<RelocatedStore, WorktreeLifecycleError>;
    fn restore_relocated_store(
        &self,
        store: &RelocatedStore,
    ) -> Result<(), WorktreeLifecycleError>;
    fn remove_tombstone_tree(
        &self,
        tombstone: PinnedTombstone,
    ) -> Result<(), WorktreeLifecycleError>;
}
```

Unix opens namespace, candidate, trash, and quarantine with
`O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`, renames with `renameat`, reopens the
destination, and compares device/inode. Unsupported platforms return false and
never fall back to string-path deletion.

- [ ] **Step 4: Implement the final-enumeration tombstone protocol**

Under `gc.lock`, acquire cleanup locks, repeat hardened Git enumeration, reread
owner and metadata digests, revalidate identity, rename, validate the moved
identity, and write a checksummed tombstone record. Names must match exactly:

```text
wt-[0-9a-f]{64}-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{32}
```

Quarantine uses the same handle/identity protocol but is operator-retained.

```bash
cargo test -p loomweave-core --test worktree_gc tombstone_
cargo test -p loomweave-core --test worktree_gc quarantine_
```

Expected: every TOCTOU and reappearance case preserves data or moves one
validated inactive store; no recursive deletion exists yet.

- [ ] **Step 5: Commit safe relocation**

```bash
git add Cargo.toml Cargo.lock crates/loomweave-core
git commit -m "feat(core): quarantine and tombstone worktree stores safely"
```

### Task 4: Delete only delayed, validated Linux tombstones

**Files:**

- Create: `crates/loomweave-core/src/worktree/lifecycle_fs/linux.rs`
- Modify: `crates/loomweave-core/src/worktree/lifecycle_fs.rs`
- Modify: `crates/loomweave-core/src/worktree/gc.rs`
- Modify: `crates/loomweave-core/tests/worktree_gc.rs`

- [ ] **Step 1: Add failing delayed-deletion and escape tests**

```text
tombstone_before_recovery_window_is_preserved
later_absence_after_recovery_window_deletes_tombstone
reappeared_identity_prevents_deletion
only_validated_direct_trash_children_are_deleted
malformed_record_or_changed_identity_is_preserved
nested_symlink_preserves_entire_tombstone
nested_mount_preserves_entire_tombstone
revalidation_race_stops_without_escape
unsupported_platform_preserves_tombstone
quarantine_is_never_recursively_deleted
```

Use a fake lifecycle filesystem for deterministic mount/race cases. On Linux,
add a real `openat2` test that skips with an explicit message when the kernel or
test namespace cannot create a bind mount.

- [ ] **Step 2: Run deletion tests and verify RED**

```bash
cargo test -p loomweave-core --test worktree_gc later_absence_
cargo test -p loomweave-core --test worktree_gc nested_
```

Expected: tombstones are always retained because recursive deletion is absent.

- [ ] **Step 3: Implement validate-first, delete-second Linux traversal**

Construct the resolve flags at the point of each descriptor-relative open:

```rust
let resolve = rustix::fs::ResolveFlags::BENEATH
    | rustix::fs::ResolveFlags::NO_SYMLINKS
    | rustix::fs::ResolveFlags::NO_XDEV;
```

First build a bounded post-order manifest using descriptor-relative opens. Cap
it at 100,000 entries, depth 128, and 16 MiB of relative-name bytes; overflow
preserves the tombstone. Reject symlinks, special files, mount crossings, and
unknown top-level store entries. Then reopen and revalidate every identity
before `unlinkat`. Remove the tombstone directory last. Never call
`remove_dir_all`.

- [ ] **Step 4: Run GREEN and commit delayed deletion**

```bash
cargo test -p loomweave-core --test worktree_gc later_absence_
cargo test -p loomweave-core --test worktree_gc nested_
cargo test -p loomweave-core --test worktree_gc unsupported_platform_
git add crates/loomweave-core
git commit -m "feat(core): delete validated worktree tombstones after recovery"
```

### Task 5: Schedule fail-soft cleanup after analysis and during serve

**Files:**

- Create: `crates/loomweave-cli/src/worktree_cleanup.rs`
- Create: `crates/loomweave-cli/tests/worktree_cleanup.rs`
- Modify: `crates/loomweave-cli/src/cli.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/serve.rs`
- Modify: `crates/loomweave-cli/src/serve/bootstrap.rs`

- [ ] **Step 1: Add failing trigger, environment, and lock-release tests**

```text
analysis_success_schedules_after_all_store_locks_drop
analysis_failure_still_schedules_fail_soft_cleanup
cleanup_failure_preserves_analysis_exit_status
serve_startup_schedules_only_when_due
long_running_serve_ticks_every_six_hours
concurrent_helpers_serialize_on_gc_lock
helper_inherits_no_store_lock_descriptors
helper_uses_explicit_git_and_sanitized_environment
cleanup_failure_does_not_fail_mcp_startup
```

- [ ] **Step 2: Run the scheduler test and verify RED**

```bash
cargo test -p loomweave-cli --test worktree_cleanup
```

Expected: no helper or scheduler is available.

- [ ] **Step 3: Add the hidden helper and scheduler**

```rust
pub struct CleanupScheduler {
    tx: tokio::sync::mpsc::Sender<GcTrigger>,
    join: tokio::task::JoinHandle<()>,
}

impl CleanupScheduler {
    pub fn start(
        context: WorktreeContext,
        git: TrustedGitContext,
        executable: std::path::PathBuf,
    ) -> Result<Self, CleanupScheduleError>;
    pub async fn schedule_startup_if_due(&self);
    pub async fn schedule_analysis_complete(&self);
}

pub fn run_cleanup_helper(invocation: CleanupInvocation) -> GcPassReport;
```

Add hidden `loomweave worktree cleanup-helper` arguments for primary path,
trigger, and absolute trusted Git executable. Spawn the current executable with
`env_clear`, the pre-dotenv allowlist, close-on-exec lock descriptors, null
stdout, and inherited stderr. The helper re-resolves and validates context; it
does not trust paths merely because the parent supplied them.

Analysis finalization order is durable run, metadata update, release metadata,
writer, intent, and activity, then schedule cleanup. Preserve the original
analysis result. The serve scheduler ticks every six hours; the helper rechecks
due state under non-blocking `gc.lock`.

- [ ] **Step 4: Run GREEN and commit scheduling**

```bash
cargo test -p loomweave-cli --test worktree_cleanup
git add crates/loomweave-cli
git commit -m "feat(cli): schedule fail-soft worktree cleanup"
```

### Task 6: Expose cleanup diagnostics and operator recovery

**Files:**

- Modify: `crates/loomweave-mcp/src/tools/status.rs`
- Modify: `crates/loomweave-mcp/tests/storage_tools.rs`
- Modify: `crates/loomweave-cli/src/doctor.rs`
- Modify: `crates/loomweave-cli/tests/doctor.rs`
- Modify: `docs/operator/getting-started.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add failing status and doctor tests**

```text
project_status_reports_last_cleanup_diagnostic
project_status_malformed_gc_state_is_check_due
project_status_reports_disabled_gc_reason
project_status_reports_recursive_delete_support
doctor_reports_operator_managed_quarantine
doctor_reports_legacy_linked_store_without_deleting
doctor_reports_retained_unsafe_tombstone
```

- [ ] **Step 2: Run status/doctor tests and verify RED**

```bash
cargo test -p loomweave-mcp --test storage_tools project_status_reports_
cargo test -p loomweave-cli --test doctor doctor_reports_
```

Expected: current status has no worktree cleanup block and doctor has no
lifecycle diagnostics.

- [ ] **Step 3: Read live GC state fail-soft and document recovery**

Add this status shape:

```json
{
  "worktree_cleanup": {
    "gc_capability": "enabled-owned-default",
    "last_attempt_at": null,
    "last_success_at": null,
    "last_error": null,
    "check_due": true,
    "recursive_delete_supported": true
  }
}
```

Read `gc-state.json` on each status request rather than caching it at server
construction. Malformed state reports `check_due=true`; it never authorizes a
mutation. Doctor identifies disabled ownership/confinement, legacy local stores,
manual quarantines, and retained unsafe tombstones without deleting them.

- [ ] **Step 4: Run GREEN and commit diagnostics**

```bash
cargo test -p loomweave-mcp --test storage_tools project_status_reports_
cargo test -p loomweave-cli --test doctor doctor_reports_
markdownlint-cli2 docs/operator/getting-started.md CHANGELOG.md
git add crates/loomweave-mcp crates/loomweave-cli \
  docs/operator/getting-started.md CHANGELOG.md
git commit -m "feat(status): expose worktree cleanup diagnostics"
```

### Task 7: Verify the integrated worktree feature and trust boundaries

**Files:**

- Modify only files required by concrete failures from these gates.

- [ ] **Step 1: Run all focused worktree suites**

```bash
cargo nextest run -p loomweave-core \
  --test hardened_git \
  --test worktree_context \
  --test worktree_store \
  --test analysis_intent \
  --test worktree_locks \
  --test worktree_gc
cargo nextest run -p loomweave-mcp \
  --test index_access \
  --test index_readiness \
  --test analyze_lifecycle
cargo nextest run -p loomweave-cli \
  --test worktree_analyze \
  --test worktree_serve_bootstrap \
  --test worktree_cleanup \
  --test runtime_path_callsite_audit
```

Expected: every focused suite passes.

- [ ] **Step 2: Run repository release gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
wardline scan . --fail-on ERROR
git diff --check
git status --short
```

Expected: formatting, Clippy, all workspace tests, Wardline, and diff checks pass.

- [ ] **Step 3: Dogfood a real divergent worktree and delayed lifecycle**

Create a temporary linked worktree with a unique uncommitted function. Start
`loomweave serve --path <linked-path>`, observe building status, query the
function on the same MCP session, and prove the main graph does not contain it.
Run the explicit command with `--`. Remove the Git worktree, use the test-only
clock seam to perform first absence, second absence after 24 hours, tombstone,
and deletion after another 24 hours. Restore/recreate between each boundary and
prove data is preserved.

- [ ] **Step 4: Commit only concrete gate fixes, then request final review**

If a gate required changes, commit each narrow fix with its regression test.
Then use `superpowers:requesting-code-review` against the full feature branch.
Do not close `clarion-c297efc752` until the reviewer has no Critical or Important
findings and all gates above remain green.
