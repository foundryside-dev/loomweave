# Worktree-Scoped Loomweave Indexes Design

**Date:** 2026-07-18

**Status:** Approved for implementation after round-seventeen zero-finding review

**Tracker:** `clarion-c297efc752`

**Decision:** Loomweave will keep the main checkout's store unchanged and give
each linked Git worktree a separate SQLite store under the primary checkout's
Loomweave store. `serve` will create and analyze a missing linked-worktree index
in the background. Loomweave will also check registered worktrees periodically
and tombstone an abandoned index only after two successful absence checks at
least 24 hours apart. It may recursively delete that tombstone only after a
later successful absence check and another 24-hour recovery window. Automatic
cleanup is restricted to an owned, canonical default store; operator-relocated
stores remain usable but are never garbage-collected automatically in v1. The
automatic cleanup runner is Linux-only in v1; other platforms report the
unsupported cleanup outcome without affecting worktree-scoped indexing.

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
  gc_preflight            provisional enabled/disabled result before namespace open
  kind                    standalone | main | linked
  git_common_dir          canonical common Git directory when available
  git_admin_identity      relative linked-worktree administrative path
  stable_id               wt-<full BLAKE3 hex digest> for linked worktrees
```

`gc_preflight` is diagnostic input only. It may report `missing-owner` for a
fresh canonical namespace, and no status, scheduler, detached helper, or
mutation decision may retain or publish it after repository open. Opening or
initializing the namespace returns a separate immutable `RepositoryAuthority`
containing the canonical repository paths, the post-open `gc_capability`, and
the validated owner ID. Owner creation and repository-root rebinding refresh
that authority in the same locked operation. Every downstream diagnostics,
status, scheduler, and helper-identity path requires this post-open authority;
the helper first compares a non-opening resolved store path, then independently
probes that exact existing namespace without creating/rebinding it and compares
the complete authority before lifecycle inspection or mutation.

Immediately after `Cli::parse()` and before loading any repository-controlled
`.env`, program startup captures two different immutable values:

- `Arc<TrustedGitContext>` resolves an absolute Git executable from the
  operator's original `PATH` and retains only the documented, minimal
  per-platform launch allowlist needed to run that executable.
- `Arc<PreDotenvProcessEnvironment>` captures the full operator environment
  needed by analysis and plugin children, then strips every Git repository
  selector, Git configuration injection, and Git execution override.

These values are not interchangeable. Git resolver, status, SEI, rename,
untracked-file, and cleanup probes use only `TrustedGitContext`. Analysis and
plugin children use only `PreDotenvProcessEnvironment`, plus explicit
invocation-specific variables. Both are threaded through analysis options,
service construction, MCP launchers, snapshots, and cleanup scheduling; neither
may be reconstructed after `.env` loading. `should_load_dotenv` suppresses a
repository `.env` for top-level analysis, nested `worktree analyze`, the hidden
analysis child, and the hidden cleanup helper.

`loomweave_core::hardened_git_command` itself is extended to strip
repository-selector environment for every existing caller, and a context-bound
variant uses the captured executable and allowlist. Every resolver and cleanup
probe must use the context-bound variant with argument arrays and no shell.

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
them. Every `rev-parse` probe has a 30-second wall-clock deadline and the single
worktree-list probe has a 60-second deadline. The runner requires an explicit
`GitTerminationDomain`. Normal callers place Git in a fresh owned process group,
kill that group, and mandatorily reap its direct child on cap/deadline. A hidden
cleanup worker instead passes its already fresh worker PGID, so Git inherits
that group. The worker's TERM handler requests cooperative cancellation; the
active runner kills and reaps Git before worker exit. The worker deadline is
propagated to each probe, whose effective deadline is the minimum of its local
cap and the global remaining time. Local probe timeout/overflow and cooperative
TERM return a typed result only after the runner reaps Git.

On Linux, every cleanup worker is owned by a small supervisor outside the
worker/Git termination group. Before it spawns the worker, the supervisor uses
rustix's safe child-subreaper API and verifies that setting. It owns the
absolute deadline, signals only the worker group, waits the direct worker, and
then drains every adopted descendant with `waitpid` until `ECHILD`. Forced KILL
therefore has an explicit reaping owner; it claims no worker return or
worker-owned Git reap. The supervisor performs no repository traversal or Git
probe. If subreaper setup fails, cleanup reports `unsupported-platform` and
spawns no worker. Non-Linux targets do not spawn the automatic cleanup
supervisor in v1. The runner never uses
`Command::output()`, whose
length could be checked only after unbounded allocation. A non-zero exit,
timeout, overflow, read error, or malformed output fails the entire probe.

When `serve` is launched as Linux PID1, the top-level process is a minimal init
wrapper around one non-PID1 inner Loomweave server. Normal child owners and waits
stay in the inner process; the wrapper forwards service signals and generically
reaps only children orphaned after their original owner exits. It waits for
`ECHILD` before returning the inner status. This covers cleanup supervisors from
the inner server, a killed analyzer, and independently launched analyzer
processes without a targeted PID registry or PID-reuse race.

The resolver uses absolute `git rev-parse` results and NUL-delimited
`git worktree list --porcelain -z` output. These operations do not hash
working-tree content. Because porcelain output does not contain each entry's
administrative directory, Loomweave performs one bounded, context-bound
`git -C <present-entry> rev-parse --absolute-git-dir` probe per listed present
worktree. It caps the number of entries and probes at 4,096. Missing/prunable
entries are matched only against already validated managed metadata and the
porcelain identity; they are never probed through a missing source path. Any
probe failure aborts resolution or the whole cleanup pass without mutation.
The parser rejects truncated or unterminated records, duplicate singleton
fields, and malformed paths. It identifies the primary worktree by selecting
the entry whose resolved Git directory equals the common Git directory; it does
not assume that a branch name or directory name is unique.

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
keep low-level root helpers. A checked syntax-aware `syn` audit walks every
production Rust expression independent of formatting or argument spelling. It
rejects calls to the complete callee inventory (`store_dir`, `db_path`,
embeddings/open-in-store, traffic log, instance, port, baseline, diagnostics,
runs, hooks/status/install/federation wrappers) and literal component joins for
`.weft/loomweave`. Only repository-store derivation inside the resolver and
explicit test fixtures are allowlisted. `config.rs` and all CLI, MCP, storage,
and federation production modules are in scope; a lexical substring list is
not an adequate gate.

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
    .diagnostics/
      cleanup-diagnostic.lock
      cleanup-diagnostic.json
    .relocations/
      wt-<64 lowercase hex>.json
    .trash/
    .quarantine/
    wt-<64 lowercase hex>/
      initializing.json   # present only during crash-consistent creation
      metadata.json
      metadata-update.pending # update-start sentinel; absent when quiescent
      metadata-update.json # present only during journaled replacement/recovery
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
`last_attempt_at`, `last_success_at`, `last_error`, and nullable
`recovery_continuation_after_stable_id`, boolean `recovery_wrap_pending`, and
nullable `continuation_after_stable_id` fields. Each cursor is null or one exact
`wt-[0-9a-f]{64}` stable ID and affects batching only, never deletion authority.
The wrap field is a required JSON boolean, and a non-null recovery cursor with
`false` is invalid rather than repairable.
Loomweave replaces the record atomically. An absent, malformed, or
future-version file means
"check due" and never means "safe to delete."
When present, `last_error` contains exactly `code` and `message`. Code is the
closed kebab-case enum `git-enumeration-failed`, `lock-unavailable`,
`capability-disabled`, `candidate-unsafe`, `relocation-failed`,
`deletion-failed`, `deadline-exceeded`, or `unsupported-platform`. Writers
preserve a message of
exactly 1,024 UTF-8 bytes and truncate longer input to the largest valid prefix
of at most 1,012 bytes plus the exact 12-byte suffix " [truncated]". Readers
reject unknown codes, unknown/duplicate/missing fields, and messages over 1,024
bytes instead of normalizing the record.

`cleanup-diagnostic.json` is a separate non-authoritative checksummed record
with schema `loomweave.worktree-cleanup-diagnostic.v1`. Its private canonical
payload contains `observed_at`, a random 32-hex `event_id`, trigger, bounded
code (64 bytes), and bounded message (1,024 bytes). Code is a closed enum whose
known ASCII values fit the cap; readers reject unknown codes. Writers preserve
exactly 1,024 message bytes and UTF-8-safely truncate longer input to a prefix of
at most 1,012 bytes plus the exact suffix " [truncated]"; readers reject
oversized persisted messages. Event-ID entropy failure leaves durable/in-memory
state unchanged and emits only a bounded diagnostic—there is no weak fallback.
Strict decoding rejects unknown, duplicate, missing, malformed, or future
fields. Status orders durable
and in-memory events by `(observed_at, event_id)`; the lexicographically greater
tuple wins, and an identical tuple denotes the same event. This deterministic
tie rule never grants cleanup authority. Atomic replacement uses the single
fixed `worktrees/.diagnostics/.cleanup-diagnostic.tmp` under
`cleanup-diagnostic.lock`; it is never inventory or deletion evidence. Before
each read or write, the lock holder boundedly validates this leaf as absent or a
direct regular single-link file no larger than the diagnostic schema cap. An
incoming writer retains the scratch handle/identity, revalidates its name before
rename, and accepts the published final only through `open_read_expected`
against that identity.
An unpublished scratch is removed and the directory fsynced; a symlink, special
file, extra scratch-shaped entry, oversize, or ambiguous final/scratch state
suppresses persistence and emits only the in-memory warning. Kill and limit/+1
tests prove interrupted writes cannot accumulate files. A missing, symlinked,
or malformed diagnostics directory likewise suppresses the diagnostic rather
than weakening GC.

`cleanup-diagnostic.lock` serializes cross-process replacement independently of
`gc.lock`. Writers try-lock it without waiting, reread and strictly validate the
current record, and replace only when the incoming `(observed_at, event_id)` is
greater. A busy lock, invalid current record, or I/O error preserves the durable
file and keeps/logs only the in-memory event. This prevents a late older writer
from overwriting a newer durable event. Diagnostic code never acquires
`gc.lock`, activity, intent, writer, or metadata after taking this lock; it runs
after per-store locks or alongside long-lived shared activity, so it cannot
enter the lifecycle lock cycle.

### Store ownership and GC eligibility

Automatic garbage collection requires all of these repository-root facts:

- `repository_store` is exactly the canonical default
  `<primary-root>/.weft/loomweave` path;
- the `.weft`, `loomweave`, and `worktrees` path components are real
  directories, not symlinks;
- the canonical store remains beneath the canonical primary root;
- `worktrees/owner.json` is a no-follow direct regular single-link file with the
  supported schema and a valid random owner ID.

`owner.json` uses schema `loomweave.worktree-store-owner.v1` and records an
OS-CSPRNG-generated 256-bit owner ID encoded as exactly 64 lowercase hexadecimal
characters, the last-bound canonical Git common directory and primary root, and
creation time. Initialization, intent, quarantine, tombstone, and relocation
nonces are independent OS-CSPRNG-generated 128-bit values encoded as exactly 32
lowercase hexadecimal characters. UUIDs remain run identifiers only. Entropy
failure aborts the operation before publishing a record or creating destructive
authority; there is no timestamp, PID, or PRNG fallback. The roots are audit
fields, not deletion authority. Loomweave creates the marker atomically before
any other child when the resolved repository store's `worktrees/` directory is
absent or empty.
Every managed store and tombstone echoes the owner ID. A missing, malformed, or
unsupported marker beside existing content is never adopted: opening that
worktree namespace fails with a diagnostic. Creating a marker in an empty
configured override permits isolation but does not grant GC capability.

Namespace initialization precedes the normal `gc.lock` order because that lock
does not exist yet. After creating or opening the no-follow `worktrees/`
directory, a process descriptor-relatively opens the existing `owner.json` with
`O_NOFOLLOW|O_CLOEXEC|O_NONBLOCK` or, only when the directory is empty, creates
it with create-new/no-follow. It requires a direct regular file with link count
one and stable device/inode/size. Every contender takes the file's
exclusive lock before reading or writing it and rechecks the directory while
locked. The first lock holder writes a new checksummed fixed-schema record only
when the file is empty/incomplete and remains the sole child, then flushes the
file and directory. Later contenders discard their generated IDs and validate
that completed winner. An incomplete unlocked marker is therefore recoverable
only while it is still the sole child. No process may create `gc.lock`, a
worktree directory, `.relocations/`, `.trash/`, or `.quarantine/` before a valid
marker exists. Symlink, hardlink, FIFO, socket, device, directory, identity, or
size ambiguity refuses ownership before creating another leaf. A crash after a
complete marker write is recoverable: the next process validates
it, creates/opens `gc.lock`, and enters the normal lock order. Any malformed
marker beside another child makes the namespace unowned and unusable until
operator repair.

When a whole repository moves, the valid marker moves with its confined default
store. Under `gc.lock`, Loomweave first revalidates the marker schema, checksum,
owner ID, no-follow path confinement, and the live common-directory/primary-root
pair, then atomically rebinds both last-bound audit roots. Only after that flush
may it quarantine absolute-root-mismatched indexes and create fresh stores.
Copying the store also copies the explicit ownership marker, but a copy whose
live repository identity cannot be proven and rebound remains GC-disabled; it
cannot borrow deletion authority from the original store.

A configured `store_dir` override, noncanonical default, symlink, path escape,
missing/invalid owner, owner/repository mismatch, or unsupported platform sets
the post-open `RepositoryAuthority.gc_capability` to disabled with a closed
`GcDisabledReason`. Its public
kebab-case values are `configured-store-override`,
`noncanonical-default-store`, `symlinked-store-path`, `unconfined-store-path`,
`missing-owner`, `invalid-owner`, `owner-repository-mismatch`, and
`unsupported-platform`. Analysis, serving, and per-worktree isolation continue
to use that store
when its namespace is otherwise valid. Sanity checks may report candidates, but
v1 never renames, quarantines, or recursively deletes anything there.
Supporting explicit adoption of an external store for GC is deferred until
there is a separate operator-confirmed command and recovery design.

Namespace open is the authority-refresh boundary. A fresh default namespace
may enter with provisional `missing-owner`, create and validate `owner.json`,
and return `EnabledOwnedDefault` plus that owner ID in one
`RepositoryAuthority`. A root rebind likewise returns the rebound authority.
The pre-open result is then discarded. Tests prove first-ever open and rebind
feed the refreshed authority to status, diagnostics, scheduling, and helper
identity without requiring process restart.

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
  "absence_confirmations": 0,
  "checksum": "<64 lowercase BLAKE3 hex>"
}
```

