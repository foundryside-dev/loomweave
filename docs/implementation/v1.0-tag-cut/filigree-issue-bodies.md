# Filigree Issue Bodies — v1.0 Tag-Cut Gaps

Pre-drafted Filigree issues for every gap in [`gap-register.md`](gap-register.md).
Every issue cites the gap register as its source of truth so the issue body
can stay terse and the register stays canonical.

**Filing convention**: every v1.0 issue gets `release:v1.0` + the
priority-appropriate `priority:Pn` label + a `category:<cat>` label. The
v1.1 backlog items get `release:v1.1` + the same category labels.

## v1.0 issues

### GOV-01 — Live GitHub governance is permissive
- **title**: `[v1.0 blocker] Enable live GitHub governance: branch protection, ruleset, RELEASE_GOVERNANCE_TOKEN`
- **priority**: P1
- **type**: feature
- **labels**: `release:v1.0`, `category:governance`, `tier:a`
- **body**:
  > Closes GOV-01 in `docs/implementation/v1.0-tag-cut/gap-register.md`.
  >
  > Operator action: configure live GitHub controls per
  > `docs/operator/v1.0-release-governance.md`. Exit criterion is
  > `scripts/check-github-release-governance.py` exits 0.
- **owner**: operator

### GOV-02 — No tag protection rule
- **title**: `[v1.0 blocker] Add ruleset for refs/tags/v* and tag-protection check to governance script`
- **priority**: P1, **type**: feature, **labels**: `release:v1.0`, `category:governance`, `tier:a`
- **body**: Closes GOV-02 in the gap register. Two-part: (1) operator enables ruleset on `refs/tags/v*`; (2) engineering extends `scripts/check-github-release-governance.py` to assert tag protection.

### GOV-03 — Rollback / yank runbook
- **title**: `[v1.0 blocker] Write rollback/yank runbook for bad-release scenario`
- **priority**: P1, **type**: docs, **labels**: `release:v1.0`, `category:governance`
- **body**: Closes GOV-03. Add `docs/operator/v1.0-release-rollback.md`. See gap register for required sections.

### GOV-04 — External-operator smoke evidence
- **title**: `[v1.0 blocker] Run external-operator smoke on fresh VMs and archive dated result`
- **priority**: P1, **type**: task, **labels**: `release:v1.0`, `category:governance`
- **body**: Closes GOV-04. Result archived at `docs/implementation/v1.0-tag-cut/external-operator-smoke-result.md`.

### DOC-01 — CHANGELOG UNAUTHORIZED → UNAUTHENTICATED
- **title**: `[v1.0 blocker] Fix CHANGELOG error-code UNAUTHORIZED → UNAUTHENTICATED`
- **priority**: P1, **type**: bug, **labels**: `release:v1.0`, `category:docs`
- **body**: Closes DOC-01. One-line edit at `CHANGELOG.md:60`.

### DOC-02 — Add BATCH_TOO_LARGE to CHANGELOG error enum
- **title**: `[v1.0 blocker] Add BATCH_TOO_LARGE to CHANGELOG error-enum list`
- **priority**: P1, **type**: bug, **labels**: `release:v1.0`, `category:docs`
- **body**: Closes DOC-02. Edit `CHANGELOG.md:60`.

### DOC-03 — NFR-SEC-03 stale post-ADR-034
- **title**: `[v1.0 blocker] Refresh NFR-SEC-03 for ADR-034 authenticated-non-loopback rule`
- **priority**: P1, **type**: docs, **labels**: `release:v1.0`, `category:docs`, `adr:034`
- **body**: Closes DOC-03. Rewrite at `docs/clarion/1.0/requirements.md:771-783`.

### DOC-04 — REQ-HTTP-03 stale post-ADR-034
- **title**: `[v1.0 blocker] Refresh REQ-HTTP-03 for ADR-034 authenticated-non-loopback rule`
- **priority**: P1, **type**: docs, **labels**: `release:v1.0`, `category:docs`, `adr:034`
- **body**: Closes DOC-04. Rewrite at `docs/clarion/1.0/requirements.md:558-573`.

