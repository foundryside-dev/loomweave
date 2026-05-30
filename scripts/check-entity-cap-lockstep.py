#!/usr/bin/env python3
"""Guard the per-run entity-count cap against ADR/code drift.

ADR-021 §2c fixes the default per-run cumulative cap on ``entity`` + ``edge`` +
``finding`` notifications from a single plugin. That number is duplicated in
two places that must stay in lockstep:

* docs/clarion/adr/ADR-021-plugin-authority-hybrid.md §2c — the normative
  ``Default: **500,000**`` statement.
* crates/clarion-core/src/plugin/limits.rs — ``EntityCountCap::DEFAULT_MAX``,
  the const the host actually enforces.

If the const drifts from the ADR, the enforced guardrail no longer matches the
documented authority model. This guard keeps the duplication mechanical:
closes V11-TEST-04 (docs/implementation/v1.0-tag-cut/gap-register.md).
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path

DEFAULT_LIMITS = Path("crates/clarion-core/src/plugin/limits.rs")
DEFAULT_ADR = Path("docs/clarion/adr/ADR-021-plugin-authority-hybrid.md")

# `pub const DEFAULT_MAX: usize = 500_000;` — underscores are stripped before int().
_RUST_DEFAULT_MAX_RE = re.compile(
    r"\bconst\s+DEFAULT_MAX\s*:\s*usize\s*=\s*(?P<value>[0-9_]+)"
)
# Anchored on the §2c section so we read *that* section's default, not another's:
# "...Entity-count cap.** ... Default: **500,000** combined records...".
_ADR_DEFAULT_RE = re.compile(
    r"Entity-count cap.*?Default:\s*\*\*(?P<value>[0-9,]+)\*\*",
    re.DOTALL,
)


class CheckError(Exception):
    """Raised when the entity-cap lockstep guard fails."""


def _to_int(label: str, raw: str) -> int:
    digits = raw.replace("_", "").replace(",", "")
    if not digits.isdigit():
        raise CheckError(f"{label} is not an integer: {raw!r}")
    return int(digits)


def rust_default_max(limits_path: Path) -> int:
    """Extract ``EntityCountCap::DEFAULT_MAX`` from limits.rs."""
    matches = _RUST_DEFAULT_MAX_RE.findall(limits_path.read_text(encoding="utf-8"))
    if not matches:
        raise CheckError(f"{limits_path} has no 'const DEFAULT_MAX: usize = ...'")
    if len({m.replace("_", "") for m in matches}) > 1:
        raise CheckError(f"{limits_path} defines DEFAULT_MAX more than once: {matches}")
    return _to_int("limits.rs DEFAULT_MAX", matches[0])


def adr_default_cap(adr_path: Path) -> int:
    """Extract the §2c default cap from ADR-021."""
    match = _ADR_DEFAULT_RE.search(adr_path.read_text(encoding="utf-8"))
    if match is None:
        raise CheckError(
            f"{adr_path} §2c does not state 'Default: **<number>**' for the entity-count cap"
        )
    return _to_int("ADR-021 §2c default", match.group("value"))


def check(limits_path: Path, adr_path: Path) -> int:
    """Return the agreed cap, or raise CheckError on drift."""
    code = rust_default_max(limits_path)
    adr = adr_default_cap(adr_path)
    if code != adr:
        raise CheckError(
            f"entity-cap drift: {limits_path} DEFAULT_MAX={code:,} but "
            f"ADR-021 §2c states {adr:,}"
        )
    return code


def write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def run_self_test() -> None:
    rust_aligned = (
        "impl EntityCountCap {\n    pub const DEFAULT_MAX: usize = 500_000;\n}\n"
    )
    adr_aligned = (
        "**2c — Entity-count cap.** Per-run cumulative cap. "
        "Default: **500,000** combined records (floor 10,000).\n"
    )

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        limits = root / "limits.rs"
        adr = root / "adr.md"

        write(limits, rust_aligned)
        write(adr, adr_aligned)
        assert check(limits, adr) == 500_000

        # Code drift must fail.
        write(limits, "    pub const DEFAULT_MAX: usize = 400_000;\n")
        _expect(limits, adr, "drift")
        write(limits, rust_aligned)

        # ADR drift must fail.
        write(
            adr, "**2c — Entity-count cap.** Default: **750,000** combined records.\n"
        )
        _expect(limits, adr, "drift")
        write(adr, adr_aligned)

        # A missing const must fail loudly, not pass vacuously.
        write(limits, "// no const here\n")
        _expect(limits, adr, "no 'const DEFAULT_MAX")
        write(limits, rust_aligned)

        # A §2c section with no Default must fail.
        write(adr, "**2c — Entity-count cap.** Per-run cumulative cap.\n")
        _expect(limits, adr, "does not state")

    print("entity-cap lockstep guard self-test passed")


def _expect(limits: Path, adr: Path, needle: str) -> None:
    try:
        check(limits, adr)
    except CheckError as exc:
        if needle not in str(exc):
            raise
    else:
        raise CheckError(f"self-test expected failure containing {needle!r}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Check EntityCountCap ADR/code lockstep"
    )
    parser.add_argument("--limits", type=Path, default=DEFAULT_LIMITS)
    parser.add_argument("--adr", type=Path, default=DEFAULT_ADR)
    parser.add_argument(
        "--self-test", action="store_true", help="run built-in guard tests"
    )
    args = parser.parse_args(argv)

    try:
        if args.self_test:
            run_self_test()
        else:
            cap = check(args.limits, args.adr)
            print(f"EntityCountCap DEFAULT_MAX matches ADR-021 §2c: {cap:,}")
    except CheckError as exc:
        print(f"entity-cap lockstep guard failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
