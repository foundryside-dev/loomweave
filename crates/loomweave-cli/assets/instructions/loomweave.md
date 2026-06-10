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

Entity IDs are `{plugin}:{kind}:{qualified_name}` (e.g.
`python:function:pkg.mod.func`); subsystems are `core:subsystem:{hash}`. You
rarely type IDs — get one from `entity_find` or `entity_at`, then copy it
verbatim into the next tool.

Index freshness and counts: `project_status_get` (or the `loomweave://context`
resource). If the index is stale, run `loomweave analyze <path>`.

LLM summaries (`entity_summary_get`) are off by default and need a configured live
provider; `project_status_get` reports the posture and `loomweave config check`
explains how to enable it.

Full workflow: the `loomweave-workflow` skill.
