## Loomweave (code archaeology)

This repo is indexed by Loomweave: it has pre-extracted the tree into a
queryable map of entities (functions, classes, modules, files), the call /
reference / import edges between them, and subsystem clusters. Before grepping
or re-reading the tree to answer "what calls X", "where is X defined", "what
subsystem owns X", or "find the thing that does Y" — ask Loomweave's MCP tools
(`mcp__loomweave__*`): `entity_find`, `entity_at`, `entity_callers_list`,
`entity_neighborhood_get`, `project_status_get`.

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
