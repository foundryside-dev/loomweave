# ADR-017: Severity Mapping, Rule-ID Round-Trip, and Dedup Policy

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: Clarion's internal finding vocabulary does not match Filigree's wire vocabulary, and Filigree's dedup key does not match Clarion's entity-centric mental model. Both need a committed mapping.

## Summary

Clarion's internal severity vocabulary is `{INFO, WARN, ERROR, CRITICAL}` for defects plus `NONE` for facts; Filigree's wire vocabulary is `{critical, high, medium, low, info}`. The mapping is one-to-one in the forward direction; round-trip fidelity is preserved by copying the internal value into `metadata.clarion.internal_severity` so read-back can recover it. Rule IDs are namespaced by emitter (`CLA-PY-*`, `CLA-INFRA-*`, `CLA-FACT-*`, `CLA-SEC-*`, plus Wardline's own namespaces) and round-trip byte-for-byte — Filigree does not enforce the namespace, but ADR-022's grammar check does at the Clarion-plugin boundary. v0.1 dedup relies on Filigree's existing key (`file_id`, `scan_source`, `rule_id`, `coalesce(line_start, -1)`) plus `mark_unseen=true` on ingest; entity moves within a file transition old-position findings to `unseen_in_latest` rather than overwriting them. Server-side per-entity dedup is deferred to v0.2 (NG-21).

## Context

Integration reconnaissance (`integration-recon.md` §2.3, §2.4) verified three concrete mismatches between Clarion's design and Filigree's production intake:

1. **Severity vocabulary.** Clarion's internal enum (`INFO`, `WARN`, `ERROR`, `CRITICAL`, `NONE`) is a defect-plus-fact split; Filigree's intake takes `{critical, high, medium, low, info}` lowercase and coerces unknowns to `"info"` with a `warnings[]` response (`db_schema.py:133-148`). A one-way emit loses information; a round-trip loses the `NONE`-is-a-fact distinction entirely.

2. **Rule-ID namespacing.** Filigree stores `rule_id` as a free-text string (`db_schema.py:154-155`) — no enum enforcement. Clarion emits several rule-ID prefix conventions (`CLA-PY-*` for Python-plugin structural findings, `CLA-INFRA-*` for pipeline failures, `CLA-FACT-*` for factual findings, `CLA-SEC-*` for security findings); Wardline emits its own (`PY-WL-*`, `SUP-*`, `SCN-*`, `TOOL-ERROR`). Without a committed mapping and a grammar rule (ADR-022), prefixes drift. The panel's self-sufficiency review (`04-self-sufficiency.md` Issue 7) found exactly this: `requirements.md` REQ-ANALYZE-06 uses `CLA-PY-PARSE-ERROR` but `detailed-design.md` §10 uses `CLA-INFRA-PARSE-ERROR` for the same finding.

3. **Dedup key.** Filigree's server-side dedup key is `(file_id, scan_source, rule_id, coalesce(line_start, -1))` (`db_schema.py:156-157`). Clarion's mental model is entity-centric — the same rule on the same entity is a single finding, even if the entity moves. The dedup key does not carry `entity_id`, so two `clarion analyze` runs that see the same entity at different line numbers produce two findings rather than one.

Each mismatch has an operational consequence: lost severity round-trip (operators can't recover Clarion's internal gradation from Filigree's storage), inconsistent rule IDs (triage dashboards have to know both spellings of the same error), and dedup blow-up on every entity move.

## Decision

### Severity mapping (emit)

Clarion's emit path uses this one-to-one mapping:

| Clarion internal | Filigree wire | Notes |
|---|---|---|
| `CRITICAL` | `critical` | Direct |
| `ERROR` | `high` | Direct |
| `WARN` | `medium` | Direct |
| `INFO` | `info` | Direct |
| `NONE` (facts) | `info` | With `metadata.clarion.kind = "fact"` to distinguish from `INFO` defects |

Every emitted finding also sets `metadata.clarion.internal_severity` to the pre-mapping value (one of the five Clarion internal values). This is the round-trip anchor.

### Severity read-back (consume)

Clarion's consume path (reading its own findings back from Filigree):

1. If `metadata.clarion.internal_severity` is present → use that value. Lossless round-trip.
2. Else if `severity` is present → reverse-map the wire value to Clarion's internal vocabulary using the table above (reversed). `critical`→`CRITICAL`, `high`→`ERROR`, `medium`→`WARN`, `low`→`INFO` (Filigree-produced only; Clarion itself never emits `low`), `info`→`INFO` if `metadata.clarion.kind != "fact"`, else `NONE`.
3. Else → default `INFO`, emit `CLA-INFRA-FINDING-SEVERITY-MISSING` on the consume path.

The read-back path handles Wardline-sourced findings (no `metadata.clarion.internal_severity`) and Filigree-authored findings (no Clarion metadata) without conflating them with Clarion-authored ones.

### SARIF-level mapping (translator input)

The `clarion sarif import` translator (ADR-015) maps SARIF `result.level` into Clarion internal severity before the emit table above applies:

| SARIF level | Clarion internal | Filigree wire (via emit table) |
|---|---|---|
| `error` | `ERROR` | `high` |
| `warning` | `WARN` | `medium` |
| `note` | `INFO` | `info` |
| `none` | `INFO` | `info` (with `CLA-INFRA-SARIF-LEVEL-NONE` translator warning — SARIF `none` is rare and usually indicates a tool-side bug) |

### Rule-ID namespacing

Clarion rule IDs are namespaced by emitter and purpose:

- `CLA-PY-*` — Python-plugin structural and rule findings (`CLA-PY-STRUCTURE-001`, `CLA-PY-UNRESOLVED-IMPORT`, etc.). Emittable only by the Python plugin. **Includes parse errors**: `CLA-PY-PARSE-ERROR` is the canonical ID for a Python source file the plugin cannot parse.
- `CLA-FACT-*` — factual observations, emittable by any plugin or the core (`CLA-FACT-TODO`, `CLA-FACT-ENTRYPOINT`, `CLA-FACT-CONDITIONAL-IMPORT`).
- `CLA-INFRA-*` — pipeline and infrastructure failures; core-only. Reserved namespace per ADR-022.
- `CLA-SEC-*` — security-domain findings; emitted by the core's security-gate subsystem (`CLA-SEC-SECRET-DETECTED`, `CLA-SEC-UNREDACTED-SECRETS-ALLOWED`).

Wardline namespaces (passed through verbatim): `PY-WL-*`, `SUP-*`, `SCN-*`, `TOOL-ERROR`. Future-tool namespaces reserved: `COV-*` (coverage), `SEC-*` (standalone security scanner). ADR-022 enforces the grammar at the Clarion plugin boundary; Filigree performs no enforcement.

**Issue 7 fix**: `CLA-PY-PARSE-ERROR` is correct; `detailed-design.md` §10's use of `CLA-INFRA-PARSE-ERROR` is the bug. A parse error is Python-plugin-emitted (the plugin tries to parse and fails); it is not a pipeline failure. The namespace rule is "emitter + purpose." This ADR's authoring is the occasion to fix that drift; the detailed-design §10 error surfaces table updates alongside ADR-017's acceptance.

### Rule-ID round-trip

Filigree stores `rule_id` byte-for-byte. Clarion's consult tools re-namespace on display: findings with `scan_source="wardline"` surface as "Wardline/PY-WL-001-GOVERNED-DEFAULT"; findings with `scan_source="clarion"` surface unprefixed. This is a rendering convention, not a storage convention.

### Dedup policy

Filigree's server-side dedup key is `(file_id, scan_source, rule_id, coalesce(line_start, -1))`. Clarion's emit path:

- **Every batch**: POST with `mark_unseen=true`. On a rule + file + line tuple that has not appeared in this run but appeared in a prior run, Filigree transitions the prior finding to `unseen_in_latest`. New findings at different line numbers insert as new rows.
- **Resume path** (`clarion analyze --resume`): POST with `mark_unseen=false`. Re-using the same `run_id` on resume must not trigger "no longer seen" transitions for findings the current run has not yet re-visited.
- **Prune policy**: `clarion analyze --prune-unseen` removes `unseen_in_latest` findings older than 30 days (configurable via `clarion.yaml:findings.prune_unseen_days`). Operators running this in CI keep the findings store bounded without losing recent history.

**Entity move within a file**: producing `unseen_in_latest` at the old line and a fresh finding at the new line is the v0.1 behaviour. It is a known coarseness — operators see the same finding twice briefly (old position marked `unseen_in_latest`, new position active) until `--prune-unseen` runs. The v0.2 improvement (NG-21) is server-side per-entity dedup: an optional `entity_id` extension field on Filigree's dedup key, so `(file_id, scan_source, rule_id, entity_id)` tracks the logical finding across position changes.

### Rule-ID synthesis (explicit non-option)

Clarion does **not** synthesise rule IDs with entity IDs (e.g., `CLA-PY-STRUCTURE-001__python:class:foo`). That would defeat Filigree's cross-run triage — every rule-on-every-entity becomes unique, so "acknowledge all STRUCTURE-001 findings" no longer works as a triage operation.

## Alternatives Considered

### Alternative 1: Rule-ID synthesis with entity IDs

Concatenate entity IDs into rule IDs to sidestep Filigree's dedup key mismatch entirely.

**Pros**: eliminates the entity-move dedup problem at the ID level; no `mark_unseen=true` workaround needed.

**Cons**: destroys the triage utility of rule IDs. Filigree's UI groups by rule ID to let operators acknowledge a whole class of findings at once; synthesised IDs make every finding unique, so the group-acknowledge operation stops working. Every Filigree query that filters by `rule_id` breaks.

**Why rejected**: the cost (losing triage) is worse than the problem (transient `unseen_in_latest` findings that prune after 30 days).

### Alternative 2: Client-side dedup in Clarion

Clarion reads back its own findings before POSTing, deduplicates client-side, and only emits deltas.

**Pros**: no Filigree-side changes; Clarion controls the dedup semantics fully.

**Cons**: introduces read-back coupling on the emit path — Clarion now depends on Filigree being reachable *and* complete before it can emit. A new `clarion analyze` run cannot start emitting until the prior run's findings are all readable. Shifts cross-run identity tracking to Clarion's side, which has no database of Filigree-stored findings; every dedup operation requires a HTTP read. Net worse than the current workaround.

**Why rejected**: creates a synchronous dependency on Filigree state during emit.

### Alternative 3: Push Filigree-side per-entity dedup in v0.1

Add `entity_id` as an optional fifth column in Filigree's dedup key in v0.1.

**Pros**: cleanest outcome — dedup matches the entity-centric mental model. No `unseen_in_latest` noise on entity moves.

**Cons**: Filigree-side schema migration. Existing `file_records` + findings need the migration, existing dedup-query paths need updating, and the `(file_id, scan_source, rule_id, coalesce(line_start, -1))` index must either be supplemented or replaced. Within-scope (same author owns Filigree) but real v0.1 engineering time on top of the registry-backend work (ADR-014).

**Why deferred**: the v0.1 `mark_unseen=true` workaround is bounded (`--prune-unseen` controls unbounded growth) and well-understood. NG-21 names the v0.2 improvement; deferral is a scope-commitment decision, not a design compromise.

### Alternative 4: Drop Clarion's internal severity; use Filigree's wire vocabulary directly

Clarion's internal records store `{critical, high, medium, low, info}` directly. No mapping needed.

**Pros**: zero mapping surface; round-trip is trivial (no metadata dance).

**Cons**: loses the `CRITICAL`/`ERROR` distinction in Clarion's own queries. `CRITICAL` means "production-impacting" in Clarion's register; `ERROR` means "design-smell or bug." Filigree's `critical`/`high` mapping preserves a *wire* distinction but the values are operator-facing triage labels, not Clarion's internal semantics. Collapsing them means Clarion's internal query "what critical findings exist?" now returns triage-labelled `critical` findings, not Clarion-semantics `CRITICAL` findings.

**Why rejected**: the internal vocabulary is load-bearing for Clarion's own reasoning; the mapping surface is small.

## Consequences

### Positive

- Round-trip fidelity for Clarion-emitted findings. An operator who re-reads Clarion's findings from Filigree recovers the internal severity exactly.
- Rule-ID namespacing is now enforceable at the Clarion boundary (ADR-022 grammar check) and round-trips cleanly to Filigree (byte-for-byte). The `CLA-INFRA-PARSE-ERROR` vs `CLA-PY-PARSE-ERROR` drift noted in the panel's self-sufficiency review has a committed resolution.
- Dedup policy is explicit and bounded. `mark_unseen=true` + 30-day prune gives operators a predictable findings-store size. The v0.2 NG-21 upgrade path is named.
- SARIF translator severity is locked; operators running `clarion sarif import wardline.sarif.baseline.json` see Wardline's `error`/`warning`/`note` land at the correct Filigree wire values.

### Negative

- Entity moves within a file produce transient duplicates (old-position `unseen_in_latest`, new-position active) until pruning. Operators reading Filigree UI during an active `clarion analyze` see both. Mitigation: the two-value window is expected and documented; `--prune-unseen` run in CI keeps the window bounded.
- The severity mapping is asymmetric by necessity (`NONE`/`INFO` both map to `info` on the wire) — recovering `NONE` on read-back relies on `metadata.clarion.kind` presence. If another emitter writes `info` without that metadata field, Clarion's read-back treats it as `INFO`. This is the correct default (unknown info-severity findings are defects by default) but operators writing custom emitters into Filigree should know the convention.
- Rule-ID namespace enforcement is at the Clarion boundary, not at Filigree. A Wardline bug that emits `CLA-PY-*` IDs (shouldn't happen, but could under maintenance error) lands in Filigree without complaint. Mitigation: ADR-022 + cross-tool fixture tests (`detailed-design.md` §9.3) catch this class of error at release time.

### Neutral

- `scan_run_id` lifecycle (create at Phase 0, complete at last batch, don't complete on resume) is already specified in detailed-design §7. This ADR doesn't re-decide it but cites it; the `mark_unseen=true` policy sits inside that lifecycle.
- SARIF property-bag preservation (ADR-019) is complementary: the severity mapping here and the property-bag mapping there together define the full SARIF → Filigree translation for the ADR-015 translator.

## Related Decisions

- [ADR-004](./ADR-004-finding-exchange-format.md) — Filigree-native intake is the target format; this ADR fills in the severity and rule-ID mapping that the format needs.
- [ADR-015](./ADR-015-wardline-filigree-emission.md) — the SARIF translator applies the severity mapping defined here. Rule-ID round-trip for Wardline's `PY-WL-*` / `SUP-*` / `SCN-*` namespaces happens through this ADR's byte-for-byte policy.
- ADR-019 (pending) — SARIF property-bag preservation complements this ADR. Where ADR-019 governs `metadata.<driver>_properties.*` for extension keys, this ADR governs severity + rule-ID values.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — grammar enforcement on rule-ID prefixes is implemented at manifest acceptance and RPC time; this ADR's namespace choices populate the allowed set.

## References

- [Clarion v0.1 detailed design §7](../v0.1/detailed-design.md) (lines 1163-1200) — the canonical mapping tables; this ADR formalises what's already there and fixes Issue 7.
- [Clarion v0.1 integration reconnaissance §2.3, §2.4](../../implementation/v0.1-reviews/pre-restructure/integration-recon.md) — empirical evidence for severity vocabulary and dedup key on Filigree's side (`db_schema.py` line numbers).
- [Panel self-sufficiency review — Issue 7](../../implementation/v0.1-reviews/panel-2026-04-17/04-self-sufficiency.md) (lines 132-134) — rule-ID namespace inconsistency this ADR resolves.
- [Clarion v0.1 requirements — REQ-FINDING-02, NG-21](../v0.1/requirements.md) — rule-ID namespace rule; v0.2 server-side per-entity dedup deferral.
