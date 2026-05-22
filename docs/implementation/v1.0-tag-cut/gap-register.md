# Clarion v1.0.0 Tag-Cut Gap Register

**Date**: 2026-05-22
**Branch**: `RC1` at `4dd7b63`
**Status**: 24 gaps open against the `v1.0.0` tag-cut criterion.

This document is the single source of truth for everything that stands between
the current RC1 commit and a defensible `v1.0.0` tag. Every gap is
evidence-cited, has a concrete fix, and has an effort estimate. The
[execution-plan.md](execution-plan.md) sequences these into a day-by-day
schedule.

## Severity scale

- **Critical** — supply-chain integrity or wire-contract correctness; ship a
  tag without these and downstream tooling breaks or an attacker has a
  concrete path. Must close before tag-cut.
- **High** — observable user-facing wrongness, missed test coverage on
  shipped surfaces, or operator-visible safety gap. Must close before
  tag-cut.
- **Medium** — drift between authoritative sources, documentation
  inaccuracies. Must close before tag-cut for any gap on the documented
  release surface.
- **Low** — internal-only polish, performance, or non-shipping discipline.
  Listed for completeness; may defer to v1.1.

## Category summary

| Category | Count | Effort |
|----------|------:|-------:|
| Operator / governance (live GitHub controls, smoke evidence) | 4 | 3.5 hr |
| Documentation and contract drift | 11 | 1 hr |
| Security fail-closed + doc gaps | 3 | 1 hr |
| CI/CD release-path correctness | 4 | 2 hr |
| Storage / SQLite discipline | 5 | 4 hr |
| Test gate wiring | 2 | 1 hr |
| Code bug (federation error-code) | 1 | 15 min |
| **Total** | **30** | **~13 hr** |

(The 24-count from the executive summary collapsed some related items;
this register breaks every item out individually for tracking. The 1.0
blocker count is unchanged.)

---

## Originating-review key

- **arch-2026-05-20** — [`../arch-analysis-2026-05-20-2124/04-final-report.md`](../arch-analysis-2026-05-20-2124/04-final-report.md) and siblings
- **deep-dive-arch** — Architecture critique subagent, 2026-05-22
- **deep-dive-security** — Threat analyst subagent, 2026-05-22
- **deep-dive-pipeline** — Pipeline reviewer subagent, 2026-05-22
- **deep-dive-db** — Embedded-database reviewer subagent, 2026-05-22
- **deep-dive-docs** — Doc consistency reviewer subagent, 2026-05-22
- **deep-dive-quality** — Coverage gap analyst subagent, 2026-05-22

---

## Gap register

### Category 1 — Operator / governance

#### GOV-01 (Critical) — Live GitHub governance is permissive

- **Origin**: arch-2026-05-20 R1; deep-dive-pipeline.
- **Evidence**: `docs/implementation/v1.0-cicd-readiness.md:103-114` documents
  the live state at 2026-05-20: `main` returns `404 Branch not protected`,
  rulesets empty, `allowed_actions=all`, `sha_pinning_required=false`.
- **Fix**: Operator action. Enable branch protection (or active ruleset)
  targeting `main` that (a) requires pull-request flow with at least the
  three release CI checks (`Rust`, `Python plugin`,
  `Sprint 1 walking skeleton (end-to-end)`), (b) enables a constrained
  Actions source policy (SHA-pinning or allow-list), (c) configures the
  `RELEASE_GOVERNANCE_TOKEN` repository Actions secret with permission to
  read branch-protection, ruleset, and Actions policy settings.
- **Runbook**: [`docs/operator/v1.0-release-governance.md`](../../operator/v1.0-release-governance.md)
- **Effort**: 1 hr operator.
- **Exit criterion**: `scripts/check-github-release-governance.py
  --repository tachyon-beep/clarion --branch main` exits 0.

#### GOV-02 (Critical) — No tag protection rule

- **Origin**: deep-dive-pipeline (net-new vs arch analysis).
- **Evidence**: `scripts/check-github-release-governance.py` does not query
  `GET /repos/{owner}/{repo}/tags/protection` or check for a ruleset
  targeting `refs/tags/v*`. The operator runbook only covers branch
  protection.
