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

## Entity IDs — the model

Every entity has an ID: `{plugin}:{kind}:{qualified_name}`
(e.g. `python:function:pkg.mod.func`, `python:class:pkg.mod.Cls`,
`python:module:pkg.mod`). Subsystems are `core:subsystem:{hash}`.

**You almost never type IDs.** Get one from `entity_find` / `entity_at`, then
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
yet; `project_status_get` and `entity_orientation_pack_get` report
`sei.populated` so you can tell which case you're in.

## Tools

| Tool | Use when | Args |
|------|----------|------|
| `entity_find` | locate an entity by name, or by a concept word in its docstring/identifier (substring) | `{"pattern": "<name-or-word>"}` |
| `entity_resolve` | resolve pasted identifiers — dotted qualnames, Rust `::` paths, SEI tokens — to entity ids + SEIs (any kind; optional `kind`/`plugin` constraints) | `{"qualnames": ["pkg.mod.Cls", "crate::mod::func"]}` |
| `entity_at` | what's at a file:line | `{"file": "rel/path.py", "line": 42}` |
| `entity_callers_list` | what calls this entity (bounded: `limit`+`cursor`) | `{"id": "<id>"}` |
| `entity_neighborhood_get` | one-hop callers+callees+container+contained+references+imports+relations (per-bucket `limit`) | `{"id": "<id>"}` |
| `entity_relation_list` | what subclasses X / what does a decorator decorate / what implements a trait — the `inherits_from`/`decorates`/`implements`/`derives` edges, with the anchoring source line | `{"id": "<id>", "direction": "in"}` |
| `entity_execution_path_list` | bounded call paths out of an entity | `{"id": "<id>", "max_depth": 5}` |
| `subsystem_member_list` | modules in a subsystem (bounded: `limit`+`cursor`) | `{"id": "core:subsystem:<hash>"}` |
| `entity_subsystem_get` | the subsystem an entity belongs to (reverse of `subsystem_member_list`) | `{"id": "<id>"}` |
| `entity_summary_get` † | on-demand prose summary of one entity | `{"id": "<id>"}` |
| `entity_summary_preview_cost_get` | preview an `entity_summary_get` call's cache status / cost before spending | `{"id": "<id>"}` |
| `entity_issue_list` | Filigree issues attached to an entity | `{"id": "<id>"}` |
| `entity_source_get` | an entity's exact indexed source span + bounded context | `{"id": "<id>", "context_lines": 10}` |
| `entity_call_site_list` | the source line(s) behind a calls/references edge | `{"id": "<id>", "role": "caller"}` |
| `entity_orientation_pack_get` | one deterministic orientation packet for an entity or file:line (entity + context + neighbors + paths + issues + freshness) | `{"file": "rel/path.py", "line": 42}` |
| `llm_config_get` | inspect configured LLM/provider/live and MCP write-tool settings | `{}` |
| `llm_config_set` | update local `loomweave.yaml` LLM/provider/live/write-tool settings; reconnect after changes | `{"provider": "codex_sidecar", "enabled": true, "allow_live_provider": true, "enable_write_tools": true}` |
| `semantic_config_get` | inspect semantic-search embedding provider and sidecar diagnostics | `{}` |
| `semantic_config_set` | update local semantic-search embedding settings; rerun analyze and reconnect | `{"provider": "local_openai", "enabled": true, "endpoint_url": "http://127.0.0.1:11434/v1", "model_id": "nomic-embed-text", "dimensions": 768}` |
| `index_diff_get` | index freshness / drift vs. the current working tree | `{}` |
| `analyze_start` † | launch a background re-index, return its `run_id` | `{}` |
| `analyze_status_get` | poll a started analyze (queued/running/terminal + progress) | `{"run_id": "<id>"}` |
| `analyze_cancel` † | stop a running analyze (group-kills plugin + Pyright) | `{"run_id": "<id>"}` |
| `project_status_get` | index freshness, counts, LLM + Filigree status | `{}` |

† **Write-gated.** `entity_summary_get`, `analyze_start`,
`analyze_cancel`, `propose_guidance`, and `promote_guidance` are registered only
when `serve.mcp.enable_write_tools: true` is set in `loomweave.yaml` (default
`false`). When the gate is off they do not appear in `tools/list` and a call
returns a tool-disabled error — run `loomweave config check` to see the active
policy. `entity_summary_get` additionally requires the live LLM provider to be
enabled (`llm_policy.enabled: true` + `allow_live_provider: true`), or it
serves cache only.

