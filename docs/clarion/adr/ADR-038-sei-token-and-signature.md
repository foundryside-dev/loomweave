# ADR-038: SEI token scheme, signature schema, and identity persistence

**Status**: Accepted
**Date**: 2026-06-02
**Deciders**: qacona@gmail.com (with Claude)
**Context**: The Loom suite's Stable Entity Identity (SEI) standard — `/home/john/wardline/docs/superpowers/specs/2026-06-01-loom-stable-entity-identity-conformance.md` — names Clarion as the identity **authority/implementer** and records two decisions as OPEN, owned by Clarion, in its §0.5 pre-lock intake: **REQ-C-01** (signature schema) and **REQ-C-02** (token scheme). These are the last items between "all four subsystems reported" and SEI lock. This ADR resolves both, plus the identity-persistence shape they imply, so the SEI standard can lock.
**Relates to**: [ADR-003](./ADR-003-entity-id-scheme.md) (the `{plugin}:{kind}:{qualname}` id this ADR demotes from *identity* to *locator*), [ADR-018](./ADR-018-identity-reconciliation.md) (qualname reconciliation — subsumed as an *identity* mechanism by SEI per the SEI spec §0.2), [ADR-011](./ADR-011-writer-actor-concurrency.md) (writer-actor; the SEI writes route through it), [ADR-029](./ADR-029-entity-associations-binding.md) (entity associations — transport unchanged; the identity value carried becomes an SEI).
**Companion plan**: [`docs/superpowers/plans/2026-06-02-clarion-integrated-delivery-plan.md`](../../superpowers/plans/2026-06-02-clarion-integrated-delivery-plan.md) — the task-level implementation.

## Summary

The qualname id (`{plugin}:{kind}:{qualname}`) is a fine *address* and a poor *identity* — rename and move change it, orphaning every cross-tool binding keyed on it. The SEI standard introduces a durable opaque surrogate identity (SEI), demotes the qualname to a resolvable *locator*, and re-establishes identity via a deterministic matcher at each re-index. This ADR fixes the three Clarion-owned shape decisions that standard left open:

1. **Token (REQ-C-02):** `clarion:eid:<lowercase-hex(blake3(utf8(locator) ++ 0x00 ++ utf8(mint_run_id)))[:32]>`, where `mint_run_id` is the UUID of the run that *mints* the SEI.
2. **Signature (REQ-C-01):** a plugin-declared, versioned JSON object stored verbatim in a plain (non-unique) `entities.signature TEXT`, compared by string equality.
3. **Identity persistence:** identity lives in a dedicated `sei_bindings` table keyed by SEI — **not** as a column on `entities` — because `entities` is cumulative and never pruned, which a `UNIQUE` SEI column cannot survive.

It also reserves the `clarion:eid:` locator namespace so `resolve(locator)` can fail-closed-reject an SEI-shaped input.

## Context

### What the SEI standard fixes, and what it left to Clarion

Verified against `clarion`/`clarion-storage` source on 2026-06-01/02:

- Clarion's entity id is derived from name + module path and upserted `ON CONFLICT(id)`. A rename/move changes the id; ADR-003 even named a deferred `EntityAlias` seam for exactly this. SEI is the product-grade form of that seam.
- The SEI standard settles the *track* (one canonical identity interface, it is SEI, fail-closed re-binding, deterministic v1 matcher, Clarion is the authority) and leaves the *shape* open until lock — including the two Clarion-owned items this ADR resolves.

### Ground truth that shaped these decisions (peer review, 2026-06-02)

An initial draft of the companion plan was reviewed against the actual code. Two intended decisions were found broken; this ADR records the corrected forms and the evidence, so a future reader does not regress them:

- **`first_seen_commit` is never populated.** `crates/clarion-cli/src/analyze.rs` writes `first_seen_commit: None` on every entity (the only non-`None` values in the tree are in unit tests). A token keyed on `first_seen_commit` therefore degenerates to `blake3(locator)` — the collision-on-reuse flaw the SEI priority brief (§3) explicitly warned against. The token must not depend on it.
- **`entities` is cumulative and never pruned.** `crates/clarion-storage/src/writer.rs` upserts `INSERT ... ON CONFLICT(id) DO UPDATE`; there is no `DELETE FROM entities` on re-index. Vanished and renamed entities' rows persist forever. A `sei TEXT UNIQUE` column on `entities` is therefore unworkable: carrying an SEI across a rename collides with the stale row that still holds it.

## Decision

### 1. SEI token (REQ-C-02)

**`clarion:eid:<lowercase-hex(blake3(utf8(locator) ++ 0x00 ++ utf8(mint_run_id)))[:32]>`** — 128 bits of identity space; `mint_run_id` is the minting run's UUID.

The correct model is that **SEI allocation is stateful**: the matcher carries-or-mints by reading the persisted `sei_bindings` table. Reproducibility of the SEI *value* comes from that persisted binding, **not** from re-deriving the token as a pure function of the entity. Consequences:

