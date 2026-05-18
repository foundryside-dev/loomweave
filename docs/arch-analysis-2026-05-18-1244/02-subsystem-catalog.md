# 02 — Subsystem Catalog

**Repository:** `/home/john/clarion`
**Branch:** `sprint-2/b8-scale-test`
**Catalog assembled:** 2026-05-18

This catalog documents the six subsystems identified in `01-discovery-findings.md` §4. Each entry was produced by a dedicated `axiom-system-archaeologist:codebase-explorer` subagent reading the crate's source in depth. Entries cite file:line throughout.

## Subsystem index

| # | Subsystem | Location | Production LOC (approx) | Inbound deps | Outbound (workspace) |
|---|---|---|---|---|---|
| A | `clarion-core` | `crates/clarion-core/` | ~3 100 | storage, mcp, cli | (none) |
| B | `clarion-storage` | `crates/clarion-storage/` | ~1 950 | mcp, cli | core (one symbol) |
| C | `clarion-mcp` | `crates/clarion-mcp/` | ~3 200 | cli | core, storage |
| D | `clarion-cli` | `crates/clarion-cli/` | ~1 740 | (binary — none) | core, storage, mcp |
| E | `clarion-plugin-fixture` | `crates/clarion-plugin-fixture/` | 131 | core (test only) | core (types only) |
| F | Python plugin | `plugins/python/src/clarion_plugin_python/` | ~2 670 | (subprocess of host) | pyright, packaging, wardline (soft) |

Crate-level graph is acyclic (verified by cross-grep of `use clarion_*::` and `Cargo.toml` deps in each subagent's pass). The Python plugin is *not* a Rust crate; the only "dep" relationship is the host-spawns-subprocess contract.

> **Footnotes on the index:** "Inbound deps" lists library-link consumers only — Cargo `[dev-dependencies]` are excluded. `clarion-cli` declares `clarion-plugin-fixture` under `[dev-dependencies]` for its `wp2_e2e` tests but does not link it in the production binary. LOC ranges throughout this catalog reflect the working tree at `sprint-2/b8-scale-test` HEAD on 2026-05-18; `clarion-mcp/src/lib.rs` is 2 623 lines as of this commit (a few entries below quote the immediate-pre-B.8 figure of 2 620 — drift of +3 lines from late-B.8 follow-up).

---

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

  *Note (corrected by validation pass):* `clarion-plugin-fixture` **does** depend on `clarion-core` for the typed protocol structs only (`use clarion_core::plugin::*` in its `main.rs`; declared at `crates/clarion-plugin-fixture/Cargo.toml:18` as `clarion-core = { path = "../clarion-core", version = "0.1.0-dev" }`). It does not link against the supervisor / writer / MCP paths. The fixture is consumed only via subprocess in `wp2_e2e`.

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
## clarion-storage

**Location:** `crates/clarion-storage/`

**Responsibility:** Persists Clarion's entity/edge graph, run provenance, and LLM caches in a single SQLite database under `.clarion/clarion.db`. All mutations funnel through a single writer-actor task (sole `rusqlite::Connection`); all reads come from a `deadpool-sqlite` pool. The crate also owns the schema migration runner, the PRAGMA discipline, the edge-contract validator, and the typed query helpers consumed by `clarion-mcp` and `clarion-cli`.

### Internal structure

**Module roster** (`src/lib.rs:7–15`, all `pub mod`):

| Module | LOC | Role |
|---|---|---|
| `writer.rs` | 817 | Writer-actor: spawn, command loop, edge-contract enforcement, per-N batch commits, parent/contains consistency check at `CommitRun` |
| `query.rs` | 569 | Read-side helpers (graph navigation, FTS-or-LIKE search, unresolved call-site fan-out) |
| `cache.rs` | 251 | `SummaryCacheKey` (5-tuple per ADR-007) + `InferredEdgeCacheKey` (4-tuple) and their upsert/lookup/touch helpers |
| `commands.rs` | 183 | `WriterCmd` enum (9 variants) + POD records + `RunStatus` |
| `schema.rs` | 118 | Embed-and-apply migration runner (`include_str!` of the single `.sql` file) |
| `reader.rs` | 88 | `ReaderPool` wrapper around `deadpool-sqlite::Pool` |
| `unresolved.rs` | 50 | Replace-by-caller bookkeeping for unresolved call sites |
| `error.rs` | 48 | `StorageError` taxonomy (11 variants, `thiserror`) |
| `pragma.rs` | 45 | WAL/synchronous=NORMAL/busy_timeout=5000/foreign_keys=ON discipline |
| `lib.rs` | 35 | Curated `pub use` facade |

**Schema (ER summary)** — single migration `migrations/0001_initial_schema.sql` (289 LOC). Eight base tables, one FTS5 virtual table, three triggers, one view, two generated columns:

```
                    ┌─────────────────────────────────────────────┐
                    │  entities  (PK id TEXT)                     │
                    │  + virtual cols scope_level / scope_rank    │
                    │  + indexes on kind, plugin_id, parent_id,   │
                    │    source_file_id, source_file_path,        │
                    │    content_hash, last_seen_commit,          │
                    │    scope_rank (partial), git_churn (partial)│
                    └──┬──────────────────┬────────┬─────────────┘
                       │self-ref          │FK FK   │FK
            parent_id  │  source_file_id  │        │
                       ▼                  ▼        ▼
   ┌──────────────┐  ┌──────────────────────────────────────┐
   │ entity_tags  │  │ edges  WITHOUT ROWID                 │
   │ (entity_id,  │  │ PK (kind, from_id, to_id)            │
   │  tag) PK     │  │ CHECK confidence IN                  │
   │ ON DELETE    │  │   (resolved, ambiguous, inferred)    │
   │ CASCADE      │  │ FKs: from_id, to_id, source_file_id  │
   └──────────────┘  │   ALL ON DELETE CASCADE              │
                    └──────────────────────────────────────┘
                       ▲                  ▲
                       │entity_id FK      │caller_entity_id FK
   ┌─────────────────────┐  ┌────────────────────────────────┐
   │ findings (PK id)    │  │ entity_unresolved_call_sites   │
   │ CHECK kind ∈ 5      │  │ PK (caller_entity_id,          │
   │ CHECK severity ∈ 5  │  │     caller_content_hash,       │
   │ CHECK status ∈ 4    │  │     site_key)                  │
   │ FK entity_id        │  └────────────────────────────────┘
   └─────────────────────┘
                       ▲caller_entity_id FK
   ┌────────────────────────────┐    ┌──────────────────────────┐
   │ inferred_edge_cache         │    │ summary_cache            │
   │ PK (caller_entity_id,       │    │ PK 5-tuple (entity_id,   │
   │     caller_content_hash,    │    │     content_hash,        │
   │     model_id,               │    │     prompt_template_id,  │
   │     prompt_version)         │    │     model_tier,          │
   │ FK caller_entity_id         │    │     guidance_fingerprint)│
   └────────────────────────────┘    │ CHECK stale_semantic∈(0,1)│
                                     └──────────────────────────┘

   ┌──────────────────────────┐   ┌────────────────────────────┐
   │ runs (PK id)             │   │ schema_migrations          │
   │ CHECK status ∈ (running, │   │ (version PK, name,         │
   │   skipped_no_plugins,    │   │  applied_at)               │
   │   completed, failed)     │   └────────────────────────────┘
   └──────────────────────────┘

   FTS5 virtual: entity_fts (entity_id UNINDEXED, name, short_name,
     summary_text, content_text); kept in sync by triggers
     entities_ai / entities_au / entities_ad.

   View: guidance_sheets — projects entities WHERE kind='guidance'
     with json_extract of `properties` + json_group_array of tags.
```

ADR-031 `CHECK` discipline (lines 89, 107–112, 124–125, 153, 200–201): closed core-owned vocabularies receive `CHECK` clauses (`edges.confidence`, `findings.{kind, severity, status}`, `summary_cache.stale_semantic`, `runs.status`). Plugin-extensible vocabularies deliberately omit `CHECK` per ADR-022 — `entities.kind` (`migrations/0001_initial_schema.sql:33–36`) and `edges.kind` (`migrations/0001_initial_schema.sql:77–81`); enforcement at those columns is the writer-actor (`writer.rs::enforce_edge_contract` for edges, manifest acceptance for entity kinds).

**Writer-actor command set** (`commands.rs::WriterCmd`, 9 variants):

| Variant | Lifecycle | Notes |
|---|---|---|
| `BeginRun` | analyze-time | `runs` INSERT with `status='running'`, opens `BEGIN` (`writer.rs:308–330`) |
| `InsertEntity` | analyze-time | Single INSERT into `entities`; counts toward batch boundary (`writer.rs:332–390`) |
| `InsertEdge` | analyze-time | Calls `enforce_edge_contract` (`writer.rs:411–472`) then `INSERT OR IGNORE`; dedupe increments `dropped_edges_total`, ambiguous accepts bump `ambiguous_edges_total` (`writer.rs:474–520`) |
| `InsertInferredEdges` | query-time (MCP) | Upserts inferred-edge cache row, GCs stale inferred edges for the caller, inserts new ones; refuses to shadow static resolved/ambiguous calls (`writer.rs:522–599`) |
| `UpsertSummaryCache` | query-time (MCP) | 5-tuple upsert on `summary_cache` (`cache.rs:48–85`) |
| `TouchSummaryCache` | query-time (MCP) | `UPDATE summary_cache SET last_accessed_at` (`cache.rs:112–132`) |
| `ReplaceUnresolvedCallSitesForCaller` | analyze-time | Delete-then-insert pattern; replaces all sites for one caller atomically inside the run transaction (`writer.rs:601–621`, `unresolved.rs:20–50`) |
| `CommitRun` | analyze-time | Runs the B.3 parent/contains dual-encoding check **inside** the open transaction (`writer.rs:733–796`); on mismatch rolls back the run's writes and marks `runs.status='failed'` with `CLA-INFRA-PARENT-CONTAINS-MISMATCH` in `stats.failure_reason`; on success folds the `runs` UPDATE into the final COMMIT (`writer.rs:671–727`) |
| `FailRun` | analyze-time | ROLLBACK + `UPDATE runs SET status='failed'` (`writer.rs:798–817`) |

The actor multiplexes analyze-time and query-time mutations on the same connection. `query_time_write` (`writer.rs:647–669`) commits any open analyze-time batch, runs the MCP write, and reopens a `BEGIN` if a run is still active — so analyze-time and MCP traffic cannot deadlock or interleave on the same transaction. Batch cadence is `DEFAULT_BATCH_SIZE = 50` writes (`writer.rs:35`, `bump_writes_and_maybe_commit` at `writer.rs:628–645`); the `INSERT OR IGNORE` edge dedupe is workload-shape-invariant because UNIQUE conflicts still bump the batch counter.

Channel-closed cleanup (`writer.rs:251–273`): if the `Writer` is dropped mid-run, the actor self-heals by issuing `ROLLBACK` and marking the surviving run row `failed` with `failure_reason="writer channel closed unexpectedly"`. This is the durability backstop for the supervisor in `clarion-cli::analyze`.

**Edge contract** (`writer.rs::enforce_edge_contract`, line 411). Ontology is hard-coded as `STRUCTURAL_EDGE_KINDS = ["contains", "in_subsystem", "guides", "emits_finding"]` (`writer.rs:394`) and `ANCHORED_EDGE_KINDS = ["calls", "references", "imports", "decorates", "inherits_from"]` (`writer.rs:395–401`) — nine kinds total per ADR-026/028. Structural edges MUST have `confidence=resolved` and NULL `source_byte_*`; anchored edges MUST have both `source_byte_*` set, and may NOT be `inferred` at scan time (`writer.rs:440–449`) because inferred-tier edges are query-time-only. Violations return `StorageError::WriterProtocol` with one of three CLA codes (`CLA-INFRA-EDGE-CONFIDENCE-CONTRACT`, `CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT`, `CLA-INFRA-EDGE-UNKNOWN-KIND`) so the surrounding `runs.stats.failure_reason` carries the code (`writer.rs:402–410`).

**Reader pool** (`reader.rs`). `ReaderPool::open` builds a `deadpool_sqlite::Pool` with `Runtime::Tokio1` and a caller-supplied `max_size` (the CLI passes its own value; tests use small caps). `with_reader` acquires from the pool, submits a `'static` closure to deadpool's `interact()` blocking task pool, and applies read-side PRAGMAs (`busy_timeout=5000`, `foreign_keys=ON`) on every acquisition. Retry-on-`SQLITE_BUSY` is delegated to SQLite itself via `busy_timeout` rather than an application-level loop — both writer and readers wait up to 5 s for the lock. WAL mode (set on the writer's first connection, `pragma.rs:16–31`) is what lets readers proceed concurrently without seeing in-flight writes. `waiting_count()` (`reader.rs:85–87`) is exposed `#[doc(hidden)]` for deterministic test polling.

