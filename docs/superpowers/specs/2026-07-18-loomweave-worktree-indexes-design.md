# Worktree-Scoped Loomweave Indexes Design

**Date:** 2026-07-18 (rewritten after owner scope reduction); amended
2026-07-28 after four-lens implementation review
(`../plans/2026-07-18-worktree-indexes-plan.review.json`)

**Status:** Approved for implementation

**Tracker:** `clarion-c297efc752`

**Decision:** Give each linked Git worktree its own SQLite store under the
primary checkout's Loomweave store. `serve` builds a missing one in the
background. A store whose worktree is no longer registered is deleted on the
next sweep. The main checkout's store path and behavior are unchanged.

## Problem

Loomweave derives every store path from the source root: `store_dir()`
(`crates/loomweave-core/src/store.rs:68`) maps a project to
`<project>/.weft/loomweave/`, `serve` enters no-index mode when the derived
database is absent, and `analyze` refuses to run until that local store exists.
A linked worktree has none of Loomweave's ignored runtime files, so an MCP
process started there cannot answer graph queries.

Pointing a linked worktree at the main checkout's database would be incorrect:
the schema stores canonical absolute source paths and uses them for incremental
analysis and integrity checks, so the main database describes the wrong files,
commit, and dirty state.

Worktrees are routinely removed, so their stores must live somewhere
discoverable after the worktree is gone.

## What this object is

**A worktree index is an ephemeral cache.** Two facts set the entire design
posture:

- **Lifetime:** it lives as long as its worktree — hours to days.
- **Cost of loss:** it is a scan of the codebase, regenerable by re-running
  `analyze`. Roughly 20–30 minutes of unattended compute on a large tree.

Three consequences, and they are not negotiable design inputs:

1. **No crash-consistency machinery.** Checksummed records, write-ahead
   journals, and recovery protocols all protect against losing something worth
   20 minutes. Unreadable metadata means *delete the store and re-analyze*, not
   *recover it*.
2. **Fail toward deletion, not preservation.** Fail-closed-preserve is right for
   user data. Here the cost matrix is inverted: wrongly deleting costs one
   re-analyze; wrongly preserving leaks directories forever, which is the bug
   this feature exists to avoid. There is no grace period, no tombstone, no
   quarantine.
3. **Match the main store's posture.** The 60 MB primary `loomweave.db`
   delegates durability to SQLite under ADR-011 and ADR-035 (WAL,
   `synchronous=NORMAL`, `application_id` identity header) and carries no
   checksums, owner markers, or journals. A worktree snapshot must not be
   protected more heavily than the durable index it is a copy of.

This follows the Weft suite's intent: lightweight tooling for solo and
small-team developers, 80% of the functionality at 20% of the resource cost.

## What confinement is for

One hard-safety property survives, and it protects the **neighbours**, not
Loomweave:

```
.weft/filigree/    ~12 MB   issues, comments, audit trail   NOT regenerable
.weft/wardline/    ~828 KB  baselines, waivers, attestations NOT regenerable
.weft/loomweave/   ~61 MB   codebase scan                   ~20-30 min
```

A cleanup sweep that escapes its namespace eats the issue tracker or the user's
source tree. So deletion is confined, unconditionally:

- Traversal is rooted at a **pinned directory handle** for
  `<repository-store>/worktrees/`, never a re-resolved string path.
- Every open is `O_NOFOLLOW`; a symlink at any level refuses the operation.
- Only direct children matching exactly `wt-[0-9a-f]{64}` are eligible.
- On Linux, `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
  RESOLVE_NO_XDEV`. A platform without race-resistant, handle-relative,
  no-cross-mount traversal disables automatic deletion and reports it.
- String-prefix checks never authorize a deletion.

This is cheap and it is the only part of the original design that guarded
something irreplaceable.

## Architecture

### Worktree context

One resolver produces a `WorktreeContext` before any runtime path is chosen:

```text
WorktreeContext
  kind              standalone | main | linked
  source_root       canonical path being analyzed
  primary_root      canonical main-worktree path
  repository_store  store_dir(primary_root)
  effective_store   repository_store, or repository_store/worktrees/<id>
  store_paths       explicit database, sidecar, run, and lock paths
  config_origin     explicit | source | primary | default-target
  stable_id         wt-<BLAKE3 hex of git admin identity>, linked only
```

The resolver uses `git rev-parse` and `git worktree list --porcelain -z`
through the existing `hardened_git_command`. It identifies the primary worktree
by the entry whose resolved Git directory equals the common Git directory — not
by branch or directory name.

The stable ID is BLAKE3 of the Git administrative identity (e.g.
`worktrees/federation-seam-followups`). It excludes branch name, HEAD, dirty
state, and absolute path, so switching branches does not orphan a store and
moving the repository does not mint a new ID.

For a main worktree or non-Git project the resolver returns the current store
path unchanged. Failing to prove a checkout is linked falls back to existing
behavior; it never guesses a primary root and writes elsewhere. The source
root is canonicalized before any identity comparison, so a checkout reached
through a symlink resolves to the same store as its target rather than
triggering a spurious `source_root` mismatch. A bare primary (a worktree hub
whose common Git directory has no main working tree) is out of scope for
isolation: the resolver classifies those checkouts as standalone and uses the
local store.

Every command and service that reads or writes runtime state receives
`StorePaths` or an explicit leaf path rather than re-deriving from a source
root. This covers `analyze`, MCP/HTTP reader state, embeddings, diagnostics,
instance ID, own port, `db`, `guidance`, hooks, and the secret-scan baseline.
`doctor` is covered deliberately partially: the `.weft/loomweave.schema`
check (both its text and JSON renderers) and the `http.instance_id` check
redirect to worktree-aware messaging for a linked worktree, and an
additive, read-only `worktree_stores` check reports every isolated store's
health under its own stable ID; every other doctor check stays root-derived
from the literal `--path`, since those surfaces (hooks, skill pack, MCP
registration, the instructions block, integration bindings, ...) are
properties of the invoking checkout itself, not of the isolated store. Two
of those root-derived checks are a known, accepted exception to that
framing rather than a clean case of it: `gitignore.current` and
`db.tracked` inspect `<worktree>/.weft/loomweave/` directly, and `--fix`
on `gitignore.current` will *create* that directory (with a `.gitignore`
inside it, no store `loomweave.db`) in a linked worktree that never had
one — a smaller version of the same decoy-materialization hazard the
`http.instance_id` redirect exists to prevent, left unrouted here as
out of scope for this pass.

### On-disk layout

The primary checkout is unchanged. Linked worktrees get subdirectories:

```text
<repository-store>/
  loomweave.db          # main checkout, untouched
  embeddings.db
  instance_id
  ephemeral.port
  runs/
  worktrees/
    gc.lock             # serializes the sweep
    wt-<64 lowercase hex>/
      metadata.json     # plain serde; unreadable => delete and rebuild
      loomweave.lock
      loomweave.db
      embeddings.db
      instance_id
      ephemeral.port
      runs/
```

Each worktree directory is a complete Loomweave store, so SQLite's
single-writer rule stays local and worktrees analyze concurrently.

### Metadata

```json
{
  "schema": "loomweave.worktree-index.v1",
  "stable_id": "wt-<64 lowercase hex>",
  "git_admin_identity": "worktrees/<name>",
  "source_root": "/absolute/path/to/linked-worktree",
  "created_at": "2026-07-18T00:00:00Z"
}
```

Plain `serde_json`. No checksum, no journal, no lock-protected
read-modify-write. It exists to answer one question — "is this store still
describing the worktree I think it is?" — and any answer other than a confident
yes means delete the directory and re-analyze.

