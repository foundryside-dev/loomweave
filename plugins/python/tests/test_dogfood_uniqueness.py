"""ADR-052 GATE: zero duplicate entity ids emitted over the plugin's own tree.

Python analog of the Rust plugin's ``dogfood_uniqueness.rs`` (ADR-049): the
extractor, run over every ``.py`` file in ``plugins/python`` itself, must emit
a globally unique id set. Duplicates *within* a file are dropped first-wins and
counted (``duplicate_entities_dropped_total``) — that is the frozen ADR-052
semantic — but an id *emitted* twice (within or across files) is a real
identity bug, not a test defect: do NOT weaken the assertion to make it pass.
"""

from __future__ import annotations

from pathlib import Path

from loomweave_plugin_python.extractor import extract_with_stats

PLUGIN_ROOT = Path(__file__).resolve().parents[1]
_EXCLUDED_PARTS = {".venv", "__pycache__"}


def test_extractor_emits_zero_duplicate_ids_over_this_plugin() -> None:
    seen: dict[str, str] = {}
    collisions: list[tuple[str, str, str]] = []
    for path in sorted(PLUGIN_ROOT.rglob("*.py")):
        if _EXCLUDED_PARTS.intersection(path.parts):
            continue
        relative = path.relative_to(PLUGIN_ROOT).as_posix()
        result = extract_with_stats(path.read_text(encoding="utf-8"), relative)
        for entity in result.entities:
            if entity["id"] in seen:
                collisions.append((entity["id"], seen[entity["id"]], relative))
            else:
                seen[entity["id"]] = relative
    assert len(seen) > 100, f"expected a substantial entity set, got {len(seen)}"
    assert collisions == [], f"ADR-052 GATE FAILED — duplicate ids emitted: {collisions!r}"
