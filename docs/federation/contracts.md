# Clarion Federation Contracts

This file pins Clarion's read-side contract for sibling products. The initial
consumer is Filigree's `ClarionRegistry` from ADR-014.

## HTTP Read API

`clarion serve` can expose the HTTP read API when enabled in `clarion.yaml`:

```yaml
serve:
  http:
    enabled: true
    bind: 127.0.0.1:9111
```

The MCP stdio server remains available on stdin/stdout. The HTTP surface is
read-only and uses Clarion's existing SQLite reader pool.

All non-2xx responses use this closed JSON error envelope:

```json
{
  "error": "path does not resolve to a Clarion file entity",
  "code": "NOT_FOUND"
}
```

The initial `code` enum is closed to `INVALID_PATH`,
`PATH_OUTSIDE_PROJECT`, `NOT_FOUND`, `STORAGE_ERROR`, and `INTERNAL`.
Clients must switch on `code`; `error` is human-readable diagnostic text.

### `GET /api/v1/files?path=&language=`

Resolves a project-relative or absolute file path to the Clarion file identity
Filigree stores as `file_records.id` when `registry_backend: clarion` is active.

Query parameters:

| Name | Required | Meaning |
|---|---:|---|
| `path` | yes | File path under the Clarion project root. |
| `language` | no | Caller-supplied language hint. If absent, Clarion infers from the source entity or file extension. |

Successful response:

```json
{
  "entity_id": "core:file:demo.py",
  "content_hash": "hash-demo-file",
  "canonical_path": "demo.py",
  "language": "python"
}
```

Semantics:

- `entity_id` is opaque to Filigree and follows ADR-003's file-kind shape
  `core:file:{qualified_name}`.
- `content_hash` is the drift signal Filigree stores with the resolved row.
- `canonical_path` is Clarion's project-relative canonical path: no leading
  `/`, no leading `./`, no trailing `/`, and `/` as the separator on every
  platform.
- `language` is the normalized language value Clarion used for the resolution.
- Unknown or outside-project paths return a non-2xx JSON error instead of
  guessing.
- If no file-kind entity row exists for the path, Clarion returns
  `404` with `code: "NOT_FOUND"` instead of synthesizing a file ID.

The contract fixture at
[`fixtures/get-api-v1-files.demo-python.json`](./fixtures/get-api-v1-files.demo-python.json)
is normative for this section. It includes `_meta`, `shape_decl`, and examples
for the happy path, not-known, blank path, outside-root, and storage-error
responses.

### `GET /api/v1/_capabilities`

Reports whether this Clarion instance can serve the registry-backend read
contract.

Successful response:

```json
{
  "api_version": 1,
  "instance_id": "9bd7234e-6d44-4a38-9ae4-76f912a10221",
  "registry_backend": true,
  "file_registry": true
}
```

Filigree should treat `registry_backend: true` as the flag that the
`/api/v1/files` resolution surface is present.

`api_version` is the HTTP read API wire-contract version, not Clarion product
semver. It increments only for incompatible changes to the wire contract
consumed by existing Filigree clients.

`instance_id` is the stable per-project Clarion instance fingerprint persisted
in `.clarion/instance_id`. Filigree should treat a changed `instance_id` for a
previously known endpoint as evidence that it is now talking to a different
Clarion project instance.

The contract fixture at
[`fixtures/get-api-v1-capabilities.json`](./fixtures/get-api-v1-capabilities.json)
is normative for this section. Its shape declaration pins `api_version` and
asserts that `instance_id` is a UUID; the example uses a seeded stable ID.
