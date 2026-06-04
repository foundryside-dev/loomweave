# 01 — Discovery Findings

**Date:** 2026-06-02 · **Branch:** `feat/road-to-first-class` · **Workspace version:** 1.1.0
**Method:** Tree archaeology (Clarion's own index is empty / `never_analyzed`, and its Rust bulk has no Rust extractor) + reconciliation against the canonical design ladder. Eight `codebase-explorer` subagents, one per subsystem, each anchored to a `system-design.md` section and tasked to hunt drift. Seeded by two prior analyses (2026-05-20 RC1, 2026-05-22 from-scratch).

This is a **delta analysis**. The structural map already existed; the value here is (a) refresh to current state, (b) doc-vs-code drift, (c) quality/debt for the architect handover.

---

## 1. Directory structure & organization

Organization is **by-crate (layer) for Rust + a wire-isolated Python plugin**:

```
clarion/
├── Cargo.toml                 # workspace, resolver=3, edition 2024, v1.1.0, 6 members
├── crates/
│   ├── clarion-core/          # entity-ID, plugin host, JSON-RPC, LLM provider
│   ├── clarion-storage/       # writer-actor + reader-pool over SQLite
│   ├── clarion-cli/           # the `clarion` binary (6 subcommands) + HTTP read API
│   ├── clarion-mcp/           # the MCP consult server (35 tools)
│   ├── clarion-scanner/       # pure secret-detection library
│   └── clarion-plugin-fixture/# test-only fixture plugin
├── plugins/python/            # clarion-plugin-python (PEP 517 console script)
├── fixtures/entity_id.json    # cross-language entity-ID parity proof
├── docs/                      # full design ladder + ADRs + implementation sprints
└── tests/                     # e2e shell scripts, perf corpora (elspeth_mini)
```

The dependency graph is a clean DAG (no inter-crate cycles): `clarion-core` at the bottom; `clarion-storage` + `clarion-scanner` are lower-layer leaves; `clarion-cli` and `clarion-mcp` are the consumers. The Python plugin shares **no** Rust dependency — it speaks the wire protocol only and consumes `fixtures/entity_id.json` for parity testing.

## 2. Entry points

| Entry point | Where | Shape |
|---|---|---|
| `clarion` CLI | `clarion-cli/src/main.rs`, `cli.rs` | 6 subcommands: `install`, `analyze`, `serve`, `hook`, `doctor`, (+ stats/internal) |
| `clarion analyze` | `clarion-cli/src/analyze.rs::run_with_options` | one-shot pipeline (~9 phases) |
| `clarion serve` | `clarion-cli/src/serve.rs` | long-running: MCP stdio (current-thread rt) + HTTP read API (multi-thread rt) sharing one `ReaderPool` |
| MCP server | `clarion-mcp/src/lib.rs::serve_stdio_with_state_on_runtime` | 35-tool JSON-RPC dispatch over stdio |
| HTTP read API | `clarion-cli/src/http_read.rs` | 16 Axum routes (federation surface) |
| Plugin (subprocess) | `clarion-core/src/plugin/host.rs` ↔ plugin binary | LSP-style Content-Length JSON-RPC over pipes |
| Python plugin | `plugins/python/.../server.py` | 5 JSON-RPC methods; drives `pyright-langserver` |

## 3. Technology stack

- **Rust** (edition 2024, MSRV 1.88), `tokio` multi-thread runtime; `rusqlite` 0.31 (bundled SQLite) + `deadpool-sqlite`; `axum` 0.7 + `tower`/`tower-http`; `reqwest` 0.12 (rustls); `clap` 4; `nix` (`setrlimit` via `pre_exec`); `xgraph` (Leiden clustering); `blake3` (SEI minting + file hashing); `serde_norway` (YAML); `sha1`/`sha2`.
- **Python** (plugin): CPython `ast` stdlib only for extraction; `pyright-langserver` as an LSP subprocess for type-resolved edges; `tomli` (manifest); dev: `ruff`, `mypy --strict`, `pytest`.

## 4. Subsystem identification (8 cohesive groups)

| # | Subsystem | Src LOC | Design anchor | Confidence |
|---|-----------|--------:|---------------|------------|
| 1 | core / plugin host | 11,981 (host.rs 2,958) | §2 Core/Plugin | High |
| 2 | policy / LLM provider | 2,500 (`llm_provider.rs`) | §5 Policy Engine | High |
| 3 | storage | 6,572 (query 1,727; sei 1,143) | §4 Storage, §3 Data Model | High |
| 4 | analysis pipeline | ~4,886 (analyze.rs 3,542) | §6 Pipeline | High |
| 5 | CLI surfaces + HTTP federation | ~9,300 (http_read 4,387) | §1, §9 Integrations | High |
| 6 | MCP consult surface | 13,796 (lib.rs 7,101) | §7 Guidance, §8 MCP | High |
| 7 | secret scanner + fixture | 1,068 | §10 Security | High |
| 8 | Python plugin | 3,173 (pyright_session 1,427) | §2 Python specifics | High |

**Total:** ~47K Rust src LOC + ~21K Rust test LOC + ~3.2K Python src.

## 5. Headline discovery: documentation drift is the dominant finding

The code is structurally sound and has *grown* materially since v1.0 (the "road to first class" work added SEI/ADR-038, Wardline taint store/ADR-036, WS5 faceted MCP shortcuts, incremental-skip). But **`system-design.md` has not kept pace.** Every explorer anchored to a §-section found the section describing a design that was never built, or superseded without an ADR/errata:

- **§2** — async/tokio plugin supervision, mpsc backpressure, streaming notifications, a `file_list` RPC, and several manifest fields: **none exist** (the host is fully synchronous). Python specifics name **tree-sitter + LibCST**; the code uses CPython `ast` only.
- **§5** — `AnthropicProvider`, 4-segment `cache_control`, async cost-estimate trait, haiku/sonnet/opus tiers, budget findings, `cost_report` tool: **none built**. The analyze-time policy engine is not wired (analyze.rs issues zero LLM calls).
- **§6** — phase-7 structural findings (`CLA-FACT-*`) and phases 0/2/4–7: **not implemented**; three new SEI/incremental phases are **undocumented**, no deferral notice.
- **§8** — still says "v1.0 ships an 8-tool subset" / shortcuts "deferred to v1.1": **false; 35 tools ship.**
- **§9** — documents `GET /api/v1/entities/resolve` as shipped: **does not exist** (`contracts.md` confirms deferred; the live surface is 16 routes).
- **`detailed-design.md`** — storage schema documents 6 tables + FTS5 (`:611-760`); reality is **13 tables + FTS5 + view across 6 migration files** (`entities.signature` and 6 tables undocumented).
- **`CLAUDE.md`** — "Layout (post-1.0)" lists **4 crates / v1.0.0** (omits **both** `clarion-mcp` and `clarion-scanner`); reality is **6 crates / v1.1.0**.

This is the gold of an Architect-Ready run: in this repo, **a doc that contradicts the code is a bug** (per the precedence rule in CLAUDE.md). Full per-finding detail is carried in `02-subsystem-catalog.md` and consolidated in `05-quality-assessment.md`.

## 6. Good news: prior-flagged issues now resolved

Verified fixed since the 2026-05 analyses:
- **`application_id` (`0x434C524E` "CLRN") + `user_version`** now set and enforced at open; future-built DBs rejected (`pragma.rs`, `schema.rs`).
- **Blocking `reqwest::blocking` in async MCP handlers** — now wrapped in `tokio::task::spawn_blocking` at all three Filigree call sites.
- **Pyright 3-restart cap** now shared across 25-file session recycles via `PyrightRunState` (was resetting per-recycle).

## 7. Limitations of this analysis

- Three of the four largest files (`mcp/lib.rs` 7,101; `http_read.rs` 4,387; `analyze.rs` 3,542) were enumerated/characterized and sampled, not read 100% end-to-end; `host.rs` impl was read end-to-end.
- HTTP wire conformance was assessed against `contracts.md` text, not by running the fixture suite.
- Drift findings are code-vs-doc; whether each drift should be fixed in *code* or in *doc* is an architect judgment captured in `06`.
