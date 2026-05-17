"""Factory for creating SchemaContract from configuration.

Bridges the gap between user-facing SchemaConfig (YAML) and runtime
SchemaContract used for validation and dual-name access.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import TYPE_CHECKING, Literal

from elspeth.contracts.schema_contract import FieldContract, SchemaContract

if TYPE_CHECKING:
    from elspeth.contracts.schema import SchemaConfig


# Type mapping from SchemaConfig field types to Python types
_FIELD_TYPE_MAP: dict[str, type] = {
    "int": int,
    "str": str,
    "float": float,
    "bool": bool,
    "any": object,  # 'any' accepts anything - use object as base type
}


def map_schema_mode(
    mode: Literal["fixed", "flexible", "observed"],
) -> Literal["FIXED", "FLEXIBLE", "OBSERVED"]:
    """Map SchemaConfig mode to SchemaContract mode.

    YAML uses lowercase (fixed/flexible/observed) per YAML convention,
    runtime uses uppercase (FIXED/FLEXIBLE/OBSERVED) for enum-like behavior.

    Args:
        mode: SchemaConfig mode ('fixed', 'flexible', or 'observed')

    Returns:
        SchemaContract mode literal (uppercase)
    """
    # Simple uppercase conversion - YAML lowercase to runtime uppercase
    return mode.upper()  # type: ignore[return-value]  # str.upper() returns str, not Literal; callers validate mode beforehand


def create_contract_from_config(
    config: SchemaConfig,
    field_resolution: Mapping[str, str] | None = None,
) -> SchemaContract:
    """Create SchemaContract from SchemaConfig.

    For fixed schemas, creates a locked contract with declared fields.
    For flexible/observed schemas, creates an unlocked contract that will
    infer additional fields from the first valid row.

    Args:
        config: Schema configuration from YAML
        field_resolution: Optional mapping of original->normalized names.
            If provided, original_name on FieldContract will use the
            original header; otherwise, original_name = normalized_name.

    Returns:
        SchemaContract ready for validation
    """
    mode = map_schema_mode(config.mode)

    # Validate normalized mode is a known value
    if mode not in ("FIXED", "FLEXIBLE", "OBSERVED"):
        raise ValueError(f"Invalid schema mode '{config.mode}' (normalized: '{mode}'). Expected 'fixed', 'flexible', or 'observed'.")

    # Validate invariant: FIXED and FLEXIBLE require explicit fields
    if mode in ("FIXED", "FLEXIBLE") and config.fields is None:
        raise ValueError(f"Schema mode '{config.mode}' requires explicit field definitions. Use 'mode: observed' to infer types from data.")

    # Build reverse mapping for looking up original names
    # field_resolution is original->normalized, we need normalized->original
    normalized_to_original: dict[str, str] = {}
    if field_resolution:
        normalized_to_original = {v: k for k, v in field_resolution.items()}

    # For explicit schemas, create FieldContracts from FieldDefinitions
    fields: tuple[FieldContract, ...] = ()

    if config.fields is not None:
        field_contracts: list[FieldContract] = []
        for fd in config.fields:
            # Look up original name if resolution provided
            original = normalized_to_original.get(fd.name, fd.name)

            fc = FieldContract(
                normalized_name=fd.name,
                original_name=original,
                python_type=_FIELD_TYPE_MAP[fd.field_type],
                required=fd.required,
                source="declared",
                nullable=fd.nullable,
            )
            field_contracts.append(fc)
        fields = tuple(field_contracts)

    # Derive locked from normalized mode, not raw config.mode.
    # FIXED schemas start locked (types are fully declared).
    # FLEXIBLE/OBSERVED start unlocked and lock after first valid row.
    locked = mode == "FIXED"

    return SchemaContract(
        mode=mode,
        fields=fields,
        locked=locked,
    )
