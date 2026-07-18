# Worktree Indexes Part 2: Bootstrap and Readiness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Automatically build a missing linked-worktree index on first `serve`
while MCP and HTTP remain connected and report authoritative readiness.

**Architecture:** Every linked-worktree analysis entry point reserves one
durable intent before opening SQLite. A shared, generation-safe `IndexAccess`
publishes a runtime-neutral `ActiveStorage` bundle only after a matching
completed run is authoritative; refreshes retain the prior bundle.
MCP tools/resources and HTTP routes consult that same state, while the owning
server supervises spawn, cancellation, activation, and retry.

**Tech Stack:** Rust 1.88, Tokio, fs2 locks, Serde/JSON, SQLite reader and writer
actors, MCP JSON-RPC, Axum HTTP, process groups, Cargo Nextest.

**Design:**
[`2026-07-18-loomweave-worktree-indexes-design.md`](../specs/2026-07-18-loomweave-worktree-indexes-design.md)

**Prerequisite:** Complete
`2026-07-18-worktree-indexes-1-foundation.md` on the same
`feat/worktree-indexes` branch.

---

## Execution preflight

- [ ] **Step 1: Verify the foundation branch and tests**

```bash
cd /home/john/loomweave/.worktrees/worktree-indexes
test "$(git branch --show-current)" = "feat/worktree-indexes"
test -z "$(git status --porcelain)"
cargo test -p loomweave-core --test worktree_context --test worktree_store
cargo test -p loomweave-cli --test worktree_analyze
```

Expected: the foundation suites pass from a clean feature branch.

## File structure

- Create `crates/loomweave-core/src/worktree/analysis_intent.rs` for durable
  reservation, activation, liveness, reconciliation, and terminal state.
- Create `crates/loomweave-core/src/worktree/process_identity.rs` for
  PID-reuse-safe liveness with fail-closed unsupported backends.
- Create `crates/loomweave-storage/src/active.rs` for the protocol-neutral
  `ActiveStorage` handles shared by MCP and HTTP.
- Create `crates/loomweave-storage/src/run_authority.rs` for the sole
  pre-activation, read-only/no-create matching-run probe.
- Create `crates/loomweave-mcp/src/index_access.rs` for the readiness state
  machine and atomically published service bundle.
- Create `crates/loomweave-mcp/src/readiness.rs` for shared structured errors.
- Move MCP configuration tools from the large `lib.rs` into
  `crates/loomweave-mcp/src/tools/config.rs`.
- Split CLI serving orchestration into `serve/bootstrap.rs` and
  `serve/activation.rs`; keep `serve.rs` as the top-level coordinator.
- Create `serve/runtime.rs` for top-level activity, actor, runtime, optional
  lifecycle-trigger, and shutdown ownership; plan 3 installs the scheduler
  behind that trigger.
- Create `crates/loomweave-cli/src/http_read/readiness.rs` for Axum readiness
  middleware and HTTP error conversion.
- Put new integration scenarios in dedicated test files rather than enlarging
  `serve.rs`, `http_read.rs`, or MCP `lib.rs`.

### Task 1: Coordinate every analysis through one durable intent

**Files:**

- Create: `crates/loomweave-core/src/worktree/analysis_intent.rs`
- Create: `crates/loomweave-core/src/worktree/process_identity.rs`
- Modify: `crates/loomweave-core/src/worktree/mod.rs`
- Modify: `crates/loomweave-core/src/worktree/locks.rs`
- Modify: `crates/loomweave-core/src/errors.rs`
- Modify: `crates/loomweave-cli/src/cli.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/analyze_lock.rs`
- Modify: `crates/loomweave-cli/src/run_lifecycle.rs`
- Create: `crates/loomweave-core/tests/analysis_intent.rs`
- Create: `crates/loomweave-cli/tests/worktree_intent_cli.rs`

- [ ] **Step 1: Add failing reservation, reclaim, and cancel-race tests**

Use deterministic clock, process-liveness, progress-heartbeat, and barrier
fakes. Add:

```text
reserve_is_atomic_and_second_launcher_attaches_same_run
activate_requires_matching_nonce_and_holds_writer_lock
stale_intent_is_not_reclaimed_while_process_is_live
stale_intent_is_not_reclaimed_while_heartbeat_is_fresh
stale_intent_reclaims_only_when_all_four_checks_agree
spawn_failure_clears_only_matching_pending_nonce
spawn_failure_cleanup_requires_shared_activity_guard
graceful_terminal_reconciles_matching_run_row
cancel_before_runs_row_terminalizes_intent
terminal_run_row_wins_cancel_race
linux_process_identity_detects_live_dead_and_pid_reuse
process_disappearing_or_becoming_inaccessible_fails_closed
unsupported_process_identity_backend_never_reclaims
committed_row_before_intent_finish_reconciles_same_nonce
intent_cannot_be_replaced_while_authority_is_probed
failed_row_before_cancel_preserves_failed_terminal
failed_row_before_intent_finish_terminalizes_failed
busy_probe_retries_without_transition
corrupt_probe_fails_closed_without_reclaim
intent_diagnostic_1024_bytes_is_preserved
intent_diagnostic_1025_bytes_is_utf8_safely_truncated
intent_reader_rejects_oversize_diagnostic
```

Put command-policy cases in the CLI integration target:

```text
manual_second_launcher_returns_exit_75_without_waiting
hidden_child_nonce_parses_without_loading_dotenv
main_and_standalone_keep_existing_lock_and_no_index_behavior
```

- [ ] **Step 2: Run the tests and verify RED**

```bash
cargo test -p loomweave-core --test analysis_intent
cargo test -p loomweave-cli --test worktree_intent_cli
```

Expected: compilation fails because the coordinator and intent records are
absent.

- [ ] **Step 3: Implement the coordinator and explicit ownership types**

Add this surface:

```rust
pub struct AnalysisIntentCoordinator {
    paths: StorePaths,
    clock: std::sync::Arc<dyn IntentClock>,
    liveness: std::sync::Arc<dyn ProcessLiveness>,
}

pub enum Reservation {
    Owned(IntentOwner),
    Attached(AnalysisIntentSnapshot),
}

#[derive(Clone, Debug)]
pub struct IntentOwner {
    pub run_id: String,
    pub nonce: String,
    pub launcher: ProcessIdentity,
}

#[derive(Clone, Debug)]
pub struct IntentDiagnostic(String);

impl IntentDiagnostic {
    pub fn new(value: &str) -> Self;
    pub fn as_str(&self) -> &str;
}

pub enum IntentTerminal {
    Completed,
    Failed { diagnostic: IntentDiagnostic },
    Cancelled,
}

pub trait RunAuthority: Send + Sync {
    fn probe(&self, database: &std::path::Path, run_id: &str)
        -> Result<RunAuthorityOutcome, IntentError>;
}

pub enum ReconcileCause<'a> {
    AttachedObserver,
    OwnedChildReaped(&'a IntentOwner),
    OperatorRecovery { run_id: &'a str, confirmation: &'a str },
}

pub enum RunAuthorityOutcome {
    Completed { completed_at: time::OffsetDateTime },
    TerminalFailure { kind: RunTerminalKind, diagnostic: IntentDiagnostic },
    NonTerminal { kind: RunNonTerminalKind },
    Missing,
    TransientBusy,
    InvalidSchemaOrCorrupt { diagnostic: IntentDiagnostic },
}

pub enum RunTerminalKind { Failed, Cancelled, SkippedNoPlugins }
pub enum RunNonTerminalKind { Running }

impl AnalysisIntentCoordinator {
    pub fn reserve(
        &self,
        activity: &SharedActivityGuard,
        requested_run_id: Option<&str>,
        launcher: ProcessIdentity,
    ) -> Result<Reservation, IntentError>;

    pub fn activate(
        &self,
        activity: SharedActivityGuard,
        owner: &IntentOwner,
    ) -> Result<ActiveAnalysisLease, IntentError>;

    pub fn finish(
        &self,
        lease: ActiveAnalysisLease,
        terminal: IntentTerminal,
    ) -> Result<(), IntentError>;

    pub fn clear_pending_after_spawn_failure(
        &self,
        activity: &SharedActivityGuard,
        owner: &IntentOwner,
    ) -> Result<bool, IntentError>;

    pub fn cancel_after_reap(
        &self,
        activity: &SharedActivityGuard,
        owner: &IntentOwner,
        authority: &dyn RunAuthority,
    ) -> Result<IntentTerminal, IntentError>;

    pub fn reconcile_or_reclaim(
        &self,
        activity: &SharedActivityGuard,
        cause: ReconcileCause<'_>,
        authority: &dyn RunAuthority,
    ) -> Result<ReconcileOutcome, IntentError>;
    pub fn snapshot(&self) -> Result<Option<AnalysisIntentSnapshot>, IntentError>;
}
```

Reservation holds shared activity, then intent, then writer during activation.
Stale reclaim requires expired lease, no heartbeat, dead process-start identity,
and an acquirable writer lock. Reconciliation retains the matching intent guard,
then non-blockingly acquires the writer guard, revalidates nonce/run, calls the
injected authority probe while both guards remain held, and conditionally
terminalizes that same intent. A fake `RunAuthority` keeps core tests
dependency-free; storage supplies the production implementation in Task 2.
`ActiveAnalysisLease` owns the shared activity guard, writer guard, and matching
owner identity. `finish` consumes it, retains activity, releases writer, then
takes intent and terminalizes only that owner/nonce. This makes the stated lock
order enforceable rather than relying on the caller to drop an unrelated guard.
`clear_pending_after_spawn_failure` likewise requires the reservation's shared
activity guard and takes intent only while that guard is live; no error path
may reacquire intent from an owner token alone.

Map the typed authority result without collapsing semantic failure into
absence. `Completed` preserves completion and `TerminalFailure` preserves the
recorded failed terminal plus its bounded diagnostic. `Missing` and
`NonTerminal` follow the cause-specific reconciliation rules. `TransientBusy`
leaves the intent unchanged and schedules a bounded retry.
`InvalidSchemaOrCorrupt` fails closed and never permits reclaim. The fourth
manual-only recovery case may mark an otherwise unchanged expired record
`abandoned` only for `ProcessLiveness::Unknown`, with an exact confirmation
token, a free writer lock, stale heartbeat, and a non-authoritative `Missing`
or `NonTerminal` authority result; `Live`, fresh heartbeat, or any authoritative
row refuses recovery.

Implement production liveness in `process_identity.rs`. Linux records PID,
`/proc/<pid>/stat` start time, and boot ID. A reused PID is dead relative to the
recorded identity. Unsupported targets, permission/inaccessibility, malformed
process data, and a process disappearing mid-probe return `Unknown` and block
reclaim with a diagnostic. Do not silently degrade to PID-only checks. Add
cfg-level tests for the unsupported backend; native macOS build/Clippy is a
final CI requirement, not something to claim from a Linux run.

Serialize `analysis-intent.json` through the foundation's canonical
`Checksummed<T>` envelope with RFC 3339 time adapters; its payload omits the
checksum and all readers reject corruption, unknown/duplicate/missing fields,
unknown schema, wrong-width nonce, or non-canonical semantic values.
`IntentDiagnostic` is the only constructor path for a persisted failure or
pre-row diagnostic. Writers preserve exactly 1,024 UTF-8 bytes; for longer
input they retain the largest valid UTF-8 prefix of at most 1,012 bytes and
append the exact 12-byte suffix " [truncated]". Readers reject persisted
diagnostics over 1,024 bytes rather than normalizing them. The same type and
bound apply to authority-probe diagnostics before any value is copied into the
intent, readiness details, HTTP, or MCP output.

- [ ] **Step 4: Route direct analysis and hidden child activation through it**

Add hidden `--analysis-intent-nonce <NONCE>`. Only a proven linked context uses
this coordinator. Main/standalone keep the existing analyze lock and degraded
no-index serve behavior. A normal linked direct invocation reserves and
activates in-process. A serve-spawned child receives run ID plus nonce,
validates the pending record, and only then opens SQLite. A second manual
invocation prints `analyze-already-running`, includes the run ID, and exits 75.
When adding the hidden child variant, extend `should_load_dotenv` and exercise
it through `hidden_child_nonce_parses_without_loading_dotenv`; it must use the
pre-dotenv process environment captured in plan 1.

