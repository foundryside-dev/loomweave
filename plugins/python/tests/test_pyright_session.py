from __future__ import annotations

import ast
import shutil
import stat
import sys
import textwrap
from pathlib import Path
from typing import TYPE_CHECKING, cast

import pytest

from clarion_plugin_python.pyright_session import (
    FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT,
    FINDING_PYRIGHT_INIT_TIMEOUT,
    FINDING_PYRIGHT_INSTALL_FAILURE,
    FINDING_PYRIGHT_POISON_FRAME,
    FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT,
    FINDING_PYRIGHT_REFERENCE_SITE_CAP,
    FINDING_PYRIGHT_RESTART,
    FINDING_PYRIGHT_UNAVAILABLE,
    LspTimeoutError,
    PyrightSession,
    _CallSite,
    _FunctionIndex,
    _FunctionInfo,
    _unresolved_call_site_total_for_function,
    _unresolved_call_sites_for_function,
)
from clarion_plugin_python.reference_resolver import ReferenceSite, ReferenceSiteKind

if TYPE_CHECKING:
    from collections.abc import Sequence

    from clarion_plugin_python.call_resolver import Finding


@pytest.fixture(scope="session")
def pyright_langserver() -> str:
    venv_candidate = Path(sys.executable).parent / "pyright-langserver"
    if venv_candidate.exists():
        return str(venv_candidate)
    resolved = shutil.which("pyright-langserver")
    if resolved is None:
        pytest.skip("pyright-langserver is not installed")
    return resolved


def _write_module(tmp_path: Path, source: str, name: str = "demo.py") -> Path:
    path = tmp_path / name
    path.write_text(textwrap.dedent(source).lstrip(), encoding="utf-8")
    return path


def test_unresolved_call_site_details_omit_expressions_over_host_cap() -> None:
    callee_expr = "factory." + ".".join(f"method_{idx:03d}" for idx in range(80))
    assert len(callee_expr.encode("utf-8")) > 512
    source = f"def caller():\n    {callee_expr}()\n"
    tree = ast.parse(source)
    function_node = cast("ast.FunctionDef", tree.body[0])
    index = _FunctionIndex(
        source=source,
        line_starts=(0, len(b"def caller():\n")),
        parse_latency_ms=0,
        module_id="python:module:demo",
        by_id={},
        by_name_position={},
        entity_by_name_position={},
        by_short_name={},
        dunder_call_by_class={},
        functions=(),
        entities=(),
        tree=tree,
    )
    function = _FunctionInfo(
        entity_id="python:function:demo.caller",
        qualified_name="demo.caller",
        name="caller",
        line=0,
        character=4,
        end_line=1,
        end_character=8,
        call_sites=(
            _CallSite(
                line=1,
                character=4,
                end_line=1,
                end_character=4 + len(callee_expr),
                callee_expr=callee_expr,
            ),
        ),
        node=function_node,
    )

    assert _unresolved_call_site_total_for_function(function, set()) == 1
    assert _unresolved_call_sites_for_function(index, function, set()) == []


def _finding_codes(result_findings: Sequence[Finding]) -> set[str]:
    return {str(finding["subcode"]) for finding in result_findings}


def _reference_site(
    source: str,
    *,
    from_id: str,
    needle: str,
    kind: str = "name",
    occurrence: int = 0,
) -> ReferenceSite:
    lines = source.splitlines(keepends=True)
    seen = 0
    byte_start = 0
    for line_no, line in enumerate(lines):
        start = 0
        while True:
            character = line.find(needle, start)
            if character < 0:
                break
            if seen == occurrence:
                line_byte_start = sum(len(prev.encode("utf-8")) for prev in lines[:line_no])
                byte_start = line_byte_start + len(line[:character].encode("utf-8"))
                return ReferenceSite(
                    from_id=from_id,
                    line=line_no,
                    character=character,
                    end_line=line_no,
                    end_character=character + len(needle),
                    source_byte_start=byte_start,
                    source_byte_end=byte_start + len(needle.encode("utf-8")),
                    kind=cast("ReferenceSiteKind", kind),
                )
            seen += 1
            start = character + len(needle)
    msg = f"needle {needle!r} occurrence {occurrence} not found"
    raise AssertionError(msg)


