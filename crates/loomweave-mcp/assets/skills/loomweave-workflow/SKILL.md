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
re-exploring the tree.** One `find_entity` + one `callers_of` answers "what
calls this?" — and one `entity_relation_list` answers "what subclasses this?" —
without reading a single file.

## When to use

- You're dropped into a codebase and need to locate a symbol or trace its callers/callees.
- You'd otherwise `grep`/read many files to answer a structural question.
- You need a function's neighborhood, execution paths, or which subsystem it belongs to.

**Not for:** editing code, reading exact implementation bodies (use `summary` or
read the file once you have its path), or codebases with no `.weft/loomweave/` index.

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
  Loomweave tool call (above).
- **`sei`** is the entity's *durable, stable identity*. It survives renames and
  moves. **When you record a cross-tool binding** — e.g. attaching a Filigree
  issue to a Loomweave entity — **bind on the `sei`, not the `id`.** A binding
  keyed on the mutable `id` silently breaks the first time the entity moves.

`sei` is `null` when the index predates SEI support or the entity has no binding
yet; `project_status` and `orientation_pack` report `sei.populated` so you can
tell which case you're in.

## Tools

| Tool | Use when | Args |
|------|----------|------|
| `find_entity` | locate an entity by name, or by a concept word in its docstring/identifier (substring) | `{"pattern": "<name-or-word>"}` |
| `entity_resolve` | resolve dotted qualnames (`pkg.mod.func`) to entity ids + SEIs — the inverse of having an id | `{"qualnames": ["pkg.mod.func"]}` |
| `entity_at` | what's at a file:line | `{"file": "rel/path.py", "line": 42}` |
| `callers_of` | what calls this entity (bounded: `limit`+`cursor`) | `{"id": "<id>"}` |
| `neighborhood` | one-hop callers+callees+container+contained+references+imports+relations (per-bucket `limit`) | `{"id": "<id>"}` |
| `entity_relation_list` | what subclasses X / what does a decorator decorate / what implements a trait — the `inherits_from`/`decorates`/`implements`/`derives` edges, with the anchoring source line | `{"id": "<id>", "direction": "in"}` |
| `execution_paths_from` | bounded call paths out of an entity | `{"id": "<id>", "max_depth": 5}` |
| `subsystem_members` | modules in a subsystem (bounded: `limit`+`cursor`) | `{"id": "core:subsystem:<hash>"}` |
| `subsystem_of` | the subsystem an entity belongs to (reverse of `subsystem_members`) | `{"id": "<id>"}` |
| `summary` † | on-demand prose summary of one entity | `{"id": "<id>"}` |
| `summary_preview_cost` | preview a `summary` call's cache status / cost before spending | `{"id": "<id>"}` |
| `issues_for` | Filigree issues attached to an entity | `{"id": "<id>"}` |
| `source_for_entity` | an entity's exact indexed source span + bounded context | `{"id": "<id>", "context_lines": 10}` |
| `call_sites` | the source line(s) behind a calls/references edge | `{"id": "<id>", "role": "caller"}` |
| `orientation_pack` | one deterministic orientation packet for an entity or file:line (entity + context + neighbors + paths + issues + freshness) | `{"file": "rel/path.py", "line": 42}` |
| `index_diff` | index freshness / drift vs. the current working tree | `{}` |
| `analyze_start` † | launch a background re-index, return its `run_id` | `{}` |
| `analyze_status` | poll a started analyze (queued/running/terminal + progress) | `{"run_id": "<id>"}` |
| `analyze_cancel` † | stop a running analyze (group-kills plugin + Pyright) | `{"run_id": "<id>"}` |
| `project_status` | index freshness, counts, LLM + Filigree status | `{}` |

† **Write-gated.** `summary` (`entity_summary_get`), `analyze_start`,
`analyze_cancel`, `propose_guidance`, and `promote_guidance` are registered only
when `serve.mcp.enable_write_tools: true` is set in `loomweave.yaml` (default
`false`). When the gate is off they do not appear in `tools/list` and a call
returns a tool-disabled error — run `loomweave config check` to see the active
policy. `summary` additionally requires the live LLM provider to be enabled
(`llm_policy.enabled: true` + `allow_live_provider: true`), or it serves cache
only.

