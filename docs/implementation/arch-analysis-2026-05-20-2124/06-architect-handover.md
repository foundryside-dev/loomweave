# RC1 Architect Handover

## Context

This handover is for a system architect or release owner taking over RC1
review. It supersedes the removed `arch-analysis-2026-05-18-1244` snapshot.

Loomweave is a local-first code archaeology tool. It analyzes a target repo,
stores a graph in SQLite, and serves consult-mode agents through MCP and the
federation HTTP read API. The RC1 branch is coherent and near release
hardening, but not yet release-ready by policy.

## Current State

- Branch: `RC1`.
- Commit analyzed: `286d92d`.
- Local branch was one commit ahead of `origin/RC1` during analysis.
- Tracker at session start had no active release-blocking work visible; only
  one P4 future issue was ready.
- Old architecture-analysis directory was removed at user request.
- New root-and-branch analysis lives in
  `docs/implementation/arch-analysis-2026-05-20-2124/`.

## Read First

1. `04-final-report.md` for the release call and priority risks.
2. `02-subsystem-catalog.md` for code geography.
3. `07-security-surface.md` for trust boundaries.
4. `08-release-readiness.md` for tag blockers.
5. `09-test-infrastructure.md` for gate alignment.
6. `10-dependency-analysis.md` for internal/external dependency risk.

## Release Gate Call

Current recommendation: do not tag yet.

The codebase appears structurally sound, but release readiness depends on live
GitHub governance, CI/release E2E gate alignment, changelog auth-code
correction, and dated external-operator smoke evidence or explicit removal
from the release checklist.

## Architecture Guardrails

- No shared runtime, shared registry, or mediator across Weft products.
- Loomweave must remain useful alone.
- Federation enriches Loomweave; it does not define Loomweave semantics.
- Plugin subprocesses are untrusted.
- Source-to-LLM flow stays behind pre-ingest scanning, live-provider opt-in,
  source-hash verification, and token budgeting.
- SQLite mutation remains centralized through the writer actor.
- MCP and HTTP response envelopes remain closed and fixture-backed.

## Highest-Risk Files

| File | Review Rule |
|---|---|
| `crates/loomweave-cli/src/analyze.rs` | Require focused tests for pipeline/run-state/subsystem changes. |
| `crates/loomweave-cli/src/http_read.rs` | Require federation contract tests and security review for auth/path/limits. |
| `crates/loomweave-core/src/plugin/host.rs` | Require plugin boundary tests for protocol/path/resource changes. |
| `crates/loomweave-core/src/llm_provider.rs` | Require provider/accounting tests for usage, JSONL parsing, live calls. |
| `crates/loomweave-mcp/src/lib.rs` | Require MCP envelope/tool tests for response shape or LLM behavior changes. |
| `plugins/python/src/loomweave_plugin_python/pyright_session.py` | Require Pyright timeout/cap/target-mapping tests. |

## Immediate Work Queue

1. Run live release governance.
2. Decide CI/release treatment for `tests/e2e/sprint_2_mcp_surface.sh` and
   `tests/e2e/phase3_subsystems.sh`.
3. Fix `CHANGELOG.md`: `UNAUTHORIZED` to `UNAUTHENTICATED`.
4. Add duplicate-fact drift tests for Python plugin metadata and Wardline
   bounds.
5. Resolve `EntityCountCap` semantics around edges/findings.
6. Archive external-operator smoke results.

## Verification To Run Before Release Claim

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
bash tests/e2e/sprint_1_walking_skeleton.sh
bash tests/e2e/sprint_2_mcp_surface.sh
bash tests/e2e/phase3_subsystems.sh
bash tests/e2e/wp5_secret_scan.sh
```

Then run release governance and release dry-run per
`docs/operator/v1.0-release-governance.md`.

## Handoff Risks

- Memory or old implementation docs may mention the removed 2026-05-18
  analysis. Prefer the new analysis and live source.
- Live GitHub policy can drift independently of the repository tree.
- Full workspace tests may require building workspace binaries first in some
  contexts.
