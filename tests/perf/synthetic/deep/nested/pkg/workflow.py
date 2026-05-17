from __future__ import annotations

from collections.abc import Callable
from typing import Protocol, TypeVar


class Step(Protocol):
    def __call__(self, payload: dict[str, str]) -> dict[str, str]: ...


T = TypeVar("T", bound=Callable[..., object])


def audit(name: str) -> Callable[[T], T]:
    def decorate(func: T) -> T:
        return func

    return decorate


@audit("normalize")
@audit("validate")
def normalize(payload: dict[str, str]) -> dict[str, str]:
    payload["normalized"] = "yes"
    return payload


def enrich(payload: dict[str, str]) -> dict[str, str]:
    payload["enriched"] = "yes"
    return payload


HANDLERS: dict[str, Step] = {
    "normalize": normalize,
    "enrich": enrich,
}


def run_direct(payload: dict[str, str]) -> dict[str, str]:
    return normalize(payload)


def run_dispatch(name: str, payload: dict[str, str]) -> dict[str, str]:
    return HANDLERS[name](payload)


class Runner:
    def __init__(self, step: Step) -> None:
        self._step = step

    def __call__(self, payload: dict[str, str]) -> dict[str, str]:
        return self._step(payload)


def run_dunder(payload: dict[str, str]) -> dict[str, str]:
    runner = Runner(enrich)
    return runner(payload)