**Gate-exempt bootstrap tools.** `llm_config_set` and `semantic_config_set`
deliberately BYPASS the write-tool gate (by design — the gate itself is one of
the settings they edit, so a read-only session could otherwise never bootstrap
write access). Treat them as write tools even when every other write surface is
gated off: from a read-only session they persistently edit `loomweave.yaml` and
can enable write tools, live (paid) LLM summaries, and live embedding spend.
Their effects survive the session; reconnect after changes for the new policy
to take effect.

`entity_callers_list` / `entity_neighborhood_get` /
`entity_execution_path_list` / `entity_relation_list` take a `confidence`
tier — one of `"resolved"` (default; only high-confidence
edges), `"ambiguous"`, or `"inferred"`. There is no `"all"` value. When you
suspect an edge is missing (e.g. dynamic dispatch), re-query at `"ambiguous"`
and union the results — a default `resolved` count can understate the true
caller set. (Relation edges are never LLM-inferred, so for
`entity_relation_list` and the `relations_in`/`relations_out` buckets
`"ambiguous"` is the widest tier; `"inferred"` adds nothing.)

**`"inferred"` is policy-gated.** It may call an LLM and write inferred-edge
cache rows, so it is rejected (`-32602`) unless the server runs with
`serve.mcp.enable_write_tools: true` — and the default is `false`. Do not plan
on `"inferred"` as your recovery path unless `project_status_get` shows write
tools enabled.

Of those, `entity_callers_list` / `entity_neighborhood_get` /
`entity_execution_path_list` also return a `scope_excludes` array listing
static blind spots the query did **not** search:
`"attribute-receiver-calls"` (like `ctx.svc.run()`) and
`"unresolved-static-calls"` (the project holds call sites the static resolver
could not bind — common for cross-module/cross-crate calls). A non-empty
`scope_excludes` means an empty/short result is **not** a guaranteed true
negative.

The recovery path that works in **every** posture: `entity_callers_list` and
`entity_neighborhood_get` also return `unresolved_name_matches` — the count of
unresolved call sites whose callee expression name-matches the entity — with a
`next_action` pointer when it is non-zero. If `callers` is empty but
`unresolved_name_matches > 0`, the truth is "N likely callers exist that
static resolution could not bind": run `entity_call_site_list`
(`{"id": "<id>", "role": "callee"}`) to see each one with file/line/line_text,
and treat those as caller candidates. Only when write tools are enabled is
re-querying at `"inferred"` (LLM-assisted binding, returns
`scope_excludes: []`) an alternative.
(`entity_relation_list` returns no `scope_excludes` and has no inferred tier;
its honesty caveat is in its description — only *declared* relations are
recorded, so a dynamically applied decorator or runtime-built class is
invisible.)

`entity_execution_path_list` returns a compact shape: `root`, a deduplicated
`nodes` table (id + short_name + location, each node once), and `paths` as
arrays of node-id strings ranked longest-first. Resolve a path id against `nodes`, not by
re-reading each path element. `truncated`/`truncation_reason` report `edge-cap`
(traversal stopped early) or `path-cap` (ranked output trimmed for size).

### Ids, SEIs, and `entity_resolve`

Every id-taking tool (`entity_callers_list`, `entity_neighborhood_get`,
`entity_summary_get`, `entity_source_get`, `entity_call_site_list`,
`entity_wardline_get`, `entity_issue_list`, `propose_guidance`, …) accepts
**either** a raw locator (`python:function:pkg.mod.func`) **or** a Stable
Entity Identity
(SEI) token (`loomweave:eid:…`). A SEI is resolved through its alive binding to
the current entity; an orphaned/unknown SEI fails closed as `entity-not-found`.
You never have to convert a SEI before passing it. `entity_find` also accepts a
pasted SEI as an **exact** lookup (it returns the one entity that SEI binds to,
not a fuzzy match).