### DOC-05 — REQ-HTTP-03 See line missing ADR-034
- **title**: `[v1.0] Add ADR-034 to REQ-HTTP-03 See line`
- **priority**: P2, **type**: docs, **labels**: `release:v1.0`, `category:docs`
- **body**: Closes DOC-05. One-line edit.

### DOC-06 — loom.md "v0.1 asterisks" labels
- **title**: `[v1.0 blocker] Rename loom.md "v0.1 asterisks" to "v1.0 asterisks"; CHANGELOG "deferred to v0.2" → "v1.1"`
- **priority**: P1, **type**: docs, **labels**: `release:v1.0`, `category:docs`
- **body**: Closes DOC-06.

### DOC-07 — CLAUDE.md HMAC misrepresentation
- **title**: `[v1.0 blocker] Update CLAUDE.md: HMAC ships in 1.0 per ADR-034, not "post-1.0 hardening"`
- **priority**: P1, **type**: docs, **labels**: `release:v1.0`, `category:docs`, `adr:034`
- **body**: Closes DOC-07. Edit `CLAUDE.md:144`.

### DOC-08 — secret-scanning.md v0.1 reference
- **title**: `[v1.0] Drop v0.1 reference from docs/operator/secret-scanning.md`
- **priority**: P2, **type**: docs, **labels**: `release:v1.0`, `category:docs`
- **body**: Closes DOC-08. One-line edit.

### DOC-09 — CHANGELOG Wardline REGISTRY asterisk
- **title**: `[v1.0] Add Wardline REGISTRY import to CHANGELOG known limitations`
- **priority**: P2, **type**: docs, **labels**: `release:v1.0`, `category:docs`
- **body**: Closes DOC-09.

### DOC-10 — CHANGELOG ADR-count phrasing
- **title**: `[v1.0] Adjust CHANGELOG ADR-count "(through ADR-034)" phrasing`
- **priority**: P3, **type**: docs, **labels**: `release:v1.0`, `category:docs`
- **body**: Closes DOC-10. Cosmetic.

### DOC-11 — Storage operator constraints in README
- **title**: `[v1.0 blocker] Document storage deployment constraints (NFS, double-analyze, backup)`
- **priority**: P1, **type**: docs, **labels**: `release:v1.0`, `category:docs`, `category:storage`
- **body**: Closes DOC-11. New `docs/clarion/1.0/operations.md` or README §Storage.

### SEC-01 — entity_briefing_block_reason fail-open
- **title**: `[v1.0 blocker] Fail-closed: entity_briefing_block_reason on malformed properties JSON`
- **priority**: P1, **type**: bug, **labels**: `release:v1.0`, `category:security`, `crate:storage`
- **body**: Closes SEC-01. One-line code change at `crates/clarion-storage/src/query.rs:296-302` plus malformed-JSON unit test.

### SEC-02 — Loopback-no-token trust assumption
- **title**: `[v1.0 blocker] Document loopback-no-token trust + emit startup banner`
- **priority**: P1, **type**: feature, **labels**: `release:v1.0`, `category:security`, `category:docs`
- **body**: Closes SEC-02. Operator-doc additions + startup-banner warning.

### SEC-03 — Pre-WP5 catalogue upgrade requirement
- **title**: `[v1.0 blocker] Document pre-WP5 .clarion/ upgrade requirement (re-analyze needed)`
- **priority**: P1, **type**: docs, **labels**: `release:v1.0`, `category:security`, `category:docs`
- **body**: Closes SEC-03.

### CI-01 — Tag-lineage in-workflow check
- **title**: `[v1.0 blocker] Add ancestor-of-main check to release.yml verify`
- **priority**: P1, **type**: feature, **labels**: `release:v1.0`, `category:ci`
- **body**: Closes CI-01. Workflow edit.

### CI-02 — Federation error code on body parse failure
- **title**: `[v1.0 blocker] Fix HMAC body-parse error code (currently returns InvalidPath)`
- **priority**: P1, **type**: bug, **labels**: `release:v1.0`, `category:federation`, `crate:cli`
- **body**: Closes CI-02. Edit `crates/clarion-cli/src/http_read.rs:426-431`.

