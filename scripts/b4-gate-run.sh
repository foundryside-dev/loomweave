#!/usr/bin/env bash
# B.4* week-2 gate runner.
#
# Calibration baseline: Clarion reference Linux workstation, x86_64,
# 32 GiB RAM, Python 3.12; reference run date 2026-05-17.
# Operators on materially slower/faster hardware may set
# OPERATOR_HARDWARE_RATIO. The ratio scales Green/Yellow/Red thresholds and
# is recorded in docs/implementation/sprint-2/b4-gate-results.md.

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
VENV="${VENV:-$REPO_ROOT/plugins/python/.venv}"
RESULT_FILE="${B4_GATE_RESULT_FILE:-$REPO_ROOT/docs/implementation/sprint-2/b4-gate-results.md}"
OPERATOR_HARDWARE_RATIO="${OPERATOR_HARDWARE_RATIO:-1.0}"
ELSPETH_FULL_ROOT="${B4_GATE_ELSPETH_FULL_ROOT:-/home/john/elspeth/src}"
ELSPETH_FULL_LOC="${B4_GATE_ELSPETH_FULL_LOC:-425000}"
NEXT_TIER_LOC="${B4_GATE_NEXT_TIER_LOC:-4000000}"

cd "$REPO_ROOT"

if [ ! -d "$VENV" ]; then
    python3 -m venv "$VENV"
fi

cargo build --workspace --release
"$VENV/bin/pip" install --quiet -e "$REPO_ROOT/plugins/python[dev]"

export REPO_ROOT
export VENV
export RESULT_FILE
export OPERATOR_HARDWARE_RATIO
export ELSPETH_FULL_ROOT
export ELSPETH_FULL_LOC
export NEXT_TIER_LOC

"$VENV/bin/python" - <<'PY'
from __future__ import annotations

import ast
import json
import os
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from clarion_plugin_python.call_resolver import NoOpCallResolver
from clarion_plugin_python.extractor import extract_with_stats
from clarion_plugin_python.pyright_session import PyrightSession


@dataclass
class CorpusMetrics:
    name: str
    file_count: int
    function_count: int
    unresolved_call_sites: int
    calls_edges: int
    ambiguous_edges: int
    outgoing_calls_requests: int
    cli_wall_ms: int
    pyright_init_ms: int
    parent_walk_ms: int
    resolution_ms: list[int]
    cli_stats: dict[str, Any]

    @property
    def median_resolution_ms(self) -> int:
        if not self.resolution_ms:
            return 0
        return int(round(statistics.median(self.resolution_ms)))

    @property
    def p95_resolution_ms(self) -> int:
        if not self.resolution_ms:
            return 0
        ordered = sorted(self.resolution_ms)
        index = max(0, min(len(ordered) - 1, int(round(len(ordered) * 0.95 + 0.499999)) - 1))
        return int(ordered[index])

    @property
    def ambiguous_ratio(self) -> float:
        if self.calls_edges == 0:
            return 0.0
        return self.ambiguous_edges / self.calls_edges

    @property
    def cli_overhead_ms(self) -> int:
        measured = self.pyright_init_ms + sum(self.resolution_ms) + self.parent_walk_ms
        return max(0, self.cli_wall_ms - measured)


def py_files(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*.py") if "__pycache__" not in path.parts)


def count_functions(root: Path) -> int:
    total = 0
    for path in py_files(root):
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"))
        except (OSError, SyntaxError, UnicodeDecodeError):
            continue
        total += sum(isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) for node in ast.walk(tree))
    return total


def read_pyright_pin(repo_root: Path) -> str:
    manifest = tomllib.loads((repo_root / "plugins/python/plugin.toml").read_text(encoding="utf-8"))
    return str(manifest["capabilities"]["runtime"]["pyright"]["pin"])


