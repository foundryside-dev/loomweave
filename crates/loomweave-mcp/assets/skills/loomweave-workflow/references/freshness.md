# Loomweave index freshness and manual scanning

Relocated verbatim from `SKILL.md` (convention C-20 budget).

## Manual scanning (re-index on demand)

The graph only reflects the last `analyze`. When the working tree moves, the index
goes stale and graph answers ("what calls X", "where is X defined") describe a
*past* version of the tree. You don't have to leave the session to fix that —
re-scan in place with the `analyze_*` tools.

**The research IS the codebase, so read-only is when stale poisons you MOST.**
Every Loomweave answer is a claim *about this source tree as of the last analyze*.
When you are orienting, researching, or answering "what calls X" — read-only by
definition — a stale index means you are reading a stale version of the very thing
you were asked about. "It's just research / just orientation / I'm not editing
anything" is precisely the case where stale gives a confidently wrong answer with
nothing downstream to catch it: no compile error, no failing test, no surprising
diff. The graph simply answers about code that no longer exists that way, and the
wrong answer just *becomes* your answer. Structural questions are not exempt —
"drift is unlikely to have moved the writer-actor" is a guess about a delta you
have not looked at, dressed up as a reason not to look.

**Refresh is cheap, and you don't pay for it twice.**

- **It's incremental.** `analyze` partitions files by whole-file blake3 hash
  against the prior-index snapshot and **skips unchanged files entirely** — they
  are never re-dispatched to a plugin; their entities and edges stay put. Only
  changed/new files pay extraction cost. A re-scan after a small delta is a small
  re-scan, not a full re-walk of the corpus. (Detection is content-hash based, not
  mtime — touching a file without changing bytes costs nothing.)
- **It's non-blocking.** `analyze_start` spawns a detached child and returns a
  `run_id` immediately; the cost is borne by the child, not your tool call.
- **It may already be in flight.** The `loomweave hook session-start` hook
  auto-starts ONE detached background `analyze` when it opens on a stale index.
  An exclusive advisory lock makes any second runner a clean no-op — you cannot
  collide or corrupt the graph by starting one. So "a refresh might already be
  running" is a reason to **check** (`analyze_status_get`), never a reason to skip.

Because refresh is incremental, non-blocking, and collision-safe, there is no
"this scan is too expensive for a quick question" tradeoff to weigh.

**The rule: if `project_status_get` reports `stale` or `stale_worktree`, refresh
before you answer.** Run the cycle below to completion, then re-issue your
navigation calls against the current graph. Do not answer a graph question off a
stale index and caveat it — the caveat is not a substitute for the refresh, and
the reader cannot tell which part of your answer the drift invalidated.

The `analyze_*` tools are write-gated (the † tools above): available whenever
`serve.mcp.enable_write_tools: true`, the **default for the local agent loop**.
The cycle:

1. **Check.** `project_status_get` for the `staleness` verdict. On `fresh`,
   proceed — your answers are sound. On `stale` / `stale_worktree`, continue.
2. **Check for an in-flight run.** Call `analyze_status_get` with the `run_id`
   from a prior `analyze_start`, or just call `analyze_start {}` — if a scan is
   already running it returns `AnalyzeAlreadyRunning` and you poll that one rather
   than starting a duplicate.
3. **Start.** `analyze_start {}` launches a background pass and returns a `run_id`
   immediately — it does not block.
4. **Poll.** `analyze_status_get {"run_id": "<id>"}` until `status` is terminal
   (`completed` / `failed` / `cancelled`); the response carries progress.
5. **Cancel** (optional). `analyze_cancel {"run_id": "<id>"}` group-kills the
   plugin + Pyright.

After a `completed` run the graph is current — re-issue your `entity_find` /
navigation calls to pick up the refreshed entities and edges.

**The one valid skip — observable, not vibes.** You may answer a `stale` index
*without* refreshing only when `index_diff_get` shows the drift does not intersect
the files or entities your task touches — i.e. every changed/staged path it
reports is disjoint from the entities you are about to read or report on. That
disjointness is the *only* thing `index_diff_get` buys you here. It does NOT
overturn the `stale` verdict: both surfaces derive from the same freshness oracle
and cannot disagree, so a per-file diff is for *scoping* the drift, never for
re-litigating whether the index is fresh. (A diff that lists only a deleted-file
tombstone is not "fresh" — it still means refresh unless that path is disjoint
from your targets.) If you have not run `index_diff_get` and confirmed
disjointness, you do not have this exception; refresh. And `stale_worktree`
(untracked new source the index has never seen) can NOT be cleared this way: there
is no indexed entity to diff against, so the new module is invisible to your read
until you analyze.