If `source_root` no longer matches the resolved worktree path (a moved
worktree, or an administrative name reused elsewhere), the store is deleted and
rebuilt rather than quarantined. Serving graph rows whose absolute paths belong
to another checkout is the failure being avoided; preserving the stale copy
serves no one.

### Configuration and sibling discovery

Precedence for linked worktrees:

1. explicit `--config` path;
2. `<source-root>/loomweave.yaml`, when present;
3. `<primary-root>/loomweave.yaml`, when present;
4. built-in defaults, with `<primary-root>/loomweave.yaml` as the write target.

This preserves branch-specific tracked configuration while letting the primary
checkout's ignored local configuration work from a worktree. The resolver
records the selected path as `ConfigOrigin`; `llm_config_set` and
`semantic_config_set` update exactly that file.

Sibling local-state discovery (Filigree's `ephemeral.port` and
`federation_token`, any future `.weft/<sibling>/` sidecar) uses one ordered,
deduplicated lookup list: source root, then primary root. Loomweave's own port
and instance ID use explicit `StorePaths` leaves so concurrent servers cannot
overwrite each other.

There is no `weft.toml [filigree].url` rung in that lookup (removed on the
1.5.0 line for a security reason that outranks discovery convenience:
repository content may be untrusted, while Filigree clients attach
operator-owned bearer tokens to the resolved endpoint — operator overrides
belong in the process environment, `WEFT_FILIGREE_URL`, or private config).
The ephemeral-port rung is the discovery mechanism, and it falls through
source root → primary root as described above.

The secret-scan baseline (`secrets-baseline.yaml`) is a per-store leaf under
`StorePaths`, not shared or copied from the primary: a freshly created
worktree store starts with no baseline at all, so any finding the primary's
baseline already justified may re-fire as new in that worktree until its own
baseline accumulates (or someone copies in) matching entries.

### Bootstrap

`serve` on a linked worktree with no index:

1. Resolve `WorktreeContext`; create the store directory and `metadata.json`.
2. Spawn a **detached** `loomweave worktree analyze` for the worktree — but
   only when no run has ever completed against this store (readiness, per
   step 4 below, is not already `Ready`). A store that has already finished
   at least one build is never re-spawned just because `serve` restarted;
   keeping an already-built index fresh is the `SessionStart` hook's job
   (plain `loomweave analyze`), not an unconditional background respawn
   fired on every session start against a worktree that is already usable.
   Reuse the SessionStart hook's *detachment technique* (`process_group(0)`,
   null stdio) — **not** its argv. The hook spawns plain `loomweave analyze
   <source-root>`, and plain `analyze` must itself resolve storage through
   `WorktreeContext`, so both spellings land in the isolated store. An
   un-routed plain analyze writing a 20–30 minute index into
   `<worktree>/.weft/loomweave/` — a store serve's readiness poll never
   observes — is the silent failure this step exists to prevent.
3. Serve immediately. Graph tools return the existing structured tool-error
   envelope (`error.code` / `error.retryable` plus top-level `diagnostics`)
   with code `index-building` and the fallback command. `index-building` and
   `index-build-failed` join the pinned `McpErrorCode` wire vocabulary.
4. Readiness is governed by "has any run row ever completed" (`completed`,
   or the legitimate terminal `skipped_no_plugins`), never by "what does the
   most recent row say": once a completed row exists, readers activate and
   stay activated regardless of a *later* row's status — a manual rebuild
   (`loomweave worktree analyze`) or a second `serve` racing this one that is
   currently `running`, or one that later `failed`, never re-blocks graph
   tools against a store that already has good data sitting in its tables.
   Recomputed by consulting run state on each tool call at the single
   dispatch chokepoint — no timer, no cached readiness state.

Double-spawn is prevented by the existing `analyze_lock.rs` (fs2 advisory
lock), not by a new durable-intent protocol. If analysis fails, tools return
`code: "index-build-failed"` with the exact `loomweave worktree analyze`
command to run.

