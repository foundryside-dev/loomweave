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

- **Stable Entity Identity (SEI) — Wave 1 / WS1 (ADR-038).** Clarion is now the
  suite's identity authority: it mints a durable, opaque **SEI**
  (`clarion:eid:<blake3(locator ++ 0x00 ++ mint_run_id)[:32]>`) for every entity
  and demotes the `{plugin}:{kind}:{qualname}` id to a mutable **locator**, so
  cross-tool bindings survive rename and move.
  - Migration `0005` adds `sei_bindings` (durable identity store, keyed by SEI,
    decoupled from the cumulative `entities` table) + `sei_lineage` (append-only
    event log) + a plain `entities.signature TEXT`. Schema version 4 → 5.
  - A deterministic, **fail-closed** re-binding matcher (`sei.rs`) carries an SEI
    on an unchanged locator, a git-detected rename with an identical body
    (`locator_changed`), or an identical body+signature at a new locator
    (`moved`); it mints a new SEI and orphans the old binding whenever sameness
    cannot be proven. A back-to-back unchanged re-run carries (never re-mints)
    every SEI. The git-rename signal is consumed behind a typed `GitRenameSource`
    seam (REQ-C-05); v1 ships `ShellGitRenameSource`.
  - The analyze pipeline runs an SEI mint pass after each successful run;
    `--no-sei` skips it. The Python plugin emits a versioned `signature` object
    per function/class (`plugin.toml [signature]`).
  - HTTP read API: `POST /api/v1/identity/resolve` (+ `:batch`),
    `GET /api/v1/identity/sei/{sei}`, `GET /api/v1/identity/lineage/{sei}`.
    `resolve` fail-closed-rejects an SEI-shaped input by the reserved
    `clarion:eid:` prefix (REQ-F-02). `_capabilities` advertises
    `sei: { supported: true, version: 1 }`.
  - The MCP tool surface carries the `sei` alongside every entity id (no MCP
    locator exception — REQ-C-04), via a read-time `sei_bindings` join.
  - The shared **SEI conformance oracle** (SEI standard §8) is authored and
    passes; the cross-tool hard-cutover backfill is documented in
    [`docs/federation/sei-migration-playbook.md`](docs/federation/sei-migration-playbook.md)
    and surfaced for owner-gated scheduling.
- **Incremental analysis — Wave 2 / T3.1.** `clarion analyze` now skips files
  whose whole-file hash matches the prior run, reusing their entities (the entity
  graph is cumulative and edges are insert-or-ignore, so the skip is speed-only).
  `skipped_files` is reported in `stats.json` and a `skipped_unchanged` progress
  event is emitted; `--no-incremental` forces a full re-index. The skip is guarded
  against the SEI matcher: the current-locator set is the union of re-analysed and
  skipped-file entities, so an unchanged file's identities are never falsely
  orphaned, and skipped entries are re-fed into the prior-index rebuild so the
  snapshot does not decay. Files carrying a secret finding are never skipped (their
  finding anchor must stay stable).
