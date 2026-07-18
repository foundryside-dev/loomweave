# Worktree Indexes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use
> `superpowers:subagent-driven-development` or `superpowers:executing-plans` to
> implement task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Let Loomweave serve and analyze from linked Git worktrees, using an
isolated store per worktree under the primary checkout, built on demand and
reclaimed when the worktree goes away.

**Design:**
[`2026-07-18-loomweave-worktree-indexes-design.md`](../specs/2026-07-18-loomweave-worktree-indexes-design.md)

**Tracker:** `clarion-c297efc752`

**Tech stack:** Rust 1.88 workspace, Clap, serde_json, BLAKE3, fs2 advisory
locks, `rustix` (Linux `openat2` for confined deletion only), Cargo Nextest.

**Scope note.** This plan replaces the earlier three-part
`2026-07-18-worktree-indexes-{1,2,3}-*` plans, which were cut by owner
directive for treating an ephemeral 20-minute cache as durable state. Read the
design's *"What this object is"* section before making any protection decision
here — if you find yourself adding a checksum, a journal, or a grace period,
stop and re-read it.

**Out of scope, split to other tickets:**

- Git environment sanitization and bounded subprocess I/O →
  `clarion-9202f4acec`. Use the existing `hardened_git_command` as-is here.
- Corrupt-index detection on open → `clarion-8becd5f189`.

---

## Execution preflight

- [ ] **Step 1: Isolated implementation worktree**

```bash
cd /home/john/loomweave
test -z "$(git status --porcelain)"
git worktree add .worktrees/worktree-indexes -b feat/worktree-indexes main
cd .worktrees/worktree-indexes
```

- [ ] **Step 2: Claim the ticket and confirm baseline**

The ticket is currently assigned to `codex`; reassign or release before
starting.

```bash
filigree start-work clarion-c297efc752 --assignee <name>
cargo nextest run -p loomweave-core
```

## File structure

- Create `crates/loomweave-core/src/worktree/mod.rs` — module boundary.
- Create `crates/loomweave-core/src/worktree/context.rs` — resolution, stable
  ID, config origin.
- Create `crates/loomweave-core/src/worktree/paths.rs` — `StorePaths`.
- Create `crates/loomweave-core/src/worktree/store.rs` — create/open/delete.
- Create `crates/loomweave-core/src/worktree/sweep.rs` — confined cleanup.
- Create `crates/loomweave-cli/src/worktree.rs` — the `worktree` subcommand.
- Add focused integration tests rather than growing existing test modules.

---

### Task 1: Resolve typed worktree context

**Files:** `worktree/mod.rs`, `worktree/context.rs`, `worktree/paths.rs`,
`crates/loomweave-core/tests/worktree_context.rs`

- [ ] **RED — write failing tests**

```rust
#[test] fn standalone_project_keeps_current_store_path() {}
#[test] fn main_worktree_keeps_current_store_path() {}
#[test] fn linked_worktree_resolves_under_primary_store() {}
#[test] fn primary_is_identified_by_git_dir_not_branch_name() {}
#[test] fn stable_id_survives_branch_switch() {}
#[test] fn stable_id_survives_repository_move() {}
#[test] fn distinct_worktrees_get_distinct_stable_ids() {}
#[test] fn non_utf8_worktree_path_is_rejected_with_typed_error() {}
#[test] fn unresolvable_git_context_falls_back_to_local_store() {}
```

Use real `git worktree add` fixtures in a tempdir, not mocks.

- [ ] **GREEN — implement**

`WorktreeContext::resolve(source_root)` shells `git rev-parse` and
`git worktree list --porcelain -z` via the existing `hardened_git_command`.
Identify the primary by the entry whose resolved Git directory equals the
common Git directory. Stable ID is `wt-` + BLAKE3 hex of the administrative
identity bytes. `StorePaths` exposes explicit `db`, `embeddings`,
`instance_id`, `port`, `runs`, `lock` leaves.

Non-UTF-8 source/primary/common-dir/admin-identity returns a structured
unsupported-path error before any store is created.

**Verify:** `cargo nextest run -p loomweave-core worktree_context`

---

### Task 2: Create isolated stores and explicit analysis

**Files:** `worktree/store.rs`, `crates/loomweave-cli/src/worktree.rs`,
`crates/loomweave-cli/src/main.rs`, `crates/loomweave-cli/src/analyze.rs`,
`crates/loomweave-core/tests/worktree_store.rs`,
`crates/loomweave-cli/tests/worktree_analyze.rs`

