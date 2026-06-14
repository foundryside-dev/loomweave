# ADR-048: Stale-finding sweep (prior-index-style finding diffing)

**Status**: Accepted
**Date**: 2026-06-08
**Deciders**: john@pgpl.net
**Context**: ADR-047 made finding ids content-keyed so re-analyses de-dupe instead
of accumulating, and explicitly deferred "a prior-index-style sweep for findings
whose code was *fixed* (no longer reproduce)" as a follow-on (clarion-87c1eba2bd,
split from clarion-772ff358da).

## Summary

After a **clean full analyze**, Loomweave retires findings the run no longer
reproduces: it `DELETE`s every `open`, Filigree-unlinked finding whose `run_id`
is not the current run. This mirrors the entity prior-index diff
(`prior_index.rs`) for findings, reusing the `run_id` signal ADR-047 already
established. Lifecycle is preserved: a finding carrying a `filigree_issue_id` or a
non-`open` status (`acknowledged` / `suppressed` / `promoted_to_issue`) is an
operator decision and is never swept locally. The sweep is gated to a run that
walked everything (`Completed`, non-`--resume`, `skipped_files == 0`,
non-`--no-sei`).

## Context

ADR-047 left the `findings` table **current-state, not an append-log**: the
content-keyed upsert (`write_finding_row`) refreshes a *reproduced* finding's
`run_id` to the current run, but nothing retires a finding a later run stopped
emitting. Two mechanics make such findings linger forever:

- `entities` is **cumulative / never-pruned** (`prior_index.rs`, `cache.rs` —
  REQ-ANALYZE-04). A vanished entity's row survives, so the
  `findings.entity_id … ON DELETE CASCADE` (migration 0001) **never fires** on
  re-analyze. A finding on *deleted* code is not cascaded away.
- A finding on code that still exists but was *fixed* is not re-emitted, so its
  row simply stays at its prior `run_id`.

The whole-project finding count (`project_status` / `project_finding_list`)
therefore only ever grows; an agent browsing findings sees defects that no longer
exist. Entities already get prior-index diffing; findings did not.

ADR-047 §Decision.3 established the diff signal we reuse: `findings_for_emit`
filters `WHERE run_id = ?1`, a **reproduced** finding carries the current run_id
(the upsert set it), and a finding that **did not reproduce** keeps its prior
run_id. "Stale" is exactly `run_id <> current` — *provided the current run
actually re-walked that finding's file*.

## Decision

1. **Add a storage sweep** `findings::sweep_stale_findings(conn, current_run_id)`:
   ```sql
   DELETE FROM findings
   WHERE status = 'open'
     AND filigree_issue_id IS NULL
     AND run_id <> :current_run_id;
   ```
   Returns the row count. Wired as `WriterCmd::SweepStaleFindings` through the
   query-time-write path (ADR-011), post-`CommitRun`, best-effort + enrich-only —
   a failure logs and never un-commits the graph.

2. **Lifecycle preservation.** `status = 'open'` and `filigree_issue_id IS NULL`
   are the only rows touched. Acknowledged / suppressed / promoted findings and
   anything linked to a Filigree issue are operator decisions, left to the
   Filigree-side lifecycle (§Cross-product boundary).

3. **Gate the sweep to a clean full pass** at the call site (`analyze.rs`,
   `RunOutcome::Completed` arm, after every finding-emitting pass):
   - **`!resume`** — `--resume` REUSES the prior run_id, so a resumed run's
     not-yet-re-emitted findings already match `current`; the run_id signal can't
     distinguish them. (Also satisfies the acceptance criterion "a `--resume`
     re-walk does not retire findings the resumed run hasn't re-emitted yet" for
     free.)
   - **`skipped_files == 0`** — an incremental run leaves *unchanged* files'
     findings at their prior run_id (they were not re-walked, so not re-emitted).
     Sweeping them would wrongly retire still-reproducing findings. A full pass
     walks every file, so `run_id <> current` is unambiguous.
   - **`source_walk_skipped_entries == 0`** — a file or directory that *errored*
     during the source walk (IO / permission / path-jail) was never read, yet the
     run still reaches `Completed`. Its findings keep a prior run_id without being
     re-examined, so without this guard a single walk error would retire a whole
     unwalked subtree's still-reproducing findings. ("Never looked" must not be
     conflated with "looked, code is fixed.")
   - **`!no_sei`** — the SEI mint pass produces the `entity-deleted` and
     `guidance-orphan` facts; `--no-sei` skips it, so those findings are not
     refreshed this run and must not be mistaken for vanished.