- **Dossier participation surface — Wave 2 / WS4.** The exact Clarion HTTP slices
  the cross-tool dossier *assembler* (Wardline) reads are pinned in
  [`docs/federation/contracts.md`](docs/federation/contracts.md) and specified in
  [`docs/superpowers/specs/2026-06-02-clarion-dossier-participation.md`](docs/superpowers/specs/2026-06-02-clarion-dossier-participation.md):
  identity (`resolve` → SEI + content-axis freshness; `resolve_sei`/`lineage` →
  identity-axis freshness), structural linkages (callers/callees), and file
  context. Clarion contributes slices and the SEI join key; it does **not** proxy
  Filigree associations (read directly from Filigree's own ADR-029 endpoint) or
  assemble the envelope. Proven end-to-end against a renamed-function fixture
  (`serve_http_dossier_participation_surface_serves_a_renamed_function`).
- **`legis` governance consumption — Wave 3 / WS9 (governed paradise).** `legis`
  consumes Clarion's stable identity as an **opt-in** governance layer a solo
  project never sees; core paradise (Wave 2) does not depend on it.
  - **Git-rename provider seam (REQ-C-05).** A second `GitRenameSource`,
    `LegisGitRenameSource`, reads `legis`'s `GET /git/renames` over HTTP and feeds
    the same file→locator translation as `ShellGitRenameSource` — `legis` supplies
    the git signal with no matcher change (SEI spec §6). Selection
    (`select_git_rename_source`, `--legis-url`) is enrich-only and
    capability-aware: the shell source remains the default and fallback; an
    unset/unreachable `legis` issues no HTTP and is byte-identical to before. The
    two suppliers observe different rename windows (Clarion's `analyze` depends on
    the working-tree window; `legis` serves only committed rev-ranges), so the
    seam is built/tested/ready but inert in the default pipeline until `legis`
    adds a working-tree surface or Clarion drives a committed re-index — a gap
    surfaced (not papered) in [`docs/federation/contracts.md`](docs/federation/contracts.md).
    The matcher is fail-closed regardless, so neither window can cause a false
    carry. Proven by
    `selector_keeps_working_tree_rename_even_when_a_reachable_legis_sees_nothing`.
  - **Audit-spine consumption.** `legis` reads Clarion's existing
    `resolve`/`resolve_sei`/`lineage` routes as its governance audit spine; the
    consumption contract is pinned in `docs/federation/contracts.md`. Per REQ-L-01
    (Option 3) `legis` owns integrity at its own boundary (snapshot-hash over
    polled lineage) — Clarion ships **no** lineage hash-chain or signature.
  - **No trust adjudication.** Clarion carries the trust vocabulary verbatim and
    adds no policy/attestation engine — Wardline analyses, `legis` governs,
    attestations key on Clarion's SEI.

## [1.1.0] — 2026-05-31

### Added

- **Wardline taint-fact store (ADR-036).** Clarion now serves as the persistent
  read+write store for Wardline's taint facts over HTTP, keyed to Clarion entity
  qualnames. New migration `0003` adds the `wardline_taint_facts` table; facts
  are written through the storage writer-actor
  (`WriterCmd::UpsertWardlineTaintFact`) and resolved/fetched via the new
  `wardline_taint` storage module. Routes:
  - `POST /api/wardline/resolve` — exact-tier qualname → entity resolution.
  - `POST /api/wardline/taint-facts` — exact-only batch write; the Wardline
    payload is stored byte-verbatim (`serde_json::value::RawValue`) so Clarion
    never reshapes Wardline's JSON.
  - `GET /api/wardline/taint-facts` and `…:batch-get` — reads carry a live
    freshness hash so a consumer can detect drift against the entity's current
    content.

  The write path is opt-in via `serve.http.wardline_taint_write` config plus an
  optional writer-actor, and disabled by default.
- **Flow B — read-time Wardline finding reconciliation (enrich-only).**
  `issues_for` and `orientation_pack` now attach a `wardline_findings` section
  reconciled against Wardline findings stored in Filigree. A pure
  qualname-reconciliation module matches `metadata.wardline.qualname` byte-exact
  against entity-ID segment-3; a two-hop Filigree read
  (`GET /api/loom/files` → `GET /api/loom/findings`) resolves a path to its
  findings. Per the enrich-only axiom, any unreachable hop degrades the section
  to `result_kind: "unavailable"` rather than failing the tool.
- **`clarion doctor [--fix]`.** A new subcommand that verifies — and with
  `--fix` repairs in place — the installed agent-orientation surfaces: the
  `clarion-workflow` skill pack, the `SessionStart` hook in
  `.claude/settings.json`, and the `clarion` entry in `.mcp.json` (which
  `clarion install` does not register automatically). It prints a per-surface
  ✓/✗ report plus the index snapshot and exits non-zero when any problem
  remains, so it is usable as a CI / pre-commit gate. Repairs reuse the same
  idempotent installers as `clarion install`, and the `.mcp.json` merge is
  never-clobber: sibling servers and a deliberately customised `command` are
  preserved (only the `--path` args are corrected).

### Changed

- Federation contracts pinned/clarified: the Wardline taint-store routes +
  freshness contract, the two consumed loom routes for Flow B, Clarion→Filigree
  ephemeral-port endpoint discovery, and the `scan_run_id` contract (stale
  Phase-0 handshake references removed).
- `docs/suite/loom.md`: added a written retirement condition to the §5
  asterisk-2 (Wardline→Filigree pipeline coupling), and refreshed the §9 status
  table to the post-1.0 state.
- ADR-036 accepted (Clarion as Wardline taint-fact store), documenting the
  in-process two-writer concurrency posture.
- The Python round-trip self-test now hard-fails (instead of silently skipping)
  when the installed `clarion-plugin-python` entry point is missing, so a broken
  editable install cannot pass CI green.
- **Shared error vocabulary (ADR-037).** New `clarion-core::errors` module is now
  the single typed source of truth for both wire error-code vocabularies:
  `HttpErrorCode` (federation HTTP read API, `SCREAMING_SNAKE_CASE`, moved out of
  `http_read.rs`) and a new `McpErrorCode` (MCP tool envelope, kebab-case) that
  replaces ~47 bare string literals with compiler-checked variants. The two
  surfaces keep their established wire spellings — co-located, not merged, since
  they have disjoint vocabularies and independent consumers — and drift tests pin
  every wire string at the definition site. Wire output is byte-identical on both
  surfaces; no consumer change. Closes the MCP/HTTP error-code drift smell
  (V11-ARCH-01).

### Fixed

- `wardline_findings_for_path` no longer silently undercounts: a truncated
  findings page (`has_more`) now fails closed to `unavailable` rather than
  returning a partial result, mirroring the existing hop-1 truncation handling.
- Wardline metadata block is now surfaced in finding output, with truncation-safe
  `no_matches` handling and a corrected tool description.
- De-duplicated the Filigree HTTP client GET paths: `associations_for` and
  `issue_detail` now route through a shared `get_json` / `get_json_or_none`,
  preserving the `404` → enrich-only-degrade semantics.

## [1.0.1] — 2026-05-30

1.0.0 was tagged but its release failed to publish (the macOS build broke at
tag-cut), so no 1.0.0 artifacts exist. 1.0.1 is the first published build and
carries the full post-1.0.0 scope below plus the build fix.

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
  deferred" limitation below.
- **`clarion analyze --resume RUN_ID` (WP9-B, REQ-FINDING-05).** Reopens a prior
  run's `runs` row (a new `WriterCmd::ResumeRun` `UPDATE`s it back to `running`
  instead of `INSERT`ing, which conflicted on the existing run PK), re-walks the
  tree, and emits findings to Filigree with `mark_unseen=false` so the re-emit
  does not flip the prior run's findings to `unseen_in_latest`. The re-walk is
  idempotent: entities and run-scoped findings now both UPSERT on `id`
  (previously only entities did), so a resumed run reproduces the same durable
  graph as the original. The emitted `mark_unseen` value is recorded in
  `stats.json`. Tracked under clarion-dd29e69e0e.
