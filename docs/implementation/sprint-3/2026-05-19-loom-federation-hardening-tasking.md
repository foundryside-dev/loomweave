# Loom Federation Hardening Tasking

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` to implement this tasking task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Bring Clarion's HTTP read API and ADR-014 federation contract up to the shared Loom standard required for Clarion 1.0 and Filigree 2.1 registry-backend mode.

**Release stance:** This is release-blocking unless ADR-014 Clarion registry mode is explicitly de-scoped from the Clarion 1.0 and Filigree 2.1 tag notes. The current RC1 tests are green, but they validate the old contract shape rather than the hardened federation contract.

**Architecture:** Clarion owns the publisher side of the file-registry read contract. Filigree's side should implement against Clarion's documented decisions, so Clarion must first pin the wire semantics in ADR-014, then make storage and HTTP behavior match the documented contract.

**Tech Stack:** Rust workspace, `axum`, `tower`/`tower-http`, `rusqlite`, `deadpool`, `serde`, `serde_json`, `uuid`, Clarion's local Filigree tracker.

---

## Current Evidence

Filigree reported, and RC1 source confirms, that the contract hardening prompt is largely not landed:

- C1: `resolve_file` storage errors still map to HTTP 400 with raw `err.to_string()` at `crates/clarion-cli/src/http_read.rs`.
- C2/C5/C11: `resolve_file` still requires disk existence via canonicalization, still synthesizes `core:file:{content_hash}@{canonical_path}`, and still converts unreadable hash failures to empty strings in `crates/clarion-storage/src/query.rs`.
- C3: federation fixtures are still bare request/response snippets, and the file fixture still documents an invalid `@` entity ID.
- C4/C7/C8/C9: HTTP read config has only `enabled` and `bind`; there is no non-loopback guard, no `allow_non_loopback`, no middleware stack, separate reader pools remain, and HTTP thread supervision only happens on shutdown.
- C6/C10/C12/C13: joint contract decisions are not pinned; capabilities still return `version: "0.1"`, there is no `instance_id`, and errors are still `{ "error": String }`.

## Contract Decisions Clarion Must Own First

Clarion owns these publisher-side choices. Land them in `docs/clarion/adr/ADR-014-filigree-registry-backend.md` before implementation so the Filigree agent has a stable reference:

1. **Capabilities version field:** use `api_version: 1`.
   `api_version` increments only when the HTTP read API wire contract changes incompatibly for existing Filigree clients. Do not use product semver here.
2. **File identity when no `kind='file'` row exists:** fail closed with 404.
   Delete the synthetic `core:file:{content_hash}@{canonical_path}` branch. It violates ADR-003 and creates shadow IDs that will not match future file-discovery rows.
3. **`canonical_path` semantics:** project-relative POSIX path.
   No leading `/`, no leading `./`, no trailing `/`, separator `/`. This survives project relocation and matches current response intent.
4. **Instance fingerprint:** stable per-project UUID in `.clarion/instance_id`, surfaced as `instance_id` in capabilities.
   First creation persists with mode `0600` on Unix. Deleting `.clarion/` may create a new instance ID; that is acceptable and should be detectable by Filigree.
5. **Error envelope:** closed shape `{ "error": String, "code": ErrorCode }`.
   Initial code enum: `INVALID_PATH`, `PATH_OUTSIDE_PROJECT`, `NOT_FOUND`, `STORAGE_ERROR`, `INTERNAL`.
6. **HTTP trust model:** unauthenticated loopback-only by default.
   Non-loopback bind is refused unless `serve.http.allow_non_loopback: true`; opt-in startup logs must warn that the surface is unauthenticated.

## Dependency Order

```text
T0 contract docs
  -> T1 storage resolution correctness (C2, C5, C10, C11)
  -> T2 HTTP status/error contract (C1, C13)
  -> T3 capabilities/instance handshake (C6, C12)
  -> T4 fixtures + contract gates (C3)
  -> T5 runtime hardening (C4, C7, C8, C9)
  -> T6 medium hardening / known-issues call (C14-C19)
