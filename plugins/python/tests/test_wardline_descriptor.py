from __future__ import annotations

import builtins
from typing import TYPE_CHECKING, Any

from loomweave_plugin_python.wardline_descriptor import (
    EXPECTED_DESCRIPTOR_VERSION,
    load_wardline_descriptor,
)

if TYPE_CHECKING:
    from pathlib import Path


_DESCRIPTOR = """\
version: wardline-generic-2
entries:
- canonical_name: external_boundary
  group: 1
  attrs: {}
- canonical_name: trust_boundary
  group: 1
  attrs:
    _wardline_to_level: TaintState
- canonical_name: trusted
  group: 1
  attrs:
    _wardline_level: TaintState
"""


class _FakePackagePath:
    def __init__(self, path: Any) -> None:
        self._path = path

    def __str__(self) -> str:
        return "wardline/core/vocabulary.yaml"

    def locate(self) -> Any:
        return self._path


def test_project_descriptor_wins_over_package_descriptor(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    project_descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    project_descriptor.parent.mkdir(parents=True)
    project_descriptor.write_text(_DESCRIPTOR, encoding="utf-8")
    package_descriptor = tmp_path / "package-vocabulary.yaml"
    package_descriptor.write_text(
        _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-9"),
        encoding="utf-8",
    )
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda name: [_FakePackagePath(package_descriptor)] if name == "wardline" else None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "enabled"
    assert state.source == "project"
    assert state.descriptor_version == EXPECTED_DESCRIPTOR_VERSION
    assert state.vocabulary is not None
    assert sorted(state.vocabulary.entries_by_name) == [
        "external_boundary",
        "trust_boundary",
        "trusted",
    ]


def test_weft_descriptor_location_is_read(tmp_path: Path, monkeypatch: Any) -> None:
    # ADR-046: the consolidated .weft/wardline/ location is read as a project
    # descriptor (preferred over the package descriptor).
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text(_DESCRIPTOR, encoding="utf-8")
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda _name: None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "enabled"
    assert state.source == "project"
    assert state.descriptor_version == EXPECTED_DESCRIPTOR_VERSION


def test_legacy_wardline_location_is_not_read(tmp_path: Path, monkeypatch: Any) -> None:
    # ADR-046 clean break: a descriptor at only the pre-consolidation .wardline/
    # path is NOT read. With no package descriptor either, the loader degrades to
    # absent — a loud signal of a mis-sequenced cutover, not a silent stale read.
    legacy = tmp_path / ".wardline" / "vocabulary.yaml"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(_DESCRIPTOR, encoding="utf-8")
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda _name: None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "absent"
    assert state.reason == "not_found"
    assert state.vocabulary is None


def test_legacy_wardline_location_does_not_shadow_package(tmp_path: Path, monkeypatch: Any) -> None:
    # A legacy .wardline/ descriptor is ignored, so the package descriptor (the
    # next resolution rung) is what loads — the legacy file never wins.
    legacy = tmp_path / ".wardline" / "vocabulary.yaml"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(
        _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-9"),
        encoding="utf-8",
    )
    package_descriptor = tmp_path / "package-vocabulary.yaml"
    package_descriptor.write_text(_DESCRIPTOR, encoding="utf-8")
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda name: [_FakePackagePath(package_descriptor)] if name == "wardline" else None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "enabled"
    assert state.source == "package"
    assert state.descriptor_version == EXPECTED_DESCRIPTOR_VERSION


def test_package_descriptor_loads_without_importing_wardline(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    package_descriptor = tmp_path / "vocabulary.yaml"
    package_descriptor.write_text(_DESCRIPTOR, encoding="utf-8")

    real_import = builtins.__import__

    def fail_import(name: str, *args: Any, **kwargs: Any) -> object:
        if name == "wardline" or name.startswith("wardline."):
            msg = f"unexpected Wardline import: {name}"
            raise AssertionError(msg)
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda name: [_FakePackagePath(package_descriptor)] if name == "wardline" else None,
    )
    monkeypatch.setattr(
        "builtins.__import__",
        fail_import,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "enabled"
    assert state.source == "package"
    assert state.vocabulary is not None
    assert state.vocabulary.confidence_basis == "descriptor"


def test_absent_descriptor_degrades_without_vocabulary(tmp_path: Path, monkeypatch: Any) -> None:
    monkeypatch.setattr(
        "loomweave_plugin_python.wardline_descriptor.metadata.files",
        lambda _name: None,
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "absent"
    assert state.vocabulary is None
    assert state.reason == "not_found"


def test_invalid_descriptor_shape_degrades_to_absent(tmp_path: Path) -> None:
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text("version: 3\nentries: nope\n", encoding="utf-8")

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "absent"
    assert state.vocabulary is None
    assert state.reason == "invalid_descriptor"


def test_duplicate_canonical_names_degrade_to_absent(tmp_path: Path) -> None:
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text(
        """\
version: wardline-generic-2
entries:
- canonical_name: trusted
  group: 1
  attrs: {}
- canonical_name: trusted
  group: 1
  attrs: {}
""",
        encoding="utf-8",
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "absent"
    assert state.reason == "invalid_descriptor"


def test_version_skew_keeps_valid_vocabulary_with_degraded_confidence(tmp_path: Path) -> None:
    descriptor = tmp_path / ".weft" / "wardline" / "vocabulary.yaml"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text(
        _DESCRIPTOR.replace("wardline-generic-2", "wardline-generic-3"),
        encoding="utf-8",
    )

    state = load_wardline_descriptor(tmp_path)

    assert state.status == "version_skew"
    assert state.descriptor_version == "wardline-generic-3"
    assert state.expected_version == EXPECTED_DESCRIPTOR_VERSION
    assert state.vocabulary is not None
    assert state.vocabulary.confidence_basis == "descriptor_version_skew"
