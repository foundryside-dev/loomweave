# Implementation Archive

This folder is the consolidated archive of Clarion's implementation and planning history. It is **not** part of the release-facing doc surface — readers entering via [`docs/README.md`](../README.md) and the [Clarion v0.1 docset](../clarion/v0.1/README.md) are not expected to need anything here.

Material is kept rather than deleted because the [ADRs](../clarion/adr/README.md) cite it for historical context (panel reviews, the v0.1 scope-commitment memo, sprint plans, and agent handoffs that motivated specific decisions).

## Layout

| Path | Contents |
|---|---|
| [v0.1-plan.md](./v0.1-plan.md) | High-level implementation plan: 11 work packages in dependency order, with anchoring docs/ADRs, exit criteria, and post-implementation cost-model validation. |
| [v0.1-scope-plans/](./v0.1-scope-plans/) | The v0.1 scope-commitment memo — *what* v0.1 ships, decision priorities, locked-in defaults. Cited by several ADRs. |
| [v0.1-reviews/](./v0.1-reviews/) | Pre-restructure design review, integration reconnaissance, and the April 2026 review-panel outputs (executive synthesis, self-sufficiency, threat model, doctrine synthesis). |
| [v0.1-publish/](./v0.1-publish/) | v1.0 publish-track work-stream plans (secret-scanner WS-A, pre-publish blockers). |
| [sprint-1/](./sprint-1/) | Sprint 1 (walking skeleton): WP1+WP2+WP3 execution plans and sign-off ladder. |
| [sprint-2/](./sprint-2/) | Sprint 2 (B-track): B.2–B.6 execution plans, gate results, B.8 scale test, openrouter swap, scope amendment, sign-offs. |
| [sprint-3/](./sprint-3/) | Sprint 3 scope amendment. |
| [handoffs/](./handoffs/) | Dated agent-to-agent handoff notes (formerly `docs/superpowers/handoffs/`). |
| [agent-plans/](./agent-plans/) | TDD-grain plan files used by individual agent runs (formerly `docs/superpowers/plans/`). |
| [arch-analysis-2026-05-18-1244/](./arch-analysis-2026-05-18-1244/) | One-shot architecture-archaeology output: discovery findings, subsystem catalogue, diagrams, final report. |

## Relationship to release-facing docs

- **Authoritative design**: [`../clarion/v0.1/system-design.md`](../clarion/v0.1/system-design.md) and [`../clarion/v0.1/detailed-design.md`](../clarion/v0.1/detailed-design.md). Each work package under this folder names the sections it implements.
- **Decisions**: [`../clarion/adr/README.md`](../clarion/adr/README.md). Each work package names the accepted ADRs it depends on and any backlog ADRs it is expected to surface.
- **Scope and commitments**: [`v0.1-scope-plans/v0.1-scope-commitments.md`](./v0.1-scope-plans/v0.1-scope-commitments.md). That memo locks *what* v0.1 ships; the work-package plans describe *how* the build proceeds.

## Conventions

- Documents under this folder are **immutable historical record**, not living plans. Update them only to correct factual errors or to repair a citation; do not retrofit narrative to match later decisions.
- Filigree (not these files) is the authoritative state-of-work tracker. Work-package plans seeded the issue list; the tracker is canonical thereafter.
- TDD-grain task breakdowns belonged in per-run agent plans (now under [agent-plans/](./agent-plans/)) and in Filigree, not in the high-level work-package documents.