- **`clarion analyze --prune-unseen` (WP9-B, REQ-FINDING-06).** After emission
  (Phase 8b), POSTs Filigree's `POST /api/loom/findings/clean-stale` retention
  route, scoped to `scan_source=clarion`, asking it to soft-archive its own
  `unseen_in_latest` Clarion findings older than
  `integrations.filigree.prune_unseen_days` (default 30). Soft-archive, not
  delete: Filigree moves them to `fixed` and auto-reopens on reappearance
  (Filigree ADR-015). Enrich-only — a Filigree outage or the integration being
  disabled is recorded in `stats.json` (`filigree_prune`) and never fails the
  run; the `scan_source` scoping is enforced server-side so the sweep can only
  touch Clarion's findings. (Closes the REQ-FINDING-06 piece of
  clarion-dd29e69e0e; the route was found to already exist in Filigree, so the
  earlier "file a request" memo is superseded — see its withdrawal banner.) The
  remaining Phase-0 scan-run-create handshake is an open contract question with
  Filigree (Clarion relies on the peer tolerating an unknown `scan_run_id`),
  tracked separately under `release:1.1`.

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

### Fixed

- **macOS release build under `-D warnings`.** The Linux-only `prlimit`
  helpers imported/defined in `clarion-core::plugin::host` (and two Linux-only
  test sites in `clarion-mcp` and `clarion-cli`) were unused on
  `*-apple-darwin`, so the release build failed with unused-import / dead-code
  errors. They are now `cfg`-gated to `target_os = "linux"` (plus `test` where
  unit tests reference them). CI gained a native macOS (aarch64) build + clippy
  leg so the gap can't recur; the x86_64 (macos-13) leg is temporarily parked
  while those runners are offline (clarion-12667da9f5, clarion-ec389a8e72).

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

[Unreleased]: https://github.com/tachyon-beep/clarion/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/tachyon-beep/clarion/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/tachyon-beep/clarion/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/tachyon-beep/clarion/releases/tag/v1.0.0
