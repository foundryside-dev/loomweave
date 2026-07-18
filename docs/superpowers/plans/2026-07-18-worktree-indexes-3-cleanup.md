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
- Linux is also the only v1 automatic external-cleanup runner: its supervisor
  uses an owned child-subreaper boundary and drains the worker/Git tree.
- Non-Linux Unix reports `recursive_delete_supported=false` and retains
  tombstones; it also reports automatic cleanup unsupported rather than
  launching a worker without an owned descendant reaper. Do not introduce a
  canonicalize/prefix or `remove_dir_all` fallback.
- Windows and other non-Unix targets use the unsupported backend: both
  `atomic_relocation_supported` and `recursive_delete_supported` are false,
  automatic lifecycle mutation never inspects, renames, or deletes a candidate,
  and active stores and tombstones are preserved with a diagnostic. A Windows
  backend requires explicit reparse-safe implementation plus compile and test
  jobs; v1 does not claim that support.

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
- Extend metadata with a pre-rename relocation journal and recovery scanner.
- Create `crates/loomweave-cli/src/worktree_cleanup.rs` for the Linux
  supervisor/worker pair and scheduler integration.
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
raw_porcelain_requires_validated_identity_enrichment
overridden_store_enrichment_uses_explicit_managed_inventory
managed_namespace_exact_entry_limit_and_plus_one
managed_namespace_exact_name_bytes_limit_and_plus_one
managed_namespace_unknown_name_or_wrong_direct_type_fails_closed
trash_and_quarantine_inventory_limits_are_independent_and_fail_closed
absent_malformed_and_future_gc_state_are_due
startup_is_not_due_before_six_hours
analysis_trigger_ignores_six_hour_throttle
present_worktree_clears_orphan_state
git_locked_worktree_is_present_and_protected
first_absence_records_one_confirmation
early_recheck_keeps_one_confirmation
second_absence_after_twenty_four_hours_is_tombstone_eligible
reappearance_during_grace_clears_orphan_state
orphan_evidence_accepts_only_canonical_zero_one_or_saturated_two
orphan_evidence_rejects_impossible_or_overflowed_persisted_combinations
main_and_quarantine_are_never_candidates
disabled_gc_reports_without_mutation
gc_pass_4097_candidates_advances_cursor_and_resumes_without_starvation
gc_pass_record_byte_boundary_stops_before_next_and_resumes
gc_cursor_namespace_change_reprocesses_without_skipping_stable_id
gc_preflight_budget_overflow_mutates_no_recovery_artifact
recovery_plan_spans_root_and_selected_metadata_artifacts
metadata_change_after_recovery_plan_skips_candidate_without_mutation
namespace_open_uses_bounded_single_store_recovery_plan
gc_pass_attempts_at_most_one_recursive_deletion
gc_reconciles_metadata_update_journal_before_absence_decision
gc_refuses_relocation_with_unresolved_metadata_scratch_or_journal
gc_state_rejects_unknown_diagnostic_code
gc_state_rejects_noncanonical_recovery_and_candidate_cursors
gc_state_rejects_nonnull_recovery_cursor_with_false_wrap_bit
gc_state_diagnostic_1024_bytes_is_preserved
gc_state_diagnostic_1025_bytes_is_utf8_safely_truncated
gc_state_reader_rejects_oversize_diagnostic
```

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test -p loomweave-core --test worktree_gc
```

Expected: GC state, inventory, and candidate decisions do not exist.

- [ ] **Step 3: Implement strict inventory and records**

```rust
pub struct RawWorktreeInventory {
    pub entries: Vec<RawWorktreeEntry>,
    pub observed_at: time::OffsetDateTime,
}

pub struct RawWorktreeEntry {
    pub source_root: std::path::PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub locked: bool,
    pub prunable: bool,
}

pub struct WorktreeInventory {
    pub entries: Vec<RegisteredWorktree>,
    pub observed_at: time::OffsetDateTime,
}

pub fn enumerate_raw_worktrees(
    git: &TrustedGitContext,
    primary_root: &std::path::Path,
    observed_at: time::OffsetDateTime,
) -> Result<RawWorktreeInventory, WorktreeContextError>;

pub fn parse_worktree_porcelain_z(
    bytes: &[u8],
    observed_at: time::OffsetDateTime,
) -> Result<RawWorktreeInventory, WorktreeContextError>;

pub fn enrich_worktree_inventory(
    raw: RawWorktreeInventory,
    git: &TrustedGitContext,
    context: &WorktreeContext,
    authority: &RepositoryAuthority,
    managed: &ManagedNamespaceInventory,
) -> Result<WorktreeInventory, WorktreeContextError>;

pub struct ManagedNamespaceInventory;

pub struct RecoveryCost {
    pub record_bytes: u64,
    pub revalidation_bytes: u64,
    pub planned_mutations: u32,
}

pub struct RecoveryPlan;

pub fn enumerate_managed_namespace(
    authority: &RepositoryAuthority,
) -> Result<ManagedNamespaceInventory, WorktreeLifecycleError>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedGcDiagnostic {
    code: GcDiagnosticCode,
    message: String,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum GcDiagnosticCode {
    GitEnumerationFailed,
    LockUnavailable,
    CapabilityDisabled,
    CandidateUnsafe,
    RelocationFailed,
    DeletionFailed,
    DeadlineExceeded,
    UnsupportedPlatform,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct GcStatePayload {
    schema: String,
    last_attempt_at: Option<time::OffsetDateTime>,
    last_success_at: Option<time::OffsetDateTime>,
    last_error: Option<PersistedGcDiagnostic>,
    recovery_continuation_after_stable_id: Option<String>,
    recovery_wrap_pending: bool,
    continuation_after_stable_id: Option<String>,
}

pub(crate) type GcState = Checksummed<GcStatePayload>;

pub struct GcDiagnosticSnapshot {
    pub code: GcDiagnosticCode,
    pub message: String,
}

pub struct GcStateSnapshot {
    pub last_attempt_at: Option<time::OffsetDateTime>,
    pub last_success_at: Option<time::OffsetDateTime>,
    pub last_error: Option<GcDiagnosticSnapshot>,
    pub check_due: bool,
}

pub enum GcTrigger { AnalysisComplete, ServeStartup, Periodic }
pub enum ServeGcTrigger { Startup, Periodic }

pub enum CandidateDecision {
    Refreshed,
    Protected,
    FirstAbsence,
    GracePending,
    TombstoneEligible,
}
```

Malformed/future GC state means `CheckDue`, never deletion authority. First
enumerate the namespace from the post-open `RepositoryAuthority`; only then run
Git and enrichment. A Git or namespace-enumeration error returns no inventory
and permits no metadata update. `parse_worktree_porcelain_z` produces only raw
Git fields. The separate enrichment step receives the explicit context,
authority, and validated namespace inventory, resolves administrative identity
through the hardened Git runner and managed metadata, and publishes a
`WorktreeInventory` only after every entry succeeds. It never derives a store
from `primary_root`; an overridden-store regression proves this. No
raw/partially enriched entry reaches candidate logic.

Before candidate evaluation, enumerate the pinned `worktrees/`, `.trash/`, and
`.quarantine/` directories without following links. Each independently permits
at most 100,000 direct entries and 16 MiB of direct-child name bytes. The
`worktrees/` root accepts only exact known control leaves with their required
types (`owner.json`, `gc.lock`, `gc-state.json`, `.relocations/`,
`.diagnostics/`, `.trash/`, `.quarantine/`) and no-follow direct directories
named `wt-[0-9a-f]{64}`. Trash and quarantine accept only no-follow direct
directories matching the full relocation name grammar; their contents are not
walked during inventory. Unknown names, wrong types, symlinks, special files,
hardlinked authority leaves, limit overflow, or a read error aborts the whole
inventory and permits no partial metadata update, rename, or deletion. This
read-only inventory also recognizes the foundation's exact fixed
`.owner.json.tmp` and `.gc-state.json.tmp`, validates their types/sizes and
surrounding finals. Build one immutable `RecoveryPlan` before mutation. In this
task a candidate-capable plan spans root ordinary artifacts plus every selected store's
metadata/pending/journal/scratch identities and reserves their complete recovery
plus final-validation work. Task 3 adds a mutually exclusive recovery-only mode
whose root/GC-state base is budgeted first and whose one relocation/destination
sub-batch is selected only against the residual cap. Only after the selected
complete plan fits its budget may execution reconcile/remove anything.
Wrong types, duplicates, oversize, ambiguity, or reservation overflow fail
closed before repair.

After preflight, process one lexicographic batch starting after validated
`GcState.continuation_after_stable_id`: at most 4,096 managed stores, 64 MiB of
aggregate authority-record bytes reserved across every read/revalidation/write,
and one recursive deletion attempt. Stop before the next store that would
exceed a bound, durably advance the cursor only after earlier mutations commit,
and resume it on the next trigger; wrap to null at namespace end. Namespace
changes may cause conservative reprocessing but cannot starve a stable ID.
The batch boundary is continuation, not an error, so 4,097 through 100,000
stores make bounded progress. Every legal single record fits the byte budget.

Execution acquires the candidate lock chain and compares every planned
presence bit, device/inode/size, checksum/version, and planned action before its
first mutation. A concurrent writer-created pending/journal/scratch or any other
change skips the whole candidate without recovery or metadata update; the next
pass inventories and budgets it. Execute only actions/cost already present in
the immutable plan. Namespace open uses the same builder in bounded
single-store mode rather than a separate unbudgeted reconciler.

