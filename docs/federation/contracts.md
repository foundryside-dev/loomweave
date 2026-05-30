# Clarion Federation Contracts

This file pins Clarion's federation contracts in both directions: the surface
Clarion *exposes* to sibling products, and the conventions and routes Clarion
*consumes* from Filigree. The exposed surface was historically read-only — the
file-resolution read API consumed by Filigree's `ClarionRegistry` (ADR-014). At
release:1.1 it also includes one **write** surface: the Wardline taint-fact store
(ADR-036), a disabled-by-default `/api/wardline/*` sub-router that lets Wardline
persist per-entity taint facts into Clarion's catalog so briefings can carry them.
That write surface is enrich-only in the loom.md §5 sense — it is off unless
explicitly enabled, Clarion never requires Wardline to be present, and Clarion's
own semantics never depend on a taint fact existing. Every consume-side coupling
here is likewise enrich-only and fail-soft — Clarion stays solo-useful when
Filigree is absent (loom.md §5).

## HTTP Read API

`clarion serve` can expose the HTTP read API when enabled in `clarion.yaml`:

```yaml
serve:
  http:
    enabled: true
    bind: 127.0.0.1:9111
    # Preferred 1.0 identity mode. Optional on loopback, required for
    # authenticated Loom component requests.
    identity_token_env: CLARION_LOOM_IDENTITY_SECRET
    # Name of the env var holding the inbound bearer token. Optional on a
    # loopback bind, accepted for compatibility on non-loopback binds. Default
    # `CLARION_LOOM_TOKEN` matches Filigree's pinned client default.
    token_env: CLARION_LOOM_TOKEN
```

