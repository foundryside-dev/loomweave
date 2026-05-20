# RC1 Release Readiness

## Verdict

RC1 is a plausible release candidate, not a release-ready commit.

The code structure, release workflow shape, and federation contract are strong.
The remaining blockers are primarily policy/evidence alignment rather than
missing core architecture.

## Ready Signals

- v1.0 distribution path is decided: GitHub Releases, not crates.io/PyPI.
- Release workflow supports `v*` tags and dry-run `workflow_dispatch`.
- Build jobs depend on verification and release-governance jobs.
- Release artifacts include checksums, cosign signing, and SLSA provenance.
- Federation HTTP read contract has docs, fixtures, and integration tests.
- Secret scanner has unit, CLI, and E2E coverage.
- Subsystem clustering and `subsystem_members` are present in the current
  code/doc shape.

## Blocking Or Pre-Tag Items

| Severity | Item | Reason |
|---|---|---|
| Blocker | Live GitHub governance must pass. | Latest readiness snapshot documented permissive Actions, unprotected `main`, and no rulesets. |
| Blocker | Required E2E gate classification must be resolved. | Local instructions list Sprint 1, Sprint 2 MCP, and Phase 3 scripts; workflows do not run all of them. |
| High | `CHANGELOG.md` auth code mismatch. | Public changelog says `UNAUTHORIZED`; contract/implementation use `UNAUTHENTICATED`. |
| High | External-operator smoke evidence missing. | Checklist exists; no dated result artifact found. |
| Medium | Duplicate version/policy facts need drift tests. | Pyright pin, Wardline bounds, and cap semantics are drift-prone. |

## Recommended Release Sequence

1. Fix the changelog auth-code mismatch.
2. Decide whether missing E2E scripts become required jobs or manually
   documented release gates.
3. Produce external-operator smoke result artifact.
4. Run full local CI floor.
5. Run live GitHub release-governance guard.
6. Run release workflow dry run from `main`.
7. Cut tag only after the release commit has full evidence.
8. Smoke-test public GitHub Release artifacts.

## Evidence Still Needed

- Live GitHub policy output from the governance guard.
- Full cargo/Python/E2E gate results from the release commit.
- Release dry-run output.
- External operator smoke result.
- Public artifact smoke result after tag.

## Release Decision

Hold tag. Continue RC1 hardening.
