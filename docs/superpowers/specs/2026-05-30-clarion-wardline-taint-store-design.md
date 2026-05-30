# Clarion as Wardline taint-fact store (SP9) — design

**Date:** 2026-05-30
**Status:** approved (brainstorm). Deliverable: this spec + an outward-facing
contract response to Wardline + tracked `release:1.1` issues.
**Request being answered:** `wardline/docs/integration/2026-05-30-wardline-clarion-taint-store-requirements.md`
(Wardline SP9 taint-store requirements).
**Builds on:** ADR-011 (writer-actor concurrency), ADR-014 (HTTP read API),
ADR-018 (identity reconciliation / qualname divergence), ADR-034 (HTTP read-API
hardening / HMAC), the federation contract `docs/federation/contracts.md`, and
`loom.md` §3–§5 (federation axiom).

---

## 1. The ask, in one paragraph

Wardline's SP9 wants to make `explain_taint` (and later overlay-scan + the full
N-hop chain) into **cheap queries against a persistent per-entity taint/provenance
store that Clarion holds**, keyed by Clarion entity, instead of re-running the
analysis on every call. Wardline computes the taint facts during `wardline scan`
and writes them to Clarion; later reads become graph lookups. The store does not
exist yet. Wardline asks Clarion for five capabilities (A–G in the request): batch
qualname→entity resolution, per-entity taint-fact upsert, per-entity fetch
(single + batch), a freshness/staleness contract, entity lifecycle handling, an
HTTP transport, and per-project isolation.

This document is Clarion's design response: the federation verdict, the answers to
the seven decisions Wardline requested, the Clarion-side architecture, and the
issue breakdown.

## 2. Federation verdict (loom.md §3–§5) — passes; ADR, not asterisk

The integration is **enrich-only and additive**, and passes the §5 failure test:

- **Solo-useful (both).** Clarion's briefings, queries, and catalog work with the
  taint store empty — it is optional enrichment on Clarion's *own* entities.
  Wardline guarantees (request §6) that the SP8 stateless re-run remains its
  permanent fallback; it boots and answers `explain_taint` with Clarion absent,
  unreachable, or stale. Neither product requires the other.
- **Pairwise-composable.** `(Wardline, Clarion)` composes directly: Clarion stores
  Wardline's taint facts and serves them back cheaply.
- **Not semantic coupling.** `wardline_json` is stored **verbatim and opaque** —
  Clarion never parses, validates, or depends on its contents. All taint
  semantics (including the single-successor chain walk) stay Wardline-side.

The one real risk is the **"no Loom store" rule (§6)**: a *generic* "any sibling
writes opaque blobs keyed by entity" API would turn Clarion into the shared
system-of-record the doctrine forbids. The guard is to keep the surface
**Wardline-specific** — a `wardline`-named table and `wardline`-scoped routes, the
same naming discipline as the ADR-018 asterisk — **not** a general `sibling_json`
bus.

Because this **passes** the failure test (rather than accepting a violation), it
is recorded as a **new ADR**, *not* a new `loom.md` §5 asterisk. The load-bearing
sentence the ADR must carry: *this is not a precedent for a general-purpose
cross-product blob store; the next sibling that wants per-entity persistence gets
its own named, justified surface or it does not get one.*

## 3. The seven decisions (Clarion's answers)

