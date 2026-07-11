# Loomweave Federation Seam Follow-ups Implementation Plan

> **Execution:** Use `superpowers:subagent-driven-development`; each code task receives an implementer, a specification review, and a quality review. Use `superpowers:test-driven-development` for every behavior change and `superpowers:verification-before-completion` before integration.

**Goal:** Close the Loomweave-owned producer seams identified by the 2026-07-12 Plainweave audit: authoritative classifier support/completeness, a supported external SQLite read boundary, a project-safe identity/auth handshake, producer-owned normative goldens, and actionable doctor diagnostics.

**Boundaries:** Modify only `/home/john/loomweave`. `/home/john/plainweave` is read-only and receives exact fixture bytes plus a handoff. Preserve briefing-block and secret-scanning behavior. Optional peer absence is non-fatal; malformed or failed local production is never reported clean. Work stays attached to `clarion-b5c50abb19`.

**Starting point:** `main@f158f167`, product version `1.4.2`, schema `PRAGMA user_version=12`, HTTP `api_version=1`. The affected baseline is green: 86 focused schema/doctor/capabilities/HMAC/catalogue tests passed.

## Contract decisions

1. `runs.stats.classifier_coverage` is versioned as `loomweave.classifier-coverage.v1`. It records source/plugin-discovery completeness and one entry for every discovered plugin, including wire statuses `not-applicable`, `complete`, `degraded`, and `failed`. A plugin with zero matching files is not active and cannot establish supported-zero. Coverage arrays, strings, counts, tag grammar, and cross-field invariants are bounded and validated fail-closed. Any discovery error makes `plugin_discovery_complete=false` and `source_walk_complete=false`, because the extension universe for the walk is then incomplete.
2. Tag-backed MCP catalogue responses carry `loomweave.classification.v1`. `state` reports declaration support independently of health: a degraded supporting plugin remains `state=supported, complete=false`. `known_tags` remains observed-cardinality evidence only. Completeness fails closed on missing/malformed/non-completed coverage, source-walk gaps, relevant plugin degradation/failure, nonzero page offsets, page/scope/scan truncation, and mixed active-plugin support.
3. The supported external SQLite surface is schema `loomweave.external-sqlite.v1`, application IDs legacy `0` or `0x4c4d5756`, and a deliberately frozen `PRAGMA user_version` range `5..=12` (not an alias of the internal current version). The safe read-only projection is `runs(id, started_at, completed_at, stats, status)`, `entities(id, plugin_id, kind, source_file_path, source_byte_start, source_byte_end, source_line_start, source_line_end, properties, content_hash)`, `entity_tags(entity_id, plugin_id, tag)`, `sei_bindings(sei, current_locator, body_hash, status)`, and the Plainweave-consumed `sei_lineage(sei, event, old_locator, new_locator, run_id, recorded_at)`. Version 12 is current, 5–11 are older-supported, and 0, 1–4, foreign application IDs, structurally missing columns, and future versions are incompatible. Consumers classify PRAGMAs before any catalogue SQL and never run Loomweave migrations. Legacy application ID 0 plus a matching structure establishes compatibility, not database authenticity.
4. HTTP `api_version` stays `1` because auth discovery and response ownership are additive. `_capabilities` adds exactly `authentication: {"protected_routes":"none|bearer|hmac","capabilities_probe":"none","contract_version":1}`—never env-var names, credentials, signatures, nonces, or timestamps. Every identity-resolution success body, including not-alive and batch envelopes, carries `api_version` and `instance_id` from the serving state so ownership is verifiable in the same response and a reused-port A→B→A switch cannot pass through an unowned identity result. A consumer compares capability and identity-response ownership with the local `.weft/loomweave/instance_id` before joining remote identity to local catalogue rows.
5. Loomweave is fixture authority. Committed JSON goldens are stable, byte-pinned, producer-rechecked, and generated from real production serializers/handlers. The classifier golden is regenerated from an actual Python analysis fixture, with explicitly normalized volatile fields.

## Task 1: Declare classifier capabilities and shared coverage types