Every timestamp in owner, initialization, worktree metadata, intent, GC,
quarantine, relocation, and tombstone records is a UTC RFC 3339 JSON string.
Optional timestamps are either an RFC 3339 string or JSON `null`; Serde's tuple
encoding for `OffsetDateTime` is forbidden. Every checksummed record has one
explicit `checksum` field containing lowercase BLAKE3 hex. The checksum is
computed over the compact UTF-8 JSON bytes of a dedicated canonical payload
struct that omits `checksum` and declares fields in schema order. Readers
re-serialize that payload identically and compare before accepting the record;
unknown or reordered input fields do not alter the canonical byte contract.
All schemas use the same `Checksummed<T>` codec: `T` is a schema-specific,
field-ordered canonical payload and the persisted envelope serializes those
payload fields followed by one `checksum` field. Decode validates supported
schema, field/nonce widths, semantic invariants, and checksum before returning
`T`; callers never deserialize an authoritative payload directly. A strict
raw-object visitor records every encountered key and rejects unknown, duplicate
(including duplicate `checksum`), or missing keys before typed deserialization
and checksum comparison. Reordering the exact known key set is accepted because
the codec reserializes it canonically. The envelope fields and unchecked
`Deserialize` implementation are private to the codec.
The JSON shown above is the persisted envelope; its canonical payload is the
same ordered field set without `checksum`. Golden JSON fixtures pin exact
serialization, time normalization, and checksum bytes for every durable record
schema.

Every quiescent authority record uses one bounded `DurableRecordFile` boundary
before the JSON codec. Plan 3's in-flight same-inode/two-link lifecycle pair is
the sole exception and uses the equally bounded `TransientRecordFile` until it
is normalized at commit. `open_read` works relative to a pinned no-follow
parent, opens the
exact direct child with `O_RDONLY|O_CLOEXEC|O_NOFOLLOW|O_NONBLOCK`, requires a
regular file with link count one, and captures device/inode/size. The narrowly
scoped `open_election_rw` variant adds `O_RDWR` but otherwise applies the same
checks and is available only for owner-election and lock leaves that must be
locked or completed in place. The universal maximum is exactly 1,048,576 bytes;
the reader reads at most one byte beyond the selected schema limit before
allocation/parsing, rejects overflow, and requires unchanged identity and size
after reading. Symlinks, hardlinks, special files, directories, and read-time
swaps fail closed. Exact-limit, plus-one, access-mode, and type/swap tests cover
owner, initialization, metadata, intent, GC, relocation, quarantine, tombstone,
and journal records.

`open_read_expected` additionally accepts a retained device/inode identity and
compares it to the newly opened final descriptor before reading. Every
scratch-to-final publication and transient-pair normalization uses this single
open-and-compare operation; a separate pathname check followed by an unchecked
open is forbidden.

Schema limits compose: a complete `metadata.json` envelope is at most 524,288
bytes and a complete `metadata-update.json` envelope is at most 786,432 bytes.
The journal embeds the intended metadata envelope as an object, not an escaped
JSON string. The writer serializes both records and rejects either limit before
publishing anything; field bounds and a worst-case serialization test prove a
maximum-size legal metadata envelope still produces a legal journal below the
shared 1,048,576-byte cap. Other schemas name their tighter limit beside their
codec and never exceed the universal maximum.

Canonical orphan evidence has only three states: zero confirmations with null
candidate time, one with a timestamp, or saturated two with a timestamp.
Repeated checks never exceed two. Any other persisted combination is invalid
metadata and cannot authorize mutation.

A short exclusive `metadata.lock` serializes read-modify-write updates from
analysis, serving, and garbage collection. Every replacement syncs complete
scratch bytes, atomically renames, and fsyncs the pinned parent before releasing
the lock. A checksummed write-ahead `metadata-update.json` additionally prevents
a power loss from resurrecting stale absence evidence: it contains the prior
metadata checksum, complete intended next envelope, and random nonce. Under the
metadata lock, first create-new the exact empty direct-regular single-link
`metadata-update.pending` and parent-fsync it. Its durable presence means an
update may have begun but not published enough bytes to recover. Then write the
journal through the one exact `.metadata-update.json.tmp`, retain its handle and
device/inode identity, file-sync, revalidate the scratch name against that
handle, atomically rename it without replacement to the absent final journal,
reopen the final no-follow, and require it to match the retained handle before
parent fsync. The metadata replacement uses the one exact `.metadata.json.tmp`
and the same retained-handle pre/post-rename identity discipline. After metadata
rename and parent fsync, cleanup removes the journal and pending sentinel, then
fsyncs the parent again.

Recovery first performs a read-only structural and budget preflight. A pending
sentinel plus an absent final journal and at most the one exact unpublished
direct-regular scratch conservatively rewrites the current valid metadata with
orphan evidence reset to zero/null before removing that scratch/sentinel; it
never simply exposes possibly stale absence evidence. Any present final journal
that fails `DurableRecordFile`, schema, or checksum validation is preserved and
disables mutation rather than entering this reset transition. Old
metadata plus a matching valid journal applies the intended next envelope;
already-next metadata removes only the journal/sentinel. Wrong types, additional
scratches, final/scratch identity mismatch, malformed current metadata, or any
other shape fails closed. GC performs this same locked reconciliation before
reading candidate metadata or deciding absence, and no relocation starts while
a metadata journal, pending sentinel, or scratch is unresolved. Kill tests
around pending creation, both scratch writes, each pre/post identity barrier,
journal rename, a reappearance clear, metadata rename, and each parent fsync
prove stale evidence never becomes deletion authority. A successful analysis updates
`last_analyzed_commit` and `last_completed_run_id`; the database's `runs` table
remains authoritative for detailed status and statistics.

