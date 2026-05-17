"""Pipeline runner protocol — structural typing for pipeline execution callbacks.

Used by the dependency resolver (L2) to accept a pipeline execution callback
from the application layer (L3) without creating an upward import dependency.
"""

from __future__ import annotations

from pathlib import Path
from typing import Protocol

from elspeth.contracts.run_result import RunResult


class PipelineRunner(Protocol):
    """Callable that loads, configures, and executes a pipeline to completion.

    Implementations live in L3 (application layer). The protocol is L0
    so L2 (engine) can depend on it without importing L3.
    """

    def __call__(self, settings_path: Path) -> RunResult: ...
