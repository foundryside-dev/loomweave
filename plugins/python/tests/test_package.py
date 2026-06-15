"""Package-level smoke tests for package metadata and plugin manifest."""

from __future__ import annotations

import tomllib
from pathlib import Path
from typing import Any

import loomweave_plugin_python
from loomweave_plugin_python.wardline_descriptor import EXPECTED_DESCRIPTOR_VERSION

_PLUGIN_ROOT = Path(__file__).resolve().parents[1]


def _read_toml(path: Path) -> dict[str, Any]:
    return tomllib.loads(path.read_text(encoding="utf-8"))


def test_package_version_matches_pyproject() -> None:
    assert loomweave_plugin_python.__version__ == "1.1.0"


def test_plugin_version_lockstep_across_pyproject_manifest_and_module() -> None:
    """Guard against the version-drift class of bug (clarion-496a8192cc).

    The three values must move together: bumping pyproject without bumping
    plugin.toml ships an sdist whose handshake reports the wrong version.
    """
    pyproject = _read_toml(_PLUGIN_ROOT / "pyproject.toml")
    manifest = _read_toml(_PLUGIN_ROOT / "plugin.toml")

    pyproject_version = pyproject["project"]["version"]
    manifest_version = manifest["plugin"]["version"]
    module_version = loomweave_plugin_python.__version__

    assert pyproject_version == manifest_version == module_version, (
        f"version drift: pyproject={pyproject_version!r}, "
        f"plugin.toml={manifest_version!r}, __init__={module_version!r}"
    )


def test_manifest_declares_current_v1_ontology_only() -> None:
    manifest = _read_toml(_PLUGIN_ROOT / "plugin.toml")

    assert manifest["plugin"]["version"] == "1.1.0"
    assert manifest["capabilities"]["runtime"]["wardline_aware"] is True
    assert manifest["integrations"]["wardline"]["expected_descriptor_version"] == (
        EXPECTED_DESCRIPTOR_VERSION
    )
    assert manifest["ontology"]["ontology_version"] == "0.8.0"
    assert manifest["ontology"]["entity_kinds"] == ["function", "class", "module"]
    assert manifest["ontology"]["edge_kinds"] == [
        "contains",
        "calls",
        "references",
        "imports",
        "inherits_from",
        "decorates",
    ]
    # The ontology kind is `decorates` (decorator → decorated), not the
    # v1.0-requirements-era `decorated_by` spelling.
    assert "decorated_by" not in manifest["ontology"]["edge_kinds"]
    assert "uses_type" not in manifest["ontology"]["edge_kinds"]
    assert "alias_of" not in manifest["ontology"]["edge_kinds"]
