# ADR-040: Semantic search — opt-in embeddings, sidecar storage, cosine scan

**Status**: Accepted
**Date**: 2026-06-02
**Deciders**: john@pgpl.net (with Claude)
**Context**: WS5b (advanced MCP queries) splits two capabilities out of the WS5 stateless catalogue that need infrastructure beyond a catalog query: `search_semantic` (this ADR) and `find_dead_code` (no ADR — pure graph query). `search_semantic` ranks entities by embedding similarity to a natural-language query, so it needs an embedding provider, vector storage, and a similarity scan. This ADR records the opt-in posture, the provider abstraction, the storage location, and the cost-governance hook.
**Relates to**: [ADR-005](./ADR-005-clarion-dir-tracking.md) (git-committable `.clarion/` — this ADR **extends** its gitignore list by its own authority; ADR-005 is unchanged), [ADR-007](./ADR-007-summary-cache-key.md) (content-hash cache invalidation — mirrored), [ADR-030](./ADR-030-on-demand-summary-scope.md) (on-demand / policy-engine cost posture), [ADR-039](./ADR-039-llm-provider-pivot-openrouter-cli.md) (the `LlmProvider` abstraction this mirrors).
**Glossary verdict**: **no clash** — `search_semantic`, `EmbeddingProvider`, and `entity_embeddings` are Clarion-local; no sibling product uses these terms. No `glossary.md` change required.

## Summary

Five decisions.

1. **Opt-in, off by default (local-first doctrine).** Embeddings are disabled by default, exactly like `llm_policy`. Loom is local-first and single-binary; nothing here makes a hosted service *required*. When semantic search is off, `search_semantic` returns an honest `result_kind: "not_enabled"` with a missing-signal note — never a faked or empty-as-if-complete result. Config lives under `semantic_search:` (`enabled`, `allow_live_provider`, `provider`, `model_id`, `dimensions`, `endpoint_url`, `api_key_env`, `timeout_seconds`, `session_token_ceiling`).

2. **`EmbeddingProvider` trait — the trait, not the choice, is load-bearing (D-WS5b-1 resolved).** `clarion_core::embedding_provider::EmbeddingProvider` mirrors `LlmProvider`: `embed(&[String]) -> Vec<Vec<f32>>`, `dimensions()`, `model_id()`, `estimate_tokens()`. Two impls ship: `RecordingEmbeddingProvider` (deterministic tests, like `RecordingProvider`) and `ApiEmbeddingProvider` (OpenAI-compatible `/embeddings` endpoint — OpenAI / Voyage / Cohere-class — opt-in + key, honest degrade when key/network absent). A bundled local-model impl (`candle`/`ort`) is **deferred follow-on work behind the same trait**: the API-endpoint impl ships first so a network/key is opt-in, and the local impl is a second provider, not a re-architecture.

3. **Sidecar storage protects the committed DB (extends ADR-005).** Embeddings are large (≈ entities × dim × 4 bytes) and rebuildable, so they must **not** bloat the committed `.clarion/clarion.db`. They live in a separate, git-ignored `.clarion/embeddings.db` (`entity_embeddings(entity_id, content_hash, model_id, dim, vec BLOB, cost_usd, tokens_input, created_at, last_accessed_at)`, PK `(entity_id, content_hash, model_id)`). Because it is a private rebuildable cache, the sidecar carries its own self-contained `CREATE TABLE IF NOT EXISTS` schema (not a row in the committed-DB migration chain) and is exempt from the `application_id` foreign-DB guard. ADR-005's gitignore default list is extended with `embeddings.db` (WAL files already covered by `*.db-wal`/`*.db-shm`). **ADR-005 itself is not edited** — this ADR adds the entry by its own authority.

4. **Bounded exact cosine scan (no ANN dependency at v1).** Stored f32 vectors are scanned with exact cosine over the candidate set (scope-pre-filtered), capped + paginated. Only embeddings whose `content_hash` matches the entity's *current* hash are considered, so stale vectors never surface (freshness, exactly like the summary cache). A scalable ANN backend (sqlite-vec / HNSW) is a **named follow-on, flagged not pre-built** — added only if the exact scan misses the latency bar (NFR-PERF-02).

5. **Cost governed by the policy engine.** Embeddings are computed at analyze time (opt-in sub-phase, after leaf summarisation) over `short_name + summary purpose + docstring`, skipping unchanged `(entity_id, content_hash)`. Embedding cost folds into the same dry-run estimate + budget-reservation + `on_exceed` flow as LLM work, governed by `semantic_search.session_token_ceiling`. The `search_semantic` query embeds one short string per call — negligible — and is not separately ledgered.

## Decision

Ship `search_semantic` as an opt-in tool behind the `EmbeddingProvider` trait, with embeddings persisted to a git-ignored sidecar and queried by a bounded exact cosine scan. Provider choice is configuration; the API-endpoint impl is the v1 default and a local-model impl is deferred behind the same trait.

## Consequences

**Positive**
- Local-first preserved: off by default; honest degrade; no hosted service required for any other Clarion semantics (enrich-only on the product's own surface).
- Committed `.clarion/clarion.db` stays unbloated; embeddings are rebuildable and never committed.
- Content-hash keying gives free invalidation: a changed entity's stale vector is ignored until re-embedded.
- The trait isolates the provider decision: swapping API ↔ local model is a new impl, not a rewrite.

**Negative / tradeoff**
- Exact cosine scan is O(N·dim) per query; at very large N this may miss NFR-PERF-02 and require the ANN follow-on. Flagged, not pre-built.
- The API-endpoint default requires an external embedding service + key when enabled; the local-model alternative (no network) is not yet shipped.
- Two storage files (`clarion.db` + `embeddings.db`) to manage operationally; the sidecar is rebuildable, so loss is non-fatal.

## Status of delivery (2026-06-04)

Shipped and tested at acceptance: the `EmbeddingProvider` trait + `RecordingEmbeddingProvider` + `ApiEmbeddingProvider` (clarion-core), `semantic_search:` config (off by default), the `.clarion/embeddings.db` sidecar (`clarion-storage::embeddings`), the `search_semantic` MCP tool (honest-degrade + bounded cosine + content-hash freshness), `serve` provider construction (`build_embedding_provider` → `with_semantic_search`), the gitignore entry, and this ADR. The read + enable path is complete.

Delivery update: `clarion analyze` now runs an opt-in post-commit embedding population pass when `semantic_search.enabled` has a configured provider. It embeds content-hashed entities into `.clarion/embeddings.db`, skips fresh `(entity_id, content_hash, model_id)` rows, and enforces `semantic_search.session_token_ceiling`.

## Follow-up

- **Local-model `EmbeddingProvider`** (`candle`/`ort`) — the no-network alternative behind the same trait.
- **ANN backend** (sqlite-vec / HNSW) — only if the exact scan misses NFR-PERF-02; logged with that trigger, never silent.
