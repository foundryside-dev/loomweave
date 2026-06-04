# MCP tool reference

The tools below are the core consult tools served by `clarion serve` over the
MCP stdio transport. The live 1.2.x surface exposes 39 tools, including
navigation, briefing, source inspection, guidance/finding enrichment, analyze
lifecycle, freshness, faceted search, and structural shortcuts. Connect an MCP
client and read `tools/list` for the complete, current catalogue.

!!! note "Default confidence is `resolved`"
    Graph-traversal tools (`callers_of`, `neighborhood`, `execution_paths_from`)
    return only **resolved** edges by default. Ambiguous static candidates and
    LLM-inferred edges are opt-in. Results carry a `scope_excludes` block naming
    static blind spots, so an empty section is never read as a guaranteed true
    negative.

---

## `entity_at(file, line)`

Returns the innermost entity whose source range contains the given file and
line, plus an `entity_context` evidence block: a `match_reason`
(`decorator_range` / `declaration` / `body_range` / `containing_range` /
`no_match`), the module→entity containing stack, the matched sub-ranges, any
same-granularity ambiguity alternatives, and index freshness. Paths are
normalised relative to the project root. A blank or comment line spanned only by
a module reports `containing_range` — never a fabricated exact match.

## `find_entity(pattern, kind?)`

Searches entities by id, name, short name, and stored summary text. Results are
paginated and ranked by full-text match where possible. Does **not** traverse
the graph and does **not** search on-demand `summary_cache` entries. Pass an
optional `kind` (`subsystem`, `function`, `class`, `module`) to filter — the way
to locate a subsystem without visually filtering results.

## `callers_of(id)`

Returns entities that call the given entity. Default confidence is `resolved`,
so ambiguous static candidates and inferred edges are excluded unless requested.
Ambiguous edges expand all candidates; inferred edges may trigger bounded LLM
dispatch. The result carries `scope_excludes` naming static blind spots, so an
empty callers list is never a guaranteed true negative.

## `execution_paths_from(id, max_depth=3)`

Returns bounded, calls-only execution paths starting at an entity. Results are
compact: a deduplicated `nodes` table plus `paths` as arrays of node ids, ranked
longest-first. Traversal stops at the server edge cap and the response is capped
at a maximum number of ranked paths; `truncated` / `truncation_reason` report
when an edge-cap or path-cap trims the result.

## `summary(id)`

Returns an on-demand, cached one-paragraph summary for one entity, dispatching
the LLM lazily. A module summary describes the module docstring and top-level
members, not an aggregation of contained summaries. If the model returns
non-JSON, the response degrades to a
deterministic `structural-fallback` summary built from the source, and that
fallback is cached so a retry is a free cache hit rather than a re-billed
failure.

## `issues_for(id, include_contained?)`

Returns Filigree issues attached to this entity, optionally including those on
contained entities. Filigree is an **enrichment source**: if unavailable, the
tool returns an `unavailable` envelope instead of failing. A `result_kind`
(`matched` / `no_matches` / `unavailable`) distinguishes a reachable-but-empty
Filigree from an unreachable one, and a `filigree_endpoint` block reports which
endpoint answered. Matched entries carry the issue's title, status, and priority
(fetched once per distinct issue). Includes an enrich-only `wardline_findings`
section reconciling Wardline findings to the entity by qualname.

## `neighborhood(id)`

Returns the one-hop neighborhood around an entity: callers, callees, container,
contained entities, references, and imports (`imports_in` = who imports this
module, `imports_out` = what it imports). Default confidence is `resolved`. When
the entity is a module, `references_in` / `references_out` are rolled up over the
symbols it contains (`references_rolled_up=true`), each neighbor carrying a `via`
naming the contained symbol the edge touches — so "who imports this module" is
answered at module altitude rather than reading empty. Carries `scope_excludes`.

## `subsystem_members(id)`

Lists the module entities assigned to a subsystem entity. The reverse lookup —
"which subsystem does this entity belong to?" — is `subsystem_of(id)`, which
accepts any entity id and resolves a function or class through its nearest
containing module.

## Additional catalogue

Use `tools/list` for exact schemas. The remaining tool families include:

- Source and orientation: `source_for_entity`, `call_sites`,
  `orientation_pack`, `project_status`, `summary_preview_cost`.
- Guidance and findings: `guidance_for`, `propose_guidance`,
  `promote_guidance`, `findings_for`, `wardline_for`.
- Analyze and freshness: `analyze_start`, `analyze_status`,
  `analyze_cancel`, `index_diff`.
- Facets and shortcuts: `find_by_tag`, `find_by_kind`, `find_by_wardline`,
  `find_circular_imports`, `find_coupling_hotspots`, `find_entry_points`,
  `find_http_routes`, `find_data_models`, `find_tests`, `find_deprecations`,
  `find_todos`, `what_tests_this`, `high_churn`, `recently_changed`,
  `find_dead_code`, `search_semantic`.