When you have an **identifier but no id** — a dotted qualname from a stack
trace, wardline `explain_taint`, a dossier, or legis `policy_explain`; a Rust
`::` path from a compiler error (normalized to the stored dotted form
automatically); or an SEI pasted from a Filigree association — use
`entity_resolve` (batch: `{"qualnames": ["a.b.c", "crate::mod::func",
"loomweave:eid:…"]}`, up to 2000, entries may mix forms). **Never hand-construct
a `{plugin}:{kind}:{qualname}` id.** All qualname-dialect entity kinds
participate (function, class, module, struct, trait, …); narrow with `kind`
and/or `plugin`, both hard constraints (an unknown value matches nothing —
honest `unresolved`, never an error; constraints don't apply to SEI entries,
which are already exact). Each input yields one `results` entry **in input
order**, echoing the input as `qualname`, with a `result_kind`:

- `resolved` — `candidates` has one `{ id, sei, kind }` you can feed straight
  into any id-taking tool.
- `unresolved` — `candidates` is empty. This is **honest-empty, not an error**:
  no entity matches that qualname (or a constraint excluded every match).
- `ambiguous` — the qualname exists under more than one `(plugin, kind)`;
  every candidate is listed (sorted). Constrain with `kind`/`plugin` to
  collapse it. A `scope_excludes` of `["heuristic-tier-not-implemented"]`
  records that only exact resolution ran.

A candidate whose entity is secret-scan-blocked collapses to the redacted stub
(id/sei withheld) — the same posture as every other identity surface.

### How `entity_find` matches — the grep replacement for "find the thing that does Y"

`entity_find` merges two recall paths so a concept word, not just an exact
identifier, lands a hit:

- **stemmed full-text ranking** over name / short name / summary, and
- **grep-equivalent substring recall** over name / short name / summary **and the
  entity's docstring**.

So a word that is only a *substring* of a compound identifier is discoverable —
`{"pattern": "library"}` finds the class `LibraryService`, which whole-token
full-text alone never matches — and a concept that lives only in docstring prose
(e.g. `borrow` mentioned in a `LoanPolicy` docstring) is found even when no
entity is named after it. This is the **always-on keyword-discovery path: reach
for `entity_find` before you grep.** It needs no embeddings — semantic *ranking*
is the separate, opt-in `entity_semantic_search_list` (below). Full-text hits
rank first, then substring-only hits. Docstrings withheld by the secret scanner
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
| `entity_guidance_list` | guidance sheets applicable to an entity, scope-ranked | `{"id": "<id>"}` |
| `entity_finding_list` | findings anchored to an entity (filter kind/severity/status) | `{"id": "<id>", "filter": {"status": "open"}}` |
| `project_finding_list` | **every** finding across the project — no entity id needed; each row carries its anchoring entity `{id, sei, file, line}` + tool/rule/kind/severity/status | `{"filter": {"severity": "ERROR"}}` |
| `entity_wardline_get` | the entity's Wardline metadata (verbatim, opaque) | `{"id": "<id>"}` |

**Faceted search:**

| Tool | Use when | Args |
|------|----------|------|
| `entity_tag_list` | entities carrying a categorisation tag | `{"tag": "<tag>", "scope": "src/**"}` |
| `entity_kind_list` | entities of a kind (`function`/`class`/`module`/…) | `{"kind": "function"}` |
| `entity_wardline_list` | entities by Wardline tier/group (best-effort); pass `has_findings:true` to page only taint-fact entities that also carry a finding | `{"tier": "exact", "has_findings": true}` |

**Exploration-elimination shortcuts** (on-demand graph/index queries — no
analyze-time precompute):

| Tool | Use when |
|------|----------|
| `module_circular_import_list` | import cycles (SCCs over `imports` edges) |
| `entity_coupling_hotspot_list` | entities ranked by fan-in + fan-out |
| `entity_entry_point_list` / `entity_http_route_list` / `entity_data_model_list` / `entity_test_list` | entities by categorisation tag |
| `entity_deprecation_list` / `entity_todo_list` | deprecated / TODO-tagged entities |
| `entity_test_caller_list` | test-tagged callers of an entity |
| `entity_high_churn_list` | entities ranked by git churn |
| `entity_recent_change_list` | entities changed since a timestamp |

`module_circular_import_list` and `entity_coupling_hotspot_list` are
edge-derived, so they take a `confidence` tier (default `resolved`, a ceiling)
and echo it. The
categorisation shortcuts read plugin-emitted tags. The Python plugin emits
conservative tags for common conventions (`entry-point`, `http-route`, `test`,
`data-model`, `cli-command`, `exported-api`), so root/tag shortcuts and
`entity_dead_list` light up on freshly analyzed Python projects where those
signals are present. `entity_deprecation_list` / `entity_todo_list` still return
honest-empty unless a plugin emits those tags. Likewise `entity_high_churn_list`
and `entity_recent_change_list` are honest-empty until churn/change signals are
populated (use `index_diff_get` for repo-level freshness).

