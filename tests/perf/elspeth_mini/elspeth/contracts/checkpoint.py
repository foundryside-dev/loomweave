"""Checkpoint and recovery domain contracts.

These types are used for checkpoint validation and resume operations.
They are NOT persisted to the audit trail (those are in audit.py).
"""

from __future__ import annotations

from dataclasses import dataclass

from elspeth.contracts.aggregation_checkpoint import AggregationCheckpointState
from elspeth.contracts.audit import Checkpoint
from elspeth.contracts.coalesce_checkpoint import CoalesceCheckpointState
from elspeth.contracts.freeze import require_int


@dataclass(frozen=True, slots=True)
class ResumeCheck:
    """Result of checking if a run can be resumed.

    Used by RecoveryManager and CheckpointCompatibilityValidator to
    communicate whether resume is possible and why/why not.
    """

    can_resume: bool
    reason: str | None = None

    def __post_init__(self) -> None:
        if self.can_resume and self.reason is not None:
            raise ValueError("can_resume=True should not have a reason")
        if not self.can_resume and self.reason is None:
            raise ValueError("can_resume=False must have a reason explaining why")


@dataclass(frozen=True, slots=True)
class ResumePoint:
    """Information needed to resume a run.

    Contains all the data needed by Orchestrator.resume() to continue
    processing from where a failed run left off.
    """

    checkpoint: Checkpoint
    token_id: str
    node_id: str
    sequence_number: int
    aggregation_state: AggregationCheckpointState | None = None
    coalesce_state: CoalesceCheckpointState | None = None

    def __post_init__(self) -> None:
        """Validate resume point fields — Tier 1 crash on invalid data.

        Per CLAUDE.md Data Manifesto: Checkpoints are Tier 1 audit data.
        Wrong types, None, or empty token_id/node_id indicate corrupted
        checkpoint data — crash immediately with distinct error messages.
        """
        if not isinstance(self.checkpoint, Checkpoint):
            raise TypeError(f"ResumePoint.checkpoint must be Checkpoint, got {type(self.checkpoint).__name__}")
        if not isinstance(self.token_id, str):
            raise TypeError(f"ResumePoint.token_id must be str, got {type(self.token_id).__name__}: {self.token_id!r}")
        if not self.token_id:
            raise ValueError("ResumePoint.token_id must not be empty")
        if not isinstance(self.node_id, str):
            raise TypeError(f"ResumePoint.node_id must be str, got {type(self.node_id).__name__}: {self.node_id!r}")
        if not self.node_id:
            raise ValueError("ResumePoint.node_id must not be empty")
        require_int(self.sequence_number, "ResumePoint.sequence_number", min_value=0)
        if self.aggregation_state is not None and not isinstance(self.aggregation_state, AggregationCheckpointState):
            raise TypeError(
                f"ResumePoint.aggregation_state must be AggregationCheckpointState or None, got {type(self.aggregation_state).__name__}"
            )
        if self.coalesce_state is not None and not isinstance(self.coalesce_state, CoalesceCheckpointState):
            raise TypeError(f"ResumePoint.coalesce_state must be CoalesceCheckpointState or None, got {type(self.coalesce_state).__name__}")
        # Invariant: duplicated fields must match the embedded Checkpoint.
        # These fields exist for convenience access but are derived data,
        # not independent inputs. Mismatch = corrupted construction.
        if self.token_id != self.checkpoint.token_id:
            raise ValueError(f"ResumePoint.token_id ({self.token_id!r}) does not match checkpoint.token_id ({self.checkpoint.token_id!r})")
        if self.node_id != self.checkpoint.node_id:
            raise ValueError(f"ResumePoint.node_id ({self.node_id!r}) does not match checkpoint.node_id ({self.checkpoint.node_id!r})")
        if self.sequence_number != self.checkpoint.sequence_number:
            raise ValueError(
                f"ResumePoint.sequence_number ({self.sequence_number}) does not match "
                f"checkpoint.sequence_number ({self.checkpoint.sequence_number})"
            )
