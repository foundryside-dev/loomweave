# ADR-007: Summary Cache Key — Five-Component Composite with TTL Backstop and Churn-Eager Invalidation

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: LLM summary cache is the primary cost-control mechanism in v0.1; key design decides whether cache hits correlate with "nothing meaningful changed"

## Summary

The summary cache is keyed on **five components**: `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`. Any component mismatch is a miss; all five must match for a hit. This makes **syntactic staleness impossible** — any code edit, template change, model tier rename, or guidance-sheet edit changes a component and forces re-summarisation. Three **semantic-staleness** paths the key alone doesn't catch (graph-neighbourhood drift, model-identity drift, guidance-worldview drift) are handled with targeted mechanisms: the cache row stores neighbourhood statistics and is flagged `stale_semantic: true` when they shift by >50%; model-identity drift is already caught because the `model_tier` component stores the concrete model ID (not the tier name); guidance-worldview drift is surfaced via `CLA-FACT-GUIDANCE-CHURN-STALE` findings combined with **churn-eager invalidation** — cache rows whose `guidance_fingerprint` includes a churn-stale sheet are invalidated immediately rather than waiting for TTL. A **TTL backstop** (180 days default) invalidates any row older than the bound on next query.

## Context

`NFR-COST-01` ($15 per run ± 50%), `NFR-COST-02` (≥95% cache hit rate after stabilisation), and `NFR-PERF-01` (≤60 min wall-clock) all depend on the summary cache hitting when it should and missing when it should. A cache that hits on stale content is a correctness bug (agents consume outdated briefings); a cache that misses on unchanged content is a cost bug (every run re-pays the full LLM spend).

The cache stores one row per `(entity × content × template × model × guidance)` combination; at elspeth scale ~200k rows (`detailed-design.md:778`). Hit rate below NFR-COST-02's target means every miss costs a Haiku, Sonnet, or Opus call depending on entity kind — hundreds to thousands of dollars difference over a project's lifetime.

Two pressures shape the key:

1. **Syntactic pressures** — any change that should invalidate a cached briefing must flip at least one key component. Code edits (hit → miss via `content_hash`), template bug fixes (miss via `prompt_template_id`), model tier upgrades (miss via `model_tier`), guidance sheet edits (miss via `guidance_fingerprint`).

2. **Semantic pressures** — changes that should invalidate but don't flip key components. A module's `content_hash` is unchanged but it became a hot path the briefing now misrepresents; a tier name `sonnet` mapped to `claude-sonnet-4-6` maps to `claude-sonnet-4-7` now; a guidance sheet unchanged in text has gone stale in worldview. The key alone cannot see these; targeted mechanisms around the key fill the gap.

The panel's cost-critique review noted that the assumed 95% hit rate is a design-time projection, not an empirically-validated number. Block C1 (cost-model spike) is the validation mechanism; this ADR ships the cache design the spike measures.

## Decision

### Five-component composite key

```
(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)
```

Each component:

- **`entity_id`** — the target being summarised (e.g., `python:class:auth.tokens.TokenManager`). Flips on entity rename/move beyond the 80% case ADR-003 handles.
- **`content_hash`** — BLAKE3 over the entity's source-slice bytes. Flips on any code edit.
- **`prompt_template_id`** — e.g., `python:class:v1` (plugin-declared, ADR-022 manifest `prompt_templates`). Flips on template revision — a plugin bumping `v1` → `v2` forces re-summarisation of every entity of that kind.
- **`model_tier`** — stores the concrete model ID (`claude-sonnet-4-6`), not the tier name (`sonnet`). Under OpenRouter, this is the configured `vendor/model` string verbatim (for example, `anthropic/claude-sonnet-4.6`). The tier resolver compares on write and treats a tier-to-model remap as a miss. Handles model-identity drift without special casing.
- **`guidance_fingerprint`** — BLAKE3 over the sorted, concatenated content of all guidance sheets active for this entity's query context. Any guidance sheet edit flips the fingerprint.

All five combined are the `summary_cache` table's PRIMARY KEY (`detailed-design.md:679-691`).

### TTL backstop

Rows older than `clarion.yaml:llm_policy.caching.max_age_days` (default **180 days**, configurable) are invalidated unconditionally on next query. The TTL catches semantic-staleness paths the key alone doesn't see, bounds worst-case stale-summary age, and forces refresh when a full Anthropic model generation has shipped.

