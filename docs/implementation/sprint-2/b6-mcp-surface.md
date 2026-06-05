# B.6 - WP8 MCP surface: `loomweave-mcp` crate with 7 navigation tools

**Status**: PANEL-REVISED STAGE 0 DESIGN - split required; no implementation
starts before this document is committed.
**Anchoring design**: [scope amendment](./scope-amendment-2026-05.md),
[B.4* calls edges](./b4-calls-edges.md),
[B.5* references edges](./b5-references-edges.md)
**Accepted ADRs**:
[ADR-007](../../loomweave/adr/ADR-007-summary-cache-key.md),
[ADR-011](../../loomweave/adr/ADR-011-writer-actor-concurrency.md),
[ADR-022](../../loomweave/adr/ADR-022-core-plugin-ontology.md),
[ADR-023](../../loomweave/adr/ADR-023-tooling-baseline.md),
[ADR-028](../../loomweave/adr/ADR-028-edge-confidence-tiers.md),
[ADR-029](../../loomweave/adr/ADR-029-entity-associations-binding.md),
[ADR-030](../../loomweave/adr/ADR-030-on-demand-summary-scope.md)
**Predecessor**: B.5* (`references` edges)
**Successor**: B.8 (elspeth scale-test)
**Filigree umbrella**: `clarion-e2a3672cc9`

---

## 1. Scope

B.6 introduces Loomweave's MVP consult surface: a new Rust library crate,
`loomweave-mcp`, and a `loomweave serve` CLI subcommand exposing seven MCP tools
over JSON-RPC stdio.

The seven tools are:

- `entity_at(file, line)`
- `find_entity(pattern)`
- `callers_of(id, confidence = "resolved")`
- `execution_paths_from(id, max_depth = 3, confidence = "resolved")`
- `summary(id)`
- `issues_for(id, include_contained = true)`
- `neighborhood(id, confidence = "resolved")`

B.6 also lands the narrowed WP6 surface required by ADR-030:
`LlmProvider`, `AnthropicProvider`, `RecordingProvider`, one leaf prompt
template, lazy summary-cache population, inferred-edge dispatch support, and a
per-session cost ceiling.

Out of scope:

- The ADR-012 HTTP/UDS read API. B.6 is MCP stdio only.
- Batched or prewarmed summaries. ADR-030 defers that to v0.2.
- Aggregated module/subsystem summaries. In v0.1, module summaries describe a
  single module's docstring and top-level members; they do not aggregate child
  function/class summaries.
- Findings emission to Filigree. ADR-029 split WP9-A binding from WP9-B
  finding export.
- Editing Filigree main branches from this Loomweave checkout. Filigree changes
  happen in the Filigree repo and PR.

Live prerequisite check at this revision:

- Loomweave branch is `sprint-2/b6-impl` from `sprint-2/b5-impl`.
- B.5* commits are present locally.
- Filigree PR 42 is OPEN, base `main`, head
  `loomweave/b7-entity-associations`, and contains the reverse
  `GET /api/entity-associations?entity_id=...` route plus
  `list_associations_by_entity` MCP/data-layer support. Some PR checks were
  still in progress at this design pass; Phase E depends on that PR being
  merged or otherwise available to the integration environment.
- The global `filigree` binary is older than Loomweave's tracker DB schema.
  Loomweave tracker commands must use:
  `uv run --project /home/john/filigree filigree ...`

## 2. Locked Surfaces

B.6 reads and writes against these existing surfaces:

- Workspace crates today are `loomweave-core`, `loomweave-storage`,
  `loomweave-cli`, and `loomweave-plugin-fixture`. `loomweave-mcp` becomes the
  fifth workspace member.
- JSON-RPC Content-Length framing already exists in
  `crates/loomweave-core/src/plugin/transport.rs`. B.6 reuses that framing but
  defines MCP-specific protocol types inside `loomweave-mcp`.
- `crates/loomweave-cli/src/cli.rs` currently has `install` and `analyze`.
  `serve` is a new subcommand on the same binary.
- `ReaderPool` is the read path. MCP tool handlers borrow readers with
  `ReaderPool::with_reader`, do short queries, and drop the connection.
- `WriterCmd::InsertEdge` is scan-time only. `writer.rs` rejects anchored
  `EdgeConfidence::Inferred` today, by design.
- B.4* locked inferred call storage to the existing `edges` table but deferred
  the query-time write command to B.6.
- B.4* and B.5* require ambiguous edge expansion as
  `to_id union properties.candidates`. B.6 must not treat the stored `to_id`
  of an ambiguous edge as the only possible callee.
- Current `summary_cache` exists in `0001_initial_schema.sql`, but it lacks
  `last_accessed_at`, neighborhood statistics, and stale semantics required
  by ADR-007.
- The Python plugin emits `source.file_path` and `source.source_range` in each
  `RawEntity`, but `map_entity_to_record` currently drops them. `entity_at`,
  `summary`, and `issues_for` depend on fixing that persistence gap.
- Python call-resolution currently reports aggregate unresolved counts, not a
  typed per-site payload. Inferred dispatch cannot be correct until those
  sites are persisted mechanically.

## 3. Design Decisions

### D1 - MCP SDK choice and process model

Decision: implement a small Rust MCP stdio server in `loomweave-mcp`, using the
existing Content-Length framing style rather than adopting a third-party SDK
for B.6.

Current crates.io reality check found viable Rust MCP crates:

- `rmcp` 1.7.0, Apache-2.0, repository `modelcontextprotocol/rust-sdk`.
- `model-context-protocol` 0.2.2, MIT OR Apache-2.0.
- `mcp-server` 0.1.0, MIT, same repository family as `rmcp`.

