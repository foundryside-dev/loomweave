# Clarion v0.1 Docset

This folder is the canonical Clarion v0.1 document set.

## Canonical design docs

1. [requirements.md](./requirements.md) — the *what*: requirements, constraints, and non-goals.
2. [system-design.md](./system-design.md) — the *how*: architecture, mechanisms, and integration posture.
3. [detailed-design.md](./detailed-design.md) — implementation detail, exact schemas, rule catalogs, and appendices.

## Supporting docs

- [../adr/README.md](../adr/README.md) — authored ADRs and remaining decision backlog.
- [../../implementation/README.md](../../implementation/README.md) — archived planning and review history (scope commitments, panel reviews, sprint plans, agent handoffs). Non-normative; supporting context only.

## Reading order

- New reader: [../../suite/briefing.md](../../suite/briefing.md) -> [../../suite/loom.md](../../suite/loom.md) -> [requirements.md](./requirements.md) -> [system-design.md](./system-design.md)
- Design reviewer (evaluating completeness, not yet implementing): new-reader path, then [detailed-design.md](./detailed-design.md) and [../adr/README.md](../adr/README.md).
- Implementation work: [requirements.md](./requirements.md) -> [system-design.md](./system-design.md) -> [detailed-design.md](./detailed-design.md) -> [../adr/README.md](../adr/README.md)

## Document roles

- `requirements.md`, `system-design.md`, and `detailed-design.md` are the authoritative layered design set.
- Reviews and planning memos under [../../implementation/](../../implementation/) are supporting context, not normative sources.
