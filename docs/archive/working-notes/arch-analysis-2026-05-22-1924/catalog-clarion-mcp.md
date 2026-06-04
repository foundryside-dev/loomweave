## 4. clarion-mcp

**Location:** `crates/clarion-mcp/`
**LOC:** 6,595 source (`lib.rs` 4,703 + `config.rs` 1,600 + `filigree.rs` 292) / 2,233 test (`tests/storage_tools.rs`)
**Crate type / role:** Library crate. Implements the MCP (Model Context Protocol) JSON-RPC tool surface that consult-mode LLM agents call to query Clarion's index; not a binary — the `clarion serve` binary in `clarion-cli` consumes this library and drives the stdio loop.

### Responsibility

`clarion-mcp` owns the JSON-RPC tool dispatch layer and the stdio transport that fronts Clarion's index for external agents. It defines the twenty MCP tools (`list_tools` at `src/lib.rs:56`), translates MCP `tools/call` requests into bounded `clarion-storage` reader queries, mediates LLM-dispatch for on-demand summaries and inferred call edges with token-budget reservation (`BudgetLedger` at `lib.rs:2244`, `reserve_budget` at `lib.rs:2066`), and brokers Filigree enrichment lookups over HTTP. It also owns the YAML configuration schema (`McpConfig` at `config.rs:9`) consumed by `clarion serve` for LLM provider selection, Filigree integration, and the (separate) HTTP read-API bind/auth posture. The crate exposes its surface via `ServerState`, `serve_stdio*`, `handle_json_rpc`, and `list_tools`, and is consumed exclusively by `clarion-cli` (see Inbound below).

### Key components

- `src/lib.rs:56-258` — `list_tools()`: the canonical 20-tool registry with names, descriptions, and JSON schemas. Single source of truth; `handle_tool_call` validates names against it (`lib.rs:401`).
- `src/lib.rs:316-2210` — `ServerState`: the stateful dispatcher. Holds `ReaderPool`, optional `SummaryLlmState` (writer mpsc + provider), optional `FiligreeLookup`, an `AnalyzeProcess` slot, a clock, an `InferredInflight` coalescer (`lib.rs:43`), and a `BudgetLedger`. Per-tool handlers (`tool_entity_at` at 493 … `tool_subsystem_members` at 1229) sit on `ServerState`.
- `src/lib.rs:2704-2876` — Transport: `handle_frame*`, `read_stdio_frame`, `serve_stdio`, `serve_stdio_with_state`, `serve_stdio_with_state_on_runtime`. Implements dual framing (LSP-style `Content-Length` and JSON-line) auto-detected from the first non-whitespace byte (`peek_stdio_frame_start` at 2774).
- `src/lib.rs:1560-2030` — Inferred-edge dispatch pipeline: cache lookup (`read_inferred_inputs` 1624), in-flight coalescing via `tokio::sync::broadcast` (`InferredInflightGuard` 2438; `coalesced_inferred_dispatch`), budget reservation, LLM provider invocation via `spawn_blocking` (`invoke_llm` 2218), and writer-channel persistence (`WriterCmd::InsertInferredEdges` at 1682/1824).
- `src/lib.rs:2237-2290` — `AnalyzeProcess` + `BudgetLedger` + `BudgetReservation`: spawns `clarion analyze` as a detached child for `analyze_start`/`analyze_status`/`analyze_cancel`; tracks reserved-vs-spent LLM tokens with a `Mutex`-guarded ledger.
- `src/config.rs:9-742` — `McpConfig` (YAML, `serde_norway`): `LlmConfig` (provider kind, model id, session token ceiling, per-caller inferred-edge cap, cache TTL), provider-specific subsections (`OpenRouterConfig`, `CodexCliConfig`, `ClaudeCliConfig`), `FiligreeConfig` (base URL, project key, token env, actor, timeout), `HttpReadConfig` (bind, loopback trust, bearer + identity HMAC env vars), and `select_provider_with_env` (677) which builds an `LlmProvider` honoring `recording_fixture_path` and `allow_live_provider`.
- `src/filigree.rs:1-292` — Filigree HTTP client: `FiligreeLookup` trait (52), blocking-reqwest implementation `FiligreeHttpClient` (64), URL builder `entity_associations_url` (148), and response shape `EntityAssociationsResponse` (11).

### Public interface (outbound)

