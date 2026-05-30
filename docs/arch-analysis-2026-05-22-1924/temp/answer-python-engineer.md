# Python Engineering Analysis: Open Questions from 04-final-report §8

**Date:** 2026-05-23 | **Scope:** Q1, Q2, Q5, and the flagged server/session interaction.

---

## Q1: Why the 25-file pyright restart constant?

The constant was introduced in commit `68b719c` ("Bound Pyright dogfood analysis", 2026-05-20)
with no rationale comment. `git log -S 'MAX_FILES_PER_PYRIGHT_SESSION'` confirms this was the
introducing commit, and the diff contains no measurement, no profiling reference, and no
explanation for the choice of `25`. The number is empirically ungrounded.

The primary driver of Pyright RSS growth per session is the type graph for imported modules,
which accumulates and does not shrink across `textDocument/didOpen`/`didClose` cycles. Per-file
marginal cost drops once the import frontier saturates. The right boundary is the inflection
point on a per-file RSS delta curve; that experiment has not been run. A `psutil`-based probe
during `analyze` on the `elspeth` target corpus would close this.

---

## Q2: `pyright_session.py` at 1,406 LOC — cohesion story

Both a file and class problem; the class is the root. `PyrightSession` (lines 131–890) bundles
five distinct concerns:

| Group | Methods | Lines |
|-------|---------|-------|
| Process lifecycle | `close`, `_ensure_process`, `_record_restart_or_poison`, `_start_process`, `_initialize`, `_resolve_executable`, `_subprocess_env`, `_start_stderr_drain`, `_drain_stderr` | 182–198, 586–680 |
| LSP transport | `_request`, `_notify`, `_live_process`, `_write_message`, `_read_message`, `_handle_server_message`, `_workspace_configuration_result`, `_configuration_for_section` | 732–834 |
| Call resolution | `resolve_calls`, `_resolve_with_pyright` | 199–256, 332–425 |
| Reference resolution | `resolve_references`, `_resolve_references_with_pyright`, `_reference_target_ids` | 257–330, 427–511 |
| Index + bookkeeping | `_deadline_for_file`, `_function_index_for_path`, `_record_finding`, `_pop_findings`, etc. | 531–590, 836–890 |

The remainder of the file (lines 893–1406) is module-level AST helpers and visitors that belong
with call resolution. The cleanest split is `_LspTransport` (process lifecycle + wire framing)
extracted as a composable object; the transport seam would enable testing the LSP-protocol layer
without wiring call or reference resolution. The `noqa: PLR0913` at `pyright_session.py:132`
(13-parameter `__init__`) is the linter's proxy for the same signal.

---

## Q5 + interaction: Magic numbers and the server/session coupling

### Constants classified

**Wire-contract-pinned (must track Rust counterparts; not WP6 candidates):**

| Constant | Location | Rust counterpart |
|----------|----------|-----------------|
| `MAX_CONTENT_LENGTH = 8 MiB` | `server.py:48` | `ContentLengthCeiling::DEFAULT` in `clarion-core/limits.rs` |
| `MAX_UNRESOLVED_CALLEE_EXPR_BYTES = 512` | `pyright_session.py:43` | Same-named constant in `clarion-core/limits.rs` |
| `STDERR_TAIL_LIMIT = 65536` | `pyright_session.py:49` | `STDERR_TAIL_BYTES = 64 KiB` in `host.rs` |

None carry a comment naming the Rust counterpart. A Rust-side change will not propagate by inspection.

**Operational tunables (WP6 `clarion.yaml` candidates):**

