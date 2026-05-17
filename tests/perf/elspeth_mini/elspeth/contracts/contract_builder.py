"""Contract builder for first-row inference and locking.

Handles the "infer-and-lock" pattern for OBSERVED and FLEXIBLE modes:
1. First row arrives
2. Types are inferred from values
3. Contract is locked
4. Subsequent rows validate against locked contract
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from elspeth.contracts.schema_contract import FieldContract, SchemaContract
from elspeth.contracts.type_normalization import normalize_type_for_contract


class ContractBuilder:
    """Manages contract state through first-row inference.

    For OBSERVED/FLEXIBLE modes, the first row determines field types.
    After processing the first row, the contract is locked and cannot
    be modified.

    Usage:
        builder = ContractBuilder(initial_contract)
        locked_contract = builder.process_first_row(first_row, resolution)
        # Use locked_contract.validate() for subsequent rows

    Attributes:
        contract: Current contract state (may be locked or unlocked)
    """

    def __init__(self, contract: SchemaContract) -> None:
        """Initialize with starting contract.

        Args:
            contract: Initial contract from config (may be locked or unlocked)
        """
        self._contract = contract

    @property
    def contract(self) -> SchemaContract:
        """Current contract state."""
        return self._contract

    def process_first_row(
        self,
        row: dict[str, Any],
        field_resolution: Mapping[str, str],
    ) -> SchemaContract:
        """Process first row to infer types and lock contract.

        For unlocked contracts (OBSERVED/FLEXIBLE):
        - Infers types from row values
        - Adds any extra fields (FLEXIBLE/OBSERVED only)
        - Locks the contract

        For locked contracts (FIXED with declared fields):
        - Returns the contract unchanged

        Args:
            row: First row data (normalized field names as keys)
            field_resolution: Mapping of original->normalized names

        Returns:
            Locked SchemaContract with all field types defined

        Raises:
            ValueError: If row contains NaN or Infinity values
        """
        # Already locked - nothing to do
        if self._contract.locked:
            return self._contract

        # Build reverse mapping: normalized -> original (with collision detection)
        normalized_to_original: dict[str, str] = {}
        for orig, norm in field_resolution.items():
            if norm in normalized_to_original:
                raise ValueError(
                    f"field_resolution collision: normalized name '{norm}' maps to "
                    f"both '{normalized_to_original[norm]}' and '{orig}'. "
                    f"Upstream normalization should prevent this — this is a source plugin bug."
                )
            normalized_to_original[norm] = orig

        # Start from current contract
        updated = self._contract

        # Get set of already-declared field names
        declared_names = {f.normalized_name for f in updated.fields}

        # Process each field in the row
        for normalized_name, value in row.items():
            if normalized_name in declared_names:
                # Field already declared - skip (type from config takes precedence)
                continue

            # New field - infer type
            # Per CLAUDE.md: No silent fallback - if field is in row but not in
            # resolution, that's a bug in the source plugin. KeyError is correct!
            original_name = normalized_to_original[normalized_name]

            # Infer type from value, mapping unsupported types to object ("any").
            try:
                python_type = normalize_type_for_contract(value)
            except TypeError:
                python_type = object

            # Null-like values (None, pd.NA, pd.NaT) normalize to type(None),
            # but for inference that means "type unknown, field is nullable" —
            # not "field is always NoneType". Use object+nullable to avoid
            # locking the field to NoneType and causing false violations on
            # subsequent rows with real values.
            nullable = False
            if python_type is type(None):
                python_type = object
                nullable = True

            new_field = FieldContract(
                normalized_name=normalized_name,
                original_name=original_name,
                python_type=python_type,
                required=False,
                source="inferred",
                nullable=nullable,
            )
            updated = SchemaContract(
                mode=updated.mode,
                fields=(*updated.fields, new_field),
                locked=updated.locked,
            )

        # Lock the contract
        updated = updated.with_locked()
        self._contract = updated

        return updated
