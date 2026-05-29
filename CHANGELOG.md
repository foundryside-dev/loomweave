# Changelog

All notable changes to Clarion are documented here. The format is loosely based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Clarion
follows [Semantic Versioning](https://semver.org/) for the `clarion` binary,
the workspace crates, and the Python plugin.

API versioning for the federation HTTP read API (`/api/v1/...`) is independent
of product semver: `api_version: 1` is the wire-contract version, and bumps
only when an incompatible change is made to that surface. See
[`docs/federation/contracts.md`](docs/federation/contracts.md).

## [Unreleased]

### Added

- `clarion db backup <output>` — a consistent, WAL-safe online backup of
  `.clarion/clarion.db` via `rusqlite::backup::Backup`. Safe to run during a
  live `clarion analyze` (captures outstanding WAL frames into a standalone
  single-file copy, unlike `cp`), writes atomically (temp sibling + rename),
  refuses to clobber without `--force`, and verifies the copy with
  `PRAGMA integrity_check` before promoting it. Closes gap-register STO-04
  (clarion-6d433b61ba).
- `source_for_entity(id, context_lines=10)` MCP tool — returns an entity's
  exact indexed source span (decorators/signature/docstring included as
  captured) plus a bounded window of line-numbered context, each line flagged
  `in_entity`. No LLM call. Reports an explicit `source_status`
  (`missing`/`no_range`/`binary`/`drifted`) rather than a misleading stale
  snippet when the on-disk source no longer matches the indexed `content_hash`.
  The MCP surface now exposes twelve tools (clarion-6077738f1c).
- `call_sites(id, role=caller|callee, kind?, confidence?, path?)` MCP tool —
  shows the actual source sites behind `calls`/`references` edges (file, line,
  byte column, line text, edge kind, confidence) so an agent can see *why*
  Clarion believes an edge exists. Statically-unbindable calls are returned in
  a separate `unresolved_sites` list, never mixed with resolved evidence.
  Filterable by edge kind and a documented best-effort production/test path
  heuristic. No LLM call. The MCP surface now exposes thirteen tools
  (clarion-9392f74881).
- **Filigree finding emission (WP9-B core, REQ-FINDING-03).** `clarion analyze`
  Phase 8 POSTs the run's persisted findings to Filigree's
  `POST /api/v1/scan-results` intake, with Clarion's richer fields nested under
  `metadata.clarion.*` (wire contract pinned in
  [`docs/federation/contracts.md`](docs/federation/contracts.md)). Emission is
  enrich-only — gated behind `integrations.filigree.{enabled,emit_findings}`
  (now **both default `false`**, so enabling Filigree for `issues_for` reads
  never silently starts outbound emission — clarion-a26de2f368), and any
  Filigree-side failure is recorded in `stats.json`
  (`CLA-INFRA-FILIGREE-UNREACHABLE`) instead of failing the run. Findings
  anchored to a `briefing_blocked` entity are excluded, matching the fail-closed
  read posture (clarion-8b32ba0d02). Resolves the 1.0 "finding emission
  deferred" limitation below. The REQ-FINDING-05/-06 lifecycle tail (Phase-0
  create handshake + `--resume`; `--prune-unseen`, which needs a Filigree-side
  prune surface) remains deferred — tracked under clarion-dd29e69e0e.

### Changed

- `entities` gains a `briefing_blocked` VIRTUAL generated column
  (`json_extract(properties, '$.briefing_blocked')`) plus a partial index
  (`ix_entities_briefing_blocked WHERE briefing_blocked IS NOT NULL`), so the
  federation read-API hot path can filter entities withheld from briefings in
  SQL instead of parsing every row's properties JSON. `project_status` now
  reports a `counts.briefing_blocked` diagnostic (how many entities are
  withheld), served by the new index. Edit-in-place in migration `0001` per
  ADR-024 (no published build). Closes gap-register STO-04 / V11-STO-04
  (clarion-bdabfd6bca).
- The writer actor now opens its batch transactions with `BEGIN IMMEDIATE`
  (via a new `clarion_storage::retry::begin_immediate` helper with bounded
  `SQLITE_BUSY`/`SQLITE_LOCKED` retry + exponential backoff) instead of a
  deferred `BEGIN`. Taking the write lock up front resolves cross-process
  write contention at lock-acquire — where `busy_timeout` is honored — rather
  than failing mid-statement on a deferred-lock upgrade the busy handler
  cannot serve. Closes gap-register STO-05 (clarion-bbb3365920).

