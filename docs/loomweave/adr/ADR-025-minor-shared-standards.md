# ADR-025: Minor Shared Standards

**Status**: Accepted
**Date**: 2026-05-05
**Deciders**: qacona@gmail.com
**Context**: small project-wide conventions keep accumulating that don't individually justify a full ADR but do need a discoverable record so they aren't reinvented at the next decision point. Without a home, they live in a single contributor's head and surface inconsistently across sprints.

## Summary

ADR-025 is the canonical home for small, single-axis project conventions that:

1. Are workspace-wide (apply to all sprints, all WPs, all contributors).
2. Are individually too small to warrant a dedicated ADR.
3. Need to be locked rather than rediscovered each time the surface comes up.

Each entry below is normative. Adding an entry to ADR-025 requires the same discipline as adding an Accepted ADR — write the entry, name a date, name a deciding rationale. ADR-025 is not a junk drawer; entries that grow in scope past "single-axis convention" should be promoted to a dedicated ADR.

## Conventions registered under ADR-025

### MSS-1 — Filigree label namespaces beyond the kickoff baseline

**Date registered**: 2026-05-05
**Trigger**: B.2 design review (`docs/implementation/sprint-2/b2-class-module-entities.md`) added a `tier:b` label that did not exist in the prior taxonomy.

**Convention**: when a new filigree label namespace is introduced during sprint work and looks reusable (i.e. `tier:c`, `tier:d`, etc. would naturally follow), the namespace becomes a project standard at the moment of first use. The label `tier:b` (Sprint-2 Tier B work, the WP3 feature-complete subset) is the first such namespace registered here.

**Established namespaces**:

| Namespace | Meaning | Source |
|---|---|---|
| `release:vX.Y` | Target release version | Sprint-1 kickoff |
| `sprint:N` | Sprint number | Sprint-1 kickoff |
| `wp:N` | Work-package number (1–11) | Sprint-1 kickoff |
| `adr:NNN` | Issue cites or implements ADR-NNN | Sprint-1 kickoff |
| `tier:X` | Sprint-internal work tier (e.g., `tier:b` = Sprint-2 WP3 feature-complete subset) | This ADR (MSS-1) |
| `from-observation` | Promoted from a filigree observation | Filigree built-in |

**Why locked**: filigree labels are a mild form of public API — search queries (`filigree list --label-prefix=tier:`) bake in the spelling, and renaming after a sprint or two costs more than spelling consistently from the start. The kickoff handoff established `release:`, `sprint:`, `wp:`, `adr:` as canonical; this ADR extends the set with `tier:` and reserves the right to add more under the same trigger (first use that looks reusable).

**How to add a new namespace**: append a row to the table above with the same one-commit edit that introduces the label. Don't invent ad-hoc namespaces in issue creation without recording them here — the next sprint's contributor will replicate the spelling exactly only if they can find it.

## What does NOT belong in ADR-025

- Decisions that have a clear architectural impact (those get a dedicated ADR, e.g., ADR-022, ADR-024).
- Tooling baselines (those go in ADR-023, the tooling-baseline ADR).
- Cross-product schema or vocabulary (those go in ADR-017 / ADR-024 / `docs/suite/glossary.md`).
- Sprint-specific conventions that don't outlive their sprint (those go in the sprint kickoff handoff).

If an entry in ADR-025 grows past one trigger / one rationale / one table, it should be promoted to a dedicated ADR and the entry replaced with a pointer.

## Consequences

### Positive

- Small standards have a discoverable home; the next contributor (or next-me) doesn't reinvent them.
- The "promote to dedicated ADR" rule keeps ADR-025 from drifting into a junk drawer.
- Sprint kickoff handoffs stay focused on sprint-specific scope rather than carrying forward project-wide conventions.

### Negative

- One more ADR to scan during design review. Mitigation: ADR-025 is short; the index in `README.md` links to it directly.
- Risk that contributors avoid the higher-rigor ADR path by stuffing weak decisions into MSS entries. Mitigation: each entry must name a trigger and a rationale, the same as any ADR.

### Neutral

- ADR-025 is mutable in a way most ADRs are not — entries can be appended without superseding the ADR. Modifying an existing entry, however, follows the same supersession rule as any Accepted ADR: write a new entry that supersedes the old one; don't edit history.

## Related Decisions

- [ADR-023](./ADR-023-tooling-baseline.md) — workspace tooling baseline. ADR-025 is the process-side complement: ADR-023 governs how code looks, ADR-025 governs how work is tracked and labelled.
- [ADR-024](./ADR-024-guidance-schema-vocabulary.md) — vocabulary discipline at the schema layer. ADR-025 is its lighter-weight cousin at the process-tooling layer.
- Project `CLAUDE.md` and root `~/CLAUDE.md` — describe the filigree workflow but don't lock label spellings; that's ADR-025's job.

## References

- B.2 design doc — [`docs/implementation/sprint-2/b2-class-module-entities.md`](../../implementation/sprint-2/b2-class-module-entities.md) §8 (the `tier:b` label introduction that triggered MSS-1).
- Filigree umbrella issue clarion-daa9b13ce2 (the first issue carrying `tier:b`).
