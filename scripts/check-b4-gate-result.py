#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import re
import sys
import tomllib
from dataclasses import dataclass
from datetime import UTC, date, datetime
from pathlib import Path


@dataclass(frozen=True)
class GateEntry:
    entry_date: date
    pyright_pin: str


def _latest_entry(text: str) -> GateEntry:
    chunks = re.split(r"(?m)^##\s+", text)
    for chunk in reversed(chunks):
        if not chunk.strip():
            continue
        date_match = re.search(r"(?m)^date:\s*([0-9]{4}-[0-9]{2}-[0-9]{2})\s*$", chunk)
        pin_match = re.search(r"(?m)^pyright_pin:\s*([^\s]+)\s*$", chunk)
        if date_match is None or pin_match is None:
            continue
        return GateEntry(
            entry_date=datetime.strptime(date_match.group(1), "%Y-%m-%d").date(),
            pyright_pin=pin_match.group(1),
        )
    raise ValueError("no gate entry with date and pyright_pin fields found")


def _manifest_pin(manifest_path: Path) -> str:
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    return str(manifest["capabilities"]["runtime"]["pyright"]["pin"])


def check(result_path: Path, manifest_path: Path, max_age_days: int) -> list[str]:
    entry = _latest_entry(result_path.read_text(encoding="utf-8"))
    expected_pin = _manifest_pin(manifest_path)
    today = datetime.now(UTC).date()
    errors: list[str] = []
    age_days = (today - entry.entry_date).days
    if age_days < 0:
        errors.append(
            f"B.4* gate result date {entry.entry_date.isoformat()} is in the future"
        )
    elif age_days > max_age_days:
        errors.append(
            f"B.4* gate result is stale: {age_days} days old; max is {max_age_days}"
        )
    if entry.pyright_pin != expected_pin:
        errors.append(
            "B.4* gate result pyright_pin mismatch: "
            f"result has {entry.pyright_pin}, plugin.toml has {expected_pin}"
        )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description="Check B.4* gate result freshness")
    parser.add_argument(
        "--result",
        type=Path,
        default=Path("docs/implementation/sprint-2/b4-gate-results.md"),
    )
    parser.add_argument(
        "--manifest",
        type=Path,
        default=Path("plugins/python/plugin.toml"),
    )
    parser.add_argument(
        "--max-age-days",
        type=int,
        default=int(os.environ.get("MAX_GATE_AGE_DAYS", "30")),
    )
    args = parser.parse_args()
    errors = check(args.result, args.manifest, args.max_age_days)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print("B.4* gate result is fresh and pyright_pin matches plugin.toml")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
