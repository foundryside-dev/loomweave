## clarion-core

**Location:** `crates/clarion-core/`

**Responsibility:** Owns the canonical entity-ID grammar, plugin-host subprocess supervisor + JSON-RPC peer, LLM-provider trait + OpenRouter implementation, plugin-manifest parser, and the jail / RSS / entity / breaker ceilings that every other crate depends on for safety.

**Public interface:**

`lib.rs` is 47 lines (see `crates/clarion-core/src/lib.rs:1-47`) and is explicit that it is a curated facade per ticket `clarion-29acbcd042`. Re-exports at the crate root, grouped by submodule:

- **From `entity_id`**: `EntityId`, `EntityIdError`, free function `entity_id(plugin, kind, qualname)` — the cross-language assembler.
- **From `llm_provider`**: trait `LlmProvider`; `OpenRouterProvider` + `OpenRouterProviderConfig`; `RecordingProvider` + `Recording` (test fixture replay); error type `LlmProviderError`; request/response DTOs `LlmRequest` / `LlmResponse` / `LlmPurpose`; `CachingModel`; prompt builders `build_leaf_summary_prompt`, `build_inferred_calls_prompt`, with stable IDs `LEAF_SUMMARY_PROMPT_TEMPLATE_ID = "leaf-v1"` and `INFERRED_CALLS_PROMPT_VERSION = "inferred-calls-v1"`.
- **From `plugin`**: `PluginHost`, `Manifest` + `parse_manifest`, `AcceptedEntity` / `AcceptedEdge` / `AnalyzeFileOutcome` / `AnalyzeFileStats`, `EdgeConfidence`, `UnresolvedCallSite`, `HostError` + `HostFinding`, `JailError`, `CapExceeded`, `DiscoveredPlugin` + `DiscoveryError` + `discover()`, `CrashLoopBreaker` + `CrashLoopState`, and the finding-ID constants (`FINDING_DISABLED_CRASH_LOOP` re-exported at root, with sibling `FINDING_*` constants reachable through `clarion_core::plugin::*`).

Implementation types deliberately *not* re-exported at the root but still reachable via fully-qualified paths: transport (`Frame`, `read_frame`, `write_frame`, `TransportError`), protocol envelopes (`RequestEnvelope`, `ResponseEnvelope`, `AnalyzeFileParams`, etc.), prlimit helpers, raw entity/edge structs. `make_notification` / `make_request` are `pub(crate)` — `mod.rs:38-42` explains they panic on serde failure and external callers should build envelopes directly.

**Internal structure:**

Top-level modules (`crates/clarion-core/src/`):

- `entity_id.rs` (610 LOC) — `EntityId` newtype + `entity_id()` constructor + `EntityIdError`. Includes the cross-language parity test against `fixtures/entity_id.json` (ADR-003, L2 lock-in).
- `llm_provider.rs` (948 LOC) — single-file LLM abstraction. See breakdown below.
- `plugin/` — supervisor + protocol + safety primitives. Submodule roles documented inline at `plugin/mod.rs:1-12`:
  - `manifest.rs` (1508 LOC) — `Manifest`, `PluginMeta`, `Capabilities`, `CapabilitiesRuntime`, `PyrightRuntime`, `Ontology`, `parse_manifest()`, kind-grammar + rule-ID-prefix validators (ADR-021/ADR-022, L5).
  - `protocol.rs` (846 LOC) — JSON-RPC 2.0 typed envelopes; `AnalyzeFileParams/Result`, `AnalyzeFileStats`, `UnresolvedCallSite`, `EdgeConfidence`, `InitializeParams/Result`, `Shutdown`, `Exit`, `ProtocolError`.
  - `transport.rs` (568 LOC) — `Frame`, `read_frame`/`write_frame`, Content-Length framing with `MAX_HEADER_LINE_BYTES = 8 KiB`; surfaces oversize via `TransportError`.
  - `jail.rs` (253 LOC) — `jail()` / `jail_to_string()` canonicalises and asserts containment under project root; `JailError::{EscapedRoot, NonUtf8Path, Io}`.
  - `limits.rs` (552 LOC) — `ContentLengthCeiling`, `EntityCountCap`, `PathEscapeBreaker`, `BreakerState`, the finding-ID constants for cap violations, and the Unix `apply_prlimit_as` / `apply_prlimit_nofile_nproc` (no-op stubs on non-Unix). `DEFAULT_MAX_RSS_MIB = 2048` (ADR-021 §2b ceiling).
  - `breaker.rs` (360 LOC) — `CrashLoopBreaker`, `CrashLoopState`, `FINDING_DISABLED_CRASH_LOOP` (ADR-002 §UQ-WP2-10).
  - `discovery.rs` (637 LOC) — `DiscoveredPlugin`, `DiscoveryError`, `discover()` (Linux only — non-Unix returns empty), `discover_on_path()`, scans `$PATH` for `clarion-plugin-*` executables and pairs them with a sibling `share/clarion/plugins/<name>/plugin.toml` (cap 64 KiB).
  - `host.rs` (3126 LOC) — see below.
  - `mock.rs` (897 LOC, `#[cfg(test)] pub(crate)`) — in-process mock plugin; reachable only from unit tests in this crate.

