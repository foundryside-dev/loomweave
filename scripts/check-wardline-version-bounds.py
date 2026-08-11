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

# Consumer-first dual-accept (wardline declaration-surface-v2 §13.1 item 1).
# Acceptance is keyed on the (schema, version) PAIR, so the manifest declares
# pairs, not a bare version list. Mirrors ACCEPTED_DESCRIPTORS in
# plugins/python/src/loomweave_plugin_python/wardline_descriptor.py; test_package
# asserts the two agree.
ACCEPTED_DESCRIPTORS = (
    ("wardline.vocabulary/v1", "wardline-generic-2"),
    ("wardline.vocabulary/v2", "wardline-generic-3"),
)
ACCEPTED_DESCRIPTOR_TOKENS = tuple(
    f"{schema}@{version}" for schema, version in ACCEPTED_DESCRIPTORS
)


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
    # Checked AFTER the pin check on purpose: every existing negative fixture
    # must keep failing for its own stated reason, not be re-labelled by this
    # newer guard.
    _check_accepted_descriptors(manifest_path, section)
    return value


def _check_accepted_descriptors(manifest_path: Path, section: dict[str, object]) -> None:
    """Validate the pair-encoded accepted-descriptor set.

    Rejects missing, reordered, duplicated, malformed, and version-only values:
    the schema/version association is the whole point of the set, so a loose
    version list must not be able to masquerade as one.
    """
    if "accepted_descriptors" not in section:
        raise CheckError(
            f"{manifest_path} [integrations.wardline] is missing accepted_descriptors"
        )
    value = section["accepted_descriptors"]
    if not isinstance(value, str):
        raise CheckError(
            f"{manifest_path} [integrations.wardline].accepted_descriptors must be a "
            f"space-separated string of schema@version pairs, got {value!r}"
        )
    tokens = tuple(value.split())
    if tokens != ACCEPTED_DESCRIPTOR_TOKENS:
        raise CheckError(
            f"{manifest_path} [integrations.wardline].accepted_descriptors is "
            f"{tokens!r}; plugin accepted set is {ACCEPTED_DESCRIPTOR_TOKENS!r}"
        )


def check(manifest_path: Path) -> str | None:
    """Return expected descriptor version if enabled, None if disabled."""
    return wardline_descriptor_version(manifest_path)


def descriptor_cross_check_hook(
    resolved_descriptor_schema: str,
    resolved_descriptor_version: str,
    manifest_path: Path,
) -> bool:
    """Seam for checking the runtime descriptor against the manifest pin.

    Answers from the accepted PAIR set, not from the single expected version:
    ``EXPECTED_DESCRIPTOR_VERSION`` is what the plugin emits today, whereas the
    pair set is what it will accept from the producer.

    Deliberate behaviour change, recorded rather than left silent: the previous
    form returned ``expected is not None and version == expected``, whose
    ``is not None`` conjunct was a CAPABILITY guard — a manifest with
    ``wardline_aware = false`` and no ``[integrations.wardline]`` block made
    ``check()`` return ``None`` without raising, so the hook answered ``False``.
    This form calls ``check()`` for its raising side-effect and then answers
    purely from the pair, so that one case now answers ``True`` for any accepted
    pair. Accepted because the case is dormant (no production callers — this
    module's self-test is the only caller), because every *enabled*-but-
    misconfigured manifest still raises ``CheckError`` inside ``check()`` and
    never reaches the return, and because the pair set is the thing this guard
    exists to make authoritative. If a production caller is ever added, restore
    the guard as ``expected = check(manifest_path)`` plus
    ``expected is not None and (schema, version) in ACCEPTED_DESCRIPTORS``.
    """
    check(manifest_path)
    return (
        resolved_descriptor_schema,
        resolved_descriptor_version,
    ) in ACCEPTED_DESCRIPTORS


def write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def run_self_test() -> None:
    tokens = " ".join(ACCEPTED_DESCRIPTOR_TOKENS)
    aligned = (
        "[capabilities.runtime]\n"
        "wardline_aware = true\n"
        "\n"
        "[integrations.wardline]\n"
        f'expected_descriptor_version = "{EXPECTED_DESCRIPTOR_VERSION}"\n'
        f'accepted_descriptors = "{tokens}"\n'
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

        # A missing accepted-descriptor set must fail loudly.
        write(
            manifest,
            "[capabilities.runtime]\nwardline_aware = true\n"
            "[integrations.wardline]\n"
            f'expected_descriptor_version = "{EXPECTED_DESCRIPTOR_VERSION}"\n',
        )
        _expect(manifest, "missing accepted_descriptors")

        # A version-only set is not a pair set.
        for bad in (
            EXPECTED_DESCRIPTOR_VERSION,
            " ".join(reversed(ACCEPTED_DESCRIPTOR_TOKENS)),
            " ".join(ACCEPTED_DESCRIPTOR_TOKENS + ACCEPTED_DESCRIPTOR_TOKENS[:1]),
            ACCEPTED_DESCRIPTOR_TOKENS[0],
            "wardline.vocabulary/v1:wardline-generic-2",
        ):
            write(
                manifest,
                "[capabilities.runtime]\nwardline_aware = true\n"
                "[integrations.wardline]\n"
                f'expected_descriptor_version = "{EXPECTED_DESCRIPTOR_VERSION}"\n'
                f'accepted_descriptors = "{bad}"\n',
            )
            _expect(manifest, "accepted set is")

        # A non-string (TOML array) accepted set must fail.
        write(
            manifest,
            "[capabilities.runtime]\nwardline_aware = true\n"
            "[integrations.wardline]\n"
            f'expected_descriptor_version = "{EXPECTED_DESCRIPTOR_VERSION}"\n'
            f'accepted_descriptors = ["{ACCEPTED_DESCRIPTOR_TOKENS[0]}"]\n',
        )
        _expect(manifest, "space-separated string")

        # The cross-check hook accepts an accepted (schema, version) PAIR only.
        write(manifest, aligned)
        for schema, version in ACCEPTED_DESCRIPTORS:
            assert descriptor_cross_check_hook(schema, version, manifest) is True
        assert (
            descriptor_cross_check_hook(
                "wardline.vocabulary/v1", "wardline-generic-3", manifest
            )
            is False
        )
        assert (
            descriptor_cross_check_hook(
                "wardline.vocabulary/v2", "wardline-generic-2", manifest
            )
            is False
        )
        assert (
            descriptor_cross_check_hook(
                "wardline.vocabulary/v2", "wardline-generic-9", manifest
            )
            is False
        )

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
