# Guidance Operator Notes

A **guidance sheet** is institutional knowledge attached to code: a short note
("refresh tokens are single-use", "this module owns the retry budget") that
Loomweave serves to consult-mode agents alongside the entities it applies to. A
sheet is a first-class entity of `kind: guidance` (`id` form
`core:guidance:<slug>`); it carries the note text plus the rules that decide
which entities it covers, a scope level, and optional pinning / expiry
(`REQ-GUIDANCE-01`, ADR-024).

Guidance is authored by operators via the `loomweave guidance` CLI (this guide)
or proposed by agents through MCP and promoted by an operator. Authored and
promoted sheets reach consult agents through the `guidance_for` MCP read tool
and are also composed into auto-generated `summary` prompts with a real
`guidance_fingerprint` cache key.

All subcommands operate on `.loomweave/loomweave.db`, so **run `loomweave analyze`
first** — the CLI errors if the database is absent.

## Authoring workflow (`REQ-GUIDANCE-03`)

### `create`

```bash
loomweave guidance create \
  --match path:src/auth/** \
  --match tag:auth \
  --scope-level module \
  --name auth-tokens \
  --content "Refresh tokens are single-use; rotate on every refresh."
```

Omit `--content` to author the note in `$EDITOR`/`$VISUAL` (or pipe it on
stdin). Useful flags:

- `--pinned` — mark the sheet preserved under token-budget pressure during
  composition.