Validate orphan evidence as exactly one of three canonical states:
`absence_confirmations == 0` with no candidate timestamp; one with a timestamp;
or saturated two with a timestamp. Repeated checks never increment beyond two.
Every other persisted combination, including values greater than two, is
invalid metadata and disables lifecycle mutation rather than being normalized.
Persist `GcState` through the foundation's schema-ordered canonical payload plus
checksum envelope, with RFC 3339 optional timestamps. Apply the same
`Checksummed<T>` codec to relocation, quarantine, and tombstone records; every
reader verifies schema, widths, canonical semantic values, and checksum before
using a record as authority. Keep payloads, envelope aliases, fields, and raw
deserializers module-private so callers cannot deserialize an unchecked payload
or expose a private `Checksummed<T>` in a public interface. Public diagnostics
receive only the validated, non-authoritative `GcStateSnapshot` projection.
`GcDiagnosticCode` serializes as the closed kebab-case values
`git-enumeration-failed`, `lock-unavailable`, `capability-disabled`,
`candidate-unsafe`, `relocation-failed`, `deletion-failed`,
`deadline-exceeded`, and
`unsupported-platform`; readers reject every unknown value. Writers preserve a
message of exactly 1,024 UTF-8 bytes. Longer input retains the largest valid
UTF-8 prefix of at most 1,012 bytes plus the exact 12-byte suffix
" [truncated]". Strict readers reject a persisted message over 1,024 bytes
rather than normalizing it.
Both `recovery_continuation_after_stable_id` and
`continuation_after_stable_id` are null or one exact `wt-[0-9a-f]{64}` value.
They select deterministic recovery and candidate batches respectively and
never supply deletion evidence. `recovery_wrap_pending` is a required JSON
boolean; a non-null recovery cursor requires it to be true. Readers reject a
cursor with a false wrap bit rather than repairing it.

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
test -z "$(git status --porcelain)"
```

### Task 2: Complete the non-blocking lifecycle lock topology

**Files:**

- Modify: `crates/loomweave-core/src/worktree/locks.rs`
- Modify: `crates/loomweave-core/src/worktree/store.rs`
- Modify: `crates/loomweave-core/src/worktree/analysis_intent.rs`
- Modify: `crates/loomweave-cli/src/analyze_lock.rs`
- Create: `crates/loomweave-core/tests/worktree_locks.rs`
- Modify: `crates/loomweave-cli/src/db.rs`
- Modify: `crates/loomweave-cli/src/guidance.rs`
- Modify: `crates/loomweave-cli/src/hook.rs`
- Modify: `crates/loomweave-cli/src/doctor.rs`
- Modify: `crates/loomweave-cli/src/install.rs`
- Modify: `crates/loomweave-cli/src/serve/runtime.rs`
- Create: `crates/loomweave-cli/tests/worktree_activity.rs`

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

Keep those primitive/order tests in core. Put command/runtime barriers in the
CLI integration target:

```text
db_backup_and_checkpoint_hold_activity_to_last_access
guidance_hook_status_and_doctor_hold_activity_to_last_access
install_and_setup_hold_activity_to_last_access
serve_runtime_releases_activity_only_after_actor_shutdown
```

- [ ] **Step 2: Run lock tests and verify RED**

```bash
cargo test -p loomweave-core --test worktree_locks
cargo test -p loomweave-cli --test worktree_activity
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
servers never request `gc.lock` while holding per-store locks. Add barriers that
pause DB backup/checkpoint, guidance, hooks/status, doctor DB inspection,
install/setup, analysis, and service actor shutdown at their last store access;
exclusive GC activity must fail at every pause.

- [ ] **Step 4: Run GREEN and commit locks**

```bash
cargo test -p loomweave-core --test worktree_locks
cargo test -p loomweave-cli --test worktree_activity
git add crates/loomweave-core crates/loomweave-cli
git commit -m "refactor(core): enforce worktree lifecycle lock order"
test -z "$(git status --porcelain)"
```

### Task 3: Add capability-safe quarantine and tombstone relocation

**Files:**

