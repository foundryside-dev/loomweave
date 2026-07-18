# Worktree-Scoped Loomweave Indexes Design

**Date:** 2026-07-18

**Status:** Design approved; implementation pending

**Tracker:** `clarion-c297efc752`

**Decision:** Loomweave will keep the main checkout's store unchanged and give
each linked Git worktree a separate SQLite store under the primary checkout's
Loomweave store. `serve` will create and analyze a missing linked-worktree index
in the background. Loomweave will also check registered worktrees periodically
and remove an abandoned index only after two successful absence checks at least
24 hours apart.

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
  kind                    standalone | main | linked
  git_common_dir          canonical common Git directory when available
  git_admin_identity      relative linked-worktree administrative path
  stable_id               wt-<full BLAKE3 hex digest> for linked worktrees
```

The resolver invokes Git directly with argument arrays, never through a shell.
It uses absolute `git rev-parse` results and NUL-delimited
`git worktree list --porcelain -z` output. It identifies the primary worktree by
resolving listed worktrees and selecting the entry whose Git directory equals
the common Git directory; it does not assume that a branch name or directory
name is unique.

For a linked worktree, the stable ID is `wt-` plus the full BLAKE3 hex digest of
the Git administrative identity bytes. It does not include the branch name,
HEAD, dirty state, or absolute repository path. A branch can change without
changing stores, and moving the whole repository does not create an arbitrary
new ID. Metadata validation still forces a fresh index when absolute source
paths change.

For a main worktree or a non-Git project, the resolver returns the current
store path unchanged. Failure to prove that a checkout is linked also falls
back to existing main/standalone behavior only when the target's local store is
the intended store; it must not guess a primary root and write elsewhere.

Every command and service that reads or writes Loomweave runtime state receives
the resolved context or its explicit effective-store path. Root-only path
derivation must not remain on linked-worktree code paths. This includes:

- analysis database, run-progress files, and analyze locks;
- MCP and HTTP reader pools;
- embeddings, diagnostics, instance ID, and Loomweave's ephemeral port;
- `doctor`, `db`, `guidance`, hooks, and MCP-triggered analysis;
- index freshness and status reads.

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
    gc.lock
    gc-state.json
    wt-<64 lowercase hex>/
      metadata.json
      metadata.lock
      activity.lock
      bootstrap.lock
      loomweave.lock
      loomweave.db
      embeddings.db
      instance_id
      ephemeral.port
      runs/
      diagnostics/
    .quarantine/
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

### Metadata contract

Each linked-worktree store has an atomically replaced `metadata.json`:

```json
{
  "schema": "loomweave.worktree-index.v1",
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

Before opening an existing store, Loomweave verifies the stable ID,
administrative identity, source root, and primary root against the live Git
context. If the identity maps to different absolute roots, Loomweave moves the
old directory under `.quarantine/` and builds a fresh store. This handles a
Git worktree move, an administrative-name reuse at a different path, and a
repository move without serving graph rows whose absolute source paths belong
to another checkout.

Quarantine uses the same confinement checks as deletion and requires an
exclusive activity lock on the old store. If a server or analysis still has the
old identity open, the new context fails explicitly instead of moving,
overwriting, or serving that store.

A same-identity, same-root reincarnation can reuse the store because it still
describes the same corpus location. Normal commit and staleness checks then
decide whether analysis is required. It cannot expose a different worktree's
absolute paths.

### Configuration and federation discovery

Configuration precedence for linked worktrees is:

1. an explicit `--config` path;
2. `<source-root>/loomweave.yaml`, when present;
3. `<primary-root>/loomweave.yaml`, when present;
4. built-in defaults.

This preserves branch-specific tracked configuration while allowing the
primary checkout's ignored local configuration to work from linked worktrees.
The repository store always honors the primary root's `weft.toml` override;
a linked worktree cannot redirect its central store with a second override.

Sibling ephemeral-port discovery checks the source root first and the primary
root second. Loomweave's own port and instance ID remain in the effective
worktree store, so simultaneous servers cannot overwrite one another's
sidecars.

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
    Serve->>Store: create metadata and migrated empty database
    Serve->>Analyze: spawn analysis for linked source root
    Serve-->>Client: initialize succeeds; index state is building
    Client->>Serve: call graph tool
    Serve-->>Client: index-building error with run ID and progress
    Analyze->>Store: commit entities, edges, and completed run
    Analyze-->>Serve: report successful child exit
    Client->>Serve: call graph tool again in same session
    Serve->>Store: query completed worktree graph
    Store-->>Client: return worktree-specific result
```

The startup sequence is:

1. Resolve and validate `WorktreeContext`.
2. Create the effective store and metadata when absent.
3. Create and migrate an empty database under the normal analyze lock.
4. Open the reader pool and start the MCP session.
5. Attach to a live analysis, or spawn the current executable as an analysis
   child for the source root, reusing the existing progress-file and run-ID
   mechanisms.
6. Monitor the child. Mark the index ready only after the database contains an
   authoritative completed run for that child.

`bootstrap.lock` makes one server the bootstrap coordinator. Before spawning,
the coordinator checks the latest run and progress heartbeat. If another server
or a manual command already started a live analysis, it monitors that run
instead of launching another. A server that cannot acquire `bootstrap.lock`
also monitors database and progress state. If the coordinator dies, the OS
releases the lock; a later server either attaches to the still-live analysis or
starts a new one after the old run is demonstrably stale. Two servers starting
against the same empty store therefore cannot race into two intentional
bootstrap analyses.

Opening the empty database before serving lets the normal `ServerState` and
reader pool remain alive. A shared readiness state gates graph-backed tools.
`project_status_get` and `analyze_status_get` stay available during bootstrap.
Graph tools return the normal structured tool-error envelope with:

- `code: "index-building"`, `retryable: true`, run ID, phase, and progress while
  the child is active;
- `code: "index-build-failed"`, `retryable: true`, the recorded failure, and the
  exact fallback command after an unsuccessful child exit.

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
      "fallback_command": null
    }
  }
}
```

A failed response uses the same keys with `index_state: "failed"`, the terminal
run status, and a non-null canonical fallback command. Existing tool-error
fields remain unchanged.

The readiness gate sits at shared MCP dispatch, not in individual query
implementations. Until the first authoritative run completes, every tool that
reads or writes graph, guidance, summary, cache, or finding state returns the
readiness error. `project_status_get`, `analyze_status_get`, and
`analyze_cancel` remain callable. `analyze_start` returns or attaches to the
existing bootstrap run instead of spawning a competitor. Automatic bootstrap
does not depend on `serve.mcp.enable_write_tools`; that flag continues to govern
client-initiated write tools.

They never query the empty database and return a misleading empty graph. When
the child succeeds, the monitor changes readiness in memory and the existing
MCP connection begins serving graph tools without a reconnect.

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
so the operator can copy it without resolving a name.

### Activity and writer locks

Each worktree retains the existing exclusive `loomweave.lock` for analysis, so
two analyses cannot write the same database concurrently.

Add `activity.lock` for lifecycle protection:

- a server holds a shared activity lock for its lifetime;
- an analysis holds a shared activity lock for its lifetime;
- garbage collection must acquire the activity lock exclusively before it can
  rename or delete a store.

Multiple readers and one analysis may therefore coexist as they do today, but
cleanup cannot remove a database that any Loomweave process is using. Garbage
collection also verifies that the analyze lock is acquirable before deletion.

The cleanup lock order is repository `gc.lock`, non-blocking exclusive
`activity.lock`, then `metadata.lock`. The serving and analysis critical paths
take a shared activity lock and use `metadata.lock` only for a short atomic
update. Their separately scheduled cleanup workers may acquire `gc.lock`, but
never while holding a store activity or metadata lock. Cleanup never waits for
activity while holding metadata, which prevents a server/cleanup deadlock.

### Periodic worktree sanity checks

Garbage collection has three triggers:

- every analysis schedules one check after resolving its current context;
- `serve` startup runs a check when the last successful repository check is at
  least six hours old;
- a long-running server schedules another check every six hours.

The analysis-triggered check is not throttled by the six-hour age, but every
trigger uses a non-blocking repository `gc.lock`. If another process is already
checking, the caller skips cleanup. Cleanup failure is logged and exposed in
status diagnostics but never changes the current analysis or MCP startup
result.

Each check runs `git worktree list --porcelain -z` against the common Git
directory. An enumeration error aborts the entire cleanup pass: no candidate is
marked and no directory is deleted.

The two-phase lifecycle is:

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
5. Only then may cleanup acquire exclusive activity protection and remove the
   isolated store.

The main store is never a candidate. A worktree that reappears during the grace
period returns to active state without data loss. Quarantined stores follow the
same minimum 24-hour, two-confirmation deletion rule.

### Deletion safety

The cleanup boundary is fail-closed. Before renaming or deleting anything,
Loomweave must prove all of these conditions:

- Git enumeration completed successfully.
- The target is not the main store and is absent or prunable, not Git-locked.
- Metadata parses, uses the supported schema, and agrees with the directory's
  stable ID.
- The directory name matches `wt-[0-9a-f]{64}`.
- The candidate is a direct, non-symlink child of the canonical worktree-store
  root.
- No candidate path component is a symlink and canonicalization cannot escape
  the worktree-store root.
- The two absence confirmations and 24-hour grace period are satisfied.
- The activity and analyze locks prove that no server or analysis is using the
  store.

Unknown files, malformed metadata, unsupported metadata versions, path
mismatches, lock errors, and permission errors all preserve the directory and
produce a diagnostic. Cleanup never follows a stored source path and never
deletes outside the repository store's `worktrees/` subtree.

## Failure Handling

- **Target is not a Git repository:** Preserve existing standalone store
  behavior. Install and analyze as today.
- **Git context is ambiguous:** Do not create a central worktree store. Fix Git
  metadata or pass a valid registered path.
- **Database creation or migration fails:** Keep MCP in degraded mode with the
  exact error. Fix the permission or schema issue and run the fallback command.
- **Background analysis is still running:** Return `index-building`; never query
  empty graph tables. Retry the graph tool or inspect analysis status.
- **Analysis fails or finds no usable plugins:** Return `index-build-failed`
  with run diagnostics. Fix the cause and run
  `loomweave worktree analyze '<canonical-path>'`.
- **Metadata disagrees with live roots:** Quarantine the old store and build
  fresh. Keep the quarantine available for manual inspection.
- **Git enumeration fails during cleanup:** Make no lifecycle changes. Retry
  after Git or the filesystem recovers.
- **Worktree disappears once:** Mark an orphan candidate only. The 24-hour grace
  period starts.
- **Worktree reappears during grace:** Clear the candidate state without data
  loss.
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

## Compatibility and Rollout

- Main and standalone store paths remain byte-for-byte unchanged.
- Existing main databases require only normal schema migrations, if any new
  readiness metadata is persisted in the database.
- Linked worktree stores are additive under `<repository-store>/worktrees/`.
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

## Verification Strategy

### Resolver and storage tests

- Create real temporary Git repositories with a main worktree and multiple
  linked worktrees.
- Verify main, standalone, linked, moved, locked, prunable, and malformed Git
  contexts.
- Verify stable IDs do not change with branch, HEAD, dirty state, or repository
  relocation, while root metadata mismatches force a fresh store.
- Verify primary `weft.toml` store overrides and configuration precedence.
- Verify every runtime sidecar uses the effective store rather than the linked
  source root.

### Isolation and concurrency tests

- Put divergent functions in main and two linked worktrees, including
  uncommitted edits, and analyze all three.
- Assert three distinct database paths and graph results specific to each
  checkout.
- Run analyses for different worktrees concurrently and assert no shared writer
  conflict.
- Run a server and analysis for one worktree while another worktree is being
  garbage-collected.

### Bootstrap and MCP tests

- Start `serve` in a linked worktree with no index and complete JSON-RPC
  initialization.
- Observe `project_status_get` reporting `building` and graph tools returning
  `index-building` with a run ID.
- Wait for analysis, then query the new graph successfully on the same MCP
  connection.
- Cover child spawn failure, migration failure, missing plugins, failed run,
  process restart during bootstrap, and a subsequent successful retry.
- Start two servers against the same empty store and verify one coordinates the
  analysis while both sessions observe the same readiness transition.
- Start a manual analysis during server bootstrap and verify the server attaches
  to that active run instead of spawning a competing child.
- Verify an existing completed index starts ready and an existing incomplete
  index re-enters bootstrap.

### Cleanup tests

- Use a fake clock with real worktree directories to test the six-hour cadence,
  first absence, less-than-24-hour recheck, second confirmation, and final
  deletion.
- Restore a worktree during the grace period and verify metadata resets.
- Verify Git enumeration failure, Git locks, shared activity locks, analyze
  locks, malformed metadata, unsupported schema, identity reuse, symlinked
  candidates, path traversal, and permission errors all preserve data.
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
4. Graph tools cannot return an authoritative empty result before the first
   analysis completes.
5. `loomweave analyze <path>` and
   `loomweave worktree analyze <name-or-path>` both build the central linked
   index.
6. Different worktrees can analyze concurrently; duplicate analysis of the same
   worktree remains locked out.
7. Every analysis triggers a fail-soft sanity check; server startup and
   long-running servers check at the six-hour cadence.
8. No worktree index is deleted until two successful absence confirmations are
   at least 24 hours apart.
9. Git-locked, active, malformed, ambiguous, unsafe, and main stores are never
   deleted automatically.
10. Existing main-checkout users see no store-path or command regression.
