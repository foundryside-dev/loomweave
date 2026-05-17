"""Sink diversion contracts — per-row write failure routing types.

These types support the failsink pattern: when a sink can't write a specific
row (value-level failure at the Tier 2 -> External boundary), it diverts the
row to a failsink. These contracts carry diversion information from the plugin's
write() method back to SinkExecutor for audit trail recording.

Layer: L0 (contracts). Imports only from L0 and stdlib.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from elspeth.contracts.errors import PluginContractViolation
from elspeth.contracts.freeze import freeze_fields, require_int
from elspeth.contracts.results import ArtifactDescriptor


@dataclass(frozen=True, slots=True)
class RowDiversion:
    """Record of a single row diverted to failsink during write().

    Created by BaseSink._divert_row() and accumulated in _diversion_log.
    Read by SinkExecutor after write() returns to record per-token outcomes.

    Attributes:
        row_index: Index in the original batch passed to write().
        reason: Why the external system rejected this row.
        row_data: The row that was diverted (for failsink write).
    """

    row_index: int
    reason: str
    row_data: Mapping[str, Any]

    def __post_init__(self) -> None:
        require_int(self.row_index, "row_index", min_value=0)
        freeze_fields(self, "row_data")


@dataclass(frozen=True, slots=True)
class SinkWriteResult:
    """Result of a sink write() call with optional diversion information.

    Replaces ArtifactDescriptor as the return type of BaseSink.write().
    Sinks with no diversions return SinkWriteResult(artifact=..., diversions=()).

    Attributes:
        artifact: ArtifactDescriptor for the primary write (may represent zero rows
            if all were diverted).
        diversions: Tuple of RowDiversion records for rows diverted during write().
            Empty tuple if no diversions occurred.
    """

    artifact: ArtifactDescriptor
    diversions: tuple[RowDiversion, ...] = ()

    def __post_init__(self) -> None:
        if not isinstance(self.artifact, ArtifactDescriptor):
            raise PluginContractViolation(
                f"SinkWriteResult.artifact must be ArtifactDescriptor, got {type(self.artifact).__name__}: {self.artifact!r}"
            )
        if not isinstance(self.diversions, tuple):
            raise PluginContractViolation(
                f"SinkWriteResult.diversions must be a tuple, got {type(self.diversions).__name__}. Use tuple(...) to convert from list."
            )
        if not self.diversions:
            return
        for i, d in enumerate(self.diversions):
            if not isinstance(d, RowDiversion):
                raise PluginContractViolation(f"SinkWriteResult.diversions[{i}] must be RowDiversion, got {type(d).__name__}: {d!r}")
        # Duplicate row_index values would collapse in set operations downstream
        # (SinkExecutor.write builds diversion_by_index as a dict), silently
        # dropping a diversion and recording the wrong terminal outcome. Crash
        # immediately — this is a plugin bug in our code, not user data.
        seen: set[int] = set()
        for d in self.diversions:
            if d.row_index in seen:
                raise PluginContractViolation(
                    f"SinkWriteResult has duplicate diversion row_index={d.row_index}. "
                    f"Each row can be diverted at most once — this is a sink plugin bug."
                )
            seen.add(d.row_index)
