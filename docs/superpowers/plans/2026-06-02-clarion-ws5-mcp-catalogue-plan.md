# WS5 — MCP Catalogue Completion — Execution Plan (Wave 4)

**Date:** 2026-06-02
**Design (source of truth):** `docs/superpowers/specs/2026-06-02-clarion-ws5-mcp-catalogue-design.md`
**Execution prompt:** `docs/superpowers/prompts/2026-06-02-wave-4-execution.md`
**Method:** subagent-driven-development, TDD, task-by-task. All ADR-023 Rust gates green.

## Ground truth established (recon, 2026-06-02)

- **Tool registry** lives in `crates/clarion-mcp/src/lib.rs`: `list_tools()` (Vec<ToolDefinition>)
  + `handle_tool_call()` match dispatch → `impl ServerState` `tool_*` async methods returning
  `Result<Value, ParamError>`. 19 stateless tools ship today.
- **SEI for free:** route every entity-returning response through `entity_json(conn, &entity)`
  (lib.rs ~4524) — it injects `sei` via `sei_for_locator(conn, id).ok().flatten()` (null pre-Wave-1).
  Never hand-roll entity JSON.
- **Envelopes:** `success_envelope(result)`, `success_envelope_with_truncation(result, reason)`,
  `tool_error_envelope(code, msg, retryable)`. `ParamError::new(msg).to_json_rpc(id)` for bad args.
- **Reader access:** `self.readers.with_reader(move |conn| { ... }).await` → storage `Result`.
- **Storage helpers:** `entity_by_id`, `find_entities`, `call_edges_from/targeting`,
  `import_edges_for_entity`, `reference_edges_for_entity`, `contained_entity_ids`,
  `containing_module_id`, `subsystem_of_entity`, `sei_for_locator`. Tables: `entities`
  (kind, source_file_path, properties incl. generated `git_churn_count`), `entity_tags`
  (entity_id, plugin_id, tag), `edges` (kind, from_id, to_id, confidence), `findings`,
  `wardline_taint_facts` (entity_id, wardline_json opaque), `guidance_sheets` view (kind='guidance').
- **Signal population reality (decides real-query vs honest-empty):**
  - Python plugin emits NONE of: entry-point / http-route / data-model / test / deprecation / todo
    categorisation tags. `entity_tags` is **unpopulated** in practice. → those shortcuts query the
    tag and return **honest-empty + missing-signal note** (never fabricate).
  - `git_churn_count` is **not populated** by the analyze pipeline → churn/recency shortcuts are
    honest-empty + missing-signal.
  - `wardline_taint_facts` populated only via Filigree Flow-B (`POST /api/wardline/taint-facts`),
    empty locally; `wardline_json` is **opaque** (contract). `wardline_for` returns it verbatim;
    `find_by_wardline` filters best-effort via `json_extract` and is honest-empty when absent.
  - No existing guidance composition logic → `guidance_for` is **new** read-side scope-ranked
    composition over the `guidance_sheets` view (READ only; authoring is WS6).
  - `find_circular_imports` / `find_coupling_hotspots` are **real** graph queries over `edges`.
- **emit_observation:** no HTTP observation-write route exists (Filigree 2.2.0: `GET
  /api/loom/observations` 200, `POST` 405); ADR-016's sanctioned `filigree mcp` subprocess
  transport is unbuilt; v0.2 HTTP trigger un-fired. → **Deferred + tracked** (no permanent-fail
  stub, no scan-results coercion, no silent CLI shell-out). Mirrors WS5b / WP9-B deferral discipline.
- No `glob`/`globset` dep → implement a minimal path-glob matcher (`*`, `**`, literal) in the
  foundation; match against `source_file_path` relative to `project_root`.

## Architecture

New submodule `crates/clarion-mcp/src/catalogue/` (child of crate root → can access `ServerState`
private fields + crate-private helpers). Implementations + helpers live there; the only edits to
the 6800-line `lib.rs` are: `mod catalogue;`, the new `ToolDefinition`s in `list_tools()`, and the
new dispatch arms. Keep those three edits serialized (one implementer at a time — never parallel).

Shared foundation (`catalogue/mod.rs`):
- `Scope` (entity-id descendants | path glob | whole project) + `resolve_scope(args)` +
  application to a candidate entity set / SQL predicate.
- `PageParams { limit, offset }` parsing with **pinned default + max** per tool; list response
  builder reporting `total` + `truncated` (NFR-PERF-03; no silent caps).
- `missing_signal` note helper (honest-empty: `{available:false, reason:…}`).
- A `tag_shortcut(conn, tag, scope, page)` helper that the categorisation shortcuts wrap.

## Tasks

- [ ] **Task 1 — Foundation + Inspection reads.** `catalogue/` scaffolding + shared helpers
  (Scope, glob, pagination, missing-signal). Tools: `guidance_for(entity_id)` (scope-ranked
  composition over `guidance_sheets`), `findings_for(entity_id, filter?)` (findings table, filter
  by kind/severity/status), `wardline_for(entity_id)` (verbatim `wardline_taint_facts`, honest-empty).
  SEI on entity-returning responses. **TDD the SEI-join contract** (null with no binding row;
  populated with one). REQ-MCP-02, ADR-038.
- [ ] **Task 2 — Faceted search.** `find_by_tag(tag, scope?)`, `find_by_kind(kind, scope?)`
  (real), `find_by_wardline(tier?, group?, scope?)` (best-effort json_extract, honest-empty).
  Bounded (limit/offset/total/truncated), scope (entity-id OR path glob), SEI. **TDD the
  bounded/pagination contract.** REQ-MCP-02, REQ-MCP-04, NFR-PERF-03.
- [ ] **Task 3 — Real graph shortcuts.** `find_circular_imports`, `find_coupling_hotspots(limit?,
  scope?)` over the import/call edge graph. Confidence tiers (ADR-028) default `>= resolved`,
  declared in the response. Bounded. REQ-MCP-03, ADR-028.
- [ ] **Task 4 — Categorisation/churn shortcuts (honest-empty).** `find_entry_points`,
  `find_http_routes`, `find_data_models`, `find_tests`, `what_tests_this(id)`, `find_deprecations`,
  `find_todos`, `recently_changed(since?, scope?)`, `high_churn(limit?, scope?)`. Each reads the
  EXISTING signal (tag / churn) and returns **honest-empty + missing-signal** because the signal is
  unemitted today. **TDD the honest-empty behaviour.** REQ-MCP-03.
- [ ] **Task 5 — emit_observation deferral.** Do NOT register a tool. File the deferral Filigree
  issue citing ADR-016 + REQ-MCP-05; record the rationale. (Mirrors WS5b/WP9-B.)
- [ ] **Task 6 — Docs.** Update `crates/clarion-mcp/assets/skills/clarion-workflow/SKILL.md` to
  describe the new tools (stateless, SEI-carrying, bounded, honest-empty). File the §8
  cursor-model "see WS5 — stateless" reconciliation doc task (Filigree). Do NOT edit the 1.0
  canonical doc's decisions.
- [ ] **Task 7 — Final gates + code review.** All ADR-023 Rust gates green (fmt, clippy -D
  warnings, build --bins, nextest, doc -D warnings, deny). Request code review; surface result.

## Invariants (every task)
Stateless (no cursor/session/server-held state). Bounded, no silent caps. SEI on every
entity-returning response. Confidence tiers on edge-derived results. Honest empty, never
fabricated. Enrich-only / opt-in (no base analyze/serve cost). No analyze-time precompute. No
edits to Accepted ADRs or archived docs.
