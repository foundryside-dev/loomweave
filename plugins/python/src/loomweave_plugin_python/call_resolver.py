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


class FacetCoverageWire(TypedDict):
    """Wire shape of one resolution facet's coverage claim (host contract)."""

    status: Literal["complete", "degraded"]
    reason: NotRequired[str]
    transient: bool
    collateral: bool


class ResolutionCoverageWire(TypedDict):
    calls: FacetCoverageWire
    references: FacetCoverageWire


@dataclass(frozen=True)
class FacetCoverage:
    """How much of one resolution facet (calls / references) a file received.

    The resolver degrades to EMPTY evidence -- not an error -- when pyright is
    unavailable, times out, or has been poisoned for the rest of the run. The
    host cannot tell that apart from a genuinely call-free file, so it treated
    the empty result as a completed analysis and its incremental skip pinned
    the hole for as long as the file's bytes stayed unchanged
    (clarion-3e517d4aff). This claim rides every ``analyze_file`` result so the
    host can re-dispatch a ``degraded`` + ``transient`` file next run and name
    the hole on its read surface.

    ``transient`` is True when re-running the unchanged file could plausibly
    recover coverage (resolver timeout, crash, poison, unavailable binary) and
    False for content-determined limits a re-run would hit again (syntax
    error, per-file site cap, nesting too complex).

    ``collateral`` (clarion-7fc41105ea, ADR-057) is decided by which catch-site
    built the claim, never by message text. Self-inflicted (``False``) means
    ONLY: ``pyright_timeout`` on this file's own budget, or
    ``pyright_transport_failure`` -- pyright died while THIS file's request was
    in flight (``pyright_local_read_error``, a read error on an unrelated
    target while pyright stayed alive, is also this file's gap). Everything
    else is collateral (``True``): ``pyright_restarting`` (found dead on
    arrival), ``pyright_spawn_failed`` (deferred spawn), and the run-disabled
    tokens ``pyright_unavailable`` / ``pyright_poisoned`` /
    ``pyright_restart_cap_exceeded``. The full vocabulary is documented at the
    top of ``pyright_session.py``.
    """

    status: Literal["complete", "degraded"] = "complete"
    reason: str | None = None
    transient: bool = False
    # The hole predates this file (pyright already dead, spawn deferred, run
    # disabled by an EARLIER file's failure): collateral, not this file's
    # doing. The host dispatches collateral files first so the troublemaker
    # goes last, and keeps a prior self-inflicted mark sticky across a
    # collateral run.
    collateral: bool = False

    @classmethod
    def degraded(cls, reason: str, *, transient: bool, collateral: bool = False) -> FacetCoverage:
        return cls(status="degraded", reason=reason, transient=transient, collateral=collateral)

    @property
    def is_degraded(self) -> bool:
        return self.status == "degraded"

    def to_wire(self) -> FacetCoverageWire:
        wire: FacetCoverageWire = {
            "status": self.status,
            "transient": self.transient,
            "collateral": self.collateral,
        }
        if self.reason is not None:
            wire["reason"] = self.reason
        return wire


@dataclass
class CallResolutionResult:
    edges: list[CallsRawEdge] = field(default_factory=list)
    unresolved_call_sites_total: int = 0
    # Bare unshadowed-builtin calls dropped before persistence; disclosed so
    # the omission is visible in run stats (clarion-8a862d8f7e).
    unresolved_call_sites_skipped_builtin_total: int = 0
    unresolved_call_sites: list[UnresolvedCallSite] = field(default_factory=list)
    pyright_query_latency_ms: list[int] = field(default_factory=list)
    pyright_index_parse_latency_ms: list[int] = field(default_factory=list)
    findings: list[Finding] = field(default_factory=list)
    coverage: FacetCoverage = field(default_factory=FacetCoverage)


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
