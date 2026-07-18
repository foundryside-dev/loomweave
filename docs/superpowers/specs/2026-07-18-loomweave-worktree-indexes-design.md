# Worktree-Scoped Loomweave Indexes Design

**Date:** 2026-07-18

**Status:** Independently reviewed; ready for implementation

**Tracker:** `clarion-c297efc752`

**Decision:** Loomweave will keep the main checkout's store unchanged and give
each linked Git worktree a separate SQLite store under the primary checkout's
Loomweave store. `serve` will create and analyze a missing linked-worktree index
in the background. Loomweave will also check registered worktrees periodically
and tombstone an abandoned index only after two successful absence checks at
least 24 hours apart. It may recursively delete that tombstone only after a
later successful absence check and another 24-hour recovery window. Automatic
cleanup is restricted to an owned, canonical default store; operator-relocated
stores remain usable but are never garbage-collected automatically in v1.

## Problem

Loomweave derives every store path from the source root. `store_dir()` maps a
project to `<project>/.weft/loomweave/`, `serve` enters no-index mode when the
derived database is absent, and `analyze` refuses to run until that local store
exists. A linked worktree normally has none of Loomweave's ignored runtime
files, so a correctly registered MCP process starts in the worktree and still
cannot answer graph queries.

Pointing a linked worktree at the main checkout's existing database would be
incorrect. The current schema stores canonical absolute source paths and uses
them for incremental analysis and integrity checks. A main-checkout database
therefore describes the wrong files, commit, and dirty state for a linked
worktree.

The fix must also account for lifecycle. Git worktrees are routinely removed,
and their source paths no longer exist after removal. Keeping each database in
the removed worktree would make it impossible for Loomweave to discover and
clean up that database later.

## Goals

- Make a linked worktree's graph reflect that worktree's own HEAD and dirty
  files, independently of the main checkout and other worktrees.
- Preserve the existing main-checkout store path and behavior.
- Let independent worktrees analyze and serve concurrently without sharing a
  SQLite writer.
- Bootstrap a missing linked-worktree index during the first `serve` session.
- Keep the MCP session usable while bootstrap runs and make readiness explicit.
- Give MCP and HTTP clients the same readiness verdict while bootstrap runs.
- Provide an explicit analysis command for recovery and operator control.
- Detect removed worktrees automatically and reclaim their Loomweave stores
  conservatively.
- Treat Git output and stored paths as untrusted input at every deletion
  boundary.

## Non-goals

- Do not partition the existing graph schema by worktree ID.
- Do not clone or rewrite the main database to seed a worktree in v1.
- Do not make the graph live-update after every file write. Existing staleness
  reporting, session hooks, and explicit analysis remain responsible for later
  edits.
- Do not run `git worktree prune` or otherwise modify Git's administrative
  state.
- Do not copy ignored `.mcp.json`, skill, hook, or instruction files into every
  worktree. Runtime index support and agent-asset installation are separate
  concerns.
- Do not silently import or delete a legacy `.weft/loomweave/` store that an
  operator manually created inside a linked worktree.
- Do not automatically delete from a `[loomweave].store_dir` override in v1.
  Analysis and serving support overrides, but cleanup remains diagnostic-only.
- Do not automatically delete quarantined stores in v1. Quarantine is a
  recoverable operator-inspection surface.

## Terminology

- **Source root:** the canonical checkout directory Loomweave analyzes.
- **Primary root:** the main worktree for the Git common directory.
- **Repository store:** the primary root's Loomweave store, including any
  `[loomweave].store_dir` override from the primary root's `weft.toml`.
- **Effective store:** the directory containing the database and sidecars for
  the current source root.
- **Git administrative identity:** the linked worktree's Git directory relative
  to the common Git directory, for example
  `worktrees/federation-seam-followups`.
- **Stable worktree ID:** a filesystem-safe digest derived from that
  administrative identity.
- **Configuration origin:** the exact file a server read and, if a configuration
  setter runs, the only file that setter may update.
- **Store paths:** a typed set of explicit runtime paths derived once from the
  worktree context; downstream components do not re-derive them from a source
  root.

## Chosen Architecture

### Worktree context

Add one resolver that produces a `WorktreeContext` before any runtime path is
chosen:

```text
WorktreeContext
  source_root             canonical path being analyzed
  primary_root            canonical main-worktree path
  repository_store        store_dir(primary_root)
  effective_store         repository_store, or repository_store/worktrees/<id>
  store_paths             explicit database, sidecar, run, and lock paths
  config_origin           explicit | source | primary | default-target
  sibling_lookup_roots    source root, then primary root, deduplicated
  gc_capability           enabled-owned-default | disabled-with-reason
  kind                    standalone | main | linked
  git_common_dir          canonical common Git directory when available
  git_admin_identity      relative linked-worktree administrative path
  stable_id               wt-<full BLAKE3 hex digest> for linked worktrees
```

Program startup captures a `TrustedGitContext` before loading any
repository-controlled `.env`. It resolves an absolute Git executable from the
operator's original `PATH` and retains only the minimal host environment needed
to launch it. `loomweave_core::hardened_git_command` itself is extended to
strip repository-selector environment for every existing caller, and a
context-bound variant uses the captured executable and allowlist. Every
resolver and cleanup probe must use the context-bound variant with argument
arrays and no shell.

The extended helper starts from `env_clear()`, restores only the trusted launch
allowlist, fixes locale for machine parsing, and reapplies the existing hostile
config/attribute overrides. It never inherits repository selectors or Git
execution overrides, including `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`,
`GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`,
`GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_NAMESPACE`, `GIT_EXEC_PATH`, or any
`GIT_CONFIG_*` value except the helper's own fixed values. Its Git-version probe
uses the same absolute executable and trusted environment.

Git stdout and stderr are drained concurrently through a bounded runner. The
compiled caps are 64 KiB per `rev-parse` stream, 8 MiB for worktree-list stdout,
and 64 KiB for worktree-list stderr; repository configuration cannot raise
them. The runner kills and reaps Git as soon as a cap is exceeded. It never
uses `Command::output()`, whose length could be checked only after unbounded
allocation. A non-zero exit, overflow, read error, or malformed output fails the
entire probe.

The resolver uses absolute `git rev-parse` results and NUL-delimited
`git worktree list --porcelain -z` output. These operations do not hash
working-tree content. The parser rejects truncated or unterminated records,
duplicate singleton fields, and malformed paths. It identifies the primary
worktree by resolving listed worktrees and selecting the entry whose Git
directory equals the common Git directory; it does not assume that a branch
name or directory name is unique.