Every other ordinary authority replacement has one schema-specific fixed
scratch under its existing exclusive lock: `.owner.json.tmp` for owner rebind,
`.gc-state.json.tmp` for GC state, and a same-directory fixed scratch named by
the record implementation for intent/initialization records. Every writer
retains the scratch handle/identity, revalidates its no-follow name immediately
before rename, and accepts the published final only through
`open_read_expected` with that identity. Before ordinary namespace inventory,
a read-only preflight
accepts only those exact direct regular single-link names, bounds them by the
destination schema, validates the surrounding final state, and reserves the
complete repair work. A live writer may finish its rename only while it still
owns the original retained handle. After restart, ordinary unjournaled
scratches are never promoted—even when their unkeyed checksum is valid.
Recovery may only discard an exact direct-regular scratch when the surrounding
authoritative final/state proves that discarding cannot increase deletion or
execution authority; otherwise it preserves the artifact and fails closed.
Any transition that must be completable after restart requires its own durable
precursor (`metadata-update.pending`) or retained two-link lifecycle anchor.
After discard, recovery fsyncs the parent and rereads the authoritative final
through `DurableRecordFile`.
Unknown, duplicate, linked, special, oversized, or semantically ambiguous
scratches disable lifecycle mutation. Thus a SIGKILL during an ordinary write
cannot turn a reserved artifact into authority or make the top-level inventory
permanently reject a safely recoverable namespace.

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

`loomweave install --force` must never recursively remove a repository store
once its managed `worktrees/` namespace exists. V1 refuses `--force` from a
linked worktree and refuses it from the main worktree when that namespace is
present, including when `.trash/` or `.quarantine/` contains data. The
diagnostic directs linked-worktree operators to
`loomweave worktree analyze --no-incremental -- <path>` and main-worktree
operators to preserve or explicitly inspect the managed namespace. There is no
override flag and no `remove_dir_all(repository_store)` path. A later explicit
reset feature would require its own GC, activity-exclusion, recovery, and audit
design.

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
4. Start the service with shared readiness and an absent `ActiveStorage` bundle.
5. The intent owner spawns the current executable as an analysis child. The
   child acquires the writer lock, creates and migrates the database, then
   performs analysis.
6. Every server, including a server attached to another process's intent,
   observes the durable intent, progress heartbeat, and matching run row. Only
   after the database contains an authoritative completed run for that intent
   may each process activate its local service actors and mark its own session
   ready.

An authoritative run is a matching `runs` row whose status is `completed` and
whose completion timestamp is present. Child exit code zero is only transport
evidence. `skipped_no_plugins`, migration failure before a run insert, plugin
discovery failure, a missing matching row, or any non-completed terminal row is
`build-failed` and never publishes an empty graph. Pre-row failures are retained
in the strict 1,024-byte private `IntentDiagnostic` defined below so a later
server can report and retry them. A legitimately completed zero-source run
remains authoritative.

There is exactly one pre-activation SQLite exception:
`RunAuthorityProbe`, implemented in `loomweave-storage` and injected into the
bootstrap control plane. The caller already holds shared activity, retains the
matching intent guard, then non-blockingly acquires and retains the writer guard
through the probe. The probe is legal in three automatic cases: a matching
terminal intent; a locally owned child that has been killed and reaped during
cancellation; or a pending/active intent whose lease and heartbeat are stale
and whose recorded process identity is proven dead. One additional manual-only
case is permitted for checksum-token-confirmed operator recovery when liveness is
exactly `unknown`, the lease/heartbeat are stale, the matching intent guard is
retained, and the writer guard is acquired non-blockingly. It refuses `live`
liveness and refuses to substitute for normal proven-dead automatic reclaim.
Under those retained locks, the caller revalidates run ID and nonce before
opening an existing database
read-only with create/migrate disabled, enables query-only mode, validates the
application/schema identity, reads exactly the requested run row, and closes
before activation. Activation authority belongs only to a matching `completed`
row with a valid completion timestamp. Closed terminal-failure variants are
reconciliation evidence, never activation authority. Missing DB/table, busy or
migrating DB, corrupt/wrong schema, wrong run ID, and malformed required fields
map to the typed outcomes below. The probe never creates a pool, writer,
embedding connection, or service actor.

The probe result is typed:

```text
completed(completed_at)
terminal-failure(kind: failed | cancelled | skipped-no-plugins, diagnostic)
non-terminal(kind: running)
missing | transient-busy | invalid-schema-or-corrupt
```

`completed` with a completion timestamp maps to completed. `failed` maps to
`failed`, except decoded stats with `terminal_reason == "cancelled"` map to
`cancelled`; `skipped_no_plugins` maps to `skipped-no-plugins`. Each terminal
status requires a valid completion timestamp. `running` is the only
non-terminal kind. Completed or terminal failure without a timestamp, malformed
classification stats, and unknown statuses are invalid/corrupt. Completed and
terminal-failure preserve and conditionally publish the matching semantic
terminal state during cancel or reconciliation. Missing/non-terminal may cancel
or reclaim only when that cause's other checks pass. Transient-busy retries
without changing intent/readiness. Invalid schema or corruption fails closed
and requires repair. Activation accepts only valid `completed`.

`Completed` conditionally terminalizes the same nonce/run and may activate.
`TerminalFailure` conditionally terminalizes its closed semantic failure but
never activates, cancels over that evidence, or permits reclaim. Only `Missing`
and `NonTerminal` enter cause-specific cancel/reclaim rules; busy retries and
invalid/corrupt fails closed. This reconciles a crash after the run-row commit
but before intent finalization and prevents a retry from replacing the intent
while an older observer probes or publishes.

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

Only linked-worktree analysis entry points participate in the new durable
intent protocol: direct `loomweave analyze`, `loomweave worktree analyze`,
automatic bootstrap, and MCP `analyze_start`. Main and standalone analysis keep
the existing analyze lock and existing no-index/degraded service behavior. This
is an explicit rollout boundary and has regression coverage. Under exclusive
`analysis-intent.lock`, a linked launcher either discovers an existing intent
or atomically writes
`analysis-intent.json` with schema, run ID, random 128-bit nonce, launcher PID
plus process-start identity, creation time, lease expiry, and `pending` state.
Persisted failure and pre-row diagnostics use one strict `IntentDiagnostic`
newtype. Writers preserve exactly 1,024 UTF-8 bytes; longer input retains the
largest valid UTF-8 prefix of at most 1,012 bytes plus the exact 12-byte suffix
" [truncated]". Readers reject oversized values rather than normalizing them,
and the same bound applies before a probe diagnostic reaches readiness, HTTP,
or MCP output. The intent codec also rejects unknown, duplicate, and missing
fields, malformed widths, unknown schemas, and checksum failures.

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
durable run result, then passes its owned `ActiveAnalysisLease` into the
coordinator's finishing operation. That operation retains shared activity,
releases the lease's writer lock, takes the intent lock, and atomically records
the matching owner/nonce terminal state and finish time. A crash in that short
gap is reconciled from the durable run row before stale-intent recovery.

Cancellation is parent-owned because the current supervisor terminates the
analysis process group with SIGKILL. `RunHandle` therefore retains the intent
nonce. After the owning server kills and reaps the child, it acquires the intent
lock, revalidates the matching nonce/run, acquires the writer guard
non-blockingly, and invokes `RunAuthorityProbe` while retaining both guards. If
that row already committed a terminal success or failure, the parent
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
Process liveness is PID-reuse-safe: on Linux it compares PID, `/proc/<pid>/stat`
start time, and boot ID. Unsupported platforms, inaccessible process metadata,
and a process disappearing mid-probe return `unknown` and fail closed; they do
not reclaim an intent. A native platform backend may be added only with the
same identity guarantee. V1 deliberately does not use PID-only recovery on
macOS or Windows.

`ServerState` and the HTTP `AppState` own the same `Arc<IndexAccess>`.
`IndexAccess` contains readiness plus an optional runtime-neutral
`Arc<ActiveStorage>` defined below the protocol layers in `loomweave-storage`.
`ActiveStorage` contains only cloneable storage handles: `ReaderPool`, the
summary-writer sender, the Wardline-writer sender, and the explicit embeddings
path/provider handle. MCP combines those handles with its private LLM and
semantic policy; HTTP combines them with its routing/auth state. Lower layers
never name MCP-private `SummaryLlmState` or `SemanticSearchState` types.

The production `ServerState` refactor is additive: it preserves the current
caps, clock, budgets, request-cancellation state, Filigree and Wardline
configuration, diagnostics, analyze configuration, and every existing builder
field. A new linked-service constructor supplies context and `IndexAccess`;
already-ready constructors remain for main/standalone and test fixtures.

The analysis child is the sole database writer before activation. Apart from
the narrowly scoped `RunAuthorityProbe` under one of the four cause-validated
conditions above, the serving process creates no database connection or actor
during an
authoritative-first-run bootstrap.
This includes `ReaderPool`, the MCP LLM writer, the HTTP Wardline writer,
embedding store connections, and any service helper that calls
`Connection::open` or `Writer::spawn`. Handlers resolve those dependencies from
`IndexAccess` only after the gate says ready; they do not retain eager clones in
separate state.

After the authoritative first run completes, the monitor constructs the whole
`ActiveStorage` bundle and publishes it together with `ready` in one synchronized
transition. A partial activation is torn down and reported as failed. If a
completed authoritative run exists, retry first attempts activation again and
does not launch a redundant analysis. Status and configuration tools use
context, metadata, intent, and progress state while no bundle exists; they may
not accidentally force the database open. `project_status_get` and
`analyze_status_get` stay available during bootstrap. Their database-derived
counts are `null`/unavailable until ready, never fabricated as zero.

