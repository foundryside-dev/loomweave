# Loomweave — dossier participation surface (WS4)

**Date:** 2026-06-02
**Status:** Specified + implemented (Wave 2 / WS4)
**Scope:** Name and pin the EXACT Loomweave HTTP surface the cross-tool entity
**dossier assembler** (Wardline) reads, so a complete, freshness-stamped,
SEI-keyed view of an entity is buildable over Loomweave's HTTP API alone — and
**stays correct after the entity is renamed**. This is the wave that closes the
suite's **core paradise**.

**Authorities:**
- Program design `2026-06-02-loomweave-first-class-program-design.md` §4 (Wave 2), §5 invariants, D3.
- Integrated delivery plan `2026-06-02-loomweave-integrated-delivery-plan.md` Phase 3 (T3.2).
- Wardline dossier design `/home/john/wardline/docs/superpowers/specs/2026-06-01-wardline-weft-entity-dossier-design.md` — the consumer.
- ADR-038 (SEI token + signature). Federation pin: `docs/federation/contracts.md` §Dossier participation surface.

---

## 1. Framing: Loomweave serves slices, it does not assemble

The dossier is **assembled by Wardline** (`core/dossier.py`); Loomweave and Filigree
are *read sources*. Loomweave's WS4 obligation is exactly two things:

1. **Guarantee** every slice the assembler needs is reachable over HTTP (the
   assembler is an HTTP client; it has no MCP session).
2. **Pin** that surface so the contract is explicit and cannot silently regress.

Loomweave does **not** build a dossier envelope, aggregate Wardline taint facts, or
proxy Filigree issues. That separation is the `weft.md` §5 enrich-only line and a
hard boundary of this wave: a sibling may add information to another product's
view but must never become the assembler for it.

## 2. The surface (each slice verified HTTP-reachable + pinned)

| Dossier section (Wardline envelope §5) | Loomweave endpoint | Returns | Origin |
|---|---|---|---|
| `identity` (entity_id, content_hash) — **content axis** | `POST /api/v1/identity/resolve` | `{ sei, current_locator, content_hash, alive }` | Wave 1 |
| `identity` (alive/orphaned + rename lineage) — **identity axis** | `GET /api/v1/identity/sei/:sei`, `GET /api/v1/identity/lineage/:sei` | `{ alive, current_locator?, content_hash?, lineage? }` | Wave 1 |
| `linkages.callers` / `linkages.callees` | `GET /api/v1/entities/:id/callers` · `…/callees` (+ `:batch-get`) | `{ entity_id, callers\|callees:[{entity_id,confidence,call_site_count}], total, truncated }` | Wave 0 |
| file context | `GET /api/v1/files?path=&language=` (+ `:resolve`, `/batch`) | `{ entity_id, content_hash, canonical_path, language, … }` | pre-1.0 |
| `work` (Filigree associations) | **Filigree's own** `GET /api/entity-associations?entity_id=…` (ADR-029) | bound-issue rows | **not Loomweave** — §4 |

All Loomweave `/api/v1/*` routes share one auth posture (HMAC `X-Weft-Component`
preferred; loopback exempt) and one error envelope (`{ error, code, details? }`),
already pinned in `contracts.md`. `linkages.http: true` and
`sei: { supported: true, version: 1 }` in `GET /api/v1/_capabilities` let the
assembler gate on capability rather than probe.

## 3. Two-axis freshness (the no-false-green property)

The dossier must reason on a typed freshness contract, never eyeball staleness.
Loomweave serves **two independent axes**; neither is inferred from the other:

- **Content axis** — `resolve(locator).content_hash` is the entity's current
  whole-file/body blake3. The assembler compares its stored fact's write-time hash
  against it → `FRESH` / `STALE`. A stale fact is still returned, but labelled.
