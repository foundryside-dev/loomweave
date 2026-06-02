## LLM Provider / Policy Engine Subsystem

**Location:** `crates/clarion-core/src/llm_provider.rs` (2,500 LOC); config layer in `crates/clarion-mcp/src/config.rs`; runtime budget/dispatch in `crates/clarion-mcp/src/lib.rs`.

**Responsibility:** Defines the `LlmProvider` trait and four concrete adapters (recording fixture, OpenRouter HTTP, Claude CLI subprocess, Codex CLI subprocess) for on-demand LLM calls; provides prompt-template builders for leaf-summary and inferred-edge tasks; measures and records token usage. Runtime policy enforcement (session budget ledger, summary-cache TTL, stale-semantic flag) lives in `clarion-mcp`. The analyze-time policy engine described in §5 (per-level modes, profile presets, dry-run estimate, `on_exceed` budget watcher) is not yet implemented — the only live LLM path is the consult-mode MCP flow. The module docstring names this scope explicitly: "LLM provider surface for WP6 and MCP on-demand tools" (`llm_provider.rs:1`).

---

### Key Components

- `llm_provider.rs:105-111` — `trait LlmProvider: Send + Sync` with five methods: `name`, `invoke`, `estimate_tokens`, `tier_to_model`, `caching_model`. Synchronous — callers in `clarion-mcp` wrap it in `tokio::task::spawn_blocking` (`lib.rs:3335`).
- `llm_provider.rs:146-198` — `RecordingProvider`: replay fixture for tests; stores `Vec<Recording>` keyed on exact `LlmRequest` equality; accumulated via `Mutex<Vec<LlmRequest>>` invocation log. Used by `clarion-mcp` integration tests.
- `llm_provider.rs:200-354` — `OpenRouterProvider`: live `reqwest::blocking::Client` transport to the OpenRouter chat-completions API. Enforces `allow_live_provider` gate and API-key presence at construction (`from_config:222-241`). Reads back `prompt_tokens_details.cached_tokens` (`llm_provider.rs:330-333`) to populate `cached_input_tokens` in the response but does NOT construct Anthropic `cache_control` breakpoints — the outbound payload is a flat single-user-message array (`llm_provider.rs:258-272`).
- `llm_provider.rs:379-576` — `CodexCliProvider`: subprocess transport. Spawns `codex exec --json --output-last-message <tmpfile> --output-schema <tmpfile>` with structured-output schema; reads JSONL stdout for usage; `cost_usd` is hardcoded `0.0` (`llm_provider.rs:544`) because Codex CLI reports no pricing signal.
- `llm_provider.rs:578-772` — `ClaudeCliProvider`: subprocess transport. Spawns `claude -p <SYSTEM_PROMPT> --output-format json --json-schema ... --permission-mode ... --max-turns ... --mcp-config {"mcpServers":{}} --strict-mcp-config --disable-slash-commands`. Always passes `--tools` explicitly (even empty) per ADR-013 (`lib.rs:2310`). Parses `structured_output`/`structuredOutput`/`result` event from `claude -p` JSON stream; accumulates usage across multi-turn event arrays.
- `llm_provider.rs:878-949` — Subprocess I/O helpers: `take_reader` (moves stdout/stderr pipe to background reader thread), `write_child_stdin`, `wait_for_child` (polling loop with kill-on-timeout), `join_reader`. The timeout is a polling loop sleeping 25 ms (`wait_for_child:919`), not a signaling mechanism.
- `llm_provider.rs:1357-1416` — `PromptTemplate`, `LeafSummaryPromptInput`, `InferredCallsPromptInput`, `build_leaf_summary_prompt`, `build_inferred_calls_prompt`: versioned plain-format builders. Template IDs `LEAF_SUMMARY_PROMPT_TEMPLATE_ID = "leaf-v1"` and `INFERRED_CALLS_PROMPT_VERSION = "inferred-calls-v1"` used as summary-cache key components.
- `llm_provider.rs:113-137` — `build_coding_agent_provider_prompt`: wraps any `LlmRequest` in a `<clarion_request>…</clarion_request>` envelope with versioned contract header `clarion-agent-provider-v1`, task-type label, and task-specific guidance. Used by both CLI providers.
- `clarion-mcp/src/config.rs:55-102` — `LlmConfig` / `LlmProviderKind` enum: TOML-deserialized config for `clarion serve`. Fields: `enabled`, `provider`, `allow_live_provider`, `session_token_ceiling` (default 1,000,000 tokens), `cache_max_age_days` (default 180), `max_inferred_edges_per_caller` (default 8), per-provider sub-structs. `LlmProviderKind::Anthropic` is a deprecated alias that returns `ConfigError::DeprecatedProvider` at construction (`config.rs:418`).
- `clarion-mcp/src/lib.rs:3250-3393` — `BudgetLedger` / `summary_budget_blocked`: session-scoped token reserve+spend ledger. `reserve_budget` increments `reserved_tokens` and returns a guard that on drop either commits spend or releases the reservation. `summary_budget_blocked` returns true when `spent + reserved >= session_token_ceiling` (`lib.rs:3268-3270`).
- `clarion-mcp/src/lib.rs:5827-5831` — `stale_semantic`: computes neighbourhood-drift flag at read time; true if stored `stale_semantic` flag is set OR if `caller_count`/`fan_out` have drifted more than the threshold since summary generation.

