# ADR-036: Loomweave as Wardline Taint-Fact Store — A Named, Read+Write HTTP Surface

**Status**: Accepted
**Date**: 2026-05-31
**Deciders**: qacona@gmail.com
**Context**: Wardline SP9 requested a persistent per-entity taint/provenance store held by Loomweave and keyed by Loomweave entity (`wardline/docs/integration/2026-05-30-wardline-loomweave-taint-store-requirements.md`). The design response that this ADR ratifies is `docs/superpowers/specs/2026-05-30-loomweave-wardline-taint-store-design.md`; the outward-facing confirmation to Wardline is `docs/federation/2026-05-30-loomweave-wardline-taint-store-response.md`.

## Summary

Wardline's `explain_taint` (and the later overlay-scan + N-hop chain work) re-runs taint analysis on every call. SP9 asks to turn those calls into cheap lookups against a persistent **per-entity taint-fact store that Loomweave holds**, keyed by Loomweave entity: Wardline computes the facts during `wardline scan`, writes them to Loomweave, and later reads become graph lookups.

This ADR records the decision to build that store **inside Loomweave, scoped specifically to Wardline** — a dedicated `wardline_taint_facts` table and a set of `/api/wardline/*` routes — and to do so as Loomweave's **first read+write use of its HTTP API** (the API is read-only today per ADR-014 and ADR-034). The decision is recorded as an ADR, **not** as a `weft.md` §5 asterisk, because the integration **passes** the federation failure test rather than accepting a violation of it (see §Federation analysis below).

The load-bearing guard, carried verbatim because it is the one sentence that keeps this decision from becoming a precedent:

> This is not a precedent for a general-purpose cross-product blob store. The next sibling that wants per-entity persistence gets its own named, justified surface or it does not get one.

## Context

### What Wardline asked for

Wardline's SP9 request (`wardline/docs/integration/2026-05-30-wardline-loomweave-taint-store-requirements.md`, referenced relative to the Wardline repo) wants a persistent per-entity taint/provenance store keyed by Loomweave entity, so that the SP8 stateless re-run becomes a layered optimization rather than the only path. The seven capabilities requested are: batch qualname→entity resolution, per-entity taint-fact upsert, per-entity fetch (single + batch), a freshness/staleness contract, entity-lifecycle handling, an HTTP transport, and per-project isolation.

### Where Loomweave stands today

- Loomweave's HTTP API is **read-only**. ADR-014 introduced the federation read API; ADR-034 hardened it (HMAC inbound auth, batch resolution, `BRIEFING_BLOCKED`, stable per-project `instance_id`). No write path is exposed over HTTP.
- Writes to the `.loomweave/` SQLite DB go through the ADR-011 writer-actor. `loomweave analyze` holds one for the duration of a run. `loomweave serve` opens the `ReaderPool` for queries and *additionally* spawns an optional MCP query-time writer-actor when an LLM summary provider is configured (`serve.rs`, for the summary/inferred-edge caches). No write path is exposed over HTTP today.
- The schema-reserved `wardline TEXT` column on `entities` is **orthogonal** to this work — it was reserved for the fingerprint/qualname reverse-map (`WardlineMeta`, `detailed-design.md` §7), a different and smaller dataset. It is not the taint store and is left as-is. Crucially, `loomweave analyze` rebuilds every `EntityRecord` with `wardline_json: None` and the `entities` UPSERT sets `wardline = excluded.wardline`, so any taint fact stored in that column would be silently wiped on the next re-analyze. A separate table is the only clobber-safe home.

### Why this needs a decision, not just an implementation

Two things make this load-bearing rather than routine. First, it flips Loomweave's HTTP API from read-only to read+write — a posture change that the security model (ADR-034) and the operator trust model (`docs/operator/loomweave-http-read-api.md`) must absorb. Second, the shape "a sibling writes opaque blobs keyed by Loomweave entity" is, generalised, exactly the **shared system-of-record** that `weft.md` §6 forbids. The decision is therefore about *boundaries* — what surface exists, what it is named, and what it must never be allowed to become — not merely about a table and four routes.

## Decision

### 1. A Wardline-specific, per-entity taint-fact store

Loomweave builds a per-entity taint-fact store **named for and scoped to Wardline**:

- A dedicated SQLite table, `wardline_taint_facts`, introduced by **migration `0003`** (`crates/loomweave-storage/migrations/0003_wardline_taint_facts.sql`; the design spec's "migration 0002" predates `0002_briefing_blocked.sql` and is superseded — `CURRENT_SCHEMA_VERSION` bumps `2 → 3`). The table is keyed by `entity_id` with `ON DELETE CASCADE` against `entities(id)`; it stores `wardline_json` (opaque, verbatim, Wardline-owned), and the queryable observability columns `scan_id`, `content_hash_at_compute`, and `updated_at`.
- A set of `/api/wardline/*` HTTP routes on `loomweave serve` (enumerated in Consequences), HMAC-gated per ADR-034 inbound auth.

The surface is `wardline`-named at every layer — the table, the routes — exactly the naming discipline the ADR-018 asterisk used. There is no generic `sibling_json` column, no `/api/blob/*` route, no capability bus. This structural specificity is what makes the federation guard (§below) enforceable rather than aspirational.

### 2. The first read+write use of the HTTP API

This is Loomweave's first read+write HTTP surface. Writes go through an **optional** ADR-011 writer-actor that `loomweave serve` spawns **only when the write API is config-enabled** — the new config knob **`serve.http.wardline_taint_write`**, which **defaults off**. With the knob off, `serve` retains exactly today's read-only posture (the `ReaderPool` alone) and the write routes reject cleanly. The writer-actor is the same ADR-011 mechanism `loomweave analyze` uses; taint writes are query-time writes (the `query_time_write` actor path, like the summary-cache upsert), not analyze-run `BeginRun`/`CommitRun` writes.

### 3. Resolution: exact-tier direct lookup; Wardline owns normalization

Writes and reads are **qualname-keyed**. Wardline sends a **pre-composed** dotted qualname; Loomweave builds the candidate entity ID `python:function:<qualname>` and resolves it by **direct existence lookup** against the local catalog. **Loomweave does no normalization at resolution time** — Wardline owns the normalization and pre-composes the qualname to byte-match Loomweave's `canonical_qualified_name` per `docs/federation/fixtures/wardline-qualname-normalization.json`. The five ADR-018 divergence traps (`<locals>`, nested-class chains, non-`src` package roots such as `lib.foo`/`app.service`, the `a.src.b` pattern) are therefore **Wardline's** conformance burden against the fixture; on Loomweave's side they reduce to **verbatim-storage** correctness (Loomweave must not strip, rewrite, or re-canonicalise the composed string).

Resolution is **exact-tier only for writes**: a write requires an `exact` match; `heuristic`/`none` results are returned in `unresolved_qualnames` and **never written** (a heuristic *write* would silently mis-attach a fact to the wrong entity). Reads may surface a `heuristic` match. The heuristic resolution tier and the conformance oracle over raw file+qualname (`scheme=wardline_qualname`) are **deferred** to Flow B B.2 (`clarion-ca2d26ffbe`), which extends — and must consume, not rebuild — this exact-tier resolver.

### 4. Concurrency posture (ADR-011)

In-process, a write-enabled `serve` may run **two** ADR-011 writer-actors against the same DB at once — the optional MCP summary writer (when an LLM provider is configured) and the taint-store writer — each on its own connection. This is a deliberate, bounded relaxation of ADR-011's single-writer-per-process expectation: the two write *streams* are independent (summary/inferred-edge caches vs. Wardline taint facts), and every writer opens its batch with `BEGIN IMMEDIATE` under the same `PRAGMA busy_timeout=5000` + `loomweave-storage::retry` capped-backoff layer, so they serialize at the SQLite write lock rather than corrupting. The same mechanism covers **cross-process** contention: a write-enabled `serve` and a concurrent `analyze` are **not expected** to write the same `.loomweave/` DB at the same time (an operational expectation, documented rather than enforced beyond the SQLite lock), but if they do, the busy-timeout + retry resolves it. A write that still cannot land after retry **fails as a retryable error**, and Wardline degrades to its SP8 stateless re-run. Per-entity replace is atomic at the row level, so Loomweave never corrupts or partially merges two scans for the same entity.

### 5. The federation guard (load-bearing, verbatim)

> This is not a precedent for a general-purpose cross-product blob store. The next sibling that wants per-entity persistence gets its own named, justified surface or it does not get one.

The guard binds future decisions. A subsequent sibling (Shuttle, or a fourth-party tool) requesting per-entity persistence does not inherit this API by extension; it must pass the same `weft.md` §3–§5 analysis on its own terms and earn its own named, justified surface. There is no generic blob bus, and this ADR must not be cited as authority for building one.

## Federation analysis (`weft.md` §3–§5) — passes; ADR, not asterisk

The integration is **enrich-only and additive**, and passes the §5 failure test on all three modes:

- **Solo-useful (both products).** Loomweave's briefings, queries, and catalog work with the taint store **empty** — the store is optional enrichment on Loomweave's *own* entities, never a precondition for Loomweave's semantics. Wardline guarantees (request §6) that its **SP8 stateless re-run is the permanent fallback**: Wardline boots and answers `explain_taint` with Loomweave absent, unreachable, write-disabled, or stale. Neither product requires the other to make sense of its own data.
- **Pairwise-composable.** `(Wardline, Loomweave)` composes directly — Loomweave stores Wardline's facts and serves them back cheaply. No third sibling mediates the pair (no pipeline coupling).
- **No semantic coupling.** `wardline_json` is stored **verbatim and opaque**. Loomweave never parses, validates, or depends on its contents; all taint semantics (including the single-successor chain walk) stay Wardline-side. Removing Wardline changes nothing about the meaning of Loomweave's own data — an empty or absent store reduces convenience, not coherence.
- **No initialization coupling.** `serve` boots and self-validates whether or not the write knob is set; with it off, the posture is identical to today's read-only `serve`.

The one real risk is the **"no Weft store" rule** (`weft.md` §6): a *generic* "any sibling writes opaque blobs keyed by entity" API would turn Loomweave into the shared system-of-record the doctrine forbids. The guard in §5 of the Decision neutralises that risk by keeping the surface **structurally Wardline-specific** (a `wardline`-named table and `wardline`-scoped routes), not a general `sibling_json` bus.

Because this integration **passes** the failure test — rather than accepting a violation with a written retirement condition — it is recorded as a **new ADR, not a new `weft.md` §5 asterisk**. Per `weft.md` §5, an asterisk is the instrument for an *accepted, temporarily-tolerated violation* of one named failure-test mode, carrying a retirement condition and an honest statement of which mode is violated. This decision violates no mode, has nothing to retire, and so is the wrong shape for an asterisk. It is a clean federation surface, recorded as a locked architectural decision.

## Consequences

### Positive

- Wardline's `explain_taint` becomes a cheap per-entity lookup instead of a re-analysis, with the SP8 re-run preserved as a permanent standalone fallback. The optimization is layered, never load-bearing.
- The store is clobber-safe by construction: a dedicated `wardline_taint_facts` table is never touched by the `entities` UPSERT, so re-analyze does not wipe taint facts the way the schema-reserved `wardline` column would.
- `scan_id` and `content_hash_at_compute` are real columns (not parsed out of the opaque blob), giving observability and an optional future prune-by-scan without ever requiring Loomweave to read `wardline_json`.
- The federation boundary is structural, not merely documented: the `wardline`-named table and routes make "Wardline-specific, not a generic blob bus" a property of the schema, enforceable at review time against the §5 guard.
- The exact-tier qualname resolver (`resolve_wardline_qualnames`) is reused, unchanged, by the MCP `entity_resolve` tool (`clarion-d76e7f7267`) — one resolution definition serves both the federation `/api/wardline/resolve` route and the consult-mode read surface; the MCP tool additionally projects each candidate through `entity_json` so it returns the SEI per candidate (ADR-038), whereas the federation route is locator-only.

### Negative / costs

- Loomweave's HTTP API gains a write posture for the first time, widening the security surface that ADR-034's HMAC auth and the operator trust model must cover. Mitigation: the write path is **off by default** (`serve.http.wardline_taint_write = false`) and HMAC-gated; with the knob off, `serve` is byte-for-byte today's read-only posture.
- `serve` gains an optional ADR-011 writer-actor, adding both an **in-process** contention surface (it coexists with the optional MCP summary writer — two writer-actors on one DB, §4) and the **cross-process** surface (vs. a concurrent `analyze`). Mitigation: every writer uses `BEGIN IMMEDIATE` + `PRAGMA busy_timeout=5000` + the `loomweave-storage::retry` capped-backoff layer; a write that cannot land fails retryably and Wardline degrades to SP8 (enrich-only, never corruption).

### What ships (artifact inventory)

- **Migration `0003`** — `crates/loomweave-storage/migrations/0003_wardline_taint_facts.sql`; the `wardline_taint_facts` table; `CURRENT_SCHEMA_VERSION` bumps `2 → 3`.
- **Routes** (HMAC-gated, on `loomweave serve`):
  - `POST /api/wardline/resolve` — batch qualname→entity resolution (exact-tier; pre-composed `python:function:<qualname>` direct lookup).
  - `POST /api/wardline/taint-facts` — batch upsert (per-entity replace), qualname-keyed, exact-only, returning `{written, unresolved_qualnames}`, `project`-guarded.
  - `GET /api/wardline/taint-facts` — single fetch by qualname; returns blob + `current_content_hash` + `exists`.
  - `POST /api/wardline/taint-facts:batch-get` — batch fetch; one round-trip; blob + `current_content_hash` + `exists` per entity.
- **Config knob** — `serve.http.wardline_taint_write` (boolean, default `false`); gates whether `serve` spawns the optional writer-actor and exposes the write/resolve routes.

### What is deferred (not in this surface)

- The **heuristic resolution tier** and the **conformance oracle** (`scheme=wardline_qualname` over raw file+qualname) remain deferred to **Flow B B.2** (`clarion-ca2d26ffbe`). The shipping resolve route here is exact-tier only; B.2 extends it and must consume this resolver rather than rebuild it.
- No general-purpose blob store, no Loomweave parsing of `wardline_json`, no replacement of the SP8 re-run, no mandatory lifecycle cascade/prune machinery beyond the freshness gate, and no ADR-029 issue↔entity bindings — see the design spec §9 for the full non-goal list.

## Weft vocabulary verdict

Per `docs/loomweave/adr/README.md` ("ADR acceptance criteria — Weft vocabulary discipline") and `weft.md` §8, this ADR introduces cross-product-visible field names that cross the Wardline↔Loomweave wire in the SP9 contract: `wardline_json`, `scan_id`, `content_hash_at_compute`, `current_content_hash`, and `unresolved_qualnames`. Verdict: **`no clash`** — each term is either Wardline-namespaced or local to this Loomweave surface, and none collides with an existing sibling term. `content_hash` semantics follow Loomweave's existing definition (whole-file `blake3`, hex; per the design spec §3), so the hash-related fields reuse rather than redefine vocabulary. These entries are **recorded in [`docs/suite/glossary.md`](../../suite/glossary.md) ("SP9 Wardline taint-store wire terms") as part of this ADR's acceptance evidence**, per the ADR-acceptance rule. The Loomweave-internal names `wardline_taint_facts` (table) and `serve.http.wardline_taint_write` (config) are deliberately excluded from the glossary — they never cross the wire to Wardline.

## Related Decisions

- [ADR-011](./ADR-011-writer-actor-concurrency.md) — Writer-actor concurrency. The taint store's optional `serve`-side writer is the same mechanism; the concurrency posture (§4) rests on ADR-011's `busy_timeout` + capped-backoff retry.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — The federation HTTP read API this ADR extends from read-only to read+write.
- [ADR-018](./ADR-018-identity-reconciliation.md) — Identity reconciliation; Wardline owns its qualnames and pre-composes them to Loomweave's canonical form. The divergence traps are Wardline's conformance burden against the normalization fixture; Loomweave's side is verbatim storage.
- [ADR-029](./ADR-029-entity-associations-binding.md) — Entity-association binding; adjacent and explicitly out of scope (request §9). Not required by, and does not require, this surface.
- [ADR-034](./ADR-034-federation-http-read-api-hardening.md) — HTTP read-API hardening; the HMAC inbound auth and `project`/instance posture the `/api/wardline/*` routes inherit.

## References

- Design spec (federation verdict §2; seven decisions §3): [`docs/superpowers/specs/2026-05-30-loomweave-wardline-taint-store-design.md`](../../superpowers/specs/2026-05-30-loomweave-wardline-taint-store-design.md).
- Outward contract response: [`docs/federation/2026-05-30-loomweave-wardline-taint-store-response.md`](../../federation/2026-05-30-loomweave-wardline-taint-store-response.md).
- Implementation plan: [`docs/superpowers/plans/2026-05-31-loomweave-wardline-taint-store.md`](../../superpowers/plans/2026-05-31-loomweave-wardline-taint-store.md).
- Wardline request: `wardline/docs/integration/2026-05-30-wardline-loomweave-taint-store-requirements.md` (Wardline repo).
- Qualname parity fixture: [`docs/federation/fixtures/wardline-qualname-normalization.json`](../../federation/fixtures/wardline-qualname-normalization.json).
- Federation doctrine: [`docs/suite/weft.md`](../../suite/weft.md) §3–§6.

— End of ADR-036 —
