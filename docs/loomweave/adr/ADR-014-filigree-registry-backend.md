# ADR-014: Filigree `registry_backend` Flag and Pluggable `RegistryProtocol`

**Status**: Accepted; partially extended by [ADR-034](./ADR-034-federation-http-read-api-hardening.md) (Security Posture and Error Envelope sections only — registry-backend protocol decision remains in force)
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: Loomweave v0.1 integration boundary with Filigree's file registry; joint deliverable (Loomweave + Filigree, same author)

## Summary

Filigree gains a pluggable `RegistryProtocol` with two modes, selected via a `registry_backend` configuration flag: `local` (Filigree's current native registry — the default) and `loomweave` (Filigree delegates file-identity operations to Loomweave's HTTP read API). Loomweave v0.1 ships expecting `registry_backend: loomweave` and degrades to shadow-registry mode when the flag is absent. Because the same author maintains both products, Filigree's implementation lands alongside Loomweave's v0.1 release rather than as a cross-team prerequisite.

## Context

Filigree today owns the file registry unconditionally. `file_records(id TEXT PRIMARY KEY)` is referenced by four NOT-NULL foreign keys (`scan_findings.file_id`, `file_associations.file_id`, `file_events.file_id`, `issues` via associations), and three code paths auto-create rows:

1. `POST /api/v1/scan-results` calls `_upsert_file_record` before inserting findings (`db_files.py:430-453`).
2. `create_observation(file_path=…)` calls `register_file` to bind the observation (`db_observations.py:135-147`).
3. `trigger_scan` / `trigger_scan_batch` call `tracker.register_file(...)` to populate `scan_runs.file_ids` (`mcp_tools/scanners.py:422, :586`).

Each auto-create path produces a Filigree-native `file_records.id` (UUID-derived: `f"{prefix}-f-{uuid.uuid4().hex[:10]}"`). Loomweave's entity-identity scheme uses symbolic canonical names (ADR-003). The two schemes currently diverge: anything Loomweave POSTs creates a shadow Filigree file row, and cross-tool "same file" queries have no answer.

Loomweave v0.1 claims to own structural truth about the codebase (`weft.md` §2). That claim is inconsistent with Filigree silently minting file identities on every POST. A protocol boundary is needed.

`registry_backend` and `FILIGREE_FILE_REGISTRY_DISPLACED` do not exist in Filigree today — verified by `grep` across `/home/john/filigree` on 2026-04-17 (see `reviews/pre-restructure/integration-recon.md` §2.1). Both are net-new additions.

## Decision

Filigree introduces a `RegistryProtocol` trait with two implementations.

**Mode `local` (default)**: Filigree's current behaviour. The three auto-create paths populate `file_records` using UUID-derived IDs. Filigree remains fully usable standalone — no Loomweave dependency, no degradation.

**Mode `loomweave`**: Filigree delegates `file_id` resolution to Loomweave's HTTP read API. The three auto-create paths call `RegistryProtocol::resolve_file(path, language) -> file_id` which, under `loomweave` mode, issues an HTTP GET to Loomweave's read API. The returned `file_id` is Loomweave's symbolic file-kind entity ID (`core:file:{qualified_name}`). The `file_records` row is created in Filigree with that ID, preserving the existing foreign-key structure.

**Flag surfacing**: `registry_backend` appears in `GET /api/files/_schema.config_flags`. Loomweave's capability probe reads it at every `loomweave analyze` start. Present + value `loomweave` → proceed with delegation. Absent or value `local` → Loomweave enters shadow-registry mode and emits `LMWV-INFRA-FILIGREE-SHADOW-REGISTRY` per batch.

**Error code**: `FILIGREE_FILE_REGISTRY_DISPLACED` is returned by Filigree to any caller that tries to directly mutate `file_records` (e.g., `register_file` MCP tool) while `registry_backend: loomweave` is active. The write path is Loomweave's; Filigree's direct file-registration MCP tool becomes a read-only query in `loomweave` mode.

**Startup failure mode**: if Filigree starts with `registry_backend: loomweave` but Loomweave's read API is unreachable, Filigree refuses writes (returns `503 Service Unavailable` from the three auto-create paths) rather than silently degrading to `local`. An explicit `--allow-local-fallback` flag exists for single-operator recovery scenarios; the default is fail-closed.

### Capability Probe Semantics

Loomweave's read API exposes `GET /api/v1/_capabilities` for Filigree's
registry-backend probe. The response includes:

```json
{
  "api_version": 1,
  "instance_id": "9bd7234e-6d44-4a38-9ae4-76f912a10221",
  "registry_backend": true,
  "file_registry": true
}
```

`api_version` is the wire-contract version for the HTTP read API. It increments
only when the HTTP read API changes incompatibly for existing Filigree clients.
It is not Loomweave product semver and must not be compared as a release version.

`instance_id` identifies the Loomweave project instance serving the API. Filigree
uses it to detect that the same endpoint has been rebound to a different
Loomweave project instance.

### File Identity Semantics

