"""Wardline NG-25 vocabulary descriptor reader.

This module deliberately reads descriptor files without importing Wardline.
Wardline remains authoritative for the vocabulary; Clarion records only the
source-observed decorator facts it can derive from that descriptor.

Two contract details below (``PROJECT_DESCRIPTOR_PATH`` and the descriptor
``version`` semantics) are Clarion-side assumptions pending Wardline's
"Pre-Rust core hardening" Task B, which has not yet published the canonical
project-local descriptor location or the ``schema: wardline.vocabulary/v1``
format-version field. The parser ignores unknown top-level keys, so a future
``schema`` field is tolerated without change; acting on it (format-version
compatibility decisions) is deferred until Task B pins the contract. Confirm
both assumptions against the Wardline descriptor ADR when it lands
(tracked: filigree clarion-6ab5668d82).
"""

from __future__ import annotations

from dataclasses import dataclass
from importlib import metadata
from pathlib import Path
from typing import Any, Literal, cast

import yaml

# PO-confirm against Wardline Task B (descriptor ADR) — canonical project-local
# location and descriptor-version semantics are not yet pinned by Wardline.
# Tracked: filigree clarion-6ab5668d82.
EXPECTED_DESCRIPTOR_VERSION = "wardline-generic-2"
PROJECT_DESCRIPTOR_PATH = Path(".wardline/vocabulary.yaml")

DescriptorSource = Literal["project", "package"]
DescriptorStatus = Literal["enabled", "version_skew", "absent"]


@dataclass(frozen=True)
class DescriptorEntry:
    canonical_name: str
    group: int
    attrs: dict[str, str]


@dataclass(frozen=True)
class WardlineVocabulary:
    version: str
    source: DescriptorSource
    confidence_basis: Literal["descriptor", "descriptor_version_skew"]
    entries_by_name: dict[str, DescriptorEntry]

    def entry_for_decorator(self, qualified_name: str) -> DescriptorEntry | None:
        return self.entries_by_name.get(qualified_name.rsplit(".", 1)[-1])


@dataclass(frozen=True)
class WardlineDescriptorState:
    status: DescriptorStatus
    expected_version: str = EXPECTED_DESCRIPTOR_VERSION
    descriptor_version: str | None = None
    source: DescriptorSource | None = None
    reason: str | None = None
    vocabulary: WardlineVocabulary | None = None

    def as_capability(self) -> dict[str, str]:
        if self.status == "absent":
            capability = {"status": "absent"}
            if self.reason:
                capability["reason"] = self.reason
            return capability
        capability = {
            "status": self.status,
            "descriptor_version": self.descriptor_version or "",
            "source": self.source or "",
        }
        if self.status == "version_skew":
            capability["expected_version"] = self.expected_version
        return capability


class _DescriptorError(ValueError):
    pass


def load_wardline_descriptor(project_root: Path | None) -> WardlineDescriptorState:
    """Resolve and parse the Wardline descriptor, degrading on every failure."""
    project_text = _read_project_descriptor(project_root)
    if project_text is not None:
        return _state_from_text(project_text, "project")

    package_text = _read_package_descriptor()
    if package_text is not None:
        return _state_from_text(package_text, "package")

    return WardlineDescriptorState(status="absent", reason="not_found")


def _read_project_descriptor(project_root: Path | None) -> str | None:
    if project_root is None:
        return None
    path = project_root / PROJECT_DESCRIPTOR_PATH
    if not path.is_file():
        return None
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return None


def _read_package_descriptor() -> str | None:
    try:
        files = metadata.files("wardline")
    except metadata.PackageNotFoundError:
        return None
    if files is None:
        return None
    for package_file in files:
        if str(package_file).replace("\\", "/").endswith("wardline/core/vocabulary.yaml"):
            try:
                return cast("str", cast("Any", package_file.locate()).read_text(encoding="utf-8"))
            except OSError:
                return None
    return None


def _state_from_text(text: str, source: DescriptorSource) -> WardlineDescriptorState:
    try:
        descriptor = yaml.safe_load(text)
        vocabulary = _parse_descriptor(descriptor, source)
    except (OSError, yaml.YAMLError, _DescriptorError):
        return WardlineDescriptorState(status="absent", reason="invalid_descriptor")
    if vocabulary.version != EXPECTED_DESCRIPTOR_VERSION:
        return WardlineDescriptorState(
            status="version_skew",
            descriptor_version=vocabulary.version,
            source=source,
            vocabulary=WardlineVocabulary(
                version=vocabulary.version,
                source=source,
                confidence_basis="descriptor_version_skew",
                entries_by_name=vocabulary.entries_by_name,
            ),
        )
    return WardlineDescriptorState(
        status="enabled",
        descriptor_version=vocabulary.version,
        source=source,
        vocabulary=vocabulary,
    )


def _parse_descriptor(descriptor: Any, source: DescriptorSource) -> WardlineVocabulary:
    if not isinstance(descriptor, dict):
        msg = "descriptor root must be a mapping"
        raise _DescriptorError(msg)
    version = descriptor.get("version")
    entries = descriptor.get("entries")
    if not isinstance(version, str) or not isinstance(entries, list):
        msg = "descriptor must carry string version and list entries"
        raise _DescriptorError(msg)

    entries_by_name: dict[str, DescriptorEntry] = {}
    for raw_entry in entries:
        entry = _parse_entry(raw_entry)
        if entry.canonical_name in entries_by_name:
            msg = f"duplicate Wardline descriptor entry: {entry.canonical_name}"
            raise _DescriptorError(msg)
        entries_by_name[entry.canonical_name] = entry
    return WardlineVocabulary(
        version=version,
        source=source,
        confidence_basis="descriptor",
        entries_by_name=entries_by_name,
    )


def _parse_entry(raw_entry: Any) -> DescriptorEntry:
    if not isinstance(raw_entry, dict):
        msg = "descriptor entry must be a mapping"
        raise _DescriptorError(msg)
    canonical_name = raw_entry.get("canonical_name")
    group = raw_entry.get("group")
    attrs = raw_entry.get("attrs")
    if not isinstance(canonical_name, str) or not isinstance(group, int):
        msg = "descriptor entry must carry canonical_name and group"
        raise _DescriptorError(msg)
    if not isinstance(attrs, dict):
        msg = "descriptor entry attrs must be a mapping"
        raise _DescriptorError(msg)
    for key, value in attrs.items():
        if not isinstance(key, str) or not isinstance(value, str):
            msg = "descriptor attrs must map strings to strings"
            raise _DescriptorError(msg)
    return DescriptorEntry(
        canonical_name=canonical_name,
        group=group,
        attrs=cast("dict[str, str]", dict(attrs)),
    )