**Files:**
- Create `crates/loomweave-core/src/classifier_coverage.rs`
- Modify `crates/loomweave-core/src/lib.rs`
- Modify `crates/loomweave-core/src/plugin/manifest.rs`
- Modify `plugins/python/plugin.toml`
- Modify `plugins/python/src/loomweave_plugin_python/server.py`
- Modify `plugins/python/tests/test_server.py`
- Modify `crates/loomweave-plugin-rust/plugin.toml`
- Modify `crates/loomweave-plugin-rust/src/serve.rs`
- Modify `packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml`
- Modify manifest/package tests in core, Python, and Rust plugin crates

1. Add failing manifest tests for sorted/deduplicated classifier tags using `[a-z][a-z0-9]*(?:-[a-z0-9]+)*`, bounded counts/string lengths, legacy default-empty behavior, and package-copy parity.
2. Add `ClassifierCoverage`, `PluginClassifierCoverage`, and closed `PluginCoverageStatus` types with strict serde validation and schema constants.
3. Declare the exact emitted classifier tags for Python and Rust. Bump each affected plugin ontology version so unchanged files re-dispatch, including Python server handshake/test lockstep and all Rust packaged manifest copies.
4. Run core manifest tests, Python package/server tests, `scripts/check-python-ontology-version.py`, Rust initialize-response tests, and Rust manifest parity tests; commit.

## Task 2: Persist authoritative per-run classifier coverage

**Files:**
- Create `crates/loomweave-cli/src/analyze/classifier_coverage.rs`
- Modify `crates/loomweave-cli/src/analyze.rs`
- Modify `crates/loomweave-storage/src/commands.rs`
- Modify `crates/loomweave-storage/src/writer.rs`
- Modify `crates/loomweave-cli/tests/analyze.rs`
- Modify focused failure-mode tests where needed

1. Add failing integration tests for a complete plugin, zero matching files (`not-applicable`), syntax-degraded files, source-walk errors, partial plugin-discovery failure, plugin crash/failure, hard failure after run open, multiple plugins, incremental skips, and ontology/classifier-tag schema changes.
2. Create one coverage accumulator per discovered plugin before extension filtering. Record matched, completed-file-batch analyzed, retained/skipped, degraded file counts, declared tags, plugin/ontology versions, and terminal status. Scheduled files do not count as analyzed until their `PluginBatchMessage::File` completes.
3. Persist coverage on every control-flow branch in both completed and failed terminal run stats: no matching files, all retained, clean, syntax-degraded, host-rejected enumeration evidence, partial output then failure, and crash-loop-skipped plugins. Capture degraded source paths before syntax-fallback suppression. Extend `WriterCmd::FailRun` to accept validated stats instead of replacing them with only `failure_reason`, and route every post-`open_run` error through one terminalization path so no running row is abandoned without coverage.
4. Record bounded discovery-error counts/samples. `plugin_discovery_complete` is false when any manifest/executable discovery failed; `source_walk_complete` is true only when discovery was clean and `source_walk_skipped_entries == 0`, ensuring existing consumers also fail closed.
5. Enforce classifier-tag declaration and ontology-version lockstep in plugin/package tests; the existing persisted ontology marker then forces full redispatch whenever the declared classifier vocabulary changes.
6. Run targeted analyze and writer terminalization tests; commit.

## Task 3: Read coverage fail-closed and attach MCP classification

**Files:**
- Create `crates/loomweave-storage/src/classifier_coverage.rs`
- Modify `crates/loomweave-storage/src/lib.rs`
- Create `crates/loomweave-mcp/src/catalogue/classification.rs`
- Modify `crates/loomweave-mcp/src/catalogue/mod.rs`
- Modify `crates/loomweave-mcp/src/catalogue/faceted.rs`
- Modify `crates/loomweave-mcp/tests/catalogue_tools.rs`
- Modify tool descriptions/tests in `crates/loomweave-mcp/src/lib.rs`

