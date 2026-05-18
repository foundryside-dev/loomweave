#!/usr/bin/env python3
"""Extract analyze-time measurements from clarion.db for the B.8 memo."""

from __future__ import annotations

import json
import sqlite3
import sys
from collections import Counter
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: extract-metrics.py <clarion.db> <output.json>")
        return 2
    db_path = Path(sys.argv[1])
    out_path = Path(sys.argv[2])

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row

    payload: dict = {}

    # Tables present
    payload["tables"] = sorted(
        row[0] for row in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table'"
        )
    )

    def table_count(name: str) -> int | None:
        if name not in payload["tables"]:
            return None
        return conn.execute(f"SELECT COUNT(*) FROM {name}").fetchone()[0]

    payload["entities_total"] = table_count("entities")
    payload["edges_total"] = table_count("edges")
    payload["findings_total"] = table_count("findings")
    payload["runs_total"] = table_count("runs")

    if "entities" in payload["tables"]:
        payload["entities_by_kind"] = {
            row["kind"]: row["c"]
            for row in conn.execute(
                "SELECT kind, COUNT(*) AS c FROM entities GROUP BY kind ORDER BY kind"
            )
        }

    if "edges" in payload["tables"]:
        payload["edges_by_kind_confidence"] = [
            {"kind": row["kind"], "confidence": row["confidence"], "count": row["c"]}
            for row in conn.execute(
                "SELECT kind, confidence, COUNT(*) AS c "
                "FROM edges GROUP BY kind, confidence ORDER BY kind, confidence"
            )
        ]

    # Runs table — most recent run details
    if "runs" in payload["tables"]:
        cols = [r[1] for r in conn.execute("PRAGMA table_info(runs)")]
        payload["runs_columns"] = cols
        rows = list(conn.execute("SELECT * FROM runs ORDER BY rowid DESC LIMIT 5"))
        payload["recent_runs"] = [dict(r) for r in rows]

    # Findings breakdown
    if "findings" in payload["tables"]:
        finding_cols = [r[1] for r in conn.execute("PRAGMA table_info(findings)")]
        payload["findings_columns"] = finding_cols
        if "code" in finding_cols:
            payload["findings_by_code"] = {
                row["code"]: row["c"]
                for row in conn.execute(
                    "SELECT code, COUNT(*) AS c FROM findings GROUP BY code ORDER BY code"
                )
            }

    # Run stats (counters embedded in runs.stats JSON if present)
    if "runs" in payload["tables"] and "stats" in (
        r[1] for r in conn.execute("PRAGMA table_info(runs)")
    ):
        stats_row = conn.execute(
            "SELECT stats FROM runs ORDER BY rowid DESC LIMIT 1"
        ).fetchone()
        if stats_row and stats_row[0]:
            try:
                payload["latest_run_stats"] = json.loads(stats_row[0])
            except json.JSONDecodeError:
                payload["latest_run_stats_raw"] = stats_row[0]

    # Unresolved-call-site side table if present
    for side_table in (
        "entity_unresolved_call_sites",
        "unresolved_call_sites",
        "unresolved_reference_sites",
    ):
        if side_table in payload["tables"]:
            payload[f"{side_table}_rows"] = conn.execute(
                f"SELECT COUNT(*) FROM {side_table}"
            ).fetchone()[0]

    # DB file size
    payload["db_file_bytes"] = db_path.stat().st_size

    out_path.write_text(json.dumps(payload, indent=2, sort_keys=True, default=str), encoding="utf-8")
    print(json.dumps({
        "wall_db_bytes": payload["db_file_bytes"],
        "entities_total": payload["entities_total"],
        "edges_total": payload["edges_total"],
        "tables": payload["tables"],
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