@pytest.mark.pyright
def test_pyright_session_resolves_direct_call(tmp_path: Path, pyright_langserver: str) -> None:
    module = _write_module(
        tmp_path,
        """
        def callee():
            pass

        def caller():
            callee()
        """,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_calls(
            module,
            ["python:function:demo.caller", "python:function:demo.callee"],
        )

    assert result.edges == [
        {
            "kind": "calls",
            "from_id": "python:function:demo.caller",
            "to_id": "python:function:demo.callee",
            "confidence": "resolved",
            "source_byte_start": result.edges[0]["source_byte_start"],
            "source_byte_end": result.edges[0]["source_byte_end"],
        },
    ]
    assert result.edges[0]["source_byte_start"] < result.edges[0]["source_byte_end"]
    assert result.pyright_query_latency_ms[0] > 0
    assert result.pyright_index_parse_latency_ms[0] > 0
    assert result.unresolved_call_sites_total == 0


@pytest.mark.pyright
def test_pyright_session_emits_unresolved_call_site_details(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        import os

        def caller():
            os.getcwd()
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert result.unresolved_call_sites_total == 1
    assert result.unresolved_call_sites == [
        {
            "caller_entity_id": "python:function:demo.caller",
            "site_ordinal": 0,
            "source_byte_start": source.encode().find(b"os.getcwd"),
            "source_byte_end": source.encode().find(b"os.getcwd") + len(b"os.getcwd"),
            "callee_expr": "os.getcwd",
        },
    ]


@pytest.mark.pyright
def test_pyright_session_resolves_module_name_reference(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        def world() -> int:
            return 42

        CONST_REF = world
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:module:demo",
        needle="world",
        occurrence=1,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == [
        {
            "kind": "references",
            "from_id": "python:module:demo",
            "to_id": "python:function:demo.world",
            "confidence": "resolved",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
        },
    ]
    assert result.reference_sites_total == 1
    assert result.references_resolved_total == 1
    assert result.unresolved_reference_sites_total == 0


@pytest.mark.pyright
def test_pyright_session_resolves_annotation_reference_to_class(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        class Foo:
            pass

        def annotated(x: Foo) -> Foo:
            return x
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    sites = [
        _reference_site(
            source,
            from_id="python:function:demo.annotated",
            needle="Foo",
            kind="annotation",
            occurrence=1,
        ),
        _reference_site(
            source,
            from_id="python:function:demo.annotated",
            needle="Foo",
            kind="annotation",
            occurrence=2,
        ),
    ]

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, sites)

    assert result.reference_sites_total == 2
    assert result.references_resolved_total == 2
    assert result.edges == [
        {
            "kind": "references",
            "from_id": "python:function:demo.annotated",
            "to_id": "python:class:demo.Foo",
            "confidence": "resolved",
            "source_byte_start": sites[0].source_byte_start,
            "source_byte_end": sites[0].source_byte_end,
        },
    ]


@pytest.mark.pyright
def test_pyright_session_skips_builtin_reference_target(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = "def annotated(x: int) -> int:\n    return x\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:function:demo.annotated",
        needle="int",
        kind="annotation",
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == []
    assert result.reference_sites_total == 1
    assert result.references_skipped_external_total == 1
    assert result.unresolved_reference_sites_total == 1


@pytest.mark.pyright
def test_pyright_session_references_dedup_to_earliest_range(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        class Foo:
            pass

        LATER = Foo
        EARLIER = Foo
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    later = _reference_site(source, from_id="python:module:demo", needle="Foo", occurrence=1)
    earlier = _reference_site(source, from_id="python:module:demo", needle="Foo", occurrence=2)

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [later, earlier])

    assert len(result.edges) == 1
    assert result.edges[0]["to_id"] == "python:class:demo.Foo"
    assert result.edges[0]["source_byte_start"] == later.source_byte_start
    assert result.edges[0]["source_byte_end"] == later.source_byte_end


def test_pyright_session_reference_unavailable_binary_missing(tmp_path: Path) -> None:
    source = "def world():\n    pass\n\nCONST_REF = world\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(source, from_id="python:module:demo", needle="world", occurrence=1)

    with PyrightSession(tmp_path, executable="clarion-missing-pyright") as session:
        result = session.resolve_references(module, [site])

    assert result.edges == []
    assert result.reference_sites_total == 1
    assert result.unresolved_reference_sites_total == 1
    assert FINDING_PYRIGHT_UNAVAILABLE in _finding_codes(result.findings)


def test_pyright_session_treats_project_local_venv_targets_as_external(tmp_path: Path) -> None:
    target = tmp_path / ".venv" / "lib" / "python3.12" / "site-packages" / "demo.py"
    target.parent.mkdir(parents=True)
    target.write_text("def helper():\n    pass\n", encoding="utf-8")
    location = {
        "uri": target.as_uri(),
        "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 10}},
    }

    session = PyrightSession(tmp_path, executable=sys.executable)

    assert session._target_id_from_location(location) == (None, True)  # noqa: SLF001


def test_pyright_session_reference_site_cap(tmp_path: Path) -> None:
    source = "def world():\n    pass\n\nCONST_REF = world\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(source, from_id="python:module:demo", needle="world", occurrence=1)

    with PyrightSession(
        tmp_path,
        executable=sys.executable,
        max_reference_sites_per_file=0,
    ) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == []
    assert result.reference_sites_total == 1
    assert result.references_skipped_cap_total == 1
    assert result.unresolved_reference_sites_total == 1
    assert FINDING_PYRIGHT_REFERENCE_SITE_CAP in _finding_codes(result.findings)


class ReferenceTimeoutSession(PyrightSession):
    def _request(self, method: str, params: dict[str, object], timeout_secs: float) -> object:
        if method == "textDocument/definition":
            raise LspTimeoutError(method)
        return super()._request(method, params, timeout_secs)


class PartialReferenceTimeoutSession(PyrightSession):
    def __init__(
        self,
        project_root: Path,
        *,
        targets_by_start: dict[int, list[str]],
        timeout_start: int,
    ) -> None:
        super().__init__(project_root, executable=sys.executable)
        self.targets_by_start = targets_by_start
        self.timeout_start = timeout_start
        self.requested_starts: list[int] = []

    def _ensure_process(self) -> bool:
        return True

    def _notify(self, method: str, params: dict[str, object]) -> None:
        _ = (method, params)

    def _reference_target_ids(
        self,
        uri: str,
        site: ReferenceSite,
        *,
        deadline: float,
        method: str = "textDocument/definition",
    ) -> tuple[list[str], bool]:
        _ = (uri, deadline, method)
        self.requested_starts.append(site.source_byte_start)
        if site.source_byte_start == self.timeout_start:
            raise LspTimeoutError(method)
        return self.targets_by_start[site.source_byte_start], False


class CountingReferenceSession(PyrightSession):
    def __init__(self, project_root: Path, *, target_id: str) -> None:
        super().__init__(project_root, executable=sys.executable)
        self.target_id = target_id
        self.requested_starts: list[int] = []

    def _ensure_process(self) -> bool:
        return True

    def _notify(self, method: str, params: dict[str, object]) -> None:
        _ = (method, params)

    def _reference_target_ids(
        self,
        uri: str,
        site: ReferenceSite,
        *,
        deadline: float,
        method: str = "textDocument/definition",
    ) -> tuple[list[str], bool]:
        _ = (uri, deadline, method)
        self.requested_starts.append(site.source_byte_start)
        return [self.target_id], False


@pytest.mark.pyright
def test_pyright_session_reference_resolution_timeout(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = "def world():\n    pass\n\nCONST_REF = world\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(source, from_id="python:module:demo", needle="world", occurrence=1)

    with ReferenceTimeoutSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == []
    assert result.reference_sites_total == 1
    assert result.unresolved_reference_sites_total == 1
    assert FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT in _finding_codes(result.findings)


def test_pyright_session_reuses_same_owner_reference_lookup(tmp_path: Path) -> None:
    source = textwrap.dedent(
        """
        def world():
            pass

        FIRST = world
        SECOND = world
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    first = _reference_site(source, from_id="python:module:demo", needle="world", occurrence=1)
    second = _reference_site(source, from_id="python:module:demo", needle="world", occurrence=2)

    with CountingReferenceSession(
        tmp_path,
        target_id="python:function:demo.world",
    ) as session:
        result = session.resolve_references(module, [first, second])
        requested_starts = session.requested_starts

    assert requested_starts == [first.source_byte_start]
    assert result.reference_sites_total == 2
    assert result.references_resolved_total == 2
    assert result.unresolved_reference_sites_total == 0
    assert result.edges == [
        {
            "kind": "references",
            "from_id": "python:module:demo",
            "to_id": "python:function:demo.world",
            "confidence": "resolved",
            "source_byte_start": first.source_byte_start,
            "source_byte_end": first.source_byte_end,
        },
    ]


def test_pyright_session_reference_timeout_skips_only_current_site(tmp_path: Path) -> None:
    source = textwrap.dedent(
        """
        def alpha():
            pass

        def beta():
            pass

        def gamma():
            pass

        FIRST = alpha
        BROKEN = beta
        THIRD = gamma
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    first = _reference_site(source, from_id="python:module:demo", needle="alpha", occurrence=1)
    broken = _reference_site(source, from_id="python:module:demo", needle="beta", occurrence=1)
    third = _reference_site(source, from_id="python:module:demo", needle="gamma", occurrence=1)

    with PartialReferenceTimeoutSession(
        tmp_path,
        targets_by_start={
            first.source_byte_start: ["python:function:demo.alpha"],
            third.source_byte_start: ["python:function:demo.gamma"],
        },
        timeout_start=broken.source_byte_start,
    ) as session:
        result = session.resolve_references(module, [first, broken, third])
        requested_starts = session.requested_starts

    assert result.edges == [
        {
            "kind": "references",
            "from_id": "python:module:demo",
            "to_id": "python:function:demo.alpha",
            "confidence": "resolved",
            "source_byte_start": first.source_byte_start,
            "source_byte_end": first.source_byte_end,
        },
        {
            "kind": "references",
            "from_id": "python:module:demo",
            "to_id": "python:function:demo.gamma",
            "confidence": "resolved",
            "source_byte_start": third.source_byte_start,
            "source_byte_end": third.source_byte_end,
        },
    ]
    assert requested_starts == [
        first.source_byte_start,
        broken.source_byte_start,
        third.source_byte_start,
    ]
    assert result.reference_sites_total == 3
    assert result.references_resolved_total == 2
    assert result.unresolved_reference_sites_total == 1
    assert FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT in _finding_codes(result.findings)


@pytest.mark.pyright
def test_pyright_session_ambiguous_dict_dispatch(tmp_path: Path, pyright_langserver: str) -> None:
    module = _write_module(
        tmp_path,
        """
        from collections.abc import Callable

        def alpha() -> None:
            pass

        def beta() -> None:
            pass

        handlers: dict[str, Callable[[], None]] = {"a": alpha, "b": beta}

        def caller(key: str) -> None:
            handlers[key]()
        """,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_calls(
            module,
            [
                "python:function:demo.alpha",
                "python:function:demo.beta",
                "python:function:demo.caller",
            ],
        )

    edge = next(edge for edge in result.edges if edge["from_id"] == "python:function:demo.caller")
    assert edge["confidence"] == "ambiguous"
    assert edge["to_id"] == "python:function:demo.alpha"
    assert edge["properties"]["candidates"] == [
        "python:function:demo.alpha",
        "python:function:demo.beta",
    ]


@pytest.mark.pyright
def test_pyright_session_ambiguous_determinism(tmp_path: Path, pyright_langserver: str) -> None:
    module = _write_module(
        tmp_path,
        """
        from collections.abc import Callable

        def beta() -> None:
            pass

        def alpha() -> None:
            pass

        handlers: dict[str, Callable[[], None]] = {"b": beta, "a": alpha}

        def caller(key: str) -> None:
            handlers[key]()
        """,
    )
    function_ids = [
        "python:function:demo.alpha",
        "python:function:demo.beta",
        "python:function:demo.caller",
    ]

    with PyrightSession(tmp_path, executable=pyright_langserver) as first:
        first_edge = first.resolve_calls(module, function_ids).edges[0]
    with PyrightSession(tmp_path, executable=pyright_langserver) as second:
        second_edge = second.resolve_calls(module, function_ids).edges[0]

    assert first_edge == second_edge
    assert first_edge["to_id"] == "python:function:demo.alpha"
    assert first_edge["properties"]["candidates"] == [
        "python:function:demo.alpha",
        "python:function:demo.beta",
    ]


@pytest.mark.pyright
def test_pyright_session_restart_on_crash(tmp_path: Path, pyright_langserver: str) -> None:
    module = _write_module(
        tmp_path,
        """
        def callee():
            pass

        def caller():
            callee()
        """,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        assert session.resolve_calls(module, ["python:function:demo.caller"]).edges
        session.kill_for_test()
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges
    assert FINDING_PYRIGHT_RESTART in _finding_codes(result.findings)


@pytest.mark.pyright
def test_pyright_session_restart_cap(tmp_path: Path, pyright_langserver: str) -> None:
    module = _write_module(
        tmp_path,
        """
        def callee():
            pass

        def caller():
            callee()
        """,
    )

    with PyrightSession(
        tmp_path,
        executable=pyright_langserver,
        max_restarts_per_run=0,
    ) as session:
        assert session.resolve_calls(module, ["python:function:demo.caller"]).edges
        session.kill_for_test()
        poisoned = session.resolve_calls(module, ["python:function:demo.caller"])
        continued = session.resolve_calls(module, ["python:function:demo.caller"])

    assert poisoned.edges == []
    assert FINDING_PYRIGHT_POISON_FRAME in _finding_codes(poisoned.findings)
    assert poisoned.unresolved_call_sites_total == 1
    assert continued.edges == []
    assert continued.unresolved_call_sites_total == 1


def _write_executable(tmp_path: Path, body: str) -> Path:
    script = tmp_path / "fake_langserver.py"
    script.write_text(body, encoding="utf-8")
    script.chmod(script.stat().st_mode | stat.S_IXUSR)
    return script


def test_pyright_session_init_timeout(tmp_path: Path) -> None:
    script = _write_executable(
        tmp_path,
        "#!/usr/bin/env python3\nimport time\ntime.sleep(60)\n",
    )
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")

    with PyrightSession(tmp_path, executable=str(script), init_timeout_secs=0.05) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert FINDING_PYRIGHT_INIT_TIMEOUT in _finding_codes(result.findings)


def test_pyright_session_unavailable_binary_missing(tmp_path: Path) -> None:
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")

    with PyrightSession(tmp_path, executable="clarion-missing-pyright") as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert result.unresolved_call_sites_total == 1
    assert FINDING_PYRIGHT_UNAVAILABLE in _finding_codes(result.findings)


def test_pyright_session_install_failure(tmp_path: Path) -> None:
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")

    with PyrightSession(
        tmp_path,
        executable=sys.executable,
        install_check=lambda _: False,
    ) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert result.unresolved_call_sites_total == 1
    assert FINDING_PYRIGHT_INSTALL_FAILURE in _finding_codes(result.findings)


class TimeoutSession(PyrightSession):
    def _request(self, method: str, params: dict[str, object], timeout_secs: float) -> object:
        if method == "callHierarchy/outgoingCalls":
            raise LspTimeoutError(method)
        return super()._request(method, params, timeout_secs)


class BudgetProbeSession(PyrightSession):
    def __init__(self, project_root: Path) -> None:
        super().__init__(
            project_root,
            executable=sys.executable,
            call_timeout_secs=10.0,
            file_timeout_secs=0.01,
        )
        self.request_timeouts: list[float] = []

    def _ensure_process(self) -> bool:
        return True

    def _notify(self, method: str, params: dict[str, object]) -> None:
        _ = (method, params)

    def _request(self, method: str, params: dict[str, object], timeout_secs: float) -> object:
        _ = (method, params)
        self.request_timeouts.append(timeout_secs)
        timeout_method = "budget probe"
        raise LspTimeoutError(timeout_method)


@pytest.mark.pyright
def test_pyright_session_call_resolution_timeout(tmp_path: Path, pyright_langserver: str) -> None:
    module = _write_module(
        tmp_path,
        """
        def callee():
            pass

        def caller():
            callee()
        """,
    )

    with TimeoutSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT in _finding_codes(result.findings)


def test_pyright_session_caps_per_file_pyright_budget(tmp_path: Path) -> None:
    module = _write_module(
        tmp_path,
        """
        def caller():
            print('x')
        """,
    )

    with BudgetProbeSession(tmp_path) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert session.request_timeouts
    assert max(session.request_timeouts) <= 0.01
    assert FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT in _finding_codes(result.findings)


def test_pyright_session_stderr_drain(tmp_path: Path) -> None:
    script = _write_executable(
        tmp_path,
        textwrap.dedent(
            """
            #!/usr/bin/env python3
            import json
            import sys

            sys.stderr.write("x" * 131072)
            sys.stderr.flush()

            def read_frame():
                headers = {}
                while True:
                    line = sys.stdin.buffer.readline()
                    if line in (b"", b"\\r\\n"):
                        return None
                    name, value = line.decode("ascii").strip().split(":", 1)
                    headers[name.lower()] = value.strip()
                    if sys.stdin.buffer.readline() == b"\\r\\n":
                        break
                return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

            def write_frame(message):
                body = json.dumps(message).encode("utf-8")
                sys.stdout.buffer.write(
                    b"Content-Length: " + str(len(body)).encode("ascii") + b"\\r\\n\\r\\n"
                )
                sys.stdout.buffer.write(body)
                sys.stdout.buffer.flush()

            while True:
                frame = read_frame()
                if frame is None:
                    break
                method = frame.get("method")
                if method == "initialize":
                    write_frame({"jsonrpc": "2.0", "id": frame["id"], "result": {}})
                elif method == "textDocument/prepareCallHierarchy":
                    write_frame({"jsonrpc": "2.0", "id": frame["id"], "result": []})
                elif method == "callHierarchy/outgoingCalls":
                    write_frame({"jsonrpc": "2.0", "id": frame["id"], "result": []})
                elif method == "shutdown":
                    write_frame({"jsonrpc": "2.0", "id": frame["id"], "result": {}})
                elif method == "exit":
                    break
            """,
        ).lstrip(),
    )
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")

    with PyrightSession(tmp_path, executable=str(script), init_timeout_secs=1.0) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert session.stderr_thread_alive is False


def test_pyright_session_answers_workspace_configuration_requests(tmp_path: Path) -> None:
    marker = tmp_path / "config-marker.txt"
    script = _write_executable(
        tmp_path,
        textwrap.dedent(
            """
            #!/usr/bin/env python3
            import json
            import os
            import sys
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
            write_frame(
                {
                    "jsonrpc": "2.0",
                    "id": 0,
                    "method": "workspace/configuration",
                    "params": {
                        "items": [
                            {"section": "python"},
                            {"section": "python.analysis"},
                            {"section": "pyright"},
                        ],
                    },
                },
            )
            config = read_frame()
            result = config.get("result", [])
            python = result[0].get("analysis", {}) if len(result) > 0 else {}
            analysis = result[1] if len(result) > 1 else {}
            ok = (
                python.get("diagnosticMode") == "openFilesOnly"
                and python.get("indexing") is False
                and "**/.venv/**" in python.get("exclude", [])
                and analysis.get("diagnosticMode") == "openFilesOnly"
                and analysis.get("indexing") is False
                and result[2] == {}
            )
            Path(os.environ["CONFIG_MARKER"]).write_text("ok" if ok else repr(config))
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
        env={"CONFIG_MARKER": str(marker)},
        init_timeout_secs=1.0,
    ) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert marker.read_text(encoding="utf-8") == "ok"
