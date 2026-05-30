# Loom suite glossary

**Audience**: anyone designing or reviewing a cross-product-visible field name, ADR, or wire-shape change in any Loom product
**Purpose**: a single read-only catalogue of terms whose meaning crosses product boundaries, so the same word never silently means two things in the federation
**Companion**: [loom.md](./loom.md) for the federation axiom this glossary defends

---

## How to use this glossary

This file is a **design-review artifact**, not infrastructure. Nothing imports it, nothing runs from it, and removing it changes no product's semantics — it is the same shape as `loom.md` itself. Per `loom.md` §5, this means the glossary is federation-safe: it does not introduce semantic coupling, initialization coupling, or pipeline coupling between siblings.

**Consult this glossary when**:

- Authoring an ADR that introduces or renames a cross-product-visible field name
- Reviewing a wire-format change that adds a new top-level key
- Onboarding to a Loom product after working on another, to surface vocabulary surprises
- Triaging a bug whose framing depends on what a word means (the trigger that produced this glossary was exactly such a triage — see [skeleton-audit](../implementation/handoffs/2026-05-03-skeleton-audit.md))

**Update this glossary when**:

- An ADR moves a term from `open` to `managed` (note the ADR ID in the Authority column)
- A new cross-product term is introduced (add a row before the ADR is Accepted; see ADR-acceptance rule below)
- A term retires from cross-product visibility (mark `retired` with the retirement ADR ID, do not delete)

**Do not** add CI lint, repo gate, or runtime check that consumes this file. Per `loom.md` §5, that would convert a federation-safe doc into shared infrastructure. The glossary is consulted by humans during design review; that is its only job.

## ADR-acceptance rule

ADRs introducing cross-product-visible field names must update this glossary before moving from Proposed to Accepted, with one of three explicit verdicts:

- **`no clash`** — the term is unique to this product, no sibling currently uses it
- **`managed clash`** — a sibling uses the same term; an explicit mapping table exists in the ADR (model: ADR-017's severity vocabulary table with `metadata.clarion.internal_severity` round-trip slot)
- **`renamed`** — the proposed term clashed with a sibling; this ADR renames the local term to avoid the clash

A vocabulary verdict is part of ADR-acceptance evidence, not a courtesy. Three of Clarion v0.1's clashes (`severity`, `rule_id`, `finding` wire shape) got managing ADRs at design time and shipped clean. Three did not (`priority`, `critical`, `source`) and required retrofit. This rule converts the next clash from "discovered during implementation" to "blocked at design review."

## Status legend

| Status | Meaning |
|---|---|
| `managed` | Same term used by ≥2 products; an Accepted ADR provides explicit mapping or namespacing |
| `renamed` | Was a clash; an Accepted ADR renamed the local term to avoid the collision; the cross-product collision is gone |
| `open` | Same term used by ≥2 products; **no managing ADR yet** — clash is live |
| `no clash (informational)` | Term is unique to one product but listed here to head off cross-product reader confusion |
| `deferred` | Clash exists; retirement condition documented; tracked elsewhere |
| `retired` | Was a clash; retiring ADR named; kept as historical record |

## Cross-product terms

### Managed clashes

| Term | Products | Semantics by product | Authority |
|---|---|---|---|
| `severity` | Clarion ↔ Filigree | Clarion internal: `INFO\|WARN\|ERROR\|CRITICAL` for defects, `NONE` for facts. Filigree wire: `critical\|high\|medium\|low\|info` (lowercase). | [ADR-017](../clarion/adr/ADR-017-severity-and-dedup.md) — explicit mapping table; `metadata.clarion.internal_severity` round-trip slot |
| `rule_id` | Clarion + Wardline → Filigree | Namespaced prefix per emitter: `CLA-PY-*`, `CLA-INFRA-*`, `CLA-FACT-*`, `CLA-SEC-*`, `WLN-*`. Filigree stores byte-for-byte; round-trip preserved. | [ADR-017](../clarion/adr/ADR-017-severity-and-dedup.md), [ADR-022](../clarion/adr/ADR-022-core-plugin-ontology.md) — namespacing convention + grammar enforcement at the Clarion-plugin boundary |
| `finding` (wire shape) | Clarion + Wardline → Filigree | Cross-product unified record type. Field ownership documented; extension via `metadata` slot (top-level keys outside the enumerated set are silently dropped). | [ADR-004](../clarion/adr/ADR-004-finding-exchange-format.md) — full wire schema with explicit ownership |

### Renamed clashes (resolved by ADR-024 — see [skeleton-audit](../implementation/handoffs/2026-05-03-skeleton-audit.md))

These entries record the resolution per the `renamed` verdict (see ADR-acceptance rule above). Each row names the pre-rename collision and the post-rename Clarion field name; Filigree's vocabulary is unchanged.

| Term (post-rename) | Products | Resolution | Authority |
|---|---|---|---|
| `scope_level` (Clarion) ← was `priority` | Clarion ↔ Filigree | Clarion's guidance scope-of-applicability field is now `scope_level` (six-level string enum, semantics unchanged) plus a companion `scope_rank` integer (CASE-mapped 1..6) for `ORDER BY` queries. Filigree's `priority` (P0..P4) keeps its name. The shared word is gone. | [ADR-024](../clarion/adr/ADR-024-guidance-schema-vocabulary.md) |
| `pinned` (Clarion) ← was `critical` | Clarion ↔ Filigree | Clarion's guidance budget-protection flag is now `pinned: bool` (semantics: preserved across token-budget pressure, unchanged). Filigree's `severity:critical` tier and informal "Critical" P0 label keep their meanings. | [ADR-024](../clarion/adr/ADR-024-guidance-schema-vocabulary.md) |
| `provenance` (Clarion) ← was `source` (on `finding` and `guidance` only) | Within-Clarion + Clarion ↔ Filigree | Clarion's `finding.source` struct (`{tool, tool_version, run_id}`) is now `finding.provenance`; the `entity.properties.source` enum on guidance entities (`"manual"\|"wardline_derived"\|"filigree_promotion"`) is now `entity.properties.provenance`. `entity.source` (`SourceRange` on code entities) is unchanged — the type name disambiguates. Filigree's `source:` taxonomy label keeps its meaning. | [ADR-024](../clarion/adr/ADR-024-guidance-schema-vocabulary.md) |

### No-clash informational entries

| Term | Owning product | Note for cross-product readers |
|---|---|---|
| `tags` (Clarion) vs `labels` (Filigree) | both | Different word, similar concept. Clarion's `tags` are free-form (plugin/LLM-emitted); Filigree's `labels` are a curated namespaced taxonomy (`area:`, `cluster:`, `effort:`, `priority:`, …). The names accurately reflect the design difference. No rename. |
| `kind` | Clarion (three uses) | Used three ways within Clarion: `entity.kind` (entity taxonomy), `edge.kind` (edge taxonomy), `finding.kind` (`defect\|fact\|classification\|metric\|suggestion`). Disambiguated by struct context; the type carries the namespace. Filigree uses `type` for the analogous concept on issues. |
| `status` | Clarion + Filigree | Distinct state machines on distinct objects: Clarion `runs.status`, Clarion `findings.status` (`open\|acknowledged\|suppressed\|promoted_to_issue` per `detailed-design.md` §6.5; Filigree-side mapping in `detailed-design.md` §7), Filigree per-type issue state machines (`bug` has `triage→confirmed→fixing→...`). Always disambiguated by table or struct. |
| `entity` | Clarion | Clarion code object (function, class, module, guidance, file, subsystem). Other products do not use this term. |
| `subsystem` | Clarion | Cluster of entities produced by Phase 3 clustering. Clarion-only. |
| `briefing` | Clarion | Structured per-entity summary served to consult-mode agents. Clarion-only. |
| `guidance sheet` | Clarion | Institutional knowledge attached to an entity. Clarion-only. |
| `observation` | Filigree | Fire-and-forget agent note that expires after 14 days. Filigree-only. (Note: Clarion `clarion-` prefixed issue IDs may surface in observations, but `observation` as a record type is Filigree-owned.) |
| `finding` (record vs. wire) | Clarion + Wardline (record); Filigree (wire) | Clarion and Wardline both produce `finding` records with internal vocabulary. The wire shape that crosses into Filigree is the managed-clash form documented above. Locally each product's `Finding` struct has product-specific fields beyond the wire schema. |
| `run` / `run_id` | Clarion + Wardline | Each product has its own analyse/scan run lifecycle. The `run_id` field on a finding is namespaced by emitter (per `provenance.tool`); the strings are not assumed cross-product-meaningful. |

### SP9 Wardline taint-store wire terms (ADR-036)

These terms cross the Wardline↔Clarion wire in the SP9 taint-store contract (`/api/wardline/*` routes). All are `no clash`: each is either Wardline-namespaced or a field name unique to this Clarion surface, and none collides with an existing sibling term. Per the ADR-acceptance rule, recorded here as part of ADR-036's acceptance evidence. (The Clarion-internal table name `wardline_taint_facts` and config key `serve.http.wardline_taint_write` are deliberately omitted — they never cross the wire to Wardline.)

| Term | Products | Semantics | Authority |
|---|---|---|---|
| `wardline_json` | Clarion ↔ Wardline | The taint/provenance fact blob. **Opaque to Clarion and Wardline-owned**: Clarion stores and returns it verbatim, never parses, validates, or depends on its contents. All taint semantics stay Wardline-side. | [ADR-036](../clarion/adr/ADR-036-wardline-taint-fact-store.md) — `no clash` |
| `scan_id` | Clarion ↔ Wardline | Wardline's scan generation identifier for a taint fact, accepted as a queryable column for observability + an optional future prune-by-scan. Wardline-namespaced; not assumed cross-product-meaningful (cf. the `run`/`run_id` entry above). | [ADR-036](../clarion/adr/ADR-036-wardline-taint-fact-store.md) — `no clash` |
| `content_hash_at_compute` | Clarion ↔ Wardline | The containing-file content hash Wardline recorded **at compute time** (whole-file `blake3`, hex — Clarion's existing definition). Stored as a queryable column; Wardline compares it against `current_content_hash` to decide freshness. | [ADR-036](../clarion/adr/ADR-036-wardline-taint-fact-store.md) — `no clash` |
| `current_content_hash` | Clarion ↔ Wardline | The entity's containing-file content hash **as derived now** at read time (same whole-file `blake3` definition), returned on fetch. Match with `content_hash_at_compute` → fact is fresh; mismatch/absent → stale → Wardline recomputes. | [ADR-036](../clarion/adr/ADR-036-wardline-taint-fact-store.md) — `no clash` |
| `unresolved_qualnames` | Clarion ↔ Wardline | The list of pre-composed qualnames a batch write could **not** resolve to an `exact` Clarion entity (heuristic/none are never written); returned so Wardline can fall back rather than guess. Distinct from the deferred L7-qualname-format clash below. | [ADR-036](../clarion/adr/ADR-036-wardline-taint-fact-store.md) — `no clash` |

### Deferred clashes (tracked, not resolved)

| Term | Products | Status | Tracked by |
|---|---|---|---|
| L7 qualname format | Clarion ↔ Wardline | Clarion's L7 emits combined dotted `module.qualified_name`; Wardline's `FingerprintEntry` stores `(module, qualified_name)` as separate fields. No semantic clash today (Sprint 1 does not join across this boundary); becomes load-bearing at WP9 (Loom integrations). | [ADR-018](../clarion/adr/ADR-018-identity-reconciliation.md) amendment trigger; filigree issue `clarion-889200006a` (sprint:2 / wp:9). Trigger: WP9 attempts the first cross-product join. |

## Wardline-side terms (for cross-product reader benefit)

These terms are owned by Wardline. Listed here so a Clarion or Filigree reader does not assume Clarion-side semantics.

| Term | Wardline meaning |
|---|---|
| `Tier N` | Trust tier classification level applied to entities. Numeric. |
| `annotation_group` / `wardline_group` | Group of related Wardline annotations sharing a tier or policy band. Used as a `match_rules.type` value in Clarion guidance sheets. |
| `FingerprintEntry` | Wardline's storage object pairing `(module, qualified_name)`. See deferred clash above. |
| `governed default` | Wardline policy concept: a default value declared as policy-governed (rule IDs like `PY-WL-001-GOVERNED-DEFAULT`). |

## Shuttle (proposed)

Shuttle is not in flight. When Shuttle's design begins, the first design-review pass against this glossary should add Shuttle's authoritative terms and explicitly check `change`, `apply`, `commit`, `rollback`, `transaction` against the existing Loom vocabulary surface.

## History

- **2026-05-03** — Glossary created during the v0.1 skeleton audit (Sprint 2 kickoff). Seeded with the three managed ADR-mediated clashes, the three open clashes resolved by ADR-024, the no-clash informational entries, and the deferred ADR-018 amendment trigger.
- **2026-05-03** — ADR-024 Accepted; the `priority`/`critical`/`source` rows moved from `open` to `renamed` (see "Renamed clashes" section). Schema migration `0001_initial_schema.sql` edited in place per the policy named in ADR-024.
- **2026-05-31** — ADR-036 Accepted; added the SP9 Wardline taint-store wire terms (`wardline_json`, `scan_id`, `content_hash_at_compute`, `current_content_hash`, `unresolved_qualnames`) as `no clash` informational entries, recorded as ADR-acceptance evidence.
