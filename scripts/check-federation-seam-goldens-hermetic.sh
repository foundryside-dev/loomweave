#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

files=(
  crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json
  crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json.sha256
  docs/federation/fixtures/classification.python.json
  docs/federation/fixtures/classification.python.json.sha256
  docs/federation/fixtures/get-api-v1-capabilities.json
  docs/federation/fixtures/get-api-v1-capabilities.json.sha256
  docs/federation/fixtures/loomweave-http-auth-v1.golden.json
  docs/federation/fixtures/loomweave-http-auth-v1.golden.json.sha256
  docs/federation/fixtures/external-sqlite-compatibility-v1.json
  docs/federation/fixtures/external-sqlite-compatibility-v1.json.sha256
  docs/federation/fixtures/identity-ownership-v1.golden.json
  docs/federation/fixtures/identity-ownership-v1.golden.json.sha256
)

PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate-federation-seam-goldens.py
before="$(sha256sum "${files[@]}")"
WEFT_TOKEN=poison-must-not-affect-goldens \
  PYTHONDONTWRITEBYTECODE=1 \
  python3 scripts/generate-federation-seam-goldens.py
after="$(sha256sum "${files[@]}")"
test "$after" = "$before"
printf 'federation seam goldens ignore inherited WEFT_TOKEN: ok\n'

# Run-to-run equality above proves generation is hermetic; it does NOT prove the
# committed bytes still match what the producer emits. Both regenerations
# overwrite the fixtures in place, so a generator that has drifted from HEAD
# leaves the tree dirty and every other guard still passes: the bytes, the
# .sha256 sidecars and the authority-note rows are all derived from the same
# regenerated files, so they agree with each other while disagreeing with the
# committed golden. Assert tree cleanliness to close that loop.
if ! git diff --exit-code -- "${files[@]}"; then
  printf '\nfederation seam goldens are stale: the generator no longer reproduces\n'
  printf 'the committed bytes (diff above). Commit the regenerated fixtures, or\n'
  printf 'fix the producer if the change was unintended.\n' >&2
  exit 1
fi
printf 'federation seam goldens match the producer: ok\n'