`project_status_get` and `analyze_status_get` remain available while building;
their database-derived counts are `null`, never fabricated as zero.

### Explicit analysis

`loomweave worktree analyze [--no-incremental] -- <name-or-path>` builds or
rebuilds a worktree index directly. It is the recovery path and the documented
fallback in every build-failed message.

### Cleanup

On `serve` startup and after each analysis, under a non-blocking `gc.lock`:

1. Enumerate direct `wt-[0-9a-f]{64}` children of the pinned `worktrees/`
   handle — the **candidate** set.
2. Resolve the repository's common Git directory (one hardened `git
   rev-parse`, its own invocation additionally stripping
   `GIT_DIR`/`GIT_COMMON_DIR`/`GIT_WORK_TREE` so it cannot be redirected by
   an ambient Git environment variable — a narrow, deletion-path-local
   guard; general `hardened_git_command` environment sanitization remains
   tracked separately, clarion-9202f4acec), then read that directory's own
   `worktrees/` administrative subdirectory with a single `readdir` — the
   **registered** set. `git worktree list` is deliberately never run for
   this: it does not expose the administrative directory name each entry
   corresponds to, and the stable ID is a hash of exactly that name.
3. Delete any candidate whose stable ID is not in the registered set,
   confined as described above.

Candidates are read **before** the registered set, deliberately — never the
reverse. Between the two reads, a worktree can only move from "unregistered,
no store" to "registered, store exists" (a fresh `git worktree add` plus
store creation completing mid-sweep) or from "registered, store exists" to
"unregistered, store still exists" (`git worktree remove`). Reading
candidates first closes the unsafe direction: a store that does not exist
yet at the candidate read is simply absent from the candidate set this
cycle, so a worktree registered and store-created entirely within the gap
between the two reads is never misclassified as unregistered and deleted.
Reading registered first would get that direction wrong — a just-registered
worktree's store, invisible when the (now-stale) registered set was read,
would be captured as a candidate and read as unregistered, and the sweep
would destroy a live worktree's just-built index.

Synchronous and in-process. No helper subprocess, so no supervisor, no child
subreaper, and no PID1 wrapper around `serve`. If `gc.lock` is held, skip —
another process is sweeping.

The administrative `worktrees/` directory not existing at all is **not**
treated as an enumeration failure: Git creates it lazily on the first `git
worktree add` and deletes it again once its last entry is removed, so a
repository with no linked worktrees (or none left) legitimately has zero
registered worktrees — that reads as an empty registered set, and the sweep
proceeds against it normally. Treating that `NotFound` as an abort would
leak the very last worktree's store forever: no future sweep would ever see
a non-empty registered set to prove it unregistered. Every *other*
Git-enumeration failure — `git` missing, `primary_root` not inside a
repository, a non-zero exit, non-UTF-8 output, or any other read error on
either the admin directory or the candidate directory — aborts the sweep
without deleting anything. Failure is logged and never affects analysis or
MCP startup.

A Git-locked worktree is registered and therefore never a candidate. The main
store is never a candidate.

When a `[loomweave].store_dir` override is active the sweep is **report-only**:
it logs would-be candidates and deletes nothing (the Non-goals entry below is
a hard requirement, not advice — an absolute override can be shared between
unrelated repositories, and repository A's registered-worktree set must never
authorize deleting repository B's stores). Every deletion the sweep performs
is logged with the store's stable ID and the reason, as is every
delete-and-rebuild triggered by unreadable or mismatched metadata: under this
posture the log line is the only audit trail an automatic deletion leaves.

Removal semantics deserve one distinction. A store swept while its worktree
still exists costs one re-analyze — the open inode keeps a live `serve`
working, and the next start rebuilds. But `git worktree remove` deletes the
working tree *and* its registration together, so for that store there is no
source tree left to rebuild from and "the next start rebuilds" does not
apply. A `serve` whose resolved source root no longer exists surfaces a
distinct `source-root-missing` state on graph and status tools instead of
continuing to report the last staleness verdict for a tree that is gone.

## Non-goals

- Do not partition the existing graph schema by worktree ID.
- Do not clone or rewrite the main database to seed a worktree.
- Do not run `git worktree prune` or modify Git's administrative state.
- Do not make the graph live-update after every file write.
- Do not automatically delete from a `[loomweave].store_dir` override —
  isolation works there, cleanup is diagnostic-only.
- **Explicitly cut as disproportionate to a 20-minute cache:** checksummed
  durable-record codecs, write-ahead metadata journals, crash-consistent
  initialization with recovery, owner-election markers, two-phase tombstone
  reclamation with 24-hour grace windows, quarantine, relocation journals with
  two-link recovery anchors, per-pass byte budgets with continuation cursors,
  child-subreaper supervisors, and the PID1 init wrapper.

## Alternatives considered

**One shared database partitioned by worktree ID** — requires schema change and
serializes all worktrees behind one SQLite writer. Rejected.

**One database inside each linked worktree** — undiscoverable after the
worktree is removed, and requires an install in every checkout. Rejected.

**Seed from the main database** — the stored absolute paths are wrong for the
new root, so the copy needs rewriting before it is usable. A 20-minute
re-analyze is simpler and always correct. Rejected.

**Conservative two-phase GC with grace windows** — the original design.
Rejected: 48-hour minimum reclamation for an object that lives hours to days,
protecting data worth 20 minutes of compute.

## Verification

- **Resolver:** linked/main/standalone classification; primary identified by
  Git directory not branch name; stable ID invariant across branch switch and
  repository move; non-UTF-8 path rejection; symlinked source root resolves to
  the same identity as its target; a bare primary classifies as standalone.
- **Isolation:** two worktrees on divergent branches produce different graphs;
  concurrent analyze in both succeeds; neither touches the main store.
- **Bootstrap:** serve on a fresh worktree returns `index-building`, then
  answers in the same session once the run completes; `analyze_lock.rs`
  prevents a second spawn; failure surfaces `index-build-failed` with the exact
  fallback command; bare `loomweave analyze <linked-worktree-path>` writes into
  the isolated store, never `<worktree>/.weft/loomweave/`.
- **Config:** source-root `loomweave.yaml` wins over primary; setters write the
  resolved origin; sibling port/token discovery falls back source → primary.
- **Cleanup:** an unregistered store is deleted; a registered one is not; a
  Git-locked worktree is preserved; Git enumeration failure deletes nothing;
  an active `[loomweave].store_dir` override makes the sweep report-only;
  every automatic deletion logs stable ID and reason.
- **Removal under serve:** after `git worktree remove`, a still-running serve
  surfaces `source-root-missing` rather than the last staleness verdict.
- **Confinement (the safety-critical suite):** a symlinked `worktrees/`
  component refuses; a symlink inside a candidate refuses; a bind mount beneath
  a candidate refuses; a non-matching directory name is never a candidate; a
  sibling `.weft/filigree/` path is unreachable from the sweep under every
  malformed-input case.
- **Gates:** the CI floor in `CLAUDE.md` — fmt, clippy pedantic, workspace
  build, nextest, rustdoc, cargo-deny.

## Acceptance criteria

1. A linked worktree's graph reflects that worktree's HEAD and dirty files.
2. The main checkout's store path and behavior are unchanged.
3. Worktrees analyze and serve concurrently without sharing a SQLite writer.
4. `serve` on an unbuilt worktree becomes usable in the same session.
5. `loomweave worktree analyze` builds or rebuilds explicitly.
6. A removed worktree's store is reclaimed on the next sweep.
7. No deletion can reach outside `<repository-store>/worktrees/`; under a
   `[loomweave].store_dir` override the sweep deletes nothing (report-only).
   Confinement guarantees are claimed only for a store namespace the sweeping
   repository exclusively owns.