---

### Dependencies

**Inbound (callers of this subsystem):**
- `crates/clarion-mcp/src/lib.rs` — primary consumer. Calls `build_leaf_summary_prompt` + `provider.invoke` on the MCP `summary` tool path (`lib.rs:3110-3165`) and `build_inferred_calls_prompt` + `provider.invoke` on the `call_sites` inferred-dispatch path (`lib.rs:2898-2926`). Also uses `provider.estimate_tokens` and `provider.tier_to_model` for pre-flight budget reservation.
- `crates/clarion-cli/src/serve.rs` — constructs the `Arc<dyn LlmProvider>` from config at startup (`serve.rs:269-344`) and injects it into the MCP server state. No other LLM provider use in `clarion-cli`.
- `crates/clarion-cli/src/analyze.rs` — **ZERO usage.** The analyze pipeline does not invoke any `LlmProvider` method. All LLM work at analysis time is deferred.

**Outbound (what this subsystem depends on):**
- `reqwest` (blocking HTTP client, TLS/DNS stack) — used only in `OpenRouterProvider`; the only `reqwest` usage in all of `crates/clarion-core/src/` (`llm_provider.rs:273, 1344, 1346`).
- `tempfile` — named temp files for Codex CLI `--output-last-message` and `--output-schema` paths (`llm_provider.rs:969-981`).
- `which` — executable resolution in `validate_cli_executable` (`llm_provider.rs:360-366`).
- `serde` / `serde_json` — request/response serialization, structured-output schemas.
- `thiserror` — `LlmProviderError` enum.
- `tracing` — `warn!` on Codex JSONL parse failures (`llm_provider.rs:1039-1056`).
- `std::process::Command`, `std::thread`, `std::sync::Mutex` — subprocess I/O and recording provider locking.
- External services: OpenRouter HTTPS API (`https://openrouter.ai/api/v1` default); `claude` CLI binary (on PATH); `codex` CLI binary (on PATH or absolute path configured).

---

### Patterns Observed