**Cache keys.** `SummaryCacheKey` (`cache.rs:7–14`) materialises ADR-007's 5-tuple exactly: `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`. `ontology_version` is *not* in the key (correct per ADR-007 — that field is handshake validation only). `InferredEdgeCacheKey` (`cache.rs:30–36`) is a 4-tuple `(caller_entity_id, caller_content_hash, model_id, prompt_version)`. **Boundary clarification**: cache lookup/upsert helpers in `cache.rs` are pure storage operations; on a miss, `clarion-mcp::lib.rs` decides whether to call the LLM (via `clarion-core::LlmProvider`), then enqueues the result via `WriterCmd::UpsertSummaryCache` or `WriterCmd::InsertInferredEdges`. This crate does not depend on `clarion-core::LlmProvider`; its only `clarion-core` dependency is `EdgeConfidence` (`commands.rs:14`, `query.rs:6`).

**Query helpers re-exported via `lib.rs`** (`lib.rs:27–32`): `entity_by_id`, `entity_at_line` (innermost-entity-at-line with tie-break by source-range size then kind preference function→class→module), `find_entities` (FTS5 if the pattern is alnum/underscore; LIKE-with-escape otherwise — see `is_fts_safe` at `query.rs:552`), `call_edges_from` / `call_edges_targeting` (apply ADR-028 confidence ceiling; for `ambiguous` edges, expand the `properties.candidates[]` JSON array into multiple match rows — `query.rs:218–235`, `523–534`), `contained_entity_ids` (iterative DFS over `contains` edges with cycle guard and `max_entities` truncation — `query.rs:354–388`), `unresolved_call_sites_for_caller`, `unresolved_callers_for_target` (LIKE-suffix match on `callee_expr` with same-file preference — `query.rs:294–332`), `candidate_entities_for_unresolved_sites`, `child_entity_ids`, `normalize_source_path` (project-root jail; both lexical normalisation and `canonicalize()` are checked — `query.rs:76–104`).

### External interface

`lib.rs` (35 LOC) re-exports a closed surface: the `WriterCmd`/`EdgeRecord`/`EntityRecord`/`RunStatus` typed boundary; `Writer` and the two channel/batch constants; `ReaderPool`; the query helpers; the cache key types and their three pure helpers (`summary_cache_lookup`, `inferred_edge_cache_lookup`, `inferred_edge_cache_key_id`); `StorageError`/`Result`. Internal modules `pragma` and `schema` are `pub mod`, used by `clarion-cli::install` (`crates/clarion-cli/src/install.rs:20`). `clarion-mcp::lib.rs:22–30` consumes 18 named symbols; `clarion-cli::analyze.rs:24–27` consumes 4 (writer/command shapes only).

### Dependencies

- **Inbound** (verified via `use clarion_storage::` grep):
  - `clarion-mcp` — full read surface + the four query-time `WriterCmd` variants
  - `clarion-cli` — `analyze.rs` (writer + commands), `install.rs` (`pragma` + `schema`), `serve.rs` (`Writer` + `ReaderPool` + batch constants)
- **Outbound** (`Cargo.toml`):
  - `clarion-core` — only for `EdgeConfidence` (used in `commands.rs` + `query.rs`); intentionally minimal
  - `deadpool-sqlite 0.8` — async-friendly read pool (ADR-011)
  - `rusqlite 0.31` — bundled SQLite, sole write driver
  - `tokio` — `mpsc` + `oneshot` channels, `spawn_blocking` for the writer task
  - `serde_json` — JSON shape validation on `InferredCallEdgeRecord.properties_json` + `ambiguous` `candidates[]` decoding
  - `thiserror`, `tracing`

No outbound dependency on `clarion-mcp`, `clarion-cli`, or any plugin crate. Crate-level acyclicity holds.

### Patterns observed

- **Actor + pool split (ADR-011).** Single writer task owns the write connection; all multi-row mutations are batched into a transaction sized by writes (entity inserts + edge insert attempts, including dedupes). The pattern is documented as L3 lock-in (`writer.rs:1–13`).
- **Typed command boundary.** Every mutation is a `WriterCmd` variant carrying its own `oneshot::Sender<Result<T>>` ack — per-command response, no batched fan-in. Adding a new mutation is a single-file append (`commands.rs`) plus a match arm (`writer.rs:152–249`).
- **Defence in depth on closed vocabularies (ADR-031).** Two enforcement layers: the writer-actor (canonical) and SQL `CHECK` (backstop). The migration's per-column comments name which ADR closes each vocabulary; plugin-extensible columns are explicitly tagged "no CHECK by policy."
- **Edge-contract failure codes are findings.** When `enforce_edge_contract` rejects, the error message embeds `CLA-INFRA-EDGE-*` codes that surface in `runs.stats.failure_reason` — making writer-rejected edges observable as machine-greppable findings rather than opaque protocol errors.
- **`query_time_write` interleaves cleanly.** Query-time MCP writes commit the analyze-batch first, then reopen `BEGIN` if a run is still in progress — the actor never holds an MCP cache row open inside an analyze transaction.
- **Validation depth on path inputs.** `normalize_source_path` does lexical normalisation *and* `canonicalize()`, and checks containment against the canonicalized project root in both forms (`query.rs:76–104`). Prevents symlink/`..` escape against `entity_at_line` and `find_entity`.
- **B.3 dual-encoding check at commit.** Parent/contains consistency is verified inside the transaction at `CommitRun` time (`writer.rs:733–796`), so an inconsistent run rolls back rather than persisting a half-corrupt graph.

### Concerns

- **Single migration, edit-in-place under ADR-024.** `migrations/0001_initial_schema.sql` has been edited three times (initial; 2026-05-03 ADR-024 vocabulary rename; 2026-05-18 ADR-031 `CHECK` clauses). The retirement trigger is documented in-file (`0001_initial_schema.sql:10–16`) but no automated check fires when the trigger condition (external operator builds `.clarion/clarion.db` from a published Clarion build) is met. Manual discipline only. Mitigated by the migration's own `schema_migrations` row idempotence (`schema.rs:81–89`).
- **Edge ontology is duplicated.** `STRUCTURAL_EDGE_KINDS` + `ANCHORED_EDGE_KINDS` are hard-coded in `writer.rs:394–401`; ADR-026/028 are the design source; the Python plugin's manifest declares `edge_kinds = ["contains", "calls", "references"]` independently. A new kind requires edits in at least three places (manifest, writer, ADR). No compile-time enforcement that these stay in sync.
- **Schema-shape FK in `entities.source_file_id` is self-referential** (`migrations/0001_initial_schema.sql:40`). Works because source-file entities are inserted before their contained functions/classes (plugin traversal order), but there is no constraint that enforces insertion order. A plugin emitting children before parents would fail with an FK violation, surfacing as an opaque `rusqlite::Error` rather than a writer-protocol error.
- **`busy_timeout=5000` is the only `SQLITE_BUSY` mitigation.** Under heavy contention a reader can fail with a SQLite-level busy error rather than being retried at the application layer. The B.8 scale test exercises this path in practice; no per-attempt retry loop exists in `with_reader`.
- **`InsertEdge` and `InsertEntity` share a single batch counter.** An edge-heavy file (e.g., a module with many `references` edges) can flush the batch boundary mid-file. Documented behaviour (`writer.rs:285–289`) but worth flagging — long transactions are not bounded by file boundary.
- **No write-side throttling on `WriterCmd` channel.** `DEFAULT_CHANNEL_CAPACITY = 256` (`writer.rs:38`); a faster producer than the actor will block via `Sender::send().await` backpressure, which is correct, but no metric is exposed for "time spent blocked on writer queue."

### Confidence

**Confidence:** High — Read 100% of every `src/*.rs` module (10 files, 1 950 LOC) and the migration in full (289 LOC). Cross-validated dependency direction by grepping `use clarion_storage::` across the workspace: only `clarion-mcp` and `clarion-cli` consume it; no inbound cycles. Confirmed `WriterCmd` variant count (9) matches the actor's match arms one-to-one. Schema CHECK constraints verified at exact line numbers against ADR-031's "closed vs. extensible" decision. Edge-contract code-paths (`enforce_edge_contract` and the three CLA codes it emits) read end-to-end. ADR cross-references are inline in both the migration and the writer source, so the "ADR says X / code does Y" gap is small.
## clarion-mcp

**Location:** `crates/clarion-mcp/`

**Responsibility:** Speaks MCP protocol revision `2025-11-25` over stdio; serves seven storage-backed read tools (`entity_at`, `find_entity`, `callers_of`, `execution_paths_from`, `summary`, `issues_for`, `neighborhood`) to consult-mode LLM agents, with on-demand LLM dispatch for leaf-scope summaries and inferred call edges, and optional Filigree enrichment for issue attachment.

**Key Components:**