### Internal organisation of `plugin/host.rs` (3126 LOC)

Despite the line count, the file is **not a god-file** — it is structurally one struct (`PluginHost<R, W>`) with two layers of constructors, an analyse-file pipeline, and a long unit-test suite. Concretely:

- **Lines 1–155: Module-level constants and finding-ID strings** (`FINDING_*` for entity/edge ontology violations, plus byte caps `MAX_ENTITY_FIELD_BYTES = 4 KiB`, `MAX_ENTITY_EXTRA_BYTES = 64 KiB`, `MAX_UNRESOLVED_CALLEE_EXPR_BYTES = 512`).
- **Lines 157–345: Pure validators / decoders** (`effective_max_nproc`, `oversize_edge_field`, `oversize_field`, `invalid_unresolved_call_site_reason`).
- **Lines 346–438: Public DTOs** — `RawEntity`, `RawEdge`, `RawSource`, `AcceptedEntity`, `AcceptedEdge`, `AnalyzeFileOutcome`, `HostError` (with 17 variants covering Spawn, Handshake, Protocol, EntityCapExceeded, PathEscapeBreakerTripped, OomKilled, Io, etc.).
- **Lines 450–637: `HostFinding` + ~14 named constructors** (`undeclared_kind`, `entity_id_mismatch`, `path_escape`, `disabled_path_escape`, `entity_cap_exceeded_finding`, `unsupported_capability`, `non_utf8_path`, `malformed_entity`, `malformed_edge`, `undeclared_edge_kind`, `edge_field_oversize`, `malformed_unresolved_call_site`, `entity_field_oversize`, `oom_killed`). One constructor per finding type — the "constructor wall" inflates line count but is mechanical.
- **Lines 639–710: `PluginHost<R, W>` struct definition + `drain_stderr_into_ring`** — generic over reader/writer so subprocess and in-process mock share one impl; `stderr_tail` is a 64 KiB ring buffer.
- **Lines 712–882: Subprocess constructor `PluginHost::spawn`** specialised to `BufReader<ChildStdout>` / `BufWriter<ChildStdin>`. This is the heaviest single function — it (1) validates manifest `executable` is a bare basename matching the discovered binary path (anti-redirect defence at host:756–780), (2) applies `RLIMIT_AS` + `nofile` + `nproc` via `pre_exec`, (3) spawns a detached stderr-drain thread, (4) reaps the child on handshake failure (host:864–880 — `Child::Drop` does *not* reap, so failure paths must explicitly wait).
- **Lines 883–957: `PluginHost::connect` + `new_inner` + accessors** (`stderr_tail`, `ontology_version`).
- **Lines 959–1028: `handshake()`** — the four documented steps: send `initialize`, validate response id, run `manifest.validate_for_v0_1()`, send `initialized`. On manifest failure the host is best-effort-shut-down without sending `initialized`.
- **Lines 1031–1198: `analyze_file()`** — the per-file orchestration pipeline. Reads as a numbered validator pipeline:
  - `0.` field-size cap → oversize finding, drop (host:1103);
  - `1.` ontology declared-kind check (host:1109);
  - `2.` entity-id identity check (host:1116);
  - `3.` path-jail check + path-escape breaker tick → kill-path on `Tripped` (host:1135);
  - `4.` entity-cap check → kill-path on exceed (host:1166).