- **Explicit live-provider opt-in gate**: `OpenRouterProvider::from_config` fails at construction with `LlmProviderError::LiveProviderNotAllowed` when `allow_live_provider == false` (`llm_provider.rs:223-225`). Same guard in `clarion-mcp/config.rs:423-424` at the `ProviderSelection` resolution layer. Two-layer defense: construction fails AND config selection short-circuits to `Disabled`.
- **Synchronous trait + async wrapper**: `LlmProvider::invoke` is synchronous (`llm_provider.rs:107`); `clarion-mcp` wraps it with `tokio::task::spawn_blocking` (`lib.rs:3335`) so blocking HTTP and subprocess calls don't stall the async MCP event loop.
- **Recording provider for test replay**: `RecordingProvider` matches on exact `LlmRequest` equality and surfaces `MissingRecording` on shape drift (`llm_provider.rs:172-184`). Invocation log behind `Mutex` allows post-hoc assertion of what was called (`llm_provider.rs:159-164`).
- **Polling subprocess timeout**: `wait_for_child` (`llm_provider.rs:912-935`) uses a `try_wait` poll loop with 25 ms sleep intervals rather than a signaling timeout. On expiry, kills the child and awaits reap before returning `LlmProviderError::Timeout`. No background reaper thread (unlike the plugin-host stderr drainer).
- **Versioned prompt contracts**: `AGENT_PROVIDER_PROMPT_VERSION = "clarion-agent-provider-v1"` and per-purpose `prompt_id` constants used as cache-key components. Version is embedded in the prompt text itself (`build_coding_agent_provider_prompt:115`) so mismatches are detectable from outputs.
- **Structured-output schema inline**: `response_format_for_purpose` and `codex_output_schema_for_purpose` (`llm_provider.rs:774-851`) define the `json_schema` / `strict: true` response format directly in code; same schema used for both OpenRouter (sent as `response_format`) and Codex/Claude CLI (written to temp file or passed as `--json-schema` flag).
- **Retryability classification on error variants**: every error variant carries or computes a `retryable: bool` (`llm_provider.rs:89-101`). HTTP 408/429/5xx are retryable (`retryable_status:1340-1342`); clean non-zero CLI exits are not (`cli_status_retryable:951-953`); signal-kill exits are retryable (`llm_provider.rs:2422-2433` test).
- **ADR-013 tool-posture enforcement via test**: `claude_cli_provider_passes_empty_tools_arg_when_no_tools_are_configured` (`llm_provider.rs:2238`, comment at `lib.rs:2310`) asserts `--tools` is always passed, even when empty, so the no-tools posture is never achieved by omission.
- **Session budget ledger** (`clarion-mcp/src/lib.rs:3350-3393`): reserve-then-commit pattern with RAII guard. On guard drop, either commits actual spend or releases the reservation. Ceiling comparison at `spent + reserved >= session_token_ceiling` prevents over-scheduling concurrent requests.
- **Summary-cache TTL + stale_semantic**: `summary_cache_expired` (`lib.rs:5843`) compares ISO-8601 day counts against `max_age_days`; `stale_semantic` (`lib.rs:5827`) re-computes neighbourhood drift flag on each read against stored `caller_count`/`fan_out`. Both consistent with ADR-007.

---

### DRIFT — Code vs §5 Design

**Section 5 of `docs/clarion/1.0/system-design.md` (lines 480-570) describes both an analyze-time policy engine and an MCP consult-mode provider layer. The analyze-time half is largely unbuilt. Specifically:**

1. **Provider name mismatch (hard drift).** §5 line 536: "v0.1 ships `AnthropicProvider` only." Code: `AnthropicProvider` is a *deprecated enum variant* that returns `ConfigError::DeprecatedProvider` at resolution time (`clarion-mcp/config.rs:418`). Three providers ship instead: `OpenRouterProvider`, `ClaudeCliProvider`, `CodexCliProvider`.

2. **`CachingModel` enum single-variant contradiction (hard drift).** §5 lines 540-549 describes a four-segment Anthropic `cache_control` breakpoint structure. `CachingModel` has exactly one variant: `CachingModel::OpenAiChatCompletions` (`llm_provider.rs:47-51`). All four providers return `CachingModel::OpenAiChatCompletions` (`llm_provider.rs:195, 351, 573, 769`). The OpenRouter payload is a flat single-user-message array — no `cache_control` breakpoints are constructed (`llm_provider.rs:258-272`). The code *reads back* `cached_tokens` from responses but does not construct the four-segment cache-control structure the doc describes.

3. **Trait signature drift (hard drift).** §5 line 527: `async fn invoke(&self, req: LlmRequest) -> LlmResponse; fn estimate_cost(&self, req: &LlmRequest) -> CostEstimate`. Code (`llm_provider.rs:105-111`): `invoke` is synchronous; there is no `estimate_cost` method returning a `CostEstimate` — the method is `estimate_tokens(&self, request: &LlmRequest) -> u64`.

4. **`tier_to_model` tier naming mismatch (hard drift).** §5 lines 510-514 names tiers `haiku | sonnet | opus`. Code: `tier_to_model` matches only `"summary"` and `"inferred_edges"` — both return `Some(self.model_id)` regardless (`llm_provider.rs:344-348`). Haiku/sonnet/opus tiers are not modelled.