### Why you refresh anyway (rationalization table)

| Excuse | Reality |
| --- | --- |
| "It's read-only / just orientation — freshness doesn't matter for reads." | Read-only is exactly when stale poisons the answer. The index IS the code under study; a stale read describes a version that no longer exists, and there is no compile error or failing test downstream to catch the wrong answer. |
| "The question is about stable architecture (which subsystem owns the writer-actor); drift won't move that." | "Unlikely" is a guess about a delta you have not examined. Run `index_diff_get` and read what actually changed; structural answers come from the same graph the drift may have altered. |
| "Re-analyzing 6000+ entities is a heavy multi-minute operation, disproportionate to a quick question." | `analyze` is incremental: unchanged files are skipped by whole-file content hash and never re-extracted, so a small delta is a small re-scan. Entity count is the index size, not the refresh cost. |
| "A background refresh is probably already in flight — starting one risks a collision." | An exclusive advisory lock makes a second runner a clean no-op; you cannot collide or corrupt the graph. "Maybe already running" is a reason to call `analyze_status_get` and wait for it, never a reason to answer off the stale index. |
| "`analyze_start` would block my turn for minutes." | `analyze_start` spawns a detached child and returns a `run_id` immediately; the cost is the child's, not your tool call's. You poll `analyze_status_get`. |
| "`index_diff_get` says `overall: fresh`, so the `stale` verdict I was handed is itself out of date — no refresh needed." | The two surfaces derive from the SAME oracle and cannot disagree. If `project_status_get` says `stale`, `index_diff_get` is reporting the same drift — use it to scope WHICH files moved, not to overturn the verdict. A tombstone-only diff is not a freshness pass. |
| "I'll answer from the current graph and just add a staleness caveat." | The caveat does not tell the reader which part of the answer the drift invalidated, and it is not a substitute for the refresh. If status is stale, refresh first, then answer. |
| "I'll refresh only if the graph returns something surprising or self-contradictory." | A stale graph returns confidently consistent wrong answers; there is no surprise to trip on. The trigger is the staleness verdict up front, not a contradiction stale data will never surface. |
| "`worktree_dirty` is false, so the drift is bounded and safe to ignore." | A clean worktree only means the changes are committed — committed deltas are exactly what re-analyze exists to absorb. Confirm disjointness with `index_diff_get` or refresh. |

### Red flags — STOP and check the staleness verdict

- Answering a graph question ("what calls X", "where is X defined", "what owns X") while `project_status_get` says `stale` or `stale_worktree`.
- Reasoning "it's read-only / research / orientation, so freshness matters less" — that is the highest-poison case, not the safest.
- Justifying a skip with a probability word about the delta ("unlikely to have moved", "probably stable", "shouldn't affect") without having run `index_diff_get`.
- Using `index_diff_get`'s `overall` field to overturn a `stale` verdict instead of to scope which files drifted.
- Citing analyze cost / entity count / "multi-minute" / "wastes work" as a reason not to refresh — the fast path is incremental.
- Treating "a refresh may already be running" as license to answer now rather than as a prompt to poll `analyze_status_get`.
- Writing a "note: the index is stale" caveat into an answer instead of refreshing.
- Deciding to refresh only if/when the graph returns something surprising.
- Skipping the refresh without being able to name the drift-vs-task-files disjointness you read off `index_diff_get`.
- Trying to clear a `stale_worktree` verdict by inspecting `index_diff_get` — untracked new source has no indexed entity to diff against; only analyze sees it.

**If `analyze_start` is missing from `tools/list`,** write tools are off for this
server. Confirm with `loomweave config check`, then either set
`serve.mcp.enable_write_tools: true` (`llm_config_set` can do this) and reconnect,
or run `loomweave analyze <path>` at the shell. A stale index you can't refresh
in-session is one you stop and flag — not one you answer over.
