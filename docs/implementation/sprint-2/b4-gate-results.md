# B.4* Week-2 Gate Results

Append-only gate log. The latest entry is parsed by the Task 11b CI freshness check.

## 2026-05-17T07:46:54+00:00 - GREEN

date: 2026-05-17
outcome: GREEN
calibration_machine: Linux 6.8.0-110-generic #110-Ubuntu SMP PREEMPT_DYNAMIC Thu Mar 19 15:09:20 UTC 2026 x86_64 GNU/Linux; Python 3.12.3
operator_hardware_ratio: 1.0
pyright_pin: 1.1.409
clarion_commit: 7337506142e12a1fa924867d117c49b054dfce56

### Corpus Results
- elspeth_mini:
  - file_count: 80
  - function_count: 828
  - total_wall_ms: 3990
  - pyright_init_ms: 150
  - per_file_resolution_median_ms: 25
  - per_file_resolution_p95_ms: 160
  - parent_walk_overhead_ms: 122
  - cli_overhead_ms: 0
  - outgoing_calls_requests_total: 828
  - outgoing_calls_requests_per_file: 10.35
  - calls_edges_total: 830
  - ambiguous_edges_total: 4
  - ambiguous_edge_ratio: 0.0048
  - unresolved_call_site_count: 3444
  - persisted_run_stats: `{"ambiguous_edges_total": 4, "dropped_edges_total": 198, "edges_inserted": 1984, "entities_inserted": 1237, "pyright_query_latency_p95_ms": 113, "unresolved_call_sites_total": 3447}`
- synthetic:
  - file_count: 1
  - function_count: 10
  - total_wall_ms: 458
  - pyright_init_ms: 142
  - per_file_resolution_median_ms: 241
  - per_file_resolution_p95_ms: 241
  - parent_walk_overhead_ms: 1
  - cli_overhead_ms: 74
  - outgoing_calls_requests_total: 10
  - outgoing_calls_requests_per_file: 10.00
  - calls_edges_total: 5
  - ambiguous_edges_total: 1
  - ambiguous_edge_ratio: 0.2000
  - unresolved_call_site_count: 2
  - persisted_run_stats: `{"ambiguous_edges_total": 1, "dropped_edges_total": 1, "edges_inserted": 17, "entities_inserted": 13, "pyright_query_latency_p95_ms": 237, "unresolved_call_sites_total": 2}`

### Extrapolation
- formula: `T_mini x (F_target / F_mini)`
- mini_wall_seconds: 3.990
- mini_function_count: 828
- elspeth_full_function_count: 4157
- elspeth_full_projected_seconds: 20.032
- elspeth_full_projected_minutes: 0.334
- next_tier_function_count: 39125
- next_tier_projected_seconds: 188.537
- next_tier_projected_minutes: 3.142

### Decision
- gate_thresholds_scaled_by_ratio: green_mini_seconds=300.000, red_mini_seconds=1800.000, green_full_seconds=3600.000, red_full_seconds=21600.000
- decision: GREEN
