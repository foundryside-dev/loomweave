# Clarion dogfood — remediation plan (2026-05-29)

Companion to [`clarion-dogfood-eval-2026-05-29.md`](clarion-dogfood-eval-2026-05-29.md).
Maps each confirmed finding to a fix and an owner ticket. Most findings were
*already anticipated* by the dogfood epic **clarion-8fe3060d4c** (31 children);
this plan records which tickets cover what, what's genuinely new, and what was
a test-harness artifact rather than a Clarion defect.

## Status legend
- **DONE** — already fixed in HEAD (verify after the served binary is rebuilt).
- **TICKETED** — an existing Filigree issue covers it; may need scope tightening.
- **GAP** — no ticket yet; file one (proposed below).
- **ARTIFACT** — caused by the eval staging, not Clarion; re-test, don't fix.

---

## A. Already resolved in HEAD (rebuild + restart to expose)

### A1. Provenance / `project_status` — **DONE** (Finding #1, the headline)
- **Commit:** `5d4aeaa feat(mcp): add project_status diagnostics tool + live Filigree URL resolution`. Ticket **clarion-084e82250c** (status: building).
- Returns repo root, db path, latest run (id/status/started/completed), entity/
  subsystem/edge/finding counts, index staleness, per-plugin entity counts, LLM
  policy (provider/live/cache), and the **resolved** Filigree endpoint. No LLM call.
- **Why the agent missed it:** the live MCP server was the pre-`5d4aeaa` binary, so
  the tool wasn't in the registered list — the exact blindness `project_status`
  exists to cure. **Action:** rebuild `target/release/clarion`, restart MCP,
  re-verify; then close clarion-084e82250c.
- **Bonus:** the same commit single-sources Filigree URL resolution at the
  `FiligreeHttpClient` construction site, fixing the stale-port path that made
  `issues_for` unreachable in the eval. Feeds clarion-318f1254eb.

---

## B. Confirmed, corpus-independent — fix these (ranked by user pain)

### B1. `execution_paths_from` output unusable by an LLM consumer — **TICKETED**
- **Finding:** 71 *correct* paths returned as a 124 KB single-line blob (every
  node fully re-serialized per path), over the MCP cap, dumped to a file the
  consult agent never receives. The one tool that answers "how does this flow"
  is the one whose output can't be ingested.
- **Ticket:** **clarion-5b3eff9a91** "Add compact ranked execution paths mode" (P2, proposed).
- **Fix:** emit a `nodes` table keyed by id (each node **once**: `id` +
  `short_name` + line span only — drop `content_hash` and absolute
  `source_file_path`), then `paths` as arrays of node-ids (or an adjacency list +
  `roots`/`leaves`). Add the "ranked" half so the top load-bearing paths come
  first under the cap. Target: the `run` flow fits in a few KB.
- **Scope add:** while here, make truncation honest — see B5.

### B2. `summary` bills on deterministic failure, no structural fallback — **GAP**
- **Finding:** `summary(Orchestrator class)` failed twice with `llm-invalid-json`,
  charged `summary_cost_usd 0.015225` **each** (`stats_delta`), wrote no cache
  entry. Same input → same paid failure on every retry; a consult agent loops and
  pays. Distinct from clarion-bacd53a2ad (`summary_preview_cost`), which only
  estimates *before* a call and does nothing for the fail-and-bill loop.
- **Ticket:** none for the fail-cheap behavior. **File a P2 bug** (proposed title:
  *"summary bills on llm-invalid-json with no cache + no structural fallback"*).
