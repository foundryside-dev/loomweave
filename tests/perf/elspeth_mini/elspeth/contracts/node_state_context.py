"""Typed node state context for the Landscape audit trail.

Replaces ``dict[str, Any]`` at the Tier 1 boundary where context
metadata is serialized into ``context_after_json``.  Follows the
same pattern as ``CoalesceMetadata`` (commit 4f7e43be) and
``TokenUsage`` (commit dffe74a6).

Trust-tier notes
----------------
* ``NodeStateContext`` — Protocol for structural typing (mypy only).
* ``PoolExecutionContext`` — typed pool stats from LLM multi-query.
* ``from_executor_stats()`` — Tier 1 factory: crash on bad data.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol

from elspeth.contracts.freeze import require_int

if TYPE_CHECKING:
    from elspeth.contracts.engine import BufferEntry
    from elspeth.contracts.results import TransformResult


class NodeStateContext(Protocol):
    """Structural protocol for node state context metadata.

    Any object with a ``to_dict()`` method can serve as context
    metadata for the audit trail.  NOT ``@runtime_checkable`` —
    conformance is verified by mypy at type-check time only.
    """

    def to_dict(self) -> dict[str, Any]: ...


@dataclass(frozen=True, slots=True)
class PoolConfigSnapshot:
    """Pool configuration at completion time."""

    pool_size: int
    max_capacity_retry_seconds: float
    dispatch_delay_at_completion_ms: float

    def __post_init__(self) -> None:
        require_int(self.pool_size, "pool_size", min_value=0)

    def to_dict(self) -> dict[str, Any]:
        return {
            "pool_size": self.pool_size,
            "max_capacity_retry_seconds": self.max_capacity_retry_seconds,
            "dispatch_delay_at_completion_ms": self.dispatch_delay_at_completion_ms,
        }


@dataclass(frozen=True, slots=True)
class PoolStatsSnapshot:
    """Pool runtime statistics at completion time."""

    capacity_retries: int
    successes: int
    peak_delay_ms: float
    current_delay_ms: float
    total_throttle_time_ms: float
    max_concurrent_reached: int

    def __post_init__(self) -> None:
        require_int(self.capacity_retries, "capacity_retries", min_value=0)
        require_int(self.successes, "successes", min_value=0)
        require_int(self.max_concurrent_reached, "max_concurrent_reached", min_value=0)

    def to_dict(self) -> dict[str, Any]:
        return {
            "capacity_retries": self.capacity_retries,
            "successes": self.successes,
            "peak_delay_ms": self.peak_delay_ms,
            "current_delay_ms": self.current_delay_ms,
            "total_throttle_time_ms": self.total_throttle_time_ms,
            "max_concurrent_reached": self.max_concurrent_reached,
        }


@dataclass(frozen=True, slots=True)
class QueryOrderEntry:
    """Ordering metadata for a single query in a pooled batch."""

    submit_index: int
    complete_index: int
    buffer_wait_ms: float

    def __post_init__(self) -> None:
        require_int(self.submit_index, "submit_index", min_value=0)
        require_int(self.complete_index, "complete_index", min_value=0)

    def to_dict(self) -> dict[str, Any]:
        return {
            "submit_index": self.submit_index,
            "complete_index": self.complete_index,
            "buffer_wait_ms": self.buffer_wait_ms,
        }


@dataclass(frozen=True, slots=True)
class PoolExecutionContext:
    """Typed pool execution metadata for the LLM multi-query audit trail.

    Replaces the untyped ``dict[str, Any]`` constructed in
    ``base_multi_query.py``.
    """

    pool_config: PoolConfigSnapshot
    pool_stats: PoolStatsSnapshot
    query_ordering: tuple[QueryOrderEntry, ...]

    def to_dict(self) -> dict[str, Any]:
        return {
            "pool_config": self.pool_config.to_dict(),
            "pool_stats": self.pool_stats.to_dict(),
            "query_ordering": [entry.to_dict() for entry in self.query_ordering],
        }

    @classmethod
    def from_executor_stats(
        cls,
        stats: dict[str, Any],
        entries: list[BufferEntry[TransformResult]],
    ) -> PoolExecutionContext:
        """Build from PooledExecutor.get_stats() and reorder buffer entries.

        This is a Tier 1 factory — bad data means a bug in our code,
        so we access keys directly (crash on missing/wrong type).
        """
        pool_config_raw = stats["pool_config"]
        pool_stats_raw = stats["pool_stats"]

        config = PoolConfigSnapshot(
            pool_size=pool_config_raw["pool_size"],
            max_capacity_retry_seconds=pool_config_raw["max_capacity_retry_seconds"],
            dispatch_delay_at_completion_ms=pool_config_raw["dispatch_delay_at_completion_ms"],
        )
        pool_stats = PoolStatsSnapshot(
            capacity_retries=pool_stats_raw["capacity_retries"],
            successes=pool_stats_raw["successes"],
            peak_delay_ms=pool_stats_raw["peak_delay_ms"],
            current_delay_ms=pool_stats_raw["current_delay_ms"],
            total_throttle_time_ms=pool_stats_raw["total_throttle_time_ms"],
            max_concurrent_reached=pool_stats_raw["max_concurrent_reached"],
        )
        ordering = tuple(
            QueryOrderEntry(
                submit_index=entry.submit_index,
                complete_index=entry.complete_index,
                buffer_wait_ms=entry.buffer_wait_ms,
            )
            for entry in entries
        )
        return cls(
            pool_config=config,
            pool_stats=pool_stats,
            query_ordering=ordering,
        )


@dataclass(frozen=True, slots=True)
class GateEvaluationContext:
    """Typed gate evaluation metadata for the audit trail.

    Replaces the untyped ``dict[str, Any]`` constructed in gate
    executor code.  Follows the same pattern as ``PoolExecutionContext``
    (this module) and ``CoalesceMetadata`` (commit 4f7e43be).

    Fields
    ------
    condition : str
        The gate's expression string (e.g. ``"amount > 1000"``).
    result : str
        The raw stringified evaluation result (e.g. ``"True"``).
    route_label : str
        The normalized routing key derived from the result
        (e.g. ``"true"``).  For boolean expressions the result and
        route_label differ only in casing; for multi-valued expressions
        the route_label is the resolved destination key.
    """

    condition: str
    result: str
    route_label: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "condition": self.condition,
            "result": self.result,
            "route_label": self.route_label,
        }


@dataclass(frozen=True, slots=True)
class AggregationFlushContext:
    """Typed aggregation flush metadata for the audit trail.

    Replaces the untyped ``dict[str, Any]`` constructed in aggregation
    executor code.  Follows the same pattern as ``PoolExecutionContext``
    (this module) and ``CoalesceMetadata`` (commit 4f7e43be).
    """

    trigger_type: str
    buffer_size: int
    batch_id: str

    def __post_init__(self) -> None:
        require_int(self.buffer_size, "buffer_size", min_value=0)

    def to_dict(self) -> dict[str, Any]:
        return {
            "trigger_type": self.trigger_type,
            "buffer_size": self.buffer_size,
            "batch_id": self.batch_id,
        }