Each service process owns a long-lived `ServeRuntime`. It retains the linked
store's shared activity guard, `IndexAccess`, a stateful `BootstrapControl`, a
dedicated control runtime, one `ActivatedActorsSlot`, and MCP/HTTP service
runtimes. The cleanup plan later adds a runtime-owned startup/periodic scheduler;
the server has no analysis-complete trigger handle. `BootstrapControl` owns the
worktree context, intent
coordinator, run-authority probe, launcher and pre-dotenv environment,
activation factory/control handle, observer cancellation/join handle, and the
sole optional child `RunHandle`. MCP receives only a cloneable
`AnalysisControlClient`; its start/cancel requests are serialized with child
exit, observer, activation, and shutdown events by the control loop. No handler
or second supervisor owns a PID.
On Unix each owned analysis child is spawned in a fresh process group through a
CLI `OwnedProcessGroup`, which retains the direct child and PGID. Cancellation
signals only that verified group and waits/reaps the child; an unrelated server
group is never targeted. The CLI declares its own target-Unix signal dependency.
Non-Unix cancellation reports unsupported rather than signaling by PID alone.
Spawn-failure intent cleanup requires the still-live shared activity guard
before it may acquire intent.

Activation returns a private pending bundle containing cloneable
`ActiveStorage` plus actor join owners. At one serialization point, the control
loop moves join owners into the empty slot and conditionally publishes
storage/readiness. Rollback joins pending owners without publication. Shutdown
wins that same serialization point, stops accepting requests, unpublishes
storage, drains protocol state, closes senders, then takes and joins the slot.
Only then does it drop runtimes and the activity guard. Stdio EOF, HTTP startup
failure, partial activation, and cancellation have bounded shutdown tests that
prove queued writes flush or fail explicitly.
On ordinary service shutdown, an in-flight bootstrap/refresh child is detached
to complete: ownership is released only after closing the local cancellation
handle, while its durable intent, process identity, writer lock, and progress
remain observable by a future server. Explicit `analyze_cancel` still kills and
reaps an owned child. All local observer tasks are cancelled and joined before
the control runtime and activity guard are dropped. Barriers cover shutdown
racing MCP cancel and shutdown racing activation.

`IndexAccess` is versioned. Every reservation/retry allocates a monotonically
increasing `ReadinessGeneration` paired with the durable run ID. State-changing
methods are conditional (`transition_if`, `publish_ready_if`) and reject a stale
generation/run key, so an old child monitor cannot overwrite a newer retry or
ready state. Observation uses versioned watch notifications plus a durable
snapshot on every wake; a missed in-process notification cannot strand a
session. Every attached server runs a bounded-backoff observer that reads a
lock-consistent intent snapshot, progress/heartbeat, and matching terminal row,
then activates its own local actors after authoritative completion. If the
owner dies, the observer participates in the same fail-closed reclaim/election
protocol. Multi-process tests cover owner completion, owner death, retry, and
both already-open sessions becoming ready.

A refresh reuses the already-published `ActiveStorage` bundle. Successful
refresh conditionally returns the matching generation to `ready`; it does not
start duplicate storage writers. Failed refresh conditionally returns to
`stale` with the same bundle and warning.

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
  existing policy visibility. Before `ActiveStorage` exists, semantic config
  status reports the explicit embeddings path/presence and
  `vector_count: null` without opening the embeddings database; after readiness
  it may obtain the count through active storage;
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
without resolving a name. The machine contract is an argument vector. When the
active configuration came from an explicit `--config`, that exact path is
preserved before `--`:

```json
["loomweave", "worktree", "analyze", "--config", "/custom/config.yaml", "--", "/canonical/worktree/path"]
```

Source-, primary-, and default-target configuration origins are discovered by
the child and omit `--config` from the fallback vector.

`--` prevents a path beginning with `-` from becoming an option. The optional
`fallback_command` string is display text rendered with platform-correct shell
escaping and is never reparsed by Loomweave. JSON escaping preserves newlines;
clients should execute `fallback_argv` directly.

On a platform whose process-start identity backend returns `unknown`, automatic
intent reclaim remains disabled. `doctor` reports the expired lease, stale
heartbeat, recorded PID/identity, writer-lock state, and a record-checksum-based
confirmation token. Recovery is an explicit operator action:

```text
loomweave worktree recover-intent <name-or-path> \
  --run-id <uuid> --confirm <token>
```

The command resolves the registered linked store, holds shared activity, locks
intent then writer non-blockingly, revalidates matching run ID/nonce, expired
lease, stale heartbeat, liveness exactly `unknown`, and an unchanged
confirmation token. It then runs the typed authority probe under both guards.
Only `missing` or `non-terminal` may mark that intent `abandoned` terminal;
`completed` or `terminal-failure` preserves the semantic terminal result,
`transient-busy` retries without mutation, and invalid schema/corruption fails
closed. It refuses `live` liveness and never sends a signal. A pending child
subsequently fails nonce/state activation; an active analyzer cannot pass the
writer-lock check. Any changed fact refuses recovery and prints a fresh doctor
report. This provides fail-closed macOS/Windows operator recovery without
weakening automatic reclaim.

### Activity and writer locks

Each worktree retains the existing exclusive `loomweave.lock` for analysis, so
two analyses cannot write the same database concurrently.

Add three coordination locks:

- a server holds a shared activity lock for its lifetime;
- an analysis holds a shared activity lock for its lifetime;
- every short-lived command that opens, checks, checkpoints, backs up, or
  mutates store state holds a shared activity guard for the full filesystem and
  SQLite operation; this includes `db`, guidance, hooks/status, `doctor` when it
  opens the database, and install/setup;
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

The top-level `ServeRuntime`, not a transient builder or protocol thread, owns
the service activity guard until every database actor and state clone has shut
down. `gc` means the repository `gc.lock`; `intent` is `analysis-intent.lock`; and
`writer` is `loomweave.lock`. Store open/create takes `gc.lock`, validates or
creates the directory, acquires shared activity protection, then releases
`gc.lock`. Normal serving and analysis never request `gc.lock` while holding an
activity, intent, writer, or metadata lock. Barrier tests race GC against each
short-lived store command and prove exclusive activity cannot succeed during
its operation. Cleanup workers run as separate operations. Every analysis entry
point reaches one terminal epilogue on both success and failure. The epilogue
persists whatever terminal run/metadata state is valid, drops every metadata,
writer, intent, and activity guard it acquired, and produces exactly one
`CleanupScheduleOutcome`. It first applies the platform and standalone-PID1
checks in the exact precedence below. A remaining Linux analysis with validated
authority attempts one detached supervisor spawn; before authority it performs
no spawn and reports `repository-unavailable` without touching a path. It does
not enqueue onto an async runtime that is about to drop and never changes the
original analysis result. On Linux the analyzer uses the CLI process utility's
provisional detached spawn mode to start a cleanup supervisor outside the
analyzer group. The provisional handle retains the direct `Child`, process
group, process identity, and dedicated startup pipes. Before ready, dropping or
cancelling it terminates and reaps the pre-worker supervisor. The pipes are
never the MCP transport. The supervisor installs TERM, enables and verifies
child-subreaper mode, and writes one exact ready/unsupported byte; it cannot
spawn a worker before the launcher's exact acknowledgement byte. The launcher
waits at most two seconds for this startup state, not for cleanup. Unsupported
is reaped and maps to the exact null-PID unsupported outcome. EOF, invalid
bytes, or timeout terminates and reaps the pre-worker supervisor and maps to
`spawn-failed`.

After ready, an analyzer consumes the provisional owner into a non-killing
`ArmedDetachedProcess` before sending the launch byte. It retains the direct
`Child` only as a non-killing wait owner. Drop or launch-write failure closes the
pipe and reaps the blocked pre-worker supervisor before returning; successful
one-byte launch relinquishes that wait owner and returns only a non-cancelling
identity. A scheduler instead promotes and stores an `OwnedCleanupSupervisor`
in `AwaitingLaunch` state before its non-consuming launch. Write failure closes
the pipe, transitions to `Draining`, and reaps; shutdown from `AwaitingLaunch`
does the same. Therefore no kill-on-drop owner exists after a worker may start.
Terminating an analyzer after the supervisor reads `0xa5` cannot kill the
supervisor. A standalone analyzer whose own PID is 1 returns the exact
unsupported outcome before spawning.

The supervisor creates a fresh worker/Git PGID and starts an independent
watchdog with one absolute monotonic ten-minute deadline. Both
supervisor and worker install safe `signal-hook` TERM flags before their
respective child/work boundaries; no new project `unsafe` exception is
introduced. The worker crosses a test-only readiness barrier before opening a
store or spawning Git. The deadline is passed into Git and checked between
every bounded
inventory, record, traversal, rename, fsync, and unlink step. At expiry the
supervisor immediately sends TERM to the worker/Git group; it performs no
repository I/O. Cooperative worker unwind best-effort records
`deadline-exceeded` after reaping Git. The supervisor waits five seconds, sends
KILL to that group if needed, waits its worker, and reaps every adopted Git
descendant before it records a terminal event and exits. `EINTR` retries
immediately and `ECHILD` is the only success. Every other wait error retains the
live supervisor, retries with exponential backoff from 25 ms to one second, and
emits a bounded diagnostic initially and at most once per minute thereafter.
Every analyzer, including a hidden serve-spawned child, owns exactly one
detached analysis-complete supervisor; the server monitor never duplicates that
trigger.

When Linux `loomweave serve` starts with real PID 1, `main` enters a minimal
init/reaper wrapper before constructing `ServeRuntime`. It installs safe
TERM/INT flags and spawns the exact executable once in a fresh process group
with the original invocation, captured pre-dotenv environment, and a bounded
control socket. The hidden invocation and socket carry the same UUID-v4
simple-form nonce; the child requires a parent PID of 1 and constant-time
validates exactly those 32 lowercase-hex bytes with no trailing data. It
immediately installs safe TERM/INT flags plus a bridge to
the ordinary non-PID1 shutdown request. It sends exact handler-READY byte `0x01`
before constructing protocol/runtime children and checks pending flags before
every child-producing boundary. Before runtime creation it exits with the
mapped signal code and no children; after partial/full creation it enters the
same non-consuming `ServeRuntime` shutdown state machine. Invalid control data,
EOF, or pre-READY exit creates no forwarding target. A two-second monotonic
READY deadline terminates/reaps the still-owned inner group, disarms its PGID,
and returns 1. The wrapper opens no repository/config state, runs no protocol
runtime, and spawns no other direct child.

