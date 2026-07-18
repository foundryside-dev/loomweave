# Worktree Indexes Part 2: Bootstrap and Readiness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Automatically build a missing linked-worktree index on first `serve`
while MCP and HTTP remain connected and report authoritative readiness.

**Architecture:** Every analysis entry point reserves one durable intent before
opening SQLite. A shared `IndexAccess` publishes a complete reader/writer bundle
only after the first run is authoritative; refreshes retain the prior bundle.
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
- Create `crates/loomweave-mcp/src/index_access.rs` for the readiness state
  machine and atomically published service bundle.
- Create `crates/loomweave-mcp/src/readiness.rs` for shared structured errors.
- Move MCP configuration tools from the large `lib.rs` into
  `crates/loomweave-mcp/src/tools/config.rs`.
- Split CLI serving orchestration into `serve/bootstrap.rs` and
  `serve/activation.rs`; keep `serve.rs` as the top-level coordinator.
- Create `crates/loomweave-cli/src/http_read/readiness.rs` for Axum readiness
  middleware and HTTP error conversion.
- Put new integration scenarios in dedicated test files rather than enlarging
  `serve.rs`, `http_read.rs`, or MCP `lib.rs`.

### Task 1: Coordinate every analysis through one durable intent

**Files:**

- Create: `crates/loomweave-core/src/worktree/analysis_intent.rs`
- Modify: `crates/loomweave-core/src/worktree/mod.rs`
- Modify: `crates/loomweave-core/src/worktree/locks.rs`
- Modify: `crates/loomweave-core/src/errors.rs`
- Modify: `crates/loomweave-cli/src/cli.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Modify: `crates/loomweave-cli/src/analyze_lock.rs`
- Modify: `crates/loomweave-cli/src/run_lifecycle.rs`
- Create: `crates/loomweave-core/tests/analysis_intent.rs`

- [ ] **Step 1: Add failing reservation, reclaim, and cancel-race tests**

Use deterministic clock, process-liveness, progress-heartbeat, and barrier
fakes. Add:

```text
reserve_is_atomic_and_second_launcher_attaches_same_run
activate_requires_matching_nonce_and_holds_writer_lock
manual_second_launcher_returns_exit_75_without_waiting
stale_intent_is_not_reclaimed_while_process_is_live
stale_intent_is_not_reclaimed_while_heartbeat_is_fresh
stale_intent_reclaims_only_when_all_four_checks_agree
spawn_failure_clears_only_matching_pending_nonce
graceful_terminal_reconciles_matching_run_row
cancel_before_runs_row_terminalizes_intent
terminal_run_row_wins_cancel_race
```

- [ ] **Step 2: Run the tests and verify RED**

```bash
cargo test -p loomweave-core --test analysis_intent
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

pub enum IntentTerminal {
    Completed,
    Failed { reason: String },
    Cancelled,
}

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
        owner: &IntentOwner,
        terminal: IntentTerminal,
    ) -> Result<(), IntentError>;

    pub fn clear_pending_after_spawn_failure(
        &self,
        owner: &IntentOwner,
    ) -> Result<bool, IntentError>;

    pub fn cancel_after_reap(
        &self,
        owner: &IntentOwner,
        durable_terminal: Option<DurableRunTerminal>,
    ) -> Result<IntentTerminal, IntentError>;

    pub fn reconcile_or_reclaim(&self) -> Result<ReconcileOutcome, IntentError>;
    pub fn snapshot(&self) -> Result<Option<AnalysisIntentSnapshot>, IntentError>;
}
```

Reservation holds shared activity, then intent, then writer during activation.
Stale reclaim requires expired lease, no heartbeat, dead process-start identity,
and an acquirable writer lock. `finish` releases writer before taking intent.

- [ ] **Step 4: Route direct analysis and hidden child activation through it**

Add hidden `--analysis-intent-nonce <NONCE>`. A normal direct invocation reserves
and activates in-process. A serve-spawned child receives run ID plus nonce,
validates the pending record, and only then opens SQLite. A second manual
invocation prints `analyze-already-running`, includes the run ID, and exits 75.

```bash
cargo test -p loomweave-core --test analysis_intent
cargo test -p loomweave-cli --test worktree_analyze
```

Expected: one run ID and writer exist in every ordering; foundation manual tests
remain green.

- [ ] **Step 5: Commit the shared intent protocol**

```bash
git add crates/loomweave-core crates/loomweave-cli
git commit -m "feat(core): coordinate worktree analysis intents"
```

### Task 2: Build the shared delayed `IndexAccess` state machine

**Files:**

- Create: `crates/loomweave-mcp/src/index_access.rs`
- Create: `crates/loomweave-mcp/src/readiness.rs`
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
```