def run_cli(repo_root: Path, venv: Path, corpus_root: Path) -> tuple[int, dict[str, Any]]:
    with tempfile.TemporaryDirectory(prefix=f"clarion-b4-{corpus_root.name}-") as tmp_raw:
        tmp = Path(tmp_raw)
        project = tmp / "project"
        shutil.copytree(corpus_root, project)
        env = os.environ.copy()
        env["PATH"] = f"{repo_root / 'target/release'}:{venv / 'bin'}:{env.get('PATH', '')}"
        started = time.perf_counter()
        subprocess.run(["clarion", "install"], cwd=project, env=env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        subprocess.run(["clarion", "analyze", "."], cwd=project, env=env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        wall_ms = int(round((time.perf_counter() - started) * 1000))
        db_path = project / ".clarion" / "clarion.db"
        with sqlite3.connect(db_path) as conn:
            row = conn.execute("select stats from runs where status = 'completed' order by started_at desc limit 1").fetchone()
        if row is None:
            raise RuntimeError(f"no completed run for corpus {corpus_root}")
        stats = json.loads(row[0])
        return wall_ms, stats


def measure_plugin(corpus_root: Path) -> tuple[int, int, int, int, int, int, list[int]]:
    files = py_files(corpus_root)
    parent_walk_ms = 0
    function_count = 0
    unresolved = 0
    calls_edges = 0
    ambiguous_edges = 0
    outgoing_calls_requests = 0
    resolution_ms: list[int] = []

    noop = NoOpCallResolver()
    for path in files:
        source = path.read_text(encoding="utf-8")
        relative = str(path.relative_to(corpus_root))
        started = time.perf_counter()
        result = extract_with_stats(source, str(path), module_prefix_path=relative, call_resolver=noop)
        parent_walk_ms += int(round((time.perf_counter() - started) * 1000))
        function_count += sum(entity["kind"] == "function" for entity in result.entities)

    with PyrightSession(corpus_root) as session:
        init_started = time.perf_counter()
        ensured = session._ensure_process()  # noqa: SLF001 - gate instrumentation boundary.
        pyright_init_ms = int(round((time.perf_counter() - init_started) * 1000))
        if not ensured:
            raise RuntimeError("pyright failed to initialize for gate corpus")

        for path in files:
            source = path.read_text(encoding="utf-8")
            relative = str(path.relative_to(corpus_root))
            result = extract_with_stats(source, str(path), module_prefix_path=relative, call_resolver=session)
            file_functions = sum(entity["kind"] == "function" for entity in result.entities)
            outgoing_calls_requests += file_functions
            unresolved += result.stats.unresolved_call_sites_total
            resolution_ms.extend(result.stats.pyright_query_latency_ms)
            call_edges = [edge for edge in result.edges if edge["kind"] == "calls"]
            calls_edges += len(call_edges)
            ambiguous_edges += sum(edge.get("confidence") == "ambiguous" for edge in call_edges)

    return (
        pyright_init_ms,
        parent_walk_ms,
        function_count,
        unresolved,
        calls_edges,
        ambiguous_edges,
        outgoing_calls_requests,
        resolution_ms,
    )


def measure_corpus(repo_root: Path, venv: Path, name: str, corpus_root: Path) -> CorpusMetrics:
    cli_wall_ms, cli_stats = run_cli(repo_root, venv, corpus_root)
    (
        pyright_init_ms,
        parent_walk_ms,
        function_count,
        unresolved,
        calls_edges,
        ambiguous_edges,
        outgoing_calls_requests,
        resolution_ms,
    ) = measure_plugin(corpus_root)
    return CorpusMetrics(
        name=name,
        file_count=len(py_files(corpus_root)),
        function_count=function_count,
        unresolved_call_sites=unresolved,
        calls_edges=calls_edges,
        ambiguous_edges=ambiguous_edges,
        outgoing_calls_requests=outgoing_calls_requests,
        cli_wall_ms=cli_wall_ms,
        pyright_init_ms=pyright_init_ms,
        parent_walk_ms=parent_walk_ms,
        resolution_ms=resolution_ms,
        cli_stats=cli_stats,
    )


def current_commit(repo_root: Path) -> str:
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo_root, text=True).strip()


def machine_label() -> str:
    uname = subprocess.check_output(["uname", "-srvmo"], text=True).strip()
    return f"{uname}; Python {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"


def append_result(result_file: Path, entry: str) -> None:
    if not result_file.exists():
        result_file.write_text(
            "# B.4* Week-2 Gate Results\n\n"
            "Append-only gate log. The latest entry is parsed by the Task 11b CI freshness check.\n\n",
            encoding="utf-8",
        )
    with result_file.open("a", encoding="utf-8") as fh:
        fh.write(entry)


def format_corpus(metrics: CorpusMetrics) -> str:
    roundtrip_per_file = metrics.outgoing_calls_requests / metrics.file_count if metrics.file_count else 0.0
    return "\n".join(
        [
            f"- {metrics.name}:",
            f"  - file_count: {metrics.file_count}",
            f"  - function_count: {metrics.function_count}",
            f"  - total_wall_ms: {metrics.cli_wall_ms}",
            f"  - pyright_init_ms: {metrics.pyright_init_ms}",
            f"  - per_file_resolution_median_ms: {metrics.median_resolution_ms}",
            f"  - per_file_resolution_p95_ms: {metrics.p95_resolution_ms}",
            f"  - parent_walk_overhead_ms: {metrics.parent_walk_ms}",
            f"  - cli_overhead_ms: {metrics.cli_overhead_ms}",
            f"  - outgoing_calls_requests_total: {metrics.outgoing_calls_requests}",
            f"  - outgoing_calls_requests_per_file: {roundtrip_per_file:.2f}",
            f"  - calls_edges_total: {metrics.calls_edges}",
            f"  - ambiguous_edges_total: {metrics.ambiguous_edges}",
            f"  - ambiguous_edge_ratio: {metrics.ambiguous_ratio:.4f}",
            f"  - unresolved_call_site_count: {metrics.unresolved_call_sites}",
            f"  - persisted_run_stats: `{json.dumps(metrics.cli_stats, sort_keys=True)}`",
        ],
    )


def main() -> int:
    repo_root = Path(os.environ["REPO_ROOT"])
    venv = Path(os.environ["VENV"])
    result_file = Path(os.environ["RESULT_FILE"])
    ratio = float(os.environ["OPERATOR_HARDWARE_RATIO"])
    if ratio <= 0:
        raise ValueError("OPERATOR_HARDWARE_RATIO must be > 0")

    corpora = {
        "elspeth_mini": repo_root / "tests/perf/elspeth_mini",
        "synthetic": repo_root / "tests/perf/synthetic",
    }
    for name, root in corpora.items():
        if not root.exists() or not py_files(root):
            raise RuntimeError(f"missing Python corpus: {name} at {root}")

    pyright_pin = read_pyright_pin(repo_root)
    commit = current_commit(repo_root)
    measured = [measure_corpus(repo_root, venv, name, root) for name, root in corpora.items()]
    mini = next(item for item in measured if item.name == "elspeth_mini")

    full_root = Path(os.environ["ELSPETH_FULL_ROOT"])
    full_function_count = count_functions(full_root) if full_root.exists() else 0
    if full_function_count <= 0:
        full_function_count = max(1, int(round(mini.function_count * (int(os.environ["ELSPETH_FULL_LOC"]) / 10000))))
    next_tier_function_count = int(round(full_function_count * (int(os.environ["NEXT_TIER_LOC"]) / int(os.environ["ELSPETH_FULL_LOC"]))))

    mini_seconds = mini.cli_wall_ms / 1000
    full_projection_seconds = mini_seconds * (full_function_count / max(mini.function_count, 1))
    next_projection_seconds = mini_seconds * (next_tier_function_count / max(mini.function_count, 1))

    green_mini = 5 * 60 * ratio
    red_mini = 30 * 60 * ratio
    green_full = 60 * 60 * ratio
    red_full = 360 * 60 * ratio
    if mini_seconds > red_mini or full_projection_seconds > red_full:
        outcome = "RED"
    elif mini_seconds > green_mini or full_projection_seconds > green_full:
        outcome = "YELLOW"
    else:
        outcome = "GREEN"

    now = datetime.now(UTC).replace(microsecond=0)
    entry = "\n".join(
        [
            f"## {now.isoformat()} - {outcome}",
            "",
            f"date: {now.date().isoformat()}",
            f"outcome: {outcome}",
            f"calibration_machine: {machine_label()}",
            f"operator_hardware_ratio: {ratio}",
            f"pyright_pin: {pyright_pin}",
            f"clarion_commit: {commit}",
            "",
            "### Corpus Results",
            *(format_corpus(metrics) for metrics in measured),
            "",
            "### Extrapolation",
            f"- formula: `T_mini x (F_target / F_mini)`",
            f"- mini_wall_seconds: {mini_seconds:.3f}",
            f"- mini_function_count: {mini.function_count}",
            f"- elspeth_full_function_count: {full_function_count}",
            f"- elspeth_full_projected_seconds: {full_projection_seconds:.3f}",
            f"- elspeth_full_projected_minutes: {full_projection_seconds / 60:.3f}",
            f"- next_tier_function_count: {next_tier_function_count}",
            f"- next_tier_projected_seconds: {next_projection_seconds:.3f}",
            f"- next_tier_projected_minutes: {next_projection_seconds / 60:.3f}",
            "",
            "### Decision",
            f"- gate_thresholds_scaled_by_ratio: green_mini_seconds={green_mini:.3f}, red_mini_seconds={red_mini:.3f}, green_full_seconds={green_full:.3f}, red_full_seconds={red_full:.3f}",
            f"- decision: {outcome}",
            "",
        ],
    )
    append_result(result_file, entry)
    print(entry)
    return 2 if outcome == "RED" else 0


raise SystemExit(main())
PY