4. **Placement: last in the `Completed` arm.** The sweep runs *after* every
   during-run `InsertFinding` and every post-commit `PersistPostRunFinding` pass
   (SEI deletion, tier-subsystem, guidance-staleness), so every finding the run
   reproduces already carries `current` before the diff runs.

## Cross-product boundary (Loomweave ↔ Filigree)

The local sweep and the existing Filigree retention path are **disjoint by
construction**:

| Finding | Owner | Retirement mechanism |
|---|---|---|
| `filigree_issue_id IS NULL`, `status = 'open'` | Loomweave (local) | this sweep (`DELETE`) |
| `filigree_issue_id IS NOT NULL` *or* non-`open` | Filigree lifecycle | `mark_unseen` per rule/file at emit + age-gated `--prune-unseen` soft-archive (default 30d) |

The sweep predicate (`filigree_issue_id IS NULL`) never overlaps the Filigree-owned
set, so a finding cannot be retired by both paths or fall between them. This
honours the cross-product identity contract (ADR-029: Filigree keys by entity;
findings carry `filigree_issue_id`) — the local id is never on the wire (ADR-047),
so deleting a local row tells Filigree nothing and breaks no linkage.

## Alternatives considered

- **Bump skipped-file findings' `run_id` to current, then `DELETE WHERE run_id <>
  current` unconditionally (no skip gate).** Rejected: `findings_for_emit` keys
  on `WHERE run_id = ?1`, so bumping a skipped finding's run_id changes the
  Filigree emit set and breaks ADR-047's "emit is identical to today" invariant.
  Keeping `run_id` strictly meaning "last run that re-walked-and-reproduced this
  finding" preserves that contract.
- **File-scope the `DELETE`** (exclude `entity_id IN (entities WHERE
  source_file_path IN :skipped_set)`) so the sweep can also run on incremental
  runs. Rejected for now: more machinery, and path-less synthetic findings
  (weak-modularity, tier facts) can't be file-scoped. The acceptance criteria do
  not require real-time incremental pruning; the full-run gate is correct by
  construction. Revisit only if incremental pruning becomes a requirement.

## Consequences

### Positive
- The whole-project finding count **drops** to reflect fixed/removed code, not
  just grows (closes the third acceptance criterion).
- Findings now match the rest of the store's current-state model (ADR-047).

### Negative / accepted trade-off
- **Stale findings linger until the next *clean full* analyze.** An incremental,
  resumed, or `--no-sei` run does not retire them. Accepted: findings are
  regenerable derived data; a `--no-incremental` run (or any full pass) settles
  the table.

### Neutral
- `run_id` keeps its ADR-047 meaning (last run that reproduced the finding); the
  sweep reads it, never writes it.

## Related decisions

- [ADR-047](./ADR-047-content-keyed-finding-ids.md) — content-keyed finding ids;
  established the `run_id` diff signal and deferred this sweep.
- [ADR-029](./ADR-029-entity-associations-binding.md) — Filigree keys findings by
  entity; the local id is never on the wire.
- [ADR-011](./ADR-011-writer-actor-concurrency.md) — the sweep is a writer-actor
  query-time write.
- [ADR-005](./ADR-005-loomweave-dir-tracking.md) (as reversed by C1) — the store
  is a regenerable cache, which is why deleting derived findings is sound.

## References

- clarion-87c1eba2bd — the implementation issue (Part A of clarion-772ff358da's
  deferred follow-on).
- clarion-772ff358da / weft-f506e5f845 — the Weft dogfood-#2 finding-accumulation
  campaign that surfaced both halves.
