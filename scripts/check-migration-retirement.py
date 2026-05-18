#!/usr/bin/env python3
"""Guard ADR-024's in-place migration retirement trigger.

Before the first external published build, Clarion may edit migration 0001 in
place. After that trigger fires, add:

    crates/clarion-storage/migrations/published_build.txt

with the git ref of the first published build whose 0001 migration must stay
stable. Once the marker exists, this guard fails if the working tree's 0001
differs from that ref; later schema changes must be additive 0002+ migrations.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path

MIGRATION_PATH = Path("crates/clarion-storage/migrations/0001_initial_schema.sql")
MARKER_PATH = Path("crates/clarion-storage/migrations/published_build.txt")


class CheckError(Exception):
    """Raised for guard failures."""


def git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=root,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def git_ok(root: Path, *args: str) -> None:
    proc = git(root, *args)
    if proc.returncode != 0:
        raise CheckError(
            f"git {' '.join(args)} failed:\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )


def first_marker_ref(root: Path, marker_path: Path = MARKER_PATH) -> str | None:
    marker = root / marker_path
    if not marker.exists():
        return None
    for raw_line in marker.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line and not line.startswith("#"):
            return line
    raise CheckError(f"{marker_path} exists but does not name a git ref")


def migration_text_at_ref(root: Path, ref: str, migration_path: Path = MIGRATION_PATH) -> str:
    proc = git(root, "show", f"{ref}:{migration_path.as_posix()}")
    if proc.returncode != 0:
        raise CheckError(
            f"{MARKER_PATH} names {ref!r}, but git cannot read "
            f"{migration_path} at that ref:\n{proc.stderr}"
        )
    return proc.stdout


def check(root: Path) -> None:
    ref = first_marker_ref(root)
    if ref is None:
        print(f"{MARKER_PATH} absent; ADR-024 in-place migration policy is still pre-trigger.")
        return

    migration = root / MIGRATION_PATH
    if not migration.exists():
        raise CheckError(f"{MIGRATION_PATH} is missing")

    current = migration.read_text(encoding="utf-8")
    published = migration_text_at_ref(root, ref)
    if current != published:
        raise CheckError(
            f"{MIGRATION_PATH} differs from published marker {ref!r}. "
            "ADR-024 has retired in-place edits; add a 0002+ migration instead."
        )
    print(f"{MIGRATION_PATH} matches published marker {ref!r}.")


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def run_self_test() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        git_ok(root, "init", "-q")
        git_ok(root, "config", "user.email", "clarion-test@example.invalid")
        git_ok(root, "config", "user.name", "Clarion Test")

        write(root / MIGRATION_PATH, "initial migration\n")
        git_ok(root, "add", MIGRATION_PATH.as_posix())
        git_ok(root, "commit", "-q", "-m", "initial migration")
        git_ok(root, "tag", "published-build")

        check(root)

        write(root / MARKER_PATH, "published-build\n")
        check(root)

        write(root / MIGRATION_PATH, "edited migration\n")
        try:
            check(root)
        except CheckError as exc:
            if "0002+" not in str(exc):
                raise
        else:
            raise CheckError("self-test expected edited 0001 to fail after marker")

    print("migration retirement guard self-test passed")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true", help="run built-in guard tests")
    args = parser.parse_args(argv)

    try:
        if args.self_test:
            run_self_test()
        else:
            check(Path.cwd())
    except CheckError as exc:
        print(f"migration retirement guard failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