- Modify: `Cargo.toml`
- Modify: `crates/loomweave-core/Cargo.toml`
- Modify: `crates/loomweave-core/src/worktree/mod.rs`
- Create: `crates/loomweave-core/src/worktree/lifecycle_fs.rs`
- Create: `crates/loomweave-core/src/worktree/lifecycle_fs/unix.rs`
- Create: `crates/loomweave-core/src/worktree/lifecycle_fs/unsupported.rs`
- Modify: `crates/loomweave-core/src/worktree/record.rs`
- Modify: `crates/loomweave-core/src/worktree/gc.rs`
- Modify: `crates/loomweave-core/src/worktree/store.rs`
- Modify: `crates/loomweave-core/src/worktree/metadata.rs`
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
kill_before_journal_publish_leaves_active_store_authoritative
kill_mid_journal_temp_write_cleans_reserved_scratch_and_preserves_active
kill_after_journal_link_before_temp_unlink_recovers_active_side
two_link_journal_pair_uses_bounded_transient_reader_and_retains_anchor
journal_scratch_swap_before_link_never_publishes_attacker_bytes
journal_final_swap_after_post_link_verification_retains_trusted_scratch
kill_after_journal_publish_recovers_active_side
kill_after_rename_recovers_trash_side
kill_after_rename_before_source_parent_fsync_recovers_from_journal
kill_after_source_before_destination_parent_fsync_recovers_from_journal
rename_or_restore_parent_fsync_failure_retains_journal
kill_after_revalidation_completes_tombstone
kill_mid_tombstone_record_temp_write_recovers_and_completes
kill_after_tombstone_record_link_before_temp_unlink_recovers
two_link_final_record_pair_normalizes_only_at_commit
post_transient_read_final_swap_and_kill_retains_recovery_anchor
tombstone_record_scratch_swap_never_publishes_attacker_bytes
tombstone_final_swap_after_post_link_verification_retains_journal_and_scratch
kill_after_tombstone_publish_removes_journal
ambiguous_relocation_journal_preserves_both_sides
quarantine_kill_after_journal_recovers_active_side
quarantine_kill_after_rename_completes_quarantine_record
kill_mid_quarantine_record_temp_write_recovers_and_completes
kill_after_quarantine_record_link_before_temp_unlink_recovers
quarantine_record_scratch_swap_never_publishes_attacker_bytes
quarantine_final_swap_after_verification_retains_journal_and_scratch
final_record_plus_journal_removes_only_journal
journal_and_final_reserved_scratch_symlink_or_special_file_fail_closed
journal_bounds_or_unknown_entries_disable_lifecycle_mutation
large_recovery_backlog_advances_cursor_without_candidate_mutation
wrap_bit_fsync_precedes_first_recovery_mutation
recovery_cursor_clears_only_after_no_recovery_wrap
kill_after_recovery_cursor_clear_keeps_wrap_due
recovery_byte_boundary_resumes_without_wedging
root_base_plus_recovery_uses_exact_residual_byte_boundary
root_scratch_cost_cannot_wedge_recovery_selection
over_budget_single_recovery_unit_leaves_cursor_unchanged
unselected_recovery_journals_and_destinations_are_not_read
non_null_recovery_cursor_is_due_inside_six_hours
successful_recovery_subpass_self_continues_before_deadline
recovery_subpass_advances_attempt_but_preserves_last_success
clean_candidate_pass_advances_last_success
malformed_recovery_unit_stops_without_hot_loop
recovery_plan_spans_selected_relocation_and_destination_records
late_metadata_artifact_before_relocation_skips_without_reconcile
override_missing_owner_relocated_or_symlinked_journal_is_report_only
```

- [ ] **Step 2: Run relocation tests and verify RED**

```bash
cargo test -p loomweave-core --test worktree_gc
```

Expected: lifecycle filesystem and relocation records are missing.

- [ ] **Step 3: Add direct rustix support and the lifecycle abstraction**

Plan 1 already added workspace/core `rustix` with `fs`. Add only the Linux
test's `mount` feature here (feature-unified through the core dev/test target):

```toml
[workspace.dependencies]
rustix = { version = "1.1.4", features = ["fs", "mount"] }
```

Use filesystem APIs only under `cfg(unix)` in core; use
`rustix::mount::{mount_bind, unmount}` only under `cfg(target_os = "linux")`
for the privileged test's RAII bind-mount guard. Do not shell out to `mount` or
silently introduce a second mount dependency. Define:

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

Under `gc.lock`, first structurally inventory only the bounded direct
`.relocations/` entries. Group exact names, no-follow types, sizes, link shapes,
and uniqueness by stable ID without decoding every journal or inspecting
destination contents. Starting after validated
`GcState.recovery_continuation_after_stable_id`, select only the next
lexicographic sub-batch whose schema-maximum journal, destination-record,
read/write/revalidation, and fsync cost fits
`64 MiB - root_base_cost`. A recovery-only plan's base contains only root
ordinary artifacts plus the GC-state read/write; it never includes candidate
metadata. Reject a base at or above the cap. Then extend Task 1's immutable
`RecoveryPlan` by decoding those selected
journals, validating their confined references against the bounded
trash/quarantine direct-child inventory, and inspecting only their referenced
destination final/scratch. A selected scratch-only unit proves its
pre-publication state from that same inventory. Do not mutate while planning.
Reject an ambiguous structural inventory or an over-budget single unit.

A pass that first discovers recovery from a null phase durably sets
`recovery_wrap_pending=true` before its first recovery mutation; reserve that
prelude in the root base. A pass with recovery work performs only that planned
sub-batch, advances the recovery cursor only after earlier units are durable,
and performs no candidate metadata or lifecycle mutation. If no unit sorts
after a non-null cursor, clear only the cursor durably and stop the pass; keep
the wrap bit true. Only a later pass that starts with a null cursor and true wrap
bit, finds no recovery unit, and completes candidate-capable work clears the bit
in the same GC-state replacement that advances `last_success_at`. This survives
a kill immediately after cursor-clear fsync. A malformed selected journal,
unsafe destination, or late identity/version change leaves the cursor before
that unit and fails closed without candidate mutation. Namespace open builds
the same complete plan in bounded single-store scope and has no repository
cursor or wrap bit.

A non-null recovery cursor or true wrap bit always makes `check_due` true. After
each successful recovery-only or cursor-clear pass, the worker releases
`gc.lock` and immediately runs another bounded pass while its absolute deadline
remains. `last_attempt_at` advances on every
subpass; `last_success_at` advances only when a pass starts with a null recovery
cursor, finds no recovery unit, and completes candidate-capable work. A
malformed or unsafe selected unit records a terminal diagnostic and stops the
worker instead of hot-looping. If the supervisor deadline expires, the cursor
or wrap bit keeps the next scheduler tick due regardless of the six-hour
cadence.

After the recovery gate, candidate execution performs only planned actions.
For each candidate, acquire cleanup locks and require every metadata and
lifecycle snapshot still to match. A late artifact or version change skips
that candidate without repair.

Then acquire cleanup locks, repeat hardened Git enumeration, reread owner and
metadata digests, and revalidate identity. The budgeted `RecoveryPlan` executor
may run the foundation metadata-journal/scratch reconciler only for an exact
planned action before candidate decision. After taking each candidate's
metadata lock, candidate execution performs a validation-only match against the
plan's expected clean post-recovery shape. A newly appeared or changed pending,
journal, or scratch skips the whole candidate with no repair or metadata write
and forbids relocation. Add a barrier test that creates each artifact between
planned recovery and this validation. Publish exactly
`worktrees/.relocations/<stable-id>.json` atomically. Create a direct regular
scratch file named
`.<stable-id>-journal-<32 lowercase hex nonce>.tmp` with `create_new` and
`O_NOFOLLOW`, retain its open handle and device/inode identity, write the
complete canonical checksummed bytes, and `sync_all` it. Immediately before the
no-replace descriptor-relative hard link, inspect the scratch name without
following links and require it to match the still-open handle. Immediately
after the link, open the final name without following links and require it to
match that same handle before accepting it as authority. Fsync `.relocations/`,
then re-open/revalidate the final name against the still-open scratch handle.
Treat the exact final/scratch same-inode link-count-two shape as a narrow
`TransientPublicationPair`. Read it only through `TransientRecordFile`, which
requires both exact names to match the retained handle at link count two,
enforces the schema byte cap and stable identity/size, and invokes the strict
checksummed codec over that descriptor. Keep the trusted scratch reachable
through relocation and destination-record publication, revalidating final
against it immediately before and after every destructive boundary. This is the
only exception to `DurableRecordFile`'s single-link input. A mismatch, extra
link, duplicate scratch, or name change fails closed, retains every reachable
anchor, and performs no further mutation. Platforms that can publish from an
open file handle use the same boundary. Schema
`loomweave.worktree-relocation.v1` records owner/stable ID, operation
(`tombstone`/`quarantine`), source direct-child name, destination kind
(`trash`/`quarantine`), destination direct-child name, captured identities and
digests, evidence, RFC 3339 timestamp, and 32-hex nonce. Only after the final
journal link is durable may the store rename begin. Then rename, fsync both the
pinned `worktrees/` source parent and selected `.trash/`/`.quarantine/`
destination parent, and validate the moved identity. A restore rename likewise
fsyncs both pinned parents. Any parent-fsync failure retains the journal and
stops reconciliation. Publish the matching checksummed `tombstone.json` or
`quarantine.json` by the same retained-handle, pre-link and post-link identity
protocol using, inside the pinned moved directory,
`.tombstone-<nonce>.tmp` or `.quarantine-<nonce>.tmp`. Fsync that directory
and revalidate the final record while its scratch handle remains open. Apply
the same `TransientRecordFile` boundary and retain both trusted scratches until
the commit point. Then revalidate/unlink/fsync the destination scratch and
decode its now-single-link final through
`DurableRecordFile::open_read_expected` against the retained identity; do the
same for the relocation scratch/final; revalidate both final names; only then
unlink the
durable journal final and fsync `.relocations/`. A failure preserves every
remaining reachable anchor. Names must match exactly:

```text
wt-[0-9a-f]{64}-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{32}
```

Quarantine uses the same journal and handle/identity protocol but is
operator-retained. At namespace open and every GC start, enumerate direct final
journals plus only the exact reserved scratch patterns above, bounded together
to 4,096 files and 16 MiB total bytes. Unknown entries, duplicates, symlinks,
special files, or overflow disable mutation. Exact direct regular scratch names
are reserved implementation artifacts: the capability-enabled reconciler may
unlink them regardless of partial bytes after validating their parent, filename,
type, stable ID, and the surrounding active/destination/final-record state.
For a durable journal whose destination exists, inspect at most the one matching
final-record scratch inside that pinned direct-child destination; any additional
or mismatched scratch is ambiguous and fail-closed.
`doctor` reports but never removes them. Reconcile under `gc.lock`:
operation/destination chooses active plus
trash or active plus quarantine. Active-only after journal means retain or
restore; destination-only means complete its final record; matching final
record plus journal means remove only the journal. Both/neither sides, wrong
destination, mismatches, malformed/future records, and permission errors
preserve everything. A crash mid-scratch-write leaves no visible authority;
reconciliation removes the reserved single-link scratch and either leaves
active unchanged or completes from the durable journal/destination. A crash
after final link but before scratch unlink is accepted only as the exact
same-inode link-count-two pair. Recovery boundedly decodes it through
`TransientRecordFile`, retains the anchor through completion, and normalizes it
only in the commit sequence above. Every other link state fails closed. Pin the
full kill matrix for both tombstone and quarantine through mid-byte writes,
publication links, both rename-parent fsyncs, restore-parent fsyncs, final
record, and journal removal. No new
relocation starts while any journal or reserved scratch is unreconciled.

Before any reconciliation mutation, freshly require
the expected post-open `RepositoryAuthority`, including
`GcCapability::EnabledOwnedDefault`, a valid matching owner/checksum, canonical
repository store, and no-follow confinement. Override, relocated,
missing-owner, copied, and symlinked
namespaces are report-only. `doctor` takes `gc.lock` for a bounded read-only
scan but never restores/completes/removes even a safely recoverable journal; it
reports the action reserved for the next capability-enabled open or GC pass.

```bash
cargo test -p loomweave-core --test worktree_gc
```

Expected: every TOCTOU and reappearance case preserves data or moves one
validated inactive store; no recursive deletion exists yet.

- [ ] **Step 5: Commit safe relocation**

```bash
git add Cargo.toml Cargo.lock crates/loomweave-core
git commit -m "feat(core): quarantine and tombstone worktree stores safely"
test -z "$(git status --porcelain)"
```

### Task 4: Delete only delayed, validated Linux tombstones

**Files:**

- Create: `crates/loomweave-core/src/worktree/lifecycle_fs/linux.rs`
- Modify: `crates/loomweave-core/src/worktree/lifecycle_fs.rs`
- Modify: `crates/loomweave-core/src/worktree/gc.rs`
- Modify: `crates/loomweave-core/tests/worktree_gc.rs`
- Create: `scripts/run-worktree-no-xdev-test.sh`
- Modify: `.github/workflows/verify.yml`

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
entry_limit_exactly_100000_is_accepted
entry_limit_100001_preserves_tombstone
depth_limit_exactly_128_is_accepted
depth_limit_129_preserves_tombstone
name_bytes_exactly_16_mib_is_accepted
name_bytes_over_16_mib_preserves_tombstone
future_or_missing_tombstone_schema_preserves_tree
unowned_relocated_and_symlinked_roots_preserve_tree
permission_failure_at_each_delete_phase_preserves_remaining_tree
real_openat2_rejects_symlink_and_parent_escape_without_skipping
privileged_real_no_xdev_rejects_nested_bind_mount
```

Use a fake lifecycle filesystem for deterministic mount/race cases. On Linux,
the ordinary unprivileged test must never skip: it exercises the real `openat2`
backend against symlink and parent-escape attempts and fails if the syscall is
unavailable while recursive deletion claims support. Add a separate ignored
bind-mount test for real `RESOLVE_NO_XDEV`. Add a repository script that first
builds the exact test binary as the normal user, then executes that binary with
`sudo -n unshare --mount --propagation private`; the test uses an RAII unmount
guard and the namespace provides final cleanup. Add an explicit script step to
the Linux Rust job in `.github/workflows/verify.yml`, before Clippy or any other
all-target compile/test step so its RED state is independently visible;
ordinary nextest does not run ignored tests. Create the script and workflow
step in this test-first step, before implementing the deletion backend.

The privileged test is not satisfied by an always-preserve stub. It first
requires the Linux backend to advertise recursive-deletion support, then proves
a candidate containing a nested bind mount returns a mount-boundary refusal and
remains wholly intact, and finally proves an otherwise identical unmounted
control tombstone is removed. Before Step 3 the support/control assertions fail;
after Step 3 the mounted case remains protected while the control is deleted.

`run-worktree-no-xdev-test.sh` uses `set -euo pipefail`, obtains the exact
`worktree_gc` executable from Cargo's JSON `--no-run` output with a small stdlib
Python parser (no newest-file glob), and executes only
`privileged_real_no_xdev_rejects_nested_bind_mount` inside the private mount
namespace. The Verify step calls this checked-in script verbatim.

- [ ] **Step 2: Run deletion tests and verify RED**

```bash
cargo test -p loomweave-core --test worktree_gc
if sudo -n true; then
  bash scripts/run-worktree-no-xdev-test.sh
else
  echo "privileged RED must be observed in the required Verify job"
fi
```

Expected: tombstones are always retained because recursive deletion is absent.
On a passwordless-sudo host, the ignored bind-mount test also fails through the
checked-in script.

Commit the test-first state as an explicit RED checkpoint before Step 3:

```bash
git add .github/workflows/verify.yml crates/loomweave-core \
  scripts/run-worktree-no-xdev-test.sh
git commit -m "test(core): pin privileged worktree deletion boundaries"
```

If local passwordless sudo is unavailable, do not record the conditional echo
as evidence. Push this checkpoint, create or update the draft PR for
`feat/worktree-indexes`, and wait for the new early Linux Verify step to fail
specifically at `privileged_real_no_xdev_rejects_nested_bind_mount` before
implementing Step 3. A draft PR is required because `ci.yml` runs on
`pull_request`, not arbitrary branch pushes. Retain the failing run URL as RED
evidence; the checkpoint commit remains in history and the Step 4 implementation
commit turns that same test green.

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

Test each cap at the exact limit and limit plus one. Inject permission failures
during root inspection, journal reconciliation, rename/record validation,
manifest traversal, per-entry unlink, and final directory unlink. Cover
malformed, missing, future, traversal, unowned, relocated, symlinked, mounted,
and unsupported records/roots. Every negative case preserves the candidate or
unremoved remainder and emits a bounded diagnostic; none converts uncertainty
into deletion authority.