- `src/lib.rs` (2620 LOC) — single-file crate root. Internal organisation, top-to-bottom:
  - **Protocol surface** (lines 36–166): `MCP_PROTOCOL_VERSION = "2025-11-25"` constant (line 36), `ToolDefinition` struct (40–45), `list_tools()` returning seven hardcoded `ToolDefinition`s with inline JSON-Schema (48–120), schema helpers `confidence_schema` / `id_schema` / `id_confidence_schema` (122–151), stateless `handle_json_rpc` router (154–166) wired to `initialize` / `tools/list` / `tools/call`.
  - **`ServerState`** (168–1252): the central handler. Fields (168–178): `project_root: PathBuf`, `readers: ReaderPool`, `execution_edge_cap: usize` (default 500), `summary_llm: Option<SummaryLlmState>`, `clock: Arc<dyn Fn() -> String + Send + Sync>`, `budget: Arc<Mutex<BudgetLedger>>`, `inferred_inflight: Arc<AsyncMutex<HashMap<InferredEdgeCacheKey, broadcast::Sender<InferredDispatchOutcome>>>>` (in-flight dispatch coalescing), `filigree_client: Option<Arc<dyn FiligreeLookup>>`. Builder methods (180–226): `new`, `with_edge_cap`, `with_summary_llm`, `with_clock`, `with_filigree_client`.
  - **Per-tool dispatch** (228–294): instance `handle_json_rpc` + `handle_tool_call` route by name to seven `tool_*` async methods.
  - **Per-tool handlers** (296–717): `tool_entity_at` (296–319), `tool_find_entity` (321–354), `tool_callers_of` (356–391), `tool_execution_paths_from` (393–434) + `inferred_execution_paths` (436–520), `tool_neighborhood` (522–581), `tool_issues_for` (583–671), `tool_summary` (673–717).
  - **LLM inferred-edges pipeline** (719–995): `ensure_inferred_for_target` / `ensure_inferred_for_caller` (719–796) — entry points called by `callers_of`/`neighborhood`/`execution_paths_from` when `confidence == Inferred`; `read_inferred_inputs` (798–833) builds an `InferredEdgeCacheKey` from caller content-hash + model_id + prompt_version (ADR-007); `materialize_cached_inferred` (835–863) on cache hit; `coalesced_inferred_dispatch` (865–908) deduplicates concurrent dispatches via a `broadcast` channel with a 60-second timeout; `perform_inferred_dispatch` (910–995) builds the prompt via `clarion_core::build_inferred_calls_prompt`, reserves budget, invokes the provider on a `spawn_blocking` task, then sends `WriterCmd::InsertInferredEdges` to the writer-actor.
  - **LLM summary pipeline** (997–1155): `read_summary_inputs` (997–1035) keys against `SummaryCacheKey { entity_id, content_hash, prompt_template_id = LEAF_SUMMARY_PROMPT_TEMPLATE_ID, model_tier, guidance_fingerprint = "guidance-empty" }` — five-tuple cache key matching ADR-007; `cached_summary_envelope` (1037–1060) bumps `last_accessed_at` via `WriterCmd::TouchSummaryCache`; `refresh_summary` (1062–1155) on miss invokes provider, then `WriterCmd::UpsertSummaryCache`. ADR-030 leaf-scope is encoded by the single `LEAF_SUMMARY_PROMPT_TEMPLATE_ID` template and the `summary` tool description (line 98).
  - **Writer-actor helper + budget** (1157–1251): `send_writer` (1157–1171) — oneshot ack roundtrip pattern; `BudgetLedger` accounting with `reserve_budget` / `BudgetReservation::commit` / `Drop` rollback (1180–1316); `summary_model_id` / `inferred_edges_model_id` / `max_inferred_edges_per_caller` (1221–1251) honour `LlmProvider::tier_to_model("summary"|"inferred_edges")`.
  - **Plain types** (1266–1607): `SummaryLlmState`, `BudgetLedger`, `SummaryRead` enum, `SummaryReady`, `IssuesForRead`, `IssuesForAccumulator` (drift-classifier matching `content_hash_at_attach` against current `entities.content_hash`, lines 1379–1411, with 100-issue cap), `InferenceLlmState`, `InferredRead`, `InferredDispatchStats` (aggregated stats_delta), `InferredDispatchFailure`, `InferredDispatchOutcome`, `InferredCallsResponse` / `InferredCallsResponseEdge`.
  - **Transport loop** (1609–1686): `McpError` enum, `handle_frame` / `handle_frame_with_state`, `serve_stdio` / `serve_stdio_with_state` / `serve_stdio_with_state_on_runtime` — Content-Length frame loop using `clarion_core::plugin::{read_frame, write_frame, ContentLengthCeiling::DEFAULT, Frame, TransportError}`; treats `UnexpectedEof` as clean shutdown.
  - **Stateless `handle_tool_call`** (1701–1736): a stub kept for the stateless `handle_json_rpc` router; emits `tool-unimplemented` envelopes for every tool — only reachable from the stateless entry point used by tests and `handle_frame`.
  - **Envelope/parsing helpers** (1738–2419): `ParamError`, `PathTraversal` (recursive walker for `execution_paths_from`, 1755–1802), `ReferenceDirection`, `required_str` / `required_i64` / `optional_usize` / `optional_bool` / `optional_confidence` argument coercers, envelope builders (`success_envelope`, `tool_error_envelope`, `tool_error_envelope_with_diagnostics`, `success_envelope_with_truncation_and_stats`), `entity_json`, `source_excerpt` + `line_range_excerpt` + `truncate_excerpt`, `inferred_records_from_result` (parses LLM JSON into `InferredCallEdgeRecord`, 2216–2266), `summary_cache_expired` + `timestamp_day_index` + `days_from_civil` (Howard Hinnant civil-from-days algorithm for the 180-day TTL), `caller_json` / `callee_json` / `path_json` / `reference_neighbors`.
  - **Unit tests** (2421–2620): 8 tests covering tool-list exact docstrings, initialize result, tools/list wrapping, unknown method/tool/params, frame dispatch round-trip, and multi-frame serve-stdio.
- `src/config.rs` (352 LOC) — `McpConfig` (YAML via `serde_norway`) with `llm` and `integrations.filigree` sections; `LlmConfig` (provider, `enabled`, `allow_live_provider`, `session_token_ceiling: u64` default 1_000_000, `model_id` default `"anthropic/claude-sonnet-4.6"`, `cache_max_age_days` default 180, `max_inferred_edges_per_caller` default 8); `LlmProviderKind::{OpenRouter, Anthropic, Recording}` (with Anthropic actively rejected — `ConfigError::DeprecatedProvider` with code `CLA-CONFIG-DEPRECATED-PROVIDER`, lines 34–43, 169–171); accepts `llm_policy` alias for the `llm` block (line 10, test at 270–284); `select_provider_with_env` (156–191) returns `ProviderSelection::{Disabled, Recording, OpenRouter{api_key_env}}` — opt-in to live OpenRouter is gated by `allow_live_provider` *or* the `CLARION_LLM_LIVE=1` env (173); presence of an API key alone is not enough (test at 287–303); `FiligreeConfig` (127–147) — `enabled` default false, `base_url` default `http://127.0.0.1:8766`, `actor` default `clarion-mcp`, `token_env` default `FILIGREE_API_TOKEN`, `timeout_seconds` default 5. Five unit tests.
- `src/filigree.rs` (238 LOC) — `EntityAssociationsResponse` / `EntityAssociation` (Filigree ADR-029 contract); `FiligreeLookup` trait (45–50); `FiligreeHttpClient` (52–111) — blocking `reqwest::blocking::Client` with `timeout_seconds.max(1)`, returns `Ok(None)` when `enabled=false` (68–70); `associations_for` GETs `{base_url}/api/entity-associations?entity_id={encoded}` with `x-filigree-actor` and optional `Bearer` token; manual percent-encoding restricted to unreserved chars (127–142); three unit tests including a real TCP-server roundtrip (194–237).
- `tests/storage_tools.rs` (1710 LOC) — heavyweight integration tests; 11+ `#[tokio::test]` cases exercising each tool against a seeded SQLite database with `RecordingProvider` LLM substitutes and a stub `FiligreeLookup` (`association` builder at line 573).

**Dependencies:**

- Inbound: `clarion-cli` (only — `crates/clarion-cli/src/serve.rs` is the sole consumer of the public API: `McpConfig::from_path`, `select_provider_with_env`, `FiligreeHttpClient::from_config`, `ServerState::new` + builders, `serve_stdio_with_state_on_runtime`).
- Outbound:
  - `clarion-core` — `EdgeConfidence`, `INFERRED_CALLS_PROMPT_VERSION`, `InferredCallsPromptInput`, `LEAF_SUMMARY_PROMPT_TEMPLATE_ID`, `LeafSummaryPromptInput`, `LlmProvider`, `LlmProviderError`, `LlmPurpose`, `LlmRequest`, `LlmResponse`, `build_inferred_calls_prompt`, `build_leaf_summary_prompt`; transport `plugin::{ContentLengthCeiling, Frame, TransportError, read_frame, write_frame}` (`lib.rs:11–21`).
  - `clarion-storage` — 20 symbols (`lib.rs:22–30`): types `CallEdgeMatch`, `EntityRow`, `InferredCallEdgeRecord`, `InferredEdgeCacheEntry`, `InferredEdgeCacheKey`, `InferredEdgeWriteStats`, `ReaderPool`, `StorageError`, `SummaryCacheEntry`, `SummaryCacheKey`, `UnresolvedCallSiteRow`, `WriterCmd`; functions `call_edges_from`, `call_edges_targeting`, `candidate_entities_for_unresolved_sites`, `child_entity_ids`, `contained_entity_ids`, `entity_at_line`, `entity_by_id`, `find_entities`, `inferred_edge_cache_key_id`, `inferred_edge_cache_lookup`, `normalize_source_path`, `summary_cache_lookup`, `unresolved_call_sites_for_caller`, `unresolved_callers_for_target`.
  - External crates: `tokio` (mpsc/oneshot/broadcast, current-thread runtime, `spawn_blocking`), `serde` / `serde_json`, `serde_norway` (YAML), `reqwest::blocking` (Filigree HTTP), `rusqlite` (one direct `prepare` for `reference_neighbors` at line 2378 — the only raw SQL in this crate; everything else routes through `clarion-storage` helpers), `thiserror`.

**Patterns Observed:**

- **Read-side dispatch is genuinely thin.** Six of the seven tools (`entity_at`, `find_entity`, `callers_of` non-inferred, `execution_paths_from` non-inferred, `neighborhood` non-inferred, `issues_for`) call a `clarion-storage` helper through `ReaderPool::with_reader`, then envelope the result. Transformation done in `clarion-mcp` is narrow: building tool envelopes (`ok` / `result` / `error` / `diagnostics` / `truncated` / `truncation_reason` / `stats_delta`), shaping `entity_json` / `caller_json` / `callee_json` / `path_json` projections, and the `IssuesForAccumulator` drift classifier. The one substantive in-crate transform is `inferred_records_from_result` (2216–2266) which parses LLM JSON into `InferredCallEdgeRecord` and joins it back against the unresolved-site rows by `site_key`.
- **LLM dispatch lives in this crate, not in `clarion-core`.** `clarion-core` provides prompt templates, the `LlmProvider` trait, and request/response types. The MCP layer owns: cache lookup (`summary_cache_lookup`, `inferred_edge_cache_lookup`), cache-staleness checks (`stale_semantic`, `summary_cache_expired`, ADR-007 five-tuple key in `read_summary_inputs:1010–1016`), the budget ledger (`BudgetReservation` with RAII rollback on drop), the in-flight dispatch coalescer (`inferred_inflight` keyed by `InferredEdgeCacheKey`, 60s timeout), prompt construction, provider invocation (`spawn_blocking` to bridge to the sync provider trait), and the writeback via `WriterCmd::{UpsertSummaryCache, TouchSummaryCache, InsertInferredEdges}`.
- **Filigree integration is genuinely enrich-only.** Three independent skip paths route to `issues_unavailable` (lines 589–594, 619–628), which returns `ok=true` with `available=false` and a `reason` enum (`filigree-disabled` / `filigree-unreachable` / `filigree-client-error` / `entity-not-found`). The other six tools have no Filigree dependency. `FiligreeHttpClient::from_config` returns `Ok(None)` when disabled (filigree.rs:68–70) — disabled is the default. No code path makes a Filigree response required for the tool to succeed; this matches Loom federation §3 (`docs/suite/loom.md`).
- **Confidence-tier opt-in (ADR-028).** `optional_confidence` (1861–1872) defaults to `Resolved`. `inferred` is the only tier that triggers LLM dispatch (via `ensure_inferred_for_target` / `ensure_inferred_for_caller`). `ambiguous` is read-only over existing static-edge rows.
- **Writer-actor handoff.** All mutating storage operations go through `mpsc::Sender<WriterCmd>` + a `oneshot::Sender<Result<T, StorageError>>` ack (`send_writer`, 1157–1171). Translates `WriterGone` / `WriterNoResponse` storage errors into retryable tool envelopes. ADR-011 compliance.
- **Stateful and stateless entry points co-exist.** The stateless `handle_json_rpc` (line 154) + `handle_tool_call` (line 1701) pair always emits `tool-unimplemented` envelopes for every tool; the stateful `ServerState::handle_json_rpc` (line 228) is the real dispatcher. The stateless path remains usable for `initialize` / `tools/list` and is exercised by unit tests inside the file.
- **Truncation contract** — every list-shaped success envelope can carry `truncation_reason: "edge-cap" | "issue-cap" | "entity-cap"`. `execution_paths_from` enforces `execution_edge_cap` (default 500); `issues_for` enforces a hardcoded 100-issue cap (line 631) and a 1000-contained-entity cap (`read_issues_for_entities:655`).
- **Time normalisation as plain math.** No `chrono` / `time` dependency — `default_now_string` emits `unix:{seconds}`, `timestamp_day_index` accepts either `unix:N` or ISO `YYYY-MM-DD…` prefix, and `days_from_civil` is the Howard Hinnant algorithm inline (2306–2314).

### Tool table

| Tool | Required inputs | Optional inputs | Primary storage helper | LLM dispatch path |
|------|------------------|------------------|-------------------------|--------------------|
| `entity_at` | `file: string`, `line: int ≥ 1` | — | `entity_at_line` (after `normalize_source_path`) | No |
| `find_entity` | `pattern: string` | `limit: 1..=100` (default 20), `cursor: string\|null` (numeric offset) | `find_entities` | No |
| `callers_of` | `id: string` | `confidence: resolved\|ambiguous\|inferred` (default `resolved`) | `call_edges_targeting` + `entity_by_id` | Only when `confidence=inferred` → `ensure_inferred_for_target` |
| `execution_paths_from` | `id: string` | `max_depth: 1..=8` (default 3), `confidence` (default `resolved`) | `call_edges_from` + `PathTraversal` (resolved/ambiguous) or `inferred_execution_paths` (inferred) | Only when `confidence=inferred` → bounded BFS of `ensure_inferred_for_caller` per node |
| `summary` | `id: string` | — | `summary_cache_lookup` then `WriterCmd::TouchSummaryCache` / `UpsertSummaryCache` | **Yes** — leaf-scope (ADR-030), `LlmPurpose::Summary`, max_output_tokens 512 |
| `issues_for` | `id: string` | `include_contained: bool` (default true) | `entity_by_id` + `contained_entity_ids` (cap 1000) | No — Filigree HTTP per entity via `spawn_blocking` |
| `neighborhood` | `id: string` | `confidence` (default `resolved`) | `entity_by_id` + `call_edges_targeting` + `call_edges_from` + `child_entity_ids` + `reference_neighbors` | Only when `confidence=inferred` → both `ensure_inferred_for_target` *and* `ensure_inferred_for_caller` (lines 528–534) |

