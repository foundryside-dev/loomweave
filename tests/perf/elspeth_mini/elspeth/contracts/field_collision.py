"""Field name collision detection.

Pure utility used by both engine pre-emission checks and plugin
implementations (batch_replicate, etc.) when transforms enrich rows with
new fields. Silent overwrites are data loss; this helper exists to make
collision detection mandatory rather than opt-in per plugin.

Lives at L0 (contracts) because it operates on field-name sets — schema-
contract primitives — and is consumed by both L2 (engine) and L3 (plugins).
"""

from __future__ import annotations

from collections.abc import Iterable


def detect_field_collisions(
    existing_fields: set[str],
    new_fields: Iterable[str],
) -> list[str] | None:
    """Detect field name collisions between existing row fields and new fields.

    Args:
        existing_fields: Field names already present in the row.
        new_fields: Field names the transform intends to add.

    Returns:
        Sorted list of colliding field names, or None if no collisions.
    """
    collisions = sorted(f for f in new_fields if f in existing_fields)
    return collisions or None


__all__ = ["detect_field_collisions"]
