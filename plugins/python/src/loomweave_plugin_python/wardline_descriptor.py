"""Wardline NG-25 vocabulary descriptor reader.

This module deliberately reads descriptor files without importing Wardline.
Wardline remains authoritative for the vocabulary; Loomweave records only the
source-observed decorator facts it can derive from that descriptor.

``PROJECT_DESCRIPTOR_PATHS`` remains a Loomweave-side assumption pending
Wardline's "Pre-Rust core hardening" Task B, which has not yet published the
canonical project-local descriptor location. Confirm it against the Wardline
descriptor ADR when it lands (tracked: filigree clarion-6ab5668d82).

This parser READS exactly four top-level keys, and acceptance is keyed on the
``(schema, version)`` PAIR (``ACCEPTED_DESCRIPTORS``):

* ``schema`` — the descriptor format version. Absent means the pre-schema v1
  era (``wardline.vocabulary/v1``) by definition.
* ``version`` — the vocabulary version (``wardline-generic-2`` today).
* ``entries`` — the seeding trust markers.
* ``facets`` — under ``wardline.vocabulary/v2`` only: decorators that are
  attributed but seed no taint (declaration-surface-v2 §7).

Accepting a schema is the OBLIGATION to read every section that schema defines,
not permission to ignore the ones this reader has not learned yet: accepting a
pair while dropping one of its sections would report full ``confidence_basis:
"descriptor"`` confidence over a half-understood descriptor — an
accept-and-ignore fail-open. So when Wardline adds a section to a schema this
reader accepts, the reader is extended in the SAME consumer-first change (spec
P6), and a section that the declared schema does not define fails closed rather
than being tolerated as an unknown key.
"""

from __future__ import annotations

from dataclasses import dataclass, replace
from importlib import metadata
from pathlib import Path
from typing import Any, Literal, cast

import yaml

# PO-confirm against Wardline Task B (descriptor ADR) — canonical project-local
# location and descriptor-version semantics are not yet pinned by Wardline.
# Tracked: filigree clarion-6ab5668d82.
EXPECTED_DESCRIPTOR_VERSION = "wardline-generic-2"
# Pre-schema descriptors (no `schema:` key) are the v1 era by definition.
_DEFAULT_SCHEMA = "wardline.vocabulary/v1"
# Consumer-first dual-accept (wardline declaration-surface-v2 §13.1 item 1): the
# (schema, version) PAIR gates acceptance — generic-3 is accepted only under
# the v2 schema, BEFORE wardline emits it. EXPECTED_DESCRIPTOR_VERSION remains
# the canonical current version for messages/tooling and the manifest pin.
ACCEPTED_DESCRIPTORS: frozenset[tuple[str, str]] = frozenset(
    {
        ("wardline.vocabulary/v1", "wardline-generic-2"),
        ("wardline.vocabulary/v2", "wardline-generic-3"),
    }
)

