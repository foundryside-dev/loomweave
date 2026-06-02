# Wave 4 — execution prompt (WS5 · MCP catalogue completion)

**Date:** 2026-06-02
**Use:** Drop the fenced prompt below into an agent to plan and execute **Wave 4 — WS5: MCP
catalogue completion**, the first standalone-first-class wave.
**Position:** Wave 4. **Ungated** — runs concurrently with the suite waves (0–3); does not wait
for core paradise. Committed slot, not "as capacity allows."
**Source of truth:** `docs/superpowers/specs/2026-06-02-clarion-ws5-mcp-catalogue-design.md` (the
design — read it fully); program §2/§4 (WS5 = Wave 4).
**Companion:** Wave 5 (WS5b — `search_semantic` + `find_dead_code`) extends this surface; those two
tools are **NOT** in Wave 4.

---

```
You are implementing **Wave 4 — WS5: MCP catalogue completion** of the Clarion first-class
program, in the Clarion repo at /home/john/clarion. WS5 completes the consult-mode MCP
surface — the tools agents actually reach for — as a **stateless** catalogue. Your job is to
PLAN and EXECUTE it: real code, real tests, all CI gates green.

## Position & gate
Wave 4 is **ungated** — start anytime; it runs concurrently with the suite waves (0–3). There
is no hard gate. Two things to confirm before building (verify, don't assume):
1. **Ground-truth the current MCP surface.** WS5 EXTENDS a live surface — `crates/clarion-mcp/`
   already ships ~19 stateless tools (entity_at, find_entity, callers_of, neighborhood,
   summary, metadata, source_for_entity, orientation_pack, issues_for, subsystem_*, …). Read
   the actual registration before adding tools; do not duplicate what exists.
2. **SEI field posture.** Wave 1 (SEI) may or may not have landed. WS5 is NOT gated on it: every
   entity-returning tool carries an `sei` field that is `null` until Wave 1's `sei_bindings`
   exist, populated after (join `entities.id = sei_bindings.current_locator AND status='alive'`).
   Design the field in now; do not block on Wave 1.

## Read these first
1. docs/superpowers/specs/2026-06-02-clarion-ws5-mcp-catalogue-design.md — THE design. Read all
   of it: §1 stateless decision, §2 current-surface inventory, §3 the tool catalogue, §4 SEI,
   §5 bounds, §6 the WS5/WS6 boundary, §7 the Wave-5 split-out.
2. docs/clarion/1.0/system-design.md §8 — the INTENDED catalogue. Its cursor/session model is
   **ratified-away** (see below); read it to know what NOT to build.
3. docs/clarion/adr/ADR-030-on-demand-summary-scope.md (on-demand posture), ADR-028 (edge
   confidence tiers), ADR-038 (the SEI field). CLAUDE.md (CI gates, Filigree, ADR immutability).

## Scope — the WS5 tools (stateless; explicit IDs/scopes; bounded; SEI-carrying)
`scope?` accepts an entity ID (descendants) OR a path glob (`"src/auth/**"`); omitted → whole project.

- **Inspection (read):** `guidance_for(entity_id)`, `findings_for(entity_id, filter?)`,
  `wardline_for(entity_id)`. (`metadata`, `source_for_entity` already exist — do not re-implement.)
- **Faceted search:** `find_by_tag(tag, scope?)`, `find_by_kind(kind, scope?)`,
  `find_by_wardline(tier?, group?, scope?)`. (`find_entity` already covers name/qualname — leave it.)
- **Exploration-elimination shortcuts (ON-DEMAND graph/index queries — no new analyze-time
  precompute):** `find_entry_points`, `find_http_routes`, `find_data_models`, `find_tests`,
  `what_tests_this(id)`, `recently_changed(since?, scope?)`, `high_churn(limit?, scope?)`,
  `find_circular_imports`, `find_coupling_hotspots(limit?, scope?)`, `find_deprecations`,
  `find_todos`. Each reads EXISTING signals (plugin categorisation tags, git churn on file
  entities, the import/call edge graph).
- **Observation emit (write):** `emit_observation(entity_id, text)` → Filigree observation via the
  existing integration; enrich-only (Filigree absence → honest failure, never silent).

## LOCKED design decisions (from the WS5 spec) — implement as written
- **Stateless. Build NO cursor/session model.** No `goto`/`back`/`zoom_*`/`breadcrumbs`/
  `set_scope_lens`/`session_info`, no server-held per-session state. system-design §8's cursor
  model is **ratified-away as never-built**; `orientation_pack` already serves the "get oriented
  in one call" need. File a one-line "see WS5 — stateless" reconciliation note against §8 (a doc
  task; do NOT edit the 1.0 canonical doc's decisions).
- **SEI in every entity-returning response** (`Option<String>`, null pre-Wave-1). No locator-keyed
  identity on the MCP surface.
- **On-demand, no precompute.** Shortcuts are queries over the already-built catalog; add nothing
  to `analyze` (ADR-030 posture).
- **Bounded (NFR-PERF-03).** List tools paginate (`limit?`/`offset?`, pinned default+max) and
  report `total` + `truncated`. No unbounded sets; **no silent caps** — truncation is always
  declared.
- **Confidence tiers (ADR-028).** Edge-derived results (coupling, circular imports) carry
  `resolved | ambiguous | inferred` and default to `>= resolved`.
- **Honest empty, never fabricated.** Where a shortcut needs a categorisation the plugin does not
  emit, return an honest empty/no-op (and surface the missing-signal gap) — never invent a result.

## Hard boundaries — do NOT
- Do NOT build the cursor/session model (see above) — stateless only.
- Do NOT build `search_semantic` or `find_dead_code` — those are **Wave 5 (WS5b)**, they need
  infrastructure beyond a catalog query. (`…-ws5b-advanced-queries-plan.md`.)
- Do NOT build guidance AUTHORING — `propose_guidance`, `promote_guidance`, the `clarion guidance`
  CLI, staleness review are **WS6 (Wave 6)**. WS5 owns guidance READ (`guidance_for`) only.
- Do NOT add analyze-time precompute. Do NOT edit Accepted ADRs. Do NOT touch archived docs.

## Method
- Use superpowers:executing-plans / subagent-driven-development, task-by-task. TDD: the bounded/
  pagination contract, the honest-empty behaviour, and the SEI-field join are test-first.
- Verify ground truth before building: which categorisation tags the plugin actually emits
  (entry-point/route/data-model/test) — ship tools that read existing tags, honest-empty for
  missing ones. If you extend plugin categorisation emission, Python CI gates apply.
- All ADR-023 Rust gates green (fmt, clippy -D warnings, build --bins, nextest, doc -D warnings,
  deny). Python gates if the plugin is touched.
- Invariants: opt-in (read surface + one enrich-write; no base-path cost), enrich-only, fail-closed
  (honest empty/failure, never false-green).

## Filigree
Track per CLAUDE.md (atomic start-work, `--actor`). Issues per tool group (inspection, faceted
search, shortcuts, emit_observation); cite WS5, the design spec, REQ-MCP-*, NFR-PERF-03,
ADR-028/030. Close as you land each.

## Definition of done (Wave 4 / WS5)
- The §3 tools are implemented, registered, stateless, bounded, SEI-carrying, and tested —
  including empty-result honesty where a categorisation signal is absent.
- Faceted search + the cheap shortcuts ship; `search_semantic` + `find_dead_code` are explicitly
  left to Wave 5 (not built here, not silently dropped).
- The `clarion-workflow` skill / MCP tool docs describe the new tools (stateless, SEI-carrying);
  the §8-cursor-model reconciliation note is filed as a doc task.
- All CI gates green.

When implemented, tested, and gate-green, request a code review and surface the result. Wave 4
ends at the complete stateless catalogue; the two advanced tools are Wave 5 (WS5b).
```