- **Risk**: An actor with tag-push permission can push `refs/tags/v1.0.0`
  pointing at any commit (a feature branch, a detached commit). If that
  commit passes the workflow's gates, the release publishes from it.
- **Fix**: Add a GitHub ruleset targeting `refs/tags/v*` that restricts
  who can push matching tags (creators allow-list: repository owner only,
  or the same actors who can write to `main`). Add a tag-protection check
  to `check-github-release-governance.py`.
- **Effort**: 5 min operator (ruleset) + 30 min engineering (script).
- **Exit criterion**: Governance script asserts tag protection on `refs/tags/v*`.

#### GOV-03 (Critical) — No rollback / yank runbook

- **Origin**: deep-dive-pipeline (net-new).
- **Evidence**: Neither `docs/operator/v1.0-release-governance.md` nor
  `docs/implementation/v1.0-cicd-readiness.md` covers what happens after
  a bad release ships. Concrete gaps for h+30min "bad release" scenario:
  GitHub Release asset deletion guidance absent; Sigstore Rekor
  non-revocability not stated; downstream (Filigree) notification path
  undocumented.
- **Fix**: Add `docs/operator/v1.0-release-rollback.md` covering:
  (a) `gh release edit v1.0.0 --prerelease` as the first action;
  (b) asset-deletion policy (don't delete unless sensitive — broken URLs
  are worse than stale files); (c) explicit statement that cosign
  signatures in Rekor cannot be revoked, only superseded; (d) `v1.0.1`
  publication procedure with `superseded by` note in the release body;
  (e) Filigree consumer notification path.
- **Effort**: 1 hr docs.
- **Exit criterion**: Runbook exists, reviewed, linked from
  `v1.0-release-governance.md`.

#### GOV-04 (High) — No dated external-operator smoke result

- **Origin**: arch-2026-05-20 R6.
- **Evidence**: `tests/e2e/external-operator-smoke.md` defines a checklist.
  No dated result file exists in the tree.
- **Fix**: Operator runs the checklist on a fresh Linux x86_64 VM and a
  fresh macOS host. Archive the result as
  `docs/implementation/v1.0-tag-cut/external-operator-smoke-result.md`
  with a timestamp, OS+arch, the install command run, the
  "improvisation events" count (target: 0), and an attestation signature.
- **Effort**: 2 hr operator (mostly VM provisioning + walkthrough time).
- **Exit criterion**: Dated result file committed.

---

### Category 2 — Documentation and contract drift

#### DOC-01 (Critical) — CHANGELOG advertises non-existent `UNAUTHORIZED` error code

- **Origin**: arch-2026-05-20 R3; deep-dive-docs C1.
- **Evidence**: `CHANGELOG.md:60` lists `UNAUTHORIZED`. Authoritative
  sources (`docs/federation/contracts.md:82`, `82,202,276`,
  `docs/clarion/adr/ADR-014:126`, `docs/clarion/adr/ADR-034:45,87,145`,
  implementation tests at `crates/clarion-cli/tests/serve.rs:1457,1495,1547,1579,1614`)
  all use `UNAUTHENTICATED`.
- **Fix**: `Edit` `UNAUTHORIZED` → `UNAUTHENTICATED` in `CHANGELOG.md:60`.
- **Effort**: 1 min.

#### DOC-02 (High) — CHANGELOG error enum missing `BATCH_TOO_LARGE`

- **Origin**: deep-dive-docs H1 (net-new).
- **Evidence**: `CHANGELOG.md:59-61` lists six codes plus `UNAUTHORIZED`.
  `docs/federation/contracts.md:83-84` and `ADR-034:87` enumerate the same
  set plus `BATCH_TOO_LARGE`. PR #12's own body claims the correction was
  made — it was made in `contracts.md` but not in the CHANGELOG.
- **Fix**: `Edit` `CHANGELOG.md:60` to add `BATCH_TOO_LARGE`.
- **Effort**: 1 min.

#### DOC-03 (High) — NFR-SEC-03 is stale post-ADR-034

