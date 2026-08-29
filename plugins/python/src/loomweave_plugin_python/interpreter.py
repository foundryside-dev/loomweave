"""Project-interpreter discovery for pyright (clarion-5cf9643de9).

``pyright-langserver`` decides which Python environment to type-check against
by running whatever ``python`` is first on its ``PATH`` unless the client sets
``python.pythonPath``. Under ``loomweave analyze`` launched from an agent hook
that ``python`` is the system interpreter, which cannot import the project's
editable install, and every ``tests/`` -> ``src/`` call target came back empty
while the coverage claim still said ``complete``. This module picks the
project's own interpreter deterministically so the answer no longer depends
on who launched the run.

The order below is a CROSS-LANGUAGE CONTRACT with
``crates/loomweave-core/src/plugin/interpreter.rs`` (the host runs the same
discovery, exports the winner as ``LOOMWEAVE_PYTHON_INTERPRETER``, and keys the
incremental skip on it). Change both or neither.

Paths are returned as absolute and lexically normalised (``Path.absolute`` +
``os.path.normpath``): ``.``/``..`` collapsed, symlinks preserved. A venv's
``bin/python`` is typically a symlink to the base interpreter; handing pyright
the symlink path keeps it within the project's venv site-packages, while
resolving to the realpath would escape to the base interpreter's site-packages.
"""

from __future__ import annotations

import os
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Final, Literal

if TYPE_CHECKING:
    from collections.abc import Mapping

# Operator/host override for the interpreter pyright resolves against.
# Basis: the host's discovery is authoritative under ``analyze`` and must reach
#   the plugin; operators on venv-less layouts need a pin.
# Override surface: this env var IS the override surface (set by
#   ``PluginHost::spawn_unhandshaken`` or by the operator).
# Retune trigger: none — a path, not a tunable.
# Coupling: ``loomweave_core::plugin::interpreter::PYTHON_INTERPRETER_ENV``
#   must carry the same literal.
INTERPRETER_OVERRIDE_ENV: Final = "LOOMWEAVE_PYTHON_INTERPRETER"

InterpreterSource = Literal["override", "dotvenv", "virtual_env", "conda", "path", "none"]

_PREFIX_SOURCES: Final[tuple[tuple[str, InterpreterSource], ...]] = (
    ("VIRTUAL_ENV", "virtual_env"),
    ("CONDA_PREFIX", "conda"),
)
_PINNED_SOURCES: Final[frozenset[InterpreterSource]] = frozenset(
    {"override", "dotvenv", "virtual_env", "conda"},
)


@dataclass(frozen=True)
class ProjectInterpreter:
    """The interpreter pyright will be pointed at, and where it came from."""

    path: str | None
    source: InterpreterSource

    @property
    def pinned(self) -> bool:
        """True when the interpreter is project-owned (not a PATH guess)."""
        return self.source in _PINNED_SOURCES


def _usable(candidate: Path) -> Path | None:
    if candidate.is_file() and os.access(candidate, os.X_OK):
        return Path(os.path.normpath(candidate.absolute()))
    return None


def discover_project_interpreter(
    project_root: Path,
    environ: Mapping[str, str] | None = None,
) -> ProjectInterpreter:
    """Resolve the project's interpreter in the contract order (see module doc)."""
    env = os.environ if environ is None else environ
    override = env.get(INTERPRETER_OVERRIDE_ENV)
    if override:
        if (hit := _usable(Path(override))) is not None:
            return ProjectInterpreter(path=str(hit), source="override")
        sys.stderr.write(
            f"loomweave-plugin-python: {INTERPRETER_OVERRIDE_ENV}={override!r} is not an "
            "executable file; ignoring the override and discovering the interpreter\n",
        )
    if (hit := _usable(Path(project_root) / ".venv" / "bin" / "python")) is not None:
        return ProjectInterpreter(path=str(hit), source="dotvenv")
    for var, source in _PREFIX_SOURCES:
        prefix = env.get(var)
        if prefix and (hit := _usable(Path(prefix) / "bin" / "python")) is not None:
            return ProjectInterpreter(path=str(hit), source=source)
    for name in ("python", "python3"):
        found = shutil.which(name, path=env.get("PATH", ""))
        if found is not None and (hit := _usable(Path(found))) is not None:
            return ProjectInterpreter(path=str(hit), source="path")
    return ProjectInterpreter(path=None, source="none")
