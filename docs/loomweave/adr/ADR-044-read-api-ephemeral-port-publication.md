# ADR-044: Read-API Ephemeral Port Publication

**Status**: Accepted
**Date**: 2026-06-06
**Relates to**: [ADR-034](./ADR-034-federation-http-read-api-hardening.md)
**Tracking**: clarion-7f574bc34f

> **Accepted** on branch `feat/serve-no-index-chirp` (deterministic band
> `9400–10399`). Acceptance evidence: the cross-product-visible
> `.loomweave/ephemeral.port` term carries a **managed-clash** verdict in
> [`docs/suite/glossary.md`](../../suite/glossary.md), with the explicit
> `.filigree/ephemeral.port` ↔ `.loomweave/ephemeral.port` mapping table below
> (per the README acceptance criteria, model ADR-017).

## Context

`loomweave serve` exposes a federation HTTP read API. Its bind address is a
static `127.0.0.1:9111` — the default (`loomweave-federation/src/config.rs`) and
the value the installer stamps into every project's `loomweave.yaml`
(`crates/loomweave-cli/src/install.rs`). Every project gets the same port.

Consequences observed live:

- **Bind collision.** Two `loomweave serve` instances cannot run concurrently —
  whichever starts first binds 9111, the second fails with `Address already in
  use`. A `legis` session held 9111 for hours while another project's serve
  could not come up.
- **Consumer mis-targeting.** Consumers point at the static port too
  (`wardline.yaml: loomweave.url: http://127.0.0.1:9111`). A second project's
  wardline therefore reaches the *first* project's loomweave instance. ADR-034's
  instance-ID guard correctly rejects the cross-project taint write
  (`PROJECT_MISMATCH`) — no data is corrupted — but federation is silently dead
  for the mis-targeted project.

Loomweave already solved the *consumer* side of this problem for the sibling
direction: `loomweave-federation/src/filigree_url.rs` resolves Filigree's live
endpoint by reading `<project>/.filigree/ephemeral.port` (Filigree publishes a
per-project, deterministic-but-unpredictable port `8400 + sha256(path) % 1000`,
atomically, present only while running; consumers read it, never compute it, and
fail soft to configured URL). Loomweave never applied the same convention to its
*own* read API.

Picking a free port at install time does not fix this: it is TOCTOU (a port free
at install can be taken before `serve` runs, and two installs at different times
can pick the same "first free" port). That is precisely why the established
pattern publishes the live port at runtime rather than assigning it at install.

## Decision

Mirror Filigree's endpoint-discovery convention symmetrically for loomweave's
own read API. The **interop surface is the file**, not loomweave's resolver:
the resolver below is loomweave's own conforming reader, but the contract that
binds siblings is `.loomweave/ephemeral.port` itself. Cross-product consumers
(notably Wardline, which is Python and cannot call a Rust resolver) implement
their own reader against the file contract — the same "this is the contract,
consumers conform" posture as the SEI token (ADR-038). The normative file
contract and resolution semantics are pinned below.

1. **Deterministic per-project port, ephemeral fallback.** `serve` binds a
   per-project deterministic port derived from the canonical project path, in a
   loomweave-specific band chosen to *not* overlap Filigree's `8400–9399` band
   (so the two products never contend for the same number). If that port is
   taken, fall back to an OS-assigned ephemeral port (`bind :0`). The
   bind-and-discover primitive already exists in test form at
   `crates/loomweave-cli/src/http_read.rs` and is generalized to the production
   serve path.
2. **Publish the live port** to `<project>/.loomweave/ephemeral.port` per the
   file contract below — the loomweave twin of `.filigree/ephemeral.port`.
3. **Loomweave-side resolver.** Add a resolver in `loomweave-federation` (the
   twin of `resolve_filigree_url`) implementing the resolution semantics below.
   Loomweave's own consumers use it (`doctor`, `project_status_get`, which report
   the resolved source). It is *one* conforming reader, not the interop surface.
