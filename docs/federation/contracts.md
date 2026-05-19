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
    # Name of the env var holding the inbound bearer token. Optional on a
    # loopback bind, required on a non-loopback bind. Default
    # `CLARION_LOOM_TOKEN` matches Filigree's pinned client default.
    token_env: CLARION_LOOM_TOKEN
```

The MCP stdio server remains available on stdin/stdout. The HTTP surface is
read-only and uses Clarion's existing SQLite reader pool.

### Authentication

The `/api/v1/files`-family endpoints require
`Authorization: Bearer <token>` when Clarion has resolved a token at
startup; `/api/v1/_capabilities` is **always** unauthenticated so
siblings can probe the API surface pre-auth.

Trust matrix (enforced by `HttpReadConfig::validate_auth_trust` at
startup, before binding):

| Bind | `token_env` resolved | Behaviour |
|---|---|---|
| Loopback | unset | Unauthenticated; allow all requests. |
| Loopback | set | Bearer required on protected routes; capabilities always allowed. |
| Non-loopback | unset | **Refuse to start** with `CLA-CONFIG-HTTP-NO-AUTH`. |
| Non-loopback | set | Bearer required on protected routes. |

Bearer rejection (any of: header absent, wrong scheme, wrong token,
blank token) returns:

```http
HTTP/1.1 401 Unauthorized
Content-Type: application/json

{"error": "authentication required", "code": "UNAUTHORIZED"}
```

Token comparison is constant-time so a wrong-length-token client cannot
distinguish "header absent" from "token mismatch" via timing. The token
itself is never logged; the bind-time log line records
`auth=bearer` or `auth=none`, not the token value.

All non-2xx responses use this closed JSON error envelope:

```json
{
  "error": "path does not resolve to a Clarion file entity",
  "code": "NOT_FOUND"
}
```

The initial `code` enum is closed to `INVALID_PATH`,
`PATH_OUTSIDE_PROJECT`, `NOT_FOUND`, `BRIEFING_BLOCKED`, `UNAUTHORIZED`,
`STORAGE_ERROR`, and `INTERNAL`. Clients must switch on `code`; `error`
is human-readable diagnostic text.

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
- If the file-kind entity row carries a `briefing_blocked` property (set
  by the pre-ingest secret scanner or the unscanned-source defense-in-
  depth path), Clarion returns `403` with
  `code: "BRIEFING_BLOCKED"` and the response body
  `{"error": "entity is briefing-blocked and cannot be exposed",
  "code": "BRIEFING_BLOCKED"}`. The response does not include the
  `entity_id`, `content_hash`, `canonical_path`, or `language` fields,
  so Filigree must not infer file identity from a 403 envelope. Distinguish
  this from `404 NOT_FOUND`, which means no entity row exists at all;
  `403 BRIEFING_BLOCKED` confirms the file is known but withheld.

The contract fixture at
[`fixtures/get-api-v1-files.demo-python.json`](./fixtures/get-api-v1-files.demo-python.json)
is normative for this section. It includes `_meta`, `shape_decl`, and examples
for the happy path, not-known, blank path, outside-root, briefing-blocked,
and storage-error responses.

### `POST /api/v1/files/batch`

Resolves up to **256** file paths in a single request. Filigree's
`ClarionRegistry` uses this for cold-start hydration so that one rehydration
costs one round-trip and one pooled-connection checkout, rather than N of each.

Request body (`application/json`, max 16 KiB):

```json
{
  "queries": [
    {"path": "src/foo.py", "language": "python"},
    {"path": "src/bar.py", "language": ""}
  ]
}
```

Successful response (`200 OK`) — every input path is partitioned into exactly
one of four lists:

```json
{
  "resolved": [
    {
      "requested_path": "src/foo.py",
      "entity_id": "core:file:src/foo.py",
      "content_hash": "<hash>",
      "canonical_path": "src/foo.py",
      "language": "python"
    }
  ],
  "not_found": ["src/missing.py"],
  "briefing_blocked": ["src/secrets.py"],
  "errors": [
    {
      "requested_path": "../escapes.py",
      "code": "PATH_OUTSIDE_PROJECT",
      "message": "path is outside project root"
    }
  ]
}
```

Semantics:

- `resolved[*]` echoes the requested path back as `requested_path` so the
  client can correlate without re-canonicalising; the rest of the fields
  match the `GET /api/v1/files` response shape for the same input.
- `not_found[]` and `briefing_blocked[]` are plain string arrays of the
  requested paths — Filigree must not infer file identity from the
  `briefing_blocked` partition (same withholding semantics as the
  single-file `403 BRIEFING_BLOCKED`).
- `errors[]` carries per-path resolution errors (`INVALID_PATH`,
  `PATH_OUTSIDE_PROJECT`, `STORAGE_ERROR`, `INTERNAL`). Errors are
  per-item, not envelope-level; the response is still `200 OK`.

Failure modes (envelope-level):

| Status | Code | When |
|---|---|---|
| 400 | `INVALID_PATH` | Body is not a valid `{"queries": [...]}` JSON object. |
| 400 | `BATCH_TOO_LARGE` | `queries.len() > 256`. Filigree must split client-side. |
| 401 | `UNAUTHORIZED` | Bearer auth missing or wrong (when configured — see §Authentication). |
| 413 | n/a | Request body exceeds the 16 KiB cap (transport-level). |
| 500/503 | `STORAGE_ERROR` / `INTERNAL` | Whole-batch storage failure. |

ETag is **not** applied to the batch endpoint; clients that want
conditional fetch semantics should use the single-file endpoint. The whole
batch runs inside one pooled `ReaderPool::with_reader` checkout —
implementors must not regress this to per-query checkout, since the
per-query model defeats the only reason the endpoint exists.

The contract fixture at
[`fixtures/post-api-v1-files-batch.json`](./fixtures/post-api-v1-files-batch.json)
is normative for this section.

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

## Path normalization

Both `GET /api/v1/files` and `POST /api/v1/files/batch` accept the same
input-path shape:

- **Lexical**, not filesystem-canonical. Path normalization joins the
  configured project root with the requested path (or treats an absolute
  path as-is when it falls under the project root), then folds `.` /
  `..` lexically. The path **does not need to exist on disk** at lookup
  time — Clarion resolves against its entity catalog
  (`entities.source_file_path`), not against `stat(2)`. This is important
  for replay scenarios where the catalog row outlives the file.
- **Forward-slash separators only**. Both project-relative paths
  (`src/foo.py`) and project-root-anchored absolute paths
  (`/var/run/clarion-corpus/src/foo.py`) are accepted; backslash
  separators are not.
- **Project-relative or absolute under the project root**. A request
  whose normalized form escapes the project root returns 400
  `PATH_OUTSIDE_PROJECT` (single-file) or surfaces as an
  `errors[].code = "PATH_OUTSIDE_PROJECT"` entry (batch).
- **Symlink-resolved equivalents are not reconciled**. If your project
  contains symlinks, both Clarion and Filigree must agree on the same
  canonical form for the same logical file (typically the lexically-
  joined form). Clarion does **not** call `canonicalize()` on the
  request path; the catalog row carries the canonical form chosen at
  ingest.

Reference implementation: `clarion-storage::query::normalize_lookup_path`
(file path: [`crates/clarion-storage/src/query.rs`](../../crates/clarion-storage/src/query.rs)).
The function signature is stable for the lifetime of `api_version: 1`;
the *implementation* is free to change as long as the lexical /
no-disk-touch / forward-slash / under-root contract holds.