- **Origin**: deep-dive-docs H2 (net-new).
- **Evidence**: `docs/clarion/1.0/requirements.md:771-783` says non-loopback
  "When enabled, startup logs a warning that the endpoint is
  unauthenticated and must be protected outside Clarion." `ADR-034:43`
  says "Non-loopback binds require both `allow_non_loopback: true` and a
  resolved HMAC identity secret or legacy bearer token; either alone is
  insufficient." Per CLAUDE.md precedence (ADR > requirements), the
  requirement statement is the bug.
- **Fix**: Rewrite NFR-SEC-03 statement and verification clause to
  describe the post-ADR-034 rule: non-loopback **requires** auth; the
  startup warning is for the loopback-without-token mode only.
- **Effort**: 10 min docs.

#### DOC-04 (High) — REQ-HTTP-03 is stale post-ADR-034

- **Origin**: deep-dive-docs H2.
- **Evidence**: `docs/clarion/1.0/requirements.md:558-573` still describes
  the HTTP API as "unauthenticated and loopback-only by default" and the
  verification as "unauthenticated-surface warning". Same drift as DOC-03
  but on the routing side.
- **Fix**: Rewrite REQ-HTTP-03 statement and verification to reflect
  ADR-034's authenticated-non-loopback rule.
- **Effort**: 10 min docs.

#### DOC-05 (Medium) — REQ-HTTP-03 `See` line missing ADR-034

- **Origin**: deep-dive-docs M3.
- **Evidence**: `requirements.md:573` reads `See: System Design §9
  (Integrations, HTTP Read API), ADR-014.` ADR-034 partially extends
  ADR-014's Security Posture and Error Envelope; a reader cannot
  discover ADR-034 from the requirement.
- **Fix**: `Edit` to add `, ADR-034`.
- **Effort**: 1 min.

#### DOC-06 (High) — loom.md still labels asterisks as "v0.1"

- **Origin**: deep-dive-docs H3.
- **Evidence**: `docs/suite/loom.md:65,69` heads "v0.1 asterisks" and says
  "The asterisk ships with v0.1 and retires in v0.2." CLAUDE.md:64 and
  the CHANGELOG say both asterisks persist into v1.0 and retire
  post-release.
- **Fix**: Rename "v0.1 asterisks" → "v1.0 asterisks" and update the
  retirement wording. Update `CHANGELOG.md:108` "deferred to v0.2" →
  "deferred to a future release (tracked under `release:v1.1`)."
- **Effort**: 5 min docs.

#### DOC-07 (High) — CLAUDE.md misrepresents HMAC as post-1.0

- **Origin**: deep-dive-docs H4.
- **Evidence**: `CLAUDE.md:144` "HMAC inbound auth (C-4) — bearer is the
  1.0 wire surface; HMAC is forward-compatible and tracked for post-1.0
  hardening." Code reads `identity_token_env` and enforces
  `X-Loom-Component: clarion:<hmac>` today
  (`crates/clarion-cli/src/http_read.rs:129-130,184,373-374`); ADR-034
  marks HMAC as the preferred mechanism; CHANGELOG:115-117 documents it
  shipping.
- **Fix**: Update CLAUDE.md HMAC paragraph: HMAC ships in v1.0 per
  ADR-034 as the preferred non-loopback authentication; document
  `identity_token_env` config. Move "post-1.0 hardening" to refer only
  to replay protection (timestamp + nonce window, ADR-034 forward-work).
- **Effort**: 10 min.

#### DOC-08 (Medium) — secret-scanning.md carries v0.1 reference

- **Origin**: deep-dive-docs M2.
- **Evidence**: `docs/operator/secret-scanning.md:83` "Contextual
  credential suppression only recognises shell/Python `#` comments in
  v0.1."
- **Fix**: Drop the version qualifier or change to "in v1.0".
- **Effort**: 1 min.

#### DOC-09 (Medium) — loom.md asterisk 2 (Wardline REGISTRY) absent from CHANGELOG

- **Origin**: deep-dive-docs M1.
- **Evidence**: `loom.md:70` names the Wardline REGISTRY import as
  asterisk 2 with no retirement condition citation. CHANGELOG "Known
  v1.0 limitations" (lines 105-117) does not mention it.
