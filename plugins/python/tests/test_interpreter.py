"""Project-interpreter discovery (clarion-5cf9643de9)."""

from __future__ import annotations

import os
import shutil
import stat
import subprocess
from typing import TYPE_CHECKING

import pytest

from loomweave_plugin_python import interpreter as interpreter_module
from loomweave_plugin_python.interpreter import (
    INTERPRETER_OVERRIDE_ENV,
    ProjectInterpreter,
    discover_project_interpreter,
)

if TYPE_CHECKING:
    from pathlib import Path


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

    assert found == ProjectInterpreter(path=str(dotvenv), source="dotvenv")
    assert found.pinned


def test_override_env_wins_over_dotvenv(tmp_path: Path) -> None:
    _make_python(tmp_path / ".venv" / "bin" / "python")
    override = _make_python(tmp_path / "custom" / "python")

    found = discover_project_interpreter(tmp_path, {INTERPRETER_OVERRIDE_ENV: str(override)})

    assert found.source == "override"
    assert found.path == str(override)
    assert found.pinned


def test_unusable_override_is_ignored_and_discovery_continues(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    dotvenv = _make_python(tmp_path / ".venv" / "bin" / "python")
    missing = tmp_path / "nope" / "python"

    found = discover_project_interpreter(tmp_path, {INTERPRETER_OVERRIDE_ENV: str(missing)})

    assert found.source == "dotvenv"
    assert found.path == str(dotvenv)
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

    assert discover_project_interpreter(tmp_path).path == str(dotvenv)


def test_empty_environ_does_not_leak_to_os_environ(tmp_path: Path) -> None:
    # With an empty environ (no PATH), even if os.environ has a python on PATH,
    # the discovery should return "none" because the injected environ takes precedence.
    found = discover_project_interpreter(tmp_path, {})

    assert found == ProjectInterpreter(path=None, source="none")
    assert not found.pinned


def test_symlink_paths_are_preserved(tmp_path: Path) -> None:
    # Create a real interpreter at base/python3.12 and symlink .venv/bin/python to it.
    base_python = _make_python(tmp_path / "base" / "python3.12")
    venv_bin = tmp_path / ".venv" / "bin"
    venv_bin.mkdir(parents=True)
    symlink_python = venv_bin / "python"
    symlink_python.symlink_to(base_python)

    found = discover_project_interpreter(tmp_path, {})

    # The result should be the symlink path, not the target.
    assert found.path == str(symlink_python)
    assert found.source == "dotvenv"


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


def _git(root: Path, *args: str) -> None:
    # GIT_CONFIG_GLOBAL=/dev/null + GIT_CONFIG_NOSYSTEM=1 on top of the
    # author/committer identity, so a developer's own global git config
    # cannot alter these fixtures.
    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "t",
        "GIT_AUTHOR_EMAIL": "t@t",
        "GIT_COMMITTER_NAME": "t",
        "GIT_COMMITTER_EMAIL": "t@t",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
    }
    subprocess.run(  # noqa: S603 — fixture builder; argv comes from this test module
        ["git", *args],  # noqa: S607 — the fixture builder deliberately uses PATH's git
        cwd=root,
        env=env,
        check=True,
        capture_output=True,
    )


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
def test_a_repository_tracked_dotvenv_is_skipped_and_the_ladder_continues(
    tmp_path: Path, capsys: pytest.CaptureFixture[str], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(interpreter_module, "_tracked_dotvenv_warned", False)
    _git(tmp_path, "init", "-q")
    _make_python(tmp_path / ".venv" / "bin" / "python")
    _git(tmp_path, "add", "-f", ".venv/bin/python")
    _git(tmp_path, "commit", "-q", "-m", "hostile")
    venv = _make_python(tmp_path / "operator-venv" / "bin" / "python")

    chosen = discover_project_interpreter(
        tmp_path, {"VIRTUAL_ENV": str(tmp_path / "operator-venv")}
    )

    assert chosen.source == "virtual_env"
    assert chosen.path == str(venv)
    assert "skipped .venv/bin/python" in capsys.readouterr().err


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
def test_the_tracked_dotvenv_warning_is_logged_once_per_process(
    tmp_path: Path, capsys: pytest.CaptureFixture[str], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(interpreter_module, "_tracked_dotvenv_warned", False)
    _git(tmp_path, "init", "-q")
    _make_python(tmp_path / ".venv" / "bin" / "python")
    _git(tmp_path, "add", "-f", ".venv/bin/python")
    _git(tmp_path, "commit", "-q", "-m", "hostile")
    _make_python(tmp_path / "operator-venv" / "bin" / "python")
    env = {"VIRTUAL_ENV": str(tmp_path / "operator-venv")}

    discover_project_interpreter(tmp_path, env)
    capsys.readouterr()  # drain the first call's warning
    discover_project_interpreter(tmp_path, env)

    assert capsys.readouterr().err == ""


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
def test_an_untracked_dotvenv_in_a_repository_is_still_chosen(tmp_path: Path) -> None:
    _git(tmp_path, "init", "-q")
    venv = _make_python(tmp_path / ".venv" / "bin" / "python")

    chosen = discover_project_interpreter(tmp_path, {})

    assert chosen.source == "dotvenv"
    assert chosen.path == str(venv)


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
def test_a_tracked_symlink_dotvenv_is_skipped(tmp_path: Path) -> None:
    _git(tmp_path, "init", "-q")
    _make_python(tmp_path / "payload" / "bin" / "python")
    (tmp_path / ".venv").symlink_to(tmp_path / "payload")
    _git(tmp_path, "add", "-f", ".venv")
    _git(tmp_path, "commit", "-q", "-m", "hostile")

    assert discover_project_interpreter(tmp_path, {}).source != "dotvenv"


def test_outside_a_repository_the_dotvenv_rung_is_unchanged(tmp_path: Path) -> None:
    venv = _make_python(tmp_path / ".venv" / "bin" / "python")

    chosen = discover_project_interpreter(tmp_path, {})

    assert chosen.source == "dotvenv"
    assert chosen.path == str(venv)