| Constant | Location | Priority |
|----------|----------|---------|
| `MAX_FILES_PER_PYRIGHT_SESSION = 25` | `server.py:49` | High — see interaction below |
| `MAX_PYRIGHT_RESTARTS_PER_RUN = 3` | `pyright_session.py:44` | High — name says "per run"; implementation is per session (see below) |
| `PYRIGHT_INIT_TIMEOUT_SECS = 30.0` | `pyright_session.py:46` | High — gates every restart on slow nodes |
| `PYRIGHT_CALL_TIMEOUT_SECS = 5.0` | `pyright_session.py:47` | Medium |
| `PYRIGHT_FILE_TIMEOUT_SECS = 3.0` | `pyright_session.py:48` | Medium |
| `MAX_REFERENCE_SITES_PER_FILE = 2000` | `pyright_session.py:45` | Low-medium |

### The undocumented interaction

`MAX_PYRIGHT_RESTARTS_PER_RUN` is named "per run" but the `_restart_count` and `_disabled`
that implement it are instance state on `PyrightSession` (`pyright_session.py:158–159`).
`server.py:217–219` destroys the instance every 25 files (`state.pyright.close(); state.pyright = None`),
creating a fresh instance with `_restart_count = 0`. The constant's name states the intent; the
implementation drifts from it.

**Failure mode A:** A Pyright binary that crashes reliably exhausts its 3-restart budget,
gets disabled via `_disabled = True` (`pyright_session.py:601–608`), and then silently regains
full fault tolerance at file 26 when the server creates a new instance. On a 1000-file project
this produces up to 40 restart cycles instead of one `CLA-PY-PYRIGHT-POISON-FRAME` disabling
Pyright for the run. The Rust `CrashLoopBreaker` does not catch this — it operates at the
plugin-process level, not the Pyright-subprocess level.

**Failure mode B:** `_disabled = True` is also set on `FINDING_PYRIGHT_UNAVAILABLE` /
`FINDING_PYRIGHT_INSTALL_FAILURE` (`pyright_session.py:620, 628, 646, 660, 670`). An environment
where `pyright-langserver` is absent will call `shutil.which` and emit a redundant not-found
finding on every 25-file boundary for the entire run.

**Fix direction:** Promote `_disabled` and `_restart_count` from `PyrightSession` instance state
to `ServerState`, so the 25-file restart doesn't reset them. Five lines.

---

## Confidence Assessment

| Finding | Confidence | Basis |
|---------|------------|-------|
| `25` has no empirical basis in commit history | High | Full diff of `68b719c` verified; no measurement cited |
| Five cohesion groups in `PyrightSession` | High | Full method map from `grep -nE` on all 1,406 lines |
| Wire-contract constants coupled to Rust | High | Cross-referenced `server.py:48`, `pyright_session.py:43`, catalog entry for `clarion-core` |
| Interaction failure modes A and B | High | `_restart_count`/`_disabled` instance-scoped at `pyright_session.py:158–159`; instance destroyed at `server.py:217–219`; `_disabled` set at lines 620, 628, 646, 660, 670 |
| Pyright RSS growth mechanism | Moderate | Standard Node.js LSP behaviour; no heap profile of this version |

## Risk Assessment

- **Interaction fix** — Low risk, Easy revert. Fixing it changes observable finding counts in
  existing tests (`test_pyright_session.py`); update expected counts accordingly.
- **Wire-contract constants** — High severity if they drift from Rust; Low likelihood today.
  Add `# NOTE: must match clarion-core/…` inline comments before the next Rust-side refactor.
- **Moving tunables to config** — Wait for the WP6 config-schema design; premature promotion
  produces unstable YAML key names.

## Information Gaps

1. **Pyright RSS profile on `elspeth`** — needed to replace `25` with an evidence-based bound.
2. **Characterization test for cross-session restart behaviour** — failure mode A has no dedicated
   test asserting the reset behaviour, making a fix unverifiable without one.
3. **WP6 timeline** — "WP6" is named in `breaker.rs:7` but its current status is not visible
   from source.

## Caveats

- `extractor.py` (918 LOC) is the second-largest Python file and was not audited here; it may
  carry its own cohesion debt.
- Q2 split recommendation is structural, not test-driven. Characterization tests for
  `resolve_calls` / `resolve_references` / `close` must precede any extract.