1. Add storage tests for no run, running/failed/skipped latest run, malformed stats JSON, missing metadata, wrong schema, duplicate plugin IDs/tags, impossible counts/statuses, oversized arrays/strings, contradictory walk flags, and valid coverage.
2. Implement `latest_classifier_coverage` without falling back to older completed runs; order deterministically by `started_at DESC, id DESC`. A latest `running`, `failed`, or `skipped_no_plugins` run is `unavailable` regardless of embedded coverage.
3. Add pure classification tests for supported-zero/nonzero, mixed-plugin partial, unsupported active plugins, only-not-applicable plugins, degraded/failed supporting plugins, source-walk failure, and every enumeration truncation flag.
4. Attach classification to all tag-backed shortcuts and `entity_tag_list`. State is `supported`, `partial`, `unsupported`, or `unavailable`; `complete=true` only for all-active-plugin support with complete current coverage, `page.offset=0`, `page.returned=page.total`, and no page/scope/scan truncation.
5. Make `signal.available` derive from support, and `signal.complete` derive from the classification. Preserve `known_tags` only as observed evidence.
6. Run storage and MCP focused tests; commit.

## Task 4: Publish and type the external SQLite compatibility boundary

**Files:**
- Create `crates/loomweave-storage/src/external_sqlite.rs`
- Modify `crates/loomweave-storage/src/lib.rs`
- Modify `crates/loomweave-storage/src/schema.rs`
- Add storage integration tests
- Modify `docs/federation/contracts.md`
- Create `docs/loomweave/adr/ADR-055-external-sqlite-federation-read-contract.md`

1. Add failing tests for legacy/current/foreign `application_id`, `user_version=0`, too-old v4, minimum v5, older-supported v5–11, current v12, future v13, negative/out-of-range, every required safe table/column (including `sei_lineage`), and read-only connection behavior.
2. Add a typed serializable compatibility report with `Compatible`, `OlderSupported`, and `Incompatible` states, closed reason codes, found application/user versions, schema name, and frozen min/max constants. Classification performs only PRAGMA/schema introspection and never catalogue SQL or migrations.
3. Validate the declared safe projection's required tables/columns after the version check; structural drift is incompatible even when the integer is in range.
4. Document the exact check order, application-ID policy, read-only URI/open posture, safe columns, forbidden internal tables, JSON subcontracts, and migration ownership. Add a safe-surface schema snapshot so published migrations cannot silently drift the external projection.
5. Run storage schema/compatibility tests; commit.

## Task 5: Make identity and authentication discovery load-bearing

**Files:**
- Modify `crates/loomweave-cli/src/http_read.rs`
- Modify `crates/loomweave-cli/src/http_read/auth.rs`
- Modify `crates/loomweave-cli/src/http_read/identity.rs`
- Modify `crates/loomweave-cli/tests/serve.rs`
- Modify `docs/federation/contracts.md`
- Create `docs/loomweave/adr/ADR-056-identity-response-ownership-and-auth-discovery.md`

1. Add failing capability tests for unauthenticated loopback, bearer, and HMAC modes against the exact authentication object above. The response must never reveal env-var names/values or request credentials.
2. Add deterministic canonical-signing vector tests covering uppercase wire method normalization, exact path+query, SHA-256 body digest, timestamp, nonce, lowercase HMAC, the inclusive five-minute window, and poisoned replay-cache fail-closed behavior.
3. Add golden-backed success/failure tests for bearer and HMAC: missing, malformed, wrong, stale, replayed, and valid credentials. All failures use the same redacted `401/UNAUTHENTICATED` body.
4. Add response tests proving every identity route/body (single locator, SEI alive/not-alive, lineage, and batch) carries the serving `api_version` and `instance_id`; prove two roots emit distinct ownership and that the response itself exposes a mismatch before any local join.
5. Add a producer-owned reference handshake validator/helper that checks capability ownership and identity-response ownership without catalogue SQL. Document that Plainweave owns enforcement of the local join order, and validate that enforcement read-only in Task 9.
6. Document exact canonical client configuration: server keys `serve.http.identity_token_env` / `serve.http.token_env`; consumer pointer envs `WEFT_LOOMWEAVE_IDENTITY_TOKEN_ENV` / `WEFT_LOOMWEAVE_TOKEN_ENV`; defaults `WEFT_IDENTITY_SECRET` / `WEFT_TOKEN`; HMAC precedence; exact `Authorization`, `X-Weft-Component`, `X-Weft-Timestamp`, and `X-Weft-Nonce` headers; signing bytes, failure codes, redaction, and ownership comparison order.
7. Run HTTP/auth/serve tests; run Wardline because request headers and config are trust-boundary inputs; commit.