Loomweave resolves only existing file-kind rows. When no `kind = 'file'` entity
row exists for the requested path, the API fails closed with `404 NOT_FOUND`.
Loomweave must not synthesize a `core:file:{content_hash}@{canonical_path}`
identity. That pattern violates ADR-003's entity-ID grammar and creates shadow
IDs that will not match future file-discovery rows.

### Batch Resolution Amendment

Loomweave also exposes `POST /api/v1/files:resolve` for callers that need to
resolve many file paths without N HTTP round trips. The request body is
`{"paths": [{"path": "...", "language": "..."}, ...]}` with a fixed
1000-path envelope cap plus the normal HTTP body limit. The response preserves
input order as `results[]`, where each entry carries the original `path` and a
`response` object:

- `status: "resolved"` with the same body shape as `GET /api/v1/files`.
- `status: "not_found"` with the `NOT_FOUND` error envelope.
- `status: "blocked"` with the `BRIEFING_BLOCKED` error envelope and no file
  identity fields.
- `status: "error"` with a per-path error envelope.

This endpoint is a transport optimization, not a replacement for the single
`GET /api/v1/files` URI model. ETag semantics remain single-GET only.

### Canonical Path Semantics

`canonical_path` is the normalized project-relative POSIX path for the file:

- no leading `/`
- no leading `./`
- no trailing `/`
- `/` as the separator on every platform

The path is relative to the Loomweave project root so file identity responses
survive project relocation. It is the path Filigree should store as human and
drift context, not an absolute filesystem path.

### Instance Fingerprint

Loomweave persists a stable per-project UUID at `.loomweave/instance_id`. The first
creation writes the file with mode `0600` on Unix. `GET /api/v1/_capabilities`
surfaces the UUID as `instance_id`.

Deleting `.loomweave/` may create a new instance ID. That is acceptable because it
represents a new Loomweave project instance and should be detectable by Filigree.

### Error Envelope

Non-2xx read API responses use a closed JSON envelope:

```json
{
  "error": "path does not resolve to a Loomweave file entity",
  "code": "NOT_FOUND"
}
```

The initial `ErrorCode` enum is closed to:

- `INVALID_PATH`
- `PATH_OUTSIDE_PROJECT`
- `NOT_FOUND`
- `UNAUTHENTICATED`
- `STORAGE_ERROR`
- `INTERNAL`

Clients must switch on `code`, not on human-readable `error` text.

### Security Posture

The HTTP read API is loopback-only by default and may remain unauthenticated for
local sidecar workflows. Authenticated deployments configure
`serve.http.identity_token_env`; Loomweave refuses to start if that env var is
missing, and protected read routes require
`X-Weft-Component: loomweave:<hmac>`.

The HMAC is lowercase hex HMAC-SHA256 over `METHOD`, `PATH_AND_QUERY`, and the
SHA-256 request-body hash separated by newlines. Loomweave refuses non-loopback
HTTP binds unless `serve.http.allow_non_loopback: true` is set, and a
non-loopback bind must have either the HMAC identity secret or the legacy bearer
token resolved at startup.

## Alternatives Considered

### Alternative 1: Loomweave-native registry without a flag — hard displacement

Filigree always delegates to Loomweave. No `registry_backend` flag; no `local` mode.

**Pros**: single code path in Filigree; no dual-mode testing surface.

**Cons**: violates Weft federation (`weft.md` §4 composition law). Filigree becomes semantically dependent on Loomweave running — "removing Loomweave changes the meaning of Filigree's own data" (§5 failure test). `weft.md` §5's explicit Filigree example ("Filigree creates and closes tickets exactly the same way whether Loomweave is installed or not") fails. Also makes Filigree's existing deployments (including `filigree` itself, which uses Filigree for its own issue tracking) require Loomweave, which is absurd for a product that ships standalone today.

**Why rejected**: pairwise composability is a hard rule, not an aspiration.

### Alternative 2: Schema-level surgery — replace `file_records(id)` with a foreign key into Loomweave

Eliminate `file_records` and reference Loomweave's entity catalog directly via an external database handle or JSONB column.

**Pros**: single source of truth at the storage layer; no RPC round-trip on every operation.

**Cons**: fundamentally couples Filigree's database to Loomweave's. Violates `weft.md` §6 ("A central store or database... No shared SQLite/Postgres sits under the suite"). Every Filigree operator would need a local Loomweave database even in pure-Filigree deployments. Migration cost is high and irreversible.

**Why rejected**: the whole point of the Weft architecture is that each product owns its storage. Schema surgery is a stealth-monolith pattern.

### Alternative 3: Event-driven sync — Loomweave pushes entity state to Filigree

Loomweave maintains its catalog and publishes file-identity events to Filigree via a webhook or event bus. Filigree reconciles its `file_records` asynchronously.

**Pros**: keeps Filigree's storage independent; allows eventual consistency.

**Cons**: introduces an event-delivery mechanism that does not exist today. Filigree writes (from non-Loomweave sources, e.g., manual scans, other Weft siblings) would have no immediate Loomweave ID available and would need deferred reconciliation. Error-recovery semantics are complex (what if the event bus is down?). Also, Weft already prohibits shared infrastructure (`weft.md` §6) — an event bus qualifies.

