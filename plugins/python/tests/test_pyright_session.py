from __future__ import annotations

import ast
import contextlib
import errno
import json
import os
import shutil
import stat
import subprocess
import sys
import textwrap
import threading
import time
from pathlib import Path
from typing import TYPE_CHECKING, Any, cast

import pytest

from loomweave_plugin_python import pyright_session as pyright_session_module
from loomweave_plugin_python.call_resolver import CallResolutionResult, FacetCoverage
from loomweave_plugin_python.interpreter import ProjectInterpreter
from loomweave_plugin_python.pyright_session import (
    FILE_DEADLINE_TERMINAL_SAFETY_MARGIN_SECS,
    FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT,
    FINDING_PYRIGHT_INIT_TIMEOUT,
    FINDING_PYRIGHT_INSTALL_FAILURE,
    FINDING_PYRIGHT_POISON_FRAME,
    FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT,
    FINDING_PYRIGHT_REFERENCE_SITE_CAP,
    FINDING_PYRIGHT_RESOURCE_EXHAUSTED,
    FINDING_PYRIGHT_RESTART,
    FINDING_PYRIGHT_SPAWN_DEFERRED,
    FINDING_PYRIGHT_TOTAL_RESTART_CAP_EXCEEDED,
    FINDING_PYRIGHT_UNAVAILABLE,
    FINDING_PYRIGHT_WEDGED_RESTART,
    HOST_FILE_WATCHDOG_SECS,
    MAX_CONSECUTIVE_SPAWN_DEFERRALS,
    MAX_CONSECUTIVE_TIMEOUT_FILES,
    MAX_PYRIGHT_RESTARTS_PER_RUN,
    PYRIGHT_CALL_TIMEOUT_SECS,
    PYRIGHT_FILE_TIMEOUT_BASE_SECS,
    PYRIGHT_FILE_TIMEOUT_CAP_SECS,
    PYRIGHT_FILE_TIMEOUT_PER_LINE_SECS,
    PYRIGHT_SHUTDOWN_TIMEOUT_SECS,
    LspTimeoutError,
    LspTransportClosedError,
    LspWriteTimeoutError,
    PyrightRunState,
    PyrightSession,
    _build_function_index,
    _CallSite,
    _containing_function_id,
    _descendant_pids,
    _filter_relation_candidates,
    _FunctionIndex,
    _FunctionInfo,
    _merge_reference_site,
    _position_to_byte,
    _reference_accumulator_to_edge,
    _reference_fast_path_names,
    _resolved_call_site_keys,
    _unresolved_call_site_total_for_function,
    _unresolved_call_sites_for_function,
    resolve_host_file_watchdog_secs,
)
from loomweave_plugin_python.reference_resolver import ReferenceSite, ReferenceSiteKind

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence
    from typing import NoReturn

    from loomweave_plugin_python.call_resolver import Finding


@pytest.fixture(scope="session")
def pyright_langserver() -> str:
    venv_candidate = Path(sys.executable).parent / "pyright-langserver"
    if venv_candidate.exists():
        return str(venv_candidate)
    resolved = shutil.which("pyright-langserver")
    if resolved is None:
        pytest.fail(
            "pyright-langserver not found on PATH or in the active virtualenv. "
            "It is a hard runtime dependency of loomweave-plugin-python "
            "(pyproject.toml `dependencies`); a missing executable means the "
            "install is broken. Skipping these tests would mask a regression.",
        )
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
        lines=tuple(source.splitlines(keepends=True)),
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

    assert _unresolved_call_site_total_for_function(function, set()) == (1, 0)
    assert _unresolved_call_sites_for_function(index, function, set()) == []


