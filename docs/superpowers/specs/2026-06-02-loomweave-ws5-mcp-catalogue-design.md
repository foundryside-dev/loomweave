# WS5 — MCP Catalogue Completion — Design

**Date:** 2026-06-02
**Status:** Design (ready for an implementation plan)
**Workstream:** WS5 of the Loomweave first-class program
(`2026-06-02-loomweave-first-class-program-design.md` §2). Parallel band — autonomous, ungated.
**Scope:** Complete the consult-mode MCP surface — the tools agents actually reach for — as a
**stateless** catalogue, retiring the never-built cursor/session model. Faceted search and the
exploration-elimination shortcuts; defers semantic search and dead-code analysis.

**Inputs / authorities:**
- `docs/loomweave/1.0/system-design.md` §8 (MCP Consult Surface) — the *intended* catalogue; its
  cursor/session model is **ratified-away here** (see §1).
- `docs/loomweave/1.0/requirements.md` REQ-MCP-*, NFR-PERF-02/03.
- `docs/loomweave/adr/ADR-030-on-demand-summary-scope.md` — the on-demand posture this inherits.
- `docs/loomweave/adr/ADR-038-sei-token-and-signature.md` — SEI carried in responses.
- Ground truth: `crates/loomweave-mcp/` (the live 19-tool stateless surface).

---

## 1. Decision: a stateless catalogue (ratifying shipped reality)

`system-design.md` §8 describes a **cursor-based session model** as central: `goto(id)` sets a
server-held "here", and `summary`/`neighbors`/`callers` default to that cursor, with breadcrumbs
and a scope-lens. **That model was never built.** Ground truth (`crates/loomweave-mcp/src/`): the
shipped surface is **19 stateless tools**, each taking explicit entity IDs; there is no cursor,
no breadcrumbs, no scope-lens, no per-session server state.

WS5 **ratifies the stateless reality** and does not build the cursor model. Rationale:

- The shipped surface is already stateless; building a stateful path now would mean two code
  paths (cursor vs explicit-id) and per-session server state to keep coherent.
- `orientation_pack` already serves the "get oriented in one call" need the cursor UX was meant
  to deliver — without holding state.
- Stateless calls are self-contained, cacheable, and resumable; an LLM agent passes the ID it
  already holds at near-zero cost. The "exploration elimination" value (Principle 2) comes from
  the **content** of the tools (precomputed catalog, cheap shortcuts), not from a navigation UX.

**Doc reconciliation (not an edit):** `system-design.md` §8's cursor/session model, the
`goto`/`back`/`zoom_*`/`breadcrumbs`/`set_scope_lens`/`session_info` tools, and the "cursor
defaults" response behaviour are **superseded by this design**. §8 is a 1.0 canonical doc and is
immutable in spirit; this spec is the forward authority for the MCP surface. A one-line "see
WS5 design — stateless" annotation against §8 is a follow-up doc task, tracked, not done here.

---

## 2. Current surface (ground truth) vs. the gap

**Shipped today (19, stateless):** `entity_at`, `find_entity`, `callers_of`, `call_sites`,
`execution_paths_from`, `neighborhood`, `subsystem_members`, `subsystem_of`, `summary`,
`summary_preview_cost`, `source_for_entity`, `metadata`, `issues_for`, `orientation_pack`,
`project_status`, `analyze_start`, `analyze_status`, `analyze_cancel`, `index_diff`.

So the gap is **narrower than the roadmap's "8 of ~35" framing** — inspection (`metadata`,
`source_for_entity`), navigation-free graph queries, the analyze lifecycle, and orientation are
already present. What is missing is the **read-side guidance/findings/wardline inspection**, the
**faceted search** facets, the **exploration-elimination shortcuts**, and **observation emit**.

---

## 3. WS5 tool catalogue (what this workstream adds)

All tools: stateless (explicit IDs / scopes), bounded responses (§5), SEI carried where an
entity ID is returned (§4). `scope?` accepts an entity ID (descendants of that entity) **or** a
path glob (`"src/auth/**"`); omitted → whole project.

### 3.1 Inspection (read)
| Tool | Signature | Returns |
|---|---|---|
| `guidance_for` | `(entity_id)` | guidance sheets applicable to the entity (composed, scope-ranked) |
| `findings_for` | `(entity_id, filter?)` | findings on the entity (filter by kind/severity/status) |
| `wardline_for` | `(entity_id)` | `WardlineMeta` (declared tier, groups, boundary contracts) if present |

*(`metadata`, `source_for_entity` already exist — not re-implemented.)*

### 3.2 Faceted search
| Tool | Signature | Returns |
|---|---|---|
| `find_by_tag` | `(tag, scope?)` | entities carrying the tag |
| `find_by_kind` | `(kind, scope?)` | entities of a plugin-declared kind |
| `find_by_wardline` | `(tier?, group?, scope?)` | entities matching Wardline tier/group |

*(`find_entity(pattern)` already covers name/qualname search — not re-implemented.)*

### 3.3 Exploration-elimination shortcuts (on-demand graph/index queries)
Each is a cheap read over the already-built catalog/edges — **no new analyze-time precompute**
(ADR-030 posture). Each takes an optional `scope?`.

