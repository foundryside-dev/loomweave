# External-operator smoke test (publish gate)

Source: WS-D of [thread-1-pre-publish-blockers.md](../../docs/implementation/v0.1-publish/thread-1-pre-publish-blockers.md#5-workstream-d--publish-gate-external-operator-smoke-test)
and Filigree issue `clarion-3e0e481ef7`.

## Purpose

Verify that an outside operator, working from a fresh machine and reading
**only** the top-level `README.md` and `docs/operator/getting-started.md`, can
install Clarion, analyse a small public Python project, connect a consult-mode
MCP client, ask substantive questions, re-run idempotently, and observe the
pre-ingest secret-block fire on a planted credential.

The test fails if any step requires reading source code outside of `README.md`
and `docs/operator/getting-started.md`. A failure here is a B.1/B.2 docs bug,
not a runtime defect.

## Test environment

| Knob | Value |
|---|---|
| OS | `ubuntu:24.04` (or equivalent fresh image) |
| Operator | Someone (or a fresh agent session) with no prior knowledge of this repo |
| Working dir | `/tmp/smoke-test/` inside the container |
| `OPENROUTER_API_KEY` | Required for steps 4.3 and 5; the test can run *without* the key but steps that depend on live LLM dispatch are then explicit skips, not silent passes |
| Network | Outbound HTTPS to `api.openrouter.ai` and `github.com` (and PyPI/crates if installing from source) |

## Steps

| # | Step | Pass criteria |
|---|---|---|
| 1 | Install the Rust binary per WS-C's chosen path (GitHub Releases per ADR-033) | `clarion --version` prints a version on `$PATH` |
| 2 | Install the Python plugin via `pipx` per the tutorial | `which clarion-plugin-python` resolves; the binary is on `$PATH` |
| 3 | `clarion install` against a small Python project (`requests` source tarball ~7k LOC) | `.clarion/` exists; `.clarion/clarion.db` exists; init log line emitted |
| 4.1 | `clarion analyze` | Exit 0; `analyze complete: run <uuid> ok` reported; entity count > 0 |
| 4.2 | Start `clarion serve` in one shell; connect MCP client in another | MCP `tools/list` returns the 8 tools (`entity_at`, `find_entity`, `callers_of`, `execution_paths_from`, `summary`, `issues_for`, `neighborhood`, `subsystem_members`) |
| 4.3 | Three pre-scripted questions over MCP: <br>(a) `find_entity(pattern="Session")` lists module-level matches <br>(b) `callers_of(id="python:function:requests.api.get|function")` returns a non-empty list <br>(c) `summary(id="python:function:requests.sessions.Session.send|function")` returns a paragraph | (a) and (b) return non-empty; (c) returns a paragraph (live LLM) or `null` summary plus a documented skip if `OPENROUTER_API_KEY` absent |
| 5 | Re-run `clarion analyze` | Idempotent: entity/edge counts on the second run match the first |
| 6 | Plant `.env` with `AWS_ACCESS_KEY_ID=AKIA0123456789ABCDEF`; re-run `clarion analyze` | Exit 0 (`soft_failed`); `CLA-SEC-SECRET-DETECTED` finding in `findings` table; affected entities carry `briefing_blocked = "secret_present"` |
| 7 | `clarion serve` against the post-secret-block DB; call `summary(id)` on a blocked entity's id | Returns `briefing_blocked` envelope with no LLM call |
| 8 | Operator-improvisation tally | Total count of "I had to look at the source to understand what to do next" events. **Target: 0.** Any positive count is a B.1/B.2 docs bug. |

## Recording results

Capture the smoke test as a single Markdown report at
`tests/e2e/external-operator-smoke-results-<date>.md` with:

- The date / agent identity / container image tag.
- Per-step: status (`pass` / `fail` / `skip`), command run, abridged stdout
  excerpt, and any improvisation events that occurred.
- A final aggregate verdict: `gate_passed: true` only if all non-skipped steps
  pass and the improvisation tally is 0.

Attach the report as a comment on Filigree issue `clarion-3e0e481ef7` before
closing.

## Repeating on additional platforms

This document specifies one (Linux x86_64) run. The release matrix in
[ADR-033](../../docs/clarion/adr/ADR-033-v1.0-distribution.md) also produces
`x86_64-apple-darwin` and `aarch64-apple-darwin` binaries; re-run this
checklist on at least one macOS target before the v1.0 tag is treated as
generally publishable. Document each platform's run as a separate
`*-results-*.md` file.