- **20 MCP tools** (the consult-agent contract, from `list_tools`):
  1. `entity_at` — innermost entity containing `(file, line)`; path-normalized against project root.
  2. `project_status` — orientation snapshot: index age, latest run, graph counts, git state, plugin discovery, LLM policy, Filigree routing, analyze lifecycle.
  3. `analyze_start` — spawn background `clarion analyze` child; supports `allow_no_plugins`.
  4. `analyze_status` — child process state + latest persisted `runs` row.
  5. `analyze_cancel` — kill the background analyze child if running.
  6. `find_entity` — paginated FTS-ranked search over entity id/name/short-name/summary; does *not* search `summary_cache`.
  7. `source_for_entity` — line-numbered source span with bounded context; includes decorator lines.
  8. `entity_context` — resolve by id or `(file,line)`; returns containing stack + source + diagnostics.
  9. `call_sites` — caller/callee evidence with source snippets at recorded byte offsets.
  10. `callers_of` — callers, default confidence `resolved`; opt-in to `ambiguous` (expand candidates) or `inferred` (may trigger LLM dispatch).
  11. `execution_paths_from` — bounded calls-only traversal; default `max_depth=3`, hard `execution_edge_cap` (default 500, settable via `with_edge_cap`).
  12. `execution_paths_ranked` — compact ranked path view; optional `exclude_tests`, `max_paths`.
  13. `summary` — on-demand cached *leaf* summary; module entities return scope-deferred policy envelope, not aggregation.
  14. `summary_preview_cost` — cache status, model/provider, token estimate, live-spend requirement; no dispatch.
  15. `issues_for` — Filigree associations for an entity, optionally `include_contained`; returns `unavailable` envelope if Filigree disabled rather than failing.
  16. `orientation_pack` — deterministic first-pass packet (status, source, callers, callees, issues, next reads); no LLM.
  17. `index_diff` — freshness signals: latest run, DB mtime, source files newer than index, changed entity hashes.
  18. `neighborhood` — one-hop graph (callers, callees, container, contained, references).
  19. `subsystem_members` — module entities for a subsystem entity.
  20. (twentieth slot) `summary_preview_cost` and `summary` count separately; full enumeration above totals 19 visible — the registry actually contains 19 entries, not 20. **Note:** the discovery doc cites "twenty tools"; the registry as of `lib.rs:56-257` contains 19 distinct `ToolDefinition` entries. Flagging in Concerns.
- **Rust API:**
  - `pub fn list_tools() -> Vec<ToolDefinition>` (`lib.rs:56`).
  - `pub fn handle_json_rpc(&Value) -> Option<Value>` (`lib.rs:295`) — state-free metadata-only path (`initialize`, `tools/list`).
  - `pub struct ServerState` with builder methods `new`, `with_edge_cap`, `with_summary_llm`, `with_clock`, `with_filigree_client` (`lib.rs:316-376`); `ServerState::handle_json_rpc(&Value) -> Option<Value>` (377).
  - `pub fn serve_stdio`, `serve_stdio_with_state`, `serve_stdio_with_state_on_runtime` (`lib.rs:2831-2876`).
  - `pub fn handle_frame`, `handle_frame_with_state` (`lib.rs:2704-2721`) — frame-level entry points for callers that own their own loop.
  - `pub enum McpError` (`lib.rs:2690`), `pub const MCP_PROTOCOL_VERSION = "2025-11-25"` (`lib.rs:40`).
- **Config API (`config` module):** `McpConfig::from_path`, `McpConfig::from_yaml_str`, `select_provider_with_env`, `resolve_filigree_http_target`, `resolve_filigree_base_url`, `HttpReadConfig::{validate_loopback_trust, validate_auth_trust, is_loopback_bind}`.
- **Filigree API (`filigree` module):** `FiligreeLookup` trait, `FiligreeHttpClient::from_config`, `entity_associations_url`, `parse_entity_associations_response`.

### Dependencies

- **Inbound (who calls this):**
  - `crates/clarion-cli/src/serve.rs:13-150` — sole production caller. Imports `config::{McpConfig, LlmConfig, select_provider_with_env, ...}`, `filigree::FiligreeHttpClient`, `ServerState::new`, `with_summary_llm`, `with_filigree_client`, `serve_stdio_with_state_on_runtime`. Spawns the stdio loop on a named thread `clarion-mcp-stdio`.
  - `crates/clarion-cli/src/http_read.rs:18` — reuses `clarion_mcp::config::HttpReadConfig` to drive the *separate* HTTP read-API server (which is implemented in `clarion-cli`, not here).
  - `crates/clarion-cli/src/install.rs:68` — references the literal string `"clarion-mcp"` in template output (the default Filigree actor).
