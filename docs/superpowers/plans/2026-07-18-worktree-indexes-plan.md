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

**Revision:** 2026-07-28, after a four-lens implementation review
([review ledger](2026-07-18-worktree-indexes-plan.review.json), verdict
CHANGES_REQUESTED, 7 blockers — all resolved in this revision). Task order,
module placement, and file lists changed materially; do not execute from an
older copy of this plan.

**Tech stack:** Rust 1.88 workspace, Clap, serde_json, BLAKE3, fs2 advisory
locks, `rustix` **1.1.4** (pin this version — `Cargo.lock` already resolves
1.1.4 transitively and a second/third resolved version trips `deny.toml`'s
`multiple-versions` warning) for Linux `openat2` confined deletion only, in
**`loomweave-cli`** (not core — see File structure), Cargo Nextest.

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

**Rollout notes (document, don't code):**

- A **pre-worktree binary** downgrade plus `loomweave install --force` deletes
  the whole store including `worktrees/` (old `ensure_safe_to_force_remove`
  predates the namespace). Accepted per the ephemeral posture; cost is one
  re-analyze per worktree. Mention in the release notes.
- ADR-046 calls the `weft.toml` `[loomweave].store_dir` override
  *operator-private*; nothing enforces that it stays untracked. A tracked
  override is safe once Task 3 lands (plain analyze routes through
  `WorktreeContext`, and the sweep is report-only under an override) but keep
  the convention stated in operator docs.

---

## Execution preflight

- [ ] **Step 1: Isolated implementation worktree — off the ref that carries
  this plan**

The cut design + this plan live on `fix/week-review-followups` until that
branch merges; `main`'s same-named design file is the **stale pre-cut
version**. Branch from whichever ref actually contains this plan, and verify
you got the right one before doing anything else:

```bash
cd /home/john/loomweave
test -z "$(git status --porcelain)"
if git cat-file -e main:docs/superpowers/plans/2026-07-18-worktree-indexes-plan.md 2>/dev/null; then BASE=main; else BASE=fix/week-review-followups; fi
git worktree add .worktrees/worktree-indexes -b feat/worktree-indexes "$BASE"
cd .worktrees/worktree-indexes
test -f docs/superpowers/plans/2026-07-18-worktree-indexes-plan.md   # hard stop if absent
grep -q "rewritten after owner scope reduction" docs/superpowers/specs/2026-07-18-loomweave-worktree-indexes-design.md   # hard stop if stale design
```

- [ ] **Step 2: Claim the ticket and confirm baseline**

The ticket is assigned to `claude` (stale claim from 2026-07-18); re-claim or
heartbeat under your own actor name rather than assuming it is free.

```bash
filigree start-work clarion-c297efc752 --assignee <name>
cargo nextest run -p loomweave-core
```

## File structure

Module placement follows the `analyze_lock.rs` precedent: identity/path
resolution is shared by CLI and MCP and lives in core; everything that shells
git for lifecycle decisions, deletes directories, or holds process-level locks
lives in the CLI crate (its only callers are CLI entry points, `fs2` is
already a CLI dep, and it keeps `rustix` out of the deliberately minimal core,
which already carries `nix` for the plugin sandbox).

- Create `crates/loomweave-core/src/worktree/mod.rs` — module boundary.
- Create `crates/loomweave-core/src/worktree/context.rs` — resolution, stable
  ID, config origin.
- Create `crates/loomweave-core/src/worktree/paths.rs` — `StorePaths`.
- Create `crates/loomweave-cli/src/worktree/confine.rs` — the confined-delete
  primitive (Task 2; everything that removes a store goes through it).
- Create `crates/loomweave-cli/src/worktree/store.rs` — create/open/delete.
- Create `crates/loomweave-cli/src/worktree/sweep.rs` — cleanup policy.
- Create `crates/loomweave-cli/src/worktree/cmd.rs` — the `worktree`
  subcommand (a `worktree/` dir module and a sibling `worktree.rs` cannot
  coexist; everything lives under `worktree/`).
- Add `rustix = "1.1.4"` (feature `fs`) to `crates/loomweave-cli/Cargo.toml`
  via `[workspace.dependencies]`.
- Add focused integration tests rather than growing existing test modules.

---

### Task 1: Resolve typed worktree context

**Files:** `crates/loomweave-core/src/worktree/{mod,context,paths}.rs`,
`crates/loomweave-core/tests/worktree_context.rs`

- [ ] **RED — write failing tests**

```rust
#[test] fn standalone_project_keeps_current_store_path() {}
#[test] fn main_worktree_keeps_current_store_path() {}
#[test] fn linked_worktree_resolves_under_primary_store() {}
#[test] fn primary_is_identified_by_git_dir_not_branch_name() {}
#[test] fn bare_primary_classifies_as_standalone() {}
#[test] fn symlinked_source_root_resolves_to_same_identity_as_target() {}
#[test] fn stable_id_survives_branch_switch() {}
#[test] fn stable_id_survives_repository_move() {}
#[test] fn distinct_worktrees_get_distinct_stable_ids() {}
#[test] fn non_utf8_worktree_path_is_rejected_with_typed_error() {}
#[test] fn unresolvable_git_context_falls_back_to_local_store() {}
```

Use real `git worktree add` fixtures in a tempdir, not mocks (established
pattern — `doctor.rs`, `sei_git.rs`, `index_diff.rs` tests all do this;
`hardened_git_command` nulls global/system git config so no `safe.directory`
setup is needed).

- [ ] **GREEN — implement**

`WorktreeContext::resolve(source_root)` shells `git rev-parse` and
`git worktree list --porcelain -z` via the existing `hardened_git_command`.
Identify the primary by the entry whose resolved Git directory equals the
common Git directory; a bare primary (no main working tree) classifies as
standalone. Canonicalize the source root before identity comparison. Stable ID
is `wt-` + BLAKE3 hex of the administrative identity bytes. `StorePaths`
exposes explicit `db`, `embeddings`, `instance_id`, `port`, `runs`, `lock`
leaves.

Decoding the `-z` output strictly is **new code**: the existing NUL-delimited
consumer (`list_untracked_files`) uses `String::from_utf8_lossy`, which would
silently *accept* mangled paths — the design requires non-UTF-8
source/primary/common-dir/admin-identity to return a structured
unsupported-path error before any store is created. Do not reuse the lossy
pattern.

**Verify:** `cargo nextest run -p loomweave-core worktree_context`

---

### Task 2: Confined deletion primitive

**This is the safety-critical task** (it moved ahead of store creation so no
interim string-path deletion ever exists — every later task that removes a
store calls this primitive). Everything deleted through it is cheap;
everything *adjacent* is not: `.weft/filigree/` holds the issue tracker and
audit trail, `.weft/wardline/` holds baselines and waivers, and neither is
regenerable.

**Files:** `crates/loomweave-cli/src/worktree/{mod,confine}.rs`,
`crates/loomweave-cli/Cargo.toml`, `Cargo.toml` (workspace dep table),
`crates/loomweave-cli/tests/worktree_confine.rs`,
`.github/workflows/verify.yml`

- [ ] **RED — confinement (must all fail closed)**

```rust
#[test] fn symlinked_worktrees_component_refuses_deletion() {}
#[test] fn symlink_inside_candidate_refuses_deletion() {}
#[test] fn non_matching_directory_name_is_never_deletable() {}
#[test] fn sibling_weft_directories_are_unreachable() {}
#[test] fn deletion_is_rooted_at_pinned_handle_not_resolved_path() {}
#[test] fn unsupported_platform_reports_and_deletes_nothing() {}
#[test] fn deletion_logs_stable_id_and_reason() {}
#[test] #[ignore] fn bind_mount_beneath_candidate_refuses_deletion() {}
```

**The bind-mount test has no existing CI pattern to copy — there is no
privileged test leg in this repository; create the mechanism as part of this
task.** The test enters an unprivileged user + mount namespace
(`unshare(CLONE_NEWUSER | CLONE_NEWNS)` in-process, available to unprivileged
processes on `ubuntu-latest`), bind-mounts a directory beneath a candidate,
and asserts the primitive refuses. It is `#[ignore]`d for ordinary runs; add
an explicit step to `verify.yml`'s Linux rust job:

```yaml
- name: confined-deletion privileged suite
  run: cargo nextest run -p loomweave-cli --run-ignored ignored-only -E 'test(bind_mount)'
```

If namespace creation fails (e.g. a runner without unprivileged userns), the
test must **fail with a diagnostic, not skip** — a silently-skipped
confinement test is the exact failure mode "must all fail closed" forbids.

Non-Linux: the primitive compiles everywhere; off Linux it returns a
structured `unsupported-platform` result and deletes nothing. The
namespace/symlink tests are `#[cfg(target_os = "linux")]`; CI's nextest runs
Linux-only (the `rust-macos` job is clippy + build), so state this gating in
the test file header for local macOS devs.

- [ ] **GREEN**

Deletion uses Linux `openat2` with
`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV` via `rustix` 1.1.4
(a safe fn — no new `unsafe` surface). Traversal is rooted at a pinned
directory handle for `<repository-store>/worktrees/`; every open is
`O_NOFOLLOW`; only direct children matching exactly `wt-[0-9a-f]{64}` are
eligible; string-prefix checks never authorize anything. Every deletion emits
one log line with the stable ID and the reason. No `remove_dir_all` on a
string path anywhere in this module — and, from this task on, anywhere in the
worktree feature.

**Verify:** `cargo nextest run -p loomweave-cli worktree_confine`

---

### Task 3: Isolated stores, explicit analysis, and plain-analyze routing

**Files:** `crates/loomweave-cli/src/worktree/store.rs`,
`crates/loomweave-cli/src/worktree/cmd.rs`, `crates/loomweave-cli/src/main.rs`,
`crates/loomweave-cli/src/analyze.rs`,
`crates/loomweave-cli/tests/worktree_store.rs`,
`crates/loomweave-cli/tests/worktree_analyze.rs`

- [ ] **RED**

```rust
#[test] fn creating_a_store_writes_plain_metadata() {}
#[test] fn unreadable_metadata_deletes_and_rebuilds() {}
#[test] fn source_root_mismatch_deletes_and_rebuilds() {}
#[test] fn removed_and_readded_worktree_name_rebuilds_not_reuses() {}
#[test] fn delete_and_rebuild_logs_the_reason() {}
#[test] fn same_identity_same_root_reuses_the_store() {}
#[test] fn main_store_path_is_untouched_by_worktree_creation() {}
// CLI-level
#[test] fn worktree_analyze_builds_an_index_by_path() {}
#[test] fn worktree_analyze_builds_an_index_by_name() {}
#[test] fn worktree_analyze_no_incremental_rebuilds() {}
#[test] fn plain_analyze_on_linked_worktree_writes_isolated_store() {}
#[test] fn two_worktrees_analyze_concurrently_without_contention() {}
```

`plain_analyze_on_linked_worktree_writes_isolated_store` is the
review-critical case: bare `loomweave analyze <linked-worktree-path>` — the
argv the SessionStart hook spawns — must land in
`<repository-store>/worktrees/<id>/`, and `<worktree>/.weft/loomweave/` must
not be created. Without this routing, the hook's analyze and Task 5's
bootstrap write a 20–30 minute index to a store serve never polls, and the
worktree sticks at `building` forever with every other test green. (The
per-store `analyze_lock` cannot prevent that two-location double-analyze.)

Concurrency test: drive analyze with the fixture plugin
(`loomweave-plugin-fixture`) or a fast JSON-RPC fixture script (both are
established patterns in `crates/loomweave-cli/tests/analyze.rs`) — do not run
a real Python/Pyright pass in tests.

- [ ] **GREEN**

Store creation is `create_dir_all` + write `metadata.json` with `serde_json`,
**plus an empty-initialized `loomweave.db`** (touching the DB is what keeps
`serve`'s existing `db_path().exists()` dispatch from short-circuiting into
no-index mode before Task 5 rewires it — see Task 5). No lock dance, no
checksum, no initialization sentinel. Metadata that fails to parse, or whose
`source_root` no longer matches (compare canonicalized), causes the directory
to be deleted **via the Task 2 primitive** and recreated, logging the reason.

`analyze.rs::run_with_options` resolves its storage through
`WorktreeContext::resolve` **unconditionally** — not only under the new
subcommand — replacing the direct `store_dir(&project_root)` call
(`analyze.rs:357`). Add
`loomweave worktree analyze [--no-incremental] -- <name-or-path>`, wired to
the same path with `StorePaths` supplied explicitly.

Intermediate-state note: until Task 4 lands, a worktree analyze resolves
config to built-in defaults (no `ConfigOrigin` chain yet). Expected and
acceptable mid-migration; not a bug, and not shippable as final behavior.

**Verify:** `cargo nextest run -p loomweave-cli worktree_store worktree_analyze`

---

### Task 4: Route configuration and sibling discovery through context

**Files:** `crates/loomweave-cli/src/config.rs`,
`crates/loomweave-core/src/worktree/context.rs`,
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
Sibling discovery (Filigree `ephemeral.port`, `federation_token` — today
single-root in `loomweave-federation/src/filigree_url.rs:163` and
`filigree.rs:302`) uses one ordered deduplicated lookup list: source root,
then primary root. Loomweave's own sidecars use `StorePaths`.

**Verify:** `cargo nextest run -p loomweave-cli worktree_config`

---

### Task 5: Serve bootstrap

**Files:** `crates/loomweave-cli/src/serve.rs`,
`crates/loomweave-mcp/src/lib.rs`, `crates/loomweave-mcp/src/tools/status.rs`,
`crates/loomweave-core/src/errors.rs`, `.config/nextest.toml` (only if
needed — see below), `crates/loomweave-mcp/tests/worktree_bootstrap.rs`

- [ ] **RED**

```rust
#[test] fn serve_on_unbuilt_worktree_starts_and_reports_building() {}
#[test] fn serve_gate_routes_linked_worktree_to_building_not_no_index() {}
#[test] fn graph_tool_returns_retryable_index_building_envelope() {}
#[test] fn session_becomes_usable_after_run_completes_without_reconnect() {}
#[test] fn second_serve_does_not_spawn_a_duplicate_analyze() {}
#[test] fn failed_build_returns_index_build_failed_with_fallback_command() {}
#[test] fn status_counts_are_null_not_zero_while_building() {}
#[test] fn removed_source_root_surfaces_source_root_missing() {}
#[test] fn main_and_standalone_keep_existing_no_index_behavior() {}
```

- [ ] **GREEN — four pinned decisions; do not improvise alternatives**

1. **The dispatch gate is owned here.** `serve.rs::run()`'s top-of-function
   `db_path(path).exists()` fork (`serve.rs:24-30`) currently routes any
   missing DB to the fully degraded `serve_no_index` loop. Resolve
   `WorktreeContext` *before* that gate: linked worktrees go to the full
   `ServerState` path with readiness `building` (Task 3's eager empty DB means
   the path exists; the readiness enum, not file existence, now carries the
   state). Main/standalone keep today's behavior exactly.
2. **Readiness is recomputed per call at the one chokepoint.** A small enum —
   `building | ready | build_failed` — evaluated by consulting the runs state
   (completed-run row) inside `handle_tool_call`
   (`crates/loomweave-mcp/src/lib.rs:1625`), the single dispatch funnel for
   every graph tool. No background timer, no cached readiness flag, no
   per-tool checks forked across `graph.rs`/`orientation.rs`/`catalogue/*`.
   That is what makes `session_becomes_usable_after_run_completes_without_reconnect`
   deterministic: complete the run, the next tool call sees it.
3. **The spawn is the new subcommand with the hook's detachment technique.**
   Detached child via `process_group(0)` + null stdio (as
   `hook.rs::spawn_detached_analyze` does), but the argv is
   `loomweave worktree analyze <path>` — do **not** call the hook's function
   or its plain-`analyze` argv. Double-spawn is guarded by the existing
   per-store `analyze_lock.rs` fs2 lock. No durable-intent protocol, no
   generation-versioned readiness type, no run authority probe.
4. **Errors ride the existing envelope and the pinned code enum.** The wire
   shape is `tool_error_envelope_with_diagnostics`
   (`loomweave-mcp/src/lib.rs:3851`): `error.code` / `error.message` /
   `error.retryable` plus top-level `diagnostics` — **not** a bespoke
   `{code, retryable, details}` object. Add `index-building`,
   `index-build-failed`, and `source-root-missing` to `McpErrorCode`
   (`crates/loomweave-core/src/errors.rs:89`) and extend the
   `mcp_error_code_wire_strings_are_pinned` test. Diagnostics carry the
   `run_id` and the exact `loomweave worktree analyze` fallback argv.

`source-root-missing`: when the resolved source root no longer exists (the
worktree was removed under a live serve — the design's accepted race), graph
and status tools return that code instead of the last staleness verdict.
Detection is a cheap existence check during the same per-call readiness
consult; no watcher, no lock.

Test-infra note: drive these tests over in-process stdio (no TCP). If any
test genuinely needs the HTTP surface, put it in a new serial nextest group
(`.config/nextest.toml`, `max-threads = 1`, patterned on `serve-http` — whose
filter covers only loomweave-cli's `serve` binary and will not protect a new
loomweave-mcp test binary).

**Verify:** `cargo nextest run -p loomweave-mcp worktree_bootstrap`

---

### Task 6: Cleanup sweep

**Files:** `crates/loomweave-cli/src/worktree/sweep.rs`,
`crates/loomweave-cli/src/serve.rs`, `crates/loomweave-cli/src/analyze.rs`,
`crates/loomweave-cli/tests/worktree_sweep.rs`

- [ ] **RED**

```rust
#[test] fn unregistered_store_is_deleted() {}
#[test] fn registered_store_is_preserved() {}
#[test] fn store_created_for_a_just_registered_worktree_is_preserved() {}
#[test] fn git_locked_worktree_is_preserved() {}
#[test] fn main_store_is_never_a_candidate() {}
#[test] fn git_enumeration_failure_deletes_nothing() {}
#[test] fn admin_dir_enumeration_failure_deletes_nothing() {}
#[test] fn override_store_dir_makes_sweep_report_only() {}
#[test] fn held_gc_lock_skips_the_sweep() {}
#[test] fn sweep_failure_does_not_fail_serve_or_analyze() {}
```

`store_created_for_a_just_registered_worktree_is_preserved` pins the ordering
invariant that today holds only by construction (`git worktree add` registers
the admin entry before any analyze can target it, and the store name is
deterministic from that identity) — a refactor that creates store directories
before registration would otherwise silently turn bootstrap into a
sweep-then-rebuild loop.

`override_store_dir_makes_sweep_report_only` is the review-critical case: an
absolute `[loomweave].store_dir` override is not scoped to this repository
(`store.rs:100`, and the existing test at `store.rs:242` proves
`/var/lib`-style paths resolve). Two repositories sharing an override resolve
to the *same* `<repository-store>`, so repo A's registered-worktree set must
never authorize deleting repo B's `wt-*` stores — under an active override the
sweep logs candidates and deletes nothing. This is the design's Non-goals
requirement made executable; note that the namespace-confinement guarantee
alone cannot catch it, because the cross-repo deletion happens *inside* the
shared namespace.

- [ ] **GREEN**

Under non-blocking `gc.lock`: enumerate registered worktrees via hardened Git;
enumerate direct children of the **pinned** `worktrees/` handle; delete —
through the Task 2 primitive only — any `wt-[0-9a-f]{64}` directory whose ID
is not registered. If `store_dir_override()` is active, run the enumeration
and log what *would* be deleted, then delete nothing.

**Deriving each registered worktree's stable ID.** `git worktree list
--porcelain` does *not* expose an entry's administrative directory, and the
stable ID is a hash of exactly that. Enumerate the common Git directory's own
`worktrees/` admin entries directly and hash each name — do not probe each
present working tree with `rev-parse --absolute-git-dir`. Reading the admin
directory is one cheap readdir, needs no subprocess per entry, and correctly
covers a registered-but-prunable worktree whose working path is temporarily
gone (an unmounted volume must not read as "unregistered" and trigger
deletion). If the admin directory cannot be enumerated, abort the sweep.

**Known accepted race — stated precisely.** There are no activity locks, so a
store can be swept while a `serve` process holds it open. Two sub-cases:
*(a)* the worktree still exists (e.g. identity re-hash after a move): the open
inode keeps the running server working and the next analyze rebuilds — cost is
one re-analyze. *(b)* `git worktree remove` happened: source tree and
registration are gone together, nothing can rebuild that store, and the live
serve surfaces `source-root-missing` (Task 5) instead of stale answers. This
is a deliberate simplification over the original design's
activity/intent/writer lock ordering; do not reintroduce those locks to close
it without re-reading the design's *"What this object is"*.

Call the sweep on `serve` startup and after each analysis — from every
context, main checkout included (that is what reclaims a store when the
removed worktree itself never runs Loomweave again). Synchronous, in-process —
no helper subprocess.

**Verify:** `cargo nextest run -p loomweave-cli worktree_sweep`

---

### Task 7: Migrate remaining root-derived path consumers

**Files:** `crates/loomweave-cli/src/{db,guidance,doctor,hook,install,instance}.rs`,
`crates/loomweave-cli/src/secret_scan/baseline.rs`,
`crates/loomweave-mcp/src/{snapshot,index_diff}.rs`,
`crates/loomweave-mcp/src/catalogue/semantic.rs`, federation port helpers

- [ ] **RED**

```rust
#[test] fn every_runtime_leaf_resolves_from_store_paths() {}
#[test] fn install_force_refuses_to_remove_a_populated_worktrees_namespace() {}
#[test] fn doctor_reports_worktree_stores() {}
```

- [ ] **GREEN**

Replace production `store_dir()` / `db_path()` / `embeddings_db_path()` calls
on linked-worktree paths with `StorePaths` or explicit leaves. The review's
grep audit found consumers the original list missed — this list is the
verified floor, not a suggestion:

- `crates/loomweave-cli/src/instance.rs:45` (instance_id);
- `crates/loomweave-cli/src/secret_scan/baseline.rs:16` (secrets baseline —
  named in the design text, previously in no task);
- `crates/loomweave-mcp/src/catalogue/semantic.rs:101` (embeddings sidecar).

Re-run the audit and **adjudicate each of these explicitly** (fix here, or
record why Tasks 4–5 already covered them):
`crates/loomweave-mcp/src/tools/status.rs:215`,
`crates/loomweave-mcp/src/lib.rs:2110`, `crates/loomweave-mcp/src/lib.rs:3422`.
`snapshot.rs` / `index_diff.rs` already take `project_root` parameters — their
migration is the `lib.rs` caller wiring, not signature surgery. Tests and
fixtures may keep the low-level helpers.

`crates/loomweave-cli/src/install.rs:338` currently `remove_dir_all`s the
store directory under `--force`. Make it refuse when `worktrees/` is
populated, with a diagnostic pointing at `loomweave worktree analyze`.

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

Then dogfood on this repository — literal sequence, including the second
action that actually triggers the sweep (it only runs on serve startup or
after an analyze):

```bash
cd /home/john/loomweave
find .weft/filigree .weft/wardline -type f | sort > /tmp/weft-siblings-before.txt
git worktree add .worktrees/dogfood -b dogfood/worktree-index
target/release/loomweave worktree analyze -- .worktrees/dogfood
ls .weft/loomweave/worktrees/                  # exactly one wt-<64hex> store
test ! -d .worktrees/dogfood/.weft/loomweave   # nothing written inside the worktree
# graph reflects the worktree HEAD: serve from it, then project_status_get via
# the stdio JSON-RPC probe (/tmp/lwq.py pattern) and compare analyzed commit
git worktree remove .worktrees/dogfood && git branch -D dogfood/worktree-index
target/release/loomweave analyze .             # any analyze/serve triggers the sweep
ls .weft/loomweave/worktrees/                  # store reclaimed
find .weft/filigree .weft/wardline -type f | sort > /tmp/weft-siblings-after.txt
diff /tmp/weft-siblings-before.txt /tmp/weft-siblings-after.txt   # empty: siblings untouched
```