4. **Installer stops pinning a port.** `install` no longer stamps a fixed
   `serve.http.bind: 127.0.0.1:9111`. The `loomweave.yaml` stub documents that
   the read-API port is auto-selected and published; an explicit `bind` override
   remains honored for operators who need a fixed port.

## File contract (normative)

`.loomweave/ephemeral.port` is the cross-product interop surface. Producers
(loomweave `serve`) and every consumer (loomweave, Wardline, future siblings)
conform to exactly this:

- **Path:** `<project_root>/.loomweave/ephemeral.port`, where `<project_root>`
  is the directory the consumer is scanning/serving (the same anchor as
  `.filigree/ephemeral.port`).
- **Content:** a single plain-ASCII integer — the **TCP port only**. No host, no
  scheme, no key. An optional single trailing `\n` is permitted and ignored. No
  other bytes.
- **Host/scheme are implied, not stored:** `127.0.0.1` and `http`. This is sound
  *only* because publication is loopback-only (next bullet); a consumer composes
  `http://127.0.0.1:<port>`.
- **Loopback-only publication.** The file is written **only when `serve` binds a
  loopback address**. If an operator opts into a non-loopback bind
  (`allow_non_loopback`, ADR-034), `serve` does **not** publish the file — that
  deployment is explicit-config territory and consumers fall back to their
  configured URL (where the operator set the reachable host). This keeps the
  port-only format unambiguous and prevents a port-only reader from mis-targeting
  a non-loopback host.
- **Atomic write:** write to a temp file in `.loomweave/` and `rename(2)` into
  place, so a reader never observes a partial/torn value.
- **Lifecycle:** created/refreshed on successful loopback bind; removed on clean
  shutdown. Present-only-while-serving is best-effort, not guaranteed — a crash
  leaves a stale file, which resolution semantics handle (below).
- **Git-ignored** runtime artifact, consistent with ADR-005's treatment of
  run-time-only state.

## Managed-clash verdict

`ephemeral.port` is a cross-product-visible term: Filigree owns the original
`.filigree/ephemeral.port` endpoint-discovery convention, and this ADR adopts the
same filename for Loomweave's own read API. Per the ADR-acceptance criteria
(`docs/loomweave/adr/README.md`), this is a **managed clash** — the same term is
used by a sibling, governed here by an explicit mapping table (model: ADR-017).
The verdict is recorded in [`docs/suite/glossary.md`](../../suite/glossary.md).

| Product | Path | Format | Publication | Band (internal, not contract) |
|---|---|---|---|---|
| Filigree | `.filigree/ephemeral.port` | single plain-ASCII TCP port, optional trailing `\n`, atomic temp+rename | loopback-only, present only while running | `8400–9399` |
| Loomweave | `.loomweave/ephemeral.port` | identical | identical | `9400–10399` (disjoint) |

The clash is *managed*, not *renamed*: the shared filename is deliberate (one
convention siblings recognize), the paths are distinct per product, the wire
format is identical, and the deterministic bands are disjoint so the two products
never contend for the same port. The band is never part of the file contract —
consumers read the published file, never recompute a peer's port.

## Resolution semantics (normative)

Every consumer resolves **at consume time** (each scan / read), never caches the
resolution at install time — a port resolved once and reused goes stale exactly
when another project rebinds. Wardline's filigree leg, which resolves at install
time today, is the cautionary case (see related follow-up).

**Precedence (highest wins):**

1. An **explicit, deliberate target** — a `--loomweave-url` flag the operator
   *types* or an environment override they set — always wins. The published port
   must never override a target the operator chose on purpose (remote loomweave,
   debugging a specific instance). Provenance, not flag spelling, is what makes a
   value level 1: an **installer-seeded `--loomweave-url` baked into `.mcp.json`**
   (e.g. the deterministic URL `loomweave install` stamps into Wardline's MCP
   args) is **not** an operator's deliberate choice — it is config-tier
   (precedence 3), so the published file overrides it and self-heals when an
   ephemeral fallback fired.
