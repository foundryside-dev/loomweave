# Registry-Backend & File-Identity Displacement — Cross-Project Sequencing Memo

**Status:** Draft (2026-05-19)
**Scope:** Sequencing memo for bringing Clarion ADR-014's vision forward across Filigree and Clarion. Pairs with [Filigree ADR-014](../architecture/decisions/ADR-014-registry-backend-and-file-identity-displacement.md).
**Sibling docs:**
- Clarion ADR-014 (the original 2026-04-18 design)
- Clarion ADR-029 (entity-associations, shipped 2026-05-17)
- Clarion v0.1 plan §WP10 (the cross-product work package this memo activates)

---

## 1. Problem in one paragraph

`loom.md` §2 says "Clarion owns the file registry." The 2.1.0 Filigree code still mints Filigree-native shadow file IDs on every `POST /api/loom/scan-results`, every `create_observation(file_path=…)`, and every `trigger_scan*`. The Clarion-side design exists (ADR-014, 2026-04-18) and was accepted; the Filigree-side counterpart was never drafted; the Clarion-side WP10 was deferred to v0.2 by the 2026-05-16 Sprint 2 amendment. ADR-029 (entity_associations, May 17) ships the *peer* primitive — issue↔entity binding — but does not close the file-identity split. This memo names what each side must do to close it.

## 2. Why now

- ADR-029 is the last work that *could* have substituted for ADR-014's vision. It explicitly does not.
- Filigree's 2.1.0 release prep is mid-flight (see `2026-05-18-2.1.0-release-prep.md`); landing the schema migration alongside that release minimises the operator-visible migration count.
- Clarion's Sprint 2 closed clean (2026-05-17 signoff); Sprint 3 is unscoped and can absorb the Clarion-side HTTP read API as its anchor work package.
- The Loom URI spec (`2026-05-17-loom-uri-spec.md`) is in draft and depends on a coherent file-identity story across the federation. Closing the split here removes a foundational ambiguity from that spec's scope.

## 3. Work, owned by project

### 3.1 Clarion side (Sprint 3, anchor work)

WP10 from `docs/implementation/v0.1-plan.md` was always Clarion's side of this story. Sprint 2 deferred it; this memo lifts it back into active scope. Plus one new item the original plan did not have: Clarion has no HTTP server today. The `resolve_file` surface ADR-014 assumes must be built.

| ID | Title | Notes |
|---|---|---|
| C-WP10.1 | Clarion HTTP read API — `axum` server in `clarion-cli` | New crate-internal module; reuses `ReaderPool`. Bind on a port advertised in `clarion.yaml`. Wire into `clarion serve` alongside MCP stdio. |
| C-WP10.2 | `GET /api/v1/files?path=&language=` endpoint | Returns `{entity_id, content_hash, canonical_path, language}`. Backed by `clarion-storage::reader` queries; pure read, no writer involvement. |
| C-WP10.3 | Contracts directory for the read API | `docs/federation/contracts.md` on Clarion's side, parity with Filigree's. Publish a JSON fixture for `GET /api/v1/files`. |
| C-WP10.4 | Capability probe response | `GET /api/v1/_capabilities` (or equivalent) returning `{file_registry: true, version: "0.1"}` so Filigree's `ClarionRegistry` can fail fast on incompatible deployments. |
| C-WP10.5 | Sprint 3 scope amendment memo | Mirrors `sprint-2/scope-amendment-2026-05.md` — names the lift, the rationale (this memo), the dependency on Filigree Phase A→B landing. |

Estimated cost: one Clarion sprint (~2 weeks). The dominant lift is C-WP10.1 (no HTTP machinery exists today).

### 3.2 Filigree side (2.1.x or 2.2.0)

Maps onto ADR-014 Phases B / C / E.

| Phase | Title | Maps to ADR-014 § |
|---|---|---|
| F-B.1 | `RegistryProtocol` interface + `LocalRegistry` impl | §1, §3 |
| F-B.2 | Refactor `_upsert_file_record` to consume `RegistryProtocol` | §1 |
| F-B.3 | Refactor `register_file` (db_files, db_observations) to consume `RegistryProtocol` | §1 |
| F-B.4 | Refactor the three `tracker.register_file` call sites in `mcp_tools/scanners.py` | §1 |
| F-B.5 | Schema migration: add `file_records.content_hash`, `file_records.registry_backend` | §3 |
| F-B.6 | Test suite parameterisation over `registry_backend` (default-only after B) | §1 |
| F-C.1 | `registry_backend` config flag wiring (`.filigree.conf`, `ProjectConfig`) | §2 |
| F-C.2 | `ClarionRegistry` implementation (reqwest-equivalent in Python) | §1 |
| F-C.3 | Capability probe in `GET /api/files/_schema.config_flags` | §4 |
| F-C.4 | `FILIGREE_FILE_REGISTRY_DISPLACED` error code + the three direct-mutation surfaces that emit it | §6 |
| F-C.5 | Fail-closed startup under `clarion` mode; `--allow-local-fallback` flag | §7 |
| F-C.6 | `filigree migrate-registry` CLI verb (dry-run, execute, rollback, manifest) | §5 |
| F-E.1 | `docs/federation/contracts.md` update referencing Clarion's read API | §8 |
| F-E.2 | Cross-project launch runbook | §"Sequencing" |