```bash
cargo test -p loomweave-core --test analysis_intent
cargo test -p loomweave-cli --test worktree_analyze
cargo test -p loomweave-cli --test worktree_intent_cli
```

Expected: one run ID and writer exist in every ordering; foundation manual tests
remain green.

- [ ] **Step 5: Commit the shared intent protocol**

```bash
git add crates/loomweave-core crates/loomweave-cli
git commit -m "feat(core): coordinate worktree analysis intents"
test -z "$(git status --porcelain)"
```

### Task 2: Build the shared delayed `IndexAccess` state machine

**Files:**

- Create: `crates/loomweave-mcp/src/index_access.rs`
- Create: `crates/loomweave-mcp/src/readiness.rs`
- Create: `crates/loomweave-storage/src/active.rs`
- Create: `crates/loomweave-storage/src/run_authority.rs`
- Modify: `crates/loomweave-storage/src/lib.rs`
- Create: `crates/loomweave-storage/tests/run_authority.rs`
- Modify: `crates/loomweave-cli/src/cli.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/worktree.rs`
- Modify: `crates/loomweave-cli/src/doctor.rs`
- Modify: `crates/loomweave-cli/tests/worktree_intent_cli.rs`
- Create: `crates/loomweave-mcp/tests/index_access.rs`
- Modify: `crates/loomweave-mcp/src/lib.rs`
- Modify: `crates/loomweave-core/src/errors.rs`

- [ ] **Step 1: Add failing transition and atomic-publication tests**

```text
first_run_states_have_no_active_bundle
ready_publish_installs_the_complete_bundle_atomically
refreshing_retains_the_prior_bundle
refresh_failure_returns_to_stale_with_prior_bundle
activation_failure_never_publishes_a_partial_bundle
activation_retry_does_not_request_new_analysis
invalid_readiness_transitions_are_rejected
stale_generation_cannot_overwrite_newer_retry_or_ready
versioned_observer_cannot_miss_a_transition
authority_probe_opens_existing_database_read_only_without_migration
authority_probe_requires_exact_completed_row_and_completion_time
authority_probe_rejects_absent_busy_corrupt_or_wrong_schema_database
authority_probe_never_starts_a_pool_writer_or_embedding_connection
authority_probe_maps_skipped_no_plugins_to_terminal_failure
authority_probe_maps_failed_cancel_reason_to_cancelled_terminal
authority_probe_maps_ordinary_failed_to_failed_terminal
authority_probe_rejects_completed_without_completion_time
authority_probe_rejects_terminal_failure_without_completion_time
authority_probe_rejects_unknown_run_status
real_probe_reconciles_commit_before_terminal_intent
operator_recovery_requires_unchanged_token_free_writer_and_no_authority
operator_recovery_unknown_liveness_succeeds
operator_recovery_live_liveness_refuses
operator_recovery_fresh_heartbeat_refuses
operator_recovery_authority_row_refuses_abandonment
recover_intent_appears_in_worktree_help
recover_intent_double_dash_accepts_dash_prefixed_path
recover_intent_requires_run_id_and_confirmation
```

- [ ] **Step 2: Run the new test and verify RED**

```bash
cargo test -p loomweave-mcp --test index_access
cargo test -p loomweave-storage --test run_authority
cargo test -p loomweave-cli --test worktree_intent_cli
```

Expected: compilation fails because `IndexAccess` and readiness states do not
exist.

- [ ] **Step 3: Implement state and bundle ownership**

```rust
#[derive(Clone, Debug)]
pub enum IndexReadiness {
    Missing,
    Initializing { run_id: String },
    Building { run_id: String, progress: Option<ProgressSnapshot> },
    Activating { run_id: String },
    BuildFailed { run_id: String, failure: BuildFailure },
    ActivationFailed { run_id: String, failure: ActivationFailure },
    Ready,
    Stale { warning: StaleWarning },
    Refreshing { run_id: String, warning: StaleWarning },
}

pub struct ActiveStorage {
    pub readers: ReaderPool,
    pub summary_writer: Option<tokio::sync::mpsc::Sender<WriterCmd>>,
    pub wardline_writer: Option<
        tokio::sync::mpsc::Sender<WriterCmd>,
    >,
    pub embeddings_path: std::path::PathBuf,
}

pub struct IndexAccess {
    inner: tokio::sync::RwLock<IndexSnapshot>,
    changed: tokio::sync::watch::Sender<ReadinessVersion>,
}

impl IndexAccess {
    pub async fn snapshot(&self) -> IndexSnapshot;
    pub async fn require_active(&self) -> Result<std::sync::Arc<ActiveStorage>, IndexUnavailable>;
    pub async fn transition_if(
        &self,
        key: ReadinessRunKey,
        next: IndexReadiness,
    ) -> Result<(), TransitionError>;
    pub async fn publish_ready_if(
        &self,
        key: ReadinessRunKey,
        active: ActiveStorage,
    ) -> Result<(), TransitionError>;
    pub async fn publish_refresh_failure_if(
        &self,
        key: ReadinessRunKey,
        warning: StaleWarning,
    ) -> Result<(), TransitionError>;
}
```

Keep actor owners and join handles in an `ActivatedActors` supervisor object;
`ActiveStorage` is defined in `loomweave-storage`, contains only runtime-neutral
cloneable handles, and does not name MCP-private LLM/semantic types. MCP and
HTTP combine it with their own policy. Construct privately, tear down partial
activation, then publish once. Allocate a monotonically increasing
`ReadinessGeneration` for each reservation/retry and pair it with run ID in
`ReadinessRunKey`; every mutating method rejects stale keys. Watch subscribers
always reread the durable/current snapshot after a version change.

