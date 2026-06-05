# SEI hard-cutover migration playbook

**Status:** Normative procedure. **Owner-gated** — this is a *coordinated
cross-tool release*, not a self-service migration. Do **not** fire it
unilaterally; it is scheduled by the suite owner once Filigree and Wardline are
ready to cut at the same time.

**Authority:** Weft SEI conformance standard §7 / §7.1; Loomweave ADR-038.

---

## What this migration does

The Weft suite is moving every cross-tool binding off the mutable **locator**
(`{plugin}:{kind}:{qualname}`) and onto the durable, opaque **SEI**
(`loomweave:eid:<hex>`). Before the cutover, producers store and emit locators;
after it, they store and emit SEIs. There is deliberately **no mixed-format
window** — a single hard cutover, because one owner controls all four release
cycles (§7.1). A feed that emitted a *mix* of locators and SEIs would be
uninterpretable, since every consumer treats the id as opaque.

## Roles

| Subsystem | Role in the cutover |
|---|---|
| **Loomweave** (authority) | Mints an SEI for **every** current entity on its first SEI-aware analyze run, and serves `resolve(locator)` so producers can re-key. This is the only Loomweave action — it has no cross-tool bindings of its own to re-key. |
| **Filigree** | Re-keys every stored `loomweave_entity_id` association from its locator to the resolved SEI. |
| **Wardline** | Re-keys every stored taint fact (and dossier handle) from its locator to the resolved SEI. |
| **legis** (when present) | Re-keys governance attestations onto SEIs. |

## Loomweave's side (already shipped in Wave 1)

1. **Mint-all-on-first-run.** The analyze SEI mint pass (ADR-038 §3) mints an
   `alive` `sei_bindings` row for every current entity on the first SEI-aware
   run, and carries (never re-mints) them on every subsequent unchanged run. No
   separate "backfill job" is needed on Loomweave's side — minting *is* the
   first-run behaviour, and it is **idempotent** (a re-run carries every SEI;
   proven by `analyze_carries_sei_on_unchanged_rerun`).
2. **Resolution surface.** `POST /api/v1/identity/resolve` maps a locator to its
   alive SEI. Producers drive their re-key off this endpoint.
3. **Reserved-prefix rejection (REQ-F-02) — the safety interlock.** `resolve`
   **rejects** any SEI-shaped input (reserved `loomweave:eid:` prefix) with `400`.
   This is what makes each producer's backfill **idempotent and resumable**: an
   already-migrated row whose value is now an SEI is *rejected*, never
   mis-resolved, so re-running a partially-completed backfill is safe.

## Producer backfill protocol (Filigree / Wardline / legis)

Each producer owns its **own** progress cursor (a rowid or a migration-state
side table). No Loomweave-side generation marker is required.

For each stored binding the producer holds:

1. Read the stored id. If it already begins with `loomweave:eid:`, it is **already
   migrated** — skip (this is why the backfill is resumable).
2. Call `POST /api/v1/identity/resolve` (or `…/resolve:batch`) with the locator.
3. On `{ alive: true, sei }` → rewrite the stored id to `sei`, advance the cursor.
4. On `{ alive: false }` → the locator no longer resolves (already orphaned by a
   past rename). **Flag it ORPHAN for human review — never silently drop it**
   (the suite's no-false-green ethos, §7).
5. On `400` (REQ-F-02 rejection) → the value was already an SEI; treat as
   already-migrated and skip.

A backfill that fails partway is simply **re-run**: already-migrated rows are
skipped (step 1 / step 5), so it converges.

## Cutover sequencing (owner-gated, single coordinated release)

1. **Freeze.** Quiesce writes across the producers being cut.
2. **Loomweave cuts first.** Deploy SEI-aware Loomweave and run analyze so every
   entity has an alive SEI (verify via `_capabilities.sei.supported: true` and a
   non-empty `sei_bindings`).
3. **Producers backfill** their stored ids locator→SEI (protocol above), each to
   completion, surfacing ORPHANs.
4. **Flip the feeds.** Every federation feed that carries entity ids
   (e.g. Filigree's `affected_entities` on `GET /api/weft/changes`) switches to
   emitting **only SEIs** — never a mix.
5. **Unfreeze.** Resume writes; from here all new bindings key on SEIs.

## Verification & rollback

- **Conformance:** the shared SEI oracle
  ([`fixtures/sei-conformance-oracle.json`](./fixtures/sei-conformance-oracle.json))
  must pass for each subsystem before it is declared conformant (no
  grandfathering). Loomweave's pass:
  `cargo test -p loomweave-storage --test sei_conformance_oracle`.
- **Idempotency / resumability** rests entirely on the REQ-F-02 rejection
  contract; do not relax it.
- **Rollback** is a coordinated re-deploy of the prior producer builds; because
  the backfill rewrites ids in place, a rollback that needs the original
  locators must restore from the pre-cutover backup taken at step 1. Take that
  backup.

## Scheduling

This procedure is **surfaced for owner scheduling**, not executed here. It runs
only when Filigree and Wardline are both ready to cut in the same release. Until
then Loomweave mints and serves SEIs additively (enrich-only); consumers that have
not yet cut keep working on locators and degrade on `_capabilities.sei`.
