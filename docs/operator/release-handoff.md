# Release Handoff

Clarion no longer owns live GitHub branch/ruleset release-control enforcement.

The old v1.0 guard required Clarion's release workflow to inspect repository
branch protection, tag rulesets, Actions policy, and a dedicated maintainer
secret before build jobs could start. That requirement is retired for Clarion.
Governance enforcement is moving to Legis, which is delivered alongside the
next Clarion release.

Clarion's standalone release semantics are now:

- the release workflow verifies the source tree, builds artifacts, and publishes
  GitHub Releases for `v*` tags;
- Clarion does not require live repository ruleset inspection to build or
  publish its own artifacts;
- any Legis governance signal is external enrichment and must not become a
  prerequisite for Clarion to remain useful alone.

Keep future Clarion release controls local to Clarion's own artifact integrity
unless a new accepted ADR says otherwise. If Legis publishes an attestation for
the same release train, link it from release notes or operator handoff material
without adding a mandatory sibling dependency to Clarion's release workflow.

## Current Release Sequence

1. Confirm the release branch or PR is green under Clarion's CI floor.
2. Run `.github/workflows/release.yml` with `workflow_dispatch` from the release
   commit to validate the build and artifact path.
3. Push the `v*` tag from the reviewed release commit.
4. After the tag workflow finishes, download the public artifacts from the
   GitHub Release and repeat the
   [getting started walkthrough](./getting-started.md) on a clean machine or VM.

## See Also

- [`v1.0-release-rollback.md`](./v1.0-release-rollback.md) - post-publish
  incident runbook.
