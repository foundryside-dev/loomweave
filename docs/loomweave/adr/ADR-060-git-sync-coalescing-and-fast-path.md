# ADR-060: Git-Sync Refresh Discipline — Fast Path, Coalescing, Hook Set, Degrade-Not-Fail

**Status**: Accepted
**Date**: 2026-08-31
**Deciders**: john@foundryside.dev
**Context**: clarion-78d75e45c9 (salvage worklist B1; the cause behind C5, the "can I trust the index" hesitation). On a shared checkout the managed `post-commit` / `post-checkout` / `post-merge` hooks produced 12–148 analyze runs a day over ten days, with an 11–17 s floor and a 25–67 s typical cost for a one-file (or zero-file) change; 9 of 39 runs on 2026-08-29 failed with "plugin python exceeded the per-file analysis timeout (120000 ms) and was killed", each persisting nothing (`entities_inserted: 0`) so the next hook redid the whole refresh.

## Summary

Four decisions, one PR. (1) `analyze` settles a run without a walk when the only drift is the commit clock and the committed range touched nothing the pipeline ingests or reads — the **no-indexed-changes fast path**. (2) A hook that finds the analyze lock held **queues** a refresh (a marker beside the lock) that the running analyze drains on exit, instead of forking a child doomed to lose the lock — a burst of N events costs two runs. (3) The managed hook set is **`post-merge` + `post-checkout` (branch switches only)**; `post-commit` is retired and its block removed on the next install. (4) A per-file watchdog kill **degrades the file, not the run**: the plugin is respawned and continues, bounded to three kills per plugin per run.

## Context

- The freshness oracle (`index_diff::compute_freshness`) treats `HEAD != analyzed_at_commit` as drift, and every completed run stamps `analyzed_at_commit`. So every commit — docs, CI config, a changelog line — makes the index read `stale`, and the only thing that cleared it was a full incremental run: secret-scan walk of ~3,100 files, plugin dispatch, a ~14 s Leiden clustering pass, the SEI mint.
- `post-commit` adds nothing the file-drift channel does not already carry: a commit does not change file content, it moves the commit clock. Refreshing on merge and branch switch (bulk content moves), on session start, and on demand (`analyze_start`) covers what agents actually consult.
- The plugin side of the watchdog contract already existed (ADR-050/057: 90 s plugin cap under the 120 s host deadline, restart headroom); the host still treated one kill as a plugin crash and failed the run.

## Decision

1. **Fast path.** In incremental mode, after the lock and config resolution and before any walk, `analyze` asks the same oracle `project_status_get` reads (`loomweave_mcp::commit_only_drift`): if the verdict is *drift, commit channel only* — no in-place modification of an indexed file, no staged change touching an indexed path, no untracked source of an ingested type, and no observation blindness (stat failures, truncated scan, unparsable run timestamp, unknown commits) — it runs `git diff --name-only <analyzed>..HEAD` (hardened, read-only) and, when no path has an ingested extension and none is an analyzer input (`loomweave.yaml`, the secrets baseline, `.env*` sidecars), records a **completed run at HEAD** carrying the base run's stats with the per-run insertion counters zeroed and a `fast_path` block (`reason`, `base_run_id`, `from_commit`, `to_commit`, `paths_changed`). `--no-incremental` and `--resume` bypass it. Blind never means unchanged: every `None` from the oracle runs the full pipeline.
2. **Coalescing.** The pending marker is `<lock path>` with extension `pending` (so linked worktrees queue independently). A hook that finds the lock held touches it and says so ("a follow-up refresh is queued"). `analyze` **consumes** the marker right after taking the lock (this run is the refresh the request wanted) and, after its run completes, **drains** a marker that appeared meanwhile by running again — at most two follow-ups per process (`MAX_PENDING_FOLLOW_UPS`), with a fresh run id and no progress file (an MCP `analyze_start` caller tracks the run it asked for, not the drain).
3. **Hook set.** `GIT_SYNC_HOOKS = [post-checkout, post-merge]`; `RETIRED_GIT_SYNC_HOOKS = [post-commit]`. The `post-checkout` block is wrapped in `if [ "${3:-1}" = "1" ]` (git's branch-switch flag; a file checkout passes 0). `install`/`doctor --fix` remove Loomweave's block from retired hooks under the cede discipline (only our bytes; a file left with nothing but the shebang is deleted); `doctor` reports a lingering retired block as `Stale`.
4. **Degrade, not fail.** In the plugin file loop, an `analyze_file` error while the watchdog's expired phase is `File` skips that file — its stored rows are retained and its content hash is not advanced, so it re-dispatches next run — records the existing `LMWV-PY-TIMEOUT` finding with `phase=file`, `file=<path>`, `skipped_file=true`, tears the dead plugin down (our own SIGKILL, so never classified as an OOM event), spawns and handshakes a fresh one, and continues. Past `MAX_FILE_TIMEOUT_RESPAWNS = 3` per plugin per run the next kill is terminal, exactly as before. The crash-loop breaker is not ticked (a kill is the host's doing, not a plugin crash).