**Why rejected**: too much mechanism for a problem that has a simpler synchronous answer.

### Alternative 4: Leave it as shadow-registry permanently — no displacement

Accept that Filigree mints its own file IDs forever; Loomweave reconciles post-hoc via path + hash.

**Pros**: zero Filigree-side work; preserves total independence.

**Cons**: the "Loomweave owns structural truth" claim in `weft.md` §2 becomes a lie — Filigree owns the authoritative file ID for everything it stores, and Loomweave's catalog is the shadow. Cross-tool "same file" queries have no deterministic answer when file paths change. Issues referencing Filigree file IDs cannot round-trip to Loomweave entity IDs without a fragile path-based join.

**Why rejected**: turns a v0.1 deferral into a permanent identity-model concession; the cost compounds across every future cross-tool query.

## Consequences

### Positive

- Preserves Filigree's standalone usability (`registry_backend: local` is the default).
- Makes "Loomweave owns structural truth" honest: in `registry_backend: loomweave` mode, Filigree's file IDs *are* Loomweave's entity IDs, not a shadow mapping.
- Creates a clean contract surface for v0.2+ alternative registry backends (e.g., `registry_backend: git-objects`).
- `FILIGREE_FILE_REGISTRY_DISPLACED` surfaces the coupling explicitly; operators running `loomweave` mode know why direct file-registration MCP calls fail.
- Capability probe (`GET /api/files/_schema.config_flags`) means Loomweave discovers the mode at runtime rather than requiring synchronised deployment.

### Negative

- Two Filigree code paths per auto-create operation (local vs delegated). Testing surface doubles for file-registry operations.
- `registry_backend: loomweave` mode introduces a synchronous RPC hop on every Filigree write that touches `file_records`. Latency impact: one local HTTP round-trip to Loomweave (typically <5ms on loopback). Acceptable for developer-machine workloads; would need re-evaluation for high-throughput server deployments.
- Fail-closed startup (Filigree refuses writes if Loomweave is unreachable under `loomweave` mode) means operators must start Loomweave before Filigree — or use the explicit `--allow-local-fallback` recovery flag.
- Shadow-registry mode (for operators who never want to run Loomweave) remains available, but Loomweave now has *two* v0.1 shapes it needs to test (`loomweave` mode and shadow mode). ADR-020's degraded-mode policy covers the testing burden.

### Neutral

- Loomweave's existing HTTP read API is the contract; no new endpoints are introduced for Filigree's consumption — `resolve_file(path, language)` is just a read through the existing file-entity query surface.
- The `FILIGREE_FILE_REGISTRY_DISPLACED` error code lives in Filigree's error-code registry alongside existing codes; it does not become a cross-product shared enum.

## Related Decisions

- [ADR-003](./ADR-003-entity-id-scheme.md) — symbolic entity IDs are what `loomweave` mode uses as `file_records.id` values.
- [ADR-004](./ADR-004-finding-exchange-format.md) — findings intake uses the same `file_id` that `resolve_file` returns.
- ADR-008 (superseded) — earlier framing of this decision as a "feature flag"; the recon revealed it is an interface, not a flag.
- [ADR-012](./ADR-012-http-auth-default.md) — superseded for this registry-backend HTTP read surface; ADR-014 owns the unauthenticated loopback-only trust model and non-loopback guard.
- [ADR-016](./ADR-016-observation-transport.md) — the `create_observation(file_path=…)` auto-create path is one of the three delegated operations; under `registry_backend: loomweave` mode the `file_id` resolution in that path uses this ADR's protocol regardless of whether ADR-016's transport is MCP-spawn (v0.1) or HTTP (v0.2).
- [ADR-017](./ADR-017-severity-and-dedup.md) — `mark_unseen=true` dedup relies on stable file IDs, which `loomweave` mode provides and shadow mode does not.
- [ADR-018](./ADR-018-identity-reconciliation.md) — the qualname ↔ EntityId translation layer is adjacent; `loomweave` mode's `file_id` resolution is one slice of the broader identity-reconciliation surface.
- ADR-020 (pending; see the [ADR index backlog](./README.md)) — shadow-registry mode is one of the enumerated degraded modes.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — file-kind entities are the narrowest ontology surface the plugin-vs-core boundary governs; ADR-022 states the `file` kind's core ownership as a first-class decision, and this ADR is the downstream consumer that depends on it.

## References

- [Loomweave v0.1 system design §9](../v0.1/system-design.md) — integration posture; capability probe; degraded modes.
- [Integration reconnaissance §2.1](../../implementation/v0.1-reviews/pre-restructure/integration-recon.md) — `file_records` schema; four NOT-NULL foreign keys; three auto-create paths; verified absence of `registry_backend` and error code.
- [Weft doctrine §4, §5, §6](../../suite/weft.md) — pairwise composability; enrichment failure test; no-shared-store rule.
- [Loomweave v0.1 scope commitments](../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md) — Q2 commits `registry_backend` to v0.1 as within-scope Filigree work.
