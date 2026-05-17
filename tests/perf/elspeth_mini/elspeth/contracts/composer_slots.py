"""Shared composer recipe slot contracts.

This module is Layer 0 and intentionally has no dependency on web/composer
runtime code. Runtime recipe validation remains in
``elspeth.web.composer.recipes``; the default-value guard here exists so recipe
authors get an import-time failure for malformed optional defaults.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal
from uuid import UUID

SlotType = Literal["blob_id", "str", "float", "int", "str_list"]


@dataclass(frozen=True, slots=True)
class SlotSpec:
    """Declares one input slot for a recipe."""

    slot_type: SlotType
    required: bool = True
    default: Any = None
    description: str = ""

    def __post_init__(self) -> None:
        if self.required or self.default is None:
            return
        try:
            _coerce_default(f"<default for {self.slot_type}>", self.slot_type, self.default)
        except ValueError as exc:
            raise ValueError(f"SlotSpec default {self.default!r} does not satisfy slot_type {self.slot_type!r}: {exc}") from exc


def _coerce_default(name: str, slot_type: SlotType, raw: Any) -> Any:
    """Validate optional SlotSpec defaults without importing web runtime code."""
    if slot_type == "blob_id":
        if type(raw) is not str:
            raise ValueError(f"slot '{name}' must be a UUID string")
        try:
            UUID(raw)
        except ValueError as exc:
            raise ValueError(f"slot '{name}' must be a valid UUID") from exc
        return raw

    if slot_type == "str":
        if type(raw) is not str:
            raise ValueError(f"slot '{name}' must be a string")
        return raw

    if slot_type == "float":
        if type(raw) is bool:
            raise ValueError(f"slot '{name}' must be a number")
        if type(raw) in (int, float):
            return float(raw)
        if type(raw) is str:
            return float(raw)
        raise ValueError(f"slot '{name}' must be a number")

    if slot_type == "int":
        if type(raw) is bool:
            raise ValueError(f"slot '{name}' must be an integer")
        if type(raw) is int:
            return raw
        if type(raw) is str:
            return int(raw)
        raise ValueError(f"slot '{name}' must be an integer")

    if slot_type == "str_list":
        if type(raw) not in (list, tuple):
            raise ValueError(f"slot '{name}' must be a JSON array of strings")
        for index, item in enumerate(raw):
            if type(item) is not str:
                raise ValueError(f"slot '{name}'[{index}] must be a string")
        return tuple(raw)

    raise ValueError(f"recipe slot type {slot_type!r} is not implemented")