- `--expires <WHEN>` — see [Expiry](#expiry-semantics) below.
- `--name <slug>` — the `core:guidance:<slug>` id segment. Defaults to a slug
  derived from the first `--match` rule.

`create` refuses to overwrite an existing id; use `edit` to change a sheet.

### `edit <id>`

Opens the sheet's **content** in `$EDITOR`/`$VISUAL`. Only `content` changes;
every other property — including `authored_at` (the staleness baseline),
`provenance`, `pinned`, `expires`, `scope_level`, and `match_rules` — is
preserved.

### `show <id>` / `list`

```bash
loomweave guidance show core:guidance:auth-tokens
loomweave guidance list
loomweave guidance list --for-entity python:function:auth.tokens.refresh
```

`list` is ordered by `scope_rank` (project → function). `--for-entity` filters
to sheets whose `match_rules` apply to that entity id. See
[Staleness](#staleness) for `--stale` / `--expired`.

### `delete <id>`

Removes the sheet. Its matched entities' cached summaries are invalidated (see
[Cache behaviour](#cache-behaviour)).

### `promote <observation-id>`

```bash
loomweave guidance promote loomweave-obs-abc123
```

Promotes a reviewed Filigree observation produced by MCP `propose_guidance`
into a local guidance sheet (`provenance: filigree_promotion`). Arbitrary
observations are rejected: the observation detail must contain Loomweave's
guidance-proposal payload. This is the anti-poisoning boundary (`NFR-SEC-02`):
an agent proposal is inert until an operator promotes it.

MCP also exposes the same lifecycle:

- `propose_guidance(entity_id, content, scope_level?, match_rules?, name?,
  pinned?, expires?)` creates a Filigree observation, not a sheet.
- `promote_guidance(observation_id)` consumes a reviewed observation and writes
  the local sheet.

## `--match` rules

Each `--match` value is `<type>:<value>`, split on the **first** colon only
(subsystem and entity values contain colons of their own). Repeat `--match` to
add several rules; a sheet matches an entity if any rule matches.

| Rule | Matches |
|---|---|
| `path:<glob>` | entities whose source path matches the glob (e.g. `path:src/auth/**`) |
| `tag:<tag>` | entities carrying the categorisation tag |
| `kind:<entity-kind>` | entities of a kind (`function`, `class`, `module`, …) |
| `subsystem:<id>` | members of a subsystem (e.g. `subsystem:core:subsystem:abcd`) |
| `entity:<entity-id>` | one specific entity (e.g. `entity:python:function:auth.tokens.refresh`) |

## `--scope-level`

One of `project | subsystem | package | module | class | function` (ADR-024).
Scope level drives the **composition order**: when several sheets apply to one
entity, `guidance_for` ranks them by `scope_rank` ascending — project-scoped
sheets first, function-scoped last — so narrower, more specific guidance is
ordered after (and can override) broader guidance. Within a scope level, ties
break by `authored_at` then id.

## Expiry semantics

`--expires` accepts:

- a full ISO-8601 instant (`2026-12-31T23:59:59Z`);
- an offset form (`2026-06-03T12:00:00+02:00`), converted to UTC; or
- a bare date (`2026-12-31`), taken as **start-of-day UTC**
  (`2026-12-31T00:00:00.000Z`).

The value is normalized to a full UTC instant before storage so the read path's
lexical expiry compare is correct. Unparseable input is rejected at create time.
The read path excludes expired sheets from composition; analyze also surfaces
them as a finding (below).

## Wardline-derived guidance (`REQ-GUIDANCE-04`)

When `wardline.yaml` is present, `loomweave analyze` generates deterministic,
pinned guidance sheets from the Wardline bundle:

- `core:guidance:wardline-tier-<name>`
- `core:guidance:wardline-boundary-<name>`
- `core:guidance:wardline-annotation-group-<name>`

The parser accepts real Wardline output (`tiers: [...]`, `module_tiers: [...]`,
`wardline.fingerprint.json`, `wardline.exceptions.json`, and
`**/wardline.overlay.yaml`) plus the earlier guidance-map shape with `paths`,
optional `content`, optional `scope_level`, and optional explicit
`match_rules`. The bundle hash folds in the root manifest, fingerprint baseline,
exceptions register, and overlay boundary files, so drift in any governance
artifact can make preserved overrides reviewable. Generated sheets carry
`provenance: wardline_derived`, `pinned: true`, `wardline_manifest_hash`,
artifact hash/count metadata, and a generated-signature guard.

If an operator edits a generated sheet, the next analyze preserves the edit and
marks the sheet `provenance: wardline_derived_overridden`. If the Wardline
bundle changes while an override remains in place, analyze emits
`LMWV-FACT-GUIDANCE-STALE` so the override can be reviewed instead of silently
overwritten.

## Staleness

Two independent staleness signals exist, and they are **not** the same thing:

1. **Age / review cadence** — `loomweave guidance list --stale [--days N]` shows
   sheets not touched (the later of `reviewed_at` / `authored_at`) within `N`
   days (default 90). This is a review-cadence prompt, computed at list time.
2. **Churn-based finding** — `loomweave analyze` emits
   `LMWV-FACT-GUIDANCE-CHURN-STALE` when the code under a sheet has churned (see
   the findings table). This is a separate heuristic, not the `--days` age
   signal.

`--stale` and `--expired` are independent filters that compose by intersection
(AND): `loomweave guidance list --stale --expired` shows sheets that are both.

### Staleness findings (`REQ-GUIDANCE-05`)

`loomweave analyze` persists these findings over the committed graph (anchored to
the guidance sheet). See `detailed-design.md` §5 for the canonical catalogue.

| Rule | Severity | When |
|---|---|---|
| `LMWV-FACT-GUIDANCE-ORPHAN` | WARN | The sheet's `guides` edge **or** a `match_rules` `entity:<id>` rule points at an entity deleted between runs. The sheet's guidance is stranded. |
| `LMWV-FACT-GUIDANCE-EXPIRED` | INFO | The sheet's `expires` instant is in the past. The read path already excludes it from composition; this surfaces the state operatively (the sheet is not deleted). |
| `LMWV-FACT-GUIDANCE-STALE` | WARN | A Wardline-derived override carries an older `wardline_manifest_hash` than the current Wardline bundle. |
| `LMWV-FACT-GUIDANCE-CHURN-STALE` | WARN (confidence 0.7) | The aggregate `git_churn_count` over the sheet's matched entities meets the staleness threshold (50; 20 for `pinned: true` sheets). |

> **`LMWV-FACT-GUIDANCE-CHURN-STALE` is currently inert.** It is emitted only
> when churn data is available, and the analyze pipeline does not yet populate
> `git_churn_count`. In production today it never fires. The other guidance
> findings are live.

## Team sharing: export / import (`REQ-GUIDANCE-06`)

Guidance is committable team knowledge. Export writes one deterministic,
sorted-key JSON file per sheet (byte-stable across runs on identical DB state,
diff-friendly):

```bash
loomweave guidance export --to ./shared/guidance     # --to takes a flag
loomweave guidance import ./shared/guidance           # dir is positional
```

Import is **additive and idempotent**: each sheet is upserted by id, ids are
preserved exactly, and local sheets not present in the directory are left
untouched. Re-importing the same directory changes nothing. A malformed `*.json`
aborts the whole import naming the offending file (a silently-dropped sheet
would be data loss).

> **Export does not prune.** A sheet deleted locally still has its file in the
> export directory, so a teammate's additive `import` will resurrect it. To
> mirror local state exactly (rather than merge into it), **clear the export
> directory before exporting**.

## Cache behaviour

Authoring — `create`, `edit`, `delete`, `promote`, `import`, and Wardline
regeneration — invalidates the cached summaries of the entities the affected
sheet's `match_rules` cover (ADR-007 churn-eager invalidation). Without this,
new or changed guidance would stay inert until each matched entity's code next
changed. Over-invalidation is safe; the CLI prints how many summaries it
dropped.

## Not yet available

These pieces of the guidance system are **deferred** and do not ship today.
Authored guidance reaches consult agents through both `guidance_for` and
auto-generated summaries, but the following are not yet wired:

- **In-browser staleness-review UI (`NG-13`)** — deferred. Ticket:
  clarion-0d7e22c6cb.
