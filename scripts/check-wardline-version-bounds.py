#!/usr/bin/env python3
"""Validate the Wardline integration version bounds in the Python plugin manifest.

The Python plugin declares the Wardline version range it integrates against in
``plugins/python/plugin.toml`` under ``[integrations.wardline]``:

* ``min_version`` — inclusive lower bound (the oldest Wardline the plugin's
  ``wardline.core.registry`` import surface is verified against).
* ``max_version`` — exclusive upper bound, deliberately set to the next major
  so a future major release triggers an explicit re-pin rather than silent
  drift (see the comment in plugin.toml and loom.md §5 asterisk 2).

This guard enforces the *local* half of the contract. If
``capabilities.runtime.wardline_aware`` is ``true``, both bounds must be
present, parse as semver, and form a sane half-open range ``[min, max)``. If the
capability is ``false``, the bounds block must be absent so dormant manifest
metadata cannot look like usable semantic integration. The *server-side*
cross-check (confirming the resolved Wardline actually advertises a version
inside the range at integration time) is future work — see
``server_side_cross_check_hook`` for the documented seam.

Closes V11-TEST-03 (docs/implementation/v1.0-tag-cut/gap-register.md).
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
import tomllib
from pathlib import Path

DEFAULT_MANIFEST = Path("plugins/python/plugin.toml")

# A core MAJOR.MINOR.PATCH semver, optionally with a -prerelease and/or +build.
# We only order by the numeric core, which is all the bounds contract needs.
_SEMVER_RE = re.compile(
    r"^(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)"
    r"(?:-(?P<pre>[0-9A-Za-z.\-]+))?(?:\+(?P<build>[0-9A-Za-z.\-]+))?$"
)


class CheckError(Exception):
    """Raised when the Wardline version-bounds guard fails."""


def parse_semver(label: str, value: object) -> tuple[int, int, int]:
    """Parse ``value`` as a semver core triple, raising CheckError on failure."""
    if not isinstance(value, str) or not value.strip():
        raise CheckError(f"{label} must be a non-empty semver string, got {value!r}")
    match = _SEMVER_RE.match(value.strip())
    if match is None:
        raise CheckError(f"{label} is not valid semver: {value!r}")
    return (int(match["major"]), int(match["minor"]), int(match["patch"]))


def wardline_bounds(manifest_path: Path) -> tuple[str, str]:
    """Return the raw (min_version, max_version) strings from the manifest."""
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    try:
        section = manifest["integrations"]["wardline"]
    except KeyError as exc:
        raise CheckError(f"{manifest_path} is missing [integrations.wardline]") from exc
    missing = [key for key in ("min_version", "max_version") if key not in section]
    if missing:
        raise CheckError(
            f"{manifest_path} [integrations.wardline] is missing {', '.join(missing)}"
        )
    return str(section["min_version"]), str(section["max_version"])


def check(manifest_path: Path) -> tuple[str, str]:
    """Return (min, max) if the bounds are valid, else raise CheckError."""
    raw_min, raw_max = wardline_bounds(manifest_path)
    min_core = parse_semver("[integrations.wardline].min_version", raw_min)
    max_core = parse_semver("[integrations.wardline].max_version", raw_max)
    if min_core >= max_core:
        raise CheckError(
            "[integrations.wardline] bounds are not a half-open range [min, max): "
            f"min_version={raw_min} must be strictly below max_version={raw_max}"
        )
    return raw_min, raw_max


def server_side_cross_check_hook(resolved_version: str, manifest_path: Path) -> bool:
    """Seam for the future server-side cross-check.

    When Wardline can report its own version at integration time, the resolved
    version should be confirmed to satisfy ``[min, max)`` here. Until then this
    guard only enforces the locally-checkable invariants and this hook is not
    wired into ``main``.
    """
    raw_min, raw_max = check(manifest_path)
    resolved = parse_semver("resolved Wardline version", resolved_version)
    return parse_semver("min", raw_min) <= resolved < parse_semver("max", raw_max)


def write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def run_self_test() -> None:
    aligned = '[integrations.wardline]\nmin_version = "1.0.0"\nmax_version = "2.0.0"\n'

    with tempfile.TemporaryDirectory() as tmp:
        manifest = Path(tmp) / "plugin.toml"

        write(manifest, aligned)
        assert check(manifest) == ("1.0.0", "2.0.0")

        # Inverted bounds must fail.
        write(
            manifest,
            '[integrations.wardline]\nmin_version = "2.0.0"\nmax_version = "1.0.0"\n',
        )
        _expect(manifest, "half-open range")

        # Equal bounds (empty range) must fail.
        write(
            manifest,
            '[integrations.wardline]\nmin_version = "1.0.0"\nmax_version = "1.0.0"\n',
        )
        _expect(manifest, "half-open range")

        # Non-semver bound must fail.
        write(
            manifest,
            '[integrations.wardline]\nmin_version = "1.0" \nmax_version = "2.0.0"\n',
        )
        _expect(manifest, "not valid semver")

        # A missing section must fail loudly, not pass vacuously.
        write(manifest, "[ontology]\nx = 1\n")
        _expect(manifest, "missing [integrations.wardline]")

        # The cross-check hook accepts an in-range version and rejects out-of-range.
        write(manifest, aligned)
        assert server_side_cross_check_hook("1.4.2", manifest) is True
        assert server_side_cross_check_hook("2.0.0", manifest) is False
        assert server_side_cross_check_hook("0.9.0", manifest) is False

    print("Wardline version-bounds guard self-test passed")


def _expect(manifest: Path, needle: str) -> None:
    try:
        check(manifest)
    except CheckError as exc:
        if needle not in str(exc):
            raise
    else:
        raise CheckError(f"self-test expected failure containing {needle!r}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Validate Wardline integration version bounds"
    )
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument(
        "--self-test", action="store_true", help="run built-in guard tests"
    )
    args = parser.parse_args(argv)

    try:
        if args.self_test:
            run_self_test()
        else:
            raw_min, raw_max = check(args.manifest)
            print(f"Wardline version bounds valid: [{raw_min}, {raw_max})")
    except CheckError as exc:
        print(f"Wardline version-bounds guard failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