The wrapper forwards TERM/INT only to the inner server group and is the sole
`waitpid(-1, WNOHANG)` caller in PID1. Its 25-ms loop buffers the first signal
until READY, forwards each observed signal at most once after READY, drains all
available exits, and retains only the inner PID/status plus bounded counters.
The inner bridge stops protocol intake, begins scheduler close, drains protocol
and actors, retries owner-retaining scheduler close/join, and tears down sinks,
activity, and runtimes; it cannot return while a supervisor owner remains.
Direct-child owners and waits remain inside the inner process. Detached
analyzers, their supervisors, and independently
launched analyzer supervisors become wrapper children only after their original
parents exit, so generic reaping cannot steal another owner's child status. The
wrapper records the inner status, immediately disarms its PGID target, and
remains until `ECHILD`. Normal codes are preserved; signals map to
`128 + signal` (`SIGTERM` 143 and `SIGINT` 130); spawn failure returns 1 with a
bounded diagnostic. `EINTR` retries immediately. `ECHILD` succeeds only after
capturing inner status; all other wait errors retain PID1, back off from 25 ms
to one second, and log initially plus at most once per minute. It stores no
per-child registry, naturally handles overlapping and unregistered orphans, and
never targets a possibly reused PID. The inner
runtime freezes every new scheduler producer before ordinary shutdown; the
wrapper remains as persistent init until detached analysis and cleanup finish.
A server uses a capacity-one startup/periodic scheduler: `try_send` never waits,
a full send sets one pending bit, and the worker reruns once after the current
helper.
One server helper runs at a time, and begin-close/cancel/join are explicit
shutdown steps. Begin-close immediately closes intake, cancels the timer, and
clears or freezes pending work without dropping an active supervisor. A
cleanup-enabled `ServeRuntime` is the scheduler's sole owner. A server
trigger launches a short-lived cleanup helper with
close-on-exec lock descriptors; the helper owns no activity lock and skips the
parent server's active store when its exclusive activity probe fails. The
helper is spawned through the captured full, sanitized
`PreDotenvProcessEnvironment`, whose applicator starts with `env_clear()`, and
receives the absolute Git executable explicitly. Before dotenv, it reconstructs
`TrustedGitContext` through an explicit-path constructor that validates and
uses that exact canonical executable without resolving `PATH`; it never
inherits the server's repository-loaded environment. The invocation also carries
the exact parent config origin and expected post-open `RepositoryAuthority`:
canonical repository store, GC capability, and owner ID when enabled. The
helper uses a two-stage gate. First it resolves config and canonical repository
store without opening or creating a namespace and requires that path to equal
the expected store. On mismatch it records memory/stderr only and touches
neither store. Only after path equality does an existing-only, non-creating,
non-rebinding probe open that exact namespace and compare capability plus owner
ID. A missing owner or changed binding fails closed; cleanup helpers never
initialize or rebind authority. An explicit override, first-open owner
transition, rebind, or provenance change can therefore never redirect one to
the default store.

Every hidden analysis child and cleanup worker opens stdin as null before spawn.
A cleanup supervisor receives only the dedicated bounded startup stdin/stdout
pipes above, closes them after acknowledgement, and never inherits the MCP
transport. Analysis progress/output uses only its explicit progress file or
protocol channel; the worker uses null stdout and inherited stderr. No detached
or serve-spawned child retains the MCP transport's stdin descriptor across
server shutdown.

Cleanup takes every per-store lock non-blockingly. Failure to acquire one skips
the candidate. It never waits for activity while holding metadata, and an
analyzer cannot hold intent without already holding shared activity. These
rules prevent both server/cleanup deadlocks and the reservation race identified
in review.

### Periodic worktree sanity checks

On supported Linux, garbage collection has three triggers:

- every analysis schedules one check after completing and releasing its store
  locks;
- `serve` startup runs a check when the last successful repository check is at
  least six hours old;
- a long-running server schedules another check every six hours.

The analysis-triggered check is not throttled by the six-hour age, but every
trigger uses a non-blocking repository `gc.lock`. If another process is already
checking, the caller skips cleanup. Cleanup failure is logged and exposed in
status diagnostics but never changes the current analysis or MCP startup
result. On other platforms the same scheduling points return
`unsupported-platform` without spawning a supervisor.

Failures before a helper can update `gc-state.json` are still observable. A
bounded, core-owned `CleanupDiagnosticSink` records scheduler-full/coalesced,
scheduler-closed, spawn, and abnormal-helper-exit events in memory and writes a
separate checksummed, non-authoritative `cleanup-diagnostic.json` by atomic
replace through the one fixed `.cleanup-diagnostic.tmp` under its independent
lock. Locked pre-read/write recovery prevents scratch accumulation, and
concurrent replacements cannot tear the
record; the deterministic event ordering above resolves durable/in-memory
races. Corruption only suppresses the diagnostic and never affects GC cadence
or deletion authority. `project_status_get` rereads the durable diagnostic and
merges it with the service process's newer in-memory event. Diagnostic write
failure is logged and remains fail-soft.

Core opens this diagnostic boundary under `gc.lock` from the post-open
`RepositoryAuthority`: it revalidates owned capability/no-follow confinement,
creates and pins `worktrees/.diagnostics` before shared activity, and returns a
cloneable `CleanupDiagnosticsHandle` (or a memory-only disabled handle). Linked
store open retains it and the authority in the store guard; main/standalone
repository entry points obtain both before activity. A later path swap cannot
redirect writes through the pinned handle. Core constructs the sink from that
handle and generates event IDs with its existing `getrandom::fill` boundary.
`ServeRuntime` owns one sink and one `Arc<RepositoryAuthority>` and clones both
into the scheduler worker and additive `ServerState`/HTTP status constructors;
every analyzer owns a separate sink from its repository-open
handle. Shutdown stops protocol intake and calls scheduler begin-close to stop
the timer, close trigger intake, and freeze pending work. It then drains
protocol state and joins the scheduler while the runtime sink remains live. The
scheduler retains each Linux cleanup supervisor as its direct child, signals
TERM to the dedicated supervisor-only group, and keeps the supervisor wait
owner alive. That handler requests cancellation without exiting.
The supervisor sends TERM only to its owned worker/Git group, waits five
seconds, and sends KILL to that group if needed. Its control loop checks TERM,
the deadline, `Child::try_wait`, and non-blocking descendant `waitpid` at most
every 25 ms; it never hides the signal flag behind a blocking wait. Persistent
wait errors back off to one second and log at most once per minute. It drains
the direct worker and all adopted descendants to `ECHILD` before acknowledging
completion. Only then does the
scheduler reap its direct supervisor and join. It never kills or abandons the
process that owns descendant reaping. A stop failure keeps that ownership and
returns from a non-consuming scheduler shutdown method; `ServeRuntime` retains
the scheduler, refuses later teardown, and retries until the same owner drains.
Only successful join consumes the owner. Non-Linux lifecycle targets record
unsupported and spawn no external supervisor. The sink records
the terminal event, then
drops before activity and runtimes. A real barrier test keeps an unrelated
server-group sibling alive while proving the supervisor reaps its worker and
Git descendant. The PID1 namespace gate additionally proves the outer wrapper
reaps every orphan after the inner server exits.

Sink snapshots also use the pinned handle. They strictly read durable state,
merge it with the in-memory event by `(observed_at, event_id)`, and return a
separate bounded read warning. A malformed or unreadable durable record cannot
hide a valid in-memory event; status exposes both the latest event and any read
warning.

The three nullable status projections have exact closed shapes. `last_error`
contains `code` plus `message` using the GC code enum and 1,024-byte rule above.
`last_scheduler_diagnostic` contains RFC 3339 `observed_at`, a 32-hex
`event_id`, `trigger`, `code`, and `message`. Trigger is `startup`, `periodic`,
or `analysis-complete`; code is `scheduler-full`, `scheduler-closed`,
`helper-spawn-failed`, `helper-exit-abnormal`, or
`helper-deadline-exceeded`; message uses the same
1,024-byte UTF-8-safe truncation rule. `scheduler_diagnostic_read_warning`
contains only `code` and `message`; code is `invalid-durable-diagnostic` or
`durable-diagnostic-io`, and the internally generated message has the same
1,024-byte bound. Readers reject unknown persisted diagnostic values rather
than normalizing them. Exact non-null and null JSON serialization tests pin all
field names, enum spellings, and bounds.

Each check first no-follow enumerates and validates the managed namespace from
the post-open `RepositoryAuthority`, then runs
`git worktree list --porcelain -z` through the context-bound hardened Git runner
against the common Git directory. Parsing creates a raw porcelain inventory
only; a separate all-or-nothing enrichment receives the explicit
`WorktreeContext`, `RepositoryAuthority`, and already validated
`ManagedNamespaceInventory`. It resolves administrative identity through
hardened Git and that inventory before any candidate logic receives a
registered-worktree inventory. It never re-derives an overridden store from
`primary_root`. An error at any stage aborts the entire cleanup pass.

The same pass no-follow enumerates the pinned `worktrees/`, `.trash/`, and
`.quarantine/` roots to discover stores absent from Git. Each independently
allows at most 100,000 direct entries and 16 MiB of child-name bytes.
`worktrees/` accepts only its exact typed control leaves/directories and
`wt-[0-9a-f]{64}` direct directories; trash/quarantine accept only the exact
relocation-name grammar. Unknown names, wrong types, links, special files,
hardlinked authority leaves, overflow, or read errors fail the whole inventory.
No partial metadata update, rename, or deletion occurs. When `gc_capability` is
disabled, the complete bounded check reports what it would have considered but
performs no lifecycle mutation.

Those directory caps are not the pass budget. Under `gc.lock`, each pass first
builds one immutable, read-only `RecoveryPlan`. Recovery-only passes have a
root-only base containing ordinary root artifacts plus the GC-state read/write;
they never plan candidate metadata. Clean candidate-capable passes instead add
the selected candidates' metadata/pending/journal artifacts. The plan pins
names, types, device/inode/size, record checksum/version, intended action, and
schema-maximum cost for every recovery, final revalidation, and state write. It
reserves the complete cost before any mutation; plan overflow or ambiguity does
nothing. Namespace-open recovery uses the same builder with a bounded
single-store scope.