Implement `RunAuthorityProbe` in storage, not core, to preserve dependency
direction. It implements core's `RunAuthority` trait and is injected into
bootstrap control. The coordinator retains shared activity plus the matching
intent guard, revalidates the run/nonce, then non-blockingly acquires and retains
the writer guard through the probe. The coordinator validates the cause before
invoking the cause-agnostic storage probe. Permit invocation only for a matching
terminal intent; a locally killed/reaped child; an expired/stale intent whose
process identity is proven dead; or checksum-token-confirmed operator recovery
whose matching intent and token are unchanged, lease and heartbeat are stale,
liveness is exactly `Unknown`, and writer acquisition succeeded
non-blockingly. The fourth case refuses `Live` and never substitutes for
automatic proven-dead reclaim. Then open the existing DB with
read-only/no-create flags, set query-only mode, validate application/schema
identity, read exactly the requested run row, and close. Activation authority
belongs only to a matching `completed` row with a valid completion timestamp.
The closed terminal-failure variants are reconciliation evidence, never
activation authority. Missing DB/table, busy/migrating state, corruption,
wrong schema/run ID, and malformed required fields map to the typed outcomes
below. No other pre-ready service `Connection::open` is permitted.

Return the complete `RunAuthorityOutcome` enum from this probe. A matching
`completed` row with `completed_at` maps to `Completed`. A matching `failed`
row whose decoded stats have `terminal_reason == "cancelled"` maps to
`TerminalFailure { kind: Cancelled }`; an ordinary matching `failed` row maps
to `TerminalFailure { kind: Failed }`; and `skipped_no_plugins` maps to
`TerminalFailure { kind: SkippedNoPlugins }`, never success. Every terminal
status requires a valid completion timestamp; absence maps to
`InvalidSchemaOrCorrupt`. A `running` row maps to
`NonTerminal { kind: Running }`; an absent row or database maps to `Missing`;
lock contention maps to `TransientBusy`; and schema/identity/corruption
failures map to `InvalidSchemaOrCorrupt`. Treat malformed stats needed for
terminal classification and every unknown status as `InvalidSchemaOrCorrupt`,
not absence. Tests pin every storage mapping and the coordinator's semantic
terminalization behavior.

Implement `worktree recover-intent <name-or-path> --run-id <uuid> --confirm
<token>` now that the production probe exists. `doctor` derives the token from
the checked intent payload and reports prerequisites. The command enters the
same coordinator method with an operator-recovery cause: it retains shared
activity and matching intent, acquires writer non-blockingly, revalidates token,
lease, heartbeat, run/nonce, and invokes `RunAuthorityProbe` under both guards.
`Completed` or `TerminalFailure` preserves and terminalizes the matching
semantic result. Only an unchanged `Missing` or `NonTerminal` result with
liveness exactly `Unknown` is marked `abandoned`; `TransientBusy` retries and
`InvalidSchemaOrCorrupt` fails closed. It never signals a PID.

- [ ] **Step 4: Add shared structured error details and run GREEN**

Add core/MCP error codes for `index-building`, `index-build-failed`,
`index-activation-failed`, and `analyze-not-owned`. `IndexUnavailable` must
produce run ID, state, progress/failure, retryability, `fallback_argv`, and
display-only `fallback_command` from one serializer.

```bash
cargo test -p loomweave-mcp --test index_access
cargo test -p loomweave-mcp readiness::tests
cargo test -p loomweave-storage --test run_authority
cargo test -p loomweave-cli --test worktree_intent_cli
```

Expected: transitions, prior-graph retention, and error payload tests pass.

- [ ] **Step 5: Commit delayed index access**

```bash
git add crates/loomweave-core crates/loomweave-storage crates/loomweave-mcp \
  crates/loomweave-cli
git commit -m "feat(mcp): add atomic index readiness state"
test -z "$(git status --porcelain)"
```

### Tracer checkpoint: one same-session MCP bootstrap

**Files:**

- Create: `crates/loomweave-cli/src/serve/bootstrap.rs`
- Modify: `crates/loomweave-cli/src/serve.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Create: `crates/loomweave-cli/tests/worktree_serve_bootstrap.rs`

Before broad tool/resource/actor/HTTP migration, first add
`linked_first_serve_reader_tracer_activates_same_session` and run it RED:

```bash
cargo test -p loomweave-cli --test worktree_serve_bootstrap \
  linked_first_serve_reader_tracer_activates_same_session
```

Expected: current serve remains no-index and the same session never activates.
Then add the smallest linked-only slice in `serve/bootstrap.rs`: reserve intent,
spawn the hidden analysis child, observe one matching completed run row,
publish a reader-only `ActiveStorage`, and answer one graph tool on the original
MCP session. It may use a test-only single-reader activation factory, but it
must use the real durable intent and generation/run-key checks. Do not proceed
to Task 3 until the same command passes GREEN:

```bash
cargo test -p loomweave-cli --test worktree_serve_bootstrap \
  linked_first_serve_reader_tracer_activates_same_session
git add crates/loomweave-cli crates/loomweave-mcp crates/loomweave-storage
git commit -m "feat(serve): prove linked bootstrap tracer"
test -z "$(git status --porcelain)"
```

### Task 3: Gate MCP tools and resources without enlarging `lib.rs`

**Files:**

- Create: `crates/loomweave-mcp/src/tools/config.rs`
- Modify: `crates/loomweave-mcp/src/tools/mod.rs`
- Modify: `crates/loomweave-mcp/src/lib.rs`
- Modify: `crates/loomweave-mcp/src/tools/analyze.rs`
- Modify: `crates/loomweave-mcp/src/tools/status.rs`
- Modify: `crates/loomweave-mcp/src/tools/orientation.rs`
- Modify: `crates/loomweave-mcp/src/tools/summary.rs`
- Modify: `crates/loomweave-mcp/src/tools/graph.rs`
- Modify: `crates/loomweave-mcp/src/catalogue/semantic.rs`
- Modify: `crates/loomweave-mcp/src/catalogue/shortcuts.rs`
- Modify: `crates/loomweave-mcp/src/catalogue/inspection.rs`
- Modify: `crates/loomweave-mcp/src/catalogue/faceted.rs`
- Modify: `crates/loomweave-mcp/src/analyze_runs.rs`
- Modify: `crates/loomweave-mcp/tests/analyze_lifecycle.rs`
- Create: `crates/loomweave-mcp/tests/index_readiness.rs`

- [ ] **Step 1: Add failing building/failed/resource/tool-policy tests**

```text
initialize_succeeds_while_linked_index_is_building
tools_list_while_building_preserves_policy_inventory
database_tool_returns_structured_index_building
project_status_building_has_null_database_counts
context_resource_building_reads_no_database
write_tools_disabled_hides_analyze_start_and_cancel
attached_server_cancel_returns_analyze_not_owned
same_connection_serves_graph_after_activation
activation_failure_retries_only_activation
semantic_config_get_building_reports_null_count_without_sqlite
semantic_config_set_building_writes_origin_without_opening_embeddings
semantic_config_activation_failed_reports_null_count_without_sqlite
server_state_retains_post_open_repository_authority
```

- [ ] **Step 2: Run readiness tests and verify RED**

```bash
cargo test -p loomweave-mcp --test index_readiness
```

Expected: current `ServerState` requires an eager `ReaderPool`, and graph tools
can still reach it.

- [ ] **Step 3: Refactor `ServerState` and dispatch**

Production construction becomes:

```rust
pub struct ServerState {
    // Preserve every existing production field and builder contract, including
    // caps, clock, budgets, request cancellation, Filigree, Wardline,
    // diagnostics, and analyze configuration.
    context: std::sync::Arc<WorktreeContext>,
    repository_authority: std::sync::Arc<RepositoryAuthority>,
    index_access: std::sync::Arc<IndexAccess>,
    analysis: std::sync::Arc<AnalysisControlClient>,
    tool_policy: McpToolPolicy,
}