- [ ] **Step 2: Run the new test and verify RED**

```bash
cargo test -p loomweave-mcp --test index_access
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

pub struct ActiveIndex {
    pub readers: loomweave_storage::ReaderPool,
    pub summary_llm: Option<SummaryLlmState>,
    pub semantic_search: Option<SemanticSearchState>,
    pub wardline_writer: Option<
        tokio::sync::mpsc::Sender<loomweave_storage::WriterCmd>,
    >,
}

pub struct IndexAccess {
    inner: tokio::sync::RwLock<IndexSnapshot>,
    changed: tokio::sync::Notify,
}

impl IndexAccess {
    pub async fn snapshot(&self) -> IndexSnapshot;
    pub async fn require_active(&self) -> Result<std::sync::Arc<ActiveIndex>, IndexUnavailable>;
    pub async fn transition(&self, next: IndexReadiness) -> Result<(), TransitionError>;
    pub async fn publish_ready(&self, active: ActiveIndex) -> Result<(), TransitionError>;
    pub async fn publish_refresh_failure(
        &self,
        warning: StaleWarning,
    ) -> Result<(), TransitionError>;
}
```

Keep actor owners and join handles in an `ActivatedActors` supervisor object;
`ActiveIndex` stores only cloneable service handles. Construct privately, tear
down partial activation, then publish once.

- [ ] **Step 4: Add shared structured error details and run GREEN**

Add core/MCP error codes for `index-building`, `index-build-failed`,
`index-activation-failed`, and `analyze-not-owned`. `IndexUnavailable` must
produce run ID, state, progress/failure, retryability, `fallback_argv`, and
display-only `fallback_command` from one serializer.

```bash
cargo test -p loomweave-mcp --test index_access
cargo test -p loomweave-mcp readiness::tests
```

Expected: transitions, prior-graph retention, and error payload tests pass.

- [ ] **Step 5: Commit delayed index access**

```bash
git add crates/loomweave-core crates/loomweave-mcp
git commit -m "feat(mcp): add atomic index readiness state"
```

### Task 3: Gate MCP tools and resources without enlarging `lib.rs`

**Files:**

- Create: `crates/loomweave-mcp/src/tools/config.rs`
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
    context: std::sync::Arc<WorktreeContext>,
    index_access: std::sync::Arc<IndexAccess>,
    analysis: std::sync::Arc<AnalysisSupervisor>,
    tool_policy: McpToolPolicy,
}

impl ServerState {
    pub fn for_service(
        context: std::sync::Arc<WorktreeContext>,
        index_access: std::sync::Arc<IndexAccess>,
        analysis: std::sync::Arc<AnalysisSupervisor>,
    ) -> Self;
}
```

Keep an already-ready test constructor while migrating fixtures. Before the
tool match, exempt only project/analyze status, config get/set, and policy-
allowed analyze start/cancel. Every other database-backed tool calls
`require_active`. Move config methods into `tools/config.rs`; setters use
`context.config_origin.path` exactly.

`resources/list` and prompts remain available. `loomweave://context` builds a
pre-ready snapshot from context, metadata, intent, and progress; DB counts are
JSON null. No resource handler opens SQLite before activation.

- [ ] **Step 4: Make run ownership and cancellation durable**

Extend `RunHandle` with its `IntentOwner`. The owning cancel path kills and
reaps first, reads a matching terminal DB row if one exists, then calls
`cancel_after_reap`. Natural completed/failed rows win. Attached servers never
signal a PID. Preserve the current process-group kill behavior.

```bash
cargo test -p loomweave-mcp --test index_readiness
cargo test -p loomweave-mcp --test analyze_lifecycle
```

Expected: policy, resource, same-session activation, and cancellation tests
pass.

- [ ] **Step 5: Commit MCP readiness integration**

```bash
git add crates/loomweave-mcp crates/loomweave-core/src/errors.rs
git commit -m "feat(mcp): gate tools and resources on index readiness"
```

### Task 4: Supervise first-serve bootstrap and atomic activation

**Files:**

- Create: `crates/loomweave-cli/src/serve/bootstrap.rs`
- Create: `crates/loomweave-cli/src/serve/activation.rs`
- Modify: `crates/loomweave-cli/src/serve.rs`
- Modify: `crates/loomweave-cli/src/main.rs`
- Modify: `crates/loomweave-cli/src/config.rs`
- Modify: `crates/loomweave-cli/src/http_read.rs`
- Modify: `crates/loomweave-mcp/src/analyze_runs.rs`
- Create: `crates/loomweave-cli/tests/worktree_serve_bootstrap.rs`

