"""Sink header mode resolution.

Sinks can output headers in three modes:
- NORMALIZED: Use Python identifier names (default)
- ORIGINAL: Restore original source header names
- CUSTOM: Use explicit mapping for external system handover

This module bridges SchemaContract (which stores original names)
with sink output configuration.
"""

from __future__ import annotations

from enum import StrEnum, auto
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from elspeth.contracts.schema_contract import SchemaContract


class HeaderMode(StrEnum):
    """Header output mode for sinks."""

    NORMALIZED = auto()  # Python identifiers: "amount_usd"
    ORIGINAL = auto()  # Source headers: "'Amount USD'"
    CUSTOM = auto()  # Explicit mapping: "AMOUNT_USD"


def parse_header_mode(
    config: str | dict[str, str] | None,
) -> HeaderMode:
    """Parse header mode from sink config.

    Args:
        config: One of:
            - None: Default to NORMALIZED
            - "normalized": Use normalized names
            - "original": Restore original names
            - dict: Custom mapping

    Returns:
        HeaderMode enum value

    Raises:
        ValueError: If config is invalid string
    """
    if config is None:
        return HeaderMode.NORMALIZED

    if isinstance(config, dict):
        if not config:
            raise ValueError(
                "Empty dict is not valid for CUSTOM header mode. "
                "All fields must be explicitly mapped — provide a complete "
                "normalized_name → display_name mapping."
            )
        return HeaderMode.CUSTOM

    if config == "normalized":
        return HeaderMode.NORMALIZED

    if config == "original":
        return HeaderMode.ORIGINAL

    raise ValueError(f"Invalid header mode '{config}'. Expected 'normalized', 'original', or mapping dict.")


def resolve_headers(
    *,
    contract: SchemaContract | None,
    mode: HeaderMode,
    custom_mapping: dict[str, str] | None,
    field_names: list[str] | None = None,
) -> dict[str, str]:
    """Resolve output headers based on mode and contract.

    Args:
        contract: Schema contract with original name metadata
        mode: Header mode (NORMALIZED, ORIGINAL, CUSTOM)
        custom_mapping: Custom mapping for CUSTOM mode
        field_names: Field names if contract is None

    Returns:
        Dict mapping normalized_name -> output_header
    """
    # Determine field names to process
    if contract is not None:
        names = [f.normalized_name for f in contract.fields]
    elif field_names is not None:
        names = field_names
    else:
        raise ValueError(
            "resolve_headers() called with both contract and field_names as None. Caller must provide at least one source of field names."
        )

    # Validate CUSTOM mode preconditions up front
    if mode == HeaderMode.CUSTOM and not custom_mapping:
        raise ValueError("CUSTOM header mode requires a non-empty custom_mapping. Got None or empty dict — this is a configuration error.")

    result: dict[str, str] = {}

    for name in names:
        if mode == HeaderMode.NORMALIZED:
            result[name] = name

        elif mode == HeaderMode.ORIGINAL:
            if contract is not None:
                try:
                    field = contract.get_field(name)
                except KeyError as e:
                    raise KeyError(f"Contract corruption detected: missing field '{name}' during ORIGINAL header resolution.") from e
                result[name] = field.original_name
            else:
                result[name] = name

        elif mode == HeaderMode.CUSTOM:
            if custom_mapping is None:
                raise ValueError("CUSTOM header mode: custom_mapping is None despite precondition check. This is an internal logic error.")
            if name not in custom_mapping:
                raise ValueError(
                    f"CUSTOM header mode has no mapping for field '{name}'. "
                    f"All fields must be explicitly mapped — silent fallback to normalized "
                    f"names risks data corruption in external system handover. "
                    f"Mapped fields: {sorted(custom_mapping.keys())}"
                )
            result[name] = custom_mapping[name]

    return result