- [ ] **RED**

```rust
#[test] fn creating_a_store_writes_plain_metadata() {}
#[test] fn unreadable_metadata_deletes_and_rebuilds() {}
#[test] fn source_root_mismatch_deletes_and_rebuilds() {}
#[test] fn same_identity_same_root_reuses_the_store() {}
#[test] fn main_store_path_is_untouched_by_worktree_creation() {}
// CLI-level, in the cli crate
#[test] fn worktree_analyze_builds_an_index_by_path() {}
#[test] fn worktree_analyze_builds_an_index_by_name() {}
#[test] fn worktree_analyze_no_incremental_rebuilds() {}
#[test] fn two_worktrees_analyze_concurrently_without_contention() {}
```

- [ ] **GREEN**

Store creation is `create_dir_all` + write `metadata.json` with `serde_json`.
No lock dance, no checksum, no initialization sentinel. Metadata that fails to
parse, or whose `source_root` no longer matches, causes the directory to be
deleted (confined — see Task 5) and recreated.

Add `loomweave worktree analyze [--no-incremental] -- <name-or-path>`, wired to
the existing analyze path with `StorePaths` supplied explicitly.

**Verify:** `cargo nextest run -p loomweave-core -p loomweave-cli worktree`

---

### Task 3: Route configuration and sibling discovery through context

**Files:** `crates/loomweave-cli/src/config.rs`, `worktree/context.rs`,
`crates/loomweave-mcp/src/lib.rs`,
`crates/loomweave-cli/tests/worktree_config.rs`

- [ ] **RED**

```rust
#[test] fn source_root_config_wins_over_primary() {}
#[test] fn primary_config_is_used_when_source_has_none() {}
#[test] fn explicit_config_flag_wins_over_both() {}
#[test] fn llm_config_set_writes_the_resolved_origin() {}
#[test] fn semantic_config_set_writes_the_resolved_origin() {}
#[test] fn setter_creates_primary_target_when_no_file_existed() {}
#[test] fn sibling_port_lookup_falls_back_source_then_primary() {}
#[test] fn own_port_and_instance_id_use_effective_store_leaves() {}
```

- [ ] **GREEN**

Thread `ConfigOrigin` through `ServerState` and the CLI config commands.
Sibling discovery (Filigree `ephemeral.port`, `federation_token`) uses one
ordered deduplicated lookup list. Loomweave's own sidecars use `StorePaths`.

**Verify:** `cargo nextest run -p loomweave-cli worktree_config`

---

### Task 4: Serve bootstrap

**Files:** `crates/loomweave-cli/src/serve.rs`,
`crates/loomweave-mcp/src/tools/status.rs`,
`crates/loomweave-mcp/src/lib.rs`,
`crates/loomweave-mcp/tests/worktree_bootstrap.rs`

- [ ] **RED**

```rust
#[test] fn serve_on_unbuilt_worktree_starts_and_reports_building() {}
#[test] fn graph_tool_returns_retryable_index_building_envelope() {}
#[test] fn session_becomes_usable_after_run_completes_without_reconnect() {}
#[test] fn second_serve_does_not_spawn_a_duplicate_analyze() {}
#[test] fn failed_build_returns_index_build_failed_with_fallback_command() {}
#[test] fn status_counts_are_null_not_zero_while_building() {}
#[test] fn main_and_standalone_keep_existing_no_index_behavior() {}
```

- [ ] **GREEN**

On a linked worktree with no completed run, spawn a **detached**
`loomweave analyze` — the same mechanism the SessionStart hook uses. Guard
double-spawn with the existing `analyze_lock.rs` fs2 lock. Do not build a
durable-intent protocol, a generation-versioned readiness type, or a run
authority probe.

Readiness is a small enum: `building | ready | build-failed`. Graph tools
consult it and return the structured envelope:

```json
{ "code": "index-building", "retryable": true,
  "details": { "run_id": "...", "fallback_argv": [...] } }
```

**Verify:** `cargo nextest run -p loomweave-mcp worktree_bootstrap`

---

### Task 5: Confined cleanup sweep

**This is the safety-critical task.** Everything deleted here is cheap;
everything *adjacent* to it is not. `.weft/filigree/` holds the issue tracker
and audit trail, `.weft/wardline/` holds baselines and waivers, and neither is
regenerable. Confinement is the whole point of this task.

