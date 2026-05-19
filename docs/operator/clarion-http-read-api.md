# Clarion HTTP Read API

Clarion can expose a read-only HTTP API for local sibling integrations such as
Filigree's `registry_backend: clarion` mode. The wire contract is documented in
the [federation contracts](../federation/contracts.md).

## Trust Model

The HTTP read API is unauthenticated. By default, `clarion serve` binds it only
to loopback addresses so it is reachable from local processes on the same host,
not from the network.

Do not bind the HTTP read API to a non-loopback address unless an authenticated
reverse proxy or equivalent access-control layer is in front of it. Clarion
refuses non-loopback binds unless `serve.http.allow_non_loopback: true` is set.
That opt-in is an operator assertion that the unauthenticated Clarion HTTP
surface is protected outside Clarion.

Startup logs for non-loopback opt-in must warn that the API is unauthenticated.
Treat the endpoint as source-code metadata exposure: anyone who can reach it can
read Clarion's catalog responses for the project.

## Contract Summary

`GET /api/v1/_capabilities` returns the read API `api_version`, the project
`instance_id`, and booleans indicating whether registry-backend file resolution
is available.

`GET /api/v1/files?path=&language=` resolves an existing Clarion file-kind row
to the entity ID and project-relative canonical path Filigree should store. It
fails closed when the path is invalid, outside the project, missing from the
catalog, or unavailable because of storage errors.