impl ServerState {
    pub fn for_service(
        context: std::sync::Arc<WorktreeContext>,
        repository_authority: std::sync::Arc<RepositoryAuthority>,
        index_access: std::sync::Arc<IndexAccess>,
        analysis: std::sync::Arc<AnalysisControlClient>,
    ) -> Self;
}
```

This is an additive constructor, not a replacement struct definition: audit
the live `ServerState` fields/builders and retain them all. The authority is the
post-open value from plan 1 and is retained for plan 3 status/cleanup; no
service field publishes `context.gc_preflight`. Keep an
already-ready test constructor while migrating fixtures. Before the
tool match, exempt only project/analyze status, config get/set, and policy-
allowed analyze start/cancel. Every other database-backed tool calls
`require_active`. Move config methods into `tools/config.rs`; setters use
`context.config_origin.path` exactly.

`resources/list` and prompts remain available. `loomweave://context` builds a
pre-ready snapshot from context, metadata, intent, and progress; DB counts are
JSON null. No resource handler opens SQLite before activation.

The live semantic config status path currently calls `semantic_sidecar_count`
and opens SQLite. Refactor get/set status to accept `IndexAccess` plus the
explicit embeddings path. Before active storage, report path/presence and
`vector_count: null` without opening the sidecar; after active storage, obtain
the count through the active bundle. Instrument connection opens and assert the
authority probe is the only permitted pre-ready SQLite connection.

- [ ] **Step 4: Add the PID-free analysis-control request boundary**

Define `AnalysisControlRequest` and a cloneable `AnalysisControlClient` in MCP
around a bounded request sender. Start/cancel handlers serialize a request and
await its typed reply; they never open the database and never own or receive a
PID/`RunHandle`. Exercise handlers with a fake receiver here, including
attached `analyze-not-owned`, queue-closed, and cancellation-reply mapping.
Task 4 supplies the CLI receiver, sole `RunHandle` owner, kill/reap operation,
and authority reconciliation after `BootstrapControl` exists.

```bash
cargo test -p loomweave-mcp --test index_readiness
cargo test -p loomweave-mcp --test analyze_lifecycle
```

Expected: policy, resource, same-session activation, and the PID-free request
contract pass against a fake control receiver.

- [ ] **Step 5: Commit MCP readiness integration**

```bash
git add crates/loomweave-mcp crates/loomweave-core/src/errors.rs
git commit -m "feat(mcp): gate tools and resources on index readiness"
test -z "$(git status --porcelain)"
```

### Task 4: Supervise first-serve bootstrap and atomic activation

**Files:**

- Modify: `crates/loomweave-cli/Cargo.toml`
- Create: `crates/loomweave-cli/src/owned_process_group.rs`
- Modify: `crates/loomweave-cli/src/serve/bootstrap.rs`
- Create: `crates/loomweave-cli/src/serve/activation.rs`
- Create: `crates/loomweave-cli/src/serve/runtime.rs`
- Modify: `crates/loomweave-cli/src/serve.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/config.rs`
- Modify: `crates/loomweave-cli/src/http_read.rs`
- Modify: `crates/loomweave-cli/tests/serve.rs`
- Modify: `crates/loomweave-mcp/src/analyze_runs.rs`
- Modify: `crates/loomweave-cli/tests/worktree_serve_bootstrap.rs`

- [ ] **Step 1: Add failing bootstrap, actor, retry, and refresh tests**

```text
linked_first_serve_builds_and_serves_on_same_connection
linked_first_serve_opens_no_service_database_actor_early
two_servers_attach_to_one_durable_run
attached_server_observes_owner_completion_and_activates_locally
attached_server_reclaims_only_after_pid_safe_owner_death
old_monitor_cannot_overwrite_newer_retry_generation
spawn_failure_clears_only_owned_pending_intent
owner_cancel_before_database_creation_terminalizes_intent
natural_completion_wins_cancel_race
build_failure_retries_analysis
activation_failure_retries_without_analysis
refresh_keeps_prior_graph_and_failed_refresh_returns_stale
serve_spawn_uses_pre_dotenv_environment
bootstrap_child_nulls_mcp_stdin
bootstrap_child_owns_dedicated_process_group
cancel_signals_only_bootstrap_child_group_and_reaps
exit_zero_without_completed_row_is_build_failure
skipped_no_plugins_is_build_failure
completed_zero_source_run_is_authoritative
owner_and_attached_servers_use_same_authority_probe_result
terminal_intent_with_missing_or_corrupt_run_row_stays_failed
stdio_eof_flushes_actors_before_activity_release
http_start_failure_rolls_back_partial_activation
```

Use test-only barriers at reservation, spawn, activation, run-row commit, and
writer-lock acquisition. Do not use sleeps for ordering.

- [ ] **Step 2: Run the integration test and verify RED**

