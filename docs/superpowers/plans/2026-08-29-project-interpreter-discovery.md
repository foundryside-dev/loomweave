# Project Interpreter Discovery + Resolver-Environment Marker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Python plugin's call/reference resolution independent of *who launched* `loomweave analyze`: pin pyright to the project's own interpreter, say so honestly when no project interpreter exists, invalidate the incremental skip when the resolver environment changes, and let `doctor --fix` reap `runs` rows abandoned by dead builders.

**Architecture:** Discovery of the project interpreter is implemented twice with an identical, tabulated order — once in Rust (`loomweave-core`, authoritative under `analyze`: it exports the choice to the plugin via `LOOMWEAVE_PYTHON_INTERPRETER` and derives the marker fingerprint before the incremental partition) and once in Python (the plugin's fallback when driven outside the host, and the source of the honesty claim). The plugin passes the chosen path to pyright as `python.pythonPath` in its `workspace/configuration` reply. `plugin_index_meta` gains a nullable `resolver_environment` column; a change in it forces a full re-dispatch of that plugin's files exactly like a version/ontology bump. `doctor` gains an `index.runs` check that, holding the analyze lock, marks every `running` row failed.

**Tech Stack:** Rust 2024 (`nix`, `rusqlite`, `serde`), Python 3.11+ (`pyright-langserver` over LSP/JSON-RPC), SQLite migrations, pytest, cargo-nextest.

**Spec:** Filigree ticket `clarion-5cf9643de9` (description + bisect table) is the binding spec. Controller rulings that refine it (recorded in the SDD ledger):
- R1: honesty token is `interpreter_unpinned`, `transient=false`, `collateral=false` — a re-run with the *same* interpreter cannot recover, so it must not spend the re-dispatch budget; healing comes from the marker (R3).
- R2: the operator override is the env var `LOOMWEAVE_PYTHON_INTERPRETER` (honoured by host and plugin); no `loomweave.yaml` key in this plan.
- R3: the interpreter fingerprint rides `plugin_index_meta.resolver_environment`; first run after upgrade sees `NULL != Some(..)` and re-dispatches once (this heals elspeth's 400 pinned rows without `--no-incremental`).
- R4: `doctor --fix` reaps `running` rows using the existing `mark_abandoned_running_runs_failed` while holding the analyze lock (the lock IS the liveness proof); no pid probing.
- R5: suggestion 3 (explicit `extraPaths`) and suggestion 4 (hook PATH) are NOT implemented — the plugin now finds the venv itself.

## Global Constraints

- CI floor (ADR-023) must be green before any task is called done: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo build --workspace --bins`, `cargo nextest run --workspace --all-features`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`, `cargo deny check`, `plugins/python/.venv/bin/ruff check plugins/python`, `plugins/python/.venv/bin/ruff format --check plugins/python`, `plugins/python/.venv/bin/mypy --strict plugins/python`, `plugins/python/.venv/bin/pytest plugins/python`. Run `cargo build --workspace --bins` BEFORE nextest (CLI integration tests exec the built binaries).
- `unsafe_code = "deny"` workspace-wide; clippy `pedantic`. No new `unsafe`.
- **Discovery order is a cross-language contract.** Both implementations MUST resolve in exactly this order and stop at the first hit; a hit means "the path exists, is a regular file, and has an execute bit"; every returned path is canonicalised (`realpath` / `fs::canonicalize`):

  | # | source token | candidate |
  |---|---|---|
  | 1 | `override` | `$LOOMWEAVE_PYTHON_INTERPRETER` (as given) |
  | 2 | `dotvenv` | `<project_root>/.venv/bin/python` |
  | 3 | `virtual_env` | `$VIRTUAL_ENV/bin/python` |
  | 4 | `conda` | `$CONDA_PREFIX/bin/python` |
  | 5 | `path` | first of `python`, `python3` found on `$PATH` |
  | 6 | `none` | nothing found; path is absent |

  Sources 1–4 are **pinned** (project-owned); 5–6 are **unpinned**. An override that is set but does not pass the hit test is ignored with one stderr warning line and discovery continues at 2.
- **Fingerprint format** (Rust only, stored in the marker): pinned → the canonical path string; `path` → `unpinned:<canonical path>`; `none` → `unpinned:none`. Plugins whose manifest lacks `[capabilities.runtime.pyright]` get no fingerprint (`None`).
- Coverage reason vocabulary gains exactly one token: `interpreter_unpinned` (`transient=false`, `collateral=false`). It is applied ONLY when a facet would otherwise be `complete`; a degraded facet keeps its real reason.
- Every new operational constant or env override carries an ADR-035 four-axis declaration (basis / override surface / retune trigger / coupling) in a comment at its definition.
- Version lockstep: do NOT bump plugin/workspace versions. New migration must be registered in `schema.rs` `MIGRATIONS` and `CURRENT_SCHEMA_VERSION` bumped to 15; run `python3 scripts/check-migration-retirement.py` afterwards.
- Merge target is `release/1.5.0` (never literal `main`); work on a feature branch `fix/clarion-5cf9643de9-project-interpreter`.

---

### Task 1: Python plugin — `interpreter.py` discovery module

**Files:**
- Create: `plugins/python/src/loomweave_plugin_python/interpreter.py`
- Test: `plugins/python/tests/test_interpreter.py`

**Interfaces:**
- Produces:
  ```python
  INTERPRETER_OVERRIDE_ENV: Final = "LOOMWEAVE_PYTHON_INTERPRETER"
  InterpreterSource = Literal["override", "dotvenv", "virtual_env", "conda", "path", "none"]
  @dataclass(frozen=True)
  class ProjectInterpreter:
      path: str | None
      source: InterpreterSource
      @property
      def pinned(self) -> bool: ...   # source in {"override","dotvenv","virtual_env","conda"}
  def discover_project_interpreter(project_root: Path, environ: Mapping[str, str] | None = None) -> ProjectInterpreter
  ```

- [ ] **Step 1: Write the failing tests**

```python
"""Project-interpreter discovery (clarion-5cf9643de9)."""

from __future__ import annotations

import os
import stat
from pathlib import Path

import pytest

from loomweave_plugin_python.interpreter import (
    INTERPRETER_OVERRIDE_ENV,
    ProjectInterpreter,
    discover_project_interpreter,
)


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
        {"VIRTUAL_ENV": str(venv.parent.parent), "CONDA_PREFIX": str(conda.parent.parent), "PATH": str(on_path.parent)},
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_interpreter.py -q`
Expected: FAIL — `ModuleNotFoundError: loomweave_plugin_python.interpreter`

- [ ] **Step 3: Implement the module**

```python
"""Project-interpreter discovery for pyright (clarion-5cf9643de9).

``pyright-langserver`` decides which Python environment to type-check against
by running whatever ``python`` is first on its ``PATH`` unless the client sets
``python.pythonPath``. Under ``loomweave analyze`` launched from an agent hook
that ``python`` is the system interpreter, which cannot import the project's
editable install, and every ``tests/`` -> ``src/`` call target came back empty
while the coverage claim still said ``complete``. This module picks the
project's own interpreter deterministically so the answer no longer depends
on who launched the run.

The order below is a CROSS-LANGUAGE CONTRACT with
``crates/loomweave-core/src/plugin/interpreter.rs`` (the host runs the same
discovery, exports the winner as ``LOOMWEAVE_PYTHON_INTERPRETER``, and keys the
incremental skip on it). Change both or neither.
"""

from __future__ import annotations

import os
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Final, Literal

if TYPE_CHECKING:
    from collections.abc import Mapping

# Operator/host override for the interpreter pyright resolves against.
# Basis: the host's discovery is authoritative under ``analyze`` and must reach
#   the plugin; operators on venv-less layouts need a pin.
# Override surface: this env var IS the override surface (set by
#   ``PluginHost::spawn_unhandshaken`` or by the operator).
# Retune trigger: none — a path, not a tunable.
# Coupling: ``loomweave_core::plugin::interpreter::PYTHON_INTERPRETER_ENV``
#   must carry the same literal.
INTERPRETER_OVERRIDE_ENV: Final = "LOOMWEAVE_PYTHON_INTERPRETER"

InterpreterSource = Literal["override", "dotvenv", "virtual_env", "conda", "path", "none"]

_PINNED_SOURCES: Final[frozenset[str]] = frozenset({"override", "dotvenv", "virtual_env", "conda"})


@dataclass(frozen=True)
class ProjectInterpreter:
    """The interpreter pyright will be pointed at, and where it came from."""

    path: str | None
    source: InterpreterSource

    @property
    def pinned(self) -> bool:
        """True when the interpreter is project-owned (not a PATH guess)."""
        return self.source in _PINNED_SOURCES


def _usable(candidate: Path) -> Path | None:
    if candidate.is_file() and os.access(candidate, os.X_OK):
        return candidate.resolve()
    return None


def discover_project_interpreter(
    project_root: Path,
    environ: Mapping[str, str] | None = None,
) -> ProjectInterpreter:
    """Resolve the project's interpreter in the contract order (see module doc)."""
    env = os.environ if environ is None else environ
    override = env.get(INTERPRETER_OVERRIDE_ENV)
    if override:
        if (hit := _usable(Path(override))) is not None:
            return ProjectInterpreter(path=str(hit), source="override")
        sys.stderr.write(
            f"loomweave-plugin-python: {INTERPRETER_OVERRIDE_ENV}={override!r} is not an "
            "executable file; ignoring the override and discovering the interpreter\n",
        )
    if (hit := _usable(Path(project_root) / ".venv" / "bin" / "python")) is not None:
        return ProjectInterpreter(path=str(hit), source="dotvenv")
    for var, source in (("VIRTUAL_ENV", "virtual_env"), ("CONDA_PREFIX", "conda")):
        prefix = env.get(var)
        if prefix and (hit := _usable(Path(prefix) / "bin" / "python")) is not None:
            return ProjectInterpreter(path=str(hit), source=source)  # type: ignore[arg-type]
    for name in ("python", "python3"):
        found = shutil.which(name, path=env.get("PATH"))
        if found is not None and (hit := _usable(Path(found))) is not None:
            return ProjectInterpreter(path=str(hit), source="path")
    return ProjectInterpreter(path=None, source="none")
```

Remove the `# type: ignore` by typing the tuple as `tuple[tuple[str, InterpreterSource], ...]` if mypy --strict complains either way; the final code must pass `mypy --strict` and `ruff` with no ignores.

- [ ] **Step 4: Run tests to verify they pass**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_interpreter.py -q`
Expected: 8 passed

- [ ] **Step 5: Lint + type-check, then commit**

Run: `plugins/python/.venv/bin/ruff check plugins/python && plugins/python/.venv/bin/ruff format plugins/python && plugins/python/.venv/bin/mypy --strict plugins/python`

```bash
git add plugins/python/src/loomweave_plugin_python/interpreter.py plugins/python/tests/test_interpreter.py
git commit -m "feat(plugin-python): discover the project interpreter in a fixed cross-language order (clarion-5cf9643de9)"
```

---

### Task 2: Python plugin — pin pyright to the interpreter and claim `interpreter_unpinned` honestly

**Files:**
- Modify: `plugins/python/src/loomweave_plugin_python/pyright_session.py` (module docstring vocabulary; `__init__`; `_configuration_for_section`; calls-pass and references-pass success paths; `_spawn_and_initialize`)
- Modify: `plugins/python/src/loomweave_plugin_python/server.py` (`ServerState`, `handle_initialize`, `handle_analyze_file`)
- Test: `plugins/python/tests/test_pyright_session.py`, `plugins/python/tests/test_server.py`

**Interfaces:**
- Consumes: Task 1's `ProjectInterpreter`, `discover_project_interpreter`.
- Produces: `PyrightSession(project_root, *, interpreter: ProjectInterpreter | None = None, ...)` — `None` means "discover from `project_root` + `os.environ` at construction"; attribute `self.interpreter: ProjectInterpreter`. `ServerState.interpreter: ProjectInterpreter | None`. `initialize` response gains `capabilities.python_interpreter = {"path": str|None, "source": str, "pinned": bool}`.

- [ ] **Step 1: Write the failing tests**

Append to `plugins/python/tests/test_pyright_session.py` (reuse the script pattern of `test_pyright_session_answers_workspace_configuration_requests`; copy its `read_frame`/`write_frame` helpers verbatim into the new script):

```python
def test_workspace_configuration_carries_python_path_when_pinned(tmp_path: Path) -> None:
    marker = tmp_path / "config-marker.txt"
    fake_python = tmp_path / ".venv" / "bin" / "python"
    fake_python.parent.mkdir(parents=True)
    fake_python.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    fake_python.chmod(0o755)
    script = _write_executable(
        tmp_path,
        textwrap.dedent(
            """
            #!/usr/bin/env python3
            import json, os, sys
            from pathlib import Path

            def read_frame():
                headers = {}
                while True:
                    line = sys.stdin.buffer.readline()
                    if not line:
                        return None
                    if line == b"\\r\\n":
                        break
                    name, value = line.decode("ascii").strip().split(":", 1)
                    headers[name.lower()] = value.strip()
                return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

            def write_frame(message):
                body = json.dumps(message).encode("utf-8")
                sys.stdout.buffer.write(
                    b"Content-Length: " + str(len(body)).encode("ascii") + b"\\r\\n\\r\\n"
                )
                sys.stdout.buffer.write(body)
                sys.stdout.buffer.flush()

            initialize = read_frame()
            write_frame({"jsonrpc": "2.0", "id": 0, "method": "workspace/configuration",
                         "params": {"items": [{"section": "python"}]}})
            config = read_frame()
            python = config.get("result", [{}])[0]
            Path(os.environ["CONFIG_MARKER"]).write_text(json.dumps(python))
            write_frame({"jsonrpc": "2.0", "id": initialize["id"], "result": {}})
            while True:
                frame = read_frame()
                if frame is None:
                    break
                method = frame.get("method")
                if method == "textDocument/prepareCallHierarchy":
                    write_frame({"jsonrpc": "2.0", "id": frame["id"], "result": []})
                elif method == "shutdown":
                    write_frame({"jsonrpc": "2.0", "id": frame["id"], "result": {}})
                elif method == "exit":
                    break
            """,
        ).lstrip(),
    )
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")

    with PyrightSession(
        tmp_path,
        executable=str(script),
        env={"CONFIG_MARKER": str(marker), "LOOMWEAVE_PYTHON_INTERPRETER": ""},
        init_timeout_secs=1.0,
    ) as session:
        assert session.interpreter.source == "dotvenv"
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    python_section = json.loads(marker.read_text())
    assert python_section["pythonPath"] == str(fake_python.resolve())
    assert python_section["analysis"]["diagnosticMode"] == "openFilesOnly"
    assert result.coverage.status == "complete", "a pinned interpreter never degrades coverage"


def test_unpinned_interpreter_degrades_an_otherwise_complete_facet(tmp_path: Path) -> None:
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")
    unpinned = ProjectInterpreter(path="/usr/bin/python3", source="path")

    session = RestartProbeSession(tmp_path, interpreter=unpinned)
    try:
        calls = session.resolve_calls(module, ["python:function:demo.caller"])
        references = session.resolve_references(module, [])
    finally:
        session.close()

    for coverage in (calls.coverage, references.coverage):
        assert coverage.status == "degraded"
        assert coverage.reason == "interpreter_unpinned"
        assert coverage.transient is False
        assert coverage.collateral is False


def test_unpinned_interpreter_never_masks_a_real_degradation(tmp_path: Path) -> None:
    module = _write_module(tmp_path, "def caller():\n    print('x')\n", name="slow.py")
    unpinned = ProjectInterpreter(path=None, source="none")

    session = RestartProbeSession(
        tmp_path, interpreter=unpinned, timeout_stems={"slow"}, call_timeout_secs=0.01
    )
    try:
        calls = session.resolve_calls(module, ["python:function:slow.caller"])
    finally:
        session.close()

    assert calls.coverage.reason == "pyright_timeout", calls.coverage
```

Check how `RestartProbeSession` and `timeout_stems` produce a `pyright_timeout` in the existing tests (grep `timeout_stems=` in the test file) and adapt the third test to that exact recipe. Import `ProjectInterpreter` from `loomweave_plugin_python.interpreter` at the top of the test file.

Append to `plugins/python/tests/test_server.py`:

```python
def test_initialize_discovers_and_advertises_the_project_interpreter(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake_python = tmp_path / ".venv" / "bin" / "python"
    fake_python.parent.mkdir(parents=True)
    fake_python.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    fake_python.chmod(0o755)
    monkeypatch.delenv("LOOMWEAVE_PYTHON_INTERPRETER", raising=False)
    state = server_module.ServerState()

    response = server_module.handle_initialize(
        {"protocol_version": "1.0", "project_root": str(tmp_path)}, state
    )

    assert response["capabilities"]["python_interpreter"] == {
        "path": str(fake_python.resolve()),
        "source": "dotvenv",
        "pinned": True,
    }
    assert state.interpreter is not None and state.interpreter.pinned


def test_analyze_file_hands_the_discovered_interpreter_to_pyright(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    captured: dict[str, Any] = {}

    class FakePyrightSession:
        def __init__(self, project_root: Path, **kwargs: Any) -> None:
            captured.update(kwargs)
            self.project_root = project_root

        def resolve_calls(self, file_path: str, function_ids: list[str]) -> CallResolutionResult:
            _ = (file_path, function_ids)
            return CallResolutionResult()

        def resolve_references(
            self, file_path: str, sites: Sequence[ReferenceSite]
        ) -> ReferenceResolutionResult:
            _ = (file_path, sites)
            return ReferenceResolutionResult()

        def close(self) -> None:
            pass

    monkeypatch.setattr(server_module, "PyrightSession", FakePyrightSession, raising=False)
    demo = tmp_path / "demo.py"
    demo.write_text("def hello():\n    pass\n", encoding="utf-8")
    interpreter = ProjectInterpreter(path="/x/python", source="override")
    state = server_module.ServerState(initialized=True, project_root=tmp_path, interpreter=interpreter)

    server_module.handle_analyze_file({"file_path": str(demo)}, state)

    assert captured["interpreter"] == interpreter
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_pyright_session.py -k "python_path or unpinned" plugins/python/tests/test_server.py -k interpreter -q`
Expected: FAIL (`TypeError: unexpected keyword argument 'interpreter'`, missing `python_interpreter` capability)

- [ ] **Step 3: Implement**

In `pyright_session.py`:

1. Module docstring vocabulary — add after the `syntax_error` / `reference_site_cap` bullet:
   ```
   - ``interpreter_unpinned`` -- environment-determined (``transient=False``,
     ``collateral=False``): no project-owned interpreter (override / ``.venv`` /
     ``VIRTUAL_ENV`` / ``CONDA_PREFIX``) was found, so pyright resolved against
     whatever ``python`` was on ``PATH`` and cross-module targets may be missing
     (clarion-5cf9643de9). Only applied to a facet that would otherwise be
     ``complete``. A re-run with the same interpreter cannot recover it; the
     host re-dispatches when the interpreter fingerprint changes.
   ```
2. `from loomweave_plugin_python.interpreter import ProjectInterpreter, discover_project_interpreter`.
3. `__init__`: add keyword `interpreter: ProjectInterpreter | None = None`; after `self.project_root = ...`:
   ```python
   self.interpreter = (
       interpreter
       if interpreter is not None
       else discover_project_interpreter(self.project_root, self._subprocess_env())
   )
   ```
   (`_subprocess_env()` merges `self.env` over `os.environ`, so a test that passes `env=` steers discovery; note `self.env` must be assigned BEFORE this line — move the assignment up.)
4. `_configuration_for_section`: for `section == "python"` return `{"pythonPath": self.interpreter.path, "analysis": analysis}` when `self.interpreter.path is not None`, else `{"analysis": analysis}`.
5. Add a helper next to `_unavailable_coverage`:
   ```python
   def _environment_qualified(self, coverage: FacetCoverage) -> FacetCoverage:
       """Honesty gate for a facet that came back ``complete`` (R1)."""
       if coverage.is_degraded or self.interpreter.pinned:
           return coverage
       return FacetCoverage.degraded("interpreter_unpinned", transient=False)
   ```
   Apply it at the single point where each pass builds its returned result: in `resolve_calls` wrap the `coverage` passed into the final `CallResolutionResult(...)` and in `resolve_references` wrap the coverage on the final `ReferenceResolutionResult(...)`. Do NOT touch the early-return branches (`_unavailable_coverage`, syntax error, site cap) — they are already degraded.
6. In `_spawn_and_initialize`, after the handshake succeeds and only once per session (guard with `self._interpreter_announced: bool` initialised in `__init__`), write one stderr line: `loomweave-plugin-python: pyright interpreter <path or 'none'> (source=<source>, pinned=<bool>)`.

In `server.py`:

1. `ServerState` gains `interpreter: ProjectInterpreter | None = field(default=None)`.
2. `handle_initialize`: after `state.project_root` is set, `state.interpreter = discover_project_interpreter(state.project_root or Path.cwd())`; add to the returned `capabilities`: `"python_interpreter": {"path": state.interpreter.path, "source": state.interpreter.source, "pinned": state.interpreter.pinned}`.
3. `handle_analyze_file`: `PyrightSession(state.project_root or path.parent, run_state=state.pyright_run_state, interpreter=state.interpreter)`.

- [ ] **Step 4: Run the whole plugin suite**

Run: `plugins/python/.venv/bin/pytest plugins/python -q`
Expected: all pass (previous count 408 + 11 new). If `test_initialize_roundtrip` or `test_server` handshake tests assert an exact `capabilities` dict, extend their expectation with the new key rather than weakening them.

- [ ] **Step 5: Lint, type-check, commit**

Run: `plugins/python/.venv/bin/ruff check plugins/python && plugins/python/.venv/bin/ruff format plugins/python && plugins/python/.venv/bin/mypy --strict plugins/python`

```bash
git add plugins/python
git commit -m "fix(plugin-python): pin pyright to the project interpreter and claim interpreter_unpinned honestly (clarion-5cf9643de9)"
```

---

### Task 3: Host — Rust discovery, env pass-through, `resolver_environment` marker, forced re-dispatch

**Files:**
- Create: `crates/loomweave-core/src/plugin/interpreter.rs`
- Modify: `crates/loomweave-core/src/plugin/mod.rs` (declare + re-export), `crates/loomweave-core/src/plugin/host.rs` (`spawn_unhandshaken` env), `crates/loomweave-core/src/lib.rs` (re-export if `plugin::` items are re-exported there — follow the existing pattern for `LANGUAGE_SERVER_MAX_AS_MIB`)
- Create: `crates/loomweave-storage/migrations/0015_plugin_resolver_environment.sql`
- Modify: `crates/loomweave-storage/src/schema.rs` (`MIGRATIONS`, `CURRENT_SCHEMA_VERSION = 15`), `crates/loomweave-storage/src/prior_index.rs` (`PluginIndexMarker.resolver_environment`, load/upsert), any storage test constructing `PluginIndexMarker`
- Modify: `crates/loomweave-cli/src/analyze.rs` (marker comparison block near the `plugin_tag_schema_changed` computation)
- Test: unit tests in `interpreter.rs`; `crates/loomweave-storage/src/prior_index.rs` tests; `crates/loomweave-cli/tests/analyze.rs` (new integration test)

**Interfaces:**
- Produces:
  ```rust
  pub const PYTHON_INTERPRETER_ENV: &str = "LOOMWEAVE_PYTHON_INTERPRETER";
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum InterpreterSource { Override, DotVenv, VirtualEnv, Conda, Path, None }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct ProjectInterpreter { pub path: Option<PathBuf>, pub source: InterpreterSource }
  impl ProjectInterpreter {
      pub fn pinned(&self) -> bool;
      pub fn fingerprint(&self) -> String;   // see Global Constraints
  }
  pub fn discover_project_interpreter(project_root: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> ProjectInterpreter;
  /// `Some(fingerprint)` for manifests declaring `[capabilities.runtime.pyright]`, else `None`.
  pub fn resolver_environment_for(manifest: &Manifest, project_root: &Path) -> Option<String>;
  ```
  `PluginIndexMarker { ..., pub resolver_environment: Option<String> }`.

- [ ] **Step 1: Write failing unit tests for discovery (in `interpreter.rs` `#[cfg(test)]`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    fn make_python(path: &Path) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        path.canonicalize().unwrap()
    }

    fn env(map: &HashMap<&str, String>) -> impl Fn(&str) -> Option<OsString> + '_ {
        move |key| map.get(key).map(OsString::from)
    }

    #[test]
    fn dotvenv_wins_over_virtual_env_and_path() {
        let dir = tempfile::tempdir().unwrap();
        let dotvenv = make_python(&dir.path().join(".venv/bin/python"));
        let other = make_python(&dir.path().join("elsewhere/bin/python"));
        let map = HashMap::from([
            ("VIRTUAL_ENV", dir.path().join("elsewhere").display().to_string()),
            ("PATH", other.parent().unwrap().display().to_string()),
        ]);
        let found = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(found, ProjectInterpreter { path: Some(dotvenv.clone()), source: InterpreterSource::DotVenv });
        assert!(found.pinned());
        assert_eq!(found.fingerprint(), dotvenv.display().to_string());
    }

    #[test]
    fn override_wins_and_an_unusable_override_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        let dotvenv = make_python(&dir.path().join(".venv/bin/python"));
        let custom = make_python(&dir.path().join("custom/python"));
        let map = HashMap::from([(PYTHON_INTERPRETER_ENV, custom.display().to_string())]);
        assert_eq!(discover_project_interpreter(dir.path(), &env(&map)).source, InterpreterSource::Override);
        let map = HashMap::from([(PYTHON_INTERPRETER_ENV, dir.path().join("nope").display().to_string())]);
        let found = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(found.source, InterpreterSource::DotVenv);
        assert_eq!(found.path, Some(dotvenv));
    }

    #[test]
    fn virtual_env_then_conda_then_path_then_none() {
        let dir = tempfile::tempdir().unwrap();
        let venv = make_python(&dir.path().join("venv/bin/python"));
        let conda = make_python(&dir.path().join("conda/bin/python"));
        let on_path = make_python(&dir.path().join("bin/python3"));
        let path_dir = on_path.parent().unwrap().display().to_string();
        let map = HashMap::from([
            ("VIRTUAL_ENV", dir.path().join("venv").display().to_string()),
            ("CONDA_PREFIX", dir.path().join("conda").display().to_string()),
            ("PATH", path_dir.clone()),
        ]);
        assert_eq!(discover_project_interpreter(dir.path(), &env(&map)).path, Some(venv));
        let map = HashMap::from([
            ("CONDA_PREFIX", dir.path().join("conda").display().to_string()),
            ("PATH", path_dir.clone()),
        ]);
        assert_eq!(discover_project_interpreter(dir.path(), &env(&map)).path, Some(conda));
        let map = HashMap::from([("PATH", path_dir)]);
        let unpinned = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(unpinned.source, InterpreterSource::Path);
        assert!(!unpinned.pinned());
        assert_eq!(unpinned.fingerprint(), format!("unpinned:{}", on_path.display()));
        let map = HashMap::from([("PATH", dir.path().join("empty").display().to_string())]);
        let none = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(none, ProjectInterpreter { path: None, source: InterpreterSource::None });
        assert_eq!(none.fingerprint(), "unpinned:none");
    }

    #[test]
    fn path_lookup_prefers_python_over_python3_and_skips_non_executables() {
        let dir = tempfile::tempdir().unwrap();
        let py = make_python(&dir.path().join("bin/python"));
        make_python(&dir.path().join("bin/python3"));
        let map = HashMap::from([("PATH", py.parent().unwrap().display().to_string())]);
        assert_eq!(discover_project_interpreter(dir.path(), &env(&map)).path, Some(py.clone()));
        fs::set_permissions(&py, fs::Permissions::from_mode(0o644)).unwrap();
        let found = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(found.path.unwrap().file_name().unwrap(), "python3");
    }
}
```

`tempfile` is already a dev-dependency of `loomweave-core` (check `crates/loomweave-core/Cargo.toml`; add `tempfile = { workspace = true }` under `[dev-dependencies]` if absent).

- [ ] **Step 2: Implement `interpreter.rs`**

```rust
//! Project-interpreter discovery for language-server plugins
//! (clarion-5cf9643de9).
//!
//! `pyright-langserver` type-checks against whatever `python` is first on its
//! `PATH` unless the client pins `python.pythonPath`. An `analyze` launched
//! from an agent hook carries no project venv on `PATH`, so every
//! `tests/ -> src/` call target came back empty while the coverage claim said
//! `complete`, and the incremental skip pinned the hole. The host now runs
//! this discovery before the incremental partition, exports the winner to the
//! plugin as [`PYTHON_INTERPRETER_ENV`], and keys `plugin_index_meta.
//! resolver_environment` on [`ProjectInterpreter::fingerprint`] so a changed
//! interpreter forces a full re-dispatch of the plugin's files.
//!
//! The order is a CROSS-LANGUAGE CONTRACT with
//! `plugins/python/src/loomweave_plugin_python/interpreter.py`. Change both or
//! neither.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::manifest::Manifest;

/// Env var carrying the host's (or the operator's) interpreter choice to the
/// plugin.
///
/// - Basis: the host's discovery is authoritative under `analyze`; the plugin
///   trusts this var first so the two agree by construction.
/// - Override surface: this var IS the override surface.
/// - Retune trigger: none — a path, not a tunable.
/// - Coupling: `loomweave_plugin_python.interpreter.INTERPRETER_OVERRIDE_ENV`
///   carries the same literal.
pub const PYTHON_INTERPRETER_ENV: &str = "LOOMWEAVE_PYTHON_INTERPRETER";

/// Where the interpreter came from, in contract order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpreterSource {
    Override,
    DotVenv,
    VirtualEnv,
    Conda,
    /// First `python` / `python3` on `PATH` — a guess, not project-owned.
    Path,
    /// Nothing found; pyright falls back to its own discovery.
    None,
}

