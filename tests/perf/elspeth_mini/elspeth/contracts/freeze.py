"""Recursive deep-freeze utility for immutable dataclass fields.

Converts mutable containers to their immutable equivalents:
- ``dict`` (and any ``Mapping``) → ``MappingProxyType``
- ``list`` → ``tuple``
- ``set`` → ``frozenset``

Already-frozen containers (``MappingProxyType``, ``tuple``, ``frozenset``)
are recursed into to freeze any mutable contents. When all children are
already frozen, the original object is returned (identity-preserving
idempotency for repeated ``__post_init__`` calls).

This module is L0 (contracts layer) — no imports from core, engine, or plugins.
"""

from __future__ import annotations

from collections.abc import Mapping
from types import MappingProxyType
from typing import Any


def deep_freeze(value: Any) -> Any:
    """Recursively freeze mutable containers.

    Converts ``dict`` (and any ``Mapping``) → ``MappingProxyType`` and
    ``list`` → ``tuple``, recursing into values. Non-container types
    (str, int, float, bool, None, enum members, dataclass instances)
    are returned unchanged.

    This is the standard freeze function for ``__post_init__`` guards
    on frozen dataclasses throughout the contracts layer.

    Examples:
        >>> deep_freeze({"a": [1, {"b": 2}]})
        MappingProxyType({'a': (1, MappingProxyType({'b': 2}))})

        >>> deep_freeze([{"x": 1}, {"y": 2}])
        (MappingProxyType({'x': 1}), MappingProxyType({'y': 2}))

        >>> from types import MappingProxyType
        >>> already = MappingProxyType({"k": "v"})
        >>> deep_freeze(already) == already  # detached copy, same content
        True
    """
    if isinstance(value, dict):
        return MappingProxyType({k: deep_freeze(v) for k, v in value.items()})
    if isinstance(value, list):
        return tuple(deep_freeze(item) for item in value)
    # MappingProxyType is a READ-ONLY VIEW, not a detached copy. The
    # underlying dict may still be mutable through other references.
    # Always create a fresh dict from the proxy's items to detach.
    if isinstance(value, MappingProxyType):
        return MappingProxyType({k: deep_freeze(v) for k, v in value.items()})
    if isinstance(value, tuple):
        frozen_tup = tuple(deep_freeze(item) for item in value)
        if all(a is b for a, b in zip(frozen_tup, value, strict=True)):
            return value
        return frozen_tup
    if isinstance(value, set):
        return frozenset(deep_freeze(item) for item in value)
    if isinstance(value, frozenset):
        # frozenset elements are unordered; recurse but can only detect
        # change by identity of the rebuilt set.
        frozen_fs = frozenset(deep_freeze(item) for item in value)
        if frozen_fs == value:
            return value
        return frozen_fs
    # Non-dict Mapping types (OrderedDict is a dict subclass so handled above,
    # but other Mapping implementations like custom read-only wrappers are not).
    # Convert to dict first, then freeze recursively.
    if isinstance(value, Mapping):
        return MappingProxyType({k: deep_freeze(v) for k, v in value.items()})
    # Scalars and opaque objects (str, int, float, bool, None, enums, dataclasses)
    return value


def freeze_fields(instance: object, *field_names: str) -> None:
    """Freeze named container fields on a frozen dataclass instance.

    Applies deep_freeze() to each named field, with identity-preserving
    idempotency (skips object.__setattr__ when the field is already frozen).

    This is the standard utility for __post_init__ methods on frozen
    dataclasses. Use it instead of hand-rolling deep_freeze + setattr.

    Example:
        @dataclass(frozen=True, slots=True)
        class MyRecord:
            data: Mapping[str, Any]
            items: Sequence[str]

            def __post_init__(self) -> None:
                freeze_fields(self, "data", "items")

    Raises:
        AttributeError: If any field_name is not a declared dataclass field.
    """
    import dataclasses

    declared = {f.name for f in dataclasses.fields(instance)}  # type: ignore[arg-type]
    unknown = set(field_names) - declared
    if unknown:
        raise AttributeError(f"freeze_fields: {sorted(unknown)} not declared on {type(instance).__name__}. Declared: {sorted(declared)}")
    for name in field_names:
        value = getattr(instance, name)
        frozen = deep_freeze(value)
        if frozen is not value:
            object.__setattr__(instance, name, frozen)


def deep_thaw(value: Any) -> Any:
    """Recursively convert frozen containers to JSON-serializable mutable types.

    Converts ``MappingProxyType`` → ``dict`` and ``tuple`` → ``list``,
    recursing into values. Used by ``to_dict()`` methods to produce
    JSON-serializable output from deeply frozen fields.

    **Not a true inverse of ``deep_freeze``**: converts ALL tuples to
    lists, including tuples that were native (not converted from lists
    by ``deep_freeze``). This is intentional — JSON has no tuple type,
    so ``to_dict()`` callers need lists. For JSON-like input (only dicts
    and lists), ``deep_thaw(deep_freeze(x)) == x`` holds.

    Examples:
        >>> deep_thaw(MappingProxyType({"a": (1, MappingProxyType({"b": 2}))}))
        {'a': [1, {'b': 2}]}
    """
    if isinstance(value, MappingProxyType):
        return {k: deep_thaw(v) for k, v in value.items()}
    if isinstance(value, tuple):
        return [deep_thaw(item) for item in value]
    if isinstance(value, frozenset):
        return [deep_thaw(item) for item in value]
    if isinstance(value, dict):
        return {k: deep_thaw(v) for k, v in value.items()}
    if isinstance(value, list):
        return [deep_thaw(item) for item in value]
    return value


def require_int(
    value: object,
    field_name: str,
    *,
    optional: bool = False,
    min_value: int | None = None,
) -> None:
    """Validate that a value is strictly int (not bool, str, or float).

    Tier 1 offensive validation: crash immediately on wrong types.
    bool is rejected because ``isinstance(True, int)`` is ``True`` in Python
    (bool is a subclass of int), which means a bool could silently pass
    through int-typed fields without this guard.

    Args:
        value: The value to validate.
        field_name: Field name for error messages.
        optional: If True, None is acceptable.
        min_value: If set, value must be >= min_value.

    Raises:
        TypeError: If value is not int (or not None when optional=True).
        ValueError: If value < min_value.
    """
    if optional and value is None:
        return
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{field_name} must be int, got {type(value).__name__}: {value!r}")
    if min_value is not None and value < min_value:
        raise ValueError(f"{field_name} must be >= {min_value}, got {value}")
