# Clarion — Integrated Delivery Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`
> (recommended) or `superpowers:executing-plans` to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Date:** 2026-06-02  
**Status:** Authoritative delivery plan  
**Inputs:**
- `docs/superpowers/specs/2026-06-01-clarion-roadmap-to-first-class.md` — the final-form target
- `/home/john/wardline/docs/superpowers/specs/2026-06-02-clarion-priority-brief.md` — the suite-unlocking priority stack
- `/home/john/wardline/docs/superpowers/specs/2026-06-01-loom-stable-entity-identity-conformance.md` — SEI spec (canonical)

**Goal:** Deliver the priority-brief's three-phase critical path (HTTP linkages → SEI authority →
core paradise / dossier) **and** integrate as much standalone-quality work (MCP catalogue
completion, guidance maturity, incremental analysis) as sequencing allows. The suite moves as
fast as Clarion executes; every P0 item is autonomous and starts today.

---

## Decisions baked in (REQ-C-01 and REQ-C-02 — resolved here)

These are the two decisions the priority brief identifies as the last thing between "all four
subsystems reported" and SEI lock. They are Clarion's to make. They are made here.

### REQ-C-01 — Signature schema

**Decision: plugin-declared, versioned, discrete JSON field (`signature TEXT`, *not* unique) on
the `entities` table, stored verbatim and compared by equality.**

The schema for that object is declared per entity-kind in the plugin manifest
(`signature_schemas: { "function": { "v": 1, "fields": ["params", "return_ann"] } }`).
The Rust core stores and compares the JSON string verbatim (no parsing); a changed schema
version counts as a changed signature. The Python plugin emits:
```json
{ "v": 1, "params": ["x: int", "y: str"], "return_ann": "bool" }
```
for functions; `{ "v": 1, "bases": ["Base1", "Base2"] }` for classes; `null` for modules
and other kinds where signature comparison is not meaningful. A `null` signature means the
move case cannot match on signature — that is acceptable and fail-closed.

> **Scope honesty (peer review, 2026-06-02).** Signature is **near-redundant for the v1
> deterministic move case**: that case requires a *byte-identical body*, and the `def foo(x, y):`
> line is part of the body, so identical-body already implies identical-signature. Signature is
> carried because (a) the SEI spec §3 lists it in the move predicate, and (b) it is the
> load-bearing input for the **North-Star fuzzy matcher** (body edited, signature stable). It is
> forward-investment + spec-conformance, **not** a v1 necessity. The plan keeps it; it does not
> pretend the deterministic v1 move depends on it.

This is plugin-declared and versioned. Core never parses it. The manifest declares
`signature_schema_version: 1`; a version bump in the manifest voids cached signatures. The
column is **plain `TEXT`** — signatures are not unique and carry no `UNIQUE` constraint.

### REQ-C-02 — SEI token scheme

**Decision: `clarion:eid:<lowercase-hex(blake3(utf8(locator) ++ 0x00 ++ utf8(mint_run_id)))[:32]>`**,
where `mint_run_id` is the UUID of the run in which the SEI is *minted* (not carried).

> **Correcting the framing (peer review, 2026-06-02).** An earlier draft keyed the token on
> `first_seen_commit` to be a pure function of the entity, preserving byte-identical-run
> determinism. That was wrong on two counts. (1) **`first_seen_commit` is never populated** —
> `crates/clarion-cli/src/analyze.rs` writes `first_seen_commit: None` on every entity; it is a
> schema column the pipeline does not fill. A token keyed on it degenerates to `blake3(locator)`,
> which is exactly the collision-on-reuse flaw the priority brief warned against. (2) The
> pure-function frame is the wrong model: **SEI allocation is inherently stateful** — the matcher
> carries-or-mints by reading prior state (§T2.1). Reproducibility of the SEI *value* comes from
> the persisted `sei_bindings` table, **not** from re-deriving the token. The byte-identical-run
> determinism guarantee covers entity/edge/finding *state*; it does **not** extend to identity
> *values* (two from-scratch runs with different `run_id`s will mint different SEIs for a
> brand-new entity — correct, because in a real re-index the prior binding is *carried*, never
> re-minted).

Properties this satisfies:
- **Collision-free under locator reuse**: a reused locator is only ever *minted* (never carried),
  and minting happens in a later run with a different `mint_run_id` → different token. The matcher
  mints only when it cannot confidently match — precisely the reuse case.
- **Unique within a run**: locators are unique per run, so `blake3(locator ++ run_id)` cannot
  collide between two entities of the same run.