- **Identity axis** — `resolve_sei(sei).alive` (+ `lineage`). The SEI is a durable
  surrogate that **survives rename/move**: after a rename the *locator* changes but
  `resolve_sei` on the carried SEI stays `alive`, and `lineage` carries a
  `locator_changed` (or `moved`) event. An entity whose SEI has no live binding is
  `orphaned` — surfaced honestly, never silently treated as clean.

This is what closes the dossier's ORPHAN gap (Wardline design §6.1, §10.2): the
keystone refactor-stable identity is exactly Loomweave's SEI, and a renamed function
now yields a complete dossier with its facts intact rather than an empty section.

## 4. Filigree associations — the resolved "gap" (decision, not omission)

The Wardline dossier reads its `work` section **directly from Filigree's own**
`GET /api/entity-associations?entity_id=…` (ADR-029, frozen), comparing
`content_hash_at_attach` itself to set the `DRIFT` verdict. Loomweave's `issues_for`
is MCP-only, but that is **not** a dossier gap:

- Adding a Loomweave HTTP endpoint that serves Filigree associations would make
  Loomweave a **proxy/aggregator** for a sibling's data — a direct violation of the
  enrich-only axiom (`weft.md` §5: semantic/initialization/pipeline coupling) and
  the Wave 2 hard boundary ("do NOT aggregate Filigree issues into a Loomweave
  object").
- The join is already federation-correct: all three tools key on **one identity**.
  Loomweave's WS4 contribution to `work` is precisely the **join key** — the SEI from
  `resolve` — which Filigree associations (and Wardline taint facts) bind on.

**Recommendation:** the assembler reads Filigree directly and keys on Loomweave's
SEI. No Loomweave endpoint is added. (If a future consumer genuinely needs
associations over Loomweave's HTTP, that is a new ADR with an enrich-only
justification, not a silent fill.)

## 5. `scc_peers` — named, decided, not silently dropped

The Wardline envelope lists `scc_peers[]` under `linkages`. Loomweave exposes
subsystem **clustering** (`subsystem_members` / `subsystem_of`, MCP-only), which is
**not** strongly-connected-component membership — serving it under `scc_peers`
would be a semantic mismatch. The dossier already degrades gracefully on partial
linkages: `callers`/`callees` carry the load-bearing `synthesis` ("fix locus /
responsible boundary, N hops up the call graph"), which does not depend on SCC.

**Recommendation:** leave `scc_peers` HTTP-unreachable for now; expose a thin
subsystem-peers route (same additive pattern as the Wave 0 callers/callees wrap)
**only if** the assembler confirms it wants subsystem peers there. Surfaced as a
follow-up, not a blocker.

## 6. Conformance / proof

- Each slice is independently pinned and tested (files, callers/callees, identity
  resolve/sei/lineage) in `contracts.md` + `crates/loomweave-cli/tests/serve.rs` and
  `http_read.rs`.
- The **composition** is proven end-to-end against a renamed-function fixture by
  `serve_http_dossier_participation_surface_serves_a_renamed_function`
  (`tests/serve.rs`): `resolve(new_locator)` carries the SEI + content_hash;
  `resolve(old_locator)` is dead; `resolve_sei` is alive at the new locator with a
  `locator_changed` lineage event; `callers`/`callees` resolve at the new locator;
  file context is reachable. SEI carried, facts not orphaned, freshness stamped.

## 7. Definition of done (WS4)

- [x] Participation spec written (this doc) naming the exact surface + what it returns.
- [x] Every depended-on endpoint HTTP-reachable and pinned in `contracts.md`
      (or the gap surfaced with a recommendation — Filigree-direct §4, scc_peers §5).
- [x] Two-axis freshness explicit (content via `resolve`, identity via `resolve_sei`).
- [x] `dossier(entity)` achievable over Loomweave's HTTP surface for a renamed
      function — demonstrated by the serve e2e.

**Core paradise (Loomweave's half):** a rename/move of a function preserves its
SEI-keyed identity and structural linkages over HTTP, with honest two-axis
freshness — the assembler composes the rest. WS4 closes here; it does not enter
the parallel band (WS5–WS8) or WS9.
