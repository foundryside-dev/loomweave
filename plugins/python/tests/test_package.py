"""Package-level smoke tests for package metadata and plugin manifest."""

from __future__ import annotations

import tomllib
from pathlib import Path
from typing import Any

import clarion_plugin_python

_PLUGIN_ROOT = Path(__file__).resolve().parents[1]


def _read_toml(path: Path) -> dict[str, Any]:
    return tomllib.loads(path.read_text(encoding="utf-8"))


def test_package_version_matches_pyproject() -> None:
    assert clarion_plugin_python.__version__ == "1.1.0"


def test_plugin_version_lockstep_across_pyproject_manifest_and_module() -> None:
    """Guard against the version-drift class of bug (clarion-496a8192cc).

    The three values must move together: bumping pyproject without bumping
    plugin.toml ships an sdist whose handshake reports the wrong version.
    """
    pyproject = _read_toml(_PLUGIN_ROOT / "pyproject.toml")
    manifest = _read_toml(_PLUGIN_ROOT / "plugin.toml")

    pyproject_version = pyproject["project"]["version"]
    manifest_version = manifest["plugin"]["version"]
    module_version = clarion_plugin_python.__version__

    assert pyproject_version == manifest_version == module_version, (
        f"version drift: pyproject={pyproject_version!r}, "
        f"plugin.toml={manifest_version!r}, __init__={module_version!r}"
    )


def test_manifest_declares_references_edge_kind() -> None:
    manifest = _read_toml(_PLUGIN_ROOT / "plugin.toml")

    assert manifest["plugin"]["version"] == "1.1.0"
    assert manifest["ontology"]["ontology_version"] == "0.6.0"
    assert manifest["ontology"]["edge_kinds"] == ["contains", "calls", "references", "imports"]
