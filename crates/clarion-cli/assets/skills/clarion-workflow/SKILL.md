---
name: clarion-workflow
description: >
  Use when orienting in an unfamiliar or large codebase and you want to avoid
  re-reading or grepping the whole source tree: answering "what calls X",
  "where is X defined", "what does X depend on", "what subsystem is X in", or
  "find the function/class/module that does Y". Applies whenever a Clarion
  code-archaeology MCP server (clarion serve / mcp__clarion__* tools) is
  available for the project.
---

# Clarion Workflow

## Overview

Clarion pre-extracts a codebase into a queryable map — entities (functions,
classes, modules, files), the call/reference/import edges between them, and
subsystem clusters — and serves it over MCP. **Ask Clarion instead of
re-exploring the tree.** One `find_entity` + one `callers_of` answers "what
calls this?" without reading a single file.

## When to use

- You're dropped into a codebase and need to locate a symbol or trace its callers/callees.
- You'd otherwise `grep`/read many files to answer a structural question.
- You need a function's neighborhood, execution paths, or which subsystem it belongs to.

**Not for:** editing code, reading exact implementation bodies (use `summary` or
read the file once you have its path), or codebases with no `.clarion/` index.

## Entity IDs — the model

Every entity has an ID: `{plugin}:{kind}:{qualified_name}`
(e.g. `python:function:pkg.mod.func`, `python:class:pkg.mod.Cls`,
`python:module:pkg.mod`). Subsystems are `core:subsystem:{hash}`.

**You almost never type IDs.** Get one from `find_entity` / `entity_at`, then
**copy it verbatim** into the next tool. Don't hand-construct or guess IDs.

## Tools

| Tool | Use when | Args |
|------|----------|------|
| `find_entity` | locate an entity by name/text | `{"pattern": "<name>"}` |
| `entity_at` | what's at a file:line | `{"file": "rel/path.py", "line": 42}` |
| `callers_of` | what calls this entity | `{"id": "<id>"}` |
| `neighborhood` | one-hop callers+callees+container+contained | `{"id": "<id>"}` |
| `execution_paths_from` | bounded call paths out of an entity | `{"id": "<id>", "max_depth": 5}` |
| `subsystem_members` | modules in a subsystem | `{"id": "core:subsystem:<hash>"}` |
| `summary` | on-demand prose summary of one entity | `{"id": "<id>"}` |
| `issues_for` | Filigree issues attached to an entity | `{"id": "<id>"}` |
| `project_status` | index freshness, counts, LLM + Filigree status | `{}` |

`callers_of` / `neighborhood` / `execution_paths_from` take a `confidence`
tier — one of `"resolved"` (default; only high-confidence edges),
`"ambiguous"`, or `"inferred"`. There is no `"all"` value. When you suspect an
edge is missing (e.g. dynamic dispatch), re-query at `"ambiguous"` and
`"inferred"` and union the results — a default `resolved` count can understate
the true caller set.

## Workflow: orient, then navigate

1. **Anchor.** `find_entity` by name (or `entity_at` for a file:line) to get the
   entity and its `id`.
2. **Navigate.** Feed that `id` into `callers_of`, `neighborhood`,
   `execution_paths_from`, or `summary`. Chain results' IDs to keep walking.

## Gotchas (read before hunting for a subsystem)

- **To find a package's subsystem, search the package NAME, not "subsystem".**
  Subsystems are *named after* their dominant package (e.g. `mypkg`), so
  `find_entity {"pattern":"subsystem"}` returns nothing. Search the package name
  and pick the result whose `kind` is `subsystem`, then call `subsystem_members`.
- **There is no module→subsystem reverse lookup and no kind filter.**
  `neighborhood` does **not** return the entity's subsystem. Membership is only
  reachable forward via `subsystem_members(subsystem_id)`.
- **`find_entity` is paginated** (~20/page, `next_cursor`); narrow the pattern
  rather than paging if you can.

## Launch

`clarion serve --path <dir>` where `<dir>` contains `.clarion/clarion.db`
(built by `clarion analyze <dir>`). In an MCP client the tools appear as
`mcp__clarion__find_entity`, etc.