`callers_of` / `neighborhood` / `execution_paths_from` / `entity_relation_list`
take a `confidence` tier — one of `"resolved"` (default; only high-confidence
edges), `"ambiguous"`, or `"inferred"`. There is no `"all"` value. When you
suspect an edge is missing (e.g. dynamic dispatch), re-query at `"ambiguous"`
and `"inferred"` and union the results — a default `resolved` count can
understate the true caller set. (Relation edges are never LLM-inferred, so for
`entity_relation_list` and the `relations_in`/`relations_out` buckets
`"ambiguous"` is the widest tier; `"inferred"` adds nothing.)

Of those, `callers_of` / `neighborhood` / `execution_paths_from` also return a
`scope_excludes` array listing static blind spots the query did **not** search
(e.g. `"attribute-receiver-calls"` like `ctx.svc.run()`). A non-empty
`scope_excludes` means an empty/short result is **not** a guaranteed true
negative — re-query at `"inferred"` (which searches those categories and returns
`scope_excludes: []`) before concluding "nothing calls this."
(`entity_relation_list` returns no `scope_excludes` and has no inferred tier;
its honesty caveat is in its description — only *declared* relations are
recorded, so a dynamically applied decorator or runtime-built class is
invisible.)

`execution_paths_from` returns a compact shape: `root`, a deduplicated `nodes`
table (id + short_name + location, each node once), and `paths` as arrays of
node-id strings ranked longest-first. Resolve a path id against `nodes`, not by
re-reading each path element. `truncated`/`truncation_reason` report `edge-cap`
(traversal stopped early) or `path-cap` (ranked output trimmed for size).

### Ids, SEIs, and `entity_resolve`

Every id-taking tool (`callers_of`, `neighborhood`, `summary`, `source_for_entity`,
`call_sites`, `wardline_for`, `issues_for`, `propose_guidance`, …) accepts **either**
a raw locator (`python:function:pkg.mod.func`) **or** a Stable Entity Identity
(SEI) token (`loomweave:eid:…`). A SEI is resolved through its alive binding to
the current entity; an orphaned/unknown SEI fails closed as `entity-not-found`.
You never have to convert a SEI before passing it. `find_entity` also accepts a
pasted SEI as an **exact** lookup (it returns the one entity that SEI binds to,
not a fuzzy match).

When you have a **dotted qualname** but no id — e.g. a name from a stack trace or
another tool — use `entity_resolve` (batch: `{"qualnames": ["a.b.c", …]}`, up to
2000). Each input yields one `results` entry **in input order** with a
`result_kind`:

- `resolved` — `candidates` has one `{ id, sei, kind }` you can feed straight
  into any id-taking tool.
- `unresolved` — `candidates` is empty. This is **honest-empty, not an error**:
  no entity matches that qualname.
- `ambiguous` — reserved for a future heuristic tier (the exact tier never
  emits it). A `scope_excludes` of `["heuristic-tier-not-implemented"]` records
  that only exact resolution ran.

A candidate whose entity is secret-scan-blocked collapses to the redacted stub
(id/sei withheld) — the same posture as every other identity surface.

### How `find_entity` matches — the grep replacement for "find the thing that does Y"

`find_entity` merges two recall paths so a concept word, not just an exact
identifier, lands a hit:

- **stemmed full-text ranking** over name / short name / summary, and
- **grep-equivalent substring recall** over name / short name / summary **and the
  entity's docstring**.

So a word that is only a *substring* of a compound identifier is discoverable —
`{"pattern": "library"}` finds the class `LibraryService`, which whole-token
full-text alone never matches — and a concept that lives only in docstring prose
(e.g. `borrow` mentioned in a `LoanPolicy` docstring) is found even when no
entity is named after it. This is the **always-on keyword-discovery path: reach
for `find_entity` before you grep.** It needs no embeddings — semantic *ranking*
is the separate, opt-in `search_semantic` (below). Full-text hits rank first,
then substring-only hits. Docstrings withheld by the secret scanner
(`briefing_blocked`) are never matched. A pasted **SEI** (`loomweave:eid:…`) is
treated as an exact lookup — it returns the single bound entity, not a fuzzy
substring scan over the token.

## Catalogue tools — inspection · faceted search · shortcuts

Beyond navigation, Loomweave serves a **stateless catalogue** of read tools. All
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
| `project_finding_list` | **every** finding across the project — no entity id needed; each row carries its anchoring entity `{id, sei, file, line}` + tool/rule/kind/severity/status | `{"filter": {"severity": "error"}}` |
| `wardline_for` | the entity's Wardline metadata (verbatim, opaque) | `{"id": "<id>"}` |

