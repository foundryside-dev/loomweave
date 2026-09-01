# Implementation Archive

This folder is the consolidated archive of Loomweave's implementation and planning history. It is **not** part of the release-facing doc surface — readers entering via [`docs/README.md`](../README.md) and the [Loomweave 1.0 docset](../loomweave/1.0/README.md) are not expected to need anything here.

Material is kept rather than deleted because the [ADRs](../loomweave/adr/README.md) cite it for historical context (panel reviews, the v0.1 scope-commitment memo, sprint plans, and agent handoffs that motivated specific decisions).

## Layout

| Path | Contents |
|---|---|
| [v0.1-plan.md](./v0.1-plan.md) | High-level implementation plan: 11 work packages in dependency order, with anchoring docs/ADRs, exit criteria, and post-implementation cost-model validation. |
| [v0.1-scope-plans/](./v0.1-scope-plans/) | The v0.1 scope-commitment memo — *what* v0.1 ships, decision priorities, locked-in defaults. Retained for historical context. |
| [v0.1-reviews/](./v0.1-reviews/) | Pre-restructure design review, integration reconnaissance, and the April 2026 review-panel outputs (executive synthesis, self-sufficiency, threat model, doctrine synthesis). |
| [v0.1-publish/](./v0.1-publish/) | v1.0 publish-track work-stream plans (secret-scanner WS-A, pre-publish blockers). |
| [sprint-1/](./sprint-1/) | Sprint 1 (walking skeleton): WP1+WP2+WP3 execution plans and sign-off ladder. |
| [sprint-2/](./sprint-2/) | Sprint 2 (B-track): B.2–B.6 execution plans, gate results, B.8 scale test, openrouter swap, scope amendment, sign-offs. |
| [sprint-3/](./sprint-3/) | Sprint 3: [WP10 scope amendment](./sprint-3/scope-amendment-2026-05.md) and the [Weft federation hardening tasking](./sprint-3/2026-05-19-weft-federation-hardening-tasking.md). |
| [handoffs/](./handoffs/) | Dated agent-to-agent handoff notes (formerly `docs/superpowers/handoffs/`). |
| [agent-plans/](./agent-plans/) | TDD-grain plan files used by individual agent runs (formerly `docs/superpowers/plans/`). |
| [v1.0-tag-cut/](./v1.0-tag-cut/) | v1.0.0 tag-cut readiness archive: execution plan, gap register, and Filigree issue bodies. |
| [v1.0-cicd-readiness.md](./v1.0-cicd-readiness.md) | v1.0 CI/CD readiness memo. |
| [qa/](./qa/) | Rust-plugin QA memos: Sprint 3 dogfood + scale QA, Sprint 4 gold QA and its v2 addendum. |
| [2026-06-11-phase3-rust-analyzer-go-no-go.md](./2026-06-11-phase3-rust-analyzer-go-no-go.md) | Phase 3 (rust-analyzer enrichment) go / no-go memo. |
| [2026-07-10-public-surface-tags-plainweave-validation.md](./2026-07-10-public-surface-tags-plainweave-validation.md) | Validation memo for the public-surface-tags work consumed by Plainweave. |
| `../archive/arch-analysis-2026-05-20-2124/` | RC1 root-and-branch architecture archaeology output (moved to [`docs/archive/`](../archive/README.md) 2026-08-31, alongside its working notes). |
| `../archive/implementation-memos/` | Dated audits, dogfood evaluations, review sweeps, and rename/injection plans formerly at this level (moved to [`docs/archive/implementation-memos/`](../archive/README.md) 2026-08-31). |

## Relationship to release-facing docs

- **Authoritative design**: [`../loomweave/1.0/system-design.md`](../loomweave/1.0/system-design.md) and [`../loomweave/1.0/detailed-design.md`](../loomweave/1.0/detailed-design.md). Each work package under this folder names the sections it implements.
- **Decisions**: [`../loomweave/adr/README.md`](../loomweave/adr/README.md). Each work package names the accepted ADRs it depends on and any backlog ADRs it is expected to surface.
- **Scope and commitments**: [`v0.1-scope-plans/v0.1-scope-commitments.md`](./v0.1-scope-plans/v0.1-scope-commitments.md). That memo locks *what* v0.1 ships; the work-package plans describe *how* the build proceeds.

## Conventions

- Documents under this folder are **immutable historical record**, not living plans. Update them only to correct factual errors or to repair a citation; do not retrofit narrative to match later decisions.
- Filigree (not these files) is the authoritative state-of-work tracker. Work-package plans seeded the issue list; the tracker is canonical thereafter.
- TDD-grain task breakdowns belonged in per-run agent plans (now under [agent-plans/](./agent-plans/)) and in Filigree, not in the high-level work-package documents.
