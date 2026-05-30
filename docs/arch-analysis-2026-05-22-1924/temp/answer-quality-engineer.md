# Quality-Engineering View on the Five Open Questions

**Date:** 2026-05-23
**Source evidence:** `04-final-report.md` §6/#11, §8; `02-subsystem-catalog.md` §1 (clarion-core), §4 (clarion-mcp); all source files and test files cited below read directly.

---

## Q1: Why 25 files per pyright restart? Is there a test that catches a regression in pyright memory behavior?

The constant is `MAX_FILES_PER_PYRIGHT_SESSION = 25` at `plugins/python/src/clarion_plugin_python/server.py:49`.

One test exists: `test_analyze_file_restarts_pyright_after_file_budget` (`plugins/python/tests/test_server.py:396`). It monkeypatches the constant to `2`, uses a fake `PyrightSession`, and asserts the state-machine transition: the session is closed and `state.pyright_files_since_restart` resets to `0`. That test catches a **mechanical regression in the recycle loop** — it does not and cannot validate that 25 is the right number. The fake session carries no memory state.

**Missing test:** A soak/memory-growth integration test that runs a real `PyrightSession` against 50+ small Python files, samples the pyright subprocess RSS at each session boundary, and asserts that the process never exceeds a threshold before recycling. Without this, the value 25 is empirical lore that a future committer will treat as arbitrary and reduce to 10 or raise to 100 with no regression net. If the motivation is memory growth in pyright's Node heap, that is the test that makes the number legible.

---

## Q2: Which monolith has the most asymmetric test coverage for its change risk? Where is the highest-priority test gap?

The four monoliths compete, but the answer is `clarion-cli/src/analyze.rs` (`run_with_options`, 570 lines, 13 ordered phases).

The other three are better served than they look:

- `host.rs` (2,935 LOC) has extensive inline coverage: T1, T5 (path-escape), T6 (breaker trip), T8a–e (oversize fields), T9 (cap exceeded), T9b (stderr drain), T10–T11 (manifest security), plus the 876-LOC `mock.rs` enabling in-process pipeline testing without subprocesses.
- `llm_provider.rs` (2,467 LOC) has 22 inline tests across all four provider variants, including fake-subprocess tests for `CodexCliProvider` and `ClaudeCliProvider`.
- `mcp/lib.rs` (4,703 LOC) has a `tests/storage_tools.rs` (2,200 LOC) that covers all 19 tool dispatch paths.

`analyze.rs` has no such seam. Its 13 phases form a strict sequential gate: orphan recovery → secret scan → `BeginRun` → plugin loop → entity ingestion → clustering → `CommitRun`. The whole pipeline is tested only end-to-end by `wp1_e2e.rs` and `wp2_e2e.rs` which spin up a real plugin and a real database. There is no per-phase test seam — you cannot, for example, inject a crash after `BeginRun` but before entity ingestion and assert the run row ends in `failed`, or verify that `SoftFail` vs. `HardFail` branching produces the correct `runs.status` without running the entire binary.

**Highest-priority missing test:** an integration test against `run_with_options` (or a factored helper it calls) that injects a writer-actor failure mid-run and asserts `CommitRun(Failed)` vs. `FailRun` semantics. This is the test that would unblock any refactor of the 570-line function because it establishes what each phase transition must guarantee independently of the others.

---

## Q3: Are the OpenRouter / Claude-CLI / Codex-CLI providers tested? What gap would a clarion-llm extraction surface?

All four providers have tests. `CodexCliProvider` and `ClaudeCliProvider` use fake bash scripts at `llm_provider.rs:1853+` and `2087+`. `OpenRouterProvider` is exercised with a real in-process TCP listener at `clarion-mcp/tests/storage_tools.rs:1237`. `RecordingProvider` is used throughout.

The gap is not per-provider — it is **trait-contract uniformity across providers**. No test runs the same `LlmRequest` through all four `LlmProvider` implementations and asserts:
- Identical `LlmResponse` field shapes (particularly `model_id` passthrough and token count presence).
- Consistent timeout behavior (both CLI providers have reaper threads; OpenRouter uses `reqwest` timeouts — no test verifies they degrade identically).
- Ring-buffer overflow in CLI stdout (the bounded ring in `ClaudeCliProvider`/`CodexCliProvider` is untested at capacity).

If `clarion-llm` is extracted as a crate, the extraction would immediately require a shared integration test fixture that exercises the trait contract across all four providers against a common suite. That fixture does not currently exist; the extraction would surface its absence as a compilation-time discovery of test gaps rather than a runtime one.

---

## Q4: What test would catch a cross-version DB collision? Does it exist?

Adding `PRAGMA application_id` and `PRAGMA user_version` to `clarion-storage` is one change in `pragma.rs`. The tests to validate that change do not exist.

The specific missing tests in `crates/clarion-storage/tests/schema_apply.rs`:

1. **`open_refuses_db_with_foreign_application_id`** — create a SQLite file with `PRAGMA application_id = 0x0F11BEEF` (a hypothetical Filigree or Wardline value), call `apply_write_pragmas` + `apply_migrations`, assert it returns an error before touching schema. Without this test, adding the PRAGMA has no regression net: a future relaxation of the check would go undetected.

2. **`open_refuses_db_from_future_user_version`** — create a DB with `PRAGMA user_version = 999`, call `apply_migrations`, assert it refuses to downgrade. This is the test that catches two Clarion versions sharing a DB by accident — the scenario that prompted Q4.