```bash
cargo test -p loomweave-cli --test worktree_serve_bootstrap
```

Expected: current serve enters no-index mode and never starts a child.

- [ ] **Step 3: Implement bootstrap and activation boundaries**

```rust
pub enum BootstrapDecision {
    Owned(IntentOwner),
    Attached(AnalysisIntentSnapshot),
    Authoritative,
}

pub struct BootstrapControl {
    context: std::sync::Arc<WorktreeContext>,
    coordinator: AnalysisIntentCoordinator,
    authority_probe: std::sync::Arc<RunAuthorityProbe>,
    launcher: AnalyzeLauncher,
    process_env: std::sync::Arc<PreDotenvProcessEnvironment>,
    activation_factory: ActiveStorageFactory,
    control_runtime: tokio::runtime::Handle,
    index_access: std::sync::Arc<IndexAccess>,
    actor_slot: std::sync::Arc<ActivatedActorsSlot>,
    requests: tokio::sync::mpsc::Receiver<AnalysisControlRequest>,
    observer: Option<ObserverTask>,
    owned_run: Option<RunHandle>,
}

impl BootstrapControl {
    pub fn ensure(&mut self) -> Result<BootstrapDecision, BootstrapError>;

    pub fn start_observer(
        &mut self,
        decision: BootstrapDecision,
    ) -> Result<(), BootstrapError>;

    pub async fn run_control_loop(&mut self) -> Result<(), BootstrapError>;
}

pub struct ActiveStorageFactory;

impl ActiveStorageFactory {
    pub fn activate(
        context: &WorktreeContext,
        config: &ActivationConfig,
        runtime: &tokio::runtime::Handle,
    ) -> Result<PendingActivation, ActivationFailure>;
}
```

For a missing linked index, start MCP/HTTP with no `ActiveStorage`, reserve before
DB creation, and spawn the current executable with run ID, intent nonce, exact
config origin, `env_clear`, and the pre-dotenv environment. Progress uses only
the explicit progress file. The hidden analyzer uses null stdin/stdout and never
inherits the MCP transport. Main/standalone no-index behavior remains unchanged.

Add a target-Unix direct CLI dependency on
`nix = { workspace = true, features = ["signal"] }` and centralize child
ownership in `owned_process_group.rs`. On Unix, `OwnedProcessGroup::spawn`
calls the safe `CommandExt::process_group(0)` before spawn, retains the direct
`Child`, its dedicated PGID, and process-start identity, and refuses to signal
unless the PGID still equals the owned child group. TERM/KILL always targets
that group and is followed by direct-child wait/reap. Non-Unix builds expose an
explicit unsupported-cancellation outcome rather than PID-only signaling. Move
the current MCP process-group mechanics behind this CLI owner as MCP now sends
control requests and owns no child. Tests put an unrelated sibling in the
server group and prove cancellation kills/reaps only the dedicated analysis
group.

The same module exposes a narrower `spawn_provisional_detached_in_fresh_group`
for plan 3's cleanup supervisor. On Unix it applies the identical fresh-PGID
spawn barrier and returns a `ProvisionalDetachedProcess` retaining the direct
`Child`, PGID, process-start identity, and dedicated startup pipes. Before a
ready byte, `terminate_and_reap` and `Drop` kill and wait the direct child.
After ready, a consuming `arm_detached_before_launch` converts it into an
`ArmedDetachedProcess`: this state can no longer kill the supervisor, retains
the direct `Child`, launch pipe, and identity as a non-killing wait owner, and
closes the pipe then waits/reaps the blocked pre-worker supervisor on `Drop`.
Its consuming `commit_launch` writes the exact one-byte launch value. Write
failure closes the pipe, waits/reaps, and only then returns `spawn-failed`;
success closes the pipe, relinquishes the `Child` without signalling it, and
returns the non-cancelling `DetachedProcessIdentity`. Abrupt process death falls
back to the operating-system init or plan 3's PID1 wrapper.

For a server scheduler, consuming `promote_owned_before_launch` instead creates
an `OwnedCleanupSupervisor` in `AwaitingLaunch` state. The scheduler stores that
owner before its non-consuming `commit_launch(&mut self)` writes the launch byte
and transitions to `Running`. Write failure closes the pipe, transitions to
`Draining`, reaps the pre-worker child, and returns `spawn-failed`; shutdown from
`AwaitingLaunch` also closes the pipe before waiting. Shutdown retains and
drains every state. Thus no kill-on-drop owner exists after a worker can start.
Barrier tests cancel or terminate at spawn, ready, armed, launch-readable, and
post-launch boundaries. No pre-launch worker exists, while every launched
supervisor retains its worker-tree reaper independently of the launcher.

Every server runs the monitor, including `Attached`. It uses bounded backoff and
versioned watch notifications, rereads the lock-consistent intent plus
progress/heartbeat and matching run row, and activates that process's own
local actors after authority is proven. Owner death enters the same
PID-reuse-safe reconcile/election path. Mutations use the expected
`ReadinessRunKey`; a stale observer cannot overwrite a newer retry.

`BootstrapControl` is the sole owner of every child `RunHandle`; `ensure(&mut
self)` stores a newly spawned handle before returning. MCP receives only a
cloneable `AnalysisControlClient` that sends start/cancel requests to the
control loop. Child exit, explicit cancel, observer results, activation, and
shutdown are serialized there, so no second supervisor or handler owns a PID.
Each `RunHandle` retains its `IntentOwner`. On owning cancel, this loop preserves
the current process-group behavior, kills and reaps first, then calls
`cancel_after_reap(&activity, &owner, authority_probe.as_ref())`; a typed
completed/failed authority result wins the race. Attached servers have no
handle, never signal, and return `analyze-not-owned`.