180 days chosen as the intersection of:

- Long enough that cache hit rates stay high during active development windows (few-month feature cycles).
- Short enough that stale models don't persist across a generation boundary.
- Short enough that guidance-worldview drift has a hard ceiling.

### Churn-eager invalidation

When `CLA-FACT-GUIDANCE-CHURN-STALE` fires (stale `critical: true` guidance sheet on a high-churn entity), every `summary_cache` row whose `guidance_fingerprint` includes that sheet is invalidated **eagerly**, not at TTL. The operator now sees the churn-stale finding *and* experiences cost pressure (the next few analyses cache-miss on affected entities), which creates an action-forcing loop.

Implementation: `CLA-FACT-GUIDANCE-CHURN-STALE` emission Phase-7 runs a `DELETE FROM summary_cache WHERE guidance_fingerprint LIKE '%{sheet_hash}%'` under the writer actor. Bounded by the number of affected rows; O(affected_rows) time.

### Neighbourhood-drift flag

Each cache row additionally stores `caller_count` and `fan_out` at the time of summary generation. On each read, the consult path compares these against the current graph state. If either has shifted by more than `clarion.yaml:llm_policy.caching.neighborhood_drift_threshold` (default **50%**), the briefing is returned with `stale_semantic: true` in the response envelope. Agents consuming the briefing downweight `risks` and `relationships` claims accordingly.

This is a *flag*, not a miss. Forcing a miss on every graph-topology change would tank NFR-COST-02's hit rate. The flag lets agents reason about staleness; the next `clarion analyze` refreshes flagged entities.

### Explicit non-decisions (v0.2 territory)

Per Q1 scope commitment (`v0.1-scope-commitments.md:68`), summary-cache optimisations "beyond a simple in-memory cache" defer to v0.2. That phrasing covers, specifically:

- Cross-session cache warm-up (pre-computing summaries for unvisited entities).
- Semantic drift detection beyond the neighbourhood-stats flag (e.g., downstream-impact-analysis-based invalidation).
- Distributed / shared-team cache (useful only once `.clarion/clarion.db` is routinely git-committed and merged across team members — v0.2 concern).
- Per-agent cache affinity / multi-tenant scoping.

v0.1 ships the SQLite-backed `summary_cache` table with the 5-part key, TTL backstop, churn-eager invalidation, and neighbourhood-drift flag. Everything else waits for v0.2.

## Alternatives Considered

### Alternative 1: Single-component key (just `entity_id`)

**Pros**: simplest possible design.

**Cons**: code edits don't invalidate (briefings stale instantly); template changes don't invalidate; model upgrades don't invalidate; guidance edits don't invalidate. The cache is correct only while nothing has changed — which is rarely.

**Why rejected**: loses correctness for marginal simplicity gain.

### Alternative 2: Two-component (`entity_id, content_hash`)

**Pros**: catches code edits; simple.

**Cons**: template bug fixes, model upgrades, and guidance edits silently serve stale briefings. Every prompt-template revision in a plugin would require cache-wipe instructions to operators. Guidance sheets become silent no-ops against the cache.

**Why rejected**: key underspecifies; silent-staleness surface.

### Alternative 3: Content-addressed (hash the full composed prompt)

Cache key is the hash of the complete rendered prompt (template + entity content + guidance + model).

**Pros**: naturally handles every variation; one component.

**Cons**: can't explain cache misses ("which of the five things changed?"); can't invalidate by policy ("all Python-function summaries" or "all briefings using sheet X"); aggregate diagnostics (hit-rate by template, by model tier) become impossible.

**Why rejected**: hides the audit surface. Operators need to reason about cache behaviour; a single opaque hash makes that impossible.

### Alternative 4: Five-component key without TTL backstop

**Pros**: maximum hit rate; no forced refresh.

**Cons**: semantic-staleness paths the key doesn't catch (neighbourhood drift past the 50% flag threshold, unchanged guidance whose underlying assumptions went stale, persistent model-identity issues after a generation boundary) accumulate indefinitely. A briefing from 2 years ago is served today with no refresh pressure.

**Why rejected**: semantic correctness bound needs a fallback. TTL is the bound.

### Alternative 5: Six-component key with neighbourhood-stats as a component

Add `caller_count + fan_out` into the primary key.

**Pros**: graph drift invalidates automatically; no flag-interpretation needed by agents.