**Files:** `worktree/sweep.rs`, `crates/loomweave-cli/src/serve.rs`,
`crates/loomweave-cli/src/analyze.rs`, `crates/loomweave-core/Cargo.toml`,
`Cargo.toml`, `crates/loomweave-core/tests/worktree_sweep.rs`

- [ ] **RED — behavior**

```rust
#[test] fn unregistered_store_is_deleted() {}
#[test] fn registered_store_is_preserved() {}
#[test] fn git_locked_worktree_is_preserved() {}
#[test] fn main_store_is_never_a_candidate() {}
#[test] fn git_enumeration_failure_deletes_nothing() {}
#[test] fn held_gc_lock_skips_the_sweep() {}
#[test] fn sweep_failure_does_not_fail_serve_or_analyze() {}
```

- [ ] **RED — confinement (must all fail closed)**

```rust
#[test] fn symlinked_worktrees_component_refuses_sweep() {}
#[test] fn symlink_inside_candidate_refuses_deletion() {}
#[test] fn bind_mount_beneath_candidate_refuses_deletion() {}
#[test] fn non_matching_directory_name_is_never_a_candidate() {}
#[test] fn sibling_weft_directories_are_unreachable_from_sweep() {}
#[test] fn deletion_is_rooted_at_pinned_handle_not_resolved_path() {}
#[test] fn unsupported_platform_reports_and_deletes_nothing() {}
```

The bind-mount test needs privileges; gate it behind the same CI leg pattern
used for other privileged tests and make it observably skip, not silently pass.

- [ ] **GREEN**

Under non-blocking `gc.lock`: enumerate registered worktrees via hardened Git;
enumerate direct children of the **pinned** `worktrees/` handle; delete any
`wt-[0-9a-f]{64}` directory whose ID is not registered.

**Deriving each registered worktree's stable ID.** `git worktree list
--porcelain` does *not* expose an entry's administrative directory, and the
stable ID is a hash of exactly that. Enumerate the common Git directory's own
`worktrees/` admin entries directly and hash each name — do not probe each
present working tree with `rev-parse --absolute-git-dir`. Reading the admin
directory is one cheap readdir, needs no subprocess per entry, and correctly
covers a registered-but-prunable worktree whose working path is temporarily
gone (an unmounted volume must not read as "unregistered" and trigger
deletion). If the admin directory cannot be enumerated, abort the sweep.

**Known accepted race.** There are no activity locks in this design, so a
worktree removed while a `serve` process holds its store open can have that
store swept underneath it. On Linux the open inode survives the unlink, so the
running server keeps working and the next start rebuilds — cost is one
re-analyze. This is a deliberate simplification over the original design's
activity/intent/writer lock ordering; do not reintroduce those locks to close
it without re-reading the design's *"What this object is"*.

Deletion uses Linux `openat2` with
`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV` via `rustix`. Other
platforms report unsupported and delete nothing. No `remove_dir_all` on a
string path anywhere in this module.

Call the sweep on `serve` startup and after each analysis. Synchronous,
in-process — no helper subprocess.

**Verify:** `cargo nextest run -p loomweave-core worktree_sweep`

---

### Task 6: Migrate remaining root-derived path consumers

**Files:** `crates/loomweave-cli/src/{db,guidance,doctor,hook,install}.rs`,
`crates/loomweave-mcp/src/{snapshot,index_diff}.rs`, federation port helpers

- [ ] **RED**

```rust
#[test] fn every_runtime_leaf_resolves_from_store_paths() {}
#[test] fn install_force_refuses_to_remove_a_populated_worktrees_namespace() {}
#[test] fn doctor_reports_worktree_stores() {}
```

- [ ] **GREEN**

Replace production `store_dir()` / `db_path()` calls on linked-worktree paths
with `StorePaths` or explicit leaves. Tests and fixtures may keep the low-level
helpers.

`crates/loomweave-cli/src/install.rs:338` currently `remove_dir_all`s the store
directory under `--force`. Make it refuse when `worktrees/` is populated, with
a diagnostic pointing at `loomweave worktree analyze`.

A grep-based audit is sufficient here — the earlier plan's `syn` AST gate was
scoped to a much larger refactor and is not warranted.

**Verify:** `cargo nextest run --workspace`

---

## Final verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

Then dogfood on this repository: create a worktree, `serve` from it, confirm
the graph reflects that worktree's HEAD, remove the worktree, confirm the store
is reclaimed on the next sweep and that `.weft/filigree/` and `.weft/wardline/`
are untouched.