def test_unresolved_call_sites_drop_unshadowed_builtins_and_disclose_the_count() -> None:
    # clarion-8a862d8f7e: ``len(...)`` can never resolve to a project entity;
    # persisting it as an unresolved site is pure noise (10k rows on elspeth).
    source = "def caller(x):\n    len(x)\n    helper(x)\n    str(x)\n"
    tree = ast.parse(source)
    function_node = cast("ast.FunctionDef", tree.body[0])
    index = _FunctionIndex(
        source=source,
        line_starts=(0, len(b"def caller(x):\n")),
        lines=tuple(source.splitlines(keepends=True)),
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
    call_sites = tuple(
        _CallSite(
            line=line, character=4, end_line=line, end_character=4 + len(name), callee_expr=name
        )
        for line, name in ((1, "len"), (2, "helper"), (3, "str"))
    )
    function = _FunctionInfo(
        entity_id="python:function:demo.caller",
        qualified_name="demo.caller",
        name="caller",
        line=0,
        character=4,
        end_line=3,
        end_character=10,
        call_sites=call_sites,
        node=function_node,
    )
    builtin_names = frozenset(_reference_fast_path_names(tree))
    assert {"len", "str"} <= builtin_names

    assert _unresolved_call_site_total_for_function(function, set(), builtin_names) == (1, 2)
    sites = _unresolved_call_sites_for_function(index, function, set(), builtin_names)
    assert [site["callee_expr"] for site in sites] == ["helper"]
    # Shadowed in-file: the builtin shortcut is off, the site is kept.
    shadowed = frozenset(_reference_fast_path_names(ast.parse("def len(x):\n    pass\n")))
    assert "len" not in shadowed
    assert _unresolved_call_site_total_for_function(function, set(), shadowed) == (2, 1)


def test_resolved_call_site_keys_map_pyright_token_ranges_onto_ast_callee_ranges() -> None:
    """pyright anchors an attribute call's ``fromRange`` on the terminal token
    (``helper`` in ``self.helper()``); the AST call site spans the whole callee
    expression. Exact-key matching therefore only ever matched bare ``Name()``
    calls, so every resolved method call was ALSO counted unresolved. A range
    resolves the SMALLEST call site that contains it -- so in
    ``Svc().helper()`` the ``Svc`` token claims the inner site and ``helper``
    the outer one -- and a range inside no site resolves nothing.
    """
    site = _CallSite
    sites = (
        site(line=1, character=4, end_line=1, end_character=15, callee_expr="self.helper"),
        site(line=2, character=4, end_line=2, end_character=16, callee_expr="Svc().helper"),
        site(line=2, character=4, end_line=2, end_character=7, callee_expr="Svc"),
        site(line=3, character=4, end_line=3, end_character=13, callee_expr="free_func"),
        site(line=4, character=4, end_line=5, end_character=9, callee_expr="a.b(\n).c"),
    )
    function = _FunctionInfo(
        entity_id="python:function:demo.f",
        qualified_name="demo.f",
        name="f",
        line=0,
        character=4,
        end_line=6,
        end_character=0,
        call_sites=sites,
        node=cast("ast.FunctionDef", ast.parse("def f():\n    pass\n").body[0]),
    )
    resolved = _resolved_call_site_keys(
        function,
        [
            (1, 9, 1, 15),  # ``helper`` token inside ``self.helper``
            (2, 4, 2, 7),  # ``Svc`` == the inner site exactly
            (2, 10, 2, 16),  # ``helper`` token of ``Svc().helper``
            (3, 4, 3, 13),  # bare name, exact
            (5, 6, 5, 9),  # ``c`` token on the second line of a multi-line callee
            (7, 0, 7, 3),  # inside no call site at all
        ],
    )
    assert resolved == {
        (1, 4, 1, 15),
        (2, 4, 2, 16),
        (2, 4, 2, 7),
        (3, 4, 3, 13),
        (4, 4, 5, 9),
    }


@pytest.mark.skipif(sys.platform != "linux", reason="/proc children walk is Linux-only")
def test_terminate_process_kills_the_language_server_grandchild(tmp_path: Path) -> None:
    """clarion-ebf404dfbb: the venv's pyright-langserver is a Python wrapper
    around a node grandchild; killing the wrapper alone orphaned node (seen
    at 103% CPU, ppid 1, for 1h40m). ``_terminate_process`` must take the
    subtree.
    """
    wrapper = subprocess.Popen(
        ["/bin/sh", "-c", "sleep 300 & wait"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    deadline = time.monotonic() + 5
    grandchildren: list[int] = []
    while not grandchildren and time.monotonic() < deadline:
        grandchildren = _descendant_pids(wrapper.pid)
        time.sleep(0.02)
    assert len(grandchildren) == 1, "the sleep must be visible as a descendant"
    sleeper = grandchildren[0]

    session = PyrightSession(tmp_path)
    session._process = wrapper  # noqa: SLF001
    session._terminate_process()  # noqa: SLF001

    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            os.kill(sleeper, 0)
        except ProcessLookupError:
            break
        # Still present: it may be a zombie awaiting its (dead) parent's
        # reap by init; a live sleeper would still show state S.
        stat = Path(f"/proc/{sleeper}/stat").read_text(encoding="ascii")
        if ") Z " in stat:
            break
        time.sleep(0.02)
    else:
        pytest.fail(f"grandchild {sleeper} outlived _terminate_process")
    assert wrapper.poll() is not None


def _finding_codes(result_findings: Sequence[Finding]) -> set[str]:
    return {str(finding["subcode"]) for finding in result_findings}


def test_pyright_index_uses_declaration_name_token_positions(tmp_path: Path) -> None:
    source = textwrap.dedent(
        """
        def f():
            pass

        def d(d):
            return d

        class c:
            pass

        async def af():
            pass
        """,
    ).lstrip()
    path = _write_module(tmp_path, source)

    index = _build_function_index(tmp_path, path, source)

    assert index.by_id["python:function:demo.f"].character == 4
    assert index.by_id["python:function:demo.d"].character == 4
    assert index.entity_by_name_position[(6, 6)] == "python:class:demo.c"
    assert index.by_id["python:function:demo.af"].character == 10


def test_containing_function_fallback_prefers_deepest_span(tmp_path: Path) -> None:
    source = textwrap.dedent(
        """
        def outer():
            def inner():
                return helper()
            return inner()
        """,
    ).lstrip()
    path = _write_module(tmp_path, source)
    index = _build_function_index(tmp_path, path, source)

    assert (
        _containing_function_id(
            index,
            {
                "start": {"line": 2, "character": 15},
                "end": {"line": 2, "character": 21},
            },
        )
        == "python:function:demo.outer.<locals>.inner"
    )


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
def test_pyright_session_resolves_class_instantiation_as_call_to_class(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    """clarion-e5224c3aff: ``Name(...)`` naming a class is a call to that class.

    pyright reports every instantiation as an outgoing call whose target IS the
    class item (its selection range sits on the class name), whether the class
    defines ``__init__`` itself, inherits it, or gets it synthesised by
    ``@dataclass``. Those targets used to be looked up only in the FUNCTION
    index and so were silently dropped as unresolved -- an index of a real
    project held zero ``calls`` edges into any ``python:class:*`` entity.
    """
    _write_module(
        tmp_path,
        """
        from dataclasses import dataclass


        class WithInit:
            def __init__(self, value: int) -> None:
                self.value = value


        class Child(WithInit):
            pass


        @dataclass
        class Record:
            value: int
        """,
        name="models.py",
    )
    module = _write_module(
        tmp_path,
        """
        from models import Child, Record, WithInit


        class Local:
            pass


        def build():
            WithInit(1)
            Child(2)
            Record(3)
            Local()
        """,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_calls(module, ["python:function:demo.build"])

    assert [(edge["from_id"], edge["to_id"], edge["confidence"]) for edge in result.edges] == [
        ("python:function:demo.build", "python:class:models.WithInit", "resolved"),
        ("python:function:demo.build", "python:class:models.Child", "resolved"),
        ("python:function:demo.build", "python:class:models.Record", "resolved"),
        ("python:function:demo.build", "python:class:demo.Local", "resolved"),
    ]
    assert result.unresolved_call_sites_total == 0
    assert result.unresolved_call_sites == []


@pytest.mark.pyright
def test_pyright_session_resolved_attribute_calls_are_not_double_counted_as_unresolved(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    """A method/attribute call pyright resolved must not ALSO be an unresolved site.

    Found while fixing clarion-e5224c3aff: ``self.helper()``, ``svc.free_func()``
    and ``s.run()`` each produced a resolved edge AND an unresolved site,
    because the site match keyed on pyright's token range. The partition
    (resolved + unresolved + skipped builtins == AST call sites) is what the
    host and ``entity_callers_list``'s ``traversal_complete`` rely on.
    """
    module = _write_module(
        tmp_path,
        """
        def free_func() -> int:
            return 1


        class Svc:
            def helper(self) -> int:
                return 1

            def run(self) -> int:
                return self.helper()


        def outer() -> int:
            import demo

            s = Svc()
            return demo.free_func() + s.run() + Svc().helper()
        """,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_calls(
            module,
            ["python:function:demo.Svc.run", "python:function:demo.outer"],
        )

    assert sorted((edge["from_id"], edge["to_id"]) for edge in result.edges) == [
        ("python:function:demo.Svc.run", "python:function:demo.Svc.helper"),
        ("python:function:demo.outer", "python:class:demo.Svc"),
        ("python:function:demo.outer", "python:class:demo.Svc"),
        ("python:function:demo.outer", "python:function:demo.Svc.helper"),
        ("python:function:demo.outer", "python:function:demo.Svc.run"),
        ("python:function:demo.outer", "python:function:demo.free_func"),
    ]
    assert result.unresolved_call_sites == []
    assert result.unresolved_call_sites_total == 0


@pytest.mark.pyright
def test_pyright_session_overload_index_uses_implementation_body(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    module = _write_module(
        tmp_path,
        """
        from typing import overload

        def helper(value: object) -> object:
            return value

        @overload
        def parse(value: str) -> str: ...

        @overload
        def parse(value: int) -> int: ...

        def parse(value: object) -> object:
            return helper(value)
        """,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_calls(
            module,
            [
                "python:function:demo.helper",
                "python:function:demo.parse",
            ],
        )

    assert [(edge["from_id"], edge["to_id"], edge["confidence"]) for edge in result.edges] == [
        (
            "python:function:demo.parse",
            "python:function:demo.helper",
            "resolved",
        ),
    ]


@pytest.mark.pyright
def test_pyright_session_call_range_uses_utf16_lsp_positions_but_emits_bytes(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        def callee():
            pass

        def caller():
            marker = "🐍"; callee()
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_calls(
            module,
            ["python:function:demo.caller", "python:function:demo.callee"],
        )

    assert len(result.edges) == 1
    edge = result.edges[0]
    assert edge["source_byte_start"] == source.encode().find(
        b"callee", source.encode().find(b"marker")
    )
    assert edge["source_byte_end"] == edge["source_byte_start"] + len(b"callee")
    assert source.encode()[edge["source_byte_start"] : edge["source_byte_end"]] == b"callee"


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


def _accumulated_edges(
    site: ReferenceSite,
    candidate_ids: list[str],
) -> list[dict[str, object]]:
    accumulators: dict[tuple[str, str, str], object] = {}
    _merge_reference_site(accumulators, site, candidate_ids)  # type: ignore[arg-type]
    return [
        cast("dict[str, object]", _reference_accumulator_to_edge(acc))  # type: ignore[arg-type]
        for acc in accumulators.values()
    ]


def test_merge_base_site_accumulates_inherits_from_edge() -> None:
    source = "class Base:\n    pass\n\nclass Child(Base):\n    pass\n"
    site = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Base",
        kind="base",
        occurrence=1,
    )

    assert _accumulated_edges(site, ["python:class:demo.Base"]) == [
        {
            "kind": "inherits_from",
            "from_id": "python:class:demo.Child",
            "to_id": "python:class:demo.Base",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
            "confidence": "resolved",
        },
    ]


def test_merge_decorator_site_inverts_direction_for_decorates_edge() -> None:
    source = "def deco(fn):\n    return fn\n\n@deco\ndef target():\n    pass\n"
    site = _reference_site(
        source,
        from_id="python:function:demo.target",
        needle="deco",
        kind="decorator",
        occurrence=1,
    )

    assert _accumulated_edges(site, ["python:function:demo.deco"]) == [
        {
            "kind": "decorates",
            "from_id": "python:function:demo.deco",
            "to_id": "python:function:demo.target",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
            "confidence": "resolved",
        },
    ]


def test_merge_keeps_same_pair_distinct_across_edge_kinds() -> None:
    source = "class Base:\n    pass\n\nclass Child(Base):\n    x: Base\n"
    base_site = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Base",
        kind="base",
        occurrence=1,
    )
    annotation_site = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Base",
        kind="annotation",
        occurrence=2,
    )

    accumulators: dict[tuple[str, str, str], object] = {}
    _merge_reference_site(accumulators, base_site, ["python:class:demo.Base"])  # type: ignore[arg-type]
    _merge_reference_site(accumulators, annotation_site, ["python:class:demo.Base"])  # type: ignore[arg-type]

    kinds = sorted(
        cast("dict[str, str]", _reference_accumulator_to_edge(acc))["kind"]  # type: ignore[arg-type]
        for acc in accumulators.values()
    )
    assert kinds == ["inherits_from", "references"]


def test_filter_relation_candidates_enforces_kind_and_self_edge_discipline() -> None:
    source = "class Base:\n    pass\n\nclass Child(Base):\n    pass\n"
    base_site = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Base",
        kind="base",
        occurrence=1,
    )
    deco_source = "def deco(fn):\n    return fn\n\n@deco\ndef target():\n    pass\n"
    decorator_site = _reference_site(
        deco_source,
        from_id="python:function:demo.target",
        needle="deco",
        kind="decorator",
        occurrence=1,
    )
    name_site = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Base",
        kind="name",
        occurrence=1,
    )

    # Base targets: class entities only, and never the subclass itself.
    assert _filter_relation_candidates(
        base_site,
        [
            "python:function:demo.make",
            "python:class:demo.Base",
            "python:class:demo.Child",
            "python:module:demo",
        ],
    ) == ["python:class:demo.Base"]
    # Decorator candidates: functions and classes both decorate; self dropped.
    assert _filter_relation_candidates(
        decorator_site,
        [
            "python:function:demo.target",
            "python:function:demo.deco",
            "python:class:demo.Deco",
        ],
    ) == ["python:function:demo.deco", "python:class:demo.Deco"]
    # Plain reference sites are untouched.
    candidates = ["python:module:demo", "python:class:demo.Child"]
    assert _filter_relation_candidates(name_site, candidates) == candidates


def test_target_id_from_location_relation_sites_skip_module_fallback(tmp_path: Path) -> None:
    """Relation sites resolve to precise entities only — no module-id coarse
    fallback (Rust parity: an alias/assignment target is dropped like an
    External derive, not coarsened to the defining module)."""
    source = "class Base:\n    pass\n\nAlias = Base\n"
    path = _write_module(tmp_path, source)
    session = PyrightSession(tmp_path)
    # Location of the `Alias` assignment target: a declaration position that
    # is not an entity name-token position.
    location = {
        "uri": path.as_uri(),
        "range": {
            "start": {"line": 3, "character": 0},
            "end": {"line": 3, "character": 5},
        },
    }

    assert session._target_id_from_location(location) == (  # noqa: SLF001
        "python:module:demo",
        False,
    )
    assert session._target_id_from_location(  # noqa: SLF001
        location,
        precise_only=True,
    ) == (None, False)


@pytest.mark.pyright
def test_pyright_session_resolves_base_site_to_inherits_from_edge(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        class Base:
            pass

        class Child(Base):
            pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Base",
        kind="base",
        occurrence=1,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == [
        {
            "kind": "inherits_from",
            "from_id": "python:class:demo.Child",
            "to_id": "python:class:demo.Base",
            "confidence": "resolved",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
        },
    ]
    assert result.references_resolved_total == 1
    assert result.unresolved_reference_sites_total == 0


@pytest.mark.pyright
def test_pyright_session_resolves_decorator_site_to_decorates_edge(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        def deco(fn):
            return fn

        @deco
        def target():
            pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:function:demo.target",
        needle="deco",
        kind="decorator",
        occurrence=1,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == [
        {
            "kind": "decorates",
            "from_id": "python:function:demo.deco",
            "to_id": "python:function:demo.target",
            "confidence": "resolved",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
        },
    ]


@pytest.mark.pyright
def test_pyright_session_skips_external_base_target(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = "class Boom(Exception):\n    pass\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:class:demo.Boom",
        needle="Exception",
        kind="base",
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == []
    assert result.references_skipped_external_total == 1
    assert result.unresolved_reference_sites_total == 1


@pytest.mark.pyright
def test_pyright_session_resolves_nested_builtin_shadow_base(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        def outer():
            class Exception:
                pass

            class Child(Exception):
                pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:class:demo.outer.<locals>.Child",
        needle="Exception",
        kind="base",
        occurrence=1,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == [
        {
            "kind": "inherits_from",
            "from_id": "python:class:demo.outer.<locals>.Child",
            "to_id": "python:class:demo.outer.<locals>.Exception",
            "confidence": "resolved",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
        },
    ]


@pytest.mark.pyright
def test_pyright_session_resolves_nested_builtin_shadow_decorator(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        def outer():
            def property(fn):
                return fn

            @property
            def target():
                pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:function:demo.outer.<locals>.target",
        needle="property",
        kind="decorator",
        occurrence=1,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == [
        {
            "kind": "decorates",
            "from_id": "python:function:demo.outer.<locals>.property",
            "to_id": "python:function:demo.outer.<locals>.target",
            "confidence": "resolved",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
        },
    ]


@pytest.mark.pyright
def test_pyright_session_base_resolving_to_function_is_dropped(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    """`inherits_from` targets are class entities only: a base name that
    resolves to a function (factory aliases, `def base(): ...` shadowing)
    yields no edge rather than a class-inherits-function fact."""
    source = textwrap.dedent(
        """
        def base():
            return object

        class Child(base):
            pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="base",
        kind="base",
        occurrence=1,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == []
    assert result.unresolved_reference_sites_total == 1


@pytest.mark.pyright
def test_pyright_session_self_decoration_via_redefinition_is_filtered(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    """The relation discipline (precise_only + self-edge drop) is load-bearing.

    ``@helper`` above the redefining ``def helper`` resolves at this position
    (a control query with ``kind="name"`` yields an ambiguous references edge
    whose candidates include ``python:function:demo.helper`` plus the
    module-fallback id), so the empty edge list here is produced by the
    discipline, not by pyright failing to resolve: precise_only drops the
    module-fallback candidate and the self-edge filter drops the function id
    (first-wins dedup gives both ``helper`` definitions one entity id).
    """
    source = textwrap.dedent(
        """
        def helper(fn):
            return fn

        @helper
        def helper():
            pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:function:demo.helper",
        needle="helper",
        kind="decorator",
        occurrence=2,
    )

    with PyrightSession(tmp_path, executable=pyright_langserver) as session:
        result = session.resolve_references(module, [site])

    assert result.edges == []
    # Counted as unresolved: the candidates existed but were disciplined away.
    assert result.unresolved_reference_sites_total == 1


def test_merge_ambiguous_base_site_emits_candidates_payload() -> None:
    source = "class Base:\n    pass\n\nclass Child(Base):\n    pass\n"
    site = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Base",
        kind="base",
        occurrence=1,
    )

    assert _accumulated_edges(
        site,
        ["python:class:other.Base", "python:class:demo.Base"],
    ) == [
        {
            "kind": "inherits_from",
            "from_id": "python:class:demo.Child",
            "to_id": "python:class:demo.Base",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
            "confidence": "ambiguous",
            "properties": {
                "candidates": ["python:class:demo.Base", "python:class:other.Base"],
            },
        },
    ]


def test_merge_ambiguous_decorator_site_candidates_are_from_side() -> None:
    """Ambiguous `decorates` candidates list alternative FROM-side decorator
    entities (direction is inverted), with from_id = the sorted-first one."""
    source = "def deco(fn):\n    return fn\n\n@deco\ndef target():\n    pass\n"
    site = _reference_site(
        source,
        from_id="python:function:demo.target",
        needle="deco",
        kind="decorator",
        occurrence=1,
    )

    assert _accumulated_edges(
        site,
        ["python:function:demo.deco", "python:class:demo.Deco"],
    ) == [
        {
            "kind": "decorates",
            "from_id": "python:class:demo.Deco",
            "to_id": "python:function:demo.target",
            "source_byte_start": site.source_byte_start,
            "source_byte_end": site.source_byte_end,
            "confidence": "ambiguous",
            "properties": {
                "candidates": ["python:class:demo.Deco", "python:function:demo.deco"],
            },
        },
    ]


def test_pyright_session_reference_unavailable_binary_missing(tmp_path: Path) -> None:
    source = "def world():\n    pass\n\nCONST_REF = world\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(source, from_id="python:module:demo", needle="world", occurrence=1)

    with PyrightSession(tmp_path, executable="loomweave-missing-pyright") as session:
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

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = (method, params, deadline)

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

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = (method, params, deadline)

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


class ExternalReferenceSession(PyrightSession):
    def __init__(self, project_root: Path) -> None:
        super().__init__(project_root, executable=sys.executable)
        self.requested_starts: list[int] = []

    def _ensure_process(self) -> bool:
        return True

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = (method, params, deadline)

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
        return [], True


class MappedReferenceSession(PyrightSession):
    def __init__(
        self,
        project_root: Path,
        *,
        results_by_start: dict[int, tuple[list[str], bool]],
    ) -> None:
        super().__init__(project_root, executable=sys.executable)
        self.results_by_start = results_by_start
        self.requested_starts: list[int] = []

    def _ensure_process(self) -> bool:
        return True

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = (method, params, deadline)

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
        return self.results_by_start[site.source_byte_start]


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
    assert result.coverage.is_degraded
    assert result.coverage.reason == "pyright_timeout"
    assert result.coverage.transient is True


def test_pyright_session_reference_lookup_cache_includes_source_position(tmp_path: Path) -> None:
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

    assert requested_starts == [first.source_byte_start, second.source_byte_start]
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


def test_pyright_session_skips_unshadowed_builtin_without_query(tmp_path: Path) -> None:
    source = "VALUE: str\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:module:demo",
        needle="str",
        kind="annotation",
    )

    with CountingReferenceSession(
        tmp_path,
        target_id="python:class:demo.str",
    ) as session:
        result = session.resolve_references(module, [site])
        requested_starts = session.requested_starts

    assert requested_starts == []
    assert result.edges == []
    assert result.reference_sites_total == 1
    assert result.references_skipped_external_total == 1
    assert result.unresolved_reference_sites_total == 1


@pytest.mark.parametrize(
    ("source", "occurrence"),
    [
        ("class str:\n    pass\n\nVALUE: str\n", 1),
        ("from custom_builtins import *\n\nVALUE: str\n", 0),
    ],
)
def test_pyright_session_queries_potentially_shadowed_builtin(
    tmp_path: Path,
    source: str,
    occurrence: int,
) -> None:
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:module:demo",
        needle="str",
        kind="annotation",
        occurrence=occurrence,
    )

    with CountingReferenceSession(
        tmp_path,
        target_id="python:class:demo.str",
    ) as session:
        result = session.resolve_references(module, [site])
        requested_starts = session.requested_starts

    assert requested_starts == [site.source_byte_start]
    assert result.references_resolved_total == 1
    assert result.references_skipped_external_total == 0
    assert result.unresolved_reference_sites_total == 0


def test_pyright_session_queries_implicit_module_name(tmp_path: Path) -> None:
    source = 'if __name__ == "__main__":\n    pass\n'
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:module:demo",
        needle="__name__",
    )

    with CountingReferenceSession(
        tmp_path,
        target_id="python:module:demo",
    ) as session:
        result = session.resolve_references(module, [site])
        requested_starts = session.requested_starts

    assert requested_starts == [site.source_byte_start]
    assert result.references_resolved_total == 1
    assert result.references_skipped_external_total == 0
    assert result.unresolved_reference_sites_total == 0


@pytest.mark.skipif(
    sys.version_info < (3, 12),
    reason="PEP 695 type parameter syntax requires Python 3.12",
)
def test_pyright_session_queries_builtin_named_type_parameter(tmp_path: Path) -> None:
    source = "def identity[str](value: str) -> str:\n    return value\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(
        source,
        from_id="python:function:demo.identity",
        needle="str",
        kind="annotation",
        occurrence=1,
    )

    with CountingReferenceSession(
        tmp_path,
        target_id="python:module:demo",
    ) as session:
        result = session.resolve_references(module, [site])
        requested_starts = session.requested_starts

    assert requested_starts == [site.source_byte_start]
    assert result.references_resolved_total == 1
    assert result.references_skipped_external_total == 0


def test_pyright_session_queries_repeated_external_import_positions(tmp_path: Path) -> None:
    source = textwrap.dedent(
        """
        from external_lib import Remote

        FIRST: Remote
        SECOND: Remote
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    first = _reference_site(
        source,
        from_id="python:module:demo",
        needle="Remote",
        kind="annotation",
        occurrence=1,
    )
    second = _reference_site(
        source,
        from_id="python:module:demo",
        needle="Remote",
        kind="annotation",
        occurrence=2,
    )

    with ExternalReferenceSession(tmp_path) as session:
        result = session.resolve_references(module, [first, second])
        requested_starts = session.requested_starts

    assert requested_starts == [first.source_byte_start, second.source_byte_start]
    assert result.edges == []
    assert result.reference_sites_total == 2
    assert result.references_skipped_external_total == 2
    assert result.unresolved_reference_sites_total == 2


def test_pyright_session_does_not_reuse_unconfirmed_import_result(tmp_path: Path) -> None:
    source = textwrap.dedent(
        """
        from project_lib import Remote

        FIRST: Remote
        SECOND: Remote
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    first = _reference_site(
        source,
        from_id="python:module:demo",
        needle="Remote",
        kind="annotation",
        occurrence=1,
    )
    second = _reference_site(
        source,
        from_id="python:module:demo",
        needle="Remote",
        kind="annotation",
        occurrence=2,
    )

    with CountingReferenceSession(
        tmp_path,
        target_id="python:class:project_lib.Remote",
    ) as session:
        result = session.resolve_references(module, [first, second])
        requested_starts = session.requested_starts

    assert requested_starts == [first.source_byte_start, second.source_byte_start]
    assert result.references_resolved_total == 2
    assert result.references_skipped_external_total == 0
    assert result.unresolved_reference_sites_total == 0


def test_pyright_session_does_not_cache_rebound_import_as_external(tmp_path: Path) -> None:
    source = textwrap.dedent(
        """
        from external_lib import Remote

        class Remote:
            pass

        FIRST: Remote
        SECOND: Remote
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    first = _reference_site(
        source,
        from_id="python:module:demo",
        needle="Remote",
        kind="annotation",
        occurrence=2,
    )
    second = _reference_site(
        source,
        from_id="python:module:demo",
        needle="Remote",
        kind="annotation",
        occurrence=3,
    )

    with ExternalReferenceSession(tmp_path) as session:
        result = session.resolve_references(module, [first, second])
        requested_starts = session.requested_starts

    assert requested_starts == [first.source_byte_start, second.source_byte_start]
    assert result.references_skipped_external_total == 2
    assert result.unresolved_reference_sites_total == 2


def test_pyright_session_does_not_reuse_external_result_across_dotted_targets(
    tmp_path: Path,
) -> None:
    source = textwrap.dedent(
        """
        import pkg

        class External(pkg.Path):
            pass

        class Local(pkg.Local):
            pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    external = _reference_site(
        source,
        from_id="python:class:demo.External",
        needle="pkg.Path",
        kind="base",
    )
    local = _reference_site(
        source,
        from_id="python:class:demo.Local",
        needle="pkg.Local",
        kind="base",
    )

    with MappedReferenceSession(
        tmp_path,
        results_by_start={
            external.source_byte_start: ([], True),
            local.source_byte_start: (["python:class:project.Local"], False),
        },
    ) as session:
        result = session.resolve_references(module, [external, local])
        requested_starts = session.requested_starts

    assert requested_starts == [external.source_byte_start, local.source_byte_start]
    assert result.edges == [
        {
            "kind": "inherits_from",
            "from_id": "python:class:demo.Local",
            "to_id": "python:class:project.Local",
            "confidence": "resolved",
            "source_byte_start": local.source_byte_start,
            "source_byte_end": local.source_byte_end,
        },
    ]
    assert result.references_resolved_total == 1
    assert result.references_skipped_external_total == 1
    assert result.unresolved_reference_sites_total == 1


def test_pyright_session_does_not_reuse_external_result_across_owners(
    tmp_path: Path,
) -> None:
    source = textwrap.dedent(
        """
        import pkg

        class External(pkg.Base):
            pass

        def outer():
            class pkg:
                class Base:
                    pass

            class Local(pkg.Base):
                pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    external = _reference_site(
        source,
        from_id="python:class:demo.External",
        needle="pkg.Base",
        kind="base",
    )
    local = _reference_site(
        source,
        from_id="python:class:demo.outer.<locals>.Local",
        needle="pkg.Base",
        kind="base",
        occurrence=1,
    )

    with MappedReferenceSession(
        tmp_path,
        results_by_start={
            external.source_byte_start: ([], True),
            local.source_byte_start: (
                ["python:class:demo.outer.<locals>.pkg.Base"],
                False,
            ),
        },
    ) as session:
        result = session.resolve_references(module, [external, local])
        requested_starts = session.requested_starts

    assert requested_starts == [external.source_byte_start, local.source_byte_start]
    assert result.references_resolved_total == 1
    assert result.references_skipped_external_total == 1


def test_pyright_session_does_not_reuse_external_result_across_site_kinds(
    tmp_path: Path,
) -> None:
    source = textwrap.dedent(
        """
        from external_lib import Remote

        class Child(Remote):
            value: Remote
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    base = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Remote",
        kind="base",
        occurrence=1,
    )
    annotation = _reference_site(
        source,
        from_id="python:class:demo.Child",
        needle="Remote",
        kind="annotation",
        occurrence=2,
    )

    with MappedReferenceSession(
        tmp_path,
        results_by_start={
            base.source_byte_start: ([], True),
            annotation.source_byte_start: (["python:class:project.Remote"], False),
        },
    ) as session:
        result = session.resolve_references(module, [base, annotation])
        requested_starts = session.requested_starts

    assert requested_starts == [base.source_byte_start, annotation.source_byte_start]
    assert result.references_resolved_total == 1
    assert result.references_skipped_external_total == 1


def test_pyright_session_does_not_reuse_external_result_after_same_owner_rebinding(
    tmp_path: Path,
) -> None:
    source = textwrap.dedent(
        """
        from external_lib import deco

        @deco
        @(deco := local_deco)
        @deco
        def target():
            pass
        """,
    ).lstrip()
    module = _write_module(tmp_path, source)
    first = _reference_site(
        source,
        from_id="python:function:demo.target",
        needle="deco",
        kind="decorator",
        occurrence=1,
    )
    second = _reference_site(
        source,
        from_id="python:function:demo.target",
        needle="deco",
        kind="decorator",
        occurrence=4,
    )

    with MappedReferenceSession(
        tmp_path,
        results_by_start={
            first.source_byte_start: ([], True),
            second.source_byte_start: (["python:function:demo.local_deco"], False),
        },
    ) as session:
        result = session.resolve_references(module, [first, second])
        requested_starts = session.requested_starts

    assert requested_starts == [first.source_byte_start, second.source_byte_start]
    assert result.references_resolved_total == 1
    assert result.references_skipped_external_total == 1


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
    # clarion-3e517d4aff: once the run is poisoned, every later file is
    # collateral -- degraded, transient, and NOT this file's doing. The host
    # dispatches collateral files first so the troublemaker goes last.
    assert continued.coverage.is_degraded
    assert continued.coverage.reason == "pyright_poisoned"
    assert continued.coverage.transient is True
    assert continued.coverage.collateral is True
    assert session.run_state.disabled_reason == "pyright_poisoned"


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

    with PyrightSession(tmp_path, executable="loomweave-missing-pyright") as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.edges == []
    assert result.unresolved_call_sites_total == 1
    assert FINDING_PYRIGHT_UNAVAILABLE in _finding_codes(result.findings)
    assert result.coverage.is_degraded
    assert result.coverage.reason == "pyright_unavailable"
    assert result.coverage.transient is True


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


def _popen_raising(err: int) -> Callable[..., NoReturn]:
    def _factory(*args: object, **kwargs: object) -> NoReturn:
        _ = (args, kwargs)
        raise OSError(err, os.strerror(err))

    return _factory


def test_transient_spawn_failure_defers_without_disabling(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """EAGAIN on spawn is transient: skip the file, retry next, never poison."""
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")
    monkeypatch.setattr(subprocess, "Popen", _popen_raising(errno.EAGAIN))
    run_state = PyrightRunState()

    with PyrightSession(tmp_path, executable=sys.executable, run_state=run_state) as session:
        first = session.resolve_calls(module, ["python:function:demo.caller"])
        second = session.resolve_calls(module, ["python:function:demo.caller"])

    # A transient resource squeeze must NOT permanently disable pyright...
    assert run_state.disabled is False
    # ...and every file re-attempts the spawn (skip-and-continue).
    assert run_state.consecutive_spawn_deferrals == 2
    # One finding per pressure episode (the 0 -> 1 transition), not per file,
    # and never the permanent install-failure poison.
    assert FINDING_PYRIGHT_SPAWN_DEFERRED in _finding_codes(first.findings)
    assert FINDING_PYRIGHT_SPAWN_DEFERRED not in _finding_codes(second.findings)
    assert FINDING_PYRIGHT_INSTALL_FAILURE not in _finding_codes(first.findings)
    assert first.edges == []
    assert first.unresolved_call_sites_total == 1
    assert second.unresolved_call_sites_total == 1


def test_permanent_spawn_failure_disables(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A non-transient errno (ENOENT) is a genuine install defect: disable."""
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")
    monkeypatch.setattr(subprocess, "Popen", _popen_raising(errno.ENOENT))
    run_state = PyrightRunState()

    with PyrightSession(tmp_path, executable=sys.executable, run_state=run_state) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert run_state.disabled is True
    assert FINDING_PYRIGHT_INSTALL_FAILURE in _finding_codes(result.findings)
    assert FINDING_PYRIGHT_SPAWN_DEFERRED not in _finding_codes(result.findings)
    assert result.unresolved_call_sites_total == 1


def test_sustained_spawn_pressure_trips_resource_exhausted(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Unrelenting EAGAIN eventually gives up — with its own finding, not poison."""
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")
    monkeypatch.setattr(subprocess, "Popen", _popen_raising(errno.EAGAIN))
    run_state = PyrightRunState()
    codes: set[str] = set()

    with PyrightSession(tmp_path, executable=sys.executable, run_state=run_state) as session:
        for _ in range(MAX_CONSECUTIVE_SPAWN_DEFERRALS + 1):
            result = session.resolve_calls(module, ["python:function:demo.caller"])
            codes |= _finding_codes(result.findings)

    assert run_state.disabled is True
    assert FINDING_PYRIGHT_RESOURCE_EXHAUSTED in codes
    # The soft-stop is distinct from the install-failure poison.
    assert FINDING_PYRIGHT_INSTALL_FAILURE not in codes


@pytest.mark.pyright
def test_successful_spawn_resets_deferral_counter(
    tmp_path: Path,
    pyright_langserver: str,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """After a deferred file, a clean spawn clears the pressure counter."""
    module = _write_module(
        tmp_path,
        """
        def callee():
            pass

        def caller():
            callee()
        """,
    )
    real_popen = cast("Callable[..., subprocess.Popen[bytes]]", subprocess.Popen)
    calls = {"n": 0}

    def flaky_popen(*args: object, **kwargs: object) -> subprocess.Popen[bytes]:
        # Fail only the *first pyright* spawn. _start_process incidentally shells
        # out via ctypes.util.find_library (ldconfig/gcc/objdump); those must pass
        # through to the real Popen, or the injected EAGAIN lands on the wrong call.
        argv = args[0] if args else kwargs.get("args")
        executable = argv[0] if isinstance(argv, (list, tuple)) and argv else None
        if isinstance(executable, str) and executable.endswith("pyright-langserver"):
            calls["n"] += 1
            if calls["n"] == 1:
                raise OSError(errno.EAGAIN, os.strerror(errno.EAGAIN))
        return real_popen(*args, **kwargs)

    monkeypatch.setattr(subprocess, "Popen", flaky_popen)
    run_state = PyrightRunState()
    function_ids = ["python:function:demo.caller", "python:function:demo.callee"]

    with PyrightSession(tmp_path, executable=pyright_langserver, run_state=run_state) as session:
        deferred = session.resolve_calls(module, function_ids)
        resolved = session.resolve_calls(module, function_ids)

    assert FINDING_PYRIGHT_SPAWN_DEFERRED in _finding_codes(deferred.findings)
    assert deferred.edges == []
    # The second file spawned cleanly: not disabled and the counter is reset.
    assert run_state.disabled is False
    assert run_state.consecutive_spawn_deferrals == 0
    assert resolved.edges


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
            file_timeout_base_secs=0.01,
            file_timeout_per_function_secs=0.0,
            file_timeout_per_line_secs=0.0,
        )
        self.request_timeouts: list[float] = []

    def _ensure_process(self) -> bool:
        return True

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = (method, params, deadline)

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
    # clarion-3e517d4aff: empty evidence from a timeout is NOT a call-free
    # file; the claim must be degraded + transient so the host re-dispatches.
    assert result.coverage.is_degraded
    assert result.coverage.reason == "pyright_timeout"
    assert result.coverage.transient is True


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


# --- clarion-7f527d3d32: partial evidence on abort + size-proportional budget


def test_pyright_session_file_deadline_scales_with_function_count(tmp_path: Path) -> None:
    session = PyrightSession(
        tmp_path,
        executable=sys.executable,
        file_timeout_base_secs=2.0,
        file_timeout_per_function_secs=0.5,
    )
    path = tmp_path / "demo.py"

    before = time.monotonic()
    deadline = session._deadline_for_file(path, n_functions=6, n_lines=0)  # noqa: SLF001
    after = time.monotonic()

    assert before + 5.0 <= deadline <= after + 5.0
    # The deadline is memoised per file: the references pass re-asks with the
    # same path and must share the calls pass's budget, not restart it.
    assert session._deadline_for_file(path, n_functions=1000, n_lines=0) == deadline  # noqa: SLF001


def test_default_call_timeout_admits_a_large_file_warm_up_query() -> None:
    """clarion-5d83413c36: pyright's FIRST callHierarchy query on a big file
    triggers full analysis of that file and took >5 s on elspeth's 5k-line
    modules, aborting the whole calls pass. 30 s covers the measured warm-up
    (the files then finish in ~11 s total) while the file budget still bounds
    a wedged server.
    """
    assert PYRIGHT_CALL_TIMEOUT_SECS == 30.0
    assert PYRIGHT_CALL_TIMEOUT_SECS < PYRIGHT_FILE_TIMEOUT_CAP_SECS


def test_budgeted_timeout_grants_the_call_timeout_when_the_file_budget_is_larger(
    tmp_path: Path,
) -> None:
    session = PyrightSession(tmp_path, executable=sys.executable)
    path = tmp_path / "big.py"
    # 290 functions → base 10 + 0.25*290 = 82.5 s file budget (< 90 s cap).
    deadline = session._deadline_for_file(path, n_functions=290, n_lines=0)  # noqa: SLF001
    grant = session._budgeted_timeout(deadline)  # noqa: SLF001
    assert 30.0 - 0.5 <= grant <= 30.0
    # ...and never more than what is left of the file budget.
    session._file_deadlines[path] = session._now() + 4.0  # noqa: SLF001
    assert session._budgeted_timeout(session._file_deadlines[path]) <= 4.0  # noqa: SLF001


class _IgnoresShutdownSession(PyrightSession):
    """A process that never answers ``shutdown``, recording the timeout used."""

    def __init__(self, project_root: Path) -> None:
        super().__init__(project_root, executable=sys.executable)
        self.request_timeouts: list[tuple[str, float]] = []
        self._process = cast("Any", _FakeProcess())

    def _request(self, method: str, params: dict[str, object], timeout_secs: float) -> object:
        _ = params
        self.request_timeouts.append((method, timeout_secs))
        if method == "shutdown":
            raise LspTimeoutError(method)
        return None

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = (method, params, deadline)


def test_close_uses_the_shutdown_timeout_not_the_call_timeout(tmp_path: Path) -> None:
    """clarion-5d83413c36 follow-up: close()'s ``shutdown`` request must not
    inherit the 30 s analyze-path call-timeout grant. An unresponsive server
    at ``MAX_FILES_PER_PYRIGHT_SESSION`` recycle (server.py) or plugin
    shutdown (server.py) must still fall back to ``kill()`` within the
    pre-existing ~5 s budget, not wait up to 30 s.
    """
    session = _IgnoresShutdownSession(tmp_path)
    fake_process = cast("_FakeProcess", session._process)  # noqa: SLF001

    session.close()

    # One deadline spans the whole teardown, so the shutdown request is
    # granted the shutdown budget minus whatever close() already spent.
    assert [method for method, _ in session.request_timeouts] == ["shutdown"]
    granted = session.request_timeouts[0][1]
    assert PYRIGHT_SHUTDOWN_TIMEOUT_SECS - 0.5 <= granted <= PYRIGHT_SHUTDOWN_TIMEOUT_SECS
    assert PYRIGHT_SHUTDOWN_TIMEOUT_SECS < PYRIGHT_CALL_TIMEOUT_SECS
    assert fake_process.killed


def test_pyright_session_file_deadline_is_capped_for_very_large_files(tmp_path: Path) -> None:
    session = PyrightSession(
        tmp_path,
        executable=sys.executable,
        file_timeout_base_secs=2.0,
        file_timeout_per_function_secs=0.5,
        file_timeout_cap_secs=10.0,
    )

    before = time.monotonic()
    deadline = session._deadline_for_file(tmp_path / "demo.py", n_functions=1000, n_lines=0)  # noqa: SLF001
    after = time.monotonic()

    assert before + 10.0 <= deadline <= after + 10.0


def test_pyright_session_file_budget_scales_with_lines_for_def_sparse_files(
    tmp_path: Path,
) -> None:
    # clarion-bf3986e301: elspeth's tool_batch.py (2,354 lines, 17 defs) got
    # base + 17 * 0.25 ~= 14 s under the per-function-only budget. The line
    # term buys a def-sparse monster the time its code volume demands; the
    # per-function term still governs the common many-small-defs shape, and
    # the cap still bounds both.
    session = PyrightSession(tmp_path, executable=sys.executable)

    sparse = session._file_timeout_for(17, 2354)  # noqa: SLF001
    assert sparse == pytest.approx(
        PYRIGHT_FILE_TIMEOUT_BASE_SECS + 2354 * PYRIGHT_FILE_TIMEOUT_PER_LINE_SECS
    )
    dense = session._file_timeout_for(200, 400)  # noqa: SLF001
    assert dense == pytest.approx(PYRIGHT_FILE_TIMEOUT_BASE_SECS + 200 * 0.25)
    assert session._file_timeout_for(17, 100_000) == PYRIGHT_FILE_TIMEOUT_CAP_SECS  # noqa: SLF001


def test_pyright_session_default_file_timeout_cap_stays_under_the_host_watchdog() -> None:
    # The host's DEFAULT_PLUGIN_FILE_TIMEOUT (crates/loomweave-cli/src/analyze.rs)
    # kills an analyze_file call at 120s; the plugin must hand back its partial
    # result before that, or the evidence it kept is lost with the call.
    host_watchdog_secs = 120.0
    assert host_watchdog_secs > PYRIGHT_FILE_TIMEOUT_CAP_SECS


_TWO_CALLER_MODULE = """
def callee():
    pass

def first():
    callee()

def second():
    callee()
"""


class ScriptedCallSession(PyrightSession):
    """A fake pyright for the calls pass.

    Every call site of a requested function resolves to
    ``callee_by_caller[function]``. The function named ``fail_on`` raises
    ``failure`` when it reaches ``fail_at`` (for ``callHierarchy/outgoingCalls``
    only the item with ordinal ``fail_on_item``). ``items_per_function`` lets a
    test drive the per-item sub-loop with more than one call-hierarchy item.
    """

    def __init__(  # noqa: PLR0913 - each knob selects one abort shape under test.
        self,
        project_root: Path,
        *,
        module: Path,
        callee_by_caller: dict[str, str],
        fail_on: str | None = None,
        fail_at: str = "textDocument/prepareCallHierarchy",
        fail_on_item: int = 0,
        items_per_function: int = 1,
        failure: Callable[[str], Exception] = LspTimeoutError,
        budget_expires_after: str | None = None,
        run_state: PyrightRunState | None = None,
        fake_spawn: bool = True,
        **kwargs: Any,
    ) -> None:
        kwargs.setdefault("executable", sys.executable)
        super().__init__(project_root, run_state=run_state, **kwargs)
        self.fake_spawn = fake_spawn
        self.spawns = 0
        self.module = module
        self.callee_by_caller = callee_by_caller
        self.fail_on = fail_on
        self.fail_at = fail_at
        self.fail_on_item = fail_on_item
        self.items_per_function = items_per_function
        self.failure = failure
        self.budget_expires_after = budget_expires_after
        self.budget_expired = False
        self.visited: list[str] = []
        self.closed_documents = 0
        self.cancel_requests = 0

    def _ensure_process(self) -> bool:
        return True

    def _spawn_and_initialize(self, init_timeout_secs: float | None = None) -> bool:
        self.spawns += 1
        if not self.fake_spawn:
            return super()._spawn_and_initialize(init_timeout_secs)
        return True

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        if not self.fake_spawn and method in ("initialized", "exit"):
            super()._notify(method, params, deadline=deadline)
            return
        if method == "textDocument/didClose":
            self.closed_documents += 1
        if method == "$/cancelRequest":
            self.cancel_requests += 1

    def _file_budget_expired(self, deadline: float) -> bool:
        return self.budget_expired or super()._file_budget_expired(deadline)

    def _maybe_fail(self, caller: str, method: str, item_ordinal: int) -> None:
        if caller != self.fail_on or method != self.fail_at:
            return
        if method == "callHierarchy/outgoingCalls" and item_ordinal != self.fail_on_item:
            return
        raise self.failure(method)

    def _request(self, method: str, params: dict[str, object], timeout_secs: float) -> object:
        if not self.fake_spawn and method in ("initialize", "shutdown"):
            # A real spawn needs the real handshake over the real transport.
            return super()._request(method, params, timeout_secs)
        # Mirror the real ``_request``'s in-flight bookkeeping so the skip
        # path's ``$/cancelRequest`` is observable through the fake.
        self._request_id_in_flight = self._next_id
        self._next_id += 1
        index = self._function_index_for_path(self.module)
        if method == "textDocument/prepareCallHierarchy":
            position = cast("dict[str, int]", params["position"])
            function = index.by_name_position[(position["line"], position["character"])]
            self.visited.append(function.entity_id)
            self._maybe_fail(function.entity_id, method, 0)
            return [
                {"caller": function.entity_id, "ordinal": ordinal}
                for ordinal in range(self.items_per_function)
            ]
        if method == "callHierarchy/outgoingCalls":
            item = cast("dict[str, object]", params["item"])
            caller = cast("str", item["caller"])
            ordinal = cast("int", item["ordinal"])
            self._maybe_fail(caller, method, ordinal)
            function = index.by_id[caller]
            callee = index.by_id[self.callee_by_caller[caller]]
            if ordinal == self.items_per_function - 1 and caller == self.budget_expires_after:
                self.budget_expired = True
            return [
                {
                    "to": {
                        "uri": self.module.as_uri(),
                        "selectionRange": {
                            "start": {"line": callee.line, "character": callee.character},
                            "end": {"line": callee.line, "character": callee.end_character},
                        },
                    },
                    "fromRanges": [
                        {
                            "start": {"line": site.line, "character": site.character},
                            "end": {"line": site.end_line, "character": site.end_character},
                        }
                        for site in function.call_sites
                    ],
                },
            ]
        msg = f"unexpected request {method}"
        raise AssertionError(msg)


_TWO_CALLERS = ["python:function:demo.first", "python:function:demo.second"]
_CALLEE_BY_CALLER = dict.fromkeys(_TWO_CALLERS, "python:function:demo.callee")


def _ast_call_sites_total(session: PyrightSession, module: Path, function_ids: list[str]) -> int:
    # Same expression ``resolve_calls`` uses for its own total.
    index = session._function_index_for_path(module)  # noqa: SLF001
    return sum(len(index.by_id[function_id].call_sites) for function_id in function_ids)


def _assert_partial_call_evidence(
    session: ScriptedCallSession,
    module: Path,
    result: object,
    *,
    reason: str,
) -> None:
    assert isinstance(result, CallResolutionResult)
    total = _ast_call_sites_total(session, module, _TWO_CALLERS)
    # Arithmetic closure is the load-bearing check: it catches a double-count
    # (edges kept AND the same sites counted unresolved) that "first function's
    # edge is present" cannot.
    assert len(result.edges) + result.unresolved_call_sites_total == total
    assert [edge["from_id"] for edge in result.edges] == ["python:function:demo.first"]
    assert result.edges[0]["to_id"] == "python:function:demo.callee"
    assert [site["caller_entity_id"] for site in result.unresolved_call_sites] == [
        "python:function:demo.second",
    ]
    assert result.coverage == FacetCoverage.degraded(reason, transient=True)
    assert session.closed_documents == 1


def _assert_skipped_function_call_evidence(
    session: ScriptedCallSession,
    module: Path,
    result: CallResolutionResult | dict[str, object],
    *,
    resolved: str,
    skipped: str,
) -> None:
    """Skip-and-continue closure (clarion-bf3986e301): one function's per-query
    timeout costs ONLY that function; the pass completes with its sites
    disclosed as unresolved and the abandoned computation cancelled."""
    assert isinstance(result, CallResolutionResult)
    total = _ast_call_sites_total(session, module, _TWO_CALLERS)
    # Arithmetic closure is the load-bearing check: it catches a double-count
    # (edges kept AND the same sites counted unresolved) that "the resolved
    # function's edge is present" cannot.
    assert len(result.edges) + result.unresolved_call_sites_total == total
    assert [edge["from_id"] for edge in result.edges] == [resolved]
    assert result.edges[0]["to_id"] == "python:function:demo.callee"
    assert [site["caller_entity_id"] for site in result.unresolved_call_sites] == [skipped]
    assert not result.coverage.is_degraded
    assert session.cancel_requests == 1
    assert session.closed_documents == 1
    findings = [
        finding
        for finding in result.findings
        if finding["subcode"] == FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT
    ]
    assert [finding["metadata"]["function"] for finding in findings] == [skipped]
    assert all(finding["metadata"]["skipped_function"] is True for finding in findings)


def test_pyright_session_timeout_on_the_first_function_skips_it_and_resolves_the_rest(
    tmp_path: Path,
) -> None:
    # Pre-clarion-bf3986e301 this was the worst case: a timeout on the FIRST
    # function forfeited the whole file. Now it costs only that function.
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller=_CALLEE_BY_CALLER,
        fail_on="python:function:demo.first",
        interpreter=_PINNED_TEST_INTERPRETER,
    ) as session:
        result = session.resolve_calls(module, _TWO_CALLERS)

    _assert_skipped_function_call_evidence(
        session,
        module,
        result,
        resolved="python:function:demo.second",
        skipped="python:function:demo.first",
    )
    assert session.visited == _TWO_CALLERS


def test_pyright_session_call_resolution_timeout_skips_the_function_and_keeps_the_rest(
    tmp_path: Path,
) -> None:
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller=_CALLEE_BY_CALLER,
        fail_on="python:function:demo.second",
        interpreter=_PINNED_TEST_INTERPRETER,
    ) as session:
        result = session.resolve_calls(module, _TWO_CALLERS)

    _assert_skipped_function_call_evidence(
        session,
        module,
        result,
        resolved="python:function:demo.first",
        skipped="python:function:demo.second",
    )
    assert session.visited == _TWO_CALLERS


def test_pyright_session_all_functions_timing_out_stays_degraded_for_the_wedge_breaker(
    tmp_path: Path,
) -> None:
    # A pass where EVERY function timed out resolved nothing: claiming
    # ``complete`` would lie AND reset the consecutive-timeout streak the
    # ADR-057 wedge breaker keys on. It stays ``pyright_timeout``.
    source = "def callee():\n    pass\n\ndef only():\n    callee()\n"
    module = _write_module(tmp_path, source)
    run_state = PyrightRunState()

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller={"python:function:demo.only": "python:function:demo.callee"},
        fail_on="python:function:demo.only",
        run_state=run_state,
        interpreter=_PINNED_TEST_INTERPRETER,
    ) as session:
        result = session.resolve_calls(module, ["python:function:demo.only"])

    assert result.edges == []
    assert result.coverage == FacetCoverage.degraded("pyright_timeout", transient=True)
    assert run_state.consecutive_timeout_files == 1
    assert session.cancel_requests == 1


def test_pyright_session_call_resolution_timeout_mid_function_item_loop_keeps_arithmetic_closure(
    tmp_path: Path,
) -> None:
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller=_CALLEE_BY_CALLER,
        fail_on="python:function:demo.second",
        fail_at="callHierarchy/outgoingCalls",
        items_per_function=2,
        fail_on_item=1,
        interpreter=_PINNED_TEST_INTERPRETER,
    ) as session:
        result = session.resolve_calls(module, _TWO_CALLERS)

    # ``second``'s first call-hierarchy item resolved a genuine candidate before
    # its second item timed out. A function's edges are appended only after its
    # whole item sub-loop finishes, so that partial per-item state must be
    # discarded (counted unresolved), never kept AND counted -- the skip only
    # changes what happens NEXT (continue, not abort).
    _assert_skipped_function_call_evidence(
        session,
        module,
        result,
        resolved="python:function:demo.first",
        skipped="python:function:demo.second",
    )


def test_pyright_session_call_resolution_file_budget_expiry_keeps_edges_resolved_before_it(
    tmp_path: Path,
) -> None:
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller=_CALLEE_BY_CALLER,
        budget_expires_after="python:function:demo.first",
    ) as session:
        result = session.resolve_calls(module, _TWO_CALLERS)

    _assert_partial_call_evidence(session, module, result, reason="pyright_timeout")
    # The budget check runs before ``second`` is ever asked about.
    assert session.visited == ["python:function:demo.first"]
    timeout_findings = [
        finding
        for finding in result.findings
        if finding["subcode"] == FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT
    ]
    assert [finding["metadata"]["method"] for finding in timeout_findings] == [
        "analyze_file budget",
    ]


def test_pyright_session_call_resolution_transport_failure_keeps_edges_resolved_before_it(
    tmp_path: Path,
) -> None:
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)
    run_state = PyrightRunState()

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller=_CALLEE_BY_CALLER,
        fail_on="python:function:demo.second",
        failure=LspTransportClosedError,
        run_state=run_state,
    ) as session:
        result = session.resolve_calls(module, _TWO_CALLERS)

    _assert_partial_call_evidence(session, module, result, reason="pyright_transport_failure")
    assert result.coverage.collateral is False
    # The process died during THIS file's request: file-attributed, restarted
    # immediately, and not charged to the run-level crash budget.
    assert run_state.file_attributed_restart_count == 1
    assert run_state.restart_count == 0
    assert session.spawns == 1
    restart_findings = [
        finding for finding in result.findings if finding["subcode"] == FINDING_PYRIGHT_RESTART
    ]
    assert restart_findings
    # clarion-bf3986e301 direction 3: the restart finding carries the dead
    # process's stderr tail so the next crash class is attributable without
    # a manual probe. (Empty here -- the fake has no stderr -- but present.)
    assert all("stderr_tail" in finding["metadata"] for finding in restart_findings)


class PartialReferenceTransportFailureSession(PartialReferenceTimeoutSession):
    failure: Callable[[str], Exception] = LspTransportClosedError
    spawns = 0

    def _spawn_and_initialize(self, init_timeout_secs: float | None = None) -> bool:
        _ = init_timeout_secs
        self.spawns += 1
        return True

    def _reference_target_ids(
        self,
        uri: str,
        site: ReferenceSite,
        *,
        deadline: float,
        method: str = "textDocument/definition",
    ) -> tuple[list[str], bool]:
        _ = (uri, deadline)
        self.requested_starts.append(site.source_byte_start)
        if site.source_byte_start == self.timeout_start:
            raise self.failure(method)
        return self.targets_by_start[site.source_byte_start], False


def test_pyright_session_reference_transport_failure_keeps_edges_resolved_before_it(
    tmp_path: Path,
) -> None:
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

    with PartialReferenceTransportFailureSession(
        tmp_path,
        targets_by_start={
            first.source_byte_start: ["python:function:demo.alpha"],
            third.source_byte_start: ["python:function:demo.gamma"],
        },
        timeout_start=broken.source_byte_start,
    ) as session:
        result = session.resolve_references(module, [first, broken, third])

    assert [edge["to_id"] for edge in result.edges] == ["python:function:demo.alpha"]
    # The pipe is dead after ``broken``: ``third`` is never asked about.
    assert session.requested_starts == [first.source_byte_start, broken.source_byte_start]
    assert result.reference_sites_total == 3
    assert result.references_resolved_total == 1
    assert result.unresolved_reference_sites_total == 2
    assert (
        result.references_resolved_total + result.unresolved_reference_sites_total
        == result.reference_sites_total
    )
    assert result.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert FINDING_PYRIGHT_RESTART in _finding_codes(result.findings)
    assert session.run_state.file_attributed_restart_count == 1
    assert session.run_state.restart_count == 0
    assert session.spawns == 1


class DidOpenTransportFailureSession(PyrightSession):
    def _ensure_process(self) -> bool:
        return True

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = (params, deadline)
        if method == "textDocument/didOpen":
            raise LspTransportClosedError(method)


def test_pyright_session_reference_transport_failure_before_any_site_is_visited_is_fully_unresolved(
    tmp_path: Path,
) -> None:
    source = "def alpha():\n    pass\n\nFIRST = alpha\n"
    module = _write_module(tmp_path, source)
    first = _reference_site(source, from_id="python:module:demo", needle="alpha", occurrence=1)

    run_state = PyrightRunState()
    with DidOpenTransportFailureSession(
        tmp_path, executable=sys.executable, run_state=run_state
    ) as session:
        result = session.resolve_references(module, [first])

    assert result.edges == []
    assert result.unresolved_reference_sites_total == 1
    # Nothing of this file was ever asked about: the process was already dead
    # when it arrived, so the hole is collateral and the run-level budget pays.
    assert result.coverage == FacetCoverage.degraded(
        "pyright_restarting", transient=True, collateral=True
    )
    assert run_state.restart_count == 1
    assert run_state.file_attributed_restart_count == 0


# ---------------------------------------------------------------------------
# clarion-7fc41105ea: restart attribution + immediate restart + safety cap.
# ---------------------------------------------------------------------------


class _FakeProcess:
    """Stands in for ``subprocess.Popen``: alive until ``die()`` or ``kill()``."""

    def __init__(self) -> None:
        self.returncode: int | None = None
        self.killed = False

    def poll(self) -> int | None:
        return self.returncode

    def die(self) -> None:
        self.returncode = 1

    def kill(self) -> None:
        self.killed = True
        self.returncode = -9

    def wait(self, timeout: float | None = None) -> int:
        _ = timeout
        return self.returncode if self.returncode is not None else 0


# Restart/wedge/deferral tests assert on restart ATTRIBUTION, not interpreter
# pinning. Without a fixed default, ``RestartProbeSession``'s inherited
# discovery would resolve against whatever interpreter the host happens to
# expose (``VIRTUAL_ENV`` set or not), swinging otherwise-complete coverage
# claims to ``interpreter_unpinned`` on some machines/CI runs and not others
# (clarion-5cf9643de9). Pinned so those tests stay environment-independent;
# a test exercising pinning itself passes its own ``interpreter=`` and wins.
_PINNED_TEST_INTERPRETER = ProjectInterpreter(path=sys.executable, source="override")


class RestartProbeSession(PyrightSession):
    """A fake pyright that models death and respawn without a subprocess.

    A request named in ``crash_methods`` for a file whose stem is in
    ``crash_stems`` kills the fake process mid-flight and raises the same
    ``LspTransportClosedError`` the real transport raises on EOF, after
    running for ``crash_latency_secs`` of fake clock (so a crash can land
    late in the file's watchdog window). Every spawn advances the fake clock
    by ``spawn_latency_secs`` so deadline arithmetic can be asserted without
    sleeping; a spawn slower than the handshake bound a headroom-limited
    respawn passes in behaves like the real one -- it burns the bound and
    times out.
    """

    def __init__(  # noqa: PLR0913 - each knob selects one restart shape under test.
        self,
        project_root: Path,
        *,
        crash_stems: set[str] | None = None,
        crash_methods: set[str] | None = None,
        timeout_stems: set[str] | None = None,
        timeout_latency_secs: float = 0.0,
        spawn_latency_secs: float = 0.0,
        first_spawn_latency_secs: float | None = None,
        crash_latency_secs: float = 0.0,
        spawn_ok: bool = True,
        run_state: PyrightRunState | None = None,
        **kwargs: Any,
    ) -> None:
        kwargs.setdefault("interpreter", _PINNED_TEST_INTERPRETER)
        super().__init__(project_root, executable=sys.executable, run_state=run_state, **kwargs)
        self.crash_stems = crash_stems or set()
        # Stems whose first query hangs past its timeout while the fake
        # process stays alive: the wedged-but-alive shape (ADR-057 breaker).
        self.timeout_stems = timeout_stems or set()
        self.timeout_latency_secs = timeout_latency_secs
        self.crash_latency_secs = crash_latency_secs
        self.first_spawn_latency_secs = first_spawn_latency_secs
        self.crash_methods = crash_methods or {
            "textDocument/prepareCallHierarchy",
            "textDocument/definition",
        }
        self.spawn_latency_secs = spawn_latency_secs
        self.spawn_ok = spawn_ok
        self.spawns = 0
        self.clock = 1000.0
        self.current_uri: str | None = None
        self.fake_process: _FakeProcess | None = None

    def _now(self) -> float:
        return self.clock

    def _spawn_and_initialize(self, init_timeout_secs: float | None = None) -> bool:
        self.spawns += 1
        bound = self.init_timeout_secs if init_timeout_secs is None else init_timeout_secs
        latency = self.spawn_latency_secs
        if self.spawns == 1 and self.first_spawn_latency_secs is not None:
            latency = self.first_spawn_latency_secs
        if latency > bound:
            # The handshake ran out of its bound: the process exists but
            # never answered ``initialize``.
            self.clock += bound
            self.fake_process = _FakeProcess()
            self._process = cast("Any", self.fake_process)
            return self._handle_initialize_timeout(bound)
        self.clock += latency
        if not self.spawn_ok:
            return False
        self.fake_process = _FakeProcess()
        self._process = cast("Any", self.fake_process)
        return True

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = deadline
        self._live_process()
        if method == "textDocument/didOpen":
            document = cast("dict[str, str]", params["textDocument"])
            self.current_uri = document["uri"]

    def _request(self, method: str, params: dict[str, object], timeout_secs: float) -> object:
        _ = (params, timeout_secs)
        self._live_process()
        if method == "shutdown":
            return {}
        assert self.current_uri is not None
        stem = Path(self.current_uri).stem
        if stem in self.timeout_stems and method in self.crash_methods:
            self.clock += self.timeout_latency_secs
            raise LspTimeoutError(method)
        if stem in self.crash_stems and method in self.crash_methods:
            assert self.fake_process is not None
            self.clock += self.crash_latency_secs
            self.fake_process.die()
            message = "EOF while reading LSP header"
            raise LspTransportClosedError(message)
        return []


def _write_named_module(tmp_path: Path, stem: str) -> Path:
    return _write_module(
        tmp_path, "def callee():\n    pass\n\ndef caller():\n    callee()\n", f"{stem}.py"
    )


def _caller_id(stem: str) -> str:
    return f"python:function:{stem}.caller"


def _restart_findings(findings: Sequence[Finding]) -> list[Finding]:
    return [finding for finding in findings if finding["subcode"] == FINDING_PYRIGHT_RESTART]


def test_pyright_session_in_flight_crash_restarts_immediately_and_next_file_is_complete(
    tmp_path: Path,
) -> None:
    trouble = _write_named_module(tmp_path, "trouble")
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(tmp_path, crash_stems={"trouble"}, run_state=run_state) as session:
        crashed = session.resolve_calls(trouble, [_caller_id("trouble")])
        assert session.spawns == 2, "the restart must happen before resolve_calls returns"
        after = session.resolve_calls(clean, [_caller_id("clean")])

    assert crashed.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    restarts = _restart_findings(crashed.findings)
    assert len(restarts) == 1
    assert restarts[0]["metadata"]["attribution"] == "in_flight"
    # File-attributed restarts do not spend the run-level crash budget.
    assert run_state.file_attributed_restart_count == 1
    assert run_state.restart_count == 0
    assert run_state.disabled is False
    # The next file arrives at a live process: complete, not collateral.
    assert after.coverage == FacetCoverage()
    assert _restart_findings(after.findings) == []
    assert session.spawns == 2


def test_pyright_session_found_dead_on_arrival_restarts_under_run_cap_and_file_proceeds(
    tmp_path: Path,
) -> None:
    first = _write_named_module(tmp_path, "first")
    second = _write_named_module(tmp_path, "second")
    run_state = PyrightRunState()

    with RestartProbeSession(tmp_path, run_state=run_state) as session:
        assert session.resolve_calls(first, [_caller_id("first")]).coverage == FacetCoverage()
        assert session.fake_process is not None
        # pyright dies AFTER answering ``first``; ``second`` discovers it.
        session.fake_process.die()
        arrived = session.resolve_calls(second, [_caller_id("second")])

    # Not this file's doing, so it spends the run-level budget, not the
    # file-attributed one -- and the arriving file still gets a live process.
    assert run_state.restart_count == 1
    assert run_state.file_attributed_restart_count == 0
    assert arrived.coverage == FacetCoverage()
    restarts = _restart_findings(arrived.findings)
    assert len(restarts) == 1
    assert restarts[0]["metadata"]["attribution"] == "arrival"
    assert "on arrival" in restarts[0]["message"]
    assert session.spawns == 2


def test_pyright_session_calls_transport_failure_before_any_function_is_collateral(
    tmp_path: Path,
) -> None:
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)
    run_state = PyrightRunState()

    with DidOpenTransportFailureSession(
        tmp_path, executable=sys.executable, run_state=run_state
    ) as session:
        result = session.resolve_calls(module, _TWO_CALLERS)

    assert result.edges == []
    assert result.coverage == FacetCoverage.degraded(
        "pyright_restarting", transient=True, collateral=True
    )
    assert run_state.restart_count == 1
    assert run_state.file_attributed_restart_count == 0


def test_pyright_session_transient_spawn_failure_is_collateral(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")
    monkeypatch.setattr(subprocess, "Popen", _popen_raising(errno.EAGAIN))
    run_state = PyrightRunState()

    with PyrightSession(tmp_path, executable=sys.executable, run_state=run_state) as session:
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert run_state.disabled is False
    # The file never got a process for reasons that have nothing to do with
    # its own content: collateral, so the host dispatches it early next run.
    assert result.coverage == FacetCoverage.degraded(
        "pyright_spawn_failed", transient=True, collateral=True
    )


def test_pyright_session_pure_timeout_skips_the_function_and_does_not_restart(
    tmp_path: Path,
) -> None:
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)
    run_state = PyrightRunState()

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller=_CALLEE_BY_CALLER,
        fail_on="python:function:demo.second",
        run_state=run_state,
    ) as session:
        result = session.resolve_calls(module, _TWO_CALLERS)

    # A timeout means pyright is alive but slow: restarting would throw away
    # its warm cache for nothing (ADR-057 deviation from the ticket text) --
    # and since clarion-bf3986e301 a per-query timeout also does not abort:
    # the one slow function is skipped (its computation cancelled) and the
    # pass completes, so the file is not re-dispatched every run for it.
    assert not result.coverage.is_degraded or result.coverage.reason == "interpreter_unpinned"
    assert result.unresolved_call_sites_total >= 1
    assert run_state.restart_count == 0
    assert run_state.file_attributed_restart_count == 0
    # A pass that completes (some functions genuinely answered) resets the
    # wedge streak; only an all-functions-timeout file feeds it.
    assert run_state.consecutive_timeout_files == 0
    assert session.spawns == 0
    assert session.cancel_requests == 1


def test_pyright_session_unrelated_read_error_in_flight_is_not_a_restart(
    tmp_path: Path,
) -> None:
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)
    run_state = PyrightRunState()

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller=_CALLEE_BY_CALLER,
        fail_on="python:function:demo.second",
        failure=FileNotFoundError,
        run_state=run_state,
    ) as session:
        # The process is alive: a bare OSError here is a local read failure on
        # some target file, not pyright dying.
        session._process = cast("Any", _FakeProcess())  # noqa: SLF001
        result = session.resolve_calls(module, _TWO_CALLERS)
        session._process = None  # noqa: SLF001

    _assert_partial_call_evidence(session, module, result, reason="pyright_local_read_error")
    assert result.coverage.collateral is False
    assert run_state.restart_count == 0
    assert run_state.file_attributed_restart_count == 0
    assert session.spawns == 0
    assert FINDING_PYRIGHT_RESTART not in _finding_codes(result.findings)


