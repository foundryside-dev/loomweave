# MCP / command-surface audit — 2026-06-11

**Scope.** Loomweave's command surfaces, audited from the position of the
surface's primary consumer (a consult-mode LLM agent): the MCP tool surface
(primary), the CLI verbs, and the federation HTTP read API. Method: read of the
registered tool surface (`crates/loomweave-mcp/src/lib.rs::list_tools`,
37 read tools + 5 write-gated), live stdio JSON-RPC probe sessions against
`target/debug/loomweave serve` at HEAD `be0e780` (index fresh at same commit),
direct queries against `.weft/loomweave/loomweave.db`, and cross-checks against
the shipped `loomweave-workflow` SKILL.md, CLAUDE.md, server `instructions`,
and open Filigree tickets (X-series, calls-envelope).

**Verdict in one line.** The surface's *design discipline* is genuinely strong
(honest-empty, result_kind, scope_excludes, bounded everything, SEI on every
row, write-gating) — but three frictions undercut the "absolutely friction-free"
bar: the flagship question ("what calls X?") returns silently-confident empties
for most Rust cross-module calls; the canonical onboarding skill teaches tool
names that don't exist on the wire; and the surface taxes every session ~11k
tokens of tools/list prose plus per-response boilerplate.

---

## F1 — HIGH — `entity_callers_list` gives confident wrong empties; the honesty machinery under-declares

The flagship flow (skill workflow step 2: "feed the id into callers_of") is
broken for the dominant Rust call shape, and the result *looks* trustworthy.

Evidence (live, at HEAD):

- `entity_callers_list(rust:function:loomweave_storage.query.entity_by_id)` →
  `callers: []`, `scope_excludes: ["attribute-receiver-calls"]`.
- Reality: `entity_by_id` is called from `query.rs` (same file), `guidance.rs`
  (same crate), and ~10 call sites across `loomweave-mcp` (cross-crate). The
  store *knows* this: `entity_unresolved_call_sites` holds **35 rows whose
  `callee_expr` is exactly `entity_by_id`**.
- Magnitude: the index holds **1,654 resolved `calls` edges vs 27,346
  unresolved call sites** (~94 % of recorded call sites are unresolved). Rust
  resolved-calls skew heavily intra-module (388 from `loomweave_mcp`, 16 from
  `loomweave_storage`); zero `calls` edges target anything in
  `loomweave_storage.query`.
- The blind spot is **not declared**: `scope_excludes` names only
  `attribute-receiver-calls`. Unresolved bare-name / qualified-path /
  cross-crate calls — the actual reason the list is empty — are not named.
- The documented recovery path is **unavailable in the default posture**: the
  skill says "re-query at `inferred` before concluding nothing calls this", but
  in explicit read-only mode (`enable_write_tools: false`)
  `confidence=inferred` is rejected with a policy error
  (`-32602: confidence=inferred/all is disabled by MCP tool policy…`). The
  escape hatch is gated off exactly where it's most needed.
- The *data layer is honest*: `entity_call_site_list(id, role=callee)` returns
  the 35 name-matched `unresolved_sites` with file/line/line_text. And
  `entity_dead_list` already consumes this signal
  (`unresolved_call_site_suppressed`). Only the navigation layer ignores it.

**Recommendation (surface-layer, independent of resolver depth-investment):**

1. `entity_callers_list` (and the `callers` bucket of neighborhood/orientation
   pack) should carry `unresolved_name_matches: N` (count of
   `entity_unresolved_call_sites` rows whose `callee_expr` matches the target's
   short name) plus a `next_action` pointer at
   `entity_call_site_list role=callee`. Cheap: the query and the precedent
   (`entity_dead_list`) both exist.
2. Extend the `scope_excludes` vocabulary to name the unresolved-call-site
   blind spot (e.g. `unresolved-static-calls`) whenever the unresolved table is
   non-empty for the project.
3. Fix the skill/description claim about the `inferred` re-query so it states
   the policy gate (or allow `ambiguous`-tier reads to include name-matched
   unresolved sites as candidates).
4. Long-term fix is the already-chosen resolution-depth bet (workspace-level
   symbol table; re-export map). This finding is about *honesty until then*.

Relationship to existing work: complements (does not duplicate)
`clarion-e9cfde2773` (Python envelope pinning) and the resolution-depth
investment; the MCP honesty surface above is currently untracked.

## F2 — HIGH — the canonical skill teaches a tool dialect that doesn't exist on the wire

`crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md` (embedded via
`include_str!`, served via `prompts/get`, installed by `loomweave install
--skills`; byte-identical to the installed copy) uses the **old names
exclusively** — `find_entity`, `callers_of`, `neighborhood`, `subsystem_of` —
with **zero occurrences** of the registered names, and its Launch section
states "the tools appear as `mcp__loomweave__find_entity`". They don't:
`tools/list` registers `entity_find`, `entity_callers_list`, … and that is what
an MCP client exposes. The `RENAME_MAP` shim only rescues raw JSON-RPC callers
(verified live: `tools/call name=find_entity` works on stdio), but an MCP-client
agent calling `mcp__loomweave__find_entity` fails client-side — the name isn't
in `tools/list`. Meanwhile CLAUDE.md and the server `instructions` use the new
names, so the canon speaks two dialects and every skill reader pays a mental
mapping tax across ~20 names (or worse, burns a failed call).

**Recommendation:** regenerate SKILL.md in the registered dialect (one file
fixes asset + prompt + installed copies); optionally keep a short legacy-alias
table. Add a CI check (`scripts/check-*` family) asserting every backticked
tool name in SKILL.md appears in `list_tools()` — this is exactly the drift
class the version-lockstep checks already prevent elsewhere.

## F3 — MEDIUM/HIGH — context tax: ~11k-token tools/list; instructions truncated by real clients

Measured: `tools/list` = 37 tools, **44,380 bytes (~11k tokens)**, loaded into
every consuming session. Top descriptions: `entity_neighborhood_get` 1,718
chars, `entity_relation_list` 1,698, `entity_orientation_pack_get` 1,286,
`entity_dead_list` 1,260. The `initialize.instructions` blob (~2.5 KB) is
demonstrably **truncated mid-sentence by Claude Code** in this very session
("…until then they are no… [truncated]") — the write-gating note never reaches
the agent it was written for.

The descriptions are excellent *documentation* but they are doing the skill's
job in the schema channel. **Recommendation:** set a description budget
(~350 chars: what it answers, key args, one honesty caveat) and move the long
rationale (ADR references, anchor-semantics essays, nine-bucket truncation-map
explanations) into the skill/prompt, which is fetched once and is already the
designated deep-dive surface. Target ≤ 5k tokens for tools/list.

## F4 — MEDIUM — typo'd filter values silently match nothing

`entity_finding_list` / `project_finding_list` declare `filter.kind/severity/
status` as free strings. Verified live: `filter.severity = "eror"` → `ok:true`,
0 findings, no diagnostic — indistinguishable from a genuinely clean entity.
This violates the honest-empty doctrine in spirit: the empty means "your filter
value is not a value", not "nothing matched". The arg-validation layer already
rejects unknown *keys* with precise messages, so the precedent exists.

**Recommendation:** enums in the schema (values are already closed sets,
documented in the descriptions), or a `diagnostics` note when a filter value
matches no known enum member. Same applies to `entity_kind_list` (unknown kind
"matches no rows" by documented design — at minimum emit the known-kinds list
in the empty result).

## F5 — MEDIUM — three pagination dialects and three limit ceilings

- Cursor-string + `next_cursor`: `entity_find`, `entity_callers_list`,
  `subsystem_member_list`, `entity_relation_list` (max limit 100).
- `limit`/`offset` + `page{total,returned,truncated}`: all catalogue tools
  (max limit 200; coupling default 20; semantic max 100).
- Per-bucket `limit`, **no cursor**, `truncated` map: `entity_neighborhood_get`.
- Outlier: `index_diff_get` max limit 2000.

Each idiom is internally justified (the neighborhood no-cursor rationale is
sound), but an agent must remember which tool speaks which dialect.
**Recommendation:** converge new/changed tools on one idiom (the wardline-style
cursor pattern the hub already blessed) and document the neighborhood exception
in one place. Note: ticket **clarion-b24df21158 (X-6) is partially stale** —
it claims `entity_find` is "unbounded with no truncation cursor", but at HEAD
it defaults to 20 with a working `next_cursor` (verified live; `'20'` then
`null`). The live remainder of X-6 is the idiom convergence + slim projection,
not boundedness.

## F6 — MEDIUM — `entity_resolve` rejects the native Rust dialect and non-function kinds

Verified live: `loomweave_storage.query.entity_by_id` → resolved;
`loomweave_storage::query::entity_by_id` → `unresolved` with no hint. Every
Rust artifact an agent holds (stack trace, compiler error, rustdoc path) is
`::`-separated, so the precise "paste an identifier from another tool" flow
fails for the language that is 62 % of this index. Separately `kind` is locked
to `function` — a struct/class/module qualname can never resolve.

