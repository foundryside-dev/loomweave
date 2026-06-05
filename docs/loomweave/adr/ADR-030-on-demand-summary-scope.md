# ADR-030: On-Demand Summary Scope — Narrowing WP6 to MCP-Driven Generation

**Status**: Accepted
**Date**: 2026-05-16
**Deciders**: qacona@gmail.com
**Context**: WP6 in `v0.1-plan.md` scopes LLM summarisation as a batched pipeline pass: Phase 4 (leaf summaries), Phase 5 (module summaries), Phase 6 (subsystem summaries), all executed during `loomweave analyze`. Sprint 2's mid-sprint scope amendment narrows WP6 to the minimum the MVP MCP surface needs: `summary(entity_id)` as an on-demand MCP tool, computed lazily, cached on the existing 5-tuple cache key (ADR-007). The batched pipeline shape and the module/subsystem aggregation tiers move to v0.2. This ADR locks the narrowing and reconciles the summary-cache and inferred-edge-cache (ADR-028) shapes.

## Summary

Three decisions:

1. **`summary(entity_id)` is an MCP tool, not a pipeline phase.** Summaries are generated on the first MCP call that asks for them; subsequent calls hit the cache. `loomweave analyze` does not generate summaries unless explicitly requested via a `--prewarm-summaries` flag (deferred to v0.2; out of scope for the resumed Sprint 2).
2. **The cache key remains ADR-007's 5-tuple `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`.** No new components; ADR-030 does not amend ADR-007. The `inferred edges` cache (ADR-028 Decision 3) is a separate 4-tuple cache; the two are explicitly distinct cache shapes for distinct query types.
3. **Module-tier and subsystem-tier summarisation are deferred to v0.2.** The MVP MCP surface answers `summary(entity_id)` for leaf entities (functions, classes) and modules-as-leaves (where the module summary is a one-shot of the module's docstring + signature list, not an aggregation across leaf summaries). The hierarchical-aggregation shape from `system-design.md` §6 Phase 5–6 is not built in v0.1.

## Context

### What WP6 was scoped to do

The original `v0.1-plan.md` WP6 scope:

- `loomweave.yaml` config loader for LLM policy.
- `LlmProvider` trait with `OpenRouterProvider` + `RecordingProvider` (the latter from WP1 §5.1).
- Prompt templates for leaf / module / subsystem tiers.
- Five-tuple cache key (ADR-007) with TTL backstop and churn-eager invalidation.
- **Phase 4** — leaf summarisation (per-entity Haiku calls).
- **Phase 5** — module summarisation (aggregate leaf summaries).
- **Phase 6** — subsystem summarisation (aggregate module summaries).
- Phases gated behind RecordingProvider replay; live runs require `LOOMWEAVE_LLM_LIVE=1`.

This is a batched-pipeline design: every entity gets a summary during `loomweave analyze`, written to the cache, served from the cache by downstream surfaces (MCP, HTTP, briefings).

### Why the MVP doesn't need batched-pipeline summarisation

The MVP MCP surface (`summary(entity_id)` as one of the 7 tools) is a single-entity question from a consult-mode agent. The agent asks for the summary of the entity it's currently navigating around; it does not benefit from pre-computed summaries of the 50,000 other entities in the project. The batched-pipeline shape is valuable for the v0.2 briefings story (where a subsystem briefing is a digest of pre-computed module summaries, which are themselves digests of pre-computed leaf summaries), but briefings are not in the MVP scope.

The cost asymmetry makes the case sharper:

- **Batched pipeline at elspeth scale**: ~50,000 entities × ~1,500 tokens/leaf Haiku call ≈ 75M tokens upfront, well into the hundreds of dollars per `loomweave analyze` run.
- **On-demand**: the agent asks for ~10–100 summaries per consultation session. Cost is bounded by query traffic, not codebase size.

The 5-tuple cache key (ADR-007) already supports both shapes — a cache lookup is the same operation whether the cache was populated lazily or eagerly. Narrowing to lazy population costs nothing in cache architecture; the prompt templates and `LlmProvider` plumbing are unchanged.

### What about prewarming

A future operator may want to prewarm summaries for a subset of the project (high-traffic entities, recently-changed entities) so the first consult-mode query doesn't incur LLM latency. This is the right v0.2 enhancement: a `--prewarm-summaries [SELECTOR]` flag on `loomweave analyze` that drives the same `summary(id)` code path against a pre-selected entity set. The infrastructure for it lands in v0.1 (the MCP tool + cache); the orchestration around prewarming lands in v0.2.

### Why module/subsystem summarisation slips to v0.2

Hierarchical aggregation requires:

- A clustering pass (ADR-006 / ADR-032 / WP4 Phase 3 — Leiden / weighted-components). Deferred to v0.2 by the scope-amendment memo.
- A determinate ordering for aggregation (Phase 8's entity-set diff — deferred).
- An aggregation prompt that handles entity-count variability per module/subsystem.

None of these are MVP-blocking, but each is real design+implementation work. Slipping the tier to v0.2 lets the MVP surface land with a single-tier summary tool and adds the hierarchical tier when the surrounding pipeline is in place.

### Cache discipline: summaries vs. inferred edges

ADR-028 introduces an `inferred edges` cache keyed on `(caller_entity_id, caller_content_hash, model_id, prompt_version)` — a 4-tuple. ADR-007's summary cache is the 5-tuple `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`. The two are different shapes for different reasons:

- **Summary cache** includes `guidance_fingerprint` because summaries are conditioned on the guidance sheets that apply to the entity (a guidance change can change the summary even if the code hasn't). Inferred edges are not guidance-conditioned.
- **Summary cache** uses `model_tier`, which stores the concrete configured model ID. Under OpenRouter this is the `vendor/model` string (for example, `anthropic/claude-sonnet-4.6`). Inferred-edge cache uses `model_id` for the same concrete provider model. Different query paths, different cache keys.

Keeping the two caches structurally distinct prevents a future bug-class: a summary cache hit returning content rendered under a different LLM than the inference call expects, or vice versa. The two MCP tool paths are independent; their caches are independent.

## Decision

### Decision 1 — `summary(entity_id)` is an MCP tool, not a pipeline phase

WP6 ships:

- The `LlmProvider` trait (per WP1 §5.1).
- The `OpenRouterProvider` implementation.
- The `RecordingProvider` test wrapper.
- Prompt templates for the leaf tier (one template, versioned).
- The 5-tuple cache (ADR-007), populated lazily.
- The MCP `summary(entity_id)` tool dispatching through the cache + provider.
- Token-ceiling enforcement, retryable provider errors, and one concrete configured model ID for v0.1.

WP6 does NOT ship in the resumed Sprint 2:

- Pipeline Phase 4 / 5 / 6 orchestrator (the batched pass).
- Module and subsystem prompt templates.
- Hierarchical aggregation.
- Per-run cost-ceiling enforced across thousands of leaf calls (the per-query cost-ceiling still applies; the cross-query budget is an operator concern via `loomweave.yaml`).

The `loomweave analyze` pipeline in v0.1 produces no summaries. A consult-mode agent or operator calls `summary(entity_id)` via MCP when they want a summary.

### Decision 2 — Cache key remains ADR-007's 5-tuple

No amendment to ADR-007. The cache shape is:

```
cache_key = hash(
    entity_id,
    content_hash,
    prompt_template_id,
    model_tier,
    guidance_fingerprint,
)
```

For the MVP, `prompt_template_id` is always the leaf template's version string (e.g., `"leaf-v1"`). When v0.2 adds module and subsystem templates, `prompt_template_id` takes on additional values; the cache key shape doesn't change.

`guidance_fingerprint` is the SHA-256 of the sorted-ordered list of (guidance_sheet_id, content_hash) pairs that apply to this entity. For the MVP, guidance system is deferred (WP7); the fingerprint is `hash([])` = a constant. The cache key component is present from day one so v0.2's guidance integration is a content-of-fingerprint change, not a key-shape change.

### Decision 3 — Module/subsystem summarisation deferred to v0.2

The v0.1 MCP surface answers `summary(entity_id)` for:

- `function` entities — leaf summary of the function's docstring, signature, body (token-bounded).
- `class` entities — leaf summary of the class's docstring, method signatures, attribute list (token-bounded).
- `module` entities — **leaf-style** summary of the module's docstring + top-level entity list. NOT an aggregation of contained class/function summaries (that's the v0.2 module-tier shape).

If the consult-mode agent asks for `summary(module_id)` and expects an aggregated module-level summary, v0.1 returns the leaf-style "what's in this module" view. The MCP tool description names this scope explicitly so agents don't assume aggregation.

The v0.2 shape (Phase 4 / 5 / 6 hierarchical pass) is preserved in `system-design.md` §6 and `detailed-design.md` §4 as the target architecture; this ADR scopes only what ships in v0.1.

### 2026-05-18 OpenRouter swap amendment — session token ceiling

The OpenRouter provider swap resolves provider accounting as an MCP
server-session token budget:

- `loomweave.yaml` exposes `llm_policy.session_token_ceiling`, defaulting to 1,000,000
  tokens. The budget is process-local and resets when `loomweave serve` restarts.
- `summary()` and lazy inferred-edge dispatch share the same in-memory
  `BudgetLedger` with `spent`, `reserved`, and `blocked` state. Cache hits do
  not reserve or spend budget.
- Before a provider call, the MCP server reserves the provider's estimated
  request tokens. If `spent + reserved + estimate` exceeds the configured
  ceiling, the tool returns an MCP error envelope with code
  `token-ceiling-exceeded` without calling the provider.
- After a provider call, the reservation is released and OpenRouter-reported
  `usage.total_tokens` is debited. If the actual token total pushes the session
  over the ceiling, that response returns `token-ceiling-exceeded` and the
  session is blocked for further live LLM dispatch. Provider failures release
  the reservation.
- Ceiling responses emit `LMWV-LLM-TOKEN-CEILING-EXCEEDED` and
  `token_ceiling_exceeded_total: 1` in the MCP response envelope.

## Alternatives Considered

### Alternative 1 — Ship full WP6 (Phases 4-6) before the MVP MCP surface

Keep the original plan order; ship batched-pipeline summarisation before MCP exposure.

**Why rejected**: violates the MVP timing. Batched-pipeline summarisation at elspeth scale is hundreds of dollars per run and weeks of careful work (cache discipline, parallel rate-limit management, aggregation prompt design). None of it is on the critical path to "the agent can ask `summary(entity_id)` and get an answer."

### Alternative 2 — Skip summaries entirely in v0.1

Defer all LLM-bearing work to v0.2.

**Why rejected**: the MVP MCP surface's value proposition explicitly includes `summary(id)`. A consult-mode agent asking "what does this function do" and getting "no summary available, defer to v0.2" defeats the navigation-aid purpose. The minimum shippable summary is one-tier on-demand; that's the floor.

### Alternative 3 — Eager leaf-tier summarisation only (no module/subsystem)

Pre-compute leaf summaries during `loomweave analyze`; defer hierarchical tiers to v0.2.

**Why rejected**: still hundreds of dollars per `loomweave analyze` run for entities that may never be queried. The on-demand path is strictly cheaper at MVP query traffic (agent navigates a small subset of entities per consultation). The "prewarming" path described above lets an operator opt into eager population when they want it; that's a better default than imposing it on every run.

### Alternative 4 — Merge summary cache and inferred-edge cache into one shape

Add edge-vs-summary as a cache-key component; share one cache implementation.

**Why rejected**: the two caches have different conditioning (guidance for summaries, candidates for inferences) and different abstraction levels (`model_tier` vs `model_id`). Merging forces the lowest-common-denominator key shape, which loses the guidance dimension for summaries or adds noise to the inference cache. Two caches with similar discipline are clearer than one cache with a discriminator.

## Consequences

### Positive

- **MVP ships with $0 amortised LLM cost.** Operators who never call `summary()` pay nothing for LLM access. Operators who do pay per-query, bounded by their `loomweave.yaml` token ceiling and OpenRouter-side billing controls.
- **WP6 scope shrinks dramatically.** From three pipeline phases plus aggregation prompts plus parallel rate-limit management to: trait + provider + one prompt template + lazy cache population + one MCP tool. Order-of-magnitude reduction in implementation surface.
- **Cache shape locked early.** ADR-007's 5-tuple becomes battle-tested under the on-demand path before v0.2 adds the eager prewarming and hierarchical tiers. Cache bugs that would have been discovered in v0.2 under hierarchical load get found in v0.1 under interactive load.
- **v0.2 path is additive, not migratory.** Module/subsystem tiers add new `prompt_template_id` values to the same cache. Hierarchical aggregation is new pipeline code that consumes the existing cache. Nothing in v0.1 has to be rewritten or migrated.
- **The v0.1 provider is explicit.** Loomweave ships an OpenRouter-specific provider implementation using OpenAI-compatible Chat Completions HTTP. The trait remains in place, and the common wire format keeps future providers or proxies additive rather than a call-site rewrite.

### Negative

- **First MCP `summary` call is slow.** Cold cache + live LLM means ~1–3 seconds of latency on the first query for any entity. Acceptable for a consult-mode agent; operators wanting batch responsiveness can prewarm in v0.2.
- **The "summary" of a module is not what some agents will expect.** An agent reading `system-design.md` §6 sees the aggregation tiers described as v0.1 design; the MCP tool docstring needs to name the scope narrowing explicitly. Risk of agent confusion mitigated by clear tool description.
- **Three caches in the system.** Summary cache (ADR-007), inferred-edge cache (ADR-028), and the existing `entity_fts` SQLite FTS index. Each has its own invalidation rules. Documentation discipline: a single "Cache topology" section in `detailed-design.md` §"Cache" enumerates all three and their invalidation triggers.
- **`prewarm` flag eventually needs design.** Punting to v0.2 means an operator who wants prewarming today has to write their own loop over `summary(id)` calls. Workable, but not first-class.

### Neutral

- The `LlmProvider` trait shape is unchanged from WP1 §5.1. Same trait, same `RecordingProvider`, new `OpenRouterProvider`. The narrowing is on what calls into the trait, not the trait itself.
- The `--no-llm` mode (preserved from WP1) still works: with no LLM, `summary(id)` returns `{ available: false, reason: "llm-disabled" }` rather than blocking on an unreachable provider.

## Open Questions

- **Token-ceiling enforcement granularity** — RESOLVED by the OpenRouter swap:
  v0.1 uses a per-MCP-server-session ceiling configured by
  `llm_policy.session_token_ceiling`. Dollar budgets are handled in OpenRouter's
  operator-facing billing controls; a daily or cross-process aggregate ceiling
  is deferred beyond v0.1.
- **Cache eviction.** ADR-007 names a TTL backstop but no eviction policy for the lazy-populated case (the eager case naturally bounds cache size to entity count). Decision deferred; initial implementation can run unbounded with manual cache rotation, since v0.1 operators are the same humans who can `rm .loomweave/cache/`.
- **Multi-model cache coexistence.** If an operator changes `model_tier` in `loomweave.yaml`, the cache key changes and prior entries become unreachable but not garbage-collected. Same TTL discipline applies. Acceptable for v0.1.

## Related Decisions

- [ADR-007](./ADR-007-summary-cache-key.md) — summary cache key (5-tuple). ADR-030 narrows the population path; the key shape and invalidation rules are unchanged.
- [ADR-028](./ADR-028-edge-confidence-tiers.md) — inferred-edge cache shape. ADR-030 explains why the two caches stay structurally distinct.
- [WP6 in `v0.1-plan.md`](../../implementation/v0.1-plan.md#wp6--llm-dispatch-policy-engine-summary-cache-phases-46) — the work-package this ADR narrows. The full Phases 4–6 batched-pipeline shape is preserved for v0.2; the v0.1 scope is the trait + provider + one template + one MCP tool.
- [WP7 in `v0.1-plan.md`](../../implementation/v0.1-plan.md#wp7--guidance-system) — guidance system (deferred to v0.2 by the scope-amendment memo). The `guidance_fingerprint` cache key component is present from day one but evaluates to a constant in v0.1.
- [WP11 in `v0.1-plan.md`](../../implementation/v0.1-plan.md#wp11--post-implementation-costperf-validation) — cost validation spike. With on-demand summaries, WP11's $-per-run measurement becomes $-per-query-session, which is the more honest framing for a navigation-aid tool.

## References

- [Sprint 2 scope-amendment memo](../../implementation/sprint-2/scope-amendment-2026-05.md) — the larger context: MVP MCP surface, what stays in v0.1, what slips to v0.2.
- [`system-design.md` §6 "Analysis pipeline"](../v0.1/system-design.md#6-analysis-pipeline) — the v0.2-target hierarchical-aggregation pipeline; preserved as the architectural endpoint, not built in v0.1.
- [`detailed-design.md` §4 "Policy engine and caching"](../v0.1/detailed-design.md#4-policy-engine-and-caching) — the cache implementation reference.