`find_entry_points`, `find_http_routes`, `find_data_models`, `find_tests`, `what_tests_this(id)`,
`recently_changed(since?, scope?)`, `high_churn(limit?, scope?)`, `find_circular_imports`,
`find_coupling_hotspots(limit?, scope?)`, `find_deprecations`, `find_todos`.

These read existing signals: entity categorisation tags (entry-point/route/data-model/test,
emitted by the plugin in Phase 1.5), git churn metadata on file entities, and the import/call
edge graph. Where a shortcut depends on a categorisation the plugin does not yet emit, the gap is
surfaced as a finding/no-op (honest empty result), never a fabricated answer.

### 3.4 Observation emit (write)
| Tool | Signature | Effect |
|---|---|---|
| `emit_observation` | `(entity_id, text)` | writes a Filigree observation via the existing integration; enrich-only (Filigree absence → honest failure, never silent) |

---

## 4. SEI in responses

Every WS5 tool that returns an entity ID also returns `sei` (`Option<String>`), per ADR-038 and
the Wave-1 mandate (no binding keyed on a locator on any surface). WS5 is autonomous/parallel-band
and **not gated on Wave 1**: before SEI authority lands, the join `entities.id =
sei_bindings.current_locator AND status='alive'` yields no row and `sei` is `null` (graceful
degrade). The field is designed in now and populates itself once Wave 1 ships — no rework.

---

## 5. Response shape & bounds

- **Bounded (NFR-PERF-03).** List-returning tools paginate (`limit?`, `offset?`, default/max
  pinned per tool) and report `total` + `truncated`. No unbounded result sets.
- **Confidence tiers.** Any edge-derived result (callers, coupling, circular imports) carries the
  `resolved | ambiguous | inferred` tier and defaults to `>= resolved` (ADR-028).
- **No silent caps.** If a tool bounds coverage, it says so (`truncated: true`), never returns a
  truncated set as if complete.
- **Opt-in / enrich-only.** Nothing here adds cost to the base `analyze`/`serve` path; the tools
  are read surface plus one Filigree-enrich write.

---

## 6. WS5 / WS6 boundary

WS5 owns the **read/inspection + query surface + observation emit**. WS6 (guidance maturity) owns
the **guidance authoring lifecycle**. The split, by tool:

| WS5 (this spec) | WS6 (separate cycle) |
|---|---|
| `guidance_for` (read composed sheets) | `propose_guidance`, `promote_guidance` (authoring) |
| `findings_for`, `wardline_for` | the `loomweave guidance` CLI |
| `emit_observation` (general agent capability) | guidance staleness review + `promote_observation` |

Rationale: reading guidance is part of the inspection surface; *authoring* guidance is a lifecycle
with its own anti-poisoning flow (propose→observation→promote, NFR-SEC-02) that belongs with WS6.

---

## 7. Split out, not slipped — `search_semantic` + `find_dead_code` → WS5b

Two tools are **not** in WS5 because they need infrastructure beyond a catalog query — but they
are **scheduled, not deferred indefinitely.** They form **WS5b — Advanced MCP queries**, which has
its own design + delivery plan
(`docs/superpowers/plans/2026-06-02-loomweave-ws5b-advanced-queries-plan.md`) and sequences as the
parallel-band wave immediately after WS5.

- **`search_semantic`** — needs embedding infrastructure (an `EmbeddingProvider`, a vector store,
  an opt-in policy). It is a real capability with its own cost and storage posture, not an
  MCP-surface afterthought. WS5b Part A delivers it.
- **`find_dead_code`** — whole-graph reachability with genuine false-positive economics (dynamic
  dispatch, reflection, framework magic). It is closer to a static-analysis capability than a
  catalog read. WS5b Part B delivers it.

WS5 ships the stateless catalogue (§3); WS5b ships these two on a defined plan. Neither is a
backlog entry that quietly never happens.

---

## 8. Sequencing & relationship to other workstreams

- **Autonomous / parallel band.** WS5 has no hard gate; run it alongside Waves 0–2 as capacity
  allows (program §4).
- **Soft relationship to Wave 1 (SEI).** The `sei` response field is null until Wave 1; no rework
  when it lands. If WS5 ships after Wave 1, SEI is populated from day one.
- **Feeds WS4 (dossier).** `guidance_for`/`findings_for`/`wardline_for` are part of the entity
  slice the dossier assembler may read; WS5 makes them available (over MCP; the dossier's HTTP
  reachability is WS4's concern, not WS5's).

---

## 9. Definition of done

- The §3 tools are implemented, registered, stateless, bounded, and tested (including empty-result
  honesty where a categorisation signal is absent).
- Every entity-returning tool carries the `sei` field (null pre-Wave-1, populated post).
- Faceted search + the cheap exploration shortcuts ship; `search_semantic` and `find_dead_code`
  are logged as deferred follow-ons.
- The `loomweave-workflow` skill / MCP tool docs are updated to describe the new tools (stateless,
  SEI-carrying); the §8-cursor-model reconciliation note is filed as a doc task.
- All Rust CI gates green; Python gates if the plugin's categorisation emission is touched.