**Faceted search:**

| Tool | Use when | Args |
|------|----------|------|
| `find_by_tag` | entities carrying a categorisation tag | `{"tag": "<tag>", "scope": "src/**"}` |
| `find_by_kind` | entities of a kind (`function`/`class`/`module`/…) | `{"kind": "function"}` |
| `find_by_wardline` | entities by Wardline tier/group (best-effort); pass `has_findings:true` to page only taint-fact entities that also carry a finding | `{"tier": "exact", "has_findings": true}` |

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
categorisation shortcuts read plugin-emitted tags. The Python plugin emits
conservative tags for common conventions (`entry-point`, `http-route`, `test`,
`data-model`, `cli-command`, `exported-api`), so root/tag shortcuts and
`find_dead_code` light up on freshly analyzed Python projects where those
signals are present. `find_deprecations` / `find_todos` still return
honest-empty unless a plugin emits those tags. Likewise `high_churn` and
`recently_changed` are honest-empty until churn/change signals are populated (use
`index_diff` for repo-level freshness).

`search_semantic` is also in the catalogue — embedding-similarity *ranking* for a
natural-language query. It is opt-in under `semantic_search:`; when enabled,
`loomweave analyze` populates the git-ignored `.weft/loomweave/embeddings.db`
sidecar and the query path filters stale vectors by content hash. When it is off
(the default) it returns `result_kind: "not_enabled"` rather than a fabricated or
empty-as-complete result — **that is not a dead end: `find_entity` already does
keyword/substring/docstring discovery with no embeddings required** (see "How
`find_entity` matches" above), so it is the right reach for "find the thing that
does Y" out of the box.

> Not in this catalogue: `emit_observation` as a general-purpose write surface.

**Guidance authoring has an operator boundary.** Operators can manage sheets via
`loomweave guidance create/edit/show/list/delete/promote` (plus `export`/`import`
for team sharing). Agents may call `propose_guidance` to create a Filigree
observation, but that proposal is inert until an operator promotes it through
`promote_guidance` or the CLI. Promoted sheets reach you through `guidance_for`
and are composed into `summary` prompts with a real guidance fingerprint.
(`propose_guidance` and `promote_guidance` are write-gated — see the † note above.)

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
- **`find_entity` is paginated** (~20/page, `next_cursor`); a broad concept word
  now matches docstring/identifier substrings too, so it can return many hits —
  narrow the pattern (or add a `kind` filter) rather than paging if you can.
- **`callers_of` and `subsystem_members` are bounded** (`limit` default 50, max
  100, plus a numeric-offset `cursor`). Each response carries `next_cursor`
  (null when exhausted) and an explicit `truncated` flag — re-call with
  `{"cursor": "<next_cursor>"}` to walk the full set. An empty page on a non-null
  cursor means you paged past the end.
- **`neighborhood` caps each bucket independently** with one per-bucket `limit`
  and reports a `truncated` **map** (`{callers, callees, contained,
  references_in, references_out, imports_in, imports_out, relations_in,
  relations_out}`) — it has **no cursor**. When a bucket is `truncated:true`,
  switch to that relation's dedicated cursor-paginated tool (e.g. `callers_of`,
  `entity_relation_list`) for the complete set; `neighborhood` is a one-hop
  overview, not a paging surface.
- **Relation direction reads as a sentence** (`from KIND to`, ADR-051):
  `entity_relation_list` with `direction: "in"` on a class answers "what
  subclasses / implements / derives this"; `direction: "out"` on a *decorator*
  answers "what does this decorate" (the decorator is the FROM side — inverted
  from where the `@decorator` line sits). Each entry carries the anchoring
  file/line/line-text so you can see the declaration behind the edge.

## Launch

`loomweave serve --path <dir>` where `<dir>` contains `.weft/loomweave/loomweave.db`
(built by `loomweave analyze <dir>`). In an MCP client the tools appear as
`mcp__loomweave__find_entity`, etc.

Besides the tools, the server exposes a `loomweave://context` **resource** — live
entity/subsystem/finding counts and index freshness as JSON, a lightweight read
when you only want the numbers (`project_status` is the fuller tool-based view).
