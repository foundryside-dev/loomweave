"""Cross-language conformance for git_trust.tracked_state (fixtures/git_tracked_paths.json).

The Rust twin is crates/loomweave-core/tests/git_tracked_paths.rs.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

import pytest

from loomweave_plugin_python.git_trust import tracked_state, treat_as_tracked

# Repo root is four parents up: plugins/python/tests/test_git_trust.py → /repo
_REPO_ROOT = Path(__file__).resolve().parents[3]
_FIXTURE = _REPO_ROOT / "fixtures" / "git_tracked_paths.json"
_GIT_ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@t",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@t",
}


def _git(root: Path, *args: str) -> None:
    subprocess.run(  # noqa: S603 — fixture builder; argv comes from the checked-in vectors
        ["git", *args],  # noqa: S607 — the fixture builder deliberately uses PATH's git
        cwd=root,
        env=_GIT_ENV,
        check=True,
        capture_output=True,
    )


def _build(root: Path, case: dict[str, Any]) -> None:
    for entry in case.get("layout", []):
        if "file" in entry:
            path = root / str(entry["file"])
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(str(entry.get("content", "#!/bin/sh\nexit 0\n")))
            path.chmod(int(str(entry.get("mode", "0644")), 8))
        elif "dir" in entry:
            (root / str(entry["dir"])).mkdir(parents=True, exist_ok=True)
        elif "symlink" in entry:
            path = root / str(entry["symlink"])
            path.parent.mkdir(parents=True, exist_ok=True)
            target = Path(str(entry["target"]))
            path.symlink_to(target if target.is_absolute() else root / target)
    for step in case.get("git", []):
        if step == "init":
            _git(root, "init", "-q")
        elif step == "commit":
            _git(root, "commit", "-q", "--allow-empty", "-m", "fixture")
        else:
            _git(root, *str(step).split(" "))


@pytest.mark.skipif(shutil.which("git") is None, reason="git unavailable")
@pytest.mark.parametrize(
    "case",
    json.loads(_FIXTURE.read_text()),
    ids=lambda c: str(c["description"]),
)
def test_tracked_state_matches_the_shared_conformance_vectors(
    tmp_path: Path, case: dict[str, Any]
) -> None:
    _build(tmp_path, case)
    state = tracked_state(tmp_path, Path(str(case["query"])))
    assert state == case["expected"], case["description"]
    assert state != "unknown"


def test_treat_as_tracked_fails_closed() -> None:
    assert treat_as_tracked("tracked")
    assert treat_as_tracked("unknown")
    assert not treat_as_tracked("untracked")
    assert not treat_as_tracked("not_a_git_work_tree")


def test_hung_git_reports_unknown(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    fake = tmp_path / "bin"
    fake.mkdir()
    (fake / "git").write_text("#!/bin/sh\nsleep 30\n")
    (fake / "git").chmod(0o755)
    # Prepend, never replace: the stub shadows the real git but still needs
    # `sleep` from the operator's PATH, or it exits instantly and the test
    # would pass for the wrong reason.
    monkeypatch.setenv("PATH", f"{fake}{os.pathsep}{os.environ['PATH']}")
    repo = tmp_path / "repo"
    repo.mkdir()
    started = time.monotonic()
    assert tracked_state(repo, Path("x"), timeout_seconds=0.3) == "unknown"
    elapsed = time.monotonic() - started
    assert elapsed >= 0.3, f"returned in {elapsed:.3f}s — the deadline was not what fired"
    assert elapsed < 10.0, f"took {elapsed:.3f}s — the hung child was not killed at the deadline"
