# ADR-056: Identity Response Ownership and Authentication Discovery

**Status**: Accepted
**Date**: 2026-07-12
**Deciders**: john
**Extends**: [ADR-034](./ADR-034-federation-http-read-api-hardening.md), [ADR-042](./ADR-042-hmac-freshness-and-replay-window.md)

## Context

The unauthenticated `GET /api/v1/_capabilities` response already identifies a
serving project with `api_version` and `instance_id`. Identity resolution is a
separate protected request, however, and its success bodies previously carried
neither value. A client could therefore probe project A, have a reused local
port rebound to project B, resolve an identity from B, and combine it with A's
local SQLite catalogue. Capability ownership alone cannot close that
time-of-check/time-of-use gap.

Federation clients also had to infer whether protected routes expected bearer,
HMAC, or no credentials. Guessing creates noisy `401` probes and encourages
clients to send credentials to endpoints without a producer-declared mode.
Discovery must remain safe on the unauthenticated capability route: it may name
the wire mode, but never a secret, signature, nonce, timestamp, credential
header, or environment-variable pointer.

## Decision

### 1. Add non-secret authentication discovery

`GET /api/v1/_capabilities` adds exactly:

```json
{
  "authentication": {
    "protected_routes": "none|bearer|hmac",
    "capabilities_probe": "none",
    "contract_version": 1
  }
}
```

`protected_routes` reports the effective startup mode after secret resolution.
HMAC takes precedence whenever `serve.http.identity_token_env` resolves;
otherwise a resolved `serve.http.token_env` selects bearer; otherwise loopback
serves in `none` mode. `capabilities_probe` is always `none`, meaning the probe
is unauthenticated. The object contains no configuration key names, environment
variable names, credential values, signatures, timestamps, or nonces.

This is an additive v1 response field, so HTTP `api_version` remains `1`.
`authentication.contract_version` versions only this descriptor.

### 2. Bind ownership into every identity success body

Every successful identity response carries top-level `api_version: 1` and the
serving state's `instance_id`, including:

- locator resolve, both alive and not alive;
- batch resolve envelopes;
- SEI lookup, both alive and not alive;
- SEI lineage envelopes.

Error envelopes remain the closed shared HTTP error shape and do not gain
ownership fields. Ownership describes evidence a client may join; an error is
not joinable identity evidence.

Loomweave provides a pure reference validator that reads the two ownership
fields from capability and identity JSON. It rejects malformed UUIDs, malformed
or unsupported API versions, a capability instance different from the local
instance file, and an identity-response instance different from that same local
instance. It opens no catalogue connection.

The required client order is:

1. read the existing local `.weft/loomweave/instance_id` without creating it;
2. call `_capabilities` without credentials;
3. require integer `api_version == 1`, a UUID `instance_id`, and equality with
   the local instance ID;
4. select credentials from the declared authentication mode;
5. issue the protected identity request;
6. require its `api_version` and `instance_id` to match both the capability
   response and the local instance ID;
7. only then query or join local catalogue rows.

Plainweave owns enforcement of this local join order. Loomweave owns the
producer fields, their serializer tests, and the reference validator. Repeating
the ownership check on the identity body means a reused-port A-to-B switch is
detected before the local join even when the earlier capability probe belonged
to A.

### 3. Pin the client authentication contract

Loomweave server configuration uses:

- `serve.http.identity_token_env`: optional name of the HMAC-secret variable;
- `serve.http.token_env`: name of the bearer-token variable, default
  `WEFT_TOKEN`.

The canonical consumer pointer variables are:

- `WEFT_LOOMWEAVE_IDENTITY_TOKEN_ENV`, naming the HMAC-secret variable and
  defaulting to the name `WEFT_IDENTITY_SECRET`;
- `WEFT_LOOMWEAVE_TOKEN_ENV`, naming the bearer-token variable and defaulting
  to the name `WEFT_TOKEN`.

On the consumer side, a blank pointer falls back to its default target name and
a blank resolved credential is absent. On the Loomweave server, configuring
`serve.http.identity_token_env` commits to HMAC: a missing or blank resolved
secret hard-fails startup with `LMWV-CONFIG-HTTP-IDENTITY-MISSING`, including on
loopback. A missing or blank bearer value is unauthenticated only on loopback;
without HMAC it makes a non-loopback startup fail with
`LMWV-CONFIG-HTTP-NO-AUTH`. A nonblank HMAC secret always takes precedence over
bearer. Clients do not authenticate `_capabilities`.

