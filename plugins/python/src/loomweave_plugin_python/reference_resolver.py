from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Literal, NotRequired, Protocol, TypedDict

if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path

    from loomweave_plugin_python.call_resolver import Finding


# ``name``/``annotation`` resolve into ``references`` edges; ``base`` and
# ``decorator`` are relation sites resolving into ``inherits_from`` /
# ``decorates`` edges respectively (clarion-43416be550). All four ride the
# same pyright resolution machinery (external-skip, per-file cap, lookup
# cache); the site kind selects the emitted edge kind.
ReferenceSiteKind = Literal["name", "annotation", "base", "decorator"]


@dataclass(frozen=True)
class ReferenceSite:
    from_id: str
    line: int
    character: int
    end_line: int
    end_character: int
    source_byte_start: int
    source_byte_end: int
    kind: ReferenceSiteKind


class ReferencesEdgeProperties(TypedDict):
    candidates: list[str]


class ReferencesRawEdge(TypedDict):
    """Anchored edge produced by the reference-site resolution pass.

    Despite the name (historic — B.5* shipped ``references`` alone), this is
    the wire shape for all three site-derived edge kinds. ``candidates`` in
    ``properties`` is present only on ambiguous edges; for ``decorates`` the
    candidates are alternative *from*-side decorator entities (direction is
    inverted relative to the site owner).
    """

    kind: Literal["references", "inherits_from", "decorates"]
    from_id: str
    to_id: str
    source_byte_start: int
    source_byte_end: int
    confidence: Literal["resolved", "ambiguous"]
    properties: NotRequired[ReferencesEdgeProperties]


@dataclass
class ReferenceResolutionResult:
    edges: list[ReferencesRawEdge] = field(default_factory=list)
    reference_sites_total: int = 0
    references_resolved_total: int = 0
    references_skipped_external_total: int = 0
    references_skipped_cap_total: int = 0
    unresolved_reference_sites_total: int = 0
    pyright_query_latency_ms: list[int] = field(default_factory=list)
    pyright_index_parse_latency_ms: list[int] = field(default_factory=list)
    findings: list[Finding] = field(default_factory=list)


class ReferenceResolver(Protocol):
    def resolve_references(
        self,
        file_path: str | Path,
        sites: Sequence[ReferenceSite],
    ) -> ReferenceResolutionResult: ...


class NoOpReferenceResolver:
    def resolve_references(
        self,
        file_path: str | Path,
        sites: Sequence[ReferenceSite],
    ) -> ReferenceResolutionResult:
        _ = (file_path, sites)
        return ReferenceResolutionResult()
