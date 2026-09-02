"""Is a path repository content? (ADR-063)

CROSS-LANGUAGE CONTRACT with ``crates/loomweave-core/src/hardened_git.rs``
``tracked_state``: same pathspec construction (the path, its ancestors, and —
when it resolves inside the repository — the canonical target and its
ancestors), same tri-state, same fail-closed reading. Conformance vectors:
``fixtures/git_tracked_paths.json``. Change both or neither.

The git invocation mirrors the Rust hardened builder: cleared environment plus
``PATH``, pinned ``C`` locale, operator/system config nulled, optional locks
off, ``core.fsmonitor``/``core.untrackedCache`` forced off. ``ls-files`` never
hashes working-tree content, so no repo-controlled filter can run.
"""

from __future__ import annotations

import contextlib
import os
import select
import subprocess
import time
from typing import TYPE_CHECKING, Literal

if TYPE_CHECKING:
    from pathlib import Path

TrackedState = Literal["tracked", "untracked", "not_a_git_work_tree", "unknown"]

_STDERR_TAIL = 64 * 1024
_READ_CHUNK = 8192
_POLL_INTERVAL = 0.25
_REAP_TIMEOUT = 5.0
_NOT_A_REPOSITORY_EXIT = 128


def treat_as_tracked(state: TrackedState) -> bool:
    """Fail-closed reading for trust decisions."""
    return state in ("tracked", "unknown")


def _hardened_env() -> dict[str, str]:
    env = {
        "PATH": os.environ.get("PATH", ""),
        "LC_ALL": "C",
        "LANG": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_ATTR_NOSYSTEM": "1",
    }
    ceiling = os.environ.get("GIT_CEILING_DIRECTORIES")
    if ceiling:
        env["GIT_CEILING_DIRECTORIES"] = ceiling
    return env


def _self_and_ancestors(path: Path, root: Path, specs: list[str]) -> None:
    try:
        rel = path.relative_to(root)
    except ValueError:
        return
    while str(rel) not in ("", "."):
        if str(rel) not in specs:
            specs.append(str(rel))
        rel = rel.parent


def _pathspecs(repo_root: Path, path: Path) -> list[str]:
    absolute = path if path.is_absolute() else repo_root / path
    specs: list[str] = []
    _self_and_ancestors(absolute, repo_root, specs)
    with contextlib.suppress(OSError):
        _self_and_ancestors(absolute.resolve(strict=True), repo_root.resolve(strict=True), specs)
    return specs


def _reap(proc: subprocess.Popen[bytes]) -> None:
    if proc.poll() is None:
        proc.kill()
        with contextlib.suppress(subprocess.TimeoutExpired):
            proc.wait(timeout=_REAP_TIMEOUT)


def tracked_state(repo_root: Path, path: Path, *, timeout_seconds: float = 30.0) -> TrackedState:
    """Whether ``path`` is repository content under ``repo_root``.

    ``path`` may be absolute or relative to ``repo_root``. Tracked means the
    path, any ancestor of it, or — when it resolves inside the repository — its
    canonical target or any ancestor of that has an entry in the git index.
    """
    specs = _pathspecs(repo_root, path)
    if not specs:
        # Entirely outside the repository (and not resolving into it).
        return "untracked"
    argv = [
        "git",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
        # The specs are paths, not patterns: a component containing `*`, `?`,
        # `[…]` or a leading `:` must not be read as a glob or pathspec magic.
        "--literal-pathspecs",
        "-C",
        str(repo_root),
        "ls-files",
        "-z",
        "--",
        *specs,
    ]
    try:
        proc = subprocess.Popen(  # noqa: S603 — fixed argv; the specs are repo-relative paths
            argv,
            env=_hardened_env(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError:
        return "unknown"
    return _classify(proc, time.monotonic() + timeout_seconds)


def _classify(proc: subprocess.Popen[bytes], deadline: float) -> TrackedState:
    """Drain the bounded probe and map its outcome onto the tri-state."""
    stdout_pipe, stderr_pipe = proc.stdout, proc.stderr
    if stdout_pipe is None or stderr_pipe is None:  # pragma: no cover — both are PIPEs
        _reap(proc)
        return "unknown"
    stdout_seen = False
    stderr_tail = bytearray()
    open_fds = {stdout_pipe.fileno(): "out", stderr_pipe.fileno(): "err"}
    try:
        while open_fds and not stdout_seen:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return "unknown"
            ready, _, _ = select.select(list(open_fds), [], [], min(remaining, _POLL_INTERVAL))
            for fd in ready:
                chunk = os.read(fd, _READ_CHUNK)
                if not chunk:
                    del open_fds[fd]
                elif open_fds[fd] == "out":
                    stdout_seen = True  # any output ⇒ tracked; stop draining
                    break
                else:
                    stderr_tail += chunk
                    del stderr_tail[:-_STDERR_TAIL]
        if stdout_seen:
            # Before the exit code: closing stdout early can make git die of
            # EPIPE, and a tracked path must not be reported as unknown.
            return "tracked"
        try:
            code = proc.wait(timeout=max(0.0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            return "unknown"
    finally:
        stdout_pipe.close()
        stderr_pipe.close()
        _reap(proc)
    if code == 0:
        return "untracked"
    if code == _NOT_A_REPOSITORY_EXIT and b"not a git repository" in bytes(stderr_tail):
        return "not_a_git_work_tree"
    return "unknown"