### LLM cache-key dispatch table

| Path | Cache key tuple (ADR-007) | Source line |
|------|----------------------------|-------------|
| Summary | `entity_id` + `content_hash` + `prompt_template_id = LEAF_SUMMARY_PROMPT_TEMPLATE_ID` + `model_tier` (from `LlmProvider::tier_to_model("summary")`) + `guidance_fingerprint = "guidance-empty"` | `lib.rs:1010–1016` |
| Inferred edges | `caller_entity_id` + `caller_content_hash` + `model_id` (from `tier_to_model("inferred_edges")`) + `prompt_version = INFERRED_CALLS_PROMPT_VERSION` | `lib.rs:816–821` |

On miss, both paths reserve via `BudgetLedger` (token-pessimistic, ceiling default 1_000_000 input+output combined), invoke the provider on `spawn_blocking`, commit actual tokens (or flip `blocked=true`), and on success write back via the writer-actor. Subsequent dispatches for the same key short-circuit through the in-flight coalescer (`coalesced_inferred_dispatch`, 865–908) which `broadcast::subscribe`s and waits up to 60 s.

**Concerns:**

- **`lib.rs` is 2620 LOC in one file.** Discovery question #1 — internal structure is *coherent* (clear bands: protocol surface → `ServerState` → per-tool handlers → LLM pipelines → transport loop → helpers), but the file is large enough to warrant subdivision on size grounds alone. The `IssuesForAccumulator`, the LLM inferred-edges pipeline (719–995), the LLM summary pipeline (997–1155), the budget ledger (1180–1316), and the envelope/projection helpers (1874–2400) are each plausible standalone modules. The current single-file layout is not a god-file in the architectural sense — each band has a single responsibility — but the size will degrade code-review velocity and grep ergonomics.
- **Dead stateless `handle_tool_call` stub** (`lib.rs:1701–1736`): emits `tool-unimplemented` envelopes for every tool name. Reachable only via the stateless `handle_json_rpc` (154) and `handle_frame` (1621) paths. `handle_frame` is exercised by one unit test (2542–2560) and is exported from the crate, so any external consumer that wires the stateless path will get permanently-broken `tools/call` responses. Not a runtime defect inside the CLI (which uses `handle_frame_with_state`), but a footgun in the public API.
- **`reference_neighbors` (lib.rs:2363–2400)** issues raw SQL against the `edges` table directly, bypassing the `clarion-storage` helper layer. It is the *only* place in this crate that does so. This creates a hidden coupling to the `edges` schema (`kind = 'references'`, `confidence`, `source_byte_start`, `source_byte_end`) outside the storage crate. If the schema changes, `clarion-storage` callers and tests will catch it but this function will not.
- **`InferredCallsResponseEdge::confidence: Option<f64>` field is parsed but unused for envelope shape** (lib.rs:1601–1607, used in 2247–2255). The model's reported confidence is stored as a property in the inferred-edge row's `properties_json`, but the storage layer does not surface it back through `caller_json`/`callee_json` — the tool envelope only exposes `edge_confidence` (the tier: resolved/ambiguous/inferred). The model's per-edge confidence is therefore preserved on disk but not queryable; consumers needing the score must hand-parse `properties_json`.
- **Coalesced-dispatch waiters get a generic stats delta when the leader fails.** `InferredDispatchOutcome::from_result` (1574–1595, used at 902–908) broadcasts a clonable outcome; non-leader waiters that receive a failure outcome surface it as their own (line 884–887). Acceptable, but the leader's diagnostics (e.g. `CLA-LLM-INVALID-JSON` usage block) are visible to the leader only; waiters see the failure code/message but lose the per-response diagnostics array. Minor observability gap.
- **`source_excerpt` (lib.rs:2151)** uses `std::fs::read_to_string` on the *current on-disk file path* to build LLM prompt input. This is *not* the content hash that keyed the cache; the file may have changed between the time the entity was scanned and the time the tool runs. The cache key still uses the stored `content_hash`, so a stale read here produces a cache miss with a fresh-but-misaligned prompt. The `stale_semantic` flag covers structural drift (caller_count / fan_out) but not source-text drift. Documented as a known v0.1 trade-off in the surrounding code? — no comment found.
- **`BudgetLedger::blocked` is sticky for the lifetime of the `ServerState`**: once any reservation overshoots, `blocked` flips to `true` (1196, 1296) and every subsequent LLM tool returns `token-ceiling-exceeded` until process restart. No reset path; no way to lift the ceiling without dropping the state. Matches a session-token semantic but is undocumented in the public API.
- **`FiligreeConfig::actor` blank handling**: `FiligreeHttpClient::associations_for` only sets the `x-filigree-actor` header when the actor string is non-blank (filigree.rs:94–96); Filigree currently requires the header for some endpoints. Silent omission rather than rejection at config-load.
- **No `Drop` cleanup for in-flight broadcast senders on leader cancellation.** If the leader task panics or is dropped *before* reaching the explicit `remove` at line 904, the `inferred_inflight` entry leaks until the next dispatch for the same key. `broadcast::Sender` itself is not a resource leak, but the map entry blocks subsequent dispatches from claiming leadership; subsequent callers will subscribe to a now-dead sender and time out at 60 s. Low-probability but present.

### Confidence Assessment

**Confidence:** High — Read 100% of `config.rs` (352 lines), 100% of `filigree.rs` (238 lines), 100% of the `lib.rs` declarations/dispatch (1–1700) and the helper/test bands (1700–2620). Sampled five handler bodies in full (`tool_entity_at`, `tool_find_entity`, `tool_callers_of`, `inferred_execution_paths`, `tool_issues_for`, `tool_summary`) and the LLM pipelines (`ensure_inferred_for_caller` through `perform_inferred_dispatch`). Cross-verified inbound dependency by reading `clarion-cli/src/serve.rs` (137 lines). Cross-verified outbound dependency claims against the `use` block at `lib.rs:11–34`. Cross-verified ADR alignment by quoting cache-key construction sites (5-tuple at 1010–1016, 4-tuple at 816–821) and ADR-028 default in `optional_confidence` (1864).

### Risk Assessment

- **Size-induced review burden** (lib.rs 2620 LOC) — Medium operational risk; not a correctness risk. The internal banding makes incremental refactors safe.
- **Dead stateless `handle_tool_call` stub in public API surface** — Low surface but high blast radius for any external consumer. Single fix: either remove from public exports or make it forward to the same handlers as the stateful path.
- **`reference_neighbors` raw SQL** — Schema-coupling risk; would not be caught by `clarion-storage` integration tests.
- **`source_excerpt` reads live disk, not the hashed snapshot** — Correctness drift risk under concurrent file modification; affects prompt fidelity but not cache correctness.
- **Sticky budget ledger** — Operational risk: process restart required after one ceiling breach. Acceptable for v0.1 / session semantics.
- **Filigree enrich-only contract** — Verified clean. Discovery question #5 answered: no Filigree code path is load-bearing.

### Information Gaps

- The Sprint-2 e2e script `tests/e2e/sprint_2_mcp_surface.sh` was not read in this pass (out of scope per task framing); coverage of the seven tools end-to-end is documented as "presumed" in `01-discovery-findings.md:138`.
- The integration tests at `tests/storage_tools.rs` (1710 LOC) were enumerated but not read in full; the `RecordingProvider` plumbing and the `state_for_filigree` stub were sampled to confirm they exist (lines 244–252).
- ADR text for ADR-007/028/029/030 was not re-opened in this pass; alignment claims rely on the cache-key/confidence-tier code matching the documented intent paraphrased in the task brief.
- The model's per-edge `confidence: Option<f64>` field is parsed from LLM output but not surfaced in tool responses — whether this is intentional (ADR-028 tiers are coarse-grained on purpose) or a documentation gap was not verified against the ADR.

### Caveats

- The "thin dispatch" characterisation is true for the six read-only tools but does not apply to the LLM-dispatch paths (`tool_summary` plus the `confidence=inferred` branches of `callers_of` / `execution_paths_from` / `neighborhood`), which carry substantive in-crate logic: cache-key construction (ADR-007), budget reservation, in-flight coalescing, prompt construction via `clarion-core` helpers, provider invocation on `spawn_blocking`, JSON-shape validation, and writeback via the writer-actor.
- The LOC band offsets cited in the catalog were computed from the on-disk `lib.rs` and are stable against the working tree at the time of analysis; minor drift (the file grew by 171 lines during B.8 per `01-discovery-findings.md:323`) means line numbers in this section are post-B.8.
- "MCP protocol revision `2025-11-25`" is sourced from the in-code constant (`lib.rs:36`); whether that matches the upstream MCP spec revision identifier was not independently verified.
- The Filigree HTTP client uses `reqwest::blocking` despite living in an otherwise async crate — calls are wrapped in `tokio::task::spawn_blocking` at `lib.rs:613`. Not a defect, but worth flagging if the crate is ever migrated to an async Filigree client.
## clarion-cli

**Location:** `crates/clarion-cli/`

**Responsibility:** Glue binary for the `clarion` executable; `clap`-driven subcommand dispatch (`install`, `analyze`, `serve`) that wires `clarion-core` (plugin host, LLM provider), `clarion-storage` (writer-actor + reader-pool), and `clarion-mcp` (stdio server) into a single end-user tool, and converts the storage layer's `RunStatus` taxonomy into shell exit codes.

**Key Components:**

- `src/main.rs` (33 lines) — process entry. Loads `.env` via `dotenvy::dotenv()` from CWD or any ancestor *before* `init_tracing()` so a `.env`-supplied `RUST_LOG` is in effect by the time the `EnvFilter` is built (commit `dc9bf41`, `main.rs:16–17`). Parses `cli::Cli`, then dispatches: `Install` and `Serve` run synchronously, `Analyze` builds an ad-hoc multi-thread `tokio::runtime::Builder` and `block_on`s `analyze::run(path)` (`main.rs:21–26`). No top-level runtime — each subcommand owns its own concurrency story.

- `src/cli.rs` (43 lines) — `clap` derive structs only. `Install { --force, --path=. }`, `Analyze { path=. }`, `Serve { --path=., --config=Option<PathBuf> }`. `--force` is declared but documented in code as "not implemented in Sprint 1" (`cli.rs:17–18`).

- `src/install.rs` (168 lines) — `.clarion/` bootstrap. Refuses if the dir already exists (`install.rs:104–110`); refuses if `--force` is passed because Sprint-1 never implemented overwrite (`install.rs:87–92`). On `mkdir` success delegates to `populate_after_mkdir`, which (a) opens `clarion.db`, applies `clarion_storage::pragma::apply_write_pragmas` then `clarion_storage::schema::apply_migrations` (`install.rs:162–167`), (b) writes a stub `config.json` (`schema_version: 1, last_run_id: null`, `install.rs:22–26`), (c) writes a `.gitignore` carrying ADR-005's tracked-vs-excluded rules (`install.rs:54–77`), (d) writes a substantive `clarion.yaml` stub at project-root with LLM and Filigree-integration scaffolding (`install.rs:28–52`). Crucially, `clarion.yaml` is left untouched if it already exists (`install.rs:150–158`). Includes a cleanup guard: any failure inside `populate_after_mkdir` triggers `fs::remove_dir_all(.clarion)` before bubbling the error so the next attempt isn't blocked by the existence check (`install.rs:117–127`; references issue `clarion-ed5017139f`).

