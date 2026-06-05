# Phase 3 Clustering Performance Result

Date: 2026-05-18

Corpus: `/tmp/loomweave-b8-elspeth-full-20260518T0016Z`

Reproducibility status: directional historical measurement. The temporary
corpus path is not committed; see `corpus-provenance.md` and
`tests/perf/b8_scale_test/derive-elspeth-corpus.sh` for the committed procedure
to re-derive a comparable corpus from an elspeth checkout.

Command:

```bash
corpus_dir=$(bash tests/perf/b8_scale_test/derive-elspeth-corpus.sh \
  /path/to/elspeth \
  /tmp/loomweave-b8-elspeth-corpus-$(date -u +%Y%m%dT%H%M%SZ))
target/release/loomweave install --force --path "$corpus_dir"
python3 tests/perf/b8_scale_test/results/2026-05-18T0114Z/analyze-with-rss.py \
  tests/perf/b8_scale_test/results/2026-05-18T1138Z-phase3/analyze-metrics.json \
  /home/john/loomweave/target/release/loomweave analyze \
  "$corpus_dir"
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

The run emitted `LMWV-FACT-CLUSTERING-WEAK-MODULARITY` because the full elspeth
graph modularity score was 0.020884003737243844, below the v0.1 threshold of
0.3.