- **No time/RNG component**: `run_id` is an already-allocated per-run UUID (no ad-hoc RNG, which
  Clarion's determinism posture forbids); the token is not time-ordered, so the §8 oracle need not
  assume ordering.
- **Reproducible-given-state**: re-deriving a carried SEI is never required — it is read back from
  `sei_bindings`. The token construction only runs at mint time.

The oracle tests behaviour and opacity, not the internal form. This token satisfies both.

---

## Architecture overview

```
Phase 1 (P0 — autonomous, start now)
  HTTP linkages  ──────────────────────────────────────────► dossier gate (half)
  Prior-index retention  ──────────────────────────────────► SEI matcher + incremental analyze
  REQ-C-01/C-02 decisions (above)  ───────────────────────► SEI lock

  ▼ (SEI lock)

Phase 2 (P1 — after lock; Clarion-autonomous)
  Migration 0005 (sei_bindings + sei_lineage + entities.signature)
  SEI minting + deterministic matcher + lineage (identity lives in sei_bindings,
    NOT in an entities column — entities is cumulative/never-deleted, see §"SEI
    persistence model")
  HTTP wire (resolve / resolve_sei / lineage / _capabilities)
  MCP surface carries SEI (read-time join entities↔sei_bindings)

  ▼

Phase 3 (P2 — closes core paradise)
  Dossier surface documentation + incremental analysis
  Filigree/Wardline cutover coordination (scheduled release)

Parallel track (run alongside Phase 2 as capacity allows)
  MCP catalogue: navigation + inspection + search
  Guidance CLI + maturity
  Plugin manifest published spec
```

---

## SEI persistence model (peer-review correction, 2026-06-02)

> **Why identity is NOT a column on `entities`.** Ground truth: `crates/clarion-storage/src/writer.rs`
> upserts entities with `INSERT ... ON CONFLICT(id) DO UPDATE` and there is **no `DELETE FROM
> entities`** on re-index anywhere in the pipeline — `entities` is a **cumulative, never-pruned**
> table; vanished and renamed entities' rows persist forever. An earlier draft put `sei TEXT UNIQUE`
> on `entities`. That is broken: when the matcher carries an SEI across a rename
> (`m:func:f` → `m:func:g`), the stale `m:func:f` row **still holds that SEI**, so the carry write
> to the new row violates `UNIQUE`. There is also an impedance mismatch — the matcher reasons over
> *last-run* state, but `entities` accumulates *all* runs, so "orphan the vanished entity" has no
> clean target row.

**The fix: identity lives in a dedicated `sei_bindings` table, keyed by SEI, decoupled from the
cumulative `entities` table.** Orphaning is a `status` flip on the binding (not a row collision).
The MCP/HTTP read path joins `entities.id = sei_bindings.current_locator AND status = 'alive'` to
attach an SEI to an entity. `entities` gains only a plain (non-unique) `signature TEXT` column.

| Table | Keyed by | Role | Lifecycle |
|---|---|---|---|
| `sei_prior_index` (0004) | `locator` | last successful run's snapshot (`body_hash`, `signature`) — feeds the matcher and incremental analysis; shape-independent (no SEI column) | rebuilt each run |
| `sei_bindings` (0005) | `sei` | durable identity store: `current_locator`, `body_hash`, `signature`, `status` (`alive`/`orphaned`/`superseded`) — source of truth for `resolve`/`resolve_sei` | cumulative; orphans persist via status |
| `sei_lineage` (0005) | `sei` | append-only event log (`born`/`locator_changed`/`moved`/`orphaned`/`superseded`) | append-only, no backfill |

## Migration plan

| # | File | Contents | Phase |
|---|---|---|---|
| 0004 | `0004_sei_prior_index.sql` | `sei_prior_index` side table (locator → body_hash + signature; **no SEI column** — shape-independent, safe pre-lock) | Phase 1 |
| 0005 | `0005_sei.sql` | `sei_bindings` table (durable identity store) + `sei_lineage` table + plain `entities.signature TEXT` (**no `entities.sei` column**) | Phase 2 |

---

## Testing discipline

The invoked skill (`subagent-driven-development` / `executing-plans`) is test-first. Within each
task, the `write tests` step is **RED before GREEN**: write the failing test, then implement to
green. Where a task lists tests after implementation steps below, treat that as authoring order on
the page, not execution order — the test is written and observed failing first. The
correctness-critical tasks (T2.1 matcher, T2.4 `resolve` rejection, the T1.0/ADR token) are called
out explicitly as test-first.

---

## Phase 1 — P0 Foundation

*Autonomous; starts today; unblocks everything.*

### File map

| File | Responsibility | Tasks |
|---|---|---|
| `docs/clarion/adr/ADR-038-sei-token-and-signature.md` | Records REQ-C-01 + REQ-C-02 decisions as Accepted ADRs | T1.0 |
| `crates/clarion-storage/migrations/0004_sei_prior_index.sql` | Prior-index side table DDL | T1.1 |
| `crates/clarion-storage/src/schema.rs` | Register migration 0004, bump `CURRENT_SCHEMA_VERSION` to 4 | T1.1 |
| `crates/clarion-storage/src/prior_index.rs` | Upsert + read the last-run `locator → body_hash + signature` snapshot (no SEI column — shape-independent) | T1.2 |
| `crates/clarion-storage/src/commands.rs` | `WriterCmd::UpsertPriorIndex` variant | T1.3 |
| `crates/clarion-storage/src/writer.rs` | Actor dispatch arm for prior-index writes | T1.3 |
| `crates/clarion-cli/src/analyze.rs` | Flush prior-index snapshot at end of each successful run | T1.4 |
| `crates/clarion-cli/src/http_read.rs` | `GET /api/v1/entities/{id}/callers`, `.../callees`, batch POST variants; `_capabilities` linkages flag | T1.5, T1.6 |
| `docs/federation/contracts.md` | Pin new linkage routes + linkages capability flag | T1.7 |

---

### T1.0 — ADR-038: token scheme + signature schema decisions ✅ DONE (2026-06-02)

**Doc task, no TDD.** Write it complete and correct the first time; ADRs are immutable once
Accepted.

> **Status: complete.** `docs/clarion/adr/ADR-038-sei-token-and-signature.md` is authored and
> Accepted; the ADR index (`docs/clarion/adr/README.md`) and the Loom glossary
> (`docs/suite/glossary.md`: `SEI` + `locator`, verdict `no clash`) are updated. ADR-038 is
> numbered 038 because **ADR-037 was already taken** (shared error vocabulary). The remaining
> Phase-1 tasks (T1.1–T1.7) and all of Phase 2/3 are still open. The checklist below records what
> the (now-written) ADR carries, for traceability.

- [x] **Step 1: Write ADR-038.** Follow the repo ADR format (see ADR-035 for header shape).
  Must carry:
  - **Status:** Accepted. **Date:** 2026-06-02.
  - **Context:** SEI spec §0.3 requires Clarion to report its REQ-C-01 (signature schema)
    and REQ-C-02 (token scheme) decisions before lock. These are the last open items in
    §0.5.
  - **Decision (token):** `clarion:eid:<lowercase-hex(blake3(utf8(locator) ++ 0x00 ++
    utf8(mint_run_id)))[:32]>`, where `mint_run_id` is the UUID of the run that *mints* the
    SEI — 128 bits of identity space, no time/RNG component. The SEI is stored in
    `sei_bindings` (migration 0005). The oracle tests opacity and behaviour, not the token's
    internal form.
  - **Why `blake3(locator ++ mint_run_id)` and NOT `first_seen_commit`:** `first_seen_commit`
    is **never populated** by the pipeline (`analyze.rs` writes `None`), so a token keyed on it
    degenerates to `blake3(locator)` — the collision-on-reuse flaw the priority brief warned
    against. The correct model is that **SEI allocation is stateful** (the matcher carries-or-mints
    against `sei_bindings`); reproducibility of the SEI *value* comes from the persisted binding,
    not from re-deriving the token. Collision-freedom under locator reuse holds because a reused
    locator is only ever *minted* (in a later run → different `mint_run_id` → different token),
    never carried. The byte-identical-run determinism guarantee covers entity/edge/finding
    *state*, **not** identity values — state this explicitly in the ADR so a future reader does not
    re-introduce a "make the token a pure function" regression.
  - **Decision (signature):** A plugin-declared, versioned JSON object stored verbatim in a
    plain (non-unique) `entities.signature TEXT`. Manifest declares `signature_schemas` per kind.
    Core stores and compares by string equality; schema version bump voids cached comparison.
    `null` for kinds where signature comparison is not meaningful — the move case degrades
    gracefully (no match, fail-closed mint). **Scope note:** signature is near-redundant for the
    v1 deterministic move case (byte-identical body already implies identical signature); it is
    carried for spec-conformance (§3) and as the load-bearing input to the North-Star fuzzy matcher.
  - **Identity persistence (load-bearing):** identity lives in a dedicated `sei_bindings` table,
    **not** as a column on `entities`. `entities` is cumulative and never pruned
    (`ON CONFLICT(id) DO UPDATE`, no `DELETE`), so a `UNIQUE` SEI column would be violated the
    moment a rename carries an SEI while the stale row still holds it. Orphaning is a `status` flip
    on the binding. Record this rationale in the ADR.
  - **Reserved namespace:** the `clarion:eid:` prefix is **reserved** — no plugin locator may
    occupy it. This is what lets `resolve(locator)` reject an SEI-shaped input (REQ-F-02); the ADR
    states the reservation.
  - **Consequences:** migration 0004 adds `sei_prior_index` (no SEI column); migration 0005 adds
    `sei_bindings` + `sei_lineage` + plain `entities.signature`; plugin manifest gains optional
    `signature_schema_version`; `_capabilities` gains `sei: { supported: true, version: 1 }` once
    Phase 2 ships.
  - Reference: SEI spec §1–§3, §0.5, REQ-C-01, REQ-C-02; supersedes the REQ-C-01/02 reasoning
    sketched in the roadmap Appendix A.

- [x] **Step 2: Register in ADR index.** Add ADR-038 row to `docs/clarion/adr/README.md`.

- [x] **Step 3: Loom vocabulary verdict.** `SEI` and `locator` are cross-product-visible; the
  ADR-acceptance rule requires a glossary verdict before Accepted. Both added to
  `docs/suite/glossary.md` with verdict `no clash` (new suite-wide terms, single meaning across all
  four subsystems), ADR-038 as authority.

---

### T1.1 — Migration 0004: `sei_prior_index` table

**Files:**
- Create: `crates/clarion-storage/migrations/0004_sei_prior_index.sql`
- Edit: `crates/clarion-storage/src/schema.rs`

- [ ] **Step 1: Write migration SQL.**

```sql
-- Migration 0004 — last-run entity snapshot (prior-index retention).
--
-- Stores the previous successful run's `locator → body_hash + signature` so
-- (a) incremental analysis can skip unchanged files/entities, and (b) the
-- Phase-2 SEI matcher can detect vanished locators and compare bodies for the
-- move/rename cases. SHAPE-INDEPENDENT: no SEI column, so this is safe to ship
-- before SEI lock. The SEI itself lives in `sei_bindings` (migration 0005),
-- which is the identity source of truth; the matcher reads SEIs from there.
-- Rebuilt each run; cleared by `clarion install --force` (full .clarion/ wipe).
-- Not part of the main entity graph; does not FK into entities.
BEGIN;

CREATE TABLE sei_prior_index (
    locator      TEXT    PRIMARY KEY,  -- the entity's full id string (plugin:kind:qualname)
    body_hash    TEXT    NOT NULL,     -- entities.content_hash at prior-run time
    signature    TEXT,                 -- entities.signature (nullable) at prior-run time
    recorded_at  TEXT    NOT NULL      -- ISO-8601 UTC; prior-run completion timestamp
);

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (4, '0004_sei_prior_index', datetime('now'));

COMMIT;
```

- [ ] **Step 2: Register the migration in `schema.rs`.** Add the new `Migration` entry
  and bump `CURRENT_SCHEMA_VERSION` to 4. The compile-time assert will enforce that the
  constant matches the last migration's version.

---

### T1.2 — `prior_index.rs`: storage helpers

**Files:**
- Create: `crates/clarion-storage/src/prior_index.rs`
- Edit: `crates/clarion-storage/src/lib.rs` — re-export public items

- [ ] **Step 1: Write `prior_index.rs`.** Implement:
  - `pub struct PriorIndexEntry { pub locator: String, pub body_hash: String, pub signature: Option<String> }` (no SEI — identity is in `sei_bindings`)
  - `pub fn upsert_prior_index_entry(conn: &Connection, entry: &PriorIndexEntry) -> Result<()>` — INSERT OR REPLACE.
  - `pub fn load_prior_index(conn: &Connection) -> Result<HashMap<String, PriorIndexEntry>>` — full table load; called once at start of re-index for the incremental-analysis body_hash compare and (Phase 2) as a matcher input.
  - `pub fn clear_prior_index(conn: &Connection) -> Result<()>` — DELETE FROM; called by `--force` path (if .clarion/ is wiped, this never runs — but it should exist for explicit reset).

- [ ] **Step 2: Re-export from `lib.rs`.**

- [ ] **Step 3: Write unit tests** in `prior_index.rs`:
  - Upsert round-trip: insert, reload, assert values match.
  - Upsert is idempotent: upsert same locator twice (second with different body_hash) → only latest row remains.

---

### T1.3 — WriterCmd: UpsertPriorIndex

**Files:**
- Edit: `crates/clarion-storage/src/commands.rs`
- Edit: `crates/clarion-storage/src/writer.rs`

- [ ] **Step 1: Add `WriterCmd::UpsertPriorIndex(PriorIndexEntry)` variant** to
  `commands.rs`, following the pattern of existing variants (e.g. `UpsertWardlineTaintFact`).

- [ ] **Step 2: Add dispatch arm** in `writer.rs` that calls `upsert_prior_index_entry`.
  Use the `query_time_write` path (same as summary cache writes — not tied to a run
  transaction boundary).

---

### T1.4 — Analysis pipeline: flush prior index after each run

**Files:**
- Edit: `crates/clarion-cli/src/analyze.rs`

The prior index is written at successful run completion (Phase 8 / post-emission). It
replaces the previous run's snapshot atomically: we DELETE all rows and re-insert the
current run's entities. This ensures the prior index is always exactly "the last
successful run."

- [ ] **Step 1: After Phase 8 emission and before final stats write**, collect the
  current run's entities (locator, content_hash, signature) from the DB and write them
  to `sei_prior_index` via the writer actor. Use a `REPLACE INTO` / upsert-all approach:
  build a `Vec<PriorIndexEntry>` from the run's entity set, send one
  `WriterCmd::UpsertPriorIndex` per entry. After the flush, send a single DELETE for
  any locators in the prior index that were not in the current run (deletions detected
  from the entity-set diff, which already exists at Phase 7).

- [ ] **Step 2: Write integration test** (can share the existing `tempdir` pattern from
  `tests/install.rs`): run two back-to-back analyzes on a small fixture; after the second,
  assert that `sei_prior_index` contains exactly the current run's entities (no stale rows
  from the first run that were removed in the second).