- `src/serve.rs` (136 lines) — MCP stdio server wiring, in this exact sequence (`serve.rs:14–91`): (1) assert `.clarion/clarion.db` exists or hint the operator to run `install` first (`serve.rs:15–21`); (2) canonicalise the project root; (3) read `clarion.yaml` via `McpConfig::from_path` or default to `McpConfig::default()` (`serve.rs:26–33`); (4) resolve provider selection via `select_provider_with_env` with a `std::env::var` closure (`serve.rs:34`); (5) build the `Arc<dyn LlmProvider>` via local `build_llm_provider` — `Disabled`/`Recording` (loads JSON fixture from `config.llm.recording_fixture_path` relative to project_root, `serve.rs:122–136`) / `OpenRouter` (passes `api_key`, `allow_live_provider: true`, model id, endpoint, and the attribution Referer/Title); (6) build the optional `FiligreeHttpClient::from_config` (`serve.rs:36–39`); (7) lock `stdin`/`stdout` and wrap stdin in `BufReader`; (8) build a **single-thread current-thread** tokio runtime, `runtime.enter()` as a guard so spawned tasks attach; (9) open the `ReaderPool` (size 16, `serve.rs:50`); (10) construct `ServerState::new(project_root, readers)`; (11) if a provider exists, spawn an LLM-only `Writer` against the same db_path with `DEFAULT_BATCH_SIZE`/`DEFAULT_CHANNEL_CAPACITY` and attach it via `state.with_summary_llm(writer.sender(), config.llm.clone(), provider)` (`serve.rs:55–65`); (12) if the Filigree client built, attach via `state.with_filigree_client(Arc::new(client))` (`serve.rs:66–68`); (13) hand off to `clarion_mcp::serve_stdio_with_state_on_runtime` (`serve.rs:70–72`); (14) drop `state`, drop `llm_writer` to close its sender, `runtime.block_on(handle)` to join the writer (`serve.rs:73–84`); (15) propagate `serve_result?` then the writer's `result?` so a clean MCP loop still fails the process if the writer errored.

- `src/analyze.rs` (1436 lines) — the orchestrator. Internal structure, in source order:

  - **Public entry `pub async fn run(project_path: PathBuf)` (`analyze.rs:40–542`)** — single ~500-line function flagged `#[allow(clippy::too_many_lines)]`. Phases (clearly demarcated by `── … ──` banner comments):
    1. Path validation + `.clarion/` existence check (`:41–56`).
    2. **Writer actor lifecycle — open**: `Writer::spawn(db_path, DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY)` returns a `(writer, handle)` pair; mints a `Uuid::new_v4()` run id and `BeginRun` via `writer.send_wait(...)` (`:60–75`).
    3. **Plugin discovery** via `clarion_core::discover()`; successes pushed into `plugins`, failures collected into `discovery_errors` (`:78–97`).
    4. **No-plugins branch** (`:99–171`): distinguishes "zero discovered + zero errors" (`SkippedNoPlugins` via `CommitRun`, exits 0 with a "skipped_no_plugins" stdout line) from "zero usable + non-empty errors" (`FailRun` with the concatenated error list, then `bail!` for non-zero exit). The fix for hiding manifest-parse bugs as `SkippedNoPlugins` is explicit in the comment at `:100–103`.
    5. **Extension union + source walk** — collect_source_files walks once over the union of every plugin's declared extensions (`:174–184`).
    6. **Per-plugin loop** (`'plugins:` label, `:211–412`): filter the global file list to this plugin's extensions; if empty, `continue`. Otherwise dispatch a `tokio::task::spawn_blocking(move || run_plugin_blocking(...))` and route its `JoinResult` through `handle_plugin_task_join_result` (which normalises a `JoinError` panic into a crash-reason string rather than `?`-propagating, `:259–271`, regression-tested as `clarion-cf17e4e779`). On `Err(reason)`: push into `crash_reasons`, tick `CrashLoopBreaker::record_crash`, and `break 'plugins` if `CrashLoopState::Tripped` (`:274–292`). On `Ok(BatchResult { entities, edges, unresolved_call_sites, stats, findings })`: fold per-batch stats into per-run accumulators; log every `HostFinding` individually (`:312–327`); flush entities then unresolved call-site replacements (`WriterCmd::ReplaceUnresolvedCallSitesForCaller`) then edges to the writer-actor in that exact order (`:341–391`). Any writer `send_wait` error stops the per-plugin loop and sets `run_outcome = HardFailed { reason }` (`:392–402`).
    7. **`RunOutcome` resolution** (`:421–522`) — `enum RunOutcome { Completed, SoftFailed { reason }, HardFailed { reason } }` at `:558–563`. Promotion rule: `Completed` + non-empty `crash_reasons` → `SoftFailed` so the entity batch still commits *and* the run row marks failed (`:421–429`). Snapshots `writer.dropped_edges_total` and `writer.ambiguous_edges_total` (atomic counters held on the `Writer` handle) and `pyright_latency.p95_ms()` into the stats JSON (`:435–441`). The three terminal branches dispatch:
       - `Completed` → `WriterCmd::CommitRun { status: RunStatus::Completed, … }`
       - `SoftFailed { reason }` → `WriterCmd::CommitRun { status: RunStatus::Failed, … }` plus `"failure_reason": reason` in the stats JSON; the writer folds `UPDATE runs SET status='failed'` into the open entity tx so the partial work commits atomically with the failure marker (`:478–509`, comment at `:551–553`).
       - `HardFailed { reason }` → `WriterCmd::FailRun` — rolls back the open tx (`:510–521`).
    8. **Writer-actor lifecycle — close**: `drop(writer)` closes the command channel; `handle.await` joins the actor task (`:524–528`); on any `fail_reason`, `bail!` for non-zero exit — the run row is already correctly marked, this is purely about surfacing failure to the shell (`:530–534`).

  - **`run_plugin_blocking` (`:646–759`)** — synchronous worker that `spawn_blocking` runs. Spawns the plugin via `PluginHost::spawn`; loops `host.analyze_file(file)` for each file in the per-plugin extension-filtered list; accumulates entities, edges, per-file `AnalyzeFileStats`, and per-file unresolved call sites; on the happy path tries `host.shutdown()` and falls back to `child.kill()` if shutdown writes to a closed pipe (`:731–742`); always `reap_and_classify_exit(&mut child, ...)` afterwards because `std::process::Child::Drop` does not `wait()` on Unix and would leak zombies (`:746–747`, comment at `:641–645`).

  - **`reap_and_classify_exit` (`:769–815`)** — on `signal() == SIGKILL (9)` or `SIGSEGV (11)` appends `HostFinding::oom_killed(plugin_id, signal)` per ADR-021 §2d; other signals or non-zero exits get a `warn` log but no finding.

  - **`classify_host_error` (`:818–843`)** — the explicit `HostError` → `String` mapping for fail-run reasons. Match arms: `EntityCapExceeded(_)` ("exceeded entity-count cap"), `PathEscapeBreakerTripped` ("tripped path-escape breaker"), `Spawn(msg)`, `Handshake(me)`, `Transport(te)`, `Protocol(pe)` (formats `code` + `message`), wildcard `other`.

  - **Entity/edge mapping** (`:846–946`) — `map_entity_to_record` derives `short_name` by rsplit('.'), serialises `entity.raw.extra` into `properties_json`, computes `content_hash` via local `content_hash_for_entity` (BLAKE3 over full file bytes for `module` kind, over normalised joined source lines `[start_line-1..end_line]` otherwise — `:907–927`). `map_edge_to_record` is a near-direct field-copy. Worth noting: `source_byte_start`/`source_byte_end` are hard-coded to `None` on entities — only the line range is captured. Edges keep byte ranges.

  - **Unresolved call-site bookkeeping (B.4*)** (`:948–1055`) — `map_unresolved_call_sites_for_file` groups sites by caller; checks "authoritative" mode (where `stats.unresolved_call_sites_total == stats.unresolved_call_sites.len()`) and pre-creates empty `PendingUnresolvedCallSites` for every function entity in the batch so the writer's `ReplaceUnresolvedCallSitesForCaller` can clear stale rows for callers that no longer have any unresolved sites. `validate_unresolved_call_site` enforces non-negative ordinal, non-empty/`<=512`-byte callee_expr, monotone byte range. `unresolved_call_site_key` is a BLAKE3 of `caller_entity_id || start_be || end_be || callee_expr`.

  - **Source-tree walk** (`:1063–1170`) — `walk_dir` is hand-rolled `std::fs::read_dir` recursion (no `walkdir` dep), with `SKIP_DIRS = [".clarion",".git",".hg",".svn",".jj",".venv","__pycache__","node_modules"]`, symlink skipping, and per-entry I/O error counting (skipped entries are tallied and surfaced as one summary `warn` line at end of walk to avoid silent partial analysis). **Does not honour `.gitignore`** — flagged P4 at `:1078`.

  - **Time helpers** (`:1180–1220`) — hand-rolled `iso8601_now()` using Howard Hinnant's `civil_from_days`, to avoid bringing in `chrono` for one format string.

- `src/stats.rs` (37 lines) — `pub(crate) struct P95Accumulator { samples_ms: Vec<u64> }` with `record_many` and a nearest-rank `p95_ms()` (`stats.rs:21`). Sole client is the pyright-query-latency rollup in `analyze.rs`.

**Integration tests** (`crates/clarion-cli/tests/`, 1217 LOC total):
- `install.rs` (247 lines) — black-box `assert_cmd` cases covering `.clarion/` contents, `.gitignore` rule presence, refusal-on-existing, `--force` refusal.
- `analyze.rs` (389 lines) — black-box analyze coverage.
- `serve.rs` (213 lines) — runs `clarion serve` as a child process, frames JSON-RPC bodies via `clarion_core::plugin::{Frame, read_frame, write_frame}` (re-exported through the `plugin` facade for test consumers), exercises the MCP `initialize` round-trip and `summary` tool dispatch (it imports `LEAF_SUMMARY_PROMPT_TEMPLATE_ID`).
- `wp1_e2e.rs` (74 lines) — WP1 walking-skeleton; mostly install/analyze sanity.
- `wp2_e2e.rs` (494 lines) — WP2 walking-skeleton consuming the on-disk `clarion-plugin-fixture` binary (declared as a `[dev-dependencies]` workspace member in `Cargo.toml:33`).

**Dependencies:**

- Inbound:
  - End-users / shell / CI (the binary).
  - `tests/e2e/sprint_1_walking_skeleton.sh` and `tests/e2e/sprint_2_mcp_surface.sh` invoke it as a subprocess.
  - `tests/perf/b8_scale_test/driver.py` (B.8 scale-test harness) invokes it as a subprocess.
  - No Rust-level inbound deps — this is a `[[bin]]`-only crate and exposes no library API (`Cargo.toml:12–14`).

- Outbound:
  - `clarion-core` — `PluginHost`, `discover`, `Manifest`, `DiscoveredPlugin`, `AcceptedEntity`/`AcceptedEdge`, `AnalyzeFileOutcome`, `AnalyzeFileStats`, `UnresolvedCallSite`, `CrashLoopBreaker`/`CrashLoopState`, `HostError`, `HostFinding`, `FINDING_DISABLED_CRASH_LOOP`, `LlmProvider`, `OpenRouterProvider`/`OpenRouterProviderConfig`, `Recording`/`RecordingProvider` (used in `analyze.rs:19–23`, `serve.rs:7–9`).
  - `clarion-storage` — `Writer`, `ReaderPool`, `WriterCmd`, `EntityRecord`, `EdgeRecord`, `RunStatus`, `UnresolvedCallSiteRecord`, `DEFAULT_BATCH_SIZE`/`DEFAULT_CHANNEL_CAPACITY`, `pragma`, `schema` (used in `analyze.rs:24–27`, `install.rs:20`, `serve.rs:12`).
  - `clarion-mcp` — `ServerState`, `config::{McpConfig, ProviderSelection, select_provider_with_env}`, `filigree::FiligreeHttpClient`, `serve_stdio_with_state_on_runtime` (used in `serve.rs:10–11, 52, 71`).
  - Third-party: `clap` (derive CLI), `tokio` (runtime + `spawn_blocking` + `block_on`), `anyhow` (error type), `tracing` + `tracing-subscriber` (with `EnvFilter`), `dotenvy` (env loading), `uuid` (run IDs), `blake3` (content hashes + unresolved-site keys), `rusqlite` (install-time direct connection only — analyze and serve go through `Writer`/`ReaderPool`), `serde_json`.

**Patterns Observed:**

- **Pattern A buffering** (documented at `analyze.rs:7`): plugin work runs synchronously inside `spawn_blocking`, returns a `BatchResult` to the async caller, and only then does the caller emit `WriterCmd::Insert{Entity,Edge}` over the writer-actor channel. The blocking task never touches the writer directly.
- **Tri-state run outcome** (`Completed` / `SoftFailed` / `HardFailed`): explicitly distinguishes "plugin crashed but other plugins' entities should still persist" (SoftFailed → `CommitRun(Failed)`) from "writer-actor itself is broken" (HardFailed → `FailRun`). Comment at `:543–556` is load-bearing — the chosen WriterCmd differs by branch.
- **Atomic claim+transition style errors**: `JoinError` is *not* `?`-propagated; it's intercepted by `handle_plugin_task_join_result` and reshaped into a crash reason so the run-row resolution machinery still fires. The bypass regression is named in code (`clarion-cf17e4e779`) at `:1230–1236`.
- **Best-effort cleanup with auditable fallback**: `install.rs` removes a partial `.clarion/` on bootstrap failure and logs (not bails) if cleanup itself fails; `analyze.rs::run_plugin_blocking` always reaps the child even on the kill path.
- **Single-thread current-thread runtime for stdio loops, multi-thread for analyze**: `serve.rs:45–48` deliberately uses `Builder::new_current_thread()` (the stdin loop is the only thing actually running); `main.rs:22–25` uses `new_multi_thread` for analyze because `spawn_blocking` needs a worker thread.
- **Direct schema apply in install, writer-actor everywhere else**: `install.rs` opens its own `rusqlite::Connection` to run pragmas + migrations (the writer-actor doesn't exist yet at install time); after that, every write goes through `WriterCmd::*`.
- **Stats JSON is structured but stringified**: `CommitRun.stats_json` is built via `serde_json::json!` and then `.to_string()` (`analyze.rs:452–465`), matching `clarion-storage`'s `String` field signature.

**Concerns:**

- **`analyze::run` is a single ~500-line `async fn` with `#[allow(clippy::too_many_lines)]` at `:39`.** It mixes plugin discovery, file walking, per-plugin orchestration, writer-actor lifecycle, crash-loop policy, and three-way outcome resolution. The phase banners (`── Writer actor ──`, `── Discover plugins ──`, etc.) prove the author recognised the seams; extracting at least the per-plugin loop and the outcome-resolution match into named helpers would reduce the cognitive cost and make adding a fourth `RunOutcome` variant safer.
- **`source_byte_start` / `source_byte_end` are hard-coded `None` on entity records** (`analyze.rs:873–874`). The schema columns exist (`EntityRecord` carries them) and edges do populate them. Either the plugin should emit byte offsets and the CLI should plumb them, or the entity columns are documented dead weight.
- **Hand-rolled date math** (`civil_from_unix_secs`, `:1197–1220`) was justified for Sprint 1 by avoiding `chrono`. With `dotenvy`, `blake3`, `uuid`, and `tracing-subscriber` already in tree, the "we don't have a date dep" rationale is thinner now; the comment at `:1175–1179` itself anticipates promoting `chrono` "at that point." Worth a follow-up issue.
- **The source walk does not honour `.gitignore`** (P4 noted in-code at `:1078`). On the `elspeth` corpus this likely means walking generated and vendored code paths the operator considers out-of-scope. `SKIP_DIRS` is a coarse stopgap.
- **`install.rs::initialise_db` opens a connection, applies pragmas + migrations, and lets it drop without an explicit close.** SQLite handles this safely (the dtor closes the handle and flushes WAL), but the symmetry with the writer-actor's explicit `Drop` and the explicit ordering elsewhere makes this stand out. Not a defect; a stylistic gap.
- **Serve's LLM writer is independent from the analyze writer.** When `clarion serve` is up *and* `clarion analyze` is run in another process, two `Writer` actors hold connections to the same `clarion.db`. The pragma layer (WAL + `busy_timeout`) is what keeps that safe — this is a `clarion-storage` invariant the CLI relies on but doesn't enforce or test in the cli-tests. Worth verifying in the storage subsystem brief.
- **`--force` accepted but unimplemented.** `install.rs:87–92` rejects the flag with a `bail!`. Sprint-2+ has not implemented the overwrite path despite the `cli.rs:17` doc-comment hinting at one. If the operator workflow has stabilised, either implement it or remove the flag.
- **No integration test exercises the `SoftFailed` path end-to-end.** `analyze.rs` tests cover `Completed` and `SkippedNoPlugins`; the soft-fail branch (where one plugin crashes but another succeeds) is the most subtle path — it's the one where an entity batch and a `status='failed'` UPDATE share a single SQLite transaction — and no `tests/analyze.rs` case (per a `grep` of the test file's signatures, not deep-read this pass) targets it. The crash-loop unit tests inside `analyze.rs:tests` cover `handle_plugin_task_join_result` but not the writer side of the soft-fail folding.

