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

### `id` vs `sei` — which one to bind on

Every entity in a tool response now carries an `sei` field alongside its `id`.
They are not interchangeable:

- **`id`** is the entity's *locator* — a mutable address. It changes when the
  code is renamed or moved, and it's the right thing to feed into the next
  Clarion tool call (above).
- **`sei`** is the entity's *durable, stable identity*. It survives renames and
  moves. **When you record a cross-tool binding** — e.g. attaching a Filigree
  issue to a Clarion entity — **bind on the `sei`, not the `id`.** A binding
  keyed on the mutable `id` silently breaks the first time the entity moves.

`sei` is `null` when the index predates SEI support or the entity has no binding
yet; `project_status` and `orientation_pack` report `sei.populated` so you can
tell which case you're in.

## Tools

| Tool | Use when | Args |
|------|----------|------|
| `find_entity` | locate an entity by name/text | `{"pattern": "<name>"}` |
| `entity_at` | what's at a file:line | `{"file": "rel/path.py", "line": 42}` |
| `callers_of` | what calls this entity | `{"id": "<id>"}` |
| `neighborhood` | one-hop callers+callees+container+contained+references+imports | `{"id": "<id>"}` |
| `execution_paths_from` | bounded call paths out of an entity | `{"id": "<id>", "max_depth": 5}` |
| `subsystem_members` | modules in a subsystem | `{"id": "core:subsystem:<hash>"}` |
| `subsystem_of` | the subsystem an entity belongs to (reverse of `subsystem_members`) | `{"id": "<id>"}` |
| `summary` | on-demand prose summary of one entity | `{"id": "<id>"}` |
| `summary_preview_cost` | preview a `summary` call's cache status / cost before spending | `{"id": "<id>"}` |
| `issues_for` | Filigree issues attached to an entity | `{"id": "<id>"}` |
| `source_for_entity` | an entity's exact indexed source span + bounded context | `{"id": "<id>", "context_lines": 10}` |
| `call_sites` | the source line(s) behind a calls/references edge | `{"id": "<id>", "role": "caller"}` |
| `orientation_pack` | one deterministic orientation packet for an entity or file:line (entity + context + neighbors + paths + issues + freshness) | `{"file": "rel/path.py", "line": 42}` |
| `index_diff` | index freshness / drift vs. the current working tree | `{}` |
| `analyze_start` | launch a background re-index, return its `run_id` | `{}` |
| `analyze_status` | poll a started analyze (queued/running/terminal + progress) | `{"run_id": "<id>"}` |
| `analyze_cancel` | stop a running analyze (group-kills plugin + Pyright) | `{"run_id": "<id>"}` |
| `project_status` | index freshness, counts, LLM + Filigree status | `{}` |

`callers_of` / `neighborhood` / `execution_paths_from` take a `confidence`
tier — one of `"resolved"` (default; only high-confidence edges),
`"ambiguous"`, or `"inferred"`. There is no `"all"` value. When you suspect an
edge is missing (e.g. dynamic dispatch), re-query at `"ambiguous"` and
`"inferred"` and union the results — a default `resolved` count can understate
the true caller set.

These three tools also return a `scope_excludes` array listing static blind
spots the query did **not** search (e.g. `"attribute-receiver-calls"` like
`ctx.svc.run()`). A non-empty
`scope_excludes` means an empty/short result is **not** a guaranteed true
negative — re-query at `"inferred"` (which searches those categories and returns
`scope_excludes: []`) before concluding "nothing calls this."

`execution_paths_from` returns a compact shape: `root`, a deduplicated `nodes`
table (id + short_name + location, each node once), and `paths` as arrays of
node-id strings ranked longest-first. Resolve a path id against `nodes`, not by
re-reading each path element. `truncated`/`truncation_reason` report `edge-cap`
(traversal stopped early) or `path-cap` (ranked output trimmed for size).

## Catalogue tools — inspection · faceted search · shortcuts

Beyond navigation, Clarion serves a **stateless catalogue** of read tools. All
of them: take explicit ids/scopes (no cursor/session — there is no `goto`/`back`
state to manage); **paginate** (`limit`/`offset`, with a `page` block reporting
`total`/`returned`/`truncated` — no silent caps); carry `sei` on every entity
they return; and are **honest-empty** — where a signal isn't present they return
an empty result with a `signal` note (`available:false`, the reason), never a
fabricated answer.

`scope?` (where accepted) takes **either** an entity id (→ that entity's
descendants) **or** a path glob (`"src/auth/**"`); omit it for the whole project.

**Inspection (read):**