For a linked worktree, the stable ID is `wt-` plus the full BLAKE3 hex digest of
the Git administrative identity bytes. It does not include the branch name,
HEAD, dirty state, or absolute repository path. A branch can change without
changing stores, and moving the whole repository does not create an arbitrary
new ID. Metadata validation still forces a fresh index when absolute source
paths change.

V1 rejects a linked-worktree context when its source root, primary root, Git
common directory, or administrative identity cannot be represented losslessly
as UTF-8. The resolver returns a structured unsupported-path error before it
creates or opens a central store. NUL-delimited parsing still supports valid
UTF-8 paths containing spaces or newlines.

For a main worktree or a non-Git project, the resolver returns the current
store path unchanged. Failure to prove that a checkout is linked also falls
back to existing main/standalone behavior only when the target's local store is
the intended store; it must not guess a primary root and write elsewhere.

Every command and service that reads or writes Loomweave runtime state receives
`WorktreeContext`, `StorePaths`, `ConfigOrigin`, or an explicit leaf path.
Root-only path derivation must not remain on linked-worktree code paths.

| Surface | Required input after this change |
|---|---|
| `analyze`, run progress, writer and intent locks | `StorePaths` |
| MCP and HTTP reader/writer state | source root + `StorePaths` + readiness |
| Embeddings and semantic status | explicit embeddings database path |
| Diagnostics, instance ID, own port | explicit `StorePaths` leaves |
| `db`, `guidance`, `doctor`, hooks | resolved context and paths |
| Secret-scan baseline | explicit baseline path from `StorePaths` |
| MCP analysis status/cancel | explicit runs, database, and ownership state |
| LLM and semantic config get/set | `ConfigOrigin` + embeddings path |
| Filigree port and token lookup | ordered sibling lookup roots |
| Federation Loomweave port helpers | explicit own-port path |
| `install` store setup | effective store; assets still use source root |

Implementation must inventory every production root-derived path helper and
check it against this table. This includes direct `store_dir()`/`db_path()`
calls and wrappers such as `llm_traffic_log_path`, embedding
`open_in_store_dir`, instance/baseline helpers, and federation port
publish/read APIs. Production wrappers must accept `StorePaths`, a resolved
context, or an explicit leaf rather than a source root. Tests and fixtures may
keep low-level root helpers. A checked source audit fails on direct calls,
legacy wrapper calls, or literal `.weft/loomweave` joins outside the resolver
and test allowlists.

The low-level function that honors `weft.toml` remains useful for calculating a
repository store, but linked-worktree callers may not treat
`store_dir(source_root)` as their effective store.

### On-disk layout

The primary checkout keeps its existing layout:

```text
<repository-store>/
  loomweave.db
  embeddings.db
  instance_id
  ephemeral.port
  runs/
```

Linked worktrees use isolated subdirectories:

```text
<repository-store>/
  worktrees/
    owner.json
    gc.lock
    gc-state.json
    .trash/
    .quarantine/
    wt-<64 lowercase hex>/
      initializing.json   # present only during crash-consistent creation
      metadata.json
      metadata.lock
      activity.lock
      analysis-intent.lock
      analysis-intent.json
      loomweave.lock
      loomweave.db
      embeddings.db
      instance_id
      ephemeral.port
      runs/
      diagnostics/
```

Each worktree directory is a complete Loomweave store. Separate databases keep
SQLite's single-writer rule local to one worktree and let the main checkout and
multiple linked worktrees analyze concurrently.

`gc-state.json` only throttles repository-wide checks; it is not an index
registry. `gc.lock` serializes the short enumeration and cleanup operation.
Neither file participates in graph reads or analysis writes.

`gc-state.json` uses schema `loomweave.worktree-gc.v1` with
`last_attempt_at`, `last_success_at`, and `last_error` fields. Loomweave
replaces it atomically. An absent, malformed, or future-version file means
"check due" and never means "safe to delete."

### Store ownership and GC eligibility

Automatic garbage collection requires all of these repository-root facts:

- `repository_store` is exactly the canonical default
  `<primary-root>/.weft/loomweave` path;
- the `.weft`, `loomweave`, and `worktrees` path components are real
  directories, not symlinks;
- the canonical store remains beneath the canonical primary root;
- `worktrees/owner.json` has the supported schema and a valid random owner ID.

`owner.json` uses schema `loomweave.worktree-store-owner.v1` and records a
random 256-bit owner ID, the last-bound canonical Git common directory and
primary root, and creation time. The roots are audit fields, not deletion
authority. Loomweave creates the marker atomically before any other child when
the resolved repository store's `worktrees/` directory is absent or empty.
Every managed store and tombstone echoes the owner ID. A missing, malformed, or
unsupported marker beside existing content is never adopted: opening that
worktree namespace fails with a diagnostic. Creating a marker in an empty
configured override permits isolation but does not grant GC capability.

Namespace initialization precedes the normal `gc.lock` order because that lock
does not exist yet. After creating or opening the no-follow `worktrees/`
directory, a process opens the existing `owner.json` or, only when the directory
is empty, creates it with `create_new`. Every contender takes the file's
exclusive lock before reading or writing it and rechecks the directory while
locked. The first lock holder writes a new checksummed fixed-schema record only
when the file is empty/incomplete and remains the sole child, then flushes the
file and directory. Later contenders discard their generated IDs and validate
that completed winner. An incomplete unlocked marker is therefore recoverable
only while it is still the sole child. No process may create `gc.lock`, a
worktree directory, `.trash/`, or `.quarantine/` before a valid marker exists. A
crash after a complete marker write is recoverable: the next process validates
it, creates/opens `gc.lock`, and enters the normal lock order. Any malformed
marker beside another child makes the namespace unowned and unusable until
operator repair.

When a whole repository moves, the valid marker moves with its confined default
store. Under `gc.lock`, Loomweave may atomically refresh its last-bound audit
roots before quarantining old absolute-path indexes and creating fresh ones.
Copying the store also copies the explicit ownership marker, but cleanup still
cannot escape that copy's canonical default `.weft/loomweave/worktrees/`
boundary.

A configured `store_dir` override, a symlinked default store, a path escape, or
an ownership mismatch sets `gc_capability` to disabled with an operator-visible
reason. Analysis, serving, and per-worktree isolation continue to use that store
when its namespace is otherwise valid. Sanity checks may report candidates, but
v1 never renames, quarantines, or recursively deletes anything there.
Supporting explicit adoption of an external store for GC is deferred until
there is a separate operator-confirmed command and recovery design.

