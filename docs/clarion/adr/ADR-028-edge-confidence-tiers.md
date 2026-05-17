# ADR-028: Edge Confidence Tiers

**Status**: Accepted
**Date**: 2026-05-16
**Deciders**: qacona@gmail.com
**Context**: Sprint 2's mid-sprint scope amendment (`docs/implementation/sprint-2/scope-amendment-2026-05.md`) adds plugin-emitted `calls` and `references` edges as part of the MVP MCP surface. The Python plugin will use pyright as the resolution engine. Pyright resolves some call sites confidently, returns multiple candidates for others (Protocol dispatch, dict-of-callables, duck-typed sites), and cannot resolve dynamic dispatch at all. The existing design (`detailed-design.md:105-106`) names a manifest-level `confidence_basis` capability (`ast_match`, `name_match`, ...) — a *static* per-edge-kind claim about how the plugin produces edges. That is insufficient for a consult-mode agent asking "is this call edge ground truth or a guess?" — different call sites in the same `calls` edge kind have different epistemic weight. This ADR lifts confidence from a per-edge-kind manifest claim to a per-edge runtime tier, and specifies how unresolved call sites become tiered edges instead of silent omissions.

## Summary

Three decisions, locked together:

1. **Three confidence tiers on every `calls` / `references` edge**: `resolved` (pyright/AST resolved the symbol unambiguously), `ambiguous` (resolution returned N>1 candidates from static analysis; no LLM was consulted), `inferred` (no static resolution; an LLM was asked to guess the callee from caller context). The tier is a load-bearing field on the edge wire shape and storage row.
2. **MCP query default is `confidence >= resolved`.** Tools like `callers_of`, `execution_paths_from`, `neighborhood` accept a `confidence` parameter; absent the parameter, only `resolved` edges are returned. Inferred edges must be explicitly opted into. This prevents the consult-mode agent from treating LLM hallucinations as ground truth.
3. **Inferred edges are lazy-computed at MCP query time, not at scan time.** Static analysis produces only `resolved` and `ambiguous` edges during `clarion analyze`. The first MCP query that touches an unresolved call site triggers an LLM call (subject to the same `LlmProvider` discipline as summaries — RecordingProvider in tests, cost ceiling, model-tier mapping). Results cache keyed on the content hash of the caller and the candidate set.

## Context

### What pyright actually produces

Pyright is not a single JSON dump. The call-resolution path inside the Python plugin is roughly:

- For each call expression in the AST, walk to the called symbol.
- If pyright's type inference resolves the symbol to one function/method declaration in the project, the edge is `resolved`.
- If pyright sees a union or Protocol that maps to N>1 candidates inside the project, the edge is `ambiguous`. (Examples: `handlers[event_type](payload)` where `handlers` is `dict[str, Callable]`; a Protocol method whose implementations the analyzer can enumerate; a parameter typed as a Union of callables.)
- If the call expression's target is a name pyright cannot resolve to any in-project entity (dynamic dispatch, `getattr`, decorator-modified callables it cannot trace through, calls into untyped third-party code), no edge is produced statically.

The `references` edge kind is the same shape modulo "mention without invocation" — pyright's cross-reference index resolves most module-level name lookups; ambiguity is rarer than for calls.

### Why a static `confidence_basis` is not enough

`detailed-design.md:105-106` says `calls` has `confidence_basis: ast_match` and notes inline that this is "reliable for direct same-scope calls; approximate (name-match) for method calls". This is a manifest-time *capability claim*, not a per-edge label. A consumer of a single `calls` edge cannot tell from the manifest whether *that specific edge* came from a same-scope name match (high confidence) or a Protocol-dispatch heuristic (low confidence). For an MCP tool returning `callers_of(target)`, the difference is the difference between "these definitely call you" and "one of these probably calls you, and an LLM made up the others."

### Why incomplete graphs are not "honestly incomplete"

The alternative to confidence tiers is silently skipping unresolved call sites — ship a clean graph of only `resolved` edges. This is honest about what static analysis knows, but it lies by omission to the consult-mode agent: the agent reasons over the graph as if it were complete, so a missing edge reads as "this function has no callers" rather than "we don't know who calls this function." For navigation tools, missing edges are worse than uncertain ones because the agent has no signal that uncertainty exists.

The `ambiguous` tier captures exactly the case static analysis is best suited to expose: "pyright sees N candidates, pick one." That is the question the consult-mode agent is good at answering; surfacing it as an `ambiguous` edge with the candidate set in `properties.candidates` turns it into a productive query.

### Why inferred edges are lazy, not eager