- **Fix**: Add an entry to CHANGELOG "Known limitations": "The Python
  plugin imports `wardline.core.registry.REGISTRY` at startup
  (loom.md §5 asterisk 2). Retirement condition: Wardline ships a
  stable runtime probe API."
- **Effort**: 5 min.

#### DOC-10 (Low) — CHANGELOG "Documentation" ADR count phrasing

- **Origin**: deep-dive-docs L2.
- **Evidence**: CHANGELOG says "28 Accepted at 1.0 (through ADR-034)";
  the math is right but the parenthetical "(through ADR-034)" reads as
  a contiguous range when in fact ADR-008/009/010/019/020 are
  Backlog/Superseded.
- **Fix**: Adjust phrasing to "(ADR-001…ADR-034 with the documented
  Backlog/Superseded subset excluded)" or similar.
- **Effort**: 2 min.

#### DOC-11 (High) — Storage operator constraints missing from README

- **Origin**: deep-dive-db low-severity finding, promoted to High here
  because the cross-process race (STO-01) will surface as operator
  confusion without operator-facing guidance.
- **Evidence**: There is no operator-facing doc that says (a) do not
  put `.clarion/` on NFS, (b) do not run two `clarion analyze`
  simultaneously, (c) backup procedure is "stop analyze →
  `PRAGMA wal_checkpoint(TRUNCATE)` → file copy".
- **Fix**: Add a §Storage paragraph to top-level README or a new
  `docs/clarion/1.0/operations.md` covering deployment constraints.
- **Effort**: 30 min.

---

### Category 3 — Security: fail-closed + documentation

#### SEC-01 (Critical) — `entity_briefing_block_reason` fail-open on malformed JSON

- **Origin**: deep-dive-security T-11 (net-new).
- **Evidence**: `crates/clarion-storage/src/query.rs:296-302`
  `entity_briefing_block_reason` returns `None` (= unblocked) when
  `serde_json::from_str(properties_json)` fails. A plugin emitting
  malformed `properties` JSON silently disables the WP5 briefing
  block, exposing the entity through every federation read path.
- **Fix**: One-line change: on `from_str` failure, return
  `Some("malformed_properties_json")` (or equivalent
  block-with-reason). Add a unit test covering the malformed-JSON
  path.
- **Effort**: 15 min code + 15 min test.

#### SEC-02 (High) — Loopback-no-token trust assumption not documented

- **Origin**: deep-dive-security T-9.
- **Evidence**: `crates/clarion-cli/src/http_read.rs:384-386` admits any
  request when both `identity_secret` and `auth_token` are `None`.
  `validate_auth_trust` (`crates/clarion-mcp/src/config.rs:307-345`)
  only refuses *non-loopback* binds. On a shared developer host or CI
  runner, any local process can read the entire (non-blocked)
  catalogue.
- **Fix**: (a) Add explicit operator-doc section to
  `docs/operator/secret-scanning.md` and `docs/operator/clarion-http-read-api.md`
  describing the loopback-without-token trust assumption. (b) Add a
  startup-banner line when loopback-no-token mode is in effect:
  "HTTP API serving on loopback without authentication; any local
  process can read the catalogue."
- **Effort**: 20 min docs + 20 min code.

#### SEC-03 (High) — Legacy `.clarion/` upgrade requirement undocumented

- **Origin**: deep-dive-security T-11 (sub-point).
- **Evidence**: `entity_briefing_block_reason` reads
  `properties.briefing_blocked`. Pre-WP5 binaries never wrote that
  property; a 1.0 binary opening a pre-WP5 `.clarion/clarion.db` will
  serve the entire catalogue without refusal because every row's
  `briefing_blocked` is structurally absent.
- **Fix**: Document the upgrade requirement in
  `docs/operator/secret-scanning.md` and in the CHANGELOG "Known
  limitations": "Upgrading from a pre-WP5 binary requires
  `clarion analyze` re-run before any HTTP API serves the catalogue."
- **Effort**: 10 min.

---

### Category 4 — CI / release path correctness

