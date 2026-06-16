from __future__ import annotations

import ast
import errno
import os
import shutil
import stat
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import TYPE_CHECKING, cast

import pytest

from loomweave_plugin_python.pyright_session import (
    FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT,
    FINDING_PYRIGHT_INIT_TIMEOUT,
    FINDING_PYRIGHT_INSTALL_FAILURE,
    FINDING_PYRIGHT_POISON_FRAME,
    FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT,
    FINDING_PYRIGHT_REFERENCE_SITE_CAP,
    FINDING_PYRIGHT_RESOURCE_EXHAUSTED,
    FINDING_PYRIGHT_RESTART,
    FINDING_PYRIGHT_SPAWN_DEFERRED,
    FINDING_PYRIGHT_UNAVAILABLE,
    MAX_CONSECUTIVE_SPAWN_DEFERRALS,
    LspTimeoutError,
    PyrightRunState,
    PyrightSession,
    _build_function_index,
    _CallSite,
    _containing_function_id,
    _filter_relation_candidates,
    _FunctionIndex,
    _FunctionInfo,
    _merge_reference_site,
    _reference_accumulator_to_edge,
    _unresolved_call_site_total_for_function,
    _unresolved_call_sites_for_function,
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

    with PyrightSession(tmp_path, executable="loomweave-missing-pyright") as session:
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