**Confidence:** High — read `main.rs`, `cli.rs`, `install.rs`, `serve.rs`, `stats.rs`, and `Cargo.toml` in full; read `analyze.rs:1–542` (the full async `run`), `:559–815` (RunOutcome, JoinError helper, BatchResult/BatchStats/PendingUnresolvedCallSites structs, run_plugin_blocking, reap_and_classify_exit), `:818–843` (classify_host_error), `:846–946` (entity/edge mapping + content hashing), `:948–1055` (unresolved-call-site mapping), `:1057–1170` (source walk), and `:1180–1220` (time helpers). Cross-validated outbound deps by reading the import block at `analyze.rs:19–27`, `serve.rs:7–12`, `install.rs:20`; confirmed against `Cargo.toml:17–29`. Sample of test headers read for `tests/install.rs` and `tests/serve.rs`. Phase-level walkthrough of `analyze::run` annotated with line ranges throughout. The five open questions in the brief are answered in the text above with file:line citations.

**Risk Assessment:**

- **God-function risk on `analyze::run`** (high likelihood, medium impact). Length is already a documented smell. Mitigation: extract the per-plugin loop and outcome resolution; add `SoftFailed` integration coverage before extraction so the refactor has a regression net.
- **Stale CLI surface risk** (low likelihood, low impact). `--force` advertised but unimplemented; could mislead a CI author. Mitigation: implement or remove.
- **Multi-process writer correctness** (low likelihood, high impact). Two `Writer` actors against one DB rely on `clarion-storage` pragma discipline (WAL + `busy_timeout`). The CLI is the only place this combination is wired in production. Mitigation: a serve+analyze concurrent integration test would prove the invariant from the CLI side; right now the discipline is owned by `clarion-storage` and trusted by the CLI.

**Information Gaps:**

- `tests/analyze.rs` (389 lines) was not deep-read this pass; the claim that no test exercises the `SoftFailed` path is based on the test file's signature surface and the comments in `analyze.rs`, not on a function-by-function read. A follow-up could promote that claim from "likely" to "verified" or refute it.
- `select_provider_with_env`'s exact precedence (env-var override of YAML vs YAML wins) is `clarion-mcp::config`'s concern, not visible from CLI source. The CLI passes a `|name| std::env::var(name).ok()` closure (`serve.rs:34`) and trusts the resolver.
- Whether the LLM `Writer` spawned in `serve.rs` and the `analyze` `Writer` would conflict on the same `clarion.db` was not verified end-to-end — only that pragma discipline in `clarion-storage` is intended to absorb it.

**Caveats:**

- Line counts include `#[cfg(test)]` blocks where present in `src/`. `analyze.rs:1224+` contains an in-source `mod tests` whose contents were not deep-read beyond the `handle_plugin_task_join_result` regression comment.
- The `clarion.yaml` stub written by `install` (`install.rs:28–52`) commits the CLI to a specific config shape (model id `anthropic/claude-sonnet-4.6`, default Filigree port `8766`, default OpenRouter endpoint). Those defaults belong to the operator config story, not the CLI's responsibility surface — flagged here only as a fact, not a critique.
- No code-quality assessment is made — that is an `axiom-system-architect:assess-architecture` concern.
## Test-only Rust fixture plugin (`clarion-plugin-fixture`)

**Location:** `crates/clarion-plugin-fixture/src/`

**Responsibility:** Protocol-compatible stand-in for a real language plugin: a minimal Rust binary speaking the same Content-Length-framed JSON-RPC 2.0 protocol on stdin/stdout as the Python plugin, used by `clarion-core`'s `host_subprocess` integration test to exercise `PluginHost::spawn` end-to-end without bringing a Python interpreter and pyright into the test loop.

**Key Components:**

- `Cargo.toml` (19 lines) — declares a single `[[bin]]` target (`clarion-plugin-fixture`, `src/main.rs`); depends on `clarion-core` (path dep, version `0.1.0-dev`) and `serde_json` from the workspace; inherits workspace `[lints]`. No library is published.
- `src/main.rs` (128 lines, full code) — the entire plugin. One blocking `loop` over `read_frame(&mut reader, ContentLengthCeiling::DEFAULT)` (`main.rs:33`); per-frame `serde_json::from_slice` to a free-form `Value` so it can branch on `id`-presence (notification vs. request) before typed deserialisation (`main.rs:37-46`). Five method branches matching the L4 protocol surface:
  - `initialize` (request) → `InitializeResult { name: "clarion-plugin-fixture", version: "0.1.0", ontology_version: "0.1.0", capabilities: {} }` (`main.rs:68-76`).
  - `initialized` (notification) → state transition only, no reply (`main.rs:50-53`).
  - `analyze_file` (request) → extracts `params.file_path` (or `""`), echoes it back inside one stub entity `{"id": "fixture:widget:demo.sample", "kind": "widget", "qualified_name": "demo.sample", "source": {"file_path": <echoed>}}`, returns `AnalyzeFileResult { entities: vec![entity], edges: vec![], stats: default }` (`main.rs:77-108`).
  - `shutdown` (request) → empty `ShutdownResult` (`main.rs:109-112`).
  - `exit` (notification) → `std::process::exit(0)` (`main.rs:54-56`).
- `src/lib.rs` (3 lines) — comment-only stub explaining the crate is binary-only; exists so Cargo resolves the workspace member cleanly.

**Dependencies:**

- Inbound: `crates/clarion-core/tests/host_subprocess.rs` is the sole consumer — it locates the binary via `CARGO_BIN_EXE_clarion-plugin-fixture`, falling back to `<target_dir>/{debug,release}/clarion-plugin-fixture`; the manifest `tests/fixtures/plugin.toml` is `include_bytes!`-embedded at compile time (`host_subprocess.rs:16`). CI's `walking-skeleton` job builds this binary as part of `cargo build --workspace --bins` so the test can find it on disk (see `CLAUDE.md` build-commands section: "wp2_e2e tests need clarion-plugin-fixture on disk").
- Outbound: `clarion-core::plugin::limits::ContentLengthCeiling` (the 8 MiB default), `clarion-core::plugin::transport::{Frame, read_frame, write_frame}` (the shared framing codec), `clarion-core::plugin::{AnalyzeFileParams, AnalyzeFileResult, AnalyzeFileStats, InitializeResult, JsonRpcVersion, ResponseEnvelope, ResponsePayload, ShutdownResult}` (the typed protocol structs); `serde_json` for the free-form `Value` pre-dispatch.

**Patterns Observed:**

