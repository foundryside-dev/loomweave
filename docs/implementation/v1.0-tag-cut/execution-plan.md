# Loomweave v1.0.0 — Tag-Cut Execution Plan

**Date**: 2026-05-22
**Closes**: every gap in [`gap-register.md`](gap-register.md).
**Total effort**: ~13 hours engineering + ~3.5 hours operator.

> **Superseded note (2026-06-05, post-1.3.0).** Historical v1.0.0 tag-cut
> record. `scripts/check-github-release-governance.py` (and the `gap-register.md`
> gaps that reference it) was **removed** after v1.0; release-governance
> enforcement is handed off to Legis and `release.yml` no longer invokes it. The
> steps below that run that script are dead — retained as-is for v1.0 provenance.

This plan sequences the gaps into three days of focused work, with explicit
parallel-execution markers. The first day is mechanical doc + bug fixes
that have no inter-dependencies and can run as parallel PRs. Day 2 is the
storage + CI hardening work that needs sequential review. Day 3 is the
operator-led governance + smoke + tag-cut.

## Branching strategy

**Decision required from operator before Day 1**: The current `RC1` branch
carries two post-1.0 dogfood commits (`a32c162`, `4dd7b63`) on top of the
release-prep stack. Pick one:

- **Option A (recommended)**: Branch a clean `release/1.0` from `a089a21`
  (last pure release-prep commit) and apply the gap-closure work there.
  Leave the dogfood commits on `RC1` as the post-1.0 working branch.
  - Pro: 1.0 tag is what it claims to be — release-prep only.
  - Pro: Dogfood work continues unblocked on `RC1`.
  - Con: One-time branch surgery; PR #12 needs re-targeting.
- **Option B**: Keep `RC1` as the release branch; document the two
  dogfood commits in CHANGELOG as "1.0 also ships agent-orientation
  affordances (`project_status`, `analyze_start/cancel/status`,
  `index_diff`, etc.)" Update the operator MCP surface section to
  enumerate the additional tools.
  - Pro: No branch surgery; PR #12 ships as-is.
  - Con: CHANGELOG and operator docs need expansion to cover the
    new MCP tools; commitment to support the additional surface
    under semver.

This plan assumes **Option A** unless the operator instructs otherwise.
If Option B, add a Day 1.5 step to expand CHANGELOG and MCP operator
docs to cover the additional tools.

---

## Day 1 — Mechanical fixes (parallelisable, no review interlock)

**Owner**: engineering, can be dispatched to parallel agents.
**Duration**: 4–5 hours wall-clock with parallel execution.
**Exit**: all PRs merged or in-review with green CI.

### Stream 1A — Single-line doc edits (one PR, ~30 min)

Bundle these into a single PR titled `docs: pre-tag-cut v1.0 contract drift fixes`.

| Gap | File | Change |
|-----|------|--------|
| DOC-01 | `CHANGELOG.md:60` | `UNAUTHORIZED` → `UNAUTHENTICATED` |
| DOC-02 | `CHANGELOG.md:60` | Add `BATCH_TOO_LARGE` |
| DOC-05 | `docs/loomweave/1.0/requirements.md:573` | `See:` line add `, ADR-034` |
| DOC-08 | `docs/operator/secret-scanning.md:83` | drop `in v0.1` qualifier |
| DOC-10 | `CHANGELOG.md` | adjust "(through ADR-034)" phrasing |

### Stream 1B — Multi-line doc rewrites (one PR, ~1 hr)

Bundle: `docs: refresh v1.0 docs against ADR-034`.

| Gap | File | Change |
|-----|------|--------|
| DOC-03 | `docs/loomweave/1.0/requirements.md:771-783` | Rewrite NFR-SEC-03 statement + verification for ADR-034 rules |
| DOC-04 | `docs/loomweave/1.0/requirements.md:558-573` | Rewrite REQ-HTTP-03 statement + verification |
| DOC-06 | `docs/suite/weft.md:65-70`, `CHANGELOG.md:108` | "v0.1 asterisks" → "v1.0 asterisks"; "deferred to v0.2" → "deferred to v1.1" |
| DOC-07 | `CLAUDE.md:144` | Rewrite HMAC paragraph; HMAC is preferred in 1.0 |
| DOC-09 | `CHANGELOG.md` (known limitations) | Add Wardline REGISTRY asterisk entry |

### Stream 1C — New operator docs (one PR, ~1.5 hr)

Bundle: `docs(operator): pre-tag-cut governance + storage docs`.

| Gap | New file | Content |
|-----|----------|---------|
| GOV-03 | `docs/operator/v1.0-release-rollback.md` | Rollback / yank runbook (see gap register) |
| DOC-11 | append §Storage section to `README.md` and / or new `docs/loomweave/1.0/operations.md` | Deployment constraints (NFS prohibition, no double-analyze, backup procedure) |
| SEC-02 | append section to `docs/operator/secret-scanning.md` and `docs/operator/loomweave-http-read-api.md` | Loopback-no-token trust statement |
| SEC-03 | append to `docs/operator/secret-scanning.md` and CHANGELOG known-limits | Pre-WP5 catalogue upgrade requirement |

