#!/usr/bin/env python3
"""Validate the Wardline descriptor contract in the Python plugin manifest.

When the Python plugin advertises Wardline semantic extraction, it declares the
NG-25 descriptor version it consumes in ``plugins/python/plugin.toml`` under
``[integrations.wardline].expected_descriptor_version``.

This guard enforces the *local* half of the contract. If
``capabilities.runtime.wardline_aware`` is ``true``, the descriptor version must
be present and equal to the plugin's pinned expectation. If the capability is
``false``, the integration block must be absent so dormant manifest metadata
cannot look like usable semantic integration.

Closes V11-TEST-03 (docs/implementation/v1.0-tag-cut/gap-register.md).
"""

from __future__ import annotations

import argparse
import sys
import tempfile
import tomllib
from pathlib import Path

DEFAULT_MANIFEST = Path("plugins/python/plugin.toml")
EXPECTED_DESCRIPTOR_VERSION = "wardline-generic-2"


class CheckError(Exception):
    """Raised when the Wardline descriptor guard fails."""


def load_manifest(manifest_path: Path) -> dict[str, object]:
    """Load the TOML manifest."""
    return tomllib.loads(manifest_path.read_text(encoding="utf-8"))


def wardline_aware(manifest_path: Path, manifest: dict[str, object]) -> bool:
    """Return the explicit Wardline capability flag."""
    try:
        value = manifest["capabilities"]["runtime"]["wardline_aware"]  # type: ignore[index]
    except (KeyError, TypeError) as exc:
        raise CheckError(
            f"{manifest_path} is missing capabilities.runtime.wardline_aware"
        ) from exc
    if not isinstance(value, bool):
        raise CheckError(
            f"{manifest_path} capabilities.runtime.wardline_aware must be boolean"
        )
    return value


def wardline_descriptor_version(manifest_path: Path) -> str | None:
    """Return expected descriptor version, or None when capability is off."""
    manifest = load_manifest(manifest_path)
    enabled = wardline_aware(manifest_path, manifest)
    integrations = manifest.get("integrations")
    section = None
    if isinstance(integrations, dict):
        section = integrations.get("wardline")

    if not enabled:
        if section is not None:
            raise CheckError(
                f"{manifest_path} has [integrations.wardline] while "
                "capabilities.runtime.wardline_aware is false"
            )
        return None

    try:
        if not isinstance(section, dict):
            raise KeyError
    except KeyError as exc:
        raise CheckError(
            f"{manifest_path} advertises Wardline awareness but is missing "
            "[integrations.wardline]"
        ) from exc
    if "min_version" in section or "max_version" in section:
        raise CheckError(
            f"{manifest_path} [integrations.wardline] must use "
            "expected_descriptor_version, not package min_version/max_version"
        )
    if "expected_descriptor_version" not in section:
        raise CheckError(
            f"{manifest_path} [integrations.wardline] is missing "
            "expected_descriptor_version"
        )
    value = section["expected_descriptor_version"]
    if not isinstance(value, str) or not value.strip():
        raise CheckError(
            f"{manifest_path} [integrations.wardline].expected_descriptor_version "
            f"must be a non-empty string, got {value!r}"
        )
    if value != EXPECTED_DESCRIPTOR_VERSION:
        raise CheckError(
            f"{manifest_path} expects Wardline descriptor {value!r}; "
            f"plugin pin is {EXPECTED_DESCRIPTOR_VERSION!r}"
        )
    return value


def check(manifest_path: Path) -> str | None:
    """Return expected descriptor version if enabled, None if disabled."""
    return wardline_descriptor_version(manifest_path)


def descriptor_cross_check_hook(resolved_descriptor_version: str, manifest_path: Path) -> bool:
    """Seam for checking the runtime descriptor against the manifest pin."""
    expected = check(manifest_path)
    return expected is not None and resolved_descriptor_version == expected


def write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def run_self_test() -> None:
    aligned = (
        "[capabilities.runtime]\n"
        "wardline_aware = true\n"
        "\n"
        "[integrations.wardline]\n"
        f'expected_descriptor_version = "{EXPECTED_DESCRIPTOR_VERSION}"\n'
    )
    disabled = "[capabilities.runtime]\nwardline_aware = false\n"

    with tempfile.TemporaryDirectory() as tmp:
        manifest = Path(tmp) / "plugin.toml"

        write(manifest, aligned)
        assert check(manifest) == EXPECTED_DESCRIPTOR_VERSION

        write(manifest, disabled)
        assert check(manifest) is None

        write(
            manifest,
            disabled
            + "\n[integrations.wardline]\n"
            + f'expected_descriptor_version = "{EXPECTED_DESCRIPTOR_VERSION}"\n',
        )
        _expect(manifest, "wardline_aware is false")

        # Old package-version bounds must fail.
        write(
            manifest,
            "[capabilities.runtime]\nwardline_aware = true\n"
            '[integrations.wardline]\nmin_version = "1.0.0"\nmax_version = "1.0.0"\n',
        )
        _expect(manifest, "expected_descriptor_version")

        # Wrong descriptor pin must fail.
        write(
            manifest,
            "[capabilities.runtime]\nwardline_aware = true\n"
            '[integrations.wardline]\nexpected_descriptor_version = "wardline-generic-9"\n',
        )
        _expect(manifest, "plugin pin")

        # An enabled capability without bounds must fail loudly, not pass vacuously.
        write(manifest, "[capabilities.runtime]\nwardline_aware = true\n")
        _expect(manifest, "missing [integrations.wardline]")

        # Missing capability flag is malformed.
        write(manifest, "[ontology]\nx = 1\n")
        _expect(manifest, "missing capabilities.runtime.wardline_aware")

        # The cross-check hook accepts an exact descriptor version only.
        write(manifest, aligned)
        assert descriptor_cross_check_hook(EXPECTED_DESCRIPTOR_VERSION, manifest) is True
        assert descriptor_cross_check_hook("wardline-generic-9", manifest) is False

    print("Wardline descriptor guard self-test passed")


def _expect(manifest: Path, needle: str) -> None:
    try:
        check(manifest)
    except CheckError as exc:
        if needle not in str(exc):
            raise
    else:
        raise CheckError(f"self-test expected failure containing {needle!r}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Validate Wardline descriptor contract")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument(
        "--self-test", action="store_true", help="run built-in guard tests"
    )
    args = parser.parse_args(argv)

    try:
        if args.self_test:
            run_self_test()
        else:
            descriptor_version = check(args.manifest)
            if descriptor_version is None:
                print("Wardline integration not advertised; no descriptor pin required")
            else:
                print(f"Wardline descriptor pin valid: {descriptor_version}")
    except CheckError as exc:
        print(f"Wardline descriptor guard failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