### Crash-consistent store creation

Effective-store creation runs under `gc.lock` and makes metadata authoritative
before any database, analysis intent, or service actor can start:

1. Create the stable directory without following symlinks.
2. Create and flush `initializing.json` with `create_new`; it contains the owner
   ID, stable ID, source/primary roots, and a random initialization nonce.
3. Write, flush, and atomically rename matching `metadata.json`, then flush the
   directory.
4. Create/acquire the shared activity lock, remove `initializing.json`, flush
   the directory again, and only then release `gc.lock`.

There are two narrowly recoverable crash shapes. An empty direct stable
directory may be treated as a crash between steps 1 and 2. A directory with a
valid matching initialization record may contain only the expected zero-length
lock files, `metadata.json`, and same-nonce metadata temporary file. Under
`gc.lock`, after proving no activity/intent/writer lock is held, a later process
may finish or restart those steps. Any database, sidecar, unknown file,
symlink, mismatched nonce, malformed record, or owner mismatch makes the partial
store unsafe: preserve it and fail with a diagnostic. A valid metadata file
plus a leftover matching initialization record is finalized, not quarantined.
No general "malformed metadata" recovery rule is introduced.

### Metadata contract

Each linked-worktree store has an atomically replaced `metadata.json`:

```json
{
  "schema": "loomweave.worktree-index.v1",
  "owner_id": "<64 lowercase hex>",
  "stable_id": "wt-<64 lowercase hex>",
  "git_admin_identity": "worktrees/<git-admin-name>",
  "source_root": "/absolute/path/to/linked-worktree",
  "primary_root": "/absolute/path/to/main-worktree",
  "created_at": "2026-07-18T00:00:00Z",
  "last_seen_at": "2026-07-18T00:00:00Z",
  "last_analyzed_commit": null,
  "last_completed_run_id": null,
  "orphan_candidate_since": null,
  "absence_confirmations": 0
}
```

Loomweave writes metadata through a same-directory temporary file, flushes it,
and renames it over the previous version. A short exclusive `metadata.lock`
serializes read-modify-write updates from analysis, serving, and garbage
collection. A successful analysis updates `last_analyzed_commit` and
`last_completed_run_id`; the database's `runs` table remains authoritative for
detailed status and statistics.

Before opening an existing store, Loomweave verifies the owner ID, stable ID,
administrative identity, source root, and primary root against the live Git
context. If the identity maps to different absolute roots, Loomweave moves the
old directory under `.quarantine/` and builds a fresh store, but only when the
repository root passes the ownership gate. Otherwise it fails with a diagnostic
and requires the operator to move the mismatched store manually. This handles a
Git worktree move, an administrative-name reuse at a different path, and a
repository move without serving graph rows whose absolute source paths belong
to another checkout.

Quarantine uses the same pinned-directory, no-follow, lock-order, pre-rename,
and post-rename identity checks as tombstoning and requires an exclusive
activity lock on the old store. If a server or analysis still has the old
identity open, the new context fails explicitly instead of moving, overwriting,
or serving that store.

The quarantine name is
`<stable-id>-<YYYYMMDDTHHMMSSZ>-<32 lowercase hex nonce>`. Its
`quarantine.json` records the owner ID, original store path, metadata digest,
reason, and quarantine time. V1 never automatically deletes `.quarantine/`; an
operator must inspect and remove those directories manually. Quarantine
therefore has its own confinement root and does not pretend to satisfy the
active-store deletion rule.

A same-identity, same-root reincarnation can reuse the store because it still
describes the same corpus location. Normal commit and staleness checks then
decide whether analysis is required. It cannot expose a different worktree's
absolute paths.

### Configuration and federation discovery

Configuration precedence for linked worktrees is:

1. an explicit `--config` path;
2. `<source-root>/loomweave.yaml`, when present;
3. `<primary-root>/loomweave.yaml`, when present;
4. built-in defaults, with `<primary-root>/loomweave.yaml` as the future write
   target.

This preserves branch-specific tracked configuration while allowing the
primary checkout's ignored local configuration to work from linked worktrees.
The repository store always honors the primary root's `weft.toml` override;
a linked worktree cannot redirect its central store with a second override.

The resolver records both the selected path and its origin. `ServerState` and
CLI config commands carry that `ConfigOrigin`; `llm_config_set` and
`semantic_config_set` update exactly that path. If no file existed at startup,
a linked-worktree setter creates the primary-root target, while a main or
standalone setter keeps its existing source-root target. A setter never creates
a new linked-worktree-local file merely because the server source root is
linked. Serve-spawned analysis receives the same resolved path after it exists.

All sibling local-state discovery uses one ordered, deduplicated lookup-root
list: source root first, primary root second. This applies to Filigree's
`ephemeral.port` and `federation_token` and to any future `.weft/<sibling>/`
sidecar. Environment and explicit-configuration rungs retain their existing
higher precedence. Loomweave's own port and instance ID use explicit
`StorePaths` leaves in the effective worktree store, so simultaneous servers
cannot overwrite one another's sidecars.

### Automatic first-serve bootstrap

Automatic bootstrap applies only to a proven linked worktree. Main and
standalone projects keep their existing no-index behavior.

```mermaid
sequenceDiagram
    participant Client as MCP client
    participant Serve as loomweave serve
    participant Resolver as WorktreeContext resolver
    participant Store as Isolated worktree store
    participant Analyze as Background loomweave analyze

    Client->>Serve: start server for linked source root
    Serve->>Resolver: resolve source, primary, and effective store
    Resolver-->>Serve: linked context with stable ID
    Serve->>Store: create directory and metadata
    Serve->>Store: reserve run ID before database initialization
    Serve->>Analyze: spawn analysis for linked source root
    Serve-->>Client: initialize succeeds; index state is initializing
    Analyze->>Store: acquire writer lock and migrate empty database
    Serve-->>Client: index state is building
    Client->>Serve: call graph tool
    Serve-->>Client: index-building error with run ID and progress
    Analyze->>Store: commit entities, edges, and completed run
    Analyze-->>Serve: report successful child exit
    Serve->>Store: activate readers/writers; index state is ready
    Client->>Serve: call graph tool again in same session
    Serve->>Store: query completed worktree graph
    Store-->>Client: return worktree-specific result
```

