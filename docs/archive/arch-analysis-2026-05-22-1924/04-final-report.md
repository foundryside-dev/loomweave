# 04 — Final Report: Clarion Architecture Analysis

**Date:** 2026-05-22
**Scope:** Entire Rust workspace (`crates/`) plus the Python language plugin (`plugins/python/`).
**Method:** From-scratch source archaeology. Seven independent codebase-explorer subagents read source, manifests, migration SQL, fixtures, and tests for one subsystem each. **No existing design docs** (`docs/clarion/**`, `docs/suite/**`, `docs/implementation/**`, ADRs, sprint READMEs, prior arch-analyses) were consulted during the analysis. A separate validator subagent spot-checked eight load-bearing factual claims against source.
**Validator status:**
- *Subsystem catalog (`02-`):* NEEDS_REVISION (warnings) — one factual error (Python plugin's `MAX_FILES_PER_PYRIGHT_SESSION` literal) fixed inline; one cosmetic nit accepted; all eight load-bearing spot-check claims passed.
- *Final report (this doc):* APPROVED with editorial warnings — discovery-doc tool-count sweep completed; this top matter clarified.

---

## 1. Executive Summary

Clarion is a **single-binary Rust code-archaeology tool** that ingests source trees through out-of-tree language plugins, persists a typed entity/edge graph in an embedded SQLite database, and exposes that graph to LLM agents through two distinct read surfaces — a stdio **MCP server** with 19 navigation tools, and an authenticated **HTTP read API** for cross-product federation. A reference **Python plugin** (the only in-tree language plugin) drives `pyright` as an LSP subprocess to extract type-resolved call and reference edges.

The architecture is structurally simple — 7 subsystems, ~50K LOC, no inter-crate cycles — but **operationally subtle**. The hard parts live in three places:

1. **Plugin-host subprocess supervision** (`clarion-core::plugin::host`): generic-over-IO synchronous JSON-RPC supervisor, per-frame ceiling rejection without body-consume, four enforcement layers (frame size, path jail, entity cap, crash-loop), forked-child resource limits via `pre_exec`-installed `setrlimit`, detached stderr-drain thread.
2. **Storage's writer-actor discipline** (`clarion-storage::writer`): every mutation routes through one bounded-mpsc actor on `spawn_blocking`; per-run super-transaction; batch commits every 50 writes; wire-contract enforcement (edge kind/confidence/source-range tables, parent↔contains bijection) at the writer boundary so caller bugs cannot corrupt graph shape.
3. **The analyze pipeline as a sequence of nested gates** (`clarion-cli::analyze`): orphan-run recovery → secret scan → BeginRun → per-plugin loop with two distinct breakers (path-escape inside the host, crash-loop in the run loop) → unresolved-call-site → edge resolution → clustering → CommitRun. All in a single 570-line function.

The system has invested heavily in **failure containment**: 11+ hardcoded resource limits with `CLA-INFRA-*` finding subcodes, two independent rolling-window breakers, drop-with-finding vs. kill-with-error asymmetry, cross-language byte-for-byte fixture parity for entity IDs, and a baseline mechanism that intentionally does **not** suppress drifted hashes at the same line. These are the marks of a system that has thought hard about adversarial-plugin and partial-failure scenarios.

The clearest **architectural debt** is at the file-size level: four files (`mcp/lib.rs` 4703, `core/plugin/host.rs` 2935, `cli/analyze.rs` 2549, `core/llm_provider.rs` 2467) hold the bulk of the operational complexity. They are not poorly factored *internally* — each has clearly named functions and inline-test discipline — but each is one file's-worth of change risk per touch.

---

## 2. The System in Code (no doc references)

### 2.1 Subsystem inventory

| # | Subsystem | Type | Source LOC | Test LOC | Confidence |
|---|-----------|------|-----------:|---------:|------------|
| 1 | `clarion-core` | library | 11,653 | 325 (+ inline) | High |
| 2 | `clarion-storage` | library | 3,199 | 4,871 | High |
| 3 | `clarion-cli` | binary (`clarion`) | ~6,800 | ~6,400 | High |
| 4 | `clarion-mcp` | library | ~6,600 | ~2,200 | High (sampled `lib.rs`) |
| 5 | `clarion-scanner` | library | 881 | 655 | High |
| 6 | `clarion-plugin-fixture` | test bin | 187 | (see consumers) | High |
| 7 | `plugins/python` | external bin (`clarion-plugin-python`) | 3,028 | 3,440 | High |

**Totals:** ~32K source / ~18K test (Rust) + 3K source / 3.4K test (Python) = ~50K LOC of first-party code. Test corpus is **~57% of source LOC** — high by typical standards, dominated by `clarion-storage` where tests outweigh source 1.5×.

### 2.2 External dependencies (architecturally significant)

- `tokio` (multi-thread + sync + macros) — runtime for `serve` and the writer-actor.
- `rusqlite` 0.31 with bundled SQLite — embedded DB, no external server.
- `deadpool-sqlite` 0.8 (`rt_tokio_1`) — async-friendly reader pool.
- `axum` 0.7 + `tower` / `tower-http` — HTTP read API (`/api/v1/files*`).
- `reqwest` 0.12 with rustls — outbound HTTP to OpenRouter LLM, Filigree associations.
- `clap` 4 — CLI.
- `nix` — `setrlimit` for plugin children via `pre_exec`.
- `xgraph` — Leiden community detection (with a hand-rolled fallback).
- `serde_norway` — YAML (`clarion.yaml`, secret-scan baseline).
- Python: `pyright` (LSP subprocess), `tomli` (manifest), `pytest`/`mypy --strict` for dev.

No mocked-out networking; the LLM provider trait has a `RecordingProvider` for tests but production hits the real OpenRouter HTTP endpoint or shells out to `claude`/`codex` CLIs.

### 2.3 Wire surfaces (external interfaces)

| Surface | Where | Transport | Auth |
|---|---|---|---|
| `clarion` CLI | `clarion-cli/src/main.rs`, `cli.rs` | argv | n/a |
| Plugin JSON-RPC 2.0 | `clarion-core/src/plugin/protocol.rs` | LSP-style `Content-Length` framing over stdio pipes | none (process boundary is the trust boundary) |
| MCP server | `clarion-mcp/src/lib.rs::serve_stdio_with_state_on_runtime` | stdio (auto-detects `Content-Length` framing vs. bare-JSON-line) | none (caller-trusted) |
| HTTP read API | `clarion-cli/src/http_read.rs:347-372` | HTTPS-capable Axum on `0.0.0.0:<port>` | **HMAC-SHA256** > bearer > **loopback-trust** with operator WARN (4 routes, precedence in code) |
| OpenRouter | `clarion-core/src/llm_provider.rs::OpenRouterProvider` | HTTPS | API key (env) |
| Filigree | `clarion-mcp/src/filigree.rs` | HTTPS | bearer (env) + `x-filigree-actor` header |
| Pyright LSP | (Python plugin) `clarion_plugin_python/pyright_session.py` | LSP subprocess pipes | none |

---

## 3. Architecture Narrative

### 3.1 The shape: a CLI with two persistent modes

The `clarion` binary has **three subcommands** (`install`, `analyze`, `serve`) but two architectural shapes. `install` and `analyze` are one-shot processes; `serve` is a long-running supervised topology with two threads sharing one `ReaderPool`:

- A **current-thread Tokio runtime** drives the MCP stdio server (`clarion-mcp::serve_stdio_with_state_on_runtime`).
- A **multi-thread Tokio runtime** drives the Axum HTTP read API on a configurable port.

`clarion-cli::serve::run` enforces shared-pool identity with `Arc::ptr_eq` (`reader.rs:26-119` exposes a `shares_pool_with` runtime proof via an `Arc<()>` identity tag) — a structural guarantee that both servers observe the same database snapshot. Failure of either thread crashes the binary; there is no per-surface restart.

### 3.2 The analyze pipeline

`clarion-cli::analyze::run_with_options` is a single 570-line function (`analyze.rs:75-645`) that linearises 13 phases:

1. canonicalize project path
2. load `clarion.yaml`
3. raw `UPDATE runs SET status='failed' WHERE status='running'` — **orphan-run recovery before the writer-actor even exists**
4. spawn writer-actor, mint `run_id`
5. plugin discovery via `$PATH` scan for `clarion-plugin-*` executables (`clarion-core::plugin::discovery`)
6. compute extension union from plugin manifests
7. tree walk
8. **parallel secret scan, BEFORE BeginRun, BEFORE any plugin spawn** (`clarion-scanner` driven by `secret_scan::scan_source_files_parallel`)
9. `BeginRun` command to writer-actor
10. per-plugin loop (each plugin runs in `spawn_blocking`): handshake → `analyze_file` × N (with heartbeat logging) → `shutdown`; per-run `CrashLoopBreaker` from `clarion-core` ticks on >3 crashes / 60 s and drops remaining plugins with `FINDING_DISABLED_CRASH_LOOP`
11. entities → unresolved-call-sites → edges ingestion in **strict FK order**
12. phase-3 clustering via `clustering::cluster_with_leiden` (Leiden through `xgraph` with deterministic seed; falls back to `local_weighted_components` if Leiden returns ≤1 community)
13. `CommitRun` (or `SoftFail` / `HardFail` if invariants tripped)

The function carries `#[allow(clippy::too_many_lines)]` (`analyze.rs:74`). The reviewer rated this the single largest concern in the CLI — every change vector listed above goes through the same scope.

### 3.3 Plugin host: the most carefully engineered surface

`clarion-core::plugin::host::PluginHost<R: BufRead, W: Write>` (`host.rs:384-1182`) is generic over reader/writer so the in-process `mock.rs` (876 LOC, `#[cfg(test)] pub(crate)`) can drive it without a subprocess. The four-stage per-entity validation pipeline at `host.rs:866-975` runs for every entity a plugin emits:

0. **Field-size** — 4 KiB per scalar field, 64 KiB per `#[serde(flatten)]` extras map.
1. **Ontology** — `kind ∈ manifest.ontology.entity_kinds`.
2. **Identity** — recomputed `entity_id(plugin_id, kind, qualified_name)` must equal the wire `id` (prevents ID-namespace spoofing).
3. **Jail** — `jail_to_string(project_root, source.file_path)` must succeed; on failure, tick `PathEscapeBreaker`.

Steps 0–2 **drop-with-finding** (continue). Step 3 drops + records a finding; >10 path-escapes / 60 s trips the breaker and **kills the plugin**. A separate post-step **entity cap** (`EntityCountCap::DEFAULT_MAX = 500_000` cumulative) on overflow kills the plugin.

Subprocess hygiene:
- `spawn` returns `(PluginHost, std::process::Child)` — the **caller owns reaping**. `Child::Drop` does not `waitpid` on Unix (documented at `host.rs:630-641`); a handshake-failure inside `spawn` reaps before returning.
- **`pre_exec`-installed `setrlimit`** for `RLIMIT_AS` (virtual address) and `RLIMIT_NPROC` (bumped to 4096 when the plugin manifest declares Pyright capability, because Node's LSP host spawns helper processes counted against the user's nproc).
- **Detached stderr-drain thread** (`host.rs:609-620`) named `clarion-plugin-stderr-drain:<plugin_id>` reads `ChildStderr` 4 KiB at a time into a 64 KiB ring buffer. Rationale at `host.rs:550-561`: an inherited stderr could either flood the operator's terminal or deadlock the plugin on `write(2)` when the host blocks in `analyze_file`.

Validator-confirmed via `host.rs:609-620`, `writer.rs:35,38,813`, `analyze.rs:242,244,277+`.

### 3.4 Storage: an actor + a pool over one SQLite file

`clarion-storage` is the **only path** to SQLite. Concurrency model:

- **One writer-actor** spawned on `tokio::task::spawn_blocking`. Bounded `mpsc::Receiver<WriterCmd>` (capacity 256, `writer.rs:35`), 11 command variants each carrying a `oneshot::Sender<Result<T>>` ack. Per-run super-transaction; batch-cadence commits every 50 writes (`writer.rs:38, 813`).
- **Reader pool** via `deadpool-sqlite` (`Runtime::Tokio1`). Reader PRAGMAs reapplied per acquisition. Production sizes: 16 in `serve.rs`, 4 in `http_read.rs` test.
- **PRAGMA discipline** (`pragma.rs:16-45`): WAL (asserted — the assertion is hard, not advisory), `synchronous=NORMAL`, `busy_timeout=5000`, `wal_autocheckpoint=1000`, `foreign_keys=ON`. **No `application_id` / `user_version`** are set; cross-tool collisions on the DB file are not detected at the SQLite level.
- **Schema migrations** (`schema.rs:17-91`): single `include_str!`-embedded migration, idempotent via a `schema_migrations` table. Explicit comment at lines 45-49 warns that `.ok()` instead of `OptionalExtension::optional()` would silently mask `DatabaseLocked` / `CorruptDb`.

**Wire contracts are enforced at the writer boundary** (`writer.rs:425-582`):
- `STRUCTURAL_EDGE_KINDS = {contains, in_subsystem, guides, emits_finding}` — must be `confidence=resolved`, must have NULL byte ranges.
- `ANCHORED_EDGE_KINDS = {calls, references, imports, decorates, inherits_from}` — must have both byte-start AND byte-end, must NOT be `inferred` at scan time.
- `parent_contains_mismatch` (`writer.rs:954-1021`) — bidirectional SQL pair asserting `entities.parent_id` and `edges WHERE kind='contains'` are bijective. Failure aborts the entire run with `CLA-INFRA-PARENT-CONTAINS-MISMATCH`.

The migration SQL defines 9 tables, 1 FTS5 virtual, 3 triggers, 2 generated columns, 1 view, and crucially `edges` is `WITHOUT ROWID` on natural PK `(kind, from_id, to_id)`.

### 3.5 MCP server: 19 tools (not 20)

`clarion-mcp::serve_stdio_with_state_on_runtime` registers **19 tools** in a `ToolDefinition` registry at `lib.rs:56-257`. The original discovery doc said "twenty"; the catalog and validator both confirmed 19 via direct enumeration. (Discovery's `grep -c 'ToolDefinition {'` had counted the struct declaration plus 19 `vec![]` instances.) Discovery has been corrected in place.

Tool dispatch is **strictly sequential per session** — there is no concurrent tool execution within one MCP connection. The dispatcher auto-detects framing (LSP `Content-Length` vs. bare JSON line) by peeking the first non-whitespace byte. Read path goes through the reader pool (~30 sites); the writer is touched by only **3 command variants** across 4 sites — `InsertInferredEdges` (×2), `TouchSummaryCache` (×1), `UpsertSummaryCache` (×1) — and all are gated on the summary-LLM writer field being configured.

### 3.6 HTTP read API: the federation surface

`clarion-cli::http_read.rs:347` exposes four routes on Axum:

- `GET /api/v1/files`
- `POST /api/v1/files/batch` (cap 256)
- `POST /api/v1/files:resolve` (cap 1000)
- `GET /api/v1/_capabilities` (unprotected)

Auth precedence is **hand-rolled HMAC-SHA256 > bearer > loopback-trust with WARN** (constant-time compare, 16 KiB body cap, 10 s timeout, 64 concurrency). The middleware panics on unenumerated errors — a deliberate fail-loud posture.

### 3.7 Python plugin: pyright-as-a-service

`clarion-plugin-python` is a PEP 517 console-script binary (`pyproject.toml:32-33`) per ADR-021 bare-basename convention. It implements 5 JSON-RPC methods (`initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`; `server.py:237-272`) over Content-Length framing with an 8 MiB cap. The reference plugin and Python plugin share their wire shape with `clarion-plugin-fixture`.

The interesting machinery is **pyright integration**:
- Pyright runs as a subprocess (`pyright-langserver --stdio`), driven by a full LSP client (`PyrightSession`).
- Session is recycled every **25 files** (`MAX_FILES_PER_PYRIGHT_SESSION` at `server.py:49`) — a wholly-separate-from-the-3-restart-cap heuristic.
- Calls use `prepareCallHierarchy` + `callHierarchy/outgoingCalls`.
- References use `textDocument/definition` with `typeDefinition` fallback for annotation references.
- All failures degrade to zero edges + a `CLA-PY-PYRIGHT-*` finding.

The extractor walks the AST three times (recursive `_walk`, `_ImportEdgeCollector`, `_ReferenceSiteCollector`). `@overload` stubs are dropped pre-emit; surviving collisions drop first-wins. Cross-language entity-ID parity is enforced by `tests/test_entity_id.py:25` consuming the same `fixtures/entity_id.json` as the Rust `entity_id.rs:371-587`.

A **`wardline_probe.py`** module attempts `import wardline.core.registry` and reports `{status: absent|enabled|version_out_of_range}` from `initialize`. Fail-soft.

### 3.8 The secret scanner

`clarion-scanner` is a **pure detection library** — it does not walk the FS, does not decide *what* to scan. Callers invoke `Scanner::scan_bytes(&[u8])` per-file. `clarion-cli::secret_scan::scan_source_files_parallel` drives it across the project tree in `analyze.rs:242-243`, **before** `BeginRun` and before any plugin spawn.

Detection: **12 named pattern rules** (AWS, GitHub 3×, Anthropic, OpenAI, Stripe, Slack, JWT, PEM private key, contextual `password|token|api_key = "…"`) + **2 entropy classes** (base64 min-len 20 min-entropy 4.5; hex min-len 40 min-entropy 3.0). All map to a closed 14-variant `DetectSecretsRule` enum at `lib.rs:102-118`, rule-ids aligned to Yelp `detect-secrets`.

The **baseline file** is YAML keyed by path, suppressing on exact `(file, rule_type, hashed_secret, line_number)` quadruples — **only when `is_secret: false`**. Drifted hashes at the same line are deliberately NOT suppressed. Parse-time validation rejects non-1.0 version, absolute/`..` paths, missing justifications, unknown rule types, invalid hex hashes. (Tests at `tests/scanner.rs:509-556` lock this in.)

---

## 4. Dependency Topology

**Inter-subsystem dependencies (Rust):**

```
clarion-cli ──► clarion-core
            ├──► clarion-storage
            ├──► clarion-scanner
            └──► clarion-mcp

clarion-mcp ──► clarion-storage
            └──► clarion-core (for protocol::read_frame, BriefingBlockReason, LLM types)

clarion-storage ──► clarion-core (EdgeConfidence, RESERVED_ENTITY_KINDS — facade leak)

clarion-scanner ── (no internal deps)
clarion-plugin-fixture ──► clarion-core (only for JSON-RPC types)
plugins/python ── (no Rust deps; speaks the wire only)
```

**No cycles.** The dependency graph is a DAG with `clarion-core` at the bottom, `clarion-storage` and `clarion-scanner` as leaves of the lower layer, and `clarion-cli` and `clarion-mcp` as the consumers.

**Notable inbound reach (the facade leak):** `clarion-storage::writer.rs:427` reaches `clarion_core::plugin::manifest::RESERVED_ENTITY_KINDS` *directly* through the module path, bypassing the `lib.rs:13-49` `pub use` facade. The policy comment at `clarion-core::lib.rs:5-7` describes the facade as the supported surface; this constant is not re-exported there.

**The Python plugin is intentionally isolated** — no shared crate dependency, no shared registry. It speaks the wire protocol, consumes the same `fixtures/entity_id.json` the Rust side uses for parity testing, and is otherwise independent of the Rust workspace.

---

## 5. Cross-Cutting Concerns

| Concern | Where |
|---|---|
| **Error model** | Per-module `thiserror` enums composed via `#[from]`. `HostError` (`host.rs:334-370`) wraps eight underlying errors plus three policy variants. `StorageError` (`storage/src/error.rs`) similar. CLI uses `anyhow` at the boundary. |
| **Logging / tracing** | `tracing` + `tracing-subscriber` with `env-filter`. Plugin heartbeats logged from analyze loop (`analyze.rs:1272-1307`). |
| **Async runtime** | `tokio` multi-thread. **Two exceptions**: writer-actor uses `spawn_blocking` (synchronous SQLite); plugin host is fully synchronous (`BufRead`/`Write`, no async). |
| **Config** | `serde_norway` (YAML) for `clarion.yaml`; `clap` derive for CLI. Subprocess plugins read TOML manifests (`plugin.toml`). |
| **Migrations** | Embedded via `include_str!` in `clarion-storage::schema`. Tracked in `schema_migrations` table. No `application_id` / `user_version`. |
| **Security boundaries** | (a) Plugin process boundary (jail + setrlimit + entity cap + path-escape breaker + frame ceiling). (b) HTTP API HMAC > bearer > loopback. (c) Pre-ingest secret scan with baseline suppression that won't mask drift. |
| **Findings vocabulary** | `CLA-INFRA-*` (host enforcement), `CLA-PY-PYRIGHT-*` (Python plugin pyright failures), `CLA-SEC-SECRET-DETECTED` (scanner), `CLA-INFRA-PARENT-CONTAINS-MISMATCH` (storage invariant). Surfaced via `HostFinding` (core) and the standard `Finding` record persisted in SQLite. |
| **Determinism** | Clustering uses a deterministic seed (`clustering.rs`). Entity IDs are pure functions of (plugin_id, kind, canonical_qualified_name). Cross-language byte-for-byte parity test gates against `fixtures/entity_id.json`. |

---

## 6. Risks and Smells (Concrete, Source-Cited)

Severity bands:
- **🔴 High** — affects correctness, security, or change-amplification surface
- **🟡 Medium** — operational friction, will degrade over time
- **🟢 Low** — cleanup opportunity

### 🔴 High

1. **Four monolithic files concentrate change risk.**
   - `clarion-mcp/src/lib.rs` — 4,703 LOC. Holds the 19 tool registry, `ServerState`, all per-tool handlers, the `BudgetLedger`, the `InferredInflight` coalescer, and tests.
   - `clarion-core/src/plugin/host.rs` — 2,935 LOC. Holds the entire `PluginHost<R, W>` impl, the four-stage pipeline, the stderr drainer, `pre_exec` setup, and the shutdown idempotency.
   - `clarion-cli/src/analyze.rs` — 2,549 LOC; `run_with_options` itself is 570 lines.
   - `clarion-core/src/llm_provider.rs` — 2,467 LOC. **Bundles the trait + reqwest HTTP transport + two CLI-subprocess transports + prompt templates in a crate whose `lib.rs:1` doc-comment says it owns "domain types, identifiers, and provider traits"**.

   Refactor split exists for each (pipeline-axis / lifecycle-axis / IO-axis for `host.rs`; tool-category split for `mcp/lib.rs`; per-phase function extraction for `analyze.rs`; new `clarion-llm` crate for `llm_provider.rs`). None is taken yet.

2. **Blocking HTTP inside async (MCP filigree client).** `clarion-mcp::filigree` issues **blocking** `reqwest::blocking::get` calls from `async` tool handlers (validator-flagged; reviewer cited as a real concern). Will tie up the runtime thread on a slow Filigree.

3. **No analyze-child timeout in `serve`.** The MCP `analyze_start` / `analyze_status` / `analyze_cancel` tool family launches an analyze process from inside the MCP server — but the catalog flags "no analyze-child timeout" as an mcp-side smell (`02-subsystem-catalog.md` §4). Inspect before claiming this is hardened.

4. **Subprocess-child reaping ownership is type-system-unenforced.** `PluginHost::spawn` returns `(Self, Child)` where the caller is documented (`host.rs:630-641`) to reap. A `KillOnDrop` newtype around `Child` would make the contract type-mechanical instead of comment-mechanical.

### 🟡 Medium

5. **`clarion-storage`'s SQLite file has no `application_id` / `user_version`.** Two Clarion versions sharing a DB by accident would not be detected at the SQLite layer; only the `schema_migrations` table catches it, and only on write. Adding both PRAGMAs is a one-line change with no downside.

6. **`llm_provider.rs` belongs in its own crate.** It pins `reqwest` and `tokio::time::timeout`-equivalent CLI machinery into `clarion-core` — the crate that supervises plugins. A `clarion-llm` crate would shrink the trust surface of the host runtime.

7. **Facade leak: `clarion-storage::writer.rs:427` reaches `clarion_core::plugin::manifest::RESERVED_ENTITY_KINDS` directly.** Either lift to the facade or expose `Manifest::is_reserved_kind`. As-is, the internal module path is semi-public.

8. **Eleven hardcoded operational limits in `clarion-core`.** `MAX_PROTOCOL_ERROR_FIELD_BYTES`, `MAX_ENTITY_FIELD_BYTES`, `MAX_ENTITY_EXTRA_BYTES`, `STDERR_TAIL_BYTES`, `MAX_HEADER_LINE_BYTES`, `MAX_UNRESOLVED_CALLEE_EXPR_BYTES`, `ContentLengthCeiling::DEFAULT`, `EntityCountCap::DEFAULT_MAX`, `DEFAULT_MAX_RSS_MIB`, `DEFAULT_MAX_NOFILE`, `DEFAULT_MAX_NPROC`. The comment at `breaker.rs:7` acknowledges this lands "in WP6". Every tunable is a recompile today.

9. **Path jail is TOCTOU-by-design.** `jail.rs:67-72` documents this: the canonical-path return is a membership proof at canonicalization time, not a durable file handle. The current consumer doesn't open files, so this is latent; a future caller that does could miss the contract.

10. **Pyright session restart constant `25` is divorced from session's own 3-restart cap.** Interaction between server-driven recycling and session-driven failure restarts is not centrally documented.

### 🟢 Low

11. **Integration tests for `clarion-core` cover only the happy path.** Most `HostError` variants and `CLA-INFRA-*` finding subcodes are tested via inline `#[cfg(test)]` blocks, which is fine but uneven — `tests/host_subprocess.rs` is 325 lines, covering one walkthrough.

12. **Tool count drift in discovery vs. registry.** Discovery initially said 20; actual is 19. Already corrected in this analysis; flag for future readers.

13. **`mock.rs` at 876 LOC is `#[cfg(test)] pub(crate)`.** Volume itself is a smell — the host's pipeline is stateful enough to need a sub-DSL to test, which corroborates the `host.rs` size concern.

---

## 7. Strengths Worth Naming

Concrete things this codebase does *well* that a brownfield analysis should not understate:

- **Generic-over-IO supervisor with in-process mock.** `PluginHost<R: BufRead, W: Write>` plus `mock.rs` is a textbook approach to testing subprocess supervision without spawning subprocesses. The seam at `connect()` (`host.rs:658`) is clean.
- **Wire-contract enforcement at the writer boundary.** Caller bugs cannot corrupt the graph shape. Three structurally-distinct invariants — edge-kind tables, source-file-anchor kinds, parent↔contains bijection — all enforced in SQL-adjacent Rust at `writer.rs:425-582, 954-1021`.
- **Cross-language byte-for-byte parity fixture.** `fixtures/entity_id.json` is consumed by both `clarion-core::entity_id.rs:371-587` and `plugins/python/tests/test_entity_id.py:25`. Drift between the two implementations is caught in CI before either side ships.
- **Per-frame ceiling rejection before body-consume.** `transport.rs:67-71` rejects with `TransportError::FrameTooLarge` without consuming bytes — closing a memory-exhaustion attack vector that "read then check" would leave open.
- **Two breakers, two scopes.** `PathEscapeBreaker` (per-plugin, host-owned) polices one misbehaving plugin's emissions; `CrashLoopBreaker` (per-run, caller-driven) polices the fleet. Same shape, different scope. Named asymmetrically only in *ownership*, not in *interface*.
- **The secret-scanner baseline that won't mask drift.** `(file, rule_type, hashed_secret, line_number)` quadruple match with `is_secret: false` requirement means a changed secret at the same line is *not* suppressed (locked in by `tests/scanner.rs:509-556`). This is a deliberate regression net — the kind of thing easy to get wrong, here gotten right.
- **Deterministic clustering with hand-rolled fallback.** `clustering.rs` uses Leiden via `xgraph` with a seeded RNG, with a `local_weighted_components` fallback for the degenerate "≤1 community" case. Determinism is testable.

---

## 8. Open Questions for the Next Phase

Things that the *catalog* could not answer from code alone, and that an architect or feature-owner should clarify:

1. **Why the 25-file pyright restart constant?** Empirical? Conservative bound on pyright memory growth? Either is fine; not knowing makes future tuning a guess.
2. **What is the post-1.0 plan for the four monolith files?** Each has a natural refactor split. Are these on the roadmap, or is the policy "no split until the file actively impedes a change"?
3. **Will `clarion-llm` become a crate?** The `llm_provider.rs` placement in `clarion-core` is the largest single argument against the lib doc-comment.
4. **What is the architect's stance on `application_id` / `user_version`?** Trivial to add; non-trivial to add *retroactively* once installed DBs exist in the wild.
5. **Operational tuning roadmap.** Eleven hardcoded limits, plus 25-file restart, plus 256/50 batch-cadence constants. WP6 is named in code comments — what is its current status?

---

## 9. Methodology and Confidence

**Validation status:** NEEDS_REVISION (warnings) → fixed → effectively APPROVED. One factual error (Python plugin restart constant value) corrected in `02-subsystem-catalog.md`. One cosmetic nit (subsystem 7 title) accepted. Eight spot-check claims all PASSED:

| Claim | Source verified | Status |
|---|---|---|
| stderr drain thread | `host.rs:609-620` | PASS |
| writer actor: 256-cap mpsc, 50-write batch | `writer.rs:35, 38, 813` | PASS |
| analyze ordering (secret scan → BeginRun → plugin spawn) | `analyze.rs:242, 244, 277+` | PASS |
| MCP tool count | `mcp/lib.rs:47-257` | 19 (discovery corrected) |
| HTTP read API: 4 routes | `http_read.rs:364-372` | PASS |
| plugin-fixture: 5 methods | `main.rs:51, 54, 68, 77, 116` | PASS |
| `clarion-plugin-python` binary name | `pyproject.toml:32-33` | PASS |
| Scanner: 12 named + 2 entropy | `patterns.rs:194-269`, `entropy.rs:11-18` | PASS |

**Overall confidence: High** for everything in §3, §4, §5, §6, §7. **Medium-High** for §2 LOC counts (some are approximate-from-discovery, validated to within 2%). **Medium** for §8 (recommendations) — these depend on architect intent the code cannot reveal.

**Coverage:** all 7 subsystems analyzed; every load-bearing module read at least partially; the four largest files sampled with explicit annotation of what was end-to-end-read vs. sampled (in each subsystem's Confidence statement). One file *not* sampled to completion is `clarion-mcp/src/lib.rs` (4,703 LOC) — its 19 tool registry was enumerated and the dispatcher structure was characterised, but each tool's individual handler body was not read end-to-end.

**What I would do next if continuing:**

- Quality-assessment pass on the four large files (`mcp/lib.rs`, `host.rs`, `analyze.rs`, `llm_provider.rs`) — they are the focal points for ROI on any refactor budget.
- Security-surface pass on the HTTP read API (HMAC implementation, body cap, panic-on-unenumerated-middleware-error stance).
- Test-pyramid analysis — given the 57% test/source ratio, where are the gaps? My read: storage and scanner are saturated; the host has happy-path-only integration coverage.

---

## 10. Pointers

- **Architecture diagrams:** see `03-diagrams.md` (7 Mermaid diagrams: 2 C4 levels, 2 component, 2 sequence, 1 dependency graph).
- **Per-subsystem detail:** see `02-subsystem-catalog.md`.
- **Discovery (entry points, stack):** see `01-discovery-findings.md`.
- **Validator report:** see `temp/validation-catalog.md`.

End of report.