`entity_semantic_search_list` is also in the catalogue — embedding-similarity
*ranking* for a natural-language query. It is opt-in under `semantic_search:`;
when enabled,
`loomweave analyze` populates the git-ignored `.weft/loomweave/embeddings.db`
sidecar and the query path filters stale vectors by content hash. When it is off
(the default) it returns `result_kind: "not_enabled"` rather than a fabricated or
empty-as-complete result — **that is not a dead end: `entity_find` already does
keyword/substring/docstring discovery with no embeddings required** (see "How
`entity_find` matches" above), so it is the right reach for "find the thing that
does Y" out of the box.

> Not in this catalogue: `emit_observation` as a general-purpose write surface.

### Tool notes (depth the tools/list descriptions deliberately omit)

Schema descriptions are kept short by budget; the operational detail lives here.

- **`entity_at` / `entity_orientation_pack_get` evidence:** `match_reason` is
  one of decorator_range / declaration / body_range / containing_range /
  no_match — a blank or comment line that only a module spans reports
  `containing_range`, never a fabricated exact match. The context block also
  carries the module→entity containing stack, decl/body/decorator sub-ranges,
  and same-granularity ambiguity alternatives.
- **`entity_finding_list` / `project_finding_list` filter values** (closed
  sets): `kind` = defect | fact | classification | metric | suggestion;
  `severity` = INFO | WARN | ERROR | CRITICAL | NONE; `status` = open |
  acknowledged | suppressed | promoted_to_issue. Matching is case-insensitive
  (input is canonicalised); a value outside its set is rejected as a param
  error naming the vocabulary — never a silent empty page.
- **`entity_kind_list` unknown kinds:** kinds are plugin-owned (an open set),
  so an unknown kind cannot be rejected up front — it returns an empty page
  plus `known_kinds`, the kinds the index actually holds, so a typo
  (`strcut`) is distinguishable from "kind exists, nothing in scope".
- **`entity_call_site_list` resolution:** each site is resolved | ambiguous
  (with candidate ids) | unresolved (a static call Loomweave could not bind —
  kept separate from resolved evidence). Filter with `kind`
  (`calls`/`references`) and `path` (`all`/`production`/`test` — a best-effort
  path heuristic, not an indexed partition). Sites carry file, 1-based line,
  byte column, and line text.
- **`entity_neighborhood_get` rollups:** on a module, each rolled-up
  references neighbor carries `via` (the contained symbol the edge touches);
  references_in neighbors also carry `importer_module`, so reverse-import
  answers name importing modules, not just symbols.
- **`entity_relation_list` anchors:** each entry carries the anchoring
  file/line/line-text behind the edge. For `decorates` the anchor lives in the
  DECORATED side's file (the `@decorator` line), and ambiguous `candidates`
  are alternative FROM-side decorators — inverted relative to every other
  kind.
- **`entity_dead_list` reasoning:** reachability counts ALL confidence tiers,
  dynamic-dispatch/reflection barrier tags force entities live,
  framework-magic kinds are excluded from candidacy, and there is no
  `confidence` argument (a ceiling would only make more code look dead).
  Results are heuristic findings (confidence < 1), never certainties.
- **`index_diff_get` mechanics:** compares the persisted analyzed commit vs
  git HEAD (falling back to dates), lists indexed files modified/missing and
  dirty working-tree files touching indexed paths, and is fail-soft — a
  missing git binary degrades to `git.available: false`, never an error.
- **`entity_summary_get` fallback:** non-JSON LLM output degrades to a
  deterministic structural summary (kind: structural-fallback) that is cached,
  so a retry is a free cache hit rather than a re-billed failure.
  `entity_summary_preview_cost_get` reports `live_spend_would_occur` — true
  only when no fresh cache row exists AND a live provider is wired; a disabled
  LLM is reported distinctly from a cache miss.
- **`entity_issue_list` endpoint evidence:** the `filigree_endpoint` block
  reports configured vs resolved URL + resolution source (e.g. a live
  ephemeral port), and matched entries embed the issue's title/status/priority
  fetched once per distinct issue.

**Guidance authoring has an operator boundary.** Operators can manage sheets via
`loomweave guidance create/edit/show/list/delete/promote` (plus `export`/`import`
for team sharing). Agents may call `propose_guidance` to create a Filigree
observation, but that proposal is inert until an operator promotes it through
`promote_guidance` or the CLI. Promoted sheets reach you through
`entity_guidance_list` and are composed into `entity_summary_get` prompts with
a real guidance fingerprint.
(`propose_guidance` and `promote_guidance` are write-gated — see the † note above.)

## Workflow: orient, then navigate

1. **Anchor.** `entity_find` by name (or `entity_at` for a file:line) to get the
   entity and its `id`. For a code location you're about to dig into, prefer
   `entity_orientation_pack_get` — it returns the entity, its context, one-hop
   neighbors, execution paths, attached issues, and index freshness in one
   deterministic call, instead of hand-composing those queries.
2. **Navigate.** Feed that `id` into `entity_callers_list`,
   `entity_neighborhood_get`, `entity_execution_path_list`, or
   `entity_summary_get`. Chain results' IDs to keep walking.

## Gotchas (read before hunting for a subsystem)

- **To find a package's subsystem, search the package NAME with `kind`.**
  Subsystems are *named after* their dominant package (e.g. `mypkg`), so
  `entity_find {"pattern":"subsystem"}` returns nothing. Search the package name
  and pass `{"kind":"subsystem"}` to return only subsystem entities, then call
  `subsystem_member_list`. (`entity_find` accepts an optional `kind` filter —
  `"subsystem"`, `"function"`, `"class"`, `"module"`, …; omit it for no filter.)
- **To go from an entity to its subsystem, use `entity_subsystem_get`.**
  `entity_neighborhood_get` does **not** return the entity's subsystem. Call
  `entity_subsystem_get {"id": "<entity-id>"}` — it accepts any entity (a function/class
  resolves through its containing module) and returns the subsystem plus the
  module it resolved through. `subsystem_member_list` is the forward direction.
- **`entity_find` is paginated** (~20/page, `next_cursor`); a broad concept word
  now matches docstring/identifier substrings too, so it can return many hits —
  narrow the pattern (or add a `kind` filter) rather than paging if you can.
- **`entity_callers_list` and `subsystem_member_list` are bounded** (`limit`
  default 50, max 100, plus a numeric-offset `cursor`). Each response carries
  `next_cursor`
  (null when exhausted) and an explicit `truncated` flag — re-call with
  `{"cursor": "<next_cursor>"}` to walk the full set. An empty page on a non-null
  cursor means you paged past the end.
- **`entity_neighborhood_get` caps each bucket independently** with one
  per-bucket `limit`
  and reports a `truncated` **map** (`{callers, callees, contained,
  references_in, references_out, imports_in, imports_out, relations_in,
  relations_out}`) — it has **no cursor**. When a bucket is `truncated:true`,
  switch to that relation's dedicated cursor-paginated tool (e.g.
  `entity_callers_list`, `entity_relation_list`) for the complete set;
  `entity_neighborhood_get` is a one-hop overview, not a paging surface.
- **Relation direction reads as a sentence** (`from KIND to`, ADR-051):
  `entity_relation_list` with `direction: "in"` on a class answers "what
  subclasses / implements / derives this"; `direction: "out"` on a *decorator*
  answers "what does this decorate" (the decorator is the FROM side — inverted
  from where the `@decorator` line sits). Each entry carries the anchoring
  file/line/line-text so you can see the declaration behind the edge.

## Launch

`loomweave serve --path <dir>` where `<dir>` contains `.weft/loomweave/loomweave.db`
(built by `loomweave analyze <dir>`). In an MCP client the tools appear as
`mcp__loomweave__entity_find`, etc. — exactly the names registered in
`tools/list` and used throughout this skill.

**Legacy aliases.** Pre-1.0 docs and transcripts may use retired names
(find_entity, callers_of, neighborhood, subsystem_of, summary, …). The server's
rename shim still accepts them on raw JSON-RPC `tools/call`, but they are NOT
in `tools/list`, so an MCP client cannot call them — always use the registered
names above.

Besides the tools, the server exposes a `loomweave://context` **resource** — live
entity/subsystem/finding counts and index freshness as JSON, a lightweight read
when you only want the numbers (`project_status_get` is the fuller tool-based view).
