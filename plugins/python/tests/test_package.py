"""Package-level smoke tests for package metadata and plugin manifest."""

from __future__ import annotations

import tomllib
from pathlib import Path

import clarion_plugin_python

_PLUGIN_ROOT = Path(__file__).resolve().parents[1]


def test_package_version_matches_pyproject() -> None:
    assert clarion_plugin_python.__version__ == "0.1.4"


def test_manifest_declares_references_edge_kind() -> None:
    manifest = tomllib.loads((_PLUGIN_ROOT / "plugin.toml").read_text(encoding="utf-8"))

    assert manifest["plugin"]["version"] == "0.1.4"
    assert manifest["ontology"]["ontology_version"] == "0.5.0"
    assert manifest["ontology"]["edge_kinds"] == ["contains", "calls", "references"]