The startup sequence is:

1. Resolve and validate `WorktreeContext`.
2. Create the effective store and metadata when absent.
3. Reserve an analysis intent or attach to an existing intent before any
   database initialization.
4. Start the service with shared readiness and an absent `ActiveIndex` bundle.
5. The intent owner spawns the current executable as an analysis child. The
   child acquires the writer lock, creates and migrates the database, then
   performs analysis.
6. Monitor the child. Only after the database contains an authoritative
   completed run for that child may the service activate all configured
   database actors and mark the index ready.

The readiness state machine is:

```text
missing -> initializing -> building -> activating -> ready
                              \-----> build-failed -> initializing (retry)
activating -> activation-failed -> activating (retry)
ready -> stale -> refreshing -> ready
                     \-------> stale (refresh failed; prior graph retained)
```

`ready` and `stale` both have an authoritative completed run; stale data keeps
the existing query-with-warning behavior. `refreshing` also keeps the existing
reader and writer bundle: queries use the prior authoritative graph while
status exposes the active refresh run. A refresh failure returns to `stale` and
adds a warning rather than replacing usable data with `index-build-failed`.
`initializing` and `building` have no authoritative first run. `activating` has
a completed run but no usable service bundle. All three, plus their distinct
failure states, gate database-backed reads.

Every linked-worktree analysis entry point participates in one durable intent
protocol: direct `loomweave analyze`, `loomweave worktree analyze`, automatic
bootstrap, and MCP `analyze_start`. Under exclusive `analysis-intent.lock`, a
launcher either discovers an existing intent or atomically writes
`analysis-intent.json` with schema, run ID, random 128-bit nonce, launcher PID
plus process-start identity, creation time, lease expiry, and `pending` state.

A serve launcher writes the pending intent before it spawns the child and passes
the run ID and nonce. The child reacquires the intent lock, verifies the nonce,
acquires `loomweave.lock` non-blockingly, changes the intent to `active`, then
releases the intent lock while retaining the writer lock for database creation,
migration, and the whole analysis run. A direct analyzer performs the same
reservation and activation in-process. Any second launcher sees `pending` or
`active`: servers attach to its durable run ID; a manual CLI reports the active
run with `analyze-already-running` and exits 75 (`EX_TEMPFAIL`) rather than
competing.

The existing progress heartbeat acts as the renewable liveness signal after the
initial intent lease. On graceful success or failure the analyzer commits the
durable run result, releases the writer lock, then takes the intent lock and
atomically records the matching terminal state and finish time. A crash in that
short gap is reconciled from the durable run row before stale-intent recovery.

Cancellation is parent-owned because the current supervisor terminates the
analysis process group with SIGKILL. `RunHandle` therefore retains the intent
nonce. After the owning server kills and reaps the child, it acquires the intent
lock only after reconciling the child's wait status and the matching durable run
row. If that row already committed a terminal success or failure, the parent
publishes that semantic result; it must not overwrite completion merely because
the cancel raced the child's exit. Otherwise it atomically changes only the
matching pending/active nonce to `cancelled`, even when database creation or the
`runs` insert never happened. A guarded database terminal update is secondary
and best-effort when a matching non-terminal row exists; it is not the durable
cancellation source. An attached server has no matching handle/nonce authority
and returns `analyze-not-owned`.

The progress file retains detailed history; a later launcher may replace a
terminal intent with a newly reserved run under the intent lock. It may not
replace a pending or active nonce.

Spawn failure clears only a matching pending nonce. A process may reclaim a
pending or active intent only when its lease expired, `loomweave.lock` is
acquirable, no fresh progress heartbeat exists, and the recorded launcher
process-start identity is no longer live. An older Loomweave process that holds
only `loomweave.lock` remains protected: the new launcher waits for a run row or
progress record and attaches, or reports already-running; it does not classify
writer-lock contention as bootstrap failure. Barrier-driven tests pin the race
between manual reservation, child spawn, and writer-lock acquisition.

`ServerState` and the HTTP `AppState` own the same `Arc<IndexAccess>`.
`IndexAccess` contains readiness plus an optional `ActiveIndex` bundle: the
reader pool and every configured database-backed actor or sender. The service
can therefore initialize before the database exists.

The analysis child is the sole database process before activation. The serving
process creates no database actor during an authoritative-first-run bootstrap.
This includes `ReaderPool`, the MCP LLM writer, the HTTP Wardline writer,
embedding store connections, and any service helper that calls
`Connection::open` or `Writer::spawn`. Handlers resolve those dependencies from
`IndexAccess` only after the gate says ready; they do not retain eager clones in
separate state.

After the authoritative first run completes, the monitor constructs the whole
`ActiveIndex` bundle and publishes it together with `ready` in one synchronized
transition. A partial activation is torn down and reported as failed. If a
completed authoritative run exists, retry first attempts activation again and
does not launch a redundant analysis. Status and configuration tools use
context, metadata, intent, and progress state while no bundle exists; they may
not accidentally force the database open. `project_status_get` and
`analyze_status_get` stay available during bootstrap. Their database-derived
counts are `null`/unavailable until ready, never fabricated as zero.
Graph tools return the normal structured tool-error envelope with:

- `code: "index-building"`, `retryable: true`, run ID, phase, and progress while
  the child is active;
- `code: "index-build-failed"`, `retryable: true`, the recorded failure, and the
  exact fallback argv plus display command after an unsuccessful child exit.

The additive error detail contract is concrete rather than encoded in message
text. A building response contains:

```json
{
  "error": {
    "code": "index-building",
    "message": "The linked-worktree index is still building.",
    "retryable": true,
    "details": {
      "index_state": "building",
      "run_id": "<uuid>",
      "status": "running",
      "phase": "extracting",
      "processed_files": 12,
      "total_files": 40,
      "fallback_argv": null,
      "fallback_command": null
    }
  }
}
```

A build-failure response uses the same keys with `index_state: "failed"`, the
terminal run status, `code: "index-build-failed"`,
`failure_phase: "analysis"`, and the canonical analysis fallback as both a
structured `fallback_argv` and display-only `fallback_command`. An activation
failure uses `code: "index-activation-failed"`,
`failure_phase: "service-activation"`, and the exact failed component; it does
not recommend a redundant analysis. Existing tool-error fields remain
unchanged.