- **Fix:** (a) on `llm-invalid-json`, fall back to a deterministic **structural**
  summary (members, signatures, docstring) instead of returning an error — this is
  also strictly better output; (b) cap/chunk oversized entities before the LLM
  call so the prompt-budget overflow that causes the invalid JSON can't happen;
  (c) never bill twice for an identical deterministic failure (negative-cache the
  failure, or don't bill a non-result). Related but not a substitute:
  clarion-bacd53a2ad (preview), clarion-bacd…/spend controls are out of the epic's
  deterministic scope per the epic note.

### B3. Incompleteness is invisible (`[]` reads as "none") — **PARTIALLY TICKETED → GAP**
- **Finding:** `callers_of`/`neighborhood` return a clean `[]` for *known* scope
  limits — attribute-receiver calls (`ctx.orchestrator.resume()`, `cli.py:1548`)
  and module-level reference rollup — indistinguishable from a true negative. A
  senior reads "nothing calls this → safe to change." Verified the graph is
  *correct where it answers* (11 real `_emit_telemetry` callers; correct
  `ResumePoint` reference edges), so this is a **UX/contract bug, not a graph bug**.
- **Tickets touching the area:** clarion-9392f74881 (call_sites evidence),
  clarion-893c46cc5f (clarion://context degraded-snapshot signal). Neither adds a
  per-result `scope_excludes` flag.
- **Fix (file a P2):** every `callers_of`/`neighborhood`/`execution_paths_from`
  result carries a `scope_excludes: [...]` array naming what was *not* searched
  (e.g. `"attribute-receiver-calls"`, `"module-level-reference-rollup"`,
  `"cross-module-imports"`). An empty set that's silently incomplete is worse than
  no answer. Cheap, high-leverage, corpus-independent.

### B4. No module-altitude / upstream-import queries — **TICKETED**
- **Finding:** references exist symbol-to-symbol, but "who imports this
  module/contract?" answers `[]` because module entities don't aggregate their
  symbols' edges and there's no reverse-import lookup. A senior thinks in modules
  first.
- **Tickets:** **clarion-923cf62b2c** "No module->subsystem reverse lookup" (P2)
  covers the subsystem direction; the **module reference rollup + reverse-import**
  query is adjacent and may need its own scope line on clarion-9392f74881 or a new
  child. **Action:** confirm coverage on clarion-923cf62b2c; if it's subsystem-only,
  add a sibling for module-level reference rollup.

### B5. `execution_paths_from` exceeds transport budget without `truncated:true` — **GAP**
- **Finding:** 124,036-char response with `truncated:false` / `truncation_reason:null`
  — the truncation contract the tool description advertises didn't fire; the MCP
  layer truncated out-of-band. Whatever B1 lands, the truncation signal must be
  honest. **File as part of clarion-5b3eff9a91** (acceptance criterion: any response
  trimmed for the cap sets `truncated` + `truncation_reason`).

### B6. Subsystem discoverability & labels — **MOSTLY TICKETED**
- **Findings:** no "list all subsystems" entry point (`find_entity("subsystem")` is
  FTS and misses namespace-named clusters); opaque hash names
  (`Subsystem 9d59f183f130`).
- **Tickets:** **clarion-aaa25a4f10** "Improve subsystem name UX: derive from common
  module prefix" is **closed** — verify the served binary actually carries it
  (eval still saw hash names, but on the contaminated corpus). **clarion-bccfcd4c49**
  "find_entity has no kind filter" partly helps discovery. **Gap:** an explicit
  `list_subsystems` enumerator — fold into clarion-599a34d40a (orientation_pack) or
  file a small P2.

### B7. Aggregating (non-leaf) summaries — **DESIGN-DEFERRED**
- **Finding:** `summary` is leaf-scope by design (v0.1), so there's no
  "summarize this subsystem/package" — the altitude a senior starts at.
- **Action:** not a bug; a roadmap item. Track under clarion-599a34d40a
  (orientation_pack) or a dedicated post-1.0 feature; note the dependency on
  subsystem labels (B6) and module rollup (B4).

---

## C. Needs re-test on the real corpus (contaminated by the eval staging)

### C1. `entity_at` "dead" — **ARTIFACT + likely-real fragility, RE-TEST**
- **Eval saw:** every call errored — input path "escapes project root
  `/tmp/clarion-b8-elspeth-full-…`" while the DB stored
  `/home/john/clarion/tests/perf/.../core.py`. That `/tmp` root was the eval's dead
  `serve --config` project_root, **not** a property of a correctly-served instance.
- **Re-test** with `--path /home/john/elspeth` (project_root matches the stored
  `/home/john/elspeth/...` paths). If it still rejects valid in-tree paths, it's a
  real normalization bug → file against clarion-460def6a51's area. Until re-tested,
  do not pin "100% broken" on the tool.

### C2. Subsystem clustering quality (modularity 0.093) — **ARTIFACT, RE-TEST**
- **Eval saw** near-random clusters mixing Clarion's own source with the
  `elspeth_mini` fixture — because the served DB was Clarion's 1872-entity
  self-analysis, not elspeth. Real elspeth has **134** subsystems over 36,814
  entities. Re-run subsystem missions there before judging cluster quality.

### C3. "Corpus contamination / analysis had no scoping" — **WITHDRAWN (ARTIFACT)**
- Not a Clarion bug. The real elspeth analysis is correctly scoped (verified:
  36,680 elspeth entities, 0 fixture/clarion rows). The wrong DB was *served*, via
  the staging artifact below. Remove from the bug list.

### C4. `serve` keeps serving a deleted DB inode — **MINOR ROBUSTNESS, OPTIONAL**
- **Root cause of the whole corpus mix-up:** the configured DB was hot-swapped
  (`rm` + sqlite `.backup` → new inode) under a running server; the connection pool
  held the old, now-unlinked inode and silently kept serving it. Normal operation
  restarts `serve`, so this is low priority — but a periodic `stat`/`PRAGMA
  data_version` check (or surfacing inode/db identity in `project_status`) would let
  an agent notice. **Optional P3**; `project_status` (A1) already mitigates by making
  the served corpus inspectable.

---

## D. Execution order

1. **Rebuild + restart MCP**, then re-verify A1 (`project_status`) and close
   clarion-084e82250c. (Unblocks honest re-testing of everything else.)
2. **B3 + B5** — `scope_excludes` + honest truncation. Small, corpus-independent,
   highest trust-per-line-of-code. (B5 rides clarion-5b3eff9a91; B3 = new P2.)
3. **B1** — compact ranked execution paths (clarion-5b3eff9a91).
4. **B2** — summary fail-cheap + structural fallback (new P2 bug).
5. **C1/C2** re-test on `--path /home/john/elspeth`; convert survivors to bugs.
6. **B4/B6/B7** — module-altitude queries, `list_subsystems`, aggregating summaries
   (roadmap; partly under clarion-923cf62b2c / clarion-599a34d40a).

## E. New tickets to file (gaps with no current owner)
- **P2 bug:** summary bills on `llm-invalid-json`, no cache, no structural fallback (B2).
- **P2 feature:** `scope_excludes` on every graph-query result (B3).
- **P3 task:** honest `truncated` flag on capped `execution_paths_from` (B5; or AC on clarion-5b3eff9a91).
- **P2 (if not covered):** module-level reference rollup + reverse-import query (B4).
- **P3 (optional):** `serve` detects a swapped/deleted DB (C4).