`rmcp` has an acceptable trust posture to revisit later, but B.6 needs a
stdio-only server with seven tools and already owns JSON-RPC framing. Adding
an SDK now imports transport, macro, schema, and runtime choices before the
local server shape is known. B.6 keeps the protocol layer thin and local; a
follow-up can replace it if the local MCP module grows beyond initialize,
tools/list, and tools/call.

Process model:

- `loomweave serve` runs one foreground MCP stdio process. This is not the
  ADR-012 HTTP/UDS read API.
- It opens a `ReaderPool` to `.loomweave/loomweave.db`.
- It starts the storage writer actor when summary, inferred-edge, or
  query-time stats writes are enabled.
- It owns `ServerState`: config, readers, optional writer, provider factory,
  Filigree client, session stats, budget ledger, and inferred-dispatch guard.

### D2 - Crate structure

Decision: create `crates/loomweave-mcp/` as a library crate.

`crates/loomweave-mcp/Cargo.toml` must use the workspace floor:

```toml
[package]
name = "loomweave-mcp"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true
```

Dependencies:

- `loomweave-core`: entity IDs, edge confidence, provider traits, prompt
  metadata, and reused framing.
- `loomweave-storage`: `ReaderPool`, writer actor handle, schema helpers, query
  helpers.
- `serde` / `serde_json`: MCP protocol and tool schemas.
- `tokio`: stdio loop, writer actor integration, in-flight coalescing.
- `thiserror` / `anyhow` as already used in neighboring crates.
- `reqwest` with rustls only when Phase E starts and the Filigree client is
  actually implemented.

`loomweave-cli` owns CLI parsing and calls `loomweave_mcp::serve(options)`.
`loomweave-mcp` is a library so tests can call dispatch functions without
spawning a process for every unit case.

### D3 - Query-time writer commands

Decision: add explicit query-time writer commands instead of generalizing
`WriterCmd::InsertEdge` with a `WriteOrigin` enum.

New commands:

- `WriterCmd::InsertInferredEdges`
- `WriterCmd::UpsertSummaryCache`
- `WriterCmd::TouchSummaryCache`
- `WriterCmd::RecordMcpSessionStats`, if stats persistence is included
- `WriterCmd::ReplaceUnresolvedCallSitesForCaller`, used during analyze, not
  by MCP

The first four run as single-command transactions through the same writer
actor and do not require an active analyze run. Analyze-time commands still
require `BeginRun` / `CommitRun`. Readers remain read-only per ADR-011; MCP
handlers never open their own write connection.

`InsertInferredEdges` contract:

- Input is one caller plus a full inference cache key.
- Kind is always `calls`; confidence is always `inferred`.
- Source range comes from `entity_unresolved_call_sites`.
- Properties include `model_id`, `prompt_version`, `caller_content_hash`,
  `inference_cache_key`, optional model confidence, and optional rationale.
- FK validity is enforced.
- If a resolved or ambiguous static call already exists for `(from_id, to_id)`,
  the inferred materialization is skipped and counted; it must not downgrade or
  overwrite static evidence.

This keeps "query-time cache write" visible at the call site and avoids a
conditional branch inside the scan-time contract.

### D4 - Unresolved-call-sites queryability and wire contract

Decision: create a sibling `entity_unresolved_call_sites` table, not synthetic
placeholder `edges` rows.

This intentionally differs from the handoff's recommended synthetic-edge
option. The current `edges` table has `to_id TEXT NOT NULL REFERENCES
entities(id)` and a natural key `(kind, from_id, to_id)`. An unresolved call
site has no honest callee. Inventing a pseudo-callee would pollute traversal
semantics and make `neighborhood` lie.

Schema:

```sql
CREATE TABLE entity_unresolved_call_sites (
    caller_entity_id    TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    caller_content_hash TEXT NOT NULL,
    site_key            TEXT NOT NULL,
    site_ordinal        INTEGER NOT NULL,
    source_file_id      TEXT REFERENCES entities(id),
    source_byte_start   INTEGER NOT NULL,
    source_byte_end     INTEGER NOT NULL,
    callee_expr         TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    PRIMARY KEY (caller_entity_id, caller_content_hash, site_key)
);
CREATE INDEX ix_unresolved_call_sites_caller
  ON entity_unresolved_call_sites(caller_entity_id);
CREATE INDEX ix_unresolved_call_sites_expr
  ON entity_unresolved_call_sites(callee_expr);
```

`site_key` is `blake3(caller_entity_id || source_byte_start || source_byte_end
|| callee_expr)`. `site_ordinal` is display-only. Re-analysis replaces all
rows for the caller's current content hash and deletes rows for previous
hashes, so moved/deleted unresolved sites cannot persist as false anchors.

Typed payload:

- Python: `UnresolvedCallSite(TypedDict)` with `caller_entity_id`,
  `site_ordinal`, `source_byte_start`, `source_byte_end`, and `callee_expr`.
- Python `CallResolutionResult`: `unresolved_call_sites:
  list[UnresolvedCallSite]`.
- JSON response: `stats.unresolved_call_sites` defaults to `[]`; the existing
  aggregate `unresolved_call_sites_total` remains.
- Rust protocol: `AnalyzeFileStats.unresolved_call_sites:
  Vec<UnresolvedCallSite>`.
- Host validation: caller id is accepted for this file, byte range is bounded,
  range is non-empty, `callee_expr` is UTF-8 and capped at 512 bytes.
- CLI mapping: after entity inserts, call
  `ReplaceUnresolvedCallSitesForCaller` for every caller present in the stats
  payload.

Invariant:

- When Pyright is available and no cap fires,
  `len(unresolved_call_sites) == unresolved_call_sites_total`.
