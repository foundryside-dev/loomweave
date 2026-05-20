# RC1 Test Infrastructure

## Test Surface Map

| Area | Coverage Observed |
|---|---|
| Rust unit/integration | Workspace crate tests for storage, core host, CLI install/analyze/serve, scanner, MCP storage tools. |
| Python plugin | Unit tests, subprocess protocol tests, installed-entrypoint smoke, Pyright-marked integration tests, Wardline probe tests. |
| E2E scripts | Sprint 1 walking skeleton, Sprint 2 MCP surface, Phase 3 subsystems, WP5 secret scan, external operator smoke checklist. |
| Release/governance | Static workflow/pinning/dependabot checks plus live GitHub policy guard. |
| Performance | B.8/Phase 3 artifacts, currently directional because temporary corpora are not committed. |

## Strong Coverage

- Storage migrations and query helpers use real SQLite temp databases.
- Reader-pool tests cover exhaustion queuing, closure SQL errors, panic-to-pool
  errors, and reuse.
- Writer actor tests cover schema/run/entity/edge/cache behavior.
- CLI install tests cover expected files, migration once, overwrite refusal,
  config preservation, partial cleanup.
- Analyze tests cover no-plugin success and `core:file:*` row creation.
- HTTP read API tests cover fixtures, briefing-block non-disclosure, traversal
  rejection, storage error envelopes, batching, resolve results, auth, limits.
- Scanner tests cover byte offsets, non-UTF8, baseline suppression, broad
  high-entropy behavior.
- MCP tests cover metadata, storage tools, LLM caching/accounting, coalescing,
  hallucinated targets, Filigree drift/caps.
- Python plugin tests cover server framing, extraction, Pyright
  timeout/restart/caps, Wardline import/version states.

## Gaps And Risks

| Gap | Risk | Recommendation |
|---|---|---|
| CI/release omit some named E2E scripts. | Release can be green while local release instructions are not fully exercised. | Add missing scripts to CI/release or document as manual gates with dated evidence. |
| External-operator smoke has no result artifact. | Release checklist cannot be audited from the tree. | Add dated result file. |
| Fixture plugin emits no edge happy path. | Subprocess edge-ingest path is less directly tested. | Add a fixture mode or second fixture that emits one accepted edge. |
| Pyright pin duplicated without drift test. | Plugin manifest can diverge from package dependency. | Add package/manifest lockstep test. |
| Wardline bounds duplicated without drift test. | Optional compatibility reporting can drift. | Add manifest/server constant lockstep test or derive constants. |
| Core cap semantics ambiguous. | Future plugin breaker behavior may differ from documented expectation. | Add edge-heavy/finding-heavy cap tests. |

## Verification Commands

ADR-023 floor:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

Python plugin:

```bash
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
```

E2E:

```bash
bash tests/e2e/sprint_1_walking_skeleton.sh
bash tests/e2e/sprint_2_mcp_surface.sh
bash tests/e2e/phase3_subsystems.sh
bash tests/e2e/wp5_secret_scan.sh
```

## This Analysis Pass

This archaeology pass did not run the full test suite. It inspected source and
test coverage and then ran documentation-level validation after writing the new
report set.