- **Outbound (what this calls):**
  - `clarion-core` (`plugin::{Frame, TransportError, ContentLengthCeiling, read_frame, write_frame}`; LLM primitives `LlmProvider`, `LlmRequest`, `LlmResponse`, `LlmProviderError`, `LlmPurpose`, `build_inferred_calls_prompt`, `build_leaf_summary_prompt`, `INFERRED_CALLS_PROMPT_VERSION`, `LEAF_SUMMARY_PROMPT_TEMPLATE_ID`, `InferredCallsPromptInput`, `LeafSummaryPromptInput`, `EdgeConfidence`).
  - `clarion-storage` — read side via `ReaderPool::with_reader` (~30 call sites including `entity_at_line`, `entity_by_id`, `find_entities`, `call_edges_from`, `call_edges_targeting`, `reference_edges_for_entity`, `child_entity_ids`, `contained_entity_ids`, `existing_entity_ids`, `subsystem_members`, `summary_cache_lookup`, `inferred_edge_cache_lookup`, `inferred_edge_cache_key_id`, `unresolved_call_sites_for_caller`, `unresolved_callers_for_target`, `candidate_entities_for_unresolved_sites`, `normalize_source_path`); write side strictly via `WriterCmd` over `mpsc::Sender<WriterCmd>` — only three variants emitted: `InsertInferredEdges` (twice, `lib.rs:1682`/`1824`), `TouchSummaryCache` (`lib.rs:1924`), `UpsertSummaryCache` (`lib.rs:2024`).
  - External crates: `tokio` (current-thread runtime, `AsyncMutex`, `mpsc`, `oneshot`, `broadcast`, `spawn_blocking`), `reqwest::blocking` (Filigree), `rusqlite` (re-exported types only — no direct `Connection::open`; all DB access flows through `ReaderPool`/`WriterCmd`), `serde_norway` (YAML config), `time`, `blake3`, `thiserror`, `tracing`.
- **External services:**
  - **SQLite** via `clarion-storage::ReaderPool` — read concurrency; writes only via the writer-actor channel.
  - **Subprocess: `clarion analyze`** — spawned by `tool_analyze_start` using `std::env::current_exe()` (`lib.rs:572-599`), stdin/stdout/stderr null'd.
  - **Filigree HTTP** — blocking `reqwest` GET to `{base_url}/api[/p/{project_key}]/entity-associations?entity_id=...` with `x-filigree-actor` and bearer auth (`filigree.rs:98-127`).
  - **LLM providers** — pluggable `Arc<dyn LlmProvider>` invoked via `tokio::task::spawn_blocking` (`lib.rs:2218`); config chooses OpenRouter (HTTP), Codex CLI, Claude CLI, or a deterministic Recording fixture (`config.rs:677`).

### Internal architecture

The crate is a single stateful dispatcher (`ServerState`) plus a thin stdio I/O loop. `ServerState::handle_json_rpc` (`lib.rs:377`) routes `initialize` and `tools/list` to static helpers, and `tools/call` to `handle_tool_call` (394) which dispatches on tool name via a 19-arm `match` (`lib.rs:413-488`). Each `tool_*` handler is `async`, takes the argument map, parses required/optional params (`required_str`, `optional_bool`, …), and either returns a JSON envelope or a `ParamError` that becomes a JSON-RPC error response. Storage reads flow through `self.readers.with_reader(|conn| …)`, which offloads onto the reader pool's blocking workers; the handler awaits the result and wraps it in `envelope_from_storage_result`.

