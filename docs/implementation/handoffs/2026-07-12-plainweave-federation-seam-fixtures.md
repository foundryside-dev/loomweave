# Plainweave federation seam fixture handoff

**Date:** 2026-07-12
**Producer:** Loomweave 1.5.0 worktree artifacts
**Consumer owner:** Plainweave ticket `plainweave-f8303b4b50`

Loomweave now publishes the classifier, external SQLite, identity-ownership,
and authentication contracts exposed by the Plainweave seam audit. This page
records the exact producer bytes, the copy/validation procedure, and one live
end-to-end fixture proof. It does not claim that Plainweave's repository-owned
golden oracle or every required consumer-side enforcement check has landed.

## Producer authorities

Copy fixtures byte-for-byte. The `.sha256` sidecars contain the digest and
basename expected by both repositories.

| Fixture | SHA-256 |
|---|---|
| `crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json` | `f818252e8a6fd28d8890014cb8f8eccc76de86f8448202ed8fe5a56f364c8d6f` |
| `docs/federation/fixtures/classification.python.json` | `2b3822d1bd3db4f986646f04d269cad52ccfa9c85257bedb567aab1ef2c4d3c5` |
| `docs/federation/fixtures/get-api-v1-capabilities.json` | `61020b20aadaef75a3de523f0a8f83be03d1d503ffdca719c78d949d20beeced` |
| `docs/federation/fixtures/loomweave-http-auth-v1.golden.json` | `cd4a8a1598bedafdfe247d47a616e9a82a148e7cf8feaac9299a21550b2c720b` |
| `docs/federation/fixtures/external-sqlite-compatibility-v1.json` | `2f08a5723b84fc7901be18361547dcb64cc5e51e6f8666485dc8578365596d74` |
| `docs/federation/fixtures/identity-ownership-v1.golden.json` | `919d5a73723b42406788e14675aa8fe48dfb9a3b6412ea3b2ef35a8065d7656b` |

The authoritative contract and regeneration procedure are in
[`docs/federation/2026-07-12-federation-seam-golden-authority.md`](../../federation/2026-07-12-federation-seam-golden-authority.md).
The generated metadata records these explicit artifacts:

| Artifact | Version | Ontology |
|---|---:|---:|
| `target/debug/loomweave` | 1.5.0 | n/a |
| `target/debug/loomweave-rust-plugin` | 1.5.0 | 0.9.0 |
| `plugins/python/.venv/bin/loomweave-plugin-python` | 1.5.0 | 0.12.0 |

Provenance is intentionally narrower than “all fixtures came from analysis.”
The real Python analysis and production handlers produce classifier coverage,
all four MCP classification responses, capabilities, identity ownership, and
the live bearer/HMAC handler matrix. The external-SQLite compatibility cases
and canonical HMAC signing vector are deterministic constructions; owning tests
feed them back through production compatibility/auth code and reject drift.

## Plainweave copy and repository-owned oracle

Run this from `/home/john/plainweave` under ticket
`plainweave-f8303b4b50`. These commands are a handoff recipe; Task 8 did not run
them and did not edit Plainweave.

```bash
mkdir -p tests/conformance/fixtures
cp /home/john/loomweave/crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json \
  tests/conformance/fixtures/
cp /home/john/loomweave/crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json.sha256 \
  tests/conformance/fixtures/

cmp /home/john/loomweave/crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json \
  tests/conformance/fixtures/classifier-coverage-v1.golden.json
(cd tests/conformance/fixtures && sha256sum --check classifier-coverage-v1.golden.json.sha256)
```

Add Plainweave's producer-consumer oracle at
`tests/conformance/test_classifier_coverage_oracle.py`, then run it without
creating bytecode, pytest cache, or repository-local temporary files:

```bash
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=src .venv/bin/pytest \
  -p no:cacheprovider \
  --basetemp=/tmp/plainweave-classifier-golden-pytest \
  -q tests/conformance/test_classifier_coverage_oracle.py \
     tests/test_loomweave_adapter.py -k 'classifier or golden'
```

Until that repository-owned oracle lands, Loomweave provides a read-only
compatibility harness around Plainweave's real `LoomweaveAdapter.list_catalog`
path:

```bash
before=$(git -C /home/john/plainweave status --porcelain=v1)
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=/home/john/plainweave/src \
  /home/john/plainweave/.venv/bin/python \
  scripts/validate-plainweave-classifier-golden.py \
  --plainweave-root /home/john/plainweave
test "$(git -C /home/john/plainweave status --porcelain=v1)" = "$before"
```

On 2026-07-12 the harness printed:

```text
Plainweave LoomweaveAdapter accepted producer golden: supported-complete, http-route=0
```

Plainweave's status was the same before and after; its pre-existing changes were:

```text
 M AGENTS.md
 M CLAUDE.md
```

## Live fixture proof

The live project was a staged copy of
`tests/fixtures/federation_classifier_python/`. Before analysis, the staged
fixture was committed to a temporary Git repository after `loomweave install`.
Both status captures were empty:

```text
pre-analysis:  <clean>
post-analysis: <clean>
```

