# Clarion v0.1 skeleton audit — vocabulary, schema, and test-debt sweep

**Date**: 2026-05-03
**Trigger**: Investigating filigree issue `clarion-4cd11905e2`
(`entities.priority` TEXT-affinity bug) revealed the issue body assumed P0–P4
numeric urgency while the design says `priority` is a six-level string enum
(`project | subsystem | package | module | class | function`). That misframing
is itself a symptom: the same word is doing different work in Clarion vs in
Filigree, with no ADR mediating the clash.

User direction (paraphrased): *"Don't shuffle one bone of the skeleton in
isolation — audit what else is wrong before Tier B starts pouring concrete on
top of v0.1's vocabulary."*

This document is that audit. It enumerates findings, severity-rates them, and
proposes a bundled triage so we can decide once instead of finding the same
class of problem one issue at a time during Sprint 2.

## Scope

In scope:

1. **Cross-product vocabulary clashes** (Clarion vs Filigree, Clarion vs
   Wardline) — same word, different meaning, no managing ADR.
2. **Within-Clarion overloads** — same word doing distinct jobs in the
   product's own model.
3. **Schema affinity / type concerns** in `0001_initial_schema.sql` — the
   class of bug `clarion-4cd11905e2` belongs to.
4. **Test-debt patterns** uncovered while triaging the priority bug — tests
   that *document* a bug rather than *encode* the design.

Out of scope (deliberate; flag for separate audit if useful):

- ADR coverage gaps (decisions made implicitly without an ADR).
- Cross-product wire-shape audit (ADR-018 already names one example with
  Wardline `FingerprintEntry`; carryover ticket `clarion-889200006a`).
- Documentation drift between requirements / system-design / detailed-design
  / code (Sprint 1 reviews already touched this).
- Performance / security / correctness review of WP1+WP2+WP3 code itself.

## Method

For each candidate concept:

1. Locate every site in `docs/clarion/v0.1/{requirements,system-design,detailed-design}.md`,
   `docs/clarion/adr/`, `crates/clarion-storage/migrations/0001_initial_schema.sql`,
   and any code path that reads/writes the column.
