# Clarion — Architecture Analysis Report

**Repository:** `/home/john/clarion`
**Branch:** `sprint-2/b8-scale-test` (17 commits ahead of `main`)
**Commit at validation:** `363bb0a` (working-tree refreshed during validation pass)
**Tags:** `v0.1-sprint-1`, `v0.1-sprint-2`
**Date:** 2026-05-18
**Methodology:** `axiom-system-archaeologist:analyze-codebase` — coordinator + per-subsystem subagents + validation gates
**Source artifacts:**
- `01-discovery-findings.md` — holistic scan, 384 lines
- `02-subsystem-catalog.md` — six subsystem entries, ~650 lines, validation **NEEDS_REVISION (warnings)** → corrections applied inline
- `03-diagrams.md` — five Mermaid diagrams (C4 Context + Container + 2 sequences + 1 component), validation **APPROVED**

---

## 1. Executive summary

Clarion is a **code-archaeology service** — single-binary Rust core + out-of-process language plugins — that ingests a Python codebase, normalises it into a typed entity/edge graph in local SQLite, and serves that graph plus on-demand LLM-generated leaf summaries and inferred call edges to consult-mode LLM agents over MCP. It is one of four products in the Loom suite (Filigree, Wardline, Clarion, Shuttle) authored by a single owner; the federation axiom (solo-useful + pairwise-composable + enrich-only, `docs/suite/loom.md` §3–§5) is the load-bearing constraint on cross-product design.

The codebase is **roughly 30 000 LOC** — 24 727 Rust across 5 workspace crates, 5 629 Python in one plugin — backed by 92 markdown documents (requirements / system design / detailed design / 25 Accepted ADRs / per-sprint work-package docs / sprint sign-offs / handoff memos). Sprint 1 closed with the walking-skeleton end-to-end at `v0.1-sprint-1`; Sprint 2 closed at `v0.1-sprint-2` with seven MCP tools live and the B.8 scale test green against the `elspeth` corpus after a repair rerun. The current branch carries the B.8 fix follow-up (an extractor dedup hardening and an `ADR-031` schema-validation policy that adds `CHECK` constraints to closed core-owned vocabularies).

