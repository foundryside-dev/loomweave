"""Project-interpreter discovery (clarion-5cf9643de9)."""

from __future__ import annotations

import os
import stat
from typing import TYPE_CHECKING

from loomweave_plugin_python.interpreter import (
    INTERPRETER_OVERRIDE_ENV,
    ProjectInterpreter,
    discover_project_interpreter,
)

if TYPE_CHECKING:
    from pathlib import Path

    import pytest


def _make_python(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def test_dotvenv_wins_over_virtual_env_and_path(tmp_path: Path) -> None:
    dotvenv = _make_python(tmp_path / ".venv" / "bin" / "python")
    other = _make_python(tmp_path / "elsewhere" / "bin" / "python")
    environ = {"VIRTUAL_ENV": str(other.parent.parent), "PATH": str(other.parent)}

    found = discover_project_interpreter(tmp_path, environ)

    assert found == ProjectInterpreter(path=str(dotvenv.resolve()), source="dotvenv")
    assert found.pinned


def test_override_env_wins_over_dotvenv(tmp_path: Path) -> None:
    _make_python(tmp_path / ".venv" / "bin" / "python")
    override = _make_python(tmp_path / "custom" / "python")

    found = discover_project_interpreter(tmp_path, {INTERPRETER_OVERRIDE_ENV: str(override)})

    assert found.source == "override"
    assert found.path == str(override.resolve())
    assert found.pinned


def test_unusable_override_is_ignored_and_discovery_continues(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    dotvenv = _make_python(tmp_path / ".venv" / "bin" / "python")
    missing = tmp_path / "nope" / "python"

    found = discover_project_interpreter(tmp_path, {INTERPRETER_OVERRIDE_ENV: str(missing)})

    assert found.source == "dotvenv"
    assert found.path == str(dotvenv.resolve())
    assert INTERPRETER_OVERRIDE_ENV in capsys.readouterr().err


def test_virtual_env_then_conda_then_path(tmp_path: Path) -> None:
    venv = _make_python(tmp_path / "venv" / "bin" / "python")
    conda = _make_python(tmp_path / "conda" / "bin" / "python")
    on_path = _make_python(tmp_path / "bin" / "python3")

    assert discover_project_interpreter(
        tmp_path,
        {
            "VIRTUAL_ENV": str(venv.parent.parent),
            "CONDA_PREFIX": str(conda.parent.parent),
            "PATH": str(on_path.parent),
        },
    ) == ProjectInterpreter(path=str(venv.resolve()), source="virtual_env")
    assert discover_project_interpreter(
        tmp_path, {"CONDA_PREFIX": str(conda.parent.parent), "PATH": str(on_path.parent)}
    ) == ProjectInterpreter(path=str(conda.resolve()), source="conda")
    unpinned = discover_project_interpreter(tmp_path, {"PATH": str(on_path.parent)})
    assert unpinned == ProjectInterpreter(path=str(on_path.resolve()), source="path")
    assert not unpinned.pinned


def test_path_prefers_python_over_python3(tmp_path: Path) -> None:
    py = _make_python(tmp_path / "bin" / "python")
    _make_python(tmp_path / "bin" / "python3")

    found = discover_project_interpreter(tmp_path, {"PATH": str(tmp_path / "bin")})

    assert found.path == str(py.resolve())


def test_nothing_found_is_none_and_unpinned(tmp_path: Path) -> None:
    found = discover_project_interpreter(tmp_path, {"PATH": str(tmp_path / "empty")})

    assert found == ProjectInterpreter(path=None, source="none")
    assert not found.pinned


def test_non_executable_dotvenv_python_is_not_a_hit(tmp_path: Path) -> None:
    target = tmp_path / ".venv" / "bin" / "python"
    target.parent.mkdir(parents=True)
    target.write_text("", encoding="utf-8")
    target.chmod(stat.S_IRUSR | stat.S_IWUSR)
    assert not os.access(target, os.X_OK)

    found = discover_project_interpreter(tmp_path, {"PATH": str(tmp_path / "empty")})

    assert found.source == "none"


def test_environ_defaults_to_os_environ(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    dotvenv = _make_python(tmp_path / ".venv" / "bin" / "python")
    monkeypatch.delenv(INTERPRETER_OVERRIDE_ENV, raising=False)

    assert discover_project_interpreter(tmp_path).path == str(dotvenv.resolve())
