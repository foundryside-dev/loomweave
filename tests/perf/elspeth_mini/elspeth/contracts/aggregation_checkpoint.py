"""Typed checkpoint state for aggregation buffers.

Replaces ``dict[str, Any]`` with frozen dataclasses that encode the
three-level checkpoint structure at the type level:

  AggregationCheckpointState          (Level 1 — full state)
    └── AggregationNodeCheckpoint     (Level 2 — per-node)
         └── AggregationTokenCheckpoint (Level 3 — per-token)

Trust-tier notes
----------------
* ``to_dict()`` — serialization boundary for ``checkpoint_dumps()``.
  Preserves the existing wire format exactly (flat ``_version`` + node_id keys).
* ``from_dict()`` — Tier 1 reconstruction.  Checkpoints are our data (per
  CLAUDE.md Data Manifesto): crash on any structural corruption, no coercion.
"""

from __future__ import annotations

import math
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from elspeth.contracts.errors import AuditIntegrityError
from elspeth.contracts.freeze import deep_thaw, freeze_fields


@dataclass(frozen=True, slots=True)
class AggregationTokenCheckpoint:
    """Checkpoint state for a single buffered token (Level 3).

    Attributes:
        token_id: Unique token identity for lineage tracking.
        row_id: Source row identity.
        branch_name: Fork branch name, or ``None`` for linear paths.
        fork_group_id: Fork group identity, or ``None``.
        join_group_id: Join group identity, or ``None``.
        expand_group_id: Deaggregation expansion group, or ``None``.
        row_data: Row payload as plain dict (opaque — PipelineRow owns format).
        contract_version: Version hash of the SchemaContract at checkpoint time.
        contract: SchemaContract checkpoint dict (opaque — SchemaContract owns format).
    """

    token_id: str
    row_id: str
    branch_name: str | None
    fork_group_id: str | None
    join_group_id: str | None
    expand_group_id: str | None
    row_data: Mapping[str, Any]
    contract_version: str
    contract: Mapping[str, Any]

    def __post_init__(self) -> None:
        if not self.token_id:
            raise ValueError("AggregationTokenCheckpoint.token_id must not be empty")
        if not self.row_id:
            raise ValueError("AggregationTokenCheckpoint.row_id must not be empty")
        if not self.contract_version:
            raise ValueError("AggregationTokenCheckpoint.contract_version must not be empty")
        if not isinstance(self.row_data, Mapping):
            raise TypeError(f"AggregationTokenCheckpoint.row_data must be a Mapping, got {type(self.row_data).__name__}")
        if not isinstance(self.contract, Mapping):
            raise TypeError(f"AggregationTokenCheckpoint.contract must be a Mapping, got {type(self.contract).__name__}")
        freeze_fields(self, "row_data", "contract")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to checkpoint dict format."""
        return {
            "token_id": self.token_id,
            "row_id": self.row_id,
            "branch_name": self.branch_name,
            "fork_group_id": self.fork_group_id,
            "join_group_id": self.join_group_id,
            "expand_group_id": self.expand_group_id,
            "row_data": deep_thaw(self.row_data),
            "contract_version": self.contract_version,
            "contract": deep_thaw(self.contract),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> AggregationTokenCheckpoint:
        """Reconstruct from checkpoint dict (Tier 1 — crash on corruption).

        Args:
            data: Token dict from checkpoint.

        Raises:
            AuditIntegrityError: If required keys are missing.
        """
        required_fields = {
            "token_id",
            "row_id",
            "row_data",
            "branch_name",
            "fork_group_id",
            "join_group_id",
            "expand_group_id",
            "contract_version",
            "contract",
        }
        missing = required_fields - set(data.keys())
        if missing:
            raise AuditIntegrityError(
                f"Corrupted aggregation token checkpoint: missing required fields {missing}. Found: {set(data.keys())}"
            )

        # Tier 1 type guards — crash on type corruption, not on downstream access.
        # Scalars must be str (not int, bool, etc. from malformed JSON).
        for field_name in ("token_id", "row_id", "contract_version"):
            value = data[field_name]
            if not isinstance(value, str):
                raise AuditIntegrityError(
                    f"Corrupted aggregation token checkpoint: '{field_name}' must be str, got {type(value).__name__}: {value!r}"
                )
        # Optional str fields — must be str or None
        for field_name in ("branch_name", "fork_group_id", "join_group_id", "expand_group_id"):
            value = data[field_name]
            if value is not None and not isinstance(value, str):
                raise AuditIntegrityError(
                    f"Corrupted aggregation token checkpoint: '{field_name}' must be str or None, got {type(value).__name__}: {value!r}"
                )
        # Container fields — must be dict (JSON object)
        for field_name in ("row_data", "contract"):
            value = data[field_name]
            if not isinstance(value, dict):
                raise AuditIntegrityError(
                    f"Corrupted aggregation token checkpoint: '{field_name}' must be a dict, got {type(value).__name__}"
                )

        return cls(
            token_id=data["token_id"],
            row_id=data["row_id"],
            branch_name=data["branch_name"],
            fork_group_id=data["fork_group_id"],
            join_group_id=data["join_group_id"],
            expand_group_id=data["expand_group_id"],
            row_data=data["row_data"],
            contract_version=data["contract_version"],
            contract=data["contract"],
        )


@dataclass(frozen=True, slots=True)
class AggregationNodeCheckpoint:
    """Checkpoint state for one aggregation node (Level 2).

    Attributes:
        tokens: Buffered tokens awaiting aggregation flush.
        batch_id: Active batch identity (non-optional — must exist if tokens buffered).
        elapsed_age_seconds: Seconds since first accept for timeout preservation.
        count_fire_offset: Trigger fire-time offset for count trigger, or ``None``.
        condition_fire_offset: Trigger fire-time offset for condition trigger, or ``None``.
    """

    tokens: tuple[AggregationTokenCheckpoint, ...]
    batch_id: str
    elapsed_age_seconds: float
    count_fire_offset: float | None
    condition_fire_offset: float | None

    def __post_init__(self) -> None:
        if not self.batch_id:
            raise ValueError("AggregationNodeCheckpoint.batch_id must not be empty")
        if self.elapsed_age_seconds < 0 or not math.isfinite(self.elapsed_age_seconds):
            raise ValueError(
                f"AggregationNodeCheckpoint.elapsed_age_seconds must be non-negative and finite, got {self.elapsed_age_seconds}"
            )
        if self.count_fire_offset is not None and (self.count_fire_offset < 0 or not math.isfinite(self.count_fire_offset)):
            raise ValueError(f"AggregationNodeCheckpoint.count_fire_offset must be non-negative and finite, got {self.count_fire_offset}")
        if self.condition_fire_offset is not None and (self.condition_fire_offset < 0 or not math.isfinite(self.condition_fire_offset)):
            raise ValueError(
                f"AggregationNodeCheckpoint.condition_fire_offset must be non-negative and finite, got {self.condition_fire_offset}"
            )
        freeze_fields(self, "tokens")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to checkpoint dict format."""
        return {
            "tokens": [t.to_dict() for t in self.tokens],
            "batch_id": self.batch_id,
            "elapsed_age_seconds": self.elapsed_age_seconds,
            "count_fire_offset": self.count_fire_offset,
            "condition_fire_offset": self.condition_fire_offset,
        }

    @classmethod
    def from_dict(cls, node_id: str, data: dict[str, Any]) -> AggregationNodeCheckpoint:
        """Reconstruct from checkpoint dict (Tier 1 — crash on corruption).

        Args:
            node_id: Node ID for error messages (not stored on the dataclass).
            data: Node-level dict from checkpoint.

        Raises:
            AuditIntegrityError: If required keys are missing or structure is invalid.
        """
        required_fields = {
            "tokens",
            "batch_id",
            "elapsed_age_seconds",
            "count_fire_offset",
            "condition_fire_offset",
        }
        missing = required_fields - set(data.keys())
        if missing:
            raise AuditIntegrityError(
                f"Corrupted aggregation node checkpoint '{node_id}': missing required fields {missing}. Found: {set(data.keys())}"
            )

        tokens_data = data["tokens"]
        if not isinstance(tokens_data, list):
            raise AuditIntegrityError(
                f"Corrupted aggregation node checkpoint '{node_id}': 'tokens' must be a list, got {type(tokens_data).__name__}"
            )

        batch_id = data["batch_id"]
        if batch_id is None:
            raise AuditIntegrityError(
                f"Corrupted aggregation node checkpoint '{node_id}': 'batch_id' is None. Checkpoint entries with tokens must include a batch_id."
            )

        tokens = tuple(AggregationTokenCheckpoint.from_dict(t) for t in tokens_data)

        return cls(
            tokens=tokens,
            batch_id=batch_id,
            elapsed_age_seconds=data["elapsed_age_seconds"],
            count_fire_offset=data["count_fire_offset"],
            condition_fire_offset=data["condition_fire_offset"],
        )


