"""Typed coalesce metadata for the Landscape audit trail.

Replaces loose ``dict[str, Any]`` at 4 construction sites in
``coalesce_executor.py`` with a frozen dataclass that makes every
field visible to mypy and enforces immutability.

Trust-tier notes
----------------
* Factory classmethods — used by our code (Tier 1/2).
* ``to_dict()`` — serialization boundary for ``context_after_json``.
  Omits ``None`` fields so the JSON shape is identical to the
  pre-dataclass dict literals.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass, replace
from typing import Any

from elspeth.contracts.coalesce_enums import CoalescePolicy, MergeStrategy
from elspeth.contracts.freeze import freeze_fields


@dataclass(frozen=True, slots=True)
class ArrivalOrderEntry:
    """One branch's arrival timing relative to first arrival."""

    branch: str
    arrival_offset_ms: float

    def to_dict(self) -> dict[str, Any]:
        return {"branch": self.branch, "arrival_offset_ms": self.arrival_offset_ms}


@dataclass(frozen=True, slots=True)
class CoalesceMetadata:
    """Typed metadata for coalesce merge/failure audit records.

    All 4 construction sites in ``coalesce_executor.py`` produce
    subsets of these fields.  ``to_dict()`` omits ``None`` values so
    the serialized output is identical to the pre-dataclass dicts.

    Attributes:
        policy: Coalesce policy (require_all, first, quorum, best_effort).
        reason: Human-readable reason for late arrival failure.
        merge_strategy: Merge strategy (union, nested, select).
        expected_branches: All configured branch names.
        branches_arrived: Branches that actually arrived before merge/failure.
        branches_lost: Mapping of branch name to loss reason.
        select_branch: Selected branch name (for select merge strategy).
        arrival_order: Chronological arrival entries.
        wait_duration_ms: Total wall-clock wait from first arrival to merge.
        quorum_required: Quorum threshold (for quorum policy failures).
        timeout_seconds: Configured timeout (for timeout-triggered failures).
        union_field_collisions: Field name to contributing branches (union merge).
        union_field_origins: Field name to originating branch (every union merge).
        union_field_collision_values: Field name to tuple of ``(branch, value)``
            entries in merge order (populated only when collisions occurred).
    """

    policy: CoalescePolicy

    def __post_init__(self) -> None:
        if not self.policy:
            raise ValueError("CoalesceMetadata.policy must not be empty")
        # Freeze all container fields — catches direct construction with raw lists/dicts
        fields_to_freeze = []
        if self.expected_branches is not None:
            fields_to_freeze.append("expected_branches")
        if self.branches_arrived is not None:
            fields_to_freeze.append("branches_arrived")
        if self.arrival_order is not None:
            fields_to_freeze.append("arrival_order")
        if self.branches_lost is not None:
            fields_to_freeze.append("branches_lost")
        if self.union_field_collisions is not None:
            fields_to_freeze.append("union_field_collisions")
        if self.union_field_origins is not None:
            fields_to_freeze.append("union_field_origins")
        if self.union_field_collision_values is not None:
            fields_to_freeze.append("union_field_collision_values")
        if self.lost_branch_expected_fields is not None:
            fields_to_freeze.append("lost_branch_expected_fields")
        if fields_to_freeze:
            freeze_fields(self, *fields_to_freeze)

    # Failure context
    reason: str | None = None

    # Merge context
    merge_strategy: MergeStrategy | None = None
    expected_branches: tuple[str, ...] | None = None
    branches_arrived: tuple[str, ...] | None = None
    branches_lost: Mapping[str, str] | None = None
    select_branch: str | None = None

    # Timing
    arrival_order: tuple[ArrivalOrderEntry, ...] | None = None
    wait_duration_ms: float | None = None

    # Failure policy fields
    quorum_required: int | None = None
    timeout_seconds: float | None = None

    # Union merge collision info
    union_field_collisions: Mapping[str, tuple[str, ...]] | None = None

    # Union merge provenance (populated for every union merge)
    union_field_origins: Mapping[str, str] | None = None

    # Union merge collision values (populated only when collisions occurred).
    # Outer key: field name. Inner tuple entries: (branch_name, value) in merge order.
    # The last entry is the winner under last_wins; first under first_wins.
    union_field_collision_values: Mapping[str, tuple[tuple[str, Any], ...]] | None = None

    # Lost branch expected fields (populated when branches_lost is non-empty).
    # Outer key: branch name. Value: tuple of field names that branch would have
    # contributed. This enables audit queries like "what fields were expected
    # from lost branch X?" without requiring DAG traversal.
    lost_branch_expected_fields: Mapping[str, tuple[str, ...]] | None = None

    # ------------------------------------------------------------------
    # Serialization
    # ------------------------------------------------------------------

    def to_dict(self) -> dict[str, Any]:
        """Serialize to plain dict, omitting None fields.

        Produces output identical to the current dict literals
        in ``coalesce_executor.py``.
        """
        result: dict[str, Any] = {"policy": self.policy.value}
        if self.reason is not None:
            result["reason"] = self.reason
        if self.merge_strategy is not None:
            result["merge_strategy"] = self.merge_strategy.value
        if self.expected_branches is not None:
            result["expected_branches"] = list(self.expected_branches)
        if self.branches_arrived is not None:
            result["branches_arrived"] = list(self.branches_arrived)
        if self.branches_lost is not None:
            result["branches_lost"] = dict(self.branches_lost)
        if self.select_branch is not None:
            result["select_branch"] = self.select_branch
        if self.arrival_order is not None:
            result["arrival_order"] = [e.to_dict() for e in self.arrival_order]
        if self.wait_duration_ms is not None:
            result["wait_duration_ms"] = self.wait_duration_ms
        if self.quorum_required is not None:
            result["quorum_required"] = self.quorum_required
        if self.timeout_seconds is not None:
            result["timeout_seconds"] = self.timeout_seconds
        if self.union_field_collisions is not None:
            result["union_field_collisions"] = {k: list(v) for k, v in self.union_field_collisions.items()}
        if self.union_field_origins is not None:
            result["union_field_origins"] = dict(self.union_field_origins)
        if self.union_field_collision_values is not None:
            result["union_field_collision_values"] = {
                field: [list(entry) for entry in entries] for field, entries in self.union_field_collision_values.items()
            }
        if self.lost_branch_expected_fields is not None:
            result["lost_branch_expected_fields"] = {k: list(v) for k, v in self.lost_branch_expected_fields.items()}
        return result

    # ------------------------------------------------------------------
    # Factories
    # ------------------------------------------------------------------

    @classmethod
    def for_late_arrival(cls, *, policy: CoalescePolicy, reason: str) -> CoalesceMetadata:
        """Late arrival after merge/failure already completed."""
        return cls(policy=policy, reason=reason)

    @classmethod
    def for_failure(
        cls,
        *,
        policy: CoalescePolicy,
        expected_branches: Sequence[str],
        branches_arrived: Sequence[str],
        branches_lost: dict[str, str] | None = None,
        lost_branch_expected_fields: dict[str, tuple[str, ...]] | None = None,
        quorum_required: int | None = None,
        timeout_seconds: float | None = None,
    ) -> CoalesceMetadata:
        """Merge failure (timeout, missing branches, quorum not met)."""
        return cls(
            policy=policy,
            expected_branches=tuple(expected_branches),
            branches_arrived=tuple(branches_arrived),
            branches_lost=branches_lost,
            lost_branch_expected_fields=lost_branch_expected_fields,
            quorum_required=quorum_required,
            timeout_seconds=timeout_seconds,
        )

    @classmethod
    def for_select_not_arrived(
        cls,
        *,
        policy: CoalescePolicy,
        merge_strategy: MergeStrategy,
        select_branch: str,
        branches_arrived: Sequence[str],
    ) -> CoalesceMetadata:
        """Select branch not in arrived set at merge time."""
        return cls(
            policy=policy,
            merge_strategy=merge_strategy,
            select_branch=select_branch,
            branches_arrived=tuple(branches_arrived),
        )

    @classmethod
    def for_merge(
        cls,
        *,
        policy: CoalescePolicy,
        merge_strategy: MergeStrategy,
        expected_branches: Sequence[str],
        branches_arrived: Sequence[str],
        branches_lost: dict[str, str],
        lost_branch_expected_fields: dict[str, tuple[str, ...]] | None = None,
        arrival_order: Sequence[ArrivalOrderEntry],
        wait_duration_ms: float,
    ) -> CoalesceMetadata:
        """Successful merge with full audit context."""
        return cls(
            policy=policy,
            merge_strategy=merge_strategy,
            expected_branches=tuple(expected_branches),
            branches_arrived=tuple(branches_arrived),
            branches_lost=branches_lost,
            lost_branch_expected_fields=lost_branch_expected_fields,
            arrival_order=tuple(arrival_order),
            wait_duration_ms=wait_duration_ms,
        )

    @classmethod
    def with_union_result(
        cls,
        base: CoalesceMetadata,
        *,
        field_origins: Mapping[str, str],
        collisions: Mapping[str, Sequence[str]] | None = None,
        collision_values: Mapping[str, Sequence[tuple[str, Any]]] | None = None,
    ) -> CoalesceMetadata:
        """Layer union-merge provenance onto an existing metadata instance.

        ``field_origins`` is always populated for union merges. ``collisions``
        and ``collision_values`` are populated only when at least one field
        was produced by more than one branch.
        """
        return replace(
            base,
            union_field_origins=dict(field_origins),
            union_field_collisions=({k: tuple(v) for k, v in collisions.items()} if collisions is not None else None),
            union_field_collision_values=(
                {k: tuple(tuple(entry) for entry in v) for k, v in collision_values.items()} if collision_values is not None else None
            ),
        )
