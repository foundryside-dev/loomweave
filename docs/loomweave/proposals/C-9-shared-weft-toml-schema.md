# Proposal: Shared `weft.toml` key layout (C-9 cross-member schema)

**Status**: DRAFT — loomweave authors, hub blesses (weft-a2f4cf95c7 / clarion-164f88c510)
**Author**: loomweave
**Date**: 2026-06-08
**Tracks**: weft conventions.md C-9(d), conflict-register §A-14, glossary §8 (no-duplication)
**Reference reader**: `crates/loomweave-core/src/store.rs` (member-private form, shipped)

---

## 1. Why this proposal exists

C-9 / §A-14 are DECIDED and the **member-private** form is shipped: each member
owns `.weft/<member>/` (sole writer) and reads its own `[<member>]` table in the
operator-authored, read-only `weft.toml` (canonical knob `store_dir`; legis is the
reference; loomweave landed it in `store.rs`).

What is still **PENDING** — and what no member may bake until the hub pins it — is
the **shared / cross-member key layout**: where a fact that *one member writes-by-
operator and another member reads* lives. The concrete forcing case is the
**sibling federation endpoint** (a member's HTTP URL). loomweave surfaced the gap
(it refused to guess the schema under dispatch); wardline is the sharpest consumer
(it calls *both* filigree and loomweave and is retiring its `[wardline.filigree].url`
/ `[wardline.loomweave].url` keys per the 2026-06-07 ruling). This proposal pins:

1. the **single well-known home** for a shared fact (no per-member duplication);
2. the **precedence** ladder for resolving a sibling endpoint;
3. the **invariants** restated for the shared layer;
4. the **reader semantics** a member implements.

### Non-goals (already settled or explicitly excluded)

- `[<member>].store_dir` and the member-private form — **shipped**, not re-opened.
- Authority / signing keys (Legis) — **never** enter the shared `.weft/`/`weft.toml`
  namespace; capability confinement (proposed C-8) governs them (C-9 carve-out).
- On-disk *location* discovery (`.weft/<sibling>/ephemeral.port`) — already C-9(e);
  this proposal covers the operator-declared *endpoint URL*, a distinct rung.
- Writing `weft.toml` — **out of scope by construction**: members are read-only
  (C-9b; the C-4 multi-writer truncation lesson, gate weft-eb3dee402f).

---

## 2. Proposal

### 2.1 The shared home: `[<member>]` top-level table, allowlisted cross-read keys

A shared fact about member **X** lives **once**, at the top-level `[X]` table, and
is read by any member. The endpoint of X is:

```toml
# weft.toml — operator-authored, project root
[filigree]
url = "http://127.0.0.1:8749"      # filigree's federation endpoint — read by wardline, loomweave, legis…

[loomweave]
url = "http://127.0.0.1:9111"      # loomweave's endpoint — read by wardline…
store_dir = "custom/store"          # member-PRIVATE — read ONLY by loomweave

[wardline]
url = "http://127.0.0.1:7000"
```

**Ownership rule (the crux).** The `[X]` table is X's home for *both* its
member-private keys *and* its cross-readable keys. To keep that unambiguous:

- A member reads its **own** full `[<self>]` table (private + shared keys).
- A member reads **only the allowlisted cross-read keys** from a **sibling's**
  `[X]` table. The v1 cross-read allowlist is **`url`** (and, reserved for the
  next fact, **`enabled`**). Everything else under `[X]` is private to X.
- A shared fact is **never** duplicated into a second section. There is exactly
  one `url` for X, at `[X].url`. `[wardline.filigree].url` and any
  `[<member>.<sibling>]` form are **retired / forbidden** (glossary §8 clash rule).

This satisfies "live once at a well-known top-level path any member may read" with
**no new table**: the home of X's endpoint is X's own section. (Alternative B in §4
keeps a dedicated `[federation]` table instead; recommended against for v1.)

### 2.2 Precedence ladder (resolving a sibling endpoint)

A member resolving sibling **X**'s endpoint walks, highest wins:

| Rung | Source | Who sets it | Lifetime |
|---|---|---|---|
| 1 | CLI flag (`--filigree-url …`) | invoking agent/operator | this invocation |
| 2 | env var (`WEFT_<X>_URL`) | shell / `.mcp.json` env | this process |
| 3 | **`weft.toml` `[X].url`** ← *this proposal* | operator (durable) | project |
| 4 | on-disk discovery `.weft/<X>/ephemeral.port` (C-9e) | X's live process | while X runs |
| 5 | built-in default | the member | always |

Rationale for **3 above 4**: `weft.toml [X].url` is the operator's *durable,
explicit* declaration — it is exactly the "persisted operator-declared remote-URL"
case C-9(d) names (e.g. X runs on another host, so there is no local
`ephemeral.port` to find). A live flag/env (1–2) still overrides it for a one-off.
For a purely-local federation the operator declares no `url`, so resolution falls
straight through to on-disk discovery (4) — the common case is unchanged.

**Operator-overlay vs member-authoritative precedence:** `weft.toml` is the
operator overlay and outranks a member's *built-in default* (rung 5) for shared
facts, but never a runtime flag/env (rungs 1–2). A member's own *authoritative
config* (e.g. `loomweave.yaml`) governs member-private behavior only; it does not
declare another member's endpoint (that would re-introduce the duplicate).

### 2.3 Invariants restated for the shared layer

All of these are C-9 invariants; this proposal confirms they hold for cross-read
keys, not just private ones:

- **Malformed = absent (NORMATIVE).** A missing / unparseable `weft.toml`, an
  absent `[X]` table, an absent or wrong-typed `url`, or a blank value → the rung
  is skipped and resolution falls through. A member MUST NOT hard-fail. (Same
  fail-soft path `store.rs` already implements for `store_dir`.)
- **Operator is sole writer.** No member's `install` / CLI / `doctor` writes
  `weft.toml` — including its own `[<self>].url`. The operator (or `weft init`)
  authors it.
- **No duplication.** One fact, one home (§2.1). A reader never has to reconcile
  two declarations of X's endpoint.
- **Forward-compatible parse.** Unknown top-level tables and unknown keys within
  any table are ignored, never a parse rejection (so a member built before a new
  shared key exists still loads the file). `store.rs` already does this for
  sibling tables; the shared reader extends the same posture.

### 2.4 Reader semantics (what a member implements, post-bless)

Extends the existing `WeftToml` deserialization in `loomweave-core::store`:

```rust
// Today (shipped): reads only its own private table.
#[derive(Deserialize)] struct WeftToml { loomweave: Option<LoomweaveTable> }

// Post-bless: also reads allowlisted cross-read keys from sibling tables.
#[derive(Deserialize)] struct SiblingTable { url: Option<String> /* , enabled: Option<bool> */ }
#[derive(Deserialize)] struct WeftToml {
    loomweave: Option<LoomweaveTable>,           // own: private + shared
    filigree:  Option<SiblingTable>,             // sibling: url only
    wardline:  Option<SiblingTable>,
    legis:     Option<SiblingTable>,
}
```

A sibling-endpoint resolver returns the first rung that yields a non-blank value
and reports its `source` (`flag` / `env` / `weft.toml` / `discovery` / `default`)
on the wire — so the resolved-vs-configured gap is **loud, not silent** (the
lacuna-401 lesson; loomweave's `project_status_get` already reports resolved vs
configured endpoints in one call and is the model).

---

## 3. Worked example: wardline (the multi-sibling consumer)

wardline calls both filigree and loomweave. Per the 2026-06-07 ruling it **retires**
`[wardline.filigree].url` / `[wardline.loomweave].url`. After this schema lands:

- filigree endpoint ← `--filigree-url` › `WEFT_FILIGREE_URL` › `weft.toml [filigree].url` › `.weft/filigree/ephemeral.port` › default.
- loomweave endpoint ← `--loomweave-url` › `WEFT_LOOMWEAVE_URL` › `weft.toml [loomweave].url` › `.weft/loomweave/ephemeral.port` › default.

No `[wardline.*]` sibling keys. The operator declares a remote filigree once at
`[filigree].url`; wardline, loomweave, and legis all read that one line.

> Note: this resolves the *route* (which endpoint). The *token* (F1/weft-23574069a1)
> is a sibling concern — auth still flows via the daemon/tier-1
> `WEFT_FEDERATION_TOKEN`, not a per-project mint. Endpoint-resolution and
> token-resolution are independent ladders; this proposal pins only the endpoint.

---

## 4. Alternatives considered

**B. Dedicated `[federation]` (or `[endpoints]`) table.**
```toml
[federation]
filigree = "http://127.0.0.1:8749"
loomweave = "http://127.0.0.1:9111"
```
*Pro:* clean separation of shared facts from member-private tables; no per-table
allowlist needed. *Con:* a member's endpoint now lives apart from its `[<member>]`
table (two places to look for "everything about X"); and it does not generalize as
cleanly to a second shared key (`enabled`) without either nesting
(`[federation.filigree] url=…, enabled=…`, which is just §2.1 with a prefix) or a
second parallel table. **Recommend §2.1** (member-table home) for v1; B remains
available if the hub prefers strict shared/private separation.

**C. Keep per-member `[<member>.<sibling>].url`.** Rejected: it is the duplication
glossary §8 forbids and the exact pattern wardline is retiring.

---

## 5. Questions for the hub (to resolve at bless)

1. **Home:** §2.1 member-table + allowlist (recommended), or §4-B `[federation]` table?
2. **Precedence:** confirm rung 3 (`weft.toml url`) sits **above** rung 4 (on-disk
   discovery). Local-only federations are unaffected either way; the question is
   purely the remote-declared-sibling case.
3. **Allowlist v1:** is `url` the only cross-read key, or do we pin `enabled` now
   (a member reading whether a sibling is operator-disabled)?
4. **Env var spelling:** standardize `WEFT_<MEMBER>_URL` (e.g. `WEFT_FILIGREE_URL`)
   as the rung-2 name across members?

## 6. Sequencing

Fast-follow, **not** a dogfood-#2 gate blocker (C-9 sequencing). Order: **hub
blesses this schema → members implement the cross-read reader** (loomweave extends
`store.rs` per §2.4; clarion-164f88c510 covers loomweave's reader). Until blessed,
members resolve sibling endpoints by flag / env / on-disk discovery only and bake
**no** `weft.toml` endpoint keys (C-9d).
