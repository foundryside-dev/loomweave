# Phase 3 Clustering Performance Result

Date: 2026-05-18

Corpus: `/tmp/clarion-b8-elspeth-full-20260518T0016Z`

Command:

```bash
target/release/clarion install --force --path /tmp/clarion-b8-elspeth-full-20260518T0016Z
python3 tests/perf/b8_scale_test/results/2026-05-18T0114Z/analyze-with-rss.py \
  tests/perf/b8_scale_test/results/2026-05-18T1138Z-phase3/analyze-metrics.json \
  /home/john/clarion/target/release/clarion analyze \
  /tmp/clarion-b8-elspeth-full-20260518T0016Z
```

Baseline from `2026-05-18T0114Z`:

- Wall time: 484.651s
- Peak RSS: 188.699 MiB

Phase 3 run:

- Wall time: 377.811s
- Peak RSS: 198.098 MiB
- `runs.stats.clustering.duration_ms`: 280ms
- Module count: 1,526
- Module dependency edges: 7,217
- Subsystems inserted: 100
- `in_subsystem` edges inserted: 1,122

Acceptance:

- Wall-time overhead: PASS. Phase 3 measured clustering duration is 0.280s,
  below the 60s limit.
- RSS overhead: PASS. Whole-run peak RSS is +9.399 MiB relative to the B.8
  baseline, below the 500 MiB limit.

The run emitted `CLA-FACT-CLUSTERING-WEAK-MODULARITY` because the full elspeth
graph modularity score was 0.020884003737243844, below the v0.1 threshold.
