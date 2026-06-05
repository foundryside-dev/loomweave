"""L2 3-segment EntityId assembler matching WP1's Rust ``entity_id()`` byte-for-byte.

Per ADR-003 + ADR-022, every Loomweave entity has a 3-segment ID of the
form ``{plugin_id}:{kind}:{canonical_qualified_name}``.

Validation (mirrors ``crates/loomweave-core/src/entity_id.rs``):

- ``plugin_id`` and ``kind`` must match the identifier grammar
  ``[a-z][a-z0-9_]*`` (ADR-022).
- No segment may contain a literal ``:`` (reserved separator).
- No segment may be empty.

The shared fixture ``fixtures/entity_id.json`` (Task 5) drives the
cross-language parity check: both the Rust assembler and this Python
assembler consume the same fixture rows and must produce identical
strings byte-for-byte.
"""

from __future__ import annotations

import re

_GRAMMAR = re.compile(r"^[a-z][a-z0-9_]*$")


class EntityIdError(ValueError):
    """Base class for all ``entity_id()`` validation failures."""


class EmptySegmentError(EntityIdError):
    """A segment (``plugin_id``, ``kind``, or ``canonical_qualified_name``) was empty."""

    def __init__(self, field: str) -> None:
        super().__init__(f"segment {field} empty")
        self.field = field


class GrammarViolationError(EntityIdError):
    """A segment did not match the ADR-022 grammar ``[a-z][a-z0-9_]*``."""

    def __init__(self, field: str, value: str) -> None:
        super().__init__(f"segment {field} violates ADR-022 grammar [a-z][a-z0-9_]*: {value!r}")
        self.field = field
        self.value = value


class SegmentContainsColonError(EntityIdError):
    """A segment contained the reserved ``:`` separator (UQ-WP1-07)."""

    def __init__(self, field: str, value: str) -> None:
        super().__init__(f"segment {field} contains reserved ':' separator: {value!r}")
        self.field = field
        self.value = value


def _validate_grammar(field: str, value: str) -> None:
    """Mirror ``validate_grammar`` in the Rust side — empty, colon, then regex."""
    if not value:
        raise EmptySegmentError(field)
    if ":" in value:
        raise SegmentContainsColonError(field, value)
    if not _GRAMMAR.fullmatch(value):
        raise GrammarViolationError(field, value)


def entity_id(plugin_id: str, kind: str, canonical_qualified_name: str) -> str:
    """Assemble the 3-segment EntityId string with full validation."""
    _validate_grammar("plugin_id", plugin_id)
    _validate_grammar("kind", kind)
    qn_field = "canonical_qualified_name"
    if not canonical_qualified_name:
        raise EmptySegmentError(qn_field)
    if ":" in canonical_qualified_name:
        raise SegmentContainsColonError(qn_field, canonical_qualified_name)
    return f"{plugin_id}:{kind}:{canonical_qualified_name}"
