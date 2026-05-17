"""Shared primitives for reorder buffer implementations.

Reorder buffers accept results out-of-order and emit them in submission order.
This module provides common building blocks used by both:
- RowReorderBuffer (streaming, event-driven, with backpressure)
- ReorderBuffer (batch-oriented, polling-based)

The implementations remain separate because they serve different architectural
needs, but share these foundational primitives.
"""

from __future__ import annotations


class UnfilledSentinel:
    """Sentinel distinguishing 'not yet completed' from a legitimate None result.

    Reorder buffers track pending entries that may complete with any value,
    including None. A dedicated sentinel class ensures we can always distinguish
    "slot not filled yet" from "slot filled with None."

    Usage:
        from elspeth.contracts.reorder_primitives import UNFILLED, UnfilledSentinel

        # Check if slot is unfilled
        if entry.result is UNFILLED:
            ...

        # Type annotation for result field that may be unfilled
        result: T | UnfilledSentinel = UNFILLED
    """

    __slots__ = ()

    def __repr__(self) -> str:
        return "<UNFILLED>"


# Global sentinel instance - use identity comparison (is UNFILLED)
UNFILLED: UnfilledSentinel = UnfilledSentinel()