2. The **published port file** `.loomweave/ephemeral.port` (composed to
   `http://127.0.0.1:<port>`). This **beats a stale/default configured URL** so
   resolution self-heals without a config edit.
3. The **configured URL** (e.g. `wardline.yaml: loomweave.url`).
4. **None** — federation is simply absent for this read; degrade, do not error.

**Fail-soft is mandatory at every step:**

- The port value MUST be validated to `1..=65535`. Missing, non-integer,
  out-of-range, or otherwise malformed content → fall through to the next
  precedence level (it is not an error).
- A **resolved-but-refused** connection (file present, but the port is closed —
  crashed serve / stale file) MUST be treated as soft: fall through to configured
  URL or none. This — not malformed content — is the case a live consumer hits
  most, and it must never surface as a hard error.
- The instance-ID guard (ADR-034) is the **correctness backstop** that lets the
  reader be simple rather than perfect: even if a stale file points at a port now
  owned by *another* project's serve, the write is rejected `PROJECT_MISMATCH`,
  fail-soft — a stale file degrades, never corrupts. Consumers rely on this; they
  do not need to verify project identity before connecting.

## Consequences

- Two or more projects can `serve` concurrently without port contention; the
  cross-project `PROJECT_MISMATCH` federation failure disappears because each
  consumer resolves *its own* project's live port.
- The read-API port becomes a *read-this-file*, never a *compute-or-configure*,
  fact — matching the discipline loomweave already imposes on consuming
  Filigree. "Read, never compute" is the load-bearing rule: nothing hard codes or
  re-derives the band formula to guess a peer's port.
- Consumers pinned to a literal `:9111` (e.g. existing `wardline.yaml` files)
  self-heal once they prefer the published file over config (precedence 2 > 3) —
  no user edit required. Until a consumer adopts the resolver it fails soft to the
  configured URL — degraded, not broken.
- Federation stays enrich-only and solo-useful: a project with no published port
  file (serve not running, feature disabled, or non-loopback bind) degrades to
  the configured `base_url`, never to a sibling-internal default.

## Verification

- Two serves on distinct project paths bind distinct ports and each publishes
  its own `.loomweave/ephemeral.port`; neither fails to bind.
- A deterministic-port collision forces the ephemeral-`0` fallback, and the
  published file reflects the *actually* bound port (not the deterministic
  guess).
- File contract: published content is a bare port (optional trailing newline),
  written via temp + rename; a non-loopback bind publishes **no** file.
- Precedence: an explicit `--loomweave-url`/env target overrides the published
  file; the published file overrides a stale/default configured URL; absent file
  falls through to config, then none.
- Fail-soft: missing / non-integer / out-of-range (`0`, `>65535`) content, and a
  **resolved-but-refused** connection (stale file, closed port), each degrade to
  the next precedence level rather than erroring.
- The published file is removed on clean shutdown.
- A wardline scan against a project whose loomweave serve is running on a
  non-9111 port resolves and writes taint successfully (no `PROJECT_MISMATCH`).

## Related follow-up (not blocking this ADR)

Consume-time live-port resolution should apply to **both** sibling directions.
Wardline reads `.filigree/ephemeral.port` only at install time and uses the
static config URL at scan time, so its filigree leg carries the same latent
staleness this ADR removes for the loomweave leg. Unifying both consumers on
consume-time resolution is Wardline-side work, tracked separately; flagged here
so the two legs are not designed divergently.

- Wardline should treat install-seeded MCP args (the `--loomweave-url` baked into
  `.mcp.json` by `loomweave install`) as config-tier and resolve consume-time, so
  the published `.loomweave/ephemeral.port` file wins over the baked deterministic
  URL when an ephemeral fallback fired. Tracked clarion-7f574bc34f.