### Stream 1D — One-line code fixes (one PR, ~1 hr including tests)

Bundle: `fix(v1.0): pre-tag correctness fixes`.

| Gap | File | Change |
|-----|------|--------|
| SEC-01 | `crates/loomweave-storage/src/query.rs:296-302` | Fail-closed `entity_briefing_block_reason`; add malformed-JSON unit test |
| CI-02 | `crates/loomweave-cli/src/http_read.rs:426-431` | Use a non-`InvalidPath` error code on body-parse failure; add a test exercising oversized body |
| SEC-02 (code half) | `crates/loomweave-mcp/src/config.rs` and / or `crates/loomweave-cli/src/serve.rs` startup | Emit explicit startup banner line when loopback-no-token mode is in effect |

### Stream 1E — Filigree issue creation (one batch, ~30 min)

Create one Filigree issue per gap, labelled `release:v1.0`. File the v1.1
backlog items with `release:v1.1`. See [`filigree-issue-bodies.md`](filigree-issue-bodies.md).

### Day 1 exit

- 4 PRs (1A–1D) green in CI.
- All Filigree issues filed and labelled.
- `git log` shows no remaining R3/D1–D10/SEC-01/CI-02 gaps.

---

## Day 2 — Storage + CI hardening (sequential review)

**Owner**: engineering. Single reviewer for the storage work.
**Duration**: 4–5 hours.
**Exit**: storage and CI gaps closed; `walking-skeleton` job green with
new gates wired.

### 2.1 — Storage cross-process safety (STO-01, ~2 hr)

PR: `fix(storage): cross-process lock for loomweave analyze`.

- Add `fs2 = "0.4"` to `crates/loomweave-cli/Cargo.toml`.
- At top of `analyze::run` (and `serve` write paths), open
  `.loomweave/loomweave.lock`, call `try_lock_exclusive`. Hold for the
  duration of the writer-actor lifetime.
- Refuse a second concurrent invocation with a clear error message:
  "another `loomweave analyze` is in progress against this project".
- Add a test that spawns two `loomweave analyze` subprocesses against the
  same project root and asserts exactly one succeeds.
- Update `docs/loomweave/1.0/operations.md` (or the README §Storage)
  paragraph from Stream 1C to reference the fail-fast behaviour.

### 2.2 — Storage identity + integrity (STO-02, STO-04, ~1 hr)

PR: `fix(storage): application_id PRAGMA + e2e integrity check`.

- Add `PRAGMA application_id = 0x434C524E` to
  `crates/loomweave-storage/src/pragma.rs::apply_write_pragmas`.
- On open, assert `application_id == 0` (then set) or `0x434C524E`
  (recognise); refuse any other value with a clear error.
- Add `PRAGMA integrity_check` final assertion to
  `tests/e2e/sprint_1_walking_skeleton.sh`. Fail the script if
  output is not exactly `ok`.
- Unit test for the application_id assertion path.

### 2.3 — CI wiring (TEST-01, TEST-02, CI-01, ~1 hr)

PR: `ci: wire MCP surface, Phase 3, and tag-lineage gates`.

- Add steps to `walking-skeleton` job in `.github/workflows/ci.yml`:
  ```yaml
  - name: Sprint 2 MCP surface
    run: bash tests/e2e/sprint_2_mcp_surface.sh
  - name: Phase 3 subsystem clustering determinism
    run: bash tests/e2e/phase3_subsystems.sh
  ```
- Mirror in `release.yml verify` job.
- Add ancestor check to `release.yml verify`:
  ```yaml
  - name: Assert tagged commit is on main
    run: |
      git fetch origin main
      git merge-base --is-ancestor "$GITHUB_SHA" origin/main || \
        { echo "::error::tag does not point to a commit on main"; exit 1; }
  ```
- Verify both scripts pass under the CI environment before merging.

### 2.4 — SLSA coverage for Python sdist (CI-03, ~30 min)

PR: `ci: extend SLSA provenance to plugin sdist`.

- Edit `release-subjects` step in `.github/workflows/release.yml:201-225`
  to glob `loomweave-*.tar.gz` + `loomweave_plugin_python*.tar.gz`.
- Update release notes template to mention plugin-sdist provenance.

### 2.5 — Post-publish verification (CI-04, ~45 min, optional for v1.0)

Recommended as a v1.0 Critical-but-not-blocking; if time is tight,
defer to v1.1 with a documented gap in the rollback runbook.

PR: `ci: verify published release matches signed artifacts`.

- Add `verify-published-release` job after `create-release` that
  downloads from the public Release URL, recomputes SHA256, and
  cosign-verifies against public Rekor entries.

### Day 2 exit

- All Critical / High storage and CI gaps closed except STO-03
  (deferred to tag-cut moment) and CI-04 (optional).
- CI floor on `main` passes including new gates.