- When Pyright is unavailable, init-capped, or file-capped, the total may be
  non-zero while the list is empty. Those cases must carry named findings and
  tests.

For reverse `callers_of(target, confidence="inferred")`, the server runs a
bounded candidate pass over unresolved callers before LLM dispatch:

1. Exact short-name or qualified-name suffix match against `callee_expr`.
2. Same-module unresolved sites before cross-module sites.
3. Caller id lexical tie-break.
4. Hard cap: 50 unresolved callers considered per request.

The pass reserves budget before dispatch and may return `truncated=true`
without attempting every unresolved caller.

### D5 - Inferred-edge cache and coalescing guard

Decision: separate the full inference result cache from materialized current
edges.

Schema:

```sql
CREATE TABLE inferred_edge_cache (
    caller_entity_id    TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    caller_content_hash TEXT NOT NULL,
    model_id            TEXT NOT NULL,
    prompt_version      TEXT NOT NULL,
    result_json         TEXT NOT NULL,
    cost_usd            REAL NOT NULL DEFAULT 0.0,
    token_count         INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    last_accessed_at    TEXT NOT NULL,
    PRIMARY KEY (caller_entity_id, caller_content_hash, model_id, prompt_version)
);
```

`edges` stores the current materialized inferred calls used by graph traversal.
On a fresh inference for a caller:

1. Upsert `inferred_edge_cache`.
2. Delete stale materialized inferred rows for that caller whose
   `properties.inference_cache_key` differs from the active key.
3. Insert the active inferred rows unless a static resolved/ambiguous row
   already exists for the same `(from_id, to_id)`.
4. Count skipped static duplicates separately.

`ServerState` also owns an in-flight registry keyed by the full cache key:

```rust
struct InferenceCacheKey {
    caller_entity_id: String,
    caller_content_hash: String,
    model_id: String,
    prompt_version: String,
}

tokio::sync::Mutex<HashMap<InferenceCacheKey, broadcast::Sender<InferredDispatchResult>>>
```

On cold miss:

1. Acquire the mutex.
2. If a sender exists, subscribe and wait.
3. If not, create a broadcast channel, insert it, drop the mutex, and dispatch.
4. After provider response and cache/materialization write, broadcast the
   result.
5. Remove the key by drop guard on success, error, timeout, or caller
   cancellation.

Waiters receive the same structured success or failure as the owner. Waiters
use a 60-second timeout; timeout increments
`inferred_dispatch_wait_timeout_total` and does not cancel the owner task.

### D6 - Filigree HTTP client and auth

Decision: `loomweave-mcp` owns a small Filigree HTTP client, configured from
`loomweave.yaml`.

Config:

```yaml
integrations:
  filigree:
    enabled: true
    base_url: "http://127.0.0.1:8766"
    actor: "loomweave-mcp"
    token_env: FILIGREE_API_TOKEN
    timeout_seconds: 5
```

Auth follows Filigree's actor-identity pattern from PR 42. If the route later
requires Bearer auth, Loomweave reads `token_env`; otherwise it still sends
`actor` where Filigree accepts it. Filigree absence never breaks Loomweave solo
mode: `issues_for` returns an unavailable envelope with reason
`filigree-unreachable`.

### D7 - Filigree reverse-route dependency

Decision: depend on Filigree's reverse lookup route rather than scanning all
issues client-side.

Required route:

`GET /api/entity-associations?entity_id=<id>`

Required response shape from PR 42:

```json
{
  "associations": [
    {
      "issue_id": "filigree-...",
      "loomweave_entity_id": "python:function:demo.hello",
      "content_hash_at_attach": "...",
      "attached_at": "...",
      "attached_by": "..."
    }
  ]
}
```

Current PR 42 includes this route, its data-layer helper, HTTP tests, and MCP
tool tests. B.6 Phase E still treats it as a live external dependency:

- If PR 42 merges before Phase E, Loomweave integration tests use the merged
  Filigree route.
- If PR 42 remains open, Phase E does not close B.6; it may prepare Loomweave
  code against the contract, but the seven-tool MVP requires a real
  route-backed integration test before close.

Why not client-side scan:

- Fetching all issues and then every issue's associations is O(issue_count)
  HTTP calls per `issues_for`.
- PR 42 already adds the entity association index and reverse route; Loomweave
  should use that one-hop lookup.

### D8 - `include_contained` traversal cap

Decision: default `issues_for(id, include_contained=true)` traverses `contains`
edges with deterministic DFS and hard-truncates at 100 unique issues. B.6 v0.1
does not implement pagination tokens for `issues_for`; correct resumability
would need to persist traversal state, seen issue IDs, contained-entity stack,
and Filigree offsets.

Algorithm:

1. Start with the requested `entity_id`.
2. If `include_contained`, walk `edges where kind='contains' and from_id=?`
   with a visited set.
3. For each visited entity, call the Filigree reverse route.
4. Deduplicate by `issue_id`.
5. Stop at 100 issues, 1000 contained entities, or a 5-second total Filigree
   deadline.

Response includes `truncated: true` and `truncation_reason` when a cap fires.
B.6 cannot close on a fake route; the route compatibility test must hit a real
Filigree server or in-process Filigree app configured from PR 42's code.

### D9 - `execution_paths_from` traversal cap

Decision: DFS over `calls` edges only, bounded by both:

- `max_depth`, default 3, max 8;
- `edge_visit_cap`, default 500.

Confidence is a tier set:

- `resolved`: include resolved calls only.
- `ambiguous`: include resolved and ambiguous calls.
- `inferred`: include resolved, ambiguous, and inferred calls.

Ambiguous expansion uses the candidate expansion helper from D17. Inferred
expansion may trigger D5 dispatch for reached callers.