## Task 6: Add producer-generated normative federation goldens

**Files:**
- Add a small Python fixture project under `tests/fixtures/federation_classifier_python/`
- Create `scripts/generate-federation-seam-goldens.sh` or an equivalent repository-native generator
- Create `crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json`
- Create `crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json.sha256`
- Create `docs/federation/fixtures/classification.python.json`
- Update `docs/federation/fixtures/get-api-v1-capabilities.json`
- Create `docs/federation/fixtures/loomweave-http-auth-v1.golden.json`
- Create `docs/federation/fixtures/loomweave-http-auth-v1.golden.json.sha256`
- Create `docs/federation/fixtures/external-sqlite-compatibility-v1.json`
- Create `docs/federation/fixtures/identity-ownership-v1.golden.json`
- Create digest sidecars or hard-coded byte pins for classification, capabilities, external-SQLite, and identity-ownership fixtures
- Create `scripts/validate-plainweave-classifier-golden.py`
- Add producer recheck/byte-pin tests in the owning crates

1. Build the workspace CLI/Rust plugin, sync the Python plugin environment, and use explicit worktree artifact paths—never the ambient uv-installed `loomweave`. Record artifact paths and versions in generator metadata.
2. Build a fixture with exactly five CLI commands, five entry points, zero HTTP routes, and zero explicit exports.
3. Run the real Python plugin through the explicit built CLI; extract the latest persisted coverage and obtain all four real MCP tag responses. Publish the coverage at the exact paths already consumed by Plainweave's producer-golden gate. Normalize only run UUID/timestamps/absolute temp root, documenting every normalization.
4. Generate stable pretty JSON with trailing newline. Byte-pin every fixture and prove a one-byte mutation fails.
5. Producer-recheck classification, compatibility, capabilities, identity ownership, canonical HMAC vectors, and auth error bodies through the real serializers/handlers, not by comparing a fixture to itself. Factor time validation behind a helper accepting `now` so deterministic vectors exercise production logic.
6. Add a read-only compatibility harness that loads the exact coverage bytes into a temporary compatible SQLite catalogue and invokes Plainweave's real `LoomweaveAdapter` parser/list path from `/home/john/plainweave/src`; it must assert supported-complete zero-route behavior. Set `PYTHONDONTWRITEBYTECODE=1`, use external temp/cache paths, and verify Plainweave's pre/post git status is byte-identical.
7. Add an authority handoff note naming the exact paths, hashes, regeneration command, normalization rules, and Plainweave ticket `plainweave-f8303b4b50`.
8. Run the generator twice and assert byte-identical output; run all golden contract tests and the read-only Plainweave parser harness; commit.

## Task 7: Extend doctor diagnostics

**Files:**
- Modify `crates/loomweave-cli/src/doctor.rs`
- Modify `crates/loomweave-cli/tests/doctor.rs`
- Modify operator documentation

1. Add failing text/JSON doctor tests for: no DB (optional/missing and no file creation), legacy application ID, compatible older DB with actual version, foreign/incompatible DB, completed run without classifier metadata, malformed metadata, incomplete source walk, failed/degraded/not-applicable plugin, explicit empty classifier tags, healthy complete metadata, bearer/HMAC/none posture, malformed YAML, malformed/missing instance ID, and configured-but-missing auth secret.
2. Reuse storage compatibility and classifier readers; do not duplicate parsing rules in doctor.
3. Emit stable check IDs and machine-readable details. Missing optional DB/HTTP peer is warning/info; malformed, incompatible, or failed production is a gating problem.
4. Report active plugin classifier tags from the latest coverage, not merely installed manifests, and report enumeration completeness separately from tag support. Rework the SEI population check to use a read-only open so doctor never creates a missing DB.
5. Run doctor unit/integration tests; commit.

