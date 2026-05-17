# B.5* Reference Scale-Smoke Results

Append-only gate log for the B.5* `references`-edge scale guard.

## 2026-05-17 - GREEN

Command:

```bash
plugins/python/.venv/bin/python tests/perf/b5_reference_scale_smoke.py
```

Implementation mitigation included in this run:

- repeated reference lookups are cached per file by `(from_id, site kind, source lexeme)`;
- annotation `typeDefinition` fallback is skipped when `definition` already resolved only outside `project_root`.

### Corpus Results

- corpus: `elspeth_mini`
- corpus_root: `/home/john/clarion/tests/perf/elspeth_mini`
- file_count: 80
- function_count: 828
- reference_sites_total: 6186
- reference_requests_total: 4003
- definition_requests_total: 3762
- type_definition_requests_total: 241
- reference_requests_per_file: 50.0375
- reference_requests_per_b4_function_query: 4.8345
- references_edges_total: 915
- ambiguous_reference_edges_total: 0
- references_resolved_total: 1503
- references_skipped_external_total: 4322
- references_skipped_cap_total: 0
- unresolved_reference_sites_total: 4683
- pyright_init_ms: 147
- per_file_resolution_median_ms: 160
- per_file_resolution_p95_ms: 1216
- total_wall_ms: 25044
- findings_by_subcode: `{}`

### Projection

- projection_root: `/home/john/elspeth/src`
- elspeth_full_file_count: 421
- elspeth_full_function_count: 4124
- projected_elspeth_full_reference_requests: 19938
- b4_full_function_query_count: 4124
- projected_reference_to_b4_function_query_ratio: 4.8346
- green_under_5x_b4_function_queries: true

### Calibration

- date: 2026-05-17
- outcome: GREEN
- calibration_machine: Linux 6.8.0-110-generic x86_64; Python 3.12.3
- pyright_pin: 1.1.409
- clarion_commit_at_measurement: `098d61829eb59462596809205dc068b0282a8852` plus the B.5 review-follow-up working-tree patch

### Decision

GREEN. The smoke had zero cap skips and the projected full-Elspeth reference
request count is 19,938, which is below `5 * 4,124 = 20,620` B.4-style
function-query requests.
