#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

cargo build --workspace --bins
uv sync --project plugins/python --locked --extra dev
# `uv sync` does not refresh Hatch shared-data when the editable package's
# version is unchanged. Force the installed plugin.toml to match this checkout.
uv pip install --python plugins/python/.venv/bin/python --reinstall --no-deps -e plugins/python

test -x target/debug/loomweave
test -x target/debug/loomweave-rust-plugin
test -x plugins/python/.venv/bin/loomweave-plugin-python

PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate-federation-seam-goldens.py
