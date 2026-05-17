# B.8 - Elspeth Scale-Test Plan

**Status**: PANEL-REVISED STAGE 0 DESIGN
**Date opened**: 2026-05-18
**Filigree umbrella**: `clarion-6222134e0d`
**Branch**: `sprint-2/b8-scale-test`
**Purpose**: close Sprint 2 with measured evidence from `clarion analyze` and
`clarion serve` against an elspeth-scale corpus.
**Read with**: [scope amendment](./scope-amendment-2026-05.md),
[B.4* gate results](./b4-gate-results.md),
[B.8 rollback playbook](./b8-scale-test.md), and
[B.6 MCP surface](./b6-mcp-surface.md).

This is a measurement package, not a feature package. B.8 does not touch
production code. It builds a reproducible harness, runs the scale gate, records
the result, and then decides Green / Yellow / Red using the pre-written B.8
rollback playbook.

---

## 1. Corpus Identification

Primary corpus:

```text
path: /home/john/elspeth/tests
elspeth_commit: deab8f5b21335f37e72ed70fb494a30e2c237b21
python_file_count: 1037
python_loc: 429870
```

Why this is the primary B.8 corpus: it is the local elspeth path that matches
the requested "~425k LOC / ~1,100 files" scale. `/home/john/elspeth` as a whole
is larger: 1,519 Python files and 608,930 LOC. The full checkout is useful as a
future stress target but is not the named B.8 slice.

Representativeness checks before analyze: confirm path, file count, LOC, pinned
commit, and dirty state; record any Python files Clarion excludes. B.8 measures
what Clarion actually analyzes, so the memo must distinguish "corpus LOC" from
"files accepted by Clarion".

Fallback corpus:

- If the primary corpus cannot be analyzed, use the existing
  `tests/perf/elspeth_mini/` corpus with a clearly labeled synthetic scale-up.
- A fallback run cannot disconfirm the B.4* elspeth-full extrapolation. It can
  only prove the harness and MCP surface still work.
- Fallback evidence is recorded as `ABORTED` / harness-only. It cannot produce a
  Green / Yellow / Red B.8 verdict and cannot close Sprint 2.

---

## 2. Measurement Matrix

`docs/implementation/sprint-2/b8-results.md` is append-only. Each run entry
records the fields below, even when a field is unavailable. Unknown values use
`not_collected` plus a one-line reason.

### Analyze-Time Fields

Run identity: run_id, timestamps, Clarion commit, elspeth commit, corpus path,
Python file count, Python LOC, dirty state, command line, pyright pin,
calibration machine, and operator hardware ratio.

Wall-clock: total analyze time plus phase timings for discovery, plugin
initialization, per-file analysis, and commit batches.

Resources: peak RSS for the `clarion analyze` process group, peak
`.clarion/clarion.db` size, and final `.clarion/` directory size.

Graph and B.4* comparability: corpus function count, accepted function count,
entities by kind, edges by kind and by `(kind, confidence)`, calls-edge count by
confidence, ambiguous ratio, unresolved-per-function, `dropped_edges_total`,
`ambiguous_edges_total`, `unresolved_call_sites_total`, B.4* projected seconds,
observed/projected ratio, and whether assumptions changed.

Pyright and findings: pyright per-file p50/p95 latency, restart count,
`CLA-PY-PYRIGHT-*` findings by code, `CLA-INFRA-*` findings by code, and any
other material finding count that changes gate interpretation.

### MCP-Serve-Time Fields

Per sample: pattern, request label, tool, phase (`cold_start`, `warmup`,
`steady_state`), cache state (`none`, `cold`, `warm`, `inferred`), latency,
response KB, estimated response tokens, and parsed envelope stats.

Per tool and per pattern: call count, ok count, available count, useful non-empty
result count, p50/p95/max latency, response size p50/p95 KB, response-token
p50/p95, average parent-context growth over the 20+ call consult pattern, error
count, unavailable count, truncation count, and truncation reasons.

NFR-PERF fields: initialize latency, hot-cache p95 for each storage-backed tool
against the 50ms target, summary cold-miss latency, and a concurrent
summary-miss probe proving other MCP calls are not blocked.

