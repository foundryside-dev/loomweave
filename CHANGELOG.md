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

(none yet — see [`docs/implementation/`](docs/implementation/) for in-flight
sprint planning.)

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