| Tool | Use when | Args |
|------|----------|------|
| `guidance_for` | guidance sheets applicable to an entity, scope-ranked | `{"id": "<id>"}` |
| `findings_for` | findings anchored to an entity (filter kind/severity/status) | `{"id": "<id>", "filter": {"status": "open"}}` |
| `wardline_for` | the entity's Wardline metadata (verbatim, opaque) | `{"id": "<id>"}` |

**Faceted search:**

| Tool | Use when | Args |
|------|----------|------|
| `find_by_tag` | entities carrying a categorisation tag | `{"tag": "<tag>", "scope": "src/**"}` |
| `find_by_kind` | entities of a kind (`function`/`class`/`module`/…) | `{"kind": "function"}` |
| `find_by_wardline` | entities by Wardline tier/group (best-effort) | `{"tier": "exact"}` |

**Exploration-elimination shortcuts** (on-demand graph/index queries — no
analyze-time precompute):

| Tool | Use when |
|------|----------|
| `find_circular_imports` | import cycles (SCCs over `imports` edges) |
| `find_coupling_hotspots` | entities ranked by fan-in + fan-out |
| `find_entry_points` / `find_http_routes` / `find_data_models` / `find_tests` | entities by categorisation tag |
| `find_deprecations` / `find_todos` | deprecated / TODO-tagged entities |
| `what_tests_this` | test-tagged callers of an entity |
| `high_churn` | entities ranked by git churn |
| `recently_changed` | entities changed since a timestamp |

`find_circular_imports` and `find_coupling_hotspots` are edge-derived, so they
take a `confidence` tier (default `resolved`, a ceiling) and echo it. The
categorisation shortcuts read plugin-emitted tags; the **Python plugin emits no
categorisation tags today**, so `find_entry_points`/`find_http_routes`/
`find_data_models`/`find_tests`/`find_deprecations`/`find_todos`/`what_tests_this`
return honest-empty with a missing-signal note — an empty result means "the
signal is absent", not "there is nothing here". Likewise `high_churn` and
`recently_changed` are honest-empty until churn/change signals are populated (use
`index_diff` for repo-level freshness).

> Not in this catalogue: `search_semantic` and `find_dead_code` (need embedding
> / whole-graph-reachability infrastructure — a separate wave) and
> `emit_observation` as a general-purpose write surface.

**Guidance authoring has an operator boundary.** Operators can manage sheets via
`clarion guidance create/edit/show/list/delete/promote` (plus `export`/`import`
for team sharing). Agents may call `propose_guidance` to create a Filigree
observation, but that proposal is inert until an operator promotes it through
`promote_guidance` or the CLI. Promoted sheets reach you through `guidance_for`
and are composed into `summary` prompts with a real guidance fingerprint.

## Workflow: orient, then navigate

1. **Anchor.** `find_entity` by name (or `entity_at` for a file:line) to get the
   entity and its `id`. For a code location you're about to dig into, prefer
   `orientation_pack` — it returns the entity, its context, one-hop neighbors,
   execution paths, attached issues, and index freshness in one deterministic
   call, instead of hand-composing those queries.
2. **Navigate.** Feed that `id` into `callers_of`, `neighborhood`,
   `execution_paths_from`, or `summary`. Chain results' IDs to keep walking.

## Gotchas (read before hunting for a subsystem)

- **To find a package's subsystem, search the package NAME with `kind`.**
  Subsystems are *named after* their dominant package (e.g. `mypkg`), so
  `find_entity {"pattern":"subsystem"}` returns nothing. Search the package name
  and pass `{"kind":"subsystem"}` to return only subsystem entities, then call
  `subsystem_members`. (`find_entity` accepts an optional `kind` filter —
  `"subsystem"`, `"function"`, `"class"`, `"module"`, …; omit it for no filter.)
- **To go from an entity to its subsystem, use `subsystem_of`.**
  `neighborhood` does **not** return the entity's subsystem. Call
  `subsystem_of {"id": "<entity-id>"}` — it accepts any entity (a function/class
  resolves through its containing module) and returns the subsystem plus the
  module it resolved through. `subsystem_members` is the forward direction.
- **`find_entity` is paginated** (~20/page, `next_cursor`); narrow the pattern
  rather than paging if you can.

## Launch

`clarion serve --path <dir>` where `<dir>` contains `.clarion/clarion.db`
(built by `clarion analyze <dir>`). In an MCP client the tools appear as
`mcp__clarion__find_entity`, etc.

Besides the tools, the server exposes a `clarion://context` **resource** — live
entity/subsystem/finding counts and index freshness as JSON, a lightweight read
when you only want the numbers (`project_status` is the fuller tool-based view).