#### CI-01 (Critical) — No in-workflow tag-lineage check

- **Origin**: deep-dive-pipeline (net-new).
- **Evidence**: `release.yml verify` job runs against `$GITHUB_SHA`
  (whatever the pushed tag resolves to). Nothing in the workflow asserts
  that `$GITHUB_SHA` is an ancestor of `origin/main`. Combined with
  GOV-02, this is the supply-chain bypass path.
- **Fix**: Add to the start of `verify`:
  ```yaml
  - name: Assert tagged commit is on main
    run: |
      git fetch origin main
      git merge-base --is-ancestor "$GITHUB_SHA" origin/main || \
        { echo "::error::tag does not point to a commit on main"; exit 1; }
  ```
- **Effort**: 10 min.

#### CI-02 (Critical) — Federation error code wrong on HMAC body parse failure

- **Origin**: deep-dive-arch (architecture critic) + deep-dive-security
  (corroborating).
- **Evidence**: `crates/clarion-cli/src/http_read.rs:426-431` — if
  `to_bytes(body, HTTP_BODY_LIMIT_BYTES)` fails inside HMAC middleware,
  the response returns `ErrorCode::InvalidPath` with message
  "request body is invalid". A federation client pattern-matching on
  `code` will mis-route a body/IO failure as a path-validation failure.
- **Fix**: Use a separate `ErrorCode::InvalidBody` (or
  `ErrorCode::Internal`) on body-parse failure. Add a test that
  exercises an oversized body and asserts the code field.
- **Effort**: 15 min code + 15 min test.

#### CI-03 (High) — Python sdist not SLSA-attested

- **Origin**: deep-dive-pipeline.
- **Evidence**: `release-subjects` in `.github/workflows/release.yml:201-225`
  globs only `clarion-*.tar.gz` (Rust archives) into the SLSA provenance
  subjects. The Python plugin sdist has cosign signing but no SLSA
  attestation file. A user installing via `pipx install` from the
  release URL has no `slsa-verifier` path.
- **Fix**: Either (a) extend the glob to include
  `clarion_plugin_python*.tar.gz` and append to the existing provenance
  file, or (b) document the gap explicitly in the release notes and in
  `docs/operator/v1.0-release-governance.md`.
- **Effort**: 30 min for option (a); 5 min for option (b).
- **Recommended**: option (a) — it's a one-line change to the glob.

#### CI-04 (Medium) — No automated post-publish verification

- **Origin**: deep-dive-pipeline.
- **Evidence**: `release.yml` runs cosign verify on the same runner
  immediately after signing (lines 313-323). This proves signing
  worked; it does not prove the published GitHub Release artifacts
  match the signed local copies.
- **Fix**: Add a final `verify-published-release` job that runs after
  `create-release`, downloads the assets from the public Release URL,
  recomputes SHA256, and cosign-verifies against the public Rekor
  entries.
- **Effort**: 45 min.

---

### Category 5 — Storage / SQLite discipline

#### STO-01 (Critical) — No cross-process lock; second `clarion analyze` corrupts run state

- **Origin**: deep-dive-db (highest-priority finding).
- **Evidence**: `crates/clarion-cli/src/run_lifecycle.rs:19-25`
  unconditionally executes `UPDATE runs SET status='failed' WHERE
  status='running'` at the top of every `clarion analyze`, then opens
  the writer-actor connection. There is no `fs2::FileExt::try_lock_exclusive()`
  on `.clarion/clarion.lock` or the DB file. A second concurrent
  `clarion analyze` flips the live run's status to `failed` while the
  first writer holds an open connection mid-batch.
- **Fix**: Add `fs2 = "0.4"` to `clarion-cli/Cargo.toml`. At the top
  of `analyze::run` (and `serve` write paths), acquire
  `File::open(".clarion/clarion.lock")?.try_lock_exclusive()`. Hold for
  writer-actor lifetime. Fail fast with "another clarion analyze is in
  progress against this project".
- **Effort**: 1-2 hr code + test.

#### STO-02 (High) — No `PRAGMA application_id`