5. **Prompt caching strategy unimplemented (architecture gap).** §5 lines 540-549 specifies four named cache-control segments. The OpenRouter provider sends a flat prompt to the API and reads back server-side cached token counts. The four-segment structure is absent from both the request construction and the `CachingModel` variant set.

6. **Analyze-time policy engine absent (architecture gap).** §5 lines 486-521 describes: three-tier config hierarchy (`~/.config/clarion/defaults.yaml` → `clarion.yaml` → CLI flags), per-level modes (`function|class|global|module|subsystem|cross_cutting`), profile presets (`budget|default|deep|custom`), dry-run estimate, `on_exceed: stop|warn`, `runs/<run_id>/partial.json`. None of these exist in the codebase. `clarion-cli/src/analyze.rs` has zero LLM provider invocations. The three-tier config hierarchy has not been verified absent (it may exist in `clarion-cli` config paths not read during this analysis — **information gap**).

7. **Observability gap (partial implementation).** §5 line 569 claims: "Per-run `stats.json` records … cache hit rate, phase durations. `cost_report(since)` MCP tool returns structured cost data. Budget events emit `CLA-INFRA-BUDGET-WARNING` / `CLA-INFRA-BUDGET-EXCEEDED` findings." Code: `session_token_ceiling` enforcement emits no findings (`lib.rs:3250-3280` — ceiling reached simply returns a budget-blocked flag; `CLA-INFRA-BUDGET-WARNING` / `CLA-INFRA-BUDGET-EXCEEDED` finding subcodes are absent from the entire workspace). No `cost_report` MCP tool exists — confirmed absent from the registered MCP tool surface (`summary_preview_cost` is the only cost-adjacent tool). Per-run stats are written by the analyze pipeline but do not include LLM cost per the absence of LLM invocations in `analyze.rs`.

8. **lib.rs scope claim vs reality (structural).** `clarion-core/src/lib.rs:1`: "clarion-core — domain types, identifiers, and provider traits." The crate pulls `reqwest` (TLS + DNS), `tempfile`, and `which` as direct workspace dependencies — all needed only by the concrete transports in `llm_provider.rs`. The crate that supervises plugin subprocesses has a live outbound HTTP transport as a first-class dependency. This is the placement concern discussed in clarion-141e9c08c8.

**What matches §5:**
- Summary-cache TTL via `max_age_days` (default 180) — code: `LlmConfig::default().cache_max_age_days = 180` (`config.rs:85`), `summary_cache_expired` (`lib.rs:5843`). Matches ADR-007.
- Neighbourhood-drift `stale_semantic` flag — code: `stale_semantic` (`lib.rs:5827-5831`) computes drift against stored `caller_count`/`fan_out`. Matches ADR-007.
- Explicit live-provider opt-in (CON-ANTHROPIC-01 intent honoured in spirit, different name).
- `RecordingProvider` for testability.

---

### Quality Concerns / Debt

**Medium — crate placement (clarion-141e9c08c8, `release:1.1`).**
`clarion-core` lists `reqwest`, `tempfile`, and `which` as direct workspace deps solely to serve `llm_provider.rs`. All three are unused by every other file in the crate (`crates/clarion-core/src/` grep confirms `reqwest` appears only at `llm_provider.rs:273, 1344, 1346`). The plugin supervisor and entity-ID assembler share a trust surface with a live TLS/DNS HTTP client. Extraction to `crates/clarion-llm/` (filigree issue clarion-141e9c08c8, `proposed/P2`) would: drop all three deps from `clarion-core`; honor `lib.rs:1`'s "domain types, identifiers, and provider traits" claim; harden the plugin-supervisor security surface by removing outbound HTTP capability. Cost: new workspace crate, repoint `clarion-cli` and `clarion-mcp` dependencies from `clarion-core` to `clarion-llm` (the issue recommends dropping the re-export alias, not adding one). All existing nextest tests pass per-file with the content unchanged. **Issue is P2/proposed; SME panel said ship 1.0 as-is, split before 1.1. The case has not weakened since the May analysis.**

**Medium — Codex CLI `cost_usd` hardcoded to 0.0 (`llm_provider.rs:544`).**
`LlmResponse.cost_usd` is unconditionally 0.0 for every Codex CLI invocation. Codex JSONL events carry no pricing data, so the 0.0 is accurate for the current API, but it means the session budget ledger's `spent_tokens` accounting (which does accumulate tokens for Codex calls via `commit_spend` in `lib.rs:3375`) diverges from any cost-based reporting. Any future `cost_report` MCP tool will show $0 for all Codex runs. Fix sketch: document the limitation prominently and/or expose an optional per-token pricing config for Codex (a rates-table in `LlmConfig`).

