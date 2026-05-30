#!/usr/bin/env python3
"""Cross-workspace version lockstep guard.

Asserts that the Rust workspace package version
(`Cargo.toml [workspace.package].version`) matches the Python plugin's
`[project].version` (`plugins/python/pyproject.toml`). The Python plugin
manifest and `__init__.__version__` are already cross-checked by
`plugins/python/tests/test_package.py`; this script catches the wider
ecosystem-level drift (someone bumps Cargo but not the Python plugin, or
vice versa) before a release tag is cut.

Designed to run as a CI step on every PR — fast, no third-party deps,
parses with the stdlib `tomllib` (Python 3.11+).

Exit codes:
    0  versions agree
    1  versions disagree, or required keys are missing
    2  usage / I/O error (e.g. file not found)

The acceptable-drift policy in v1.0 is "no drift": the Rust workspace and
Python plugin ship as a single product version. Post-1.0 patch releases
that need divergent semver should drop the strict-equality check and
replace it with a compatibility-bound check in the same script.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
CARGO_TOML = REPO_ROOT / "Cargo.toml"
PYPROJECT_TOML = REPO_ROOT / "plugins/python/pyproject.toml"


def _read_toml(path: Path) -> dict[str, Any]:
    if not path.is_file():
        print(f"check-workspace-version-lockstep: missing {path}", file=sys.stderr)
        sys.exit(2)
    return tomllib.loads(path.read_text(encoding="utf-8"))


def _get_path(data: dict[str, Any], *keys: str) -> Any:
    cursor: Any = data
    for key in keys:
        if not isinstance(cursor, dict) or key not in cursor:
            path_repr = ".".join(keys)
            print(
                f"check-workspace-version-lockstep: key {path_repr!r} not found",
                file=sys.stderr,
            )
            sys.exit(1)
        cursor = cursor[key]
    return cursor


def main() -> int:
    cargo = _read_toml(CARGO_TOML)
    pyproject = _read_toml(PYPROJECT_TOML)

    rust_version = _get_path(cargo, "workspace", "package", "version")
    python_version = _get_path(pyproject, "project", "version")

    if rust_version != python_version:
        print(
            "check-workspace-version-lockstep: drift detected",
            file=sys.stderr,
        )
        print(f"  Cargo.toml [workspace.package].version = {rust_version!r}", file=sys.stderr)
        print(f"  plugins/python/pyproject.toml [project].version = {python_version!r}", file=sys.stderr)
        print(
            "Bump them in lockstep, or split this guard into compatibility-bounded "
            "ranges if v1.0+ wants divergent semver.",
            file=sys.stderr,
        )
        return 1

    print(f"check-workspace-version-lockstep: ok ({rust_version})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