/// The interpreter pyright will be pointed at, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInterpreter {
    pub path: Option<PathBuf>,
    pub source: InterpreterSource,
}

impl ProjectInterpreter {
    /// Project-owned (override / `.venv` / `VIRTUAL_ENV` / `CONDA_PREFIX`).
    #[must_use]
    pub fn pinned(&self) -> bool {
        !matches!(self.source, InterpreterSource::Path | InterpreterSource::None)
    }

    /// Stable string for `plugin_index_meta.resolver_environment`.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        match (&self.path, self.pinned()) {
            (Some(path), true) => path.display().to_string(),
            (Some(path), false) => format!("unpinned:{}", path.display()),
            (None, _) => "unpinned:none".to_owned(),
        }
    }
}

fn usable(candidate: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::metadata(candidate).ok()?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    candidate.canonicalize().ok()
}

fn which(name: &str, path_var: Option<&OsString>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var).find_map(|dir| usable(&dir.join(name)))
}

/// Resolve the project's interpreter in the contract order (module docs).
/// `env` abstracts `std::env::var_os` so tests can inject an environment.
#[must_use]
pub fn discover_project_interpreter(
    project_root: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> ProjectInterpreter {
    if let Some(raw) = env(PYTHON_INTERPRETER_ENV).filter(|value| !value.is_empty()) {
        if let Some(path) = usable(Path::new(&raw)) {
            return ProjectInterpreter { path: Some(path), source: InterpreterSource::Override };
        }
        tracing::warn!(
            override_path = %raw.to_string_lossy(),
            "{PYTHON_INTERPRETER_ENV} is not an executable file; ignoring the override"
        );
    }
    if let Some(path) = usable(&project_root.join(".venv/bin/python")) {
        return ProjectInterpreter { path: Some(path), source: InterpreterSource::DotVenv };
    }
    for (var, source) in [("VIRTUAL_ENV", InterpreterSource::VirtualEnv), ("CONDA_PREFIX", InterpreterSource::Conda)] {
        if let Some(prefix) = env(var).filter(|value| !value.is_empty())
            && let Some(path) = usable(&Path::new(&prefix).join("bin/python"))
        {
            return ProjectInterpreter { path: Some(path), source };
        }
    }
    let path_var = env("PATH");
    for name in ["python", "python3"] {
        if let Some(path) = which(name, path_var.as_ref()) {
            return ProjectInterpreter { path: Some(path), source: InterpreterSource::Path };
        }
    }
    ProjectInterpreter { path: None, source: InterpreterSource::None }
}

/// The resolver-environment fingerprint a plugin's index depends on: `Some`
/// only for manifests declaring `[capabilities.runtime.pyright]`.
#[must_use]
pub fn resolver_environment_for(manifest: &Manifest, project_root: &Path) -> Option<String> {
    manifest.capabilities.runtime.pyright.as_ref()?;
    Some(discover_project_interpreter(project_root, &|key| std::env::var_os(key)).fingerprint())
}
```

Gate the unix-only pieces with `#[cfg(unix)]` following the pattern `host.rs` uses for `effective_as_mib`; the crate already targets Linux/macOS only for the plugin host. Declare `pub mod interpreter;` in `plugin/mod.rs` and re-export `PYTHON_INTERPRETER_ENV, ProjectInterpreter, InterpreterSource, discover_project_interpreter, resolver_environment_for` wherever `LANGUAGE_SERVER_MAX_AS_MIB` is re-exported.

- [ ] **Step 3: Pass the choice to the plugin in `spawn_unhandshaken`**

In `host.rs`, right after `let mut command = std::process::Command::new(executable);` add:

```rust
// clarion-5cf9643de9: language-server plugins resolve against the
// interpreter the HOST chose, so the index never depends on the launcher's
// PATH. Only a pinned (project-owned) choice is exported; an operator's
// own LOOMWEAVE_PYTHON_INTERPRETER is left untouched (it wins discovery).
if manifest.capabilities.runtime.pyright.is_some() && std::env::var_os(PYTHON_INTERPRETER_ENV).is_none() {
    let chosen = discover_project_interpreter(&canonical_root, &|key| std::env::var_os(key));
    if let (true, Some(path)) = (chosen.pinned(), chosen.path.as_ref()) {
        command.env(PYTHON_INTERPRETER_ENV, path);
    }
}
```

Add a host unit test (next to the existing `effective_as_mib` tests, using `pyright_small_rss_manifest()`) asserting that a manifest WITHOUT the pyright capability never triggers discovery — implement by extracting the block into `fn exported_interpreter(manifest: &Manifest, root: &Path) -> Option<PathBuf>` and testing it directly with a tempdir containing `.venv/bin/python`: pyright manifest → `Some(path)`, non-pyright manifest → `None`.

- [ ] **Step 4: Storage migration + marker field**

`crates/loomweave-storage/migrations/0015_plugin_resolver_environment.sql`:

```sql
-- Migration 0015: resolver-environment fingerprint per plugin (clarion-5cf9643de9).
--
-- The Python plugin's call/reference evidence depends on which interpreter
-- pyright resolved against. `analyze` now records the host-discovered
-- interpreter fingerprint here and forces a full re-dispatch of the plugin's
-- files when it changes, exactly as for a plugin/ontology version bump.
-- NULL = never recorded (pre-migration rows) -> treated as changed, so the
-- first run after upgrade re-dispatches once and heals rows pinned by a
-- launcher-dependent interpreter. Non-language-server plugins keep NULL.

BEGIN;

ALTER TABLE plugin_index_meta
ADD COLUMN resolver_environment TEXT;

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (
    15,
    '0015_plugin_resolver_environment',
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
);

COMMIT;
```

`schema.rs`: append the `Migration { version: 15, name: "0015_plugin_resolver_environment", sql: include_str!("../migrations/0015_plugin_resolver_environment.sql") }` entry and set `CURRENT_SCHEMA_VERSION = 15`. `prior_index.rs`: add `pub resolver_environment: Option<String>` to `PluginIndexMarker` (doc: "Host-discovered resolver-environment fingerprint; `None` for plugins without one or for rows written before migration 0015"), read it as column 4 in `load_plugin_index_markers`, and write it in `upsert_plugin_index_marker` (`?5`, with `recorded_at` moving to `?6`; add `resolver_environment = excluded.resolver_environment` to the upsert). Fix every test/constructor of `PluginIndexMarker` (`grep -rn "PluginIndexMarker {" crates`). Add a storage test: upsert `Some("x")` then `None`, reload, assert each round-trips. Run `python3 scripts/check-migration-retirement.py` and fix whatever it asks for (it may want the migration listed somewhere).

- [ ] **Step 5: Wire the comparison into `analyze.rs`**

Near `let plugin_version = plugin.manifest.plugin.version.clone();`:

```rust
// clarion-5cf9643de9: the interpreter pyright resolves against is part of
// the evidence contract. A change (or an unrecorded prior) forces the same
// full re-dispatch as a plugin/ontology bump.
let resolver_environment =
    loomweave_core::resolver_environment_for(&plugin.manifest, &project_root);
let resolver_environment_changed = match prior_plugin_marker {
    Some(prior) => prior.resolver_environment != resolver_environment,
    None => resolver_environment.is_some(),
};
```

Fold it into `plugin_index_contract_changed` (`|| resolver_environment_changed`), add `resolver_environment_changed` to the `tracing::info!` on the forced re-dispatch, and set `resolver_environment` on the pushed `PluginIndexMarker`. If `resolver_environment_changed` is true, also log at `info` the new fingerprint so an operator can see WHY the run re-dispatched.

- [ ] **Step 6: Integration test in `crates/loomweave-cli/tests/analyze.rs`**

Model on the test near line 5745 (`ontology_version` bump ⇒ `skipped_files == 0`). Use the fixture plugin with a manifest that declares `[capabilities.runtime.pyright] pin = "1.1.409"` (copy the manifest from `wp2_e2e.rs::setup_language_server_plugin_dir`, `plugin_id = "fixture"`). Sequence:
1. Project dir with two `.ls` files and an executable `.venv/bin/python` shell stub. Run 1 (fresh). Assert `plugin_index_meta.resolver_environment` for `fixture` equals the canonical stub path (query the DB directly with rusqlite as other tests in the file do).
2. Run 2 (unchanged): `skipped_files == 2`.
3. Remove `.venv`; run 3 with `PATH` set to a tempdir containing an executable `python3` stub (`Command::env("PATH", ...)`): `skipped_files == 0`, marker now `unpinned:<stub>`.
4. Run 4 (unchanged again): `skipped_files == 2`.
Also assert the plugin child received `LOOMWEAVE_PYTHON_INTERPRETER` in run 1: the fixture binary can be asked to echo env — check `crates/loomweave-plugin-fixture/src/main.rs` for an existing env-dump knob (e.g. how `LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB` is read); if none exists, add `LOOMWEAVE_FIXTURE_DUMP_ENV_TO=<file>` which writes `key=value` lines at `initialize`, and assert the file contains the stub path.

- [ ] **Step 7: Full Rust floor, then commit**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo build --workspace --bins && cargo nextest run --workspace --all-features && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features && python3 scripts/check-migration-retirement.py`

```bash
git add crates/loomweave-core crates/loomweave-storage crates/loomweave-cli crates/loomweave-plugin-fixture
git commit -m "feat(host): discover the project interpreter, export it to pyright plugins, and key the incremental skip on it (clarion-5cf9643de9)"
```

---

### Task 4: `doctor` — `index.runs` check reaps abandoned `running` rows under `--fix`

**Files:**
- Modify: `crates/loomweave-cli/src/doctor.rs` (new `check_runs_json`, registration in `json_report` and text `run`, `default_next_action`, and the `index.resolution_coverage` next-action wording)
- Test: `crates/loomweave-cli/src/doctor.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `loomweave_storage::mark_abandoned_running_runs_failed`, `crate::analyze_lock::try_acquire_analyze_lock`, `loomweave_core::store::{db_path, store_dir}`.
- Produces: check id `"index.runs"`.

- [ ] **Step 1: Write the failing tests** (inside the existing `mod tests`, reusing `migrated_db(root)`):

```rust
fn seed_run(conn: &Connection, id: &str, status: &str, owner_pid: Option<i64>) {
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status, owner_pid) \
         VALUES (?1, '2026-01-01T00:00:00.000Z', NULL, '{}', '{}', ?2, ?3)",
        rusqlite::params![id, status, owner_pid],
    )
    .unwrap();
}

fn run_status(conn: &Connection, id: &str) -> String {
    conn.query_row("SELECT status FROM runs WHERE id = ?1", rusqlite::params![id], |r| r.get(0)).unwrap()
}

#[test]
fn runs_check_is_ok_when_nothing_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let db = migrated_db(dir.path());
    let conn = Connection::open(&db).unwrap();
    seed_run(&conn, "done", "completed", None);
    let check = check_runs_json(dir.path(), false);
    assert_eq!(check.status, "ok", "{}", check.message);
}

#[test]
fn runs_dry_run_warns_and_lists_abandoned_rows_without_touching_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = migrated_db(dir.path());
    let conn = Connection::open(&db).unwrap();
    seed_run(&conn, "stuck", "running", Some(99_999_999));
    let check = check_runs_json(dir.path(), false);
    assert_eq!(check.status, "warning", "{}", check.message);
    assert!(!check.fixed);
    let details = check.details.as_ref().unwrap();
    assert_eq!(details["running_rows"], 1);
    assert_eq!(details["runs"][0]["id"], "stuck");
    assert_eq!(details["runs"][0]["owner_pid"], 99_999_999);
    assert_eq!(run_status(&conn, "stuck"), "running");
}

#[test]
fn runs_fix_marks_every_running_row_failed_while_holding_the_analyze_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = migrated_db(dir.path());
    let conn = Connection::open(&db).unwrap();
    seed_run(&conn, "stuck-a", "running", Some(1));
    seed_run(&conn, "stuck-b", "running", None);
    seed_run(&conn, "done", "completed", None);
    let check = check_runs_json(dir.path(), true);
    assert_eq!(check.status, "fixed", "{}", check.message);
    assert!(check.fixed);
    assert_eq!(check.details.as_ref().unwrap()["repaired_rows"], 2);
    assert_eq!(run_status(&conn, "stuck-a"), "failed");
    assert_eq!(run_status(&conn, "stuck-b"), "failed");
    assert_eq!(run_status(&conn, "done"), "completed");
}

#[test]
fn runs_check_is_ok_not_warning_when_a_live_analyze_holds_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = migrated_db(dir.path());
    let conn = Connection::open(&db).unwrap();
    seed_run(&conn, "live", "running", Some(std::process::id().into()));
    let store = loomweave_core::store::store_dir(dir.path());
    let _guard = match crate::analyze_lock::try_acquire_analyze_lock(&store).unwrap() {
        crate::analyze_lock::TryAnalyzeLock::Acquired(guard) => guard,
        crate::analyze_lock::TryAnalyzeLock::Held { .. } => panic!("lock free in a fresh tempdir"),
    };
    for fix in [false, true] {
        let check = check_runs_json(dir.path(), fix);
        assert_eq!(check.status, "ok", "fix={fix}: {}", check.message);
        assert!(check.message.contains("analyze lock"), "{}", check.message);
    }
    assert_eq!(run_status(&conn, "live"), "running");
}
```

Check whether `try_acquire_analyze_lock` from the SAME process can observe its own lock as `Held` (fs2 advisory locks are per-open-file on Linux, so a second `try_lock_exclusive` on a separate fd in the same process DOES fail with WouldBlock — verify with a quick experiment; if it does not, spawn `sleep` under the lock via a helper instead).

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p loomweave-cli runs_check runs_dry_run runs_fix`
Expected: compile error (`check_runs_json` undefined)

- [ ] **Step 3: Implement `check_runs_json`**

```rust
/// `runs` rows left `running` by a builder that died uncleanly (OOM-kill,
/// `kill -9`, reboot) — clarion-5cf9643de9 aside. `analyze` itself only
/// sweeps rows whose heartbeat is >24 h old (`mark_stale_running_runs_failed`);
/// a fresher abandoned row poisons `project_status_get` / the hook snapshot
/// until then. The analyze lock is the liveness proof: every `analyze` holds
/// it from before `BeginRun` to after its last transaction, so if `doctor`
/// can take it, no builder is alive and every `running` row is abandoned.
fn check_runs_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    const ID: &str = "index.runs";
    let db = loomweave_core::store::db_path(project_root);
    if !db.exists() {
        return DoctorJsonCheck::warning(ID, "loomweave.db is absent");
    }
    let Ok(conn) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return DoctorJsonCheck::warning(ID, "loomweave.db is absent or unreadable");
    };
    if let Err(err) = validate_external_sqlite_read_gate(&conn) {
        return DoctorJsonCheck::problem(ID, format!("runs unavailable: {}", err.message()))
            .with_details(serde_json::json!({ "external_sqlite": err.details() }));
    }
    let running = match running_runs(&conn) {
        Ok(rows) => rows,
        Err(err) => return DoctorJsonCheck::warning(ID, format!("runs could not be read: {err}")),
    };
    if running.is_empty() {
        return DoctorJsonCheck::ok(ID, "no analyze run is recorded as running");
    }
    drop(conn);
    let loomweave_dir = loomweave_core::store::store_dir(project_root);
    let details = serde_json::json!({ "running_rows": running.len(), "runs": running });
    match crate::analyze_lock::try_acquire_analyze_lock(&loomweave_dir) {
        Ok(crate::analyze_lock::TryAnalyzeLock::Held { .. }) => DoctorJsonCheck::ok(
            ID,
            format!("{} running run(s); an analyze holds the analyze lock, so they are live", running.len()),
        )
        .with_details(details),
        Ok(crate::analyze_lock::TryAnalyzeLock::Acquired(_guard)) if fix => {
            match repair_abandoned_runs(&db) {
                Ok(count) => DoctorJsonCheck::fixed(
                    ID,
                    format!("marked {count} abandoned running run(s) failed (no live analyze holds the lock)"),
                )
                .with_details(serde_json::json!({ "repaired_rows": count, "runs": running })),
                Err(err) => DoctorJsonCheck::problem(ID, format!("abandoned run repair failed: {err}")),
            }
        }
        Ok(crate::analyze_lock::TryAnalyzeLock::Acquired(_guard)) => DoctorJsonCheck::warning(
            ID,
            format!("{} run(s) recorded as running but no analyze holds the lock (abandoned)", running.len()),
        )
        .with_details(details)
        .with_next_action("Run `loomweave doctor --fix --path <project>` to mark the abandoned runs failed."),
        Err(err) => DoctorJsonCheck::problem(
            ID,
            format!("{} running run(s) and the analyze lock could not be taken: {err:#}", running.len()),
        )
        .with_details(details),
    }
}