- [ ] **Step 4: Run GREEN and commit delayed deletion**

```bash
cargo test -p loomweave-core --test worktree_gc
if sudo -n true; then
  bash scripts/run-worktree-no-xdev-test.sh
else
  echo "local privileged leg unavailable; green Verify is required"
fi
git add .github/workflows/verify.yml crates/loomweave-core \
  scripts/run-worktree-no-xdev-test.sh
git commit -m "feat(core): delete validated worktree tombstones after recovery"
test -z "$(git status --porcelain)"
```

The conditional local command is not green evidence when it prints the
unavailable message. Before this task is cleared, the required Linux Verify job
must execute the script and pass the real bind-mount test.

### Task 5: Schedule fail-soft cleanup after analysis and during serve

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/loomweave-cli/Cargo.toml`
- Modify: `crates/loomweave-core/src/worktree/gc.rs`
- Modify: `crates/loomweave-core/src/worktree/store.rs`
- Modify: `crates/loomweave-core/tests/worktree_gc.rs`
- Create: `crates/loomweave-cli/src/worktree_cleanup.rs`
- Create: `crates/loomweave-cli/tests/worktree_cleanup.rs`
- Modify: `crates/loomweave-cli/tests/dotenv_policy.rs`
- Modify: `crates/loomweave-cli/src/cli.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/worktree.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/serve.rs`
- Modify: `crates/loomweave-cli/src/serve/bootstrap.rs`
- Modify: `crates/loomweave-cli/src/serve/runtime.rs`
- Modify: `crates/loomweave-cli/src/owned_process_group.rs`
- Modify: `.github/workflows/verify.yml`
- Create: `scripts/run-worktree-pid1-reaper-test.sh`
- Reuse the target-Unix `nix` signal dependency introduced by plan 2 Task 4.

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
cleanup_helper_reconstructs_exact_parent_git_without_path_lookup
cleanup_helper_revalidates_parent_config_store_capability_and_owner
first_open_scheduler_uses_refreshed_enabled_authority
owner_rebind_scheduler_and_status_use_refreshed_authority
explicit_override_helper_never_opens_default_repository_store
override_removed_before_helper_keeps_default_namespace_absent
supervisor_uses_dedicated_control_not_mcp_stdin_and_worker_nulls
cleanup_helper_skips_repository_dotenv
cleanup_helper_is_hidden_from_worktree_help_but_parses_internally
cleanup_worker_skips_dotenv_and_is_hidden_from_help
cleanup_failure_does_not_fail_mcp_startup
saturated_scheduler_coalesces_without_waiting
serve_spawned_analyzer_owns_exactly_one_detached_cleanup
server_shutdown_detached_analyzer_still_schedules_cleanup
server_monitor_never_schedules_analysis_cleanup
closed_scheduler_is_fail_soft
full_scheduler_diagnostic_is_persisted
closed_scheduler_diagnostic_is_persisted
analysis_spawn_failure_diagnostic_survives_process_exit
cleanup_diagnostic_rejects_unknown_duplicate_and_future_fields
cleanup_diagnostic_equal_timestamp_uses_event_id_tie_break
older_process_event_cannot_replace_newer_durable_diagnostic
snapshot_preserves_memory_when_durable_diagnostic_is_invalid
cleanup_diagnostic_codes_are_closed_and_at_most_64_bytes
cleanup_diagnostic_message_1024_bytes_is_preserved
cleanup_diagnostic_message_1025_bytes_is_utf8_safely_truncated
cleanup_diagnostic_reader_rejects_oversize_message
cleanup_diagnostic_entropy_failure_leaves_state_unchanged_and_logs
cleanup_diagnostic_fixed_scratch_recovers_after_kill_without_accumulation
cleanup_diagnostic_scratch_exact_cap_plus_one_and_wrong_type_fail_soft
cleanup_diagnostic_scratch_swap_never_publishes_attacker_bytes
diagnostic_directory_is_created_only_during_owned_gc_locked_open
symlinked_diagnostic_directory_disables_persistence
pinned_diagnostic_handle_cannot_be_redirected_by_path_swap
analysis_detached_helper_survives_short_lived_runtime_drop
analyzer_group_cancel_after_helper_spawn_does_not_kill_helper
detached_cleanup_supervisor_deadline_exits_fail_closed
worker_deadline_cooperative_term_reaps_active_git
linux_subreaper_forced_kill_reaps_worker_and_git
supervisor_and_worker_term_handlers_precede_child_and_work_boundaries
linux_supervisor_verifies_subreaper_before_worker_spawn
subreaper_setup_failure_spawns_no_worker
supervisor_ready_handshake_precedes_spawned_outcome
supervisor_unsupported_handshake_returns_exact_null_identity_outcome
supervisor_startup_eof_or_timeout_is_spawn_failed_and_spawns_no_worker
provisional_supervisor_drop_or_cancel_terminates_and_reaps
armed_detached_drop_closes_launch_pipe_without_killing_supervisor
armed_launch_write_failure_closes_pipe_and_reaps_before_spawn_failed
owned_launch_write_failure_transitions_draining_and_reaps
kill_after_launch_byte_read_never_loses_worker_tree_reaper
supervisor_term_poll_bounds_cancel_latency_independent_of_deadline
server_shutdown_with_active_git_uses_owned_supervisor
pid1_supervisor_reaps_forced_killed_worker_and_git
pid1_wrapper_executes_server_as_its_only_direct_child
pid1_wrapper_reaps_sigkilled_analyzer_prelaunch_supervisor
pid1_wrapper_reaps_standalone_analyzer_supervisor
pid1_wrapper_reaps_two_overlapping_supervisors_without_registry
pid1_wrapper_waits_for_echild_after_inner_server_exit
pid1_wrapper_forwards_term_only_to_inner_server_group
pid1_wrapper_buffers_term_and_int_until_inner_ready
pid1_wrapper_ready_timeout_reaps_and_disarms_inner
pid1_inner_signal_bridge_runs_nonconsuming_shutdown
pid1_wrapper_disarms_forwarding_after_inner_exit
pid1_wrapper_maps_normal_signal_and_spawn_exit_status
pid1_wrapper_persistent_wait_error_backs_off_and_retains_init
standalone_analyze_pid1_reports_unsupported_without_spawn
cleanup_schedule_precedence_nonlinux_before_repository_authority
cleanup_schedule_precedence_pid1_before_repository_authority
cleanup_schedule_precedence_linux_repository_before_supervisor_startup
production_pid1_fixture_reports_real_getpid_one
default_release_parser_rejects_pid1_fixture_command
pid1_script_uses_cargo_json_executable_with_custom_target_dir
json_analysis_reports_detached_cleanup_process_identity
json_analysis_reports_cleanup_spawn_failure_exactly
json_analysis_reports_cleanup_identity_unavailable_exactly
json_analysis_reports_repository_unavailable_exactly
json_analysis_process_identity_shape_is_exact_and_bounded
serve_shutdown_closes_cancels_and_joins_scheduler
serve_shutdown_begin_close_freezes_timer_and_pending_work
serve_shutdown_with_inflight_supervisor_drains_worker_tree
shutdown_error_retains_supervisor_owner_and_retry_reaps_it
supervisor_descendant_wait_error_retries_internally_to_echild
supervisor_persistent_wait_error_backs_off_and_rate_limits_diagnostic
cleanup_supervisor_owns_separate_worker_process_group
shutdown_signals_only_cleanup_worker_group
unsupported_platform_scheduler_spawns_no_external_supervisor
serve_runtime_shares_one_diagnostic_sink_with_scheduler
diagnostic_sink_outlives_scheduler_join_during_shutdown
```

- [ ] **Step 2: Run the scheduler test and verify RED**

```bash
cargo test -p loomweave-core --test worktree_gc
cargo test -p loomweave-cli --test worktree_cleanup
bash scripts/run-worktree-pid1-reaper-test.sh
```

The host-side script creates a temporary Git repository, result file, target
directory, and nonce. It builds the real CLI with the non-default
`pid1-test-fixture` feature and Cargo JSON output, selects the canonical
non-null `compiler-artifact.executable` whose target name is exactly
`loomweave`, and runs that absolute executable directly as:

```bash
CARGO_TARGET_DIR="$target_dir" cargo build -p loomweave-cli --bin loomweave \
  --features pid1-test-fixture --message-format=json-render-diagnostics \
  >"$artifacts"
binary="$(jq -r \
  'select(.reason == "compiler-artifact" and .target.name == "loomweave" \
  and .target.kind == ["bin"] and .executable != null) | .executable' \
  "$artifacts" | tail -n 1)"
test -n "$binary" && test -x "$binary"
binary="$(realpath -- "$binary")"
LOOMWEAVE_PID1_FIXTURE_NONCE="$nonce" \
unshare --user --map-root-user --pid --fork --kill-child --mount-proc \
  "$binary" worktree pid1-reaper-fixture \
  --scenario "$scenario" --repository "$repository" \
  --result "$result" --nonce "$nonce"
```

Expected RED: the feature-gated fixture and production PID1 wrapper path do
not exist. Lack of user/PID namespaces is an unavailable local leg, not green
evidence; the required Linux Verify job below must run it.

- [ ] **Step 3: Add the hidden helper and scheduler**

Add workspace and direct CLI `signal-hook = "0.3.18"`. Extend the workspace
`rustix` dependency with `process` and add it directly to the CLI; on Linux the
cleanup supervisor uses its safe child-subreaper and wait APIs. Both supervisor
and worker use safe atomic-flag registration for TERM. This introduces no
project `unsafe` exception and does not depend on Tokio's disabled `signal`
feature. Install each handler before its child/work boundary, and cross a
readiness barrier before the worker opens repository state or spawns Git.
Non-Linux targets record `unsupported-platform` and spawn no automatic cleanup
supervisor in v1. Extend rather than replace the CLI's existing feature table.