**Note:** X-2 (`clarion-c2bb394f46`) is *currently claimed and building*
(assignee `claude-x2`, claimed 2026-06-11 05:01). Do not start parallel work;
instead these belong as acceptance criteria on X-2: (a) normalize `::` → `.`
(or echo a dialect hint in the unresolved result), (b) accept all entity kinds,
(c) SEI-in → identity-row-out (already in X-2's ask).

## F7 — LOW/MEDIUM — response payload noise

- Constant envelope boilerplate on every response:
  `diagnostics:[], error:null, ok:true, stats_delta:{}, truncated:false,
  truncation_reason:null` (~110 bytes), double-bookkeeping MCP's own `isError`.
- Every entity row in *list* results carries `content_hash` (64 hex chars) and
  an absolute `source_file_path` (the project-root prefix repeats on every
  row). Measured: `entity_find` page of 20 = **11.3 KB** (~550 B/row); the
  navigation-relevant fields are ~6.
- `project_status_get` embeds two static paragraph-length caveat notes
  (`staleness_note`, `worktree_dirty_note`) in every response.

**Recommendation:** slim list-row projection (id, sei, kind, short_name,
relative file:line) with full rows reserved for `entity_at` /
`entity_source_get`; omit null/empty envelope fields; move static caveats into
the description/skill. This is the "slim projection" half of X-6's ask.

## F8 — LOW — split error channels and a phantom `all` tier

Schema/policy violations surface as JSON-RPC protocol errors (`-32602`), while
domain errors (`entity-not-found`) surface as `isError:true` tool results with
an inner `{ok,error{code,message,retryable}}` envelope — two places to look.
Message quality is high in both (good). Inconsistency: `confidence: "all"` is
not in the schema enum, yet the policy check fires *first* with a message
implying `all` is a valid-but-gated tier ("confidence=inferred/all is disabled
by MCP tool policy…"). Either admit `all` in the enum or drop it from the
policy path/message.

## F9 — LOW — `entity_orientation_pack_get` exactly-one constraint not in schema

Runtime enforcement is clear ("provide exactly one of: `entity` (id), or
`file` + `line`" — verified for both the both-given and neither-given cases),
but the schema has no `oneOf`, no `required`, so a schema-validating client
can't catch it before the round-trip. Encode as `oneOf` in the input schema.

## CLI and HTTP read API — brief

**CLI** (`install/analyze/serve/hook/db/guidance/config/doctor/sarif`): sound
shape for the consult-mode posture; long-form `--help` texts are genuinely
good (self-documenting config via `config example`/`check`, `doctor` as a
repair gate). No graph-query verb from the shell is a deliberate gap (MCP is
the query surface) — fine, but consider one `loomweave query <tool> <json>`
escape hatch for debugging the server without an MCP client; today the only
way to ask the graph from a terminal is hand-rolled JSON-RPC over stdio (this
audit had to do exactly that).

**HTTP read API**: `/api/v1/files*`, `/api/v1/entities/:id/callers|callees`
(+ batch custom-methods), `/api/v1/identity/*` (SEI resolve/lineage),
`/api/v1/_capabilities`, all behind identity middleware with sane body limits.
One asymmetry: the wardline taint-store group lives at **unversioned**
`/api/wardline/*` while everything else is `/api/v1/*` — worth versioning
before 1.0 freeze, since federation siblings pin to these paths.

## What is genuinely good (keep, and defend in review)

- Honest-empty + `result_kind` + missing-signal notes — the empties you *can*
  trust are clearly distinguished from signal-absent.
- `scope_excludes` as a concept (F1 is about its vocabulary, not the idea).
- `entity_call_site_list` as an evidence surface ("why does this edge exist").
- `entity_orientation_pack_get` — the one-call anchor genuinely eliminates a
  5-call hand-composed dance.
- Write-gating + per-tool `ToolMetadata` (read_only / may_call_llm /
  spawns_process) — exemplary for client permissioning.
- `instructions` derived from the *active* policy (can't advertise unregistered
  tools).
- Unknown-argument rejection with precise messages
  ("unknown argument for entity_find: bogus_param").
- Batch-first `entity_resolve` (1..=2000) with in-input-order results.
- SEI on every returned row + SEI-accepted-everywhere on id-taking tools.

## Suggested ticket set (not yet filed)

1. **P1** `entity_callers_list` honesty: `unresolved_name_matches` count +
   `scope_excludes` vocabulary + skill/description fix for the gated
   `inferred` recovery path (F1).
2. **P2** Regenerate SKILL.md in the registered tool dialect + CI name-drift
   check (F2).
3. **P2** tools/list token budget: trim descriptions to ~350 chars, move depth
   to the skill/prompt (F3).
4. **P2** Enum-ify finding filters (or diagnostic on unknown filter value) (F4).
5. **P3** Pagination idiom convergence + slim list projection; re-scope stale
   X-6 accordingly (F5+F7).
6. **(comment on X-2, in progress)** `::` dialect normalization + all-kinds
   resolve as acceptance criteria (F6).
7. **P3** Version the wardline HTTP group under `/api/v1/` before 1.0 (HTTP).
8. **P3** `oneOf` schema for orientation pack; reconcile the phantom `all`
   confidence tier (F8+F9).
