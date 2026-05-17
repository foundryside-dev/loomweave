"""Shared frozen-registry primitive for contract-layer registries.

The Tier-1 exception registry and declaration-contract registry both need the
same mechanical invariants: an ordered registration list, an auxiliary lookup
structure, a freeze flag, and one lock protecting every read/write/freeze
transition. Keep that machinery here so new registries do not re-learn the
same concurrency and post-bootstrap rules by copy/paste.

This module is L0 (contracts layer) and imports only stdlib.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator
from contextlib import contextmanager
from threading import RLock
from typing import TypeVar

ItemT = TypeVar("ItemT")
AuxiliaryT = TypeVar("AuxiliaryT")

FrozenErrorFactory = Callable[[], BaseException]
FrozenGetter = Callable[[], bool]
FrozenSetter = Callable[[bool], None]


class FrozenRegistry[ItemT, AuxiliaryT]:
    """Ordered registry with an auxiliary map, freeze flag, and shared lock."""

    def __init__(
        self,
        *,
        name: str,
        auxiliary: AuxiliaryT,
        frozen_getter: FrozenGetter | None = None,
        frozen_setter: FrozenSetter | None = None,
    ) -> None:
        if not name.strip():
            raise ValueError("FrozenRegistry requires a non-empty name")
        if (frozen_getter is None) != (frozen_setter is None):
            raise ValueError("FrozenRegistry frozen_getter and frozen_setter must be supplied together")
        self.name = name
        self.items: list[ItemT] = []
        self.auxiliary = auxiliary
        self.lock = RLock()
        self._frozen = False
        self._frozen_getter = frozen_getter
        self._frozen_setter = frozen_setter

    def _is_frozen_unlocked(self) -> bool:
        if self._frozen_getter is not None:
            return self._frozen_getter()
        return self._frozen

    def _set_frozen_unlocked(self, value: bool) -> None:
        if self._frozen_setter is not None:
            self._frozen_setter(value)
            return
        self._frozen = value

    @contextmanager
    def read(self) -> Iterator[FrozenRegistry[ItemT, AuxiliaryT]]:
        """Hold the registry lock for a consistent read snapshot."""
        with self.lock:
            yield self

    @contextmanager
    def write_unfrozen(
        self,
        frozen_error: FrozenErrorFactory | None = None,
    ) -> Iterator[FrozenRegistry[ItemT, AuxiliaryT]]:
        """Hold the registry lock for a mutation, failing if frozen."""
        with self.lock:
            if self._is_frozen_unlocked():
                if frozen_error is not None:
                    raise frozen_error()
                raise RuntimeError(f"{self.name} registry is frozen")
            yield self

    def freeze(self) -> None:
        """Seal the registry under the same lock used by writers."""
        with self.lock:
            self._set_frozen_unlocked(True)

    def is_frozen(self) -> bool:
        """Return whether the registry is sealed."""
        with self.lock:
            return self._is_frozen_unlocked()