**Medium — Codex JSONL usage under-reporting (self-documented, `llm_provider.rs:1039-1056`).**
The code's own `warn!` comment acknowledges that malformed JSONL lines cause `session_token_ceiling` enforcement to "diverge from true accounting." A sufficiently chatty Codex session could consume more tokens than the ledger records. The fix (more robust JSONL parsing or an alternative API mode) depends on the Codex CLI API surface. Note as a known risk in operator documentation until the API provides a structured usage endpoint.

**Medium — 4-segment Anthropic prompt-caching strategy absent (`llm_provider.rs:47-51`, system-design.md §5 lines 540-549).**
`CachingModel` has exactly one variant and the OpenRouter transport sends flat single-turn messages. The planned order-of-magnitude cost savings on elspeth-scale module/subsystem synthesis (§5's "~1,100 Sonnet calls where Segment 1 caches globally") are not achievable until this is built. This is an architecture gap, not an implementation bug — it should become a tracked story for the WP6 implementation when the analyze-time pipeline gains LLM calls.

**Low-Medium — Polling subprocess timeout, no background stdout drain (`llm_provider.rs:912-935`).**
`wait_for_child` polls `try_wait` every 25 ms. On timeout it kills and awaits the child, then reads accumulated stdout/stderr via `join_reader`. If the child produces large output before the timeout, the background reader threads (`take_reader`) buffer it in-memory unbounded (`Vec::new() + read_to_end`). There is no ring-buffer cap equivalent to the plugin-host stderr drainer. A pathological LLM CLI that writes gigabytes before being killed could exhaust process memory. Low-Medium: real-world LLM CLIs rarely emit >a few MB, but the omission is architectural. Fix sketch: cap the `take_reader` accumulator at, say, 64 MiB.

**Low — No provider-contract uniformity test (`llm_provider.rs:1418-2499`).**
Tests cover each provider in isolation (fake-TCP server for OpenRouter, fake-bash scripts for Codex/Claude, exact-match for Recording) but no single test drives all four providers through one `LlmRequest` asserting shape, token-field presence, and timeout consistency. The per-provider test isolation means interface divergence (e.g., one provider returning `total_tokens = 0` when input+output=N) could go undetected. This gap is the third acceptance criterion in clarion-141e9c08c8; extraction would surface it by construction.

**Low — `estimate_text_tokens` character-div-4 approximation is not per-provider (`llm_provider.rs:1351-1355`).**
All three live providers use the same `chars / 4` heuristic as their `estimate_tokens` fallback. This is acknowledged as an estimate; the risk is that session-budget reservation over- or under-reserves depending on actual tokenization. No fix needed at current scale, but worth noting as an input to the C1 cost-model spike.

---

### Confidence

High for `llm_provider.rs` — read end-to-end (all 2,500 lines). Confirmed `analyze.rs` has zero LLM provider usage (grep). Confirmed `reqwest` is confined to `llm_provider.rs` within `clarion-core/src/`. Read `clarion-mcp/src/config.rs` (LlmConfig, LlmProviderKind, ProviderSelection) in full. Sampled `clarion-mcp/src/lib.rs` at all LLM-adjacent call sites (budget ledger, summary/inferred-edge invoke paths, stale_semantic, cache-expiry). Read §5 of system-design.md fully. Read ADR-007. Read filigree issue clarion-141e9c08c8 in full.

**Information gaps:** (1) Three-tier config hierarchy (`~/.config/clarion/defaults.yaml` → `clarion.yaml` → CLI flags) — not verified absent or present; `clarion-mcp/config.rs` shows a single TOML file model with no user-global merge logic visible, but `clarion-cli` config loading was not read. (2) Whether `CLA-INFRA-BUDGET-*` finding subcodes exist anywhere in the workspace (only grepped `clarion-mcp/src/lib.rs`). (3) `detailed-design.md:1745` reference to "clarion-llm intended post-1.0 boundary" — not verified in the current file (detailed-design.md not read during this pass).
