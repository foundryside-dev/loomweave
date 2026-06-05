# Loomweave → Wardline: Taint-Store Contract Response (SP9)

**From:** Loomweave maintainers
**To:** Wardline maintainer
**Date:** 2026-05-30
**Re:** `wardline/docs/integration/2026-05-30-wardline-loomweave-taint-store-requirements.md`
**Status:** Loomweave confirms the contract. Decisions answered below; Loomweave-side
build is sequenced under `release:1.1`. Wardline may begin its SP9 spec against
this response.
**Full design:** `docs/superpowers/specs/2026-05-30-loomweave-wardline-taint-store-design.md`.

---

## Verdict

**Confirmed — Loomweave will build the per-entity taint store.** The ask passes the
Weft federation failure test (`weft.md` §3–§5): enrich-only, both products stay
solo-useful (Loomweave queries work with the store empty; your SP8 re-run is the
permanent fallback), and the blob stays opaque to Loomweave. We recorded the
decision as a Loomweave **ADR**, not a `weft.md` asterisk, with one explicit guard:
the surface is **Wardline-specific** (a `wardline`-named table + `wardline`-scoped
routes), **not** a general-purpose cross-product blob store. The next sibling that
wants per-entity persistence gets its own named, justified surface.

## Answers to your seven decisions

| # | Your ask | Loomweave's answer |
|---|---|---|
| 1 | Key by `EntityId` or qualname? | **Qualname-keyed**, as you preferred. Loomweave resolves internally. **Writes require an `exact` resolution** — `heuristic`/`none` come back in `unresolved_qualnames` and are *not* written (a heuristic write would mis-attach a fact). Reads may surface `heuristic`. |
| 2 | Store `scan_id`/generation? | **Yes**, as a real column (for observability + an optional future prune-by-scan). Correctness rests on the freshness gate + per-entity replace, exactly as you proposed. |
| 3 | **Content-hash definition + per-entity exposure** | **Adopt Loomweave's: `blake3` of the containing file's raw bytes, hex, whole-file.** *Not* sha256, *not* LF-normalized. It is file-granular (matches your "re-stale the whole file" intent) and is returned per entity on fetch as `current_content_hash`. Please pin `blake3`/raw-bytes/whole-file as the single source of truth on the Wardline side. |
| 4 | Cascade vs per-scan prune? | **Neither is needed.** Loomweave's lifecycle is **wipe-and-rerun**; there is no incremental delete to cascade off. The **freshness gate is the safety net**: deleted / renamed / edited entities all surface to you as hash-mismatch or `exists:false` → you recompute. No cascade, no mandatory prune. (We can add `prune-by-scan` later via the `scan_id` column if it earns its keep.) |
| 5 | HTTP+JSON or local-only? | **HTTP+JSON**, stdlib-`urllib`-callable, on `loomweave serve`, with a `--loomweave-url` analog. Auth is HMAC (`X-Weft-Component`, ADR-034), the same posture as your Filigree emitter's bearer path. Note: this makes Loomweave's HTTP API read+write for the first time — covered by the ADR. |
| 6 | Per-project isolation + `project` handle | **Confirmed.** One `loomweave serve` = one project (`.loomweave/` under the project root). The `project` field is accepted as a **guard** (must match the served project) rather than a selector. |
| 7 | Timeline | Sequenced below; tracked in Loomweave's `release:1.1`. |

## What Loomweave commits to build (sequence)

1. **ADR** — the federation decision + the read+write shift + the not-a-blob-store guard.
2. **Storage** — migration `0002`, a dedicated `wardline_taint_facts` table
   (entity-keyed, `wardline_json` verbatim/opaque, `scan_id`,
   `content_hash_at_compute`), writer-actor upsert/replace.
3. **Write endpoint** — `POST /api/wardline/taint-facts` (batch, qualname-keyed,
   exact-only, `{written, unresolved_qualnames}`, `project` guard).
4. **Read endpoints** — single + `:batch-get`, returning `wardline_json` verbatim +
   `current_content_hash` + `exists`.
5. **Resolve oracle** — `GET /api/v1/entities/resolve?scheme=wardline_qualname` (your
   §A). This is a pre-designed, previously-deferred Loomweave feature; it ships as its
   own unit.
6. **Contract pin** — all new routes + the freshness contract pinned in
   `docs/federation/contracts.md`.

## One thing to note on your proposed shapes

Your §B/§C/§E penciled in `wardline_json` living in Loomweave's schema-reserved
`wardline` column. Loomweave is instead using a **dedicated `wardline_taint_facts`
table** — because `loomweave analyze` re-UPSERTs the `entities` row (writing
`wardline = NULL`) and would clobber facts stored in that column on every
re-analyze. The dedicated table is untouched by analyze, keeps ownership clean, and
makes `scan_id`/hash queryable without parsing your blob. No change to your wire
shapes — the column-vs-table choice is entirely Loomweave-internal.

## Reciprocal: what we're relying on from you (your §6)

SP8 stateless re-run stays the permanent fallback; `wardline_json` stays
opaque/versioned behind `schema_version`; writes idempotent / per-entity replace;
qualname conformance per Round 1 + the shared corpus; Wardline owns the fresh/stale
decision (Loomweave supplies `current_content_hash`, you decide).

Route-backs welcome on any shape — the capability is the contract.

---

## Addendum (T3.4): rename-stable read-by-SEI

Additive extension, shipped after the original SP9 surface. It closes T3.4 — *a
taint fact survives a rename* — without a primary-key change or a backfill, so it
is safe to ship ahead of the suite-wide SEI cutover (Option A of the two we
weighed; the alternative hard re-key to `sei` PRIMARY KEY was rejected as it would
force write-ordering + a one-shot backfill into the coordinated cutover).

- **Storage:** migration `0006` adds a nullable `sei TEXT` column (+ partial
  index) to `wardline_taint_facts`. Facts stay locator-keyed; `sei` is a *second*,
  rename-stable lookup key.
- **Write:** `POST /api/wardline/taint-facts` accepts an optional opaque `sei` per
  fact. Omitted ⇒ Loomweave resolves it from the alive `sei_bindings` row for the
  resolved locator (batched). Supplied ⇒ stored verbatim. Pre-SEI/unbound ⇒ `null`
  (locator-keyed only). You already hold the SEI from `SeiResolver`, so sending it
  is the fast path.
- **Read:** `POST /api/wardline/taint-facts/by-sei` (body `{ project?, seis: [] }`)
  returns the most-recent fact per SEI regardless of the locator it was written
  under, with the same live whole-file `current_content_hash`. HMAC-gated; SEIs
  opaque (no locator-shape validation).
- **Detection:** gate on the discrete `taint_store.read_by_sei` capability flag,
  **not** `sei.supported` (an older SEI-capable Loomweave lacks this route). Fall
  back to the locator-keyed read when absent.
- **Named window:** a fact written *before* migration `0006` (`sei = null`) whose
  entity is renamed *before* its next re-scan is reachable by neither key until you
  recompute — the freshness gate surfaces it as stale and the rewrite populates the
  `sei`. Self-healing; Loomweave does not backfill historical rows.

Full shapes pinned in [`contracts.md`](./contracts.md#post-apiwardlinetaint-factsby-sei-read-batch-by-sei).