Build and plugin synchronization used the explicit producer artifacts:

```bash
cargo build --workspace --bins
uv sync --project plugins/python --locked --extra dev
uv pip install --python plugins/python/.venv/bin/python \
  --reinstall --no-deps -e plugins/python
```

The pre-analysis latest-run query returned `null`. The normalized post-analysis
diff was:

```diff
- latest_run: null
+ latest_run:
+   id: <run-id>
+   status: completed
+   started_at: <timestamp>
+   completed_at: <timestamp>
+   stats.classifier_coverage:
+     schema: loomweave.classifier-coverage.v1
+     source_walk_complete: true
+     source_walk_skipped_entries: 0
+     plugin_discovery_complete: true
+     plugin_discovery_errors: 0
+     plugin_discovery_error_samples: []
+     plugins:
+       - plugin_id: python
+         plugin_version: 1.4.2
+         ontology_version: 0.12.0
+         matched_files: 5
+         analyzed_files: 5
+         retained_files: 0
+         degraded_files: 0
+         status: complete
+         classifier_tags:
+           - cli-command
+           - data-model
+           - entry-point
+           - exported-api
+           - framework-handler
+           - http-route
+           - public-surface
+           - test
```

Only run identity and timestamps are normalized in this diff. The exact
coverage object is byte-pinned by
`classifier-coverage-v1.golden.json` and its digest above.

### Live MCP responses

The real stdio MCP server received four `entity_tag_list` calls with
`limit: 200` and `offset: 0`. After the documented run-ID, fixture-root, and
SEI normalization, all four response envelopes were byte-equal to the
`responses` object in `classification.python.json`.

| Tag | State | Complete | Matches | Page | Scope | Scan | Source/discovery evidence |
|---|---|---:|---:|---|---|---|---|
| `cli-command` | supported | true | 5 | returned 5/total 5, not truncated | not truncated | not truncated | complete |
| `entry-point` | supported | true | 5 | returned 5/total 5, not truncated | not truncated | not truncated | complete |
| `http-route` | supported | true | 0 | returned 0/total 0, not truncated | not truncated | not truncated | complete |
| `exported-api` | supported | true | 0 | returned 0/total 0, not truncated | not truncated | not truncated | complete |

The two zero rows are supported-zero evidence, not inferred support from
observed tags. Every response also had top-level `truncated: false`,
`truncation_reason: null`, `classification.reasons: []`,
`signal.available: true`, and `signal.complete: true`.

### Doctor proof

Both doctor formats exited zero. The staged project used the stable fixture
instance ID before the doctor calls, so identity validity is part of the proof
rather than an inferred optional posture. The checked-in evidence preserves the
complete outputs and normalizes only the analysis run UUID, instance UUID, and
analysis timestamp:

- [complete normalized JSON report](./evidence/2026-07-12-federation-seam-doctor.json)
- [complete normalized human report](./evidence/2026-07-12-federation-seam-doctor.txt)

The JSON report has `ok: true`, contains every doctor check, records
`http.instance_id` as present and valid, and has no warning/problem check and no
next action. The human report ends with `All orientation surfaces healthy.`
No credential value, signature, nonce, or secret environment-variable name is
present in either artifact.

## Consumer rules that remain load-bearing

- Open `.weft/loomweave/loomweave.db` read-only, validate
  `application_id`, `user_version`, and the exact safe surface before catalogue
  SQL, and name columns explicitly. Do not use `SELECT *`.
- Read only the latest `(started_at DESC, id DESC)` run. Do not fall back when
  the newest row is non-terminal or malformed.
- Treat `supported` separately from `complete`; only supported-complete zero is
  evidence that a surface class is absent.
- Require all page, scope, scan, source-walk, and discovery truncation flags to
  be clean before declaring the denominator complete.
- Probe capabilities without credentials, discover `none` / `bearer` / `hmac`,
  then authenticate protected identity routes according to that contract.
- Before joining HTTP identity to local SQLite, match `api_version` and
  `instance_id` in the identity response itself against both capabilities and
  the local project instance. This closes the reused-port project-switch
  window.

## Plainweave work still open

A final read-only crawl of the live Plainweave adapter on 2026-07-12 confirmed
that the producer fixtures and focused consumer tests pass, but also reproduced
consumer-side gaps that those tests do not yet reject. Under
`plainweave-f8303b4b50`, Plainweave still must:

- validate `api_version` and `instance_id` from the identity response itself,
  not only capability probes before and after the identity request;
- validate every required safe-surface column before reporting an in-range
  SQLite database compatible, distinguish older-supported versions 5--11 from
  the current compatible version 12, and replace `SELECT *` catalogue reads
  with the explicitly named public columns;
- parse and validate the exact capability `authentication` descriptor, then
  use its declared `none`, `bearer`, or `hmac` mode when authenticating the
  protected identity request; and
- vendor the authoritative fixture and byte pin and add the repository-local
  producer-consumer oracle described above.

Until these items land, the read-only harness proves classifier-golden parsing
and supported-complete zero-route behavior only. It is not evidence that
Plainweave enforces the complete identity/auth/SQLite join contract.
