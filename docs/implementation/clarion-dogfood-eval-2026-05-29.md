# Clarion dogfood evaluation — senior-user verdict (2026-05-29)

Evaluator: senior engineer, day-one orientation via the 8 MCP query tools only.
Read-only. Every claim below is cited to an actual MCP call or a source `Read`.

> **Corpus caveat (this is Finding #1, read it first).** The live MCP server is
> **not** serving the 425k-LOC elspeth the brief described. It is serving Clarion's
> own repo DB (`/home/john/clarion/.clarion/clarion.db`, 1872 entities), which
> swept together the `elspeth_mini` test fixture (`tests/perf/elspeth_mini/…`,
> ~30k LOC / 80 files), Clarion's *own* plugin source, and a `.env` file. The
> real elspeth DB (36,814 entities, 134 subsystems, 259 MB) exists at
> `/home/john/elspeth/.clarion/clarion.db` but the served tools cannot reach it.
> I evaluated the tool surface against what is actually served, per the only
> substrate the 8 tools expose. Clustering/scale findings are therefore partly
> corpus-dependent and flagged as such; tool-mechanic findings hold regardless.

> **Maintainer reconciliation (added post-run, evidence-verified).** The corpus
> mismatch above is an *operational staging artifact*, not a Clarion
> analysis-scoping defect — and the way it happened is itself a finding. The MCP
> server's configured DB path had been wiped from `/tmp`; the DB was restored by
> `rm` + sqlite `.backup` (a **new inode**) while the server was still running.
> The server held a pooled connection to the **old, now-unlinked inode** (a
> Clarion self-analysis DB) and silently kept serving it. On-disk the path now
> holds a clean, properly-scoped **real elspeth** snapshot (36,680 elspeth
> entities + 134 subsystems, 0 fixture/clarion rows — verified by direct
> `sqlite3`), but the live tools never saw it. So:
> - **Finding #5 ("corpus contamination / analysis had no scoping") is withdrawn**
>   as a Clarion bug — the real elspeth analysis *is* scoped correctly; the wrong
>   DB was simply served. It is re-cast as: *a running `serve` keeps serving a
>   deleted DB inode with no detection* (minor robustness note) + further proof of
>   the provenance gap (Finding #1).
> - **Findings #2 (`entity_at` root mismatch) and the subsystem-quality /
>   modularity-0.093 verdict are corpus-contaminated** and need re-test against
>   the real-elspeth DB (proper `project_root=/home/john/elspeth`, 134 real
>   subsystems) before they can be pinned on the tool. Re-run requires an MCP/
>   session restart so the server opens the fresh inode.
> - **Everything else holds corpus-independently** and is the real harvest:
>   `execution_paths_from` token blob, `summary` billing-on-failure + no
>   structural fallback, invisible incompleteness (`scope_excludes`),
>   attribute-receiver caller blind spot, and the missing-capability wishlist
>   (`project_status`, `list_subsystems`, module-altitude/upstream-import queries,
>   aggregating summaries).
> - **Finding #1 (provenance) is the headline and is now doubly validated:** a
>   careful evaluating agent *and* the operator both lost track of which corpus was
>   live. If a `project_status` tool existed, neither would have.

---

## 1. TL;DR

The leaf-`summary` tool is genuinely good — its checkpoint-module briefing was
accurate, well-structured, and saved me a real read. But as a day-one
orientation surface the tool is **not yet trustworthy**, for three reasons I hit
within the first ten calls: (1) I could not tell *what corpus I was even
querying* from inside the tool surface — I had to drop to `sqlite3` to discover
the server is pointed at a fixture, not elspeth; (2) the graph is *accurate
where it claims an edge* (verified: 11 real callers of `_emit_telemetry`,
class-to-class reference edges on `ResumePoint`) but has hard **scope blind
spots** — attribute-receiver method calls (`ctx.orchestrator.resume()`) are
invisible at every confidence level, and references live only at symbol
granularity so module-level "who imports me" answers `[]` — and *nothing in the
output tells the agent it's looking at a blind spot rather than a true negative*;
(3) `entity_at` is
**dead** on this DB and `execution_paths_from` **blows the token cap** with a
128 KB dump the consult-agent never receives. Would I reach for it again? For
single-entity summaries, yes. For blast-radius or flow questions, not until the
edge graph and a `project_status` are real. **The single biggest thing it needs:
a `project_status`/provenance tool so an agent knows what it's looking at and
whether to trust it.**

---

## 2. Missions run

### Mission A — "Where does checkpointing live and how does it hang together?"
- **Chain:** `find_entity("checkpoint")` → `summary(contracts.checkpoint)` →
  `neighborhood(contracts.checkpoint)` → `subsystem_members` on both hash-named
  subsystems.
- **Succeeded partially.** `summary` was the win: it correctly described
  `ResumeCheck`/`ResumePoint`, named the real collaborators
  (`RecoveryManager`, `CheckpointCompatibilityValidator`, `Orchestrator.resume`),
  and even flagged a real redundancy risk (token_id/node_id duplicated between
  `ResumePoint` and the embedded `Checkpoint`). I verified all of that against
  `contracts/checkpoint.py` — accurate.
- **Fell back to grep for upstream "who imports checkpoint."** `neighborhood`
  on the *module* reported `references_in: []` / `references_out: []`, reading as
  "nothing imports checkpoint." But this is a **granularity** effect, not a
  dropped edge: re-querying `neighborhood(class ResumePoint)` shows references
  ARE tracked symbol-to-symbol — `references_out` lists `audit.Checkpoint`,
  `AggregationCheckpointState`, `CoalesceCheckpointState` (all `resolved`, with
  byte offsets), matching source. So downstream wiring is queryable per-class.
  What's missing is (a) a *module rollup* of contained-symbol references, and
  (b) the upstream direction — `contracts/__init__.py:69 from … import
  ResumeCheck, ResumePoint` did not appear as a `references_in` on the
  `ResumePoint` class either, so "who imports this contract" still required
  `grep -rn`.
- **Subsystem layer was useless for orientation.** `subsystem_members` works
  mechanically, but the two clusters it returned lump Clarion's own
  `clarion_plugin_python.extractor`/`server`/`stdout_guard` in *with* elspeth
  contracts modules (modularity_score `0.093` — near-random). Names are opaque
  hashes (`Subsystem 9d59f183f130`). "What subsystem owns checkpointing?" had
  no meaningful answer.

### Mission B — "If I change `Orchestrator.resume`, what breaks?"
- **Chain:** `find_entity("Orchestrator")` → `callers_of(Orchestrator.resume)`
  at `resolved` then `ambiguous` → cross-check with grep.
- **Failed for this entity — but the tool itself works.** `callers_of(resume)`
  returned `{"callers":[]}` at **both** `resolved` and `ambiguous`. Source has a
  real caller: `cli.py:1548: return ctx.orchestrator.resume(...)`. I verified
  `callers_of` is *not* broken by running a reverse-edge consistency check:
  `callers_of(_emit_telemetry)` returned **11 real callers** (`run`, `resume`,
  `_emit_failed_ceremony`, `_execute_export_phase`, …), all `resolved` with byte
  offsets, every one confirmed in source. So direct `self.`-calls resolve
  correctly; the `resume` miss is purely the **attribute-receiver** limitation —
  `ctx.orchestrator.resume()` can't be bound to the class statically, and even
  `ambiguous` doesn't surface it as a candidate. A senior asking "blast radius"
  still got the dangerous answer "nothing calls this," and *nothing flagged that
  this was a known-incomplete case rather than a true negative*. I got the real
  answer from grep.

### Mission C — "How does a run flow through the system?"
- **Chain:** `execution_paths_from(Orchestrator.run, max_depth=4)` → verify
  against `core.py`.
- **Data correct, delivery broken.** The 71 paths it computed are *real*: `run
  → _emit_failed_ceremony → _emit_telemetry`, `run → _delete_checkpoints`,
  `run → derive_terminal_run_status`, etc. — all confirmed in
  `core.py` lines 1471–1701. So the intra-class `self.`-call graph is accurate
  and the flow is genuinely informative. **But** the response was 124,036 chars
  on one line, exceeded the MCP token cap, and got dumped to a file the consult
  agent cannot ingest. `truncated:false` — it didn't even hit its own edge cap;
  it just serialized 52 distinct nodes fully re-expanded across 71 overlapping
  paths. The one tool that answers my favourite orientation question is the one
  whose output I literally cannot receive in-band.

---

## 3. Per-tool notes

| Tool | What it's for | Delivered? | Friction (cited) |
|---|---|---|---|
| `find_entity` | FTS search over entity rows | **Yes** | Clean, paginates correctly (`cursor:"10"` → `next_cursor:"13"`). But `find_entity("subsystem")` returned only the 2 *hash*-named subsystems; the 2 namespace-named ones (`tests.perf`, `tests.perf.elspeth_mini.elspeth`, confirmed via sqlite) are invisible — subsystems aren't reliably discoverable through search. |
| `summary` | On-demand leaf briefing | **Mostly** | Best tool here: `summary(contracts.checkpoint)` was accurate + high-signal. But `summary(Orchestrator class)` failed **twice** with `llm-invalid-json` and **charged $0.0152 each time** (~$0.03 burned, nothing cached, no fallback). Large entities silently exceed the prompt budget and return prose, not JSON. |
| `entity_at` | file+line → innermost entity | **No (broken)** | Every call errors. DB normalizes paths against a dead root `/tmp/clarion-b8-elspeth-full-20260518T0016Z`; the real on-disk absolute path is rejected as "escapes project root," and the relative path errors `No such file or directory`. Unusable on this DB. |
| `neighborhood` | one-hop callers/callees/refs | **Partial** | `contained` correct. References are tracked at **symbol granularity** (`neighborhood(ResumePoint).references_out` correctly lists `audit.Checkpoint` etc.), but the **module** entity rolls up nothing (`references_in/out: []`), and **upstream** import edges (`who imports ResumePoint`) don't appear as `references_in` on the class either. So downstream-per-class works; module rollup and upstream don't. |
| `callers_of` | reverse call edges | **Works for direct calls** | Verified true-positive: `callers_of(_emit_telemetry)` → **11 real `resolved` callers** with byte offsets, all confirmed in source. Returns `[]` for attribute-receiver calls (`ctx.orchestrator.resume()` from `cli.py:1548`) at both confidences — a documented scope limit, not a bug — but the empty answer reads as "safe to change" with no flag that it's incomplete. |
| `subsystem_members` | modules in a subsystem | **Mechanically yes** | Works, but the clusters are near-random (modularity 0.093) and mix Clarion's own source with the fixture. Names are opaque hashes. Low orientation value here — likely corpus-contamination, not pure tool fault. |
| `execution_paths_from` | bounded calls-only paths | **Data yes, delivery no** | Paths are accurate and useful, but 128 KB / one line / over the token cap → dumped to a file. Format re-serializes every node in full per path. See Mission C. |
| `issues_for` | Filigree issues on an entity | **Graceful** | Filigree was down; returned a clean `available:false` / `filigree-unreachable` envelope without failing Clarion — exactly the enrich-only degradation the description promises. Could not exercise the populated path. |

---

## 4. Findings, categorized

### Friction (works but painful, most painful first)
1. **`execution_paths_from` output is unusable by an LLM consumer.** 71 paths,
   52 nodes, fully re-serialized → 128 KB over the token cap. The data is right;
   the shape defeats the entire "don't make the agent re-explore" thesis. A
   node-table + edge-list (each node once) would be a small fraction of this.
2. **`summary` charges money on hard failures and offers no fallback for big
   entities.** Two `llm-invalid-json` failures on the Orchestrator class, ~$0.03
   gone, no cache write, no degraded "here are the members" fallback. A consult
   agent will retry-and-pay in a loop.
3. **Confidence levels don't visibly change anything for the failure cases I
   hit.** `ambiguous` returned identical bytes to `resolved` for both
   `neighborhood` and `callers_of`. If the higher tier can't surface the
   unresolved `ctx.orchestrator.resume()` candidate, it's not earning its
   opt-in cost.
4. **Opaque subsystem names.** `Subsystem 9d59f183f130` tells a human nothing;
   I can't decide whether a cluster is worth opening without a label or a
   one-line synthesized description.

### Missing (capabilities a senior user needs that don't exist, ranked)
1. **`project_status` / provenance.** I could not learn from *any* tool: what
   corpus is served, how many entities, when analyzed, what project root,
   whether the source still matches the DB. I had to use `sqlite3`/`find` to
   discover I was querying a fixture, not elspeth. A blind consult agent would
   have been silently misled. **This is the headline gap.**
2. **A way to enumerate subsystems.** There is no "list all subsystems" tool;
   `find_entity("subsystem")` is FTS and misses the namespace-named ones. The
   top-down "give me the map" entry point — the most natural day-one move —
   doesn't exist.
3. **Module-level reference rollup + an upstream "who imports this" query.**
   References exist symbol-to-symbol, but "who depends on this module/contract?"
   — the core archaeology question — answers `[]` because module entities don't
   aggregate their symbols' edges and there's no reverse-import lookup. A senior
   thinks in modules first; the data is there but not queryable at that altitude.
4. **Aggregating (non-leaf) summaries.** `summary` is explicitly leaf-scope, so
   there is no "summarize this subsystem / this package" — the altitude at which
   a senior actually starts.
5. **Ranked blast-radius, not raw caller lists.** Even when `callers_of` works,
   I want "N callers, here are the load-bearing ones," not an unordered dump.

### Pointless (earned no value in real use — with justification)
- **`subsystem_members` (on this corpus only).** I read its intent — map modules
  to Leiden clusters so an agent can reason at subsystem altitude — and used it
  in earnest on both clusters. With modularity 0.093 and Clarion's own source
  mixed into the elspeth fixture, the clusters carry no real architectural
  signal, so it earned nothing *here*. I attribute this to corpus
  contamination + 30k-LOC scale, not to the tool's design; on a clean 425k-LOC
  run it could be the most valuable tool. **Not pointless by design — pointless
  on this data.**
- I found **no tool that is pointless by design.** Each of the 8 has a clear,
  defensible reason to exist; the failures above are correctness/economy/
  provenance gaps, not redundant features.

---

## 5. What I need from Clarion (prioritized wishlist to the maintainer)

1. **Ship `project_status` (new tool).** Return: served DB path, entity counts
   by kind, subsystem count, analyzed-at timestamp, project root, and a
   staleness/drift flag (does source still match `content_hash`?). Without this
   an agent cannot calibrate trust, and I cannot tell a fixture from production.
2. **Re-shape `execution_paths_from` (and any multi-node response) for token
   economy.** Emit each node **once** in a `nodes` table keyed by id, then
   `paths` as arrays of ids (or an adjacency list + a separate `roots`/`leaves`
   set). Drop `content_hash` and absolute `source_file_path` from path nodes;
   keep `id` + `short_name` + line span. Target: the `run` flow should fit in a
   few KB, not 128 KB.
3. **Make `entity_at` resilient to a moved/renamed project root.** Normalize
   against the *stored absolute `source_file_path`s*, not a dead tmp root; or
   expose the root in `project_status` so callers know what prefix to pass.
   Right now it's 100% dead on this DB.
4. **Make incompleteness visible, and widen two scope gaps.** (a) Every `[]`
   result from `callers_of`/`neighborhood` should carry a `scope_excludes` note
   (e.g. "attribute-receiver calls excluded", "no module-level rollup") so an
   agent never reads "incomplete" as "none". (b) `callers_of(...,"ambiguous")`
   should surface attribute-receiver candidates like `ctx.orchestrator.resume()`.
   (c) Add module-level reference rollup + a reverse-import lookup. An empty set
   that's silently incomplete is worse than no answer.
5. **Make `summary` fail cheap and degrade.** Cap input by chunking large
   entities; on `llm-invalid-json`, fall back to a structural summary (members,
   signatures, docstring) instead of charging and returning an error. Never bill
   twice for a deterministic failure.
6. **Human-readable subsystem labels + a one-line cluster gist**, and a
   `list_subsystems` enumerator so top-down orientation has an entry point.

---

## 6. Bugs / correctness issues (with evidence)

1. **Silent, unflagged false-negatives from documented scope limits (UX bug,
   not a graph bug).** I confirmed the graph is *correct where it answers*:
   `callers_of(_emit_telemetry)` → 11 verified callers; `neighborhood(ResumePoint)`
   → correct class-to-class reference edges. The problem is that the two
   *incomplete* cases return a clean empty result indistinguishable from a true
   negative:
   - `callers_of(Orchestrator.resume)` = `{"callers":[]}` (attribute receiver
     `ctx.orchestrator.resume()`, `cli.py:1548` — unresolvable statically).
   - `neighborhood(contracts.checkpoint).references_in` = `[]` (module rollup not
     implemented; the import `contracts/__init__.py:69` isn't surfaced upstream
     even on the class).
   Neither is a dropped edge — both are known v0.1 scope. The bug is that the
   response carries **no signal** ("excludes attribute-receiver calls" /
   "no module-level reference rollup") to stop a consult agent from reading `[]`
   as "safe to change / nothing depends on this."
2. **`entity_at` is broken by a stale project root baked into the DB.**
   Error: `escapes project root /tmp/clarion-b8-elspeth-full-20260518T0016Z`
   for the very absolute path the entity rows store
   (`/home/john/clarion/tests/perf/.../core.py`). The DB's normalization root
   and its stored `source_file_path`s are mutually inconsistent — an internal
   data bug, independent of corpus choice.
3. **`summary` bills on deterministic failure.** Two identical
   `llm-invalid-json` errors on the Orchestrator class, `summary_cost_usd`
   `0.015225` each in `stats_delta`, no cache entry written. Same input → same
   paid failure on every retry.
4. **`execution_paths_from` exceeds its transport budget without setting
   `truncated`.** 124,036-char response, `truncated:false`,
   `truncation_reason:null` — so the truncation contract the description
   advertises ("responses say when they are truncated") didn't fire; the MCP
   layer truncated it instead, out of band.
5. **Corpus contamination (provenance bug).** The served DB analyzed Clarion's
   own repo — `/home/john/clarion/.env`,
   `clarion_plugin_python/__init__.py`, `server.py` — and lumped them into
   subsystems with the elspeth_mini fixture. The analysis run had no scoping to
   the intended corpus. (Evidence: `sqlite3 … select source_file_path …` and the
   mixed `subsystem_members` output.)
