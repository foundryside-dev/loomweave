# Clarion HTTP Read API

Clarion can expose a read-only HTTP API for local sibling integrations such as
Filigree's `registry_backend: clarion` mode. The wire contract is documented in
the [federation contracts](../federation/contracts.md).

## Trust Model

By default, `clarion serve` binds the HTTP read API only to loopback addresses so
it is reachable from local processes on the same host, not from the network. A
loopback-only API may run without authentication for local sidecar workflows.

For authenticated mode, set `serve.http.identity_token_env` to the name of an
environment variable that contains the shared Loom component secret:

```yaml
serve:
  http:
    enabled: true
    bind: 127.0.0.1:9111
    identity_token_env: CLARION_LOOM_IDENTITY_SECRET
```

When `identity_token_env` is configured, Clarion refuses to start unless the env
var is present and non-empty. Protected `/api/v1/files` routes then require
`X-Loom-Component: clarion:<hmac>`. The HMAC is lowercase hex HMAC-SHA256 over:

```text
<METHOD>
<PATH_AND_QUERY>
<SHA256_HEX_OF_REQUEST_BODY>
```

For example, a GET of `/api/v1/files?path=demo.py&language=python` signs the
method `GET`, that exact path-and-query string, and the SHA-256 hash of an empty
body. `GET /api/v1/_capabilities` stays unauthenticated so siblings can probe
the API surface before sending protected reads.

Clarion still accepts the older `serve.http.token_env` bearer-token path for
compatibility. Prefer `identity_token_env` for new deployments.

Clarion refuses non-loopback binds unless `serve.http.allow_non_loopback: true`
is set. Non-loopback deployments must also configure authentication; otherwise
Clarion refuses to bind. Treat the endpoint as source-code metadata exposure:
anyone who can reach it can read Clarion's catalog responses for the project.

## Contract Summary

`GET /api/v1/_capabilities` returns the read API `api_version`, the project
`instance_id`, and booleans indicating whether registry-backend file resolution
is available.

`GET /api/v1/files?path=&language=` resolves an existing Clarion file-kind row
to the entity ID and project-relative canonical path Filigree should store. It
fails closed when the path is invalid, outside the project, missing from the
catalog, or unavailable because of storage errors.

## Trust assumption: loopback-no-token mode

When both `serve.http.token_env` (legacy bearer) and
`serve.http.identity_token_env` (HMAC, preferred per
[ADR-034](../clarion/adr/ADR-034-federation-hardening.md)) are unset and the
bind is loopback (default: `127.0.0.1:9111`), the HTTP read API serves
unauthenticated. This is the intended single-user developer-workstation
trust model — the loopback socket is reachable only from processes on the
same host, and Clarion's catalogue is no more sensitive than the project
source those processes can already read.

**On a multi-tenant developer host or shared CI runner this trust model
does not hold.** Any local process — any UID with read access to the
loopback bind socket — can read the entire non-blocked catalogue,
including every file's `entity_id`, `canonical_path`, `language`, and
`content_hash`. This is the documented v1.0 trust matrix and is not a
defect, but operators on multi-tenant hosts must configure authentication
before binding.

Multi-tenant operators MUST set `identity_token_env` (HMAC, preferred) or
`token_env` (bearer, legacy) before running `clarion serve`. The HMAC
configuration shape is documented in the [Trust Model](#trust-model)
section above.

The Clarion `serve` startup banner emits a `[TRUST]` line warning when
loopback-no-token mode is active (forward-reference: the banner code is a
SEC-02 follow-up PR; the trust assumption itself is current as of v1.0).