**Cons**: every graph change (every `imports` edge added to any member of a fan-out) flips the component and forces a miss. At elspeth scale this would cause cascading re-summarisations on every incremental analyse. NFR-COST-02 (95% hit rate) becomes impossible.

**Why rejected**: correctness-for-cost trade swings the wrong way. The flag is the right midpoint.

### Alternative 6: Tier-only `model_tier` (store the tier name, not the concrete model)

Store `"sonnet"` instead of `"claude-sonnet-4-6"`.

**Pros**: tier-name stability across model generations; simpler display.

**Cons**: when `sonnet` remaps to `claude-sonnet-4-7`, every row with `model_tier = "sonnet"` is still a hit — serving briefings from the old model. Model-identity drift silently wrong.

**Why rejected**: loses the one behaviour model-identity drift actually needs.

## Consequences

### Positive

- Syntactic staleness is impossible. Every change that *should* invalidate flips a component.
- TTL backstop bounds worst-case semantic-staleness exposure. 180 days is the effective ceiling on any briefing's freshness.
- Churn-eager invalidation creates cost pressure on guidance neglect. Operators feel the friction of stale critical guidance in their Anthropic bill, not just in `CLA-FACT-*` findings.
- Neighbourhood-drift flag gives agents actionable staleness information without tanking hit rate.
- Cache-miss reasoning is explainable. A 5-tuple mismatch can be inspected: "which component changed?" → actionable diagnosis.

### Negative

- 5-component composite primary key has nontrivial index overhead. Lookup is a 5-way PK scan; reasonable performance on SQLite but not free. Mitigation: the PK is the natural access pattern (no secondary indexes on the cache needed).
- At ~200k rows × JSON summaries, the `summary_cache` table is the largest single table in `clarion.db` (500 MB – 2 GB range). Operators on constrained disks need to know. Mitigation: `clarion cache prune --older-than <days>` CLI exists for manual reduction.
- NFR-COST-02's 95% hit-rate assumption is design-time. Block C1 is the validation; this ADR ships the design the spike measures. If Block C1 shows hit rate materially below 95%, the decision surface is whether to raise the target (accept lower hit rate) or adjust the key / TTL.
- Neighbourhood-drift flag relies on agents correctly downweighting `stale_semantic: true` briefings. If agent implementations ignore the flag, correctness degrades silently. Mitigation: the response-envelope convention is documented; MCP tool responses carry the flag prominently.

### Neutral

- The cache is SQLite-backed, same store as entities. Clarion's git-commit posture (ADR-005) keeps the cache available across developer machines; team members pulling the committed DB benefit from each other's summaries.
- Churn-eager invalidation lands through the writer actor (ADR-011). Large invalidations (thousands of rows at once) are per-N-files transactions just like analyse writes.
- `model_tier` storing the concrete model ID is not a surprise — it's the data that was already being stored; the ADR is just making "why that's correct" explicit.

## Related Decisions

- [ADR-001](./ADR-001-rust-for-core.md) — Rust + SQLite commitment. The cache is an SQLite table within the ADR-001 ecosystem.
- [ADR-003](./ADR-003-entity-id-scheme.md) — `entity_id` is the first cache-key component; ADR-003's symbolic canonical names make the cache survive file moves that don't change the canonical name.
- [ADR-011](./ADR-011-writer-actor-concurrency.md) — churn-eager invalidation issues bulk `DELETE` through the writer actor; per-N-files batching covers it.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — plugin-declared `prompt_template_id` is a cache-key component; ADR-022 guarantees the core treats it as an opaque string.

## References

- [Clarion v0.1 detailed design §4 (Summary cache key design)](../v0.1/detailed-design.md) (lines 965-983) — the target this ADR formalises.
- [Clarion v0.1 detailed design §3 schema — `summary_cache` table](../v0.1/detailed-design.md) (lines 679-691) — storage shape.
- [Clarion v0.1 requirements — NFR-COST-01, NFR-COST-02, NFR-PERF-01](../v0.1/requirements.md) — cost/performance envelopes this cache design serves.
- [Clarion v0.1 scope commitments — Q1](../v0.1/plans/v0.1-scope-commitments.md) (line 68) — v0.2 deferrals: cache optimisations beyond the simple shape.
- [Clarion v0.1 scope commitments — Validation](../v0.1/plans/v0.1-scope-commitments.md) (lines 237-247) — empirical validation plan for NFR-COST-01/02 and the 95% hit-rate assumption this ADR depends on.
