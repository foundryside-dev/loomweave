# WS5b — Advanced MCP Queries (semantic search + reachability) — Design & Delivery Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or
> `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Date:** 2026-06-02
**Status:** Design + delivery plan
**Workstream:** WS5b of the Clarion first-class program — the two tools split out of WS5
(`2026-06-02-clarion-ws5-mcp-catalogue-design.md` §7). Parallel band; soft-gated on WS5 (the
stateless catalogue surface it extends).
**Goal:** Deliver `search_semantic` (Part A) and `find_dead_code` (Part B) — the two capabilities
that need infrastructure beyond a catalog query. These are **scheduled, not deferred**.

**Inputs / authorities:**
- `2026-06-02-clarion-ws5-mcp-catalogue-design.md` — the stateless MCP surface this extends
- `docs/clarion/1.0/system-design.md` §5 (Policy Engine / LLM provider abstraction — the pattern
  the `EmbeddingProvider` mirrors), §3 (Data Model — edges)
- `docs/clarion/adr/ADR-005-clarion-dir-tracking.md` (git-committable `.clarion/` — the
  embedding-storage posture must respect this)
- `docs/clarion/adr/ADR-028-edge-confidence-tiers.md` (reachability uses edge tiers)

---

## 0. Two parts, independent

Part A (semantic) and Part B (reachability) share nothing but the WS5 surface they plug into. They
can ship in either order or in parallel. Part B is lighter (no new infra, no external dependency)
and is recommended first.

---

## Part A — `search_semantic` (embeddings)

### A.1 Design decisions

**Opt-in, mirroring the LLM policy (local-first doctrine).** Embeddings are **disabled by
default**, exactly like `llm_policy`. Loom is local-first and single-binary; nothing here may make
a hosted service *required*. When semantic search is off, `search_semantic` returns an honest
`"semantic search not enabled"` — never a faked or empty-as-if-complete result.

**`EmbeddingProvider` trait**, mirroring `LlmProvider` (system-design §5):
```
trait EmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> usize;
    fn model_id(&self) -> &str;
}
```
Configured under `clarion.yaml: semantic_search:` (`enabled`, `provider`, `model_id`,
`api_key_env`, dim). A `RecordingEmbeddingProvider` exists for deterministic tests (like
`RecordingProvider`).

**Storage posture (load-bearing — protects the git-committable DB).** Embeddings are large
(~100–200k entities × dim × 4 bytes ≈ hundreds of MB) and must NOT bloat the committed
`.clarion/clarion.db`. They live in a **separate, git-ignored sidecar** `.clarion/embeddings.db`
(added to the ADR-005 `.gitignore` list), keyed by `(entity_id, content_hash, model_id)` so they
invalidate on content change exactly like the summary cache. Textual export excludes them
(rebuildable).

**Vector store + query.** Store f32 vectors in the sidecar SQLite. For the elspeth target scale,
**v1 uses a bounded exact cosine scan** over the candidate set (optionally pre-filtered by
`scope`), capped + paginated — no ANN index dependency. A scalable ANN backend (sqlite-vec / HNSW)
is a named follow-on if the exact scan misses the latency bar (NFR-PERF-02); flag, do not
pre-build.

**When embeddings are computed.** At `analyze` time when enabled, as a new optional sub-phase
after leaf summarisation (the text embedded is the entity's `short_name` + summary `purpose` +
docstring). Cost is governed by the **policy engine** like LLM work: the dry-run estimate includes
embedding cost; `on_exceed` applies. Re-embedding skips unchanged `(entity_id, content_hash)`.

### A.2 Owner-decision (flag before building)

- **D-WS5b-1 — embedding provider.** Default proposal: reuse the configured LLM provider's
  embedding endpoint (Anthropic has none today, so practically an OpenAI/Voyage/Cohere-class
  endpoint **or** a bundled local model via `candle`/`ort`). Local-first leans toward a small
  bundled local model (no network, no key); simplest-to-ship leans toward an API endpoint
  (opt-in + key, degrade when absent). **This is the one genuine architectural choice in WS5b** —
  it sets a dependency and a cost posture. Resolve it before A.4. Recommendation: ship the
  **provider trait + API-endpoint impl first** (opt-in, degrade-honest), add a **local-model impl**
  as a second provider behind the same trait — so the trait, not the choice, is load-bearing.

### A.3 Tasks — Part A

- [ ] **A.T0 — ADR.** `ADR-039-semantic-search-embeddings.md`: opt-in posture, `EmbeddingProvider`
  trait, sidecar storage (ADR-005 amendment for `.clarion/embeddings.db` gitignore), policy-engine
  cost governance, the D-WS5b-1 provider decision. Register in the ADR index + glossary if any
  cross-product term is introduced (likely none — `search_semantic` is Clarion-local).
- [ ] **A.T1 — sidecar + gitignore.** `.clarion/embeddings.db` (separate connection; not the
  writer-actor's committed DB). Add to the ADR-005 `.gitignore` defaults. Migration for the
  embeddings schema (`entity_embeddings(entity_id, content_hash, model_id, dim, vec BLOB)`).
- [ ] **A.T2 — `EmbeddingProvider` trait + impls** (test-first): the trait, the chosen impl
  (D-WS5b-1), and `RecordingEmbeddingProvider`. Disabled-by-default config under `semantic_search:`.
- [ ] **A.T3 — analyze embedding sub-phase** (opt-in): embed `(short_name + purpose + docstring)`
  after leaf summarisation; skip unchanged `(entity_id, content_hash)`; cost into the dry-run +
  budget watcher.
- [ ] **A.T4 — `search_semantic(query, limit?, scope?)` MCP tool** (test-first): embed the query,
  bounded exact cosine over the (scope-filtered) candidate set, return ranked entities + `sei` +
  score, paginated. Honest `"not enabled"` when off.
- [ ] **A.T5 — docs**: contracts/skill update; note the ANN follow-on if latency misses NFR-PERF-02.

---

## Part B — `find_dead_code` (reachability)

### B.1 Design decisions

**Reachability from comprehensive roots.** Dead-code = entities not reachable from any "called
from outside" root over the call+import graph. Roots are the union of: entry points, public/exported
API, test functions, HTTP routes, CLI commands (the §3.3 categorisations WS5 already surfaces).
Forward-traverse call + import edges; unreached entities are candidates.

**Fail toward "live", not "dead" (false-positive economics).** Calling live code dead is the
harmful error (an agent deletes working code). So reachability is **conservative**: it counts
`resolved` **and** `ambiguous` **and** `inferred` edges as reachable (ADR-028 tiers), and treats
dynamic-dispatch / reflection signals as reachability barriers that keep targets live. Better to
under-report dead code than to over-report it.

**Heuristic output, honestly labelled.** Results are **Findings**
(`CLA-FACT-DEAD-CODE-CANDIDATE`, `confidence < 1`, `confidence_basis: heuristic`) and the MCP tool
returns the same with a confidence and the reason it could not prove reach. Never presented as
certain. Framework-magic entry kinds (decorated handlers, plugin hooks) are excluded from
candidacy by default with a documented exclusion list.

**Cost + caching.** Whole-graph BFS is O(V+E) — feasible at the target scale. Computed **on-demand**
(ADR-030 posture) and cached per run keyed on the index content (invalidate when the graph
changes). No new analyze-time precompute required.

### B.2 Tasks — Part B

- [ ] **B.T1 — root set** (test-first): assemble roots from the existing categorisations
  (entry_points ∪ exported API ∪ tests ∪ http_routes ∪ cli_commands). Document the framework-magic
  exclusion list.
- [ ] **B.T2 — conservative reachability** (test-first): forward BFS over call+import edges across
  all confidence tiers; dynamic-dispatch/reflection barriers keep targets live. Cache the reachable
  set per index version. Fixtures: a known-dead leaf is flagged; a reflectively-called function is
  NOT flagged; an ambiguous-edge target is NOT flagged.
- [ ] **B.T3 — `find_dead_code(scope?)` MCP tool + `CLA-FACT-DEAD-CODE-CANDIDATE` finding**:
  unreached entities as candidates with confidence + reason; `sei` carried; bounded + paginated.
- [ ] **B.T4 — docs**: rule catalogue entry for the new finding; contracts/skill update.

---

## Sequencing & gates

- **Soft-gated on WS5** (it extends the stateless catalogue + reuses §3.3 categorisations as
  reachability roots). Recommended order: **Part B first** (no new infra/dependency), then Part A.
- **Part A is gated on D-WS5b-1** (embedding provider) — resolve before A.T2.
- Both are parallel-band: no critical-path dependency on Waves 0–2; `sei` in results is null until
  Wave 1, populated after (same posture as WS5 §4).

## Definition of done

- **Part A:** `search_semantic` ships opt-in (off by default, honest degrade); embeddings live in
  the git-ignored sidecar (committed DB unbloated); cost governed by the policy engine; results
  carry `sei` + score, bounded. ADR-039 accepted; ADR-005 gitignore amended.
- **Part B:** `find_dead_code` ships on-demand, conservative (fails toward "live"), heuristic
  results labelled with confidence; `CLA-FACT-DEAD-CODE-CANDIDATE` finding emitted; fixtures prove
  no live-code false-positives on reflection/ambiguous edges.
- Deferrals from WS5 are now **delivered**, not slipped. Any further deferral (an ANN backend if
  the exact scan misses latency) is itself logged with a trigger, never silent.
- All Rust CI gates green; Python gates if categorisation emission is touched.