## [1.0.0] — 2026-05-19

First publishable release. Clarion ships as a Rust core (`clarion` binary, five
workspace crates) plus an editable-install Python language plugin
(`clarion-plugin-python`). Released under the [MIT License](LICENSE).

Targets the `v1.0.0` tag (cut by the operator once all release blockers are
green); supersedes the pre-release `v0.1-sprint-1` and `v0.1-sprint-2`
working tags, which remain in the repo as historical anchors.

### Core

- `clarion install --path` initialises a project's `.clarion/` directory
  (instance ID, SQLite DB, migrations).
- `clarion analyze` walks a Python corpus and persists the structural graph
  (entities + `contains`, `calls`, `references`, `imports` edges) to a local
  SQLite store via the writer-actor pattern (ADR-011).
- `clarion serve` exposes the MCP stdio surface for consult-mode agents:
  `entity_at`, `find_entity`, `callers_of`, `execution_paths_from`,
  `summary`, `issues_for`, `neighborhood`, `subsystem_members`.

### Python plugin (`clarion-plugin-python` 1.0.0)

- Pyright-backed entity extraction for functions, classes, and modules; resolved
  / ambiguous / inferred call edges per ADR-022.
- `wardline` runtime probe with version-range pinning (`>=1.0.0,<2.0.0`).
- Module-level `imports` candidate edges (Phase 3 Task 3).
- Strict-typed L4 JSON-RPC handshake with declared `entity_kinds`,
  `edge_kinds`, `ontology_version`, and `rule_id_prefix` (ADR-022).

### Federation HTTP read API (ADR-014)

The publisher-side of Clarion's federation contract with Filigree's
`ClarionRegistry`. Pinned in [`docs/federation/contracts.md`](docs/federation/contracts.md);
fixtures under [`docs/federation/fixtures/`](docs/federation/fixtures/) are
normative.

- `GET /api/v1/_capabilities` — registry-backend probe, returns `instance_id`,
  `api_version: 1`, and feature flags. Always unauthenticated so siblings can
  discover the surface pre-auth.
- `GET /api/v1/files?path=&language=` — single-file identity resolution.
  Closed error envelope `{error, code}` with codes `INVALID_PATH`,
  `PATH_OUTSIDE_PROJECT`, `NOT_FOUND`, `BRIEFING_BLOCKED`, `UNAUTHENTICATED`,
  `BATCH_TOO_LARGE`, `STORAGE_ERROR`, `INTERNAL`. ETag / `If-None-Match` supported.
- `POST /api/v1/files/batch` — bulk resolution, up to 256 queries per
  request, single pooled `ReaderPool` checkout per batch. Four-way
  partitioning: `resolved` / `not_found` / `briefing_blocked` / `errors`.
- **Bearer authentication.** `serve.http.token_env` (default
  `CLARION_LOOM_TOKEN`) names the env var holding the inbound bearer
  token. Loopback-without-token stays unauthenticated for the v0.1 trust
  model; non-loopback-without-token is refused at startup with
  `CLA-CONFIG-HTTP-NO-AUTH`.
- **Briefing-blocked propagation.** Files flagged by the pre-ingest secret
  scanner return `403 BRIEFING_BLOCKED` on the single-file endpoint and
  surface in the `briefing_blocked[]` partition on the batch endpoint.
  Identity fields (`entity_id`, `content_hash`, `canonical_path`,
  `language`) are deliberately omitted from blocked responses so siblings
  cannot infer file identity from a refusal.

### Secret scanner (WP5, ADR-013)

- Pre-ingest scan annotates entities with `briefing_blocked` when a file
  contains high-entropy or pattern-matched secrets; ingest proceeds, but
  every federation read path refuses to surface the entity.

### Subsystem clustering (WP4 Phase 3)

- Storage-side queries for subsystem membership and inverse lookup.
- Analyze-time clustering writes subsystem entities and `in_subsystem` edges.
- `subsystem_members(id)` MCP tool surfaces cluster members to consult agents.

### LLM providers

- OpenRouter (default, requires `OPENROUTER_API_KEY`).
- Codex CLI and Claude CLI as local-login providers — no API key surface,
  the user's OS-level auth carries.