- **Origin**: deep-dive-db.
- **Evidence**: `crates/clarion-storage/src/pragma.rs` sets WAL,
  synchronous, busy, foreign-keys, but never `application_id` or
  `user_version`. The SQLite file has no identity marker; tooling
  like `file(1)` or `sqlite3 .dbinfo` cannot distinguish a Clarion DB
  from any other SQLite file.
- **Fix**: Add `PRAGMA application_id = 0x434C524E` ("CLRN") to
  `apply_write_pragmas`. On open, assert the application_id is 0
  (legacy / unset, then set it) or `0x434C524E` (recognise). Refuse
  any other value with a clear error.
- **Effort**: 30 min code + 15 min test.

#### STO-03 (Critical) — No `published_build.txt` migration marker

- **Origin**: deep-dive-db; arch-2026-05-20 follow-up.
- **Evidence**: ADR-024 migration retirement guard
  (`scripts/check-migration-retirement.py`) requires
  `crates/clarion-storage/migrations/published_build.txt` to mark the
  v1.0 commit SHA as the baseline. The file does not exist.
- **Fix**: Create the file at tag-cut time with the exact
  `v1.0.0` commit SHA. Block CI on its presence going forward.
- **Effort**: 1 min, but must be done at tag-cut moment after the
  release-prep PR merges.
- **Sequencing note**: STO-03 is the *last* gap closed, immediately
  before pushing the `v1.0.0` tag.

#### STO-04 (High) — No backup / `integrity_check` path

- **Origin**: deep-dive-db.
- **Evidence**: No `VACUUM INTO`, no `rusqlite::backup::Backup`, no
  `PRAGMA integrity_check` invocation in CI. A user who `cp`s
  `.clarion/clarion.db` during a live `clarion analyze` gets a torn
  copy because WAL pages live in `clarion.db-wal` separately.
- **Fix v1.0**: (a) Add `PRAGMA integrity_check` final assertion to
  `tests/e2e/sprint_1_walking_skeleton.sh`. (b) Document the
  supported backup procedure in DOC-11's README §Storage paragraph
  (shutdown → `PRAGMA wal_checkpoint(TRUNCATE)` → file copy).
  (c) `clarion db backup` subcommand deferred to v1.1.
- **Effort**: 30 min (parts a and b only).

#### STO-05 (Medium) — `recover_preexisting_running_runs` has no liveness guard

- **Origin**: deep-dive-db.
- **Evidence**: `crates/clarion-cli/src/run_lifecycle.rs:19-25`
  recovery sweep is `UPDATE runs SET status='failed' WHERE
  status='running'`. No PID column, no heartbeat, no startup-instance
  token. STO-01's fs2 lock is necessary but not sufficient; even
  fixing it leaves a same-process restart vulnerable.
- **Fix v1.0**: Document the constraint in DOC-11. Defer the
  schema-additive `runs.owner_pid` + `heartbeat_at` columns to v1.1.
- **Effort**: 5 min (doc only; the v1.1 follow-up is filed as a
  separate issue).

---

### Category 6 — Test gate wiring

#### TEST-01 (Critical) — `sprint_2_mcp_surface.sh` not in CI

- **Origin**: arch-2026-05-20 R2; deep-dive-quality (highest-severity
  finding).
- **Evidence**: `tests/e2e/sprint_2_mcp_surface.sh` exists and exercises
  all 8 MCP navigation tools (`entity_at`, `find_entity`, `callers_of`,
  `execution_paths_from`, `summary`, `issues_for`, `neighborhood`,
  `subsystem_members`) over stdio against a real `clarion analyze`
  output with the Python plugin venv. The MCP `serve.rs` integration
  test only covers `initialize`; the `storage_tools.rs` test exercises
  the underlying storage layer but not the MCP wire serialization. A
  wrong-JSON-shape regression in any tool would silently corrupt an
  agent's context.
- **Fix**: Add an additional step to the `walking-skeleton` job in
  `ci.yml` after the existing two scripts:
  ```yaml
  - name: Sprint 2 MCP surface
    run: bash tests/e2e/sprint_2_mcp_surface.sh
  ```
  Mirror in `release.yml verify` job.
