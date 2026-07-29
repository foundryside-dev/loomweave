# Loomweave navigation tools

Relocated verbatim from `SKILL.md` (convention C-20 budget).

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
`true` for the local agent loop). When the gate is off they do not appear in `tools/list` and a call
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
`serve.mcp.enable_write_tools: true`. If an operator has explicitly disabled
write tools, do not plan on `"inferred"` as your recovery path unless
`project_status_get` shows write tools enabled.

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
