# 02 — Subsystem Catalog

**Date:** 2026-06-02 · Branch `feat/road-to-first-class` · v1.1.0
Eight subsystems. Full per-subsystem backing detail (every file:line) lives in `temp/catalog-{core,policy-llm,storage,pipeline,cli-http,mcp,scanner-fixture,python}.md`. This document is the synthesized canonical entry set.

Severity key: 🔴 High (correctness / security / change-amplification) · 🟡 Medium (operational friction) · 🟢 Low (cleanup).

---

## 1. Core / Plugin Host

**Location:** `crates/clarion-core/src/` (11,981 LOC) — `plugin/{host,manifest,protocol,transport,discovery,breaker,jail,limits,mock,host_findings}.rs`, `entity_id.rs`, `errors.rs`.

**Responsibility:** Assemble deterministic 3-segment entity IDs, supervise untrusted language-plugin subprocesses over a synchronous JSON-RPC wire, and own the frozen wire-error vocabularies (`HttpErrorCode`, `McpErrorCode` in the new `errors.rs`).

**Key components:** `PluginHost<R: BufRead, W: Write>` (`host.rs`, 2,958 LOC — generic-over-IO so `mock.rs` drives it in-process); 4-stage per-entity validation pipeline (field-size → ontology → identity recompute → path-jail); `transport.rs` frame-ceiling-before-body; `breaker.rs` (`PathEscapeBreaker` host-owned, `CrashLoopBreaker` caller-owned); `limits.rs` (`setrlimit` via `pre_exec`, `EntityCountCap`); `discovery.rs` ($PATH scan for `clarion-plugin-*`); `entity_id.rs` (pure function, parity-fixture gated).

**Dependencies:** Inbound — `clarion-cli` (analyze), `clarion-mcp` (protocol/error types), `clarion-storage` (one facade-bypass leak). Outbound — `nix`, `blake3`, std process/thread.

**Patterns:** Generic-over-IO supervisor + in-process mock; drop-with-finding vs kill-with-error asymmetry; per-frame ceiling rejection without body-consume; detached stderr-drain thread into a ring buffer; caller-owns-reaping subprocess contract.

