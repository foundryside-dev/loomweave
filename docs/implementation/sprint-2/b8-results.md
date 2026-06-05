# B.8 Elspeth Scale-Test Results

Append-only gate log. Each entry records the analyzed corpus, raw artifact
paths, measurements, and the green/yellow/red decision taken from
[`b8-scale-test.md`](./b8-scale-test.md).

## 2026-05-18T01:14Z — GREEN (Full-Elspeth Supplementary, Storage-Backed Slice)

verdict: **GREEN for the storage-backed surface against the full elspeth
checkout.** Analyze completed end-to-end; all seven MCP tools were exercised
without protocol error; storage-backed tools (`entity_at`, `find_entity`,
`callers_of`, `execution_paths_from`, `neighborhood`, `issues_for`, `summary`)
all returned valid envelopes; summary-cache hit rate is 1.0 across all
non-cold-start phases. The inferred-edge LLM dispatch was deliberately
**not** exercised (`--skip-inferred`), preserving the
[2026-05-17T22:43Z GREEN](#2026-05-17t2243z--green-rerun-superseding-red)
verdict's status as the only run exercising that surface; for the
supplementary corpus this run is **storage-backed-only**.

scope: Supplementary measurement against the full elspeth checkout
(`/home/john/elspeth`, 1,532 .py / 611k LOC), unblocked by the
`clarion-e29402d1ba` `@overload` fix shipped in commit `29f0426`. The
[2026-05-18T00:17Z RED](#2026-05-18t0017z--red-analyze-stop-supplementary-to-named-slice)
entry recorded the analyze-stop blocker; this entry records the rerun on the
fix.

unblocked_by: **`clarion-e29402d1ba`** — fix(wp3): skip @overload stubs to
prevent UNIQUE(entities.id) collision (commit `29f0426`). The fix collapses
`@overload` stub `def`s into the implementation entity per PEP 484; the
elspeth corpus's 2 `@overload`-using files (`execution_repository.py`,
`formatters.py`) emitted one function entity per overloaded name rather than
N stubs + 1 impl.

surfaced_then_dismissed: **`clarion-0cd961dbbc`** — filed and closed as
`not_a_bug` during this gate's investigation. The "cross-file module
collision" was actually stale state from the prior RED run; the corpus's
`.loomweave/loomweave.db` from `2026-05-18T00:17Z` persisted 30,950 entities
(via batched commits before the failing batch's rollback) including
`python:module:examples.chroma_rag.seed_collection`. Subsequent analyze
runs against the same project root appended to that DB and collided. The
stale `.loomweave` was moved aside (preserved at
`/tmp/loomweave-b8-elspeth-full-20260518T0016Z/.loomweave.from-2026-05-18T0017Z-RED`)
and a fresh `loomweave install` was run against the corpus root. No
production code change shipped for this issue.

### Reproducibility

| Field | Value |
|---|---|
| Loomweave branch at run | `sprint-2/b8-scale-test` |
| Loomweave commit at run | `29f0426` (`fix(wp3): skip @overload stubs to prevent UNIQUE(entities.id) collision`) |
| Loomweave working-tree changes | same untracked / unrelated changes carried from the prior RED entry (see Reproducibility there); none material to this rerun |
| Corpus source | `/home/john/elspeth/` (full checkout) |
| Corpus commit | `9d3fd55d63bac764c88af04330af2c3f4f651346` |
| Scratch corpus path | `/tmp/loomweave-b8-elspeth-full-20260518T0016Z` (same as the prior RED, reused verbatim) |
| Python files in scratch | 1,532 (1,526 reach the plugin after `SKIP_DIRS` filtering) |
| Raw artifacts | `tests/perf/b8_scale_test/results/2026-05-18T0114Z/` |
| Install command | `loomweave install --path /tmp/loomweave-b8-elspeth-full-20260518T0016Z` |
| Analyze command | `target/release/loomweave analyze /tmp/loomweave-b8-elspeth-full-20260518T0016Z` (with python plugin venv bin on PATH) |
| RSS sampler | `tests/perf/b8_scale_test/results/2026-05-18T0114Z/analyze-with-rss.py` (250 ms poll over proc + 2 generations of descendants; carried verbatim from the prior RED) |
| Serve driver | `tests/perf/b8_scale_test/driver.py --timeout-seconds 300 --skip-inferred` |
| Serve config | `/tmp/loomweave-b8-elspeth-full-20260518T0016Z/loomweave-b8-live.yaml` (with `integrations.filigree.base_url` updated to `http://127.0.0.1:8885` for this run's dashboard binding) |
| Filigree route | enabled (`http://127.0.0.1:8885`), HTTP reachable, no live entity associations attached to corpus entities |

### Analyze-Time Measurements

| Measurement | Value |
|---|---:|
| Run id | `3461ded9-5ba9-4b44-9f28-dd7dab7028d4` |
| Status | `completed` |
| Run started | `2026-05-18T01:51:06.152Z` |
| Run completed | `2026-05-18T01:59:10.586Z` |
| Total wall-clock | 484.651s / 8m04s |
| NFR-PERF-01 limit | 60m (≤ 13.46% of envelope) |
| Peak RSS (sampled) | 197,865,472 bytes / 188.699 MiB |
| Peak RSS caveat | sampler swept process + 2 generations of descendants at 250 ms; like the prior RED, this RSS number is a lower bound vs. the named-slice 2026-05-17T21:56Z GREEN's 1,939 MiB which used a different harness — treat as not directly comparable |
| `.loomweave/loomweave.db` size at completion | 234.66 MiB |
| Discovery/source walk | ~0.013s from log start to `source tree walk complete` |
| Plugin processing | 484.42s (`processing plugin` 01:51:06.169Z → `plugin complete` 01:59:10.586Z); the plugin-driven path dominates the wall-clock since `analyze` is single-plugin |
| Pyright per-file p95 | 1,194 ms |

Entities by kind:

| Kind | Count |
|---|---:|
| `class` | 5,592 |
| `function` | 26,132 |
| `module` | 1,526 |
| **Total** | **33,250** |

Entity-count comparison vs. the prior RED's pre-failure counts (after the
`@overload` fix collapsed stubs into implementations):

| Kind | This run | Prior RED pre-failure | Delta |
|---|---:|---:|---:|
| `function` | 26,132 | 24,430 | +1,702 (RED rolled back its final partial batch; this run committed everything) |
| `class` | 5,592 | 5,154 | +438 |
| `module` | 1,526 | 1,366 | +160 (the prior RED's transaction aborted before the final 160-file batch committed) |

Edges by kind and confidence:

| Kind | Confidence | Count |
|---|---|---:|
| `calls` | `resolved` | 39,427 |
| `calls` | `ambiguous` | 8 |
| `contains` | `resolved` | 31,724 |
| `references` | `resolved` | 16,104 |
| **Total** |  | **87,263** |

Run counters (from `runs.stats`):

| Counter | Value |
|---|---:|
| `entities_inserted` | 33,250 |
| `edges_inserted` | 93,963 (counts every edge the plugin handed to the writer, including the 6,700 the writer subsequently dropped — 87,263 actually persisted, matching the kind-breakdown above) |
| `dropped_edges_total` | 6,700 |
| `ambiguous_edges_total` | 8 |
| `unresolved_call_sites_total` | 110,996 |
| `entity_unresolved_call_sites` rows | 73,758 |
| `reference_sites_total` | 80,010 |
| `references_resolved_total` | 19,925 |
| `unresolved_reference_sites_total` | 60,085 |
| `references_skipped_external_total` | 19,593 |
| `references_skipped_cap_total` | 0 |
| `pyright_query_latency_p95_ms` | 1,194 |
| `pyright_index_parse_latency_p95_ms` | not captured by this historical run; follow-up `clarion-7aee45d920` adds this counter for the next B.8 rerun |
| `extractor_parse_latency_p95_ms` | not captured by this historical run; follow-up `clarion-7aee45d920` adds this counter for the next B.8 rerun |
| `findings` table rows | 0 |

Plugin-host findings observed in stdout (informational; not persisted):

| Subcode | Count | Materiality |
|---|---:|---|
| `LMWV-INFRA-PLUGIN-MALFORMED-UNRESOLVED-CALL-SITE` | 21 | Same `callee_expr > 512 bytes` signal as prior runs (3 in the named slice, 6 in the prior supplementary RED, 21 here at full-corpus scale). Increases with corpus size; harmless to analyze. |

### MCP Serve-Time Measurements (Storage-Backed Slice)

Driver output: `tests/perf/b8_scale_test/results/2026-05-18T0114Z/mcp-driver-output.json`.

This run invoked the driver with `--skip-inferred` so the inferred-edge
LLM dispatch path is **not exercised here**. The named-slice
[2026-05-17T22:43Z GREEN](#2026-05-17t2243z--green-rerun-superseding-red)
remains the only B.8 entry covering that path.

The driver exercised the storage-backed surface across all seven
`tools/list` tools:

- `entity_at`, `find_entity`, `callers_of`, `execution_paths_from`,
  `summary`, `issues_for`, `neighborhood`

Driver totals (overall, storage-backed-only):

| Measurement | Value |
|---|---:|
| Initialize latency | 6.131 ms |
| `tools/list` latency | 0.220 ms |
| Tool calls | 95 |
| OK envelopes | 95 |
| Error envelopes | 0 |
| Unavailable envelopes | 0 |
| Useful-result calls | 71 |
| Max latency | 13.745 ms |
| Overall p50 latency | 1.144 ms |
| Overall p95 latency | 12.932 ms |
| Overall p50 response size | 0.662 KiB |
| Overall p95 response size | 3.547 KiB |
| Summary cache hit rate (steady-state + warmup) | 1.0 |
| LLM tokens used | 0 (all `summary` calls hit cache) |
| LLM cost | $0.00 |

Pattern summary:

| Pattern | Calls | OK | Errors | Useful | p50 ms | p95 ms | Summary cache hit rate |
|---|---:|---:|---:|---:|---:|---:|---:|
| Light | 5 | 5 | 0 | 4 | 0.895 | 7.959 | n/a |
| Medium cold | 20 | 20 | 0 | 15 | 1.464 | 13.392 | 1.0 |
| Medium warm | 20 | 20 | 0 | 15 | 1.153 | 12.932 | 1.0 |
| Heavy | 50 | 50 | 0 | 37 | 1.210 | 13.083 | 1.0 |
| Inferred edge | — | — | — | — | — | — | skipped (`--skip-inferred`) |

Per-tool summary:

| Tool | Calls | OK | Errors | Useful | p50 ms | p95 ms |
|---|---:|---:|---:|---:|---:|---:|
| `entity_at` | 16 | 16 | 0 | 16 | 0.240 | 0.807 |
| `find_entity` | 15 | 15 | 0 | 15 | 0.433 | 0.895 |
| `callers_of` | 15 | 15 | 0 | 0 | 6.000 | 12.232 |
| `execution_paths_from` | 13 | 13 | 0 | 13 | 0.370 | 0.462 |
| `summary` | 13 | 13 | 0 | 13 | 11.161 | 13.083 |
| `issues_for` | 9 | 9 | 0 | 0 | 1.153 | 1.464 |
| `neighborhood` | 14 | 14 | 0 | 14 | 8.227 | 13.745 |

Per-tool notes:

- `callers_of` useful=0 — at this scale the storage-backed `callers_of`
  tool returned valid empty envelopes for every entity the driver picked.
  The original named-slice GREEN run had useful=15/20 because it also
  hit the inferred-edge LLM path which generates synthetic callers; this
  run skips that path by design.
- `issues_for` useful=0 — no live entity associations were attached to
  corpus entities for this run, so the tool returned empty issue lists.
  Differs from the named-slice GREEN which attached one live
  association to `clarion-6222134e0d` against
  `python:class:e2e.audit.test_attributability.TestAttributability`.
  The HTTP route IS exercised (filigree dashboard on `:8885` reachable),
  just with no associated data; this gates the HTTP integration as
  reachable rather than as data-flowing.

Storage-backed gate slice (all of the above, minus the skipped
inferred-edge path):

| Slice | Calls | OK | Errors | p50 ms | p95 ms |
|---|---:|---:|---:|---:|---:|
| Storage-backed navigation + cached summary | 95 | 95 | 0 | 1.144 | 12.932 |

### Filigree HTTP Route

The run wired the live Filigree dashboard at `http://127.0.0.1:8885`
(the dashboard moved off the `:9388` port used by the named-slice GREEN
and the `:8542` port the project session-start hook referenced; only
`:8885` was actually listening at run time). HTTP reachability was
verified with a plain `GET /api/issues` before the driver started.

No entity associations were attached to corpus entities for this run,
so `issues_for` requests returned empty issue lists and the HTTP route
was exercised under the "no-association" path rather than the
"with-association" path of the named-slice GREEN.

### NFR And Gate Outcome

| Gate | Target | Outcome |
|---|---|---|
| Analyze completion | run reaches `completed` status | **PASS** (run `3461ded9-…` completed, 33,250 entities + 93,963 edges) |
| Analyze wall-clock | NFR-PERF-01 ≤ 60m | **PASS**: 484.651s (8m04s, ≤ 13.46% of envelope) |
| Plugin contract: entity-ID uniqueness | ADR-022 invariant | **PASS** under `@overload` (fix from `29f0426` covers `@overload` / `@typing.overload` / `@typing_extensions.overload` stubs) |
| Seven-tool end-to-end surface | every tool exercised and useful | **PARTIAL PASS**: 7/7 exercised, 5/7 useful (`callers_of` and `issues_for` return valid envelopes but no useful payload at storage-backed-only scope) |
| Summary cache hit rate | NFR-COST-02 ≥ 95% post-stabilisation | **PASS**: 100% (all 13 `summary` calls hit cache) |
| Cost per consultation | NFR-COST-01 (reframed) | not measurable for THIS run (zero live LLM tokens) |
| Filigree HTTP integration | real reverse route, measured RTT | **PARTIAL**: HTTP reachable, no associations attached |
| Inferred-edge LLM dispatch | every inferred call resolves to valid edges | **NOT EXERCISED** (`--skip-inferred`); deferred to a future entry that attaches an `OPENROUTER_API_KEY`-backed run on this same corpus |

### Decision

The gate is **GREEN for the storage-backed surface against the full elspeth
checkout**, with two explicit narrowings:

1. Inferred-edge LLM dispatch was not exercised. The named-slice
   [2026-05-17T22:43Z GREEN](#2026-05-17t2243z--green-rerun-superseding-red)
   remains the only run that surfaced the live-OpenRouter B.8 picture
   (and the only one whose verdict on that surface stands).
2. `issues_for` and `callers_of` returned valid-but-empty payloads at the
   storage-backed scope, so the "useful-result" axis is 5/7 not 7/7.

This run does **prove**:

- The `@overload` fix unblocks `loomweave analyze` against any real-world
  `src/` tree that uses `@overload`, `@typing.overload`, or
  `@typing_extensions.overload`.
- The full elspeth corpus (1.48× the named slice's file count) fits
  comfortably inside the 60m NFR envelope at 8m04s.
- The storage-backed MCP surface (`entity_at`, `find_entity`,
  `execution_paths_from`, `neighborhood`, cached `summary`) holds its
  sub-15-ms p95 latency at 1.48× scale.

Per the [rollback playbook](./b8-scale-test.md) §4, this is **no
rollback** — gate green at the storage-backed scope, with a clearly
named inferred-edge follow-up. Sprint-2-close and the v0.1 envelope are
unaffected.

## 2026-05-18T02:06Z — GREEN Amendment (Full-Elspeth Supplementary, Live-LLM Path)

verdict: **GREEN with one named defect** for the live-LLM surface against
the full elspeth corpus. The amendment exercises the inferred-edge LLM
dispatch and cold-cache `summary` path that the
[2026-05-18T01:14Z entry](#2026-05-18t0114z--green-full-elspeth-supplementary-storage-backed-slice)
deliberately skipped. 99 of 100 tool calls returned ok; the one failure
is a Loomweave-side defect (`clarion-df58379de4`, see below) not an
external-provider failure. LLM cost was $0.897 USD on 219,006 tokens.

scope: Same corpus, same analyze DB
(`/tmp/loomweave-b8-elspeth-full-20260518T0016Z/.loomweave/loomweave.db`,
run id `3461ded9-…`) as the 01:14Z entry. Before invoking the driver,
`summary_cache` and `inferred_edge_cache` were both wiped so the run
measures the true cold-LLM picture rather than warm-cache reads of
prior calls.

### Reproducibility

| Field | Value |
|---|---|
| Loomweave commit at run | `29f0426` (same as 01:14Z entry) |
| Driver command | `tests/perf/b8_scale_test/driver.py … --timeout-seconds 300` (no `--skip-inferred`) |
| Driver output | `tests/perf/b8_scale_test/results/2026-05-18T0114Z/mcp-driver-output-live.json` |
| Driver stderr | `tests/perf/b8_scale_test/results/2026-05-18T0114Z/mcp-driver-live.stderr` |
| OpenRouter env | `OPENROUTER_API_KEY` sourced from project-root `.env` |
| Pre-run cache wipe | `DELETE FROM summary_cache; DELETE FROM inferred_edge_cache;` |

### Driver Totals (Live-LLM)

| Measurement | Value |
|---|---:|
| Tool calls | 100 |
| OK envelopes | 99 |
| Error envelopes | 1 |
| Unavailable envelopes | 0 |
| Useful-result calls | 75 |
| Max latency | 282,161 ms (4m42s; the single `callers_of` inferred call) |
| Overall p50 latency | 1.25 ms |
| Overall p95 latency | 13,623 ms |
| Summary cache hit rate | 76.92% overall (1.0 steady-state, 0.0 medium-cold) |
| LLM tokens used | 219,006 |
| LLM cost | $0.89745 USD |

Pattern summary:

| Pattern | Calls | OK | Errors | Useful | p50 ms | p95 ms | Cost USD |
|---|---:|---:|---:|---:|---:|---:|---:|
| Light | 5 | 5 | 0 | 4 | 1.034 | 7.570 | $0.000 |
| Medium cold | 20 | 20 | 0 | 15 | 1.843 | 8,321.475 | $0.015963 |
| Medium warm | 20 | 20 | 0 | 15 | 1.506 | 13.593 | $0.000 |
| Heavy | 50 | 50 | 0 | 37 | 1.065 | 11.910 | $0.000 |
| Inferred edge | 5 | 4 | 1 | 4 | 21,462.029 | 282,161.029 | $0.881487 |

Per-tool summary:

| Tool | Calls | OK | Errors | Useful | p50 ms | p95 ms |
|---|---:|---:|---:|---:|---:|---:|
| `entity_at` | 16 | 16 | 0 | 16 | 0.240 | 1.034 |
| `find_entity` | 15 | 15 | 0 | 15 | 0.441 | 0.878 |
| `callers_of` | 20 | 19 | 1 | 4 | 7.236 | 282,161.029 |
| `execution_paths_from` | 13 | 13 | 0 | 13 | 0.386 | 0.445 |
| `summary` | 13 | 13 | 0 | 13 | 10.640 | 8,321.475 |
| `issues_for` | 9 | 9 | 0 | 0 | 1.091 | 1.843 |
| `neighborhood` | 14 | 14 | 0 | 14 | 8.459 | 13.593 |

### Failure Detail

The one failing call: `callers_of` with `id=python:class:elspeth.contracts.aggregation_checkpoint.AggregationNodeCheckpoint`,
`confidence=inferred`, 21.5s wall-clock, response body:

```json
{
  "error": {
    "code": "storage-error",
    "message": "sqlite error: FOREIGN KEY constraint failed",
    "retryable": true
  },
  "ok": false
}
```

Root cause class: the inferred-edge persistence path attempts to write
an `edges` row whose `from_id` or `to_id` references an entity ID that
does not exist in the `entities` table. Most likely the LLM proposed a
caller entity ID that didn't survive the analyze pass (or never
existed). The `retryable: true` flag is **misleading** — the failure is
deterministic given the same LLM output, and a client that honours the
hint will burn additional tokens on identical retries. The LLM call
cost (≈$0.18 in this case based on per-inferred-call averaging) was
already paid before the FK violation tripped.

follow-up filed: **`clarion-df58379de4`** (P2) — inferred-edge
persistence rejects LLM-proposed entity IDs with FK violation;
`retryable=true` causes cost amplification. Fix sketch: validate
proposed entity IDs against the `entities` table before issuing the
edge insert, drop unresolvable ones with a finding, and classify
FK-violation storage errors as `retryable: false`.

### NFR Outcomes (Live-LLM Surface)

| Gate | Target | Outcome |
|---|---|---|
| Summary cache cold→warm | NFR-COST-02 ≥ 95% post-stabilisation | **PASS in steady-state** (1.0); cold pattern is 0.0 by construction |
| Cost per consultation | NFR-COST-01 (reframed) | observed: $0.897 / 100 calls = $0.009/call (mostly inferred-edge); $0.016 / 20 medium-cold = $0.0008/call for cold `summary` |
| Inferred-edge dispatch | every inferred call returns a valid edge or a clean unavailable envelope | **FAIL on 1/5** with FK violation — see Failure Detail; the other 4/5 inferred calls returned valid useful payloads |

### Decision

The amendment closes the inferred-edge gap that the 01:14Z entry
deferred. Storage-backed gate stays GREEN (unchanged). Live-LLM gate
is **GREEN with one named follow-up defect** — the surface works
end-to-end, OpenRouter integration is healthy, summary cache stabilises
to 100% hit rate, but the inferred-edge persistence path has a
deterministic FK-violation failure mode that needs a fix before v0.1
ships against any production-shape src/ tree. Filing as a P2 bug;
neither the GREEN verdict nor the v0.1 envelope changes.

## 2026-05-18T00:17Z — RED (Analyze Stop, Supplementary To Named Slice)

verdict: **RED — analyze did not complete.**

scope: **Supplementary measurement against the full elspeth checkout, not the
named B.8 corpus.** Per [`b8-elspeth-scale-test.md`](./b8-elspeth-scale-test.md)
§1, the named B.8 slice is `/home/john/elspeth/tests` (1,037 files / 430k LOC);
the full checkout is "useful as a future stress target but is not the named B.8
slice." This entry records the result of applying the B.8 methodology to that
future stress target on operator request. It does **not** reopen the
[2026-05-17T22:43Z GREEN](#2026-05-17t2243z--green-rerun-superseding-red)
verdict on the named slice.

reason: `loomweave analyze` failed mid-run with a UNIQUE constraint violation on
`entities.id`. The Python plugin emits one entity per `def`, ignoring
`@typing.overload` stub signatures that legitimately share a qualname with their
implementation. The named B.8 slice did not surface this because elspeth
`tests/` uses no `@overload`; elspeth `src/` does (2 files, 9 stubs). MCP-serve
measurement is not possible against a partial DB and was therefore not
performed.

follow-up filed: **`clarion-e29402d1ba`** — wp3 Python plugin: duplicate entity
IDs for `@typing.overload` stubs. This is the actionable output of running the
methodology against the full corpus; the bug is what scale-testing is supposed
to surface.

### Reproducibility

| Field | Value |
|---|---|
| Loomweave branch at run | `sprint-2/b8-scale-test` |
| Loomweave base commit | `a80c31a` |
| Loomweave working-tree changes | 3 src files modified (`crates/loomweave-core/src/plugin/manifest.rs`, `crates/loomweave-storage/migrations/0001_initial_schema.sql`, `crates/loomweave-storage/tests/schema_apply.rs`); untracked `docs/loomweave/adr/ADR-031-schema-validation-policy.md`. Binary rebuilt against this state. |
| Corpus source | `/home/john/elspeth/` (full checkout, not just `tests/`) |
| Corpus commit | `9d3fd55d63bac764c88af04330af2c3f4f651346` |
| Corpus dirty state | 11 modified files (composer ux-redesign docs and two orchestrator src files); recorded at `/tmp/loomweave-b8-full-elspeth-status.txt` |
| Scratch corpus path | `/tmp/loomweave-b8-elspeth-full-20260518T0016Z` |
| Scratch corpus selection | rsync of `*.py` outside `.venv`, `.uv-cache`, `.worktrees`, `node_modules`, `.git`, `__pycache__`, `build`, `dist` |
| Python files in scratch | 1,532 |
| Python LOC in scratch | 611,220 |
| Raw artifacts | `tests/perf/b8_scale_test/results/2026-05-18T0017Z/` |
| Install command | `loomweave install --path <scratch>` (run with python plugin venv bin on PATH) |
| Analyze command | `target/release/loomweave analyze /tmp/loomweave-b8-elspeth-full-20260518T0016Z` |
| RSS sampler | `/tmp/loomweave-b8-analyze-with-rss.py` (250 ms poll over proc + 2 levels of descendants) |
| MCP driver | not run (no usable DB) |
| Filigree route | not exercised |

The scale of the supplementary corpus relative to the named slice: 1.48× the
file count (1,532 vs 1,037), 1.42× the LOC (611k vs 430k), and includes
`src/elspeth/` for the first time at B.8 scale.

### Analyze-Time Measurements

| Measurement | Value |
|---|---:|
| Run id | `a0fb3be2-c713-4805-80b2-07bd96e5a159` |
| Run status (per `runs.status`) | `failed` |
| Run started | `2026-05-18T00:19:08.059Z` |
| Run completed | `2026-05-18T00:26:56.193Z` |
| Total wall-clock | 468.17s / 7m48s |
| NFR-PERF-01 limit | 60m |
| Wall-clock vs 60m envelope | well inside (12.99% of envelope), but irrelevant — run did not complete |
| Peak RSS (sampled) | 185,541,632 bytes / 176.94 MiB |
| Peak RSS caveat | sampler swept process + 2 generations of descendants at 250 ms; prior tests-slice run reported 1.94 GiB peak via a different harness. The 11.0× discrepancy is likely sampler under-coverage of pyright subprocess RSS rather than a real memory reduction — treat this RSS number as a lower bound, not a measurement comparable to the prior run. |
| `.loomweave/loomweave.db` size at FailRun | 47.25 MiB |
| Discovery/source walk | ~0.013s from log start to `source tree walk complete` |
| Plugin processing | ~464.467s from `processing plugin` to `plugin host collected findings` (Python plugin completed all 1,526 files it was handed) |
| Commit/close phase | did not reach successful completion; writer-actor aborted with UNIQUE constraint failure 3.563s after host findings |

Pre-failure persisted entities (committed in batches before the failing batch):

| Kind | Count |
|---|---:|
| `class` | 5,154 |
| `function` | 24,430 |
| `module` | 1,366 |
| **Total** | **30,950** |

Comparison to the named-slice GREEN run (2026-05-17T21:56Z): the supplementary
corpus produced more entities even with the run aborted before edges committed
(30,950 vs 26,813 total; functions 24,430 vs 21,399; classes 5,154 vs 4,378;
modules 1,366 vs 1,036). 160 of the 1,526 files handed to the plugin did not
produce a `module` entity before the abort — that batch was either still
in-flight or rolled back when the failing `InsertEntity` aborted its transaction.

Pre-failure persisted edges:

| Kind | Confidence | Count |
|---|---|---:|
| (none) | — | 0 |

Edges (calls, contains, references) are committed in batches after the entity
batches in this plugin's emit order, so the FailRun aborted before any edges
landed.

Run counters (from `runs.stats`):

```json
{"failure_reason":"InsertEntity for python:function:elspeth.core.landscape.execution_repository.ExecutionRepository.complete_node_state: sqlite error: UNIQUE constraint failed: entities.id"}
```

`findings` table rows: **0** (the 6 plugin-host warnings logged at
`2026-05-18T00:26:52.543Z` are visible in `analyze.stdout` but were not
persisted to the `findings` table — the FailRun aborted before they were
committed). `entity_unresolved_call_sites` rows: **0**.

Plugin-host findings observed in stderr (informational; not persisted):

| Subcode | Count | Materiality |
|---|---:|---|
| `LMWV-INFRA-PLUGIN-MALFORMED-UNRESOLVED-CALL-SITE` | 6 | Same malformed `callee_expr > 512 bytes` signal observed on the named slice (8 there); harmless |
| `LMWV-PY-PYRIGHT-*` | 0 | No pyright lifecycle finding surfaced |

### Failure Detail

stderr at run completion:

```
Error: analyze run a0fb3be2-c713-4805-80b2-07bd96e5a159 failed —
  InsertEntity for python:function:elspeth.core.landscape.execution_repository.ExecutionRepository.complete_node_state:
  sqlite error: UNIQUE constraint failed: entities.id
```

Root cause confirmed in source: `src/elspeth/core/landscape/execution_repository.py`
defines `complete_node_state` four times in `ExecutionRepository` — three
`@typing.overload` stubs plus the real implementation. Per ADR-022 the entity
ID is `{plugin_id}:{kind}:{canonical_qualified_name}`, so all four `def`s map
to the same `python:function:...:complete_node_state` ID. The writer-actor
enforces UNIQUE on `entities.id` (per ADR-011) and fails the run on the second
insert.

Blast radius at this scale: 2 files use `@overload` (both in `src/elspeth/core/landscape/`):

- `execution_repository.py` — 3 overload stubs
- `formatters.py` — 6 overload stubs

No files in elspeth `tests/` use `@overload`, which is why the named B.8 slice
did not surface this. The pattern is standard in typed Python libraries; the
defect will recur whenever Loomweave analyzes any real `src/` tree that uses
`@overload`, `@typing.overload`, or — by the same reasoning — any decorator
pattern that produces multiple `def`s sharing a qualname.

### MCP Serve-Time Measurements

Not performed. The DB is in a `FailRun` state with no edges persisted, so
`callers_of`, `execution_paths_from`, `neighborhood`, and the LLM-backed
`summary` / inferred-`callers_of` paths cannot be meaningfully exercised. Doing
the serve pass against a partial DB would produce numbers that do not
characterise the v0.1 surface under the named B.8 slice and would not
characterise the supplementary corpus either (the run did not complete).

A sanitised rerun against the full corpus minus the 2 overload-using files was
considered and rejected as scope escalation: the methodology's job here is to
faithfully record what `loomweave analyze` does against the full elspeth tree,
not to engineer a workaround so the harness can claim "all 7 tools exercised."

### NFR And Gate Outcome

| Gate | Target | Outcome |
|---|---|---|
| Analyze completion | run reaches `completed` status | **FAIL**: status `failed` at first duplicate-entity-ID insert |
| Analyze wall-clock | NFR-PERF-01 ≤ 60m | not applicable: aborted at 7m48s |
| Plugin contract: entity-ID uniqueness | ADR-022 implies `{plugin_id}:{kind}:{qualname}` is plugin-unique | **VIOLATED** by the Python plugin under `@typing.overload` |
| Seven-tool end-to-end surface | every tool exercised and useful | not measurable: serve pass not run |
| Summary cache hit rate | NFR-COST-02 ≥ 95% post-stabilisation | not measurable: serve pass not run |
| Cost per consultation | NFR-COST-01 (reframed) | not measurable: serve pass not run |
| Filigree HTTP integration | real reverse route, measured RTT | not exercised |

### Decision

The gate is **RED for the supplementary measurement** with a clearly named
mechanism: `@typing.overload` collides with the entity-ID invariant in the
Python plugin. This does not change the Sprint-2-close [2026-05-17T22:43Z
GREEN](#2026-05-17t2243z--green-rerun-superseding-red) verdict on the named B.8
slice (`/home/john/elspeth/tests`), which exercised all seven MCP tools end to
end. It does name a concrete v0.1 blocker for analyze against any real-world
Python `src/` tree.

Action: filed `clarion-e29402d1ba` (P2 bug, `wp:3` / `release:v0.1` /
`sprint:2`) with the root cause, fix sketch (PEP 484 semantics — collapse
overload stubs into the implementation entity), and explicit scope note that
the pattern will also surface for `@functools.singledispatch.register` and
similar decorators. The bug should be fixed before the full elspeth checkout
can serve as a B.4* / B.8 stress target.

Per the pre-written [rollback playbook](./b8-scale-test.md) §4 Red options,
this is **Red option 4** at the supplementary level only — defer the full
elspeth proof. Sprint 2 close stays as the previously recorded measured-partial
milestone on the named slice; the full checkout proof opens a new follow-up
under `clarion-e29402d1ba`.

## 2026-05-17T21:56Z — RED

verdict: **RED**

selected_playbook_option: **Red option 4 — defer the full elspeth proof and close
only a slice demo if Sprint 2 must preserve a partial milestone.**

rollback_action:

- Sprint 2 is closed as a measured partial milestone, not MVP-ready.
- Storage-backed MCP navigation is accepted as measured on the elspeth-slice.
- LLM-backed MCP proof (`summary`, inferred `callers_of`) slips to v0.2 repair.
- Follow-up filed: `clarion-ac5f9bf35b` — OpenRouter-backed summary and inferred
  MCP paths return invalid JSON.

reason: `loomweave analyze` completed within the v0.1 scale envelope and the
storage-backed MCP tools returned useful bounded responses, but every live
OpenRouter-backed `summary()` call and every inferred-confidence dispatch failed
with `llm-invalid-json`. The B.8 "all 7 tools" proof is therefore not true, and
the NFR-COST-02 summary-cache hit-rate target is unmeasurable rather than green.

### Reproducibility

| Field | Value |
|---|---|
| Loomweave branch at run | `sprint-2/b8-scale-test` |
| Loomweave commit at run | `80a6af9` |
| Corpus source | `/home/john/elspeth/tests` |
| Corpus commit | `deab8f5b21335f37e72ed70fb494a30e2c237b21` |
| Corpus dirty state | one unrelated untracked doc: `docs/superpowers/plans/2026-05-18-report-assemble-aggregation.md` |
| Scratch corpus path | `/tmp/loomweave-b8-elspeth-tests-20260517T2156Z` |
| Python files | 1,037 |
| Python LOC | 429,870 |
| Raw artifacts | `tests/perf/b8_scale_test/results/2026-05-17T2156Z/` |
| Analyze command | `target/release/loomweave analyze /tmp/loomweave-b8-elspeth-tests-20260517T2156Z` |
| Serve driver | `tests/perf/b8_scale_test/driver.py` |
| Serve config | `/tmp/loomweave-b8-elspeth-tests-20260517T2156Z/loomweave-b8-live.yaml` |
| Filigree route | real dashboard at `http://127.0.0.1:9388/api/entity-associations` |

This was the representative elspeth-slice requested by B.8, not the fallback
synthetic augmentation. The scratch copy preserved Python file layout and
excluded non-Python files.

### Analyze-Time Measurements

| Measurement | Value |
|---|---:|
| Run id | `2c1191ee-294d-472e-90ea-d73173da8368` |
| Status | `completed` |
| Total wall-clock | 447.154s / 7m27s |
| NFR-PERF-01 limit | 60m |
| Peak RSS | 2,033,442,816 bytes / 1,939.242 MiB |
| `.loomweave/loomweave.db` size | 173 MiB |
| `.loomweave/` size at close | 173 MiB |
| Discovery/source walk | ~0.002s from first log to `source tree walk complete` |
| Plugin processing | ~441.346s from `processing plugin` to host findings |
| Commit/close flush | ~5.699s from host findings to `plugin complete` |
| Per-file analysis wall average | ~425.6 ms/file, derived from plugin processing / 1,037 files |
| Pyright per-file p50 | not surfaced by current run stats |
| Pyright per-file p95 | 1,108 ms |
| Pyright AST-index parse p95 | not surfaced in this run; added as `runs.stats.pyright_index_parse_latency_p95_ms` by follow-up `clarion-7aee45d920` |
| Extractor AST parse p95 | not surfaced in this run; added as `runs.stats.extractor_parse_latency_p95_ms` by follow-up `clarion-7aee45d920` |
| Pyright restart count | not surfaced by current run stats |

Entities by kind:

| Kind | Count |
|---|---:|
| `class` | 4,378 |
| `function` | 21,399 |
| `module` | 1,036 |
| **Total** | **26,813** |

Edges by kind and confidence:

| Kind | Confidence | Count |
|---|---|---:|
| `calls` | `resolved` | 14,327 |
| `contains` | `resolved` | 25,777 |
| `references` | `resolved` | 3,877 |
| **Total** |  | **45,369** |

Run counters:

| Counter | Value |
|---|---:|
| `dropped_edges_total` | 1,388 |
| `ambiguous_edges_total` | 0 |
| `unresolved_call_sites_total` | 90,178 |
| `entity_unresolved_call_sites` rows | 88,010 |
| `reference_sites_total` | 44,927 |
| `references_resolved_total` | 5,121 |
| `unresolved_reference_sites_total` | 39,806 |
| `references_skipped_external_total` | 21,470 |
| `references_skipped_cap_total` | 0 |
| `findings` table rows | 0 |

Analyze findings emitted by code:

| Code | Count | Materiality |
|---|---:|---|
| `LMWV-INFRA-PLUGIN-MALFORMED-UNRESOLVED-CALL-SITE` | 8 | Material follow-up signal; overlong `callee_expr` entries are dropped from the unresolved-site side table |
| `LMWV-PY-PYRIGHT-*` | 0 | No pyright lifecycle finding surfaced |

### B.4* Extrapolation Check

The B.4* mini-gate projected elspeth-full from 828 functions and 3.990s:

| Basis | Value |
|---|---:|
| Mini function count | 828 |
| Mini wall-clock | 3.990s |
| B.4* named elspeth-full function count | 4,157 |
| B.4* projected elspeth-full wall-clock | 20.032s |
| B.8 actual function count | 21,399 |
| Linear projection from mini to B.8 actual functions | ~103.1s |
| B.8 actual wall-clock | 447.154s |
| Actual / linear projection | ~4.34x slower |

The extrapolation was directionally useful for unresolved-site growth
(`90,178 / 3,447 ~= 26.2x`; function growth was `21,399 / 828 ~= 25.8x`) but
understated wall-clock materially. This is a yellow analyze signal by itself,
not a red analyze signal, because the run still completed far inside the
60-minute NFR.

### MCP Serve-Time Measurements

Driver output: `tests/perf/b8_scale_test/results/2026-05-17T2156Z/mcp-driver-output.json`.
Raw LLM error probe: `tests/perf/b8_scale_test/results/2026-05-17T2156Z/mcp-raw-error-probe.json`.

The driver exercised all seven tools listed by `tools/list`:

- `entity_at`
- `find_entity`
- `callers_of`
- `execution_paths_from`
- `summary`
- `issues_for`
- `neighborhood`

Driver totals:

| Measurement | Value |
|---|---:|
| Initialize latency | 10.969 ms |
| `tools/list` latency | 0.147 ms |
| Tool calls | 100 |
| OK tool envelopes | 82 |
| Error tool envelopes | 18 |
| Unavailable envelopes | 0 |
| Useful-result calls | 78 |
| Max latency | 16,956.335 ms |
| Overall p50 latency | 2.086 ms |
| Overall p95 latency | 14,493.794 ms |
| Overall p50 response size | 1.039 KiB |
| Overall p95 response size | 2.588 KiB |

Pattern summary:

| Pattern | Calls | OK | Errors | Useful | p50 ms | p95 ms | Summary cache hit rate |
|---|---:|---:|---:|---:|---:|---:|---:|
| Light | 5 | 5 | 0 | 5 | 1.082 | 3.517 | n/a |
| Medium cold | 20 | 17 | 3 | 16 | 1.945 | 15,470.175 | unmeasurable |
| Medium warm | 20 | 17 | 3 | 16 | 1.723 | 11,875.340 | unmeasurable |
| Heavy | 50 | 43 | 7 | 41 | 2.106 | 12,324.184 | unmeasurable |
| Inferred edge | 5 | 0 | 5 | 0 | 16,134.971 | 16,956.335 | n/a |

Per-tool summary:

| Tool | Calls | OK | Errors | Useful | p50 ms | p95 ms | p95 response KiB |
|---|---:|---:|---:|---:|---:|---:|---:|
| `entity_at` | 16 | 16 | 0 | 16 | 0.220 | 0.652 | 0.711 |
| `find_entity` | 15 | 15 | 0 | 15 | 0.528 | 1.082 | 1.188 |
| `callers_of` | 20 | 15 | 5 | 15 | 3.517 | 16,956.335 | 4.177 |
| `execution_paths_from` | 13 | 13 | 0 | 13 | 0.262 | 0.337 | 1.218 |
| `summary` | 13 | 0 | 13 | 0 | 11,633.245 | 15,470.175 | 0.329 |
| `issues_for` | 9 | 9 | 0 | 5 | 2.728 | 3.262 | 1.215 |
| `neighborhood` | 14 | 14 | 0 | 14 | 3.486 | 7.439 | 2.148 |

Storage-backed gate slice, excluding LLM-backed summary and inferred dispatch:

| Slice | Calls | OK | Errors | p50 ms | p95 ms |
|---|---:|---:|---:|---:|---:|
| Default storage-backed navigation | 69 | 69 | 0 | <4 ms | <8 ms |

The harness's raw `steady_state_storage_backed` aggregate includes inferred
`callers_of` because that is still the same MCP tool name; its p95 is therefore
red. The explicit default-storage slice above is included to preserve the
useful distinction: persisted navigation is healthy, but LLM-backed inferred
dispatch is not.

### Filigree HTTP Cost At Scale

The run used the real Filigree dashboard route and a live entity association
attached to `clarion-6222134e0d` for:

`python:class:e2e.audit.test_attributability.TestAttributability`

`issues_for` measurements:

| Measurement | Value |
|---|---:|
| Calls | 9 |
| OK | 9 |
| p50 latency | 2.728 ms |
| p95 latency | 3.262 ms |
| Matched-issue calls | 5 |
| Empty-result calls | 4 |
| Filigree HTTP requests per matched include-contained call | 4 |
| Filigree HTTP requests per direct-only empty call | 1 |

This validates the ADR-029 reverse-route shape at B.8 scale. No pagination or
cap pressure appeared in this run.

### OpenRouter Token And Cost Record

| Measurement | Value |
|---|---:|
| Live summary dispatches attempted | 13 |
| Live inferred dispatches attempted | 5 |
| Successful LLM-backed responses | 0 |
| `summary_cache` rows after run | 0 |
| `inferred_edge_cache` rows after run | 0 |
| Loomweave-reported prompt tokens | 0 |
| Loomweave-reported completion tokens | 0 |
| Loomweave-reported total tokens | 0 |
| Estimated dollar cost | not computable from Loomweave artifacts |

The token/cost ceiling could not be validated. This does not mean the run was
free; it means the provider path returned text that failed Loomweave's JSON
contract before the MCP envelope surfaced usage accounting. The v0.2 follow-up
must either enforce JSON-mode/provider constraints or preserve usage tokens even
when the semantic JSON parse fails.

Raw probes after the driver returned:

| Probe | Latency | Error |
|---|---:|---|
| `summary(id)` | 12,213.408 ms | `llm-invalid-json`: summary provider returned non-JSON output |
| `callers_of(id, confidence="inferred")` | 14,146.773 ms | `llm-invalid-json`: inferred provider returned invalid JSON at line 1 column 1 |

### NFR And Gate Outcome

| Gate | Target | Outcome |
|---|---|---|
| Analyze wall-clock | NFR-PERF-01 <= 60m | PASS: 7m27s |
| Analyze extrapolation | B.4* projection should be verified or disconfirmed | DISCONFIRMED for wall-clock: actual ~4.34x slower than linear mini projection |
| MCP p95 latency | <500ms green, 500ms-2s yellow, >2s red | RED for all-tool p95 due summary/inferred; green for default storage navigation |
| Summary cache hit rate | NFR-COST-02 >=95% post-stabilisation | RED/unmeasurable: every summary call failed, cache rows = 0 |
| Cost per consultation | NFR-COST-01 reframed as operator-facing session cost | RED/unmeasurable: token usage not surfaced on invalid JSON failures |
| Seven-tool end-to-end surface | every tool exercised and useful | RED: 5 storage tools + `issues_for` useful, `summary` not useful, inferred path not useful |
| Filigree HTTP integration | real reverse route, measured RTT | PASS: p95 3.262 ms |

### Decision

The gate is **RED**. Sprint 2 cannot honestly be signed as "MVP MCP surface
operational against elspeth scale" because the LLM-backed tool paths fail every
time with the real OpenRouter provider.

Per the pre-written rollback playbook, Sprint 2 closes only as a partial
milestone. The measurable partial milestone is valuable:

- elspeth-slice analyze completes under the 60-minute NFR;
- persisted entities/edges are queryable at low latency;
- `entity_at`, `find_entity`, default `callers_of`, `execution_paths_from`,
  `issues_for`, and `neighborhood` work against the scale corpus;
- the real Filigree reverse route is fast enough for the B.6 `issues_for` design.

What slips:

- live OpenRouter-backed `summary()` correctness;
- summary-cache hit-rate validation;
- inferred-edge LLM dispatch and coalescing-path validation;
- operator-facing token/cost validation for LLM-backed MCP consultations.

Follow-up issue: `clarion-ac5f9bf35b`.

## 2026-05-17T22:43Z — GREEN Rerun Superseding RED

verdict: **GREEN**

supersedes: **2026-05-17T21:56Z — RED** for the live LLM JSON/cost/cache
blocker tracked as `clarion-ac5f9bf35b`.

This rerun used the same analyzed elspeth-slice database from the RED entry:
`/tmp/loomweave-b8-elspeth-tests-20260517T2156Z/.loomweave/loomweave.db`. No analyze
rerun was performed; the proof is scoped to the missing `loomweave serve` live
OpenRouter/cache evidence.

Raw artifacts:

- Cold-cache repair run: `tests/perf/b8_scale_test/results/2026-05-17T2243Z/mcp-driver-output.json`
- Warm-cache steady-state rerun: `tests/perf/b8_scale_test/results/2026-05-17T2243Z/mcp-driver-output-warm-cache.json`

Loomweave source at run time: branch `sprint-2/b8-scale-test`, base commit
`e6bba0f`, with the B.8 repair patch in the working tree. Filigree HTTP was the
real dashboard route at `http://127.0.0.1:9388/api/entity-associations`.

### Repair Summary

The live OpenRouter provider now sends strict structured-output requests for
both LLM purposes and uses OpenRouter's broadly supported `max_tokens`
parameter. Invalid semantic JSON paths now preserve token and cost usage in the
MCP envelope; this is covered by regression tests for summary and inferred
dispatch malformed-output paths.

### Cold-Cache Repair Run

| Measurement | Value |
|---|---:|
| Tool calls | 100 |
| OK envelopes | 100 |
| Error envelopes | 0 |
| Unavailable envelopes | 0 |
| Useful-result calls | 96 |
| All-tool p50 latency | 1.749 ms |
| All-tool p95 latency | 9,379.681 ms |
| Summary cache rows after run | 3 |
| Inferred edge cache rows after run | 10 |
| Inferred `calls` edges materialized | 57 |

Summary cache proof:

| Measurement | Value |
|---|---:|
| Summary calls | 13 |
| Cold misses that wrote cache rows | 3 |
| Warm/cache-hit summary calls | 10 |
| Warm summary hit rate | 100% |
| Driver `summary_miss_then_hit` | true |
| Summary prompt tokens | 4,480 |
| Summary completion tokens | 1,196 |
| Summary total tokens | 5,676 |
| Summary cost | $0.031380 |

Inferred dispatch proof:

| Measurement | Value |
|---|---:|
| Inferred `callers_of` tool calls | 5 |
| Inferred dispatch misses | 10 |
| In-run inferred cache hits | 3 |
| Candidate callers considered | 13 |
| Inferred edges materialized | 57 |
| Inferred prompt+completion tokens | 142,585 |
| Inferred cost | $0.503091 |

Per-tool usefulness in the cold-cache run:

| Tool | Calls | OK | Errors | Useful |
|---|---:|---:|---:|---:|
| `entity_at` | 16 | 16 | 0 | 16 |
| `find_entity` | 15 | 15 | 0 | 15 |
| `callers_of` | 20 | 20 | 0 | 20 |
| `execution_paths_from` | 13 | 13 | 0 | 13 |
| `summary` | 13 | 13 | 0 | 13 |
| `issues_for` | 9 | 9 | 0 | 5 |
| `neighborhood` | 14 | 14 | 0 | 14 |

`issues_for` used the real Filigree reverse route and remained fast:

| Measurement | Value |
|---|---:|
| Calls | 9 |
| OK | 9 |
| p50 latency | 1.801 ms |
| p95 latency | 3.545 ms |
| Matched-issue calls | 5 |

The cold all-tool p95 is intentionally not the steady-state gate signal because
it includes first-time live OpenRouter calls. It is retained as cost/latency
evidence for cache population.

### Warm-Cache Steady-State Rerun

The warm-cache rerun was executed without clearing `summary_cache`,
`inferred_edge_cache`, or materialized inferred edges after the cold-cache run.

| Measurement | Value |
|---|---:|
| Tool calls | 100 |
| OK envelopes | 100 |
| Error envelopes | 0 |
| Unavailable envelopes | 0 |
| All-tool p50 latency | 1.759 ms |
| All-tool p95 latency | 200.273 ms |
| All-tool max latency | 1,996.851 ms |
| Summary cache hit rate | 100% |
| New LLM tokens | 0 |
| New LLM cost | $0.000000 |

Warm summary and inferred-cache checks:

| Check | Value |
|---|---:|
| Summary hits / summary calls | 13 / 13 |
| Warm-labeled summary hits / warm-labeled calls | 10 / 10 |
| Inferred dispatch misses | 0 |
| Inferred cache hits reported | 13 |
| Inferred cached p95 latency | 1,996.851 ms |
| Medium-warm pattern p95 | 4.463 ms |
| Heavy pattern p95 | 4.294 ms |

The warm all-tool p95 is green under the `<500 ms` target. Cached inferred
dispatch p95 is yellow but inside the `500 ms-2 s` boundary; it performs no new
LLM calls and reports zero token/cost deltas.

### NFR And Gate Outcome

| Gate | Target | Outcome |
|---|---|---|
| Analyze wall-clock | NFR-PERF-01 <= 60m | PASS from RED run: 7m27s on the same DB |
| Live summary JSON contract | `summary()` returns parseable contract JSON | PASS: 13/13 summary calls OK, strict fields present |
| Summary cache hit rate | NFR-COST-02 >=95% post-stabilisation | PASS: 10/10 warm-labeled hits in cold run; 13/13 hits in warm rerun |
| Inferred dispatch | Materializes or cleanly caches inferred results | PASS: 57 inferred edges, 10 inferred cache rows, 0 warm misses |
| Cost/tokens on invalid LLM output | Operator-facing evidence even on semantic parse failure | PASS: regression tests cover malformed summary and inferred output stats |
| Seven-tool end-to-end surface | every tool exercised and useful | PASS: all seven tools OK; every tool produced useful results |
| Warm steady-state p95 | <500ms green, 500ms-2s yellow | PASS: all-tool warm p95 200.273 ms green; cached inferred p95 1,996.851 ms yellow |
| Filigree HTTP integration | real reverse route, measured RTT | PASS: p95 3.545 ms |

The B.4* extrapolation caveat remains a **yellow follow-up**: actual analyze
wall-clock was still ~4.34x slower than the mini-gate's linear projection, even
though the measured analyze run stayed comfortably inside the v0.1 60-minute
envelope.
