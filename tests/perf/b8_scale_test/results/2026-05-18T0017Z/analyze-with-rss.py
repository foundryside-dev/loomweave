#!/usr/bin/env python3
"""Run loomweave analyze and sample peak RSS for B.8 measurement.

Mirrors the analyze-metrics.json schema from the 2026-05-17 B.8 run.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path


def _rss_bytes_for_tree(root_pid: int) -> int:
    """Sum VmRSS across the process and its descendants."""
    pids = [root_pid]
    try:
        with open(f"/proc/{root_pid}/task/{root_pid}/children", "r") as fh:
            for child_pid in fh.read().split():
                pids.append(int(child_pid))
    except FileNotFoundError:
        pass
    # one more level for plugin grandchildren (pyright etc.)
    expanded = list(pids)
    for pid in pids[1:]:
        try:
            with open(f"/proc/{pid}/task/{pid}/children", "r") as fh:
                for child_pid in fh.read().split():
                    expanded.append(int(child_pid))
        except FileNotFoundError:
            pass
    total = 0
    for pid in expanded:
        try:
            with open(f"/proc/{pid}/status", "r") as fh:
                for line in fh:
                    if line.startswith("VmRSS:"):
                        kib = int(line.split()[1])
                        total += kib * 1024
                        break
        except FileNotFoundError:
            continue
    return total


def main() -> int:
    if len(sys.argv) < 3:
        print("usage: analyze-with-rss.py <output-json> <loomweave-bin> [args...]")
        return 2
    output = Path(sys.argv[1])
    cmd = sys.argv[2:]

    env = os.environ.copy()
    plugin_bin_dir = "/home/john/loomweave/plugins/python/.venv/bin"
    env["PATH"] = plugin_bin_dir + ":" + env.get("PATH", "")

    start = time.monotonic()
    proc = subprocess.Popen(cmd, env=env)
    samples: list[dict[str, float | int]] = []
    peak = 0
    try:
        while True:
            ret = proc.poll()
            now = time.monotonic() - start
            rss = _rss_bytes_for_tree(proc.pid)
            samples.append({"t": round(now, 3), "rss_bytes": rss})
            if rss > peak:
                peak = rss
            if ret is not None:
                break
            time.sleep(0.25)
    except KeyboardInterrupt:
        proc.terminate()
        raise
    wall = time.monotonic() - start
    rc = proc.returncode

    payload = {
        "command": cmd,
        "peak_rss_bytes": peak,
        "peak_rss_mb": round(peak / (1024 * 1024), 3),
        "returncode": rc,
        "sample_count": len(samples),
        "samples_tail": samples[-20:],
        "wall_seconds": round(wall, 3),
    }
    output.write_text(json.dumps(payload, indent=2, sort_keys=True), encoding="utf-8")
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