## Task 8: Documentation, live proof, and Plainweave handoff

**Files:**
- Modify `docs/operator/language-support.md`
- Modify `crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md`
- Modify `docs/federation/contracts.md`
- Create `docs/implementation/handoffs/2026-07-12-plainweave-federation-seam-fixtures.md`
- Modify `CHANGELOG.md`

1. Document supported-zero, partial/unsupported/unavailable, all truncation rules, safe SQLite reads, instance matching, auth discovery, and golden authority.
2. Record exact fixture hashes and a copy/validation recipe for Plainweave; do not edit Plainweave.
3. Build binaries and sync the Python plugin environment. Capture the fixture's pre-analysis latest-run JSON (or explicit no-run state), analyze it with explicit built artifacts while preserving before/after git status, then capture post-analysis latest-run JSON and a normalized diff artifact.
4. Capture all four MCP responses. Required proof: `5/5/0/0`, all four supported and complete, no page/scope/scan/source-walk/discovery truncation.
5. Capture `loomweave doctor --json` and human output for the healthy fixture.
6. Update the Unreleased changelog; commit.

## Task 9: Verification and independent seam sign-off

1. Run focused suites for core manifests, analyze coverage/failures, storage compatibility, MCP classification, HTTP identity/auth, goldens, doctor, and Python/Rust plugin packaging.
2. Run canonical gates:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
uv sync --project plugins/python --locked --extra dev
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
bash tests/e2e/sprint_1_walking_skeleton.sh
bash tests/e2e/sprint_2_mcp_surface.sh
bash tests/e2e/phase3_subsystems.sh
wardline scan . --fail-on ERROR
```

3. Run the read-only Plainweave compatibility commands explicitly:

```bash
PLAINWEAVE_STATUS_BEFORE=$(git -C /home/john/plainweave status --porcelain=v1)
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=/home/john/plainweave/src \
  /home/john/plainweave/.venv/bin/python scripts/validate-plainweave-classifier-golden.py \
  --plainweave-root /home/john/plainweave
cd /home/john/plainweave && PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=src .venv/bin/pytest -q \
  -p no:cacheprovider --basetemp=/tmp/plainweave-loomweave-seam-pytest \
  tests/test_loomweave_adapter.py tests/state/test_trace_links.py \
  -k 'classifier or hmac or instance or schema or peer_contract'
test "$(git -C /home/john/plainweave status --porcelain=v1)" = "$PLAINWEAVE_STATUS_BEFORE"
```

These exercise the live consumer without writing to Plainweave. Its not-yet-landed vendored-file oracle remains external ticket `plainweave-f8303b4b50`; do not claim that repository-owned oracle passed until it exists.
4. Assign a fresh subagent, uninvolved in implementation, to crawl the entire Loomweave↔Plainweave seam. Plainweave remains read-only. Require it to validate every acceptance item, exact fixture bytes/hashes, schema gate ordering, same-response project ownership, auth redaction, Loomweave validators, and the read-only Plainweave consumer harness/tests.
5. Fix every in-scope finding and repeat affected gates plus the final crawl until the reviewer signs off.

## Task 10: Integrate and close

1. Rebase the feature branch onto live `main`; repeat canonical gates if `main` moved.
2. Fast-forward only into a clean `/home/john/loomweave` main worktree. Do not stash or overwrite owner changes.
3. Update `clarion-b5c50abb19` with commit SHAs, schema decisions, exact test counts, latest-run JSON, doctor output, fixture paths/hashes, and the independent seam sign-off. Transition through `verifying` and close only after all evidence is on main.
4. Report any remaining operator action (principally copying the authoritative fixtures into Plainweave and configuring the selected bearer/HMAC env var); do not present those external actions as completed.