Pre-computing LLM-inferred edges for every unresolved call site in elspeth (~425k LOC, plausibly ~50k unresolved sites) is an upfront LLM bill in the high hundreds to low thousands of dollars for value most queries never consume. Most unresolved call sites are in code paths the agent will never query. The right time to spend the LLM tokens is when the query path actually reaches the edge.

Lazy computation also makes the cost surface visible to the operator: the `summary(id)` / `execution_paths_from(id, confidence>=inferred)` calls incur LLM spend at query time, the same way `summary` does. The same RecordingProvider (ADR-007) replay discipline applies.

## Decision

### Decision 1 — Three confidence tiers

`RawEdge` (ADR-026 wire shape) gains a `confidence` field:

```rust
pub struct RawEdge {
    pub kind: String,
    pub from_id: String,
    pub to_id: String,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
    pub confidence: EdgeConfidence,  // NEW — required for calls/references; defaults to Resolved for structural edges
    #[serde(default)]
    pub properties: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum EdgeConfidence {
    Resolved,
    Ambiguous,
    Inferred,
}
```

`EdgeConfidence` is `PartialOrd + Ord` with `Resolved < Ambiguous < Inferred` (lower = more trustworthy); query-side `confidence >= Resolved` filters return only `Resolved` edges. The ordinal is "permissiveness", not "epistemic strength" — naming it this way means `confidence >= X` reads as "include tiers as permissive as X or stricter," matching how it appears in MCP tool signatures.

The `confidence` column is added to the `edges` schema:

```sql
ALTER TABLE edges ADD COLUMN confidence TEXT NOT NULL DEFAULT 'resolved'
    CHECK (confidence IN ('resolved', 'ambiguous', 'inferred'));
CREATE INDEX ix_edges_kind_confidence ON edges(kind, confidence);
```

Per ADR-024's edit-in-place migration policy, this lands as a modification to `0001_initial_schema.sql` rather than a new migration file — Clarion has not published a build that writes a `calls` or `references` row, so no consumer-side migration is required. (The policy retires the first time an external operator pulls a published build; ADR-028's amendment of `0001` is the last edit it permits to the edges table.)

**Per-kind invariant.** Plugins emitting `calls` or `references` MUST emit `confidence`. Plugins emitting structural edges (`contains`, `in_subsystem`, `guides`, `emits_finding`) MUST emit `confidence = Resolved` (these edges are facts about graph structure, not inferences). The writer-actor enforces this by kind; an edge violating it is rejected with `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT`.

**`properties.candidates` for ambiguous edges.** An `ambiguous` edge whose `to_id` is the agent's best static guess MAY carry `properties.candidates: ["id1", "id2", ...]` listing the other candidates pyright surfaced. This lets `callers_of` / `execution_paths_from` expand a single ambiguous edge into the full candidate set when the consumer asks. The properties shape is documented per-kind in the plugin's manifest.

### Decision 2 — MCP query default is `confidence >= Resolved`

All MCP tools that traverse `calls` or `references` edges accept a `confidence` parameter typed as `EdgeConfidence`. The parameter's default is `Resolved`. Tools affected (initial set; see ADR-029 / scope-amendment memo for the full list):

- `callers_of(entity_id, confidence: Resolved)` — returns only resolved-tier callers by default.
- `execution_paths_from(entity_id, max_depth: 3, confidence: Resolved)` — DFS does not traverse `ambiguous` / `inferred` edges by default.
- `neighborhood(entity_id, confidence: Resolved)` — one-hop view defaults to resolved edges only.
- `references_to(entity_id, confidence: Resolved)` — same.

The MCP tool descriptions surface the parameter and its semantics explicitly so the consult-mode agent knows the tier exists and can opt up. Default-resolved is non-negotiable: a tool that silently returns inferred edges leaks LLM hallucinations into the agent's reasoning surface.

### Decision 3 — Inferred edges are lazy-computed at MCP query time

Scan-time (`clarion analyze`) populates only `resolved` and `ambiguous` edges. The plugin's pyright-walk emits both tiers; no LLM is invoked during analyze.

At MCP query time, when a tool is called with `confidence >= Inferred` and the traversal reaches an entity with unresolved call sites (sites where pyright produced no edge at all), the MCP server:

1. Looks up the cached inferred edges for that entity, keyed on `(caller_entity_id, caller_content_hash, model_id, prompt_version)`.
2. On cache miss, dispatches an LLM call via the `LlmProvider` trait (ADR-007 / WP1 §5.1) asking the model for a best-guess list of callees from the entity's body. The response shape is a list of `(target_id, candidate_confidence_float, rationale)` triples bounded by a per-call token budget.
3. Writes the resulting `Inferred` edges into the `edges` table (or a sibling `inferred_edges` cache table; see Open Question §"Storage location for inferred edges" below).
4. Returns them as part of the query result, tagged `confidence: Inferred`.