```toml
# workspace dependencies after Task 3
rustix = { version = "1.1.4", features = ["fs", "mount", "process"] }
signal-hook = "0.3.18"

# loomweave-cli additive non-default feature
[features]
pid1-test-fixture = []

# loomweave-cli target dependencies
[target.'cfg(target_os = "linux")'.dependencies]
rustix.workspace = true
signal-hook.workspace = true
```

```rust
pub struct CleanupScheduler {
    tx: tokio::sync::mpsc::Sender<ServeGcTrigger>,
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Option<tokio::task::JoinHandle<CleanupSchedulerWorkerExit>>,
    incomplete_supervisor: Option<OwnedCleanupSupervisor>,
    shutdown_started: bool,
    diagnostics: CleanupDiagnosticSink,
}

impl CleanupScheduler {
    pub fn start(
        context: std::sync::Arc<WorktreeContext>,
        authority: std::sync::Arc<RepositoryAuthority>,
        git: std::sync::Arc<TrustedGitContext>,
        process_env: std::sync::Arc<PreDotenvProcessEnvironment>,
        diagnostics: CleanupDiagnosticSink,
        executable: std::path::PathBuf,
    ) -> Result<Self, CleanupScheduleError>;
    pub fn try_schedule_startup_if_due(&self) -> CleanupScheduleOutcome;
    pub fn try_schedule_periodic(&self) -> CleanupScheduleOutcome;
    pub fn begin_close(&mut self);
    pub async fn close_cancel_and_join(&mut self)
        -> Result<(), CleanupScheduleError>;
}

pub enum SupervisorStartupStatus { Ready, Unsupported }
pub struct OwnedCleanupSupervisor;
pub enum OwnedCleanupSupervisorState { AwaitingLaunch, Running, Draining }
pub struct Pid1ServeWrapper;

impl Pid1ServeWrapper {
    pub fn run(
        invocation: ServeInvocation,
        process_env: PreDotenvProcessEnvironment,
    ) -> Result<std::process::ExitCode, Pid1WrapperError>;
}

pub enum CleanupSchedulerWorkerExit {
    Drained,
    Incomplete {
        supervisor: OwnedCleanupSupervisor,
        error: CleanupScheduleError,
    },
}

pub fn run_cleanup_supervisor(
    invocation: CleanupInvocation,
) -> Result<(), CleanupScheduleError>;
pub fn run_cleanup_worker(invocation: CleanupInvocation) -> GcPassReport;

pub struct CleanupInvocationIdentity {
    pub primary_root: std::path::PathBuf,
    pub config_origin: ConfigOrigin,
    pub expected_repository_store: std::path::PathBuf,
    pub expected_gc_capability: GcCapability,
    pub expected_owner_id: Option<String>,
    pub trusted_git_executable: std::path::PathBuf,
}

#[derive(Clone)]
pub struct CleanupDiagnosticsHandle;

#[derive(Clone)]
pub struct CleanupDiagnosticSink;

pub struct CleanupDiagnosticView {
    pub latest: Option<CleanupDiagnosticSnapshot>,
    pub read_warning: Option<CleanupDiagnosticReadWarning>,
}

pub enum CleanupDiagnosticReadWarningCode {
    InvalidDurableDiagnostic,
    DurableDiagnosticIo,
}

pub struct CleanupDiagnosticReadWarning {
    pub code: CleanupDiagnosticReadWarningCode,
    pub message: String,
}

impl CleanupDiagnosticSink {
    pub fn from_handle(handle: CleanupDiagnosticsHandle) -> Self;
    pub fn record(&self, event: CleanupDiagnosticEvent);
    pub fn snapshot(&self) -> CleanupDiagnosticView;
}

pub fn open_repository_cleanup_diagnostics(
    authority: &RepositoryAuthority,
) -> CleanupDiagnosticsHandle;

```

Install this scheduler directly into `ServeRuntime`. The runtime is its sole
owner and only startup plus the six-hour timer send `ServeGcTrigger`; protocol
handlers and bootstrap monitors receive no analysis-complete trigger handle.
Every analyzer process owns exactly one post-run detached cleanup spawn, so a
serve-spawned analyzer that outlives server shutdown still performs the check
without a duplicate server trigger.

Add hidden `loomweave worktree cleanup-helper` arguments for primary path,
trigger, absolute trusted Git executable, exact `ConfigOrigin`, and the complete
expected post-open `RepositoryAuthority` (canonical repository store,
`GcCapability`, and owner ID when enabled). Spawn the current executable with
`PreDotenvProcessEnvironment::apply_to_command`, close-on-exec lock descriptors,
the dedicated startup pipes, and inherited stderr. After acknowledgement the
supervisor closes those pipes and spawns `cleanup-worker` with null
stdin/stdout. The hidden pair receives the
same process environment captured before dotenv, then reconstructs
`TrustedGitContext` with `from_explicit_executable`; it requires an absolute,
canonicalizable regular executable and runs all Git validation/probes through
that exact path without a `PATH` lookup. It applies an explicit config origin
exactly and otherwise re-runs normal provenance selection without opening a
namespace. Stage one canonicalizes the resolved repository store and compares
it to the expected store. Only on equality does stage two call plan 1's
non-creating, non-rebinding `probe_existing_repository_authority` on that exact
path and compare capability/owner. Mismatch emits a memory/stderr diagnostic
and touches neither the expected nor default store. It does not trust paths
merely because the parent supplied them. Extend
`should_load_dotenv` and the real-binary `dotenv_policy.rs` target now that this
hidden pair exists; neither the `cleanup-helper` supervisor nor its
`cleanup-worker` child may load repository dotenv, and both null MCP stdin.

Add a non-default CLI feature `pid1-test-fixture = []`. Only that feature
compiles the Linux-only hidden `loomweave worktree pid1-reaper-fixture`
subcommand; an ordinary release parser rejects the command rather than merely
hiding it from help. The fixture requires
`LOOMWEAVE_PID1_FIXTURE_NONCE` to equal its 32-lowercase-hex `--nonce`, accepts
only a closed `--scenario`, temporary repository, and new result path, and
invokes the real PID1 wrapper, inner `ServeRuntime`, `BootstrapControl`, hidden
analyzer, supervisor, and worker paths. The closed scenarios are `normal`,
`term-before-ready`, `term-after-ready`, `int-after-ready`, and
`wrapper-wait-error`. Feature-gated barriers and one injected wait result may
pause those production paths, but the fixture cannot substitute test-only
process owners or reapers. `normal` exercises SIGKILL before supervisor launch,
an independently invoked analyzer, and two overlapping supervisors, then
atomically writes this strict result only after the wrapper reaches `ECHILD`:

```json
{
  "schema_version": 1,
  "scenario": "normal",
  "self_pid": 1,
  "inner_server_pid_non1": true,
  "sigkill_prelaunch_reaped": true,
  "standalone_analyzer_supervisor_reaped": true,
  "overlapping_supervisors_reaped": 2,
  "worker_tree_echild": true,
  "final_echild": true,
  "shutdown_complete": true
}
```

The other scenarios write the exact alternate shape below. The signal cases
self-signal through the production wrapper at their named barrier. Before-ready
signals remain buffered until the inner bridge is ready; signal cases require
inner code 143 for TERM or 130 for INT. The wait-error case requires observed
backoff and rate limiting before recovery.

```json
{
  "schema_version": 1,
  "scenario": "term-before-ready",
  "self_pid": 1,
  "inner_ready": true,
  "scheduler_frozen": true,
  "actor_activity_teardown": true,
  "owned_supervisor_drained": true,
  "inner_exit_code": 143,
  "post_exit_forward_attempts": 0,
  "wait_error_backoff_observed": false,
  "final_echild": true,
  "shutdown_complete": true
}
```

The outer host-side script validates the exact schema and values, then confirms
there are no surviving namespace descendants for every scenario. It also builds
the default feature set separately and proves that binary rejects the fixture
command. Exact tests additionally pin normal exits 0 and nonzero, pre-READY
inner failure, inner spawn failure 1, post-exit signal disarming, and signal
status mapping.

Supervisor spawn uses Plan 2's provisional owned handle and dedicated piped
stdin/stdout for a fixed one-byte startup protocol; neither pipe is the MCP
transport. The supervisor installs TERM, enables and verifies child-subreaper
mode, then writes exactly `0x01` (ready) or `0x02` (unsupported) and cannot spawn
the worker until it reads the launcher's exact `0xa5` acknowledgement. The
launcher retains terminate/reap ownership while allowing two seconds for ready.
`UNSUPPORTED` exits and is reaped, returning `unsupported-platform` with null
PID/identity. EOF, invalid bytes, or timeout makes the owning launcher terminate
and reap the pre-worker supervisor and return `spawn-failed` with
`helper-spawn-failed`.

After ready, an analyzer first consumes the provisional handle into Plan 2's
non-killing `ArmedDetachedProcess`, which still owns the direct `Child` for
waiting. `commit_launch` writes `0xa5`, closes the pipes, and on success releases
only that wait owner into the detached identity. Drop or launch-write failure
closes the pipe and reaps the pre-worker supervisor before returning. Abrupt
analyzer death falls back to init; if death occurs after the write, no kill
owner exists and the supervisor survives to drain its worker tree. A scheduler
instead promotes the provisional handle to `OwnedCleanupSupervisor`, stores it
in its in-flight slot, and only then calls non-consuming `commit_launch`. Write
failure transitions `AwaitingLaunch` to `Draining`, closes the pipe, and reaps
before returning `spawn-failed`. Ready plus successful launch returns `spawned`
or `spawned-identity-unavailable` without waiting for cleanup. The worker always
uses null stdin/stdout. A standalone analyzer whose own real PID is 1 returns
unsupported before spawn.

The server scheduler has capacity one and coalesces startup/periodic work with
an atomic pending bit. Every trigger uses `try_send` and never waits behind a
slow supervisor. On a full channel it sets pending; after each supervisor the
worker takes that bit and reruns once. Analysis-complete is not a scheduler
input and
always bypasses the six-hour throttle through its analyzer-owned detached
supervisor. At most one server supervisor runs; closed/full channels are
fail-soft diagnostics. `CleanupScheduler::begin_close` immediately closes
trigger intake, cancels the six-hour producer, and freezes and clears the
pending bit without dropping an active supervisor. It then completes the non-consuming
close/cancel/join sequence during shutdown. Advancing fake time while shutdown
is blocked cannot create a new supervisor.

