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
  "entity_id": "core:file:hash-demo@demo.py",
  "content_hash": "hash-demo-file",
  "canonical_path": "demo.py",
  "language": "python"
}
```

Semantics:

- `entity_id` is opaque to Filigree.
- `content_hash` is the drift signal Filigree stores with the resolved row.
- `canonical_path` is Clarion's project-relative canonical path.
- `language` is the normalized language value Clarion used for the resolution.
- Unknown or outside-project paths return a non-2xx JSON error instead of
  guessing.

Fixture: [`fixtures/get-api-v1-files.demo-python.json`](./fixtures/get-api-v1-files.demo-python.json).

### `GET /api/v1/_capabilities`

Reports whether this Clarion instance can serve the registry-backend read
contract.

Successful response:

```json
{
  "registry_backend": true,
  "file_registry": true,
  "version": "0.1"
}
```

Filigree should treat `registry_backend: true` as the flag that the
`/api/v1/files` resolution surface is present.

Fixture: [`fixtures/get-api-v1-capabilities.json`](./fixtures/get-api-v1-capabilities.json).