The same cost ceiling, retry, and RecordingProvider replay discipline that governs `summary(id)` (ADR-007, ADR-030) governs inferred-edge dispatch.

**`MAX_INFERRED_EDGES_PER_CALLER`.** A single LLM call returns at most N inferred candidate callees per caller entity (default 8, configurable in `clarion.yaml`). Pyright's unresolved-site count for the caller bounds the cost: if the caller has zero unresolved sites, no LLM call fires regardless of `confidence >= Inferred`.

### 2026-05-17 B.6 implementation resolution — query-time inference

B.6 resolves the deferred queryability and writer-path details as follows:

- Unresolved call sites are stored in a sibling `entity_unresolved_call_sites`
  table, not as Filigree findings and not as synthetic placeholder `edges`
  rows. The table is keyed by caller entity, caller content hash, and a stable
  site key; re-analysis replaces rows for the current caller hash and deletes
  rows for older hashes. This keeps unresolved sites queryable without making
  `neighborhood` or `execution_paths_from` traverse fake callees.
- Query-time inference writes through the writer actor via
  `WriterCmd::InsertInferredEdges`. The scan-time `WriterCmd::InsertEdge`
  contract still rejects `calls` edges with `confidence='inferred'`, so the
  call site makes the cache-write origin explicit.
- B.6 keeps both an `inferred_edge_cache` table and materialized
  `edges(confidence='inferred')` rows. Cache rows are keyed by
  `(caller_entity_id, caller_content_hash, model_id, prompt_version)`.
  Materialized rows carry the cache key, model id, prompt version, unresolved
  site key, optional model confidence, and optional rationale in `properties`.
- If a static `resolved` or `ambiguous` `calls` edge already exists for the same
  `(from_id, to_id)`, `InsertInferredEdges` skips the inferred materialization
  and reports it in writer stats instead of downgrading static evidence.
- Concurrent cold MCP queries for the same inference cache key are coalesced by
  the server in-flight registry. Waiters subscribe to the owner result and
  report `inferred_dispatch_coalesced_total` in their response stats.

## Alternatives Considered

### Alternative 1 — Two tiers (resolved / inferred)

Collapse `ambiguous` into `inferred`.

**Why rejected**: ambiguity and inference are different signals. Ambiguity is "static analysis sees N candidates" — the LLM (or the human agent) can disambiguate without re-inventing the candidate set. Inference is "we have no static candidates; here is a guess." Collapsing them throws away the candidate set, which is the most useful artefact pyright produces in the ambiguous case. The two-tier model also forces a binary choice on the consumer: trust the LLM or get nothing. Three tiers give the consumer a middle path: "give me static-analysis output including the ambiguous cases, without spending LLM tokens."

### Alternative 2 — Floating-point confidence score per edge

`confidence: f32` from 0.0 to 1.0; no tiers.