On Linux the scheduler owns the supervisor as its direct child. The supervisor
first calls rustix's safe child-subreaper setter and verifies the setting, then
uses plan 2's process-group utility to spawn the cleanup worker in a fresh PGID.
Git inherits only that worker group. The supervisor performs no repository
traversal or Git probe; it owns the absolute deadline, direct worker `Child`,
and descendant wait loop. The scheduler's `OwnedProcessGroup` contains only the
supervisor; shutdown closes intake and sends TERM to that group, whose installed
handler requests cancellation but does not exit. The supervisor sends TERM only
to its worker/Git group. Its control loop never blocks in `Child::wait`: it
checks the TERM flag, absolute deadline, and `try_wait` at most every 25 ms.
After five seconds it sends KILL if needed, then uses non-blocking `waitpid`
polls at the same interval to reap the worker and adopted descendants until
`ECHILD`. `EINTR` retries immediately without a diagnostic and `ECHILD` is the
only success. Every other wait error retains the live supervisor, retries with
exponential backoff from 25 ms to one second, and emits one bounded diagnostic
on the first error plus at most one per minute thereafter. It has no
post-startup failure return and exits only after proving `ECHILD`. The scheduler
never kills or abandons that reaping owner. Its non-consuming shutdown method
polls the direct supervisor for ten seconds. Parent-side wait error or timeout
stores the owner in `incomplete_supervisor` and returns a retryable stop error.
`ServeRuntime`
retains the scheduler, refuses later activity/runtime teardown, and retries the
same method; success is the only transition that consumes the owner and joins.
Tests inject one transient and one persistent descendant `waitpid` error,
verify backoff/rate limiting without owner loss, then separately force one
parent timeout and require owner-retaining retry. Non-Linux targets record
unsupported and spawn no external supervisor.

Analyzer-owned cleanup uses Plan 2's provisional detached spawn to launch that
same supervisor, not a bare worker. Before ready it retains the provisional
kill/reap owner; after ready it arms the non-killing state before sending the
launch byte. The supervisor then creates the separate worker/Git group and
enforces the ten-minute deadline. The worker propagates
remaining time into every Git probe and checks cancellation/deadline between
every bounded filesystem step. Cooperative TERM lets the runner reap Git and
record `deadline-exceeded`; forced escalation is reaped by the surviving
subreaper supervisor. Barriers terminate the analyzer before ready, after arm,
after the supervisor reads `0xa5`, and after worker spawn; no launched worker
ever loses its supervisor reaper.

When Linux `loomweave serve` starts with real PID 1, `main` enters a minimal
`Pid1ServeWrapper` before constructing `ServeRuntime`. The wrapper installs safe
TERM/INT flags, captures the pre-dotenv environment, and spawns the same exact
executable once in a fresh process group with the original serve invocation and
a hidden bounded control socket. The wrapper passes a UUID-v4 simple-form nonce
in the hidden invocation, writes its exact 32 lowercase-hex bytes on the socket,
half-closes its write side, and accepts only the child's single READY byte before
both sides close it. The
internal child requires `getppid()` to equal 1 and constant-time validates the
socket nonce against the hidden value without accepting extra bytes;
direct user invocation fails closed. It immediately installs safe TERM/INT
flags and a bridge into the ordinary non-PID1 shutdown request, then writes the
exact one-byte handler-READY value `0x01` before constructing protocol/runtime
children. It checks any pending flag before each child-producing startup
boundary: before runtime creation it exits with the mapped signal code and no
children; after partial/full creation it enters the same non-consuming
`ServeRuntime` shutdown state machine. Invalid nonce/control data, EOF, or child
exit before READY is startup failure and creates no forwarding target. READY has
a two-second monotonic deadline; timeout terminates and reaps the still-owned
inner group, disarms the PGID, and returns 1. The wrapper process never opens
repository/config state, runs a protocol runtime, or spawns any other direct
child.

The wrapper forwards TERM/INT only to the inner server group and is the sole
`waitpid(-1, WNOHANG)` caller in PID1. Its loop polls at most every 25 ms,
buffers the first observed service signal until READY, forwards each newly
observed signal at most once after READY, drains all available exit events, and
retains only the inner PID/status plus bounded counters. The inner bridge turns
TERM/INT into protocol-intake stop, `CleanupScheduler::begin_close`, protocol
and actor drain, retrying scheduler close/cancel/join, sink/activity teardown,
and runtime stop; it never returns while an owned supervisor remains. All normal
direct-child ownership and waits remain inside the inner server. Only after
those parents exit do detached
analyzers, supervisors, or other orphans become wrapper children, so generic
reaping cannot steal a live owner's child status. The wrapper records the inner
server's exit status, immediately clears its forwardable PGID, and remains until
`waitpid(-1)` returns `ECHILD`. A later signal therefore cannot target a reused
PGID. Normal exit codes are preserved; signal termination maps to `128 + signal`
(`SIGTERM` 143, `SIGINT` 130); inner spawn failure returns 1 after a bounded
diagnostic. `EINTR` retries immediately, `ECHILD` succeeds only after the inner
status was captured, and every other wait error retains PID1 ownership, backs
off from 25 ms to one second, and logs initially plus at most once per minute.
It stores no per-child collection, so overlapping and
unregistered external analyzer supervisors cannot overwrite a slot or grow an
unbounded registry. On inner shutdown, `begin_close` freezes scheduler producers
first; the inner runtime then follows its ordinary owner-retaining shutdown and
may detach an analyzer. The wrapper remains as the persistent init until that
analyzer and every later supervisor finish. A standalone analyzer whose own PID
is 1 still returns exact `unsupported-platform` and spawns no supervisor.

Add a bounded `CleanupDiagnosticSink`. It retains the newest event in memory
for the running service and atomically replaces a separate checksummed,
non-authoritative `cleanup-diagnostic.json` through one fixed
`.cleanup-diagnostic.tmp` under `cleanup-diagnostic.lock` for
scheduler-full/coalesced, scheduler-closed, helper-spawn failure, and abnormal
helper exit/deadline. Before read/write, the lock holder accepts only an absent scratch
or one direct regular single-link scratch within the diagnostic schema cap;
the writer retains its handle/identity, revalidates the scratch name before
rename, and accepts the final only through `open_read_expected` against that
identity.
Unambiguously unpublished scratch is removed and the directory fsynced. A
wrong type, extra scratch-shaped name, oversize, or final/scratch ambiguity
suppresses durable persistence. Atomic replacement prevents torn concurrent
writes; the deterministic event ordering below resolves which
durable/in-memory event status displays. This record never controls cadence,
eligibility, or deletion; malformed records are ignored with a warning.
Diagnostic persistence failure is also logged and cannot change the analysis
or serve result. Task 6 merges the latest durable event with a newer in-memory
service event in status.

The durable handle and sink are core-owned, not constructed from CLI string
paths. `open_repository_cleanup_diagnostics` receives the post-open authority,
acquires `gc.lock`, freshly validates capability/ownership and no-follow
confinement, creates and pins the
real diagnostics directory when permitted, and otherwise returns a
memory-only handle with a bounded reason. `open_or_initialize_linked_store`
uses the same internal under-`gc.lock` operation before shared activity and
retains a cloneable handle plus refreshed authority in `LinkedStoreGuard`;
main/standalone entry points call the public repository-open function before
taking activity. Directory
swaps after open cannot redirect the pinned handle. CLI constructs
`CleanupDiagnosticSink::from_handle` and never reopens a path for writes.

`ServeRuntime` creates and owns exactly one sink before scheduler start. It
clones that sink into `CleanupScheduler` and retains a status-sink accessor for
Task 6; scheduler call sites record full/closed and worker spawn/exit events.
Every analyzer, including a serve-spawned hidden child, creates its own sink from
the handle obtained during store/repository open, retains it after dropping all
per-store guards, and owns exactly one detached supervisor spawn plus its
failure diagnostic. The server monitor never schedules analysis cleanup. On
service shutdown, stop protocol intake, call scheduler begin-close, drain
protocol state, then close/cancel/join the scheduler while the runtime-owned
sink is still live. Drop the sink before final activity/runtime teardown. A
detached analyzer remains responsible for its own post-run spawn after server
shutdown.

Use `RepositoryStorePaths.cleanup_diagnostic`; no caller joins the filename.
Its parent is the typed `cleanup_diagnostics_dir`; the one fixed atomic-write
scratch remains inside that non-authoritative directory and is exactly
`.cleanup-diagnostic.tmp`. It is never inventory,
eligibility, or deletion evidence, and a missing/symlinked diagnostics directory
suppresses the diagnostic fail-soft.
Create and pin that real directory only through the core repository/namespace
open above while `gc.lock` is held, before taking shared activity. Override,
unowned, relocated, or symlinked namespaces get an in-memory-only handle. Later
scheduling never requests `gc.lock` while holding activity and never lazily
creates a path. Handle creation is fail-soft and always yields a usable sink;
persistence setup failure is recorded/logged, not propagated.
The core handle also pins the typed `cleanup_diagnostics_lock` leaf. Persistence
try-locks it without waiting, rereads and strictly validates the current record,
and atomically replaces only when the incoming `(observed_at, event_id)` tuple
is greater. Busy lock, invalid current record, or I/O failure preserves the
durable file and leaves the event in memory/logs. Diagnostic code never takes
`gc.lock` or a per-store lock after taking this lock; server calls may coexist
with their lifetime shared activity because no diagnostic path is a GC
candidate. A reversed two-process write test proves an older event cannot
replace a newer durable event.
Kill-before-rename, exact-cap, plus-one, wrong-type, and repeated-crash tests
prove the fixed scratch is reconciled or persistence stays fail-soft without
artifact accumulation.
Both writes and `snapshot()` use the pinned `CleanupDiagnosticsHandle`; no
status read accepts `RepositoryStorePaths` or re-resolves the directory. Extend
the path-swap race to prove writes and reads remain on the original handle and
never consume an attacker replacement. `snapshot()` strictly reads the durable
record, merges it with memory by the tuple rule, and returns a bounded separate
`read_warning` if durable read/validation fails. Such failure suppresses only
durable state: a valid in-memory event remains in `latest`.
The private canonical payload schema is
`loomweave.worktree-cleanup-diagnostic.v1` with RFC 3339 `observed_at`, a random
32-hex `event_id`, trigger, code capped at 64 bytes, and message capped at 1,024
bytes. Reuse the strict `Checksummed<T>` codec, rejecting unknown, duplicate,
missing, malformed, and future fields. Compare events by
`(observed_at, event_id)` and select the lexicographically greater tuple; equal
tuples identify the same event. Core generates `event_id` through its existing
`getrandom::fill` CSPRNG boundary; CLI gains no random-number dependency and
UUID remains reserved for run IDs. This deterministic rule is for diagnostic
persistence and status only; it never affects cadence, eligibility, or deletion
authority.

