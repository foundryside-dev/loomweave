"""Canonical hashing for the contracts layer.

Provides canonical JSON serialization (RFC 8785/JCS) and stable hashing
for data that contains JSON-safe primitives and their frozen equivalents.
Frozen container types produced by ``deep_freeze`` (``MappingProxyType``,
``tuple``) are normalized to their mutable equivalents before serialization.

This module exists to break the circular dependency between contracts/
and core/canonical.py. For data containing pandas/numpy types or
PipelineRow, use elspeth.core.canonical instead — it adds a normalization
phase for domain-specific types before delegating to rfc8785.
"""

from __future__ import annotations

import hashlib
import math
from collections.abc import Mapping
from typing import Any

import rfc8785

# Version string stored with every run for hash verification.
# Single source of truth — core/canonical.py imports this constant.
CANONICAL_VERSION = "sha256-rfc8785-v1"


def _normalize_frozen_and_reject_non_finite(obj: Any) -> Any:
    """Normalize frozen containers and reject non-finite floats.

    Single recursive traversal that:
    - Converts any ``Mapping`` (including ``MappingProxyType``) → ``dict``
    - Converts ``tuple`` → ``list``
    - Rejects ``frozenset`` with ``TypeError`` (no canonical JSON ordering)
    - Rejects NaN/Infinity with ``ValueError``
    - Returns the normalized structure ready for ``rfc8785.dumps()``
    """
    if isinstance(obj, float):
        if math.isnan(obj):
            raise ValueError(f"Cannot canonicalize NaN. Use None for missing values, not NaN. Got: {obj!r}")
        if math.isinf(obj):
            raise ValueError(f"Cannot canonicalize Infinity. Use None for missing values, not Infinity. Got: {obj!r}")
        return obj
    if isinstance(obj, frozenset):
        raise TypeError(
            f"frozenset is not JSON-serializable and has no canonical ordering. Use list or tuple for ordered collections. Got: {obj!r}"
        )
    # Mapping ABC covers both dict and MappingProxyType. The dict
    # comprehension normalizes MappingProxyType → dict for rfc8785.
    if isinstance(obj, Mapping):
        return {k: _normalize_frozen_and_reject_non_finite(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_normalize_frozen_and_reject_non_finite(item) for item in obj]
    return obj


def canonical_json(obj: Any) -> str:
    """Produce canonical JSON per RFC 8785/JCS.

    Handles JSON-safe primitives and their frozen equivalents
    (``MappingProxyType`` → ``dict``, ``tuple`` → ``list``).
    For data containing pandas/numpy types or PipelineRow, use
    ``elspeth.core.canonical.canonical_json()`` instead.

    Args:
        obj: JSON-safe data structure, optionally containing frozen containers

    Returns:
        Canonical JSON string (deterministic key order, no whitespace)

    Raises:
        ValueError: If data contains NaN or Infinity
        TypeError: If data contains frozenset or other non-serializable types
    """
    normalized = _normalize_frozen_and_reject_non_finite(obj)
    result: bytes = rfc8785.dumps(normalized)
    return result.decode("utf-8")


def stable_hash(obj: Any) -> str:
    """Compute SHA-256 hash of canonical JSON for primitive data.

    For data containing pandas/numpy types or PipelineRow, use
    ``elspeth.core.canonical.stable_hash()`` instead.

    Args:
        obj: JSON-safe data structure (no pandas/numpy types)

    Returns:
        SHA-256 hex digest of canonical JSON
    """
    canonical = canonical_json(obj)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _stable_repr(obj: Any) -> str:
    """Produce a deterministic repr by sorting unordered containers.

    Dicts are sorted by key, sets/frozensets are sorted by repr of elements.
    Applied recursively so nested containers are also deterministic.
    """
    if isinstance(obj, dict):
        items = ", ".join(f"{_stable_repr(k)}: {_stable_repr(v)}" for k, v in sorted(obj.items(), key=lambda kv: repr(kv[0])))
        return "{" + items + "}"
    if isinstance(obj, (set, frozenset)):
        items = ", ".join(sorted(_stable_repr(e) for e in obj))
        prefix = "frozenset" if isinstance(obj, frozenset) else ""
        return f"{prefix}{{{items}}}" if items else f"{prefix}()"
    if isinstance(obj, (list, tuple)):
        items = ", ".join(_stable_repr(e) for e in obj)
        if isinstance(obj, tuple):
            return f"({items},)" if len(obj) == 1 else f"({items})"
        return f"[{items}]"
    return repr(obj)


def repr_hash(obj: Any) -> str:
    """Generate SHA-256 hash of repr() for non-canonical data.

    Used as fallback when canonical_json fails (NaN, Infinity, or other
    non-serializable types). Deterministic within the same Python version
    but NOT stable across versions due to repr() implementation differences.

    Sorts dict keys and set elements before repr() to ensure deterministic
    hashes regardless of insertion order.

    Appropriate for Tier-3 (external data) trust boundary where data is
    already malformed and being quarantined.

    Args:
        obj: Any Python object

    Returns:
        SHA-256 hex digest of stable repr(obj)
    """
    return hashlib.sha256(_stable_repr(obj).encode("utf-8")).hexdigest()