# Weft store consolidation (ADR-046): sibling runtime state lives under the
# shared ``.weft/<member>/`` dotdir, so the Wardline descriptor is read only from
# the consolidated ``.weft/wardline/`` location. There is no fallback to the
# pre-consolidation ``.wardline/`` path: after the coordinated cutover every
# sibling is at ``.weft/`` by construction, so a descriptor found only on the
# legacy path means a mis-sequenced cutover; resolving it would silently bind a
# stale dir. Instead the project descriptor reads as absent and the loader falls
# through to the package descriptor (a loud, visible signal). Loomweave never
# writes a sibling's subtree — this is read-only.
WEFT_DESCRIPTOR_PATH = Path(".weft/wardline/vocabulary.yaml")
PROJECT_DESCRIPTOR_PATHS = (WEFT_DESCRIPTOR_PATH,)

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
    schema: str
    source: DescriptorSource
    confidence_basis: Literal["descriptor", "descriptor_version_skew"]
    entries_by_name: dict[str, DescriptorEntry]
    facets_by_name: dict[str, DescriptorEntry]

    def entry_for_decorator(self, qualified_name: str) -> DescriptorEntry | None:
        return self.entries_by_name.get(qualified_name.rsplit(".", 1)[-1])

    def facet_for_decorator(self, qualified_name: str) -> DescriptorEntry | None:
        """A facet from the v2 ``facets:`` section.

        Facets are decorators too, but they seed no taint
        (declaration-surface-v2 §7), so they resolve through their own lookup
        and can never be mistaken for a trust claim.
        """
        return self.facets_by_name.get(qualified_name.rsplit(".", 1)[-1])


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
    # Read only the consolidated .weft/wardline/ location (ADR-046); the
    # pre-consolidation .wardline/ path is not consulted.
    for relative in PROJECT_DESCRIPTOR_PATHS:
        path = project_root / relative
        if not path.is_file():
            continue
        try:
            return path.read_text(encoding="utf-8")
        except OSError:
            return None
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
    if (vocabulary.schema, vocabulary.version) not in ACCEPTED_DESCRIPTORS:
        return WardlineDescriptorState(
            status="version_skew",
            descriptor_version=vocabulary.version,
            source=source,
            # `replace` rather than a field-by-field rebuild: a hand-copied
            # rebuild silently drops every newly-parsed field (it would have
            # discarded `facets_by_name` the moment it was added).
            vocabulary=replace(vocabulary, confidence_basis="descriptor_version_skew"),
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

    schema = descriptor.get("schema")
    if schema is None:
        schema = _DEFAULT_SCHEMA
    if not isinstance(schema, str):
        msg = "descriptor schema must be a string when present"
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
        schema=schema,
        source=source,
        confidence_basis="descriptor",
        entries_by_name=entries_by_name,
        facets_by_name=_parse_facets(descriptor, schema, entries_by_name),
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


def _parse_facets(
    descriptor: dict[Any, Any],
    schema: str,
    entries_by_name: dict[str, DescriptorEntry],
) -> dict[str, DescriptorEntry]:
    """Parse the ``wardline.vocabulary/v2`` ``facets:`` section.

    Facets are decorators that seed no taint (declaration-surface-v2 §7), which
    is why Wardline gives them their own section — and why Loomweave gives them
    their own map rather than folding them into ``entries_by_name``. An absent
    section yields an empty map, so every v1 descriptor parses unchanged.

    A ``facets:`` key under the v1 schema is a contract violation, not an
    unknown key: v1's shape is known exactly, and honouring a section the
    declared schema does not have is the fail-open this reader exists to avoid.
    """
    if "facets" not in descriptor:
        return {}
    if schema == _DEFAULT_SCHEMA:
        msg = "descriptor carries a facets section under the v1 schema"
        raise _DescriptorError(msg)
    facets = descriptor["facets"]
    if not isinstance(facets, list):
        msg = "descriptor facets must be a list"
        raise _DescriptorError(msg)

    facets_by_name: dict[str, DescriptorEntry] = {}
    for raw_facet in facets:
        facet = _parse_facet(raw_facet)
        if facet.canonical_name in facets_by_name:
            msg = f"duplicate Wardline descriptor facet: {facet.canonical_name}"
            raise _DescriptorError(msg)
        if facet.canonical_name in entries_by_name:
            msg = f"Wardline descriptor facet collides with an entry: {facet.canonical_name}"
            raise _DescriptorError(msg)
        facets_by_name[facet.canonical_name] = facet
    return facets_by_name


def _parse_facet(raw_facet: Any) -> DescriptorEntry:
    """Parse one ``facets:`` element.

    A facet carries ``canonical_name`` and ``group`` but stamps no
    ``_wardline_*`` level attributes — Wardline registers facets in their own
    vocabulary group precisely so ``apply_marker`` rejects level attributes and
    a facet can never become a trust claim (§7). ``attrs`` is therefore absent
    or an explicit empty mapping; a facet carrying attrs fails closed rather
    than being honoured as a trust marker.
    """
    if not isinstance(raw_facet, dict):
        msg = "descriptor facet must be a mapping"
        raise _DescriptorError(msg)
    canonical_name = raw_facet.get("canonical_name")
    group = raw_facet.get("group")
    if not isinstance(canonical_name, str) or not isinstance(group, int):
        msg = "descriptor facet must carry canonical_name and group"
        raise _DescriptorError(msg)
    attrs = raw_facet.get("attrs", {})
    if not isinstance(attrs, dict) or attrs:
        msg = "descriptor facet must not carry attrs (a facet seeds no taint)"
        raise _DescriptorError(msg)
    return DescriptorEntry(canonical_name=canonical_name, group=group, attrs={})
