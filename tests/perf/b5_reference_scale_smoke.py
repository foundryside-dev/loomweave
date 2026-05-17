from __future__ import annotations

import argparse
import ast
import json
import math
import os
import platform
import statistics
import subprocess
import sys
import time
import tomllib
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "plugins/python/src"))

from clarion_plugin_python.extractor import extract_with_stats  # noqa: E402
from clarion_plugin_python.pyright_session import PyrightSession  # noqa: E402

EXCLUDED_PARTS = frozenset(
    {
        ".clarion",
        ".git",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".tox",
        ".venv",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
        "target",
        "venv",
    },
)


class CountingPyrightSession(PyrightSession):
    def __init__(
        self, project_root: Path, *, executable: str = "pyright-langserver"
    ) -> None:
        super().__init__(project_root, executable=executable)
        self.definition_requests_total = 0
        self.type_definition_requests_total = 0

    @property
    def reference_requests_total(self) -> int:
        return self.definition_requests_total + self.type_definition_requests_total

    def _request(
        self, method: str, params: dict[str, object], timeout_secs: float
    ) -> object:
        if method == "textDocument/definition":
            self.definition_requests_total += 1
        elif method == "textDocument/typeDefinition":
            self.type_definition_requests_total += 1
        return super()._request(method, params, timeout_secs)


def py_files(root: Path) -> list[Path]:
    return sorted(
        path
        for path in root.rglob("*.py")
        if not EXCLUDED_PARTS.intersection(path.relative_to(root).parts)
    )


def count_functions(files: list[Path]) -> int:
    total = 0
    for path in files:
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"))
        except (OSError, SyntaxError, UnicodeDecodeError):
            continue
        total += sum(
            isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            for node in ast.walk(tree)
        )
    return total


def percentile_95(samples: list[int]) -> int:
    if not samples:
        return 0
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, math.ceil(len(ordered) * 0.95) - 1))
    return ordered[index]


def current_commit() -> str:
    return subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, text=True
    ).strip()


def read_pyright_pin() -> str:
    manifest = tomllib.loads(
        (REPO_ROOT / "plugins/python/plugin.toml").read_text(encoding="utf-8"),
    )
    return str(manifest["capabilities"]["runtime"]["pyright"]["pin"])