**Why rejected**: false precision. Pyright does not produce a probability; it produces a deterministic resolution outcome. An LLM-inferred edge could carry a confidence score from the model, but mixing pyright-source edges (which would all be 1.0 or thereabouts) with LLM-source edges on the same numeric axis erases the source distinction the consumer cares about. Tiers preserve the *provenance* of the confidence claim, which is what determines how the consumer should treat the edge. Per-LLM-inference confidence (the model's own score) MAY ride in `properties.model_confidence` for the rare consumer that wants it.

### Alternative 3 — Eager inferred-edge computation at scan time

Compute inferred edges for every unresolved site during `clarion analyze`.

**Why rejected**: cost at scale. At elspeth's ~425k LOC with the unresolved-site density typical of mid-typed Python code, the upfront LLM bill is in the hundreds-to-thousands-of-dollars range for value most queries never consume. The lazy path makes the cost proportional to actual query traffic; the eager path makes it proportional to codebase size. The eager path also blocks every `clarion analyze` run on LLM availability, which conflicts with the existing `--no-llm` mode (still supported per WP1 §5.1 and `system-design.md:580`).

### Alternative 4 — Inferred edges live only in a separate read-only cache table, never in `edges`

`edges` only ever holds `resolved` + `ambiguous` rows; inferred rows live in `inferred_edges_cache` and the MCP server joins them at query time.

**Why partially deferred**: this is a viable shape; it has the appeal of keeping the `edges` table pure-static (auditable: "every row here was produced by analyze, not by a query"). The downside is that two tables need parallel indexes for traversal queries, and joins at query time add latency. Decision deferred to the B.4* implementation pass — the engineer implementing inferred-edge dispatch picks the storage layout based on observed write-path complexity. The wire shape and confidence enum are language-of-discourse for both layouts.

## Consequences

### Positive

- **Honesty under uncertainty.** The graph reports what it knows and how it knows it. Ambiguous edges turn pyright's hardest-to-resolve cases into productive questions for the agent rather than silent omissions.
- **Default-safe MCP surface.** Agents who never opt into inferred edges see only ground-truth static analysis. Agents who do opt in carry the responsibility for treating inferences as inferences.
- **Cost scales with query traffic.** No analyze-time LLM bill for inferred edges. Operators who never hit `confidence >= Inferred` queries pay zero LLM cost on the inference path.
- **Provenance is preserved.** A consumer reading an edge can tell whether the claim came from AST parsing, type-resolution disambiguation, or LLM guessing — without parsing source-code metadata.

### Negative

- **Three places to enforce the per-kind invariant.** Plugin emission, writer-actor accept, MCP query path. A bug in any of the three leaks the wrong tier into the consumer. The writer-actor invariant (`CLA-INFRA-EDGE-CONFIDENCE-CONTRACT`) is the load-bearing one; plugin-side bugs surface as rejected runs rather than silent corruption.
- **MCP tools grow a `confidence` parameter.** Every traversal tool has one more argument. The default mitigates this for casual use, but the tool surface is wider than the v0.1-scope-commitments memo originally specified.
- **Cache key has one more component.** The summary cache key (ADR-007) is a 5-tuple. The inferred-edge cache key is a 4-tuple `(caller_entity_id, caller_content_hash, model_id, prompt_version)`. Different shape, similar discipline. ADR-030 reconciles the two caching shapes.
- **`#[serde(rename_all = "lowercase")]`** on `EdgeConfidence` couples the wire representation to the Rust enum name casing. Renaming the enum requires a wire-protocol pass. This is a tolerable trade.

### Neutral

- The `ambiguous` tier requires plugins to surface the candidate set. For pyright-based emission this is mechanical; for hypothetical future plugins using cheaper resolution engines, the `candidates` field is optional but recommended.
- ADR-006's Leiden clusterer reads `imports` and `calls` edges. The clusterer should weight `resolved` calls heavier than `ambiguous` and ignore `inferred` entirely (clustering is a stability decision; LLM-inferred edges are a query-time concern). The clusterer's reading discipline is documented in the B.4* design doc, not here.

## Open Questions

- **Storage location for inferred edges** — RESOLVED by B.6: inferred results
  are cached in `inferred_edge_cache` and materialized into the same `edges`
  table with `confidence='inferred'` for traversal. This is additive to the
  B.4* confidence-column decision and preserves the existing graph indexes.
- **Unresolved-call-site queryability** — RESOLVED by B.6: unresolved sites live
  in `entity_unresolved_call_sites`; they are not per-site Filigree findings and
  not synthetic zero-candidate edges.
- **Inference-result longevity** — RESOLVED by B.6 for v0.1: caller content-hash
  changes invalidate the inference cache key and re-analysis removes old-hash
  unresolved-site anchors. Candidate target churn is handled by the cache row's
  materialization step; static duplicate edges win over inferred rows.
- **Per-model confidence-tier mapping** — if Haiku produces an inference and Sonnet produces a different inference for the same caller, are both stored? Initial answer: keyed on `model_id`, so yes; query path returns the model the operator's `clarion.yaml` names as the inference tier. Refinable in ADR-030.

## Related Decisions

- [ADR-026](./ADR-026-containment-wire-and-edge-identity.md) — edge wire shape. ADR-028 adds the `confidence` field to `RawEdge`; the rest of ADR-026's shape (envelope, per-kind source-range contract, natural PK identity) is unchanged.
- [ADR-007](./ADR-007-summary-cache-key.md) — summary cache key. The inferred-edge cache is a peer; ADR-030 reconciles the two cache shapes.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — core/plugin ontology boundary. `calls` and `references` are plugin-emitted edge kinds; the confidence tiers ride on the plugin's emission. The core enforces the per-kind invariant but does not invent confidence values.
- [ADR-024](./ADR-024-guidance-schema-vocabulary.md) — edit-in-place migration policy. ADR-028's `confidence` column on `edges` lands under that policy.

## References

- [Sprint 2 scope-amendment memo](../../implementation/sprint-2/scope-amendment-2026-05.md) — the larger context: pyright integration, MVP MCP surface, where confidence tiers fit in the new sprint shape.
- [`detailed-design.md` §"Python plugin specifics — call graph precision"](../v0.1/detailed-design.md#python-plugin-specifics-call-graph-precision) — the prior text this ADR formalises.
- [B.4* design doc — calls edges + pyright + confidence] — to be written as part of the resume; cites this ADR.