`ServeRuntime` owns `BootstrapControl` and one `ActivatedActorsSlot`. Plan 3
adds its runtime-owned startup/periodic cleanup scheduler directly; there is no
analysis-complete trigger handle in the server because each analyzer process
owns that post-run scheduling step.
`ActiveStorageFactory` returns a private `PendingActivation` containing both
cloneable storage handles and actor join owners. The control loop installs the
join owners into the empty slot and publishes storage/readiness as one guarded
operation; rollback removes/joins the same pending owners without publication.
Shutdown closes request intake, wins the same serialization point against
cancel/activation, unpublishes storage/readiness so no new clone can escape,
drains protocol state clones, closes actor senders, takes and joins the actor
slot, then cancels/joins the observer. Ordinary non-PID1 shutdown detaches an
in-flight child to finish under its own durable intent/activity/writer guards;
explicit cancel kills/reaps it. Plan 3 ensures the real `ServeRuntime` is also
non-PID1 by placing a minimal init/reaper wrapper above it when the command
starts as Linux PID1. The wrapper remains alive until every detached analyzer
and cleanup descendant is reaped. Tests barrier shutdown-vs-cancel,
shutdown-vs-activation, partial rollback, and a new server attaching to a
detached child.

Authority means a matching `runs` row with `status == completed` and a
completion timestamp. Child exit zero, `skipped_no_plugins`, pre-row migration
or discovery failure, missing matching row, and other terminal statuses are
build failures. Preserve pre-row failure diagnostics in the strict 1,024-byte
private intent field defined in Task 1. A completed zero-source run is valid.

Activation order is reader pool, optional MCP LLM writer, optional HTTP
Wardline writer, and semantic provider with explicit embeddings path. Tear down
all earlier actors if a later step fails. Publish only the complete bundle.

Implement a top-level `ServeRuntime` that owns the shared activity guard,
`IndexAccess`, dedicated control runtime, `ActivatedActors` join handles, and
MCP/HTTP runtimes. Database actors live on the control runtime; protocol
runtimes receive senders only. On shutdown: stop accepting, unpublish
storage/readiness, drain/drop protocol state clones, close senders, await actor
joins, cancel/join the observer, then drop runtimes and activity. Test stdio
EOF, HTTP failure, partial activation rollback, retry, pending-write flush, and
bounded shutdown.

Refresh success reuses the current `ActiveStorage` and conditionally transitions
the matching run key to ready; failure retains that same bundle as stale. It
must not start duplicate writers.

- [ ] **Step 4: Remove eager DB opens and run GREEN**

Remove eager `ReaderPool` construction from `serve::run`, LLM `Writer::spawn`
from `run_mcp_stdio`, and Wardline `Writer::spawn` from the HTTP server thread.
Handlers obtain clones from `IndexAccess` after readiness.

```bash
cargo test -p loomweave-cli --test worktree_serve_bootstrap
cargo test -p loomweave-mcp --test index_readiness
```

Expected: first serve transitions in-place; no service DB actor opens early;
refresh behavior remains compatible.

- [ ] **Step 5: Commit automatic bootstrap and activation**

```bash
git add crates/loomweave-cli crates/loomweave-mcp/src/analyze_runs.rs
git commit -m "feat(serve): bootstrap linked indexes on first serve"
test -z "$(git status --porcelain)"
```

### Task 5: Share readiness with the HTTP API

**Files:**

- Create: `crates/loomweave-cli/src/http_read/readiness.rs`
- Modify: `crates/loomweave-cli/src/http_read.rs`
- Modify: `docs/federation/fixtures/get-api-v1-capabilities.json`
- Modify: `docs/federation/fixtures/get-api-v1-capabilities.json.sha256`
- Modify: `docs/federation/contracts.md`
- Modify: `docs/federation/2026-07-12-federation-seam-golden-authority.md`
- Modify the embedded capabilities BLAKE3/shape pin in
  `crates/loomweave-cli/tests/serve.rs`; the generator does not own it.

- [ ] **Step 1: Add failing HTTP readiness tests**

```text
capabilities_available_while_index_builds
database_routes_return_503_index_building
database_routes_return_503_index_build_failed_with_fallback_argv
database_routes_return_503_index_activation_failed
existing_listener_serves_after_activation_without_rebind
wardline_writer_is_absent_before_and_present_after_activation
http_and_mcp_error_details_are_identical
```

Also add the ignored, repository-owned
`print_capabilities_golden_blake3` helper beside the existing pin now. It
hashes the included fixture with the already-pinned Rust `blake3` dependency
and prints lowercase hex. Normal test runs skip it intentionally; Step 4 calls
it explicitly after regeneration.

- [ ] **Step 2: Run the HTTP tests and verify RED**

```bash
cargo test -p loomweave-cli --bin loomweave http_read::readiness
```

Expected: current `AppState` requires eager readers and the Wardline writer.

- [ ] **Step 3: Put `IndexAccess` in `AppState` and add middleware**

```rust
pub struct AppState {
    pub context: std::sync::Arc<WorktreeContext>,
    pub index_access: std::sync::Arc<IndexAccess>,
    pub instance_id: crate::instance::InstanceId,
    pub auth_token: Option<std::sync::Arc<String>>,
    pub identity_secret: Option<std::sync::Arc<String>>,
    pub hmac_replay_cache: crate::http_read::auth::SharedHmacReplayCache,
}
```

Apply readiness middleware to database-backed `/api/v1` and Wardline routes.
Exclude `/api/v1/_capabilities`, which remains available and reports readiness.
Map the three index errors to HTTP 503 with the exact shared details serializer.

- [ ] **Step 4: Regenerate the capability golden and run GREEN**

Run the repository producer wrapper, which builds the workspace binary,
force-refreshes the editable Python plugin, and invokes the JSON/SHA-256
generator. Then invoke the ignored helper added in Step 1, copy its digest into
`CAPABILITIES_GOLDEN_BLAKE3` in
`crates/loomweave-cli/tests/serve.rs`, and update the capability row in the
federation golden authority note using the repository's documented hash-table
format. The generator does not update either pin. `loomweave-cli` is
binary-only, so target `--bin loomweave`, never `--lib`.

```bash
bash scripts/generate-federation-seam-goldens.sh
cargo test -p loomweave-cli --test serve \
  print_capabilities_golden_blake3 -- --ignored --exact --nocapture
# Copy the printed lowercase digest into CAPABILITIES_GOLDEN_BLAKE3 and the
# authority-note capability row before running the ordinary suites.
cargo test -p loomweave-cli --bin loomweave http_read::readiness
cargo test -p loomweave-cli --bin loomweave http_read::tests
cargo test -p loomweave-cli --test serve
cargo test -p loomweave-mcp --test index_readiness
cargo test -p loomweave-storage --test classifier_coverage \
  authority_note_hash_table_is_locked_to_every_fixture_and_sidecar
bash scripts/check-federation-seam-goldens-hermetic.sh
```

