"""Coalesce policy and merge strategy enums for the audit trail.

These replace bare ``str`` in CoalesceMetadata so that mypy catches
invalid policy/strategy values at development time. Values match the
Literal strings in ``CoalesceSettings`` (core/config.py).
"""

from enum import StrEnum


class CoalescePolicy(StrEnum):
    """How a coalesce point handles partial branch arrivals."""

    REQUIRE_ALL = "require_all"
    QUORUM = "quorum"
    BEST_EFFORT = "best_effort"
    FIRST = "first"


class MergeStrategy(StrEnum):
    """How a coalesce point combines row data from branches."""

    UNION = "union"
    NESTED = "nested"
    SELECT = "select"
