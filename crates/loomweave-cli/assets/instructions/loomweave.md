## Loomweave (code archaeology)

This repo is indexed by Loomweave: it has pre-extracted the tree into a
queryable map of entities (functions, classes, modules, files), the call /
reference / import edges plus relation edges (inherits_from / decorates /
implements / derives), and subsystem clusters. Before grepping the tree to
answer "what calls X", "what subclasses X", "where is X defined", "what
subsystem owns X", or "find the thing that does Y" — ask Loomweave's MCP tools
(`mcp__loomweave__*`): `entity_find`, `entity_at`, `entity_callers_list`,
`entity_relation_list`, `entity_neighborhood_get`, `project_status_get`.

`entity_find` is the grep replacement for "find the thing that does Y": it
matches a concept word by substring over name, summary, and docstring content
(e.g. `library` finds `LibraryService`), with no embeddings required — reach for
it before grepping. Semantic *ranking* is the separate, opt-in
`entity_semantic_search_list`.

Entity IDs are `{plugin}:{kind}:{qualified_name}`; subsystems are
`core:subsystem:{hash}`. Never hand-construct one: get it from `entity_find` /
`entity_at`, or — for a pasted qualname, Rust `::` path, or SEI token — from
`entity_resolve`, then copy it verbatim into the next tool.

Index freshness and counts: `project_status_get` (or the `loomweave://context`
resource). If the index is stale, run `loomweave analyze <path>`.

LLM summaries (`entity_summary_get`) are off by default and need a live
provider; `project_status_get` reports the posture, `loomweave config check`
explains enabling.

Full workflow: the `loomweave-workflow` skill.