Repository-wide GC first structurally inventories only the bounded direct
`.relocations/` entries: exact names, types, sizes, link shapes, uniqueness, and
stable-ID grouping. It does not decode every journal or inspect destination
contents at this stage. Starting after the recovery cursor, it selects one
lexicographic sub-batch by reserving each unit's schema-maximum journal,
destination-record, revalidation, write, and fsync cost only from
`64 MiB - root_base_cost`. A root base at or above the cap fails closed. Only
then does it decode the selected journals,
validate their confined destination references against the already bounded
trash/quarantine direct-child inventory, and inspect the selected destination
final/scratch. A scratch-only unit uses that same inventory to prove the
pre-publication surrounding state. Before the first recovery mutation from a
null phase, GC durably sets `recovery_wrap_pending=true`; this preliminary state
write is itself in the root base plan. A pass with recovery work executes only
that sub-batch, durably advances the recovery cursor after earlier units commit,
and performs no candidate metadata or lifecycle mutation. A pass that finds no
recovery unit after a non-null cursor clears only the cursor and leaves the wrap
bit true. Only a later pass that starts with a null recovery cursor and true wrap
bit, finds no recovery unit, and completes candidate-capable work atomically
clears the bit while advancing `last_success_at`. This durable phase catches
lower-sorting work and survives a kill after cursor-clear fsync. A malformed
selected journal, unsafe destination, or over-budget single unit leaves the
cursor before that unit and fails closed; every legal single unit fits the
residual budget.

Recovery continuation is not six-hour throttled. A non-null recovery cursor or
true `recovery_wrap_pending` always makes `check_due` true. After a successful
recovery-only or cursor-clear pass, the same worker releases `gc.lock` and
immediately starts the next bounded pass while its global deadline remains; if
the deadline expires, the next scheduler tick still sees durable due state.
`last_attempt_at` advances for every subpass, but
`last_success_at` advances only after a pass starts with a null recovery cursor,
finds no recovery unit, and completes its candidate-capable work. A malformed
or unsafe recovery unit records a terminal diagnostic and stops the helper
rather than hot-looping.

After that recovery gate, GC processes one deterministic lexicographic
candidate batch of at most 4,096 managed stores and at most 64 MiB of aggregate
authority-record bytes, plus at most one recursive tombstone deletion attempt.
The nullable, strictly validated `continuation_after_stable_id` starts the next
pass after its stable ID and wraps to the beginning after reaching the end.
Namespace changes may cause conservative reprocessing but cannot skip a stable
ID forever. If the next record would exceed the byte budget, the pass stops
before that record, commits the cursor only after all earlier mutations are
durable, and retries it next time. Every legal single record fits the budget.
Thus 4,097 or 100,000 abandoned stores make bounded forward progress instead of
wedging. Structural preflight overflow or ambiguity fails closed before repair;
recovery and candidate batch boundaries are continuation, not errors. Recursive
traversal retains its 100,000-entry, depth-128, and 16-MiB relative-name limits.

Metadata writers do not take `gc.lock`, so execution never assumes the plan is
still current. After acquiring a candidate's activity/intent/writer/metadata
locks, GC rechecks every planned leaf identity, size, checksum/version, and
absence/presence bit against the immutable plan before its first mutation. Any
late pending/journal/scratch or other mismatch skips that entire candidate with
no repair or metadata update; it is rediscovered and budgeted by a later pass.
Execution performs only actions already in the plan and reserves worst-case
revalidation/write bytes, so concurrent analysis cannot create unbudgeted work.

The candidate-to-tombstone phase is:

1. A present, unlocked worktree refreshes `last_seen_at` and clears orphan
   fields.
2. A Git-locked worktree is present and protected even if its filesystem path
   is temporarily unavailable.
3. A missing or Git-prunable worktree becomes an orphan candidate on the first
   successful check. Loomweave records `orphan_candidate_since` and one
   confirmation but keeps every file.
4. A later successful enumeration must still find it absent, must occur at
   least 24 hours after the first confirmation, and saturates the confirmation
   count at two.
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
5. Atomically publish `worktrees/.relocations/<stable-id>.json`. Create an
   `O_NOFOLLOW|O_CREAT|O_EXCL` direct regular scratch named
   `.<stable-id>-journal-<32 lowercase hex nonce>.tmp`, retain its open handle
   and device/inode identity, write and `sync_all` the complete canonical
   checksummed envelope, then revalidate the no-follow scratch name against the
   handle immediately before publication. Descriptor-relatively hard-link it
   without replacement to the absent final name, open that final name with
   no-follow semantics, and require it to match the still-open scratch handle
   before accepting it as authority. Fsync `.relocations/`, then re-open and
   revalidate final against the still-open handle. This final-plus-scratch
   `TransientPublicationPair` is the only permitted two-link state. A narrow
   `TransientRecordFile` boundary requires the exact two names to identify the
   retained inode with link count exactly two, stable size/identity, the
   schema-specific byte cap, and no additional reserved scratch; it boundedly
   reads that retained descriptor and invokes the same strict checksummed codec.
   It is the only exception to the single-link `DurableRecordFile` input rule.
   Keep both verified names reachable through rename and destination-record
   publication so a crash or final-name swap cannot destroy the sole recovery
   anchor. Revalidate final against the retained handle immediately before and
   after every destructive boundary. Platforms that can publish directly from
   the open handle use the same transient boundary. A mismatch fails closed,
   retains the trusted scratch/journal and every ambiguous artifact, and
   performs no further mutation. The
   `loomweave.worktree-relocation.v1` payload records owner ID, stable ID,
   operation (`tombstone` or `quarantine`), direct child source name,
   destination-root kind (`trash` or `quarantine`), direct child destination
   name, captured filesystem/metadata identities, absence or mismatch evidence,
   RFC 3339 timestamp, and the 128-bit relocation nonce. The rename cannot begin
   until the final journal link is durable.
6. Use a handle-relative atomic rename to move the whole directory on the same
   filesystem to
   `.trash/<stable-id>-<YYYYMMDDTHHMMSSZ>-<32 lowercase hex nonce>`.
   Fsync both pinned rename parents (`worktrees/` and the selected `.trash/` or
   `.quarantine/`) before proceeding. A restore rename fsyncs both parents too;
   any parent-fsync failure retains the durable journal and stops.
7. Open the renamed directory without following symlinks and require its
   device/inode identity to match step 1. On a mismatch, restore it when safe,
   fsync both restore parents, and preserve it unmodified with a diagnostic.
8. Publish `tombstone.json` with the owner ID, original device/inode and metadata
   digests, absence evidence, rename time, and relocation nonce using the same
   retained-handle, pre-link/post-link identity, file-fsync, no-replace-link,
   and directory-fsync protocol with a direct `.tombstone-<nonce>.tmp` in the
   moved directory. Quarantine uses
   `.quarantine-<nonce>.tmp` and `quarantine.json`. Apply the same narrow
   `TransientRecordFile` boundary to the destination record and retain both
   trusted scratches while validating the moved directory/final record. At the
   commit point, first revalidate and unlink/fsync the destination-record
   scratch, require/decode its now-single-link final through
   `DurableRecordFile::open_read_expected` against the retained identity, then
   revalidate and unlink/fsync the relocation-journal scratch and
   require/decode that now-single-link final with the same expected-identity
   operation. Revalidate both final
   names once more, remove the relocation journal final, and fsync
   `.relocations/`. Any failure preserves every remaining reachable anchor.
   Release the per-store locks.

At the start of every namespace open and GC pass, Loomweave enumerates
`.relocations/` under `gc.lock` without following links.
Only direct files named `wt-[0-9a-f]{64}.json` and exact reserved direct regular
scratch names are accepted; at most 4,096 files and 16 MiB total bytes are
inspected. During a durable journal's reconciliation, the selected moved
directory may contain at most its one exact `.tombstone-<nonce>.tmp` or
`.quarantine-<nonce>.tmp` scratch. Exceeding a bound, duplicates, unknown
entries, symlinks/special files, or malformed/future authority records disable
all lifecycle mutation and produce a diagnostic. Exact reserved regular
scratches are non-authoritative managed artifacts. A crash before publication
leaves one single-link scratch, which the mutating reconciler may unlink only
after fresh capability, parent, filename, type, stable-ID, size, and
surrounding-state validation. A crash after publication may leave exactly one
final/scratch same-inode pair with link count exactly two. Recovery reads it
only through bounded `TransientRecordFile`, retains the scratch through any
required rename/final-record completion, and normalizes both pairs to
single-link finals only at the commit sequence above. Any other link count,
identity, duplicate, or name combination fails closed. `doctor` only reports
these transitions. There is at most one outstanding journal per stable ID, and
no new relocation starts until every journal or scratch is reconciled.

Mutating reconciliation first freshly revalidates
the expected post-open `RepositoryAuthority`, including
`GcCapability::EnabledOwnedDefault`, the owner/checksum, canonical store, and
no-follow canonical confinement. Overrides, relocated/unowned stores,
missing/mismatched owners, and
symlinked namespaces are report-only; no record, directory, or journal is
changed. Reconciliation then uses the operation/destination kind to inspect
exactly the
active plus `.trash/` sides for tombstones or the active plus `.quarantine/`
sides for quarantine. Before journal publication only the active side is
authoritative. After publication and before rename, the matching active side is
restored/retained and the journal may be removed. After rename, the matching
destination side is completed into the final `tombstone.json` or
`quarantine.json` through atomic publication, and only then is the journal
removed and the
namespace flushed. A matching final record plus matching journal is finalized
by removing the journal. Both sides, neither side after a published journal,
mismatched identities/nonces, wrong destination kind, malformed/future records,
or permission failures preserve all data and require operator recovery.
Kill-point tests cover both operations during scratch byte writes, after the
no-replace publication link but before scratch cleanup, after directory fsync,
after rename and each source/destination parent fsync, after restore and each
restore-parent fsync, after post-rename validation, during final-record scratch
writes, after final-record link, and after journal removal. Exact partial
scratch files are removed under the reserved-artifact rules; a partial write is
never visible at an authority filename. This closes both byte-publication
windows and the window between rename and the authoritative destination record.