Expected: the existing listener serves after activation without a rebind, and
HTTP never exposes an empty authoritative graph. The generated JSON fixture,
SHA-256 sidecar, embedded BLAKE3 pin/shape, and live `serve` contract agree.

- [ ] **Step 5: Commit HTTP readiness and contracts**

```bash
git add crates/loomweave-cli/src/http_read.rs \
  crates/loomweave-cli/src/http_read crates/loomweave-cli/tests/serve.rs \
  docs/federation
git commit -m "feat(http): share linked-index readiness"
test -z "$(git status --porcelain)"
```

### Task 6: Pin cross-entry races and remove the path-audit allowlist

**Files:**

- Modify, when still reported by the production call-site audit:
  `crates/loomweave-cli/src/serve.rs`,
  `crates/loomweave-cli/src/serve/**/*.rs`,
  `crates/loomweave-cli/src/http_read.rs`, and
  `crates/loomweave-cli/src/http_read/**/*.rs`.
- Modify, when still reported by that same audit: production files under
  `crates/loomweave-mcp/src/**/*.rs`, `crates/loomweave-storage/src/**/*.rs`,
  and `crates/loomweave-federation/src/**/*.rs`. Before editing, save the
  audit's exact file list in the task log; no unreported production file enters
  this task without a concrete compile/audit failure.
- Modify: `crates/loomweave-mcp/tests/analyze_lifecycle.rs`
- Modify: `crates/loomweave-cli/tests/worktree_serve_bootstrap.rs`
- Modify: `crates/loomweave-cli/tests/runtime_path_callsite_audit.rs`
- Modify: `crates/loomweave-mcp/tests/storage_tools.rs`
- Modify: `crates/loomweave-mcp/tests/catalogue_tools.rs`
- Modify: `crates/loomweave-mcp/tests/federation_classification_golden.rs`

- [ ] **Step 1: Add the full barrier matrix and fallback argv cases**

Run direct analyze, worktree analyze, bootstrap, two servers, and MCP
`analyze_start` through every reservation/spawn/activation ordering. Assert one
run ID and writer. Add argv cases for leading dash, spaces, quotes,
metacharacters, and newlines; execute argv directly and never parse the display
command. Add an explicit-origin case and assert fallback argv is exactly
`["loomweave", "worktree", "analyze", "--config", "/custom", "--",
"/canonical/path"]`; source/primary/default origins omit `--config`.

In a real repository, barrier main plus two linked analyses concurrently and
assert three DB paths, lock files, run IDs, sidecars, and divergent graphs. The
serve/analyze-versus-collection case belongs to plan 3 after GC exists.

- [ ] **Step 2: Run race tests and verify any remaining RED paths**

```bash
cargo test -p loomweave-mcp --test analyze_lifecycle
cargo test -p loomweave-cli --test worktree_serve_bootstrap
```

Expected: any missed direct path or cancellation ordering fails deterministically.

- [ ] **Step 3: Migrate all remaining service path consumers**

Remove production uses of root-based DB, embeddings, diagnostics, instance,
port, runs, config, and baseline helpers from CLI serve/HTTP and MCP. Change all
test constructors to pass a `WorktreeContext` plus explicit `StorePaths`.

Remove the temporary service allowlist from the syntax-aware
`runtime_path_callsite_audit.rs`. Audit every production call/method/join
independent of argument spelling, including `config.rs` and all root-taking DB,
store, embeddings, traffic, instance, port, baseline, diagnostics, runs,
hooks/status/install, and federation wrappers. The only remaining root-derived
store call is repository-store calculation inside the worktree resolver.

- [ ] **Step 4: Run the audit and service suites GREEN**

```bash
cargo test -p loomweave-cli --test runtime_path_callsite_audit
cargo test -p loomweave-mcp --test index_readiness
cargo test -p loomweave-mcp --test analyze_lifecycle
cargo test -p loomweave-mcp --test storage_tools
cargo test -p loomweave-mcp --test catalogue_tools
cargo test -p loomweave-mcp --test federation_classification_golden
cargo test -p loomweave-cli --test serve
bash scripts/generate-federation-seam-goldens.sh
bash scripts/check-federation-seam-goldens-hermetic.sh
cargo check -p loomweave-storage -p loomweave-federation \
  -p loomweave-mcp -p loomweave-cli
```

Expected: no production service can re-derive an effective store from a source
root.

- [ ] **Step 5: Commit race coverage and completed routing**

```bash
git add crates/loomweave-cli crates/loomweave-mcp \
  crates/loomweave-storage crates/loomweave-federation
git commit -m "test(worktree): pin bootstrap races and path routing"
test -z "$(git status --porcelain)"
```

## Part 2 verification

- [ ] **Step 1: Run service-focused gates**

```bash
cargo nextest run -p loomweave-core --test analysis_intent
cargo nextest run -p loomweave-storage --test run_authority
cargo nextest run -p loomweave-mcp \
  --test index_access \
  --test index_readiness \
  --test analyze_lifecycle
cargo nextest run -p loomweave-cli \
  --test dotenv_policy \
  --test worktree_intent_cli \
  --test worktree_serve_bootstrap \
  --test runtime_path_callsite_audit \
  --test serve
cargo test -p loomweave-cli --bin loomweave http_read::readiness
bash scripts/generate-federation-seam-goldens.sh
bash scripts/check-federation-seam-goldens-hermetic.sh
cargo fmt --all -- --check
cargo clippy -p loomweave-core -p loomweave-mcp -p loomweave-cli \
  --all-targets -- -D warnings
git status --short
```

Expected: automatic first serve, same-session activation, refresh retention,
tool policy, HTTP readiness, and all race contracts pass.

- [ ] **Step 2: Continue directly to plan 3**

Do not merge or release yet. Continue in the same feature worktree with
`2026-07-18-worktree-indexes-3-cleanup.md`; periodic sanity checks and safe
orphan reclamation remain acceptance requirements.
