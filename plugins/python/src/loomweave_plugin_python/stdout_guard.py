"""Stdout discipline for JSON-RPC plugins (WP2 UQ-WP2-08 plugin-side resolution).

The Loomweave plugin protocol reserves ``stdout`` for Content-Length-framed
JSON-RPC frames. A stray ``print()`` or library-emitted message on stdout
would corrupt the framing parser on the host side and trip either the
Content-Length ceiling or the JSON decoder.

``install_stdio()`` captures the real ``stdin``/``stdout`` byte streams,
replaces ``sys.stdout`` with a guard that raises ``StdoutGuardError`` on
any write, and returns the captured ``(stdin, stdout)`` pair for the
server to use. Callers must invoke this exactly once, before reading or
writing any framed data.
"""

from __future__ import annotations

import sys
from typing import IO


class StdoutGuardError(RuntimeError):
    """Raised when Python code writes to the guarded stdout after ``install_stdio``."""


class _GuardedTextStdout:
    """``sys.stdout`` replacement that refuses every write.

    Only implements the attributes and methods CPython routinely looks up
    on ``sys.stdout`` — enough to surface the guard error clearly instead of
    failing with ``AttributeError`` first.
    """

    encoding = "utf-8"
    errors = "strict"

    def write(self, _data: str) -> int:
        msg = (
            "plugin stdout is reserved for JSON-RPC framing; "
            "write to sys.stderr for diagnostics or raise an exception"
        )
        raise StdoutGuardError(msg)

    def writelines(self, _lines: object) -> None:
        self.write("")

    def flush(self) -> None:
        return

    def isatty(self) -> bool:
        return False

    def fileno(self) -> int:
        msg = "guarded stdout has no fileno"
        raise StdoutGuardError(msg)


def install_stdio() -> tuple[IO[bytes], IO[bytes]]:
    """Reserve stdout for JSON-RPC; return the real ``(stdin, stdout)`` byte streams."""
    real_stdin = sys.stdin.buffer
    real_stdout = sys.stdout.buffer
    sys.stdout = _GuardedTextStdout()
    return real_stdin, real_stdout
