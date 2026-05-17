from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Literal, NotRequired, Protocol, TypedDict

if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path


class CallsEdgeProperties(TypedDict):
    candidates: list[str]


class CallsRawEdge(TypedDict):
    kind: Literal["calls"]
    from_id: str
    to_id: str
    source_byte_start: int
    source_byte_end: int
    confidence: Literal["resolved", "ambiguous"]
    properties: NotRequired[CallsEdgeProperties]


class Finding(TypedDict):
    subcode: str
    severity: Literal["info", "warning", "error"]
    message: str
    metadata: dict[str, object]


class UnresolvedCallSite(TypedDict):
    caller_entity_id: str
    site_ordinal: int
    source_byte_start: int
    source_byte_end: int
    callee_expr: str


@dataclass
class CallResolutionResult:
    edges: list[CallsRawEdge] = field(default_factory=list)
    unresolved_call_sites_total: int = 0
    unresolved_call_sites: list[UnresolvedCallSite] = field(default_factory=list)
    pyright_query_latency_ms: list[int] = field(default_factory=list)
    findings: list[Finding] = field(default_factory=list)


class CallResolver(Protocol):
    def resolve_calls(
        self,
        file_path: str | Path,
        function_ids: Sequence[str],
    ) -> CallResolutionResult: ...


class NoOpCallResolver:
    def resolve_calls(
        self,
        file_path: str | Path,
        function_ids: Sequence[str],
    ) -> CallResolutionResult:
        _ = (file_path, function_ids)
        return CallResolutionResult()