---

### T1.5 — HTTP linkages: callers and callees

**Files:**
- Edit: `crates/clarion-cli/src/http_read.rs`

The storage layer already provides `call_edges_targeting` (callers) and `call_edges_from`
(callees) in `clarion-storage/src/query.rs`. These need HTTP wrappers with pagination and
confidence-tier filtering.

- [ ] **Step 1: Add `LinkageEntry` response struct** (serializable):
  ```rust
  struct LinkageEntry {
      entity_id: String,
      confidence: String, // "resolved" | "ambiguous" | "inferred"
      call_site_count: usize,
  }
  ```

- [ ] **Step 2: Add `GET /api/v1/entities/{entity_id}/callers`** handler. Parameters:
  - `confidence` (optional): `resolved` | `ambiguous` | `inferred` | `all` (default `all`)
  - `limit` (optional, default 50, max 200)
  - `offset` (optional, default 0)
  Response: `{ entity_id, callers: [LinkageEntry], total: N, truncated: bool }`.
  Uses `call_edges_targeting` from `query.rs`.

- [ ] **Step 3: Add `GET /api/v1/entities/{entity_id}/callees`** handler. Same shape,
  using `call_edges_from`.

- [ ] **Step 4: Add `POST /api/v1/entities/callers:batch-get`** handler. Request body:
  `{ entity_ids: [String], confidence?: String, limit?: u32 }` (max 50 entity_ids per
  batch). Returns `{ results: { [entity_id]: [LinkageEntry] } }`. Cap batch size at a
  named constant (`LINKAGES_BATCH_MAX = 50`).