**Concurrency model.** The crate is built around a borrowed Tokio current-thread runtime (`serve_stdio_with_state_on_runtime` accepts `&Runtime` from the caller; `serve_stdio_with_state` builds its own). The dispatch path is fully async, but the stdio I/O is synchronous: the loop does a blocking `read_stdio_frame` on `&mut impl std::io::BufRead`, then `runtime.block_on(handle_stdio_frame_with_state(...))`, then a blocking `write_stdio_response`. There is no parallel request handling — frames are processed strictly sequentially per stdio session. Three internal concurrency primitives matter:
  1. `BudgetLedger` behind a `std::sync::Mutex` (`lib.rs:322`) — gates LLM dispatch with a reserve/commit pattern and a `blocked` latch.
  2. `InferredInflight` — `Arc<AsyncMutex<HashMap<InferredEdgeCacheKey, broadcast::Sender<...>>>>` (`lib.rs:43`) — coalesces concurrent `callers_of`/inferred-edge requests for the same `(caller_id, content_hash, model_id, prompt_version)` tuple so only one LLM call fires; followers subscribe via `broadcast` and `RAII`-clean up via `InferredInflightGuard` (`lib.rs:2438`).
  3. `analyze_process` — a `std::sync::Mutex<Option<AnalyzeProcess>>` slot for the at-most-one in-flight `clarion analyze` child.

**State ownership.** `ServerState` owns the `ReaderPool`, the optional writer `mpsc::Sender<WriterCmd>` (cloned out of `SummaryLlmState` per dispatch), the optional Filigree client behind `Arc<dyn FiligreeLookup>`, the `clock` (injectable for tests), and the analyze-process slot. The writer-actor itself lives in `clarion-cli/src/serve.rs:135-144`, not here — `clarion-mcp` is a writer *client*, never a writer host.

**Transport.** The stdio frame reader auto-detects between LSP-style `Content-Length` framing (delegated to `clarion_core::plugin::read_frame`/`write_frame`) and bare-JSON-line framing by peeking the first non-whitespace byte (`lib.rs:2757-2789`). The choice is per-frame, recorded in `StdioFrame::framing`, and reused on the response. JSON-RPC notifications (method present, id absent) are silently dropped (`is_json_rpc_notification` at `lib.rs:2740`). Test `serve_stdio_handles_multiple_content_length_frames` (`lib.rs:4309`) and `serve_stdio_with_state_uses_json_line_transport_for_json_line_requests` (4400) pin the dual-framing contract.

**Error model.** Two layers: `McpError` (`lib.rs:2690`) — `Json | Transport | Runtime` — surfaces only from frame I/O and JSON decode failures and aborts the loop; per-tool errors stay inside the JSON-RPC response as either error responses (`ParamError`) or success envelopes with `tool_error_envelope(code, message, retryable)` (e.g. `analyze-already-running`, `analyze-start-failed`, `token-ceiling-exceeded`, `llm-disabled`, `invalid-path`). The retryability flag is on the wire, not just in logs. `ConfigError` (`config.rs:742`) uses stable string error codes (`CLA-CONFIG-*`) so the CLI can route them.

### Patterns observed

- **Tool registry as single source of truth** — `list_tools()` validates incoming names at dispatch (`lib.rs:401`) and is also served verbatim to `tools/list`; adding a tool requires both a registry entry and a `match` arm, but the unreachable arm (`lib.rs:487`) is documented by the prior validation.
- **Reader pool + writer-actor split** — every DB read goes through `ReaderPool::with_reader` (~30 sites); every DB write goes through `mpsc::Sender<WriterCmd>` with an `oneshot` ack (`send_writer` helper at `lib.rs:2043`). The crate never opens a `rusqlite::Connection` directly.
- **Coalescing + RAII guards for expensive async work** — `InferredInflightGuard` and `BudgetReservation` both use `Drop` to release in-flight slots / reserved tokens even if the awaiting future is cancelled (`lib.rs:2278-2290`, `2471-2486`). The `Drop` paths for guards spanning runtime boundaries use `tokio::runtime::Handle::try_current` to schedule async cleanup.
- **Dual-framing transport with byte-peek detection** — same `serve_stdio` loop handles LSP-style and JSON-line clients without configuration (`lib.rs:2757`).
- **Capability gating via `Option<…>` builder fields** — LLM features (`summary_llm`) and Filigree enrichment (`filigree_client`) are off unless `with_summary_llm` / `with_filigree_client` is called. Tool handlers check for `None` and return policy envelopes (`llm-disabled`, Filigree `unavailable` envelope) rather than failing.
- **Stable error-code strings on the wire** — both tool envelopes (`token-ceiling-exceeded`, `analyze-already-running`, `invalid-path`) and config errors (`CLA-CONFIG-HTTP-NO-AUTH`, `CLA-CONFIG-FILIGREE-PORT-CONFLICT`, etc.) use prefix-namespaced identifiers callers can switch on.
- **Recording-provider determinism hook** — `LlmConfig.recording_fixture_path` (`config.rs:78`) + `select_provider_with_env` (677) let tests pin LLM responses without network calls; the test suite at `tests/storage_tools.rs` uses `RecordingProvider` heavily.