fn running_runs(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, started_at, owner_pid, heartbeat_at FROM runs WHERE status = 'running' ORDER BY started_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, String>(0)?,
            "started_at": row.get::<_, String>(1)?,
            "owner_pid": row.get::<_, Option<i64>>(2)?,
            "heartbeat_at": row.get::<_, Option<String>>(3)?,
        }))
    })?;
    rows.collect()
}

fn repair_abandoned_runs(db_path: &Path) -> Result<usize> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("open index {} for repair", db_path.display()))?;
    loomweave_storage::pragma::apply_write_pragmas(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    loomweave_storage::mark_abandoned_running_runs_failed(&conn).map_err(|e| anyhow::anyhow!("{e}"))
}
```

Register `check_runs_json(project_root, fix)` in `json_report` directly after `check_resolution_coverage_json`, and in the text `run` as `tally += emit_json_check_text(&check_runs_json(&project_root, fix));` at the same position. Add `"index.runs"` to `default_next_action`. In the `index.resolution_coverage` next-action string, change "Content-determined ones (syntax error / site cap)" to "Content- or environment-determined ones (syntax error / site cap / `interpreter_unpinned` — set `LOOMWEAVE_PYTHON_INTERPRETER` or create `.venv`)".

- [ ] **Step 4: Run tests, floor, commit**

Run: `cargo nextest run -p loomweave-cli doctor` then the full Rust floor.

```bash
git add crates/loomweave-cli/src/doctor.rs
git commit -m "feat(doctor): index.runs reaps running rows abandoned by dead builders under --fix (clarion-5cf9643de9)"
```

---

### Task 5: Documentation — ADR-058, ADR index, ADR-057 vocabulary, plugin README, ADR-035 inventory

**Files:**
- Create: `docs/loomweave/adr/ADR-058-project-interpreter-discovery.md`
- Modify: `docs/loomweave/adr/README.md` (index row after ADR-057), `docs/loomweave/adr/ADR-057-pyright-restart-attribution.md` (one paragraph in the operational-note section naming `interpreter_unpinned` as environment-determined and outside attribution), `docs/loomweave/adr/ADR-035-operational-tuning-discipline.md` (§2 Python inventory: add `INTERPRETER_OVERRIDE_ENV` line noting it is an override surface, not a tunable), `plugins/python/README.md` (new "Interpreter discovery" section: the order table, the env var, the `interpreter_unpinned` claim), `docs/operator/getting-started.md` (two sentences under the analyze section: create `.venv` or set `LOOMWEAVE_PYTHON_INTERPRETER` for full test→src call resolution)

- [ ] **Step 1: Write ADR-058** with sections Context (the bisect table from the ticket, verbatim numbers: 39/13 vs 26/0), Decision (1. discovery contract table; 2. `python.pythonPath` in the configuration reply; 3. `interpreter_unpinned` semantics — transient=false, collateral=false, applied only to an otherwise-complete facet; 4. `plugin_index_meta.resolver_environment` + fingerprint format + NULL-means-changed; 5. host exports `LOOMWEAVE_PYTHON_INTERPRETER` only when pinned and unset; 6. `doctor index.runs`), Consequences (first run after upgrade re-dispatches every Python file once; projects with no venv and a flapping PATH re-dispatch on each flip; Windows layouts (`Scripts\python.exe`) not covered), Alternatives (hook puts venv on PATH — rejected: fixes one launcher; explicit `extraPaths` — unnecessary; transient=true — rejected: burns the re-dispatch budget with no path to recovery), Related (ADR-021, ADR-035, ADR-050, ADR-057). Status Accepted, Date 2026-08-29, Tickets clarion-5cf9643de9.

- [ ] **Step 2: Apply the other doc edits listed above.**

- [ ] **Step 3: Verify links resolve and commit**

Run: `grep -n "ADR-058" docs/loomweave/adr/README.md && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`

```bash
git add docs plugins/python/README.md
git commit -m "docs: ADR-058 project interpreter discovery + resolver-environment marker (clarion-5cf9643de9)"
```

---

### Task 6 (controller-owned, not a subagent task): floor, PR, deploy, elspeth acceptance, ticket close

- [ ] Full CI floor (Rust + Python gates) green locally; push branch; open PR against `release/1.5.0`; `gh pr merge --admin --merge` once checks are green.
- [ ] Deploy: `cargo build --release -p loomweave-cli`; atomic temp+mv into `~/.local/share/uv/tools/loomweave/bin/loomweave`; plugin into BOTH venvs (`uv pip install --python ~/.local/share/uv/tools/loomweave/bin/python --no-deps --reinstall ./plugins/python` and `uv tool install --reinstall --force ./plugins/python`).
- [ ] Acceptance on elspeth, hook-style env: `cd /home/john/elspeth && env -i HOME="$HOME" PATH=/usr/local/bin:/usr/bin:/bin:$HOME/.local/bin setsid nohup loomweave analyze . > /tmp/claude-…/elspeth-acc.log 2>&1 < /dev/null &`. Expect: the run logs `plugin index contract changed … resolver_environment_changed=true`, `skipped_files == 0`, `plugin_index_meta.resolver_environment` = `/home/john/elspeth/.venv/bin/python` realpath.
- [ ] Measure: `SELECT count(*) FROM (test files importing elspeth with zero calls edges into elspeth.*)` — expected ≈ 0 (down from 400); `build_step_chat_context_block` has test callers; `tests/unit/elspeth_lints` resolves; degraded rows unchanged (3) with no `interpreter_unpinned` on elspeth.
- [ ] `loomweave doctor --fix` on elspeth: `index.runs` repairs the two stuck rows (dce107d2, 9badd1f3).
- [ ] Filigree: `clarion-5cf9643de9` triage → confirmed (severity) → fixing → verifying (fix_verification = the numbers above) → closed. File the aside "five runs died to the 120 s host watchdog (90 s cap + references pass)" as a new TBC bug if not already tracked.
- [ ] Memory note + MEMORY.md line.