When the edge cap fires, return partial paths already found plus
`truncated=true` and `truncation_reason="edge-cap"`. B.6 v0.1 does not
support path continuation. Correct continuation would need to encode DFS
stack, path-local visited sets, ambiguous candidate position, DB snapshot, and
inferred-dispatch state.

### D10 - Schema additions and migration policy

Decision: edit `0001_initial_schema.sql` in place for B.6 unless a published
Loomweave build has already produced databases with the Sprint 2 branch schema.

Reasoning:

- `main` / `origin/main` do not contain B.4* or B.5* edge rows.
- B.4* and B.5* are branch-local Sprint 2 work.
- More importantly, no external operator has produced a `.loomweave/loomweave.db`
  from a published build containing these branch schemas. That external
  published-DB boundary is ADR-024's real retirement trigger for in-place
  edits.

Schema changes:

- Add `entities.source_file_path TEXT` and `ix_entities_source_file_path`.
- Keep and populate existing source line/byte range columns.
- Ensure `entities.content_hash` is populated where source bytes are
  available.
- Add `entity_unresolved_call_sites` from D4.
- Add `inferred_edge_cache` from D5.
- Extend `summary_cache` to match ADR-007:
  - `last_accessed_at TEXT NOT NULL`;
  - `caller_count INTEGER NOT NULL`;
  - `fan_out INTEGER NOT NULL`;
  - `stale_semantic INTEGER NOT NULL DEFAULT 0 CHECK (stale_semantic IN (0, 1))`.

No `0002` migration is created unless B.4*/B.5* are merged and a published
build uses their branch schemas before B.6 implementation starts.

### D11 - Cost ceiling and budget ledger

Decision: per MCP server session cost ceiling, default USD 10.00, enforced by
a shared reservation ledger so concurrent cold misses cannot overspend.

Config:

```yaml
llm:
  enabled: false
  provider: anthropic
  session_cost_ceiling_usd: 10.0
  summary_model_id: claude-haiku-4-5
  inferred_edges_model_id: claude-haiku-4-5
  max_inferred_edges_per_caller: 8
  cache_max_age_days: 180
```

Ledger:

```rust
struct BudgetLedger {
    ceiling_usd: Decimal,
    spent_usd: Decimal,
    reserved_usd: Decimal,
}
```

Before a live provider call, the caller reserves an estimated cost. If
`spent + reserved + estimate` exceeds the ceiling, the request returns
`available=false`, reason `cost-ceiling-exceeded`, without calling the
provider. After the call, the server reconciles actual cost and releases any
unused reservation. Network failures release the reservation. Provider
responses that include usage record actual cost.

When the ceiling is reached:

- `summary()` returns `available=false`, reason `cost-ceiling-exceeded`.
- Inferred-edge dispatch returns the same reason.
- Server stats increment `cost_ceiling_exceeded_total`.
- Response diagnostics include `LMWV-LLM-COST-CEILING-EXCEEDED`.

The ceiling resets on process restart.

### D12 - Tool descriptions

Decision: the exact MCP tool descriptions are:

`entity_at`:
"Return the innermost Loomweave entity whose source range contains a file and line. Paths are normalized relative to the project root. Returns no match rather than guessing when ranges are absent."

`find_entity`:
"Search Loomweave entities by id, name, short name, and summary text stored on entity rows. Results are paginated and ranked by FTS match where possible. This does not traverse the graph and does not search on-demand summary_cache entries."

`callers_of`:
"Return entities that call the given entity. Default confidence is resolved, so ambiguous static candidates and LLM-inferred edges are excluded unless explicitly requested. Ambiguous edges expand all candidates; inferred edges may trigger bounded LLM dispatch."

`execution_paths_from`:
"Return bounded calls-only execution paths starting at an entity. Default confidence is resolved. max_depth defaults to 3 and traversal also stops at the server edge cap; responses say when they are truncated."

`summary`:
"Return an on-demand cached summary for one entity. In v0.1 this is leaf scope only: module summaries describe the module docstring and top-level members, not an aggregation of contained function/class summaries."

`issues_for`:
"Return Filigree issues attached to this Loomweave entity, optionally including issues attached to contained entities. Filigree is an enrichment source; if unavailable, the tool returns an unavailable envelope instead of failing Loomweave."

`neighborhood`:
"Return the one-hop Loomweave neighborhood around an entity: callers, callees, container, contained entities, and references. Default confidence is resolved; ambiguous and inferred calls are opt-in. References are not execution flow."

### D13 - WP6 narrowing matrix

| Surface | B.6 action |
|---|---|
| `LlmProvider` | Replace Sprint 1 stub with request/response/cost methods |
| `AnthropicProvider` | Implement live provider behind explicit config and opt-in |
| `RecordingProvider` | Deterministic tests and fixtures for summary and inference |
| Prompt template | One leaf template with version id, e.g. `leaf-v1` |
| Summary cache | Lazy summary cache with ADR-007 5-tuple key and TTL backstop |
| Summary MCP tool | Cold call, warm cache hit, disabled/unavailable envelopes |
| Inferred call prompt | One prompt for unresolved call-site resolution |
| Batched phases | Not built in v0.1 |
| Aggregation tiers | Not built in v0.1 |

### D14 - Entity source metadata and content hashes

Decision: B.6a first persists source path, line ranges, byte ranges where
available, and content hashes before implementing `entity_at`, `summary`, or
`issues_for`.

Current reality:

- `plugins/python` emits `source.file_path` and nested `source.source_range`.
- `AcceptedEntity` keeps `source_file_path`.
- `map_entity_to_record` drops source path and range today.
- `entities.content_hash` exists but is not populated by analyze.

Mapping changes:

- `EntityRecord` gains `source_file_path`.
- Writer SQL inserts `source_file_path`, line range, byte range, and
  `content_hash`.
- Analyzer builds a file-to-module map before storage mapping. A module row's
  `source_file_id` is its own id for B.6; class/function rows point to the
  module entity for their file. Future core-owned file entities may supersede
  this without changing MCP tool contracts.
- Parse `source.source_range.start.line` and `.end.line` as one-based lines.
- Preserve byte ranges when plugins provide them. If only line ranges are
  available, byte columns stay `NULL`.
- Compute `content_hash` with BLAKE3 over the source slice for entities with
  byte ranges. For Python entities that only have line ranges, compute the
  hash over the inclusive one-based line slice with normalized `\n` joins.
  Module hashes use the full file bytes.
- Missing or malformed ranges do not crash analyze. They produce a named
  finding and persist the entity with null range/hash; MCP tools then return
  unavailable or no-match rather than guessing.

`entity_at(file, line)` normalizes the input path, matches
`entities.source_file_path`, filters rows whose line range contains `line`,
then orders by:

1. Smallest range width.
2. Kind precedence: function, class, module, then other.
3. Entity id lexical order.

The implementation must not use `scope_level`; the Python plugin does not emit
that value today.

### D15 - Response, error, and schema contracts

Decision: every tool returns a mechanically typed envelope:

```rust
struct ToolResponse<T> {
    ok: bool,
    result: Option<T>,
    error: Option<ToolError>,
    diagnostics: Vec<Diagnostic>,
    truncated: bool,
    truncation_reason: Option<String>,
    stats_delta: McpStatsDelta,
}

struct ToolError {
    code: String,
    message: String,
    retryable: bool,
}

struct Diagnostic {
    code: String,
    severity: String,
    message: String,
    metadata: serde_json::Value,
}
```

Transport mapping:

- Malformed JSON-RPC: JSON-RPC error.
- Unknown method: JSON-RPC `-32601`.
- Invalid tool parameters: JSON-RPC `-32602`.
- Unknown tool name: JSON-RPC `-32601` from the MCP call handler.
- Entity not found, empty result, LLM disabled, Filigree unavailable, cost
  ceiling, timeout, and cap events are tool-level envelopes, not transport
  failures.

There are no continuation tokens in B.6 v0.1. Tool responses use
`truncated`/`truncation_reason` only.

Schema tests:

- Golden JSON fixture for each tool success response.
- Golden JSON fixture for each normal unavailable/error envelope.
- Protocol tests for invalid params and unknown tool mapping.
- `tools/list` test proving the exact docstrings from D12 are exposed.

### D16 - LLM provider selection and no-live-CI rule

Decision: live provider selection is explicit opt-in. CI and local tests must
not call a live LLM even if API key environment variables are present.

Config rules:

- Default `llm.enabled=false`.
- `RecordingProvider` is selected by tests and fixture configs.
- `AnthropicProvider` is selected only when `llm.enabled=true` and either the
  config has `allow_live_provider=true` or the process has
  `LOOMWEAVE_LLM_LIVE=1`.
- Missing API key with live provider selected is a startup/config error.
- API key present while live provider is not explicitly allowed has no effect.

Tests:

- Provider factory test with `ANTHROPIC_API_KEY` set and live opt-in absent
  still selects disabled/recording behavior.
- E2E test unsets live opt-in and asserts the recording fixture was used.
- Any live-provider test is ignored by default and has a name containing
  `live`.

### D17 - Candidate expansion helper

Decision: implement one shared storage helper for call target matching:
`call_edges_targeting(target_id, confidence_tier_set)`.

The helper returns direct resolved calls plus, when ambiguous is requested,
ambiguous rows where either:

- `to_id = target_id`, or
- `target_id` appears in `properties.candidates`.

The helper deduplicates by `(kind, from_id, to_id, source range)` so an
ambiguous row whose chosen `to_id` also appears in `properties.candidates`
does not double-count. A regression test must cover the case where the target
appears only in `properties.candidates`.

`callers_of`, `execution_paths_from`, and `neighborhood` all use this helper
instead of open-coding JSON1 queries.

### D18 - Observability and session stats

Decision: `ServerState` tracks process-local MCP stats and returns per-call
`stats_delta`.

Counters:

- `mcp_tool_calls_total{tool}`
- `mcp_tool_errors_total{tool,code}`
- `mcp_tool_truncated_total{tool,reason}`
- `summary_cache_hits_total`
- `summary_cache_misses_total`
- `summary_llm_calls_total`
- `llm_disabled_total`
- `llm_unavailable_total`
- `cost_ceiling_exceeded_total`
- `filigree_unavailable_total`
- `inferred_dispatch_started_total`
- `inferred_dispatch_coalesced_total`
- `inferred_dispatch_failed_total`
- `inferred_dispatch_wait_timeout_total`
- `inferred_materialized_edges_total`
- `inferred_skipped_static_duplicates_total`

Tests assert counter deltas for cache hit/miss, Filigree unavailable,
truncation, cost ceiling, coalescing hit, and dispatch failure.

## 4. Tool Contracts

All tools use the D15 envelope. The `result` payloads below omit the envelope
for readability.

### `entity_at(file, line)`

Input schema:

```json
{"file": "src/demo.py", "line": 42}
```

Result:

```json
{"entity": {"id": "...", "kind": "function", "name": "..."}}
```

Semantics:

- Normalize `file` relative to project root and reject path escape.
- Match normalized path against `entities.source_file_path`.
- Filter rows whose one-based line range contains `line`.
- Use D14 tie-breakers.
- Empty result is `entity: null`, `ok=true`.

Errors:

- Invalid path escape: `code="invalid-path"`, retryable false.
- Non-positive line: invalid params.

### `find_entity(pattern)`

Input schema:

```json
{"pattern": "TokenManager", "limit": 20, "cursor": null}
```

Result:

```json
{"entities": [{"id": "...", "kind": "class", "name": "..."}], "next_cursor": null}
```

Semantics:

- Search entity id, name, short name, and `entities.summary`.
- Use `entity_fts MATCH ?` when the pattern is token-like.
- Fall back to escaped `LIKE` for punctuation-heavy IDs.
- Default limit 20, max 100.
- `summary_cache` is not indexed for search in B.6.

Errors:

- Blank pattern: invalid params.
- Bad cursor: invalid params.

### `callers_of(id, confidence)`

Input schema:

```json
{"id": "python:function:demo.world", "confidence": "resolved"}
```

Result:

```json
{"callers": [{"id": "...", "edge_confidence": "resolved", "source_range": {}}]}
```

Semantics:

- Missing target entity returns `available=false`, reason `entity-not-found`.
- `resolved` includes resolved calls only.
- `ambiguous` includes resolved plus ambiguous candidate expansion via D17.
- `inferred` includes resolved, ambiguous, and D5 inferred dispatch.
- Inferred reverse lookup uses D4's bounded unresolved-caller candidate pass.

Timeout:

- Inferred dispatch has a 60-second wait timeout. The response may include
  partial static results plus a diagnostic.

### `execution_paths_from(id, max_depth, confidence)`

Input schema:

```json
{"id": "python:function:demo.entry", "max_depth": 3, "confidence": "resolved"}
```

Result:

```json
{"paths": [[{"id": "..."}, {"id": "..."}]], "edge_count_visited": 12}
```

Semantics:

- Calls-only DFS.
- Cycle-safe path-local visited set.
- D9 depth and edge caps.
- Ambiguous candidate expansion via D17.
- Inferred tier may dispatch per reached caller.
- Cap responses return partial paths and set `truncated=true`.

### `summary(id)`

Input schema:

```json
{"id": "python:function:demo.hello"}
```

Result:

```json
{
  "available": true,
  "cache": "hit",
  "summary": {"purpose": "...", "relationships": [], "risks": []},
  "stale_semantic": false
}
```

Semantics:

- Missing entity: `available=false`, reason `entity-not-found`.
- Entity without content hash: `available=false`, reason
  `content-hash-unavailable`.
- LLM disabled: `available=false`, reason `llm-disabled`.
- Cache key is ADR-007's 5-tuple.
- Cache hit updates `last_accessed_at`.
- TTL expiry is a miss.
- Cold call uses `RecordingProvider` in tests and live provider only by D16.

### `issues_for(id, include_contained)`

Input schema:

```json
{"id": "python:module:demo", "include_contained": true}
```

Result:

```json
{
  "available": true,
  "matched": [],
  "drifted": [],
  "not_found": []
}
```

Semantics:

- Uses the Filigree reverse route from D7.
- Missing entity returns `available=false`, reason `entity-not-found`.
- Entity without content hash can still query Filigree but cannot classify
  drift; response includes diagnostic `LMWV-ENTITY-CONTENT-HASH-MISSING`.
- Compares `content_hash_at_attach` to current `entities.content_hash`.
- Includes contained traversal per D8.
- Filigree unavailable is a normal unavailable envelope.

### `neighborhood(id, confidence)`

Input schema:

```json
{"id": "python:function:demo.hello", "confidence": "resolved"}
```

Result:

```json
{
  "entity": {"id": "..."},
  "callers": [],
  "callees": [],
  "container": null,
  "contained": [],
  "references": []
}
```

Semantics:

- One hop only.
- Calls use confidence tier sets and D17 candidate expansion.
- References are included with confidence filtering but never used as
  executable flow.
- Missing entity returns `available=false`, reason `entity-not-found`.

## 5. TDD Matrix

Each row is a named red/green requirement. The red run must be recorded in the
task commit notes or Filigree comment before implementation lands.

| Surface | Required red tests |
|---|---|
| Protocol | `tools_list_exposes_exact_docstrings`; `call_tool_rejects_unknown_tool`; `call_tool_rejects_invalid_params`; golden success/error envelopes |
| `entity_at` | happy innermost function; module fallback; no range no match; path escape rejection; non-ASCII line-range fixture |
| `find_entity` | FTS hit; punctuation-heavy ID fallback; empty result; limit cap; blank pattern rejection; summary_cache not searched |
| `callers_of` | resolved default; ambiguous target only in candidates; duplicate candidate dedupe; missing entity; inferred disabled; inferred dispatch via recording |
| `execution_paths_from` | resolved DFS; cycle safety; ambiguous expansion; depth cap; edge cap with partial paths; inferred dispatch path |
| `summary` | missing entity; content hash missing; LLM disabled; cache miss recording call; cache hit touch; TTL miss; cost ceiling |
| `issues_for` | direct attached issue; include-contained DFS; drifted hash; Filigree unavailable; 100-issue truncation; live reverse-route compatibility |
| `neighborhood` | incoming/outgoing calls; contains parent/children; references included but not executable; ambiguous expansion; missing entity |
| Source metadata | entity source path/range persistence; malformed range finding; content hash for module/function/class; line-slice hash stability |
| Writer commands | `InsertEdge(calls,inferred)` still fails; `InsertInferredEdges` succeeds; static duplicate skipped; stale inferred rows deleted; summary cache touch/upsert |
| Unresolved sites | Python payload length equals total when available; capped/unavailable exceptions; host rejects bad caller/range; storage replace removes stale rows |
| Provider factory | default no live; API key alone does not select live; recording provider selected in tests; live tests ignored |
| Stats | per-tool calls/errors; truncations; cache hits/misses; cost ceiling; Filigree unavailable; coalesced inferred dispatch |
| E2E | real Content-Length framed `initialize`, `tools/list`, and all seven `tools/call` requests against walking-skeleton DB |