The readiness gate sits at shared service dispatch, not in individual query
implementations. Until the first authoritative run completes, every MCP tool
that reads or writes graph, guidance, summary, cache, or finding state returns
the readiness error. This gate does not apply to `refreshing`, which serves the
prior graph with a warning. The initial-building/failed tool inventory is exact:

- `project_status_get`, `analyze_status_get`, `llm_config_get`,
  `llm_config_set`, `semantic_config_get`, and `semantic_config_set` retain their
  existing policy visibility;
- `analyze_start` and `analyze_cancel` appear only when
  `serve.mcp.enable_write_tools` permits them;
- `analyze_start` returns the existing reserved run while building, retries
  service activation when an authoritative run exists, and creates a retry
  intent only when no authoritative run exists;
- `analyze_cancel` succeeds only when the active `ServerState` owns that child
  handle; an attached server returns `analyze-not-owned` and never signals a PID
  it did not spawn;
- every other database-backed tool returns `index-building`,
  `index-build-failed`, or `index-activation-failed`.

Automatic bootstrap does not depend on `serve.mcp.enable_write_tools`; that
flag continues to govern every client-initiated analysis or cancellation. V1
does not add cross-server cancellation or durable arbitrary-PID signalling.

MCP `resources/list` and prompt methods remain available during bootstrap.
`resources/read` for `loomweave://context` returns a readiness snapshot built
from context, metadata, intent, and progress, with database-derived fields null.
It does not read the empty database. Any future database-backed resource uses
the same structured readiness failure as tools.

The HTTP `/api/v1/_capabilities` route remains available, but every graph, catalogue,
finding, and write route checks `IndexAccess` before obtaining a reader or
writer. During bootstrap they return HTTP 503 with `INDEX_BUILDING`; after
analysis failure they return 503 with `INDEX_BUILD_FAILED`; after service
activation failure they return 503 with `INDEX_ACTIVATION_FAILED`. Payloads
carry the same run ID, retryability, progress/failure details, and applicable
fallback fields as MCP. The HTTP listener may bind and publish its
worktree-specific port while building, but it cannot return an authoritative
empty catalogue or start its Wardline writer early.

Neither service queries the empty database and returns a misleading empty
graph. When the child succeeds, the monitor changes shared readiness in memory;
the existing MCP connection and HTTP listener begin serving graph data without
a reconnect or rebind.

If a linked store exists but has no completed run, a new `serve` process
attempts bootstrap again. An existing authoritative run makes the service ready
immediately; ordinary index-staleness reporting still applies after that.

Automatic analysis preserves the existing untrusted-checkout boundary. The
parent captures the process environment before `serve` loads a repository
`.env`; every serve-spawned analysis uses `env_clear()` plus that captured
environment. Repository-controlled values loaded for MCP or LLM configuration
cannot leak into plugin subprocesses. The child also uses the normal secret
scan, plugin discovery, and confirmation policy. This safe spawn path applies
to both bootstrap and client-initiated `analyze_start`.

### Explicit analysis surface

The existing command becomes worktree-aware:

```text
loomweave analyze <linked-worktree-path>
```

It resolves the same central effective store and therefore works without a
local `.weft/loomweave/` directory.

Add a discoverable fallback command:

```text
loomweave worktree analyze <name-or-path>
```

An existing path must resolve to a registered linked worktree. A non-path name
matches an exact worktree path basename or exact Git administrative basename.
Zero matches fail with the registered choices; multiple matches fail as
ambiguous. The command never selects by fuzzy branch matching. It creates the
central store if needed and delegates to the normal analysis pipeline.

An automatic-bootstrap failure reports the canonical path form of this command
without resolving a name. The machine contract is an argument vector:

```json
["loomweave", "worktree", "analyze", "--", "/canonical/worktree/path"]
```

`--` prevents a path beginning with `-` from becoming an option. The optional
`fallback_command` string is display text rendered with platform-correct shell
escaping and is never reparsed by Loomweave. JSON escaping preserves newlines;
clients should execute `fallback_argv` directly.

### Activity and writer locks

Each worktree retains the existing exclusive `loomweave.lock` for analysis, so
two analyses cannot write the same database concurrently.

Add three coordination locks:

- a server holds a shared activity lock for its lifetime;
- an analysis holds a shared activity lock for its lifetime;
- garbage collection must acquire the activity lock exclusively before it can
  rename a store;
- `analysis-intent.lock` serializes durable run reservation and activation;
- `metadata.lock` serializes short metadata read-modify-write operations.

Multiple readers and one analysis may therefore coexist as they do today, but
cleanup cannot rename a database that any Loomweave process is using. The lock
order is fixed:

- Open/create store: `gc` -> shared `activity`.
- Reserve/activate analysis: shared `activity` -> `intent` -> `writer`.
- Update metadata: shared `activity` -> `metadata`.
- Mark candidate: `gc` -> `metadata`.
- Tombstone/quarantine: `gc` -> exclusive `activity` -> `intent` -> `writer`
  -> `metadata`.
- Delete tombstone: `gc` -> trash-path validation.

`gc` means the repository `gc.lock`; `intent` is `analysis-intent.lock`; and
`writer` is `loomweave.lock`. Store open/create takes `gc.lock`, validates or
creates the directory, acquires shared activity protection, then releases
`gc.lock`. Normal serving and analysis never request `gc.lock` while holding an
activity, intent, writer, or metadata lock. Their cleanup workers run as
separate operations. An analysis schedules cleanup only after releasing every
per-store lock. A server trigger launches a short-lived cleanup helper with
close-on-exec lock descriptors; the helper owns no activity lock and skips the
parent server's active store when its exclusive activity probe fails. The
helper is spawned with `env_clear()` from the pre-dotenv trusted launch
allowlist and receives the absolute Git executable explicitly; it never
inherits the server's repository-loaded environment.

Cleanup takes every per-store lock non-blockingly. Failure to acquire one skips
the candidate. It never waits for activity while holding metadata, and an
analyzer cannot hold intent without already holding shared activity. These
rules prevent both server/cleanup deadlocks and the reservation race identified
in review.

### Periodic worktree sanity checks

Garbage collection has three triggers:

- every analysis schedules one check after completing and releasing its store
  locks;
- `serve` startup runs a check when the last successful repository check is at
  least six hours old;
- a long-running server schedules another check every six hours.

The analysis-triggered check is not throttled by the six-hour age, but every
trigger uses a non-blocking repository `gc.lock`. If another process is already
checking, the caller skips cleanup. Cleanup failure is logged and exposed in
status diagnostics but never changes the current analysis or MCP startup
result.

