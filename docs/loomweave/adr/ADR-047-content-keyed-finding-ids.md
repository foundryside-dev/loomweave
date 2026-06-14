# ADR-047: Content-keyed finding IDs (cross-run idempotent findings)

**Status**: Accepted
**Date**: 2026-06-08
**Deciders**: john@pgpl.net
**Context**: Weft dogfood-#2 surfaced that Loomweave's `findings` table grows on
every re-analyze of an unchanged tree (`255 → 259 → 263`), so an agent browsing
findings sees ever-multiplying duplicates (L1, weft-f506e5f845 / clarion-772ff358da).

## Summary

A finding's primary key changes from a **run-scoped** id
(`core:finding:{run_id}:<discriminator>`) to a **content-keyed** id
(`core:finding:<discriminator>`). The `ON CONFLICT(id) DO UPDATE` upsert in
`write_finding_row` now de-duplicates the same logical finding across *fresh*
runs — not just a `--resume` re-walk — and the `run_id` *column* updates to the
latest run. Migration `0010` clears legacy run-scoped finding rows (findings are
regenerable). The accepted trade-off: **findings are current-state, not a
per-run append-log.**

## Context

`write_finding_row` upserts `ON CONFLICT(id)`. Every finding id embedded its
`run_id`, with the *intent* (documented in the old code comment) that "cross-run
ids never collide and a fresh run only ever INSERTs" — preserving a per-run
history. The consequence in practice:

- Each `loomweave analyze` mints a **new** run_id, so the same logical finding
  (same anchor entity, rule, evidence) became a **new row** every run. The
  `findings` table accumulated unboundedly. An agent calling a finding browser
  (which does not filter by the current run) saw all historical copies.
- Worse, the lifecycle columns the upsert is careful to preserve
  (`status`, `suppression_reason`, `filigree_issue_id`, `created_at`) were
  **orphaned** every run: the fresh row started at `status='open'`,
  `filigree_issue_id=NULL`. A finding promoted to a Filigree issue, or
  suppressed, silently lost that linkage on the next analyze.

Every discriminator was *already* content-derived — `entity-deleted:{entity_id}`,
`guidance-orphan:{guidance_id}:{deleted_entity_id}`, `secret:{blake3(entity,rule,evidence)}`,
`source-walk:{blake3(...)}`, `weak-modularity` (one per project), etc. — so the
`run_id` segment was the *only* thing making the id run-unique.

## Decision

1. **Drop `run_id` from the finding id** at every minting site (`analyze.rs`
   ×12, `secret_scan/findings.rs`). The id is now `core:finding:<discriminator>`,
   stable across runs for the same logical finding.
2. **The upsert is unchanged** and now does the right thing across runs:
   `ON CONFLICT(id) DO UPDATE` refreshes analysis-derived columns + `run_id` +
   `updated_at` to the latest run, while **preserving** `status`,
   `suppression_reason`, `filigree_issue_id`, `created_at`. A finding's Filigree
   linkage and suppression therefore **survive** re-analysis.
3. **`findings_for_emit(run_id)` is unchanged** (`WHERE run_id = ?1`). A
   reproduced finding carries the current run_id (the upsert set it), so it is in
   the emit set exactly as before; a finding that did *not* reproduce keeps its
   prior run_id and falls out of the set — identical to today's behavior, and the
   existing Filigree prune/soft-archive path handles its lifecycle.
4. **Migration `0010` clears legacy findings** (`DELETE FROM findings`). On an
   existing database the new content-keyed rows would otherwise land *beside* the
   orphaned run-scoped ones (a one-time worse doubling that no sweep matches).
   Findings are fully regenerable derived data; the next `analyze` repopulates
   them. (The store is itself a regenerable cache per ADR-005 as reversed by
   C1/weft-d822a7de2d, so dropping derived rows is consistent.)

### Resume idempotency is unaffected (in fact stronger)

A content-keyed id is deterministic on the finding's content, so a `--resume`
re-walk (same inputs) regenerates the same id and upserts — exactly as the
run-scoped id did under the same-run_id resume path, now independent of run_id.

## Alternatives Considered

- **Keep run-scoped ids + a stale-finding sweep that deletes prior runs'
  findings on commit.** Rejected as the primary fix: it would still orphan
  lifecycle (the new run's rows start fresh) and is strictly more machinery than
  making the id content-keyed. A prior-index-style sweep for findings whose code
  was *fixed* (no longer reproduce) remains a useful **follow-on** (mirrors
  entity diffing) and is tracked under clarion-772ff358da.
- **Change filigree dedup.** Not applicable: Loomweave's finding id is **never
  sent on the wire** — the emit payload is path/rule_id/message/severity/line +
  metadata, and Filigree computes its own fingerprint and assigns its own
  `clarion-sf-*` id. The id scheme is purely Loomweave-internal.

## Consequences

### Positive
- The `findings` table no longer accumulates duplicates across re-analyses.
- A finding's `filigree_issue_id` and suppression `status` now survive
  re-analysis instead of being orphaned every run.

### Negative / accepted trade-off
- **Findings are current-state, not a per-run append-log.** The prior (commented)
  intent of keeping each run's findings as distinct rows is reversed. Per-run
  finding history is available via the `runs` table + emit records, not by
  multiple rows per finding.

### Neutral
- `run_id` remains a column on `findings` (last-seen run), still NOT NULL, still
  the key `findings_for_emit` filters on.

## Related Decisions

- [ADR-005](./ADR-005-loomweave-dir-tracking.md) (as reversed by C1) — the store
  is a regenerable cache, which is why migration `0010` may drop derived findings.
- [ADR-011](./ADR-011-writer-actor-concurrency.md) — `write_finding_row` runs on
  the writer actor; the upsert semantics live there.

## References

- clarion-772ff358da — the L1 implementation issue (Part A here; Part B = a
  project-wide finding browser / `has_findings` filter).
- weft-f506e5f845 — the Weft dogfood-#2 residual-tail campaign issue.