```

If the team needs parallelism after T0 lands:

- Worker A owns storage: `crates/clarion-storage/src/query.rs`, `crates/clarion-storage/tests/query_helpers.rs`.
- Worker B owns HTTP contract: `crates/clarion-cli/src/http_read.rs`, `crates/clarion-cli/tests/serve.rs`, fixtures.
- Worker C owns runtime guard/supervision: `crates/clarion-mcp/src/config.rs`, `crates/clarion-cli/src/serve.rs`, `crates/clarion-cli/src/http_read.rs`.
- Worker D owns docs: ADR-014, ADR-003, `docs/federation/contracts.md`, `docs/operator/clarion-http-read-api.md`, `docs/suite/loom.md`.

Workers are not alone in the codebase. Do not revert or overwrite unrelated RC1 work; keep each task's write set narrow.

## Tracker Shape

Create one P0 epic:

```text
Sprint 3 RC1 — Loom federation HTTP read API hardening
```

Create these child phases/tasks:

- P0 phase: Contract decisions and docs
- P0 bug: C2 — remove invalid synthetic file IDs and fail closed
- P0 bug: C1 — classify HTTP read storage errors correctly
- P0 task: C3 — upgrade federation fixtures into contract tests
- P0 task: C4 — enforce loopback trust model
- P1 task: C5/C11 — unreadable/deleted file behavior
- P1 task: C6/C12 — api_version and instance_id capability handshake
- P1 task: C7/C8/C9 — HTTP runtime resource limits, supervision, shared reader pool
- P1 task: C13 — closed error envelope
- P2 phase: Medium hardening C14-C19 or known-issues note

Use:

```bash
filigree --actor clarion-agent start-work <id> --assignee clarion-agent
filigree --actor clarion-agent add-comment <id> "summary, tests, commits"
filigree --actor clarion-agent close <id> --reason="implemented and tested ..."
```

Do not close any tracker item until the code, tests, docs, and commit for that item exist.

## T0: Contract Decisions and ADR Patch

**Files:**

- Modify: `docs/clarion/adr/ADR-014-filigree-registry-backend.md`
- Modify: `docs/clarion/adr/ADR-003-entity-id-scheme.md`
- Modify: `docs/federation/contracts.md`
- Create: `docs/operator/clarion-http-read-api.md`
- Modify: `docs/suite/loom.md`

**Acceptance criteria:**

- ADR-014 has sections for capability probe semantics, canonical path semantics, instance fingerprint, error envelope, and security posture.
- ADR-003 explicitly states file-kind IDs are `core:file:{qualified_name}` and may not contain `@`.
- Federation contract docs show `api_version: 1`, `instance_id`, project-relative `canonical_path`, and closed error codes.
- Operator docs state the HTTP read API is unauthenticated, loopback-only by default, and must be fronted by an authenticated reverse proxy if bound non-loopback.

**Steps:**

- [ ] Patch ADR-014 with the six contract decisions above.
- [ ] Patch ADR-003 with a file-kind ID grammar note and reject the former `content_hash@path` pattern as non-conforming.
- [ ] Patch `docs/federation/contracts.md` to show the new request/response and error shapes.
- [ ] Create `docs/operator/clarion-http-read-api.md` with a `Trust model` section.
- [ ] Patch `docs/suite/loom.md` to link the operator trust model.
- [ ] Run `cargo test -p clarion-cli --test serve` to ensure docs-only edits did not disturb tests.
- [ ] Commit as `C0 docs: pin Loom federation read contract decisions`.

## T1: Storage Resolution Correctness

**Findings covered:** C2, C5, C10, C11.

**Files:**

- Modify: `crates/clarion-storage/src/query.rs`
- Modify: `crates/clarion-storage/tests/query_helpers.rs`

**Implementation requirements:**

- `resolve_file` must return `Ok(None)` when no `kind='file'` row exists for the path.
- Delete the synthetic ID branch.
- Stop requiring the candidate file to exist on disk before checking catalog rows.
- Deleted-on-disk but cataloged files resolve to 200-equivalent storage success using cataloged entity data.
- `file_content_hash` must return `Result<String, io::Error>` if still used as a fallback.
- Never return empty `content_hash` because disk read failed. If no catalog hash exists and a hash fallback fails, propagate the error.
- `canonical_path` returned by `resolve_file` must be project-relative POSIX and must not start with `/`, `./`, or `../`.
- Caller-supplied `language` must not poison the catalog response when Clarion can infer a better value.

**Tests to add first:**

- `resolve_file_returns_none_when_no_file_kind_entity_exists`
- `resolve_file_deleted_on_disk_but_cataloged_row_resolves`
- `resolve_file_unreadable_hash_failure_propagates`
- `resolve_file_returns_project_relative_posix_canonical_path`
- `resolve_file_does_not_echo_invalid_requested_language_over_catalog_inference`

**Commands:**

```bash
cargo test -p clarion-storage resolve_file -- --nocapture
cargo test -p clarion-storage
```

**Commit guidance:**

- Commit C2 separately if possible: `C2 fix: remove synthetic file identity branch`.
- Commit C5/C11 together only if the same refactor is inseparable: `C5 C11 fix: resolve cataloged files without empty hash fallback`.
- Mention in the commit body why `@` IDs are forbidden by ADR-003.

## T2: HTTP Status and Error Envelope Contract

**Findings covered:** C1, C13.

**Files:**

- Modify: `crates/clarion-cli/src/http_read.rs`
- Modify: `crates/clarion-cli/tests/serve.rs`
- May modify: `crates/clarion-storage/src/error.rs` only if a small helper is needed.

**Implementation requirements:**

- Replace `Err(err) => json_error(StatusCode::BAD_REQUEST, &err.to_string())`.
- Map storage errors:
  - `InvalidSourcePath`, `InvalidQuery` -> 400
  - candidate file not known -> 404 via `Ok(None)`
  - deleted/unreadable candidate under project root when no catalog row can answer -> 404 or stable `NOT_FOUND`
  - project-root canonicalization failure -> 500
  - SQLite, pool, pool interact, pool build -> 500 or 503
- Do not echo raw storage errors to clients.
- Log 500-class errors with the full error chain using `tracing::error!`.
- Return `{ "error": "...", "code": "..." }` for every non-2xx response.
- Use the closed code set documented in T0.

**Tests to add first:**

- blank path returns 400 `INVALID_PATH`
- path traversal returns 400 `PATH_OUTSIDE_PROJECT`
- unknown catalog file returns 404 `NOT_FOUND`
- storage/pool/sqlite failure returns 500 or 503 with `STORAGE_ERROR`
- response body never contains the raw SQLite/private path detail for 500-class errors

**Commands:**

```bash
cargo test -p clarion-cli --test serve
cargo test -p clarion-storage
```

**Commit guidance:**

- `C1 fix: classify HTTP read storage errors`
- `C13 fix: pin HTTP read error envelope`

## T3: Capability Probe and Instance Fingerprint

**Findings covered:** C6, C12.

**Files:**

- Modify: `crates/clarion-cli/src/http_read.rs`
- Modify: `crates/clarion-cli/tests/serve.rs`
- Modify or create helper in: `crates/clarion-cli/src/serve.rs` or a small `instance.rs` module if that keeps `http_read.rs` focused.
- Modify: `docs/federation/fixtures/get-api-v1-capabilities.json`

**Implementation requirements:**

- Capabilities response must be:

```json
{
  "registry_backend": true,
  "file_registry": true,
  "api_version": 1,
  "instance_id": "uuid-string"
}
```

- On startup, read `.clarion/instance_id`; if absent, create a UUID v4 and persist it.
- On Unix, persist with mode `0600`.
- The ID must survive process restarts.
- The capability fixture must document the shape with `_meta`, `shape_decl`, and `examples`.

**Tests to add first:**

- first startup creates `.clarion/instance_id`
- second startup reuses the same ID
- capabilities returns `api_version: 1`
- capabilities includes the stored `instance_id`
- invalid existing instance-id file is rejected or replaced according to ADR-014's documented rule

**Commands:**

```bash
cargo test -p clarion-cli --test serve capabilities -- --nocapture
```

**Commit guidance:**

- `C6 fix: expose api_version in HTTP capabilities`
- `C12 fix: add stable Clarion instance fingerprint`

## T4: Contract Fixtures and Shape Gate

**Finding covered:** C3.

**Files:**

- Modify: `docs/federation/fixtures/get-api-v1-files.demo-python.json`
- Modify: `docs/federation/fixtures/get-api-v1-capabilities.json`
- Modify: `crates/clarion-cli/tests/serve.rs` or create `crates/clarion-cli/tests/http_contract.rs`

**Implementation requirements:**

- Fixtures must have:
  - `_meta`: `contract`, `stability`, `authority`, `verification`, `updated`
  - `shape_decl`: human-readable shape rules
  - `examples`: named examples with request and response
- `/api/v1/files` examples:
  - 200 happy path with a real file-kind entity
  - 404 file-not-known
  - 400 blank path
  - 400 outside project root
  - 500/503 storage error
- `/_capabilities` examples:
  - one valid response with `api_version: 1` and `instance_id`
  - future extension point note
- Add a Rust test that boots the HTTP server and validates real responses against fixture shape declarations.

**Commands:**

```bash
cargo test -p clarion-cli --test serve
```

**Commit guidance:**

- `C3 test: pin HTTP read API federation fixtures`

## T5: HTTP Trust Model and Runtime Hardening

**Findings covered:** C4, C7, C8, C9.

**Files:**

- Modify: `crates/clarion-mcp/src/config.rs`
- Modify: `crates/clarion-cli/src/http_read.rs`
- Modify: `crates/clarion-cli/src/serve.rs`
- Modify: `crates/clarion-cli/Cargo.toml`
- Modify: workspace `Cargo.toml` and `Cargo.lock` if adding dependencies
- Modify: `crates/clarion-cli/tests/serve.rs`

**Implementation requirements:**

- Add `serve.http.allow_non_loopback: bool`, default false.
- Refuse to start the HTTP read API if enabled and bind is not loopback unless `allow_non_loopback` is true.
- Log normal startup with `auth = "none"` and bind address.
- Log opt-in non-loopback startup as WARN and state that the surface is unauthenticated.
- Add middleware:
  - `TraceLayer::new_for_http()`
  - `TimeoutLayer::new(Duration::from_secs(10))`
  - `RequestBodyLimitLayer::new(16 * 1024)`
  - `ConcurrencyLimitLayer::new(64)`
  - `LoadShedLayer::new()`
- Share the existing `ReaderPool` between MCP and HTTP; do not open a second pool.
- Add supervision so a failed HTTP server aborts or fails the serve process visibly before MCP stdio continues pretending healthy.
- Name threads if a dedicated thread remains: OS thread `clarion-http-read`, Tokio workers `clarion-http-worker`.

**Tests to add first:**

- enabled loopback without flag starts
- enabled non-loopback without flag is refused
- enabled non-loopback with flag starts and logs warning
- HTTP server uses the shared reader pool path
- request body/timeout/concurrency behavior is enforced enough to prove middleware is wired
- supervised HTTP failure is surfaced before shutdown-only join handling

**Commands:**

```bash
cargo test -p clarion-cli --test serve
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

