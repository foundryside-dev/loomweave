"""AuditEvidence nominal base class (ADR-010 §Decision 1).

Violation classes that contribute structured context to ExecutionError.context
via NodeStateGuard.__exit__ MUST inherit AuditEvidenceBase explicitly. A
structural Protocol was rejected (see ADR-010 §Alternative 3) because
single-method @runtime_checkable Protocols admit accidental duck-type matches
from unrelated classes, which would let stray to_audit_dict methods (e.g.,
in test helpers or third-party exceptions) populate the audit record with
attacker-chosen or accidental shapes.

This is L0 contracts — no imports from core, engine, or plugins.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from collections.abc import Callable, Mapping
from typing import Any, cast


class AuditEvidenceBase(ABC):
    """Nominal abstract base for audit-contributing exceptions.

    Inheriting this class is an explicit author declaration: "this exception
    contributes a structured payload to the Landscape audit trail." Subclasses
    MUST implement ``to_audit_dict()`` returning a JSON-serializable mapping.

    Any non-serializable value will surface as AuditIntegrityError at write
    time (Tier-1) — the canonical-JSON serializer crashes on non-primitive
    values.

    Implementation note: CPython 3.13 routes ``BaseException.__new__`` through
    a C-level fast-path that bypasses the ``ABCMeta.__call__`` abstract-method
    guard for exception subclasses. ``AuditEvidenceBase`` therefore installs a
    checked ``__init__`` wrapper on every subclass via ``__init_subclass__``
    and keeps a direct ``__init__`` guard for the base class itself. That
    restores the invariant even when a subclass uses non-cooperative ``__init__``
    logic or orders an exception base before ``AuditEvidenceBase`` in the MRO.
    """

    _InitCallable = Callable[..., None]

    @staticmethod
    def _raise_if_abstract(cls: type[object]) -> None:
        # Direct attribute access is safe: ABCMeta always sets
        # ``__abstractmethods__`` on every class in the hierarchy (empty
        # frozenset when all are implemented).
        abstract_methods = cast(frozenset[str], cast(Any, cls).__abstractmethods__)
        if abstract_methods:
            names = ", ".join(sorted(abstract_methods))
            raise TypeError(f"Can't instantiate abstract class {cls.__name__} without an implementation for abstract method(s) {names}")

    def __init_subclass__(cls, **kwargs: object) -> None:
        super().__init_subclass__(**kwargs)

        original_init = cast(AuditEvidenceBase._InitCallable, cls.__init__)

        def checked_init(self: object, *args: object, **inner_kwargs: object) -> None:
            AuditEvidenceBase._raise_if_abstract(type(self))
            original_init(self, *args, **inner_kwargs)

        cls.__init__ = checked_init  # type: ignore[method-assign]

    def __init__(self, *args: object, **kwargs: object) -> None:
        AuditEvidenceBase._raise_if_abstract(type(self))
        super().__init__(*args, **kwargs)

    @abstractmethod
    def to_audit_dict(self) -> Mapping[str, Any]:
        """Return a JSON-serializable mapping for the audit record."""
        raise NotImplementedError
