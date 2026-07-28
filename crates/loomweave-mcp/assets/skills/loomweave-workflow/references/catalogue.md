# Loomweave catalogue tools

Relocated verbatim from `SKILL.md` (convention C-20 budget).

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
| `entity_entry_point_list` / `entity_http_route_list` / `entity_exported_api_list` / `entity_cli_command_list` / `entity_data_model_list` / `entity_test_list` | entities by categorisation tag |
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

**Tag classification is declaration evidence, not a guess from observed rows.**
`entity_tag_list` and every tag shortcut return
`classification.schema: "loomweave.classification.v1"` with `state`,
`complete`, `matches`, supporting/unsupported plugins, latest `run_id` /
`run_status`, and reasons. Read it before interpreting either an empty or a
non-empty page:

- `supported`: every active source plugin declares the tag;
- `partial`: some active plugins declare it and some do not;
- `unsupported`: no active plugin declares it;
- `unavailable`: latest-run coverage cannot support a decision, or no source
  plugin matched files.

The state and completeness axes are separate. `supported` + `complete` +
`matches: 0` is a proven supported-zero result. Any other zero is not proof of
absence. A nonzero incomplete result is real positive evidence for the returned
entities, but the full denominator may be larger.

`complete: true` requires all of the following: a completed latest run with
valid `loomweave.classifier-coverage.v1` metadata; complete plugin discovery and
source walking; every active plugin at status `complete` with no degraded
files; `page.offset == 0`; `page.returned == page.total`;
`page.truncated == false`; `scope_truncated == false`; and
`scan_truncated == false`. Raising `limit` does not repair a nonzero `offset`, a
truncated scope/tag scan, degraded plugin evidence, or incomplete discovery.
`known_tags` is only a diagnostic list of tags observed in stored rows; it never
proves support. The companion `signal.available` is true only for `supported`,
and `signal.complete` mirrors `classification.complete`.

When classification is incomplete, run
`loomweave doctor --format json --path <project>` and inspect the stable checks
`classifier.enumeration` and `classifier.tags`. Re-analyze after fixing plugin
discovery, source-walk, or degraded-file failures, then request the first full
page again.

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