**Architectural shape.** A clean layered crate graph (acyclic) — `clarion-core` (entity-ID, plugin host, LLM provider abstraction) → `clarion-storage` (writer-actor + reader-pool over a single SQLite database) → `clarion-mcp` (MCP server, ADR-007 5-tuple cache, budget ledger) → `clarion-cli` (glue binary). The Python plugin is a separate process speaking JSON-RPC over Content-Length-framed stdio. Two named v0.1 federation asterisks (Wardline→Filigree pipeline coupling routed through Clarion; Python plugin's soft import of `wardline.core.registry`) are documented and have retirement conditions.

**Standout strengths.**
- **Boundary discipline is real, not aspirational.** Plugin authority is enforced at the host (5-step validator pipeline at `clarion-core/src/plugin/host.rs:1031–1198`); storage authority is enforced at the writer-actor (edge contract with three `CLA-INFRA-EDGE-*` codes at `writer.rs:411`); cache-key correctness is enforced at the call site (ADR-007 5-tuple at `clarion-mcp/src/lib.rs:1010–1016`). Defence-in-depth between the writer-actor and ADR-031's new SQL `CHECK` clauses is a thoughtful choice, not a copy of one to the other.
- **Plugin separation is a process boundary, not a trait abstraction.** The wire protocol is the contract; the test fixture is a 131-LOC second implementation that proves the contract by speaking it.
- **Sprint-1 lock-ins held under Sprint-2 pressure.** L1–L9 from `docs/implementation/sprint-1/README.md` §4 are visible as concrete code shapes: ADR-003 entity-ID parity, ADR-011 actor/pool split, ADR-021 jail+limits, ADR-022 ontology authority, all still visibly load-bearing in Sprint-2 code without rewriting.
- **No god-files.** The two largest source files (`clarion-core/src/plugin/host.rs` at 3 126 LOC and `clarion-mcp/src/lib.rs` at 2 712 LOC, growing during Sprint-2 follow-up) are coherent, banded, and internally documented. The catalog confirms `host.rs` is roughly 1 450 production LOC plus 1 700 LOC of test scaffolding.

**Notable risks (full list in §5).**
- One single-file `analyze::run` of ~500 lines orchestrates the analyze lifecycle without coverage for its `SoftFailed` branch (the soft-fail path that commits partial entity work and a `runs.status='failed'` UPDATE inside the same transaction).
- Two source-of-truth duplications without compile-time enforcement: edge ontology (writer, manifest, ADR) and Python plugin `ONTOLOGY_VERSION` (`server.py` + `plugin.toml`).
- One MCP raw SQL site (`reference_neighbors` at `clarion-mcp/src/lib.rs:2381`) bypasses the storage layer's typed query helpers and would not be caught by storage-side schema tests.
- ADR-024's single edit-in-place migration has been edited three times; the retirement trigger ("external operator builds `.clarion/clarion.db` from a published Clarion build") is documented in-file but is on manual discipline only.
- `BudgetLedger.blocked` is sticky for the lifetime of `ServerState` — one ceiling breach disables LLM tools until process restart, with no documented reset path.

**Recommended next steps (full list in §7).**
- Add `SoftFailed`-branch integration coverage to `crates/clarion-cli/tests/analyze.rs` before any extraction refactor of `analyze::run`.
- Extract the per-tool dispatch handlers from `clarion-mcp/src/lib.rs` into a sibling `tools/` submodule once a Sprint-3 surface settles.
- Audit cross-product vocabulary duplications: one issue per duplication (edge ontology; Python `ONTOLOGY_VERSION`).
- Surface a Filigree-Linked tracker entry for the ADR-024 migration-retirement trigger so the manual condition is observable.

---

## 2. Architecture at a glance

Clarion's deployment model is **single binary + one subprocess per language plugin**. Operators run `clarion install` once to lay down `.clarion/clarion.db` + `.clarion/clarion.yaml` + a `.gitignore`. Then `clarion analyze` does the ingest run, and `clarion serve` (Sprint-2 addition) hosts the MCP stdio server for consult-mode agents. The plugin host spawns each `clarion-plugin-*` binary discovered on `$PATH`, speaks five JSON-RPC methods (`initialize` / `initialized` / `analyze_file` / `shutdown` / `exit`), and validates everything coming back through the five-step pipeline before persisting via the writer-actor.

The federation context, the binary's container view, the analyze lifecycle, the MCP `summary` cache-miss → LLM dispatch flow, and the plugin host validator pipeline are diagrammed in `03-diagrams.md`. They are not duplicated here — refer to that file for the visual reference.

| Layer | Crate(s) | Production LOC | Owns |
|---|---|---|---|
| Domain + safety | `clarion-core` | ~3 100 | EntityId grammar; PluginHost supervisor; LlmProvider trait + OpenRouter; manifest parser; jail + RSS + entity + crash-loop breaker |
| Persistence | `clarion-storage` | ~1 950 | Writer-actor (sole `rusqlite::Connection`); deadpool-sqlite reader pool; schema migration runner; PRAGMA discipline; edge-contract validator; 5-tuple summary cache + 4-tuple inferred-edge cache |
| Read surface | `clarion-mcp` | ~3 300 (`lib.rs` 2 712) | MCP 2025-11-25 over stdio; 7 read tools; ADR-007 cache lookup; budget ledger; in-flight LLM dispatch coalescer; Filigree enrich-only HTTP client |
| Glue + UX | `clarion-cli` | ~1 740 | `clap` dispatch; `.clarion/` bootstrap; analyze orchestrator (~500-line async run); MCP server wiring |
| Test fixture | `clarion-plugin-fixture` | 131 | Protocol-compatible test stand-in for the host's `host_subprocess` integration test |
| Language plugin | `plugins/python` | ~2 670 | Python AST → entities + edges; `pyright-langserver` LSP client (1 251 LOC); Wardline soft-import probe; ADR-018 canonical qualname |

The Rust workspace lints at `unsafe_code = "deny"` (downgraded from `forbid` with one documented exception: `pre_exec` + `setrlimit` in `clarion-core/src/plugin/limits.rs`, the only `unsafe` block in the codebase). Clippy is `pedantic = warn` with three pragmatic allows. Python is `mypy --strict`, `ruff select = ["ALL"]` with pragmatic ignores, complexity capped at 15 to match clippy. ADR-023 names these and the corresponding CI jobs as the floor; every PR must pass them.

---

## 3. Subsystem walkthrough

Each entry below is a 1–2 paragraph synthesis of the corresponding catalog entry. For the full per-subsystem treatment with file:line citations, see `02-subsystem-catalog.md`.

### 3.1 `clarion-core` — domain + safety

`clarion-core` owns the four primitives every other crate depends on: the canonical `EntityId` (a three-segment newtype validated against the cross-language `fixtures/entity_id.json` parity proof); the plugin host (`PluginHost<R, W>` — generic over reader/writer so subprocess and in-process mock share one validator pipeline); the `LlmProvider` trait with `OpenRouterProvider` + `RecordingProvider` implementations and stable prompt-template IDs (`LEAF_SUMMARY_PROMPT_TEMPLATE_ID = "leaf-v1"`, `INFERRED_CALLS_PROMPT_VERSION = "inferred-calls-v1"` at `llm_provider.rs:10–11`); and the safety ceilings (jail, content-length, entity cap, RSS via `setrlimit`, crash-loop breaker).

The crate's two heavy files are coherent: `plugin/host.rs` (3 126 LOC) is one struct + two constructors + a five-step validator pipeline + a 1 700-LOC test suite; `llm_provider.rs` (948 LOC) is request/response DTOs + the trait + `RecordingProvider` + `OpenRouterProvider` with its strict-JSON schema gate (the B.8 GREEN-rerun fix at `response_format_for_purpose:297`). Caching is not done in `clarion-core` — the 5-tuple cache key is materialised inside `clarion-mcp` and `clarion-storage`; the provider is a stateless dispatcher. The crate is fully synchronous (no `tokio` dependency); MCP-side async-from-sync bridging is via `spawn_blocking`.

### 3.2 `clarion-storage` — persistence

A writer-actor (single `tokio::task` owning the sole `rusqlite::Connection`) plus a `deadpool-sqlite` reader pool, both targeting one `.clarion/clarion.db` file. Nine `WriterCmd` variants (`BeginRun` / `InsertEntity` / `InsertEdge` / `InsertInferredEdges` / `UpsertSummaryCache` / `TouchSummaryCache` / `ReplaceUnresolvedCallSitesForCaller` / `CommitRun` / `FailRun`), each with a typed `oneshot` ack. Batch cadence is 50 writes (`DEFAULT_BATCH_SIZE` at `writer.rs:35`); query-time MCP writes interleave with analyze-time runs via `query_time_write` which commits the open batch before reopening `BEGIN` if a run is in progress.

The schema is a single migration (289 lines at `migrations/0001_initial_schema.sql`) covering 8 base tables, 1 FTS5 virtual, 3 triggers, 1 view, 2 generated columns. ADR-031 (new on this branch) adds `CHECK` constraints on closed core-owned vocabularies (`edges.confidence`, `findings.{kind,severity,status}`, `summary_cache.stale_semantic`, `runs.status`) and deliberately omits them from plugin-extensible columns (`entities.kind`, `edges.kind`) — the writer-actor remains the canonical validator; CHECK is defence-in-depth. The migration has been edited three times (initial; 2026-05-03 ADR-024 vocabulary rename; 2026-05-18 ADR-031 CHECKs) under ADR-024's "edit-in-place until external operators build from a published Clarion build" policy.

### 3.3 `clarion-mcp` — read surface

The Sprint-2 addition. One Rust crate, three source files (`lib.rs` 2 712 + `config.rs` 352 + `filigree.rs` 238) plus a 1 710-line integration test file. The `lib.rs` grew through B.8 follow-up commits (`87036b1` reservation-poison fix; `363bb0a` inferred-target pre-filter) — visible growth trajectory, monitored not refactor-blocked. Speaks MCP protocol revision `2025-11-25` over stdio. Seven read tools (`entity_at`, `find_entity`, `callers_of`, `execution_paths_from`, `summary`, `issues_for`, `neighborhood`). Six are thin dispatch over `clarion-storage` helpers; the seventh (`summary`) plus the `confidence=inferred` branches of three others carry substantive LLM logic — cache lookup (ADR-007 5-tuple summary, 4-tuple inferred), budget reservation with RAII rollback, in-flight dispatch coalescing via a `broadcast` channel with a 60-second timeout, prompt construction via `clarion-core` helpers, provider invocation on `spawn_blocking`, JSON-shape validation, writeback via the writer-actor.

Filigree integration is genuinely enrich-only: three independent skip paths route to an `issues_unavailable` envelope (`filigree-disabled` / `filigree-unreachable` / `filigree-client-error` / `entity-not-found`). The other six tools have no Filigree dependency. The single architectural smell is one raw SQL site (`reference_neighbors` at `lib.rs:2381`) that bypasses the storage layer; everything else routes through typed query helpers.

### 3.4 `clarion-cli` — glue binary

The `clarion` binary; `clap`-driven three-subcommand dispatch (`install`, `analyze`, `serve`). Loads `.env` from CWD or ancestor before tracing init. `install.rs` (168 LOC) is the only place that opens SQLite directly — apply PRAGMAs + migrations, write a stub `config.json`, write the ADR-005 `.gitignore`, write the operator `clarion.yaml`. `serve.rs` (136 LOC) wires a single-thread current-thread Tokio runtime, an LLM-only `Writer`, a `ReaderPool` (size 16), and the MCP server.

The orchestrator is `analyze.rs` (1 436 LOC) with `analyze::run` a single ~500-line async function marked `#[allow(clippy::too_many_lines)]`. It owns plugin discovery, source-tree walk, per-plugin `spawn_blocking` dispatch, crash-loop accounting via the breaker from `clarion-core`, and the three-way `RunOutcome` resolution (`Completed` → `CommitRun(Completed)`; `SoftFailed` → `CommitRun(Failed)` with partial work persisted; `HardFailed` → `FailRun` rolling everything back). The phase banners inside `analyze::run` mark the obvious seams; extraction is a worthwhile but currently un-test-covered refactor (see §5).

### 3.5 `clarion-plugin-fixture` — protocol test stand-in

131 LOC across `main.rs` + `lib.rs`. Speaks the same Content-Length-framed JSON-RPC 2.0 protocol as a real plugin, but with hard-coded responses (one stub entity, no edges, fixed `ontology_version = "0.1.0"`). Consumed only by `crates/clarion-core/tests/host_subprocess.rs`. Re-uses `clarion-core`'s typed protocol structs so a breaking change to the wire shape fails compilation rather than at runtime. Same `ContentLengthCeiling::DEFAULT` (8 MiB) as production plugins.

### 3.6 `plugins/python` — Python language plugin

Ten Python files, 2 670 LOC. Speaks the L4 JSON-RPC protocol; uses `pyright-langserver` (pinned to `1.1.409`) for cross-reference resolution. The largest file is `pyright_session.py` at 1 251 LOC — a long-running LSP client with three-restart-then-poison fail-soft semantics, a 5-second per-call deadline, and a 64 KiB stderr-tail ring buffer for diagnostics. `extractor.py` (744 LOC, +98 on this branch for the B.8 fix) is the AST → wire-shape pass; the B.8 fix is layered defence — a semantic skip for `@overload` stubs plus a first-wins safety-net dedup that also catches aliased overload imports and `@singledispatch.register def _():`.

`wardline_probe.py` runs once per session at handshake. The `wardline.core.registry` import (`wardline_probe.py:38`) is the named v0.1 doctrine asterisk — Sprint 1 only proves the import + version pin; consumption is deferred. `ONTOLOGY_VERSION = "0.5.0"` is declared in both `plugin.toml:46` and `server.py:36` without a cross-check; the handshake validates the manifest value, leaving the constant as a maintenance hazard.

---

## 4. Cross-cutting concerns

The following spans more than one subsystem and is governed by either an ADR or by `docs/suite/loom.md`. All are visible in catalog citations.

| Concern | Governance | Implementation sites |
|---|---|---|
| **Entity-ID format** (3 colon-segments) | ADR-003 + ADR-022 | `clarion-core::entity_id.rs` (Rust); `plugins/python/.../entity_id.py` (Python); `fixtures/entity_id.json` (parity proof) |
| **JSON-RPC L4 protocol** (Content-Length framing, 5 methods, 8 MiB cap) | ADR-002 | `clarion-core::plugin::transport` + `protocol.rs`; `clarion-plugin-fixture::main`; `plugins/python/.../server.py` |
| **Plugin authority + jail + RSS** | ADR-021 | `clarion-core::plugin::{jail, limits, breaker, host}` |
| **Core / plugin ontology ownership** | ADR-022 | Host validator pipeline + manifest validator |
| **Edge confidence tiers** (`resolved` / `ambiguous` / `inferred`) | ADR-028 | `clarion-storage::writer::enforce_edge_contract`; `clarion-mcp::optional_confidence` |
| **Edge ontology** (`contains` / `calls` / `references` + 6 anchored / structural kinds) | ADR-026 / ADR-028 | Hard-coded in `clarion-storage::writer.rs:394–401`; declared in `plugin.toml:38`; documented in ADR — **3-place duplication, no cross-check** |
| **Ontology version semver** (currently `0.5.0` after B.5*) | ADR-027 | Validated at host handshake; declared in `plugin.toml:46` + `server.py:36` |
| **Summary cache 5-tuple key** | ADR-007 | `clarion-storage::cache::SummaryCacheKey`; lookup in `clarion-mcp::read_summary_inputs:1010–1016` |
| **Schema migration governance** (edit-in-place until external build) | ADR-024 | `migrations/0001_initial_schema.sql` + `schema.rs`; manual retirement trigger |
| **Schema-validation policy** (CHECK on closed vocabularies, not on plugin-extensible) | ADR-031 (new) | Six CHECK clauses in the migration; writer-actor remains canonical validator |
| **Loom federation axiom** (solo-useful + pairwise-composable + enrich-only) | `docs/suite/loom.md` §3–§5 | `clarion-mcp::filigree` (enrich-only); `plugins/python/.../wardline_probe.py` (soft import; the named asterisk) |
| **Filigree entity bindings** | ADR-029 | `clarion-mcp::filigree::EntityAssociation` + `FiligreeLookup`; `IssuesForAccumulator` drift classifier |
| **Summary scope (leaf-only for v0.1)** | ADR-030 | `LEAF_SUMMARY_PROMPT_TEMPLATE_ID` is the single template; `summary` tool description encodes leaf scope |
| **Tooling baseline** (cargo fmt / clippy -D warnings / nextest / doc-D / deny; ruff / mypy / pytest) | ADR-023 | `.github/workflows/ci.yml`: three jobs (`rust`, `python-plugin`, `walking-skeleton`) |

---

## 5. Observations & risks (synthesised)

This section folds the per-subsystem "Concerns" sections of the catalog into a prioritised global view. Each entry cites the originating catalog entry (and source line numbers where they sharpen the claim).

### 5.1 High — worth a Sprint-3 thread

**H-1. `analyze::run` is a single ~500-line `async fn` whose `SoftFailed` branch has no end-to-end coverage.**
Catalog: `clarion-cli` §Concerns. Source: `crates/clarion-cli/src/analyze.rs:40–542` is the function; `:421–429` is the `Completed` → `SoftFailed` promotion (`Completed` + non-empty `crash_reasons`); `:478–509` is the `SoftFailed` branch that folds `UPDATE runs SET status='failed'` into the open entity transaction so partial work commits atomically with the failure marker. `analyze::run` is annotated `#[allow(clippy::too_many_lines)]` — the seams are documented with banner comments, the author clearly sees them. The risk is the unique-to-this-branch invariant: entity inserts and the `failed` marker share a single SQLite transaction. The other two branches (`Completed`, `HardFailed`) are tested; the soft-fail middle is not. **Recommendation:** add `tests/analyze.rs` coverage for `Completed-with-crash → CommitRun(Failed) with partial entity commit` before any extraction refactor.

**H-2. Edge ontology is duplicated across three sites with no compile-time enforcement.**
Catalog: `clarion-storage` §Concerns. `STRUCTURAL_EDGE_KINDS` + `ANCHORED_EDGE_KINDS` are hard-coded at `writer.rs:394–401`; the Python plugin's manifest declares `edge_kinds = ["contains", "calls", "references"]` independently at `plugin.toml:38`; ADRs 026/028 are the design source. Adding a kind requires edits in all three places; no test fails when only one moves. The host handshake catches manifest/manifest skew but not manifest/writer skew. **Recommendation:** lift `EdgeKindRegistry` to `clarion-core` so both the writer and the manifest validator consume one definition; alternatively, a unit test that builds the manifest's `edge_kinds` set and asserts it is a subset of the writer's hard-coded constants.

**H-3. The ADR-024 single-migration retirement trigger is on manual discipline.**
Catalog: `clarion-storage` §Concerns. The migration file (`0001_initial_schema.sql:10–16`) documents the trigger ("when an external operator builds `.clarion/clarion.db` from a published Clarion build") but no automated check fires. Three documented edits already (initial; 2026-05-03 ADR-024 vocabulary rename; 2026-05-18 ADR-031 CHECKs). A fourth edit that adds a column when an external Clarion build is in the wild silently breaks downstream operators. **Recommendation:** file a Filigree tracker entry for the trigger condition; ideally surface via a CI guard that fails when the migration file is modified after a published-build marker (a `published_build.txt` or git tag).

### 5.2 Medium — worth a follow-up issue

**M-1. One MCP raw SQL site bypasses `clarion-storage`.**
Catalog: `clarion-mcp` §Concerns. `clarion-mcp/src/lib.rs:2470` (inside `reference_neighbors` declared at line 2455) is the only `conn.prepare(` site in the crate; everything else routes through typed query helpers in `clarion-storage::query`. A schema change to `edges` (column rename, ADR-026 kind addition) would not surface as a `clarion-storage` test failure. **Recommendation:** push `reference_neighbors` into `clarion-storage::query` with a typed result row; this keeps the storage layer the single point where edge-table SQL evolves.

**M-2. `BudgetLedger.blocked` is sticky for the lifetime of `ServerState`.**
Catalog: `clarion-mcp` §Concerns (`lib.rs:1180–1316`). Once any reservation overshoots, `blocked` flips true and every subsequent LLM tool returns `token-ceiling-exceeded` until process restart. No documented reset path; no surface to lift the ceiling. Matches a session-token semantic but is undocumented in the public API. **Recommendation:** either document the session semantic in the `clarion.yaml` operator docs OR expose an MCP admin tool `reset_budget` that requires a token-ceiling-exceeded precondition.

**M-3. Python plugin's `ONTOLOGY_VERSION` is duplicated.**
Catalog: Python plugin §Concerns. `server.py:36` declares `ONTOLOGY_VERSION = "0.5.0"`; `plugin.toml:46` declares the same. No shared import. The handshake validates the manifest value, leaving the constant as silent skew. Same shape of duplication as H-2 but lower blast radius (single language plugin, single ADR). **Recommendation:** import from `tomllib` at startup; assert equality on first use.

**M-4. Dead stateless `handle_tool_call` stub in MCP public API.**
Catalog: `clarion-mcp` §Concerns. `lib.rs:1701–1736` emits `tool-unimplemented` envelopes for every tool name. Reachable from the stateless `handle_json_rpc` (line 154) and `handle_frame` (line 1621) paths, which are exported. CLI uses `handle_frame_with_state` (the stateful path) so this is not a runtime defect inside Clarion, but it is a footgun in the public API. **Recommendation:** either remove the stateless tool-call surface from public exports, or forward to the stateful handlers behind a `OnceCell`-style state initialiser.

**M-5. `source_excerpt` reads live disk, not the hashed snapshot.**
Catalog: `clarion-mcp` §Concerns. `lib.rs:2151` uses `std::fs::read_to_string` on the on-disk file path to build LLM prompt input. The cache key still uses the stored `content_hash`, so a stale read produces a cache miss with a fresh-but-misaligned prompt. `stale_semantic` covers structural drift but not source-text drift. **Recommendation:** either persist a hash-keyed snapshot of source excerpts at ingest time, or refuse the summary call when the file's current hash != the stored hash (returning a typed `content-drift` error that callers can act on).

### 5.3 Lower — observed but bounded

**L-1.** `--force` flag declared but unimplemented in `clarion install` (`cli.rs:17–18`; rejected at `install.rs:87–92`). Either implement or drop.
**L-2.** `clarion-cli` walks the source tree but does not honour `.gitignore` (P4 in-code at `analyze.rs:1078`); `SKIP_DIRS` is a coarse stopgap. At `elspeth` scale this likely means walking generated/vendored paths.
**L-3.** Hand-rolled date math (`civil_from_unix_secs` in `analyze.rs:1197–1220`; `days_from_civil` in `clarion-mcp/src/lib.rs:2306–2314`). Justified for Sprint 1 by avoiding `chrono`; with `dotenvy`, `blake3`, `uuid`, `tracing-subscriber` already in tree, the rationale is thinner.
**L-4.** AST is re-parsed once by `extractor.py` and once by `pyright_session._build_function_index` per `analyze_file`. At elspeth scale (~425k LOC Python) this is two AST walks per file. Not yet measured; B.8 is exactly where this would surface.
**L-5.** `clarion-cli` serve and analyze can run concurrently against the same `.clarion/clarion.db` (two writer-actor processes); correctness depends entirely on `clarion-storage` WAL + `busy_timeout=5000`. The CLI does not test this combination; the discipline is in `clarion-storage` and trusted from above.
**L-6.** `clarion-core/src/plugin/host.rs` has 14 `HostFinding` constructors at lines 450–637 inflating the file size; moving them to a sibling `host_findings.rs` would shave ~200 LOC without altering semantics.
**L-7.** Filigree `actor` header silently omitted when blank rather than rejected at config-load (`filigree.rs:94–96`).
**L-8.** No `Drop` cleanup for in-flight `broadcast::Sender` map entries on leader cancellation in MCP coalesced dispatch; low-probability leak, time-bounded by the 60-second wait.

### 5.4 Risks the doctrine consciously accepts (do not "fix")

**A-1.** Wardline soft-import asterisk (`wardline_probe.py:38`). Documented in `docs/suite/loom.md` §5 with a retirement condition. Sprint-1 lock-in.
**A-2.** Wardline → Filigree pipeline coupling routed through Clarion. Same §5 register.
**A-3.** `clarion-core/src/plugin/host.rs` size (3 126 LOC, prod 1 450). Coherent; banded; tests dominate; not a refactor priority.
**A-4.** `clarion-mcp/src/lib.rs` size (2 623 LOC). Coherent; banded (protocol surface → `ServerState` → per-tool handlers → LLM pipelines → transport loop → helpers). Worth subdivision on size grounds, not correctness grounds. The catalog calls out the five plausible standalone modules.

---

## 6. Sprint-2 deltas and working-tree state

(Source: `01-discovery-findings.md` §7; cross-validated by `git log` and `git status` at analysis time.)

**Whole-of-Sprint-2.** Six merged feature work-packages plus the OpenRouter provider swap since `v0.1-sprint-1`:
- **B.2** — class + module entities (merge `e53191d`)
- **B.3** — contains edges (merge `f9bd31e`, ontology `0.3.0`)
- **B.4*** — calls edges + confidence tiers (merge `837d965`, ontology `0.4.0`)
- **B.5*** — references edges via pyright (merge `e988a83`, ontology `0.5.0`)
- **B.6** — seven-tool MCP surface (merge `ed64a16`)
- **B.8** — scale test on the `elspeth` corpus (sign-off `ffdfd79`, GREEN after one repair rerun)
- **OpenRouter swap** — replace Anthropic with OpenRouter (merge `35be4db`, infrastructure change, not a feature work-package)

**Sprint hygiene** commits worth keeping in cache: `9ffc5c8` replaced `serde_yaml` with `serde_norway` (the safe YAML parser); `dc9bf41` made `.env` loading happen before tracing init; `a80c31a` added `.env` to `.gitignore`; `c7ec1dd` introduced ADR-031 CHECK clauses; `0cb61b4` fixed the manifest rule-ID grammar quantifier.

**Current branch (`sprint-2/b8-scale-test`) vs. `main`** (at analysis-emit time, commit `363bb0a`). 17 commits ahead, 59 files changed, 25 650 insertions, 85 deletions. The bulk of insertions are committed test artifacts (B.8 scale-test result JSON snapshots, including new raw artifacts from `caa6459`); substantive source changes are modest:
- `crates/clarion-core/src/llm_provider.rs` +193 lines (OpenRouter strict-JSON path)
- `crates/clarion-mcp/src/lib.rs` +260 lines through `363bb0a` (B.8 follow-up — reservation-poison fix `87036b1` + inferred-target pre-filter `363bb0a`)
- `crates/clarion-storage/migrations/0001_initial_schema.sql` +33 lines (ADR-031 CHECK clauses)
- `crates/clarion-storage/tests/schema_apply.rs` +148 lines (new test verifying CHECK enforcement)
- `docs/clarion/adr/ADR-031-schema-validation-policy.md` +426 lines (new ADR)
- `plugins/python/src/clarion_plugin_python/extractor.py` +98 lines (B.8 `@overload`-stub skip + safety-net dedup)
- `plugins/python/tests/test_extractor.py` +184 lines

**Working tree (not yet committed).** `git status` at analysis time shows seven modified files + one new ADR (`ADR-031`) + a B.8 result-snapshot directory (`tests/perf/b8_scale_test/results/2026-05-18T0017Z/`). The GREEN-rerun results are partially committed and partially still in the working tree — this is in-flight work, not stale.

---

## 7. Recommended follow-ups (prioritised)

Each item maps to a §5 entry. Recommend filing in Filigree under `release:v0.1`, `sprint:3`.

| # | Priority | Item | Rationale | Effort |
|---|---|---|---|---|
| 1 | High | Add `SoftFailed`-branch integration test to `tests/analyze.rs` | H-1 — unique transaction invariant currently uncovered | S |
| 2 | High | Unify edge-kind registry across writer / manifest / ADR | H-2 — silent skew risk on Sprint-3 ontology additions | M |
| 3 | High | Filigree tracker for ADR-024 migration retirement condition | H-3 — manual discipline at risk when external operators arrive | XS |
| 4 | Medium | Push `reference_neighbors` SQL into `clarion-storage::query` | M-1 — schema-coupling outside the storage layer | S |
| 5 | Medium | Decide on session-budget reset semantic + document | M-2 — operator surprise after one ceiling breach | S |
| 6 | Medium | Single-source `ONTOLOGY_VERSION` in Python plugin (read `plugin.toml` at startup) | M-3 — same shape as H-2, lower blast radius | XS |
| 7 | Medium | Remove or fold the stateless `handle_tool_call` MCP stub | M-4 — public-API footgun | XS |
| 8 | Medium | Decide `source_excerpt` policy: snapshot at ingest, or refuse on drift | M-5 — correctness drift in LLM prompts under file edits | M |
| 9 | Low | Implement or remove `clarion install --force` | L-1 | XS |
| 10 | Low | `.gitignore`-aware source walk | L-2 — relevant at elspeth-scale | M |
| 11 | Low | Decide on `chrono` adoption vs. keep hand-rolled date math | L-3 | XS |
| 12 | Low | Measure AST re-parse cost at elspeth scale (B.8 telemetry) | L-4 | S |
| 13 | Low | Document or test concurrent `serve` + `analyze` against one DB | L-5 | M |
| 14 | Low | Move `HostFinding` constructors to a sibling module | L-6 — cosmetic | XS |
| 15 | Low | Reject blank Filigree `actor` at config-load | L-7 | XS |
| 16 | Low | `Drop` cleanup for `inferred_inflight` map entries | L-8 — bounded leak | S |

(Carryover items from Sprint 1, listed in `CLAUDE.md` and not duplicated here: `clarion-48c5d06578` supervisor drain/discard; `clarion-928349b60f` jail TOCTOU; `clarion-35688034f0` read_frame deadline; `clarion-c0977ac293` RLIMIT_AS end-to-end observation; `clarion-adeff0916d` fixture-binary self-build. These should be triaged into Sprint-3 alongside §5's new finds.)

---

## 8. Confidence and limitations

### Confidence summary

| Aspect | Confidence | Basis |
|---|---|---|
| Crate boundaries + dependency graph | **High** | `Cargo.toml` workspace + `use clarion_*::` grep per crate |
| File-level structure of every subsystem | **High** | Per-subsystem agent read each crate's `src/*.rs` modules in full or near-full; LOC totals verified by `wc -l` |
| ADR alignment claims | **High where ADR-cited; Medium where inferred** | ADRs 002/003/007/011/021/022/023/024/026/027/028/029/030/031 read or quoted; ADR text not re-opened in every subsystem pass |
| LLM dispatch path correctness | **High** | Cache-key construction sites cited in both `clarion-mcp::lib.rs` and `clarion-storage::cache`; ADR-007 5-tuple verified literal-for-literal |
| Sprint-2 deltas | **High** | `git log v0.1-sprint-1..HEAD` + `git diff --stat main..HEAD` + `git status` |
| Quality / aesthetic judgments | **Out of scope** | This pass produces description, not assessment; see `axiom-system-architect:assess-architecture` for an opinionated quality review |

### Limitations and information gaps

- **Test coverage is not depth-read.** Test files were enumerated and sampled (signatures, top-level test names); no test was read end-to-end. Claims like "no `SoftFailed` integration coverage" rely on file-signature inspection, not function-by-function reading.
- **External siblings (Filigree, Wardline) are not vendored.** Cross-product claims rely on manifest pins (`min_version = "1.0.0"`, `max_version = "2.0.0"`) plus the doctrine memos under `docs/suite/`. The actual sibling repo state was not inspected.
- **The `scripts/` directory** was inventoried by `ls` only (one bash + small helpers per discovery); not deep-read this pass.
- **B.8 scale-test result data** (~22 000 lines of JSON snapshots in `tests/perf/b8_scale_test/results/`) was not inspected. Only the harness (`driver.py`) was sampled for shape, and the sign-off (`docs/implementation/sprint-2/b8-results.md`) was read.
- **One catalog contradiction was corrected during validation** (the `clarion-core` entry incorrectly stated `clarion-plugin-fixture` does not depend on `clarion-core`; the actual dep is on `clarion-core` types only and was corrected in-place). One minor LOC drift was footnoted (`clarion-mcp/src/lib.rs` 2 620 → 2 623). No other factual discrepancies survived the validation pass.

### Audit trail

```
docs/arch-analysis-2026-05-18-1244/
├── 00-coordination.md          coordination plan + execution log
├── 01-discovery-findings.md    holistic scan (384 lines, single explorer)
├── 02-subsystem-catalog.md     6 entries (~650 lines, 5 parallel explorers)
├── 03-diagrams.md              5 Mermaid diagrams (this coordinator)
├── 04-final-report.md          this report (this coordinator)
└── temp/
    ├── section-*.md             6 raw section files from subsystem agents
    ├── validation-02-subsystem-catalog.md   NEEDS_REVISION (warnings) → fixed
    └── validation-03-diagrams.md            APPROVED
```

Subagent identities used: `axiom-system-archaeologist:codebase-explorer` (6 instances, 5 in parallel + 1 for discovery); `axiom-system-archaeologist:analysis-validator` (2 instances — catalog + diagrams).