- [ ] **Step 5: Add `POST /api/v1/entities/callees:batch-get`** handler — same shape.

- [ ] **Step 6: Register all four routes** in `router()`. These routes are **protected**
  (HMAC-gated) — same `route_layer` as the existing `/api/v1/files` routes.

- [ ] **Step 7: Write HTTP tests** (mirror the existing wardline-route test patterns):
  - Callers for known entity returns correct callers.
  - Callees for known entity returns correct callees.
  - Unknown entity_id → 404.
  - Confidence-tier filter works correctly.
  - Batch-get with mixed known/unknown entity_ids.
  - Batch exceeding `LINKAGES_BATCH_MAX` → 400.

---

### T1.6 — `_capabilities`: add linkages flag

**Files:**
- Edit: `crates/clarion-cli/src/http_read.rs`

- [ ] **Step 1: Add `linkages: LinkagesCapability` to `CapabilitiesResponse`**:
  ```rust
  struct LinkagesCapability {
      http: bool,   // true once T1.5 ships
  }
  ```
  Set `http: true`.

- [ ] **Step 2: Add test** that `GET /api/v1/_capabilities` returns `linkages: { http: true }`.

---

### T1.7 — Federation contracts: pin linkage routes

**Files:**
- Edit: `docs/federation/contracts.md`

- [ ] **Step 1: Add a Linkages section** to `contracts.md` documenting the four new routes,
  their request/response schemas, confidence-tier vocabulary, pagination parameters, and the
  `linkages.http` capability flag. Follow the existing route-pinning format.

---

## Phase 2 — P1 SEI Authority