- `recording` provider for deterministic test fixtures.

### Operational

- ADR-023 tooling baseline: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo nextest`, `cargo doc -D warnings`, `cargo deny`, `ruff`,
  `ruff format --check`, `mypy --strict`, `pytest`, end-to-end walking-skeleton
  script — all enforced in CI.
- Pre-commit hooks wire the Python gates.
- `.env` loaded from CWD or ancestor before tracing setup.

### Known v1.0 limitations

- **Python only.** Other-language plugins (`NG-15`) are v2.0+ scope.
- **Filigree finding emission deferred to a future release (tracked under
  `release:v1.1`).** Cross-product POSTing of Clarion-generated findings into
  Filigree's intake (WP9-B) is deferred per the [Sprint 2 scope amendment](docs/implementation/sprint-2/scope-amendment-2026-05.md).
  `issues_for(id)` (the WP9-A binding for reading from Filigree) ships in 1.0.
  *(WP9-B core shipped post-1.0 — see the Filigree-finding-emission entry under
  [Unreleased]; only the REQ-FINDING-05/-06 lifecycle tail remains deferred.)*
- **HTTP file language inference** uses persisted plugin manifest language when
  available, with a narrow core-extension fallback for files that predate
  manifest capture.
- **Cooperative HMAC inbound auth** ships for the HTTP read API via
  `serve.http.identity_token_env` and `X-Loom-Component: clarion:<hmac>`.
  The older bearer-token path remains available for compatibility.
- **Python plugin imports `wardline.core.registry.REGISTRY` at startup**
  (loom.md §5 asterisk 2). Initialization coupling scoped to the
  Wardline-aware plugin only; Clarion core and non-Wardline-aware plugins are
  unaffected. *Retirement condition*: Wardline ships a stable runtime probe
  API.
- **Wardline state-file ingest deferred to a future release (tracked under
  `release:v1.1`).** Only the `wardline.core.registry.REGISTRY` version-pin
  probe ships in v1.0; the state-file readers for `wardline.yaml` + overlays,
  `wardline.fingerprint.json`, `wardline.exceptions.json`, the
  `wardline.sarif.baseline.json` translator baseline, and the three-scheme
  identity-reconciliation oracle (REQ-INTEG-WARDLINE-02 through -06) all land
  with WP9-B / WP10.
- **Pre-WP5 catalogue upgrade requirement.** Briefing-blocked annotations
  are stored as a JSON property on file entities at v1.0 (v1.1 promotes
  the field to a typed column). A v1.0 binary opening a `.clarion/clarion.db`
  produced by a pre-WP5 binary finds no `briefing_blocked` properties —
  pre-WP5 analyzers never wrote them — and will serve the entire catalogue
  without refusal. Operators upgrading from a pre-WP5 install MUST run
  `clarion analyze` (scanner active by default) against the project root
  before exposing the HTTP read API or calling the `summary` MCP tool. See
  [`docs/operator/secret-scanning.md`](docs/operator/secret-scanning.md#pre-wp5-catalogue-upgrade-requirement).

### Documentation

- Design ladder under [`docs/clarion/1.0/`](docs/clarion/1.0/) — `requirements.md`,
  `system-design.md`, `detailed-design.md`.
- ADRs under [`docs/clarion/adr/`](docs/clarion/adr/) — 28 Accepted at 1.0
  (ADR-001…ADR-034 with the documented Backlog/Superseded subset excluded).
  ADR-012 is superseded by ADR-014, whose Security
  Posture and Error Envelope are in turn partially extended by ADR-034
  for the Sprint 3 federation hardening. Four ADRs (ADR-009, ADR-010,
  ADR-019, ADR-020) remain Backlog and are tracked inside
  `system-design.md` §12 / `detailed-design.md` §11 until promoted.
- Loom-suite doctrine at [`docs/suite/loom.md`](docs/suite/loom.md).
- Federation contract surface at [`docs/federation/contracts.md`](docs/federation/contracts.md).
- Operator guides under [`docs/operator/`](docs/operator/) — getting-started,
  OpenRouter setup, HTTP read API.

[Unreleased]: https://github.com/tachyon-beep/clarion/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/tachyon-beep/clarion/releases/tag/v1.0.0
