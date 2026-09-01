# RC1 Root-And-Branch Architecture Report

**Date**: 2026-05-20  
**Branch/commit**: `RC1` at `286d92d`  
**Scope**: source, tests, workflows, release/federation docs, and relevant
implementation archive references.  
**Execution model**: coordinator plus six subsystem exploration agents.

## Bottom Line

RC1 is architecturally coherent and close to v1.0 release shape, but it should
not be treated as release-ready until live repository governance and the
remaining test-gate/documentation drifts are resolved.

The product structure is good: Rust workspace boundaries match
responsibilities, the Python plugin sits behind a defined protocol boundary,
SQLite mutation is serialized, and both MCP and HTTP surfaces are shaped by
closed contracts. The main risks are release-hardening risks concentrated at
policy enforcement, drift between duplicate configuration facts, and
high-blast-radius files where future changes can accidentally bypass existing
controls.

## Replacement Of The Removed Analysis

The old `docs/implementation/arch-analysis-2026-05-18-1244/` snapshot has
been removed. This report replaces it as the current RC1 architecture
snapshot. It intentionally does not preserve the old H/L finding labels;
current action should be driven by this report, current source, and live
Filigree state.

## Architecture Assessment

Loomweave remains aligned with its local-first mission. It can install, analyze,
store, and serve without mandatory sibling runtime. Federation integrations are
read/enrichment paths, not semantic dependencies.

The crate/plugin split is sensible:

- Core contracts and plugin host concerns live in `loomweave-core`.
- Durable graph storage lives in `loomweave-storage`.
- Operator commands and federation HTTP live in `loomweave-cli`.
- Consult-mode MCP lives in `loomweave-mcp`.
- Pre-ingest secret detection lives in `loomweave-scanner`.
- Python language semantics live in `plugins/python`.

The main maintainability pressure comes from large files that carry many
responsibilities inside otherwise sound crate boundaries. `analyze.rs`,
`http_read.rs`, `plugin/host.rs`, `llm_provider.rs`, and
`loomweave-mcp/src/lib.rs` should be treated as "touch with tests and local
factoring" files.

## Strengths

- Strong source-of-truth ladder: ADRs first, then requirements/design, then
  implementation history.
- Local-first design is preserved; no shared registry/mediator dependency has
  crept into core semantics.
- Storage uses a disciplined single-writer/pool-reader model.
- Plugin outputs are treated as untrusted and validated before persistence.
- Secret scanner blocks risky LLM paths before source leaves the machine.
- HTTP read API is contract-heavy: closed envelopes, auth policy, path
  traversal rejection, ETags, limits.
- MCP surface has broad tests around metadata, storage tools, LLM
  caching/accounting, coalescing, hallucinated targets, Filigree drift/caps.
- Release workflow includes static governance checks, checksum generation,
  cosign signing, and SLSA provenance.

## Priority Risks

### R1. Release Governance Is Required But Live State Is Unverified

The release lane expects protected `main`, restricted Actions policy, and
repository rulesets. The latest readiness snapshot says live GitHub policy was
still permissive. Static workflow checks are useful, but they do not prove the
repository is configured safely.

**Recommendation:** before tagging, run the governance guard against live
GitHub and resolve any policy failures.

### R2. End-To-End Gate Mismatch

Local instructions name walking skeleton, Sprint 2 MCP surface, and Phase 3
subsystem E2E as gates. CI/release workflows run walking skeleton and
secret-scan smoke, but not every named E2E script.

**Recommendation:** add `tests/e2e/sprint_2_mcp_surface.sh` and
`tests/e2e/phase3_subsystems.sh` to required jobs, or explicitly document them
as manual release gates with dated evidence.

### R3. Contract Drift In Public/Operator Docs

`CHANGELOG.md` lists HTTP auth error code `UNAUTHORIZED`, while federation
contracts and implementation use `UNAUTHENTICATED`.

**Recommendation:** correct the changelog before release.

### R4. Duplicate Version/Policy Facts Can Drift

Python plugin Pyright pins are duplicated in `plugin.toml` and
`pyproject.toml`. Wardline bounds are duplicated in manifest and server
constants. Core cap semantics are described differently between `limits.rs`
and host edge processing.

**Recommendation:** add direct drift tests or derive one side from the other.
For cap semantics, decide whether edges share the cap and make comments, tests,
and code agree.

### R5. Local Crypto Code Needs Review Discipline

The HTTP HMAC path is implemented locally with `sha2`. It may be acceptable
for RC1, but it is not a place for casual edits.

**Recommendation:** either migrate to a vetted HMAC crate or mark the current
code as requiring crypto-specific review for future changes.

### R6. Release Evidence Gap For External Operator Smoke

The external-operator smoke checklist exists, but no dated result artifact was
found in the tree.

**Recommendation:** produce and archive a dated smoke-result report before
release or explicitly remove it from the release-ready checklist.

## Architectural Decisions To Preserve

- Keep federation enrich-only. Do not add a shared runtime, shared registry, or
  cross-product mediator to simplify Loomweave.
- Keep plugin subprocesses untrusted.
- Keep scanner-before-LLM as a hard ordering constraint.
- Keep storage mutation serialized through the writer actor.
- Keep MCP/HTTP response envelopes closed and fixture-backed.

## Targeted Follow-Ups

| Priority | Follow-Up | Owner Area |
|---|---|---|
| P1 | Run live GitHub release-governance guard and fix policy blockers. | Release/governance |
| P1 | Add or explicitly classify missing E2E release gates. | CI/release |
| P1 | Correct `CHANGELOG.md` auth code to `UNAUTHENTICATED`. | Docs/federation |
| P2 | Add Pyright pin lockstep test for `plugin.toml` and `pyproject.toml`. | Python plugin |
| P2 | Add Wardline version-bound drift test or derive server constants. | Python plugin |
| P2 | Clarify/test `EntityCountCap` edge/finding semantics. | Core/plugin host |
| P2 | Decide whether HTTP HMAC should use a vetted HMAC crate. | CLI/HTTP security |
| P2 | Add fixture subprocess edge-ingest happy path. | Core/fixture tests |
| P3 | Decide whether `summary_cache.entity_id` intentionally lacks an FK. | Storage |
| P3 | Archive external-operator smoke results. | Release/e2e |

## Release Recommendation

Do not tag from RC1 yet. The code architecture is strong enough for release
candidate hardening, but the release policy/evidence checklist is not fully
satisfied in the analyzed tree. Treat RC1 as candidate pending governance and
gate alignment.

## Confidence

High for subsystem boundaries, source contracts, and current
documentation/workflow shape. Medium for release readiness because this pass
did not run the full CI floor, live GitHub policy checks, or external operator
smoke.
