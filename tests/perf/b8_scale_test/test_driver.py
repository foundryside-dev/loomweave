import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent))

import driver  # noqa: E402


def test_percentile_uses_nearest_rank_for_tail_latency() -> None:
    values = [100.0, 20.0, 40.0, 10.0]

    assert driver.percentile(values, 50) == 40.0
    assert driver.percentile(values, 95) == 100.0
    assert driver.percentile([], 95) is None


def test_parse_tool_response_extracts_envelope_and_size() -> None:
    envelope = {
        "ok": True,
        "result": {"cache": {"hit": True}},
        "error": None,
        "truncated": False,
        "stats_delta": {"summary_cache_hits_total": 1},
    }
    response = {
        "jsonrpc": "2.0",
        "id": "summary-1",
        "result": {"content": [{"type": "text", "text": json.dumps(envelope)}]},
    }

    parsed = driver.parse_tool_response(
        "medium-warm",
        "summary",
        "steady_state",
        "warm",
        response,
        125.0,
        512,
    )

    assert parsed.pattern == "medium-warm"
    assert parsed.tool == "summary"
    assert parsed.phase == "steady_state"
    assert parsed.cache_state == "warm"
    assert parsed.ok is True
    assert parsed.unavailable is False
    assert parsed.cache_hit is True
    assert parsed.estimated_response_tokens == 128
    assert parsed.stats_delta == {"summary_cache_hits_total": 1}
    assert parsed.response_bytes == 512


def test_summarize_records_groups_by_pattern_and_tool() -> None:
    records = [
        driver.CallRecord(
            "light",
            "find_entity",
            "steady_state",
            "none",
            20.0,
            100,
            25,
            True,
            False,
            False,
            True,
            None,
            None,
            {},
        ),
        driver.CallRecord(
            "light",
            "find_entity",
            "steady_state",
            "none",
            40.0,
            120,
            30,
            True,
            False,
            False,
            True,
            None,
            None,
            {},
        ),
        driver.CallRecord(
            "light",
            "summary",
            "warmup",
            "cold",
            100.0,
            200,
            50,
            True,
            False,
            False,
            True,
            True,
            None,
            {"summary_tokens_total": 5},
        ),
        driver.CallRecord(
            "light",
            "summary",
            "steady_state",
            "warm",
            300.0,
            240,
            60,
            True,
            False,
            True,
            False,
            False,
            "llm-disabled",
            {},
        ),
    ]

    summary = driver.summarize_records(records)

    find = summary["by_pattern"]["light"]["tools"]["find_entity"]
    assert find["call_count"] == 2
    assert find["p50_latency_ms"] == 40.0
    assert find["p95_latency_ms"] == 40.0
    assert find["response_size_p95_kb"] == pytest.approx(0.117, abs=0.001)
    assert find["response_tokens_p95"] == 30
    assert find["useful_result_count"] == 2

    pattern = summary["by_pattern"]["light"]
    assert pattern["call_count"] == 4
    assert pattern["unavailable_count"] == 1
    assert pattern["summary_cache_hit_rate"] == 0.5
    assert pattern["tokens_total"] == 5
    assert summary["by_phase"]["steady_state"]["call_count"] == 3
    assert summary["gate"]["steady_state_storage_backed"]["p95_latency_ms"] == 40.0
    assert driver.summary_miss_then_hit(records) is False


def test_manifest_is_deterministic_and_marks_cache_state() -> None:
    targets = driver.QueryTargets(
        entity_at=[{"id": "e0", "file": "demo.py", "line": 1}],
        find_patterns=["demo"],
        caller_targets=["target"],
        path_roots=["entry"],
        summary_ids=["summary"],
        issues_ids=["issues"],
        neighborhood_ids=["neighbor"],
        inferred_targets=["inferred"],
    )

    requests, skipped = driver.build_requests(
        targets, heavy_count=50, include_inferred=True
    )

    assert skipped == []
    assert [(request.label, request.tool) for request in requests[:5]] == [
        ("L01-entity-at", "entity_at"),
        ("L02-find-entity", "find_entity"),
        ("L03-callers-of", "callers_of"),
        ("L04-neighborhood", "neighborhood"),
        ("L05-entity-at-repeat", "entity_at"),
    ]
    medium_summary = [
        request
        for request in requests
        if request.pattern == "medium-cold" and request.tool == "summary"
    ]
    assert len(medium_summary) == 3
    assert {request.phase for request in medium_summary} == {"warmup"}
    assert {request.cache_state for request in medium_summary} == {"cold"}

    warm_summary = [
        request
        for request in requests
        if request.pattern == "medium-warm" and request.tool == "summary"
    ]
    assert len(warm_summary) == 3
    assert {request.arguments["id"] for request in warm_summary} == {"summary"}
    assert {request.cache_state for request in warm_summary} == {"warm"}
