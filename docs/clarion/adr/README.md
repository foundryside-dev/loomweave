# Clarion ADR Index

This folder is the canonical home for authored Clarion architecture decision records.

## Authored ADRs

| ADR | Title | Status |
|---|---|---|
| [ADR-001](./ADR-001-rust-for-core.md) | Rust for the core | Accepted |
| [ADR-002](./ADR-002-plugin-transport-json-rpc.md) | Plugin transport: Content-Length framed JSON-RPC subprocess | Accepted |
| [ADR-003](./ADR-003-entity-id-scheme.md) | Entity ID scheme: symbolic canonical names | Accepted |
| [ADR-004](./ADR-004-finding-exchange-format.md) | Finding-exchange format: Filigree-native intake | Accepted |
| [ADR-005](./ADR-005-clarion-dir-tracking.md) | `.clarion/` git-committable by default; DB included, run logs excluded | Accepted |
| [ADR-006](./ADR-006-clustering-algorithm.md) | Clustering algorithm — Leiden on imports+calls subgraph; Louvain fallback | Accepted |
| [ADR-007](./ADR-007-summary-cache-key.md) | Summary cache key — 5-part composite with TTL backstop and churn-eager invalidation | Accepted |
| [ADR-011](./ADR-011-writer-actor-concurrency.md) | Writer-actor concurrency with per-N-files transactions; `--shadow-db` opt-in | Accepted |
| [ADR-012](./ADR-012-http-auth-default.md) | HTTP read-API authentication — UDS default with token fallback | Accepted |
| [ADR-013](./ADR-013-pre-ingest-secret-scanner.md) | Pre-ingest secret scanner with LLM-dispatch block | Accepted |
| [ADR-014](./ADR-014-filigree-registry-backend.md) | Filigree `registry_backend` flag and pluggable `RegistryProtocol` | Accepted |
| [ADR-015](./ADR-015-wardline-filigree-emission.md) | Wardline→Filigree emission ownership — Clarion-side SARIF translator (v0.1), native Wardline emitter (v0.2) | Accepted |
| [ADR-016](./ADR-016-observation-transport.md) | Observation transport — MCP-spawn (v0.1), Filigree HTTP endpoint (v0.2) | Accepted |
| [ADR-017](./ADR-017-severity-and-dedup.md) | Severity mapping, rule-ID round-trip, and dedup policy | Accepted |
| [ADR-018](./ADR-018-identity-reconciliation.md) | Identity reconciliation — Clarion translates; Wardline owns its qualnames; direct REGISTRY import with version pinning | Accepted |
| [ADR-021](./ADR-021-plugin-authority-hybrid.md) | Plugin authority model: hybrid (declared capabilities + core-enforced minimums) | Accepted |
| [ADR-022](./ADR-022-core-plugin-ontology.md) | Core/plugin ontology ownership boundary | Accepted |
| [ADR-023](./ADR-023-tooling-baseline.md) | Rust + Python tooling baseline (edition 2024, pedantic, cargo-deny, nextest, CI; ruff + mypy-strict + pre-commit) | Accepted |
| [ADR-024](./ADR-024-guidance-schema-vocabulary.md) | Guidance schema vocabulary rename (priority→scope_level/scope_rank; critical→pinned; source→provenance) and in-place migration policy | Accepted |
| [ADR-025](./ADR-025-minor-shared-standards.md) | Minor shared standards — registry of small project-wide conventions; first entry MSS-1 locks the `tier:*` filigree label namespace | Accepted |
| [ADR-026](./ADR-026-containment-wire-and-edge-identity.md) | Containment wire shape and edge identity (top-level `edges` field; drop `edges.id` column; per-kind `source_byte_start/end` contract) | Accepted |
| [ADR-027](./ADR-027-ontology-version-semver.md) | Ontology version semver policy (MAJOR/MINOR/PATCH semantics for `[ontology].ontology_version`; clarifies ADR-022) | Accepted |
| [ADR-028](./ADR-028-edge-confidence-tiers.md) | Edge confidence tiers (`resolved` / `ambiguous` / `inferred`); MCP queries default to `>= resolved`; inferred edges lazy-computed at query time | Accepted |
| [ADR-029](./ADR-029-entity-associations-binding.md) | Entity associations — Filigree-side `entity_associations` table; `add_entity_association` MCP tool on Filigree; `issues_for` MCP tool on Clarion; WP9 split into A (binding, v0.1) and B (findings emission) | Accepted |
| [ADR-030](./ADR-030-on-demand-summary-scope.md) | On-demand summary scope — narrows WP6 to MCP-driven `summary(id)`; 5-tuple cache key unchanged; module/subsystem aggregation deferred to v0.2 | Accepted |

## Backlog still tracked in the detailed design

The following decisions are still backlog items rather than authored ADR files. Their current summaries live in [../v0.1/detailed-design.md](../v0.1/detailed-design.md) §11 and [../v0.1/system-design.md](../v0.1/system-design.md) §12.

| ADR | Title | Current state |
|---|---|---|
| ADR-008 | Filigree file-registry displacement as breaking change | Superseded by ADR-014 |
| ADR-009 | Structured briefings vs free-form prose | Backlog |
| ADR-010 | MCP as first-class surface | Backlog |
| ADR-019 | SARIF property-bag preservation | Backlog |
| ADR-020 | Degraded-mode policy and explicit suite fallbacks | Backlog |

## Pre-implementation scope commitments

The priorities and scope implied by these ADRs are committed in [../v0.1/plans/v0.1-scope-commitments.md](../v0.1/plans/v0.1-scope-commitments.md). The ADR authoring sprint is staged against that memo.

## ADR acceptance criteria — Loom vocabulary discipline

ADRs introducing cross-product-visible field names must update [`docs/suite/glossary.md`](../../suite/glossary.md) before moving from Proposed to Accepted, with one of three explicit verdicts:

- **`no clash`** — the term is unique to this product, no sibling currently uses it
- **`managed clash`** — a sibling uses the same term; an explicit mapping table exists in the ADR (model: [ADR-017](./ADR-017-severity-and-dedup.md))
- **`renamed`** — the proposed term clashed with a sibling; this ADR renames the local term to avoid the clash

The verdict is part of acceptance evidence, not a courtesy. Three of v0.1's clashes (`severity`, `rule_id`, `finding` wire shape) shipped clean because they got managing ADRs at design time; three did not (`priority`, `critical`, `source`) and required retrofit via ADR-024. The rule converts the next clash from "discovered during implementation" to "blocked at design review." See `glossary.md` for federation-safety constraints — the glossary is a human-consulted design-review artifact, not infrastructure.
