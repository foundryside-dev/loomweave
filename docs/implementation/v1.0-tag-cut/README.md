# Clarion v1.0.0 — Tag-Cut Readiness

**Status**: RC1 hardening — `v1.0.0` tag held pending closure of the gap register.

This directory holds the canonical pre-tag-cut artifacts for Clarion v1.0.0.
It supersedes `docs/implementation/v0.1-publish/` (which was renamed in intent
when the v0.1 → v1.0 rebrand landed but never moved on disk) as the current
program-of-work surface for the tag.

## Documents

| File | Purpose |
|------|---------|
| [`gap-register.md`](gap-register.md) | Single source of truth for every gap between the current RC1 commit and a defensible `v1.0.0` tag. 24 gaps in 7 categories with evidence, fix, and effort. |
| [`execution-plan.md`](execution-plan.md) | Day-by-day sequenced execution plan, with parallel-execution markers, operator-vs-engineering split, and the exit criteria for each day. |
| [`filigree-issue-bodies.md`](filigree-issue-bodies.md) | Pre-drafted bodies for the Filigree issues that track each gap. Reference for issue creation; the live issues are authoritative once created. |

## Origin

The gap register is the integrated output of seven review passes:

1. The 2026-05-20 RC1 architecture archaeology at
   [`../arch-analysis-2026-05-20-2124/`](../arch-analysis-2026-05-20-2124/),
   which originated risks R1–R6.
2. Six parallel subagent deep-dives executed 2026-05-22:
   - Architecture critique of the five flagged blast-radius files.
   - Test/quality coverage and E2E gate audit.
   - Security threat review of the federation HTTP API and WP5 secret scanner.
   - CI/CD pipeline review of release governance and tag safety.
   - Embedded-database (SQLite) discipline review against the 13-sheet
     reference set.
   - Documentation/contract drift audit against ADR precedence.

The arch-analysis snapshot is preserved as the prior baseline; this gap
register is the operational supersession.

## Reading order

For tag-cut decisions: `gap-register.md` → `execution-plan.md`.

For Filigree issue creation: `filigree-issue-bodies.md`.

For why a specific gap was raised: the gap register cites the originating
review file:line.