- [ ] **Step 1: Add failing bootstrap, actor, retry, and refresh tests**

```text
linked_first_serve_builds_and_serves_on_same_connection
linked_first_serve_opens_no_service_database_actor_early
two_servers_attach_to_one_durable_run
spawn_failure_clears_only_owned_pending_intent
owner_cancel_before_database_creation_terminalizes_intent
natural_completion_wins_cancel_race
build_failure_retries_analysis
activation_failure_retries_without_analysis
refresh_keeps_prior_graph_and_failed_refresh_returns_stale
serve_spawn_uses_pre_dotenv_environment
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

pub struct BootstrapSupervisor;

impl BootstrapSupervisor {
    pub fn ensure(
        context: &WorktreeContext,
        coordinator: &AnalysisIntentCoordinator,
        launcher: &AnalyzeLauncher,
    ) -> Result<BootstrapDecision, BootstrapError>;

    pub async fn monitor(
        &self,
        decision: BootstrapDecision,
        index_access: std::sync::Arc<IndexAccess>,
    ) -> Result<(), BootstrapError>;
}

pub struct ActiveIndexFactory;

impl ActiveIndexFactory {
    pub fn activate(
        context: &WorktreeContext,
        config: &ActivationConfig,
        runtime: &tokio::runtime::Handle,
    ) -> Result<ActivatedIndex, ActivationFailure>;
}
```

For a missing linked index, start MCP/HTTP with no `ActiveIndex`, reserve before
DB creation, and spawn the current executable with run ID, intent nonce, exact
config origin, `env_clear`, and the pre-dotenv environment. Main/standalone
no-index behavior remains unchanged.

Activation order is reader pool, optional MCP LLM writer, optional HTTP
Wardline writer, and semantic provider with explicit embeddings path. Tear down
all earlier actors if a later step fails. Publish only the complete bundle.

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
```

### Task 5: Share readiness with the HTTP API

**Files:**

- Create: `crates/loomweave-cli/src/http_read/readiness.rs`
- Modify: `crates/loomweave-cli/src/http_read.rs`
- Modify: `docs/federation/fixtures/get-api-v1-capabilities.json`
- Modify: `docs/federation/fixtures/get-api-v1-capabilities.json.sha256`
- Modify: `docs/federation/contracts.md`
- Modify: `docs/federation/2026-07-12-federation-seam-golden-authority.md`

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

- [ ] **Step 2: Run the HTTP tests and verify RED**

```bash
cargo test -p loomweave-cli http_read::readiness --lib
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
    pub hmac_replay_cache: HmacReplayCache,
}
```

Apply readiness middleware to database-backed `/api/v1` and Wardline routes.
Exclude `/api/v1/_capabilities`, which remains available and reports readiness.
Map the three index errors to HTTP 503 with the exact shared details serializer.

- [ ] **Step 4: Regenerate the capability golden and run GREEN**

Use the existing golden generator/test path; do not edit the checksum by hand.

```bash
cargo test -p loomweave-cli http_read::readiness --lib
cargo test -p loomweave-cli http_read::tests --lib
cargo test -p loomweave-mcp --test index_readiness
```

Expected: the existing listener serves after activation without a rebind, and
HTTP never exposes an empty authoritative graph.

- [ ] **Step 5: Commit HTTP readiness and contracts**

```bash
git add crates/loomweave-cli/src/http_read.rs \
  crates/loomweave-cli/src/http_read docs/federation
git commit -m "feat(http): share linked-index readiness"
```

### Task 6: Pin cross-entry races and remove the path-audit allowlist

**Files:**

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
command.

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

Remove the temporary service allowlist from
`runtime_path_callsite_audit.rs`. The only remaining root-derived store call is
the repository-store calculation inside the worktree resolver.

- [ ] **Step 4: Run the audit and service suites GREEN**

```bash
cargo test -p loomweave-cli --test runtime_path_callsite_audit
cargo test -p loomweave-mcp --test index_readiness
cargo test -p loomweave-mcp --test analyze_lifecycle
cargo test -p loomweave-mcp --test storage_tools
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
```

## Part 2 verification

- [ ] **Step 1: Run service-focused gates**

```bash
cargo nextest run -p loomweave-core --test analysis_intent
cargo nextest run -p loomweave-mcp \
  --test index_access \
  --test index_readiness \
  --test analyze_lifecycle
cargo nextest run -p loomweave-cli \
  --test worktree_serve_bootstrap \
  --test runtime_path_callsite_audit
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