class LocalReadErrorReferenceSession(PartialReferenceTransportFailureSession):
    failure: Callable[[str], Exception] = FileNotFoundError


def test_pyright_session_unrelated_read_error_in_reference_pass_is_not_a_restart(
    tmp_path: Path,
) -> None:
    source = "def alpha():\n    pass\n\ndef beta():\n    pass\n\nFIRST = alpha\nBROKEN = beta\n"
    module = _write_module(tmp_path, source)
    first = _reference_site(source, from_id="python:module:demo", needle="alpha", occurrence=1)
    broken = _reference_site(source, from_id="python:module:demo", needle="beta", occurrence=1)

    with LocalReadErrorReferenceSession(
        tmp_path,
        targets_by_start={first.source_byte_start: ["python:function:demo.alpha"]},
        timeout_start=broken.source_byte_start,
    ) as session:
        session._process = cast("Any", _FakeProcess())  # noqa: SLF001
        result = session.resolve_references(module, [first, broken])
        session._process = None  # noqa: SLF001

    assert [edge["to_id"] for edge in result.edges] == ["python:function:demo.alpha"]
    assert result.coverage == FacetCoverage.degraded(
        "pyright_local_read_error", transient=True, collateral=False
    )
    assert session.spawns == 0
    assert FINDING_PYRIGHT_RESTART not in _finding_codes(result.findings)


