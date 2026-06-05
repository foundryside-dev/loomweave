# External-operator smoke test (publish gate)

Source: WS-D of [thread-1-pre-publish-blockers.md](../../docs/implementation/v0.1-publish/thread-1-pre-publish-blockers.md#5-workstream-d--publish-gate-external-operator-smoke-test)
and Filigree issue `clarion-3e0e481ef7`.

The automated harness lives at
[`external-operator-smoke.sh`](external-operator-smoke.sh). The operator
runs that script and then fills in step 8 (the human-judgement part).
This document is the procedure overview and the rationale for each step.

## Purpose

Verify that an outside operator, working from a fresh machine and reading
**only** the top-level `README.md` and `docs/operator/getting-started.md`, can
install Loomweave, analyse a small public Python project, connect a consult-mode
MCP client, ask substantive questions, re-run idempotently, and observe the
pre-ingest secret-block fire on a planted credential.

The test fails if any step requires reading source code outside of `README.md`
and `docs/operator/getting-started.md`. A failure here is a B.1/B.2 docs bug,
not a runtime defect.

## How to run the harness

### CI / repo-internal smoke (in-tree build)

From the repo root on the target machine (Linux or macOS):

```bash
bash tests/e2e/external-operator-smoke.sh
```

The harness builds the workspace in release mode, installs the plugin into the
editable venv at `plugins/python/.venv`, fetches the canonical corpus
(`psf/requests` at the pinned tag), and walks steps 1–7. Step 8 is left for
the operator to fill in. A draft results file is written to
`tests/e2e/external-operator-smoke-results-YYYY-MM-DD-<host>.md`.

### External-operator smoke (binary install)

The outside-operator scenario assumes Loomweave has been installed via the
GitHub Release archive and the Python plugin via `pipx`. Tell the harness to
skip the build step and point it at the operator-installed binaries:

```bash
export CARGO_BUILD=0
export LOOMWEAVE_BIN=/path/to/installed/loomweave
export LOOMWEAVE_PLUGIN_BIN=/path/to/installed/loomweave-plugin-python
bash external-operator-smoke.sh        # script is published as a release asset
```

The script makes no other repo assumptions in this mode; the operator can run
it from any directory.

### Environment knobs

| Variable | Default | Purpose |
|---|---|---|
| `CARGO_BUILD` | `1` | Build loomweave + plugin from source. `0` skips. |
| `LOOMWEAVE_BIN` | (autodetected) | Path to the `loomweave` binary. |
| `LOOMWEAVE_PLUGIN_BIN` | (autodetected) | Path to `loomweave-plugin-python`. |
| `CORPUS_REPO` | `https://github.com/psf/requests.git` | Public Python corpus to analyse. |
| `CORPUS_REF` | `v2.32.3` | Pinned tag for reproducibility. |
| `OPENROUTER_API_KEY` | unset | If unset, step 4.3(c) `summary` is skipped (not failed). |
| `RESULTS_FILE` | `external-operator-smoke-results-<date>-<host>.md` | Override the output path. |

## Test environment

| Knob | Value |
|---|---|
| OS | `ubuntu:24.04` (or equivalent fresh image) for the canonical run; macOS for the platform-coverage run |
| Operator | Someone (or a fresh agent session) with no prior knowledge of this repo |
| Working dir | the harness creates its own `mktemp` scratch; the corpus is cloned there |
| `OPENROUTER_API_KEY` | Required for step 4.3(c); other steps work without it |
| Network | Outbound HTTPS to `api.openrouter.ai` and `github.com` (and PyPI/crates if installing from source) |

## Steps

The harness automates steps 1–7. Step 8 is human-judged and the operator
fills it into the generated results file.

| # | Step | Pass criteria | Automated? |
|---|---|---|---|
| 1 | Install the Rust binary per WS-C's chosen path (GitHub Releases per ADR-033) | `loomweave --version` prints a version on `$PATH` | ✅ |
| 2 | Install the Python plugin via `pipx` per the tutorial | `which loomweave-plugin-python` resolves; the binary is on `$PATH` | ✅ |
| 3 | `loomweave install` against a small Python project (the harness uses `psf/requests` at a pinned tag) | `.loomweave/` exists; `.loomweave/loomweave.db` exists; init log line emitted | ✅ |
| 4.1 | `loomweave analyze` | Exit 0; entity count > 0 | ✅ |
| 4.2 | Start `loomweave serve`; connect MCP client | MCP `tools/list` returns the 9 tools (`entity_at`, `find_entity`, `callers_of`, `execution_paths_from`, `summary`, `issues_for`, `neighborhood`, `subsystem_members`, `project_status`) | ✅ |
| 4.3 | Three MCP queries against analyzed corpus: <br>(a) `find_entity(pattern="Session")` lists module-level matches <br>(b) `callers_of(id=<highest-in-degree function>)` returns a non-empty list <br>(c) `summary(id=<any function>)` returns a paragraph | (a) and (b) return non-empty; (c) returns a paragraph (live LLM) or is skipped when `OPENROUTER_API_KEY` is absent | ✅ |
| 5 | Re-run `loomweave analyze` | Idempotent: entity/edge counts on the second run match the first | ✅ |
| 6 | Plant `.env` with `AWS_ACCESS_KEY_ID=AKIA0123456789ABCDEF`; re-run `loomweave analyze` | `briefing_blocked` entities count > 0; `LMWV-SEC-SECRET-DETECTED` finding emitted | ✅ |
| 7 | `loomweave serve` against the post-secret-block DB; call `summary(id)` on a blocked entity's id | Returns `briefing_blocked` envelope with no LLM call | ✅ |
| 8 | Operator-improvisation tally | Total count of "I had to look at the source to understand what to do next" events. **Target: 0.** Any positive count is a B.1/B.2 docs bug. | ❌ (human-judged) |

## Recording results

The harness writes a Markdown report at
`tests/e2e/external-operator-smoke-results-<date>-<host>.md` automatically.
Steps 1–7 are populated with PASS/FAIL/SKIP and a one-line detail. The
operator fills in:

- Step 8 improvisation tally (count + per-event bullets)
- Final attestation signature + date

For a release gate, attach the completed report as a comment on Filigree
issue `clarion-3e0e481ef7` (or `clarion-e464251ab3`, the renamed GOV-04
issue) before closing.

## Repeating on additional platforms

The release matrix in
[ADR-033](../../docs/loomweave/adr/ADR-033-v1.0-distribution.md) produces
`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, and `aarch64-apple-darwin`
binaries. Run the harness on at least one Linux target and one macOS target
before the `v1.0.0` tag is treated as generally publishable. Each platform
produces its own dated results file (the host tag is part of the default
filename) — both must be archived before tag-cut.