## 6. Implementation Task Ledger

### Stage 0 - Design gate

Task 1 - Design doc and panel

- Create this document.
- Run 5 reviewers: architecture, quality, reality, systems,
  python-engineering.
- Reconcile every blocking finding in the panel record.
- Commit: `docs(wp8): design B.6 MCP surface`.

### B.6a - MCP scaffold and five storage-backed tools

B.6a is a checkpoint PR/commit series. It does not close
`clarion-e2a3672cc9` or `clarion-73ab0da435`.

Task 2 - Crate scaffold and protocol skeleton

- Create `crates/loomweave-mcp/` with workspace package metadata and
  `[lints] workspace = true`.
- Add workspace member.
- Implement MCP `initialize`, `tools/list`, `tools/call`, and D15 error
  mapping.
- Add schema/golden protocol tests.
- Commit: `feat(mcp): scaffold stdio MCP server`.

Task 3 - `loomweave serve`

- Add CLI subcommand and options for project path/config path.
- Open `.loomweave/loomweave.db`, build `ReaderPool`, instantiate `ServerState`.
- Smoke test `loomweave serve --help` and stdio initialize.
- Commit: `feat(cli): add loomweave serve subcommand`.

Task 4 - Config and provider factory shell

- Add `loomweave.yaml` config parsing for MCP, LLM disabled defaults, and
  Filigree config.
- Add D16 provider factory tests before live provider implementation.
- Commit: `feat(config): add MCP and LLM config defaults`.

Task 5 - Entity source metadata and content hashes

- Edit `0001_initial_schema.sql` for `entities.source_file_path`.
- Extend `EntityRecord`, writer SQL, and analyzer mapping per D14.
- Add BLAKE3 dependency through workspace dependencies.
- Add tests proving Python module/function/class rows have source path, line
  range, and content hash.
- Commit: `feat(storage): persist entity source ranges for MCP`.

Task 6 - Storage query helpers

- Implement D17 `call_edges_targeting`.
- Add entity lookup, contains traversal, and path normalization helpers.
- Add tests for candidate-only ambiguous targets and dedupe.
- Commit: `feat(storage): add MCP graph query helpers`.

Task 7 - Five storage-backed tools

- Implement `entity_at`, `find_entity`, resolved/ambiguous `callers_of`,
  resolved/ambiguous `execution_paths_from`, and resolved/ambiguous
  `neighborhood`.
- Each tool gets red/green tests from the TDD matrix.
- Add D9 truncation behavior.
- Commit: `feat(mcp): add storage-backed navigation tools`.

Task 8 - B.6a end-to-end MCP test

- Add `tests/e2e/sprint_2_mcp_surface.sh`.
- Start `loomweave serve` against a demo DB.
- Send real Content-Length framed `initialize`, `tools/list`, and the five
  storage-backed tool calls.
- Assert docstrings, response envelopes, confidence defaults, empty paths, and
  truncation where applicable.
- Commit: `test(e2e): cover storage-backed MCP tools`.

### B.6b - WP6, inferred calls, Filigree, and seven-tool close

Task 9 - Summary and inferred cache schema

- Extend `summary_cache` per D10.
- Add `inferred_edge_cache` per D5.
- Add cache read/write/touch helpers in `loomweave-storage`.
- Commit: `feat(storage): add MCP LLM cache tables`.

Task 10 - LLM providers and prompt templates

- Extend `LlmProvider`.
- Add `AnthropicProvider` and `RecordingProvider`.
- Add leaf summary prompt and inferred-call prompt with version strings.
- Enforce D16 no-live-CI rule.
- Commit: `feat(core): add WP6 LLM provider surface`.

Task 11 - `summary` tool

- Implement cache miss/hit, disabled/unavailable envelopes, TTL backstop,
  stale-semantic flag, and D11 budget ledger.
- Add stats deltas and cost ceiling tests.
- Commit: `feat(mcp): add on-demand summary tool`.

Task 12 - Unresolved call-site storage

- Add D4 table and writer command.
- Extend Python typed payload, Rust protocol structs, host validation, and CLI
  storage mapping.
- Add parity tests for count/list behavior and capped/unavailable exceptions.
- Commit: `feat(storage): persist unresolved call sites for inferred MCP`.

Task 13 - Inferred-edge dispatch

- Add `WriterCmd::InsertInferredEdges`.
- Implement D5 cache/materialization and coalescing guard.
- Add RecordingProvider inference fixtures.
- Add writer-command and concurrent coalescing tests.
- Commit: `feat(mcp): add inferred call-edge dispatch`.

Task 14 - Filigree contract verification

- Verify PR 42's reverse route is merged or available in the integration
  environment.
- Add a contract test hitting `GET /api/entity-associations?entity_id=...`.
- File a Filigree follow-up only if the committed route diverges from D7.
- Commit Loomweave-side contract fixture updates if needed.

Task 15 - `issues_for`

- Add Filigree HTTP client.
- Implement reverse-route integration, include-contained traversal, drift
  comparison, caps, unavailable envelopes, and stats.
- Commit: `feat(mcp): add issues_for Filigree integration`.

Task 16 - Seven-tool end-to-end test

- Extend `tests/e2e/sprint_2_mcp_surface.sh` to cover `summary`,
  inferred-tier `callers_of` / `execution_paths_from`, and `issues_for`.