Summary and inference: cache hits, cache misses, cold and warm on-demand hit
rates, explicit `not_NFR_COST_02_full_validation` marker unless three unchanged
runs plus run 4 are executed, cache miss reason counts, TTL-expiry count,
`stale_semantic` count, guidance/churn invalidation count or `not_collected`
reason, inferred-edge LLM dispatch count, dispatch p50/p95 latency, coalescing
count, provider, model, rate source, prompt/completion/total tokens, cost-ceiling
outcomes, and operator cost estimate.

Filigree integration: `issues_for` HTTP request count, p50/p95 HTTP latency,
unavailable count, matched issue count, drifted association count, contained
traversal entity count, and raw transcript artifact path for all seven tools.

---

## 3. Consultation Patterns

The harness in `tests/perf/b8_scale_test/driver.py` runs manifest
`B8-MCP-001`. Target selection is deterministic:

```sql
entity_at: first 200 ranged entities ordered function, class, other, id
find_entity: first 20 short names from the same ordered entity set
callers_of: SELECT DISTINCT to_id FROM edges WHERE kind='calls' ORDER BY to_id
paths: SELECT DISTINCT from_id FROM edges WHERE kind='calls' ORDER BY from_id
summary/issues/neighborhood: SELECT id FROM entities ORDER BY id
inferred: deterministic unresolved-site candidate query, or not_applicable
```

Every request records label, tool, arguments, phase, and expected cache state.

### Light Pattern

5 fixed cold-start calls:

| Label | Tool | Cache state |
|---|---|---|
| L01-entity-at | `entity_at` | none |
| L02-find-entity | `find_entity` | none |
| L03-callers-of | `callers_of` | none |
| L04-neighborhood | `neighborhood` | none |
| L05-entity-at-repeat | `entity_at` | none |

### Medium Pattern

20 calls covering all seven tools. `medium-cold` is warm-up and includes exactly
three cold `summary()` calls at slots 5, 11, and 17. `medium-warm` repeats the
same 20 slots and summary IDs, marks those three requests `cache_state=warm`,
and is the on-demand cache sanity gate.

### Heavy Pattern

50 calls minimum, generated by repeating the same 20-slot manifest with labels
`H01`...`H50`. This is steady-state session pressure, not a full NFR-COST-02
validation unless the optional three-unchanged-runs-plus-run-4 sequence is
executed.

### Inferred-Edge Pattern

Up to five `callers_of(id, confidence="inferred")` calls from deterministic
unresolved-site targets, plus `execution_paths_from(..., confidence="inferred")`
when a path root exists. Records dispatch count, coalescing count, latency, and
tokens. If no unresolved sites exist, records `not_applicable`.

---

## 4. Decision Criteria

B.8 uses the pre-written [rollback playbook](./b8-scale-test.md). The gate
verdict is not allowed to drift after seeing the data.

### Green

From the playbook: the elspeth run completes inside the v0.1 scale envelope,
MCP smoke checks return useful bounded responses, and calls edges include
resolved rows plus bounded ambiguous rows without pathological
`ambiguous_edges_total` or `unresolved_call_sites_total` growth.

Operational thresholds: primary elspeth-slice run only; analyze wall-clock is
<=60 minutes per `NFR-PERF-01`; observed/projected ratio does not materially
contradict B.4*; resolved calls rows exist; ambiguous ratio and
unresolved-per-function are bounded; initialize is <=100ms; hot-cache p95 for
storage-backed tools is <=50ms with all storage-backed steady-state p95 values
<500ms; warm on-demand summary hit rate is >=95 percent; provider/model token
use and external cost estimate fit the reframed on-demand interpretation of
`NFR-COST-01`; all seven tools have useful non-empty transcript evidence.

### Yellow

From the playbook: the run completes, but one or more signals are outside the
comfortable envelope: wall-clock is materially above projection, memory
pressure is high but not fatal, MCP traversal latency is high on call-heavy
entities, ambiguous-edge volume is too noisy for default workflows, or summary
cost exceeds the current cost hypothesis while still being containable.

Operational thresholds: analyze completes but exceeds the B.4* extrapolation by
a material factor or approaches the 60-minute ceiling; hot-cache p95 misses the
50ms NFR target, or storage-backed p95 is 500ms to 2s while still answering;
ambiguous volume or unresolved-per-function is noisy but usable; warm on-demand
summary hit rate is below 95 percent but narrow enough to mitigate after Sprint
2; provider/model token use is higher than expected but bounded by the session
token ceiling.

Allowed Yellow actions, from the playbook:

- Add per-file or per-function call-resolution caching keyed by file content
  hash and pyright pin.