*Gated on SEI lock (which Phase 1's decisions unblock). Clarion-autonomous once locked.*

### File map

| File | Responsibility | Tasks |
|---|---|---|
| `crates/clarion-storage/migrations/0005_sei.sql` | `sei_bindings` + `sei_lineage` tables; plain `entities.signature` (no `entities.sei`) | T2.0 |
| `crates/clarion-storage/src/schema.rs` | Register 0005, bump to 5 | T2.0 |
| `crates/clarion-storage/src/sei.rs` | Minting, matcher, binding + lineage helpers | T2.1 |
| `crates/clarion-storage/src/commands.rs` | `WriterCmd::UpsertSeiBinding`, `OrphanSeiBinding`, `SetEntitySignature`, `AppendSeiLineage` | T2.2 |
| `crates/clarion-storage/src/writer.rs` | Dispatch arms | T2.2 |
| `crates/clarion-cli/src/analyze.rs` | SEI mint pass (post-extraction); matcher on re-index | T2.3 |
| `crates/clarion-cli/src/http_read.rs` | `resolve`, `resolve_sei`, `lineage`; `_capabilities` sei flag | T2.4 |
| `crates/clarion-mcp/src/lib.rs` + tool handlers | Return `sei` alongside `entity_id` (read-time join to `sei_bindings`) | T2.5 |
| `docs/federation/contracts.md` | Pin SEI routes + capability contract | T2.6 |

---

### T2.0 — Migration 0005: `sei_bindings`, `sei_lineage`, `entities.signature`

**Files:**
- Create: `crates/clarion-storage/migrations/0005_sei.sql`
- Edit: `crates/clarion-storage/src/schema.rs`

- [ ] **Step 1: Write migration SQL.** Note: **no `entities.sei` column** — identity lives in
  `sei_bindings` because `entities` is cumulative/never-pruned (see §"SEI persistence model").

```sql
-- Migration 0005 — SEI identity store + lineage event log.
--
-- sei_bindings:       the durable identity store, keyed by SEI. Decoupled from the
--                     cumulative `entities` table (which is never pruned), so carrying
--                     an SEI across a rename can never collide with a stale entity row.
--                     Orphaning is a `status` flip, not a deletion.
-- entities.signature: plugin-declared, versioned JSON; PLAIN TEXT, not unique.
-- sei_lineage:        append-only event log for SEI identity events.

BEGIN;

ALTER TABLE entities ADD COLUMN signature TEXT;

CREATE TABLE sei_bindings (
    sei             TEXT    PRIMARY KEY,   -- clarion:eid:<hex> (opaque)
    current_locator TEXT,                  -- current address; the alive binding's entity id
    body_hash       TEXT,                  -- content_hash at last (re)bind
    signature       TEXT,                  -- signature at last (re)bind
    status          TEXT    NOT NULL CHECK(status IN ('alive','orphaned','superseded')),
    born_run_id     TEXT    NOT NULL,
    updated_run_id  TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL        -- ISO-8601 UTC
);

-- At most ONE alive binding per locator. Partial unique index — orphaned/superseded
-- bindings may share a former locator without colliding.
CREATE UNIQUE INDEX ux_sei_alive_locator
    ON sei_bindings(current_locator)
    WHERE status = 'alive' AND current_locator IS NOT NULL;

CREATE TABLE sei_lineage (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    sei          TEXT    NOT NULL,
    event        TEXT    NOT NULL CHECK(event IN
                     ('born','locator_changed','moved','orphaned','superseded')),
    old_locator  TEXT,            -- set for locator_changed, moved, orphaned
    new_locator  TEXT,            -- set for locator_changed, moved, superseded
    run_id       TEXT    NOT NULL,
    recorded_at  TEXT    NOT NULL  -- ISO-8601 UTC
);

CREATE INDEX ix_sei_lineage_sei ON sei_lineage(sei);

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (5, '0005_sei', datetime('now'));

COMMIT;
```

- [ ] **Step 2: Register migration 0005 in `schema.rs`, bump `CURRENT_SCHEMA_VERSION` to 5.**

---

### T2.1 — `sei.rs`: minting and matching

**Files:**
- Create: `crates/clarion-storage/src/sei.rs`
- Edit: `crates/clarion-storage/src/lib.rs`

**Test-first task.** Identity is correctness-critical; write each test RED before implementing.

- [ ] **Step 1 (RED): write the `mint_sei` test**, then implement.
  `pub fn mint_sei(locator: &str, mint_run_id: &str) -> String` — REQ-C-02: `clarion:eid:` +
  lowercase hex of `blake3(locator ++ 0x00 ++ mint_run_id)` truncated to 32 hex chars (128 bits).
  Tests: same `(locator, run_id)` → same token; different `run_id` for the same locator → different
  token (the collision-on-reuse guard); output always carries the reserved `clarion:eid:` prefix.

- [ ] **Step 2: Write the binding-state helpers** in `sei.rs` (the matcher reads/writes these,
  not an `entities` column):
  - `pub fn alive_binding_for_locator(conn, locator) -> Result<Option<SeiBinding>>`
  - `pub fn alive_bindings_snapshot(conn) -> Result<HashMap<String, SeiBinding>>` (current_locator → binding) — the matcher's "what is currently bound" view
  - `SeiBinding { sei, current_locator, body_hash, signature, status }`

- [ ] **Step 3 (RED): write the matcher tests, then implement `rebind_or_mint`.**
  `pub fn rebind_or_mint(new_entity: &NewEntityDescriptor, alive: &HashMap<String, SeiBinding>, prior: &HashMap<String, PriorIndexEntry>, git_renames: &[GitRename], mint_run_id: &str) -> SeiDecision`
  where:
  ```rust
  enum SeiDecision {
      Carry { sei: String, event: Option<LineageEvent> }, // locator present, or rename/move match
      Mint  { sei: String },                              // new entity (clarion:eid minted)
  }
  // Orphaning is computed SEPARATELY (Step 5) by diffing the alive set against the
  // current run's locator set — it is a property of vanished bindings, not of a new entity.
  ```
  Per-entity logic (SEI spec §3):
  1. `alive` contains `new_entity.locator` → `Carry { sei: alive[loc].sei, event: None }`. If
     `body_hash` differs, that is the **content axis** — not an identity event.
  2. `new_entity.locator` not in `alive` (new this run), but a `GitRename` maps a vanished alive
     binding's `current_locator → new_entity.locator` AND that binding's `body_hash` is unchanged →
     `Carry { sei, event: Some(LocatorChanged) }`. OR a vanished alive binding has identical
     `body_hash` (+ identical signature, where present) at the new locator → `Carry { sei,
     event: Some(Moved) }`.
  3. Neither → `Mint { sei: mint_sei(&new_entity.locator, mint_run_id) }` + `born` lineage.
  Matcher tests: locator unchanged → carry, no event; git-rename + identical body → carry,
  `locator_changed`; move (body+sig identical, new locator) → carry, `moved`; rename **with** body
  edit → fail-closed `Mint` (no carry); brand-new locator → `Mint`.

- [ ] **Step 4: Write `GitRename` struct** and a typed `GitRenameSource` trait
  (`fn renames_since(&self, base_commit: &str) -> Vec<GitRename>`). v1 impl `ShellGitRenameSource`
  shells out to `git diff --name-status -M` (REQ-C-05 — typed interface first; legis supplies a
  concrete impl later with no model change).

- [ ] **Step 5 (RED): write the orphan-detection test, then implement.**
  `pub fn orphaned_bindings(alive: &HashMap<String, SeiBinding>, current_locators: &HashSet<String>, rematched: &HashSet<String>) -> Vec<String>` — returns SEIs of alive bindings whose
  `current_locator` is absent from the current run AND was not rematched by a rename/move carry.
  These flip to `status='orphaned'` with an `orphaned` lineage event. Test: a vanished, unmatched
  binding is returned; a vanished-but-rematched binding is NOT; a still-present binding is NOT.

---

### T2.2 — WriterCmd: SEI writes

**Files:**
- Edit: `crates/clarion-storage/src/commands.rs`
- Edit: `crates/clarion-storage/src/writer.rs`

- [ ] **Step 1: Add `WriterCmd::UpsertSeiBinding(SeiBindingRecord)`** — INSERT OR REPLACE into
  `sei_bindings` (mint a new alive binding, or update a carried binding's `current_locator` /
  `body_hash` / `signature` / `updated_run_id` / `updated_at`). `SeiBindingRecord` is
  `{ sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at }`.

- [ ] **Step 2: Add `WriterCmd::OrphanSeiBinding { sei: String, run_id: String, recorded_at: String }`** — sets `status='orphaned'` on the binding (and clears nothing else; `current_locator` is
  kept for audit).

- [ ] **Step 3: Add `WriterCmd::SetEntitySignature { entity_id: String, signature: Option<String> }`** — sets the plain `entities.signature` column for an existing entity row (the
  matcher input; separate from identity, which is in `sei_bindings`).

- [ ] **Step 4: Add `WriterCmd::AppendSeiLineage(SeiLineageEntry)`** — inserts into `sei_lineage`.
  `SeiLineageEntry` is `{ sei, event, old_locator, new_locator, run_id, recorded_at }`. INSERT only
  (append-only; no UPDATE path — REQ-L-01).

- [ ] **Step 5: Add dispatch arms in `writer.rs`** for all four. The alive-locator partial unique
  index means a carry that moves `current_locator` must run after the prior holder is orphaned or
  re-pointed within the same write batch — order the writes so the unique index never transiently
  doubles up (orphan/repoint first, then the carry).

---

### T2.3 — Analysis pipeline: SEI mint pass

**Files:**
- Edit: `crates/clarion-cli/src/analyze.rs`

This runs as a new sub-phase between Phase 1.5 (enrichment) and Phase 2 (graph completion):
"Phase 1.75 — SEI rebinding." It requires the prior index (already populated by Phase 1),
the git-rename signal, and the current run's entity list.

- [ ] **Step 1: After structural extraction (Phase 1), before graph completion (Phase 2)**,
  run the SEI mint pass. The current run's `run_id` is the `mint_run_id` for any SEI minted here:
  1. Snapshot the current alive bindings: `alive_bindings_snapshot(&conn)`.
  2. Load the prior index (`load_prior_index(&conn)`) for body/signature comparison.
  3. Collect git renames since the previous run's `last_seen_commit` via `ShellGitRenameSource`.
  4. For each entity in the current run, call `rebind_or_mint(.., mint_run_id = run_id)`. Track the
     set of carried/rematched locators.
  5. Send `WriterCmd::SetEntitySignature` for every current entity (matcher input for next run).
  6. Send `WriterCmd::UpsertSeiBinding` for every current entity — minted (`born`) or carried
     (update `current_locator`/`body_hash`/`signature`/`updated_run_id`). **Order:** process
     orphans (Step 8) and rename/move re-points before the corresponding fresh carries so the
     alive-locator unique index never transiently doubles up (T2.2 Step 5).
  7. For each `Carry` with a lineage event, and each `Mint`, send
     `WriterCmd::AppendSeiLineage(...)` (`locator_changed` / `moved` / `born`).
  8. Compute orphans via `orphaned_bindings(alive, current_locators, rematched)`; for each, send
     `WriterCmd::OrphanSeiBinding` + `AppendSeiLineage(orphaned)`.

- [ ] **Step 2: Update the prior-index flush** (T1.4) to also write `signature` alongside
  `body_hash`, now that `entities.signature` is populated.

- [ ] **Step 3: Add a `--no-sei` flag** to `clarion analyze` that skips the mint pass —
  escape hatch for diagnostic runs on pre-migration DBs.

- [ ] **Step 4: Determinism note.** Document in the code that SEI *values* are not part of the
  byte-identical-run guarantee (two from-scratch runs mint different SEIs); the guarantee is that
  carry/mint *decisions* are deterministic given the same `sei_bindings` + source. Add a test that
  a second back-to-back run against unchanged source **carries** (does not re-mint) every SEI.

---

### T2.4 — HTTP wire contract: resolve, resolve_sei, lineage

**Files:**
- Edit: `crates/clarion-cli/src/http_read.rs`

- [ ] **Step 1: Add storage helpers** in `clarion-storage/src/sei.rs`. Resolution reads
  `sei_bindings` (the identity source of truth), joining to `entities` only for `content_hash`:
  - `pub fn resolve_locator(conn, locator) -> Result<Option<SeiRecord>>` — find the alive binding
    with `current_locator = locator`; return `{ sei, current_locator, content_hash, alive: true }`.
  - `pub fn resolve_sei(conn, sei) -> Result<SeiLookupResult>` — PK lookup in `sei_bindings`; if
    `status='alive'` return the alive record; otherwise return `{ alive: false, lineage }` from
    `sei_lineage`.
  - `pub fn sei_lineage(conn, sei) -> Result<Vec<SeiLineageEntry>>`

- [ ] **Step 2: Add `POST /api/v1/identity/resolve`** handler. Input: `{ locator: String }`.
  **Validation (REQ-F-02, fail-closed):** reject any input beginning with the reserved
  `clarion:eid:` prefix (it is an SEI, not a locator) with a documented `"not a valid locator"`
  400 error — **do not** rely on a colon count, since an SEI `clarion:eid:<hex>` has the same two
  colons a locator does. Also reject inputs that are not `{plugin}:{kind}:{qualname}`-shaped
  (3 non-empty colon-separated segments). Returns `{ sei, current_locator, content_hash,
  alive: true }` or `{ alive: false }`. The reserved-prefix rule is what makes the idempotent,
  resumable backfill safe (an already-migrated SEI is rejected, never mis-resolved).

- [ ] **Step 3: Add `GET /api/v1/identity/sei/{sei}`** handler. Returns
  `{ current_locator, content_hash, alive: true }` or
  `{ alive: false, lineage: [...] }`.

- [ ] **Step 4: Add `GET /api/v1/identity/lineage/{sei}`** handler. Returns ordered
  event list.

- [ ] **Step 5: Add batch variant `POST /api/v1/identity/resolve:batch`** — same as
  the batch-get pattern used for files and taint facts.

- [ ] **Step 6: Update `_capabilities`** to add `sei: { supported: true, version: 1 }`.

- [ ] **Step 7: Write tests**:
  - `resolve` with known locator → correct SEI returned.
  - `resolve` with an SEI-shaped string → 400 "not a valid locator" (REQ-F-02).
  - `resolve_sei` for orphaned SEI → `alive: false` + lineage.
  - `lineage` returns correct event sequence for rename scenario.
  - `_capabilities` includes `sei` flag.

---

### T2.5 — MCP surface: carry SEI alongside entity_id

**Files:**
- Edit: `crates/clarion-mcp/src/lib.rs` and tool handler modules

Per invariant §4 of the priority brief: every surface that returns an identity for use as
a binding key carries the SEI. No "MCP locator exception."

- [ ] **Step 1: Add `sei: Option<String>` to all MCP tool response types** that currently
  return `entity_id`. The field is `null` on pre-SEI DBs (graceful degrade).

- [ ] **Step 2: Populate `sei`** via a read-time join `entities.id = sei_bindings.current_locator
  AND sei_bindings.status = 'alive'` (there is no `entities.sei` column) in all relevant query
  paths: `entity_at`, `find_entity`, `callers_of`, `call_sites`, `neighborhood`,
  `subsystem_members`, `summary`, `issues_for`, `execution_paths_from`. A missing binding
  (pre-SEI DB, or an orphaned locator) yields `sei: null` — graceful degrade.

- [ ] **Step 3: Add `orientation_pack` and `project_status` sei metadata** — these should
  reflect whether the current index has SEI populated.

- [ ] **Step 4: Update the `clarion-workflow` skill** (embedded in `clarion-mcp/assets/`) 
  to document that MCP tool responses carry `sei` alongside `entity_id`, and that `sei` is
  the key to use for cross-tool bindings.

---

### T2.6 — Federation contracts and cutover coordination

**Files:**
- Edit: `docs/federation/contracts.md`
- Edit: `CHANGELOG.md`

- [ ] **Step 1: Pin the SEI routes** (`/api/v1/identity/resolve`, `/api/v1/identity/sei/{sei}`,
  `/api/v1/identity/lineage/{sei}`, batch variant) in `contracts.md`, including the REQ-F-02
  rejection contract.

- [ ] **Step 2: Document the hard cutover protocol** in `contracts.md` or a new
  `docs/federation/sei-migration-playbook.md`: Clarion ships SEI, mints SEIs for all
  entities, Filigree backfill re-keys `clarion_entity_id` from locators to SEIs,
  Wardline client-layer update keys taint facts on SEI. Single coordinated release.
  Unresolvable orphans flagged for human review, never silently dropped.

---

## Phase 3 — P2 Core Paradise

*Follows Phase 2; closes the suite's core loop.*

### T3.1 — Incremental analysis (skip unchanged files)

**Files:**
- Edit: `crates/clarion-cli/src/analyze.rs`
- Edit: `crates/clarion-storage/src/query.rs`

The prior-index retention from Phase 1 (T1.1–T1.4) provides the prerequisite: we have a
per-locator `body_hash` from the previous run. File-level incremental skipping extends this
to file entities.

- [ ] **Step 1: Add `fn previously_analyzed_files(conn: &Connection) -> HashMap<String, String>`**
  in `query.rs` — returns `{ file_path → content_hash }` for files in the prior index.

- [ ] **Step 2: In Phase 1 (structural extraction)**, before dispatching `analyze_file` for
  each file: check if the file's current content hash matches the prior-run hash. If so,
  skip dispatch and re-use the prior-run entities for that file (they are already in the DB
  from the last run's upsert). Emit a `skipped_unchanged` progress event.

- [ ] **Step 3: Add `skipped_files: N` to `stats.json`** so operators can see how many
  files were skipped.

- [ ] **Step 4: Guard the SEI orphan-detection interaction (load-bearing).** The SEI mint pass
  (T2.3 Step 8) computes orphans as "alive bindings whose `current_locator` is absent from the
  **current run's locator set**." When incremental skipping is on, a skipped-unchanged file's
  entities are *still present* — they were simply not re-parsed — so the current-run locator set
  passed to `orphaned_bindings` MUST be the union of (re-analyzed entities) ∪ (entities of
  skipped-unchanged files), read from the prior index. Failing to include skipped entities would
  **falsely orphan every entity in every unchanged file** — a silent, catastrophic regression. Add
  a regression test: analyze, then re-analyze with one file changed; assert that entities in the
  *unchanged* files retain their SEI and are NOT orphaned.

- [ ] **Step 5: Update `--resume`** semantics to note that with prior-index retention,
  a fresh re-run after an interrupted run is already near-incremental (only changed files
  re-analyzed). Existing `--resume` for mid-run recovery remains unchanged.

---

### T3.2 — Dossier participation surface

**Files:**
- Create: `docs/superpowers/specs/2026-06-02-clarion-dossier-participation.md`
- Edit: `docs/federation/contracts.md`

Clarion does not assemble the dossier envelope (Wardline does). Clarion contributes its
slice over HTTP. This task makes the contract explicit.

- [ ] **Step 1: Write the participation spec** documenting exactly which Clarion endpoints
  the dossier assembler calls and what it gets back: `resolve(locator)` → SEI,
  `/api/v1/entities/{id}/callers` + `/callees` → structural linkages,
  `GET /api/v1/files/{path}` → file context,
  `issues_for` (MCP) or equivalent HTTP → Filigree associations.
  This is the surface the Wardline dossier design
  (`2026-06-01-wardline-loom-entity-dossier-design.md`) consumes.

- [ ] **Step 2: Pin any new HTTP endpoints** this reveals in `contracts.md`.

---

## Parallel track — MCP catalogue and guidance maturity

*Run alongside Phase 2 as capacity allows. High value for standalone consult mode.*

> **Scope cut (noted for honesty).** This integrated plan carries the roadmap's MCP-catalogue and
> guidance items but **defers** the roadmap's other Half-1 operational-quality items —
> `clarion doctor` DB/plugin/config extensions and cost-estimate accuracy validation. They are not
> on the suite critical path and not cut for cause; they re-enter when the P0–P2 path is clear. The
> roadmap remains the full Half-1 backlog; this plan is the critical-path-first slice of it.

### MCP-P1 — Navigation tools

**Files:**
- Edit: `crates/clarion-mcp/src/lib.rs`

- [ ] `goto(entity_id)` — set session cursor
- [ ] `goto_path(path, line?)` — resolve file+line to entity, set cursor
- [ ] `back()` — pop breadcrumb
- [ ] `zoom_out()` — navigate to parent
- [ ] `breadcrumbs()` — return navigation history
- [ ] `session_info()` — return current cursor, scope lens, session cost

---

### MCP-P2 — Inspection tools

**Files:**
- Edit: `crates/clarion-mcp/src/lib.rs`

- [ ] `source(entity_id?)` — return source range content for entity (defaults to cursor)
- [ ] `metadata(entity_id?)` — return full entity metadata including wardline, tags, properties
- [ ] `findings_for(entity_id?, filter?)` — return findings on entity with optional filter
- [ ] `set_scope_lens(lens)` — set session scope lens (`Structural | Subsystem | Wardline`)

---

### MCP-P3 — Search tools

**Files:**
- Edit: `crates/clarion-mcp/src/lib.rs`

- [ ] `find_by_tag(tag, scope?)` — entities matching a tag
- [ ] `find_by_kind(kind, scope?)` — entities of a specific kind
- [ ] `find_by_wardline(tier?, group?)` — entities with wardline metadata matching
- [ ] `recently_changed(since?, scope?)` — entities with recent `last_seen_commit`
- [ ] `high_churn(limit?, scope?)` — entities with high git churn

---

### MCP-P4 — Guidance CLI

**Files:**
- Edit: `crates/clarion-cli/src/main.rs` and new `crates/clarion-cli/src/guidance.rs`

- [ ] `clarion guidance create --match <pattern> --scope-level <level>` — create guidance sheet
- [ ] `clarion guidance list [--for-entity <id>] [--stale] [--expired]`
- [ ] `clarion guidance show <id>`
- [ ] `clarion guidance edit <id>` — open in `$EDITOR`
- [ ] `clarion guidance promote <filigree_obs_id>` — promote Filigree observation to sheet

---

## Suite invariants throughout

Per the priority brief §4 — apply to every task above:

1. **Opacity.** SEI is opaque. `resolve` and `resolve_sei` are the only legitimate entry
   points. Nothing parses `clarion:eid:…` internally.
2. **No binding keyed on a locator on any surface.** MCP and HTTP both carry SEI once Phase
   2 ships. No MCP locator exception.
3. **Fail-closed / no false-green.** When the matcher cannot prove sameness, it mints and
   orphans. `unknown` and `orphan` are never suppressed or silently patched.
4. **Typed git-rename interface.** `ShellGitRenameSource` implements a typed trait;
   `legis` supplies a second impl later without touching the model.
5. **Lineage is append-only with no backfill path.** `sei_lineage` has no UPDATE path;
   only INSERT. No Clarion-side hash-chain in v1.
6. **Prior index is a side table.** Not a retained prior `clarion.db`. Nothing inflates it.
7. **No dossier assembly.** Clarion contributes its slice; the consumer composes.

---

## Definition of done

| Milestone | Done when |
|---|---|
| **Phase 1 complete** | HTTP linkages live and tested; `sei_prior_index` populated after every run; `_capabilities` reflects `linkages: { http: true }`; ADR-038 accepted |
| **SEI lock** | REQ-C-01 and REQ-C-02 decisions (Phase 1 / ADR-038) submitted to SEI spec §0.5 intake; all four subsystems reported; oracle spec finalized |
| **Phase 2 complete** | Every alive entity has an `alive` `sei_bindings` row after analysis; matcher handles rename/move/orphan cases per test suite; a back-to-back unchanged re-run **carries** (never re-mints) SEIs; HTTP identity routes live with the REQ-F-02 `clarion:eid:` rejection; MCP responses carry SEI via the binding join; `_capabilities` reflects `sei: { supported: true, version: 1 }` |
| **Migration cutover** | Coordinated release with Filigree + Wardline; all stored locators re-keyed to SEI; orphaned locators flagged; no mixed-format state |
| **Phase 3 complete** | Incremental analysis skips unchanged files; dossier participation contract pinned; `dossier(entity)` achievable by the Wardline assembler using Clarion's HTTP surface |
| **Core paradise** | `dossier(entity)` returns complete, freshness-stamped, SEI-keyed envelope for a renamed function without orphaning its Wardline facts or Filigree associations |