| # | Decision | Clarion's answer |
|---|---|---|
| 1 | Key by `EntityId` or accept qualname? | **Qualname-keyed API.** Clarion resolves `qualname → EntityId` internally using the local-catalog reconciliation it already owns (the same mechanism as Flow B, `clarion-ca2d26ffbe`). **Writes require `exact` resolution**; `heuristic`/`none` are returned in `unresolved_qualnames` and never written (a heuristic *write* would silently attach a fact to the wrong entity). Reads may surface a `heuristic` match. |
| 2 | Store `scan_id`/generation per fact? | **Yes — as a real column** in the dedicated table (cheap, no blob parsing), for observability and to enable an *optional future* prune-by-scan. **Correctness does not depend on it** — it rests on the freshness gate (#3) + per-entity replace. |
| 3 | Content-hash definition + per-entity exposure | **`blake3` of the containing file's raw bytes, hex-encoded, whole-file** — Clarion's existing definition (`clarion-storage::query` derives it lazily as `file_content_hash`, `blake3::hash(fs::read(path))`). **Not** sha256, **not** LF-normalized. File-granular, which matches Wardline's "conservatively re-stale all of a file's functions on any edit." Wardline adopts Clarion's definition as the single source of truth, as offered in the request. Returned per entity on fetch as `current_content_hash`. |
| 4 | Lifecycle: cascade vs per-scan prune? | **Neither is the correctness mechanism.** Clarion's entity lifecycle is **wipe-and-rerun** (`clarion-storage::query` documents "v0.1 assumes a wipe-and-rerun analyze workflow"); there is no incremental entity delete to cascade off. The **§D freshness gate is the universal safety net**: deleted, renamed, and edited entities all surface to Wardline as hash-mismatch-or-`exists:false` → Wardline recomputes and re-writes. No cascade and no mandatory prune call are required. An optional `prune-by-scan` may be added later via the `scan_id` column. |
| 5 | HTTP+JSON surface, or local-only? | **HTTP+JSON**, new routes on `clarion serve`, HMAC-gated (ADR-034 inbound auth), stdlib-`urllib`-callable, reached via a `--clarion-url` analog. This makes Clarion's HTTP API **read+write** for the first time (it is read-only today); the ADR covers that shift. |
| 6 | Per-project isolation + the `project` handle | **Yes.** Clarion's DB is per-project (`.clarion/` under the project root); one `serve` instance serves exactly one project. The request's `project` field is accepted as a **guard** (it must match the served project; mismatch → error) rather than as a selector. |
| 7 | Timeline | Driven by the issue sequencing in §8. |

## 4. Architecture

### 4.1 Write path — optional writer-actor on `clarion serve`

`clarion serve` today opens only the `ReaderPool`; writes go through the ADR-011
writer-actor, held by `clarion analyze`. SP9 gives `serve` an **optional
writer-actor**, constructed only when the taint-store write API is enabled in
config (default off). Cross-process contention with a concurrent `clarion analyze`
on the same DB is handled by the existing `PRAGMA busy_timeout=5000` plus the
`clarion-storage::retry` capped-backoff layer; if a write still cannot land, it
fails as a retryable error and Wardline degrades to the SP8 re-run (enrich-only).
Operationally, a write-enabled `serve` and a concurrent `analyze` are not expected
to write the same DB at the same time; this is documented, not enforced beyond the
SQLite lock.

### 4.2 Schema — dedicated `wardline_taint_facts` table (migration `0002`)

```sql
CREATE TABLE wardline_taint_facts (
    entity_id               TEXT PRIMARY KEY
                                 REFERENCES entities(id) ON DELETE CASCADE,
    wardline_json           TEXT NOT NULL,   -- opaque, verbatim, Wardline-owned
    scan_id                 TEXT,            -- observability; from the request
    content_hash_at_compute TEXT,            -- mirror of the value inside the blob
    updated_at              TEXT NOT NULL
);
```

**Why a dedicated table, not the schema-reserved `wardline` column on `entities`:**

1. **No clobber.** `clarion analyze` builds every `EntityRecord` with
   `wardline_json: None` and the `entities` UPSERT sets `wardline = excluded.wardline`,
   so any taint fact written into that column would be wiped on the next
   re-analyze. A separate table is never touched by the entity UPSERT.
2. **Clean ownership.** Wardline's data lives in a Wardline-named table; Clarion's
   `entities` table stays Clarion-owned. This makes the "Wardline-specific, not a
   generic blob bus" federation guard structural.
3. **Queryable metadata.** `scan_id` and `content_hash_at_compute` are real columns
   (no parsing of the opaque blob), enabling observability and an optional future
   prune-by-scan.
4. `foreign_keys = ON` is already set, so `ON DELETE CASCADE` is available as
   defense-in-depth; under the wipe-and-rerun lifecycle it rarely fires (a wipe
   drops the whole DB, table included), but it is correct if an incremental entity
   delete is ever introduced.

The schema-reserved `wardline TEXT` column on `entities` is **orthogonal** — it was
reserved for the fingerprint/qualname reverse-map (`WardlineMeta`, detailed-design
§7, Flow A territory), a different and smaller dataset — and is left as-is.

### 4.3 Read path

Read endpoints run on the existing read-only pool. Each fetched fact returns:

```json
{ "qualname": "...", "wardline_json": { ... }, "current_content_hash": "<blake3-hex>", "exists": true }
```

`current_content_hash` is the entity's containing-file blake3, derived lazily at
read time (the existing `query` derivation). Wardline compares it to
`content_hash_at_compute` inside the blob; match → serve, mismatch/absent → stale →
recompute. Computing the hash reads the file once (cheap) — far cheaper than
Wardline re-running taint analysis, so the SP9 win holds.

### 4.4 Endpoints (proposed shapes; capability is the contract)

| Method + path | Purpose | Auth | Decision |
|---|---|---|---|
| `POST /api/wardline/taint-facts` | Batch upsert (per-entity replace), qualname-keyed, exact-only; returns `{written, unresolved_qualnames}` | HMAC (write) | 1,2,5,6 |
| `POST /api/wardline/taint-facts:batch-get` | Batch fetch by qualname; returns blob + `current_content_hash` + `exists` per entity | HMAC | 3,5,6 |
| `GET /api/wardline/taint-facts?qualname=` | Single fetch | HMAC | 3,5,6 |
| `GET /api/v1/entities/resolve?scheme=wardline_qualname&value=&file=` | Qualname→EntityId resolve oracle (§A) — already designed (detailed-design §7), deferred from v1.0 | HMAC | 1 |

The resolve oracle is **its own issue** (§8 W.4): it is independently valuable,
pre-designed, and bundling it into the store core would balloon the work.

## 5. Error handling (enrich-only, no fabrication)

- **Write contention / Filigree-style outage** → retryable error; Wardline retries
  or falls back to SP8. Clarion never corrupts or partially-merges two scans for
  the same entity (per-entity replace is atomic at the row level).
- **Unresolved qualname on write** → not an error; returned in
  `unresolved_qualnames` so Wardline can fall back rather than guess.
- **Fetch of an absent/renamed/deleted entity** → `exists: false`; Wardline treats
  it as stale and recomputes. No fabrication.
- **`project` guard mismatch** → reject the request (wrong project); never serve
  cross-project data.
- **Opaque blob** → Clarion stores and returns `wardline_json` verbatim; a future
  `wardline-taint-2` schema needs no Clarion change.

## 6. What Wardline guarantees in return (recorded, from request §6)

Standalone degradation (SP8 re-run is the permanent fallback), opaque/versioned
blob, idempotent batch writes, qualname conformance already established (Round 1 +
the shared corpus), and Wardline owns the fresh/stale decision. These are the
reciprocal half of the enrich-only contract.

## 7. Testing (hermetic)

- **Migration:** `0002` applies; round-trips an upsert + fetch; FK present.
- **Resolution:** qualname→entity exact match incl. the ADR-018 divergence traps
  (shared with Flow B `clarion-ca2d26ffbe`); exact-only write rejects heuristic →
  `unresolved_qualnames`.
- **Write API:** HMAC required; per-entity replace idempotent; `project` guard;
  batch with mixed resolved/unresolved.
- **Read API:** blob returned verbatim; `current_content_hash` derived; `exists`
  false for absent/renamed; batch-get one round-trip.
- **Freshness:** stored `content_hash_at_compute` vs derived `current_content_hash`
  match/mismatch behavior.
- **Concurrency:** a write under simulated busy degrades via retry, never panics.
- **Enrich-only:** server with write API disabled rejects writes cleanly; reads of
  an empty store return `exists:false`, not fabricated facts.

## 8. Issue breakdown (`release:1.1`, umbrella + children)

1. **W.0 — ADR.** "Clarion as Wardline taint-fact store (read+write HTTP surface)":
   the federation verdict (§2), the read-only→read+write shift, the writer-actor
   concurrency posture, and the *not-a-general-blob-store* guard. *Blocks the rest.*
2. **W.1 — Storage.** Migration `0002` + `wardline_taint_facts` + writer-actor
   command (upsert/replace). *Depends: W.0.*
3. **W.2 — Write endpoint.** Optional writer-actor on `serve` +
   `POST /api/wardline/taint-facts` (HMAC, qualname-keyed, exact-only,
   `unresolved_qualnames`, `project` guard). *Depends: W.1.*
4. **W.3 — Read endpoints.** Single + `batch-get` (blob + `current_content_hash` +
   `exists`). *Depends: W.1.*
5. **W.4 — Resolve oracle.** `GET /api/v1/entities/resolve?scheme=wardline_qualname`
   (§A), standalone; reuses the Flow B reconciliation. *Depends: W.0.*
6. **W.5 — Contracts pin.** Pin all new routes + the freshness contract in
   `docs/federation/contracts.md`. *Depends: W.2, W.3.*
7. **Contract response** to Wardline (`docs/federation/`), confirming the seven
   decisions, committed alongside this spec (no code dependency).

W.1 is the spine; W.2 ‖ W.3 ‖ W.4 fan out from it; W.5 documents what shipped.

## 9. Non-goals (this round)

- **No general-purpose blob store.** The surface is Wardline-specific by design
  (§2). Other siblings do not get this API by extension.
- **No Clarion parsing of `wardline_json`.** The chain walk and all taint semantics
  stay Wardline-side.
- **No replacement of the SP8 stateless re-run.** It is the permanent standalone
  fallback; SP9 is a layered optimization, never a hard dependency.
- **No overlay-scan / full N-hop chain as Clarion features.** The store backs them;
  they get their own Wardline specs.
- **No ADR-029 issue↔entity bindings.** Adjacent, separate feature (request §9).
- **No mandatory lifecycle cascade/prune machinery** beyond the freshness gate
  (§3 Decision 4).

## 10. References

- Wardline request: `wardline/docs/integration/2026-05-30-wardline-clarion-taint-store-requirements.md`.
- ADR-011 (writer-actor), ADR-014 (HTTP read API), ADR-018 (identity reconciliation),
  ADR-034 (HMAC inbound auth).
- `docs/federation/contracts.md` — read/consume contract surface.
- `docs/federation/fixtures/wardline-qualname-normalization.json` — qualname parity.
- Flow B sibling: `clarion-71f995b88a` (`B.2` reconciliation `clarion-ca2d26ffbe`
  shares the resolve logic).