- **Protocol-by-shared-types.** The fixture reuses `clarion-core`'s own protocol structs (`InitializeResult`, `AnalyzeFileResult`, `ResponseEnvelope`, …) for response serialisation — there is no parallel schema definition. A breaking change to `protocol.rs` therefore fails compilation of the fixture, not at runtime under test, which is the right ordering.
- **Same ceiling as production.** Frame reads use `ContentLengthCeiling::DEFAULT` (the ADR-021 §2b 8 MiB cap), with the source comment explicitly noting that `unbounded()` is now `#[cfg(test)]`-only (`main.rs:30-32`). The fixture lives under the same wire-cap discipline as a real plugin.
- **Fail-fast on protocol violations.** Every recoverable branch in a real plugin is `std::process::exit(1)` here — malformed frame, non-object body, missing/non-string `method`, integer-id parse failure, unknown method, params-deserialise failure (`main.rs:34, 39, 45, 57, 64, 90, 113`). Acceptable because the consumer is exclusively an integration test; the alternative would obscure protocol-violation bugs behind fixture-side error handling.
- **Notification vs. request branching on `id`-presence.** Reads the raw `Value` first, checks `id.is_some_and(|v| !v.is_null())` to decide whether the frame requires a response (`main.rs:42, 48-60`). This matches the JSON-RPC 2.0 spec and parallels the Python plugin's branching in `server.dispatch` (`server.py:239-261`).
- **Stable identity for assertions.** `plugin_id = "fixture"`, kind `"widget"`, and the literal entity ID `"fixture:widget:demo.sample"` are baked into the source — `host_subprocess.rs` asserts on this exact string, so the test signal is exact-match rather than parse-and-inspect.

**Concerns:**