Each check runs `git worktree list --porcelain -z` through the context-bound
hardened Git runner against the common Git directory. An enumeration or parse
error aborts the entire cleanup pass: no candidate is marked, renamed, or
deleted. When `gc_capability` is disabled, the check reports what it would have
considered but performs no lifecycle mutation.

The candidate-to-tombstone phase is:

1. A present, unlocked worktree refreshes `last_seen_at` and clears orphan
   fields.
2. A Git-locked worktree is present and protected even if its filesystem path
   is temporarily unavailable.
3. A missing or Git-prunable worktree becomes an orphan candidate on the first
   successful check. Loomweave records `orphan_candidate_since` and one
   confirmation but keeps every file.
4. A later successful enumeration must still find it absent, must occur at
   least 24 hours after the first confirmation, and increments the confirmation
   count.
5. Only then may cleanup attempt the tombstone protocol. It does not recursively
   delete the active store path.

The main store is never a candidate. A worktree that reappears during the grace
period returns to active state without data loss. Quarantined stores are never
part of automatic garbage collection.

### Tombstone protocol

After the second absence confirmation, cleanup performs these steps while it
holds the owned repository's `gc.lock`:

1. Open the canonical `worktrees/` and `.trash/` directories without following
   symlinks and pin directory handles. Capture the candidate directory's
   no-follow device/inode identity, owner-marker digest, and metadata digest.
2. Acquire exclusive activity, intent, writer, and metadata locks in the fixed
   order.
3. Re-run hardened Git enumeration and require the same identity to remain
   absent or prunable and unlocked.
4. Re-read the owner marker, metadata, and no-follow filesystem identity. Any
   change skips the candidate.
5. Use a handle-relative atomic rename to move the whole directory on the same
   filesystem to
   `.trash/<stable-id>-<YYYYMMDDTHHMMSSZ>-<32 lowercase hex nonce>`.
6. Open the renamed directory without following symlinks and require its
   device/inode identity to match step 1. On a mismatch, restore it when safe
   and preserve it unmodified with a diagnostic.
7. Write `tombstone.json` with the owner ID, original device/inode and metadata
   digests, absence evidence, and rename time. Release the per-store locks.

Store creation and opening also hold `gc.lock`. They can never open a directory
while cleanup is renaming it. If Git recreates the worktree around the final
enumeration, the old directory is at worst moved to trash while no Loomweave
process has it open; a subsequent open creates a fresh active store or restores
the tombstone when its metadata roots still match. It never treats a trash path
as the active store.

Recursive deletion applies only to a tombstone, never to
`worktrees/<stable-id>`. A later successful GC pass must still see the identity
absent, must occur at least 24 hours after the tombstone rename, and must
revalidate the owner marker, tombstone record, no-follow path identity, and
direct-child confinement under `.trash/`. A reappeared identity preserves or
restores the tombstone. Recursive traversal is rooted at the pinned `.trash/`
directory handle, never at a re-resolved string path, and refuses symlinks at
every level. It also refuses mount-boundary crossings: on Linux it uses
`openat2` `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV` or verifies
equivalent mount IDs; on Windows it rejects every reparse point and junction.
A platform without race-resistant, handle-relative, no-cross-mount traversal
disables automatic deletion. Failed removal leaves the tombstone for a later
pass.

### Deletion safety

The cleanup boundary is fail-closed. Both active-store rename and tombstone
deletion require:

- The repository uses the canonical default store and has a matching ownership
  marker.
- Git enumeration completed successfully.
- The target is not the main store and is absent or prunable, not Git-locked.
- No candidate path component is a symlink and canonicalization cannot escape
  the worktree-store root.
- Device/inode identity and metadata digest match both the scan and the final
  pre-rename or pre-delete validation.
- Destructive filesystem operations are relative to pinned, no-follow directory
  handles; string-prefix checks alone never authorize them.
- Recursive traversal cannot cross a filesystem mount, bind mount, junction, or
  reparse point beneath the tombstone.

Before renaming an active store, Loomweave additionally requires:

- The direct-child directory name matches exactly `wt-[0-9a-f]{64}`.
- Metadata parses, uses the supported schema, and agrees with that stable ID and
  the marker's owner ID.
- The two absence confirmations and first 24-hour grace period are satisfied.
- The activity, intent, writer, and metadata locks prove that no server or
  analysis is using the store.

Before recursively deleting a tombstone, Loomweave additionally requires:

- The direct-child trash name matches exactly
  `wt-[0-9a-f]{64}-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{32}`.
- `tombstone.json` parses, uses the supported schema, and matches the name's
  stable-ID prefix, timestamp, nonce, marker owner ID, metadata digest, and
  captured device/inode.
- The later successful absence check and second 24-hour recovery window are
  satisfied.

Unknown files, malformed metadata, unsupported metadata versions, path
mismatches, lock errors, and permission errors all preserve the directory and
produce a diagnostic. Cleanup never follows a stored source path and never
recursively deletes outside the owned repository store's `.trash/` subtree.

## Failure Handling

- **Target is not a Git repository:** Preserve existing standalone store
  behavior. Install and analyze as today.
- **Git context is ambiguous:** Do not create a central worktree store. Fix Git
  metadata or pass a valid registered path.
- **Trusted Git cannot be resolved, exits non-zero, or exceeds an output cap:**
  Do not fall back to inherited `PATH` or repository-selector environment. Make
  no central-store or cleanup change and report the probe failure.
- **A required Git path is not lossless UTF-8:** Return the structured
  unsupported-path error before creating or opening a central worktree store.
- **An analysis intent already exists:** Servers attach to its run ID. Manual
  analysis reports `analyze-already-running` and exits 75 (`EX_TEMPFAIL`); it
  does not wait while holding a coordination lock or start a second writer.
- **Database creation or migration fails:** Keep MCP in degraded mode with the
  exact error and without a reader pool. HTTP database routes return the same
  failed-readiness detail as 503. Fix the permission or schema issue and run
  the fallback command.
- **Background analysis is still running:** Return `index-building`; never query
  empty graph tables. Retry the graph tool or inspect analysis status.
- **Analysis fails or finds no usable plugins:** Return `index-build-failed`
  with run diagnostics. Fix the cause and run
  `loomweave worktree analyze '<canonical-path>'`.
- **A refresh fails after an authoritative run:** Keep serving the prior graph as
  stale and report the refresh failure; do not enter first-build failure mode.