- **Collision-free under locator reuse.** A reused locator is only ever *minted* (never carried — the matcher mints precisely when it cannot confidently match), and minting happens in a later run with a different `mint_run_id` → a different token.
- **Unique within a run.** Locators are unique per run, so two entities of the same run cannot collide.
- **No time/RNG component.** `run_id` is an already-allocated per-run UUID — no ad-hoc RNG (which Clarion's determinism posture forbids), and not time-ordered, so the SEI conformance oracle (SEI spec §8) need not assume a time-ordered token.
- **Determinism boundary (state this explicitly so it is not regressed).** Clarion's byte-identical-run guarantee (seeded RNG, `temperature: 0`, `RecordingProvider`) covers entity/edge/finding **state**. It does **not** extend to identity **values**: two from-scratch runs with different `run_id`s mint different SEIs for a brand-new entity. This is correct — in a real re-index the prior binding is *carried*, never re-minted.

SEI is **opaque** on the wire and in storage; consumers MUST NOT parse it (the same discipline ADR-003 already applies to the entity id).

### 2. Signature schema (REQ-C-01)

A **plugin-declared, versioned JSON object**, stored verbatim in a plain `entities.signature TEXT` column (no `UNIQUE`), compared by **string equality**. The plugin manifest declares `signature_schemas` per entity-kind and a `signature_schema_version`; the core never parses the JSON, and a manifest schema-version bump voids cached comparison. The Python plugin emits `{ "v": 1, "params": [...], "return_ann": "..." }` for functions, `{ "v": 1, "bases": [...] }` for classes, and `null` where signature comparison is not meaningful (modules, packages). A `null` signature simply means the move case cannot match on signature — acceptable and fail-closed.

**Scope note (honest framing):** signature is **near-redundant for the v1 deterministic move case**, which requires a byte-identical body — and the `def foo(x, y):` line is part of the body, so identical-body already implies identical-signature. Signature is carried for (a) conformance to the SEI spec §3 move predicate, and (b) as the load-bearing input to the North-Star **fuzzy** matcher (body edited, signature stable). It is forward-investment + spec-conformance, not a v1 necessity.

### 3. Identity persistence — `sei_bindings`, not `entities.sei`

Identity lives in a dedicated, cumulative **`sei_bindings`** table keyed by SEI:

- `sei` (PK, opaque), `current_locator`, `body_hash`, `signature`, `status` (`alive` | `orphaned` | `superseded`), `born_run_id`, `updated_run_id`, `updated_at`.
- A **partial unique index** on `current_locator WHERE status = 'alive' AND current_locator IS NOT NULL` enforces at most one alive binding per locator; orphaned/superseded bindings may share a former locator without colliding.

Orphaning is a **`status` flip**, never a row deletion or collision. The read path attaches an SEI to an entity by joining `entities.id = sei_bindings.current_locator AND status = 'alive'`; a missing binding yields `sei: null` (graceful degrade on a pre-SEI DB). `entities` itself gains only the plain `signature TEXT` column from decision 2 — no `sei` column. An append-only `sei_lineage` table records `born` / `locator_changed` / `moved` / `orphaned` / `superseded` events (INSERT only — no UPDATE path, per the SEI brief REQ-L-01; lineage tamper-evidence is a consumer/`legis` concern in v1).

### 4. Reserved locator namespace

The **`clarion:eid:` prefix is reserved** — no plugin locator may occupy it. This is what lets `resolve(locator)` reject an SEI-shaped input (SEI brief REQ-F-02): a colon-count check is insufficient because an SEI `clarion:eid:<hex>` has the same two colons a locator does, so the rejection MUST key on the reserved prefix. The fail-closed rejection is what makes the idempotent, resumable cutover backfill safe — an already-migrated SEI is rejected, never mis-resolved.

## Consequences

- **Migrations.** `0004_sei_prior_index.sql` adds the last-run snapshot side table (`locator → body_hash + signature`; **no SEI column** — shape-independent, shippable before lock). `0005_sei.sql` adds `sei_bindings` + `sei_lineage` and a plain `entities.signature` (no `entities.sei`). `CURRENT_SCHEMA_VERSION` advances 3 → 4 → 5; migrations stack (the in-place edit policy retired at the 1.0 publish).
- **Plugin manifest.** Gains optional `signature_schemas` (per-kind) and `signature_schema_version`. Plugins that emit no signatures degrade to the no-signature move case.
- **HTTP read API.** Gains `resolve` / `resolve_sei` / `lineage` (and a batch resolve), and a `_capabilities` flag `sei: { supported: true, version: 1 }` so consumers degrade against a pre-SEI Clarion.
- **MCP surface.** All tool responses returning an entity id also carry `sei` (via the binding join) — no "MCP locator exception" (SEI brief invariant §4).
- **Determinism tests.** A new test asserts a back-to-back unchanged re-run **carries** (never re-mints) every SEI. The existing byte-identical-state guarantee is explicitly scoped to exclude identity values (documented in code).
- **Supersession.** Per SEI spec §0.2, ADR-003's "the derived id *is* the identity" is demoted — that string is now the *locator*; and ADR-018's qualname heuristics are subsumed *as an identity mechanism* (their reconciliation role for Wardline qualnames is unchanged). Neither ADR file is rewritten (ADRs are immutable once Accepted); this ADR records the demotion.
- **Federation.** Passes `loom.md` §3–§5: SEI is enrich-only connective tissue, minted and resolved in one authority (Clarion), with no shared runtime/store/registry. The reserved-prefix and opacity rules keep consumers from coupling to the token's internal form.

## Loom vocabulary verdict (per ADR index acceptance rule)

`SEI` and `locator` are **cross-product-visible** terms (Wardline, Filigree, and `legis` all consume SEI; `locator` is the demoted address). Verdict: **`no clash`** — both are new suite-wide terms introduced by the SEI standard with a single, identical meaning across all four subsystems; no sibling uses either word for a different concept. Both are registered in [`docs/suite/glossary.md`](../../suite/glossary.md) with this ADR as authority. SEI's authority is the SEI standard (Wardline specs tree) for the suite-wide definition; this ADR is Clarion's implementing authority for the token form, persistence, and reserved namespace.