Estimated cost: Phase B is ~1–2 weeks (mostly the refactor + tests). Phase C is ~2 weeks (config + capability + migration tool + fail-closed). Phase E is ~3 days (docs).

### 3.3 Cross-project (Phase D)

Integration tests run against a live Clarion read API. Owned jointly — fixtures published from Filigree, contract pinned-SHA per the pattern Clarion already uses for Filigree's entity-associations.

## 4. Sequencing and the critical path

```
            Clarion Sprint 3                Filigree 2.1.x or 2.2.0
            ────────────────                ─────────────────────────
            C-WP10.1  axum server
                │
                ├──→ C-WP10.2  /api/v1/files
                │
                ├──→ C-WP10.4  /api/v1/_capabilities
                │
                └──→ C-WP10.3  contracts + fixtures
                                                       F-B.1..6  refactor + schema
                                                            │
                                                            ▼
                                                       F-C.1..6  flag, mode, error, migration
                                                            │
                                                            ▼
                                       D  cross-process integration tests
                                                            │
                                                            ▼
                                       E  docs + launch runbook
```

Phase B is independent of Clarion-side work and can land first (behaviour-preserving refactor + schema columns at empty defaults). Phase C cannot land usefully until C-WP10.2 ships. Phase D blocks on both.

## 5. Decisions deferred

- **Batched resolution.** ADR-014 §Negative names sync-RPC cost as an issue under high-throughput scans. Out of scope; revisit if a real workload hits the wall. The `RegistryProtocol` interface admits batching at a future minor version (`resolve_files(paths: list[str])`).
- **Read-side displacement.** `GET /api/loom/files` continues to return Filigree's stored rows. Whether Clarion entity IDs should be the visible IDs in those responses (vs. left as-is, with consumers cross-referencing) is a UX decision; deferred until at least one cross-tool consumer asks.
- **Wardline integration.** Clarion ADR-015's native Wardline→Filigree emitter is unchanged in scope. Wardline findings still flow through Clarion's SARIF translator under both backends.

## 6. Risks

- **Schema migration adoption.** F-B.5 ships a forward-only `ALTER TABLE` with safe defaults. Verified compatible with the 2.0.x → 2.1.x migration path. Operators with custom backup tooling that snapshots `file_records` mid-migration could see inconsistent rows; documented in the release notes.
- **Clarion ADR-014's `--allow-local-fallback` semantics.** The fallback flag exists in case the operator's Clarion is unreachable at startup; if the flag is left on permanently, the operator silently runs in `local` mode while believing they run `clarion` mode. F-C.5 surfaces a persistent dashboard banner while the flag is active to prevent this.
- **Sprint 3 scope contention.** Clarion's Sprint 3 has not yet been scoped; this memo proposes WP10 as the anchor. If Sprint 3 commits to something else first, F-B can still land (behaviour-preserving) and F-C waits.

## 7. What lands when (target dates, not commitments)

| Milestone | Target | Gates |
|---|---|---|
| Filigree ADR-014 ratified | 2026-05-21 | One reviewer; user sign-off on this memo first |
| Clarion Sprint 3 scope amendment ratified | 2026-05-23 | Clarion sprint-planning meeting (author = self) |
| F-B lands on Filigree main | 2026-06-06 | Tests green; no functional change visible to operators |
| C-WP10.1..4 land on Clarion main | 2026-06-13 | `clarion serve` exposes HTTP read API on a configurable port |
| F-C lands behind feature flag | 2026-06-20 | `registry_backend: clarion` works against a local Clarion |
| Phase D integration tests green | 2026-06-27 | Cross-process CI lane in both repos |
| Phase E docs published | 2026-06-30 | Cross-project launch runbook + contract refs |

## 8. Open question for the user

This memo is a strategy proposal, not a plan. Before issues are filed in either tracker, sign-off on:

1. **Scope of "as initially designed."** This memo treats ADR-014 (2026-04-18) as the design of record. ADR-029 stays. The `entity_associations` table is *not* unwound — it remains the right primitive for issue↔entity binding. ADR-014 is additive over it.
2. **Sequencing.** Filigree Phase B first (independent, behaviour-preserving) is the safer parallelisation than waiting for Clarion. If you'd rather sequence Clarion-first to validate the read-API shape against a real consumer, F-B waits.
3. **Migration verb scope.** The `filigree migrate-registry` CLI verb is the operationally-real piece of this story. Worth a second pair of eyes (operator-facing tools land harder than refactors). If you'd rather defer the migration verb behind an explicit follow-up issue (operators stay on `local` for the first release of `clarion` mode), F-C ships smaller.

On greenlight, the next two artifacts are: (a) Filigree filigree-tracker milestone + phases + steps under the existing `planning` pack, (b) Clarion filigree-tracker milestone + phases + steps mirroring WP10. Both repos already use filigree for their own work tracking; no new tracker required.
