#!/usr/bin/env python3
"""Rust plugin manifest lockstep guard.

The `loomweave-plugin-rust` wheel ships `plugin.toml` as a tracked copy under
`packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/` (the
maturin data scheme routes it to `<venv>/share/loomweave/plugins/rust/`, where
discovery's install-prefix fallback resolves it). The CANONICAL manifest is
`crates/loomweave-plugin-rust/plugin.toml`. If the copy drifts, the wheel ships
a stale manifest while the dev/test path uses the current one — a silent
ontology/version skew.

This guard asserts the two are byte-identical. Fast, stdlib-only, CI-friendly.

Usage:
    check-rust-plugin-manifest-lockstep.py
Exit codes:
    0  identical
    1  drift (contents differ)
    2  a file is missing
"""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
CANONICAL = REPO_ROOT / "crates/loomweave-plugin-rust/plugin.toml"
WHEEL_COPY = (
    REPO_ROOT
    / "packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml"
)


def main() -> int:
    for path in (CANONICAL, WHEEL_COPY):
        if not path.is_file():
            print(f"check-rust-plugin-manifest-lockstep: missing {path}", file=sys.stderr)
            return 2
    canonical = CANONICAL.read_bytes()
    wheel_copy = WHEEL_COPY.read_bytes()
    if canonical != wheel_copy:
        print(
            "check-rust-plugin-manifest-lockstep: drift detected\n"
            f"  canonical: {CANONICAL}\n"
            f"  wheel copy: {WHEEL_COPY}\n"
            "Re-copy the canonical manifest into the wheel-data tree "
            "(they must be byte-identical).",
            file=sys.stderr,
        )
        return 1
    print("check-rust-plugin-manifest-lockstep: ok (manifests identical)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
