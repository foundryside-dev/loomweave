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
