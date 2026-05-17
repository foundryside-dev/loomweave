"""Engine-related type contracts."""

import math
from dataclasses import dataclass
from typing import ClassVar, TypedDict

from elspeth.contracts.enums import _LEGAL_TERMINAL_PAIRS, TerminalOutcome, TerminalPath
from elspeth.contracts.freeze import require_int


@dataclass(frozen=True, slots=True)
class BufferEntry[T]:
    """Entry emitted from the reorder buffer with timing metadata.

    This is the contracts-layer type for reorder buffer results, used by
    both the pooling subsystem (plugins/) and audit context types
    (contracts/node_state_context.py).

    Attributes:
        submit_index: Order in which item was submitted (0-indexed)
        complete_index: Order in which item completed (may differ from submit)
        result: The actual result value
        submit_timestamp: time.perf_counter() when submitted
        complete_timestamp: time.perf_counter() when completed
        buffer_wait_ms: Time spent waiting in buffer after completion
    """

    submit_index: int
    complete_index: int
    result: T
    submit_timestamp: float
    complete_timestamp: float
    buffer_wait_ms: float

    def __post_init__(self) -> None:
        require_int(self.submit_index, "BufferEntry.submit_index", min_value=0)
        require_int(self.complete_index, "BufferEntry.complete_index", min_value=0)
        if not math.isfinite(self.submit_timestamp) or self.submit_timestamp < 0:
            raise ValueError(f"BufferEntry.submit_timestamp must be non-negative and finite, got {self.submit_timestamp}")
        if not math.isfinite(self.complete_timestamp) or self.complete_timestamp < 0:
            raise ValueError(f"BufferEntry.complete_timestamp must be non-negative and finite, got {self.complete_timestamp}")
        if not math.isfinite(self.buffer_wait_ms) or self.buffer_wait_ms < 0:
            raise ValueError(f"BufferEntry.buffer_wait_ms must be non-negative and finite, got {self.buffer_wait_ms}")


@dataclass(frozen=True, slots=True, kw_only=True)
class PendingOutcome:
    """Pending token outcome waiting for sink durability confirmation (ADR-019).

    Carries (outcome, path) pairs through the pending_tokens queue so token
    outcomes are recorded only after sink write + flush complete successfully.
    """

    _REQUIRES_ERROR_HASH_PATHS: ClassVar[frozenset[TerminalPath]] = frozenset(
        {
            TerminalPath.ON_ERROR_ROUTED,
            TerminalPath.UNROUTED,
            TerminalPath.QUARANTINED_AT_SOURCE,
            TerminalPath.SINK_FALLBACK_TO_FAILSINK,
            TerminalPath.SINK_DISCARDED,
        }
    )

    outcome: TerminalOutcome | None
    path: TerminalPath
    error_hash: str | None = None

    def __post_init__(self) -> None:
        """Validate pair/error_hash consistency before sink side effects."""
        if self.outcome is None:
            if self.path != TerminalPath.BUFFERED:
                raise ValueError(f"PendingOutcome with outcome=None requires path=BUFFERED, got {self.path.name}")
        elif (self.outcome, self.path) not in _LEGAL_TERMINAL_PAIRS:
            raise ValueError(f"PendingOutcome has illegal (outcome, path) pair: ({self.outcome.name}, {self.path.name})")

        if self.path in self._REQUIRES_ERROR_HASH_PATHS and (self.error_hash is None or not self.error_hash.strip()):
            raise ValueError(f"PendingOutcome with path={self.path.name} requires non-empty error_hash")
        if self.path not in self._REQUIRES_ERROR_HASH_PATHS and self.error_hash is not None:
            raise ValueError(f"PendingOutcome with path={self.path.name} must not have error_hash")


class RetryPolicy(TypedDict, total=False):
    """Schema for retry configuration from plugin policies.

    All fields are optional - from_policy() applies defaults.

    Attributes:
        max_attempts: Maximum number of attempts (minimum 1)
        base_delay: Initial delay between retries in seconds
        max_delay: Maximum delay between retries in seconds
        jitter: Random jitter to add to delays in seconds
        exponential_base: Exponential backoff multiplier (default 2.0)
    """

    max_attempts: int
    base_delay: float
    max_delay: float
    jitter: float
    exponential_base: float