`code` serializes a closed `CleanupDiagnosticCode` enum; every defined value is
at most 64 ASCII bytes and readers reject unknown values. Writers preserve a
message of exactly 1,024 UTF-8 bytes. For longer input they retain the largest
valid UTF-8 prefix of at most 1,012 bytes and append the exact 12-byte suffix
" [truncated]"; readers reject persisted messages over 1,024 bytes rather than
normalizing attacker-controlled records. If injected `getrandom::fill` fails,
`record()` leaves durable and in-memory state unchanged and emits a bounded
stderr/log diagnostic; it never substitutes UUID, time, PID, or weak entropy.

Analysis does not use that async queue. Every analysis entry point reaches one
shared terminal epilogue on success and failure. The epilogue persists whatever
terminal run/metadata state is valid, releases every metadata, writer, intent,
and activity guard it acquired, and produces exactly one
`CleanupScheduleOutcome`. It first applies the platform and standalone-PID1
checks in the exact precedence below. A remaining Linux analysis with validated
authority calls `spawn_provisional_detached_in_fresh_group` once and completes
only the bounded startup handshake before the analysis runtime can drop. A
failure before validated repository open performs no spawn, reports the exact
`repository-unavailable` outcome, and opens no guessed path. It never waits for
cleanup work beyond that handshake and preserves the original analysis
result. Tests cover early failure, post-run failure, success, and double-error
unwinding to prove zero or one authority acquisition but exactly one scheduling
outcome.
The `--json` analysis result additively reports `cleanup_schedule` with exactly
four always-present fields:

```json
{
  "outcome": "spawn-failed",
  "pid": null,
  "process_start_identity": null,
  "diagnostic": {
    "code": "helper-spawn-failed",
    "message": "bounded diagnostic"
  }
}
```

The closed outcomes are `spawned`, `spawned-identity-unavailable`,
`spawn-failed`, `repository-unavailable`, and `unsupported-platform`.
`diagnostic` is null or exactly `{ code, message }`; its closed code is
`process-identity-unavailable`, `helper-spawn-failed`,
`repository-unavailable`, or `unsupported-platform`, and its message follows
the 1,024-byte UTF-8 rule. The required matrix is:

- `spawned`: positive-i32 PID, non-null identity, null diagnostic.
- `spawned-identity-unavailable`: positive-i32 PID, null identity, diagnostic
  code `process-identity-unavailable`.
- `spawn-failed`: null PID/identity, diagnostic code `helper-spawn-failed`.
- `repository-unavailable`: null PID/identity, diagnostic code
  `repository-unavailable`.
- `unsupported-platform`: null PID/identity, diagnostic code
  `unsupported-platform`.

Every other null/presence/code combination and every unknown field is rejected.
Scheduling uses this exact first-match precedence:

1. A non-Linux target returns `unsupported-platform` without resolving or
   opening repository authority.
2. A Linux standalone analyzer whose real PID is 1 returns
   `unsupported-platform` without resolving or opening repository authority.
3. Any other Linux analysis that cannot obtain validated repository authority
   returns `repository-unavailable` without spawning.
4. With Linux authority, a supervisor unsupported byte returns
   `unsupported-platform`; EOF, invalid data, timeout, or acknowledgement
   failure returns `spawn-failed` after the pre-worker supervisor is reaped;
   ready plus acknowledgement returns `spawned` or
   `spawned-identity-unavailable` according to identity availability.

Cross-product tests pin each earlier rule against every later condition. In
particular, the first two cases do not touch a repository path, and the third
does not attempt a supervisor spawn. These fields let operators observe the
detached child without cancellation authority, and every outcome preserves the
analysis result. On Linux the PID and process-start identity name the cleanup
supervisor, never its worker or Git descendant.

The only non-null v1 process identity has this exact JSON shape:

```json
{
  "kind": "linux-procfs",
  "boot_id": "01234567-89ab-cdef-0123-456789abcdef",
  "start_time_ticks": "123456"
}
```

PID is a JSON integer in `1..=i32::MAX`; boot ID is exactly 36 lowercase UUID
characters; start time is a canonical unsigned decimal string of 1 to 20
digits. Unknown kinds/fields, uppercase/noncanonical values, numeric start time,
and overflow are rejected. `spawned` requires PID plus this object;
all other identity rules are fixed by the matrix. Exact JSON tests pin all five
complete objects.
The serve scheduler ticks every six hours; the helper rechecks due state under
non-blocking `gc.lock`.

- [ ] **Step 4: Run GREEN and commit scheduling**

```bash
cargo test -p loomweave-core --test hardened_git
cargo test -p loomweave-core --test worktree_gc
cargo test -p loomweave-cli --test worktree_cleanup
cargo test -p loomweave-cli --test dotenv_policy
bash scripts/run-worktree-pid1-reaper-test.sh
git add .github/workflows/verify.yml Cargo.toml Cargo.lock \
  crates/loomweave-core crates/loomweave-cli \
  scripts/run-worktree-pid1-reaper-test.sh
git commit -m "feat(cli): schedule fail-soft worktree cleanup"
test -z "$(git status --porcelain)"
```

The Linux Verify workflow must execute the PID-namespace script as a required
step. The exact executable selected from Cargo JSON runs as namespace PID 1
through the feature-gated fixture and proves the shared production wrapper waits
until every analyzer, supervisor, worker, and Git descendant is reaped and
`waitpid(-1)` reaches `ECHILD`. The script verifies the strict result evidence,
default-release rejection, nondefault target-directory behavior, and absence of
surviving descendants. A skipped or unavailable script is never green.

### Task 6: Expose cleanup diagnostics and operator recovery

**Files:**

- Modify: `crates/loomweave-mcp/src/lib.rs`
- Modify: `crates/loomweave-mcp/src/tools/status.rs`
- Modify: `crates/loomweave-mcp/tests/storage_tools.rs`
- Modify: `crates/loomweave-cli/src/serve.rs`
- Modify: `crates/loomweave-cli/src/serve/runtime.rs`
- Modify: `crates/loomweave-cli/src/worktree_cleanup.rs`
- Modify: `crates/loomweave-cli/src/doctor.rs`
- Modify: `crates/loomweave-cli/tests/doctor.rs`
- Modify: `docs/operator/getting-started.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add failing status and doctor tests**

```text
project_status_reports_last_cleanup_diagnostic
project_status_malformed_gc_state_is_check_due
project_status_reports_disabled_gc_reason
project_status_serializes_enabled_and_disabled_gc_capability_exactly
project_status_reports_recursive_delete_support
project_status_serializes_last_gc_error_exactly
project_status_reports_scheduler_spawn_failure
project_status_prefers_newer_in_memory_scheduler_diagnostic
project_status_preserves_memory_and_reports_durable_read_warning
project_status_serializes_cleanup_diagnostic_and_read_warning_exactly
doctor_reports_operator_managed_quarantine
doctor_reports_legacy_linked_store_without_deleting
doctor_reports_retained_unsafe_tombstone
doctor_reports_incomplete_relocation_without_mutation
doctor_reports_recoverable_relocation_without_mutation
doctor_reports_unknown_liveness_recovery_token_and_command
```

- [ ] **Step 2: Run status/doctor tests and verify RED**

```bash
cargo test -p loomweave-mcp --test storage_tools project_status_reports_
cargo test -p loomweave-mcp --test storage_tools project_status_serializes_
cargo test -p loomweave-cli --test doctor doctor_reports_
```

Expected: current status has no worktree cleanup block and doctor has no
lifecycle diagnostics.

- [ ] **Step 3: Read live GC state fail-soft and document recovery**

Add this status shape:

```json
{
  "worktree_cleanup": {
    "gc_capability": {
      "state": "enabled-owned-default",
      "reason": null
    },
    "last_attempt_at": null,
    "last_success_at": null,
    "last_error": null,
    "last_scheduler_diagnostic": null,
    "scheduler_diagnostic_read_warning": null,
    "check_due": true,
    "recursive_delete_supported": true
  }
}
```

A disabled capability uses the same exact object shape:

```json
{
  "gc_capability": {
    "state": "disabled",
    "reason": "configured-store-override"
  }
}
```

Enabled requires null reason. Disabled requires exactly one closed
`GcDisabledReason` spelling from plan 1; unknown, absent, or contradictory
state/reason pairs are rejected by the serializer tests.

The three nullable fields have these exact additive JSON shapes when present:

```json
{
  "last_error": {
    "code": "git-enumeration-failed",
    "message": "bounded GC error"
  },
  "last_scheduler_diagnostic": {
    "observed_at": "2026-07-18T00:00:00Z",
    "event_id": "0123456789abcdef0123456789abcdef",
    "trigger": "analysis-complete",
    "code": "helper-spawn-failed",
    "message": "bounded diagnostic"
  },
  "scheduler_diagnostic_read_warning": {
    "code": "invalid-durable-diagnostic",
    "message": "bounded read warning"
  }
}
```

`trigger` is the closed `startup | periodic | analysis-complete` enum.
Diagnostic `code` is the closed `scheduler-full | scheduler-closed |
helper-spawn-failed | helper-exit-abnormal | helper-deadline-exceeded` enum.
Read-warning `code` is the
closed `invalid-durable-diagnostic | durable-diagnostic-io` enum. Both message
fields use the same 1,024-byte UTF-8-safe truncation rule defined in Task 5;
the read warning is generated from an internal read/validation error and never
copies unbounded persisted content. The exact-serialization test pins field
names, null behavior, enum spellings, bounds, and rejection of unknown values.

Read `gc-state.json` on each status request rather than caching it at server
construction. Serialize `gc_capability` and owner-dependent diagnostics from
the post-open `RepositoryAuthority`, never `WorktreeContext.gc_preflight`; a
first-ever namespace and a rebound namespace therefore report current
authority without restart. Malformed state reports `check_due=true`; it never
authorizes a mutation. Call the service's cloneable
`CleanupDiagnosticSink::snapshot()`;
inside core it rereads the durable record through the pinned handle, strictly
validates it, and merges it with memory by `(observed_at, event_id)`. Map
`view.latest` to `last_scheduler_diagnostic` and `view.read_warning` to
`scheduler_diagnostic_read_warning`. A durable read failure never hides a valid
in-memory event and never grants deletion authority. Wire both
`Arc<RepositoryAuthority>` and the sink through the additive `ServerState` and
HTTP-status constructors without removing any existing field; no status path
can fall back to context preflight. Doctor
identifies disabled ownership/confinement, legacy local stores,
manual quarantines, retained unsafe tombstones, and every incomplete relocation
journal shape without deleting or repairing it. Even a valid single-side
recoverable journal remains unchanged during doctor; only namespace open or GC
invokes the locked mutating reconciler.
On unsupported process-identity backends, doctor prints the expired-intent facts,
writer-lock state, checksum-derived token, and exact operator-confirmed
`worktree recover-intent` command; it never clears the intent itself.

- [ ] **Step 4: Run GREEN and commit diagnostics**

```bash
cargo test -p loomweave-mcp --test storage_tools project_status_reports_
cargo test -p loomweave-mcp --test storage_tools project_status_serializes_
cargo test -p loomweave-cli --test doctor doctor_reports_
markdownlint-cli2 docs/operator/getting-started.md CHANGELOG.md
git add crates/loomweave-mcp crates/loomweave-cli \
  docs/operator/getting-started.md CHANGELOG.md