@dataclass(frozen=True, slots=True)
class AggregationCheckpointState:
    """Full aggregation checkpoint state (Level 1).

    Wire format (preserved for ``checkpoint_dumps`` compatibility)::

        {
            "_version": "4.0",
            "node_id_1": { ... node checkpoint ... },
            "node_id_2": { ... node checkpoint ... },
        }

    Attributes:
        version: Checkpoint format version string.
        nodes: Map of node_id → per-node checkpoint.
    """

    version: str
    nodes: Mapping[str, AggregationNodeCheckpoint]

    def __post_init__(self) -> None:
        if not self.version:
            raise ValueError("AggregationCheckpointState.version must not be empty")
        if not isinstance(self.nodes, Mapping):
            raise TypeError(f"AggregationCheckpointState.nodes must be a Mapping, got {type(self.nodes).__name__}")
        freeze_fields(self, "nodes")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to flat wire-format dict.

        Preserves the existing ``{"_version": ..., "node_id": {...}, ...}`` layout.
        """
        state: dict[str, Any] = {"_version": self.version}
        for node_id, node_ckpt in self.nodes.items():
            state[node_id] = node_ckpt.to_dict()
        return state

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> AggregationCheckpointState:
        """Reconstruct from wire-format dict (Tier 1 — crash on corruption).

        Args:
            data: Flat checkpoint dict with ``_version`` key and node_id keys.

        Raises:
            AuditIntegrityError: If ``_version`` is missing or structure is invalid.
        """
        if "_version" not in data:
            raise AuditIntegrityError(f"Corrupted aggregation checkpoint: missing '_version' key. Found keys: {sorted(data.keys())}.")
        version = data["_version"]

        nodes: dict[str, AggregationNodeCheckpoint] = {}
        for key, value in data.items():
            if key == "_version":
                continue
            if key.startswith("_"):
                raise AuditIntegrityError(
                    f"Corrupted aggregation checkpoint: unexpected reserved key {key!r}. Only '_version' is a valid metadata key."
                )
            nodes[key] = AggregationNodeCheckpoint.from_dict(key, value)

        return cls(version=version, nodes=nodes)