Loomweave's listener is HTTP; it does not terminate TLS. Non-loopback operators
must place it on a trusted network or behind a TLS-terminating proxy. Consumers
may impose a stricter origin policy (Plainweave refuses to send credentials to
non-loopback cleartext HTTP), but that is consumer enforcement rather than a
Loomweave HTTPS feature.

Bearer uses exactly:

```http
Authorization: Bearer <token>
```

Scheme case, the single separator, and token boundaries are literal. Blank or
padded forms are rejected rather than trimmed.

HMAC uses exactly:

```http
X-Weft-Component: loomweave:<lowercase-hex-hmac-sha256>
X-Weft-Timestamp: <unix-seconds>
X-Weft-Nonce: <nonempty-opaque-value-up-to-128-bytes>
```

The signed UTF-8 bytes are five newline-separated fields with no trailing
newline:

```text
UPPERCASE_METHOD
EXACT_PATH_AND_QUERY
LOWERCASE_SHA256_HEX_OF_EXACT_BODY_BYTES
DECIMAL_UNIX_TIMESTAMP
EXACT_NONCE
```

The method is normalized to uppercase. Path and query retain their transmitted
ordering and encoding. The body digest covers the exact bytes subsequently
sent. A component value is exactly `loomweave:` plus 64 lowercase hexadecimal
characters. Timestamp text is canonical decimal Unix seconds with no sign,
leading zeroes, or padding. The nonce is never trimmed or canonicalized: its
exact parsed header-value bytes are signed and used as the replay-cache key.
Empty, whitespace-only, and over-128-byte nonces are rejected. The timestamp
window is inclusive at `now - 300` and `now + 300` seconds.
A nonce can succeed only once in a server process while retained by the replay
cache. A poisoned replay-cache lock fails closed.

Missing, malformed, blank, wrong, stale, replayed, or otherwise invalid bearer
or HMAC credentials all return the same response:

```http
HTTP/1.1 401 Unauthorized
Content-Type: application/json

{"error":"authentication required","code":"UNAUTHENTICATED"}
```

Request logging scrubs `Authorization`, `X-Weft-Component`,
`X-Weft-Timestamp`, and `X-Weft-Nonce`. The component header may contribute
only the non-secret component kind `loomweave` to structured logs. Capability
responses and authentication failures never echo secrets, pointer names,
signatures, timestamps, nonces, or request credentials.
Malformed component values contribute no structured log field; caller text is
never copied into the log context.

## Consequences

### Positive

- A capability/identity race cannot silently produce cross-project joined
  evidence.
- Clients discover the supported authentication mode without credential probes
  or secret disclosure.
- Canonical vectors are deterministic because freshness/replay validation can
  be evaluated against an explicit `now` value.
- Every credential failure is structurally identical and redacted.

### Negative

- Identity success bodies grow by two fields.
- Clients that join identity and SQLite data must retain and compare ownership
  twice rather than treating capability discovery as a one-time liveness probe.
- The replay cache remains process-local; a restart clears nonce history, while
  the timestamp window continues to bound replay.

## Alternatives Considered

### Trust the capability probe for the entire client operation

Rejected. It leaves a port-rebinding window between probe and identity request.

### Authenticate the capability probe

Rejected. Clients need safe pre-auth discovery and project ownership before
selecting credentials. The descriptor is deliberately non-secret.

### Return a full catalogue snapshot from the identity route

Deferred. That would remove the remote/local join but materially widen the HTTP
contract. Response ownership closes the current seam without duplicating the
catalogue projection.

## Verification

Task 5 pins production parsing and focused success/failure behavior. The
producer-owned, byte-pinned bearer/HMAC success/failure golden matrix remains a
Task 6 deliverable; this ADR does not claim that matrix is complete.

- `cargo test -p loomweave-cli --bin loomweave http_read::auth`
- `cargo test -p loomweave-cli --bin loomweave http_read::identity`
- `cargo test -p loomweave-cli --test serve`
- `wardline scan . --fail-on ERROR`

## References

- [Federation HTTP contract](../../federation/contracts.md#http-read-api)
- `crates/loomweave-cli/src/http_read.rs`
- `crates/loomweave-cli/src/http_read/auth.rs`
- `crates/loomweave-cli/src/http_read/identity.rs`
