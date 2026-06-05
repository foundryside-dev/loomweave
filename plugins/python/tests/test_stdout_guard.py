"""WP2 UQ-WP2-08 plugin-side enforcement: stdout is reserved for JSON-RPC.

The guard is tested via subprocess because ``install_stdio()`` mutates
``sys.stdout`` in the running interpreter, which would break pytest's own
output capture if run in-process.
"""

from __future__ import annotations

import subprocess
import sys


def test_install_stdio_blocks_print() -> None:
    """After install_stdio(), a Python `print()` raises StdoutGuardError."""
    code = (
        "from loomweave_plugin_python.stdout_guard import install_stdio, StdoutGuardError\n"
        "import sys\n"
        "install_stdio()\n"
        "try:\n"
        "    print('should not reach host')\n"
        "except StdoutGuardError:\n"
        "    sys.stderr.write('guard-fired\\n')\n"
        "    sys.exit(42)\n"
        "sys.exit(0)\n"
    )
    proc = subprocess.run(  # noqa: S603
        [sys.executable, "-c", code],
        check=False,
        capture_output=True,
        timeout=5,
    )
    assert proc.returncode == 42, (
        f"expected guard-fired exit 42, got {proc.returncode}; stderr={proc.stderr!r}"
    )
    assert b"guard-fired" in proc.stderr


def test_install_stdio_returns_real_streams() -> None:
    """install_stdio() yields usable stdin/stdout bytes streams."""
    code = (
        "from loomweave_plugin_python.stdout_guard import install_stdio\n"
        "stdin, stdout = install_stdio()\n"
        "stdout.write(b'raw-bytes-out')\n"
        "stdout.flush()\n"
    )
    proc = subprocess.run(  # noqa: S603
        [sys.executable, "-c", code],
        check=False,
        capture_output=True,
        timeout=5,
    )
    assert proc.returncode == 0, f"stderr={proc.stderr!r}"
    assert proc.stdout == b"raw-bytes-out"