def test_pyright_session_immediate_restart_next_file_uses_fresh_real_process(
    tmp_path: Path,
) -> None:
    """Real subprocess: a langserver that dies on the troublemaker's request."""
    script = _write_executable(
        tmp_path,
        textwrap.dedent(
            """
            #!/usr/bin/env python3
            import json
            import os
            import sys

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

            opened = None
            while True:
                frame = read_frame()
                if frame is None:
                    break
                method = frame.get("method")
                if method == "initialize":
                    write_frame({"jsonrpc": "2.0", "id": frame["id"], "result": {}})
                elif method == "textDocument/didOpen":
                    opened = frame["params"]["textDocument"]["uri"]
                elif method == "textDocument/prepareCallHierarchy":
                    if "trouble" in (opened or ""):
                        os._exit(1)
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
    trouble = _write_named_module(tmp_path, "trouble")
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with PyrightSession(
        tmp_path,
        executable=str(script),
        init_timeout_secs=5.0,
        run_state=run_state,
        interpreter=_PINNED_TEST_INTERPRETER,
    ) as session:
        crashed = session.resolve_calls(trouble, [_caller_id("trouble")])
        assert session._process is not None  # noqa: SLF001
        assert session._process.poll() is None, "restart must precede the return"  # noqa: SLF001
        after = session.resolve_calls(clean, [_caller_id("clean")])

    assert crashed.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert run_state.file_attributed_restart_count == 1
    assert run_state.restart_count == 0
    assert run_state.pyright_init_latency_total_ms > 0
    assert after.coverage == FacetCoverage()
    assert FINDING_PYRIGHT_RESTART not in _finding_codes(after.findings)


def test_pyright_session_restart_extends_shared_file_deadline_without_cross_facet_contamination(
    tmp_path: Path,
) -> None:
    source = "def alpha():\n    pass\n\ndef caller():\n    alpha()\n\nFIRST = alpha\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(source, from_id="python:module:demo", needle="alpha", occurrence=2)
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems={"demo"},
        crash_methods={"textDocument/definition"},
        spawn_latency_secs=3.0,
        file_timeout_base_secs=10.0,
        file_timeout_per_function_secs=0.0,
        file_timeout_per_line_secs=0.0,
        run_state=run_state,
    ) as session:
        anchor = session.clock
        calls = session.resolve_calls(module, ["python:function:demo.caller"])
        deadline_before = session._file_deadlines[module]  # noqa: SLF001
        references = session.resolve_references(module, [site])
        assert session.spawns == 2

    assert deadline_before == anchor + 3.0 + 10.0  # initial spawn, then the budget
    assert calls.coverage == FacetCoverage()
    assert references.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert run_state.file_attributed_restart_count == 1
    assert "pyright_timeout" not in {
        finding["metadata"].get("reason") for finding in references.findings
    }
    assert FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT not in _finding_codes(references.findings)


def test_pyright_session_restart_extension_clamped_to_host_watchdog_ceiling(
    tmp_path: Path,
) -> None:
    module = _write_named_module(tmp_path, "trouble")

    with RestartProbeSession(
        tmp_path,
        crash_stems={"trouble"},
        spawn_latency_secs=40.0,
        first_spawn_latency_secs=0.0,
        init_timeout_secs=60.0,
        file_timeout_base_secs=PYRIGHT_FILE_TIMEOUT_CAP_SECS,
        file_timeout_per_function_secs=0.0,
    ) as session:
        anchor = session.clock
        session.resolve_calls(module, [_caller_id("trouble")])
        deadline = session._file_deadlines[module]  # noqa: SLF001

    assert session.spawns == 2
    ceiling = anchor + HOST_FILE_WATCHDOG_SECS - FILE_DEADLINE_TERMINAL_SAFETY_MARGIN_SECS
    # Un-clamped this would be anchor + 90 (budget) + 40 (respawn).
    assert deadline == ceiling
    assert deadline < anchor + PYRIGHT_FILE_TIMEOUT_CAP_SECS + 40.0


def test_pyright_session_second_in_flight_crash_on_same_file_defers_restart_at_ceiling(
    tmp_path: Path,
) -> None:
    source = "def alpha():\n    pass\n\ndef caller():\n    alpha()\n\nFIRST = alpha\n"
    trouble = _write_module(tmp_path, source, "trouble.py")
    site = _reference_site(source, from_id="python:module:trouble", needle="alpha", occurrence=2)
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems={"trouble"},
        spawn_latency_secs=20.0,
        first_spawn_latency_secs=0.0,
        # Each crashing request runs 80s before pyright dies: the calls crash
        # lands at +80 of a 105s window (25s headroom: respawn, 20s), the
        # references crash at +180 (none: defer).
        crash_latency_secs=80.0,
        file_timeout_base_secs=PYRIGHT_FILE_TIMEOUT_CAP_SECS,
        file_timeout_per_function_secs=0.0,
        run_state=run_state,
    ) as session:
        anchor = session.clock
        calls = session.resolve_calls(trouble, [_caller_id("trouble")])
        assert session.spawns == 2
        assert session.clock == anchor + 100.0
        references = session.resolve_references(trouble, [site])
        # No headroom left under this file's watchdog ceiling: the restart is
        # deferred, the dead process left in place.
        assert session.spawns == 2
        assert run_state.restart_already_charged_to_file is True
        assert run_state.restart_charged_to_path == str(trouble)
        after = session.resolve_calls(clean, [_caller_id("clean")])
        assert session.spawns == 3

    assert calls.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert references.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert run_state.file_attributed_restart_count == 2
    assert run_state.ceiling_deferred_restart_count == 1
    # The next file pays the respawn silently: not charged, not degraded.
    assert run_state.restart_already_charged_to_file is False
    assert run_state.restart_charged_to_path is None
    assert run_state.restart_count == 0
    assert after.coverage == FacetCoverage()
    assert _restart_findings(after.findings) == []


def test_pyright_session_ceiling_deferred_flag_is_consumed_by_a_recycled_session(
    tmp_path: Path,
) -> None:
    # ADR-057 §3: server.py recycles the PyrightSession every
    # MAX_FILES_PER_PYRIGHT_SESSION files. When that recycle lands right after
    # a ceiling-deferred restart, the NEXT file is served by a fresh session
    # whose first spawn is the deferred restart -- it must consume the one-shot
    # flag, or the flag later masks a genuine arrival-death on an unrelated
    # file (no finding, no run-level charge).
    source = "def alpha():\n    pass\n\ndef caller():\n    alpha()\n\nFIRST = alpha\n"
    trouble = _write_module(tmp_path, source, "trouble.py")
    site = _reference_site(source, from_id="python:module:trouble", needle="alpha", occurrence=2)
    clean = _write_named_module(tmp_path, "clean")
    unrelated = _write_named_module(tmp_path, "unrelated")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems={"trouble"},
        spawn_latency_secs=20.0,
        first_spawn_latency_secs=0.0,
        crash_latency_secs=80.0,
        file_timeout_base_secs=PYRIGHT_FILE_TIMEOUT_CAP_SECS,
        file_timeout_per_function_secs=0.0,
        run_state=run_state,
    ) as first:
        first.resolve_calls(trouble, [_caller_id("trouble")])
        first.resolve_references(trouble, [site])
        assert run_state.restart_already_charged_to_file is True
        assert run_state.ceiling_deferred_restart_count == 1

    # The routine recycle: a brand-new session sharing the run state.
    with RestartProbeSession(tmp_path, run_state=run_state) as second:
        after = second.resolve_calls(clean, [_caller_id("clean")])
        assert second.spawns == 1
        assert run_state.restart_already_charged_to_file is False, (
            "the recycled session's first spawn IS the deferred restart"
        )
        assert after.coverage == FacetCoverage()
        assert run_state.restart_count == 0

        # Now a GENUINE arrival-death on an unrelated file must be visible:
        # a RESTART finding and a run-level charge, not a silent respawn.
        assert second.fake_process is not None
        second.fake_process.die()
        later = second.resolve_calls(unrelated, [_caller_id("unrelated")])
        assert second.spawns == 2

    assert run_state.restart_count == 1
    # Discovered at _ensure_process, so the arriving file still resolved on
    # the fresh process -- but the death itself is on the record.
    assert later.coverage == FacetCoverage()
    restart_findings = _restart_findings(later.findings)
    assert [finding["metadata"]["attribution"] for finding in restart_findings] == ["arrival"]


def test_resolve_host_file_watchdog_secs_mirrors_the_host_env_override() -> None:
    # ADR-057 §3: the plugin inherits the host's environment, so the SAME
    # variable the host's plugin_file_timeout() honours drives the ceiling.
    assert resolve_host_file_watchdog_secs({}) == HOST_FILE_WATCHDOG_SECS
    assert resolve_host_file_watchdog_secs({"LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS": "30000"}) == 30.0
    # The host ignores an unparsable / non-positive value and keeps its default.
    assert (
        resolve_host_file_watchdog_secs({"LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS": "soon"})
        == HOST_FILE_WATCHDOG_SECS
    )
    assert (
        resolve_host_file_watchdog_secs({"LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS": "0"})
        == HOST_FILE_WATCHDOG_SECS
    )


def test_pyright_session_reads_host_watchdog_override_from_environment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS", "30000")
    with RestartProbeSession(tmp_path) as session:
        assert session.host_file_watchdog_secs == 30.0
        assert session._watchdog_ceiling_for(1000.0) == 1000.0 + 30.0 - 15.0  # noqa: SLF001
    # A watchdog too short for the full margin keeps half of itself.
    with RestartProbeSession(tmp_path, host_file_watchdog_secs=20.0) as short:
        assert short._watchdog_ceiling_for(1000.0) == 1010.0  # noqa: SLF001


def test_pyright_session_shorter_host_watchdog_defers_restart_instead_of_respawning_late(
    tmp_path: Path,
) -> None:
    # ADR-057 §3: with LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS=30000 the real host
    # deadline is 30s. A crash on a file already 12s in has 3s of headroom
    # under the 15s ceiling window -- below MIN_RESPAWN_HEADROOM_SECS: the
    # respawn must be deferred, not attempted in-process where its latency
    # would get the whole plugin call killed.
    source = "def alpha():\n    pass\n\ndef caller():\n    alpha()\n"
    trouble = _write_module(tmp_path, source, "trouble.py")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems={"trouble"},
        spawn_latency_secs=20.0,
        first_spawn_latency_secs=0.0,
        crash_latency_secs=12.0,
        file_timeout_base_secs=PYRIGHT_FILE_TIMEOUT_CAP_SECS,
        file_timeout_per_function_secs=0.0,
        run_state=run_state,
        host_file_watchdog_secs=30.0,
    ) as session:
        anchor = session.clock
        result = session.resolve_calls(trouble, [_caller_id("trouble")])
        assert session.spawns == 1, "no in-process respawn past the real host deadline"
        assert session.clock == anchor + 12.0, "no wall-clock spent on a hopeless respawn"

    assert result.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert run_state.ceiling_deferred_restart_count == 1
    assert run_state.restart_already_charged_to_file is True


def test_pyright_session_late_crash_respawn_is_bounded_by_real_headroom(
    tmp_path: Path,
) -> None:
    # The review's case: a 90s budget under a 105s window can never satisfy a
    # static ``deadline >= ceiling`` check, so a crash 89s in used to respawn
    # unbounded (up to the 30s init timeout) against 16s of real headroom. The
    # decision is now made on ``ceiling - now``: the respawn IS attempted, but
    # its handshake is bounded to the headroom, so a hung pyright is cut off
    # at the ceiling and deferred -- the host watchdog never fires.
    trouble = _write_named_module(tmp_path, "trouble")
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems={"trouble"},
        spawn_latency_secs=20.0,
        first_spawn_latency_secs=0.0,
        crash_latency_secs=89.0,
        file_timeout_base_secs=PYRIGHT_FILE_TIMEOUT_CAP_SECS,
        file_timeout_per_function_secs=0.0,
        run_state=run_state,
    ) as session:
        anchor = session.clock
        ceiling = anchor + HOST_FILE_WATCHDOG_SECS - FILE_DEADLINE_TERMINAL_SAFETY_MARGIN_SECS
        result = session.resolve_calls(trouble, [_caller_id("trouble")])
        assert session.spawns == 2, "16s of headroom is worth an attempt"
        assert session.clock == ceiling, "the hung handshake was cut off at the ceiling"
        assert run_state.disabled is False, "a headroom-bounded timeout is not an install failure"
        assert run_state.restart_already_charged_to_file is True
        assert run_state.ceiling_deferred_restart_count == 1
        after = session.resolve_calls(clean, [_caller_id("clean")])
        assert session.spawns == 3

    assert result.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    restarts = _restart_findings(result.findings)
    assert [finding["metadata"]["outcome"] for finding in restarts] == ["deferred_to_next_file"]
    assert FINDING_PYRIGHT_INIT_TIMEOUT not in _finding_codes(result.findings)
    assert after.coverage == FacetCoverage()
    assert run_state.restart_count == 0


def test_pyright_session_late_crash_with_fast_respawn_restarts_within_headroom(
    tmp_path: Path,
) -> None:
    # Same late crash, but pyright comes back in 3s: that fits the 16s of
    # headroom, so the restart completes and the next file is served live.
    trouble = _write_named_module(tmp_path, "trouble")
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems={"trouble"},
        spawn_latency_secs=3.0,
        first_spawn_latency_secs=0.0,
        crash_latency_secs=89.0,
        file_timeout_base_secs=PYRIGHT_FILE_TIMEOUT_CAP_SECS,
        file_timeout_per_function_secs=0.0,
        run_state=run_state,
    ) as session:
        anchor = session.clock
        result = session.resolve_calls(trouble, [_caller_id("trouble")])
        assert session.spawns == 2
        assert session.clock == anchor + 92.0
        after = session.resolve_calls(clean, [_caller_id("clean")])
        assert session.spawns == 2

    assert result.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert run_state.ceiling_deferred_restart_count == 0
    assert run_state.restart_already_charged_to_file is False
    assert after.coverage == FacetCoverage()


def test_pyright_session_same_file_references_after_calls_deferral_stays_self_inflicted(
    tmp_path: Path,
) -> None:
    # A restart deferred from a file's calls facet is NOT consumed by that
    # same file's references facet: both share one watchdog window, and the
    # deferral already found it exhausted. The references facet reports the
    # file's own crash (self-inflicted), spends no wall-clock, and leaves the
    # deferral armed for the next file.
    source = "def alpha():\n    pass\n\ndef caller():\n    alpha()\n\nFIRST = alpha\n"
    trouble = _write_module(tmp_path, source, "trouble.py")
    site = _reference_site(source, from_id="python:module:trouble", needle="alpha", occurrence=2)
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems={"trouble"},
        # A 10s respawn fits the clean file's 15s window; the trouble file's
        # crash at +12 leaves 3s, below MIN_RESPAWN_HEADROOM_SECS.
        spawn_latency_secs=10.0,
        first_spawn_latency_secs=0.0,
        crash_latency_secs=12.0,
        file_timeout_base_secs=PYRIGHT_FILE_TIMEOUT_CAP_SECS,
        file_timeout_per_function_secs=0.0,
        run_state=run_state,
        host_file_watchdog_secs=30.0,
    ) as session:
        calls = session.resolve_calls(trouble, [_caller_id("trouble")])
        assert session.spawns == 1
        assert run_state.restart_already_charged_to_file is True
        clock_before = session.clock
        references = session.resolve_references(trouble, [site])
        assert session.spawns == 1, "the same file must not respawn against its exhausted window"
        assert session.clock == clock_before
        assert run_state.restart_already_charged_to_file is True
        assert run_state.restart_charged_to_path == str(trouble)
        after = session.resolve_calls(clean, [_caller_id("clean")])
        assert session.spawns == 2

    assert calls.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert references.coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    assert _restart_findings(references.findings) == [], "no second restart event happened"
    assert run_state.ceiling_deferred_restart_count == 1
    assert run_state.file_attributed_restart_count == 1
    # The next file consumed the deferral silently and resolved live.
    assert run_state.restart_already_charged_to_file is False
    assert run_state.restart_charged_to_path is None
    assert run_state.restart_count == 0
    assert after.coverage == FacetCoverage()
    assert _restart_findings(after.findings) == []


def test_pyright_session_file_attributed_restarts_do_not_count_against_run_cap(
    tmp_path: Path,
) -> None:
    stems = [f"trouble_{index}" for index in range(MAX_PYRIGHT_RESTARTS_PER_RUN + 2)]
    modules = {stem: _write_named_module(tmp_path, stem) for stem in stems}
    run_state = PyrightRunState()

    with RestartProbeSession(tmp_path, crash_stems=set(stems), run_state=run_state) as session:
        results = {stem: session.resolve_calls(modules[stem], [_caller_id(stem)]) for stem in stems}

    assert run_state.disabled is False
    assert run_state.restart_count == 0
    assert run_state.file_attributed_restart_count == len(stems)
    assert all(
        result.coverage
        == FacetCoverage.degraded("pyright_transport_failure", transient=True, collateral=False)
        for result in results.values()
    )
    assert FINDING_PYRIGHT_POISON_FRAME not in {
        code for result in results.values() for code in _finding_codes(result.findings)
    }


def test_pyright_session_total_restart_count_cap_trips_and_reports_honestly(
    tmp_path: Path,
) -> None:
    stems = ["trouble_a", "trouble_b", "trouble_c"]
    modules = {stem: _write_named_module(tmp_path, stem) for stem in stems}
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems=set(stems),
        max_total_restarts_per_run=2,
        run_state=run_state,
    ) as session:
        results = [session.resolve_calls(modules[stem], [_caller_id(stem)]) for stem in stems]
        after = session.resolve_calls(clean, [_caller_id("clean")])

    assert run_state.disabled is True
    assert run_state.disabled_reason == "pyright_restart_cap_exceeded"
    assert FINDING_PYRIGHT_TOTAL_RESTART_CAP_EXCEEDED in _finding_codes(results[-1].findings)
    # The file whose restart tripped the cap still owns its own hole.
    assert results[-1].coverage == FacetCoverage.degraded(
        "pyright_transport_failure", transient=True, collateral=False
    )
    # Everything after is honest collateral under the new token, not the old
    # ``pyright_unavailable`` fallback.
    assert after.coverage == FacetCoverage.degraded(
        "pyright_restart_cap_exceeded", transient=True, collateral=True
    )
    assert session.spawns == 3  # initial + two restarts; the third was refused


def test_pyright_session_total_restart_latency_budget_trips(tmp_path: Path) -> None:
    stems = ["trouble_a", "trouble_b"]
    modules = {stem: _write_named_module(tmp_path, stem) for stem in stems}
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        crash_stems=set(stems),
        spawn_latency_secs=1.0,
        restart_latency_budget_ms=1500,
        run_state=run_state,
    ) as session:
        first = session.resolve_calls(modules["trouble_a"], [_caller_id("trouble_a")])
        second = session.resolve_calls(modules["trouble_b"], [_caller_id("trouble_b")])

    # Initial spawn (1s) + one respawn (1s) = 2000ms > 1500ms budget.
    assert run_state.pyright_init_latency_total_ms == 2000
    assert FINDING_PYRIGHT_TOTAL_RESTART_CAP_EXCEEDED not in _finding_codes(first.findings)
    assert FINDING_PYRIGHT_TOTAL_RESTART_CAP_EXCEEDED in _finding_codes(second.findings)
    assert run_state.disabled_reason == "pyright_restart_cap_exceeded"
    assert session.spawns == 2


_NEVER_ANSWERS_LANGSERVER = textwrap.dedent(
    """
    #!/usr/bin/env python3
    import sys
    sys.stdin.buffer.read()
    """,
).lstrip()

_EXITS_IMMEDIATELY_LANGSERVER = "#!/usr/bin/env python3\nraise SystemExit(1)\n"


@pytest.mark.parametrize(
    "branch",
    ["missing_executable", "install_check", "init_timeout", "init_transport", "transient_eagain"],
)
def test_pyright_session_failed_immediate_respawn_never_clobbers_disabled_reason(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    branch: str,
) -> None:
    module = _write_module(tmp_path, _TWO_CALLER_MODULE)
    run_state = PyrightRunState()
    kwargs: dict[str, Any] = {"executable": sys.executable}
    if branch == "missing_executable":
        kwargs["executable"] = str(tmp_path / "does-not-exist")
    elif branch == "install_check":
        kwargs["install_check"] = lambda _executable: False
    elif branch == "init_timeout":
        kwargs["executable"] = str(_write_executable(tmp_path, _NEVER_ANSWERS_LANGSERVER))
        kwargs["init_timeout_secs"] = 0.2
    elif branch == "init_transport":
        kwargs["executable"] = str(_write_executable(tmp_path, _EXITS_IMMEDIATELY_LANGSERVER))
    else:
        monkeypatch.setattr(subprocess, "Popen", _popen_raising(errno.EAGAIN))

    with ScriptedCallSession(
        tmp_path,
        module=module,
        callee_by_caller=_CALLEE_BY_CALLER,
        fail_on="python:function:demo.second",
        failure=LspTransportClosedError,
        fake_spawn=False,
        run_state=run_state,
        **kwargs,
    ) as session:
        crashed = session.resolve_calls(module, _TWO_CALLERS)

    # The crashing file's own attribution is unaffected by the respawn outcome.
    _assert_partial_call_evidence(session, module, crashed, reason="pyright_transport_failure")
    assert run_state.file_attributed_restart_count == 1
    assert run_state.file_attributed_respawn_failure_count == 1
    if branch == "transient_eagain":
        assert run_state.disabled is False
        assert run_state.disabled_reason is None
        expected_reason = "pyright_spawn_failed"
    else:
        assert run_state.disabled is True
        # The original branch's token stands: the cap never overwrites it.
        assert run_state.disabled_reason == "pyright_unavailable"
        expected_reason = "pyright_unavailable"

    # The next file sees the honest outcome through the real ``_ensure_process``.
    with PyrightSession(tmp_path, run_state=run_state, **kwargs) as next_session:
        after = next_session.resolve_calls(module, _TWO_CALLERS)
    assert after.coverage == FacetCoverage.degraded(
        expected_reason, transient=True, collateral=True
    )


def test_pyright_session_run_state_counters_populate_after_a_mixed_run(tmp_path: Path) -> None:
    trouble = _write_named_module(tmp_path, "trouble")
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path, crash_stems={"trouble"}, spawn_latency_secs=0.5, run_state=run_state
    ) as session:
        session.resolve_calls(trouble, [_caller_id("trouble")])  # in-flight crash
        assert session.fake_process is not None
        session.fake_process.die()  # dies after answering: found dead on arrival
        session.resolve_calls(clean, [_caller_id("clean")])

    assert run_state.restart_count == 1
    assert run_state.file_attributed_restart_count == 1
    assert run_state.file_attributed_respawn_failure_count == 0
    assert run_state.ceiling_deferred_restart_count == 0
    assert run_state.pyright_init_latency_total_ms == 1500  # three spawns at 500ms


_TIMEOUT_COVERAGE = FacetCoverage.degraded("pyright_timeout", transient=True, collateral=False)


def _wedged_findings(findings: Sequence[Finding]) -> list[Finding]:
    return [finding for finding in findings if finding["subcode"] == FINDING_PYRIGHT_WEDGED_RESTART]


def test_pyright_session_consecutive_timeout_streak_restarts_once_charged_to_run_cap(
    tmp_path: Path,
) -> None:
    stems = [f"slow_{index}" for index in range(MAX_CONSECUTIVE_TIMEOUT_FILES)]
    modules = {stem: _write_named_module(tmp_path, stem) for stem in stems}
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(tmp_path, timeout_stems=set(stems), run_state=run_state) as session:
        results = [session.resolve_calls(modules[stem], [_caller_id(stem)]) for stem in stems]
        spawns_after_streak = session.spawns
        after = session.resolve_calls(clean, [_caller_id("clean")])

    # Every file in the streak keeps its honest self-inflicted timeout claim.
    assert all(result.coverage == _TIMEOUT_COVERAGE for result in results)
    # Exactly one restart, performed before the third file's result returned.
    assert spawns_after_streak == 2  # initial + the wedged restart
    assert run_state.restart_count == 1
    assert run_state.wedged_restart_count == 1
    assert run_state.file_attributed_restart_count == 0
    assert run_state.consecutive_timeout_files == 0
    assert run_state.disabled is False
    # The finding is anchored to the file that closed the streak and names it.
    assert not any(_wedged_findings(result.findings) for result in results[:-1])
    wedged = _wedged_findings(results[-1].findings)
    assert len(wedged) == 1
    assert str(MAX_CONSECUTIVE_TIMEOUT_FILES) in wedged[0]["message"]
    assert wedged[0]["metadata"]["consecutive_timeout_files"] == MAX_CONSECUTIVE_TIMEOUT_FILES
    assert wedged[0]["metadata"]["outcome"] == "restarted"
    assert wedged[0]["metadata"]["restart_count"] == 1
    # The next file arrives at the fresh process and completes.
    assert after.coverage == FacetCoverage()
    assert session.spawns == 2


def test_pyright_session_completing_file_resets_timeout_streak(tmp_path: Path) -> None:
    slow_a = _write_named_module(tmp_path, "slow_a")
    slow_b = _write_named_module(tmp_path, "slow_b")
    slow_c = _write_named_module(tmp_path, "slow_c")
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        timeout_stems={"slow_a", "slow_b", "slow_c"},
        max_consecutive_timeout_files=3,
        run_state=run_state,
    ) as session:
        session.resolve_calls(slow_a, [_caller_id("slow_a")])
        session.resolve_calls(slow_b, [_caller_id("slow_b")])
        assert run_state.consecutive_timeout_files == 2
        between = session.resolve_calls(clean, [_caller_id("clean")])
        assert between.coverage == FacetCoverage()
        assert run_state.consecutive_timeout_files == 0
        last = session.resolve_calls(slow_c, [_caller_id("slow_c")])

    # 2 + 1 is not a streak of 3: no restart, nothing charged.
    assert run_state.consecutive_timeout_files == 1
    assert run_state.restart_count == 0
    assert run_state.wedged_restart_count == 0
    assert session.spawns == 1
    assert not _wedged_findings(last.findings)


def test_pyright_session_persistent_wedge_ends_poisoned_not_in_a_restart_loop(
    tmp_path: Path,
) -> None:
    # Every file wedges the process, including the ones after each restart.
    # Threshold 2, run-level cap 1: files 1-2 buy the one permitted restart;
    # files 3-4 trip the cap and poison the run; file 5 is honest collateral.
    stems = [f"slow_{index}" for index in range(4)]
    modules = {stem: _write_named_module(tmp_path, stem) for stem in stems}
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        timeout_stems=set(stems),
        max_consecutive_timeout_files=2,
        max_restarts_per_run=1,
        run_state=run_state,
    ) as session:
        results = [session.resolve_calls(modules[stem], [_caller_id(stem)]) for stem in stems]
        after = session.resolve_calls(clean, [_caller_id("clean")])

    assert run_state.disabled is True
    assert run_state.disabled_reason == "pyright_poisoned"
    assert run_state.restart_count == 2  # one restart + the charge that tripped the cap
    assert run_state.wedged_restart_count == 2
    assert session.spawns == 2  # initial + one restart; the second was refused
    # The file that tripped the cap still owns its own timeout hole.
    assert results[-1].coverage == _TIMEOUT_COVERAGE
    codes = _finding_codes(results[-1].findings)
    assert FINDING_PYRIGHT_WEDGED_RESTART in codes
    assert FINDING_PYRIGHT_POISON_FRAME in codes
    assert _wedged_findings(results[-1].findings)[0]["metadata"]["outcome"] == "cap_exceeded"
    assert after.coverage == FacetCoverage.degraded(
        "pyright_poisoned", transient=True, collateral=True
    )


def test_pyright_session_wedge_restart_respects_total_latency_budget(tmp_path: Path) -> None:
    stems = ["slow_a", "slow_b"]
    modules = {stem: _write_named_module(tmp_path, stem) for stem in stems}
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        timeout_stems=set(stems),
        max_consecutive_timeout_files=2,
        spawn_latency_secs=2.0,  # the initial spawn alone exhausts the budget
        restart_latency_budget_ms=1_000,
        run_state=run_state,
    ) as session:
        results = [session.resolve_calls(modules[stem], [_caller_id(stem)]) for stem in stems]

    assert run_state.disabled is True
    assert run_state.disabled_reason == "pyright_restart_cap_exceeded"
    assert session.spawns == 1
    assert FINDING_PYRIGHT_TOTAL_RESTART_CAP_EXCEEDED in _finding_codes(results[-1].findings)
    assert results[-1].coverage == _TIMEOUT_COVERAGE


def test_pyright_session_consecutive_timeout_threshold_is_overridable(tmp_path: Path) -> None:
    slow_a = _write_named_module(tmp_path, "slow_a")
    slow_b = _write_named_module(tmp_path, "slow_b")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        timeout_stems={"slow_a", "slow_b"},
        max_consecutive_timeout_files=1,
        run_state=run_state,
    ) as session:
        first = session.resolve_calls(slow_a, [_caller_id("slow_a")])
        assert session.spawns == 2
        second = session.resolve_calls(slow_b, [_caller_id("slow_b")])

    assert _wedged_findings(first.findings)
    assert _wedged_findings(second.findings)
    assert run_state.restart_count == 2
    assert session.spawns == 3


def test_pyright_session_wedge_restart_at_ceiling_is_deferred_to_the_next_file(
    tmp_path: Path,
) -> None:
    # The streak-closing file's timeout lands with no respawn headroom left
    # under the host watchdog: the existing deferral mechanism carries the
    # restart to the next file, which pays the spawn silently.
    slow_a = _write_named_module(tmp_path, "slow_a")
    slow_b = _write_named_module(tmp_path, "slow_b")
    clean = _write_named_module(tmp_path, "clean")
    run_state = PyrightRunState()

    with RestartProbeSession(
        tmp_path,
        timeout_stems={"slow_a", "slow_b"},
        timeout_latency_secs=118.0,
        max_consecutive_timeout_files=2,
        host_file_watchdog_secs=120.0,
        run_state=run_state,
    ) as session:
        session.resolve_calls(slow_a, [_caller_id("slow_a")])
        second = session.resolve_calls(slow_b, [_caller_id("slow_b")])
        assert session.spawns == 1
        assert run_state.restart_already_charged_to_file is True
        assert run_state.ceiling_deferred_restart_count == 1
        after = session.resolve_calls(clean, [_caller_id("clean")])

    assert _wedged_findings(second.findings)[0]["metadata"]["outcome"] == "deferred_to_next_file"
    assert run_state.restart_count == 1
    assert run_state.consecutive_timeout_files == 0
    assert session.spawns == 2
    assert after.coverage == FacetCoverage()
    assert not _restart_findings(after.findings)


class ChattyTransportSession(PyrightSession):
    """Fakes the transport at the ``_read_message`` seam with a simulated clock.

    Unlike ``RestartProbeSession`` this does NOT override ``_request``: the
    request read loop itself is under test (clarion-7fc41105ea, the elspeth
    watchdog overrun). Incoming traffic is scripted per outgoing request as
    ``(gap_secs, payload)`` pairs -- notifications, the matching response, or
    a ``{"_crash": True}`` marker that kills the fake process and raises the
    transport EOF the real reader raises. A read whose granted timeout is
    smaller than the next message's gap consumes the whole grant and raises
    the same ``LspTimeoutError("LSP read")`` as the real ``_wait_readable``.
    Wall time moves only on the fake clock.
    """

    def __init__(
        self,
        project_root: Path,
        *,
        spawn_latency_secs: float = 0.0,
        **kwargs: Any,
    ) -> None:
        super().__init__(project_root, executable=sys.executable, **kwargs)
        self.clock = 5000.0
        self.spawn_latency_secs = spawn_latency_secs
        self.spawns = 0
        self.fake_process: _FakeProcess | None = None
        self.responders: dict[
            str, Callable[[dict[str, Any]], list[tuple[float, dict[str, Any]]]]
        ] = {}
        self.pending: list[tuple[float, dict[str, Any]]] = []

    def _now(self) -> float:
        return self.clock

    def _spawn_and_initialize(self, init_timeout_secs: float | None = None) -> bool:
        _ = init_timeout_secs
        self.spawns += 1
        self.clock += self.spawn_latency_secs
        self.fake_process = _FakeProcess()
        self._process = cast("Any", self.fake_process)
        return True

    def _notify(self, method: str, params: dict[str, object], *, deadline: float) -> None:
        _ = (method, params, deadline)
        self._live_process()

    def _write_message(self, message: dict[str, object], deadline: float) -> None:
        _ = deadline
        self._live_process()
        method = message.get("method")
        if isinstance(method, str) and method in self.responders:
            self.pending = list(self.responders[method](cast("dict[str, Any]", message)))

    def _read_message(self, timeout_secs: float) -> dict[str, Any]:
        read_timeout_method = "LSP read"
        if not self.pending:
            self.clock += max(timeout_secs, 0.0)
            raise LspTimeoutError(read_timeout_method)
        gap, payload = self.pending[0]
        if gap > timeout_secs:
            self.clock += max(timeout_secs, 0.0)
            raise LspTimeoutError(read_timeout_method)
        self.pending.pop(0)
        self.clock += gap
        if payload.get("_crash"):
            assert self.fake_process is not None
            self.fake_process.die()
            message = "EOF while reading LSP header"
            raise LspTransportClosedError(message)
        return payload

    def request_for_test(
        self, method: str, params: dict[str, object], timeout_secs: float
    ) -> object:
        return self._request(method, params, timeout_secs)


def _log_message() -> dict[str, Any]:
    return {"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": 3, "message": "x"}}


def _lsp_response(request: dict[str, Any], result: object) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request["id"], "result": result}


def _chatty_responder(
    *, notifications: int, gap_secs: float, result: object
) -> Callable[[dict[str, Any]], list[tuple[float, dict[str, Any]]]]:
    """A response preceded by ``notifications`` server messages ``gap_secs`` apart."""

    def responder(request: dict[str, Any]) -> list[tuple[float, dict[str, Any]]]:
        script: list[tuple[float, dict[str, Any]]] = [
            (gap_secs, _log_message()) for _ in range(notifications)
        ]
        script.append((gap_secs, _lsp_response(request, result)))
        return script

    return responder


def test_lsp_request_total_wall_time_is_bounded_by_its_timeout(tmp_path: Path) -> None:
    """clarion-7fc41105ea root cause: the request timeout must bound the WHOLE request.

    Server-initiated traffic (logMessage, publishDiagnostics,
    workspace/configuration) between the request and its response must not
    reset the read clock: with a fresh grant per message, a single
    "budgeted" query stretches arbitrarily far past the file deadline and
    the host-watchdog ceiling built on it -- the elspeth service.py kill.
    """
    with ChattyTransportSession(tmp_path) as session:
        assert session._ensure_process()  # noqa: SLF001
        # 40 notifications, each arriving 4s apart, then the response: a
        # correct implementation times out once the 5s total grant is spent.
        session.responders["textDocument/prepareCallHierarchy"] = _chatty_responder(
            notifications=40, gap_secs=4.0, result=[]
        )
        before = session.clock
        with pytest.raises(LspTimeoutError):
            session.request_for_test("textDocument/prepareCallHierarchy", {}, 5.0)
        elapsed = session.clock - before

    assert elapsed <= 5.0 + 1e-9, (
        f"a 5s request consumed {elapsed}s of wall time: per-message timeout resets "
        "let chatter carry it past its budget"
    )


def test_analyze_file_shaped_sequence_stays_under_host_watchdog_ceiling(tmp_path: Path) -> None:
    """The invariant the host watchdog relies on (clarion-7fc41105ea).

    For one analyze_file-shaped sequence (resolve_calls then
    resolve_references on the same path) the session's total wall time --
    including the arrival spawn, an in-flight crash with its inline respawn,
    cold post-restart queries, and a chatty references pass -- stays under
    ``host_file_watchdog_secs`` minus the terminal safety margin, anchored at
    the calls pass entry.
    """
    source = "def callee():\n    pass\n\ndef caller():\n    callee()\n    callee()\n"
    module = _write_module(tmp_path, source, "svc.py")
    sites = [
        _reference_site(source, from_id="python:function:svc.caller", needle="callee", occurrence=i)
        for i in range(1, 3)
    ]
    prepare_calls = 0

    with ChattyTransportSession(tmp_path, spawn_latency_secs=8.0) as session:

        def prepare_responder(request: dict[str, Any]) -> list[tuple[float, dict[str, Any]]]:
            nonlocal prepare_calls
            prepare_calls += 1
            if prepare_calls == 1:
                # In-flight pyright death 3s into this file's first query.
                return [(3.0, {"_crash": True})]
            return _chatty_responder(notifications=40, gap_secs=4.0, result=[])(request)

        session.responders["textDocument/prepareCallHierarchy"] = prepare_responder
        # Cold post-restart reference queries on a chatty transport.
        session.responders["textDocument/definition"] = _chatty_responder(
            notifications=40, gap_secs=4.0, result=[]
        )

        started = session.clock
        calls = session.resolve_calls(module, ["python:function:svc.caller"])
        references = session.resolve_references(module, sites)
        total = session.clock - started

    window = session.host_file_watchdog_secs - FILE_DEADLINE_TERMINAL_SAFETY_MARGIN_SECS
    assert total <= window, (
        f"analyze_file-shaped sequence consumed {total}s of the {session.host_file_watchdog_secs}s "
        f"host watchdog; the plugin must stay under {window}s so the host never kills the call"
    )
    # Honesty is preserved while staying inside the window: the crash restarted
    # pyright inline and the starved queries report unresolved, not resolved.
    assert session.spawns == 2
    assert calls.coverage.is_degraded
    assert references.unresolved_reference_sites_total == len(sites)


def test_timeout_finding_recorded_during_a_files_pass_rides_that_files_result(
    tmp_path: Path,
) -> None:
    """Deliverable 1c of clarion-7fc41105ea: no finding outlives its file's pop.

    A timeout finding recorded during file B's pass must come back in B's own
    result (the host anchors findings to the analyzed path of the response
    they ride in) and must leave nothing buffered in the session afterwards.
    """
    clean = _write_module(tmp_path, "def caller():\n    pass\n", "clean_a.py")
    trouble = _write_module(tmp_path, "def caller():\n    pass\n", "trouble_b.py")

    with ChattyTransportSession(tmp_path) as session:
        session.responders["textDocument/prepareCallHierarchy"] = _chatty_responder(
            notifications=0, gap_secs=0.1, result=[]
        )
        first = session.resolve_calls(clean, ["python:function:clean_a.caller"])
        # File B's transport goes silent: its query times out at the read.
        session.responders.clear()
        second = session.resolve_calls(trouble, ["python:function:trouble_b.caller"])
        buffered = list(session._findings)  # noqa: SLF001

    assert _finding_codes(first.findings) == set()
    codes = _finding_codes(second.findings)
    assert FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT in codes
    timeout_findings = [
        finding
        for finding in second.findings
        if finding["subcode"] == FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT
    ]
    assert timeout_findings[0]["message"] == "pyright query timed out: LSP read"
    assert buffered == [], "a finding left in the session buffer would ride the NEXT file's result"


class _BlockedPipeProcess:
    """A live peer whose stdin pipe is full and never drained (clarion-e3ab8a4131).

    Models a single-threaded pyright still computing an abandoned query: it
    holds the read end open (so the write never sees ``EPIPE``) but never
    reads, so the 64 KiB pipe buffer stays full and every further write must
    wait. ``fill()`` primes the buffer so even a tiny message blocks.
    """

    def __init__(self) -> None:
        read_fd, write_fd = os.pipe()
        os.set_blocking(write_fd, False)
        self._read_fd = read_fd
        self.stdin = os.fdopen(write_fd, "wb", buffering=0)
        self.returncode: int | None = None
        self.killed = False

    def fill(self) -> None:
        chunk = b"\0" * 65536
        while True:
            try:
                os.write(self.stdin.fileno(), chunk)
            except BlockingIOError:
                return

    def close_fds(self) -> None:
        with contextlib.suppress(OSError):
            self.stdin.close()
        with contextlib.suppress(OSError):
            os.close(self._read_fd)

    def poll(self) -> int | None:
        return self.returncode

    def kill(self) -> None:
        self.killed = True
        self.returncode = -9

    def wait(self, timeout: float | None = None) -> int:
        _ = timeout
        return self.returncode if self.returncode is not None else 0


def _run_bounded(target: Callable[[], None], *, join_secs: float = 5.0) -> tuple[bool, float]:
    """Run ``target`` on a daemon thread; return (finished, wall seconds).

    The plugin has no pytest-timeout, and the pre-fix code path blocks in
    ``write()`` forever -- a direct call would hang the whole suite.
    """
    started = time.monotonic()
    worker = threading.Thread(target=target, daemon=True)
    worker.start()
    worker.join(join_secs)
    return not worker.is_alive(), time.monotonic() - started


def test_write_message_times_out_when_the_peer_stops_reading(tmp_path: Path) -> None:
    """clarion-e3ab8a4131: an LSP write must be bounded like an LSP read.

    Pre-fix this blocked in ``process.stdin.write`` until the host's 120 s
    per-file watchdog SIGKILLed the plugin and lost the whole run.
    """
    session = PyrightSession(tmp_path, executable=sys.executable)
    process = _BlockedPipeProcess()
    session._process = cast("Any", process)  # noqa: SLF001
    process.fill()
    outcome: list[BaseException | None] = []

    def write_it() -> None:
        try:
            session._write_message(  # noqa: SLF001
                {
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {"textDocument": {"text": "x" * (1 << 20)}},
                },
                deadline=session._now() + 0.5,  # noqa: SLF001
            )
        except Exception as exc:  # noqa: BLE001
            outcome.append(exc)
        else:
            outcome.append(None)

    finished, elapsed = _run_bounded(write_it)
    process.close_fds()

    assert finished, "the stdin write outlived its deadline: the write is still unbounded"
    assert elapsed < 2.0, f"write took {elapsed:.2f}s for a 0.5s deadline"
    raised = outcome[0]
    assert isinstance(raised, LspTimeoutError)
    assert "textDocument/didOpen" in raised.method


class DidOpenWriteTimeoutSession(PyrightSession):
    """A live pyright that has stopped reading stdin: every ``didOpen`` write times out."""

    def __init__(self, project_root: Path, **kwargs: Any) -> None:
        kwargs.setdefault("executable", sys.executable)
        super().__init__(project_root, **kwargs)
        self._process = cast("Any", _FakeProcess())
        self.written: list[str] = []

    def _ensure_process(self) -> bool:
        return True

    def _write_message(self, message: dict[str, object], deadline: float) -> None:
        _ = deadline
        method = str(message.get("method"))
        self.written.append(method)
        if method == "textDocument/didOpen":
            label = f"{method} (write)"
            raise LspTimeoutError(label)


def test_reference_pass_didopen_write_timeout_is_reported_as_pyright_timeout(
    tmp_path: Path,
) -> None:
    """A write timeout is attributed exactly like a read timeout (ADR-057 §1).

    It is this file's own budget expiring against a live process, so:
    self-inflicted ``pyright_timeout``, transient, not collateral -- and the
    pass RETURNS instead of blocking in ``write()``.
    """
    source = "def alpha():\n    pass\n\nFIRST = alpha\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(source, from_id="python:module:demo", needle="alpha", occurrence=1)

    run_state = PyrightRunState()
    session = DidOpenWriteTimeoutSession(tmp_path, run_state=run_state)
    results: list[Any] = []
    finished, _elapsed = _run_bounded(
        lambda: results.append(session.resolve_references(module, [site]))
    )

    assert finished, "resolve_references blocked on the didOpen write"
    result = results[0]
    assert result.coverage == FacetCoverage.degraded("pyright_timeout", transient=True)
    assert result.coverage.collateral is False
    assert result.edges == []
    assert result.unresolved_reference_sites_total == 1
    assert FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT in _finding_codes(result.findings)
    assert run_state.restart_count == 0


def test_close_bounds_its_shutdown_write(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``close()``'s teardown traffic is bounded by the shutdown grant, then kills."""
    monkeypatch.setattr(pyright_session_module, "PYRIGHT_SHUTDOWN_TIMEOUT_SECS", 0.5)
    session = PyrightSession(tmp_path, executable=sys.executable)
    process = _BlockedPipeProcess()
    session._process = cast("Any", process)  # noqa: SLF001
    process.fill()

    finished, elapsed = _run_bounded(session.close)
    killed = process.killed
    process.close_fds()

    assert finished, "close() blocked writing shutdown to a peer that stopped reading"
    assert elapsed < 2.0, f"close() took {elapsed:.2f}s for a 0.5s shutdown grant"
    assert killed


def test_position_to_byte_uses_the_cached_lines(tmp_path: Path) -> None:
    """The per-file line split is built once, not re-split per call site.

    Pre-fix ``_position_to_byte`` ran ``source.splitlines(keepends=True)``
    every call -- twice per unresolved site, 4,473 sites on elspeth's
    13.6k-line ``tool_batch.py``.
    """
    source = "".join(f"value_{n} = {n}\n" for n in range(20000))
    index = _build_function_index(tmp_path, tmp_path / "big.py", source)

    assert len(index.lines) == 20000

    started = time.monotonic()
    for line in range(5000):
        assert _position_to_byte(index, line, 3) == index.line_starts[line] + 3
    elapsed = time.monotonic() - started

    assert elapsed < 2.0, f"5000 _position_to_byte calls took {elapsed:.2f}s"


class _BlockedPipeSession(PyrightSession):
    """A live pyright over a REAL pipe that it has stopped reading.

    No seam is faked below ``_ensure_process``: the didOpen goes through the
    production ``_notify`` -> ``_write_message`` -> ``_write_all`` path and
    stalls on a genuine ``EAGAIN``.
    """

    def __init__(self, project_root: Path, *, prime_full: bool = True, **kwargs: Any) -> None:
        kwargs.setdefault("executable", sys.executable)
        super().__init__(project_root, **kwargs)
        self.blocked = _BlockedPipeProcess()
        if prime_full:
            self.blocked.fill()
        self._process = cast("Any", self.blocked)

    def _ensure_process(self) -> bool:
        return self._process is not None


def test_reference_pass_survives_a_real_blocked_pipe_as_pyright_timeout(tmp_path: Path) -> None:
    """End-to-end over a real pipe: a stalled didOpen degrades, it does not hang.

    The peer is alive and holding the read end open (no ``EPIPE``), so this is
    the elspeth shape exactly -- and with the whole message still unwritten
    there is no half-frame, so the transport is NOT invalidated.
    """
    source = "def alpha():\n    pass\n\nFIRST = alpha\n"
    module = _write_module(tmp_path, source)
    site = _reference_site(source, from_id="python:module:demo", needle="alpha", occurrence=1)

    run_state = PyrightRunState()
    session = _BlockedPipeSession(
        tmp_path,
        run_state=run_state,
        file_timeout_base_secs=0.5,
        file_timeout_per_function_secs=0.0,
    )
    results: list[Any] = []
    finished, elapsed = _run_bounded(
        lambda: results.append(session.resolve_references(module, [site])),
    )
    killed = session.blocked.killed
    session.blocked.close_fds()

    assert finished, "the references pass blocked in write() on a real full pipe"
    assert elapsed < 3.0, f"took {elapsed:.2f}s for a 0.5s file budget"
    result = results[0]
    assert result.coverage == FacetCoverage.degraded("pyright_timeout", transient=True)
    assert result.coverage.collateral is False
    assert result.unresolved_reference_sites_total == 1
    # Nothing reached pyright, so the stream is still coherent: no kill, no
    # restart, no charge.
    assert not killed
    assert run_state.restart_count == 0
    assert run_state.restart_already_charged_to_file is False


def test_partial_write_timeout_invalidates_the_transport_without_charging_the_run(
    tmp_path: Path,
) -> None:
    """A half-written frame must kill pyright, uncharged (clarion-e3ab8a4131).

    With room in the pipe the write delivers a ``Content-Length`` header and
    part of the body before stalling. pyright can never resynchronise from
    that: it would splice the next file's didOpen onto the half-message, and
    every later request would read-time-out until the wedge breaker respawned
    three files later -- pinning three innocent files with sticky
    SELF-INFLICTED ``pyright_timeout`` marks (ADR-057 §4). So the transport is
    invalidated here, and because the handle is dropped the next
    ``_ensure_process`` spawns silently: no run-level restart charge, no
    collateral ``pyright_restarting`` on the next file.
    """
    run_state = PyrightRunState()
    session = PyrightSession(tmp_path, executable=sys.executable, run_state=run_state)
    process = _BlockedPipeProcess()  # deliberately NOT primed: ~64 KiB free.
    session._process = cast("Any", process)  # noqa: SLF001
    outcome: list[BaseException | None] = []

    def write_it() -> None:
        try:
            session._write_message(  # noqa: SLF001
                {
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {"textDocument": {"text": "x" * (1 << 20)}},
                },
                deadline=session._now() + 0.5,  # noqa: SLF001
            )
        except Exception as exc:  # noqa: BLE001
            outcome.append(exc)
        else:
            outcome.append(None)

    finished, elapsed = _run_bounded(write_it)
    process.close_fds()

    assert finished
    assert elapsed < 2.0
    raised = outcome[0]
    assert isinstance(raised, LspWriteTimeoutError)
    assert raised.bytes_written > 0, "the pipe had room; this is the partial-frame case"
    assert raised.method.endswith("(partial write)")
    # Transport invalidated...
    assert process.killed
    assert session._process is None  # noqa: SLF001
    # ...but nothing is charged: the next spawn is silent (ADR-057 §3).
    assert run_state.restart_already_charged_to_file is True
    assert run_state.restart_charged_to_path is None
    assert run_state.restart_count == 0
    assert run_state.file_attributed_restart_count == 0


class _ExitWriteBlockedSession(PyrightSession):
    """Answers ``shutdown`` after spending its whole grant, then stops reading.

    Wall time moves only on the simulated clock, so the ``exit`` write's real
    bound is whatever ``close()`` has left -- 0 s with a shared deadline, a
    fresh 5 REAL seconds with a per-message one. The test discriminates.
    """

    def __init__(self, project_root: Path) -> None:
        super().__init__(project_root, executable=sys.executable)
        self.clock = 4000.0
        self.request_timeouts: list[tuple[str, float]] = []
        self.blocked = _BlockedPipeProcess()
        self.blocked.fill()
        self._process = cast("Any", self.blocked)

    def _now(self) -> float:
        return self.clock

    def _request(self, method: str, params: dict[str, object], timeout_secs: float) -> object:
        _ = params
        self.request_timeouts.append((method, timeout_secs))
        self.clock += timeout_secs
        return {}


def test_close_shares_one_deadline_across_shutdown_and_exit(tmp_path: Path) -> None:
    """close()'s teardown is capped in total, not per message.

    The recycle at ``MAX_FILES_PER_PYRIGHT_SESSION`` runs inside an
    ``analyze_file`` call, so a per-message grant would let a wedged server
    spend twice the shutdown budget out of the file deadline's terminal
    safety margin under the host's 120 s watchdog.
    """
    session = _ExitWriteBlockedSession(tmp_path)

    finished, elapsed = _run_bounded(session.close)
    killed = session.blocked.killed
    session.blocked.close_fds()

    assert finished, "close() blocked writing exit to a peer that stopped reading"
    assert session.request_timeouts == [("shutdown", PYRIGHT_SHUTDOWN_TIMEOUT_SECS)]
    # The shutdown consumed the whole grant on the simulated clock, so the
    # exit write has none left and is cut off at once. A fresh per-message
    # grant would have waited PYRIGHT_SHUTDOWN_TIMEOUT_SECS of REAL time.
    assert elapsed < 1.0, f"exit write got its own fresh grant: {elapsed:.2f}s"
    assert killed


class _SpawnCountingSession(PyrightSession):
    """Fakes only the spawn; every other lifecycle path is production code."""

    def __init__(self, project_root: Path, *, run_state: PyrightRunState | None = None) -> None:
        super().__init__(project_root, executable=sys.executable, run_state=run_state)
        self.spawns = 0

    def _spawn_and_initialize(self, init_timeout_secs: float | None = None) -> bool:
        _ = init_timeout_secs
        self.spawns += 1
        self._process = cast("Any", _FakeProcess())
        return True


def test_any_spawn_consumes_the_one_shot_uncharged_restart_flag(tmp_path: Path) -> None:
    """The uncharged-restart flag can never outlive the spawn that spends it.

    ``_invalidate_partial_frame`` arms ADR-057 §3's one-shot flag, and the
    wedge breaker's ``_restart_process_for_file`` reaches ``_start_process``
    WITHOUT passing through ``_ensure_process`` -- the one route that used to
    leave it armed. With a live process then in hand, later
    ``_ensure_process`` calls take their ``poll() is None`` branch and never
    consume it, so the next genuine dead-on-arrival would be spawned silently:
    one FINDING_PYRIGHT_RESTART and one MAX_PYRIGHT_RESTARTS_PER_RUN slot lost.
    """
    run_state = PyrightRunState()
    session = _SpawnCountingSession(tmp_path, run_state=run_state)
    module = tmp_path / "demo.py"

    # A half-written didOpen invalidated the transport: kill + arm the flag.
    session._path_in_flight = module  # noqa: SLF001
    session._process = cast("Any", _FakeProcess())  # noqa: SLF001
    session._invalidate_partial_frame()  # noqa: SLF001
    assert run_state.restart_already_charged_to_file is True
    assert session._process is None  # noqa: SLF001

    # The wedge breaker respawns for the same file, bypassing _ensure_process.
    session._file_started_at[module] = session._now()  # noqa: SLF001
    assert session._restart_process_for_file(module) == "restarted"  # noqa: SLF001
    assert session.spawns == 1
    assert run_state.restart_already_charged_to_file is False, "the flag outlived its spawn"
    assert run_state.restart_charged_to_path is None

    # ...so an unrelated death later in the run is charged and reported as normal.
    cast("_FakeProcess", session._process).die()  # noqa: SLF001
    assert session._ensure_process() is True  # noqa: SLF001
    assert session.spawns == 2
    assert run_state.restart_count == 1
    assert FINDING_PYRIGHT_RESTART in _finding_codes(session._pop_findings())  # noqa: SLF001


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
    source = "def caller():\n    print('x')\n"
    module = _write_module(tmp_path, source)
    # A real site so the references pass reaches its wrapped final return
    # rather than the ``not sites`` early exit, which stays ``complete``
    # (zero requested sites means an unpinned interpreter cannot have cost
    # this facet any evidence -- see the sibling assertion below).
    site = _reference_site(source, from_id="python:module:demo", needle="print", occurrence=0)
    unpinned = ProjectInterpreter(path="/usr/bin/python3", source="path")

    session = RestartProbeSession(tmp_path, interpreter=unpinned)
    try:
        calls = session.resolve_calls(module, ["python:function:demo.caller"])
        references = session.resolve_references(module, [site])
        empty_references = session.resolve_references(module, [])
        # Last: ``resolve_calls`` resets the file's shared deadline window, so
        # an earlier placement would perturb the two results gathered above.
        empty_calls = session.resolve_calls(module, [])
    finally:
        session.close()

    for coverage in (calls.coverage, references.coverage):
        assert coverage.status == "degraded"
        assert coverage.reason == "interpreter_unpinned"
        assert coverage.transient is False
        assert coverage.collateral is False
    # A facet with nothing requested has no interpreter-caused hole to claim:
    # no pyright query was issued, so ``complete`` is exact rather than
    # optimistic. Both early returns must behave the same way.
    assert empty_references.coverage.status == "complete"
    assert empty_calls.coverage.status == "complete"


def test_unpinned_interpreter_never_masks_a_real_degradation(tmp_path: Path) -> None:
    module = _write_module(tmp_path, "def caller():\n    print('x')\n", name="slow.py")
    unpinned = ProjectInterpreter(path=None, source="none")

    session = RestartProbeSession(tmp_path, interpreter=unpinned, timeout_stems={"slow"})
    try:
        calls = session.resolve_calls(module, ["python:function:slow.caller"])
    finally:
        session.close()

    assert calls.coverage.reason == "pyright_timeout", calls.coverage


def _write_minimal_langserver(tmp_path: Path) -> Path:
    """A fake ``pyright-langserver`` that handshakes and answers every query emptily.

    Enough to make ``_spawn_and_initialize`` succeed for real (a subprocess, a
    real transport) without depending on pyright being installed.
    """
    return _write_executable(
        tmp_path,
        textwrap.dedent(
            """
            #!/usr/bin/env python3
            import json, sys

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


def test_path_rung_interpreter_is_unpinned_and_degrades_the_calls_facet(tmp_path: Path) -> None:
    """A venv-less project falls to the ``path`` rung, which is a guess, not a pin.

    The three pinned rungs above it are explicitly emptied (``""`` counts as
    unset on both sides of the cross-language contract), and ``tmp_path`` has
    no ``.venv`` -- so discovery reaches the stub on ``PATH``. That is exactly
    the launcher-dependent situation ADR-058 refuses to call ``complete``: the
    facet must come back ``degraded``/``interpreter_unpinned``.
    """
    stub_dir = tmp_path / "stub-bin"
    stub_dir.mkdir()
    stub_python = stub_dir / "python"
    stub_python.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    stub_python.chmod(stub_python.stat().st_mode | stat.S_IXUSR)
    script = _write_minimal_langserver(tmp_path)
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")
    assert not (tmp_path / ".venv").exists(), "rung 2 must not be reachable"

    with PyrightSession(
        tmp_path,
        executable=str(script),
        # The stub dir is PREPENDED rather than replacing PATH so the fake
        # server's `#!/usr/bin/env python3` still resolves, exactly as every
        # other fake-script test in this file assumes.
        env={
            "PATH": f"{stub_dir}{os.pathsep}{os.environ['PATH']}",
            "VIRTUAL_ENV": "",
            "CONDA_PREFIX": "",
            "LOOMWEAVE_PYTHON_INTERPRETER": "",
        },
        init_timeout_secs=5.0,
    ) as session:
        assert session.interpreter.source == "path"
        assert session.interpreter.pinned is False
        assert session.interpreter.path == str(stub_python)
        result = session.resolve_calls(module, ["python:function:demo.caller"])

    assert result.coverage.status == "degraded"
    assert result.coverage.reason == "interpreter_unpinned"
    assert result.coverage.transient is False


def test_interpreter_is_announced_once_per_session_not_once_per_spawn(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """``_interpreter_announced`` is a SESSION guard, not a per-process one.

    A run recycles pyright every ``MAX_FILES_PER_PYRIGHT_SESSION`` files and
    restarts it on every crash; announcing per spawn would turn one
    orientation line into a stream of duplicates in the operator's stderr for
    an interpreter that never changed.
    """
    fake_python = tmp_path / ".venv" / "bin" / "python"
    fake_python.parent.mkdir(parents=True)
    fake_python.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    fake_python.chmod(fake_python.stat().st_mode | stat.S_IXUSR)
    script = _write_minimal_langserver(tmp_path)
    module = _write_module(tmp_path, "def caller():\n    print('x')\n")

    with PyrightSession(
        tmp_path,
        executable=str(script),
        env={"LOOMWEAVE_PYTHON_INTERPRETER": ""},
        init_timeout_secs=5.0,
    ) as session:
        assert session.interpreter.source == "dotvenv"
        session.resolve_calls(module, ["python:function:demo.caller"])
        first = capsys.readouterr().err
        # A killed process forces a second, real `_spawn_and_initialize`.
        session.kill_for_test()
        restarted = session.resolve_calls(module, ["python:function:demo.caller"])
        second = capsys.readouterr().err

    announcements = [line for line in first.splitlines() if "pyright interpreter" in line]
    assert len(announcements) == 1, first
    assert str(fake_python) in announcements[0]
    assert "source=dotvenv" in announcements[0]
    assert "pinned=True" in announcements[0]
    # Non-vacuity FIRST: without a genuine second spawn the assertion below is
    # free, and a failure there would point at the wrong thing.
    assert FINDING_PYRIGHT_RESTART in _finding_codes(restarted.findings)
    assert "pyright interpreter" not in second, second
