#!/usr/bin/env python3
"""Guard the pyright version pin against drift across its three homes.

The pinned pyright version is duplicated in three places that must stay in
lockstep or CI silently caches/type-checks against mismatched toolchains:

* plugins/python/pyproject.toml — the ``pyright==X`` dev-dependency pin that
  determines which pyright actually runs under ``mypy``/``pytest`` locally and
  in the python-plugin job.
* plugins/python/plugin.toml — ``[capabilities.runtime.pyright].pin``, the
  version the Rust host reads during discovery to know which langserver the
  plugin expects on PATH.
* .github/workflows/ci.yml — every ``pyright-python-<version>-...`` cache key.
  A stale key restores a wheel for the wrong version, so the cached runtime no
  longer matches the installed dev-dependency.

This guard keeps the duplication mechanical: closes V11-TEST-02 (see
docs/implementation/v1.0-tag-cut/gap-register.md).
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
import tomllib
from pathlib import Path

DEFAULT_PYPROJECT = Path("plugins/python/pyproject.toml")
DEFAULT_MANIFEST = Path("plugins/python/plugin.toml")
DEFAULT_CI = Path(".github/workflows/ci.yml")

# Matches a ``pyright==1.1.409`` style pin inside a dependency string.
_PYRIGHT_DEP_RE = re.compile(r"^pyright\s*==\s*(?P<version>[0-9][0-9A-Za-z.\-]*)\s*$")
# Matches every ``pyright-python-1.1.409`` cache-key stem in ci.yml.
_CI_CACHE_KEY_RE = re.compile(r"pyright-python-(?P<version>[0-9][0-9A-Za-z.\-]*?)-")


class CheckError(Exception):
    """Raised when the pyright-pin guard fails."""


def pyproject_pin(pyproject_path: Path) -> str:
    """Extract the ``pyright==X`` pin from the project dependency tables.

    Scans both ``[project].dependencies`` and every
    ``[project.optional-dependencies]`` group so the guard does not care which
    table the maintainer chose to home the pin in.
    """
    data = tomllib.loads(pyproject_path.read_text(encoding="utf-8"))
    project = data.get("project", {})
    candidates: list[str] = list(project.get("dependencies", []))
    for group in project.get("optional-dependencies", {}).values():
        candidates.extend(group)
    matches = [
        m.group("version")
        for dep in candidates
        if (m := _PYRIGHT_DEP_RE.match(str(dep).strip()))
    ]
    if not matches:
        raise CheckError(
            f"{pyproject_path} does not pin pyright with 'pyright==<version>' "
            "in [project].dependencies or any optional-dependencies group"
        )
    if len(set(matches)) > 1:
        raise CheckError(
            f"{pyproject_path} pins pyright more than once: {sorted(set(matches))}"
        )
    return matches[0]


def manifest_pin(manifest_path: Path) -> str:
    """Extract ``[capabilities.runtime.pyright].pin`` from the manifest."""
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    try:
        pin = manifest["capabilities"]["runtime"]["pyright"]["pin"]
    except KeyError as exc:
        raise CheckError(
            f"{manifest_path} is missing [capabilities.runtime.pyright].pin"
        ) from exc
    if not isinstance(pin, str) or not pin.strip():
        raise CheckError(f"{manifest_path} has invalid pyright pin {pin!r}")
    return pin.strip()


def ci_cache_versions(ci_path: Path) -> list[str]:
    """Extract every ``pyright-python-<version>`` cache-key version from ci.yml."""
    versions = _CI_CACHE_KEY_RE.findall(ci_path.read_text(encoding="utf-8"))
    if not versions:
        raise CheckError(f"{ci_path} has no 'pyright-python-<version>-' cache key")
    return versions


def check(pyproject_path: Path, manifest_path: Path, ci_path: Path) -> str:
    """Return the agreed pyright version, or raise CheckError on any drift."""
    pyproject = pyproject_pin(pyproject_path)
    manifest = manifest_pin(manifest_path)
    ci_versions = ci_cache_versions(ci_path)

    found = {
        f"{pyproject_path} pin": pyproject,
        f"{manifest_path} runtime pin": manifest,
    }
    for index, version in enumerate(ci_versions):
        found[f"{ci_path} cache key #{index + 1}"] = version

    distinct = set(found.values())
    if len(distinct) > 1:
        detail = ", ".join(f"{where}={version}" for where, version in found.items())
        raise CheckError(f"pyright pin drift across {len(found)} sites: {detail}")
    return pyproject


def write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def run_self_test() -> None:
    pyproject_aligned = (
        "[project]\n"
        'name = "x"\n'
        "[project.optional-dependencies]\n"
        'dev = ["ruff==0.1.0", "pyright==1.1.409", "mypy==1.0"]\n'
    )
    manifest_aligned = '[capabilities.runtime.pyright]\npin = "1.1.409"\n'
    ci_aligned = (
        "key: pyright-python-1.1.409-${{ runner.os }}\n"
        "key: pyright-python-1.1.409-${{ runner.os }}\n"
    )

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        pyproject = root / "pyproject.toml"
        manifest = root / "plugin.toml"
        ci = root / "ci.yml"

        write(pyproject, pyproject_aligned)
        write(manifest, manifest_aligned)
        write(ci, ci_aligned)
        assert check(pyproject, manifest, ci) == "1.1.409"

        # Manifest drift must fail.
        write(manifest, '[capabilities.runtime.pyright]\npin = "1.1.410"\n')
        _expect_drift(pyproject, manifest, ci)
        write(manifest, manifest_aligned)

        # A single stale CI cache key must fail.
        write(
            ci,
            "key: pyright-python-1.1.409-${{ runner.os }}\n"
            "key: pyright-python-1.1.400-${{ runner.os }}\n",
        )
        _expect_drift(pyproject, manifest, ci)
        write(ci, ci_aligned)

        # A missing pin must fail loudly, not pass vacuously.
        write(
            pyproject,
            '[project]\nname = "x"\n[project.optional-dependencies]\ndev = ["ruff==0.1.0"]\n',
        )
        try:
            check(pyproject, manifest, ci)
        except CheckError as exc:
            if "does not pin pyright" not in str(exc):
                raise
        else:
            raise CheckError("self-test expected a missing pyright pin to fail")

    print("pyright pin lockstep guard self-test passed")


def _expect_drift(pyproject: Path, manifest: Path, ci: Path) -> None:
    try:
        check(pyproject, manifest, ci)
    except CheckError as exc:
        if "drift" not in str(exc):
            raise
    else:
        raise CheckError("self-test expected pyright pin drift to fail")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Check pyright version pin lockstep")
    parser.add_argument("--pyproject", type=Path, default=DEFAULT_PYPROJECT)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--ci", type=Path, default=DEFAULT_CI)
    parser.add_argument(
        "--self-test", action="store_true", help="run built-in guard tests"
    )
    args = parser.parse_args(argv)

    try:
        if args.self_test:
            run_self_test()
        else:
            version = check(args.pyproject, args.manifest, args.ci)
            print(f"pyright pin matches across pyproject, manifest, and CI: {version}")
    except CheckError as exc:
        print(f"pyright pin lockstep guard failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