- Add measured parallelism with multiple pyright sessions only after recording
  RSS and init overhead.
- Narrow B.8 acceptance to the representative elspeth slice for Sprint 2 close,
  while opening a follow-up optimization issue for full elspeth before v0.1 GA.
- Add query-side caps for `confidence >= ambiguous` traversals in MCP
  responses.
- Defer summary prewarming or broad summary smoke tests if calls/references
  navigation is healthy but LLM cost is the yellow signal.

### Red

From the playbook: the run does not complete, the store is unusable, MCP smoke
checks cannot answer basic navigation questions, calls-edge extraction
dominates the run beyond sprint repair, or confidence semantics are violated
in persisted data.

Operational thresholds: analyze fails to complete, the store is unusable,
calls-edge extraction dominates beyond sprint repair, or the run exceeds 60
minutes in a way that makes Sprint 2 close dishonest; basic navigation is
unusable, including >2s p95 only when it prevents useful answers; one or more of
the seven MCP tools cannot return a meaningful response; confidence semantics
are violated in persisted `edges` rows.

Required Red action, from the playbook:

- Treat Red as a scope decision, not an in-sprint tuning chore.
- Choose one path explicitly before implementation resumes:
  ship v0.1 MCP navigation without scan-time `calls` edges, ship a narrowed
  resolved-only calls mode, redesign the resolver as AST-first with pyright
  only for selected ambiguous sites, or defer the full elspeth proof and close
  only a slice demo under an explicit scope amendment.

---

## 5. Result Recording Format

Create append-only `docs/implementation/sprint-2/b8-results.md`. Newest run
goes at the top, and each entry starts `## <timestamp> -
<GREEN|YELLOW|RED|ABORTED>` followed by: Gate Verdict, Corpus, Analyze-Time
Measurements, MCP-Serve Measurements, Cost Estimate, B.4* Extrapolation Check,
Rollback Action, and Raw Artifacts.

---

## 6. Harness Contract

The driver lives at `tests/perf/b8_scale_test/driver.py`.

Responsibilities: spawn `clarion serve --path <analyzed-project>`, send
Content-Length framed JSON-RPC, exercise `initialize`, `tools/list`, and all
seven `tools/call` requests, run manifest `B8-MCP-001`, record per-call
latency/size/token/envelope stats, summarize by tool/pattern/phase/cache state,
preserve raw request labels and entity IDs, assert miss-then-hit summary smoke
before scale, and write structured JSON to `--output`.

Non-responsibilities: no production-code edits, no hiding unavailable envelopes,
and no automatic gate decision. The memo author applies the playbook to the
recorded evidence.

---

## 7. Reviewer Panel Record

Three reviewers review this plan before Stage 1 is treated as locked.

| Reviewer | Focus | Verdict | Notes |
|---|---|---|---|
| architecture | Measurement matrix covers the decisions the gate hinges on | ACCEPT-WITH-CHANGES | Added B.4* comparability, NFR-PERF-02/03, cache semantics, provider/rate-source fields, per-tool usefulness counts, and transcript artifact path. |
| quality | Consultation patterns are reproducible and avoid measuring warm-up as steady state | ACCEPT-WITH-CHANGES | Added manifest `B8-MCP-001`, deterministic target queries, phase/cache-state fields, and miss-then-hit summary smoke requirement. |
| systems | Green / Yellow / Red boundaries match the playbook and do not drift | ACCEPT-WITH-CHANGES | Closed fallback verdict loophole, restored call-edge counter boundaries, and restored unusable-store / calls-dominates Red triggers. |

Panel result: all required changes folded into this revision.

---

## 8. Stage Gates

Stage 1 - harness: add focused tests for statistics aggregation and MCP
response parsing before driver implementation, then smoke it against a small
analyzed project before elspeth scale.

Stage 2 - analyze: source `.env` only when live LLM calls are needed, run
`clarion analyze` against the pinned corpus, and stop before Stage 3 if analyze
is Red.

Stage 3 - serve: start `clarion serve`, run all patterns, and use the second
medium pass as the cache-hit gate.

Stage 4 - decision memo: append one entry to `b8-results.md`, state the verdict
explicitly, and choose from the playbook if Yellow or Red.

Stage 5 - Sprint 2 close: create `signoffs.md`, update the scope-amendment
status line, tag the close, and close B.8 only after the result memo and
signoff ladder are written.