git commit -m "feat(status): expose worktree cleanup diagnostics"
test -z "$(git status --porcelain)"
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
cargo nextest run -p loomweave-storage --test run_authority
cargo nextest run -p loomweave-mcp \
  --test index_access \
  --test index_readiness \
  --test analyze_lifecycle \
  --test storage_tools \
  --test catalogue_tools \
  --test federation_classification_golden
cargo nextest run -p loomweave-cli \
  --test dotenv_policy \
  --test worktree_analyze \
  --test worktree_intent_cli \
  --test worktree_serve_bootstrap \
  --test worktree_activity \
  --test worktree_cleanup \
  --test runtime_path_callsite_audit \
  --test serve
cargo test -p loomweave-cli --bin loomweave http_read::readiness
bash scripts/generate-federation-seam-goldens.sh
bash scripts/check-federation-seam-goldens-hermetic.sh
if sudo -n true; then
  bash scripts/run-worktree-no-xdev-test.sh
else
  echo "local NO_XDEV leg unavailable; require green Linux Verify evidence"
fi
```

In `worktree_cleanup.rs`, include the plan-2-deferred real barrier scenario:
serve/analyze one linked worktree while GC tombstones a different eligible
store; assert the live store's activity/graph is unaffected and the eligible
store alone moves. Expected: every focused suite passes.

- [ ] **Step 2: Run repository release gates**

```bash
set -euo pipefail
export RUSTFLAGS="-D warnings"
wardline_golden_url="https://raw.githubusercontent.com/foundryside-dev/"
wardline_golden_url+="wardline/main/tests/conformance/fixtures/"
wardline_golden_url+="wardline-taint-fact-wire.golden.json"
curl --fail --silent --show-error --location \
  --retry 3 --retry-all-errors --retry-delay 2 \
  --connect-timeout 10 --max-time 60 \
  "$wardline_golden_url" \
  --output /tmp/wardline-taint-fact-wire.golden.json
python scripts/check-wardline-taint-golden.py --self-test
python scripts/check-wardline-taint-golden.py \
  --authority-file /tmp/wardline-taint-fact-wire.golden.json
cargo fmt --all -- --check
python scripts/check-migration-retirement.py --self-test
python scripts/check-migration-retirement.py
python scripts/check-workspace-version-lockstep.py
python scripts/check-pyright-pin-lockstep.py --self-test
python scripts/check-pyright-pin-lockstep.py
python scripts/check-wardline-version-bounds.py --self-test
python scripts/check-wardline-version-bounds.py
python scripts/check-entity-cap-lockstep.py --self-test
python scripts/check-entity-cap-lockstep.py
cargo clippy --workspace --all-targets --all-features -- -D warnings
deps="$(cargo tree -p loomweave-core --edges normal --prefix none)"
if grep -qE '^reqwest v' <<<"$deps"; then exit 1; fi
cargo build --workspace --bins
cargo nextest run --workspace --all-features --no-tests=pass
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
python scripts/check-python-ontology-version.py --self-test
python scripts/check-python-ontology-version.py
python -m pip install uv==0.10.2
uv sync --project plugins/python --locked --extra dev
uv export --project plugins/python --locked --extra dev --no-emit-project \
  --format requirements.txt \
  --output-file /tmp/loomweave-python-dev-requirements.txt
uv run --project plugins/python --extra dev pip-audit \
  -r /tmp/loomweave-python-dev-requirements.txt
uv run --project plugins/python --extra dev python \
  scripts/check-b4-gate-result.py --run-b5-smoke
uv run --project plugins/python --extra dev ruff check plugins/python scripts
uv run --project plugins/python --extra dev ruff format --check plugins/python
uv run --project plugins/python --extra dev mypy --strict plugins/python
uv run --project plugins/python --extra dev pytest plugins/python
bash scripts/generate-federation-seam-goldens.sh
bash scripts/check-federation-seam-goldens-hermetic.sh
bash tests/e2e/sprint_1_walking_skeleton.sh
CARGO_BUILD=0 bash tests/e2e/wp5_secret_scan.sh
CARGO_BUILD=0 bash tests/e2e/sprint_2_mcp_surface.sh
CARGO_BUILD=0 bash tests/e2e/phase3_subsystems.sh
wardline scan . --fail-on ERROR
git diff --check
git status --short
```

`.github/workflows/verify.yml` is the single authoritative pre-merge/release
contract; re-read it at execution time in case it changed. The commands above
reproduce its current local-capable Rust, guard, dependency-boundary, Python,
audit, and end-to-end steps plus the project-required Wardline scan. Require the
green `Verify` workflow for its network and native macOS jobs before branch
clearance; record that as external CI evidence rather than implying the local
Linux run covered macOS. Windows lifecycle mutation remains unsupported in v1;
adding it requires a reparse-safe backend plus Windows compile and test jobs.

- [ ] **Step 3: Dogfood a real divergent worktree**

Keep dogfood outside the feature repository. First require its status to be
clean and capture `git worktree list --porcelain`. Drive the run through a
repository-owned process harness that owns every direct `Child`, places the
server in its own process group, keeps its MCP stdin, and records PID plus
PID-reuse-safe process-start identity. Create a temporary Git repository,
commit a base source file, add a linked worktree beneath the same temporary
root, and put a uniquely named uncommitted function only in the linked checkout.
Point `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, and other Loomweave state roots into
that temporary root. Install cleanup before creation; it must run on success,
failure, and interruption.

Using the just-built binary, start `loomweave serve --path <temporary-linked>`;
observe building status, query the unique function on that same MCP session,
and prove an explicit analysis/query of the temporary main checkout does not
contain it. Run `loomweave worktree analyze --json -- <temporary-linked>` as the
manual-command proof and capture the returned cleanup-helper PID and process
start identity. A missing spawn identity or spawn-failure outcome fails dogfood.
This is live first-serve/divergent-graph evidence and does not claim to wait
through lifecycle grace periods.

Before removing any temporary path, close the MCP session/stdin, wait a bounded
interval for the server child, then send TERM to its process group and wait,
followed by KILL and another wait only if needed. Reap every directly owned
child and fail if one remains. For the detached cleanup helper, which is no
longer a child of the harness, use core's `ProcessLiveness` against the captured
identity and wait boundedly until all three conditions hold: its identity is no
longer live, `gc-state.json.last_attempt_at` has advanced or a terminal cleanup
diagnostic exists, and `gc.lock` is acquirable. PID-only checks are forbidden.
The same bounded close/TERM/KILL/wait and helper-quiescence sequence runs from
the failure/interruption cleanup path before it removes state.

Only after server and helper quiescence, force-remove the temporary linked
worktree and temporary root. Disable the cleanup handler, assert no captured
process identity is live, and assert the feature repository is still clean and
its porcelain worktree listing is byte-for-byte equal to the captured baseline.
Also assert the temporary state root and both temporary checkout paths are gone.
A timeout or failed assertion is a task failure, not dogfood residue to commit.

- [ ] **Step 4: Run the accelerated real-filesystem lifecycle harness**

Use an ignored integration harness with an injected clock for removal, first
absence, second absence after 24 hours, tombstone, recovery, and deletion after
another 24 hours. Restore/recreate at every boundary and prove preservation:

```bash
cargo test -p loomweave-cli --test worktree_cleanup -- --ignored real_lifecycle
```

- [ ] **Step 5: Commit only concrete gate fixes, then request final review**

If a gate required changes, commit each narrow fix with its regression test.
Then use `superpowers:requesting-code-review` against the full feature branch.
Do not close `clarion-c297efc752` until the reviewer has no Critical or Important
findings and all gates above remain green. Finish with `git diff --check`,
`test -z "$(git status --porcelain)"`, and a final comparison against the
pre-dogfood worktree-list baseline.