`doctor` is always read-only. It may take `gc.lock` and perform the same bounded
no-follow inspection, but it never restores a side, publishes a final record,
or removes a journal/scratch—even for an otherwise safely recoverable shape. It
reports the action that the next capability-enabled namespace open or GC pass
would take.

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
equivalent mount IDs. Linux is the only v1 recursive-deletion backend.
Linux is also the only v1 automatic external-cleanup runner because it provides
the owned child-subreaper boundary. Non-Linux Unix reports automatic cleanup
and deletion as unsupported and retains tombstones rather than launching an
unreapable worker tree. Windows and other non-Unix targets use the unsupported
lifecycle backend: automatic
inspection, relocation, and deletion are disabled, so active stores and
tombstones are preserved with a diagnostic. A future Windows backend requires
explicit reparse-safe implementation plus compile and test jobs. No target
falls back to canonicalize/prefix checks or `remove_dir_all`. A platform without
race-resistant, handle-relative, no-cross-mount traversal disables automatic
deletion. Failed removal leaves the tombstone for a later pass.

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
- A supported recursive backend cannot cross a filesystem or bind mount beneath
  the tombstone; unsupported backends perform no traversal.

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
- Public federation helpers roll out additively: introduce explicit
  `*_from_roots` and `*_at_path` APIs, migrate every CLI serve/analyze/HTTP,
  integration-binding, federation, MCP, and test caller, and run checks for all
  affected crates before deprecating or removing legacy root-taking wrappers.
  No intermediate commit may knowingly leave the workspace uncompilable.
- A manually installed linked-worktree database is neither migrated nor
  deleted. `doctor` reports it as legacy/stranded while pointing at the central
  effective store.
- Global MCP registration that launches `loomweave serve --path .` works
  automatically because runtime CWD identifies the linked source root.
- Clients that rely only on checkout-local `.mcp.json` still need an explicit
  agent-asset install in that worktree. This design does not make an absent
  registration start a server.
- The status response adds worktree kind, source root, effective store, stable
  ID, readiness, bootstrap run ID, GC state, `last_scheduler_diagnostic`, and
  `scheduler_diagnostic_read_warning` using the exact closed status projections
  above. GC state and capability come from the current post-open
  `RepositoryAuthority`, never the provisional resolver preflight. A read
  warning does not hide a valid in-memory diagnostic or affect
  cleanup authority. `gc_capability` is exactly
  `{ "state": "enabled-owned-default", "reason": null }` or
  `{ "state": "disabled", "reason": "<closed-reason>" }`; contradictory or
  unknown state/reason pairs are rejected. Existing fields remain compatible.
- JSON analysis results add `cleanup_schedule` with exactly four always-present
  fields: `outcome`, nullable `pid`, nullable `process_start_identity`, and
  nullable `diagnostic`. Its closed outcome is `spawned`,
  `spawned-identity-unavailable`, `spawn-failed`, `repository-unavailable`, or
  `unsupported-platform`. PID is a positive i32 JSON integer. The only non-null
  v1 identity is
  `{ "kind": "linux-procfs", "boot_id": "<36 lowercase UUID>",
  "start_time_ticks": "<1-20 canonical decimal digits>" }`; unavailable is
  null. `spawned` requires PID/identity and null diagnostic;
  `spawned-identity-unavailable` requires PID, null identity, and a diagnostic;
  the other three require null PID/identity and a diagnostic. A diagnostic is
  exactly `{ "code": "<closed-code>", "message": "<bounded-message>" }`.
  Schedule diagnostics use the closed `process-identity-unavailable`,
  `helper-spawn-failed`, `repository-unavailable`, or `unsupported-platform`
  code plus a 1,024-byte message. `spawned` maps to null,
  `spawned-identity-unavailable` to
  `process-identity-unavailable`, `spawn-failed` to `helper-spawn-failed`, and
  the remaining outcomes to their same-named codes. Scheduling follows this
  exact first-match precedence:

  1. A non-Linux target returns `unsupported-platform` without resolving or
     opening repository authority.
  2. A Linux standalone analyzer whose real PID is 1 returns
     `unsupported-platform` without resolving or opening repository authority.
  3. Any other Linux analysis that cannot obtain validated repository
     authority returns `repository-unavailable` without spawning.
  4. With Linux authority, an explicit supervisor unsupported byte returns
     `unsupported-platform`; EOF, invalid data, timeout, or acknowledgement
     failure returns `spawn-failed` after reaping the pre-worker supervisor;
     ready plus acknowledgement returns `spawned` or
     `spawned-identity-unavailable` according to identity availability.

  Exact cross-product tests pin this precedence and reject unknown fields,
  kinds, noncanonical values, and overflow. This field grants observation only,
  never cancellation authority; the Linux identity names the supervisor, not
  its worker/Git descendants. Non-JSON output is compatible.
- HTTP clients gain additive 503 `INDEX_BUILDING` and `INDEX_BUILD_FAILED`
  responses where an unindexed worktree previously could return an empty
  result. The existing `/api/v1/_capabilities` route remains available.
- Configuration setters continue updating the file whose values are active;
  the only new default is that a linked worktree with no file creates the
  primary-root configuration instead of a checkout-local shadow file.
- Existing `install --force` behavior is narrowed: linked invocations and any
  repository store containing a managed worktree namespace fail closed instead
  of recursively deleting that namespace.

## Verification Strategy

### Resolver and storage tests

- Create real temporary Git repositories with a main worktree and multiple
  linked worktrees.
- Verify main, standalone, linked, moved, locked, prunable, and malformed Git
  contexts.
- Verify stable IDs do not change with branch, HEAD, dirty state, or repository
  relocation, while root metadata mismatches force a fresh store.
- Reject owner, initialization, metadata, and lock leaf symlinks, hardlinks,
  special files, directories, identity/size swaps, and records above the exact
  1,048,576-byte shared cap before creating any dependent authority.
- Exercise read-only and election-RW durable opens, the 524,288-byte metadata
  cap, the 786,432-byte journal cap, and worst-case nested serialization that
  must remain below the shared ceiling.
- Prove a fresh namespace and an owner-root rebind return refreshed post-open
  authority to status, diagnostics, schedulers, and helper identity without a
  restart; provisional `missing-owner` is never published after open.
- Kill metadata reappearance-clear updates around journal publication, rename,
  and parent fsync, including pending-sentinel creation and both fixed
  scratches; partial journal bytes must conservatively clear orphan evidence,
  never expose old absence evidence as deletion authority. GC must reconcile
  under metadata lock before absence decisions and refuse relocation while a
  pending sentinel, journal, or scratch is unresolved.
- Inject a checksum-valid ordinary scratch after restart and prove it is never
  promoted; only durable-precursor/two-link protocols may complete authority.