- **Effort**: 30 min (wire + verify it passes in CI).

#### TEST-02 (High) — `phase3_subsystems.sh` not in CI

- **Origin**: arch-2026-05-20 R2; deep-dive-quality.
- **Evidence**: `tests/e2e/phase3_subsystems.sh` exists, exercises
  subsystem clustering determinism (two analyze runs, byte-for-byte
  identical cluster assignments). Subsystem clustering is part of
  the v1.0 advertised surface per CHANGELOG.
- **Fix**: Add to `walking-skeleton` job after `sprint_2_mcp_surface.sh`.
  Mirror in `release.yml verify`.
- **Effort**: 30 min.

---

### Category 7 — Code bug

(CI-02 also fits this category; left in CI for sequencing.)

---

## Out-of-scope for v1.0 (file as `release:v1.1` before tag-cut)

Filed as Filigree issues with `release:v1.1` label so they don't get lost.

**Architecture refactors (deep-dive-arch v1.1 priority order):**
1. Extract `clarion-core::errors` shared error-code vocabulary.
2. Split `analyze.rs` → `analyze/phase3.rs` + `analyze/mapping.rs`.
3. Split `llm_provider.rs` per-provider.
4. Split `clarion-mcp/src/lib.rs` into `tools/` subdir.
5. Split `plugin/host.rs` validation from transport.
6. Replace local HMAC-SHA256 with `hmac` + `subtle` crates.

**Storage hardening (deep-dive-db):**
1. `runs.owner_pid` + `heartbeat_at` columns and refined recovery WHERE-clause.
2. `clarion db backup` subcommand via `rusqlite::backup::Backup`.
3. `summary_cache.entity_id` FK via table-rebuild migration (confirmed
   bug — not intentional asymmetry).
4. `briefing_blocked` generated column + partial index (federation
   read-API hot path).
5. `BEGIN IMMEDIATE` + `SQLITE_BUSY` retry helper across the writer.
6. FTS5 `content_text` dead-schema decision (drop or populate).
7. ReaderPool eager validation + PRAGMA `post_create` hook.
8. Promote `briefing_blocked` from JSON property to typed column.

**Security hardening (deep-dive-security):**
1. HMAC replay protection (timestamp + nonce window). Already called
   out in ADR-034 forward-work.
2. Mandatory authentication on loopback binds (doctrine change).
3. Make pyright-langserver-missing a CI hard-fail, not a silent skip.

**CI / release hardening (deep-dive-pipeline):**
1. Refactor `release.yml verify` into a reusable workflow shared with
   `ci.yml` (eliminates drift risk).
2. Add `pip-audit` step to `build-plugin` job.
3. Add tag-protection and `workflow_permissions` checks to the
   governance script.
4. macOS Gatekeeper workaround doc in `getting-started.md`.
5. SBOM emission (cyclonedx-bom or syft).

**Drift-test scripts (deep-dive-quality):**
1. `scripts/check-pyright-pin-lockstep.py` — pyproject.toml,
   plugin.toml, and ci.yml cache key.
2. `scripts/check-wardline-version-bounds.py` — semver validity and
   eventual server-side cross-check.
3. `scripts/check-entity-cap-lockstep.py` — limits.rs `DEFAULT_MAX`
   against ADR-021 §2c.

---

## Exit criteria for v1.0.0 tag-cut

All of:

1. Every Critical and High gap above is closed (status: ✅ in this
   register, with a commit SHA citation).
2. Every Medium documentation gap is closed (operator-facing accuracy
   is non-negotiable on a release artifact).
3. `scripts/check-github-release-governance.py` exits 0 against live
   `tachyon-beep/clarion`.
4. PR #12 (or its successor) is merged to `main` and is the parent of
   the `v1.0.0` tag commit.
5. `release.yml workflow_dispatch` dry-run from `main` produces all
   expected artifacts.
6. External-operator smoke result file is dated, signed-off, and
   shows 0 improvisation events on both target platforms.
7. Filigree F-1 lockstep (registry-backend consumer rename) confirmed
   still aligned.
8. `published_build.txt` written with the tag commit SHA (last step
   before `git tag`).
