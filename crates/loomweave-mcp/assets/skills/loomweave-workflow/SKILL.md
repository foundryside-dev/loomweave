---
name: loomweave-workflow
description: >
  Use when orienting in an unfamiliar or large codebase and you want to avoid
  re-reading or grepping the whole source tree: answering "what calls X",
  "where is X defined", "what does X depend on", "what subsystem is X in", or
  "find the function/class/module that does Y". Applies whenever a Loomweave
  code-archaeology MCP server (loomweave serve / mcp__loomweave__* tools) is
  available for the project.
---

# Loomweave Workflow

## Overview

Loomweave pre-extracts a codebase into a queryable map — entities (functions,
classes, modules, files), the call/reference/import edges between them, the
relation edges (`inherits_from`/`decorates`/`implements`/`derives`), and
subsystem clusters — and serves it over MCP. **Ask Loomweave instead of
re-exploring the tree.** One `entity_find` + one `entity_callers_list` answers
"what calls this?" — and one `entity_relation_list` answers "what subclasses
this?" — without reading a single file.

## When to use

- You're dropped into a codebase and need to locate a symbol or trace its callers/callees.
- You'd otherwise `grep`/read many files to answer a structural question.
- You need a function's neighborhood, execution paths, or which subsystem it belongs to.

**Not for:** editing code, reading exact implementation bodies (use
`entity_summary_get` or read the file once you have its path), or codebases
with no `.weft/loomweave/` index.

## Ids and SEIs — the one rule you must not get wrong

Ids are `{plugin}:{kind}:{qualified_name}`; subsystems are
`core:subsystem:{hash}`. **Never hand-construct one** — take it from
`entity_find` / `entity_at` / `entity_resolve` and copy it verbatim into the
next tool. Every id-taking tool also accepts a SEI token (`loomweave:eid:…`).

`id` is a *mutable locator* (it changes on rename/move); `sei` is the *durable
identity*. **Record cross-tool bindings on the `sei`, never the `id`** — e.g.
attaching a Filigree issue to a Loomweave entity. Full model, `entity_resolve`
semantics, and how `entity_find` matches: `references/entity-ids.md`,
`references/tools.md`.

## Workflow: orient, then navigate

1. **Anchor.** `entity_find` by name (or `entity_at` for a file:line) to get the
   entity and its `id`. For a code location you're about to dig into, prefer
   `entity_orientation_pack_get` — it returns the entity, its context, one-hop
   neighbors, execution paths, attached issues, and index freshness in one
   deterministic call, instead of hand-composing those queries.
2. **Navigate.** Feed that `id` into `entity_callers_list`,
   `entity_neighborhood_get`, `entity_execution_path_list`, or
   `entity_summary_get`. Chain results' IDs to keep walking.

## Freshness — the one hard rule

Every answer is a claim about the tree *as of the last analyze*. **If
`project_status_get` reports `stale` or `stale_worktree`, refresh before you
answer** — `analyze_start {}` is incremental, non-blocking and collision-safe,
and a staleness caveat is not a substitute for it. Read-only work is where
stale poisons you most: nothing downstream catches the wrong answer. The full
cycle, the one observable skip (`index_diff_get` disjointness), the
rationalization table, and the red flags: `references/freshness.md`.

## References

Load on demand — these carry the detail this file used to inline:

- `references/entity-ids.md` — the entity-ID model and `id` vs `sei` in full.
- `references/tools.md` — the navigation tool table, write-gating and the
  gate-exempt bootstrap tools, `confidence` tiers, `scope_excludes` and the
  `unresolved_name_matches` recovery path, `entity_resolve`, and how
  `entity_find` matches.
- `references/catalogue.md` — the stateless catalogue (inspection, faceted
  search, exploration-elimination shortcuts), tag-classification evidence,
  semantic search, per-tool operational notes, and the guidance-authoring
  operator boundary.
- `references/freshness.md` — manual scanning: why stale poisons read-only
  work, the analyze cycle, the one valid skip, the rationalization table, and
  the red flags.
- `references/gotchas.md` — subsystem lookup, pagination/truncation shapes,
  relation direction, launch, and legacy aliases.
