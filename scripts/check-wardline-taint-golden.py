#!/usr/bin/env python3
"""Fail when Loomweave's vendored taint golden differs from Wardline."""

from __future__ import annotations

import argparse
import hashlib
import sys
import tempfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
VENDORED = (
    REPO_ROOT
    / "crates/loomweave-storage/tests/fixtures/wardline-taint-fact-wire.golden.json"
)
AUTHORITY_RELATIVE = Path(
    "tests/conformance/fixtures/wardline-taint-fact-wire.golden.json"
)


def compare(vendored: Path, authority: Path) -> tuple[int, str]:
    missing = [path for path in (vendored, authority) if not path.is_file()]
    if missing:
        return 2, "missing required golden: " + ", ".join(map(str, missing))

    vendored_bytes = vendored.read_bytes()
    authority_bytes = authority.read_bytes()
    if vendored_bytes != authority_bytes:
        vendored_sha = hashlib.sha256(vendored_bytes).hexdigest()
        authority_sha = hashlib.sha256(authority_bytes).hexdigest()
        return (
            1,
            "Wardline taint golden drifted; re-vendor byte-identically\n"
            f"  vendored:  {vendored} (sha256 {vendored_sha})\n"
            f"  authority: {authority} (sha256 {authority_sha})",
        )
    return (
        0,
        f"Wardline taint golden lockstep: ok (sha256 {hashlib.sha256(vendored_bytes).hexdigest()})",
    )


def self_test() -> int:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        vendored = root / "vendored.json"
        authority = root / "authority.json"
        vendored.write_bytes(b'{"value":1}\n')
        authority.write_bytes(vendored.read_bytes())
        assert compare(vendored, authority)[0] == 0

        authority.write_bytes(b'{"value":2}\n')
        assert compare(vendored, authority)[0] == 1

        authority.unlink()
        assert compare(vendored, authority)[0] == 2
    print("check-wardline-taint-golden: self-test ok")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--authority-repo",
        type=Path,
        help="Wardline repository root containing the authority fixture",
    )
    parser.add_argument(
        "--authority-file",
        type=Path,
        help="Authority fixture fetched without a Wardline checkout",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if (args.authority_repo is None) == (args.authority_file is None):
        parser.error(
            "exactly one of --authority-repo or --authority-file is required "
            "unless --self-test is used"
        )

    authority = (
        args.authority_file
        if args.authority_file is not None
        else args.authority_repo / AUTHORITY_RELATIVE
    )
    code, message = compare(VENDORED, authority)
    print(message, file=sys.stderr if code else sys.stdout)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
