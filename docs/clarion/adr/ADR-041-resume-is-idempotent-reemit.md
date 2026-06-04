# ADR-041: Analyze Resume Is Idempotent Re-Emit, Not Checkpoint Recovery

**Status**: Accepted
**Date**: 2026-06-04
**Deciders**: qacona@gmail.com
**Context**: `clarion analyze --resume RUN_ID` shipped as a run-lifecycle and
finding-emission repair path, while older design text still promised
phase/file checkpoint recovery.

## Summary

Clarion's v1.x `--resume RUN_ID` reopens the existing `runs` row, re-walks the
analysis idempotently, and emits Filigree findings with `mark_unseen=false`.
It is not a durable phase/file checkpoint recovery mechanism.

This ADR amends ADR-005 and ADR-011 where they describe `partial.json` or
restart-at-first-uncommitted-file behavior. The SQLite WAL and writer-actor
contract remains: an unclean shutdown must not corrupt `.clarion/clarion.db`,
and committed rows survive. What changes is the resume promise: after a crash,
the operator can safely re-run the same run id without Filigree seen/unseen
flapping, but Clarion does not guarantee it will skip already completed
phases/files from that interrupted run.

## Decision

`--resume RUN_ID` has three responsibilities:

1. Reopen the existing run row through `WriterCmd::ResumeRun`.
2. Re-emit deterministic graph/finding writes under the same run id.
3. Preserve Filigree lifecycle semantics by posting with `mark_unseen=false`.

Checkpoint recovery is deferred until a future ADR defines a durable
phase/file checkpoint schema, recovery protocol, and tests that prove provider
calls are not repeated after interruption. That future design must account for
plugin-session ordering, import-edge filtering across files, summary-cache
side effects, and post-commit enrich-only phases.

## Rationale

The shipped implementation already has a coherent resume contract for the
federated finding lifecycle. Reusing the same run id prevents resumed partial
runs from marking not-yet-revisited Filigree findings as unseen. Entity and
finding writes are upsert/idempotent enough for a safe re-walk.

Durable checkpoint recovery is a different feature. It requires a first-class
checkpoint table or run-file, per-phase completion markers, file-level provider
call accounting, and careful ordering for edges that need whole-plugin context.
Adding those implicitly would turn a federation lifecycle option into a hidden
scheduler subsystem.

## Consequences

- Crash recovery remains database-safe: WAL and writer transactions preserve
  committed data and prevent corruption.
- Resume may repeat structural extraction, plugin work, and provider calls
  unless existing content-hash caches independently avoid work.
- Operators must not rely on `.clarion/runs/<run_id>/partial.json` or
  `checkpoints.jsonl`; neither is part of the v1.x contract.
- Future checkpoint recovery can be added as a separate capability without
  changing Filigree's `scan_run_id` semantics.

## Verification

- Existing resume tests must assert run-row reopening and idempotent re-emit
  behavior.
- A crash-safety test may assert database integrity and safe same-run re-walk
  after interruption.
- Tests must not assert that completed phases/files are skipped unless a
  successor ADR introduces checkpoint recovery.

## Amends

- ADR-005: removes `partial.json` as a v1.x resume material.
- ADR-011: narrows `--resume` from checkpoint recovery to idempotent re-emit.
- Requirements REQ-ANALYZE-03 and NFR-RELIABILITY-01: clarifies the active
  v1.x behavior.