def machine_label() -> str:
    return (
        f"{platform.system()} {platform.release()} {platform.machine()}; "
        f"Python {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    )


def projection_metrics(
    *,
    projection_root: Path | None,
    reference_requests_total: int,
    function_count: int,
) -> dict[str, Any]:
    if projection_root is None or not projection_root.exists():
        return {
            "projection_root": str(projection_root)
            if projection_root is not None
            else None,
            "available": False,
        }
    projection_files = py_files(projection_root)
    projection_function_count = count_functions(projection_files)
    projected_requests = 0
    if function_count:
        projected_requests = math.ceil(
            reference_requests_total * projection_function_count / function_count,
        )
    ratio = (
        projected_requests / projection_function_count
        if projection_function_count
        else 0.0
    )
    return {
        "projection_root": str(projection_root),
        "available": True,
        "elspeth_full_file_count": len(projection_files),
        "elspeth_full_function_count": projection_function_count,
        "projected_elspeth_full_reference_requests": projected_requests,
        "b4_full_function_query_count": projection_function_count,
        "projected_reference_to_b4_function_query_ratio": round(ratio, 4),
        "green_under_5x_b4_function_queries": ratio <= 5.0,
    }


def measure(
    *,
    corpus_root: Path,
    corpus_name: str,
    projection_root: Path | None,
    executable: str,
) -> dict[str, Any]:
    files = py_files(corpus_root)
    function_count = count_functions(files)
    reference_sites_total = 0
    references_resolved_total = 0
    references_edges_total = 0
    ambiguous_reference_edges_total = 0
    references_skipped_external_total = 0
    references_skipped_cap_total = 0
    unresolved_reference_sites_total = 0
    pyright_query_latency_ms: list[int] = []
    findings_by_subcode: Counter[str] = Counter()

    wall_started = time.perf_counter()
    with CountingPyrightSession(corpus_root, executable=executable) as session:
        init_started = time.perf_counter()
        if not session._ensure_process():  # noqa: SLF001 - perf gate instrumentation boundary.
            raise RuntimeError(
                "pyright failed to initialize for B.5 reference scale smoke"
            )
        pyright_init_ms = max(1, math.ceil((time.perf_counter() - init_started) * 1000))

        for path in files:
            source = path.read_text(encoding="utf-8")
            result = extract_with_stats(
                source,
                str(path),
                module_prefix_path=str(path.relative_to(corpus_root)),
                reference_resolver=session,
            )
            reference_sites_total += result.stats.reference_sites_total
            references_resolved_total += result.stats.references_resolved_total
            references_skipped_external_total += (
                result.stats.references_skipped_external_total
            )
            references_skipped_cap_total += result.stats.references_skipped_cap_total
            unresolved_reference_sites_total += (
                result.stats.unresolved_reference_sites_total
            )
            pyright_query_latency_ms.extend(result.stats.pyright_query_latency_ms)
            for finding in result.stats.findings:
                findings_by_subcode[str(finding["subcode"])] += 1
            reference_edges = [
                edge for edge in result.edges if edge["kind"] == "references"
            ]
            references_edges_total += len(reference_edges)
            ambiguous_reference_edges_total += sum(
                edge.get("confidence") == "ambiguous" for edge in reference_edges
            )

        reference_requests_total = session.reference_requests_total
        definition_requests_total = session.definition_requests_total
        type_definition_requests_total = session.type_definition_requests_total

    wall_ms = max(1, math.ceil((time.perf_counter() - wall_started) * 1000))
    mini_ratio = reference_requests_total / function_count if function_count else 0.0
    projection = projection_metrics(
        projection_root=projection_root,
        reference_requests_total=reference_requests_total,
        function_count=function_count,
    )
    decision_green = references_skipped_cap_total == 0 and bool(
        projection.get("green_under_5x_b4_function_queries", True)
    )

    return {
        "date": datetime.now(UTC).date().isoformat(),
        "outcome": "GREEN" if decision_green else "YELLOW",
        "calibration_machine": machine_label(),
        "pyright_pin": read_pyright_pin(),
        "clarion_commit": current_commit(),
        "corpus": corpus_name,
        "corpus_root": str(corpus_root),
        "file_count": len(files),
        "function_count": function_count,
        "reference_sites_total": reference_sites_total,
        "reference_requests_total": reference_requests_total,
        "definition_requests_total": definition_requests_total,
        "type_definition_requests_total": type_definition_requests_total,
        "reference_requests_per_file": round(reference_requests_total / len(files), 4)
        if files
        else 0.0,
        "reference_requests_per_b4_function_query": round(mini_ratio, 4),
        "references_edges_total": references_edges_total,
        "ambiguous_reference_edges_total": ambiguous_reference_edges_total,
        "references_resolved_total": references_resolved_total,
        "references_skipped_external_total": references_skipped_external_total,
        "references_skipped_cap_total": references_skipped_cap_total,
        "unresolved_reference_sites_total": unresolved_reference_sites_total,
        "pyright_init_ms": pyright_init_ms,
        "total_wall_ms": wall_ms,
        "per_file_resolution_median_ms": int(
            round(statistics.median(pyright_query_latency_ms))
        )
        if pyright_query_latency_ms
        else 0,
        "per_file_resolution_p95_ms": percentile_95(pyright_query_latency_ms),
        "findings_by_subcode": dict(sorted(findings_by_subcode.items())),
        "projection": projection,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run the B.5 reference scale smoke.")
    parser.add_argument(
        "--corpus-root",
        type=Path,
        default=REPO_ROOT / "tests/perf/elspeth_mini",
    )
    parser.add_argument("--corpus-name", default="elspeth_mini")
    parser.add_argument(
        "--projection-root",
        type=Path,
        default=Path(
            os.environ.get("B5_GATE_ELSPETH_FULL_ROOT", "/home/john/elspeth/src")
        ),
    )
    parser.add_argument("--pyright-langserver", default="pyright-langserver")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    result = measure(
        corpus_root=args.corpus_root.resolve(),
        corpus_name=str(args.corpus_name),
        projection_root=args.projection_root.resolve()
        if args.projection_root
        else None,
        executable=str(args.pyright_langserver),
    )
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
