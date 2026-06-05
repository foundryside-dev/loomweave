#!/usr/bin/env python3
"""Cross-workspace version lockstep guard.

Asserts that every package that ships as part of the Loomweave 1.0 product
carries the *same* version as the Rust workspace
(`Cargo.toml [workspace.package].version`):

  * `plugins/python/pyproject.toml`        `[project].version`
  * `crates/loomweave-cli/pyproject.toml`  `[project].version`  (maturin bin-wheel)

and that the maturin bin-wheel pins the plugin at exactly the workspace
version:

  * `crates/loomweave-cli/pyproject.toml`  `[project].dependencies`
        must contain `loomweave-plugin-python==<workspace version>`

The `loomweave` wheel depends on `loomweave-plugin-python==<v>`; if that pin
ever drifts from the version actually published, the `==` requirement fails
to resolve on first release. This guard catches that before a tag is cut.

The Python plugin manifest and `__init__.__version__` are already
cross-checked by `plugins/python/tests/test_package.py`; this script catches
the wider ecosystem-level drift.

Designed to run as a CI step on every PR — fast, no third-party deps, parses
with the stdlib `tomllib` (Python 3.11+).

Usage:
    check-workspace-version-lockstep.py            # check the live repo
    check-workspace-version-lockstep.py --self-test  # run built-in fixtures

Exit codes:
    0  versions agree (or --self-test passed)
    1  versions disagree, a required pin is missing, or --self-test failed
    2  usage / I/O error (e.g. file not found)

The acceptable-drift policy in v1.0 is "no drift": every component ships as a
single product version. Post-1.0 patch releases that need divergent semver
should replace the strict-equality checks with compatibility-bound checks in
the same script.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
CARGO_TOML = REPO_ROOT / "Cargo.toml"
PLUGIN_PYPROJECT_TOML = REPO_ROOT / "plugins/python/pyproject.toml"
CLI_PYPROJECT_TOML = REPO_ROOT / "crates/loomweave-cli/pyproject.toml"

PLUGIN_PACKAGE = "loomweave-plugin-python"


class _Missing(Exception):
    """A required TOML key was absent."""


def _read_toml(path: Path) -> dict[str, Any]:
    if not path.is_file():
        print(f"check-workspace-version-lockstep: missing {path}", file=sys.stderr)
        sys.exit(2)
    return tomllib.loads(path.read_text(encoding="utf-8"))


def _dig(data: dict[str, Any], *keys: str) -> Any:
    cursor: Any = data
    for key in keys:
        if not isinstance(cursor, dict) or key not in cursor:
            raise _Missing(".".join(keys))
        cursor = cursor[key]
    return cursor


def _pinned_version(dependencies: Any, package: str) -> str | None:
    """Return the `==`-pinned version for `package` in a PEP 508 dependency list.

    Returns `None` if the package is absent or not pinned with `==`.
    """
    if not isinstance(dependencies, list):
        return None
    needle = f"{package}=="
    for dep in dependencies:
        if not isinstance(dep, str):
            continue
        norm = dep.replace(" ", "")
        if norm.startswith(needle):
            return norm[len(needle) :]
    return None


def check_lockstep(
    cargo: dict[str, Any],
    plugin_pyproject: dict[str, Any],
    cli_pyproject: dict[str, Any],
) -> list[str]:
    """Return a list of drift errors. An empty list means everything is in lockstep."""
    errors: list[str] = []

    try:
        rust_version = _dig(cargo, "workspace", "package", "version")
    except _Missing as missing:
        # Without the anchor version there is nothing to compare against.
        return [f"Cargo.toml key {missing} not found"]

    try:
        plugin_version = _dig(plugin_pyproject, "project", "version")
        if plugin_version != rust_version:
            errors.append(
                f"plugin version {plugin_version!r} != workspace {rust_version!r}"
            )
    except _Missing as missing:
        errors.append(f"plugins/python/pyproject.toml key {missing} not found")

    try:
        cli_version = _dig(cli_pyproject, "project", "version")
        if cli_version != rust_version:
            errors.append(
                f"loomweave-cli version {cli_version!r} != workspace {rust_version!r}"
            )
    except _Missing as missing:
        errors.append(f"crates/loomweave-cli/pyproject.toml key {missing} not found")

    try:
        deps = _dig(cli_pyproject, "project", "dependencies")
        pin = _pinned_version(deps, PLUGIN_PACKAGE)
        if pin is None:
            errors.append(
                f"loomweave-cli pyproject does not pin {PLUGIN_PACKAGE}==<version>"
            )
        elif pin != rust_version:
            errors.append(
                f"loomweave-cli pins {PLUGIN_PACKAGE}=={pin} != workspace {rust_version!r}"
            )
    except _Missing as missing:
        errors.append(f"crates/loomweave-cli/pyproject.toml key {missing} not found")

    return errors


def _self_test() -> int:
    """Exercise check_lockstep against in-memory fixtures."""
    cargo = tomllib.loads('[workspace.package]\nversion = "1.0.0"\n')

    def plugin(version: str) -> dict[str, Any]:
        return tomllib.loads(
            f'[project]\nname = "loomweave-plugin-python"\nversion = "{version}"\n'
        )

    def cli(version: str, deps: str) -> dict[str, Any]:
        return tomllib.loads(
            f'[project]\nname = "loomweave"\nversion = "{version}"\n{deps}\n'
        )

    good_deps = 'dependencies = ["loomweave-plugin-python==1.0.0"]'
    cases: list[tuple[str, dict[str, Any], dict[str, Any], bool]] = [
        ("aligned", plugin("1.0.0"), cli("1.0.0", good_deps), True),
        ("plugin version drift", plugin("1.0.1"), cli("1.0.0", good_deps), False),
        ("cli version drift", plugin("1.0.0"), cli("0.9.0", good_deps), False),
        (
            "cli pin drift",
            plugin("1.0.0"),
            cli("1.0.0", 'dependencies = ["loomweave-plugin-python==0.9.0"]'),
            False,
        ),
        (
            "cli pin absent",
            plugin("1.0.0"),
            cli("1.0.0", 'dependencies = ["something-else>=1"]'),
            False,
        ),
        (
            "cli pin unpinned (>=)",
            plugin("1.0.0"),
            cli("1.0.0", 'dependencies = ["loomweave-plugin-python>=1.0.0"]'),
            False,
        ),
    ]

    failures = 0
    for name, plugin_py, cli_py, expect_ok in cases:
        errors = check_lockstep(cargo, plugin_py, cli_py)
        actual_ok = not errors
        if actual_ok != expect_ok:
            failures += 1
            print(
                f"  SELF-TEST FAIL [{name}]: expected ok={expect_ok}, got {errors!r}",
                file=sys.stderr,
            )
        else:
            print(f"  self-test ok [{name}]")

    if failures:
        print(
            f"check-workspace-version-lockstep: --self-test FAILED ({failures})",
            file=sys.stderr,
        )
        return 1
    print("check-workspace-version-lockstep: --self-test passed")
    return 0


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return _self_test()

    cargo = _read_toml(CARGO_TOML)
    plugin_pyproject = _read_toml(PLUGIN_PYPROJECT_TOML)
    cli_pyproject = _read_toml(CLI_PYPROJECT_TOML)

    errors = check_lockstep(cargo, plugin_pyproject, cli_pyproject)
    if errors:
        print("check-workspace-version-lockstep: drift detected", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        print(
            "Bump every component in lockstep, or split this guard into "
            "compatibility-bounded ranges if v1.0+ wants divergent semver.",
            file=sys.stderr,
        )
        return 1

    rust_version = _dig(cargo, "workspace", "package", "version")
    print(f"check-workspace-version-lockstep: ok ({rust_version})")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
