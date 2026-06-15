#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
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


def check_b5_smoke(
    *,
    script_path: Path,
    pyright_langserver: str,
    max_reference_ratio: float,
    max_p95_ms: int,
) -> list[str]:
    completed = subprocess.run(
        [
            sys.executable,
            str(script_path),
            "--pyright-langserver",
            pyright_langserver,
        ],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        return [
            "B.5 reference scale smoke failed to run: "
            f"exit={completed.returncode}, stderr={completed.stderr.strip()}"
        ]

    try:
        result = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        return [f"B.5 reference scale smoke did not emit JSON: {exc}"]

    errors, summary = evaluate_b5_smoke_result(
        result,
        max_reference_ratio=max_reference_ratio,
        max_p95_ms=max_p95_ms,
    )
    if not errors:
        print(f"B.5 reference scale smoke passed: {summary}")
    return errors


def evaluate_b5_smoke_result(
    result: dict[str, object],
    *,
    max_reference_ratio: float,
    max_p95_ms: int,
) -> tuple[list[str], str]:
    errors: list[str] = []
    skipped_cap = int(result.get("references_skipped_cap_total", 0))
    if skipped_cap != 0:
        errors.append(
            "B.5 reference scale smoke skipped reference sites due to caps: "
            f"{skipped_cap}"
        )

    projection = result.get("projection")
    if isinstance(projection, dict) and projection.get("available"):
        ratio = float(projection.get("projected_reference_to_b4_function_query_ratio", 0.0))
        ratio_label = "projected_reference_to_b4_function_query_ratio"
    else:
        ratio = float(result.get("reference_requests_per_b4_function_query", 0.0))
        ratio_label = "reference_requests_per_b4_function_query"
    if ratio > max_reference_ratio:
        errors.append(
            f"B.5 reference scale smoke {ratio_label} {ratio:.4f} exceeds "
            f"{max_reference_ratio:.4f}"
        )

    p95_ms = int(result.get("per_file_resolution_p95_ms", 0))
    if p95_ms > max_p95_ms:
        errors.append(
            f"B.5 reference scale smoke per_file_resolution_p95_ms {p95_ms} "
            f"exceeds {max_p95_ms}"
        )

    summary = f"cap_skips={skipped_cap}, {ratio_label}={ratio:.4f}, p95_ms={p95_ms}"
    return errors, summary


def self_test() -> int:
    ok, _ = evaluate_b5_smoke_result(
        {
            "references_skipped_cap_total": 0,
            "reference_requests_per_b4_function_query": 5.0,
            "per_file_resolution_p95_ms": 2000,
        },
        max_reference_ratio=5.0,
        max_p95_ms=2000,
    )
    assert ok == [], ok

    errors, _ = evaluate_b5_smoke_result(
        {
            "references_skipped_cap_total": 1,
            "reference_requests_per_b4_function_query": 5.01,
            "per_file_resolution_p95_ms": 2001,
        },
        max_reference_ratio=5.0,
        max_p95_ms=2000,
    )
    assert len(errors) == 3, errors
    print("B.4*/B.5 gate self-test passed")
    return 0


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
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument(
        "--run-b5-smoke",
        action="store_true",
        help="Also execute the synthetic B.5 reference scale smoke with thresholds.",
    )
    parser.add_argument(
        "--b5-script",
        type=Path,
        default=Path("tests/perf/b5_reference_scale_smoke.py"),
    )
    parser.add_argument(
        "--pyright-langserver",
        default=os.environ.get("PYRIGHT_LANGSERVER", "pyright-langserver"),
    )
    parser.add_argument(
        "--max-b5-reference-ratio",
        type=float,
        default=float(os.environ.get("MAX_B5_REFERENCE_RATIO", "8.0")),
    )
    parser.add_argument(
        "--max-b5-p95-ms",
        type=int,
        # Coupled to the Python plugin's per-file reference budget
        # PYRIGHT_FILE_TIMEOUT_SECS (pyright_session.py): a single file's
        # resolution is capped at that budget, so the p95 over a stress corpus
        # tracks it. The budget was raised 3s -> 10s for more-complete graphs on
        # large, heavily-typed files, which lifts the p95 ceiling accordingly;
        # this gate moves with it (budget + margin) and still catches a
        # regression that blows past the per-file budget.
        default=int(os.environ.get("MAX_B5_P95_MS", "12000")),
    )
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    errors = check(args.result, args.manifest, args.max_age_days)
    if args.run_b5_smoke:
        errors.extend(
            check_b5_smoke(
                script_path=args.b5_script,
                pyright_langserver=args.pyright_langserver,
                max_reference_ratio=args.max_b5_reference_ratio,
                max_p95_ms=args.max_b5_p95_ms,
            )
        )
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print("B.4* gate result is fresh and pyright_pin matches plugin.toml")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
