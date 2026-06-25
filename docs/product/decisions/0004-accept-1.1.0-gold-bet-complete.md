# PDR-0004: Accept the 1.1.0 / Rust-plugin-gold bet as complete

- **Date:** 2026-06-24
- **Status:** accepted (ACCEPT against PDR-0002's criteria)
- **Relates to:** PDR-0002 (gold-gates-1.1.0) — its gate is now satisfied

## Context

PDR-0002 gated the 1.1.0 cut on fixing four entity-ID collision families
(self-type-path, trait-path, `#[path]`-module, `const _`) and recording a gold
verdict, with a 2026-06-30 reversal trigger. The 2026-06-11 `current-state.md`
recorded this bet as in-flight. At this session's RESUME, reality had moved 13
days and 112 commits ahead — the bet was never checkpointed as done.

## What was observed (git, this session)

- All four collision families fixed: ADR-049 Amendments 6+7 (`c4791aa`,
  self-type + trait-path), Amendment 8 (`05b44f3`, `#[path]`-module), Amendment
  9 (`f7f8a69`, `const _`), plus a `LMWV-DUPLICATE-LOCATOR` runtime guardrail
  (`be0e780`).
- 1.1.0 GA cut via PR #57 (`a97e1d8`); 1.2.0 / 1.2.1 / 1.3.0 / 1.3.1 shipped on
  top. Workspace version is now `1.3.1`.

## The call

**Accept the bet as complete.** The PDR-0002 gate is satisfied; the
north-star reading it gated moved from 4 open collision families to **0** (see
`metrics.md`, dated 2026-06-24). The 2026-06-30 reversal trigger was never
needed and is now moot. The bet leaves "in flight" and is banked as accepted.

## Reversal trigger

If a regression re-opens any collision family (the `LMWV-DUPLICATE-LOCATOR`
runtime alarm or the adversarial QA sweep surfaces a new collision on the
reference corpora), re-open identity-correctness as a fresh bet — but the 1.1.0
acceptance itself stands; identity correctness is now a standing guardrail, not
an open bet.
