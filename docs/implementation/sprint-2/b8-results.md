# B.8 Elspeth Scale-Test Results

Append-only gate log. Each entry records the analyzed corpus, raw artifact
paths, measurements, and the green/yellow/red decision taken from
[`b8-scale-test.md`](./b8-scale-test.md).

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

reason: `clarion analyze` completed within the v0.1 scale envelope and the
storage-backed MCP tools returned useful bounded responses, but every live
OpenRouter-backed `summary()` call and every inferred-confidence dispatch failed
with `llm-invalid-json`. The B.8 "all 7 tools" proof is therefore not true, and
the NFR-COST-02 summary-cache hit-rate target is unmeasurable rather than green.

### Reproducibility

| Field | Value |
|---|---|
| Clarion branch at run | `sprint-2/b8-scale-test` |
| Clarion commit at run | `80a6af9` |
| Corpus source | `/home/john/elspeth/tests` |
| Corpus commit | `deab8f5b21335f37e72ed70fb494a30e2c237b21` |
| Corpus dirty state | one unrelated untracked doc: `docs/superpowers/plans/2026-05-18-report-assemble-aggregation.md` |
| Scratch corpus path | `/tmp/clarion-b8-elspeth-tests-20260517T2156Z` |
| Python files | 1,037 |
| Python LOC | 429,870 |
| Raw artifacts | `tests/perf/b8_scale_test/results/2026-05-17T2156Z/` |
| Analyze command | `target/release/clarion analyze /tmp/clarion-b8-elspeth-tests-20260517T2156Z` |
| Serve driver | `tests/perf/b8_scale_test/driver.py` |
| Serve config | `/tmp/clarion-b8-elspeth-tests-20260517T2156Z/clarion-b8-live.yaml` |
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
| `.clarion/clarion.db` size | 173 MiB |
| `.clarion/` size at close | 173 MiB |
| Discovery/source walk | ~0.002s from first log to `source tree walk complete` |
| Plugin processing | ~441.346s from `processing plugin` to host findings |
| Commit/close flush | ~5.699s from host findings to `plugin complete` |
| Per-file analysis wall average | ~425.6 ms/file, derived from plugin processing / 1,037 files |
| Pyright per-file p50 | not surfaced by current run stats |
| Pyright per-file p95 | 1,108 ms |
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
| `CLA-INFRA-PLUGIN-MALFORMED-UNRESOLVED-CALL-SITE` | 8 | Material follow-up signal; overlong `callee_expr` entries are dropped from the unresolved-site side table |
| `CLA-PY-PYRIGHT-*` | 0 | No pyright lifecycle finding surfaced |

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
| Clarion-reported prompt tokens | 0 |
| Clarion-reported completion tokens | 0 |
| Clarion-reported total tokens | 0 |
| Estimated dollar cost | not computable from Clarion artifacts |

The token/cost ceiling could not be validated. This does not mean the run was
free; it means the provider path returned text that failed Clarion's JSON
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
`/tmp/clarion-b8-elspeth-tests-20260517T2156Z/.clarion/clarion.db`. No analyze
rerun was performed; the proof is scoped to the missing `clarion serve` live
OpenRouter/cache evidence.

Raw artifacts:

- Cold-cache repair run: `tests/perf/b8_scale_test/results/2026-05-17T2243Z/mcp-driver-output.json`
- Warm-cache steady-state rerun: `tests/perf/b8_scale_test/results/2026-05-17T2243Z/mcp-driver-output-warm-cache.json`

Clarion source at run time: branch `sprint-2/b8-scale-test`, base commit
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
