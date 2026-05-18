# Sprint 3 — WP10 Scope Amendment

**Status**: CLOSED — Clarion-side WP10 implementation complete.
**Date opened**: 2026-05-19
**Author**: John Morrissey
**Predecessor**: [`../sprint-2/signoffs.md`](../sprint-2/signoffs.md)
**Cross-reference**: [`../../federation/filigree-side/ADR-014-registry-backend-and-file-identity-displacement.md`](../../federation/filigree-side/ADR-014-registry-backend-and-file-identity-displacement.md), [`../../federation/filigree-side/2026-05-19-registry-backend-cross-project-sequencing.md`](../../federation/filigree-side/2026-05-19-registry-backend-cross-project-sequencing.md)

This memo serves three roles in one artifact:

1. It lifts WP10 back into active Clarion scope as the Sprint 3 anchor.
2. It records the Clarion-side half of ADR-014's federation displacement now that
   the Filigree-side counterpart and sequencing memo exist.
3. It turns the 2026-05-19 cross-project sequencing memo into Clarion tracker
   work and records the implementation closeout for Clarion's side.

---

## 1. Status of Sprint 2 Carry-Forward

Sprint 2 is closed GREEN after the B.8 repair rerun. The close ladder certifies
the seven-tool MCP surface against the representative elspeth-slice and records
that B.3, B.4*, B.5*, B.6, B.7, and B.8 are closed for Sprint 2 accounting.

The following Sprint 2 caveats carry forward into Sprint 3 planning:

| Carry-forward item | Sprint 3 effect |
|---|---|
| B.4* mini-gate wall-clock extrapolation was materially optimistic. | Treat HTTP read API integration tests as real measurements, not projection-only evidence. |
| Sprint 2 certified the representative elspeth-slice, not full-repository elspeth. | WP10 exit criteria should name the exact corpus or fixture used for contract proof. |
| Non-umbrella Sprint 2 audit/follow-up issues remain open by design. | They are not WP10 blockers unless a WP10 step touches the affected surface. |

No Sprint 2 umbrella issue is reopened by this amendment.

---

## 2. Why WP10 Is the Sprint 3 Anchor

Clarion ADR-014, accepted on 2026-04-18, made `registry_backend: clarion` the
design of record for displacing Filigree's native file IDs with Clarion file
entity IDs when the products are federated. Sprint 2 deliberately deferred WP10
while it shipped the MCP consult surface and the ADR-029 entity-associations
binding.

That deferral is no longer the right default. Filigree now has its counterpart
ADR and a cross-project sequencing memo:

- `/home/john/filigree/docs/architecture/decisions/ADR-014-registry-backend-and-file-identity-displacement.md`
- `/home/john/filigree/docs/plans/2026-05-19-registry-backend-cross-project-sequencing.md`

Mirrored read-only copies are in `docs/federation/filigree-side/` so Clarion's
repo carries the federation context. These are counterparts, not replacements:
Clarion ADR-014 remains Accepted as-is, and ADR-029's `entity_associations`
primitive stays intact.

The live Clarion tree also confirms the missing implementation surface:

- `crates/clarion-cli/src/serve.rs` serves MCP over stdio only.
- No `axum`, `warp`, or `hyper` server imports exist under `crates/`.
- No `resolve_file` implementation exists under `crates/`.
- `crates/clarion-mcp/src/filigree.rs` already has the blocking `reqwest`
  client pattern used for Filigree's `entity_associations` reverse lookup.

Sprint 3 therefore anchors on the HTTP read API that Filigree's
`ClarionRegistry` needs.

---

## 3. Amended Sprint 3 Scope

### Anchor

| Box | Owning WP | Deliverable | Anchoring decisions |
|---|---|---|---|
| **C-WP10** | WP10 | Clarion HTTP read API for Filigree's `ClarionRegistry` | Clarion ADR-014; Filigree ADR-014; system-design §9, §11; detailed-design §7 |

### Clarion-side breakdown

| Step | Title | Scope |
|---|---|---|
| **C-WP10.1** | `axum` HTTP read server in `clarion-cli` (new module) | Add the server shell and configurable bind/auth posture without changing MCP tool behavior. |
| **C-WP10.2** | `GET /api/v1/files?path=&language=` endpoint | Resolve path/language to `{entity_id, content_hash, canonical_path, language}` from Clarion storage. |
| **C-WP10.3** | Contracts directory + JSON fixture for `GET /api/v1/files` | Publish the Clarion-side contract Filigree can dry-run against. |
| **C-WP10.4** | `GET /api/v1/_capabilities` endpoint | Expose `registry_backend: true` so Filigree's `ClarionRegistry` can fail fast. |
| **C-WP10.5** | Wire HTTP server into `clarion serve` alongside MCP stdio | Start the HTTP read server without regressing existing MCP-over-stdio operation. |

The Filigree sequencing memo used C-WP10.5 for this scope-amendment memo itself.
This document satisfies that planning item; Clarion's local tracker uses
C-WP10.5 for the serve-wiring implementation step requested for Sprint 3.

### Out of scope

- Changes to Clarion ADR-014, ADR-029, or any other existing ADR.
- Any unwind of ADR-029 or the `entity_associations` table.
- Filigree repository mutations; Filigree-side files are read-only inputs here.
- Wardline-native emitter work.

---

## 4. Cross-Project Dependency Direction

The dependency is one-way and should stay explicit:

| Filigree phase | Relationship to Clarion Sprint 3 |
|---|---|
| **Phase B** — `RegistryProtocol` interface, `LocalRegistry`, behavior-preserving call-site refactor, additive schema columns | Independent. It can land before Clarion exposes HTTP because it has no observable `clarion` mode yet. |
| **Phase C** — `registry_backend` flag, `ClarionRegistry`, capability probe, displaced-registration error, fail-closed behavior, migration verb | Blocks on **C-WP10.2** because the Clarion file-resolution endpoint is the first useful read contract. |
| **Phase D** — cross-process integration tests | Blocks on both Clarion C-WP10.2/C-WP10.4 and Filigree Phase C. |
| **Phase E** — docs and launch runbook | Lands after the contract and integration tests are stable. |

The practical sequencing: Filigree Phase B may proceed in parallel; Clarion
should ship C-WP10.1 -> C-WP10.2 -> C-WP10.4 -> C-WP10.5, with C-WP10.3 hanging
off C-WP10.2 once the response shape is concrete.

---

## 5. v0.1 Plan Resequencing

`v0.1-plan.md` originally framed WP10 as Filigree-side plus Wardline-side
cross-product work and assumed the Filigree side would land from Filigree's own
roadmap. That assumption is obsolete as of 2026-05-19: the Filigree-side ADR and
sequencing memo now exist, and they require a Clarion HTTP read surface that is
not implemented today.

This amendment narrows Sprint 3 to the Clarion HTTP read API needed by
Filigree's `ClarionRegistry`. The original WP10 Wardline/SARIF translator work
remains valuable but is not the Sprint 3 anchor unless explicitly added later.

---

## 6. Tracker State to Create

Create one planning milestone:

- `Sprint 3 — WP10 Filigree federation read API (ADR-014)`

Under it, create one phase:

- `Clarion HTTP read API for Filigree's ClarionRegistry`

Create the five C-WP10 steps in the order listed in §3, with dependencies:

```text
C-WP10.1 -> C-WP10.2 -> C-WP10.4 -> C-WP10.5
C-WP10.3 depends on C-WP10.2
```

Add a milestone comment linking the Filigree tracker by title because the
Filigree-side ID is not known in this repo:

```text
See /home/john/filigree's filigree tracker, milestone titled
"Registry-backend & file-identity displacement (ADR-014)".
```

---

## 7. What This Memo Does Not Change

- Sprint 2 remains closed GREEN after repair.
- ADR-029 remains the shipped peer primitive for issue-to-entity binding.
- Clarion ADR-014 remains Accepted and is not edited by this amendment.
- Clarion's MCP-over-stdio serve path remains live alongside the new opt-in
  HTTP read API.
- Filigree Phase B remains safe to land before Clarion; Filigree Phase C waits
  for Clarion's file-resolution endpoint.

---

## 8. References

- [Clarion ADR-014](../../clarion/adr/ADR-014-filigree-registry-backend.md)
- [Clarion ADR-029](../../clarion/adr/ADR-029-entity-associations-binding.md)
- [Clarion v0.1 plan §WP10](../v0.1-plan.md#wp10--cross-product-filigree--and-wardline-side-changes)
- [Sprint 2 scope amendment](../sprint-2/scope-amendment-2026-05.md)
- [Sprint 2 signoffs](../sprint-2/signoffs.md)
- [Filigree ADR-014 mirror](../../federation/filigree-side/ADR-014-registry-backend-and-file-identity-displacement.md)
- [Filigree cross-project sequencing memo mirror](../../federation/filigree-side/2026-05-19-registry-backend-cross-project-sequencing.md)

---

## 9. Implementation Closeout

The Clarion-side WP10 implementation is complete in the local tracker:

| Issue | Scope | Status |
|---|---|---|
| `clarion-44fbe093ca` | Sprint 3 WP10 milestone | `completed` |
| `clarion-f082fb6a49` | Clarion HTTP read API phase | `completed` |
| `clarion-e904d7a7ea` | C-WP10.1 HTTP read server module | `completed` |
| `clarion-aea2d917f9` | C-WP10.2 `GET /api/v1/files` | `completed` |
| `clarion-9d1379172f` | C-WP10.3 contract docs and fixtures | `completed` |
| `clarion-95643a7d5e` | C-WP10.4 `GET /api/v1/_capabilities` | `completed` |
| `clarion-2526c76071` | C-WP10.5 `clarion serve` HTTP wiring | `completed` |

Implementation artifacts:

- `crates/clarion-cli/src/http_read.rs` adds the `axum` HTTP read server.
- `crates/clarion-cli/src/serve.rs` starts the HTTP read server when
  `serve.http.enabled` is true and shuts it down after MCP stdio exits.
- `crates/clarion-storage/src/query.rs` adds `resolve_file` for read-only file
  identity resolution.
- `crates/clarion-mcp/src/config.rs` adds `serve.http.enabled` and
  `serve.http.bind` config.
- `docs/federation/contracts.md` and `docs/federation/fixtures/*.json` pin the
  Filigree-facing response shapes.

Verification evidence:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo build --workspace --bins`
- `cargo nextest run --workspace --all-features` — 405 passed, 2 skipped
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`
- `cargo deny check` — passed with duplicate/unmatched-license warnings only
- `bash tests/e2e/sprint_2_mcp_surface.sh`

The Filigree Phase B/C dependency direction remains unchanged: Phase B can land
independently, while Phase C needs Clarion's `/api/v1/files` contract.
