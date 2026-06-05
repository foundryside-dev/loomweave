# Phase 3 Corpus Provenance

The 2026-05-18T1138Z Phase 3 perf run used the B.8 elspeth corpus at:

```text
/tmp/loomweave-b8-elspeth-full-20260518T0016Z
```

That temporary directory is not committed. The committed provenance from the
corpus creation pass is:

- elspeth commit: `9d3fd55d63bac764c88af04330af2c3f4f651346`
- copied file manifest: `tests/perf/b8_scale_test/results/2026-05-18T0017Z/corpus-copy.txt`
- dirty status: `tests/perf/b8_scale_test/results/2026-05-18T0017Z/elspeth-dirty-status.txt`

To re-derive a comparable corpus from an elspeth checkout, run. The script
copies Python files reported by `git ls-files -co --exclude-standard '*.py'`,
so it includes tracked and non-ignored untracked source files without pulling in
ignored virtualenv or frontend dependency trees.

```bash
bash tests/perf/b8_scale_test/derive-elspeth-corpus.sh \
  /path/to/elspeth \
  /tmp/loomweave-b8-elspeth-corpus-$(date -u +%Y%m%dT%H%M%SZ)
```

Then use the emitted output directory as the `loomweave install --path` and
`loomweave analyze` target.