### CI-03 — SLSA coverage for Python sdist
- **title**: `[v1.0 blocker] Extend SLSA provenance to Python plugin sdist`
- **priority**: P1, **type**: feature, **labels**: `release:v1.0`, `category:ci`
- **body**: Closes CI-03. Edit `release-subjects` step.

### CI-04 — Post-publish artifact verification
- **title**: `[v1.0] Verify-published-release job after create-release`
- **priority**: P2, **type**: feature, **labels**: `release:v1.0`, `category:ci`
- **body**: Closes CI-04. Optional for v1.0; defer to v1.1 if time-constrained.

### STO-01 — Cross-process lock
- **title**: `[v1.0 blocker] fs2 advisory lock for clarion analyze (prevent concurrent corruption)`
- **priority**: P1, **type**: bug, **labels**: `release:v1.0`, `category:storage`, `crate:cli`, `crate:storage`
- **body**: Closes STO-01. Add fs2 + lock + test.

### STO-02 — PRAGMA application_id
- **title**: `[v1.0 blocker] Set PRAGMA application_id on writer open`
- **priority**: P1, **type**: bug, **labels**: `release:v1.0`, `category:storage`, `crate:storage`
- **body**: Closes STO-02.

### STO-03 — published_build.txt marker
- **title**: `[v1.0 blocker] Commit migrations/published_build.txt at v1.0 tag-cut`
- **priority**: P1, **type**: task, **labels**: `release:v1.0`, `category:storage`
- **body**: Closes STO-03. Final pre-tag commit on `main`.

### STO-04 — Backup + integrity_check
- **title**: `[v1.0 blocker] PRAGMA integrity_check in e2e + documented backup procedure`
- **priority**: P1, **type**: feature, **labels**: `release:v1.0`, `category:storage`
- **body**: Closes STO-04.

### STO-05 — Recovery liveness guard
- **title**: `[v1.0] Document run-recovery race; defer schema columns to v1.1`
- **priority**: P2, **type**: docs, **labels**: `release:v1.0`, `category:storage`
- **body**: Closes STO-05 doc-only at v1.0. v1.1 issue tracks the heartbeat columns separately.

### TEST-01 — Wire sprint_2_mcp_surface.sh
- **title**: `[v1.0 blocker] Add sprint_2_mcp_surface.sh to CI walking-skeleton job`
- **priority**: P1, **type**: feature, **labels**: `release:v1.0`, `category:ci`, `category:tests`
- **body**: Closes TEST-01.

### TEST-02 — Wire phase3_subsystems.sh
- **title**: `[v1.0 blocker] Add phase3_subsystems.sh to CI walking-skeleton job`
- **priority**: P1, **type**: feature, **labels**: `release:v1.0`, `category:ci`, `category:tests`
- **body**: Closes TEST-02.

---

## v1.1 backlog issues

(File these before tag-cut so post-1.0 work is tracked.)

### V11-ARCH-01 — Extract clarion-core::errors shared error vocabulary
- priority: P2, type: feature, labels: `release:v1.1`, `category:architecture`
- body: deep-dive-arch v1.1 priority #1. Closes the MCP/HTTP error-code drift smell.

### V11-ARCH-02 — Split analyze.rs run_with_options
- priority: P2, type: feature, labels: `release:v1.1`, `category:architecture`, `crate:cli`
- body: deep-dive-arch v1.1 priority #2. Extract `analyze/phase3.rs` and `analyze/mapping.rs`. Removes the `#[allow(too_many_lines)]`.

### V11-ARCH-03 — Split llm_provider.rs per provider
- priority: P2, type: feature, labels: `release:v1.1`, `category:architecture`, `crate:core`
- body: deep-dive-arch v1.1 priority #3. `openrouter.rs` / `cli_provider.rs` (Codex + Claude on shared base) / `prompts.rs`.

### V11-ARCH-04 — Split clarion-mcp/src/lib.rs into tools/ subdir
- priority: P3, type: feature, labels: `release:v1.1`, `category:architecture`, `crate:mcp`
- body: deep-dive-arch v1.1 priority #4.

### V11-ARCH-05 — Split plugin/host.rs validation from transport
- priority: P3, type: feature, labels: `release:v1.1`, `category:architecture`, `crate:core`
- body: deep-dive-arch v1.1 priority #5.

