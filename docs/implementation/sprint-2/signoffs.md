# Clarion Sprint 2 — Sign-off Ladder

**Status**: CLOSED — GREEN after B.8 repair rerun
**Scope**: B.3, B.4*, B.5*, B.6, B.7, B.8 from
[`scope-amendment-2026-05.md`](./scope-amendment-2026-05.md)
**Read-with**: [`b8-results.md`](./b8-results.md), [`../sprint-1/signoffs.md`](../sprint-1/signoffs.md)

This document originally closed Sprint 2 as a measured RED partial milestone at
commit `e6bba0f` / tag `v0.1-sprint-2`. The post-close B.8 repair issue
`clarion-ac5f9bf35b` is now fixed and closed. The GREEN rerun in
[`b8-results.md`](./b8-results.md#2026-05-17t2243z--green-rerun-superseding-red)
supersedes the RED verdict for live OpenRouter JSON, cost, and cache proof.
Sprint 2 now certifies the seven-tool v0.1 MVP MCP surface against the
representative elspeth-slice, with the B.4* extrapolation caveat retained as a
yellow follow-up.

Each tick below carries a verifiable artifact: a commit hash, panel record,
result memo, or Filigree issue closeout.

---

## Tier A — Sprint 2 Close

Every work-package below is closed for Sprint 2 accounting. B.8 first closed RED
and then moved to GREEN through the post-close repair issue; both checkpoints
remain part of the audit trail.

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
- [x] **Historical RED result memo commit**: `ad2ef80` (`docs(sprint-2): record B.8 red scale-test results`).
- [x] **Historical RED signoff commit**: `e6bba0f` (`docs(sprint-2): close Sprint 2 signoff ladder`), tagged `v0.1-sprint-2`.
- [x] **Repair implementation commit**: `ab6b1dd` (`fix(wp3): OpenRouter strict-JSON path for B.8 green rerun`).
- [x] **GREEN rerun attestation**: [`b8-results.md`](./b8-results.md#2026-05-17t2243z--green-rerun-superseding-red) records the cold and warm OpenRouter-backed reruns on the same analyzed DB. The cold run produced 100/100 OK MCP calls, 3 summary cache rows, 10 inferred edge cache rows, and 57 materialized inferred `calls` edges. The warm rerun produced 100/100 OK calls, all-tool p95 200.273 ms, summary cache hit rate 100%, and zero new token/cost delta.
- [x] **Repair follow-up closure**: Filigree `clarion-ac5f9bf35b` closed `closed`; close verification names the B.8 GREEN artifacts and the malformed-output regression coverage.

---

## Sprint 2 Close Verdict

**Gate label**: GREEN after repair.

**What is signed off**:

- elspeth-slice analyze completed in 7m27s with 26,813 entities and 45,369 edges.
- Storage-backed MCP navigation was operational and low-latency at scale.
- All seven MCP tools returned OK and useful results in the GREEN cold-cache rerun.
- Live OpenRouter-backed `summary()` returned strict JSON, populated the summary cache, and reached 100% warm cache hits.
- Live inferred dispatch materialized inferred `calls` edges, populated `inferred_edge_cache`, and returned zero new token/cost deltas in the warm rerun.
- `issues_for` used the real Filigree reverse route and returned matched data.
- The B.8 harness and raw cold/warm measurements are committed and reusable.

**Remaining caveats**:

- The B.4* mini-gate wall-clock extrapolation was materially optimistic: the B.8 analyze run was ~4.34x slower than the mini-gate's linear projection, even though it remained well inside NFR-PERF-01.
- This signoff certifies the representative elspeth-slice, not a full-repository elspeth proof.
- Non-umbrella Sprint-2 audit/follow-up issues remain open by design and are not part of this close ladder.

**Repair closure**: the live OpenRouter-backed `summary()` correctness,
inferred-edge JSON-contract reliability, and cost/cache validation gaps from the
RED close are repaired by `clarion-ac5f9bf35b`. Broader v0.1/v0.2 deferrals
remain as recorded in the scope amendment.

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
| `clarion-6222134e0d` | B.8 | `done`; historical close fields record RED |
| `clarion-ac5f9bf35b` | B.8 repair follow-up | `closed`; GREEN rerun verified |

Non-umbrella Sprint-2 audit/follow-up issues remain open by design; they are not
part of this close ladder.

---

## Tags

The existing `v0.1-sprint-2` tag points at the historical RED close commit
`e6bba0f`:

```bash
git tag v0.1-sprint-2 -m "Sprint 2 close — MVP MCP surface against elspeth scale-test"
```

Do **not** move that tag. It is referenceable as the original close checkpoint.
The post-repair GREEN evidence lives after the tag, in commit `ab6b1dd` and the
GREEN rerun section of [`b8-results.md`](./b8-results.md#2026-05-17t2243z--green-rerun-superseding-red).
