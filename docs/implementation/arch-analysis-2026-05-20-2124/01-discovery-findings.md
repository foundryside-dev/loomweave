# RC1 Architecture Discovery Findings

**Date**: 2026-05-20  
**Branch**: `RC1`  
**Commit analyzed**: `286d92d`  
**Mode**: root-and-branch, Option G comprehensive architecture archaeology  
**Old analysis removed**: `docs/implementation/arch-analysis-2026-05-18-1244/`

## Executive Discovery

Loomweave is a local-first code archaeology system. It ingests a repository,
extracts code entities and relationships, persists a SQLite graph, and serves
consult-mode agents through MCP and a federation HTTP read API. The v1.0
implementation is a Rust 2024 workspace plus a Python language plugin.

The RC1 branch is structurally coherent and close to release shape, but not
release-ready by policy. The codebase has strong boundary validation, broad
test coverage around the main contracts, and a deliberately local-first
federation posture. Remaining release risks are concentrated in repository
governance, test-gate mismatches, contract/documentation drift, and
high-blast-radius files that need restraint before tag.

## Repository Shape

| Area | Role |
|---|---|
| `crates/loomweave-core` | Shared contracts, entity IDs, plugin host, manifest/protocol/transport validation, LLM provider adapters. |
| `crates/loomweave-storage` | SQLite schema, writer actor, reader pool, typed graph queries, summary/inferred-edge caches. |
| `crates/loomweave-cli` | `loomweave` binary: install, analyze, serve, HTTP read API, release-facing operator paths. |
| `crates/loomweave-mcp` | MCP consult-mode JSON-RPC server and tool handlers. |
| `crates/loomweave-scanner` | Pre-ingest secret detector and detect-secrets-style baseline handling. |
| `crates/loomweave-plugin-fixture` | Subprocess fixture plugin for host/integration tests. |
| `plugins/python` | Python language extractor, JSON-RPC stdio server, Pyright-backed resolution, Wardline probe. |

Approximate source inventory from this pass:

| Area | Signal |
|---|---|
| Rust crates | About 40.5K lines under `crates/`. |
| Python plugin source/tests | About 6.4K lines under `plugins/python/src` and `plugins/python/tests`. |
| Documentation | About 36.5K markdown lines under `docs/`. |

Largest/highest-blast-radius files:

| File | Approx LOC | Why It Matters |
|---|---:|---|
| `crates/loomweave-mcp/src/lib.rs` | 3127 | Tool catalog, MCP envelope, LLM summary/inferred-edge paths, Filigree enrichment. |
| `crates/loomweave-core/src/plugin/host.rs` | 2935 | Plugin subprocess supervision, boundary validation, path/resource breakers. |
| `crates/loomweave-cli/src/analyze.rs` | 2427 | Main ingestion orchestration and subsystem clustering path. |
| `crates/loomweave-core/src/llm_provider.rs` | 2467 | Live-provider adapters, CLI/HTTP calls, usage accounting. |
| `crates/loomweave-cli/src/http_read.rs` | 1532 | Federation HTTP contract, auth, envelopes, limits. |
| `plugins/python/src/loomweave_plugin_python/pyright_session.py` | 1406 | LSP lifecycle, timeouts, target mapping. |

## Source Of Truth

The governing ladder is explicit and healthy:

1. Accepted ADRs under `docs/loomweave/adr/`.
2. `docs/loomweave/1.0/requirements.md`.
3. `docs/loomweave/1.0/system-design.md`.
4. `docs/loomweave/1.0/detailed-design.md`.
5. Implementation history under `docs/implementation/`.

Implementation-history documents are evidence, not governing design. Accepted
ADRs carry release and federation decisions. ADR-033 defines v1.0 distribution
through GitHub Releases. ADR-034 hardens the federation HTTP read API around
loopback defaults, authentication, and closed response envelopes.

## Branch Context

At analysis time:

- Branch: `RC1`.
- Local state: one commit ahead of `origin/RC1`.
- HEAD: `286d92d`.
- HEAD adds stronger `AGENTS.md` guidance around focused subagents for release
  reviews, broad audits, debugging, and independent implementation slices.

RC1's broader delta is release/federation-heavy: v1.0 changelog, federation
HTTP read API, secret scanner, subsystem clustering, release governance, and
operator documentation.

## Architecture Pattern Summary

1. `loomweave install` creates `.loomweave/loomweave.db`, config files, ignore rules,
   and applies migrations.
2. `loomweave analyze` discovers plugins, scans source before ingest, runs plugin
   analysis, writes core file/plugin entities and edges, runs graph completion
   and subsystem clustering, records run state, and persists findings.
3. `loomweave serve` opens storage, starts MCP stdio serving, and optionally
   starts the federation HTTP read API.
4. MCP clients query the graph for entity lookup, paths, neighborhoods,
   summaries, inferred calls, Filigree issue associations, and subsystem
   membership.
5. Federation consumers use the HTTP read API for file resolution and
   briefing-safe content reads without making Loomweave depend on sibling
   products.

The design preserves the Weft doctrine: sibling integrations enrich Loomweave,
but Loomweave remains useful alone.

## High-Confidence Strengths

- Clear crate boundaries map to product responsibilities.
- SQLite writes are serialized through a writer actor while reads use a pool.
- Boundary validation exists at manifests, JSON-RPC framing, plugin path jail,
  field caps, source-hash checks, HTTP envelopes, and scanner baselines.
- Secret scanning is pre-ingest and blocks LLM summary paths before source
  bytes can leave.
- MCP and HTTP surfaces use closed, test-pinned envelopes.
- The Python plugin fails soft on syntax/Pyright availability rather than
  collapsing the full run.
- Release workflow shape includes static governance checks, checksums, signing,
  and provenance.

## Discovery Concerns

- Live GitHub repository policy was documented as permissive in the latest
  readiness snapshot: `main` unprotected, permissive Actions policy, no
  rulesets. This must be rechecked live before release.
- CI/release workflows run walking-skeleton and secret-scan E2E scripts, but
  not every end-to-end gate named in `AGENTS.md`.
- `CHANGELOG.md` contains a federation auth error-code drift: `UNAUTHORIZED`
  versus canonical `UNAUTHENTICATED`.
- `crates/loomweave-core/src/plugin/limits.rs` describes `EntityCountCap` as
  covering entities, edges, and findings, while host edge processing says edges
  do not participate in the entity cap.
- HTTP HMAC is hand-rolled in `crates/loomweave-cli/src/http_read.rs`; future
  edits need crypto-specific review.
- Python plugin pins and Wardline bounds are duplicated across manifest/config
  code without a direct drift test.

## Confidence

High for source structure, subsystem boundaries, and release documentation
shape. Medium for live release posture because this pass did not run live
GitHub API policy checks or the full CI floor.