---

## Day 3 — Operator: governance, smoke, tag-cut

**Owner**: operator (single human, no parallelism).
**Duration**: 3–4 hours.
**Exit**: `v1.0.0` tag exists on `main`, GitHub Release published,
artifacts smoke-tested.

### 3.1 — Enable GitHub controls (GOV-01, GOV-02, ~30 min)

- Configure `RELEASE_GOVERNANCE_TOKEN` repository Actions secret with
  read access to branch-protection, ruleset, and Actions policy
  settings.
- Enable a ruleset targeting `main` that requires PR flow and these
  three required CI checks: `Rust`, `Python plugin`, `Sprint 1 walking
  skeleton (end-to-end)`. (After Day 2.3, optionally add the new MCP
  surface + Phase 3 checks if they appear as separate Check Runs.)
- Enable a ruleset targeting `refs/tags/v*` restricting tag creators
  to repository owner (or write-to-`main` set).
- Enable a constrained Actions source policy (SHA-pinning required,
  or allow-list).

### 3.2 — Live governance dry-run (~15 min)

- Run `scripts/check-github-release-governance.py --repository
  foundryside-dev/loomweave --branch main` locally with a token that can
  read the policy settings. Confirm exit 0.

### 3.3 — Merge the gap-closure PRs to main (~15 min)

- Review and merge the Day 1 + Day 2 PR stack into `main` (or merge
  PR #12 if Option B was chosen, with the gap-closure work cherry-picked
  on top).

### 3.4 — Release workflow dry-run (~15 min)

- `gh workflow run Release --ref main` (workflow_dispatch).
- Verify all jobs green; verify artifacts produced; verify no public
  Release is created (the workflow conditions on `event_name == 'push'`
  for the publish step).

### 3.5 — External-operator smoke (GOV-04, ~2 hr)

- Provision a fresh Linux x86_64 VM and a fresh macOS host (or use a
  CI runner that satisfies "outside-operator" semantics).
- Run `tests/e2e/external-operator-smoke.md` checklist on each.
- Record results in
  `docs/implementation/v1.0-tag-cut/external-operator-smoke-result.md`
  with timestamp, host OS+arch, install command, "improvisation events"
  count (target: 0), and an attestation signature.
- Commit and push to `main`.

### 3.6 — Filigree F-1 lockstep check (~5 min)

- Confirm Filigree's F-1 (`clarion-cd21b98463` — registry-backend
  consumer rename) status against the 2026-05-19 coordinator memo. If
  F-1 has merged and uses the old field name, halt — fix in Filigree
  first.

### 3.7 — Commit `published_build.txt` (STO-03, ~5 min)

- The final pre-tag commit on `main`:
  ```bash
  echo "$(git rev-parse HEAD)" > crates/loomweave-storage/migrations/published_build.txt
  git add crates/loomweave-storage/migrations/published_build.txt
  git commit -m "release: mark v1.0.0 as published-build baseline"
  git push origin main
  ```

### 3.8 — Cut the tag (~5 min)

- `git tag -a v1.0.0 -m "Loomweave v1.0.0"`
- `git push origin v1.0.0`
- The release workflow fires on the push. Watch the `verify` →
  `release-governance` → `build-rust` / `build-plugin` → `release` →
  `provenance` chain.

### 3.9 — Public artifact smoke (~30 min)

- Once the release is public, install Loomweave from the
  GitHub-Release-hosted assets on a fresh host (or one of the
  external-operator smoke VMs).
- Verify cosign signatures via `cosign verify-blob` with the
  Rekor entry.
- Verify SLSA provenance via `slsa-verifier`.
- Run the walking-skeleton against a small Python corpus.
- Record success or initiate the rollback runbook.

### Day 3 exit

- `v1.0.0` tag pushed, Release public, artifacts verified.
- This document and the gap register move to historical-anchor status
  (no edits except status-flip on the exit-criteria checklist).

---

## Risk mitigation

**If anything goes wrong on Day 3**:
- The rollback runbook (GOV-03) covers the h+30min "bad release"
  scenario. The first action is always
  `gh release edit v1.0.0 --prerelease`.
- The release workflow does not delete or modify the tag, so the
  worst-case is an orphaned release with assets that need re-pointing.

**If Day 2 storage work uncovers more issues**:
- The fs2 lock is the load-bearing change. If it breaks an existing
  test, the issue is likely in `serve` write paths (which we may need
  to scope more narrowly than `analyze`). Surface immediately; do not
  merge the PR.

**If governance check fails after Day 3.1**:
- The runbook at `docs/operator/v1.0-release-governance.md` covers the
  configuration steps. If the script still fails after applying them,
  the failure mode is documented in
  `scripts/check-github-release-governance.py` exit codes.

## Confidence

This plan is grounded in the seven-pass deep dive. Effort estimates are
conservative — most Day 1 streams should complete in less than the
quoted time. The main schedule risk is Day 3.5 (external-operator
smoke), which depends on VM provisioning time and the operator's
availability.