- **A client tries to cancel an attached analysis:** Return
  `analyze-not-owned`. Only the server process that spawned the child may cancel
  it, and only when MCP write tools are enabled.
- **The owning server cancels an initial build or refresh:** Reconcile natural
  completion first. A true initial cancellation enters failed readiness with a
  cancelled terminal intent; a true refresh cancellation returns to the prior
  stale graph with a warning.
- **Metadata disagrees with live roots:** In an owned default store, quarantine
  the old directory and build fresh. In an unowned or relocated store, make no
  lifecycle change and require manual repair.
- **The worktree namespace has non-empty content but no valid owner marker:**
  Refuse to adopt or open it. Preserve all content and require operator repair.
- **Store creation was interrupted:** Resume only the exact empty or valid
  initialization-record shapes. Preserve every other partial directory and
  require operator repair.
- **The namespace marker is valid but GC ownership or confinement cannot be
  proved:** Continue analysis and serving from the resolved store, but disable
  all automatic rename, quarantine, and deletion operations with a diagnostic.
- **Git enumeration fails during cleanup:** Make no lifecycle changes. Retry
  after Git or the filesystem recovers.
- **Worktree disappears once:** Mark an orphan candidate only. The 24-hour grace
  period starts.
- **Worktree reappears during grace:** Clear the candidate state without data
  loss.
- **Worktree reappears after tombstoning:** Preserve or restore the tombstone;
  never recursively delete it as absent evidence.
- **Candidate is active, locked, malformed, or unsafe:** Preserve it and emit a
  diagnostic. Require operator repair or process shutdown.
- **Legacy local linked-worktree store exists:** Ignore it, warn in `doctor`,
  and use the central store. The operator may remove it after verifying the
  central index.

## Alternatives Considered

### One shared database partitioned by worktree ID

Rejected. Worktree identity would have to enter the primary key or query
predicate of nearly every graph, edge, finding, tag, FTS, run, cache, and SEI
table. Missing one scope predicate could leak another worktree's graph. All
analyses would also contend for one SQLite writer.

### One database inside each linked worktree

Rejected. It requires per-worktree installation of ignored runtime directories,
and the database disappears with the source path before Loomweave can inspect or
clean it. It also leaves agent clients dependent on ignored checkout-local
configuration.

### Seed from the main database

Deferred. Existing entities and incremental-analysis records contain canonical
absolute paths. Copying the main database would initially expose the wrong
checkout and require a comprehensive path rewrite. V1 starts with a fresh
analysis. Portable, repository-relative source identity can enable safe seeding
later.

### Garbage-collect configured external stores

Deferred. A `store_dir` override can be absolute, relative to configuration,
shared by repositories, or administered outside the checkout. The repository
does not have enough authority to infer that every child is disposable. V1
supports isolated worktree indexes in that store but limits lifecycle checks to
diagnostics. A future adoption command needs an explicit ownership marker,
recovery contract, and operator confirmation.

## Compatibility and Rollout

- Main and standalone store paths remain byte-for-byte unchanged.
- Existing main databases require only normal schema migrations, if any new
  readiness metadata is persisted in the database.
- Linked worktree stores are additive under `<repository-store>/worktrees/`.
- Linked worktree isolation also works under a configured store override, but
  automatic quarantine and garbage collection remain disabled there.
- `StorePaths`, `ConfigOrigin`, `IndexAccess`, and the sibling lookup roots are
  internal typed boundaries. Their rollout includes migrating and auditing all
  production root-derived store consumers before enabling linked stores.
- A manually installed linked-worktree database is neither migrated nor
  deleted. `doctor` reports it as legacy/stranded while pointing at the central
  effective store.
- Global MCP registration that launches `loomweave serve --path .` works
  automatically because runtime CWD identifies the linked source root.
- Clients that rely only on checkout-local `.mcp.json` still need an explicit
  agent-asset install in that worktree. This design does not make an absent
  registration start a server.
- The status response adds worktree kind, source root, effective store, stable
  ID, readiness, bootstrap run ID, and last cleanup diagnostic. Existing fields
  remain compatible.
- HTTP clients gain additive 503 `INDEX_BUILDING` and `INDEX_BUILD_FAILED`
  responses where an unindexed worktree previously could return an empty
  result. The existing `/api/v1/_capabilities` route remains available.
- Configuration setters continue updating the file whose values are active;
  the only new default is that a linked worktree with no file creates the
  primary-root configuration instead of a checkout-local shadow file.

## Verification Strategy

### Resolver and storage tests

- Create real temporary Git repositories with a main worktree and multiple
  linked worktrees.
- Verify main, standalone, linked, moved, locked, prunable, and malformed Git
  contexts.
- Verify stable IDs do not change with branch, HEAD, dirty state, or repository
  relocation, while root metadata mismatches force a fresh store.
- Exercise hostile repository configuration and environment input while
  proving every `rev-parse` and `worktree list` probe uses the pre-dotenv trusted
  Git executable and context-bound hardened runner. Include a repository `.env`
  with a fake `PATH`, `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, and
  `GIT_CONFIG_*`; no fake Git, redirected repository, hook, pager, or filesystem
  monitor may run.
- Reject non-zero Git exits, concurrently streamed output beyond either cap,
  truncated output, malformed NUL records, duplicate singleton fields, and
  non-UTF-8 required paths before store creation. Verify the child is killed and
  reaped on overflow rather than collected unboundedly.
- Verify primary `weft.toml` store overrides and configuration precedence;
  override stores isolate worktrees but never gain automatic GC capability.
- Verify the default owner marker, symlink and escape rejection, mismatched or
  missing ownership behavior, and refusal to adopt non-empty content.
- Barrier-race two initializers in an empty worktree namespace and assert one
  owner ID wins before any process creates `gc.lock` or a store. Kill the winner
  during marker and effective-store metadata creation and verify only the exact
  safe-empty/initialization-record shapes recover; unknown content is preserved.
- Verify every row in the runtime path-consumer matrix, including embeddings,
  secret baselines, instance and port files, install setup, diagnostics, hooks,
  and analysis status, uses `StorePaths` rather than the linked source root.
- Run the checked production-callsite audit for direct path helpers, legacy
  root-taking wrappers, and literal store joins.

### Isolation and concurrency tests

- Put divergent functions in main and two linked worktrees, including
  uncommitted edits, and analyze all three.
- Assert three distinct database paths and graph results specific to each
  checkout.
- Run analyses for different worktrees concurrently and assert no shared writer
  conflict.
- Run a server and analysis for one worktree while another worktree is being
  garbage-collected.
- Select a linked-worktree configuration file, mutate configuration through MCP,
  and assert the exact selected file changes. With no file, assert creation at
  the primary root.
- Put Filigree port and token sidecars only at the primary root and verify a
  linked server discovers both after checking its source root.

### Bootstrap and MCP tests

- Start `serve` in a linked worktree with no index and complete JSON-RPC
  initialization.
- Observe `project_status_get` reporting `building` and graph tools returning
  `index-building` with a run ID.
- Assert no reader pool, MCP LLM writer, HTTP Wardline writer, embedding
  connection, or other database actor opens before the authoritative run, and
  assert configured actors activate together with readiness.
- Read `loomweave://context` while building and verify readiness with null
  database counts; no resource path may query the empty database.