### V11-SEC-01 — Replace local HMAC with hmac + subtle crates
- priority: P2, type: feature, labels: `release:v1.1`, `category:security`, `crate:cli`
- body: deep-dive-security recommendation. If schedule allows, ship in v1.0 instead.

### V11-SEC-02 — HMAC replay protection (timestamp + nonce window)
- priority: P2, type: feature, labels: `release:v1.1`, `category:security`
- body: Called out in ADR-034 forward-work. Required when non-loopback bind becomes common.

### V11-SEC-03 — Mandatory auth on loopback binds
- priority: P3, type: feature, labels: `release:v1.1`, `category:security`
- body: Doctrine change closing T-9. Breaks the documented v0.1 trust matrix.

### V11-STO-01 — runs.owner_pid + heartbeat_at columns
- priority: P2, type: feature, labels: `release:v1.1`, `category:storage`
- body: deep-dive-db. Schema-additive 0002_*; refine recovery WHERE-clause.

### V11-STO-02 — clarion db backup subcommand
- priority: P2, type: feature, labels: `release:v1.1`, `category:storage`, `crate:cli`
- body: deep-dive-db. `rusqlite::backup::Backup`-based.

### V11-STO-03 — summary_cache.entity_id FK
- priority: P2, type: bug, labels: `release:v1.1`, `category:storage`
- body: deep-dive-db. Confirmed bug, not intentional. Requires table-rebuild migration.

### V11-STO-04 — briefing_blocked generated column + index
- priority: P2, type: feature, labels: `release:v1.1`, `category:storage`
- body: Federation read-API hot path.

### V11-STO-05 — BEGIN IMMEDIATE + SQLITE_BUSY retry helper
- priority: P3, type: feature, labels: `release:v1.1`, `category:storage`
- body: Matters once V11-STO-01 cross-process work lands.

### V11-STO-06 — FTS5 content_text decision
- priority: P3, type: feature, labels: `release:v1.1`, `category:storage`
- body: Drop or populate the dead-schema column.

### V11-STO-07 — ReaderPool eager validation
- priority: P3, type: feature, labels: `release:v1.1`, `category:storage`
- body: `clarion serve` boot should fail-fast on bad DB.

### V11-STO-08 — briefing_blocked typed column
- priority: P3, type: feature, labels: `release:v1.1`, `category:storage`
- body: Promote from JSON property. Closes a class of drift.

### V11-CI-01 — Reusable verify workflow
- priority: P2, type: feature, labels: `release:v1.1`, `category:ci`
- body: Eliminate ci.yml/release.yml drift risk.

### V11-CI-02 — pip-audit in build-plugin
- priority: P3, type: feature, labels: `release:v1.1`, `category:ci`
- body: Python supply chain.

### V11-CI-03 — workflow_permissions + tag-protection in governance script
- priority: P3, type: feature, labels: `release:v1.1`, `category:ci`
- body: Extend `check-github-release-governance.py`.

### V11-CI-04 — macOS Gatekeeper workaround doc
- priority: P3, type: docs, labels: `release:v1.1`, `category:docs`
- body: In `getting-started.md`.

### V11-CI-05 — SBOM emission
- priority: P3, type: feature, labels: `release:v1.1`, `category:ci`
- body: cyclonedx-bom or syft.

### V11-TEST-01 — pyright-langserver hard-fail in CI
- priority: P2, type: bug, labels: `release:v1.1`, `category:tests`
- body: deep-dive-quality. Currently silent-skip if missing.

### V11-TEST-02 — Pyright pin lockstep script
- priority: P3, type: feature, labels: `release:v1.1`, `category:tests`
- body: deep-dive-quality provided concrete script. Adds drift defence.

### V11-TEST-03 — Wardline version-bounds validation script
- priority: P3, type: feature, labels: `release:v1.1`, `category:tests`
- body: deep-dive-quality concrete script.

### V11-TEST-04 — EntityCountCap ADR/code lockstep script
- priority: P3, type: feature, labels: `release:v1.1`, `category:tests`
- body: deep-dive-quality concrete script.