- **No request-id sanity on `shutdown`.** Unlike the Python plugin, the fixture doesn't gate `analyze_file` on having received `initialized` — `state.initialized` doesn't exist. This is fine for the single happy-path test it supports, but means the fixture cannot exercise the host's `-32002 NOT_INITIALIZED` error path. If a future test wanted to assert that the host *itself* sequences the handshake correctly, it would have to verify host-side state rather than fixture-side rejection.
- **`exit(1)` on any malformed frame is observable only as a non-zero process exit.** The host-side test gets no structured signal about which branch failed. For an integration test fixture this is by design; flagging because anyone running the fixture by hand against a non-test client will see opaque exits.
- **No stderr discipline.** A real plugin (Python's `stdout_guard.py`) reserves stdout strictly for framing; the fixture relies on the absence of any `eprintln!` or `println!` in its own code rather than installing a guard. For a 128-line file with `serde_json` as the only output-side dep this is fine, but worth noting as a delta from the production-plugin pattern.

**Confidence:** High — Read `main.rs` (128 lines, 100% of file), `lib.rs` (3 lines, 100%), `Cargo.toml` (19 lines, 100%); cross-verified consumer via `crates/clarion-core/tests/host_subprocess.rs` lines 3-7, 15-27, and 60-66 (binary-location strategy, fixture identity assertions, manifest constants). Cross-validated against `docs/arch-analysis-2026-05-18-1244/01-discovery-findings.md` §4 Subsystem E framing and `CLAUDE.md` layout summary. Protocol identity confirmed by the matching set of imports from `clarion_core::plugin::*` against the Python plugin's `server.py:7-19` docstring describing the same five methods and response shapes. Content-Length framing parity confirmed via the explicit `ContentLengthCeiling::DEFAULT` (8 MiB) source comment matching the Python `MAX_CONTENT_LENGTH = 8 * 1024 * 1024` at `server.py:48`.

**Information Gaps:**

- Did not read the upstream `clarion_core::plugin::transport` module to verify exactly how `read_frame` / `write_frame` interpret the ceiling; took the source comment at face value.
- Did not run `cargo build -p clarion-plugin-fixture` on the current branch to confirm the binary still compiles. Treated the unmodified `Cargo.toml` and the recent (b87bc1d) signoff record as sufficient evidence that the walking-skeleton CI job was green at sprint close.

**Caveats:**

- "Protocol-compat" here means *exact wire-shape compatibility* on the five L4 methods. The fixture does not exercise the `capabilities.wardline` probe shape, `parse_status` on module entities, `parent_id`/`contains` edges, calls/references resolution, the `stats` payload's `unresolved_call_sites`, or any of the Sprint-2 ontology surface. It is a *minimum*-shape test stand-in, not a feature-parity one.
- The fixture's `ontology_version = "0.1.0"` (`main.rs:72`) is deliberately the Sprint-1 baseline; this is the version against which the host's manifest-handshake validator is tested. It does *not* track the Python plugin's `0.5.0` and shouldn't.

**Risk Assessment:**

- *Drift between fixture and real plugins.* The fixture has been stable since Sprint 1 close and the protocol contract is enforced by shared `clarion-core` types, so the drift surface is bounded to behavioural-not-structural divergence (e.g. a real plugin adding handshake side-effects the fixture doesn't model). The host-side test exercises only the structural surface, so this is a known-acceptable gap.
- *Single-consumer dependency.* The fixture exists exclusively for `host_subprocess.rs`. If that test were retired, the fixture would become dead code; conversely, the test cannot be expanded to cover behaviours the fixture doesn't model without growing the fixture. Pre-existing carryover issue `clarion-adeff0916d` (fixture-binary self-build) tracks one known sharp edge here.
- *Build-ordering coupling.* The walking-skeleton CI job depends on `cargo build --workspace --bins` running before `cargo nextest run` so the binary is on disk when `host_subprocess.rs` looks for it. This is documented in `CLAUDE.md` and codified in `.github/workflows/ci.yml`'s `walking-skeleton` job, but is an implicit dependency that would break if a future contributor used `cargo nextest run --workspace` without the prior `cargo build`.
## Python language plugin (`plugins/python`)

**Location:** `plugins/python/src/clarion_plugin_python/`

**Responsibility:** Out-of-process language plugin that ingests a single Python source file at a time, extracts module/class/function entities plus `contains`/`calls`/`references` edges, and serves them to the Rust core over a Content-Length-framed JSON-RPC 2.0 channel on stdin/stdout (the L4 protocol).

**Key Components:**

- `__main__.py` (15 lines) — installs the stdout discipline guard, then delegates to `server.main()`; threads the server's exit code out to the host (`__main__.py:14`).
- `server.py` (285 lines) — L4 JSON-RPC dispatch loop. Implements the five protocol methods exactly as the Rust host's typed `protocol.rs` expects: `initialize`/`initialized`/`analyze_file`/`shutdown`/`exit` (`server.py:226-261`). Owns `ServerState` (initialized flag, shutdown flag, captured `project_root`, lazy `PyrightSession`) and the `read_frame`/`write_frame` Content-Length codec with an 8 MiB symmetric cap matching ADR-021 §2b (`server.py:48`, `71-126`). `handle_initialize` captures the host-supplied `project_root` and embeds the Wardline probe result in `capabilities.wardline` (`server.py:141-153`). `handle_analyze_file` reads the file off disk, lazily constructs the `PyrightSession`, and calls `extractor.extract_with_stats(...)` (`server.py:177-221`).
- `extractor.py` (744 lines, +98 on this branch for B.8) — AST → wire-shape extractor. `extract_with_stats` parses the source with `ast.parse`, prepends exactly one `module` entity (B.2 §3 Q1), then recursively walks via `_walk` to emit one `function` per `FunctionDef`/`AsyncFunctionDef` and one `class` per `ClassDef` (`extractor.py:261-344`, `_walk` at `589-668`). `parent_id` and one `contains` edge per non-module entity satisfy ADR-026 decision 2's dual encoding (`extractor.py:107-117`, `671-677`). `_ReferenceSiteCollector` is the separate `ast.NodeVisitor` pass for B.5* reference sites (`extractor.py:358-485`), then `extract_with_stats` hands the function IDs to `call_resolver.resolve_calls` and the reference sites to `reference_resolver.resolve_references` (`extractor.py:338-342`). Same-id collisions are handled at the emit boundary: `_has_overload_decorator` recognises `@overload` / `@typing.overload` / `@typing_extensions.overload` and skips emission *and* recursion entirely (`extractor.py:567-586`, `624`); any other duplicate (aliased overload imports, `@singledispatch.register def _():` runs) is dropped first-wins with a stderr line and a `duplicate_entities_dropped_total` bump (`extractor.py:629-637`).
- `pyright_session.py` (1251 lines) — long-running `pyright-langserver --stdio` LSP client. See sub-section below.
- `call_resolver.py` (64 lines) — `CallResolver` `Protocol` plus `CallsRawEdge` / `UnresolvedCallSite` / `Finding` TypedDicts; `NoOpCallResolver` is the test stand-in (`call_resolver.py:49-64`). `PyrightSession` is the production implementation.
- `reference_resolver.py` (69 lines) — symmetric: `ReferenceResolver` `Protocol`, `ReferenceSite` dataclass, `ReferencesRawEdge` TypedDict, `NoOpReferenceResolver` (`reference_resolver.py:54-69`).
- `entity_id.py` (75 lines) — Python side of the L2 byte-for-byte ADR-003+ADR-022 entity-ID assembler. Validates `plugin_id` / `kind` against the grammar `[a-z][a-z0-9_]*`, refuses the `:` separator inside any segment, raises typed `EmptySegmentError` / `GrammarViolationError` / `SegmentContainsColonError`. Cross-validated against `fixtures/entity_id.json` row-by-row (`entity_id.py:66-75`).
- `qualname.py` (46 lines) — ADR-018 L7 canonical qualname. Pure-AST reconstruction of CPython's runtime `__qualname__`: walks the parent chain in reverse, prepending `parent.<locals>.` for function ancestors and `parent.` for class ancestors (`qualname.py:32-46`). Lock-in: this string must equal what Wardline produces for the same definition, otherwise the cross-product join breaks.
- `wardline_probe.py` (56 lines) — L8 fail-soft Wardline probe. `importlib.import_module("wardline.core.registry")` plus `importlib.import_module("wardline")`, then a `packaging.version` half-open range check against the manifest's `[integrations.wardline].min_version` / `max_version` (`wardline_probe.py:36-56`). Returns one of three dicts: `{"status": "absent"}`, `{"status": "enabled", "version": ...}`, `{"status": "version_out_of_range", "version": ...}`. **Invoked once per session at `initialize`** (`server.py:151`), never per-file. The `wardline.core.registry` import is the named Loom-doctrine asterisk from `docs/suite/loom.md` §5; Sprint 1 only proves the import + version-pin handshake — REGISTRY is not yet consumed.
- `stdout_guard.py` (62 lines) — replaces `sys.stdout` with a `_GuardedTextStdout` that raises `StdoutGuardError` on any write; captures the real `stdin.buffer` / `stdout.buffer` byte streams for the framing codec to use (`stdout_guard.py:57-62`). Single-shot, called by `__main__` before the dispatch loop starts.
- `__init__.py` (3 lines) — `__version__ = "0.1.4"`.

**Sub-section: `pyright_session.py` (1251 lines, ~17% of total plugin LOC)**

This file is the entire pyright integration surface. Internal structure:

- *Public class `PyrightSession`* (`:117-758`) — implements both the `CallResolver` and `ReferenceResolver` Protocols. Constructed lazily once per `analyze_file` session by `server.handle_analyze_file` and held on `ServerState.pyright` for the lifetime of the connection (`server.py:193-194`, closed in `shutdown` handler at `server.py:246-248`). Public surface: `__init__`/`__enter__`/`__exit__`/`close`/`resolve_calls`/`resolve_references`/`kill_for_test`/`stderr_thread_alive`. Constructor knobs (`init_timeout_secs=30`, `call_timeout_secs=5`, `max_restarts_per_run=3`, `max_reference_sites_per_file=2000`) are exposed for tests (`pyright_session.py:118-145`).
- *Process lifecycle* — `_ensure_process` (`:505-516`) lazily spawns; `_start_process` (`:536-599`) does `subprocess.Popen([pyright-langserver, --stdio], cwd=project_root, env=..., stdin/stdout/stderr=PIPE)` and immediately calls `_initialize` with the LSP `initialize` request (`:601-616`). `_resolve_executable` (`:618-625`) walks: absolute-path → `sys.executable`'s sibling directory (i.e. the active venv) → `shutil.which`. A stderr-drain thread (`_start_stderr_drain` `:634-640`, `_drain_stderr` `:642-649`) keeps the 64 KiB `_stderr_tail` ring populated for diagnostics; the thread is daemonised.
- *Restart / poison handling* — `_record_restart_or_poison` (`:518-534`) increments `_restart_count` and emits a `CLA-PY-PYRIGHT-RESTART` finding; after 3 restarts the session goes `_disabled = True` and emits one `CLA-PY-PYRIGHT-POISON-FRAME`. Five fail-soft `CLA-PY-PYRIGHT-*` finding subcodes are defined at the top of the file (`:34-41`).
- *LSP transport* — `_request` (`:651-670`) writes Content-Length-framed JSON and busy-loops on `_read_message` skipping mismatched-id frames; `_notify` (`:672-674`) is the no-response variant; `_read_message` (`:693-714`) reads headers + body using `_read_line`/`_read_exact`/`_wait_readable` helpers (`:1218-1247`) that enforce a per-call deadline via `select.select` on the pipe fd.
- *Call resolution* (`resolve_calls` + `_resolve_with_pyright` `:181-380`) — opens the file via `textDocument/didOpen`, issues `textDocument/prepareCallHierarchy` per function entity, then `callHierarchy/outgoingCalls` per returned item. Edges are grouped by source byte range; multi-target ranges produce one `ambiguous` edge with the candidate list in `properties.candidates` (per ADR-028 confidence tiers, `:359-369`). Two AST-side enrichers — `_ambiguous_dict_dispatches` and `_dunder_call_dispatches` (`:1003-1145`) — fold dict-of-callables and `__call__`-on-instance patterns that pyright doesn't track natively into the same `grouped` map. Always followed by `textDocument/didClose` in `finally:` (`:380`).
- *Reference resolution* (`resolve_references` + `_resolve_references_with_pyright` `:228-453`) — hard cap of 2 000 sites per file (emits `CLA-PY-PY-REFERENCE-SITE-CAP` and returns early, `:238-250`); per-site `textDocument/references` queries with annotation-fallback retry (`:411-422`); deduplicates by `(from_id, to_id)` accumulator and finalises with `_reference_accumulator_to_edge` (`:922-937`). All exceptions are caught at the boundary and converted to fail-soft results.
- *AST function-indexing helpers* (`_build_function_index`, `_collect_entities`, `_CallSiteVisitor`, `_DictDispatchVisitor`, `_DunderCallDispatchVisitor` `:760-1145`) — a parallel AST pass independent from `extractor.py`'s walker; necessary because `PyrightSession` needs LSP positions (line/character) for every function and class plus the per-function call-site index, neither of which the wire shape carries.

**Dependencies:**

- Inbound: Rust core's plugin host (`crates/clarion-core/src/plugin/host.rs`) spawns this plugin via the `clarion-plugin-python` console script; the host's typed `protocol.rs` (`InitializeResult`, `AnalyzeFileResult`, `AnalyzeFileStats`, `ShutdownResult`) is the wire contract; the host's writer-actor (`clarion-storage`) consumes the emitted entities and edges; the `walking-skeleton` CI job invokes the full pipeline.
- Outbound: `pyright==1.1.409` (LSP server subprocess, pinned in both `pyproject.toml:20` and `plugin.toml:29`); `packaging>=24` (version-range parsing in the Wardline probe); Python stdlib only otherwise (`ast`, `json`, `subprocess`, `select`, `threading`, `importlib`, `pathlib`, `urllib.parse`). `wardline` is a **soft** outbound dependency — imported only to probe at `initialize`; absence is not an error. The doctrine asterisk noted in `docs/suite/loom.md` §5 is real: `wardline.core.registry` is imported by name (`wardline_probe.py:38`), and the manifest pins `[integrations.wardline] min_version=1.0.0 max_version=2.0.0` (`plugin.toml:48-55`).

**Patterns Observed:**

- **Protocol-typed wire boundary.** Every method handler returns a TypedDict whose shape mirrors the Rust host's serde structs exactly (`server.py:7-19` docstring enumerates this). The five JSON-RPC error codes used (`-32600`, `-32601`, `-32603`, `-32002`) are LSP-style (`server.py:51-54`). Out-of-spec frames raise `ProtocolError`, which propagates out of the loop and exits with status 1 (`server.py:284-285`).
- **Fail-soft pyright integration.** Every external failure mode of `pyright-langserver` (not installed, install-check rejection, init timeout, runtime timeout, transport-closed, broken pipe, OSError) is caught at the `resolve_calls` / `resolve_references` boundary, downgraded to a `CLA-PY-PYRIGHT-*` finding, and returned as "unresolved" counts in the result — never raised back into the dispatch loop (`pyright_session.py:202-217`, `265-280`). The 3-restart cap then disables the session entirely for the run.
- **Two-pass AST.** The extractor pass (`extractor.py`) produces wire entities + structural `contains` edges; a parallel AST pass inside `pyright_session.py` (`_build_function_index`, `_CallSiteVisitor`) builds the position-indexed function index pyright needs. The two never share a tree; this duplicates `ast.parse` work but keeps the extractor a pure function of source bytes.
- **`Protocol`-typed resolvers with No-Op fallback.** `CallResolver` and `ReferenceResolver` are `typing.Protocol`s with `NoOpCallResolver` / `NoOpReferenceResolver` defaults baked into `extractor.extract`'s kwargs (`extractor.py:83-84`, `247-249`). Tests can construct the extractor without spawning pyright.
- **Stdout-strictness via guard object.** `_GuardedTextStdout` raises rather than silently swallows; any library print() becomes a `StdoutGuardError`, which the dispatch boundary turns into a `_ERR_INTERNAL` JSON-RPC response (`server.py:259-260`). This is the plugin-side closure of WP2 UQ-WP2-08.
- **Path-jail-aware path handling.** `_resolve_module_path` relativises only the path used for the dotted qualname prefix; the wire `source.file_path` stays exactly as the host sent it, so the host's path-jail (which canonicalises against `project_root`) sees the original (`server.py:156-174`, `extractor.py:24-32`).
- **Single source of truth on ontology version.** The manifest declares `ontology_version = "0.5.0"` (`plugin.toml:46`); `server.py:36` redeclares the same constant `ONTOLOGY_VERSION = "0.5.0"`. Two declarations, no shared import — kept matched by hand per ADR-027.
- **B.8 fix layered defence.** The `@overload`-stub skip in `_has_overload_decorator` is the *fast path* for the named PEP 484 case; the same-id dedup loop in `_walk` is the *belt-and-suspenders* for anything the pattern-based check misses (aliased imports, `@singledispatch.register def _():`). Both feed the same wire-correctness invariant (no two entities with identical IDs) so the host's `UNIQUE(entities.id)` never trips mid-run (`extractor.py:283-294`, `624-637`, `645-652`).

**Concerns:**

- **Doctrine asterisk still live.** `wardline_probe.py:38` imports `wardline.core.registry` by name — exactly the Loom-doctrine asterisk called out in `docs/suite/loom.md` §5. Retirement condition is documented but not yet met. Not a defect; flagged because any architecture-quality review should know this is deliberate.
- **`ONTOLOGY_VERSION` is duplicated in two files (`server.py:36` and `plugin.toml:46`)** with no compile-time/runtime cross-check that they match. If a future ADR-027 minor bump updates one and forgets the other, the handshake validates the manifest value while the plugin behaves per the constant — silent skew. The comment at `server.py:38-41` acknowledges this and defers the manifest-flow-through.
- **`PyrightSession.close()` masks errors from the LSP `shutdown`/`exit` exchange** (`pyright_session.py:170`): timeouts, transport-closed, broken pipe, and `OSError` are all swallowed before the kill-and-wait. This is the correct behaviour at process-end, but it means a pyright that hangs on shutdown gives no signal beyond the eventual `process.kill()`.
- **`_resolve_with_pyright` busy-loops on mismatched `id` responses** (`pyright_session.py:663-666`). If pyright ever sends a stream of id-mismatched frames between request and response (server-initiated notifications, mismatched-cancel ack), the loop just keeps reading until the per-call deadline fires. The deadline upper-bounds it (5 s default), so this is bounded rather than fatal.
- **`_resolve_module_path`'s fall-through to the raw path on `ValueError`** (`server.py:170-173`) writes the absolute path into `source.file_path` of every emitted entity, which the host's path-jail check will reject. The comment says this is intentional ("fall back to the raw path so the host's logs show the drift"); whether that's the right failure mode versus an explicit per-file error finding is a design call worth noting for axiom-system-architect.
- **AST re-parse duplication.** `extractor.py` and `pyright_session._build_function_index` each call `ast.parse` on the same source bytes for every `analyze_file`. At elspeth-scale (~425k LOC Python) this is two AST walks per file, not one. Not yet measured; called out because the B.8 scale test on this branch is exactly where this would surface.

**Confidence:** High — Read in full: `plugin.toml`, `pyproject.toml`, `server.py` (285), `extractor.py` (744), `qualname.py` (46), `wardline_probe.py` (56), `entity_id.py` (75), `stdout_guard.py` (62), `call_resolver.py` (64), `reference_resolver.py` (69), `__init__.py`, `__main__.py`. Sampled `pyright_session.py` top-level structure (every `def`/`class` declaration line) plus full reads of `__init__`, `close`, `resolve_calls`, `resolve_references`, `_resolve_with_pyright`, `_ensure_process`, `_record_restart_or_poison`, `_start_process`, `_initialize`, `_resolve_executable`, `_subprocess_env`, `_start_stderr_drain`, `_drain_stderr`, `_request`, `_notify`, `_live_process`, `_write_message`, `_read_message`. Cross-validated: B.8 `@overload` commit `29f0426` body cited verbatim, manifest pyright pin matches `pyproject.toml` pin (`1.1.409` in both `plugin.toml:29` and `pyproject.toml:20`), Wardline probe is initialize-only (single call site at `server.py:151`), the doctrine asterisk import path matches `loom.md` §5's wording. Tests directory inventory matches source-file inventory 1:1 (10 source files, 10 test files including `test_round_trip.py`).

**Information Gaps:**

- Did not exhaustively read `pyright_session.py` lines 715-1247 (the AST function-indexing helpers, dict-dispatch visitor, byte/position translators). Sampled enough to confirm shape but not every branch.
- Did not read the test files (`tests/test_*.py`); test coverage claims would require that step.
- Did not verify wire-shape claims by running the e2e script (`tests/e2e/sprint_1_walking_skeleton.sh`); compatibility is asserted from the Python TypedDicts vs. the Rust `protocol.rs` docstring at `server.py:7-19`, not from a live run on this branch.
- Did not chase `clarion-core/src/plugin/host.rs:132-154` to confirm the `RawEntity` / `RawSource` shape claim cited in `extractor.py:8-22`. Treated as authoritative because the extractor docstring is dated to the same commit family as the host.

**Caveats:**

- LOC counts via `wc -l` include blank lines and docstring lines. The "actual code" share is lower; `extractor.py:1-54` is all docstring, for instance.
- "B.8 +98 lines on this branch" is taken from the discovery-findings document's framing; I did not run `git diff main...HEAD -- extractor.py | wc -l` to verify exactness, but the commit `29f0426` body matches the behaviour read in `extractor.py:567-668`.
- The `wardline_probe` integration is described as "fail-soft" based on the three return shapes; whether downstream consult-mode briefings actually do anything different when `status == "enabled"` versus `"absent"` is out of scope for this entry (it would require reading the MCP briefing assembler).

**Risk Assessment:**

- *Doctrine asterisk surface.* The `wardline.core.registry` import (`wardline_probe.py:38`) is a known, ratified asterisk under `loom.md` §5 — risk is bounded by the documented retirement condition, but it remains a point where the federation axiom is consciously bent. Any review must surface it.
- *Wire-shape skew risk.* The plugin re-declares `ONTOLOGY_VERSION = "0.5.0"` (`server.py:36`) and `entity_kinds`/`edge_kinds`/`rule_id_prefix` (`plugin.toml:35-39`) as parallel sources of truth with the host's `protocol.rs`. Skew here would surface only at the handshake — the host's validator rejects mismatched `ontology_version` per ADR-027, so the risk is detected, not silent.
- *Pyright-availability dependency.* The walking-skeleton CI job and any `analyze` run that requests `calls` or `references` edges hard-depends on `pyright-langserver` resolvable via PATH, the active venv, or absolute path (`pyright_session.py:618-625`). On hosts without Node, the plugin still ships entities (the no-op fallback path), but every function emits as "unresolved" — observable but not catastrophic.
- *B.8 scale-test risk.* This branch (`sprint-2/b8-scale-test`) is precisely the change that hardens the extractor against the failure mode it was discovered under (UNIQUE collision on `@overload` stubs at elspeth scale). The fix is layered (semantic skip + safety-net dedup); the residual risk is aliased-overload imports plus identical-qualname intentional redefinitions, both of which now hit the safety-net path and log to stderr rather than crash.
