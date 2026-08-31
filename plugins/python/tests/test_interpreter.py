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


def test_repository_dotvenv_is_ignored(tmp_path: Path) -> None:
    _make_python(tmp_path / ".venv" / "bin" / "python")
    trusted = _make_python(tmp_path / "elsewhere" / "bin" / "python")

    found = discover_project_interpreter(
        tmp_path, {"VIRTUAL_ENV": str(trusted.parent.parent)}
    )

    assert found == ProjectInterpreter(path=str(trusted), source="virtual_env")
    assert found.pinned

def test_override_env_wins_over_other_sources(tmp_path: Path) -> None:
    _make_python(tmp_path / ".venv" / "bin" / "python")
    override = _make_python(tmp_path / "custom" / "python")

    found = discover_project_interpreter(tmp_path, {INTERPRETER_OVERRIDE_ENV: str(override)})

    assert found.source == "override"
    assert found.path == str(override)
    assert found.pinned


def test_unusable_override_is_ignored_and_discovery_continues(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    missing = tmp_path / "nope" / "python"

    found = discover_project_interpreter(tmp_path, {INTERPRETER_OVERRIDE_ENV: str(missing)})

    assert found.source == "none"
    assert found.path is None
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
    ) == ProjectInterpreter(path=str(venv), source="virtual_env")
    assert discover_project_interpreter(
        tmp_path, {"CONDA_PREFIX": str(conda.parent.parent), "PATH": str(on_path.parent)}
    ) == ProjectInterpreter(path=str(conda), source="conda")
    unpinned = discover_project_interpreter(tmp_path, {"PATH": str(on_path.parent)})
    assert unpinned == ProjectInterpreter(path=str(on_path), source="path")
    assert not unpinned.pinned


def test_path_prefers_python_over_python3(tmp_path: Path) -> None:
    py = _make_python(tmp_path / "bin" / "python")
    _make_python(tmp_path / "bin" / "python3")

    found = discover_project_interpreter(tmp_path, {"PATH": str(tmp_path / "bin")})

    assert found.path == str(py)


def test_nothing_found_is_none_and_unpinned(tmp_path: Path) -> None:
    found = discover_project_interpreter(tmp_path, {"PATH": str(tmp_path / "empty")})

    assert found == ProjectInterpreter(path=None, source="none")
    assert not found.pinned


def test_non_executable_dotvenv_python_is_ignored(tmp_path: Path) -> None:
    target = tmp_path / ".venv" / "bin" / "python"
    target.parent.mkdir(parents=True)
    target.write_text("", encoding="utf-8")
    target.chmod(stat.S_IRUSR | stat.S_IWUSR)
    assert not os.access(target, os.X_OK)

    found = discover_project_interpreter(tmp_path, {"PATH": str(tmp_path / "empty")})

    assert found.source == "none"


def test_environ_defaults_to_os_environ(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    venv = _make_python(tmp_path / "trusted" / "bin" / "python")
    monkeypatch.delenv(INTERPRETER_OVERRIDE_ENV, raising=False)
    monkeypatch.setenv("VIRTUAL_ENV", str(venv.parent.parent))

    assert discover_project_interpreter(tmp_path).path == str(venv)

def test_empty_environ_does_not_leak_to_os_environ(tmp_path: Path) -> None:
    # With an empty environ (no PATH), even if os.environ has a python on PATH,
    # the discovery should return "none" because the injected environ takes precedence.
    found = discover_project_interpreter(tmp_path, {})

    assert found == ProjectInterpreter(path=None, source="none")
    assert not found.pinned


def test_repository_dotvenv_symlink_is_ignored(tmp_path: Path) -> None:
    base_python = _make_python(tmp_path / "base" / "python3.12")
    symlink_python = tmp_path / ".venv" / "bin" / "python"
    symlink_python.parent.mkdir(parents=True)
    symlink_python.symlink_to(base_python)

    assert discover_project_interpreter(tmp_path, {}) == ProjectInterpreter(
        path=None, source="none"
    )

def test_path_with_python_and_python3_in_different_dirs(tmp_path: Path) -> None:
    # Create python3 in dirA and python in dirB; PATH is dirA:dirB.
    # The discovery should find python from dirB (preferred over python3).
    dir_a = tmp_path / "dir_a"
    dir_b = tmp_path / "dir_b"
    _make_python(dir_a / "python3")
    python_b = _make_python(dir_b / "python")

    path_value = f"{dir_a}:{dir_b}"
    found = discover_project_interpreter(tmp_path, {"PATH": path_value})

    assert found.path == str(python_b)
    assert found.source == "path"


def test_override_path_is_lexically_normalised(tmp_path: Path) -> None:
    # Create a real interpreter and an unnecessary subdirectory for testing.
    # Pass a path with ".." in it; abspath should normalize it out.
    real_python = _make_python(tmp_path / "real" / "bin" / "python")
    _sub = tmp_path / "real" / "sub"
    _sub.mkdir()
    # Path with ".." in the middle: real/sub/../bin/python
    unnormalised_path = tmp_path / "real" / "sub" / ".." / "bin" / "python"

    found = discover_project_interpreter(
        tmp_path, {INTERPRETER_OVERRIDE_ENV: str(unnormalised_path)}
    )

    # Should normalize to the real path, not keep the "..".
    assert found.path == str(real_python)
    assert found.source == "override"


def test_override_with_a_trailing_separator_matches_the_rust_host(tmp_path: Path) -> None:
    """CROSS-LANGUAGE CONTRACT: ``<dir>/real/bin/python/`` resolves the same on both sides.

    This passes because ``PurePath`` drops trailing separators at
    *construction* -- ``Path('/x/python/')`` is already ``/x/python`` before
    ``is_file()`` runs -- not because ``stat(2)`` tolerates them; a raw
    ``stat("/x/python/")`` fails with ENOTDIR. Rust has no such construction
    step, so ``interpreter.rs`` strips the separator explicitly
    (``strip_trailing_separators``) to reach this same answer. The assertion
    exists to pin the accepted-and-normalised behaviour so a future change on
    either side has to break a test rather than silently split the two.
    """
    real_python = _make_python(tmp_path / "real" / "bin" / "python")

    found = discover_project_interpreter(tmp_path, {INTERPRETER_OVERRIDE_ENV: f"{real_python}/"})

    assert found == ProjectInterpreter(path=str(real_python), source="override")
    assert found.pinned
