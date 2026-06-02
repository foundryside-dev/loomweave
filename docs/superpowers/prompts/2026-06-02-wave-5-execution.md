# Wave 5 — execution prompt (WS5b · semantic search + reachability)

**Date:** 2026-06-02
**Use:** Drop the fenced prompt below into an agent to plan and execute **Wave 5 — WS5b: advanced
MCP queries** — the two tools split out of WS5 (`search_semantic`, `find_dead_code`). These were
**scheduled, not deferred**; this is their committed delivery.
**Position:** Wave 5. Ungated/concurrent, **soft-gated on WS5 (Wave 4)** (it extends that surface
and reuses its categorisations). **Part A is gated on owner-decision D-WS5b-1** (embedding provider).
**Source of truth:** `docs/superpowers/plans/2026-06-02-clarion-ws5b-advanced-queries-plan.md` (the
plan — read it fully); WS5 design spec §7 (why these split out); program §4 (WS5b = Wave 5).

---

```
You are implementing **Wave 5 — WS5b: advanced MCP queries** of the Clarion first-class
program, in the Clarion repo at /home/john/clarion. WS5b delivers the two tools split out of
WS5: `search_semantic` (Part A) and `find_dead_code` (Part B). They were split out because
they need infrastructure beyond a catalog query — and they are SCHEDULED, not deferred. Your
job is to PLAN and EXECUTE them: real code, real tests, all CI gates green.

## Two parts, independent — do Part B FIRST
Part A (semantic) and Part B (reachability) share nothing but the WS5 surface they plug into.
**Part B is lighter (no new infra, no external dependency, no owner-decision) — do it first.**
Part A needs an owner decision (D-WS5b-1) and new infrastructure.

## Position & gates
- Ungated/concurrent with the suite waves; **soft-gated on WS5 (Wave 4)** — it extends that MCP
  surface and reuses WS5's categorisations (entry-points/routes/etc.) as reachability roots.
  Confirm WS5 is in place (or coordinate) before wiring against it.
- **Part A gate — owner-decision D-WS5b-1 (embedding provider).** Local bundled model vs API
  endpoint. This sets a dependency + cost posture and is the OWNER's call. **Confirm it before
  building Part A's provider impl (task A.T2).** Part B is unaffected — start there.
- SEI field: like WS5, every entity-returning result carries `sei` (null until Wave 1's
  `sei_bindings` exist, populated after). Not gated on Wave 1.

## Read these first
1. docs/superpowers/plans/2026-06-02-clarion-ws5b-advanced-queries-plan.md — THE plan. Read all
   of it: Part A design (A.1) + the D-WS5b-1 decision (A.2) + tasks (A.3); Part B design (B.1) +
   tasks (B.2); sequencing + DoD.
2. docs/superpowers/specs/2026-06-02-clarion-ws5-mcp-catalogue-design.md §7 (the split-out) — the
   surface these plug into.
3. docs/clarion/adr/ADR-005-clarion-dir-tracking.md (the git-committable `.clarion/` posture the
   embedding sidecar must respect — do NOT bloat the committed DB), ADR-030 (on-demand posture),
   ADR-028 (edge tiers), ADR-038 (SEI field). CLAUDE.md (gates, Filigree, ADR immutability).

## Part B — `find_dead_code` (reachability) — DO THIS FIRST
- Roots = the union of entry points ∪ exported/public API ∪ tests ∪ HTTP routes ∪ CLI commands
  (the categorisations WS5 surfaces). Forward-traverse call+import edges; unreached = candidates.
- **Fail toward "live", not "dead" (false-positive economics).** Calling live code dead is the
  harmful error. Reachability is CONSERVATIVE: count `resolved` AND `ambiguous` AND `inferred`
  edges as reachable; treat dynamic-dispatch/reflection signals as barriers that keep targets
  live. Better to under-report dead code than over-report it.
- **Heuristic, honestly labelled.** Results are Findings (`CLA-FACT-DEAD-CODE-CANDIDATE`,
  `confidence < 1`, `confidence_basis: heuristic`) and the MCP tool returns the same with a
  confidence + the reason reach could not be proven. Never presented as certain. Framework-magic
  entry kinds (decorated handlers, plugin hooks) are excluded by a documented exclusion list.
- **On-demand BFS, cached per index version** (ADR-030 posture). No new analyze-time precompute.
- Tasks (test-first on the matcher-like logic): B.T1 root set, B.T2 conservative reachability
  (fixtures: a known-dead leaf flagged; a reflectively-called fn NOT flagged; an ambiguous-edge
  target NOT flagged), B.T3 `find_dead_code(scope?)` tool + the finding, B.T4 docs/rule catalogue.

## Part A — `search_semantic` (embeddings) — AFTER D-WS5b-1
- **Opt-in, disabled by default (local-first doctrine).** Mirror the `llm_policy`: nothing here
  may make a hosted service REQUIRED. When off, `search_semantic` returns an honest
  `"semantic search not enabled"` — never a faked/empty-as-if-complete result.
- **`EmbeddingProvider` trait** mirroring `LlmProvider` (`embed`/`dimensions`/`model_id`); a
  `RecordingEmbeddingProvider` for deterministic tests. Config under `clarion.yaml: semantic_search:`.
- **Storage (LOAD-BEARING — protects the committable DB).** Vectors are hundreds of MB at the
  elspeth scale and MUST NOT bloat `.clarion/clarion.db`. They live in a SEPARATE, git-ignored
  sidecar `.clarion/embeddings.db`, keyed by `(entity_id, content_hash, model_id)` so they
  invalidate like the summary cache. Add the sidecar to the gitignore DEFAULTS in code
  (install.rs `GITIGNORE_CONTENTS`); textual export excludes it.
- **Query.** v1 = a bounded EXACT cosine scan over the (scope-filtered) candidate set, capped +
  paginated. **Do NOT build an ANN backend (sqlite-vec/HNSW) in v1** — it is a named follow-on,
  logged with a trigger (exact scan misses NFR-PERF-02), not silently built.
- **When embedded.** At `analyze` time when enabled (after leaf summarisation; text = short_name +
  summary purpose + docstring). Cost governed by the POLICY ENGINE like LLM work (dry-run estimate
  + budget watcher); re-embedding skips unchanged `(entity_id, content_hash)`.
- **D-WS5b-1 recommendation** (confirm with owner): ship the `EmbeddingProvider` trait + an API
  impl first, add a local-model impl behind the SAME trait — so the trait, not the choice, is
  load-bearing.
- Tasks (test-first on the trait + tool): A.T0 ADR (see below), A.T1 sidecar + gitignore +
  embeddings migration, A.T2 trait + impls + disabled-by-default config, A.T3 analyze embedding
  sub-phase (opt-in, cost-governed), A.T4 `search_semantic(query, limit?, scope?)` tool (bounded
  cosine, returns entities + `sei` + score; honest "not enabled" when off), A.T5 docs.

## ADR (Part A, A.T0)
- Write `ADR-039-semantic-search-embeddings.md`. **First confirm 039 is the next free number**
  (037 = shared-error-vocabulary, 038 = SEI token — both taken; verify nothing has claimed 039).
  Record: opt-in posture, the `EmbeddingProvider` trait, the sidecar-storage decision (it EXTENDS
  ADR-005's tracked-vs-excluded list — reference ADR-005, do NOT edit its immutable body), policy-
  engine cost governance, and the D-WS5b-1 provider choice. Register in the ADR index. Glossary
  check: `search_semantic`/embeddings are Clarion-local (no cross-product term) — confirm + skip.

## Hard boundaries — do NOT
- Do NOT bloat the committed `.clarion/clarion.db` with vectors — sidecar only.
- Do NOT make semantic search required — opt-in + honest degrade (local-first).
- Do NOT build an ANN vector backend in v1 — exact scan; ANN is a logged follow-on.
- Part B: do NOT over-report dead code (fail toward "live"); do NOT present heuristic results as
  certain.
- Do NOT build Part A's provider impl before D-WS5b-1 is confirmed by the owner.
- Do NOT build the WS5 catalogue (Wave 4) or guidance authoring (WS6/Wave 6).
- Do NOT edit Accepted ADRs (ADR-005 etc.); do NOT touch archived docs.

## Method
- Use superpowers:executing-plans / subagent-driven-development, task-by-task. TDD: the
  reachability fixtures (B.T2), the `EmbeddingProvider` + `search_semantic` (A.T2/A.T4) are
  test-first. Verify ground truth (WS5 categorisations, edge tiers, the policy/provider pattern)
  before building.
- All ADR-023 Rust gates green. Python gates if categorisation emission is touched.
- Invariants: opt-in (semantic off by default; dead-code on-demand — no base-path cost),
  fail-closed (honest "not enabled"; reachability fails toward live), enrich-only.

## Filigree
Track per CLAUDE.md (atomic start-work, `--actor`). Issues per part (B first), citing WS5b, the
plan task IDs (A.T*/B.T*), D-WS5b-1, ADR-039, ADR-005/028/030. Close as you land each.

## Definition of done (Wave 5 / WS5b)
- **Part B:** `find_dead_code` ships on-demand + conservative (fails toward "live"); heuristic
  results labelled with confidence; `CLA-FACT-DEAD-CODE-CANDIDATE` emitted; fixtures prove no
  live-code false-positives on reflection / ambiguous edges.
- **Part A:** `search_semantic` ships opt-in (off by default, honest degrade); embeddings in the
  git-ignored sidecar (committed DB unbloated); cost policy-governed; results carry `sei` + score,
  bounded. ADR-039 accepted; gitignore defaults amended in code.
- The WS5 deferrals are now DELIVERED, not slipped. Any further deferral (an ANN backend) is
  logged with a trigger, never silent.
- All CI gates green.

When implemented, tested, and gate-green, request a code review and surface the result — stating
plainly that the two split-out tools are delivered, not floated. Wave 5 ends here.
```