- Exercise hostile repository configuration and environment input while
  proving every `rev-parse` and `worktree list` probe uses the pre-dotenv trusted
  Git executable and context-bound hardened runner. Include a repository `.env`
  with a fake `PATH`, `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, and
  `GIT_CONFIG_*`; no fake Git, redirected repository, hook, pager, or filesystem
  monitor may run.
- Prove nested `worktree analyze`, hidden analysis children, and cleanup helpers
  suppress repository dotenv loading, while plugin children receive the
  sanitized full `PreDotenvProcessEnvironment` rather than Git's minimal
  allowlist.
- Bound inventory to 4,096 entries/probes and verify per-entry administrative
  identity discovery, including prunable entries whose source paths are gone.
- Reject non-zero Git exits, concurrently streamed output beyond either cap,
  truncated output, malformed NUL records, duplicate singleton fields, and
  non-UTF-8 required paths before store creation. Verify silent 30/60-second
  deadline breaches and stream overflows kill the fresh Git process group and
  reap its direct child rather than leaving a detached helper stuck.
- Verify primary `weft.toml` store overrides and configuration precedence;
  override stores isolate worktrees but never gain automatic GC capability.
- Verify the default owner marker, symlink and escape rejection, mismatched or
  missing ownership behavior, and refusal to adopt non-empty content.
- Pin exact 64-hex owner IDs, 32-hex nonces, entropy-failure behavior, RFC 3339
  timestamps, canonical payload serialization, and BLAKE3 checksum golden bytes
  for every durable schema.
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
- Barrier main plus two linked analyses concurrently and assert distinct DB
  paths, locks, run IDs, sidecars, and divergent graphs. Also serve/analyze one
  linked worktree while another eligible store is collected.
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
- Treat `skipped_no_plugins`, pre-row migration/discovery failure, a missing or
  non-completed matching row, and exit-zero-without-authority as build failures;
  accept a completed zero-source row.
- Pin 1,024-byte intent diagnostics, UTF-8-safe truncation at 1,025 bytes, and
  strict rejection of oversized persisted diagnostics.
- Start two servers against the same empty store and verify one coordinates the
  analysis while both sessions observe the same readiness transition.
- Run the two servers as separate processes. Verify the attached observer
  activates both existing sessions when the owner completes, and elects a
  replacement after PID-reuse-safe owner-death checks. Race an old monitor with
  retry and prove generation/run-key CAS rejects stale transitions.
- Use barriers at reservation, spawn, activation, and writer-lock acquisition to
  race direct analysis, `worktree analyze`, two servers, and MCP
  `analyze_start`; assert one durable run ID and one writer in every ordering.
- Kill the coordinator in pending and active states. Verify a live process is
  never reclaimed and an expired, dead intent is reclaimed only after the
  lease, heartbeat, and writer-lock checks all agree.
- Prove each owned analysis child has a dedicated process group, cancellation
  signals/reaps only that group, spawn-failure cleanup retains shared activity,
  and an unrelated sibling in the server group survives.
- Verify an attached server cannot cancel a foreign child, a child-owning server
  can cancel only with write tools enabled, and read-only tool inventory never
  exposes client analysis or cancellation.
- Cancel before database creation and assert the parent terminalizes the intent.
  Barrier-race cancellation against a committed terminal run and assert semantic
  success/failure wins over `cancelled`.
- Verify fallback argv for paths beginning with `-` and containing spaces,
  quotes, shell metacharacters, and newlines. Execute the argv directly and
  treat the rendered command as display-only.
- Verify an explicit `--config /custom/path` is preserved before `--` in
  fallback argv, while source/primary/default origins are rediscovered.
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
- Exercise `crates/loomweave-cli/tests/serve.rs`, regenerate the capability
  fixture and SHA through the repository generator, update the embedded BLAKE3
  shape pin, and run the hermetic federation seam-golden script.

### Cleanup tests

- Use a fake clock with real worktree directories to test the six-hour cadence,
  first absence, less-than-24-hour recheck, second confirmation, atomic
  tombstone rename, another 24-hour recovery window, and final tombstone
  deletion.
- Separate raw porcelain parsing from all-or-nothing administrative-identity
  enrichment; enrichment receives the explicit authority and already validated
  managed inventory, an overridden store is never re-derived from primary root,
  and no partial inventory may reach candidate logic.
- Test `worktrees/`, `.trash/`, and `.quarantine/` at exactly 100,000 direct
  entries/16 MiB name bytes and one beyond, plus unknown names/types/links;
  failure permits no partial metadata or filesystem mutation.
- Test deterministic continuation across 4,097 and 100,000 candidates, the
  64-MiB record-byte boundary, cursor changes under namespace churn, and the
  one-deletion limit. Structural overflow permits no mutation, while a batch
  boundary advances durably and resumes without starvation.
- Test a recovery backlog above 64 MiB: each pass recovers only its planned
  sub-batch, advances `recovery_continuation_after_stable_id`, and performs no
  candidate mutation. Prove the cursor clears only after a no-recovery wrap and
  candidate processing resumes only on the following clean pass. Kill after
  cursor-clear fsync and require the wrap bit to remain due. Test the exact
  `64 MiB - root_base_cost` boundary with root scratches. A malformed or
  over-budget single recovery unit must leave the cursor unchanged. Pin
  immediate self-continuation, due-state, deadline, and `last_attempt_at` versus
  `last_success_at` transitions with a fake clock.
- Build each recovery plan from the full direct-entry structural inventory plus
  selected root, metadata, relocation, and destination-record artifacts. Prove
  unselected journals and destinations are not decoded. Change metadata after
  planning and prove exact identity/version recheck skips the candidate without
  unbudgeted repair; namespace open uses the bounded single-store form.
- Accept only orphan evidence states zero/null, one/timestamp, and saturated
  two/timestamp; reject every impossible or overflowed combination.
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
- Race GC against database backup/checkpoint, guidance, hooks/status, doctor DB
  access, install/setup, analysis, and the full service lifetime; each operation
  retains shared activity until its last filesystem/SQLite access.
- Test exact limit and limit-plus-one cases for 100,000 entries, depth 128, and
  16 MiB relative-name bytes. At inspection, journal, rename, tombstone-record,
  and traversal phases, inject permission failures and assert the whole
  candidate is preserved with a diagnostic. At per-entry and final-directory
  unlink phases, assert the tombstone plus every unremoved remainder is
  preserved and retryable; already completed descriptor-relative unlinks are
  not claimed to roll back.
- Cover missing/malformed/future/traversal/unowned/relocated/symlinked/mounted
  records and every relocation-journal kill point. `doctor` reports incomplete
  relocation state without mutating it.
- Swap each named journal, tombstone-record, and quarantine-record scratch
  between write, link, and final verification; attacker bytes must never become
  accepted authority, and ambiguous cleanup must disable mutation.
- Kill after each hard-link publication and prove only the exact same-inode
  link-count-two transient pair is boundedly decoded through
  `TransientRecordFile`, retained across rename/final publication, and
  normalized only at the commit point; every other link shape fails closed.
- Kill diagnostic replacement before rename repeatedly and prove the fixed
  scratch is bounded/reconciled without accumulating hidden artifacts.
- Pin closed GC, scheduler, and read-warning codes, 1,024-byte message handling,
  strict oversized/unknown rejection, and exact non-null status JSON.
- Verify relocated, unowned, symlinked, and ownership-mismatched repository
  stores can report candidates but cannot rename, quarantine, or delete them.
- Verify quarantines are excluded from periodic deletion and only direct,
  validated children of the owned `.trash/` root can be recursively removed.
- Verify a simulated platform without safe handle-relative traversal disables
  recursive deletion and leaves the tombstone intact.
- Present a nested mount/bind mount or filesystem-abstraction equivalent and
  verify no-cross-mount traversal preserves the entire tombstone. A simulated
  Windows/non-Unix unsupported backend must preserve active stores and
  tombstones without inspecting, relocating, or deleting reparse-point paths.
- Verify concurrent servers serialize cleanup with `gc.lock` and a cleanup
  error cannot fail analysis or startup.
- Verify every analyzer owns one post-run helper while the server owns only
  startup/periodic scheduling; server shutdown must neither lose nor duplicate
  the detached analyzer's check.
- Drive success and failure through one terminal analysis epilogue, proving all
  acquired store locks drop before exactly one scheduling outcome.
- Terminate the analyzer before ready, after arming, after the supervisor reads
  the launch byte, and after worker spawn. No pre-launch worker may exist, and
  no launched worker may lose its supervisor reaper. Hold an active Git probe
  at server shutdown and the watchdog boundary; the worker/Git group must die,
  Git must be reaped on cooperative TERM, and the Linux supervisor must reap the
  worker plus adopted Git after forced KILL.
- Make `scripts/run-worktree-pid1-reaper-test.sh` a required Linux Verify step.
  The host script builds the real CLI with a non-default test-fixture feature,
  extracts and canonicalizes the exact `loomweave` executable from Cargo JSON,
  and runs it under an unprivileged user/PID namespace with mounted procfs and
  `unshare --kill-child`. The feature-gated command invokes the shared production
  PID1 wrapper, inner server, analyzer, supervisor, and worker paths. Its strict
  result proves real PID1, a non-PID1 inner server, SIGKILL-before-launch reap,
  independent-analyzer reap, two overlapping supervisor reaps, worker-tree and
  final `ECHILD`, and completed shutdown. The script also proves the default
  release parser rejects the fixture command and works with a nondefault
  `CARGO_TARGET_DIR`. Unavailable namespace support is a failure in CI.
- Run feature-gated production-path scenarios for TERM before READY, TERM and
  INT after READY, and one injected wrapper wait error. Require buffered
  pre-READY signaling, scheduler freeze, actor/activity teardown,
  owner-retaining supervisor drain, exit codes 143/130, rate-limited wait retry,
  post-exit PGID disarming, and final `ECHILD` in separate fresh namespaces.
- Pin the startup-byte protocol: subreaper failure returns exact unsupported;
  EOF, invalid bytes, and two-second timeout reap the pre-worker supervisor and
  return exact spawn-failed; ready cannot spawn work before acknowledgement.
- Pin the exact non-Linux, standalone-Linux-PID1, repository-authority, and
  supervisor-startup outcome precedence without opening authority in the first
  two cases.
- Fault one supervisor wait, then persistently fault it. Require owner retention,
  25-ms-to-one-second backoff, at-most-once-per-minute diagnostics, retry, and
  final reap. Assert TERM is observed within the 25-ms poll bound rather than
  waiting for the ten-minute deadline. In PID1 serve mode, require the wrapper
  to reap every orphan and reach final `ECHILD`.
- Begin shutdown with analyzer cleanup blocked, advance fake time by more than
  six hours, and prove scheduler intake/timer/pending work were frozen before
  ownership drain, so no new periodic supervisor appears.
- Pass exact config plus post-open repository authority into helpers and prove a
  configured override removed before helper start leaves the default namespace
  absent because path comparison precedes the existing-only authority probe.
- Prove hidden analysis and cleanup children null MCP stdin, and pin the exact
  `repository-unavailable` early-failure cleanup JSON outcome.
- Prove safe `signal-hook` TERM registration completes before supervisor child
  spawn and before worker repository/Git work, and verify Linux subreaper mode
  before the worker exists, without extending the workspace unsafe policy.

### Release gates

- Pin top-level and nested CLI help, hidden flags, `--`, exit 75, structured MCP
  error-code contracts, and every modified catalogue/golden suite.
- At execution time, treat `.github/workflows/verify.yml` as the single
  authoritative pre-merge/release contract and re-read it for drift. Plan 3
  spells out every current locally reproducible command. The minimum summary is:

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo build --workspace --bins
  python scripts/check-migration-retirement.py --self-test
  python scripts/check-migration-retirement.py
  python scripts/check-workspace-version-lockstep.py
  python scripts/check-pyright-pin-lockstep.py --self-test
  python scripts/check-pyright-pin-lockstep.py
  python scripts/check-wardline-version-bounds.py --self-test
  python scripts/check-wardline-version-bounds.py
  python scripts/check-entity-cap-lockstep.py --self-test
  python scripts/check-entity-cap-lockstep.py
  cargo nextest run --workspace --all-features --no-tests=pass
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
  cargo deny check
  python scripts/check-python-ontology-version.py --self-test
  python scripts/check-python-ontology-version.py
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

- Also run the Wardline taint-golden network lockstep and core-no-reqwest guard
  from the current Verify workflow, plus cfg-level unsupported
  process/deletion backend tests locally. Before final branch clearance, require
  the entire green Verify workflow, including native macOS Clippy/build CI. V1
  has no Windows lifecycle-mutation backend; adding one requires reparse-safe
  implementation plus Windows compile and test jobs. Local verification must
  not be reported as native CI.
- Separate live dogfood from accelerated lifecycle verification. Live dogfood
  covers first-serve, same-session activation, divergent graph, and explicit
  analysis in a dedicated temporary repository/state root with trap-based
  cleanup. Its process harness owns and reaps the server, captures the detached
  cleanup helper's process-start identity from JSON analysis output, and waits
  boundedly for helper death, `gc-state` advancement or terminal diagnostic,
  and an acquirable `gc.lock` before removing paths. The trap uses bounded
  close/TERM/KILL/wait and the same PID-reuse-safe helper check. It must leave
  the feature repository clean and its worktree list byte-for-byte unchanged.
  An ignored real-filesystem harness with an injected clock covers removal and
  recovery:
  `cargo test -p loomweave-cli --test worktree_cleanup -- --ignored real_lifecycle`.

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
7. On supported Linux, every analysis triggers a fail-soft sanity check; server
   startup and long-running servers check at the six-hour cadence. Other
   platforms return the exact unsupported cleanup outcome without spawning.
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
15. A recovery backlog larger than one pass advances through a separate durable
    cursor; no candidate lifecycle mutation occurs until a later clean pass has
    observed no recovery work after the cursor wraps.
