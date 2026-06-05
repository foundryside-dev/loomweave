# ADR-024: Guidance Schema Vocabulary Rename and In-Place Migration Policy

**Status**: Accepted
**Date**: 2026-05-03
**Deciders**: qacona@gmail.com
**Context**: Investigating filigree issue `clarion-4cd11905e2` — `entities.priority` TEXT-affinity bug — surfaced that the issue body assumed numeric urgency (Filigree's `P0..P4` convention) while Loomweave's design defines `priority` as a six-level string enum for guidance composition. The same word means two unrelated things in two Weft siblings, with no managing ADR. A skeleton-audit pass surfaced two more unmanaged clashes (`critical`, `source`). This ADR resolves all three before Sprint 2 Tier B writes the first `ORDER BY` against the column and before new edge / catalog wire shapes learn the wrong names.

## Summary

Three guidance-schema fields and one finding field are renamed: `entity.properties.priority` (string enum) becomes `entity.properties.scope_level`, with a new companion integer column `scope_rank` (CASE-mapped 1..6) for ordered queries; `entity.properties.critical` (bool) becomes `entity.properties.pinned`; `finding.source` (`{tool, tool_version, run_id}`) and `entity.properties.source` on guidance entities (`"manual" | "wardline_derived" | "filigree_promotion"`) both become `provenance`. `entity.source` (`SourceRange` on code entities) is unchanged — the type name disambiguates and the field is correctly named for the role. Migration `0001_initial_schema.sql` is edited in place; the in-place edit policy retires the moment any external operator pulls a published Loomweave build, after which all schema changes stack as `0002_*.sql`, `0003_*.sql`, and so on.

## Context

The Weft federation axiom (`docs/suite/weft.md` §3–§5) requires solo-useful, pairwise-composable, enrich-only products. None of the three failure modes (semantic / initialization / pipeline coupling) directly applies to vocabulary, but cross-product readability is the *prerequisite* for honest pairwise composition. When a Weft user reads Loomweave's design and Filigree's CLI in the same session and `priority` means different things in each, every cross-product debugging pass starts from a misframe. The bug at `clarion-4cd11905e2` is the concrete proof: the issue's filer assumed Filigree's meaning, the audit pass had to reframe before any fix could land.

Three mismatches are documented in the [skeleton audit](../../implementation/handoffs/2026-05-03-skeleton-audit.md):

1. **`priority`** — Loomweave's guidance composition rank `project | subsystem | package | module | class | function`
   (`detailed-design.md:453`, `system-design.md:346`) collides with Filigree's `priority` label vocabulary
   `P0 | P1 | P2 | P3 | P4`. The schema's `priority TEXT GENERATED ALWAYS AS (json_extract(...))`
   column at `0001_initial_schema.sql:163-165` cannot serve any future `ORDER BY priority` query
   correctly: the semantic ordering (`project` outermost, `function` innermost) is non-lexicographic
   (alphabetical sort gives `class < function < module < package < project < subsystem`), and the
   TEXT-vs-INTEGER affinity discussion in the original bug body is moot — neither affinity helps
   when the input is a string enum.

2. **`critical`** — Loomweave's guidance entity has `critical: bool` (`detailed-design.md:467`)
   meaning "preserved across token-budget pressure" (do-not-drop). The same word also surfaces
   as a `tags: ["critical"]` literal on the same entity (`:449`). Both clash with Filigree's
   `severity:critical` enum tier and the conventional reading of `P0` as "Critical." Unlike
   `priority`, the meanings are related (high-importance variants), so the reader does not get
   a categorically wrong answer — but the cross-product reader still has to disambiguate
   manually every time.

3. **`source`** — Used three different ways inside Loomweave alone:
   - `entity.source = SourceRange { file_id, byte_start, byte_end, line_start, line_end }`
     (`detailed-design.md:204`) — code-anchor location.
   - `finding.source = { tool: String, tool_version: String, run_id: String }`
     (`detailed-design.md:270`) — finding provenance metadata.
   - `entity.properties.source = "manual" | "wardline_derived" | "filigree_promotion"` on
     guidance entities (`detailed-design.md:471`) — guidance authorship origin.

   And a fourth meaning on Filigree's side: `source:` taxonomy label = `scanner | review | agent`
   (how an issue was discovered; per `filigree taxonomy`). The within-Loomweave overload is the
   bigger problem; the type name `SourceRange` saves the first usage at the Rust level, but in
   prose docs and JSON keys the same word does three jobs four lines apart.

ADR-017 (severity vocabulary), ADR-022 (rule-ID namespacing), and ADR-004 (finding wire shape) are the v0.1 model managed clashes — same-word-different-meaning that got an explicit mapping or namespacing convention at design time and shipped clean. The unmanaged clashes above are the cases where the same recognition didn't happen. ADR-024 brings the unmanaged ones into the managed pattern. The companion doctrine — `docs/suite/glossary.md` plus the ADR-acceptance rule in `docs/loomweave/adr/README.md` — addresses the *recurrence* mechanism so the next clash is blocked at design review rather than discovered during implementation.

## Decision

### Field renames

| Site | Before | After | Rationale |
|---|---|---|---|
| `entity.properties.priority` (guidance) | `"project" \| "subsystem" \| "package" \| "module" \| "class" \| "function"` | `entity.properties.scope_level` — same enum values, new key | "Priority" is wrong: this is *scope of applicability*, not urgency. `scope_level` matches the existing `properties.scope.{query_types,token_budget}` already on the same struct. |
| Schema `entities.priority` generated column | `priority TEXT GENERATED ALWAYS AS (json_extract(properties, '$.priority')) VIRTUAL` + `ix_entities_priority` index | Replaced by **two** generated columns: `scope_level TEXT` (for equality filters) and `scope_rank INTEGER` (CASE-mapped 1..6, for `ORDER BY`). Index moves to `ix_entities_scope_rank ON entities(scope_rank) WHERE scope_rank IS NOT NULL`. | TEXT-affinity index can never serve `ORDER BY` on a non-lexicographic enum. The two-column form gives equality filtering on `scope_level` and ordered queries on `scope_rank` without a rank-lookup CASE in every query. |
| `entity.properties.critical` (guidance, bool) | `critical: bool` | `pinned: bool` | "Pinned" describes the behaviour: the sheet remains in the briefing under token-budget pressure when other sheets are dropped. Avoids the Filigree severity overload. The schema-doc comment makes the gloss explicit — pinning is a budget-protection behaviour, not a UI-sort behaviour. |
| `tags: ["critical"]` literal (guidance) | tag value | dropped | The boolean property `pinned` is the authoritative source. The `critical` tag value duplicated the boolean and was never exercised by any query. Removing it is mechanical. |
| `finding.source` struct | `Source { tool: String, tool_version: String, run_id: String }` | `Provenance { tool: String, tool_version: String, run_id: String }` (struct rename) | "Provenance" is the technical term for tool + version + run identity (SARIF `result.provenance`, SBOM provenance). |
| `entity.properties.source` (guidance) | `"manual" \| "wardline_derived" \| "filigree_promotion"` | `entity.properties.provenance` — same enum values, new key | Same word as above for the same role (origin / how-it-came-to-be). Using one word for both keeps the mental model uniform. The shape difference (struct vs. enum) is fine — field names describe role, not shape. |
| `entity.source` (code entities, `SourceRange`) | unchanged | unchanged | `SourceRange` (the type) disambiguates at the Rust layer. The field is correctly named: this is a *code source* anchor, not metadata about origin. Renaming would create churn without clarity gain. |

### Schema migration policy

**Decision**: edit `0001_initial_schema.sql` in place.

**Rationale**:
- `v0.1-sprint-1` git tag is the immutable historical record. `git show v0.1-sprint-1:crates/loomweave-storage/migrations/0001_initial_schema.sql` reproduces the pre-rename shape if anyone needs it. The tag does not lock the migration file's mutability; it locks the code at a published commit.
- No real consumers exist. The only writer of `.loomweave/loomweave.db` today is the walking-skeleton fixture (`tests/e2e/sprint_1_walking_skeleton.sh`). No external operator has run `loomweave analyze` against a real codebase and produced a database whose `schema_migrations` ledger we would break by rewriting `0001`.
- Stacking `0002_*.sql` to preserve a migration that nobody has applied creates ledger debt for a reader-fiction. The same anti-pattern (legacy filenames preserved "for history") is explicitly rejected by repo convention.
- ADR-011 + the comment at `0001_initial_schema.sql:1-9` says the full shape is "frozen at L1-lock time." L1 was a *design lock* (Sprint 1 lock-in #1: schema shape), not a *consumer lock*. The lock-ins exist to prevent in-flight Sprint-1 churn; they are not a no-edit-ever rule against post-Sprint-1 design correction.

**Retirement condition** (when this policy switches from "edit in place" to "stack-only"): the first time any external operator pulls a published Loomweave build (release artefact, package install, or `git clone` followed by `loomweave install` against their own codebase) and produces a `.loomweave/loomweave.db` with a `schema_migrations` ledger. After that point, rewriting `0001` would break their migration ledger; every subsequent schema change must stack as `0002_*.sql`, `0003_*.sql`, and so on.

The retirement trigger is observable: a published release tag plus an out-of-repo bug report filing or a non-author git history accessing the binary. Until then, in-place edits are the lower-debt path. After the trigger fires, this ADR retires its in-place clause and stack-only policy applies; this ADR does not need to be superseded for that transition — the trigger is named here so the policy switch is recognisable when it happens.

### CASE mapping for `scope_rank`

The semantic ordering `project → subsystem → package → module → class → function` (outer composition first) maps to integer ranks `1..6`:

| `scope_level` value | `scope_rank` | Meaning |
|---|---|---|
| `project` | 1 | Outermost; lowest precedence; broadest applicability |
| `subsystem` | 2 | |
| `package` | 3 | |
| `module` | 4 | |
| `class` | 5 | |
| `function` | 6 | Innermost; highest precedence; narrowest applicability |

`ORDER BY scope_rank ASC` produces the documented composition order. Ties are broken by `authored_at` per the existing guidance-resolver design (`system-design.md:675`).

### `findings.run_id` ↔ `runs.id` relationship

This ADR confirms that `findings.run_id` and `runs.id` are the same string. The Rust writer-actor produces both identifiers from the same `Uuid::new_v4()` per analyze invocation. The Sprint 2 F-17 follow-up encoded that provenance relationship mechanically: `findings.run_id` now references `runs(id)` in `0001_initial_schema.sql`.

## Alternatives Considered

### Alternative 1: stack `0002_*.sql` instead of editing `0001` in place

A clean second migration that drops the `priority` column and adds the new ones, keeps `0001` byte-identical to `v0.1-sprint-1`.

**Pros**: the published-tag invariant ("once tagged, never edit") is conventional in many projects; cleanly separates the v0.1-shipped state from v0.1-corrected state; no mental burden about whether a given migration file matches what's at the tag.

**Cons**: there are no real consumers whose `schema_migrations` ledger we are protecting. Stacking creates a migration sequence whose length is unrelated to actual schema-evolution complexity — a future reader sees `0002_rename_priority.sql` and assumes it's a real migration step rather than an artefact of a "do not touch tagged files" convention. The `git show v0.1-sprint-1:...sql` route reproduces the historical shape with no extra ceremony. Compounding cost: every Sprint-2 schema correction would land as `0003_*.sql`, `0004_*.sql`, ... while the codebase still has zero real users. Pure ceremony.

**Why deferred**: the policy *will* switch to stack-only the moment a real consumer exists. That trigger is named in this ADR. Pre-trigger, in-place editing is the lower-debt path; post-trigger, stacking is mandatory. Choosing the right policy for the current state, with a written transition trigger, is not the same as denying the value of stacking once consumers exist.

### Alternative 2: keep `priority` as the column name; document the ordering surprise

Leave the schema field as `priority`, fix the affinity by adding a `priority_rank` companion column without renaming. Document the cross-product clash in the glossary but accept it.

**Pros**: smallest mechanical change; no rename churn in code or docs.

**Cons**: locks in the cross-product confusion. Every Weft user reading Loomweave's `priority` field will hit the same misframing the bug filer hit. The audit's own diagnosis is that the *vocabulary* is the bug, not just the affinity. Refusing the rename means accepting that every cross-product reader pays the disambiguation cost forever; the cumulative cost outstrips the one-time rename cost within months of v0.1 release.

**Why rejected**: the audit's framing is correct. Cosmetic correctness here is structural correctness later.

### Alternative 3: split `provenance` into separate names per role

`finding.provenance` for the tool/version/run_id struct; `guidance.origin` for the manual/wardline_derived/filigree_promotion enum. Two different words for two different shapes.

**Pros**: each name describes its specific shape more concretely; the reader sees `origin` and immediately thinks "enum value," sees `provenance` and thinks "structured metadata."

**Cons**: the role is the same in both — "where did this record come from." Differentiating by shape rather than role makes the field name a function of the implementation, not the design intent. Future shape-evolution (e.g., `guidance.origin` becomes a struct in v0.2 to capture *who* manually authored a sheet) forces another rename.

**Why rejected**: field names should describe role, not shape. SARIF and SBOM both use `provenance` for both struct-shaped and enum-shaped origin metadata.

### Alternative 4: defer the rename to v0.2; comment the issue with corrected framing

Add the audit's findings to the glossary as `open` clashes; defer the schema correction to a later sprint when guidance-resolution code (WP6) actually ships.

**Pros**: avoids any Sprint-2 churn for hygiene work; lets Tier B start unblocked.

**Cons**: WP4 / B.4 (catalog rendering) is the immediate next step, and `catalog.json` plus per-subsystem markdown will reference field names. If those names are `priority` / `critical` / `source` and we later rename, every Tier-B-rendered fixture file gets stale and downstream tools learn the wrong names. Cheaper to rename now (one schema, one set of docs, one test) than after Tier B serialises the wrong vocabulary into output files.

**Why rejected**: the rename is cheaper before Tier B than after. Doing it now is the leverage move.

## Consequences

### Positive

- Cross-product reader confusion on `priority` and `critical` ends. A reader of Loomweave's docs and Filigree's CLI in the same session no longer has to disambiguate these words.
- The schema can serve correct ordered queries on guidance composition. WP6 (briefing serving) will write `ORDER BY scope_rank ASC` and get the documented composition order without query-time CASE expressions.
- The within-Loomweave `source` overload is reduced from three uses to one (`entity.source = SourceRange`), with the type name carrying the namespace.
- The glossary becomes a real artefact backed by an Accepted ADR, not a wishful one. Future cross-product field names cite the ADR-acceptance rule that produced this ADR.
- The migration-policy question that was implicit until now becomes explicit. Future schema changes have a written trigger condition for when to switch from in-place edits to stacking.

### Negative

- The rename has a wider blast radius than the schema alone. `requirements.md`, `system-design.md`, and `detailed-design.md` all carry references that must move; this ADR's commit updates them in lockstep. Historical ADRs (e.g., [ADR-007](./ADR-007-summary-cache-key.md) at line 56 still references `critical: true` in its example) are **not** touched — Accepted ADRs are immutable per repo convention. Readers of those historical ADRs should map the old field names to the post-ADR-024 ones using this ADR's rename table. Plan and review documents archived under `docs/implementation/` (`agent-plans/`, `v0.1-reviews/`) are also left alone (historical snapshots, not normative).
- The walking-skeleton fixture's `.loomweave/loomweave.db` (committed for the e2e test) is rebuilt with the new schema as part of this change. The test script's expectations stay the same; the database file changes. Mitigation: the e2e script doesn't depend on the schema's column names; it asserts on the persisted entity row's `id` and `kind`.
- An external operator who *did* run a pre-publication build of Loomweave against a real codebase between Sprint 1 close and this ADR has a `.loomweave/loomweave.db` with the old `priority` column. Mitigation: nobody has — this is the explicit pre-condition for the in-place policy. Any future operator who runs the new build sees the corrected schema only.
- Re-using the `0001` migration version number means the `schema_migrations` ledger row is identical (`version=1`, `name='0001_initial_schema'`) before and after the edit. A consumer who applied the pre-edit `0001` and then upgrades will not see any migration to apply, and their database will diverge silently from the new shape. Mitigation: the same pre-condition — no such consumer exists; the retirement trigger names exactly this case.

### Neutral

- `entity.source` (`SourceRange`) keeps its name. The audit explicitly preserved this; the type name does the disambiguation work.
- ADR-018 amendment trigger (`clarion-889200006a`, L7 qualname divergence with Wardline `FingerprintEntry`) is unaffected; that clash is a wire-shape mismatch on a different field and remains deferred to WP9.
- The CHECK-constraint policy gap (no `CHECK` on `entities.kind`, `findings.severity`, etc., relied on writer-actor validation) is *not* addressed by this ADR. Filed as filigree issue F-13 in the audit; will get its own ADR if the policy decision warrants one.

## Related Decisions

- [ADR-017](./ADR-017-severity-and-dedup.md) — the model managed-clash pattern this ADR follows. Severity vocabulary's `metadata.loomweave.internal_severity` round-trip slot is the kind of explicit mapping that prevents the unmanaged-clash failure mode this ADR retrofits.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — the rule-ID grammar enforcement at the plugin boundary is the namespacing model. This ADR does not introduce a new namespace; it removes a vocabulary collision.
- [ADR-011](./ADR-011-writer-actor-concurrency.md) — names migration `0001_initial_schema.sql` as the L1 lock target. This ADR clarifies that the lock is design-shape, not file-mutability, while no consumers exist.
- [ADR-018](./ADR-018-identity-reconciliation.md) — analogous cross-product field-name divergence (Wardline `FingerprintEntry` vs. Loomweave L7 qualname). Different field, different resolution path (deferred), same class of problem.

## References

- [Skeleton audit](../../implementation/handoffs/2026-05-03-skeleton-audit.md) — durable record of the audit pass, reviewer feedback (architecture-critic + leverage-analyst), and the reconciled plan that produced this ADR.
- [Weft suite glossary](../../suite/glossary.md) — the federation-safe design-review artefact this ADR moves three entries from `open` to `managed` within.
- [Weft federation axiom](../../suite/weft.md) §3–§5 — the doctrine the glossary defends; the failure-test mode this ADR addresses is reader-side cross-product disambiguation cost.
- `crates/loomweave-storage/migrations/0001_initial_schema.sql:163-191` — the schema sites this ADR edits in place.
- `crates/loomweave-storage/tests/schema_apply.rs:138-169` — the test that documented the bug and is rewritten to test the corrected design.
- `docs/loomweave/1.0/detailed-design.md:204, 270, 449, 453, 467, 471, 737-748` — the design-doc sites this ADR's renames touch.
- `docs/loomweave/1.0/system-design.md:346, 675` — the system-level guidance composition references.
- Filigree issue `clarion-4cd11905e2` — the misframed bug whose triage produced this audit.