**Commit guidance:**

- `C4 fix: enforce HTTP read API loopback trust model`
- `C7 fix: add HTTP read API resource limits`
- `C8 fix: supervise HTTP read server lifecycle`
- `C9 fix: share reader pool with HTTP read API`

## T6: Medium Hardening or Known-Issues Note

**Findings covered:** C14-C19.

These do not have to block the tag if the release notes explicitly identify them as known limitations. If time allows, land them after T1-T5:

- C14: read language inference from plugin manifests rather than a hardcoded extension map.
- C15: release reader-pool slots before slow disk hash fallback.
- C16: support `ETag` and `If-None-Match` on `/api/v1/files`.
- C17: structured thread/request naming, if not fully covered by T5.
- C18: per-request structured access log fields including optional `X-Loom-Component` and `X-Filigree-Actor`.
- C19: `#[serde(deny_unknown_fields)]` on `FileQuery`.

If deferred, add a `Known limitations for 1.0` section to the release notes and create Clarion tracker issues for each item retained beyond the tag.

## End-to-End Acceptance

Clarion side is in spec when all of the following are true:

- ADR-014 documents `api_version`, `canonical_path`, `instance_id`, error envelope, no synthetic IDs, and trust model.
- ADR-003 no longer permits or implies `@` file IDs.
- `cargo test -p clarion-cli --test serve` passes.
- `cargo test -p clarion-storage` passes.
- `cargo test --workspace` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo fmt --all --check` passes.
- A local HTTP smoke against `clarion serve` proves:
  - `/api/v1/_capabilities` returns `api_version: 1` and `instance_id`.
  - `/api/v1/files` returns a real `core:file:*` entity ID for a cataloged file.
  - a module-only catalog row does not synthesize a file ID and returns 404.
  - a deleted-on-disk but cataloged file returns the cataloged identity.
  - a traversal path returns a stable 400 error envelope.
- Filigree has the documented shapes needed to implement F4/F8/F9/F14 without guessing.

## Final Output Required From Implementing Agent

When finished or blocked, report:

- Findings landed and commit SHAs.
- Findings still in flight.
- Findings blocked on Filigree.
- Test suite status.
- Joint decisions made and exact doc locations.
- Any reviewer finding that proved wrong, with source evidence.

Do not mutate `/home/john/filigree`; it is read-only for this Clarion hardening pass.