Neither test exists today (`schema_apply.rs` has 10+ tests, zero touch `application_id` or `user_version`). The retroactive risk named in the report is real: once installed DBs exist in the wild, the PRAGMA values become a wire contract that cannot be set without a migration, so the window for adding this protection cheaply is before first external deployment.

---

## Q5: Which of the 11 limit constants are covered by tests that would catch a value change being wrong?

| Constant | Location | Test status | Test that catches a wrong value |
|---|---|---|---|
| `MAX_PROTOCOL_ERROR_FIELD_BYTES` | `protocol.rs:245` | **Tested** | `protocol.rs` inline: `huge` string asserts truncation at boundary |
| `MAX_ENTITY_FIELD_BYTES` | `host.rs:66` | **Tested** | `host.rs` T8a–d: oversize qualified name / file path / id / kind |
| `MAX_ENTITY_EXTRA_BYTES` | `host.rs:82` | **Tested** | `host.rs` T8e: oversize extra map is dropped with finding |
| `MAX_HEADER_LINE_BYTES` | `transport.rs:45` | **Tested** | `transport.rs:542`: oversize header returns `MalformedHeader` |
| `ContentLengthCeiling::DEFAULT` | `limits.rs` | **Tested** | `limits.rs:370,387`: default value pinned; `host.rs:2471`: surfaces through pipeline |
| `EntityCountCap::DEFAULT_MAX` | `limits.rs` | **Tested** | `limits.rs:432`: cap at 500,000 pinned; `host.rs:2316`: T9 kills plugin on overflow |
| `PYRIGHT_MAX_NPROC` | `host.rs` | **Tested** | `host.rs:1380`: `pyright_runtime_raises_process_ceiling_for_language_server` |
| `DEFAULT_MAX_RSS_MIB` | `limits.rs:261` | **Weak** | `limits.rs:561,569`: only verifies `apply_prlimit_as` does not panic, not that the child is actually memory-constrained |
| `DEFAULT_MAX_NOFILE` | `limits.rs:281` | **Weak** | No behavioral test; constant referenced by `host.rs:576` in `pre_exec`, but `host_subprocess.rs` has no test that opens > 256 file descriptors and observes enforcement |
| `DEFAULT_MAX_NPROC` | `limits.rs:289` | **Weak** | Same gap as `NOFILE` — `pre_exec` path covered only by code review |
| `MAX_UNRESOLVED_CALLEE_EXPR_BYTES` | `host.rs:70` | **Untested** | Used at `host.rs:267` to drop oversized call sites, but no inline or integration test constructs a callee expression > 512 bytes and asserts the drop |
| `STDERR_TAIL_BYTES` | `host.rs:445` | **Partially tested** | `T9b` verifies drain thread is attached and `stderr_tail()` returns `Some(_)`, but does not overflow the ring to test the drop-from-front eviction |

**Highest-value missing test:** `host_subprocess.rs::rlimit_as_actually_enforced_on_child` — spawn a plugin whose manifest requests a pyright-capable runtime (triggering the larger `RLIMIT_AS` path), allocate more than `DEFAULT_MAX_RSS_MIB` inside the child, and assert the child is killed with `CLA-INFRA-PLUGIN-OOM-KILLED`. Currently the `RLIMIT_AS` enforcement path in `host.rs:569–586` is validated only by code review and the `pre_exec` non-panic test. A value change to `DEFAULT_MAX_RSS_MIB` has no regression net beyond Clippy and the CI build.

---

## Confidence Assessment

**High** on Q1, Q4, Q5 — directly verifiable from source and test files read end-to-end.
**High** on Q3 — all provider test locations confirmed; the gap named is structural (trait-contract suite), not a coverage metric claim.
**Medium-High** on Q2 — the claim that `analyze.rs` has no per-phase seam is based on reading the function signature and the test files, not on reading all 570 lines of the function body to exhaustion.

## Risk Assessment

The highest-risk gap is Q5's `DEFAULT_MAX_RSS_MIB` / `DEFAULT_MAX_NOFILE` / `DEFAULT_MAX_NPROC` cluster: these are security-enforcement constants in the plugin isolation layer, and their behavioral coverage is "does not panic." A change that accidentally weakens plugin memory limits would not be caught in CI.

The second-highest is Q4: the `application_id`/`user_version` window closes permanently once installed databases exist in the field. The cost of adding it now is one PRAGMA + two tests. The cost after first external deployment is a migration + cross-version compatibility matrix.

## Information Gaps

- Actual pyright process RSS growth curve is not observable from test files — Q1's "empirical vs. conservative bound" distinction cannot be resolved from code alone.
- `STDERR_TAIL_BYTES` ring eviction is not confirmed to be tested; `T9b` covers attachment, not overflow. An adversarial test would need to send > 64 KiB to stderr and verify the tail contains only the last 64 KiB.

## Caveats

- The Q2 characterization of `analyze.rs` as the most asymmetric file depends on the claim that `mcp/lib.rs` has adequate per-tool coverage. That claim rests on the `storage_tools.rs` test file being broad-coverage — it was confirmed by sampling, not exhaustive read.
- The Q3 ring-buffer-overflow gap is inferred from reading the provider source and finding no test that exercises it, not from a coverage tool.