## Alternatives Considered

### Alternative 1: Re-stamp `analyzed_at_commit` from the hook instead of recording a run

**Pros**: No run row per docs commit.
**Cons**: A write path outside the writer actor and the analyze lock; the runs table stops being the history of what settled the index; `index_diff_get.latest_run` would name a run whose `analyzed_at_commit` it never had.
**Why rejected**: The run row is cheap and honest — `fast_path` says exactly what happened and which run's index it carries.

### Alternative 2: Keep `post-commit`, rely on the fast path to make it cheap

**Pros**: The index is never `stale` after a commit.
**Cons**: Source commits still cost a full run per commit; a developer committing every few minutes still produces the storm the ticket measured, merely coalesced. The staleness a commit introduces is settled in ~1 s on demand anyway.
**Why rejected**: The hook's job is to catch bulk content moves the agent did not author; per-commit refresh is the wrong granularity for that.

### Alternative 3: Skip the timed-out file without respawning (mark it and move on)

**Cons**: The watchdog already killed the process; there is nothing to move on with.
**Why rejected**: Not viable — respawn is the only way to continue.

### Alternative 4: Make the plugin itself return partial evidence before the host deadline (no host change)

**Pros**: Already the plugin's contract (ADR-057).
**Cons**: The failures observed were exactly the cases where that contract did not hold — the host must be robust to a plugin that misses its own deadline.
**Why rejected**: Defence in depth; both layers stay.

## Consequences

### Positive

- Docs/config/CI commits cost about a second; a burst of events costs two runs; a shared checkout stops re-scanning an unchanged tree dozens of times a day.
- One pathological file no longer forfeits a whole refresh; the other files' evidence lands and the file is named.
- `doctor` and `install` converge existing installs to the new hook set without touching foreign hook content.

### Negative

- After a source commit the index reads `stale` until a merge, branch switch, session start, or `analyze_start` — by design, and cheap.
- A fast-path run carries the base run's `classifier_coverage`/resolution stats forward; consumers comparing run-over-run deltas must key on `fast_path` presence.
- A skipped file's stale rows persist until a run that survives it; `entity_finding_list` on the file surfaces the timeout finding.

### Neutral

- The fast path is derived from the shared freshness oracle, so it can never disagree with `project_status_get`; it only *adds* the committed-range check on top.
- The pending marker is content-free; a marker orphaned by a killed process is consumed by the next run.

## Related Decisions

- **Related to**: [ADR-050](./ADR-050-plugin-lifecycle-deadlines.md) (the per-file watchdog this ADR makes recoverable), [ADR-057](./ADR-057-pyright-restart-attribution.md) (plugin-side deadline discipline), [ADR-045](./ADR-045-worktree-source-staleness.md) (the untracked-source channel the fast path respects), the hook cede discipline (clarion-3fbb9cdfcd / clarion-c379a8c9ee).

## References

- `crates/loomweave-cli/src/analyze/fast_path.rs`, `crates/loomweave-mcp/src/index_diff.rs` (`commit_only_drift`) — the fast path.
- `crates/loomweave-cli/src/analyze_lock.rs` (pending marker), `hook.rs`, `analyze.rs` (`run_with_options_draining_pending`) — coalescing.
- `crates/loomweave-cli/src/git_hooks.rs` — hook set and retired-block removal.
- `crates/loomweave-cli/src/analyze.rs` (`LivePlugin`, `spawn_live_plugin`, `MAX_FILE_TIMEOUT_RESPAWNS`) — degrade-not-fail; `tests/analyze_hardening.rs` and `tests/analyze_fast_path.rs` — the pins.
