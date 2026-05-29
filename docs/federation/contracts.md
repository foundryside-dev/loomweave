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
    # Preferred 1.0 identity mode. Optional on loopback, required for
    # authenticated Loom component requests.
    identity_token_env: CLARION_LOOM_IDENTITY_SECRET
    # Name of the env var holding the inbound bearer token. Optional on a
    # loopback bind, accepted for compatibility on non-loopback binds. Default
    # `CLARION_LOOM_TOKEN` matches Filigree's pinned client default.
    token_env: CLARION_LOOM_TOKEN
```

The MCP stdio server remains available on stdin/stdout. The HTTP surface is
read-only and uses Clarion's existing SQLite reader pool.

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

The initial `code` enum is closed to `INVALID_PATH`,
`PATH_OUTSIDE_PROJECT`, `NOT_FOUND`, `BRIEFING_BLOCKED`, `UNAUTHENTICATED`,
`STORAGE_ERROR`, `BATCH_TOO_LARGE`, and `INTERNAL`. Clients must switch
on `code`; `error` is human-readable diagnostic text. `BATCH_TOO_LARGE`
is only emitted by `POST /api/v1/files/batch` (see the batch endpoint
section below).

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
`exact | heuristic | none` for a candidate qualname — is named in ADR-018 as the
eventual conformance surface but is **not implemented at release:1.1**. Until it
ships, the fixture above is the contract: validate against it offline. Building
the endpoint ahead of a shipped Wardline consumer would be speculative
forward-work (loom.md §5 — Clarion translates qualnames because it owns the
catalog, but only when a consumer needs it).

## Consumed Filigree route: issue detail (enrichment)

The contracts above pin the surface Clarion *exposes*. This one pins the single
Filigree route Clarion *consumes* to enrich an entity-association match — the
read behind `issues_for`'s per-match `issue` block (clarion-51a2868c86). It is
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
