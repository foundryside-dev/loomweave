#!/usr/bin/env python3
"""Read-only downstream check for Loomweave's authoritative classifier golden."""

from __future__ import annotations

import argparse
import hashlib
import os
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path


PUBLIC_TAGS = ("cli-command", "entry-point", "exported-api", "http-route")


def git_status(root: Path) -> bytes:
    return subprocess.check_output(["git", "-C", str(root), "status", "--porcelain=v1"])


def sqlite_snapshot(root: Path) -> dict[str, tuple[int, str]]:
    store = root / ".weft/loomweave"
    return {
        path.name: (path.stat().st_size, hashlib.sha256(path.read_bytes()).hexdigest())
        for path in sorted(store.glob("loomweave.db*"))
        if path.is_file()
    }


def create_catalog(root: Path, coverage_bytes: bytes) -> None:
    store = root / ".weft/loomweave"
    store.mkdir(parents=True)
    (store / "instance_id").write_text(
        "00000000-0000-4000-8000-000000000001\n", encoding="utf-8"
    )
    with sqlite3.connect(store / "loomweave.db") as connection:
        connection.executescript(
            """
            create table entities (
              id text primary key, plugin_id text not null, kind text not null,
              name text not null, short_name text not null, parent_id text,
              source_file_id text, source_file_path text, source_byte_start integer,
              source_byte_end integer, source_line_start integer, source_line_end integer,
              properties text not null, content_hash text, summary text, wardline text,
              first_seen_commit text, last_seen_commit text, created_at text not null,
              updated_at text not null, signature text
            );
            create table entity_tags (
              entity_id text not null, plugin_id text not null, tag text not null,
              primary key (entity_id, plugin_id, tag)
            );
            create table runs (
              id text primary key, started_at text not null, completed_at text,
              config text, stats text, status text not null, analyzed_at_commit text,
              owner_pid integer, heartbeat_at text
            );
            create table sei_bindings (
              sei text primary key, current_locator text, body_hash text, signature text,
              status text not null, born_run_id text not null, updated_run_id text not null,
              updated_at text not null
            );
            create table sei_lineage (
              id integer primary key autoincrement, sei text not null, event text not null,
              old_locator text, new_locator text, run_id text not null, recorded_at text not null
            );
            """
        )
        connection.execute("pragma application_id = 1280137046")
        connection.execute("pragma user_version = 12")
        stats_bytes = b'{"classifier_coverage":' + coverage_bytes + b"}"
        assert coverage_bytes in stats_bytes
        connection.execute(
            """insert into entities(
                 id, plugin_id, kind, name, short_name, source_file_path, properties,
                 created_at, updated_at
               ) values (?, ?, ?, ?, ?, ?, '{}', ?, ?)""",
            (
                "python:module:fixture",
                "python",
                "module",
                "fixture",
                "fixture",
                str(root / "fixture.py"),
                "2026-07-12T00:00:00Z",
                "2026-07-12T00:00:00Z",
            ),
        )
        connection.execute(
            """insert into runs(id, started_at, completed_at, config, stats, status)
               values (?, ?, ?, '{}', ?, 'completed')""",
            (
                "producer-golden-run",
                "2026-07-12T00:00:00Z",
                "2026-07-12T00:00:01Z",
                stats_bytes.decode("utf-8"),
            ),
        )
        stored_stats = (
            connection.execute(
                "select stats from runs where id = 'producer-golden-run'"
            )
            .fetchone()[0]
            .encode("utf-8")
        )
        assert coverage_bytes in stored_stats, (
            "SQLite stats must retain the exact golden bytes"
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--plainweave-root", type=Path, default=Path("/home/john/plainweave")
    )
    args = parser.parse_args()
    plainweave_root = args.plainweave_root.resolve()
    before = git_status(plainweave_root)

    loomweave_root = Path(__file__).resolve().parents[1]
    coverage_path = (
        loomweave_root
        / "crates/loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json"
    )
    coverage_bytes = coverage_path.read_bytes()

    os.environ["PYTHONDONTWRITEBYTECODE"] = "1"
    sys.dont_write_bytecode = True
    source = plainweave_root / "src"
    if str(source) not in sys.path:
        sys.path.insert(0, str(source))
    from plainweave.loomweave_adapter import LoomweaveAdapter  # noqa: PLC0415

    with tempfile.TemporaryDirectory(prefix="plainweave-loomweave-golden-") as temp:
        root = Path(temp)
        create_catalog(root, coverage_bytes)
        sqlite_before = sqlite_snapshot(root)
        page = LoomweaveAdapter(root).list_catalog(limit=200, offset=0)
        classifications = {
            row["surface_class"]: row
            for row in page.classifier_coverage["classifications"]
        }
        assert set(classifications) == set(PUBLIC_TAGS), classifications
        for tag in PUBLIC_TAGS:
            row = classifications[tag]
            assert row["state"] == "supported-complete", row
            assert row["supporting_plugins"] == ["python"], row
            assert row["unsupported_plugins"] == [], row
        assert classifications["http-route"]["matches"] == 0, classifications[
            "http-route"
        ]
        assert page.coverage["complete"] is True, page.coverage
        assert page.coverage["present_tags"] == list(PUBLIC_TAGS), page.coverage
        assert page.coverage["absent_tags"] == [], page.coverage
        sqlite_after = sqlite_snapshot(root)
        assert sqlite_after == sqlite_before, (
            "Plainweave adapter mutated the temporary Loomweave DB or its SQLite sidecars",
            sqlite_before,
            sqlite_after,
        )

    after = git_status(plainweave_root)
    if after != before:
        raise SystemExit(
            "Plainweave worktree changed during read-only golden validation"
        )
    print(
        "Plainweave LoomweaveAdapter accepted producer golden: supported-complete, http-route=0"
    )


if __name__ == "__main__":
    main()