- Use RecordingProvider for LLM paths.
- Use real Filigree reverse-route code for `issues_for`.
- Assert no live LLM opt-in is present.
- Commit: `test(e2e): cover complete MVP MCP surface`.

Task 17 - Observability closeout

- Add D18 stats deltas where not already covered.
- Add tests for cache hits/misses, truncations, Filigree unavailable, cost
  ceiling, coalescing, and dispatch failure.
- Commit: `feat(mcp): report MCP session stats`.

Task 18 - ADR-compatible resolution notes and close

- Add dated implementation-resolution notes for ADR-028 D4 and inferred writes.
- Add dated implementation-resolution notes for ADR-030 D11 cost ceiling.
- Do not silently rewrite accepted ADR decisions; if a decision must change,
  write a new ADR or a supersession note.
- Move Filigree `clarion-e2a3672cc9` through verifying/done only after all
  gates pass.
- Close `clarion-73ab0da435` only after the Loomweave-side `issues_for` stitch
  is verified.
- Commit: `docs(wp8): close B.6 ADR resolution notes`.

## 7. Exit Criteria

B.6a checkpoint is done when:

- Stage 0 design doc and panel record are committed.
- `loomweave-mcp` is a workspace member with ADR-023 crate metadata/lints.
- `loomweave serve` starts an MCP stdio server.
- `tools/list` exposes exact D12 docstrings.
- Five storage-backed tools respond with documented shapes:
  `entity_at`, `find_entity`, `callers_of`, `execution_paths_from`,
  `neighborhood`.
- Default confidence behavior is resolved-only.
- Ambiguous edges expand candidate sets through D17.
- Entity source ranges and content hashes are persisted.
- B.6a e2e sends real MCP framed requests.

Full B.6 is done when:

- All B.6a criteria remain green.
- `summary` works through ADR-007 cache and RecordingProvider in tests.
- `summary_cache` is lazily populated and cache hits update stats.
- Inferred dispatch is cached, coalesced, cost-capped, and
  RecordingProvider-tested.
- `issues_for` returns data from the real Filigree reverse route and classifies
  drift.
- All seven tools are covered by the full MCP e2e.
- ADR-028 and ADR-030 resolution notes land without silently rewriting accepted
  ADR decisions.
- Filigree `clarion-e2a3672cc9` reaches done.
- `clarion-73ab0da435` closes only after the Loomweave-side stitch is in place.

Exact local gates before any B.6 close commit:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo build --workspace --bins`
- `cargo nextest run --workspace --all-features`
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`
- `cargo deny check`
- `plugins/python/.venv/bin/ruff check plugins/python`
- `plugins/python/.venv/bin/ruff format --check plugins/python`
- `plugins/python/.venv/bin/mypy --strict plugins/python`
- `plugins/python/.venv/bin/pytest plugins/python`
- `bash tests/e2e/sprint_2_mcp_surface.sh`

If a commit touches only docs, the commit message still records which gates
were run and why wider gates were deferred or unnecessary.

## 8. Split Decision

The implementation must split.

Reasons:

- The panel found cross-cutting blockers in schema, writer contracts, provider
  selection, e2e coverage, and Filigree integration.
- The task ledger now has more than 20 concrete red/green surfaces when tests
  are counted.
- Full B.6 crosses three risk domains: new MCP server, WP6 LLM plumbing, and
  cross-product Filigree integration.

Split shape:

- B.6a: Stage 0 plus Tasks 2-8. This delivers the MCP process, exact
  docstrings, response envelope, source metadata/content hashes, storage query
  helpers, and five storage-backed tools.
- B.6b: Tasks 9-18. This delivers summary, inferred dispatch, Filigree
  `issues_for`, stats closeout, and umbrella closure.

`clarion-e2a3672cc9` remains open through both halves. B.6a is a checkpoint,
not the umbrella close.

## 9. Panel Review Record

The required 5-reviewer panel ran before implementation. Verdicts were all
changes requested. Implementation may start only after this section's
reconciliations are present.

| Reviewer | Verdict | Blocking findings | Reconciliation |
|---|---|---|---|
| Architecture | Changes requested | Source ranges/content hashes missing; query-time writer path underspecified; reverse ambiguous candidate expansion missing | Added D3, D14, D17 and explicit writer/query boundaries |
| Quality | Changes requested | No per-tool TDD matrix; response schemas not pinned; no-live-LLM rule declarative; e2e too shallow; stats underspecified; ADR-023 gates incomplete | Added D15, D16, D18, Section 5 TDD matrix, exact gates, and B.6a/B.6b e2e tasks |
| Reality | Changes requested | `content_hash` not planned; source metadata incomplete; `scope_rank` tie-breaker invalid; Filigree route reality needed correction | Added D14 hash/range mapping, valid tie-breakers, and corrected D7 to current PR 42 state |
| Systems | Changes requested | Continuation state not implementable; inferred cache key/materialization stale; coalescing key too weak; concurrent budget overrun; unresolved sites stale across reanalysis; seven-tool close cannot depend on fake Filigree | Removed continuation tokens, added D5 full cache/materialization design, D11 ledger, D4 content-keyed replace, and live-route exit criteria |
| Python engineering | Changes requested | Unresolved site wire contract absent; ambiguous candidate-only callers missed; source ranges/hashes not persisted; inferred writer matrix missing | Added D4 typed payload, D17 helper, D14 persistence, and writer-command TDD rows |

Open reviewer warnings intentionally left as implementation guardrails:

- `find_entity` does not search `summary_cache` in B.6; its docstring and tests
  say so.
- Accepted ADRs are not silently rewritten. B.6 adds dated resolution notes or
  a new/superseding ADR if a decision changes.
- `issues_for` is not a full close criterion until the committed Filigree
  reverse route is verified by a real contract test.