2. Cross-reference Filigree's vocabulary via `filigree taxonomy` and
   `filigree types` (the project's `.filigree/` instance, currently active).
3. Cross-reference Wardline references in Clarion docs (we don't have the
   Wardline repo vendored, but ADR-015/ADR-017/ADR-018 + `loom.md` §5
   carry the cross-product names Clarion already commits to).
4. Severity-rate by:
   - Will Tier B / WP4 / WP6 trip on this? (HIGH if yes)
   - Is there a managing ADR? (downgrade if yes)
   - Is the disambiguation structural, or just by-context? (LOW if structural)

## Severity-rated findings

| ID | Concept | Class | Severity | Status |
|----|---------|-------|----------|--------|
| F-1 | `priority` overload | Cross-product (Clarion ↔ Filigree) | **HIGH** | Unmanaged |
| F-2 | `critical` overload | Cross-product (Clarion ↔ Filigree) | **MEDIUM** | Unmanaged |
| F-3 | `source` triple-overload | Within-Clarion + cross-product | **MEDIUM** | Unmanaged |
| F-4 | `tags` vs `labels` | Cross-product naming difference | **LOW** | Acceptable, document |
| F-5 | `kind` triple-overload (`entity`/`edge`/`finding`) | Within-Clarion | **LOW** | Structural disambiguation; document |
| F-6 | `status` overload (`runs`/`findings`/Filigree issues) | Within-Clarion + cross-product | **LOW** | Already partially managed (finding-status mapping per §7); document |
| F-7 | `priority` schema affinity | Schema design | **HIGH** | Resolved by F-1 rename + new column |
| F-8 | `schema_apply.rs` priority test asserts the bug | Test debt | **MEDIUM** | Resolved alongside F-1/F-7 |
| F-9 | `severity` (CLARION/FILIGREE vocabulary) | Cross-product | — | **MANAGED** by ADR-017 (model finding) |
| F-10 | `rule_id` namespace | Cross-product | — | **MANAGED** by ADR-017+ADR-022 (model finding) |
| F-11 | `finding` cross-product record shape | Cross-product | — | **MANAGED** by ADR-004 (model finding) |
| F-12 | L7 qualname vs Wardline `FingerprintEntry` | Cross-product | — | Tracked: `clarion-889200006a` (deferred to WP9) |

## F-1: `priority` is the same word for two different things

**Sites**:
- Clarion: `docs/clarion/v0.1/detailed-design.md:453` — guidance entity
  property; values `"project" | "subsystem" | "package" | "module" | "class" | "function"`.
- Clarion: `docs/clarion/v0.1/system-design.md:346` — same definition,
  semantic ordering `project → subsystem → package → module → class → function`
  (outer-overrides-inner composition order).
- Clarion schema: `crates/clarion-storage/migrations/0001_initial_schema.sql:163-165`
  generated column + index.
- Clarion test: `crates/clarion-storage/tests/schema_apply.rs:142-167`.
- Filigree: `P0 | P1 | P2 | P3 | P4` numeric urgency; CLI `--priority=N`,
  MCP tool field, taxonomy entry `priority`. Different domain, different shape,
  different meaning.

**Why HIGH**: The bug at `clarion-4cd11905e2` was misframed because the issue
author defaulted to Filigree's meaning of "priority" (P0–P4) when reading
Clarion's design. The same misread will happen to every cross-product reader
until the word is disambiguated. WP6 (briefing serving) will write the first
`ORDER BY priority` query against this column, and TEXT-affinity lexicographic
order doesn't match the semantic ordering — INTEGER affinity wouldn't help
either, because the input is a string enum, not a number. Both the **bug** and
the **vocabulary clash** are downstream of the same root cause: "priority"
isn't the right name for what Clarion stores.

**Suggested fix**:

1. Rename the guidance-entity property `priority` → `composition_level`
   (or `scope_level`) in `detailed-design.md` §6.4 and `system-design.md` §6.
2. Replace the schema's `priority TEXT` generated column with two columns:
   - `composition_level TEXT GENERATED ALWAYS AS (json_extract(properties, '$.composition_level')) VIRTUAL`
     — for equality filtering (`WHERE composition_level = 'subsystem'`).
   - `composition_rank INTEGER GENERATED ALWAYS AS (CASE … END) VIRTUAL` —
     six-way `CASE` mapping the enum to `1..6`. Index `composition_rank` for
     `ORDER BY`. Equality filter still uses `composition_level`.
3. Update the test in `schema_apply.rs:142` to round-trip a real enum value
   (`{"composition_level": "subsystem"}`) and assert both columns.
4. Write a short ADR (suggested **ADR-024**: "Guidance composition-level
   schema and Loom vocabulary") capturing the rename, the rank-mapping table,
   and the rule that "priority" in Clarion docs always refers to Filigree's
   P0–P4.

**Open policy question** (will recur): edit `0001_initial_schema.sql` in
place (no real consumers exist; only the walking-skeleton fixture writes a
DB) or stack `0002_*.sql`? Decide once for the project.

## F-2: `critical` is the same word for two different things

**Sites**:
- Clarion: `docs/clarion/v0.1/detailed-design.md:467` — guidance entity
  `critical: bool` flag — semantics: "preserved across token-budget pressure"
  (i.e., do-not-drop).
- Clarion: `detailed-design.md:449` — `tags: ["critical"]` literal as the
  serialised form (also overlapping the dedicated boolean field).
- Filigree: `severity:critical` enum value (highest severity tier) AND
  P0 priority is conventionally "Critical" in informal usage.

**Why MEDIUM**: Same word, related-but-distinct semantics ("must not be
dropped during budget pressure" vs "highest severity / urgency"). Less
ambiguous than F-1 because Filigree never stores a `critical: bool` field on
issues, but a cross-product reader sees `critical` and pattern-matches to
Filigree's severity tier.

**Suggested fix**:

1. Rename guidance `critical: bool` → `pinned: bool` (or `non_droppable`,
   `keep_under_pressure`) in `detailed-design.md:467` and `:449`.
2. Drop the `tags: ["critical"]` overlap — the boolean field is the
   authoritative source.
3. Bundle into the same ADR-024 if F-1 lands.

## F-3: `source` is overloaded three ways inside Clarion plus once outside

**Sites**:
- `entity.source` = `SourceRange { file_id, byte_start, byte_end, line_start, line_end }`
  (`detailed-design.md:204`).
- `finding.source` = `Source { tool: String, tool_version: String, run_id: String }`
  (`detailed-design.md:270`) — describes "what tool produced this finding", a
  totally different concept.
- `guidance.source` = `"manual" | "wardline_derived" | "filigree_promotion"`
  (`detailed-design.md:471`) — describes "how this guidance was authored", yet
  another concept.
- Filigree: `source:` label = `scanner | review | agent` — describes "how
  the issue was discovered."

**Why MEDIUM**: Within-struct disambiguation usually saves us — a Rust
`Finding.source` is type-distinct from `Entity.source` — but the JSON wire
shape, schema columns, and prose docs all flatten this into the same word.
A reader of `detailed-design.md` sees `source:` four lines apart meaning
three different things.

**Suggested fix**:

1. `entity.source` is OK as-is — it's clearly a code anchor and the type name
   (`SourceRange`) reinforces it.
2. Rename `finding.source` → `finding.provenance` (matches its role: tool +
   version + run_id is provenance metadata, not source code).
3. Rename `guidance.source` → `guidance.origin` (describes authorship origin).
4. Bundle into ADR-024 if F-1 lands; otherwise file as separate ADR.

## F-4: `tags` (Clarion) vs `labels` (Filigree)

**Sites**:
- Clarion: `entity_tags(entity_id, tag)` table; `entity.tags: BTreeSet<String>`
  (free-form).
- Filigree: namespaced `labels` (`area:`, `cluster:`, `effort:`, …) with a
  reserved-namespace taxonomy.

**Why LOW**: Different word, different semantics — Clarion's `tags` are
free-form and plugin/LLM-emitted; Filigree's `labels` are a curated
taxonomy. The naming actually reflects the design difference correctly.

**Suggested fix**: leave the names. Add a one-paragraph note in the v0.1
glossary (or in ADR-024) that maps the two so a cross-product reader doesn't
assume they're the same shape. WP9 (Loom integrations) will need this anyway.

## F-5: `kind` is overloaded three ways within Clarion

**Sites**:
- `entity.kind` (`detailed-design.md:195`) — entity taxonomy
  (`function | class | protocol | global | module | guidance | file | subsystem`),
  plugin-defined.
- `edge.kind` (`detailed-design.md:256`) — edge taxonomy
  (`contains | guides | emits_finding | in_subsystem | …`), core-reserved or
  plugin-defined.
- `finding.kind` (`detailed-design.md:275`) — finding taxonomy
  (`defect | fact | classification | metric | suggestion`).

**Why LOW**: Disambiguated by struct context; the type name carries the
namespace. The three uses are *consistent* in the abstract sense ("which
sub-taxonomy of this object am I?") even if they don't share a value space.
This is the kind of overload where renaming creates more confusion than it
removes, because `kind` is the right word for all three jobs.

**Suggested fix**: leave alone. Add a glossary entry making the three
namespaces explicit. (Filigree uses `type` for the analogous concept, which
is fine — different products, different taste; no clash.)

## F-6: `status` is overloaded across Clarion and Filigree

**Sites**:
- Clarion `runs.status` — analyze-run lifecycle.
- Clarion `findings.status` — `open | acknowledged | suppressed | promoted_to_issue`
  (`detailed-design.md:295`).
- Filigree finding-status — `open | acknowledged | fixed | false_positive | unseen_in_latest`
  (`detailed-design.md:294` comment + ADR-017).
- Filigree issue status — per-type state machine (`bug` has
  `triage → confirmed → fixing → verifying → closed | wont_fix | not_a_bug`,
  etc.).

**Why LOW**: Already partially managed — `detailed-design.md §7` is supposed
to carry the Clarion-finding-status to Filigree-finding-status mapping, and
ADR-017 references it. Distinct state machines on distinct objects are normal
and unambiguous given the table/struct context.

**Suggested fix**: verify §7 actually contains the finding-status mapping
(carryover check), and leave the broader `status` naming alone. If §7 is
silent on the mapping, that's a separate finding worth raising as a doc-debt
issue.

## F-7: `priority` schema affinity (the original bug — collapses into F-1)

Already covered by F-1's suggested fix. Listed separately so the original
filigree issue (`clarion-4cd11905e2`) has a clear traceability pointer back to
the audit's resolution: closing F-1 closes F-7.

## F-8: `schema_apply.rs:142` test asserts the bug, not the design

**Sites**:
- `crates/clarion-storage/tests/schema_apply.rs:142` —
  `let props = r#"{"priority": 2, "git_churn_count": 42}"#;` — inserts an
  integer `2` into `properties.priority`, which is **not a valid value** per
  `detailed-design.md:453` (priority is a six-level string enum).
- `:158` — comment explicitly states "priority is a TEXT-affinity generated
  column; json_extract yields the JSON-native integer but SQLite coerces it
  to text on storage" — i.e., the test was written to *document the bug*,
  not to *test correct behavior*.
- `:167` — `assert_eq!(priority.as_deref(), Some("2"))` — pins the wrong
  shape.

**Why MEDIUM**: A test that asserts buggy behavior is worse than no test —
it silently blocks the bug fix from ever landing because the test will turn
red when the fix is applied. The fix author then has to choose between
breaking the test or working around it. We've already paid this cost once
(every reader of the test learns the wrong thing about priority).

**Pattern to look for elsewhere**: tests with comments like "documents the
actual behaviour" rather than "asserts the design contract." Worth a quick
sweep — not part of this audit's scope, but a candidate for a follow-up.

**Suggested fix**: rewrite the test alongside F-1's schema change to
round-trip `{"composition_level": "subsystem"}` and assert both
`composition_level = "subsystem"` and `composition_rank = 2`.

## Findings managed by existing ADRs (model for the rest)

These are listed for completeness — they are **not action items**. They show
the pattern that worked when the same class of clash was caught at design
time, and which the unmanaged findings above should follow.

### F-9: `severity` — managed by ADR-017

- Clarion internal: `INFO | WARN | ERROR | CRITICAL` for defects, `NONE` for facts.
- Filigree wire: `critical | high | medium | low | info` (lowercase).
- Mapping: explicit one-to-one table (`detailed-design.md §7`,
  `ADR-017-severity-and-dedup.md`); round-trip preserved via
  `metadata.clarion.internal_severity`.
- **Lesson**: explicit ADR + mapping table + metadata round-trip = no clash.

### F-10: `rule_id` namespacing — managed by ADR-017 + ADR-022

- Namespace prefix per emitter: `CLA-PY-*`, `CLA-INFRA-*`, `CLA-FACT-*`,
  `CLA-SEC-*`, `WLN-*`, `FIL-*`.
- Grammar-checked at the Clarion-plugin boundary per ADR-022.
- **Lesson**: namespacing convention + grammar enforcement = no clash even
  when many emitters share the field.

### F-11: `finding` — managed by ADR-004

- Cross-product unified record type with explicit wire schema.
- Field ownership documented; extension via `metadata` slot (not `properties`,
  which is silently dropped on the Filigree side).
- **Lesson**: shared type with explicit field ownership + extension slot =
  no clash even when multiple products produce the same record.

### F-12: L7 qualname vs Wardline `FingerprintEntry` — tracked, deferred

- `clarion-889200006a` (P3, sprint:2 / wp:9). Trigger: WP9 attempts the
  first cross-product join.
- **Lesson**: when a clash is real but its trigger is far in the future,
  defer with a documented trigger condition, not silent omission.

## Recommended triage

This audit raises six unmanaged items (F-1 through F-6 plus F-8). Three
viable bundling strategies, in order of my preference:

### Option α — One ADR + one PR for F-1, F-2, F-3, F-8 (the renames)

Bundle the three vocabulary renames (`priority`, `critical`, `source` on
finding/guidance) plus the schema change and test fix into a single ADR-024
("Loom vocabulary discipline and guidance schema") and a single PR before
Tier B starts. Touches:

- `docs/clarion/v0.1/detailed-design.md` (multiple sites)
- `docs/clarion/v0.1/system-design.md` (one site)
- `docs/clarion/adr/ADR-024-*.md` (new)
- `crates/clarion-storage/migrations/0001_initial_schema.sql` (priority
  column + index; possibly add `0002_*.sql` instead — see migration policy
  question below)
- `crates/clarion-storage/tests/schema_apply.rs` (rewrite the priority test)
- Pre-existing filigree issues: close `clarion-4cd11905e2` with an
  audit-resolution pointer.

Estimated effort: **1–1.5 days** (the rename mechanics are small; the ADR
write-up and the schema-migration policy decision are the heavy lifting).

### Option β — Comment-and-defer everything except F-1+F-7

Rename only `priority` (the urgent one); leave `critical`, `source`,
glossary additions for a Tier-B-mid hygiene pass. Lower disruption now,
higher rework risk if Tier B starts emitting `source: "manual"` JSON before
F-3 lands.

### Option γ — Comment-and-defer everything

Add the vocabulary findings as filigree observations / issues, decide
later. Fastest path back to Tier B; highest cumulative cost across the
sprint.

**My recommendation**: **Option α**. Reasons:

1. The audit found these because we were *about to* shuffle one bone in
   isolation. Bundling avoids the same realization happening one more time
   when Tier B trips on `critical` or `source`.
2. The migration-policy question (edit `0001` in place vs stack `0002`)
   wants to be answered once. F-1 forces it; α answers it once for the
   whole bundle.
3. Tier B's first edge emission (B.3, `contains`) lands new wire shapes —
   the right moment for vocabulary discipline is *before* those shapes get
   names downstream tools learn.
4. ADR-024 is also the right place to document the "model" findings
   (F-9/F-10/F-11) as the convention for future cross-product fields, so
   the next clash gets a managed ADR, not a misframed bug.

## Out-of-audit (worth raising separately)

While doing this audit I noticed but did not investigate:

- **WP3's L8 Wardline probe pin** (`min_version = "1.0.0"`,
  `max_version = "2.0.0"`) is a fail-soft cross-product pin. Whether the
  pin itself wants ADR coverage (it doesn't have one today) is an open
  question.
- **`detailed-design.md §11` ADR-backlog table** lists six unauthored ADRs
  that the system-design references; some of these may be the right home
  for findings here rather than a fresh ADR-024. Worth a quick check
  before authoring.
- **`detailed-design.md §3` view `guidance_sheets`** projects
  `json_extract(e.properties, '$.priority')` directly — the view will need
  to change in lockstep with F-1.
- **`runs.status` / `runs.config` / `runs.stats`** are all `TEXT NOT NULL`
  with no CHECK constraints. Validation lives in the writer-actor, by
  design. Worth confirming no existing test asserts the wrong thing here
  (the priority test slip should make us suspicious of similar test debt).

## Decision needed from user

1. Pick a triage option (α / β / γ).
2. Settle the migration policy: edit `0001_initial_schema.sql` in place
   (treat the v0.1 tag as movable until real consumers exist) or stack
   `0002_*.sql` (treat shipped migrations as immutable on principle).
3. Confirm the ADR slot: ADR-024 is the next available number; alternatively,
   one of the §11 backlog ADRs may fit (worth a quick check).

Once those three are answered, the ADR draft + PR scope is determined.

## Reviewer additions (architecture-critic + leverage-analyst, 2026-05-03)

Both reviewers confirmed F-1 through F-8. The architecture critic added
five schema-design findings the original audit missed; they are listed
here so the durable record carries the full set. Leverage analyst's
prescription (Level 5 + Level 6 — ADR-acceptance rule + glossary)
shaped Phase 1; both reviewers' verdicts (Option α with reframed
naming and split doctrine vs. schema fix) shaped Phase 2.

| ID | Finding | Severity | Filigree |
|----|---------|----------|----------|
| F-13 | Enum-typed `TEXT` columns lack `CHECK` constraints (`entities.kind`, `edges.kind`, `findings.{kind,severity,status}`, `runs.status`); validation lives in the writer-actor by design but the policy is undocumented | MEDIUM-HIGH | `clarion-fbe50aa6e1` (P2, separate ADR likely warranted) |
| F-14 | `entity_tags(entity_id, tag)` PK has no `plugin_id` column; two plugins emitting the same tag string collide | LOW (v0.1 Python-only) / breaks at WP9 | `clarion-ef9bd365bf` (P3) |
| F-15 | `edges UNIQUE(kind, from_id, to_id)` ignores `properties`; two same-kind edges between the same pair with different properties cannot coexist; intentional but undocumented (and inconsistent with how findings dedup) | LOW | `clarion-fb1b8fb5a0` (P3, doc note) |
| F-16 | `source_file_id TEXT REFERENCES entities(id)` has implicit "rows referenced by `source_file_id` must have `kind='file'`" invariant; nothing enforces it | MEDIUM | `clarion-523b2eebad` (P2) |
| F-17 | `runs.id` ↔ `findings.run_id` is string-match only, no FK; provenance link untracked at the schema layer | LOW | `clarion-ba198ee96b` (P4, doc note) |

F-1 through F-8 are absorbed by ADR-024 (`docs/clarion/adr/ADR-024-guidance-schema-vocabulary.md`) and the Phase 1 + Phase 2 commits on this branch. The original priority-affinity bug `clarion-4cd11905e2` is closed with the audit-resolution comment.

F-13 is absorbed by [ADR-031](../../clarion/adr/ADR-031-schema-validation-policy.md) (2026-05-18): CHECK constraints on closed core-owned vocabularies (`findings.{kind,severity,status}`, `runs.status`); writer-actor + ADR-022 manifest acceptance remain the only enforcement layer for plugin-extensible vocabularies (`entities.kind`, `edges.kind`). Filigree issue `clarion-fbe50aa6e1` closed.

### Naming refinements applied to Phase 2

The original audit proposed `composition_level`/`composition_rank`,
`pinned`, and `provenance`/`origin` (split). The architecture critic
pushed back on three of these:

- `composition_level` → **`scope_level`** (matches existing
  `properties.scope.{query_types, token_budget}` already on the same
  struct; semantically more accurate — this is *applicability scope*,
  not "composition" in any compositional sense).
- `pinned` accepted with explicit gloss (rejected `non_droppable` as a
  double negative; `keep_under_pressure` was acceptable but uglier).
- `provenance`/`origin` split → **`provenance` for both** (same role,
  different shape; field name describes role not shape; SARIF and SBOM
  precedent).

ADR-024 lands with the refined names.

### Migration policy decision applied to Phase 2

Both reviewers (critic explicitly, leverage analyst implicitly) called
the migration-policy punt the audit's biggest weakness. ADR-024 makes
the call: **edit `0001` in place**, with a written retirement trigger
(first external operator pulls a published Clarion build → policy
switches to stack-only thereafter). No supersession of ADR-024 needed
for the trigger; the trigger is observable and named in the ADR.
