# ADR-034: Federation HTTP Read API Hardening — Bearer Auth, Batch Resolution, `BRIEFING_BLOCKED`, Instance ID

**Status**: Accepted
**Date**: 2026-05-19
**Deciders**: qacona@gmail.com
**Context**: Sprint 3 Loom federation hardening (see [`docs/implementation/sprint-3/2026-05-19-loom-federation-hardening-tasking.md`](../../implementation/sprint-3/2026-05-19-loom-federation-hardening-tasking.md)); extends ADR-014's read-API §"Security Posture" and §"Error Envelope"
**Extends**: [ADR-014](./ADR-014-filigree-registry-backend.md) Security Posture and Error Envelope sections only — the registry-backend protocol decision in ADR-014 §"Decision" remains in force.

## Summary

Sprint 3 hardens Clarion's HTTP read API beyond ADR-014's original posture: protected routes require an `Authorization: Bearer` token resolved from an operator-named environment variable, a new `POST /api/v1/files/batch` endpoint handles bulk path resolution in a single round trip, a distinct `403 BRIEFING_BLOCKED` response distinguishes blocked entities from "not found" entities without leaking identity, and `GET /api/v1/_capabilities` echoes a stable per-project `instance_id` so siblings can detect endpoint rebinding. These additions extend ADR-014's wire-contract surface without breaking it — `api_version` remains `1`.

## Context

ADR-014 §"Security Posture" pinned the HTTP read API as "unauthenticated and loopback-only by default" with non-loopback binds gated by `serve.http.allow_non_loopback: true`. ADR-014 §"Error Envelope" closed the initial `code` enum to `INVALID_PATH`, `PATH_OUTSIDE_PROJECT`, `NOT_FOUND`, `STORAGE_ERROR`, `INTERNAL`. Both were the right posture for the immediate ADR-014 use case (loopback-only Filigree sidecar) but were under-specified for the broader Sprint 3 federation goals:

- Filigree may run on a different host than Clarion in production deployments, and the operator should not have to layer a separate auth proxy for what is a publisher-side concern.
- Filigree's `ClarionRegistry` cold-start hydrates N file records by re-asking Clarion for each path. The single-file `GET` endpoint costs one pooled SQLite connection per call; for a 1000-file warm-up that is 1000 round trips and 1000 pool acquisitions.
- `briefing_blocked` entities (per ADR-013 secret-scan briefing block) are not "not found" — they exist but are policy-blocked. The original `NOT_FOUND` masking conflates "Clarion does not know this file" (registry coverage gap; escalate) with "Clarion knows this file but refuses to expose it" (briefing block; wait for re-scan or fix the secret). Filigree needs to distinguish the two.
- `_capabilities` is the only handshake point Filigree sees before issuing requests. If an operator rebinds the same `bind` to a different Clarion project (different `.clarion/` root, different cache), Filigree has no fingerprint to detect the swap.

Sprint 3 implemented these four hardenings (`1109560`, `acbf465`, `eb6200d`, `2c3311a`) and pinned the resulting wire contract in [`docs/federation/contracts.md`](../../federation/contracts.md). The body of ADR-014, per the ADR immutability rule in [`/home/john/clarion/CLAUDE.md`](../../../CLAUDE.md) §"Editorial conventions", cannot be rewritten in place. This ADR is therefore the authoritative source for the four hardenings; ADR-014 carries the standard `Accepted; partially extended by ADR-034 (Security Posture)` status amendment.

## Decision

### 1. Bearer authentication on protected routes

`/api/v1/files`-family endpoints require `Authorization: Bearer <token>` when Clarion has resolved a token at startup. `/api/v1/_capabilities` is **always** unauthenticated so siblings can probe the API surface before they hold a secret.

Token-resolution and bind-policy trust matrix, enforced at startup by `HttpReadConfig::validate_auth_trust` before the listener binds:

| Bind | `token_env` resolved | Behaviour |
|---|---|---|
| Loopback | unset | Unauthenticated; allow all requests (matches ADR-014's original posture for the loopback case). |
| Loopback | set | Bearer required on protected routes; capabilities always allowed. |
| Non-loopback | unset | **Refuse to start** with `CLA-CONFIG-HTTP-NO-AUTH`. |
| Non-loopback | set | Bearer required on protected routes. |

Non-loopback-without-auth refusal extends ADR-014's `allow_non_loopback` opt-in: there is no longer a "non-loopback unauthenticated" mode. Non-loopback binds require **both** `allow_non_loopback: true` **and** a resolved `token_env`; either alone is insufficient. The opt-in remains the gate that admits non-loopback binds at all, but it no longer admits them unauthenticated.

Bearer rejection (any of: header absent, wrong scheme, wrong token, blank token) returns `401 Unauthorized` with the standard error envelope and `code: "UNAUTHORIZED"`. Token comparison is constant-time so a wrong-length-token client cannot distinguish "header absent" from "token mismatch" via timing. The token value is never logged; the bind-time log line records `auth=bearer` or `auth=none`, not the token itself.

The token is resolved from an operator-named environment variable; the config field is `serve.http.token_env` (default `CLARION_LOOM_TOKEN`, matching Filigree's pinned client default). The token value itself never appears in `clarion.yaml`.

### 2. `POST /api/v1/files/batch`

Bulk file-identity resolution. One pooled SQLite connection serves the whole batch.

Request body shape (closed):

```json
{
  "queries": [
    {"path": "src/a.py", "language": null},
    {"path": "src/b.py", "language": "python"}
  ]
}
```

Response body shape (closed; arrays are disjoint partitions of the input):

```json
{
  "resolved":         [/* BatchResolvedItem */],
  "not_found":        [/* requested paths */],
  "briefing_blocked": [/* requested paths */],
  "errors":           [/* BatchErrorItem with code+message */]
}
```

The per-batch cap is **256 queries** (`BATCH_MAX_QUERIES` in [`crates/clarion-cli/src/http_read.rs`](../../../crates/clarion-cli/src/http_read.rs); referenced as `queries.len() > 256` in `contracts.md` §"`POST /api/v1/files/batch`"). The request body is additionally bounded at the transport layer to 16 KiB. A `queries` array that exceeds the cap returns `400 BATCH_TOO_LARGE` (new error code, see §3). The cap is not operator-configurable in v1.0 — it is pinned on the wire so Filigree client splitting logic can compile-in the limit. A future incompatible change to the cap is the trigger for `api_version: 2`, not a per-server override.

Individual-item errors (`INVALID_PATH`, `PATH_OUTSIDE_PROJECT`, `STORAGE_ERROR`, `INTERNAL`) go into the `errors` array; the whole request still returns `200 OK` so partial-success semantics are explicit on the wire. Briefing-blocked items are partitioned to the `briefing_blocked` array; the per-item envelope deliberately does not include `entity_id`, `content_hash`, `canonical_path`, or `language` so callers cannot infer file identity from a block-classified item.

### 3. `BRIEFING_BLOCKED` 403 on single-file `GET`

`GET /api/v1/files?path=` for an entity whose `briefing_blocked` anchor is set (per ADR-013) returns `403 Forbidden` with `code: "BRIEFING_BLOCKED"`. The 403 envelope omits `entity_id`, `content_hash`, `canonical_path`, and `language`. The structural signal that "Clarion knows this file but is refusing to expose it" is therefore *only* the status code and `code` discriminator, not any payload field.

ADR-014's original error-code set is extended to:

```
INVALID_PATH | PATH_OUTSIDE_PROJECT | NOT_FOUND |
BRIEFING_BLOCKED | UNAUTHORIZED | STORAGE_ERROR |
BATCH_TOO_LARGE | INTERNAL
```

The set remains closed. Clients must switch on `code`, not on `error` text.

### 4. Stable per-project `instance_id` in `_capabilities`

`GET /api/v1/_capabilities` echoes:

```json
{
  "api_version": 1,
  "instance_id": "9bd7234e-6d44-4a38-9ae4-76f912a10221",
  "registry_backend": true,
  "file_registry": true
}
```

`instance_id` is a v4 UUID created lazily on first `clarion serve` (via `instance::load_or_create` at [`crates/clarion-cli/src/serve.rs:29`](../../../crates/clarion-cli/src/serve.rs)) and persisted to `.clarion/instance_id`. Subsequent `clarion serve` invocations read the existing value. The file is created with mode `0600` on Unix. The ID is stable for the life of that `.clarion/` directory; deleting `.clarion/instance_id` (or the whole `.clarion/` tree) on the next `clarion serve` produces a fresh UUID, and that is intended — siblings use the change as the trigger to invalidate cached identity bindings. The file is excluded from git per ADR-005's exclusion list for per-machine state.

`instance_id` is **not** the same as a deployment fingerprint, a Filigree project ID, or a Clarion release. It is exactly the identity of one local `.clarion/` directory.

## Alternatives Considered

### Alternative 1: HMAC signatures instead of bearer tokens

Better cryptographic story (per-request authentication, replay resistance, no shared-secret leakage on `GET` URLs), but considerably more client-side complexity. Filigree's `ClarionRegistry` would need an HMAC helper, and the operator deployment story would require signing-key rotation discipline. Sprint 3 needed *some* authentication on the wire — bearer tokens are the path of least resistance and the v1.0 federation surface is small enough that a coarse "any reader, full access" trust model is acceptable. HMAC is tracked as post-1.0 hardening (filigree `clarion-6814b2ad90`).

### Alternative 2: Unix Domain Socket-only auth (ADR-012's original posture)

ADR-012 (now superseded) had UDS as the default auth model. Sprint 3's deployment story explicitly admits non-loopback Filigree — a UDS-only model would force Filigree into a sidecar-on-same-host topology that the federation contract does not otherwise require. Bearer over TCP keeps the federation deployment-topology-agnostic.

### Alternative 3: A separate `/api/v1/files/inquire` endpoint for "is this blocked?"

Distinguishing block from not-found with a second endpoint avoids the 403/404 split at the cost of two round trips and a more elaborate wire contract. Filigree's actual question on a single `GET` is "which of these three states is this path in?" and the single-endpoint, status-code-discriminated answer is the simpler shape. Declined.

### Alternative 4: Amend ADR-014 in place

The Sprint 3 tasking doc originally proposed pinning these decisions into ADR-014's body. CLAUDE.md's editorial-conventions section forbids in-place ADR mutation; the doctrine is "to revise an Accepted ADR, write a new ADR that supersedes it." This ADR is that new ADR. ADR-014's status field carries the only allowed mutation (status amendment + reference forward to ADR-034).

## Consequences

### Positive

- The federation HTTP read API now has a per-request authentication primitive. ADR-014's "unauthenticated, loopback-only" promise was a deliberate Sprint 1/2 simplification that was load-bearing for the v1.0 federation surface; the gap is now closed.
- Filigree's cold-start hydration cost drops from O(N) round trips to O(1) with the batch endpoint. The per-item error partitioning means a single malformed path in a 1000-path batch does not poison the whole request.
- `BRIEFING_BLOCKED` is mechanically distinguishable from `NOT_FOUND`. Filigree's escalation logic can branch on the two cases without parsing error text or re-issuing probe requests.
- `instance_id` gives Filigree a stable handle to detect endpoint rebinding. Pre-Sprint-3, a `bind` pointed at a swapped `.clarion/` was indistinguishable from a fresh start; that ambiguity is now resolved on the first capability probe after the swap.

### Negative

- Operators upgrading from ADR-014's unauthenticated posture to a non-loopback federation deployment must now provide a token at startup. The `CLA-CONFIG-HTTP-NO-AUTH` startup refusal makes this a fail-closed migration, not a silent one, but the operator-facing diff is non-trivial.
- Bearer tokens are coarser than HMAC. Any client that holds the token has full read access to the federation surface, and a token leak requires a rotation across both Clarion and every Filigree client. Post-1.0 hardening (HMAC, key rotation) is required for richer deployments.
- The error-code enum is now wider than ADR-014's original. Filigree's `code`-switch logic must handle `BRIEFING_BLOCKED`, `UNAUTHORIZED`, and `BATCH_TOO_LARGE` in addition to the original five. The federation contract documents this; clients that ignore the new codes will surface them as unhandled errors rather than misinterpret them.

### Neutral

- `api_version` remains `1`. The additions are non-breaking augmentations of the v1 wire contract — every pre-Sprint-3 client request shape is still accepted, just with the added option to authenticate, batch, and discriminate on the wider error set. An incompatible change to the read API will be the trigger for `api_version: 2`, not the introduction of these hardenings.
- The Sprint-3 tasking-doc items C1, C2, C5, C7, C8, C9, C10, C11 are not addressed by this ADR — they cover storage correctness and runtime supervision rather than wire-contract decisions. They land directly in the implementation without an ADR change because they implement, rather than amend, ADR-014's existing contract.

## Related Decisions

- [ADR-012](./ADR-012-http-auth-default.md) — original HTTP auth ADR; Superseded by ADR-014, whose Security Posture is in turn partially extended by this ADR.
- [ADR-013](./ADR-013-pre-ingest-secret-scanner.md) — defines the briefing-block anchor whose wire propagation §3 pins.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — registry-backend protocol; §"Security Posture" and §"Error Envelope" extended by this ADR. Other sections (capability probe contents beyond `instance_id`, file identity semantics, canonical-path semantics) remain authoritative.
- [ADR-033](./ADR-033-v1.0-distribution.md) — v1.0 distribution; the hardenings here are part of the 1.0 federation cut.

## References

- Wire spec: [`docs/federation/contracts.md`](../../federation/contracts.md)
- Sprint 3 tasking: [`docs/implementation/sprint-3/2026-05-19-loom-federation-hardening-tasking.md`](../../implementation/sprint-3/2026-05-19-loom-federation-hardening-tasking.md)
- Implementing commits:
  - `1109560` feat(http_read): return 403 BRIEFING_BLOCKED for blocked entities
  - `acbf465` feat(http_read): require Authorization: Bearer for /api/v1/files
  - `eb6200d` feat(http_read): add POST /api/v1/files/batch + document path normalization
  - `2c3311a` feat(http_read): formatting / fix(instance): UUID generation comments / test(serve)