- **Lines 1200–1299: `process_edges()`** — same drop-on-violation posture, no kill paths (edges don't participate in breakers).
- **Lines 1300–1450: `process_stats`, `shutdown`, `take_findings`, `next_id`, `do_shutdown`, `read_response_matching`** (drain-until-match for stale frames at host:1398; idempotent shutdown via `terminated` flag at host:657).
- **Lines 1452–end (≈1700 LOC of tests)**: 30+ `#[cfg(test)]` named scenarios `t2_…` through `t9_…` plus B.3/B.4*/B.5* regressions. Tests dominate the line count; *production code in `host.rs` is ~1450 LOC*.

The file is **coherent**: one supervisor type with one validator pipeline. The internal pressure to refactor is mild — splitting findings into a sibling module would shave ~200 LOC, but the drop-on-violation pipeline reads top-to-bottom and benefits from staying co-located.

### Internal organisation of `llm_provider.rs` (948 LOC)

Single-file LLM abstraction, organised top-to-bottom (no submodule because there is no `llm_provider/` directory):

- **Lines 10–11: Stable prompt-ID constants** (`LEAF_SUMMARY_PROMPT_TEMPLATE_ID = "leaf-v1"`, `INFERRED_CALLS_PROMPT_VERSION = "inferred-calls-v1"`). These form two of the five components of the ADR-007 cache key; the other three (`entity_id`, `content_hash`, `model_tier`, `guidance_fingerprint`) are assembled in `clarion-storage::cache::SummaryCacheKey`. **The 5-tuple is *not* materialised inside this file** — the provider is dispatch-only; cache keying happens in `clarion-storage` and `clarion-mcp`.
- **Lines 13–37: Request / response / purpose DTOs** (`LlmRequest`, `LlmResponse`, `LlmPurpose::{Summary, InferredEdges}`).
- **Lines 38–82: Error taxonomy** — `LlmProviderError` with six variants; `retryable()` returns the inner retryable flag for `Http`/`Provider`/`InvalidResponse`, false for `MissingRecording`/`LiveProviderNotAllowed`/`MissingApiKey`.
- **Lines 84–90: The `LlmProvider` trait** — five required methods: `name`, `invoke`, `estimate_tokens`, `tier_to_model`, `caching_model`. Trait is `Send + Sync` so providers can be shared across MCP request handlers behind an `Arc`.
- **Lines 92–151: `RecordingProvider`** — test-mode provider that replays exact-shape `LlmRequest` matches from a fixed `Vec<Recording>`. Tracks invocations in a `Mutex<Vec<LlmRequest>>` for assertion. Returns `MissingRecording` (non-retryable) on shape drift.
- **Lines 153–295: `OpenRouterProvider`** — the live provider.
  - Config gate at `from_config` (lines 173–187): requires both `allow_live_provider == true` AND a non-empty `api_key`; either failure yields `LiveProviderNotAllowed` / `MissingApiKey` (both non-retryable).
  - `invoke()` (lines 202–279): blocking `reqwest` POST to `{endpoint}/chat/completions`, 60s timeout, `Authorization: Bearer`, `HTTP-Referer`, `X-OpenRouter-Title` headers. Payload includes `"temperature": 0`, `"provider": {"require_parameters": true}`, and `response_format` from `response_format_for_purpose()`.
  - **The strict-JSON path** (the open question from §8) lives at `response_format_for_purpose` (lines 297–370): two JSON-Schema objects, both with `"strict": true` and `"additionalProperties": false`. `Summary` enforces `{purpose, behavior, relationships, risks}`; `InferredEdges` enforces `edges: [{site_key, target_id, confidence, rationale}]`. This is the B.8 GREEN-rerun fix from commit `ab6b1dd` — without `"strict": true` OpenRouter providers silently produced free-form text.
  - Error unwrap chain: the response body is first parsed as `OpenRouterErrorEnvelope` (line 250) — even on HTTP 200, choices can carry per-call provider errors at `OpenRouterChatResponse::output_text` (line 379), which surfaces `LlmProviderError::Provider`.
- **Lines 297–500: HTTP plumbing** — response struct definitions (`OpenRouterChatResponse`, `OpenRouterChoice`, `OpenRouterMessage`, `OpenRouterUsage`, `OpenRouterErrorEnvelope`, `OpenRouterErrorBody`), `provider_error_from_body`, `retryable_status` (true for 408 / 429 / ≥500), `retry_after_seconds` (header parse), `estimate_text_tokens` (chars/4 heuristic).
- **Lines 502–561: Prompt builders** — `LeafSummaryPromptInput` / `InferredCallsPromptInput` DTOs and the two `build_*_prompt` functions emitting `PromptTemplate { id, body }`. Body is a hand-written `format!` template; no Tera/Handlebars.
- **Lines 563–end: Unit tests**, including a TCP-listener-based in-process OpenRouter stub at `openrouter_provider_invokes_chat_completions_and_extracts_usage_tokens`.

**Nothing is cached inside `llm_provider.rs`.** The provider is a stateless dispatcher; caching is `clarion-storage::cache` (per discovery §4, Subsystem B) and lookup is performed by callers in `clarion-mcp::lib.rs` and the analyze-file inferred-edges path. Caching is *lazy and lookup-driven*: callers compute `SummaryCacheKey` from the 5-tuple, hit the cache, and only call `LlmProvider::invoke` on miss — then write the response back keyed on the same tuple.

**Key types & traits:**

| Type | Where | Why callers touch it |
|---|---|---|
| `EntityId`, `entity_id()` | `entity_id.rs` | Every accepted entity is identified by this; cross-language parity proof. |
| `LlmProvider` (trait) | `llm_provider.rs:84` | MCP and analyze layers receive `Arc<dyn LlmProvider>` for dispatch. |
| `OpenRouterProvider`, `RecordingProvider` | `llm_provider.rs:164`, `:99` | Provider construction in `clarion-cli::serve.rs`. |
| `PluginHost<R, W>` | `host.rs:639` | The per-file supervisor; `spawn` for production, `connect` for in-process tests. |
| `AcceptedEntity`, `AcceptedEdge`, `AnalyzeFileOutcome` | `host.rs:346–398` | Hand-off shapes from host → analyse orchestrator → writer-actor. |
| `Manifest` + `parse_manifest` | `manifest.rs:115`, `:281` | `clarion-cli::analyze` reads `plugin.toml` through this; `PluginHost` consumes it. |
| `CrashLoopBreaker` | `breaker.rs:43` | `clarion-cli::analyze` ticks this per plugin-spawn failure. |
| `discover()` / `DiscoveredPlugin` | `discovery.rs:133`, `:47` | Plugin discovery on `$PATH`. |
| `HostError` / `HostFinding` | `host.rs:400`, `:450` | Error matching + finding emission for operator diagnostics + Filigree forwarding. |
| `LlmProviderError` | `llm_provider.rs:44` | Caller uses `.retryable()` to drive WP6 backoff. |

**Dependencies (workspace + outbound):**

- **Inbound** (per `grep clarion-core` against each Cargo.toml):
  - `clarion-storage/Cargo.toml:13`
  - `clarion-mcp/Cargo.toml:13`
  - `clarion-cli/Cargo.toml:20`

  *Note*: `clarion-plugin-fixture` does **not** depend on `clarion-core` — the fixture binary speaks the wire protocol directly without sharing types. This is deliberate (the discovery flagged the fixture is consumed only via subprocess in `wp2_e2e`).

- **Outbound** (`Cargo.toml`):
  - `reqwest` (workspace: `rustls-tls-native-roots`, blocking + JSON) — only `llm_provider.rs`.
  - `serde` + `serde_json` + `toml` — DTO + manifest parsing.
  - `thiserror` — every error enum here.
  - `tracing` — host stderr-drain failures + best-effort-shutdown diagnostics.
  - `nix` (workspace, `resource` feature only) — `apply_prlimit_*` in `limits.rs`. The single `unsafe` allowance in the workspace lives here (`pre_exec` + `setrlimit`); workspace lint floor is `unsafe_code = "deny"` (downgraded from `forbid` per documented exception in workspace `Cargo.toml`).
  - `tempfile` (dev-dep) — tests only.

- **Notably absent**: no `tokio` dependency. The plugin host is fully synchronous (`BufRead + Write` generic bounds, blocking `reqwest`). Async is introduced one layer up in `clarion-storage::writer` (`tokio::task`) and `clarion-mcp::lib.rs`.

**Patterns observed:**

- **Generic reader/writer typestate-lite** — `PluginHost<R, W>` is generic; `spawn` returns a host parameterised on `BufReader<ChildStdout>` / `BufWriter<ChildStdin>`, while `connect` accepts any `BufRead + Write` pair so the in-process `mock.rs` and the e2e tests share one validator pipeline.
- **Drop-on-violation pipeline with discrete kill-paths** — `analyze_file` is a numbered five-step validator; steps 0–2 only emit findings and drop the offending record; steps 3–4 escalate to plugin termination if a breaker trips. The kill-paths route through a single `do_shutdown` (host:1352) that handles the idempotent-shutdown flag.
- **Newtype-with-constructor for safety-critical IDs** — `EntityId(String)` is private-field; the only constructors live in `entity_id.rs` and validate the grammar before producing the newtype. `FromStr` rebuilds via the same constructor.
- **Trait-object provider dispatch** — `Arc<dyn LlmProvider>` consumed by MCP and analyze; live vs recording vs disabled selected at config time (`clarion-cli::serve.rs`).
- **Finding-ID constants as first-class API** — every `FINDING_*` is a `pub const &str` exported at module root so callers (and tests) can `assert_eq!` on the stable ID rather than match on free-form messages. The ID grammar (`CLA-INFRA-*`) is asserted at manifest-parse time (`manifest.rs:414`).
- **Pure-function validators** — `oversize_field`, `oversize_edge_field`, `invalid_unresolved_call_site_reason`, `validate_kind_string`, `validate_rule_id_prefix_grammar` are all stateless and unit-tested in isolation.

**Concerns / risks:**

- **`host.rs` size** (3126 LOC including ~1700 LOC of tests). Production code is ~1450 LOC and is coherent, but the file is the codebase's largest. A future refactor could move the 14 `HostFinding` constructors and the `FINDING_*` constants to a sibling `host_findings.rs` for ~200 LOC saved without altering semantics.
- **`manifest.rs` size** (1508 LOC) — not deep-read in this pass; the public type surface is small (one `Manifest` + five field-structs + one error enum + parser + two grammar validators). Likely test-dominated like `host.rs`, but worth a separate confirm pass.
- **Subprocess constructor is Linux-coupled** — `spawn` at `host.rs:739` uses `pre_exec` + `RLIMIT_AS`/`RLIMIT_NOFILE`/`RLIMIT_NPROC`. `limits.rs:330–342` provides no-op stubs for non-Unix targets, but `discover()` at `discovery.rs:139` is also `#[cfg(unix)]` and returns empty on Windows. v0.1 is Linux-only by intent (per Sprint-1 sign-off), but the cross-platform doors are stubs, not implementations.
- **Reqwest blocking inside a `Send + Sync` trait method** — `LlmProvider::invoke` is sync. Callers from async contexts (`clarion-mcp`) must `spawn_blocking`. Not verified in this pass which side actually does the offloading; if callers run `invoke` on the runtime thread, MCP tool handlers can starve. This belongs as a follow-up question for the `clarion-mcp` catalog pass.
- **OpenRouter strict-JSON regression risk** — the `"strict": true` JSON-Schema gates were added in commit `ab6b1dd` for B.8 GREEN; the path lives entirely in `response_format_for_purpose` (`llm_provider.rs:297`) and is asserted by a test that pattern-matches the serialised request body. A schema change anywhere in the prompt-output contract must update both the prompt template (`build_*_prompt`) and the strict-JSON schema in lockstep — there is no cross-check between them.
- **`unsafe` allowance lives here** — `pre_exec` + `setrlimit` is the workspace's sole `unsafe` block (documented in workspace `Cargo.toml`). Any new unsafe in `clarion-core` should require an ADR amendment.
- **No `tokio` use, but exported types flow into async runtimes** — `AcceptedEntity` / `AcceptedEdge` cross the async boundary via `clarion-storage::WriterCmd`. The DTOs are `Clone + Send` (verified through usage patterns) but this is not enforced by trait bounds at the public boundary; a future field that is not `Send` would silently break downstream.
- **Test-only `mock.rs` (897 LOC) lives inside `src/` under `#[cfg(test)]`** rather than in `tests/`. Pro: integration tests in `tests/wp2_e2e.rs` cannot link against it — keeping mock scaffolding off the public surface is exactly the goal (`mock.rs` is `pub(crate)`). Con: 897 LOC of test scaffolding inflates `clarion-core` source counts; deferred items include `clarion-adeff0916d` (fixture-binary self-build) which may consolidate this.

**Confidence:** High — read `lib.rs` and `plugin/mod.rs` in full; read `host.rs` structural skeleton + `PluginHost` definition + `spawn` constructor + full `analyze_file` body; read `llm_provider.rs` in full through line 561 (the production code; tests sampled); cross-verified inbound deps via `grep clarion-core` against each crate's `Cargo.toml`. Two claims are Medium-confidence and called out above: (a) `manifest.rs` internal proportions not directly verified beyond signature scan; (b) whether MCP wraps `LlmProvider::invoke` in `spawn_blocking` is out-of-scope for this pass and listed as a follow-up.