### Concerns / Smells / Risks

- **`lib.rs` is 4,703 lines in a single file.** This is the dominant smell. It contains the tool registry, all 19 tool handlers, transport, framing, budget ledger, inflight coalescer, analyze-process management, error/envelope helpers, and ~700 lines of unit tests (`mod tests` at `lib.rs:4309`+). A natural split is at least three files: `tools/` (one module per tool family), `transport.rs` (stdio framing), `dispatch.rs` (inferred-edge + summary LLM machinery). Risk: merge conflicts, slow IDE feedback, hard to review.
- **Discovery doc says "twenty tools"; registry contains 19.** Enumerated above. Either the doc is stale or a tool was removed without updating discovery; worth confirming against the design intent. Flagging because the prompt called this out as a precise characterization target.
- **Dispatch is purely sequential.** `serve_stdio_with_state_on_runtime` does `runtime.block_on(...)` inside the loop and a blocking read between frames. A slow LLM-dispatching `summary` or `callers_of` request blocks all subsequent tool calls on the same stdio session. The reader pool is internally concurrent but it doesn't help here. For consult-agent UX this is probably fine; for multi-agent or long-LLM-call scenarios it is a wall.
- **`reqwest::blocking` inside an async dispatcher.** `FiligreeHttpClient::associations_for` (`filigree.rs:98`) is synchronous and is called from `tool_issues_for` (`lib.rs:1154`) which is `async`. The call appears to happen on whatever thread the `block_on` is running, blocking the current-thread runtime for the configured timeout (default 5s). Should be `spawn_blocking`-wrapped or moved to `reqwest`'s async client.
- **Writer-channel coupling.** All writer commands flow through the `summary_llm.writer` field — which means inferred-edge writes are gated on the *summary* LLM being configured (`inference_llm_snapshot` at `lib.rs:2093` clones from `summary_llm`). Two LLM features share one writer handle and one config slot. Functional today (one writer per process), but the naming overloads "summary" to mean "any LLM-related writes."
- **Mutex poisoning swallowed everywhere.** Every `self.analyze_process.lock()`, `self.budget.lock()`, etc. uses `.unwrap_or_else(std::sync::PoisonError::into_inner)` (e.g. `lib.rs:556`, `2061`, `2284`). This silently masks the panic that caused the poisoning. Acceptable as a deliberate "keep serving" choice but worth a comment.
- **`config.rs` is 1,600 lines** with the YAML schema, three provider config blocks, two HTTP trust-validators, provider selection, and a sizable error enum. Splitting per provider would help.
- **Test coverage is heavily integration-tested in `tests/storage_tools.rs`** (2,233 LOC, ~35 `#[tokio::test]`s observed) but the in-`lib.rs` unit tests (`lib.rs:4309+`) are narrow — mostly transport framing. The inferred-edge coalescing logic is intricate and would benefit from focused unit tests independent of full storage seeding.
- **No timeout on `tool_analyze_start` child.** The spawned `clarion analyze` runs unbounded; `analyze_cancel` is the only stop. If the agent forgets to poll, the child can outlive the session.
- **Filigree client is held behind `Arc<dyn FiligreeLookup>` but `FiligreeHttpClient` itself wraps `reqwest::blocking::Client` (already `Clone`).** The `dyn` indirection is fine for test substitution; just noting that the trait has exactly one production impl plus test fakes.

### Confidence: High

Read `lib.rs` lines 1-260 and 250-600 end-to-end, plus targeted reads of the LLM/budget/coalescing region (1600-1690, 2040-2310), the transport region (2680-2876), and supporting helpers. Read `filigree.rs` end-to-end (292 lines), `config.rs` lines 1-410 plus targeted reads. Confirmed the inbound caller surface by grepping all `clarion_mcp` / `clarion-mcp` references in `crates/` (one consumer: `clarion-cli`, three call sites in `serve.rs` / `http_read.rs` / `install.rs`). Confirmed writer-channel use by enumerating all `WriterCmd::` and `send_writer` sites (4 emission points, 3 distinct variants). Tool count verified by reading the registry block 56-257 directly — registry contains 19 entries, flagged against the discovery doc's claim of 20.
