# Changelog

All notable changes to Loomweave are documented here. The format is loosely based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Loomweave
follows [Semantic Versioning](https://semver.org/) for the `loomweave` binary,
the workspace crates, and the Python plugin.

API versioning for the federation HTTP read API (`/api/v1/...`) is independent
of product semver: `api_version: 1` is the wire-contract version, and bumps
only when an incompatible change is made to that surface. See
[`docs/federation/contracts.md`](docs/federation/contracts.md).

## [Unreleased]

Nothing yet.

## [1.5.0] — 2026-08-29

### Added

- **Isolated worktree indexes.** A linked Git worktree now gets its own
  per-worktree index store nested under the primary checkout's
  `.weft/loomweave/worktrees/<stable-id>/` instead of silently sharing (or
  shadowing) the primary's index. `loomweave serve` on an unbuilt linked
  worktree bootstraps it: a detached `loomweave worktree analyze` is spawned
  once, and graph tools answer structured `index-building` /
  `index-build-failed` envelopes (with the exact fallback command) until a
  run completes — no reconnect needed. The federation HTTP read API is gated
  identically (`503 INDEX_BUILDING` / `INDEX_BUILD_FAILED`;
  `/api/v1/_capabilities` stays open), so consumers polling during the build
  get an explicit not-ready signal, never empty-but-plausible answers. A
  confined deletion primitive (`openat2`-pinned, Linux) plus a cleanup sweep
  reclaims stores whose worktrees are gone; under a shared
  `[loomweave].store_dir` override — judged by the override state the
  context was *resolved* under — the sweep is report-only. New
  `loomweave worktree analyze [--no-incremental] [--config <path>] --
  <name-or-path>` builds a worktree index directly; without `--config` it
  resolves the same source→primary configuration ladder `serve` reports.

- **Catalogue list lead summaries.** Finding, faceted, shortcut, and semantic
  list responses now lead with compact totals, aggregate counts, completeness,
  confidence, and recovery advice. Paginated responses remain explicitly
  partial even on a short final page, and aggregate scans fail completeness
  closed when their bounded source scan cannot cover every candidate.

- **Single-source instruction block on a CLAUDE.md → AGENTS.md redirect**
  (weft convention C-20). When a project's `CLAUDE.md` carries, outside every
  managed block, a line that is solely `@AGENTS.md` / `@./AGENTS.md`, `loomweave
  install` now writes its block to `AGENTS.md` alone — creating it if absent —
  and migrates any legacy `CLAUDE.md` block out under the C-4 own-namespace and
  foreign-safe rules. Projects with no such redirect keep the existing
  dual-write unchanged. `loomweave doctor` inverts to match: under a redirect
  the block's *absence* from `CLAUDE.md` is the healthy state, a leftover block
  is a gating problem, and `--fix` migrates it rather than re-injecting.
  Detection fails safe — an absent, unreadable, non-UTF-8 or symlinked
  `CLAUDE.md` reads as no redirect.

### Changed

- **The always-loaded agent-context surface is now budgeted** (weft convention
  C-20). The instruction block injected into `CLAUDE.md`/`AGENTS.md` shrank from
  1,731 to 789 chars and is now a pointer — what Loomweave is, when to reach for
  it, where the full reference lives, and two load-bearing rules. The
  `loomweave-workflow` skill's `SKILL.md` shrank from 36,249 to 3,744 chars; its
  depth moved **verbatim** into five on-demand `references/*.md` files shipped
  with the skill (`tools`, `catalogue`, `freshness`, `gotchas`, `entity-ids`).
  Nothing was deleted — `loomweave install --skills` / `--codex-skills` install
  the reference files alongside `SKILL.md`. The MCP `loomweave-workflow` prompt
  serves `SKILL.md` only; the references are read from the installed skill
  directory.

### Removed

- **The `weft.toml [loomweave].url` resolution rung** — retired for the same
  reason as the `[filigree].url` rung below (owner-ratified): repository
  content must never steer where a consumer that attaches credentials sends
  them. A `weft.toml [loomweave].url` value is now ignored; operators move
  the override to the `WEFT_LOOMWEAVE_URL` environment variable or the
  consumer's private configuration. Ephemeral-port discovery is unchanged.

- **The `weft.toml [filigree].url` resolution rung.** Repository content may
  be untrusted, while Filigree clients attach operator-owned bearer tokens to
  the resolved endpoint — so a repo-controlled file may no longer steer where
  those tokens are sent. Operators who used that key must move the override
  to the `WEFT_FILIGREE_URL` environment variable or private configuration.
  Live discovery via the `.weft/filigree/ephemeral.port` sidecar is
  unchanged, and for a linked worktree it falls through source root →
  primary root; the minted `federation_token` is always read from the same
  root the port resolved from, never first-found across candidates.

### Security

- **Hardened git spawns now clear the environment.** Every production `git`
  invocation goes through `hardened_git_command`, which `env_clear()`s the
  child and re-adds a closed allowlist (`PATH`, locale pins, the git
  config/attr/locks hardening variables). Ambient `GIT_DIR` /
  `GIT_COMMON_DIR` / `GIT_WORK_TREE` (and any other inherited git variable)
  can no longer redirect Loomweave's git probes — including the
  deletion-adjacent worktree cleanup sweep — to a foreign repository. Note
  `GIT_CEILING_DIRECTORIES` is also dropped by the clear.

### Fixed

- **A transient pyright failure no longer leaves a permanent hole in the call
  graph** (clarion-3e517d4aff). The Python plugin degraded to *empty* call /
  reference evidence — not an error — whenever pyright timed out, crashed, or
  tripped its restart cap (after which every later file in the run silently
  got nothing). The host persisted that as a completed analysis and the
  incremental skip then pinned the byte-identical file forever, so
  `entity_callers_list` answered `[]` with `traversal_complete: true` for
  callees whose callers were never extracted (elspeth: 617 of 2,779
  function-bearing files). Now every `analyze_file` result carries a per-file
  `resolution_coverage` claim (`calls` / `references`: `complete` |
  `degraded`, with a `transient` flag); the host records it in a new
  `source_file_resolution_coverage` table (migration 0013) and force-
  re-dispatches `degraded && transient` files on the next incremental run.
  Indexes built by an older binary self-heal: a file with no coverage row
  that owns symbols yet carries zero `calls` edges and zero unresolved sites
  is re-dispatched once. The completion line and `runs.stats`
  (`resolution_degraded_files`) say how many files degraded this run; the
  caller-navigation tools add `"degraded-call-resolution"` to
  `scope_excludes` (withdrawing `traversal_complete`) plus a
  `degraded_call_coverage_files` count while any such file exists; and
  `loomweave doctor` gains an `index.resolution_coverage` check. Re-dispatch
  is ordered and bounded (migration 0014): files whose *own* resolution
  failed are dispatched last (a `collateral` flag on the claim marks files
  that were merely behind the troublemaker), and a file that stays degraded
  for 3 consecutive runs stops forcing re-dispatch until its bytes change —
  so one pathological file cannot make every incremental run pay the full
  cost. The external SQLite read ceiling advances to `user_version` 14
  (additive columns only).
- **Stale syntax findings now retire when files are fixed.** A file whose
  parse error was fixed no longer keeps its old `*-SYNTAX-ERROR` finding
  forever: a per-plugin sweep retires findings for files whose
  classification completed cleanly in the current run — bounded to
  batch-persisted classifications, per plugin, so a partial or degraded run
  never retires findings it has no evidence about. Plugins declaring a
  colliding `rule_id_prefix` are rejected at discovery (dropped loudly; the
  run fails if none remain) since attribution — and therefore safe
  retirement — is impossible under a shared prefix.
- **Doctor fixes.** The read-API liveness probe now targets a route the API
  actually serves (`/api/v1/_capabilities`); classifier-coverage checks no
  longer fail on a plugin-less machine (not-applicable degradation, narrow);
  git db-tracking checks are argument-injection-hardened and immune to
  repo-local `core.fsmonitor`/pager configuration.
- **Storage integrity treats non-directory source path components as stale**
  (`NotADirectory` now handled like `NotFound`), so `doctor --fix` repairs
  rather than erroring when a recorded directory was replaced by a file.
- **Doctor bootstrap-state repair.** `loomweave doctor` now warns when an
  installed catalogue has never been analysed or lacks its local instance
  identity. `loomweave doctor --fix` materialises a private project UUID and
  runs an authoritative analysis to regenerate classifier coverage metadata.
- **The Python plugin no longer trusts the launcher's `PATH` for call
  resolution** (clarion-5cf9643de9, [ADR-058](docs/loomweave/adr/ADR-058-project-interpreter-discovery.md)).
  `pyright-langserver` type-checks against whatever `python` it finds unless
  pinned; under an agent hook that was the system interpreter, which cannot
  import the project's own editable install, so `tests/` → `src/` call
  targets came back empty while coverage still said `complete`, and the
  incremental skip pinned the hole. Both the host and the Python plugin now
  run the same fixed discovery order (`LOOMWEAVE_PYTHON_INTERPRETER`
  override → `.venv` → `VIRTUAL_ENV` → `CONDA_PREFIX` → first
  `python`/`python3` on `PATH` → none) and pin pyright's `python.pythonPath`
  to the result. An unpinned interpreter now honestly demotes an
  otherwise-`complete` facet to `degraded` (`interpreter_unpinned`); a new
  `plugin_index_meta.resolver_environment` fingerprint (migration 0015)
  forces a full re-dispatch of the plugin's files when the interpreter
  changes, and `doctor`'s coverage remedy names the fix
  (`LOOMWEAVE_PYTHON_INTERPRETER` or `.venv`). `doctor` also gains an
  `index.runs` check that reaps `running` rows abandoned by a dead builder.
  The external SQLite read ceiling advances to `user_version` 15
  (`plugin_index_meta` only; not part of the external safe projection).

## 1.5.0 — 2026-07-12

Minor release on top of `1.4.2`. (Cargo SemVer `1.5.0`; Python wheels `1.5.0`.)

### Added

- **Versioned classifier coverage evidence.** Completed analyses now persist
  strict `loomweave.classifier-coverage.v1` metadata for plugin discovery,
  source walking, declared classifier tags, and per-plugin
  matched/analyzed/retained/degraded counts. Tag-backed MCP responses attach
  `loomweave.classification.v1`, separating supported-zero results from
  partial, unsupported, unavailable, or truncated enumeration.
- **Supported external SQLite federation boundary.** Local consumers can check
  typed `compatible`, `older_supported`, and `incompatible` results for the
  read-only `loomweave.external-sqlite.v1` projection before issuing catalogue
  SQL. `loomweave doctor` reports this compatibility together with classifier
  enumeration, active tag declarations, HTTP auth posture, and project
  instance-ID validity.
- **Federation seam producer goldens.** Loomweave now publishes byte-pinned
  classifier coverage/classification, capabilities, bearer/HMAC auth,
  identity-ownership, and external-SQLite fixtures. A real Python analysis and
  production handlers produce the coverage, classification, capabilities,
  identity, and live auth-handler matrix. The external-SQLite cases and the
  canonical auth signing vector are synthesized deterministically, then
  rechecked against production compatibility/auth code. The live fixture proves
  all four Plainweave denominator classes as supported and complete at
  `5/5/0/0`.

### Changed

- **Identity responses prove project ownership.** Every successful identity
  response, including not-alive and batch envelopes, now carries `api_version`
  and `instance_id`. The unauthenticated capabilities probe advertises the
  effective protected-route mode (`none`, `bearer`, or `hmac`) without exposing
  credential names or values, so consumers can authenticate and recheck
  ownership before joining HTTP identity to local catalogue rows.

## 1.4.2 — 2026-07-12

Patch release on top of `1.4.1`. (Cargo SemVer `1.4.2`; Python wheels `1.4.2`.)

### Fixed

- **Python export rebinding accuracy.** Explicit `__all__` exports now follow
  final statically visible module bindings, so a local function or class later
  replaced by an import, assignment, or other module-scope binding is no longer
  reported as the runtime exported API object.
- **Python overload export classification.** Recognized `@overload` stubs no
  longer compete with their implementation for explicit `__all__` exports; the
  implementation receives `exported-api`, and the module no longer receives the
  fallback `exported-api` tag.
- **Python reference-resolution fanout.** Unshadowed built-in names now bypass
  Pyright definition queries as known-external references, while imported or
  potentially shadowed names remain position-resolved so rebinding stays
  correct. This preserves resolved-reference results while bounding
  language-server query fanout on large typed files.
- **Unix project-anchor validation.** Phase 3 now accepts project directory
  basenames containing `:` when validating protected project anchors on Unix.
- **Partial-analysis subsystem preservation.** Runs with plugin crashes now skip
  Phase 3 clustering, preserving the prior authoritative subsystem projection
  instead of reconciling a mixed old/new graph.
- **Subsystem finding lifecycle preservation.** Before obsolete subsystem
  entities are removed, acknowledged, suppressed, promoted, or Filigree-linked
  findings are re-anchored to the project entity with their former subsystem
  recorded, preventing graph reconciliation from cascading away operator state.
- **Versioned duplicate-locator evidence.** `LMWV-DUPLICATE-LOCATOR` now
  publishes evidence contract v2 with explicit declaration and collision path
  keys. The deprecated `first_source_file_path` alias preserves the first
  declaration path instead of the colliding path, while entity
  collision reads remain compatible with unversioned findings already stored.
- **Rust public re-export reachability.** Targets of uniquely resolved,
  externally visible `pub use` edges now seed dead-code reachability; test-only
  re-exports remain excluded from `app_only`. Rust ontology `0.8.0` invalidates
  cached extraction values so unchanged files gain the new edge provenance.
- **Rust dead-code lint scope.** `#[allow(dead_code)]` and
  `#[expect(dead_code)]` now propagate through inline modules and impl blocks,
  while nested `warn` / `deny` / `forbid` levels correctly cancel inherited
  keep signals. Direct and impl-level `#[cfg(test)]` items are also classified
  as tests for `app_only` filtering. Rust ontology `0.8.1` invalidates cached
  extraction values for the corrected tags.
- **Rust compound test cfg classification.** Test-only tags now cover compound
  predicates that prove an item cannot exist with `test=false`, while
  disjunctions and `cfg_attr` forms that can admit app code remain app-visible.
  Rust ontology `0.8.2` invalidates cached extraction values for the corrected
  tags.
- **CI-visible Wardline golden drift.** The reusable verify workflow now checks
  Loomweave's vendored taint-fact fixture byte-for-byte against the public
  Wardline `main` authority inside the required Rust job, so sibling drift
  blocks PR merges and releases instead of only failing local workspaces that
  happen to have both repositories.
- **Deleted-file graph pruning.** Incremental analysis now removes scan-time
  anchored edges and unresolved call sites owned by vanished source files,
  while retaining cumulative entity rows long enough to preserve deletion
  findings and SEI lineage.
- **Fail-closed source metadata integrity checks.** Index integrity checks now
  treat only `NotFound` as a vanished source; other filesystem metadata errors
  propagate and prevent repair from deleting uncertain file entities.
- **Stable incremental subsystem materialization.** Phase 3 now reconciles the
  persisted subsystem graph to each run's authoritative clustering result,
  preserving stable subsystem IDs while removing obsolete subsystem entities
  and membership edges, including on unchanged incremental reruns.
- **Plugin syntax-finding deduplication.** Host fallbacks now use each plugin's
  declared rule namespace and are suppressed per file when the plugin already
  reported that syntax failure, removing wrong-language duplicate findings for
  degraded Rust files while retaining fallback coverage for silent plugins.
  Plugin syntax identities are file-scoped, and legacy duplicate rows merge
  into the canonical finding without losing status or Filigree linkage. A
  versioned per-plugin marker forces each plugin through the one-time
  incremental re-dispatch needed to migrate old indexes, including plugins that
  leave and later return; the run-level marker is retained when a migration
  resumes. Reconciliation is symmetric, so a later silent or partial plugin
  pass can fall back without recreating a duplicate or losing migration audit.

## [1.4.1] — 2026-07-10

### Added

- **Plainweave public-surface denominator completeness.** Loomweave now exposes
  MCP shortcuts and plugin tags for the four public-surface classes Plainweave
  uses for intent coverage: `entry-point`, `http-route`, `exported-api`, and
  `cli-command`, including Python `__all__ += [...]` exports and common
  main-guard runner wrappers.

### Changed

- **Coverage honesty over higher-looking ratios.** Expanding these tags can lower
  Plainweave coverage on real projects by adding previously invisible public
  surfaces to the denominator. That is the desired result: Plainweave's prior
  degraded output was correct; requirements are right; Loomweave now sees more
  doors.

### Security

- Bumped the transitive Rust dependency `crossbeam-epoch` `0.9.18` -> `0.9.20`
  in both the workspace and standalone Rust-plugin distribution lockfiles to
  clear RUSTSEC-2026-0204. Lockfile only; no API change.

## [1.4.0] — 2026-06-30

Feature release on top of `1.3.1`. (Cargo SemVer `1.4.0`; Python wheels `1.4.0`.)
Highlights: a new `loomweave-llm` provider crate (so `loomweave-core` links no
outbound HTTP client), the Warpline churn consumer lighting up the churn/recency
read surfaces, Rust-plugin dead-code reachability roots (ADR-054), and a batch of
federation-keying and graph-integrity fixes.

### Added

- **Warpline churn consumer.** Loomweave now consumes Warpline's frozen churn
  read to populate the `high_churn` and `recently_changed` surfaces
  (`entity_high_churn_list` / `entity_recent_change_list`). The consumer speaks
  `warpline-mcp`'s newline JSON-RPC with a bounded timeout and honest
  paging/keying disclosure. Federation is enrich-only and fail-soft — with
  Warpline absent the surfaces degrade cleanly.
- **Rust-plugin dead-code reachability roots (ADR-054).** The Rust language
  plugin now emits reachability-root tags — visibility / entry-point / test /
  handler roots, framework handlers, public-method rooting, and edition-2024
  `#[unsafe(no_mangle)]` / `#[unsafe(export_name)]` FFI entry-points — the Rust
  analog of the Python public-surface roots, so Rust dead-code analysis no longer
  reports the whole public API as dead.

### Changed

- **LLM / embedding providers extracted into a new `loomweave-llm` crate.** The
  provider traits and implementations moved out of `loomweave-core`; a CI gate
  now asserts `loomweave-core` links no outbound HTTP client. No change to the
  `summary` API or provider configuration.
- **SEI git-rename consumer re-pointed to `legis`'s `/git/rename-feed`.**
  `LegisGitRenameSource` now reads the committed leg of `legis`'s additive
  superset endpoint `GET /git/rename-feed?base=…&head=HEAD` instead of the legacy
  `GET /git/renames?rev_range=…`. `legis` pins each `committed[]` entry
  byte-identical to the old `/git/renames` array with a contract-lock test, so
  committed renames resolve unchanged; the `working_tree` leg is not consumed
  (committed-window semantics preserved — the shell source remains the sole
  working-tree authority). Enrich-only and fail-soft as before; the wire-drift
  guard now warns on a legacy flat-array body or a `committed`-less envelope.
  (Federation ledger B3.)
- **Filigree federation transport over newline JSON-RPC.** The filigree-mcp
  federation client now speaks newline-delimited JSON-RPC instead of
  `Content-Length` framing (#78).

### Fixed

- **Briefing-blocked entities keep their SEI on the read surface.** Federation
  keying no longer nulls the Stable Entity Identity of briefing-blocked entities
  on the MCP read path, so Filigree/Wardline lookups keyed by SEI resolve
  (#79; PDR-0008).
- **Same-locator collisions are disclosed on the entity read path.** A duplicate
  entity locator now surfaces as an entity-anchored `LMWV-DUPLICATE-LOCATOR`
  finding anchored to the colliding (survivor) entity, so the shadowed
  declaration is queryable from the entity read path instead of silently
  resolving to a clean row (clarion-48af930f2a, #74).
- **Stale `contains` edge pruned when a file-scope claim moves.** The writer
  prunes a stale parent/`contains` edge when a `file_scope` entity's claim moves,
  self-healing the module/package dual-claim mismatch on re-emit
  (clarion-abda98c869, #75).
- **Incremental re-analyze on a tag-schema move.** A plugin's files are
  re-dispatched when its ontology tag schema changes between runs, so tag-driven
  surfaces don't go stale on an incremental walk (clarion-e12d424f1d, #71).
- **Plugin findings can no longer forge the trusted anchor.**
  `validate_plugin_finding` strips the host-reserved `anchor_entity_id` metadata
  key, so a plugin-reported finding can no longer override the trusted file
  anchor — which previously could hard-fail the analyze run on the findings
  foreign key, or silently mis-associate the finding (#80).

### Tests

- Cross-repo conformance oracles for the loomweave↔filigree and
  loomweave↔wardline seams (Filigree entity-associations, the Wardline taint-fact
  wire, and the Wardline trust-vocabulary descriptor), pinning the wire bytes
  each sibling produces or consumes so a silent drift reds in CI.

### Security

- Bumped `anyhow` `1.0.102` → `1.0.103` to clear a RUSTSEC soundness advisory in
  `anyhow`'s `downcast_mut` (dtolnay/anyhow#451). Lockfile only; no API change.

## [1.3.1] — 2026-06-22

Patch release on top of `1.3.0`. (Cargo SemVer `1.3.1`; Python wheels `1.3.1`.)
Operator-guidance docs plus a transitive-dependency security bump — no runtime
or API change.

### Changed

- **Stale-index refresh discipline.** Rewrote the `loomweave-workflow` skill's
  "Manual scanning" section and the `StaleWorktree` / `Stale` `staleness_note`
  strings (`project_status_get`) to stop agents under-refreshing a stale index
  on "it's just research / read-only" reasoning. The guidance now leads with the
  fact that a Loomweave answer is a claim *about the source tree as of the last
  analyze* — so read-only orientation is exactly where stale poisons the result —
  states the real refresh cost (analyze is incremental: unchanged files are
  skipped by whole-file content hash; `analyze_start` is non-blocking; the
  advisory lock makes a second runner a clean no-op), and carries a
  rationalization table plus red-flags. It permits exactly one observable
  valid-skip: `index_diff_get` shows the drift disjoint from the files/entities
  the task touches (it *scopes* drift, it does not *overturn* the verdict — both
  surfaces derive from one freshness oracle). The `Fresh` note is unchanged.

### Security

- Bumped the transitive Python build/tooling dependency `msgpack` `1.1.2` →
  `1.2.1` (via `cachecontrol[filecache]`) to clear GHSA-6v7p-g79w-8964. Lockfile
  only (`plugins/python/uv.lock`); no runtime or API change.

## [1.3.0] — 2026-06-19

Feature release on top of `1.2.1`. (Cargo SemVer `1.3.0`; Python wheels `1.3.0`.)

### Added

- **Auto-refresh on session start.** The `loomweave hook session-start` hook now
  starts ONE detached, non-blocking `loomweave analyze` when it finds a stale
  index — stdio to `/dev/null`, in its own process group, never waited on, so it
  never blocks session start. The analyze advisory lock keeps it single-shot. A
  fresh index (or a spawn failure) falls back to the existing manual-analyze
  nudge. The `loomweave-workflow` skill gains a clearer "Manual scanning"
  section describing the `analyze_*` cycle and this auto-refresh.

### Changed

- **MCP write tools default on for the local agent loop.**
  `serve.mcp.enable_write_tools` now defaults to `true`, so the on-demand
  `analyze_start` / `analyze_status_get` / `analyze_cancel` and
  `entity_summary_get` tools register out of the box. Consult-mode read-only
  sessions opt back out with `enable_write_tools: false`. `loomweave config
  check` now reports the effective MCP write-tools state.

### Fixed

- **Deletion tombstones no longer wedge the index stale (clarion-23a44085f9).**
  The `entities` table is cumulative and never-pruned by design (REQ-ANALYZE-04),
  so a deleted source file leaves its rows behind as a tombstone. The freshness
  oracle previously gated on missing indexed files, which meant any repo that
  ever deleted a file read `stale` forever (re-analyze could never clear it).
  Missing files are now reported informationally in `missing_files` but no longer
  drive the freshness verdict; commit mismatch, HEAD-newer, modified files, and
  staged changes still do. Supersedes clarion-e687941a8c.

## [1.2.1] — 2026-06-18

Maintenance patch for the `1.2.0` consult-surface release. (Cargo SemVer
`1.2.1`; Python wheels `1.2.1`.)

### Fixed

- **Briefing-block redaction consistency.** `unresolved_candidates` no longer
  echoes parsed callee text from briefing-blocked owners, and custom subsystem
  member / relation-candidate projections now reuse the same high-entropy
  identity guard as the canonical blocked entity stub.
- **Dossier delegated-read honesty.** `entity_orientation_pack_get` now treats
  delegated MCP envelopes as envelopes: `ok:false` from `entity_wardline_get` or
  `entity_finding_list` becomes an explicit unavailable dossier section instead
  of a silent `null` / empty result.
- **`app_only` dead-code reachability.** `entity_dead_list app_only:true` removes
  test-tagged and core-plugin entities from reachability roots and traversal
  edges before computing liveness, so test-only reachability no longer suppresses
  app dead-code candidates.
- **Inferred caller hint wording.** `next_action` no longer claims unresolved
  name matches are absent from `callers`, which was false after inferred dispatch
  materialized a caller.
- **Release reproducibility and public version pointers.** The standalone Rust
  plugin distribution lockfile is regenerated for the current package versions,
  and README / site release pointers now name the current release.

## [1.2.0] — 2026-06-18

A consult-surface increment from dogfooding the MCP tools against a live Weft
testbed: richer orientation, more honest traversal/availability signals, and
app-scoped queries. Every addition is backward compatible — omitting the new
params leaves responses byte-identical to `1.1.0`. (Cargo SemVer `1.2.0`; Python
wheels `1.2.0`.)

### Added

- **Entity dossier via `include` on `entity_orientation_pack_get`**
  (clarion-2b87cd7a59). An optional `include: ["wardline","findings","issues"]`
  param folds a composed `dossier:{wardline,findings,issues,summary_available}`
  into the consult-mode pack — pure read-path composition of existing surfaces,
  no re-analysis. The `findings` section mirrors `entity_finding_list`'s result
  (a `findings` array plus `page` pagination metadata, so truncation past the
  first page is visible); `summary_available` is `true` only when
  `entity_summary_get` would serve from cache (full cache key present and
  unexpired). Finding fingerprints are normalized to one canonical form
  (stripping any `wlfp2:` prefix) across the dossier. Omitting `include` (or an
  empty array) leaves the packet byte-identical.
- **App-scoped roots + `app_only` filter on dead-code and coupling surfaces**
  (clarion-663aca16aa). `entity_dead_list` gains `roots: "auto"`, deriving
  reachability roots from emitted tags and reporting `roots_mode` +
  `roots_confidence` (explicit mode remains the default and is unchanged). Both
  `entity_dead_list` and `entity_coupling_hotspot_list` gain opt-in
  `app_only: true`, a read-path filter that drops test-tagged and non-first-party
  (core-plugin) entities, removing excluded endpoints from both rows and
  neighbour counts.
- **Per-query caller honesty via `traversal_complete` + `unresolved_candidates`**
  (clarion-76c31b730a). `entity_callers_list` and `entity_neighborhood_get` no
  longer emit a blanket `scope_excludes` footer: `scope_excludes` is populated
  ONLY when *this* traversal actually skipped a name-matched unresolved
  candidate, otherwise it is `[]` paired with the new `traversal_complete: true`
  — so an empty `callers` list reads as a true negative. Skipped sites surface as
  the new `unresolved_candidates` `[{path, line, callee_text, why}]` in-tool
  grep-fallback.
- **Inline entity-row tags + arg aliases** (clarion-057ff2b330). Entity rows
  carry their tags inline, and `entity_resolve` / `entity_relation_list` accept
  argument aliases for friendlier consult-mode calls.

### Fixed

- **Secret content — not entity identity — is redacted in `briefing_blocked`
  rows** (clarion-719e7320f5). A briefing-blocked entity now rides alongside its
  real, navigable identity (`id`/`kind`/`name`/`source_file_path`/line span/
  `content_hash`); only the secret-bearing file content stays withheld. A
  read-path entropy guard re-withholds an identity field only when a single path
  *segment* is itself a long, high-entropy blob (≥4.5 bits/byte) — so a secret
  embedded as a qualname segment is still redacted, while long descriptive Rust
  `::` paths, dotted qualnames, and verbose `snake_case` names stay navigable.
- **`summary_available` in the dossier now matches the full summary cache key.**
  It previously matched on `content_hash` alone, reporting `true` for a summary
  whose template / model tier / guidance had since changed —
  `entity_summary_get` keys on the full tuple and would miss it, so a consult
  caller could wrongly skip generating. It now mirrors the summary tool's exact
  gate (full key + expiry window).
- **`unresolved_candidates` is gated behind `confidence != Inferred`** (A1). On
  the inferred dispatch path `traversal_complete` is forced `true`; surfacing
  skipped candidates there contradicted that completeness claim.

## [1.1.0] — 2026-06-15

First stable **1.1.0** release — the GA of the `1.1.0rc1`–`rc10` candidate
series, cut over from the `rc` line to `main`. The final candidate (rc10) added
a self-qualifying lead summary to `find_dead_code` (`entity_dead_list`) — the
reference implementation of an agent-first convention: any tool that returns
*a number of things in various kinds* states the breakdown up front. The
cumulative changes since `1.0.0` are catalogued in the `1.1.0rc1`–`rc10`
sections below. (Cargo SemVer `1.1.0`; Python wheels `1.1.0`.)

### Added

- **Lead summary on `entity_dead_list`.** The response now carries a top-level
  `summary` block — `dead_candidates`, `reachable`, `not_analysed`
  (`plugins_without_roots` / `shielded_by_unresolved_calls` / `withheld_secret`),
  and a `confidence` verdict — so an agent reads the *shape* of the result without
  counting rows or hunting for a caveat. When an implausibly large share of
  analysed entities is unreachable (>25%, the sign that reachability roots don't
  cover the corpus — e.g. a library exercised only by tests), `confidence` reads
  `low` and an `advisory` *recruits* the operator to configure entry-point roots,
  instead of letting a misleading raw candidate count stand (an external review
  docked trust on exactly this: 141 candidates read as alarming until the caveat
  was found).

Ninth 1.1 release candidate. Fixes a bare-qualname `scope` returning nothing for a
*package* name. No package is published for release candidates. (Cargo SemVer
`1.1.0-rc9`; Python wheels normalise to PEP 440 `1.1.0rc9`.)

### Fixed

- **`scope` by package name now selects the whole namespace (`lacuna-522ab56124`).**
  A bare dotted `scope` is treated as a namespace: it selects every entity whose
  qualname is the scope or a descendant `scope.*`, via the new
  `entity_ids_in_namespace`. Previously it resolved the scope to a single *exact*
  entity and walked `contains` edges, so a package name (`specimen`) reached only
  the package `__init__`'s own members and missed sibling submodules entirely —
  returning **0** on edge-derived tools (`entity_coupling_hotspot_list`,
  `module_circular_import_list`), which was a silent-empty that read as "nothing
  here". Module-qualname scoping is unchanged (the namespace match subsumes it),
  and a scope that matches no entity still falls back to a path glob. The
  qualname/`namespace.` boundary test is underscore-safe (no `LIKE` wildcard
  hazard) and rejects prefix-but-not-namespace look-alikes (`specimentary`).

Eighth 1.1 release candidate. Fixes a routine `loomweave analyze` (the default
incremental mode) 400'ing its Filigree emit on an unchanged tree, and adds an
explicit `"default"` opt-in for the codex sidecar model. No package is published
for release candidates. (Cargo SemVer `1.1.0-rc8`; Python wheels normalise to PEP
440 `1.1.0rc8`.)

### Fixed

- **Incremental `analyze` no longer 400s its Filigree emit.** `mark_unseen` (which
  asks Filigree to sweep findings this scan did not report as gone) is now sent
  `true` only when the scan examined the whole corpus (`skipped_files_total == 0`
  — a `--no-incremental` run or a first run), not on every fresh run. An
  incremental run that skipped unchanged files has no authority to sweep their
  findings, and an incremental no-op would otherwise POST an empty batch with
  `mark_unseen=true` — which Filigree rejects with `VALIDATION` 400 ("mark_unseen
  requires at least one finding or scanned path"). `--resume` still never sweeps.

### Added

- **`codex_model = "default"` opt-in.** Setting `llm_policy.codex_cli.model` (or
  `--codex-model`) to `"default"` (any case) explicitly accepts the codex CLI's
  own default model — it acknowledges the floating-model cost instead of pinning
  one, behaves as unset (no `--model` flag is passed), and silences the
  "model is unset" config-check warning.

## [1.1.0rc7] — 2026-06-15

Seventh 1.1 release candidate. Completes the loomweave→Filigree finding-emit work
from rc6: file-*less* findings now reach Filigree too. The
`LMWV-DUPLICATE-LOCATOR` finding now carries an `anchor_file_path` (its first-seen
declaration) so it anchors to a real file instead of the file-less project anchor
— previously such findings were skipped at emit (`skipped_no_path`), leaving the
duplicate-locator lacuna untrackable in Filigree (the residual half of the
dogfood's Friction A). No package is published for release candidates.
(Cargo SemVer `1.1.0-rc7`; Python wheels normalise to PEP 440 `1.1.0rc7`.)

### Fixed

- **Duplicate-locator findings reach Filigree.** `LMWV-DUPLICATE-LOCATOR` now
  sets `anchor_file_path` (its first-seen declaration), so
  `host_finding_anchor_id` anchors it to a real file and the scan-results emit
  carries it instead of dropping it as `skipped_no_path` under the file-less
  `core:project:*` anchor.
- **File-less findings are skipped, not forced under a bogus path.** A genuinely
  project-level finding (e.g. the weak-modularity subsystem fact) has no real
  file, and Filigree's intake rejects every synthetic stand-in — an absolute
  project root, and the relative `"."` (its `_normalize_scan_path` collapses
  `"."` to `""`, rejected as "path is empty after normalization"). The emit no
  longer attempts a synthetic project-anchor path (which 400'd the whole batch);
  such findings stay store-only and are counted `skipped_no_path`. A finding
  whose path equals the project root (relativizes to `""`) is likewise treated as
  path-less rather than emitted empty.

## [1.1.0rc6] — 2026-06-15

Sixth 1.1 release candidate. Makes the Loomweave→Filigree finding-emit seam work
end-to-end for the first time against a real multi-project Filigree server —
surfaced by the Lacuna dogfood, where emit was disabled by default and then,
once enabled, posted to an unscoped endpoint with absolute paths and
out-of-range line numbers, each rejected in turn. Also fixes the catalogue
`scope` qualname-resolution gap. No package is published for release candidates.
(Cargo SemVer `1.1.0-rc6`; Python wheels normalise to PEP 440 `1.1.0rc6`.)

### Added

- **Doctor stale-port health probing.** `loomweave doctor --json` now probes
  `/health` when HTTP config resolves from `.weft/loomweave/ephemeral.port` and
  reports stale persisted port metadata as an advisory warning.
- **Instruction last-writer marker.** Installed instruction blocks now include
  stable `loomweave:last-writer:loomweave install` metadata in the canonical
  drift body so installer provenance participates in idempotency checks.
- **Filigree EntityAssociation fixture coverage.** Added a canonical
  EntityAssociation reverse-lookup fixture and parser coverage for
  `loomweave_entity_id`, `content_hash_at_attach`, and attach metadata.
- **MCP Wardline facet enrichment.** `entity_wardline_list` with
  `has_findings` now reconciles Filigree-provided Wardline findings for
  project-relative paths when the local findings table does not carry the
  anchor.
- **SEI oracle coverage guard.** Added a reference-test guard that keeps the SEI
  oracle fixture scenarios and implemented conformance tests in sync.

### Changed

- **ADR-049 lockstep posture.** Documented ADR-049 as the versioned Rust-feature
  dialect adopted by Loomweave and Wardline in lockstep, outside the clean-break
  core API freeze set.

### Fixed

- **Storage coalescing flake stabilization.** Stabilized the cold inferred-call
  coalescing test with a gated provider so follower requests wait behind the
  leader instead of racing a timing delay.
- **Loomweave→Filigree finding emit against a multi-project server.** The
  scan-results emit now pins the configured project as `?project=<key>`
  (`integrations.filigree.project`), so a shared Filigree server no longer
  rejects it as an ambiguous server-mode write; emits project-relative finding
  paths (stored absolute paths are stripped to the project root) instead of
  absolute paths Filigree rejects; and clears finding line numbers that fall past
  end-of-file before emit (mirroring Filigree's lenient single-finding
  line-attribution), so a syntax-error/degraded finding no longer 400s the whole
  batch. Together these let Loomweave findings (`LMWV-*`) reach Filigree's tracker
  for the first time end-to-end.
- **Catalogue `scope` qualname resolution.** A bare dotted `scope` argument
  (`specimen`, `specimen.dead_code`) on `entity_dead_list`,
  `module_circular_import_list`, `entity_coupling_hotspot_list`, and the other
  scope-taking tools now resolves against entity qualnames (anchor + descendants)
  instead of being misclassified as a path glob that silently matched nothing.
  An unmatched token still falls back to a path glob.

## [1.1.0rc5] — 2026-06-13

Fifth 1.1 release candidate. This candidate adds agent-driveable LLM and
semantic-search configuration (MCP tools + CLI verbs), an LLM lookup
diagnostics log, and the Filigree minted-token auth rung, then remediates the
findings of the adversarial review of that work (weft-ac59e8e730). No package
is published for release candidates. (Cargo SemVer `1.1.0-rc5`; Python wheels
normalise to PEP 440 `1.1.0rc5`.)

### Added

- **Config MCP tools.** Four new tools let an agent inspect and edit the
  project's LLM and embedding posture: `llm_config_get` / `llm_config_set`
  (provider, enabled, allow_live_provider, model pins, OpenRouter key env /
  endpoint, and `serve.mcp.enable_write_tools`) and `semantic_config_get` /
  `semantic_config_set` (provider, model, dimensions, endpoint, key env,
  timeout, token ceiling). The two `*_config_set` tools deliberately **bypass
  the read-only write-tool gate** (bootstrap exemption): the gate itself is one
  of the settings they edit, so from a read-only session they can persistently
  enable write tools and live (paid) LLM/embedding spend. This is by design and
  is stated in the MCP server instructions and the `loomweave-workflow` skill.
- **`loomweave config llm|semantic` CLI verbs.** `config llm status|set` and
  `config semantic status|set` edit `loomweave.yaml` in place with validation,
  and `config example --provider <p>` emits an annotated stub. Operator-facing
  provider aliases are accepted everywhere providers are parsed
  (`openrouter_api`/`open_router`, `codex_sidecar`/`codex`,
  `claude_sidecar`/`claude_code`, and `openai`/`local`/`openai_local` for the
  embedding providers).
- **LLM lookup traffic log.** Every configured LLM lookup appends one JSONL
  metadata record to `.weft/loomweave/diagnostics/llm-traffic.jsonl` (inside
  Loomweave's member store dir; `store_dir` overrides move it too). The log is
  metadata-only by guarantee — provider, purpose, prompt template id, model,
  outcome, token usage, cost; never prompt text or model output — and is capped
  at 10 MiB with rotation to `llm-traffic.jsonl.1`.
- **Semantic-search configuration with honest degrade.** The
  `semantic_search:` block (hosted `api` or loopback-only `local_openai`
  provider) feeds `entity_semantic_search_list`; when it is enabled but the
  provider cannot be constructed (missing live opt-in or API key), `serve`
  warns about the inert embedding provider and the search degrades to
  `not_enabled` instead of failing startup.

### Fixed

- **Filigree auth: minted federation token as the final resolution rung**
  (dogfood-4 A5, weft-c7db813d9a). Token resolution was env-only, so the MCP
  serve path — launched with an empty env in `.mcp.json` — 401'd on every
  weft-gated read. The daemon's auto-minted
  `<root>/.weft/filigree/federation_token` is now read after the
  `WEFT_FEDERATION_TOKEN` / legacy `FILIGREE_API_TOKEN` env rungs across serve,
  scan-results emit, clean-stale, and SARIF import.
- **Review remediation (weft-ac59e8e730).** The adversarial review of the
  config/diagnostics work above confirmed nine findings, fixed in this
  candidate: the semantic loopback-trust gate no longer runs when
  `semantic_search.enabled: false` (a disabled block with a stale non-loopback
  endpoint had hard-failed `serve`/`config status` and blocked
  `config semantic set --disable` itself); IPv6 loopback (`http://[::1]:…`)
  endpoints are recognised; a diagnostics write failure no longer converts a
  successful (paid-for) LLM call into an error; concurrent traffic-log appends
  are serialized (no interleaved partial JSON lines) and colliding rotations no
  longer fail calls; the traffic log moved from the legacy `.loomweave/` root
  into `.weft/loomweave/` (C-9) with the installed store `.gitignore` covering
  `diagnostics/`; the `llm_config_set`/`semantic_config_set` inputSchemas
  declare real types and provider enums instead of bare `{}`; a committed
  agent-harness debug log (`error.log`) was removed and guarded; and the README
  MCP surface claim was corrected to 46 tools.

## [1.1.0rc4] — 2026-06-11

Fourth 1.1 release candidate. This candidate makes the Rust language plugin a
first-party 1.x capability, expands the MCP read surface, finishes the Weft store
cutover, and hardens plugin/analyze failure handling. No package is published for
release candidates. (Cargo SemVer `1.1.0-rc4`; Python wheels normalise to PEP
440 `1.1.0rc4`.)

### Added

- **First-party Rust language plugin.** The 1.1 line now includes
  `loomweave-plugin-rust` alongside the Python plugin. The Rust plugin extracts
  modules, structs, enums, traits, type aliases, consts, statics, macros, impls,
  and functions, and emits `contains`, `imports`, `implements`, `calls`,
  `derives`, and `references` edges. The distribution wheel installs a
  discovery-glob-named `loomweave-plugin-rust` executable and its manifest under
  `share/loomweave/plugins/rust/`, so colocated installs discover it
  automatically.
- **Rust-plugin hardening envelope (ADR-050).** The host now bounds plugin
  lifecycle phases with handshake, per-file, and shutdown deadlines, and the
  Rust plugin degrades hostile parse shapes instead of crashing the run:
  excessive nesting emits `LMWV-RUST-DEPTH-LIMIT`, oversized files emit
  `LMWV-RUST-FILE-TOO-LARGE`, and watchdog kills are no longer mislabeled as
  OOM. The Rust plugin's address-space envelope is raised to the empirically
  validated 512 MiB line for large real corpora.
- **Relation-edge MCP read surface.** `entity_relation_list` exposes
  `inherits_from`, `decorates`, `implements`, and `derives` edges directly, and
  `entity_neighborhood_get` / `entity_orientation_pack_get` now include
  direction-tagged relation buckets.
- **Entity resolution and findings surfaces.** `entity_resolve` resolves all
  entity kinds, SEI entries, Rust `::` paths, and plugin hints; the MCP surface
  also gained whole-project finding browsing, `has_findings` filtering, stricter
  finding-filter validation, and better unknown-kind hints.
- **Duplicate-locator alarm.** `loomweave analyze` now emits
  `LMWV-DUPLICATE-LOCATOR` at ERROR severity when two declarations collide on
  one entity id, surfacing data-loss risks that SQLite upserts previously
  absorbed silently.
- **Python relation edges.** The Python plugin now emits `inherits_from` and
  `decorates` edges under ontology 0.8.0, including pyright-backed tests for the
  new relation kinds.
- **Product ownership workspace.** `docs/product/` now records the current
  state, roadmap, metrics, and first product decisions for the 1.1 release line.

### Changed

- **Project store moved `.loomweave/` → `.weft/loomweave/` (Weft store
  consolidation, clean break; ADR-046).** All machine-written state — the index
  DB, `config.json`, `.gitignore`, `embeddings.db`, `ephemeral.port`,
  `instance_id`, locks, and per-run dirs — now lives under the shared
  `.weft/<member>/` dotdir, routed through a single `loomweave_core::store`
  helper. There is **no fallback read** of the old location: existing projects
  must re-init (`loomweave install` then `loomweave analyze`) and may delete the
  orphaned `.loomweave/`. An operator may relocate the store with a
  member-private `[loomweave].store_dir` key in a project-root `weft.toml`
  (read-only to Loomweave; a missing or malformed file falls back silently to
  the default). Sibling resolution reads the consolidated `.weft/<sibling>/`
  location **only** — Filigree's live port at `.weft/filigree/ephemeral.port`,
  Wardline's trust-vocabulary descriptor at `.weft/wardline/vocabulary.yaml` —
  with no fallback to the pre-consolidation `.<sibling>/` path. A sibling found
  only on the legacy path folds to the fail-soft default (`source = "config"`
  for Filigree, an absent descriptor for Wardline), making a mis-sequenced
  cutover visible rather than a silent stale resolve. **Cutover ordering:**
  Filigree migrates to `.weft/filigree/` → this build installs → downstream
  re-init.

- **`cargo nextest run --workspace` now always completes instead of hanging.** A
  `slow-timeout` cap in `.config/nextest.toml` terminates any test that runs past
  the bound and reports it as a timeout failure, so the literal CI test command
  runs clean to a verdict even while the pre-existing emission tests
  (clarion-1d405be546) hang. The cap does not fix that bug — it makes the suite
  honestly red and fast rather than green-via-family-exclusion.

- **Filigree federation token env var renamed to `WEFT_FEDERATION_TOKEN`.** The
  `integrations.filigree.token_env` default (and the name stamped into
  `loomweave.yaml` by `loomweave install`) is now `WEFT_FEDERATION_TOKEN` —
  Weft-suite federation plumbing is named by the suite, not by the sibling
  member. The legacy `FILIGREE_API_TOKEN` name is still honoured as a deprecated
  fallback at token-resolution time, so an existing global export keeps working
  during the transition. This does not affect `serve.http.token_env`
  (inbound HTTP read-API bearer auth, default `WEFT_TOKEN`).
- **MCP instructions now use the registered tool dialect.** The generated
  `loomweave-workflow` skill, MCP `initialize` instructions, and CI drift guard
  now name the registered `entity_*` tools rather than legacy aliases; the
  server also trims its tools/list context footprint while keeping instructions
  truncation-proof.
- **Dormant `wardline.yaml` manifest ingest retired.** The unused CLI ingest
  path was removed in favour of the durable Wardline descriptor and taint-fact
  paths already documented in the Weft contracts.
- **Rust qualname canonicalization amended.** ADR-049 now covers cfg-twin
  methods, concrete generic arguments, impl self-type and trait paths,
  `#[path]` module mounts, and unnamed `const _` skip-emission; the Rust plugin
  applies the same rules and updates the conformance fixture.
- **Release and install packaging moved toward colocated Python installs.** The
  `loomweave` Python package scaffold depends on both language plugins, and the
  Rust plugin has a separate maturin distribution shim so its discovery binary
  can be shipped without polluting normal workspace builds.

### Fixed

- **Rust plugin collision families closed.** The Sprint-4 residual collisions
  from self-type paths, trait paths, `#[path]` module mounts, and unnamed
  `const _` items were fixed and re-swept to zero on the pinned QA corpora.
- **Out-of-line Rust module trees now agree on parent/contains edges.** Host and
  plugin behaviour now align for mounted module trees and cfg-twin module
  shapes.
- **Python hostile nesting now degrades cleanly.** Deep-nesting bombs in the
  Python plugin produce a `too_complex` outcome instead of escaping
  `RecursionError`.
- **Duplicate Python qualnames are pinned to first-wins semantics.** ADR-052 and
  dogfood/audit tests freeze the expected handling of repeated Python qualnames.
- **Install no longer fails on symlinked instruction files.** Symlinked
  `AGENTS.md` / `CLAUDE.md` cases degrade to warnings so install can complete.
- **E2E scripts no longer mutate global Codex config.** The smoke harnesses now
  force hermetic config paths instead of editing `~/.codex/config.toml`.
- **Untrusted repository git config is no longer executed.** Source inspection
  and rename-window logic use hardened git invocations that do not execute
  repo-local config or attribute helpers.

### Docs

- Added the Rust analysis known-limitations page, Sprint-3 scale QA report,
  Sprint-4 gold QA report, gold-v2 addendum, Rust qualname federation handoff
  letters, and MCP/command-surface audit memo.
- Refreshed the design ladder, operator getting-started flow, operator
  guidance docs, ADR index, public static site, and Weft federation contracts
  for the 1.1 release-candidate surface.

## [1.1.0rc3] — 2026-06-06

Third 1.1 release candidate. Hardens the Python plugin's pyright spawn path
against transient resource pressure and corrects the plugin process-limit
sandbox. No package is published for release candidates. (Cargo SemVer
`1.1.0-rc3`; Python wheels normalise to PEP 440 `1.1.0rc3`.)

### Fixed

- **`loomweave doctor` now gates on tracked-index DB health (check
  `.weft/loomweave.schema`).** The check previously only tested for file
  existence; it now classifies four states: absent (warning — a legitimate
  install-before-analyze intermediate, does not fail the gate); present but
  unreadable / corrupt / wrong format (problem — fails the gate); present and
  opens but `PRAGMA user_version` exceeds this build's schema version (problem —
  reports the version numbers and names the newer-build cause); and healthy
  (ok). Both the JSON and text paths report consistently so CI gates driven by
  either surface see the same verdict. The check resolves the DB path via
  `loomweave_core::store::db_path` so a `weft.toml` `[loomweave].store_dir`
  override is honoured, and opens read-only so the health check never creates
  or mutates the file.

- **Transient pyright spawn failures no longer disable analysis for the whole
  run.** A `subprocess.Popen` failure with a transient errno
  (`EAGAIN`/`ENOMEM`/`EMFILE`/`ENFILE`) now skips only the current file and
  retries a fresh spawn on the next one, instead of being treated as a permanent
  install failure. A new `LMWV-PY-PYRIGHT-SPAWN-DEFERRED` finding is emitted once
  per pressure episode, and a resettable soft-cap emits
  `LMWV-PY-PYRIGHT-RESOURCE-EXHAUSTED` (giving up only under *sustained*
  pressure); genuine defects (`ENOENT`/`EACCES`) still disable as before. Closes
  the `[Errno 11] Resource temporarily unavailable` →
  `LMWV-PY-PYRIGHT-INSTALL-FAILURE` failure seen analysing large projects.

### Changed

- **`RLIMIT_NPROC` is no longer applied to language-server plugins.** Because
  `RLIMIT_NPROC` is enforced per real UID system-wide — not per plugin subtree —
  any fixed ceiling is tripped by the operator's unrelated processes and
  intermittently fails `pyright-langserver`'s `fork(2)` with `EAGAIN`. The host
  now leaves `RLIMIT_NPROC` uncapped for plugins declaring the `pyright` runtime
  capability (relying on `RLIMIT_AS` + crash-loop supervision) and retires the
  `PYRIGHT_MAX_NPROC = 4096` constant. cgroup v2 `pids.max` is documented as the
  future tool for true per-plugin process bounds (ADR-021, ADR-035).

## [1.1.0rc2] — 2026-06-06

Second 1.1 release candidate, rolling up dogfood-friction fixes and deferred
v1.1 engineering items on top of rc1. No package is published for release
candidates. (Cargo SemVer `1.1.0-rc2`; Python wheels normalise to PEP 440
`1.1.0rc2`.)

### Added

- **Worktree-aware staleness (ADR-045).** `project_status_get`, the
  `loomweave://context` resource, and the session-start banner now surface
  `indexed_at_commit` + `worktree_dirty`, and a new `Staleness::StaleWorktree`
  verdict fires when an otherwise-fresh index has untracked source on disk.
  Detection uses a hardened, hash-free `git ls-files --others` scoped to ingested
  source extensions (false-positive guard), proven filter-safe by test — closes
  the "fresh lies about uncommitted code" friction (clarion-26c7e52027,
  clarion-d9cf8bcfa9).

### Changed

- **`.loomweave/.gitignore` (ADR-005)** now also excludes `instance_id` and
  `*.lock`, so `git add -A` no longer stages the per-project serve fingerprint or
  the analyze advisory lock; ADR-005 documents the live-index commit hazard and
  points at `loomweave db backup` (clarion-7381e6382d).
- **WAL hygiene.** The storage writer-actor runs `PRAGMA wal_checkpoint(TRUNCATE)`
  after each committed run, so the on-disk `loomweave.db` reflects committed state
  while `serve` is alive instead of lagging behind a multi-MB WAL sidecar
  (clarion-cdee445ed8).
- **Release CI parity.** `release.yml` gains a macOS aarch64 `verify-macos` gate
  (mirroring `ci.yml`) wired into the build/publish `needs` chain, closing the
  gap where a macOS-only lint/test regression could reach the build jobs
  (clarion-47d395e03c).

### Removed

- **Dead `entity_fts.content_text` column** dropped via migration 0009 — it was
  never populated and never read (content search is served by the ADR-040
  embeddings sidecar). `CURRENT_SCHEMA_VERSION` is now 9 (clarion-716449c371).

### Docs

- macOS Gatekeeper quarantine workaround added to `getting-started.md`
  Troubleshooting (clarion-03dfa1f94d).
- ADR-024 in-place migration-retirement guard activated: `published_build.txt`
  backfilled to `v1.0.0` (first published build; 0001 byte-identical since), so
  later schema changes must be additive migrations (clarion-b20448b3ac).

## [1.1.0rc1] — 2026-06-06

First 1.1 release candidate. No package is published for release candidates —
the `1.1.0` package ships only at the final tag. (Cargo SemVer `1.1.0-rc1`;
the Python wheels normalise to PEP 440 `1.1.0rc1`.)

### Added

- **Read-API ephemeral port publication (ADR-044).** `loomweave serve` binds a
  per-project **deterministic** read-API port (blake3 over the canonical project
  path, band `9400–10399`, disjoint from Filigree's `8400–9399`) with an
  OS-assigned **ephemeral fallback** when that port is taken, and publishes the
  *actually bound* port to `.loomweave/ephemeral.port` — a normative cross-product
  file contract (port-only ASCII + optional trailing newline, atomic temp+rename,
  **loopback-only**, removed on clean shutdown). This resolves the cross-project
  `127.0.0.1:9111` bind collision so multiple projects can `serve` concurrently
  without mis-targeting one another. New consume-time resolver
  `resolve_loomweave_url` (precedence: explicit target > published file >
  configured URL > none) is the reference reader; `doctor` and
  `project_status_get` report the live published endpoint. The published file is
  git-ignored.
- **No-index degraded MCP mode.** `serve` on a project with no index no longer
  exits 1 — it serves a degraded MCP stdio session that answers `initialize` and
  chirps to run `loomweave install` + `loomweave analyze` from every tool call,
  so the MCP client connects and is told how to recover.

### Changed

- **`serve.http.bind` is now optional** (`Option<SocketAddr>`). Unset — the new
  default — auto-selects and publishes the per-project deterministic port; an
  explicit value is honoured verbatim (no fallback). The installer no longer
  stamps `serve.http.bind: 127.0.0.1:9111`, the integration bindings write the
  per-project deterministic loomweave URL, and `install`/`doctor --fix` self-heal
  the stale hard-coded `9111` stamp on existing projects.
- Version bumped to `1.1.0rc1` across the Rust workspace and the Python plugin.

## [1.0.0] — Loomweave — 2026-06-05

**This release renames the product and re-baselines its version.** What shipped
as **Clarion 1.3.0** is now **Loomweave 1.0.0**. The entries below this header
(`[1.3.0]` and earlier) are the pre-rename Clarion lineage, preserved verbatim
as history; they describe the same software under its former name.

This is a **clean break with no users yet** — there are no migration shims,
on-disk path auto-detection, dual-magic fallbacks, binary symlinks, plugin-prefix
fallbacks, or MCP server-name aliases. Existing local state (a `.clarion/` index)
is not migrated; re-run `loomweave analyze` to rebuild under the new paths.

### Changed — naming

- **Product `Clarion` → `Loomweave`** and **framework/suite `Loom` → `Weft`**.
  Hierarchy: the **Weft** framework comprises **Loomweave** (this product, the
  flagship code-archaeology tool), Filigree, Wardline, and Legis (+ Shuttle planned).
- **Binary** `clarion` → `loomweave`; **workspace crates** `clarion-{core,cli,mcp,
  storage,analysis,federation,scanner,plugin-fixture}` → `loomweave-*`.
- **Python plugin** package `clarion-plugin-python` → `loomweave-plugin-python`,
  module `clarion_plugin_python` → `loomweave_plugin_python`, shared-data path
  `share/clarion/plugins/` → `share/loomweave/plugins/`.
- **Persisted identity**: `.clarion/` → `.loomweave/`, `clarion.db` →
  `loomweave.db`, `clarion.yaml` → `loomweave.yaml`; the SQLite `application_id`
  magic `0x434C524E` (`"CLRN"`) → `0x4C4D5756` (`"LMWV"`).
- **MCP server identity** `clarion` → `loomweave`; the `clarion-workflow` prompt/
  skill → `loomweave-workflow`.
- **Environment variables** `CLARION_*` → `LOOMWEAVE_*`; the federation/suite
  variables `CLARION_LOOM_*` (e.g. `CLARION_LOOM_TOKEN`) → `WEFT_*` (e.g.
  `WEFT_TOKEN`) — they name the framework, not the product.
- **Error / diagnostic codes** prefix `CLA-` → `LMWV-` (e.g.
  `CLA-INFRA-STORAGE-FOREIGN-DB` → `LMWV-INFRA-STORAGE-FOREIGN-DB`); the
  ADR-022 plugin `rule_id_prefix` grammar core prefix `CLA-` → `LMWV-`.
- **Documentation/site** repository `github.com/tachyon-beep/clarion` →
  `github.com/foundryside-dev/loomweave`; docs domain `clarion.foundryside.dev`
  → `loomweave.foundryside.dev`.

### Changed — federation contract (cross-product; Filigree + Wardline move in lockstep)

These touch the wire contract shared with sibling products and require the peers
to be updated in step (the peer pins/clients are being renamed together):

- Federation HTTP read API routes `/api/loom/...` → `/api/weft/...`; identity
  headers `X-Loom-Component` / `X-Loom-Nonce` / `X-Loom-Timestamp` →
  `X-Weft-*`; the `X-Loom-Component` identity value `clarion:<hmac>` →
  `loomweave:<hmac>`. `api_version` stays `1` (a rename is not a wire-incompatible change).
- Federation JSON field names `loom_*` (e.g. `loom_files`, `loom_findings`,
  `loom_component`) → `weft_*`.
- The Filigree entity-association field `clarion_entity_id` → `loomweave_entity_id`.
- The Stable Entity Identifier (SEI) prefix `clarion:eid:` → `loomweave:eid:`
  (ADR-038 / the Weft SEI conformance standard).

### Version

- Recut `1.3.0` → `1.0.0` across the workspace `Cargo.toml`, every
  `pyproject.toml`/`plugin.toml`, and the Python plugin `__version__`.

## [1.3.0] — 2026-06-05

### Added

- **Python plugin consumes Wardline's NG-25 trust-vocabulary descriptor
  (ADR-018 Revision 3).** The Python plugin now reads Wardline's descriptor from
  `.wardline/vocabulary.yaml` or the installed `wardline/core/vocabulary.yaml`
  data file **without importing Wardline**. Functions and classes decorated with
  Wardline trust decorators (`external_boundary` / `trust_boundary` / `trusted`)
  receive `wardline` entity metadata and `wardline:*` tags when the descriptor is
  available; a missing, invalid, or version-skewed descriptor degrades honestly to
  normal structural extraction. This **fully retires** the Loomweave-side
  `wardline.core.registry` startup coupling (federation asterisk 2, registered at
  `~/loom/asterisk-register.md`): plugin startup performs zero in-process Wardline import, so the
  plugin no longer requires a co-installed Wardline and is robust to Wardline's
  upcoming native core. Plugin-only change (no Rust-core / protocol / ontology
  change); tracked at `clarion-881e9834bc`.

### Changed

- **Filigree issue lookups key by Stable Entity Identity (SEI).** The MCP
  `entity_issue_list` path and the federation Filigree client resolve issues by
  SEI rather than source locator, aligning issue enrichment with Loomweave's stable
  identity (one key per entity — SEI xor locator, no per-row fallback).
- Refreshed release-facing README / index documentation for the 1.3.0 release
  line, including the 39-tool MCP surface, current install artifact names, fixed
  ADR/docset links, current web/operator quick starts, and the full end-to-end
  verification list.
- Archived tracked architecture-analysis working notes out of live `temp/`
  directories under `docs/archive/working-notes/`.

### Removed

- **Release-governance gate script (`scripts/check-github-release-governance.py`).**
  Release-governance enforcement is handed off to Legis; `release.yml` no longer
  invokes the script and the standalone check is removed from the tree.
- **In-repo agent-instruction files (`CLAUDE.md`, `AGENTS.md`).** Agent
  conventions now derive from the `~/loom` federation hub plus local untracked
  copies; both files are removed from the tracked tree and gitignored.
- **Bundled Filigree-workflow skill (`.agents/skills/filigree-workflow/SKILL.md`).**
  Removed from the tracked tree alongside the agent-instruction files.

### Fixed

- **`loomweave doctor` reports enrich-only integration bindings as a warning, not a
  gate failure (federation-axiom compliance).** A missing or stale
  Loomweave+Filigree+Wardline binding previously mapped to `problem` (exit 1),
  which made an enrich-only sibling effectively required — contradicting
  `weft.md` §5. Both the JSON and text doctor paths now report `warning` for
  missing/stale bindings; unparseable bindings and `--fix` repair failures remain
  `problem`. A bare `loomweave doctor` on a no-bindings (Loomweave-solo or
  Loomweave+Filigree-only) project now exits 0 with the warning surfaced.

### Security

- **Closed a config-driven command-execution path from untrusted repository
  contents (`clarion-4b5a8aff54`).** `loomweave analyze` (the SEI git-rename
  signal) and `loomweave serve` (the `index_diff_get` freshness/drift report)
  shelled `git` inside the analyzed repository with repo-local configuration and
  Git attributes enabled, so a malicious repository could execute arbitrary
  commands as the local user during an ordinary analyze/serve — via
  `core.fsmonitor`, an external diff/textconv driver, or a `filter.<name>.clean`
  selected by a `filter` attribute. All corpus-facing `git` calls now route
  through a single hardened helper (`loomweave_core::hardened_git_command`) that
  ignores operator/global/system config, strips config/exec-injecting environment
  variables, overrides the program-naming repo-local keys via highest-precedence
  `-c` flags (including `core.fsmonitor=false` and `core.attributesFile=`), and
  neutralizes the attribute sources it can — the in-tree `.gitattributes` via
  `--attr-source=<empty-tree>` and the system file via `GIT_ATTR_NOSYSTEM`. The
  one source no config flag can disable, `$GIT_DIR/info/attributes`, only triggers
  a filter when Git hashes working-tree content, so the call sites no longer do
  that on an untrusted corpus: the rename signal uses `git diff --cached`
  (index-vs-HEAD; still catches staged `git mv` renames) and the freshness probe
  replaces `git status` with `git diff --cached` plus the existing stat-based
  per-file drift check. **Behavior change:** `index_diff`'s `dirty_files` now
  lists staged changes only — unstaged working-tree modifications and untracked
  files are no longer enumerated there (unstaged edits to indexed files still
  surface in `modified_since_analyze`). Signals remain best-effort and
  enrich-only. The `--attr-source` belt-and-suspenders is applied only when a
  one-time probe confirms Git >= 2.40; older Git omits it and stays both safe (the
  `--cached` call sites carry the actual protection) and fully functional, so no
  minimum-Git floor is introduced. Reported externally and re-verified with
  working PoCs across all attribute sources; relates to the untrusted-corpus
  posture of ADR-021.

## 1.2.0 planning history — 2026-06-03

### Added

- **Guidance maturity — WS6 (REQ-GUIDANCE-03/-05/-06; ADR-007, ADR-024).** The
  guidance system moves from schema baseline to operator-usable.
  - **`loomweave guidance` CLI** — `create` / `edit` (`$EDITOR`) / `show` / `list`
    / `delete` / `export` / `import`. Match-rule syntax (`path:` / `tag:` /
    `kind:` / `subsystem:` / `entity:`), scope-levels (project→function), and
    `--expires` normalisation to a full ISO-8601 instant. Sheets are written via a
    new non-run-scoped `loomweave-storage` guidance API, and the rule-matcher is
    lifted into `loomweave-storage` as the single source of truth shared by the CLI,
    `analyze`, and the MCP read path.
  - **Staleness findings (`analyze`).** `LMWV-FACT-GUIDANCE-ORPHAN` (WARN) now also
    fires for a `match_rules {entity:…}` rule pointing at a deleted entity (was
    `guides`-edge only); new `LMWV-FACT-GUIDANCE-EXPIRED` (INFO) and
    `LMWV-FACT-GUIDANCE-CHURN-STALE` (WARN, confidence 0.7 heuristic, asymmetric
    threshold 50 / 20-pinned). Surfaced via `loomweave guidance list --stale`
    (review-cadence age) / `--expired`. CHURN-STALE is honest-empty until
    `git_churn_count` population lands.
  - **Team import/export.** `export --to <dir>` / `import <dir>` — deterministic
    one-file-per-sheet sorted-key JSON, additive idempotent import, loud-fail on
    malformed input.
  - **Cache invalidation.** Authoring (create / edit / delete / import) eagerly
    invalidates the summary cache of matched entities (ADR-007 churn-eager
    invalidation).
  - Deferred with tracking issues: the agent-mediated propose→promote lifecycle
    (no observation-write transport), Wardline-derived generation, the in-browser
    staleness-review UI, and guidance composition into summary generation.
- **Git-rename provider seam now operative — WS9 / SEI §6 (REQ-C-05).** `analyze`
  drives the committed rename window so the `legis` `GitRenameSource` is actually
  consulted, closing the window gap previously surfaced in
  [`docs/federation/contracts.md`](docs/federation/contracts.md). Each run records
  the git `HEAD` it analyzed on the run row (migration `0007`, new nullable
  `runs.analyzed_at_commit`; schema version 6 → 7) and reads the *prior* run's
  commit to query renames over `<prior_commit>..HEAD`. The SEI mint pass now
  **unions** two complementary windows (`gather_git_renames`): the working tree
  (uncommitted renames, shell `git diff -M HEAD`, always) and the committed range
  (committed renames via `legis` when `--legis-url` is set and reachable, else a
  shell fallback). Enrich-only and never a regression — without `legis`, or with
  no prior commit, only the working-tree window runs (byte-identical to pre-WS9,
  no HTTP). The prior-commit read excludes the current run (which `CommitRun`
  marks `completed` before the mint pass), so the committed window never collapses
  to `<HEAD>..HEAD`. `analyze` also applies any pending migrations on startup
  (under the analyze lock) so a binary upgrade does not hard-fail on a DB that
  `install` has not re-touched.
- **Stable Entity Identity (SEI) — Wave 1 / WS1 (ADR-038).** Loomweave is now the
  suite's identity authority: it mints a durable, opaque **SEI**
  (`loomweave:eid:<blake3(locator ++ 0x00 ++ mint_run_id)[:32]>`) for every entity
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
    `loomweave:eid:` prefix (REQ-F-02). `_capabilities` advertises
    `sei: { supported: true, version: 1 }`.
  - The MCP tool surface carries the `sei` alongside every entity id (no MCP
    locator exception — REQ-C-04), via a read-time `sei_bindings` join.
  - The shared **SEI conformance oracle** (SEI standard §8) is authored and
    passes; the cross-tool hard-cutover backfill is documented in
    [`docs/federation/sei-migration-playbook.md`](docs/federation/sei-migration-playbook.md)
    and surfaced for owner-gated scheduling.
- **Incremental analysis — Wave 2 / T3.1.** `loomweave analyze` now skips files
  whose whole-file hash matches the prior run, reusing their entities (the entity
  graph is cumulative and edges are insert-or-ignore, so the skip is speed-only).
  `skipped_files` is reported in `stats.json` and a `skipped_unchanged` progress
  event is emitted; `--no-incremental` forces a full re-index. The skip is guarded
  against the SEI matcher: the current-locator set is the union of re-analysed and
  skipped-file entities, so an unchanged file's identities are never falsely
  orphaned, and skipped entries are re-fed into the prior-index rebuild so the
  snapshot does not decay. Files carrying a secret finding are never skipped (their
  finding anchor must stay stable).
- **Dossier participation surface — Wave 2 / WS4.** The exact Loomweave HTTP slices
  the cross-tool dossier *assembler* (Wardline) reads are pinned in
  [`docs/federation/contracts.md`](docs/federation/contracts.md) and specified in
  [`docs/superpowers/specs/2026-06-02-loomweave-dossier-participation.md`](docs/superpowers/specs/2026-06-02-loomweave-dossier-participation.md):
  identity (`resolve` → SEI + content-axis freshness; `resolve_sei`/`lineage` →
  identity-axis freshness), structural linkages (callers/callees), and file
  context. Loomweave contributes slices and the SEI join key; it does **not** proxy
  Filigree associations (read directly from Filigree's own ADR-029 endpoint) or
  assemble the envelope. Proven end-to-end against a renamed-function fixture
  (`serve_http_dossier_participation_surface_serves_a_renamed_function`).
- **`legis` governance consumption — Wave 3 / WS9 (governed paradise).** `legis`
  consumes Loomweave's stable identity as an **opt-in** governance layer a solo
  project never sees; core paradise (Wave 2) does not depend on it.
  - **Git-rename provider seam (REQ-C-05).** A second `GitRenameSource`,
    `LegisGitRenameSource`, reads `legis`'s `GET /git/renames` over HTTP and feeds
    the same file→locator translation as `ShellGitRenameSource` — `legis` supplies
    the git signal with no matcher change (SEI spec §6). Selection
    (`select_git_rename_source`, `--legis-url`) is enrich-only and
    capability-aware: the shell source remains the default and fallback; an
    unset/unreachable `legis` issues no HTTP and is byte-identical to before. The
    two suppliers observe different rename windows (Loomweave's `analyze` depends on
    the working-tree window; `legis` serves only committed rev-ranges), so the
    seam is built/tested/ready but inert in the default pipeline until `legis`
    adds a working-tree surface or Loomweave drives a committed re-index — a gap
    surfaced (not papered) in [`docs/federation/contracts.md`](docs/federation/contracts.md).
    The matcher is fail-closed regardless, so neither window can cause a false
    carry. Proven by
    `selector_keeps_working_tree_rename_even_when_a_reachable_legis_sees_nothing`.
  - **Audit-spine consumption.** `legis` reads Loomweave's existing
    `resolve`/`resolve_sei`/`lineage` routes as its governance audit spine; the
    consumption contract is pinned in `docs/federation/contracts.md`. Per REQ-L-01
    (Option 3) `legis` owns integrity at its own boundary (snapshot-hash over
    polled lineage) — Loomweave ships **no** lineage hash-chain or signature.
  - **No trust adjudication.** Loomweave carries the trust vocabulary verbatim and
    adds no policy/attestation engine — Wardline analyses, `legis` governs,
    attestations key on Loomweave's SEI.

### Fixed

- **`guidance_for` no longer drops expiry-bearing sheets in production.** The MCP
  read path compared a sheet's ISO `expires` lexically against the server clock,
  whose production default is a `unix:<seconds>` string — so every sheet carrying
  any `expires` sorted as "expired" and was silently excluded from composition.
  The comparison now parses both forms to seconds (fail-open on unparseable
  input), guarded by a regression test that runs under the production clock
  (clarion-3153e74f0b).

## [1.1.0] — 2026-05-31

### Added

- **Wardline taint-fact store (ADR-036).** Loomweave now serves as the persistent
  read+write store for Wardline's taint facts over HTTP, keyed to Loomweave entity
  qualnames. New migration `0003` adds the `wardline_taint_facts` table; facts
  are written through the storage writer-actor
  (`WriterCmd::UpsertWardlineTaintFact`) and resolved/fetched via the new
  `wardline_taint` storage module. Routes:
  - `POST /api/wardline/resolve` — exact-tier qualname → entity resolution.
  - `POST /api/wardline/taint-facts` — exact-only batch write; the Wardline
    payload is stored byte-verbatim (`serde_json::value::RawValue`) so Loomweave
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
  (`GET /api/weft/files` → `GET /api/weft/findings`) resolves a path to its
  findings. Per the enrich-only axiom, any unreachable hop degrades the section
  to `result_kind: "unavailable"` rather than failing the tool.
- **`loomweave doctor [--fix]`.** A new subcommand that verifies — and with
  `--fix` repairs in place — the installed agent-orientation surfaces: the
  `loomweave-workflow` skill pack, the `SessionStart` hook in
  `.claude/settings.json`, and the `loomweave` entry in `.mcp.json` (which
  `loomweave install` does not register automatically). It prints a per-surface
  ✓/✗ report plus the index snapshot and exits non-zero when any problem
  remains, so it is usable as a CI / pre-commit gate. Repairs reuse the same
  idempotent installers as `loomweave install`, and the `.mcp.json` merge is
  never-clobber: sibling servers and a deliberately customised `command` are
  preserved (only the `--path` args are corrected).

### Changed

- Federation contracts pinned/clarified: the Wardline taint-store routes +
  freshness contract, the two consumed weft routes for Flow B, Loomweave→Filigree
  ephemeral-port endpoint discovery, and the `scan_run_id` contract (stale
  Phase-0 handshake references removed).
- `docs/suite/weft.md`: added a written retirement condition to the §5
  asterisk-2 (Wardline→Filigree pipeline coupling), and refreshed the §9 status
  table to the post-1.0 state.
- ADR-036 accepted (Loomweave as Wardline taint-fact store), documenting the
  in-process two-writer concurrency posture.
- The Python round-trip self-test now hard-fails (instead of silently skipping)
  when the installed `loomweave-plugin-python` entry point is missing, so a broken
  editable install cannot pass CI green.
- **Shared error vocabulary (ADR-037).** New `loomweave-core::errors` module is now
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

- `loomweave db backup <output>` — a consistent, WAL-safe online backup of
  `.loomweave/loomweave.db` via `rusqlite::backup::Backup`. Safe to run during a
  live `loomweave analyze` (captures outstanding WAL frames into a standalone
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
  Loomweave believes an edge exists. Statically-unbindable calls are returned in
  a separate `unresolved_sites` list, never mixed with resolved evidence.
  Filterable by edge kind and a documented best-effort production/test path
  heuristic. No LLM call. The MCP surface now exposes thirteen tools
  (clarion-9392f74881).
- **Filigree finding emission (WP9-B core, REQ-FINDING-03).** `loomweave analyze`
  Phase 8 POSTs the run's persisted findings to Filigree's
  `POST /api/v1/scan-results` intake, with Loomweave's richer fields nested under
  `metadata.loomweave.*` (wire contract pinned in
  [`docs/federation/contracts.md`](docs/federation/contracts.md)). Emission is
  enrich-only — gated behind `integrations.filigree.{enabled,emit_findings}`
  (now **both default `false`**, so enabling Filigree for `issues_for` reads
  never silently starts outbound emission — clarion-a26de2f368), and any
  Filigree-side failure is recorded in `stats.json`
  (`LMWV-INFRA-FILIGREE-UNREACHABLE`) instead of failing the run. Findings
  anchored to a `briefing_blocked` entity are excluded, matching the fail-closed
  read posture (clarion-8b32ba0d02). Resolves the 1.0 "finding emission
  deferred" limitation below.
- **`loomweave analyze --resume RUN_ID` (WP9-B, REQ-FINDING-05).** Reopens a prior
  run's `runs` row (a new `WriterCmd::ResumeRun` `UPDATE`s it back to `running`
  instead of `INSERT`ing, which conflicted on the existing run PK), re-walks the
  tree, and emits findings to Filigree with `mark_unseen=false` so the re-emit
  does not flip the prior run's findings to `unseen_in_latest`. The re-walk is
  idempotent: entities and run-scoped findings now both UPSERT on `id`
  (previously only entities did), so a resumed run reproduces the same durable
  graph as the original. The emitted `mark_unseen` value is recorded in
  `stats.json`. Tracked under clarion-dd29e69e0e.
- **`loomweave analyze --prune-unseen` (WP9-B, REQ-FINDING-06).** After emission
  (Phase 8b), POSTs Filigree's `POST /api/weft/findings/clean-stale` retention
  route, scoped to `scan_source=loomweave`, asking it to soft-archive its own
  `unseen_in_latest` Loomweave findings older than
  `integrations.filigree.prune_unseen_days` (default 30). Soft-archive, not
  delete: Filigree moves them to `fixed` and auto-reopens on reappearance
  (Filigree ADR-015). Enrich-only — a Filigree outage or the integration being
  disabled is recorded in `stats.json` (`filigree_prune`) and never fails the
  run; the `scan_source` scoping is enforced server-side so the sweep can only
  touch Loomweave's findings. (Closes the REQ-FINDING-06 piece of
  clarion-dd29e69e0e; the route was found to already exist in Filigree, so the
  earlier "file a request" memo is superseded — see its withdrawal banner.) The
  remaining Phase-0 scan-run-create handshake is an open contract question with
  Filigree (Loomweave relies on the peer tolerating an unknown `scan_run_id`),
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
  (via a new `loomweave_storage::retry::begin_immediate` helper with bounded
  `SQLITE_BUSY`/`SQLITE_LOCKED` retry + exponential backoff) instead of a
  deferred `BEGIN`. Taking the write lock up front resolves cross-process
  write contention at lock-acquire — where `busy_timeout` is honored — rather
  than failing mid-statement on a deferred-lock upgrade the busy handler
  cannot serve. Closes gap-register STO-05 (clarion-bbb3365920).

### Fixed

- **macOS release build under `-D warnings`.** The Linux-only `prlimit`
  helpers imported/defined in `loomweave-core::plugin::host` (and two Linux-only
  test sites in `loomweave-mcp` and `loomweave-cli`) were unused on
  `*-apple-darwin`, so the release build failed with unused-import / dead-code
  errors. They are now `cfg`-gated to `target_os = "linux"` (plus `test` where
  unit tests reference them). CI gained a native macOS (aarch64) build + clippy
  leg so the gap can't recur; the x86_64 (macos-13) leg is temporarily parked
  while those runners are offline (clarion-12667da9f5, clarion-ec389a8e72).

## [1.0.0] — 2026-05-19

First publishable release. Loomweave ships as a Rust core (`loomweave` binary, five
workspace crates) plus an editable-install Python language plugin
(`loomweave-plugin-python`). Released under the [MIT License](LICENSE).

Targets the `v1.0.0` tag (cut by the operator once all release blockers are
green); supersedes the pre-release `v0.1-sprint-1` and `v0.1-sprint-2`
working tags, which remain in the repo as historical anchors.

### Core

- `loomweave install --path` initialises a project's `.loomweave/` directory
  (instance ID, SQLite DB, migrations).
- `loomweave analyze` walks a Python corpus and persists the structural graph
  (entities + `contains`, `calls`, `references`, `imports` edges) to a local
  SQLite store via the writer-actor pattern (ADR-011).
- `loomweave serve` exposes the MCP stdio surface for consult-mode agents:
  `entity_at`, `find_entity`, `callers_of`, `execution_paths_from`,
  `summary`, `issues_for`, `neighborhood`, `subsystem_members`.

### Python plugin (`loomweave-plugin-python` 1.0.0)

- Pyright-backed entity extraction for functions, classes, and modules; resolved
  / ambiguous / inferred call edges per ADR-022.
- `wardline` runtime probe with version-range pinning (`>=1.0.0,<2.0.0`).
- Module-level `imports` candidate edges (Phase 3 Task 3).
- Strict-typed L4 JSON-RPC handshake with declared `entity_kinds`,
  `edge_kinds`, `ontology_version`, and `rule_id_prefix` (ADR-022).

### Federation HTTP read API (ADR-014)

The publisher-side of Loomweave's federation contract with Filigree's
`LoomweaveRegistry`. Pinned in [`docs/federation/contracts.md`](docs/federation/contracts.md);
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
  `WEFT_TOKEN`) names the env var holding the inbound bearer
  token. Loopback-without-token stays unauthenticated for the v0.1 trust
  model; non-loopback-without-token is refused at startup with
  `LMWV-CONFIG-HTTP-NO-AUTH`.
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
  `release:v1.1`).** Cross-product POSTing of Loomweave-generated findings into
  Filigree's intake (WP9-B) is deferred per the [Sprint 2 scope amendment](docs/implementation/sprint-2/scope-amendment-2026-05.md).
  `issues_for(id)` (the WP9-A binding for reading from Filigree) ships in 1.0.
  *(WP9-B core shipped post-1.0 — see the Filigree-finding-emission entry under
  [1.5.0]; only the REQ-FINDING-05/-06 lifecycle tail remains deferred.)*
- **HTTP file language inference** uses persisted plugin manifest language when
  available, with a narrow core-extension fallback for files that predate
  manifest capture.
- **Cooperative HMAC inbound auth** ships for the HTTP read API via
  `serve.http.identity_token_env` and `X-Weft-Component: loomweave:<hmac>`.
  The older bearer-token path remains available for compatibility.
- **Python plugin imports `wardline.core.registry.REGISTRY` at startup**
  (weft.md §5 asterisk 2). Initialization coupling scoped to the
  Wardline-aware plugin only; Loomweave core and non-Wardline-aware plugins are
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
  the field to a typed column). A v1.0 binary opening a `.loomweave/loomweave.db`
  produced by a pre-WP5 binary finds no `briefing_blocked` properties —
  pre-WP5 analyzers never wrote them — and will serve the entire catalogue
  without refusal. Operators upgrading from a pre-WP5 install MUST run
  `loomweave analyze` (scanner active by default) against the project root
  before exposing the HTTP read API or calling the `summary` MCP tool. See
  [`docs/operator/secret-scanning.md`](docs/operator/secret-scanning.md#pre-wp5-catalogue-upgrade-requirement).

### Documentation

- Design ladder under [`docs/loomweave/1.0/`](docs/loomweave/1.0/) — `requirements.md`,
  `system-design.md`, `detailed-design.md`.
- ADRs under [`docs/loomweave/adr/`](docs/loomweave/adr/) — 28 Accepted at 1.0
  (ADR-001…ADR-034 with the documented Backlog/Superseded subset excluded).
  ADR-012 is superseded by ADR-014, whose Security
  Posture and Error Envelope are in turn partially extended by ADR-034
  for the Sprint 3 federation hardening. Four ADRs (ADR-009, ADR-010,
  ADR-019, ADR-020) remain Backlog and are tracked inside
  `system-design.md` §12 / `detailed-design.md` §11 until promoted.
- Weft-suite doctrine at [`docs/suite/weft.md`](docs/suite/weft.md).
- Federation contract surface at [`docs/federation/contracts.md`](docs/federation/contracts.md).
- Operator guides under [`docs/operator/`](docs/operator/) — getting-started,
  OpenRouter setup, HTTP read API.

[Unreleased]: https://github.com/foundryside-dev/loomweave/compare/v1.5.0...HEAD
[1.5.0]: https://github.com/foundryside-dev/loomweave/compare/v1.4.0...v1.5.0
[1.2.1]: https://github.com/foundryside-dev/loomweave/compare/v1.2.0...v1.2.1
[1.2.0]: https://github.com/foundryside-dev/loomweave/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/foundryside-dev/loomweave/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/foundryside-dev/loomweave/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/foundryside-dev/loomweave/releases/tag/v1.0.0