The MCP stdio server remains available on stdin/stdout. The `/api/v1/*` read API
is read-only and uses Clarion's existing SQLite reader pool. The `/api/wardline/*`
sub-router (see [Wardline taint-fact store](#wardline-taint-fact-store-sp9)) adds
one write path — `POST /api/wardline/taint-facts` — which is disabled by default
and, when enabled, routes through Clarion's writer-actor rather than the reader
pool; its read paths still use the reader pool.

### Authentication

The `/api/v1/files`-family endpoints require
`X-Loom-Component: clarion:<hmac>` when Clarion has resolved
`serve.http.identity_token_env` at startup. The HMAC is lowercase hex
HMAC-SHA256 over the canonical message:

```text
<METHOD>
<PATH_AND_QUERY>
<SHA256_HEX_OF_REQUEST_BODY>
```

`/api/v1/_capabilities` is **always** unauthenticated so siblings can probe the
API surface pre-auth. Clarion still accepts the older
`Authorization: Bearer <token>` path when `token_env` resolves and
`identity_token_env` is not configured.

Trust matrix (enforced by `HttpReadConfig::validate_auth_trust` at
startup, before binding):

| Bind | `identity_token_env` resolved | `token_env` resolved | Behaviour |
|---|---|---|---|
| Loopback | unset | unset | Unauthenticated; allow all requests. |
| Loopback | set | any | HMAC required on protected routes; capabilities always allowed. |
| Loopback | configured but env missing | any | **Refuse to start** with `CLA-CONFIG-HTTP-IDENTITY-MISSING`. |
| Non-loopback | set | any | HMAC required on protected routes. |
| Non-loopback | unset | set | Bearer required on protected routes. |
| Non-loopback | unset | unset | **Refuse to start** with `CLA-CONFIG-HTTP-NO-AUTH`. |

Authentication rejection (header absent, wrong scheme/prefix, wrong token or
signature, blank token or signature) returns:

```http
HTTP/1.1 401 Unauthorized
Content-Type: application/json

{"error": "authentication required", "code": "UNAUTHENTICATED"}
```

Secret comparison is constant-time so a wrong-length client cannot distinguish
"header absent" from "secret mismatch" via timing. The secret itself is never
logged; the bind-time log line records `auth=hmac`, `auth=bearer`, or
`auth=none`, not the secret value.

All non-2xx responses use this closed JSON error envelope:

```json
{
  "error": "path does not resolve to a Clarion file entity",
  "code": "NOT_FOUND"
}
```

The `code` enum is closed to `INVALID_PATH`,
`PATH_OUTSIDE_PROJECT`, `NOT_FOUND`, `BRIEFING_BLOCKED`, `UNAUTHENTICATED`,
`STORAGE_ERROR`, `BATCH_TOO_LARGE`, `WRITE_DISABLED`, `PROJECT_MISMATCH`,
and `INTERNAL`. Clients must switch on `code`; `error` is human-readable
diagnostic text. `WRITE_DISABLED` and `PROJECT_MISMATCH` are emitted only by
the `/api/wardline/*` routes (see
[Wardline taint-fact store](#wardline-taint-fact-store-sp9)). `BATCH_TOO_LARGE`
is emitted by `POST /api/v1/files/batch` (as `400`) and by the `/api/wardline/*`
batch routes (as `413`) — the same `code` carries a **different HTTP status by
endpoint**, so a client must route on `code`, not on status.

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

### `POST /api/v1/files:resolve`

Resolves up to **1000** file paths in one request while preserving one response
slot per input path. This is an optimization for high-volume callers that want
single-GET semantics without one HTTP round trip per path.

Request body (`application/json`, max 16 KiB):

```json
{
  "paths": [
    {"path": "src/foo.py", "language": "python"},
    {"path": "src/bar.py"}
  ]
}
```

Successful response (`200 OK`):

```json
{
  "results": [
    {
      "path": "src/foo.py",
      "response": {
        "status": "resolved",
        "body": {
          "entity_id": "core:file:src/foo.py",
          "content_hash": "<hash>",
          "canonical_path": "src/foo.py",
          "language": "python"
        }
      }
    },
    {
      "path": "src/missing.py",
      "response": {
        "status": "not_found",
        "body": {
          "error": "file is not known to Clarion",
          "code": "NOT_FOUND"
        }
      }
    }
  ]
}
```

Per-path `response.status` is one of:

- `resolved` — `body` is the same shape as `GET /api/v1/files`.
- `not_found` — no file-kind entity row exists.
- `blocked` — the entity is known but `briefing_blocked`; identity fields are
  withheld, matching the single-file `403 BRIEFING_BLOCKED` posture.
- `error` — per-path validation or storage error; switch on `body.code`.

Envelope-level failures:

| Status | Code | When |
|---|---|---|
| 400 | `INVALID_PATH` | Body is not a valid `{"paths": [...]}` object or `paths.len() > 1000`. |
| 401 | `UNAUTHENTICATED` | HMAC or bearer auth missing or wrong (when configured — see §Authentication). |
| 413 | n/a | Request body exceeds the 16 KiB cap. |
| 500/503 | `STORAGE_ERROR` / `INTERNAL` | Whole-batch storage failure. |

ETag is not applied to this endpoint. `GET /api/v1/files` remains the
canonical per-URI resolution model; `files:resolve` is a batch transport
optimization.

The contract fixture at
[`fixtures/post-api-v1-files-resolve.batch.json`](./fixtures/post-api-v1-files-resolve.batch.json)
is normative for this section.

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
| 401 | `UNAUTHENTICATED` | HMAC or bearer auth missing or wrong (when configured — see §Authentication). |
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

## Wardline qualname normalization (entity reconciliation)

This contract governs how a sibling that emits Findings against Python code
(Wardline's native Filigree emitter, per ADR-018's 2026-05-29 amendment and the
2026-05-29 integration brief §4.A) must spell the entity it references so Clarion
can reconcile it. It is *enrich-only*: when the contract is honored, Clarion
attaches the entity's structural context to the Finding; when it is not, Clarion
degrades to `resolution_confidence: heuristic|none` — there is no error and no
broken state, only a worse match. Filigree's own ticket lifecycle is unaffected
either way (loom.md §5).

**The composed form.** A Finding carries `metadata.wardline.qualname` as the
**pre-composed** dotted name (Clarion's L7 `canonical_qualified_name`), not a
`(file, bare-qualname)` pair. The composition is two parts:

```text
metadata.wardline.qualname = module_dotted_name(file_path) + "." + __qualname__
```

- `module_dotted_name(file_path)` is Clarion's module-prefix rule. Its canonical
  implementation and tests are
  [`module_dotted_name`](../../plugins/python/src/clarion_plugin_python/extractor.py)
  and `test_module_dotted_name_helper` in
  [`test_extractor.py`](../../plugins/python/tests/test_extractor.py). The rule:
  strip a leading `src/` **only at position 0**; drop the `.py` suffix; collapse
  an `__init__` filename to its package; join the rest with `.`. No other root
  marker (`lib/`, `app/`, the project name, …) is stripped, and a top-level
  `__init__.py` normalizes to the empty string and is **not emitted** (Clarion
  rejects an empty qualified name).
- `__qualname__` is copied **verbatim** — `<locals>` closure markers and dotted
  nested-class chains are preserved, never rewritten.

**Normative vectors.** The byte-exact `(file_path, qualname) → dotted form`
parity set lives in
[`fixtures/wardline-qualname-normalization.json`](./fixtures/wardline-qualname-normalization.json).
It is a standalone spec vector in the same spirit as the cross-language
`fixtures/entity_id.json`: it deliberately includes the divergence traps where
naive composition silently mismatches (non-`src` roots, `src` not at position 0,
`<locals>` closures, nested-class chains, namespace-package layouts, the rejected
top-level `__init__.py`). A conformant emitter reproduces every vector exactly.

**Conformance oracle (deferred).** A live check —
`GET /api/v1/entities/resolve?scheme=wardline_qualname`, which would return
`exact | heuristic | none` for a candidate qualname *with normalization* — is
named in ADR-018 as the eventual conformance surface but is **not implemented at
release:1.1**. Until it ships, the fixture above is the contract: validate against
it offline. Building the endpoint ahead of a shipped Wardline consumer would be
speculative forward-work (loom.md §5 — Clarion translates qualnames because it owns
the catalog, but only when a consumer needs it). What *did* ship is the narrower,
exact-tier `POST /api/wardline/resolve` (see
[Wardline taint-fact store](#wardline-taint-fact-store-sp9)), which takes
**pre-composed** dotted qualnames and does a direct existence lookup with no
normalization. The heuristic resolution tier and the normalizing raw-qualname
conformance oracle both remain deferred to Flow B B.2 (`clarion-ca2d26ffbe`); B.2
extends the same resolver rather than reimplementing it.

## Wardline taint-fact store (SP9)

This pins the `/api/wardline/*` sub-router Clarion *exposes* to Wardline (ADR-036;
design spec
[`2026-05-30-clarion-wardline-taint-store-design.md`](../superpowers/specs/2026-05-30-clarion-wardline-taint-store-design.md)).
Wardline computes per-entity taint facts and persists them into Clarion's catalog
so Clarion can fold them into briefings; Clarion treats every fact's payload as an
**opaque blob** and never asserts whether it is fresh. This is enrich-only and
disabled-by-default (loom.md §5): the write path is off unless explicitly enabled,
and Clarion's own semantics never depend on a stored fact.

**Per-project isolation.** One `clarion serve` process serves exactly one project
(the `.clarion/` store under that project root). The `project` request field is a
**guard, not a selector** — it does not choose among projects; it only lets a
client assert which project it believes it is talking to. The handle is the
project-root directory name. An **empty** `project` is always accepted (no
assertion); a **non-empty** value that does not match the served project's
directory name returns `403` with `code: "PROJECT_MISMATCH"`. (Reference:
`AppState::reject_project_mismatch` in
[`http_read.rs`](../../crates/clarion-cli/src/http_read.rs).)

### Sub-router framing, auth, and limits

The `/api/wardline/*` routes sit behind the **same identity middleware** as the
protected `/api/v1/*` routes (HMAC `X-Loom-Component: clarion:<hmac>` preferred per
ADR-034, legacy `Authorization: Bearer` accepted as fallback, loopback-unauth
allowed; see [Authentication](#authentication)). The only difference is the body
limit used while reading the request to verify the HMAC signature: the wardline
guard reads up to **4 MiB** (`WARDLINE_BODY_LIMIT_BYTES`) rather than the
`/api/v1/*` 16 KiB, because batched resolves/writes carry thousands of qualnames.

| Property | `/api/v1/*` | `/api/wardline/*` |
|---|---|---|
| Body limit | 16 KiB | **4 MiB** |
| Per-request batch cap | 256 (`files/batch`) / 1000 (`files:resolve`) | **2000** facts/qualnames (`WARDLINE_TAINT_BATCH_MAX`) |
| Over-cap status | `400 BATCH_TOO_LARGE` | **`413 BATCH_TOO_LARGE`** |

Two distinct `413` sources on these routes — a client seeing `413` **must** check
for a JSON envelope to tell them apart:

- **Batch cap** — more than `2000` facts/qualnames in one request returns `413`
  with the JSON envelope `{"error": …, "code": "BATCH_TOO_LARGE"}`. Wardline
  chunks client-side against `2000`.
- **Raw body cap** — a request body over `4 MiB` is rejected at the transport layer
  with a `413` and **no JSON `code`** (same posture as the existing
  "413 | n/a" rows for `/api/v1/*`).

`GET /api/v1/_capabilities` does **not** advertise the taint store or whether the
write path is enabled (its response carries only `api_version`, `instance_id`,
`registry_backend`, `file_registry`). A Wardline client discovers the write API is
disabled by receiving `403 WRITE_DISABLED` from the write route, not by probing
capabilities.

### `POST /api/wardline/resolve`

Exact-tier resolution of **pre-composed** dotted qualnames to Clarion entity IDs.
No `&file=` disambiguator and no normalization: Wardline has already shaped each
qualname to byte-match Clarion's `canonical_qualified_name`, and Clarion does a
direct existence lookup of the candidate `python:function:<qualname>` (taint facts
are function/method-scoped; methods are `python:function:` in Clarion's ontology
per ADR-022).

Request body (`application/json`, max 4 MiB):

```json
{
  "project": "clarion",
  "qualnames": ["auth.tokens.refresh", "auth.sessions.SessionStore.load"]
}
```

`project` is optional (the guard above). Successful response (`200 OK`):

```json
{
  "resolved": {"auth.tokens.refresh": "python:function:auth.tokens.refresh"},
  "unresolved": ["auth.sessions.SessionStore.load"]
}
```

- `resolved` is a `{qualname: entity_id}` object, only for exact matches.
- `unresolved` lists every qualname with no matching `python:function:` entity.
- Resolution is **exact-only**: there is no heuristic tier and no error for an
  unresolved name — it simply lands in `unresolved`.

Failure modes:

| Status | Code | When |
|---|---|---|
| 400 | `INVALID_PATH` | Body is not a valid `{"qualnames": [...]}` object. |
| 401 | `UNAUTHENTICATED` | HMAC/bearer auth missing or wrong (when configured). |
| 403 | `PROJECT_MISMATCH` | Non-empty `project` does not match the served project. |
| 413 | `BATCH_TOO_LARGE` | `qualnames.len() > 2000`. |
| 413 | n/a | Request body exceeds the 4 MiB cap (transport-level). |
| 500/503 | `STORAGE_ERROR` / `INTERNAL` | Storage failure. |

### `POST /api/wardline/taint-facts` (write)

Persists a batch of taint facts. **Disabled by default** — only reachable when
`serve.http.wardline_taint_write: true` has spawned the optional writer-actor:

```yaml
serve:
  http:
    enabled: true
    wardline_taint_write: true   # default false; off ⇒ 403 WRITE_DISABLED
```

When disabled, the route returns `403` with `code: "WRITE_DISABLED"` **before**
parsing the body. Request body (`application/json`, max 4 MiB):

```json
{
  "project": "clarion",
  "scan_id": "wardline-scan-2026-05-30",
  "facts": [
    {
      "qualname": "auth.tokens.refresh",
      "wardline_json": {"taint": "tainted", "sources": ["request.body"]},
      "scan_id": "wardline-scan-2026-05-30",
      "content_hash_at_compute": "9c1185a5c5e9fc54612808977ee8f548b2258d31"
    }
  ]
}
```

- `wardline_json` is **opaque** to Clarion (see [Opacity contract](#opacity-contract)).
- `scan_id` and `content_hash_at_compute` are accepted as **top-level fields**
  (queryable columns); Clarion does **not** parse them out of the blob. The
  per-fact `scan_id` falls back to the batch-level `scan_id` when absent. Both are
  optional.

Successful response (`200 OK`):

```json
{
  "written": 1,
  "unresolved_qualnames": ["auth.sessions.SessionStore.load"]
}
```

- **Exact-only writes.** A fact whose qualname does not resolve exact-tier is
  reported in `unresolved_qualnames` and **never written** — there is no
  heuristic/none write path. `written` counts only persisted facts.
- **Per-entity replace (idempotent).** A write replaces the row keyed on the
  resolved `entity_id` (`ON CONFLICT(entity_id) DO UPDATE`), so re-posting the same
  qualname overwrites rather than duplicating.

Failure modes:

| Status | Code | When |
|---|---|---|
| 400 | `INVALID_PATH` | Body is not a valid `{"facts": [...]}` object. |
| 401 | `UNAUTHENTICATED` | HMAC/bearer auth missing or wrong (when configured). |
| 403 | `WRITE_DISABLED` | `serve.http.wardline_taint_write` is not `true`. |
| 403 | `PROJECT_MISMATCH` | Non-empty `project` does not match the served project. |
| 413 | `BATCH_TOO_LARGE` | `facts.len() > 2000`. |
| 413 | n/a | Request body exceeds the 4 MiB cap (transport-level). |
| 500/503 | `STORAGE_ERROR` / `INTERNAL` | Writer-actor unavailable or write failed. |

### `GET /api/wardline/taint-facts?project=&qualname=` (read, single) and `POST /api/wardline/taint-facts:batch-get` (read, batch)

Both read paths are served **regardless of whether the write API is enabled**.
They return the **same per-entity view shape**; the only difference is cardinality.

`GET` query parameters: `project` (optional guard), `qualname` (required, must not
be blank). The single GET returns **one** view object:

```json
{
  "qualname": "auth.tokens.refresh",
  "wardline_json": {"taint": "tainted", "sources": ["request.body"]},
  "current_content_hash": "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9",
  "exists": true
}
```

The batch read body is `{ "project"?, "qualnames": [..] }`; it returns a **bare
JSON array** of view objects, **one per input qualname, in input order** (not an
object wrapper):

```json
[
  {
    "qualname": "auth.tokens.refresh",
    "wardline_json": {"taint": "tainted"},
    "current_content_hash": "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9",
    "exists": true
  },
  {"qualname": "auth.sessions.SessionStore.load", "exists": false}
]
```

The view has **exactly four fields**:

- `qualname` — echoed back so the client correlates without re-ordering.
- `exists` — `true` when a stored fact exists for the resolved entity; `false` when
  the qualname does not resolve exact-tier **or** resolves but has no stored fact.
- `wardline_json` — the stored blob, returned **byte-verbatim** (see
  [Opacity contract](#opacity-contract)). **Field-absent** (not `null`) when
  `exists` is `false`.
- `current_content_hash` — the **live** freshness signal (see
  [Freshness contract](#freshness-contract)). **Field-absent** when `exists` is
  `false`, and also field-absent when `exists` is `true` but the containing file is
  deleted/unreadable at request time.

Note what is **not** echoed: the write-time `scan_id` and `content_hash_at_compute`
columns are **never returned by the read**. Wardline reads its own
`content_hash_at_compute` from *inside* the opaque `wardline_json` blob, not from a
Clarion-returned field (see the freshness contract).

Failure modes:

| Status | Code | When |
|---|---|---|
| 400 | `INVALID_PATH` | Blank/missing `qualname` (GET) or invalid `{"qualnames": [...]}` body (batch). |
| 401 | `UNAUTHENTICATED` | HMAC/bearer auth missing or wrong (when configured). |
| 403 | `PROJECT_MISMATCH` | Non-empty `project` does not match the served project. |
| 413 | `BATCH_TOO_LARGE` | `qualnames.len() > 2000` (batch). |
| 413 | n/a | Request body exceeds the 4 MiB cap (batch, transport-level). |
| 500/503 | `STORAGE_ERROR` / `INTERNAL` | Storage failure. |

### Freshness contract

`current_content_hash` is the load-bearing field of this whole surface, so its
definition is pinned exactly:

- It is the **blake3** hash of the entity's **containing file** — **whole file, raw
  bytes, lowercase hex**. It is **not** sha256, **not** LF-normalized, and **not**
  span-scoped to the entity's line range. (The stored `entities.content_hash` is
  deliberately *not* reused: for function entities that value is span-scoped and
  LF-normalized, and even a stored whole-file hash reflects the last analyze, not
  current disk.)
- It is computed by a **live filesystem read at request time**, not read from a
  stored value, so it reflects the file's current on-disk bytes.
- If the containing file is **deleted or unreadable** at request time,
  `current_content_hash` is **omitted** (field absent). `exists` still reflects the
  stored fact (it can be `true` with no `current_content_hash`).

  Reference: `clarion_storage::current_file_hash` in
  [`query.rs`](../../crates/clarion-storage/src/query.rs).

**Who decides freshness.** Wardline stamps `content_hash_at_compute` *inside* the
opaque `wardline_json` blob when it computes a fact, then on read compares that
stamp to Clarion's returned `current_content_hash`: equal ⇒ the fact is fresh;
mismatch, or `exists: false`, or `current_content_hash` absent ⇒ Wardline
recomputes. **Wardline owns the fresh/stale decision; Clarion never asserts a
freshness verdict** — it only reports the live hash and lets Wardline compare.

### Opacity contract

`wardline_json` is stored and returned **byte-verbatim**. Clarion holds it as a
serde_json `RawValue` on both the write and read paths, so object key order and
whitespace are preserved exactly — `{"b":2,"a":1}` is *not* re-emitted as
`{"a":1,"b":2}`. Clarion **never parses or validates** the blob's contents. The
only fields Clarion reads structurally are the top-level `scan_id` /
`content_hash_at_compute` accompanying a write (stored as queryable columns) — they
are taken from the request envelope, **not** parsed out of the blob.

### Verification scope

The contracts above are pinned by the W.1–W.4 tests; there is no new wire fixture
for these routes (the prose shapes here are the contract).

- The **qualname conformance oracle** is the existing fixture
  [`fixtures/wardline-qualname-normalization.json`](./fixtures/wardline-qualname-normalization.json)
  (see [Wardline qualname normalization](#wardline-qualname-normalization-entity-reconciliation));
  `resolve_wardline_qualnames` is exercised against its vectors by
  `resolves_fixture_vectors_exact` in
  [`wardline_taint.rs`](../../crates/clarion-storage/src/wardline_taint.rs).
- The **whole-file-vs-span freshness** definition is pinned by the
  `current_file_hash` tests in
  [`query.rs`](../../crates/clarion-storage/src/query.rs) (asserting whole-file
  blake3, not the span-scoped LF-normalized hash).
- The **route behaviour** — exact resolve + unresolved, project-guard mismatch,
  `WRITE_DISABLED`, per-entity replace, byte-verbatim storage, the live whole-file
  hash, deleted-file ⇒ absent hash, the bare-array batch read, and the over-cap
  `413` — is pinned by the `wardline_*` async handler tests in
  [`http_read.rs`](../../crates/clarion-cli/src/http_read.rs).

## Consumed Filigree convention: ephemeral-port endpoint discovery

The contracts above pin the surface Clarion *exposes*. This section and the ones
that follow pin what Clarion *consumes*. Endpoint discovery comes first because
it is the prerequisite for every consumed route below: before Clarion can call
`issues_for`'s issue-detail read, the scan-results intake, or the clean-stale
sweep, it must resolve *where* Filigree is actually listening.

`clarion serve` and `clarion analyze` resolve that base URL through
[`clarion_mcp::filigree_url::resolve_filigree_url`](../../crates/clarion-mcp/src/filigree_url.rs)
(added with clarion-084e82250c). It is strictly *enrich-only*: discovery only ever
*upgrades* the statically configured `integrations.filigree.base_url`; it never
gates Clarion's own semantics. Clarion stays solo-useful with Filigree absent
(loom.md §5).

**The convention.** Filigree's dashboard, when running in its default *ethereal*
mode, publishes its live listen port to `<project_root>/.filigree/ephemeral.port`
— a plain trimmed integer, written atomically, present only while the dashboard
is up. The port is chosen deterministically but unpredictably
(`8400 + sha256(project_path) % 1000`, with collision fallback), so it **must be
read, never computed**. This mirrors the Filigree sources
`filigree/src/filigree/ephemeral.py::{write,read}_port_file` and
`scanner_callback.py::resolve_scanner_api_url_with_source`.

**Resolution algorithm.** Given the configured `base_url` and the project root:

| Condition | Resolved URL | `source` label |
|---|---|---|
| Integration disabled | none (`null`) | `disabled` |
| Valid `<root>/.filigree/ephemeral.port` present | configured URL with its **port** overridden by the live port (scheme, host, path preserved) | `.filigree/ephemeral.port` |
| No / unreadable port file | configured URL unchanged | `config` |

**The negative contract (the load-bearing part).** What Clarion *refuses* to do is
the loom-§5 safety argument:

- It **reads** the published port; it never **computes** Filigree's port itself
  (no reimplementation of the `8400 + sha256 % 1000` rule).
- When no live port file is present, it falls back to Clarion's **own** configured
  `base_url`, **never** to a Filigree-internal default. Copying Filigree's
  `DEFAULT_PORT` would be a silent cross-product coupling that breaks the moment
  Filigree changes its default.
- Reading is **fail-soft**: a missing, corrupt, out-of-range, or zero-valued port
  file folds to the configured URL (`source = config`), never an error. A stale
  configured port that is simply unreachable is handled the same way every
  consumed route handles a Filigree outage — degrade, never propagate.

**Server-mode gap (named limitation).** Filigree also supports a *server* mode that
publishes its endpoint through a home-directory global
(`~/.config/filigree/server.json`) rather than the per-project `ephemeral.port`
file. Clarion does **not** read `server.json` at release:1.1 — under Filigree
server mode, discovery finds no `ephemeral.port` and degrades to the configured
`base_url` (`source = config`), which is correct but does not auto-track a
server-mode port. Closing this gap (reading the server-mode global) is tracked as
post-1.1 work; the ethereal path is the only one exercised today.

**Agent-facing surface.** `project_status` reports the resolution verbatim so an
agent can tell *where* the URL came from without probing ports:

```json
{
  "filigree": {
    "enabled": true,
    "configured_url": "http://127.0.0.1:8766",
    "resolved_url": "http://127.0.0.1:8542",
    "resolution_source": ".filigree/ephemeral.port"
  }
}
```

`resolution_source` is exactly one of the three `source` labels above
(`disabled` / `.filigree/ephemeral.port` / `config`); `resolved_url` is `null`
only when the integration is disabled.

**Verification scope.** There is no normative fixture for this convention —
connection discovery resolves a single scalar (a port), not a wire document, so a
fixture is not warranted; the shapes above are the contract. The executable check
is the test module in
[`filigree_url.rs`](../../crates/clarion-mcp/src/filigree_url.rs): it pins the
live-port override, the no-file and disabled fall-throughs, and the fail-soft
folding of corrupt / zero ports to the configured URL. Because the port file is a
*read* of a Filigree-owned convention, a change on Filigree's side (path or
format) would be caught by re-reading its `ephemeral.py`, not by Clarion CI.

## Consumed Filigree route: issue detail (enrichment)

This pins the single Filigree route Clarion *consumes* (against the endpoint
resolved above) to enrich an entity-association match — the read behind
`issues_for`'s per-match `issue` block (clarion-51a2868c86). It is
strictly *enrich-only*: if the route is absent or unreachable, the match still
resolves with `issue: null`, and Clarion's semantics are unaffected (loom.md §5).

```text
GET {filigree_base}/api/loom/issues/{issue_id}
```

- `{issue_id}` is percent-encoded as a path-segment value.
- **Request headers:** `accept: application/json`; `x-filigree-actor: <actor>`
  when an actor is configured; `Authorization: Bearer <token>` when a bearer
  token is configured. (HMAC is not used on this *outbound* read; it is an
  inbound-auth mechanism on Clarion's own exposed routes.)
- **`200` response body** — only these fields are read; **unknown fields are
  ignored** so Filigree may grow the route without breaking the consumer:

  ```json
  { "title": "string", "status": "string", "priority": 0 }
  ```

  `priority` is an integer (Clarion's `IssueDetail.priority: i64`).
- **`404`** — the issue, or the whole route, is absent. Treated as the enrich-only
  degrade signal (`Ok(None)` → `issue: null`), **not** an error.
- **Any other non-`2xx`** — surfaced as a client error; the enrichment for that
  match degrades to `issue: null` rather than failing the `issues_for` call.

There is no normative fixture for this route yet; the shape above is the
contract. The `parse_issue_detail_response` shape test in
[`filigree.rs`](../../crates/clarion-mcp/src/filigree.rs) is the executable
check.

## Consumed Filigree route: scan-results intake (finding emission)

This pins the Filigree route Clarion *consumes* to emit findings — WP9-B,
REQ-FINDING-03, ADR-004. `clarion analyze` Phase 8 POSTs this run's persisted
findings on completion via
[`FiligreeHttpClient::post_scan_results`](../../crates/clarion-mcp/src/filigree.rs).
It is *enrich-only*: emission is gated behind
`integrations.filigree.{enabled,emit_findings}` (both default `false`), and any
failure — Filigree down, transport error, build error — is recorded in
`stats.json` and logged as `CLA-INFRA-FILIGREE-UNREACHABLE`, never propagated.
The analyze run never fails because a sibling is unreachable (loom.md §5).

```text
POST {filigree_base}/api/v1/scan-results
```

- **Request headers:** `content-type: application/json`;
  `x-filigree-actor: <actor>` when configured; `Authorization: Bearer <token>`
  when a bearer token is configured. (HMAC is inbound-only, on Clarion's own
  exposed routes; this outbound POST uses bearer.)
- **Request body** — only the keys below are sent. Filigree silently drops any
  top-level finding key outside its enumerated set, so Clarion's richer fields
  nest under `metadata` and the Clarion-owned `metadata.clarion.*` slot (ADR-004,
  detailed-design §7) where verbatim preservation is verified:

  ```json
  {
    "scan_source": "clarion",
    "scan_run_id": "<run_id>",
    "mark_unseen": true,
    "create_observations": false,
    "complete_scan_run": true,
    "findings": [
      {
        "path": "src/auth/tokens.py",
        "rule_id": "CLA-PY-STRUCTURE-001",
        "message": "Circular import detected",
        "severity": "medium",
        "line_start": 12,
        "line_end": 12,
        "metadata": {
          "kind": "defect",
          "confidence": 0.95,
          "confidence_basis": "ast_match",
          "clarion": {
            "entity_id": "python:class:auth.tokens::TokenManager",
            "related_entities": ["python:class:auth.sessions::SessionStore"],
            "supports": [],
            "supported_by": [],
            "internal_severity": "WARN",
            "internal_status": "open"
          }
        }
      }
    ]
  }
  ```

  - `scan_source` is always `"clarion"`; it is part of Filigree's dedup key, so
    it is stable across runs.
  - `scan_run_id` carries Clarion's `run_id`. It is omitted entirely when unset;
    an unknown id is tolerated by Filigree (it warns and proceeds), which is how
    REQ-FINDING-05's wire shape ships without a pre-create handshake. **Clarion's
    posture** is to depend on this tolerate-unknown behavior and emit no Phase-0
    `scan-runs` create call; whether that is Filigree's *intended permanent*
    contract (vs. an explicit create endpoint) is the open §4 question in
    [`2026-05-30-prune-unseen-filigree-request.md`](2026-05-30-prune-unseen-filigree-request.md),
    pending Filigree's confirmation.
  - `mark_unseen` is `true` for a normal full run (old-position findings for the
    same rule/file transition to `unseen_in_latest`); a `--resume RUN_ID` run
    sets it `false` so the re-emit does not flip the prior run's findings to
    `unseen_in_latest`. `complete_scan_run` is `true` on the final (here: only)
    batch. **`--resume` is implemented** (REQ-FINDING-05): it reopens the prior
    run's `runs` row instead of inserting a fresh one and re-walks idempotently
    (entities and run-scoped findings UPSERT). It re-walks the tree from scratch
    (not incremental recovery) and assumes an unchanged corpus. Because a resume
    emits `mark_unseen=false`, it never creates `unseen_in_latest` state, so the
    `--prune-unseen` sweep (below) does not interact with resumes — prune is
    meaningful only after normal `mark_unseen=true` runs. The emitted
    `mark_unseen` value is recorded in the run's `stats.json` `filigree_emission`
    block.
  - `create_observations` is always `false` — Clarion emits findings, not
    observations.
  - `severity` is the **wire** vocabulary, mapped from Clarion's internal value:
    `CRITICAL→critical`, `ERROR→high`, `WARN→medium`, everything else
    (`INFO`, `NONE`, unknown) `→info`. This mirrors Filigree's own server-side
    coercion but is done client-side so the original survives under
    `metadata.clarion.internal_severity`.
  - `line_start` / `line_end` are omitted when the anchor entity has no line
    range. A finding whose anchor entity has **no `path`** is skipped (and
    counted in `stats.json`); Filigree rejects path-less findings with
    `400 VALIDATION`.
  - **briefing-blocked exclusion:** findings anchored to a `briefing_blocked`
    entity are **never emitted** (clarion-8b32ba0d02). This matches the
    fail-closed read posture — `GET /api/v1/files` refuses the same entities —
    so the write direction cannot leak a path/line the read direction withholds.

- **`200` response body** — parsed with unknown fields ignored and missing
  fields defaulted, so Filigree may grow the response without breaking the
  consumer. REQ-FINDING-03 requires the emitter to **parse** `warnings`, not
  just count them; each is logged against the run:

  ```json
  {
    "files_created": 1,
    "files_updated": 0,
    "findings_created": 1,
    "findings_updated": 0,
    "observations_created": 0,
    "observations_failed": 0,
    "new_finding_ids": ["clarion-sf-2f4cf9ca1b"],
    "warnings": ["Unknown severity 'WARN' for finding at probe/sev.py, mapped to 'info'"]
  }
  ```

- **Any non-`2xx`** — surfaced as a transport/HTTP error, folded into the
  `filigree_emission` stats blob and the `CLA-INFRA-FILIGREE-UNREACHABLE` log;
  the analyze run still completes successfully.

There is no normative fixture for this route yet; the shapes above are the
contract. The `request_serializes_to_filigree_wire_shape` and
`parses_live_response_shape` tests in
[`scan_results.rs`](../../crates/clarion-mcp/src/scan_results.rs) — the latter
pinned to a real captured Filigree response — are the executable checks.

**Verification scope.** CI exercises the emitter against a *mock* HTTP server
(`post_scan_results_sends_batch_and_parses_response` in
[`filigree.rs`](../../crates/clarion-mcp/src/filigree.rs)), and the
`analyze`-level test asserts the enrich-only degrade when Filigree is
unreachable. The wire shapes pinned above were captured from a **one-time live
probe** against a running Filigree intake (the source of the `severity`
coercion rule and the response fields); there is **no recurring end-to-end test
against a live Filigree**. A shape change on Filigree's side would be caught by
re-probing, not by CI — re-pin `parses_live_response_shape` if the live intake
changes.

## Consumed Filigree route: clean-stale retention (`--prune-unseen`)

`clarion analyze --prune-unseen` asks Filigree to run a retention sweep over its
own findings (REQ-FINDING-06). This is a **loom-generation** route, distinct
from the classic `/api/v1/scan-results` emission intake.

```
POST {filigree_base}/api/loom/findings/clean-stale
```

- **Headers** — `accept: application/json`, optional `x-filigree-actor` and
  `Authorization: Bearer <token>` (same posture as scan-results; Filigree's
  trust boundary for this route is loopback binding, not inbound auth).
- **Request body**

  ```json
  {
    "scan_source": "clarion",
    "older_than_days": 30,
    "actor": "clarion-mcp"
  }
  ```

  - `scan_source` is **required** server-side as an accident-guard (Filigree's
    core treats absent as "all sources", which the route refuses to expose).
    Clarion always sends `"clarion"`, so the sweep can only touch Clarion's
    findings — it can never affect Wardline's or any other tool's.
  - `older_than_days` comes from `integrations.filigree.prune_unseen_days`
    (default 30); a non-negative integer. `0` sweeps the whole current unseen
    backlog.
  - `actor` is Clarion's configured actor, for Filigree's audit trail.

- **Semantics — soft-archive, not delete.** Filigree moves its
  `unseen_in_latest` findings older than the threshold to `fixed` status
  (audit-preserving); a finding that reappears in a later scan auto-reopens
  (`fixed` → `open`) with its `seen_count` intact. Filigree owns the finding
  lifecycle and chose this audit-preserving policy; REQ-FINDING-06's "removes"
  is realised as soft-archive. See Filigree ADR-015.

- **`200` response body** — parsed with unknown fields ignored / missing fields
  defaulted:

  ```json
  { "findings_fixed": 4, "scan_source": "clarion", "older_than_days": 30 }
  ```

- **Enrich-only.** The sweep runs after emission (Phase 8b) for the same
  non-hard-failed outcomes. A Filigree outage, a non-`2xx`, or the integration
  being disabled is recorded in the `filigree_prune` stats blob (status
  `unreachable` / `skipped`) and the `CLA-INFRA-FILIGREE-UNREACHABLE` log — the
  analyze run still completes successfully. Prune keys on `unseen_in_latest`,
  which only `mark_unseen=true` (normal) runs create; a `--resume`
  (`mark_unseen=false`) run produces no unseen state for prune to sweep.

**Verification scope.** Same posture as the emission intake: the wire shape is
checked by `clean_stale_*` unit tests in
[`scan_results.rs`](../../crates/clarion-mcp/src/scan_results.rs) and exercised
end-to-end against a *mock* Filigree (`analyze_prune_unseen_*` in
[`analyze.rs`](../../crates/clarion-cli/tests/analyze.rs), covering the
post-after-emission path, the unreachable degrade, and the disabled no-op). The
route shape was read from Filigree's own handler + API tests; there is **no
recurring end-to-end test against a live Filigree**.