- Wait for analysis, then query the new graph successfully on the same MCP
  connection.
- Cover child spawn failure, migration failure, missing plugins, failed run,
  process restart during bootstrap, and a subsequent successful retry.
- Start two servers against the same empty store and verify one coordinates the
  analysis while both sessions observe the same readiness transition.
- Use barriers at reservation, spawn, activation, and writer-lock acquisition to
  race direct analysis, `worktree analyze`, two servers, and MCP
  `analyze_start`; assert one durable run ID and one writer in every ordering.
- Kill the coordinator in pending and active states. Verify a live process is
  never reclaimed and an expired, dead intent is reclaimed only after the
  lease, heartbeat, and writer-lock checks all agree.
- Verify an attached server cannot cancel a foreign child, a child-owning server
  can cancel only with write tools enabled, and read-only tool inventory never
  exposes client analysis or cancellation.
- Cancel before database creation and assert the parent terminalizes the intent.
  Barrier-race cancellation against a committed terminal run and assert semantic
  success/failure wins over `cancelled`.
- Verify fallback argv for paths beginning with `-` and containing spaces,
  quotes, shell metacharacters, and newlines. Execute the argv directly and
  treat the rendered command as display-only.
- Verify an existing completed index starts ready and an existing incomplete
  index re-enters bootstrap.
- Refresh a ready/stale index while querying and keep serving the prior graph
  with status/warnings. Fail the refresh and verify the prior graph remains
  available as stale.

### HTTP readiness tests

- Start HTTP and MCP together before the database exists and assert both report
  the same initializing/building run and progress.
- Assert `/api/v1/_capabilities` remains available while every database-backed HTTP
  route returns 503 before readiness and after terminal bootstrap failure.
- Complete analysis and verify the existing listener and MCP connection both
  serve the worktree graph without reconnecting or rebinding.

### Cleanup tests

- Use a fake clock with real worktree directories to test the six-hour cadence,
  first absence, less-than-24-hour recheck, second confirmation, atomic
  tombstone rename, another 24-hour recovery window, and final tombstone
  deletion.
- Restore a worktree during the grace period and verify metadata resets.
- Recreate a worktree between final Git enumeration and rename and assert the
  old store is never recursively deleted from the active path.
- Swap a candidate at the rename boundary and verify post-rename device/inode
  validation restores or preserves it without writing a valid tombstone.
- Recreate a worktree after tombstoning and verify the tombstone is preserved or
  restored.
- Verify Git enumeration failure, Git locks, shared activity locks, analyze
  and intent locks, malformed metadata, unsupported schema, device/inode or
  digest changes, identity reuse, symlinked candidates, path traversal, and
  permission errors all preserve data.
- Verify relocated, unowned, symlinked, and ownership-mismatched repository
  stores can report candidates but cannot rename, quarantine, or delete them.
- Verify quarantines are excluded from periodic deletion and only direct,
  validated children of the owned `.trash/` root can be recursively removed.
- Verify a simulated platform without safe handle-relative traversal disables
  recursive deletion and leaves the tombstone intact.
- Present a nested mount/bind mount or filesystem-abstraction equivalent and
  verify no-cross-mount traversal preserves the entire tombstone. Reject Windows
  junction/reparse-point equivalents.
- Verify concurrent servers serialize cleanup with `gc.lock` and a cleanup
  error cannot fail analysis or startup.

### Release gates

- Pin CLI help and structured MCP error-code contracts.
- Run Rust formatting, Clippy, and the complete workspace test suite.
- Run Python formatting, lint, type, and test gates used by the repository.
- Run `wardline scan . --fail-on ERROR` because Git output, filesystem paths,
  metadata, and deletion are trust boundaries.
- Dogfood against the repository's real linked worktree: first-serve bootstrap,
  divergent graph query, explicit analysis, worktree removal, grace-period
  observation, and eventual cleanup.

## Acceptance Criteria

1. Starting Loomweave from a linked worktree never opens the main checkout's
   graph database as that worktree's graph.
2. Main and each linked worktree have independent database and sidecar paths.
3. A missing linked index bootstraps automatically during `serve`, reports
   structured progress, and becomes queryable without reconnecting.
4. MCP tools/resources and HTTP database-backed surfaces cannot return an
   authoritative empty result before the first analysis completes; they expose
   the same readiness, and no database actor opens early.
5. `loomweave analyze <path>` and
   `loomweave worktree analyze <name-or-path>` both build the central linked
   index.
6. Different worktrees can analyze concurrently; every analysis entry point
   shares one durable reservation and cannot duplicate a run for the same
   worktree. Cancellation terminalizes the matching intent without overwriting
   a naturally completed run.
7. Every analysis triggers a fail-soft sanity check; server startup and
   long-running servers check at the six-hour cadence.
8. Two successful absence confirmations at least 24 hours apart permit only an
   atomic tombstone rename. Recursive deletion requires a later successful
   check and another 24-hour recovery window.
9. Only a direct child of an owned canonical default store can be tombstoned,
   and only a validated direct child of its `.trash/` root can be recursively
   deleted.
10. Git-locked, active, malformed, ambiguous, unsafe, quarantined, unowned,
    relocated, nested-mount, and main stores are never deleted automatically.
11. Every worktree resolver and cleanup Git probe uses the pre-dotenv trusted
    executable, a selector-free hardened environment, and bounded streaming;
    unsupported path encodings fail before central-store creation.
12. Configuration writes target the selected origin and sibling credential
    discovery checks source then primary roots.
13. Existing main-checkout users see no store-path or command regression.
14. Refreshing or failing to refresh an existing index keeps its prior
    authoritative graph available with staleness status and warnings.
