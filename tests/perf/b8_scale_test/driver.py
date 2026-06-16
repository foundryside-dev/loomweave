#!/usr/bin/env python3
"""B.8 scale-test MCP driver.

The driver assumes `loomweave analyze` has already produced `.weft/loomweave/loomweave.db`
for the project under test. It starts `loomweave serve`, sends Content-Length
framed MCP requests, and writes JSON measurements for the B.8 result memo.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import math
import os
import select
import sqlite3
import subprocess
import time
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class CallRecord:
    pattern: str
    tool: str
    phase: str
    cache_state: str
    latency_ms: float
    response_bytes: int
    estimated_response_tokens: int
    ok: bool
    error: bool
    unavailable: bool
    useful_result: bool
    cache_hit: bool | None
    unavailable_reason: str | None
    stats_delta: dict[str, Any]
    truncated: bool = False
    label: str | None = None


@dataclass(frozen=True)
class ToolRequest:
    pattern: str
    label: str
    tool: str
    phase: str
    cache_state: str
    arguments: dict[str, Any]


@dataclass(frozen=True)
class QueryTargets:
    entity_at: list[dict[str, Any]]
    find_patterns: list[str]
    caller_targets: list[str]
    path_roots: list[str]
    summary_ids: list[str]
    issues_ids: list[str]
    neighborhood_ids: list[str]
    inferred_targets: list[str]


def percentile(values: list[float], percent: int) -> float | None:
    """Return an upper-biased nearest-rank percentile.

    The upper bias is intentional for tail-latency reporting: a four-sample p50
    should not understate the slower half of the observed calls.
    """

    if not values:
        return None
    ordered = sorted(values)
    index = math.ceil((percent / 100.0) * (len(ordered) + 1)) - 1
    index = max(0, min(index, len(ordered) - 1))
    return round(ordered[index], 3)


def _kb(byte_count: int) -> float:
    return round(byte_count / 1024.0, 3)


def _cache_hit_from_result(result: Any) -> bool | None:
    if not isinstance(result, dict) or "cache" not in result:
        return None
    cache = result["cache"]
    if isinstance(cache, dict) and isinstance(cache.get("hit"), bool):
        return bool(cache["hit"])
    if isinstance(cache, str):
        if cache == "hit":
            return True
        if cache in {"miss", "cold", "disabled"}:
            return False
    return None


def _estimated_tokens(response_bytes: int) -> int:
    return max(1, math.ceil(response_bytes / 4))


def _has_useful_result(tool: str, result: Any) -> bool:
    if not isinstance(result, dict) or result.get("available") is False:
        return False
    keys_by_tool = {
        "entity_at": ("entity",),
        "find_entity": ("entities",),
        "callers_of": ("callers",),
        "execution_paths_from": ("paths",),
        "summary": ("summary",),
        "issues_for": ("matched", "drifted", "not_found"),
        "neighborhood": ("callers", "callees", "container", "contained", "references"),
    }
    for key in keys_by_tool.get(tool, ()):
        value = result.get(key)
        if value not in (None, [], {}, ""):
            return True
    return False


def parse_tool_response(
    pattern: str,
    tool: str,
    phase: str,
    cache_state: str,
    response: dict[str, Any],
    latency_ms: float,
    response_bytes: int,
    label: str | None = None,
) -> CallRecord:
    if response.get("error") is not None:
        return CallRecord(
            pattern,
            tool,
            phase,
            cache_state,
            round(latency_ms, 3),
            response_bytes,
            _estimated_tokens(response_bytes),
            False,
            True,
            False,
            False,
            None,
            None,
            {},
            False,
            label,
        )

    envelope: dict[str, Any] = {}
    try:
        content = response["result"]["content"]
        text = content[0]["text"]
        envelope = json.loads(text)
    except (KeyError, IndexError, TypeError, json.JSONDecodeError):
        return CallRecord(
            pattern,
            tool,
            phase,
            cache_state,
            round(latency_ms, 3),
            response_bytes,
            _estimated_tokens(response_bytes),
            False,
            True,
            False,
            False,
            None,
            None,
            {},
            False,
            label,
        )

    result = envelope.get("result")
    result_obj = result if isinstance(result, dict) else {}
    unavailable = result_obj.get("available") is False
    reason = (
        result_obj.get("reason") if isinstance(result_obj.get("reason"), str) else None
    )
    return CallRecord(
        pattern=pattern,
        tool=tool,
        phase=phase,
        cache_state=cache_state,
        latency_ms=round(latency_ms, 3),
        response_bytes=response_bytes,
        estimated_response_tokens=_estimated_tokens(response_bytes),
        ok=envelope.get("ok") is True,
        error=(envelope.get("ok") is not True) or envelope.get("error") is not None,
        unavailable=unavailable,
        useful_result=_has_useful_result(tool, result_obj),
        cache_hit=_cache_hit_from_result(result_obj),
        unavailable_reason=reason,
        stats_delta=envelope.get("stats_delta")
        if isinstance(envelope.get("stats_delta"), dict)
        else {},
        truncated=envelope.get("truncated") is True,
        label=label,
    )


def _summarize_group(records: list[CallRecord]) -> dict[str, Any]:
    cache_records = [
        record.cache_hit for record in records if record.cache_hit is not None
    ]
    tokens_total = 0
    cost_usd = 0.0
    for record in records:
        for key in ("summary_tokens_total", "inferred_tokens_total"):
            value = record.stats_delta.get(key)
            if isinstance(value, int | float):
                tokens_total += int(value)
        for key in ("summary_cost_usd", "inferred_cost_usd"):
            value = record.stats_delta.get(key)
            if isinstance(value, int | float):
                cost_usd += float(value)

    return {
        "call_count": len(records),
        "p50_latency_ms": percentile([record.latency_ms for record in records], 50),
        "p95_latency_ms": percentile([record.latency_ms for record in records], 95),
        "max_latency_ms": max((record.latency_ms for record in records), default=None),
        "response_size_p50_kb": _kb(
            int(percentile([record.response_bytes for record in records], 50) or 0)
        ),
        "response_size_p95_kb": _kb(
            int(percentile([record.response_bytes for record in records], 95) or 0)
        ),
        "response_tokens_p50": percentile(
            [record.estimated_response_tokens for record in records], 50
        ),
        "response_tokens_p95": percentile(
            [record.estimated_response_tokens for record in records], 95
        ),
        "ok_count": sum(1 for record in records if record.ok),
        "available_count": sum(
            1 for record in records if record.ok and not record.unavailable
        ),
        "useful_result_count": sum(1 for record in records if record.useful_result),
        "error_count": sum(1 for record in records if record.error),
        "unavailable_count": sum(1 for record in records if record.unavailable),
        "truncation_count": sum(1 for record in records if record.truncated),
        "summary_cache_hit_rate": (
            round(sum(1 for hit in cache_records if hit) / len(cache_records), 4)
            if cache_records
            else None
        ),
        "tokens_total": tokens_total,
        "cost_usd": round(cost_usd, 6),
    }


def summarize_records(records: list[CallRecord]) -> dict[str, Any]:
    by_pattern: dict[str, list[CallRecord]] = defaultdict(list)
    by_pattern_tool: dict[tuple[str, str], list[CallRecord]] = defaultdict(list)
    by_tool: dict[str, list[CallRecord]] = defaultdict(list)
    by_phase: dict[str, list[CallRecord]] = defaultdict(list)
    for record in records:
        by_pattern[record.pattern].append(record)
        by_pattern_tool[(record.pattern, record.tool)].append(record)
        by_tool[record.tool].append(record)
        by_phase[record.phase].append(record)

    pattern_summary: dict[str, Any] = {}
    for pattern, pattern_records in sorted(by_pattern.items()):
        entry = _summarize_group(pattern_records)
        entry["tools"] = {
            tool: _summarize_group(tool_records)
            for (tool_pattern, tool), tool_records in sorted(by_pattern_tool.items())
            if tool_pattern == pattern
        }
        pattern_summary[pattern] = entry

    storage_backed_tools = {
        "entity_at",
        "find_entity",
        "callers_of",
        "execution_paths_from",
        "neighborhood",
    }
    steady_state_storage = [
        record
        for record in records
        if record.phase == "steady_state" and record.tool in storage_backed_tools
    ]

    return {
        "overall": _summarize_group(records),
        "by_tool": {
            tool: _summarize_group(tool_records)
            for tool, tool_records in sorted(by_tool.items())
        },
        "by_phase": {
            phase: _summarize_group(phase_records)
            for phase, phase_records in sorted(by_phase.items())
        },
        "by_pattern": pattern_summary,
        "gate": {
            "steady_state_storage_backed": _summarize_group(steady_state_storage),
        },
    }


def summary_miss_then_hit(records: list[CallRecord]) -> bool:
    cold_misses = [
        record
        for record in records
        if record.tool == "summary"
        and record.cache_state == "cold"
        and record.cache_hit is False
    ]
    warm_hits = [
        record
        for record in records
        if record.tool == "summary"
        and record.cache_state == "warm"
        and record.cache_hit is True
    ]
    return bool(cold_misses) and len(warm_hits) >= len(cold_misses)


def _read_exact(fd: int, byte_count: int, timeout_seconds: float) -> bytes:
    chunks: list[bytes] = []
    remaining = byte_count
    deadline = time.monotonic() + timeout_seconds
    while remaining:
        timeout = deadline - time.monotonic()
        if timeout <= 0:
            raise TimeoutError(f"timed out reading {byte_count} response bytes")
        readable, _, _ = select.select([fd], [], [], timeout)
        if not readable:
            raise TimeoutError(f"timed out reading {byte_count} response bytes")
        chunk = os.read(fd, remaining)
        if not chunk:
            raise EOFError("server closed stdout")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_frame(
    proc: subprocess.Popen[bytes], timeout_seconds: float
) -> tuple[dict[str, Any], int]:
    if proc.stdout is None:
        raise RuntimeError("process stdout not piped")
    fd = proc.stdout.fileno()
    header = bytearray()
    while not header.endswith(b"\r\n\r\n"):
        header.extend(_read_exact(fd, 1, timeout_seconds))
    headers: dict[str, str] = {}
    for line in bytes(header).decode("ascii").split("\r\n"):
        if not line:
            continue
        name, value = line.split(":", 1)
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    body = _read_exact(fd, length, timeout_seconds)
    return json.loads(body), len(body)


def write_frame(proc: subprocess.Popen[bytes], message: dict[str, Any]) -> None:
    if proc.stdin is None:
        raise RuntimeError("process stdin not piped")
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    proc.stdin.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n")
    proc.stdin.write(body)
    proc.stdin.flush()


class McpClient:
    def __init__(
        self,
        loomweave_bin: Path,
        project: Path,
        config: Path | None,
        timeout_seconds: float,
    ) -> None:
        command = [str(loomweave_bin), "serve", "--path", str(project)]
        if config is not None:
            command.extend(["--config", str(config)])
        self.proc = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.timeout_seconds = timeout_seconds

    def request(self, message: dict[str, Any]) -> tuple[dict[str, Any], int, float]:
        started = time.perf_counter()
        write_frame(self.proc, message)
        response, size = read_frame(self.proc, self.timeout_seconds)
        latency_ms = (time.perf_counter() - started) * 1000.0
        return response, size, latency_ms

    def close(self) -> str:
        if self.proc.stdin is not None:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            self.proc.wait(timeout=10)
        stderr = ""
        if self.proc.stderr is not None:
            stderr = self.proc.stderr.read().decode("utf-8", "replace")
        if self.proc.returncode != 0:
            raise RuntimeError(
                f"loomweave serve exited {self.proc.returncode}; stderr={stderr}"
            )
        return stderr


def _rows(
    conn: sqlite3.Connection, query: str, params: tuple[Any, ...] = ()
) -> list[sqlite3.Row]:
    return list(conn.execute(query, params))


def discover_targets(project: Path) -> QueryTargets:
    db_path = project / ".weft" / "loomweave" / "loomweave.db"
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        entity_rows = _rows(
            conn,
            """
            SELECT id, kind, short_name, source_file_path, source_line_start
            FROM entities
            WHERE source_file_path IS NOT NULL AND source_line_start IS NOT NULL
            ORDER BY CASE kind WHEN 'function' THEN 0 WHEN 'class' THEN 1 ELSE 2 END, id
            LIMIT 200
            """,
        )
        entity_at = []
        for row in entity_rows:
            source_path = Path(row["source_file_path"])
            try:
                file_arg = str(source_path.relative_to(project))
            except ValueError:
                file_arg = os.path.relpath(source_path, project)
            entity_at.append(
                {
                    "id": row["id"],
                    "file": file_arg,
                    "line": int(row["source_line_start"]),
                }
            )

        ids = [str(row["id"]) for row in entity_rows]
        fallback_id = ids[0] if ids else ""
        find_patterns = [
            str(row["short_name"] or row["id"]).split(":")[-1]
            for row in entity_rows[:20]
        ]
        if not find_patterns and fallback_id:
            find_patterns = [fallback_id]

        caller_targets = [
            row["to_id"]
            for row in _rows(
                conn,
                "SELECT DISTINCT to_id FROM edges WHERE kind = 'calls' ORDER BY to_id LIMIT 50",
            )
        ]
        path_roots = [
            row["from_id"]
            for row in _rows(
                conn,
                "SELECT DISTINCT from_id FROM edges WHERE kind = 'calls' ORDER BY from_id LIMIT 50",
            )
        ]
        summary_ids = [
            row["id"]
            for row in _rows(
                conn,
                "SELECT id FROM entities WHERE content_hash IS NOT NULL ORDER BY id LIMIT 50",
            )
        ]
        issues_ids = [
            row["id"]
            for row in _rows(conn, "SELECT id FROM entities ORDER BY id LIMIT 50")
        ]
        neighborhood_ids = ids[:50] or issues_ids[:50]
        inferred_targets = [
            row["id"]
            for row in _rows(
                conn,
                """
                SELECT DISTINCT e.id
                FROM entities e
                JOIN entity_unresolved_call_sites s
                  ON s.callee_expr = e.short_name
                  OR s.callee_expr LIKE '%' || e.short_name || '%'
                ORDER BY e.id
                LIMIT 10
                """,
            )
        ]
    finally:
        conn.close()

    def fallback(values: list[str]) -> list[str]:
        return values or ([fallback_id] if fallback_id else [])

    return QueryTargets(
        entity_at=entity_at,
        find_patterns=find_patterns or ["python"],
        caller_targets=fallback(caller_targets),
        path_roots=fallback(path_roots),
        summary_ids=fallback(summary_ids),
        issues_ids=fallback(issues_ids),
        neighborhood_ids=fallback(neighborhood_ids),
        inferred_targets=fallback(inferred_targets),
    )


def _cycle(values: list[Any], index: int) -> Any:
    if not values:
        raise ValueError("no target values available")
    return values[index % len(values)]


def _request(
    pattern: str,
    label: str,
    tool: str,
    phase: str,
    cache_state: str,
    arguments: dict[str, Any],
) -> ToolRequest:
    return ToolRequest(
        pattern=pattern,
        label=label,
        tool=tool,
        phase=phase,
        cache_state=cache_state,
        arguments=arguments,
    )


def light_pattern(targets: QueryTargets) -> list[ToolRequest]:
    first_entity_at = _cycle(targets.entity_at, 0)
    return [
        _request(
            "light",
            "L01-entity-at",
            "entity_at",
            "cold_start",
            "none",
            {"file": first_entity_at["file"], "line": first_entity_at["line"]},
        ),
        _request(
            "light",
            "L02-find-entity",
            "find_entity",
            "cold_start",
            "none",
            {"pattern": _cycle(targets.find_patterns, 0), "limit": 10},
        ),
        _request(
            "light",
            "L03-callers-of",
            "callers_of",
            "cold_start",
            "none",
            {"id": _cycle(targets.caller_targets, 0)},
        ),
        _request(
            "light",
            "L04-neighborhood",
            "neighborhood",
            "cold_start",
            "none",
            {"id": _cycle(targets.neighborhood_ids, 0)},
        ),
        _request(
            "light",
            "L05-entity-at-repeat",
            "entity_at",
            "cold_start",
            "none",
            {"file": first_entity_at["file"], "line": first_entity_at["line"]},
        ),
    ]


def _medium_like(
    pattern: str,
    targets: QueryTargets,
    count: int,
    *,
    phase: str | None = None,
    summary_cache_state: str | None = None,
    label_prefix: str | None = None,
) -> list[ToolRequest]:
    warm = pattern == "medium-warm"
    phase = phase or ("steady_state" if warm else "warmup")
    cache_state = summary_cache_state or ("warm" if warm else "cold")
    label_prefix = label_prefix or ("MW" if warm else "MC")
    manifest = [
        (
            "entity_at",
            {
                "file": _cycle(targets.entity_at, 0)["file"],
                "line": _cycle(targets.entity_at, 0)["line"],
            },
        ),
        ("find_entity", {"pattern": _cycle(targets.find_patterns, 0), "limit": 20}),
        ("callers_of", {"id": _cycle(targets.caller_targets, 0)}),
        ("execution_paths_from", {"id": _cycle(targets.path_roots, 0), "max_depth": 3}),
        ("summary", {"id": _cycle(targets.summary_ids, 0)}),
        (
            "issues_for",
            {"id": _cycle(targets.issues_ids, 0), "include_contained": True},
        ),
        ("neighborhood", {"id": _cycle(targets.neighborhood_ids, 0)}),
        (
            "entity_at",
            {
                "file": _cycle(targets.entity_at, 1)["file"],
                "line": _cycle(targets.entity_at, 1)["line"],
            },
        ),
        ("find_entity", {"pattern": _cycle(targets.find_patterns, 1), "limit": 20}),
        ("callers_of", {"id": _cycle(targets.caller_targets, 1)}),
        ("summary", {"id": _cycle(targets.summary_ids, 1)}),
        ("neighborhood", {"id": _cycle(targets.neighborhood_ids, 1)}),
        ("execution_paths_from", {"id": _cycle(targets.path_roots, 1), "max_depth": 3}),
        (
            "issues_for",
            {"id": _cycle(targets.issues_ids, 1), "include_contained": False},
        ),
        (
            "entity_at",
            {
                "file": _cycle(targets.entity_at, 2)["file"],
                "line": _cycle(targets.entity_at, 2)["line"],
            },
        ),
        ("find_entity", {"pattern": _cycle(targets.find_patterns, 2), "limit": 20}),
        ("summary", {"id": _cycle(targets.summary_ids, 2)}),
        (
            "callers_of",
            {"id": _cycle(targets.caller_targets, 2), "confidence": "ambiguous"},
        ),
        (
            "neighborhood",
            {"id": _cycle(targets.neighborhood_ids, 2), "confidence": "ambiguous"},
        ),
        ("execution_paths_from", {"id": _cycle(targets.path_roots, 2), "max_depth": 2}),
    ]
    requests: list[ToolRequest] = []
    for i in range(count):
        tool, arguments = manifest[i % len(manifest)]
        request_cache_state = cache_state if tool == "summary" else "none"
        requests.append(
            _request(
                pattern,
                f"{label_prefix}{i + 1:02d}-{tool.replace('_', '-')}",
                tool,
                phase,
                request_cache_state,
                arguments,
            )
        )
    return requests


def inferred_pattern(targets: QueryTargets) -> list[ToolRequest]:
    return [
        _request(
            "inferred",
            f"callers-inferred-{i}",
            "callers_of",
            "steady_state",
            "inferred",
            {"id": target, "confidence": "inferred"},
        )
        for i, target in enumerate(targets.inferred_targets[:5])
    ]


def build_requests(
    targets: QueryTargets, heavy_count: int, include_inferred: bool
) -> tuple[list[ToolRequest], list[str]]:
    requests = []
    skipped = []
    requests.extend(light_pattern(targets))
    requests.extend(_medium_like("medium-cold", targets, 20))
    requests.extend(_medium_like("medium-warm", targets, 20))
    requests.extend(
        _medium_like(
            "heavy",
            targets,
            max(50, heavy_count),
            phase="steady_state",
            summary_cache_state="warm",
            label_prefix="H",
        )
    )
    if include_inferred and targets.inferred_targets:
        requests.extend(inferred_pattern(targets))
    else:
        skipped.append("inferred")
    return requests, skipped


def run_driver(args: argparse.Namespace) -> dict[str, Any]:
    project = args.project.resolve()
    loomweave_bin = args.loomweave_bin.resolve()
    config = args.config.resolve() if args.config else None
    targets = discover_targets(project)
    requests, skipped_patterns = build_requests(
        targets, args.heavy_count, not args.skip_inferred
    )

    client = McpClient(loomweave_bin, project, config, args.timeout_seconds)
    records: list[CallRecord] = []
    try:
        init_response, _, initialize_latency_ms = client.request(
            {
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "loomweave-b8-driver", "version": "0.1.0"},
                },
            }
        )
        tools_response, _, tools_list_latency_ms = client.request(
            {"jsonrpc": "2.0", "id": "tools", "method": "tools/list", "params": {}}
        )
        tools = [tool["name"] for tool in tools_response["result"]["tools"]]

        for ordinal, request in enumerate(requests):
            message = {
                "jsonrpc": "2.0",
                "id": f"{request.pattern}-{ordinal}",
                "method": "tools/call",
                "params": {"name": request.tool, "arguments": request.arguments},
            }
            response, response_size, latency_ms = client.request(message)
            records.append(
                parse_tool_response(
                    request.pattern,
                    request.tool,
                    request.phase,
                    request.cache_state,
                    response,
                    latency_ms,
                    response_size,
                    request.label,
                )
            )
    finally:
        client.close()

    return {
        "generated_at_unix": int(time.time()),
        "project": str(project),
        "loomweave_bin": str(loomweave_bin),
        "config": str(config) if config else None,
        "initialize": init_response,
        "initialize_latency_ms": round(initialize_latency_ms, 3),
        "tools_list_latency_ms": round(tools_list_latency_ms, 3),
        "tools": tools,
        "skipped_patterns": skipped_patterns,
        "manifest": [dataclasses.asdict(request) for request in requests],
        "records": [dataclasses.asdict(record) for record in records],
        "summary_miss_then_hit": summary_miss_then_hit(records),
        "summary": summarize_records(records),
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--project",
        type=Path,
        required=True,
        help="Analyzed project root containing .weft/loomweave/loomweave.db",
    )
    parser.add_argument(
        "--loomweave-bin", type=Path, default=Path("target/release/loomweave")
    )
    parser.add_argument(
        "--config", type=Path, help="Optional loomweave serve config path"
    )
    parser.add_argument("--output", type=Path, required=True, help="JSON output path")
    parser.add_argument("--heavy-count", type=int, default=50)
    parser.add_argument("--timeout-seconds", type=float, default=120.0)
    parser.add_argument("--skip-inferred", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    result = run_driver(args)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(result, indent=2, sort_keys=True), encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
