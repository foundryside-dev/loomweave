"""Entry point for the ``loomweave-plugin-python`` executable.

Installs stdout discipline (``stdout_guard``) and hands control to the
JSON-RPC server loop. ``sys.exit`` threads the server's exit code out to
the host process.
"""

from __future__ import annotations

import sys

from loomweave_plugin_python.server import main

if __name__ == "__main__":
    sys.exit(main())
