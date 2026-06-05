# ADR-042: HMAC Freshness and Replay Window

**Status**: Accepted
**Date**: 2026-06-04
**Deciders**: qacona@gmail.com
**Context**: Comprehensive security audit M9 found ADR-034's HMAC identity
authenticated request bytes but did not bind freshness, so a captured signed
request could be replayed inside the same deployment.
**Amends**: [ADR-034](./ADR-034-federation-http-read-api-hardening.md) HMAC
identity message shape.

## Summary

Loomweave's protected HTTP routes keep ADR-034's preferred Weft component HMAC
mode, but every signed request now carries `X-Weft-Timestamp` and
`X-Weft-Nonce`. The HMAC canonical message is:

```text
METHOD
PATH_AND_QUERY
SHA256_HEX_OF_REQUEST_BODY
X_WEFT_TIMESTAMP
X_WEFT_NONCE
```

Loomweave accepts timestamps inside a five-minute skew window and rejects reuse of
the same nonce inside the process-local replay cache for that window. Missing,
malformed, stale, replayed, or wrongly signed requests all return the existing
`401 UNAUTHENTICATED` envelope.

## Decision

- Replace the local HMAC implementation with the `hmac` crate over `Sha256`.
- Replace local byte-loop equality with `subtle::ConstantTimeEq`.
- Require `X-Weft-Timestamp` as Unix seconds and `X-Weft-Nonce` as a non-empty
  opaque string up to 128 bytes whenever `identity_token_env` is active.
- Include timestamp and nonce in the canonical HMAC message after the body hash.
- Maintain an in-memory, process-local nonce cache with a five-minute freshness
  window. A server restart clears the cache; the timestamp bound still limits
  replay usefulness across restarts.
- Preserve the legacy bearer-token mode when `identity_token_env` is absent.

## Consequences

- A captured HMAC request is no longer replayable during a running server
  process unless the attacker can also mint a fresh signature.
- Sibling clients must update their signing helper to add timestamp and nonce
  headers. This is an intentional hardening change to the authenticated wire
  shape; the authoritative contract is `docs/federation/contracts.md`
  §Authentication.
- The five-minute window is a local-federation compromise: wide enough for modest
  clock skew, narrow enough to bound captured-request utility. Wider skew or key
  rotation needs a successor ADR rather than an environment knob.
- The cache is per process, not durable. Durable nonce storage would add write
  pressure to a read path and is unnecessary for the local-first threat model.

## Related Decisions

- [ADR-034](./ADR-034-federation-http-read-api-hardening.md) — introduces the
  preferred HMAC identity mode this ADR amends.
- [ADR-036](./ADR-036-wardline-taint-fact-store.md) — `/api/wardline/*` routes
  inherit this HMAC gate, including the larger body-read limit.
- [ADR-037](./ADR-037-shared-error-vocabulary.md) — no new HTTP error code is
  introduced; freshness failures reuse `UNAUTHENTICATED`.
