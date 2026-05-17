"""Collection readiness probes — protocols and result types.

Used by commencement gates (pre-flight checks) and retrieval provider
readiness contracts (transform pre-conditions).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol, runtime_checkable

from elspeth.contracts.freeze import require_int


@dataclass(frozen=True, slots=True)
class CollectionReadinessResult:
    """Result of a collection readiness check.

    All fields are scalars — no freeze guard needed. Validates non-empty collection
    and non-negative count.

    count is None when the count is unknown (unreachable, malformed response,
    collection absent). Per the data manifesto: absence is evidence, not an
    invitation to invent a default. Fabricating count=0 for "unknown" conflates
    "empty" with "we don't know."
    """

    collection: str
    reachable: bool
    count: int | None
    message: str

    def __post_init__(self) -> None:
        if not self.collection:
            raise ValueError("collection must not be empty")
        require_int(self.count, "count", optional=True, min_value=0)
        if not self.reachable and self.count is not None:
            raise ValueError(
                f"Contradictory state: reachable=False but count={self.count}. Unreachable collections cannot have a known count."
            )


@runtime_checkable
class CollectionProbe(Protocol):
    """Probes a vector store collection for readiness.

    Implementations live in L3 (plugins/infrastructure).
    The protocol is L0 so L2 (engine) can depend on it.
    """

    collection_name: str

    def probe(self) -> CollectionReadinessResult: ...