**DRIFT:**
- 🔴 **`system-design.md` §2:155-185** describes tokio/async supervision, `tokio::sync::mpsc` 100-msg backpressure, streaming per-entity notifications, and a `file_list` RPC — **none exist**; the host is fully synchronous (`std::process::Command` + one blocking thread). No reconciling ADR.
- 🟡 §2:167 lists `clarion_version` in `initialize`; `InitializeParams` (`protocol.rs:318`) carries only `protocol_version` + `project_root`.
- 🟡 §2:193-198 lists manifest fields `tags`, `capabilities.confidence_basis`, `supported_rule_ids`, `prompt_templates`; none exist in `Manifest` (ADR-022's shipped schema is more minimal).

**Quality / debt:**
- 🔴 `host.rs` 2,958 LOC in one `impl` (+678 LOC inline tests) — split along lifecycle/pipeline/IO axes (filigree `clarion-2b8811da39`).
- 🟡 `clarion-storage/src/writer.rs:537` reaches `clarion_core::plugin::manifest::RESERVED_ENTITY_KINDS` via internal module path, bypassing the `lib.rs` facade — re-export through the facade.
- 🟡 ~11 hardcoded operational limits (recompile-to-tune); `breaker.rs:7` notes "lands in WP6".

**Confidence:** High — impl blocks read end-to-end; §2 cross-validated.

---

## 2. Policy Engine / LLM Provider

**Location:** `crates/clarion-core/src/llm_provider.rs` (2,500 LOC).

**Responsibility:** Define the `LlmProvider: Send + Sync` trait and **four** adapters, plus the three versioned prompt-template builders. The runtime budget/cache policy lives in `clarion-mcp`; the analyze-time policy engine §5 describes is **not built**.

**Key components:** `RecordingProvider` (replay fixture), `OpenRouterProvider` (live reqwest HTTPS), `ClaudeCliProvider` + `CodexCliProvider` (subprocess); prompt builders `build_leaf_summary_prompt` / `build_inferred_calls_prompt` / `build_coding_agent_provider_prompt`; `tier_to_model` (matches `"summary"|"inferred_edges"`).

**Dependencies:** Inbound — `clarion-mcp` summary path (the only live caller; `analyze.rs` issues **zero** provider calls). Outbound — `reqwest`, `tempfile`, `which`, subprocess.

**Patterns:** Provider abstraction with a recording provider for tests; per-tier model routing; session token ceiling (enforced in MCP, not here).

**DRIFT (all hard vs §5):**
- 🔴 No `AnthropicProvider` — the deprecated config variant returns `ConfigError::DeprecatedProvider` (`clarion-mcp/src/config.rs:100`); **four** provider impls ship (Recording, OpenRouter, Claude CLI, Codex CLI).
- 🔴 `CachingModel` has one variant (`OpenAiChatCompletions`); the 4-segment Anthropic `cache_control` strategy §5 describes is absent.
- 🟡 Trait is synchronous (`estimate_tokens -> u64`), not async `estimate_cost -> CostEstimate`; tiers are task-named, not `haiku|sonnet|opus`.
- 🟡 `CLA-INFRA-BUDGET-*` subcodes and the `cost_report` MCP tool are absent workspace-wide.

**Quality / debt:**
- 🟡 **Crate placement** — `reqwest`/`tempfile`/`which` are `clarion-core` deps *solely* for this file; the plugin-supervisor crate carries a live TLS/HTTP transport. Extract `clarion-llm` (filigree `clarion-141e9c08c8`, P2). Still warranted.
- 🟡 Codex CLI `cost_usd` hardcoded `0.0` (`llm_provider.rs:544`) — cost accounting blind for Codex runs; malformed Codex JSONL under-reports tokens (`:1039-1056`).
- 🟢 CLI subprocess stdout reader has no ring-buffer cap (cf. the host's stderr drainer); no cross-provider trait-contract uniformity test.

**Confidence:** High for `llm_provider.rs` + `config.rs` (read end-to-end); Medium that the 3-tier config hierarchy is fully absent.

---

## 3. Storage

**Location:** `crates/clarion-storage/src/` (6,572 LOC) — `writer, reader, query, schema, sei, wardline_taint, prior_index, cache, retry, pragma, commands, error, unresolved`.

**Responsibility:** Sole owner of the SQLite file: PRAGMA discipline, migrations, single-writer-actor mutation, reader-pool reads, graph-invariant enforcement, the read-side query catalogue, and (new) the SEI binding store and Wardline taint store.

**Key components:** writer-actor on `spawn_blocking` (bounded mpsc cap 256, batch-commit every 50 writes, per-run super-transaction, 18 `WriterCmd` variants); reader pool (`deadpool-sqlite`, `Runtime::Tokio1`); `query.rs` (1,727 LOC hand-written SQL); `sei.rs` (1,143 LOC — ADR-038 Stable Entity Identity: blake3 surrogate `clarion:eid:<128-bit hex>`, fail-closed `rebind_or_mint` matcher, `sei_bindings`/`sei_lineage`, migrations 0005/0006); `prior_index.rs` (434 LOC — `sei_prior_index` shape-independent locator→body_hash snapshot, migration 0004); `wardline_taint.rs` (534 LOC — SP9/ADR-036 taint-fact store, read-by-SEI).

**Dependencies:** Inbound — `clarion-cli`, `clarion-mcp`. Outbound — `rusqlite`, `deadpool-sqlite`, `clarion-core` (`EdgeConfidence`, `RESERVED_ENTITY_KINDS`).

**Patterns:** Single-writer-actor discipline; wire-contract enforcement at the writer boundary (`STRUCTURAL_EDGE_KINDS`/`ANCHORED_EDGE_KINDS` tables, `parent_contains_mismatch` bijection abort); PRAGMA discipline (WAL asserted, `application_id`/`user_version` enforced); FK-ordered ingestion.

**DRIFT:**
- 🟡 `detailed-design.md:611-760` documents **6 `CREATE TABLE` + 1 FTS5** virtual table; reality is **13 tables + 1 FTS5 + 1 view across 6 migration files**; 6 tables and the `entities.signature` column are undocumented.
- 🟢 detailed-design transaction-cadence text says "10 files"; actual default is 50 writes.
- 🟢 `writer.rs:537` facade-bypass leak (shared with subsystem 1).

**Quality / debt:**
- 🟡 SEI matcher loads **all** alive bindings into a HashMap at re-index start — unbounded at elspeth scale.
- 🟡 No visible unit test for `WriterCmd::ResumeRun`.
- 🟢 `query.rs` (1,727) and `writer.rs` (1,211, 18-variant match) approaching split-me size.
- ✅ **Resolved since prior:** `application_id`/`user_version` now implemented; `blake3` no longer vestigial (SEI hash + file hash).

**Confidence:** High — all 14 source + 6 migration files read end-to-end.

---

## 4. Analysis Pipeline

**Location:** `crates/clarion-cli/src/` — `analyze.rs` (3,542 LOC), `clustering.rs`, `sei_git.rs` (634), `analyze_lock.rs`, `run_lifecycle.rs` (~4,886 total).

**Responsibility:** Drive `clarion analyze` — walk the corpus, supervise plugins, ingest entities/edges in FK order, cluster into subsystems, and (new) mint/carry SEIs.

**Key components:** `run_with_options` (**836-LOC monolith**, still `#[allow(clippy::too_many_lines)]`); ~9 sequential stages; `clustering::cluster_with_leiden` (xgraph Leiden + deterministic seed + `local_weighted_components` fallback); `run_sei_mint_pass` (~192 LOC post-`CommitRun`); `sei_git.rs` rename hints (`ShellGitRenameSource` working-tree, `LegisGitRenameSource` committed ranges); `analyze_lock.rs` single-run lock; orphan-run recovery before the writer-actor exists.

**Dependencies:** Outbound — core plugin host, storage writer, scanner. Inbound — CLI main, MCP `analyze_start`.

**Patterns:** Nested gates; secret-scan-before-`BeginRun`-before-plugin-spawn ordering; two breakers (path-escape in host, crash-loop in run loop); deterministic clustering seed; new Wave-0 prior-index flush / Wave-1 SEI mint / Wave-2 incremental file-hash skip; SEI pass is enrich-only (failure swallowed post-commit), skippable via `--no-sei`.

**DRIFT:**
- 🔴 The four **Phase-7 structural findings** §6 specifies (`CLA-FACT-TIER-SUBSYSTEM-MIXING`, `CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS`, `CLA-FACT-ENTITY-DELETED`, `CLA-FACT-GUIDANCE-ORPHAN`) are **doc-specified but unimplemented**. (Note: one *other* structural finding, `CLA-FACT-CLUSTERING-WEAK-MODULARITY`, *does* ship — `analyze.rs:50` — so the phase-7 mechanism is partially present, not wholly absent.)
- 🟡 §6 phase-flow lists phases 0/2/4–7 with no deferral notice; entity-vanish is tracked via SEI orphan lineage, not the documented entity-set-diff.
- 🟡 Three implemented phases (Wave 0/1/2) are undocumented in §6.

**Quality / debt:**
- 🔴 `run_with_options` 836 LOC; split ticket `clarion-cb9676de57` (→ `analyze/phase3.rs` + `analyze/mapping.rs`) is `proposed`/unstarted.
- 🟡 Plugin `HostFinding`s are log-only ("Tier B persistence is future work", `analyze.rs:626`) — operator-invisible.

**Confidence:** High — all five files read; §6 + phase-7 catalog read; drift grep-verified.

---

## 5. CLI Surfaces + Federation HTTP Read API

**Location:** `crates/clarion-cli/src/` (analyze excluded) — `main, cli, serve, http_read (4,387), install, skill_pack, hook, hooks_settings, mcp_registration, doctor, instance, config, db, secret_scan{,/*}, stats` (~9,300 LOC).

**Responsibility:** The `clarion` binary's operator surface — 6 subcommands, `.clarion/` init, orientation-asset install (skill pack, SessionStart hook, `.mcp.json`), `doctor`, the `serve` two-runtime supervisor, the secret-scan driver, and the full federation HTTP read API.

**Key components:** `serve::run` (current-thread MCP rt + multi-thread Axum rt sharing one `ReaderPool`, identity proved via `Arc::ptr_eq`); `http_read.rs` **16 production routes** (files×3, call-graph linkages×4, SEI identity resolution×4 under `/api/v1/*`; `/api/v1/_capabilities` unprotected; `/api/wardline/*` ×4); hand-rolled HMAC-SHA256 (`http_read.rs:649-671`); `install`/`skill_pack`/`hooks_settings`/`mcp_registration`; secret-scan driver `scan_source_files_parallel`.

**Dependencies:** Outbound — core, storage, scanner, mcp (`HttpReadConfig` trust matrix lives in `clarion-mcp::config`).

**Patterns:** Auth precedence HMAC (`X-Loom-Component: clarion:<hmac>`) → bearer → loopback-trust-with-WARN, all constant-time compare; 16 KiB body cap; 64 concurrency; fail-loud middleware (panic on unenumerated error); shared-pool identity proof.

**DRIFT:**
- 🟡 `system-design.md §9` documents `GET /api/v1/entities/resolve?scheme=` as shipped — **does not exist**; `contracts.md` confirms deferred. §9 is stale vs the live 16-route surface; **`contracts.md` is the authoritative wire contract** and §9 lacks a cross-link to it.
- ✅ **Resolved:** the prior-handover `UNAUTHORIZED`→`UNAUTHENTICATED` item — HTTP *status* is `401 UNAUTHORIZED`, wire `code` is `"UNAUTHENTICATED"` per `HttpErrorCode::Unauthenticated`, matching `contracts.md:74-79`. Not a live defect.

**Quality / debt:**
- 🔴 `http_read.rs` 4,387 LOC — natural module boundaries (files / linkages / identity / wardline) exist but unexploited.
- 🟡 Optional second writer-actor (ADR-036 Wardline taint store) runs in the HTTP runtime with no separate health-check surface.

**Confidence:** High — all in-scope files read; fixtures not executed; trust-matrix impl delegated to `clarion-mcp` (cited).

---

## 6. MCP Consult Surface

**Location:** `crates/clarion-mcp/src/` (13,796 LOC) — `lib.rs` (7,101), `config, filigree, filigree_url, snapshot, index_diff, scan_results, wardline_reconcile, analyze_runs`, `catalogue/{mod,shortcuts,inspection,faceted}`.

**Responsibility:** The full MCP JSON-RPC tool surface consult-mode agents call to query the entity/graph index — stdio transport (dual-framing autodetect), `ServerState` dispatcher, `BudgetLedger`, faceted search + shortcuts, snapshot resource, index_diff, scan-results emission, Filigree client, Wardline reconciliation.

**Key components & current tool set — 35 tools** (up from 19):
- Navigation (8): `entity_at, find_entity, callers_of, execution_paths_from, neighborhood, subsystem_members, subsystem_of, call_sites`
- Enrichment/inspection (6): `summary, summary_preview_cost, source_for_entity, guidance_for, findings_for, wardline_for`
- Composite/status (3): `orientation_pack, project_status, issues_for`
- Analyze lifecycle (3): `analyze_start, analyze_status, analyze_cancel`
- Delta/freshness (1): `index_diff`
- Faceted search (3, `catalogue/faceted.rs`): `find_by_tag, find_by_kind, find_by_wardline`
- Exploration-elimination shortcuts (11, `catalogue/shortcuts.rs`): 2 on-demand graph queries (`find_circular_imports`, `find_coupling_hotspots`) + 9 honest-empty categorisation/churn shortcuts (`find_entry_points, find_http_routes, find_data_models, find_tests, find_deprecations, find_todos, what_tests_this, high_churn, recently_changed`)

**Dependencies:** Outbound — storage reader pool (~30 sites), core protocol/LLM types, `reqwest` for Filigree.

**Patterns:** Strictly sequential per-session dispatch; framing autodetect (`Content-Length` vs bare line); honest-empty SEI-carrying shortcuts; enrich-only Wardline/Filigree reconciliation by qualname; closed/fixture-backed response envelopes; token budgeting via `BudgetLedger`.

**DRIFT:**
- 🔴 `system-design.md §8:773` still says "v1.0 ships an 8-tool subset" and §8:791 marks all shortcuts "deferred to v1.1" — **false; 35 tools ship.** `detailed-design.md §6` documents only the (unbuilt) cursor-model tool surface; the 35 shipped tools are undocumented there. (The cursor/session model is correctly labelled v1.1 and not implemented — that part is *not* drift.)

**Quality / debt:**
- 🔴 `lib.rs` 7,101 LOC holds 18 tools + all infra; WS5 split only 17 tools into `catalogue/`. Finish the split into `tools/` (filigree `clarion-42cbd8a25a`).
- 🟡 `analyze_runs.rs` has no stale-`running`-row reconciliation on supervisor crash — can block future `analyze_start`.
- 🟡 Sequential stdio dispatch blocks all calls behind a slow LLM-dispatching `summary`.
- ✅ **Resolved:** blocking `reqwest::blocking` in async handlers — all three Filigree call sites now use `spawn_blocking`.

**Confidence:** High — 35-arm dispatch + `faceted.rs`/`shortcuts.rs`/`wardline_reconcile.rs` read in full; `inspection.rs` sampled; spawn_blocking fix traced.

---

## 7. Secret Scanner + Plugin Fixture

**Location:** `crates/clarion-scanner/src/` (881 LOC), `crates/clarion-plugin-fixture/src/main.rs` (187 LOC).

**Responsibility:** Scanner — a **pure detection library** (given bytes, emit deduplicated `Detection`s; YAML baseline for suppression); does not walk the FS. Fixture — a test-only binary implementing the minimum valid plugin protocol to exercise host/OOM/crash-loop paths deterministically.

**Key components:** Scanner — `default_pattern_meta()` (`patterns.rs:194-269`) = **12 named pattern rules** + **2 entropy classes** (BASE64 min_len 20 / entropy 4.5; HEX min_len 40 / entropy 3.0) = 14-variant `DetectSecretsRule`; baseline suppresses only exact `(file, rule_type, hashed_secret, line)` with `is_secret:false`. Fixture — 5 JSON-RPC methods (`initialize, initialized, analyze_file, shutdown, exit`).

**Dependencies:** Scanner — none internal. Fixture — `clarion-core` JSON-RPC types only.

**Patterns:** Pure-detection-library (caller drives FS walk + parallelism); closed rule enum aligned to Yelp `detect-secrets`; baseline-that-won't-mask-drift (changed hash at same line not suppressed — locked by `tests/scanner.rs:509-556`); entropy fallback.

**DRIFT:**
- 🟢 **ADR-013 §Coverage is NOT drift** (corrected after validation). `ADR-013:93` promises GCP service-account coverage *"via `\"private_key\"` + RSA header"* — i.e. via the **generic** mechanism, not a dedicated named rule — and `patterns.rs` detects exactly that way (general `PrivateKey` rule + entropy). Doc and code agree. The only residual note is a doc-clarity nit: no design doc enumerates all 12 named rules, so a reader must consult `patterns.rs`.
- 🟢 OpenAI extended prefixes (`sk-proj-`/`sk-svcacct-`) and Stripe *test* keys are detected in code but undocumented in ADR-013 (doc lag, not a code defect).

**Quality / debt:** 🟢 Fixture lacks stderr-on-crash, sequencing guard; Unix-only fault injection. Scanner is clean and well-tested.

**Confidence:** High — both crates read end-to-end; ADR-013 + §10 read.

---

## 8. Python Language Plugin

**Location:** `plugins/python/src/clarion_plugin_python/` (3,173 LOC) — `server, extractor (1,052), pyright_session (1,427), entity_id, reference_resolver, call_resolver, qualname, wardline_probe, stdout_guard, __main__`.

**Responsibility:** Ingest Python source — parse with CPython `ast`, emit `module`/`class`/`function` entities (3-segment IDs, L7 qualnames), anchor `contains`+`imports` directly, delegate type-resolved `calls`/`references` to a managed `pyright-langserver` subprocess; probe Wardline at `initialize`.

**Key components:** `server.py` (5 JSON-RPC methods, Content-Length framing, `MAX_CONTENT_LENGTH = 8 MiB`, `MAX_FILES_PER_PYRIGHT_SESSION = 25`); `pyright_session.py` (full LSP client; `MAX_PYRIGHT_RESTARTS_PER_RUN = 3`; new `PyrightRunState` shares the cap across recycles); `extractor.py` (3-walk AST + new ADR-038 `FunctionSignature`/`ClassSignature` TypedDicts at `SIGNATURE_SCHEMA_VERSION=1`, `DefinitionSpan` stamped to topmost decorator); `entity_id.py` (parity with Rust via `fixtures/entity_id.json`); `wardline_probe.py`; `stdout_guard.py` (protect the JSON-RPC channel).

**Dependencies:** `pyright` (LSP subprocess); the wire protocol only (no Rust crate dep); `wardline.core.registry` import (the federation **asterisk**); `fixtures/entity_id.json`.

**Patterns:** Pyright-as-a-service with 25-file session recycling; 3-walk AST extraction; fail-soft degradation to `CLA-PY-PYRIGHT-*` findings; cross-language entity-ID parity; stdout guard.

**DRIFT:**
- 🔴 **`system-design.md §2:213-228` (Python specifics)** describes **tree-sitter + LibCST**, `TYPE_CHECKING` exclusion, `alias_of` edges, and `python:unresolved:{}` placeholder entities — **none of these exist** (CPython `ast` only; no tree-sitter/LibCST in `pyproject.toml`). Most significant single drift item.

**Quality / debt:**
- 🔴 **Wardline federation asterisk live** — `wardline_probe.py:38` still `importlib.import_module("wardline.core.registry")`. `loom.md §5` records the Wardline-side prerequisite (NG-25 descriptor, SP2) as met; migration ticket `clarion-1f6241b329` is open/ready/P2. Against a rebuilt Wardline the probe returns `{"status":"absent"}` (fail-soft).
- 🟡 `pyright_session.py` 1,427 LOC; four independent AST walks over the same tree.
- ✅ **Resolved:** 3-restart cap now shared across recycles (`PyrightRunState`).

**Confidence:** High — all 11 files read; ADR-038, §2, loom.md §5, the filigree issue verified directly. (Prior catalog's `MAX_FILES_PER_PYRIGHT_SESSION=49` was a typo; correct value is 25.)
