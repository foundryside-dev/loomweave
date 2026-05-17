# Clarion Sprint 2 — Sign-off Ladder

**Status**: CLOSED — RED partial milestone
**Scope**: B.3, B.4*, B.5*, B.6, B.7, B.8 from
[`scope-amendment-2026-05.md`](./scope-amendment-2026-05.md)
**Read-with**: [`b8-results.md`](./b8-results.md), [`../sprint-1/signoffs.md`](../sprint-1/signoffs.md)

This document closes Sprint 2 as a measured reference point. Because B.8 returned
RED, the sprint does **not** certify the full v0.1 MVP MCP surface as ready.
It certifies the useful measured subset and records the scope reduction needed
before v0.1 can claim the full surface.

Each tick below carries a verifiable artifact: a commit hash, panel record,
result memo, or Filigree issue closeout.

---

## Tier A — Sprint 2 Close

Every work-package below is closed for Sprint 2 accounting. B.8 is closed with a
red gate verdict and a v0.2 repair issue, not with MVP-ready approval.

### A.1 B.3 — Contains Edges

- [x] **Design doc commit**: `5c510f1` (`docs(wp3): B.3 design — contains edges (first edge kind)`).
- [x] **Implementation range**: `ba9d178` → `50503be` (schema PK cleanup, edge writer command, RawEdge plumbing, contains emission, ontology v0.3.0, parity/e2e/stat fixes).
- [x] **Panel record**: [`b3-contains-edges.md`](./b3-contains-edges.md) design record and incorporated amendments.
- [x] **Exit criteria attestation**: Filigree `clarion-39bc17bde8` closed `done`; close reason records contains-edge row, `edges_inserted == 1`, and `dropped_edges_total == 0` in walking-skeleton verification.

### A.2 B.4* — Calls Edges And Confidence Tiers

- [x] **Design doc commit**: `1a112af` (`docs(sprint-2): B.4* design — calls edges via pyright + confidence tiers`).
- [x] **Implementation range**: `e197894` → `4f1197e` (confidence column/index, contract enforcement, pyright session, call resolver, ontology v0.4.0, parity/e2e, gate freshness, B.8 rollback pre-write).
- [x] **Panel record**: [`b4-calls-edges.md` §11](./b4-calls-edges.md#11-panel-review-record).
- [x] **Exit criteria attestation**: Filigree `clarion-2d2d1d27b5` closed `done`; close note records ADR-023 gates green and B.4* week-2 gate `GREEN` in [`b4-gate-results.md`](./b4-gate-results.md).

### A.3 B.5* — References Edges

- [x] **Design doc commit**: `95c9a5e` (`docs(wp3): design B.5 references edges via pyright`).
- [x] **Implementation range**: `6226543` → `3ed6c89` (references contract, lexical-owner collection, pyright definitions, stats, public surface, review-gap fixes).
- [x] **Panel record**: [`b5-references-edges.md`](./b5-references-edges.md) panel-reviewed design and review follow-up notes.
- [x] **Exit criteria attestation**: Filigree `clarion-b0cedfd2bb` closed `done`; close reason names commit `3ed6c89` and the GREEN scale-smoke artifact.

### A.4 B.6 — Seven-Tool MCP Surface

- [x] **Design doc commit**: `6a9a7b2` (`docs(wp8): design B.6 MCP surface`).
- [x] **Implementation range**: `b0a12a6` → `a53d2e4` (MCP stdio server, `clarion serve`, storage-backed tools, summary cache, inferred dispatch, `issues_for`, e2e observability, OpenRouter provider swap).
- [x] **Panel record**: [`b6-mcp-surface.md`](./b6-mcp-surface.md) Stage 0 panel record and reconciliation.
- [x] **Exit criteria attestation**: Filigree `clarion-e2a3672cc9` closed `done`; B.6 local gates passed with RecordingProvider coverage, not live-provider proof.

### A.5 B.7 — Entity Associations Binding

- [x] **Design source**: [ADR-029](../../clarion/adr/ADR-029-entity-associations-binding.md) and B.6 `issues_for` integration design.
- [x] **Implementation artifacts**: Filigree PR 42 merged; Clarion-side `issues_for` integration commits include `16634ae` and `29d3865`.
- [x] **Panel record**: ADR-029 federation audit and B.6 Filigree reverse-route review.
- [x] **Exit criteria attestation**: Filigree `clarion-73ab0da435` closed `done`; B.8 measured the real reverse route at p95 3.262 ms.

### A.6 B.8 — Elspeth Scale-Test

- [x] **Test-plan commit**: `5a396a5` (`test(perf): add B.8 scale-test plan and harness`).
- [x] **Harness correction commit**: `80a6af9` (`test(perf): mark B.8 heavy samples steady-state`).
- [x] **Reviewer panel record**: [`b8-elspeth-scale-test.md` §7](./b8-elspeth-scale-test.md#7-reviewer-panel-record).
- [x] **Result memo commit**: `ad2ef80` (`docs(sprint-2): record B.8 red scale-test results`).
- [x] **Exit criteria attestation**: [`b8-results.md`](./b8-results.md) records RED. Analyze passed NFR-PERF-01 and storage-backed navigation passed, but live `summary()` and inferred dispatch failed 100% with `llm-invalid-json`.
- [x] **Rollback action**: selected B.8 playbook red option 4; Sprint 2 closes as a partial milestone and defers LLM-backed summary/inferred proof to follow-up `clarion-ac5f9bf35b`.

---

## Sprint 2 Close Verdict

**Gate label**: RED.

**What is signed off**:

- elspeth-slice analyze completed in 7m27s with 26,813 entities and 45,369 edges.
- Storage-backed MCP navigation was operational and low-latency at scale.
- `issues_for` used the real Filigree reverse route and returned matched data.
- The B.8 harness and raw measurements are committed and reusable.

**What is not signed off**:

- MVP-ready seven-tool live-provider surface.
- Summary-cache hit-rate validation.
- Inferred-edge LLM dispatch validation.
- Operator-facing OpenRouter token/cost validation.

**Scope reduction**: live OpenRouter-backed `summary()` correctness, inferred-edge
JSON-contract reliability, and cost/cache validation slip out of Sprint 2. They
are tracked by `clarion-ac5f9bf35b` and must be repaired before claiming v0.1
MVP readiness.

---

## Tracker State

Sprint-2 umbrella issues verified at close:

| Issue | Work package | Status |
|---|---|---|
| `clarion-39bc17bde8` | B.3 | `done` |
| `clarion-2d2d1d27b5` | B.4* | `done` |
| `clarion-b0cedfd2bb` | B.5* | `done` |
| `clarion-e2a3672cc9` | B.6 | `done` |
| `clarion-73ab0da435` | B.7 | `done` |
| `clarion-6222134e0d` | B.8 | closes with RED verdict |

Non-umbrella Sprint-2 audit/follow-up issues remain open by design; they are not
part of this close ladder.

---

## Tag

The closing tag is `v0.1-sprint-2`:

```bash
git tag v0.1-sprint-2 -m "Sprint 2 close — MVP MCP surface against elspeth scale-test"
```

The tag marks a red partial milestone. It is referenceable for future planning,
but it must not be described as an MVP-ready release tag.
