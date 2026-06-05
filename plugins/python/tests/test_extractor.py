"""Unit tests for the AST → function-entity extractor (WP3 Task 4)."""

from __future__ import annotations

import json
import shutil
import sys
import textwrap
from pathlib import Path
from typing import TYPE_CHECKING, Literal, cast

import pytest

from loomweave_plugin_python.call_resolver import CallResolutionResult
from loomweave_plugin_python.extractor import (
    ExtractResult,
    ImportsEdgeProperties,
    RawEdge,
    _module_source_range,
    extract,
    extract_with_stats,
    module_dotted_name,
)
from loomweave_plugin_python.pyright_session import PyrightSession
from loomweave_plugin_python.reference_resolver import ReferenceResolutionResult, ReferenceSite
from loomweave_plugin_python.wardline_descriptor import DescriptorEntry, WardlineVocabulary

if TYPE_CHECKING:
    from collections.abc import Sequence

WARDLINE_QUALNAME_FIXTURE = (
    Path(__file__).resolve().parents[3]
    / "docs/federation/fixtures/wardline-qualname-normalization.json"
)


class FakeCallResolver:
    def resolve_calls(
        self,
        file_path: str | Path,
        function_ids: Sequence[str],
    ) -> CallResolutionResult:
        assert file_path == "demo.py"
        assert function_ids == [
            "python:function:demo.callee",
            "python:function:demo.caller",
        ]
        return CallResolutionResult(
            edges=[
                {
                    "kind": "calls",
                    "from_id": "python:function:demo.caller",
                    "to_id": "python:function:demo.callee",
                    "confidence": "resolved",
                    "source_byte_start": 42,
                    "source_byte_end": 48,
                },
            ],
            unresolved_call_sites_total=2,
            unresolved_call_sites=[
                {
                    "caller_entity_id": "python:function:demo.caller",
                    "site_ordinal": 0,
                    "source_byte_start": 42,
                    "source_byte_end": 48,
                    "callee_expr": "callee",
                },
            ],
            pyright_query_latency_ms=[17],
        )


class RecordingReferenceResolver:
    def __init__(self) -> None:
        self.file_path: str | Path | None = None
        self.sites: list[ReferenceSite] = []

    def resolve_references(
        self,
        file_path: str | Path,
        sites: Sequence[ReferenceSite],
    ) -> ReferenceResolutionResult:
        self.file_path = file_path
        self.sites = list(sites)
        return ReferenceResolutionResult(reference_sites_total=len(self.sites))


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


def _extract_with_pyright(
    tmp_path: Path,
    source: str,
    pyright_langserver: str,
    *,
    name: str = "demo.py",
) -> ExtractResult:
    path = tmp_path / name
    rendered = textwrap.dedent(source).lstrip()
    if not rendered.endswith("\n"):
        rendered = f"{rendered}\n"
    path.write_text(rendered, encoding="utf-8")
    with PyrightSession(tmp_path, executable=pyright_langserver) as resolver:
        return extract_with_stats(
            rendered,
            str(path),
            module_prefix_path=name,
            call_resolver=resolver,
        )


def _call_edges(edges: Sequence[RawEdge]) -> list[RawEdge]:
    return [edge for edge in edges if edge["kind"] == "calls"]


def _import_edges(edges: Sequence[RawEdge]) -> list[RawEdge]:
    return [edge for edge in edges if edge["kind"] == "imports"]


def _import_properties(edge: RawEdge) -> ImportsEdgeProperties:
    return cast("ImportsEdgeProperties", edge["properties"])


def _reference_sites_for(source: str) -> list[ReferenceSite]:
    resolver = RecordingReferenceResolver()
    extract_with_stats(source, "demo.py", reference_resolver=resolver)
    assert resolver.file_path == "demo.py"
    return resolver.sites


def test_reference_site_module_level_name_read_owned_by_module() -> None:
    source = "def world():\n    pass\n\nCONST_REF = world\n"

    sites = _reference_sites_for(source)

    assert sites == [
        ReferenceSite(
            from_id="python:module:demo",
            line=3,
            character=12,
            end_line=3,
            end_character=17,
            source_byte_start=source.encode().find(b"world", source.encode().find(b"CONST_REF")),
            source_byte_end=source.encode().find(b"world", source.encode().find(b"CONST_REF"))
            + len(b"world"),
            kind="name",
        ),
    ]


def test_reference_site_function_annotations_owned_by_function() -> None:
    sites = _reference_sites_for(
        "class Foo:\n    pass\n\ndef annotated(x: Foo) -> Foo:\n    return x\n",
    )

    assert [(site.from_id, site.kind) for site in sites] == [
        ("python:function:demo.annotated", "annotation"),
        ("python:function:demo.annotated", "annotation"),
    ]


def test_reference_site_class_body_annotation_owned_by_class() -> None:
    sites = _reference_sites_for(
        "class Foo:\n    pass\n\nclass Box:\n    item: Foo\n",
    )

    assert [(site.from_id, site.kind) for site in sites] == [
        ("python:class:demo.Box", "annotation"),
    ]


def test_reference_site_nested_subscripted_annotation_records_inner_token() -> None:
    source = (
        "class Foo:\n"
        "    pass\n\n"
        "def annotated(items: list[Foo]) -> dict[str, Foo]:\n"
        "    return {}\n"
    )
    sites = _reference_sites_for(
        source,
    )
    foo_start_positions = [
        source.encode().find(b"Foo", source.encode().find(b"list")),
        source.encode().rfind(b"Foo"),
    ]
    foo_sites = [site for site in sites if site.source_byte_start in foo_start_positions]

    assert [(site.from_id, site.kind) for site in foo_sites] == [
        ("python:function:demo.annotated", "annotation"),
        ("python:function:demo.annotated", "annotation"),
    ]
    assert all(site.source_byte_start < site.source_byte_end for site in foo_sites)


def test_reference_site_call_callee_ranges_are_suppressed() -> None:
    sites = _reference_sites_for(
        "def world():\n    pass\n\ndef caller():\n    return world()\n\nCONST_REF = world\n",
    )

    assert [(site.from_id, site.kind) for site in sites] == [
        ("python:module:demo", "name"),
    ]


def test_reference_site_subclass_and_decorator_expressions_are_excluded() -> None:
    sites = _reference_sites_for(
        "class Foo:\n"
        "    pass\n\n"
        "def deco(fn):\n"
        "    return fn\n\n"
        "@deco\n"
        "def target():\n"
        "    pass\n\n"
        "class Child(Foo):\n"
        "    pass\n",
    )

    assert sites == []


def test_reference_site_byte_offsets_handle_non_ascii_prefix() -> None:
    source = "éé = 1\nclass Foo:\n    pass\nCONST_REF = Foo\n"

    sites = _reference_sites_for(source)

    expected_start = source.encode().rfind(b"Foo")
    assert sites == [
        ReferenceSite(
            from_id="python:module:demo",
            line=3,
            character=12,
            end_line=3,
            end_character=15,
            source_byte_start=expected_start,
            source_byte_end=expected_start + len(b"Foo"),
            kind="name",
        ),
    ]


def test_empty_file_yields_one_module_entity() -> None:
    """B.2 Q1 supersession of Sprint-1 UQ-WP3-11: empty file produces one module entity, not [].

    The function-extraction part of UQ-WP3-11 still holds: zero *function* entities.
    """
    entities, _ = extract("", "empty.py")
    assert len(entities) == 1
    assert entities[0]["kind"] == "module"
    assert entities[0].get("parse_status") == "ok"
    function_entities = [e for e in entities if e["kind"] == "function"]
    assert function_entities == []


def test_extractor_with_noop_resolver_emits_no_calls() -> None:
    entities, edges = extract("def caller():\n    pass\n", "demo.py")

    assert [e["id"] for e in entities if e["kind"] == "function"] == [
        "python:function:demo.caller",
    ]
    assert [edge for edge in edges if edge["kind"] == "calls"] == []


def test_function_entity_carries_sei_signature() -> None:
    # ADR-038 REQ-C-01: functions emit a versioned signature with params +
    # return annotation; modules carry none (the move case abstains).
    entities, _ = extract(
        "def f(x: int, y: str = 'a', *args, z: bool, **kw) -> bool:\n    return z\n",
        "demo.py",
    )
    func = next(e for e in entities if e["kind"] == "function")
    assert func["signature"] == {
        "v": 1,
        "params": ["x: int", "y: str", "*args", "z: bool", "**kw"],
        "return_ann": "bool",
    }
    module = next(e for e in entities if e["kind"] == "module")
    assert "signature" not in module


def test_function_signature_omits_missing_annotations() -> None:
    entities, _ = extract("def g(a, b):\n    return a\n", "demo.py")
    func = next(e for e in entities if e["kind"] == "function")
    assert func["signature"] == {"v": 1, "params": ["a", "b"], "return_ann": None}


def test_class_entity_carries_base_signature() -> None:
    entities, _ = extract(
        "class C(Base, mixins.M):\n    pass\n",
        "demo.py",
    )
    cls = next(e for e in entities if e["kind"] == "class")
    assert cls["signature"] == {"v": 1, "bases": ["Base", "mixins.M"]}


def test_function_signature_is_stable_across_extractions() -> None:
    # The matcher compares signatures by string equality, so the emitted shape
    # must be deterministic for identical source.
    source = "def h(p: dict[str, int]) -> None:\n    pass\n"
    first = next(e for e in extract(source, "a.py")[0] if e["kind"] == "function")
    second = next(e for e in extract(source, "a.py")[0] if e["kind"] == "function")
    assert first["signature"] == second["signature"]


def test_import_statement_emits_module_import_edge() -> None:
    _entities, edges = extract("import pkg.service\n", "consumer.py")

    assert _import_edges(edges) == [
        {
            "kind": "imports",
            "from_id": "python:module:consumer",
            "to_id": "python:module:pkg.service",
            "source_byte_start": 0,
            "source_byte_end": len(b"import pkg.service"),
            "confidence": "resolved",
            "properties": {
                "imported_name": "pkg.service",
                "import_style": "import",
                "level": 0,
            },
        },
    ]


def test_from_import_emits_import_edge_to_parent_module() -> None:
    _entities, edges = extract("from pkg.service import Client\n", "consumer.py")

    assert _import_edges(edges) == [
        {
            "kind": "imports",
            "from_id": "python:module:consumer",
            "to_id": "python:module:pkg.service",
            "source_byte_start": 0,
            "source_byte_end": len(b"from pkg.service import Client"),
            "confidence": "resolved",
            "properties": {
                "imported_name": "Client",
                "import_style": "from_import",
                "level": 0,
            },
        },
    ]


def test_multi_name_from_import_emits_one_edge_per_imported_name() -> None:
    source = "from pkg.service import Client, helper, CONSTANT\n"
    _entities, edges = extract(source, "consumer.py")

    assert _import_edges(edges) == [
        {
            "kind": "imports",
            "from_id": "python:module:consumer",
            "to_id": "python:module:pkg.service",
            "source_byte_start": 0,
            "source_byte_end": len(b"from pkg.service import Client, helper, CONSTANT"),
            "confidence": "resolved",
            "properties": {
                "imported_name": imported_name,
                "import_style": "from_import",
                "level": 0,
            },
        }
        for imported_name in ("Client", "helper", "CONSTANT")
    ]


def test_relative_import_emits_package_relative_module_edge() -> None:
    _entities, edges = extract("from . import sibling\n", "pkg/consumer.py")

    assert _import_edges(edges) == [
        {
            "kind": "imports",
            "from_id": "python:module:pkg.consumer",
            "to_id": "python:module:pkg.sibling",
            "source_byte_start": 0,
            "source_byte_end": len(b"from . import sibling"),
            "confidence": "resolved",
            "properties": {
                "imported_name": "sibling",
                "import_style": "from_import",
                "level": 1,
            },
        },
    ]


def test_relative_import_from_package_init_targets_sibling_module() -> None:
    _entities, edges = extract("from . import sibling\n", "pkg/__init__.py")

    assert _import_edges(edges) == [
        {
            "kind": "imports",
            "from_id": "python:module:pkg",
            "to_id": "python:module:pkg.sibling",
            "source_byte_start": 0,
            "source_byte_end": len(b"from . import sibling"),
            "confidence": "resolved",
            "properties": {
                "imported_name": "sibling",
                "import_style": "from_import",
                "level": 1,
            },
        },
    ]


def test_level_two_relative_import_from_package_init_targets_parent_sibling() -> None:
    _entities, edges = extract("from .. import sibling\n", "pkg/sub/__init__.py")

    assert _import_edges(edges) == [
        {
            "kind": "imports",
            "from_id": "python:module:pkg.sub",
            "to_id": "python:module:pkg.sibling",
            "source_byte_start": 0,
            "source_byte_end": len(b"from .. import sibling"),
            "confidence": "resolved",
            "properties": {
                "imported_name": "sibling",
                "import_style": "from_import",
                "level": 2,
            },
        },
    ]


def test_level_two_relative_import_module_from_package_init_targets_parent_module() -> None:
    _entities, edges = extract("from ..other import x\n", "pkg/sub/__init__.py")

    assert _import_edges(edges) == [
        {
            "kind": "imports",
            "from_id": "python:module:pkg.sub",
            "to_id": "python:module:pkg.other",
            "source_byte_start": 0,
            "source_byte_end": len(b"from ..other import x"),
            "confidence": "resolved",
            "properties": {
                "imported_name": "x",
                "import_style": "from_import",
                "level": 2,
            },
        },
    ]


def test_level_three_relative_import_from_deep_package_init_targets_root_sibling() -> None:
    _entities, edges = extract("from ... import x\n", "pkg/sub/deeper/__init__.py")

    assert _import_edges(edges) == [
        {
            "kind": "imports",
            "from_id": "python:module:pkg.sub.deeper",
            "to_id": "python:module:pkg.x",
            "source_byte_start": 0,
            "source_byte_end": len(b"from ... import x"),
            "confidence": "resolved",
            "properties": {
                "imported_name": "x",
                "import_style": "from_import",
                "level": 3,
            },
        },
    ]


def test_type_checking_and_function_local_imports_carry_runtime_scope() -> None:
    source = (
        "from typing import TYPE_CHECKING\n"
        "if TYPE_CHECKING:\n"
        "    import pkg.types\n"
        "\n"
        "def load():\n"
        "    import pkg.local\n"
    )
    _entities, edges = extract(source, "consumer.py")
    imports_by_target = {edge["to_id"]: edge for edge in _import_edges(edges)}

    assert imports_by_target["python:module:pkg.types"]["properties"] == {
        "imported_name": "pkg.types",
        "import_style": "import",
        "level": 0,
        "type_only": True,
    }
    assert imports_by_target["python:module:pkg.local"]["properties"] == {
        "imported_name": "pkg.local",
        "import_style": "import",
        "level": 0,
        "scope": "function",
    }


def test_type_checking_boolean_guards_are_conservative() -> None:
    source = (
        "from typing import TYPE_CHECKING\n"
        "runtime_flag = True\n"
        "if TYPE_CHECKING or runtime_flag:\n"
        "    import pkg.runtime_or\n"
        "if TYPE_CHECKING and runtime_flag:\n"
        "    import pkg.type_and\n"
        "if not TYPE_CHECKING:\n"
        "    import pkg.not_type_checking\n"
        "if typing.TYPE_CHECKING:\n"
        "    import pkg.typing_attr\n"
        "if config.TYPE_CHECKING:\n"
        "    import pkg.other_attr\n"
    )
    _entities, edges = extract(source, "consumer.py")
    imports_by_target = {edge["to_id"]: edge for edge in _import_edges(edges)}

    assert "type_only" not in _import_properties(imports_by_target["python:module:pkg.runtime_or"])
    assert _import_properties(imports_by_target["python:module:pkg.type_and"])["type_only"] is True
    assert "type_only" not in _import_properties(
        imports_by_target["python:module:pkg.not_type_checking"]
    )
    assert (
        _import_properties(imports_by_target["python:module:pkg.typing_attr"])["type_only"] is True
    )
    assert "type_only" not in _import_properties(imports_by_target["python:module:pkg.other_attr"])


def test_import_edges_have_source_byte_range_and_resolved_confidence() -> None:
    source = "é = 1\nimport pkg.service\n"
    _entities, edges = extract(source, "consumer.py")
    import_edge = _import_edges(edges)[0]

    expected_start = len("é = 1\n".encode())
    assert import_edge["source_byte_start"] == expected_start
    assert import_edge["source_byte_end"] == expected_start + len(b"import pkg.service")
    assert import_edge["confidence"] == "resolved"


def test_extractor_appends_calls_from_resolver_and_carries_stats() -> None:
    result = extract_with_stats(
        "def callee():\n    pass\n\ndef caller():\n    callee()\n",
        "demo.py",
        call_resolver=FakeCallResolver(),
    )

    assert [edge for edge in result.edges if edge["kind"] == "calls"] == [
        {
            "kind": "calls",
            "from_id": "python:function:demo.caller",
            "to_id": "python:function:demo.callee",
            "confidence": "resolved",
            "source_byte_start": 42,
            "source_byte_end": 48,
        },
    ]
    assert result.stats.unresolved_call_sites_total == 2
    assert result.stats.unresolved_call_sites == [
        {
            "caller_entity_id": "python:function:demo.caller",
            "site_ordinal": 0,
            "source_byte_start": 42,
            "source_byte_end": 48,
            "callee_expr": "callee",
        },
    ]
    assert result.stats.pyright_query_latency_ms == [17]
    assert result.stats.pyright_index_parse_latency_ms == []
    assert result.stats.extractor_parse_latency_ms > 0


@pytest.mark.pyright
def test_extractor_emits_resolved_calls(tmp_path: Path, pyright_langserver: str) -> None:
    result = _extract_with_pyright(
        tmp_path,
        """
        def callee():
            pass

        def caller():
            callee()
        """,
        pyright_langserver,
    )

    calls = _call_edges(result.edges)
    assert len(calls) == 1
    assert calls[0]["from_id"] == "python:function:demo.caller"
    assert calls[0]["to_id"] == "python:function:demo.callee"
    assert calls[0]["confidence"] == "resolved"
    assert calls[0]["source_byte_start"] < calls[0]["source_byte_end"]


@pytest.mark.pyright
def test_extractor_skips_calls_from_dropped_duplicate_definition(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    result = _extract_with_pyright(
        tmp_path,
        """
        def callee():
            pass

        def dup():
            pass

        def dup():
            callee()
        """,
        pyright_langserver,
    )

    calls = _call_edges(result.edges)
    assert result.stats.duplicate_entities_dropped_total == 1
    assert [
        edge
        for edge in calls
        if edge["from_id"] == "python:function:demo.dup"
        and edge["to_id"] == "python:function:demo.callee"
    ] == []


@pytest.mark.pyright
def test_extractor_emits_ambiguous_calls_with_candidates(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    result = _extract_with_pyright(
        tmp_path,
        """
        from collections.abc import Callable

        def alpha() -> None:
            pass

        def beta() -> None:
            pass

        handlers: dict[str, Callable[[], None]] = {"b": beta, "a": alpha}

        def caller(key: str) -> None:
            handlers[key]()
        """,
        pyright_langserver,
    )

    calls = _call_edges(result.edges)
    assert calls == [
        {
            "kind": "calls",
            "from_id": "python:function:demo.caller",
            "to_id": "python:function:demo.alpha",
            "source_byte_start": calls[0]["source_byte_start"],
            "source_byte_end": calls[0]["source_byte_end"],
            "confidence": "ambiguous",
            "properties": {
                "candidates": [
                    "python:function:demo.alpha",
                    "python:function:demo.beta",
                ],
            },
        },
    ]


@pytest.mark.pyright
def test_extractor_no_edge_for_unresolved_external_call(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    result = _extract_with_pyright(
        tmp_path,
        """
        import os

        def caller():
            os.getcwd()
        """,
        pyright_langserver,
    )

    assert _call_edges(result.edges) == []
    assert result.stats.unresolved_call_sites_total == 1


@pytest.mark.pyright
def test_extractor_async_call_resolves(tmp_path: Path, pyright_langserver: str) -> None:
    result = _extract_with_pyright(
        tmp_path,
        """
        async def callee():
            pass

        async def caller():
            await callee()
        """,
        pyright_langserver,
    )

    calls = _call_edges(result.edges)
    assert len(calls) == 1
    assert calls[0]["from_id"] == "python:function:demo.caller"
    assert calls[0]["to_id"] == "python:function:demo.callee"


@pytest.mark.pyright
def test_extractor_decorated_callable_resolves_when_possible(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    result = _extract_with_pyright(
        tmp_path,
        """
        import functools

        def deco(fn):
            @functools.wraps(fn)
            def wrapper(*args, **kwargs):
                return fn(*args, **kwargs)
            return wrapper

        @deco
        def target():
            pass

        def caller():
            target()
        """,
        pyright_langserver,
    )

    caller_edges = [
        edge
        for edge in _call_edges(result.edges)
        if edge["from_id"] == "python:function:demo.caller"
    ]
    assert caller_edges
    assert caller_edges[0]["confidence"] in {"resolved", "ambiguous"}


@pytest.mark.pyright
def test_extractor_dunder_call_dispatch(tmp_path: Path, pyright_langserver: str) -> None:
    result = _extract_with_pyright(
        tmp_path,
        """
        class CallableThing:
            def __call__(self):
                pass

        def caller():
            thing = CallableThing()
            thing()
        """,
        pyright_langserver,
    )

    assert any(
        edge["from_id"] == "python:function:demo.caller"
        and edge["to_id"] == "python:function:demo.CallableThing.__call__"
        for edge in _call_edges(result.edges)
    )


def test_whitespace_only_file_yields_one_module_entity() -> None:
    """Whitespace + comment-only file → one module entity (parse_status='ok'), zero functions."""
    entities, _ = extract("\n\n# just a comment\n", "empty.py")
    assert len(entities) == 1
    assert entities[0]["kind"] == "module"
    assert entities[0].get("parse_status") == "ok"


def test_module_level_function() -> None:
    entities, _ = extract("def hello():\n    pass\n", "demo.py")
    function_entities = [e for e in entities if e["kind"] == "function"]
    assert len(function_entities) == 1
    entity = function_entities[0]
    assert entity["id"] == "python:function:demo.hello"
    assert entity["kind"] == "function"
    assert entity["qualified_name"] == "demo.hello"
    assert entity["source"]["file_path"] == "demo.py"
    assert entity["source"]["source_range"]["start_line"] == 1
    assert entity["source"]["source_range"]["start_col"] == 0


def test_class_method() -> None:
    entities, _ = extract("class Foo:\n    def bar(self):\n        pass\n", "demo.py")
    function_entities = [e for e in entities if e["kind"] == "function"]
    assert len(function_entities) == 1
    assert function_entities[0]["id"] == "python:function:demo.Foo.bar"


def test_nested_function_emits_both_outer_and_inner() -> None:
    entities, _ = extract("def outer():\n    def inner():\n        pass\n", "demo.py")
    function_ids = {e["id"] for e in entities if e["kind"] == "function"}
    assert function_ids == {
        "python:function:demo.outer",
        "python:function:demo.outer.<locals>.inner",
    }


def test_async_function() -> None:
    entities, _ = extract("async def aloha():\n    pass\n", "demo.py")
    function_entities = [e for e in entities if e["kind"] == "function"]
    assert len(function_entities) == 1
    assert function_entities[0]["id"] == "python:function:demo.aloha"


def test_nested_class_method() -> None:
    source = "class Outer:\n    class Inner:\n        def method(self):\n            pass\n"
    entities, _ = extract(source, "demo.py")
    function_entities = [e for e in entities if e["kind"] == "function"]
    assert len(function_entities) == 1
    assert function_entities[0]["id"] == "python:function:demo.Outer.Inner.method"


def test_syntax_error_emits_degraded_module_entity_and_logs_to_stderr(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """UQ-WP3-02 + B.2 Q1: SyntaxError files now emit a degraded module entity (was: empty list)."""
    result, _ = extract("def :", "broken.py")
    assert len(result) == 1
    assert result[0]["kind"] == "module"
    assert result[0].get("parse_status") == "syntax_error"
    captured = capsys.readouterr()
    assert "broken.py" in captured.err


def test_src_prefix_stripped() -> None:
    """UQ-WP3-05: `src/pkg/module.py` → dotted module `pkg.module`."""
    entities, _ = extract("def hello():\n    pass\n", "src/pkg/module.py")
    fn = next(e for e in entities if e["kind"] == "function")
    assert fn["qualified_name"] == "pkg.module.hello"


def test_init_py_collapsed_to_package_name() -> None:
    """UQ-WP3-06: `pkg/__init__.py` → dotted `pkg` (not `pkg.__init__`).

    ``source.file_path`` stays as the literal file path; the dotted module
    used for qualified_name is the package name only.
    """
    entities, _ = extract("def pkg_helper():\n    pass\n", "pkg/__init__.py")
    fn = next(e for e in entities if e["kind"] == "function")
    assert fn["qualified_name"] == "pkg.pkg_helper"
    assert fn["source"]["file_path"] == "pkg/__init__.py"


def test_module_prefix_path_decouples_file_path_and_dotted_prefix() -> None:
    """server passes absolute file_path + relativised module_prefix_path."""
    entities, _ = extract(
        "def hello():\n    pass\n",
        "/tmp/proj/demo.py",
        module_prefix_path="demo.py",
    )
    fn = next(e for e in entities if e["kind"] == "function")
    assert fn["source"]["file_path"] == "/tmp/proj/demo.py"
    assert fn["id"] == "python:function:demo.hello"
    assert fn["qualified_name"] == "demo.hello"


def test_module_dotted_name_helper() -> None:
    fixture = json.loads(WARDLINE_QUALNAME_FIXTURE.read_text(encoding="utf-8"))
    vectors = fixture["module_normalization_vectors"]
    assert vectors
    for vector in vectors:
        assert module_dotted_name(vector["file_path"]) == vector["expected_module"], vector[
            "description"
        ]


def test_wardline_qualname_fixture_composes_with_module_dotted_name() -> None:
    fixture = json.loads(WARDLINE_QUALNAME_FIXTURE.read_text(encoding="utf-8"))
    vectors = fixture["qualified_name_vectors"]
    assert vectors
    for vector in vectors:
        module_name = module_dotted_name(vector["file_path"])
        qualname = vector["qualname"]
        qualified_name = module_name if qualname is None else f"{module_name}.{qualname}"
        assert qualified_name == vector["expected_qualified_name"], vector["description"]
        assert f"python:{vector['kind']}:{qualified_name}" == vector["expected_entity_id"], vector[
            "description"
        ]


def test_source_range_end_fields_populated() -> None:
    entities, _ = extract("def f():\n    pass\n", "d.py")
    fn = next(e for e in entities if e["kind"] == "function")
    source_range = fn["source"]["source_range"]
    assert source_range["start_line"] == 1
    assert source_range["start_col"] == 0
    assert source_range["end_line"] == 2
    assert source_range["end_col"] >= 0


def test_definition_metadata_undecorated_function() -> None:
    """Undecorated entities carry decl/body sub-ranges but no decorator keys.

    The span still starts at the ``def`` keyword line (entity_context
    evidence, clarion-460def6a51).
    """
    entities, _ = extract("def f():\n    return 1\n", "d.py")
    fn = next(e for e in entities if e["kind"] == "function")
    assert fn["source"]["source_range"]["start_line"] == 1
    definition = fn["definition"]
    assert definition["decl_line"] == 1
    assert definition["body_line_start"] == 2
    assert "decorator_line_start" not in definition
    assert "decorator_line_end" not in definition


def test_definition_metadata_decorated_class_expands_span_to_decorator() -> None:
    """A decorated class's span starts at the decorator line so an
    ``entity_at`` query on that line resolves to the class; ``definition``
    records the decorator range and the ``class`` declaration line.
    """
    source = "import dataclasses\n\n\n@dataclasses.dataclass\nclass Config:\n    retries: int = 3\n"
    entities, _ = extract(source, "runtime.py")
    cls = next(e for e in entities if e["kind"] == "class")
    # Decorator on line 4, `class Config:` on line 5.
    assert cls["source"]["source_range"]["start_line"] == 4
    definition = cls["definition"]
    assert definition["decorator_line_start"] == 4
    assert definition["decorator_line_end"] == 4
    assert definition["decl_line"] == 5
    assert definition["body_line_start"] == 6


def test_definition_metadata_decorated_function_expands_span() -> None:
    source = "import functools\n\n\n@functools.cache\ndef compute():\n    return 1\n"
    entities, _ = extract(source, "d.py")
    fn = next(e for e in entities if e["kind"] == "function")
    assert fn["source"]["source_range"]["start_line"] == 4
    definition = fn["definition"]
    assert definition["decorator_line_start"] == 4
    assert definition["decl_line"] == 5


def test_definition_metadata_multiple_decorators_uses_topmost() -> None:
    source = "import functools\n\n@staticmethod\n@functools.cache\ndef compute():\n    return 1\n"
    # Module-level function with two stacked decorators (lines 3 and 4).
    entities, _ = extract(source, "d.py")
    fn = next(e for e in entities if e["kind"] == "function")
    assert fn["source"]["source_range"]["start_line"] == 3
    definition = fn["definition"]
    assert definition["decorator_line_start"] == 3
    assert definition["decorator_line_end"] == 4
    assert definition["decl_line"] == 5


def test_categorisation_tags_and_docstrings_are_emitted() -> None:
    """WS5b root categorisations are emitted by the Python plugin, not fabricated by MCP."""

    source = """\
import dataclasses
from fastapi import APIRouter

router = APIRouter()

@router.get("/health")
def health():
    \"\"\"Report service health.\"\"\"
    return {"ok": True}

def main():
    return health()

def test_health():
    assert health()["ok"]

@dataclasses.dataclass
class Config:
    retries: int = 3
"""
    entities, _ = extract(source, "service.py")

    health = next(e for e in entities if e["id"] == "python:function:service.health")
    main = next(e for e in entities if e["id"] == "python:function:service.main")
    test = next(e for e in entities if e["id"] == "python:function:service.test_health")
    config = next(e for e in entities if e["id"] == "python:class:service.Config")

    assert health["docstring"] == "Report service health."
    assert "http-route" in health["tags"]
    assert "framework-handler" in health["tags"]
    assert "entry-point" in main["tags"]
    assert "test" in test["tags"]
    assert "data-model" in config["tags"]


def _wardline_vocabulary(
    *,
    confidence_basis: Literal["descriptor", "descriptor_version_skew"] = "descriptor",
) -> WardlineVocabulary:
    return WardlineVocabulary(
        version="wardline-generic-2",
        source="project",
        confidence_basis=confidence_basis,
        entries_by_name={
            "external_boundary": DescriptorEntry(
                canonical_name="external_boundary",
                group=1,
                attrs={},
            ),
            "trust_boundary": DescriptorEntry(
                canonical_name="trust_boundary",
                group=1,
                attrs={"_wardline_to_level": "TaintState"},
            ),
            "trusted": DescriptorEntry(
                canonical_name="trusted",
                group=1,
                attrs={"_wardline_level": "TaintState"},
            ),
        },
    )


def test_wardline_vocabulary_attaches_decorator_metadata_and_tags() -> None:
    source = """\
from weft_markers import external_boundary, trust_boundary, trusted

@external_boundary
def read_body():
    return ""

@weft_markers.trust_boundary(to_level="ASSURED")
@trusted(level="INTEGRAL")
class Sanitizer:
    pass
"""

    entities, _ = extract(source, "service.py", wardline_vocabulary=_wardline_vocabulary())

    read_body = next(e for e in entities if e["id"] == "python:function:service.read_body")
    sanitizer = next(e for e in entities if e["id"] == "python:class:service.Sanitizer")

    assert read_body["wardline"] == {
        "descriptor_version": "wardline-generic-2",
        "confidence_basis": "descriptor",
        "decorators": [
            {
                "canonical_name": "external_boundary",
                "qualified_name": "external_boundary",
                "group": 1,
                "attrs": {},
                "line": 3,
            },
        ],
    }
    assert "wardline" in read_body["tags"]
    assert "wardline:external_boundary" in read_body["tags"]

    assert sanitizer["wardline"]["decorators"] == [
        {
            "canonical_name": "trust_boundary",
            "qualified_name": "weft_markers.trust_boundary",
            "group": 1,
            "attrs": {"_wardline_to_level": "TaintState"},
            "line": 7,
        },
        {
            "canonical_name": "trusted",
            "qualified_name": "trusted",
            "group": 1,
            "attrs": {"_wardline_level": "TaintState"},
            "line": 8,
        },
    ]
    assert "wardline:trust_boundary" in sanitizer["tags"]
    assert "wardline:trusted" in sanitizer["tags"]


def test_wardline_absent_preserves_plain_extraction_without_metadata() -> None:
    source = """\
@trusted
def compute():
    return 1
"""

    entities, _ = extract(source, "service.py", wardline_vocabulary=None)

    compute = next(e for e in entities if e["id"] == "python:function:service.compute")
    assert "wardline" not in compute
    assert "tags" not in compute or "wardline" not in compute["tags"]


def test_wardline_version_skew_marks_degraded_confidence() -> None:
    source = """\
@trusted
def compute():
    return 1
"""

    entities, _ = extract(
        source,
        "service.py",
        wardline_vocabulary=_wardline_vocabulary(confidence_basis="descriptor_version_skew"),
    )

    compute = next(e for e in entities if e["id"] == "python:function:service.compute")
    assert compute["wardline"]["confidence_basis"] == "descriptor_version_skew"


def test_module_source_range_no_trailing_newline() -> None:
    """File ending without `\\n` still produces correct end_line.

    `"a\\nb"` has one newline → end_line = 2.
    """
    rng = _module_source_range("a\nb")
    assert rng == {"start_line": 1, "start_col": 0, "end_line": 2, "end_col": 0}


def test_module_source_range_crlf() -> None:
    """CRLF-terminated file produces same end_line as LF (count('\\n') handles both)."""
    rng = _module_source_range("a\r\nb\r\n")
    # Two `\n`s → end_line = 3 (one past the last terminator).
    assert rng == {"start_line": 1, "start_col": 0, "end_line": 3, "end_col": 0}


def test_module_source_range_empty_string() -> None:
    """Empty source → end_line = 1 (count is 0; +1)."""
    rng = _module_source_range("")
    assert rng == {"start_line": 1, "start_col": 0, "end_line": 1, "end_col": 0}


def test_module_entity_emitted_for_every_call() -> None:
    """Q1: every analyze produces exactly one module entity."""
    entities, _ = extract("def hello():\n    pass\n", "demo.py")
    module_entities = [e for e in entities if e["kind"] == "module"]
    assert len(module_entities) == 1
    module = module_entities[0]
    assert module["id"] == "python:module:demo"
    assert module["kind"] == "module"
    assert module["qualified_name"] == "demo"
    assert module["source"]["file_path"] == "demo.py"
    assert module["source"]["source_range"] == {
        "start_line": 1,
        "start_col": 0,
        "end_line": 3,  # "def hello():\n    pass\n" → 2 newlines + 1
        "end_col": 0,
    }
    assert module.get("parse_status") == "ok"


def test_module_entity_for_empty_file() -> None:
    """Q1: empty file produces one module entity (parse_status='ok' since ast.parse('') succeeds)."""
    entities, _ = extract("", "empty.py")
    assert len(entities) == 1
    module = entities[0]
    assert module["kind"] == "module"
    assert module["id"] == "python:module:empty"
    assert module["source"]["source_range"] == {
        "start_line": 1,
        "start_col": 0,
        "end_line": 1,
        "end_col": 0,
    }
    assert module.get("parse_status") == "ok"


def test_module_entity_for_init_py_collapses_to_package() -> None:
    """`pkg/__init__.py` produces module entity at `python:module:pkg`."""
    entities, _ = extract("", "pkg/__init__.py")
    assert len(entities) == 1
    module = entities[0]
    assert module["id"] == "python:module:pkg"
    assert module["qualified_name"] == "pkg"


def test_module_entity_for_syntax_error_file(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Q1: syntax-error file emits one module entity with parse_status='syntax_error'."""
    entities, _ = extract("def :", "broken.py")
    assert len(entities) == 1, "syntax-error file emits only the module entity"
    module = entities[0]
    assert module["kind"] == "module"
    assert module["id"] == "python:module:broken"
    assert module.get("parse_status") == "syntax_error"
    # Source range covers the broken file (formula is uniform across parse_status values).
    assert module["source"]["source_range"] == {
        "start_line": 1,
        "start_col": 0,
        "end_line": 1,  # no `\n` in "def :"
        "end_col": 0,
    }
    captured = capsys.readouterr()
    assert "broken.py" in captured.err
    assert "syntax error" in captured.err


def test_top_level_init_py_skipped_with_stderr(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Top-level `__init__.py` (no package name) returns [] + one stderr line.

    `module_dotted_name("__init__.py")` returns "" (the empty stem case).
    Emitting an entity with empty qualified_name would crash the entity-ID
    assembler at crates/loomweave-core/src/entity_id.rs:97-101.
    """
    entities, _ = extract("def helper():\n    pass\n", "__init__.py")
    assert entities == []
    captured = capsys.readouterr()
    assert "__init__.py" in captured.err
    assert "top-level __init__.py has no package name" in captured.err


def test_class_entity_simple() -> None:
    """`class Foo: pass` → one class entity + one module entity."""
    entities, _ = extract("class Foo:\n    pass\n", "demo.py")
    class_entities = [e for e in entities if e["kind"] == "class"]
    assert len(class_entities) == 1
    cls = class_entities[0]
    assert cls["id"] == "python:class:demo.Foo"
    assert cls["kind"] == "class"
    assert cls["qualified_name"] == "demo.Foo"
    assert cls["source"]["file_path"] == "demo.py"
    # Class uses real ast end_lineno data (not the module sentinel).
    sr = cls["source"]["source_range"]
    assert sr["start_line"] == 1
    assert sr["start_col"] == 0
    assert sr["end_line"] >= 1
    # parse_status MUST NOT be on class entities.
    assert "parse_status" not in cls


def test_class_entity_nested() -> None:
    """`class A: class B: pass` → two class entities (A, A.B) + one module entity."""
    entities, _ = extract("class A:\n    class B:\n        pass\n", "demo.py")
    class_ids = {e["id"] for e in entities if e["kind"] == "class"}
    assert class_ids == {
        "python:class:demo.A",
        "python:class:demo.A.B",
    }


def test_class_in_function_qualname() -> None:
    """`def f(): class C: pass` → class entity at f.<locals>.C (function-parent gets <locals>)."""
    entities, _ = extract("def f():\n    class C:\n        pass\n", "demo.py")
    class_ids = {e["id"] for e in entities if e["kind"] == "class"}
    function_ids = {e["id"] for e in entities if e["kind"] == "function"}
    assert class_ids == {"python:class:demo.f.<locals>.C"}
    assert function_ids == {"python:function:demo.f"}


def test_class_method_emitted_as_function() -> None:
    """Class methods continue as function-kind (no separate method kind)."""
    entities, _ = extract(
        "class Foo:\n    def bar(self):\n        pass\n",
        "demo.py",
    )
    class_ids = {e["id"] for e in entities if e["kind"] == "class"}
    function_ids = {e["id"] for e in entities if e["kind"] == "function"}
    assert class_ids == {"python:class:demo.Foo"}
    assert function_ids == {"python:function:demo.Foo.bar"}


def test_async_class_method() -> None:
    """`async def` inside a class still emits as function-kind."""
    entities, _ = extract(
        "class Foo:\n    async def bar(self):\n        pass\n",
        "demo.py",
    )
    function_entities = [e for e in entities if e["kind"] == "function"]
    assert len(function_entities) == 1
    assert function_entities[0]["id"] == "python:function:demo.Foo.bar"


def test_class_source_range_uses_ast_data_not_module_sentinel() -> None:
    """Class entity uses real lineno/end_lineno (not the module-entity {1,0,N,0} sentinel).

    For `class A:\\n    pass\\n`, end_lineno is 2 and end_col_offset > 0.
    """
    entities, _ = extract("class A:\n    pass\n", "demo.py")
    cls = next(e for e in entities if e["kind"] == "class")
    sr = cls["source"]["source_range"]
    # Class body extends past the header line.
    assert sr["end_line"] == 2
    # Real column data, not the module sentinel 0.
    assert sr["end_col"] > 0


# ── B.3 contains-edge + parent_id tests ─────────────────────────────────────


def test_module_emits_no_parent_id() -> None:
    """B.3 Q2: module entities have no parent_id (NotRequired absent in JSON)."""
    entities, _ = extract("", "demo.py")
    module = next(e for e in entities if e["kind"] == "module")
    assert "parent_id" not in module


def test_top_level_function_has_module_parent_id_and_contains_edge() -> None:
    """B.3 Q3: top-level function gets parent_id=module and a contains edge."""
    entities, edges = extract("def hello():\n    pass\n", "demo.py")
    module = next(e for e in entities if e["kind"] == "module")
    fn = next(e for e in entities if e["kind"] == "function")
    assert fn["parent_id"] == module["id"]
    assert {
        "kind": "contains",
        "from_id": module["id"],
        "to_id": fn["id"],
    } in edges


def test_class_method_has_class_parent_id_and_contains_edge() -> None:
    """B.3 Q3: method's parent is the enclosing class, not the module."""
    entities, edges = extract("class Foo:\n    def bar(self):\n        pass\n", "demo.py")
    cls = next(e for e in entities if e["kind"] == "class")
    method = next(e for e in entities if e["kind"] == "function")
    assert method["parent_id"] == cls["id"]
    assert {
        "kind": "contains",
        "from_id": cls["id"],
        "to_id": method["id"],
    } in edges


def test_nested_class_emits_two_contains_edges() -> None:
    """B.3 Q3: `class A: class B: pass` emits (module → A) AND (A → A.B)."""
    entities, edges = extract("class A:\n    class B:\n        pass\n", "demo.py")
    module = next(e for e in entities if e["kind"] == "module")
    outer = next(e for e in entities if e["qualified_name"] == "demo.A")
    inner = next(e for e in entities if e["qualified_name"] == "demo.A.B")
    assert outer["parent_id"] == module["id"]
    assert inner["parent_id"] == outer["id"]
    assert {"kind": "contains", "from_id": module["id"], "to_id": outer["id"]} in edges
    assert {"kind": "contains", "from_id": outer["id"], "to_id": inner["id"]} in edges


def test_function_in_function_emits_contains_edge_with_locals_qualname() -> None:
    """B.3 Q3: nested function carries <locals> in qualname; contains edge anchors to parent function."""
    entities, edges = extract("def f():\n    def g():\n        pass\n", "demo.py")
    outer = next(e for e in entities if e["qualified_name"] == "demo.f")
    inner = next(e for e in entities if e["qualified_name"] == "demo.f.<locals>.g")
    assert inner["parent_id"] == outer["id"]
    assert {"kind": "contains", "from_id": outer["id"], "to_id": inner["id"]} in edges


def test_class_in_function_emits_contains_edge() -> None:
    """B.3 Q3: class inside function — qualname carries <locals>; contains edge anchors to function."""
    entities, edges = extract("def f():\n    class C:\n        pass\n", "demo.py")
    outer = next(e for e in entities if e["qualified_name"] == "demo.f")
    inner = next(e for e in entities if e["qualified_name"] == "demo.f.<locals>.C")
    assert inner["parent_id"] == outer["id"]
    assert {"kind": "contains", "from_id": outer["id"], "to_id": inner["id"]} in edges


def test_contains_edge_has_no_source_range_fields() -> None:
    """B.3 Q5 / ADR-026 decision 3: contains edges MUST omit source_byte_start/end."""
    _, edges = extract("def hello():\n    pass\n", "demo.py")
    assert len(edges) == 1
    edge = edges[0]
    assert edge["kind"] == "contains"
    assert "source_byte_start" not in edge
    assert "source_byte_end" not in edge


def test_every_non_module_entity_has_matching_contains_edge() -> None:
    """B.3 §5 parent-id/contains consistency: every entity with parent_id has a matching edge."""
    source = (
        "def f():\n"
        "    def g():\n"
        "        pass\n"
        "class A:\n"
        "    def m(self):\n"
        "        pass\n"
        "    class B:\n"
        "        pass\n"
    )
    entities, edges = extract(source, "demo.py")
    edge_pairs = {(e["from_id"], e["to_id"]) for e in edges if e["kind"] == "contains"}
    for entity in entities:
        if entity["kind"] == "module":
            continue
        pair = (entity["parent_id"], entity["id"])
        assert pair in edge_pairs, f"missing contains edge for {entity['id']}"


# ── @typing.overload disposition (clarion-e29402d1ba) ──────────────────────────


def test_module_overload_collapses_to_implementation_entity() -> None:
    """`@overload` stubs share a qualname with the implementation; only the impl is callable.

    Per PEP 484 the stub signatures are type-checker hints — emitting them as
    entities collides on the host's UNIQUE(entities.id). Plugin must skip the
    stubs and emit one function entity whose source range is the implementation.
    """
    source = (
        "from typing import overload\n"
        "\n"
        "@overload\n"
        "def foo(x: int) -> int: ...\n"
        "@overload\n"
        "def foo(x: str) -> str: ...\n"
        "def foo(x):\n"
        "    return x\n"
    )
    entities, edges = extract(source, "demo.py")
    function_entities = [e for e in entities if e["kind"] == "function"]
    assert len(function_entities) == 1
    fn = function_entities[0]
    assert fn["id"] == "python:function:demo.foo"
    # Source range points at the implementation (the `def foo(x):` at line 7), not a stub.
    assert fn["source"]["source_range"]["start_line"] == 7
    # Exactly one contains edge for the function.
    contains_edges = [e for e in edges if e["kind"] == "contains"]
    assert contains_edges == [
        {"kind": "contains", "from_id": "python:module:demo", "to_id": fn["id"]},
    ]


def test_method_overload_collapses_to_implementation_entity_elspeth_reproducer() -> None:
    """Exact shape of elspeth ExecutionRepository.complete_node_state (3 stubs + impl)."""
    source = (
        "from typing import overload\n"
        "\n"
        "class ExecutionRepository:\n"
        "    @overload\n"
        "    def complete_node_state(self, status: int) -> int: ...\n"
        "    @overload\n"
        "    def complete_node_state(self, status: str) -> str: ...\n"
        "    @overload\n"
        "    def complete_node_state(self, status: bool) -> bool: ...\n"
        "    def complete_node_state(self, status):\n"
        "        return status\n"
    )
    entities, _ = extract(source, "demo.py")
    function_ids = [e["id"] for e in entities if e["kind"] == "function"]
    assert function_ids == [
        "python:function:demo.ExecutionRepository.complete_node_state",
    ]


def test_qualified_typing_overload_is_recognised() -> None:
    """`@typing.overload` attribute form must also collapse stubs."""
    source = (
        "import typing\n"
        "\n"
        "@typing.overload\n"
        "def foo(x: int) -> int: ...\n"
        "@typing.overload\n"
        "def foo(x: str) -> str: ...\n"
        "def foo(x):\n"
        "    return x\n"
    )
    entities, _ = extract(source, "demo.py")
    function_ids = [e["id"] for e in entities if e["kind"] == "function"]
    assert function_ids == ["python:function:demo.foo"]


def test_typing_extensions_overload_is_recognised() -> None:
    """`@typing_extensions.overload` is the same protocol as typing.overload."""
    source = (
        "import typing_extensions\n"
        "\n"
        "@typing_extensions.overload\n"
        "def foo(x: int) -> int: ...\n"
        "def foo(x):\n"
        "    return x\n"
    )
    entities, _ = extract(source, "demo.py")
    function_ids = [e["id"] for e in entities if e["kind"] == "function"]
    assert function_ids == ["python:function:demo.foo"]


def test_async_overload_is_recognised() -> None:
    """`async def` overloads collapse the same way as sync `def`."""
    source = (
        "from typing import overload\n"
        "\n"
        "@overload\n"
        "async def foo(x: int) -> int: ...\n"
        "async def foo(x):\n"
        "    return x\n"
    )
    entities, _ = extract(source, "demo.py")
    function_ids = [e["id"] for e in entities if e["kind"] == "function"]
    assert function_ids == ["python:function:demo.foo"]


def test_overload_stub_with_extra_decorators_is_still_skipped() -> None:
    """A function carrying `@overload` alongside other decorators is still a stub."""
    source = (
        "from typing import overload\n"
        "\n"
        "class Foo:\n"
        "    @staticmethod\n"
        "    @overload\n"
        "    def bar(x: int) -> int: ...\n"
        "    @staticmethod\n"
        "    def bar(x):\n"
        "        return x\n"
    )
    entities, _ = extract(source, "demo.py")
    function_ids = [e["id"] for e in entities if e["kind"] == "function"]
    assert function_ids == ["python:function:demo.Foo.bar"]


def test_references_inside_overload_implementation_body_are_emitted() -> None:
    """Skipping stub bodies must not suppress reference collection inside the impl."""
    source = (
        "from typing import overload\n"
        "\n"
        "@overload\n"
        "def foo(x: int) -> int: ...\n"
        "def foo(x):\n"
        "    return BAR\n"
    )
    sites = _reference_sites_for(source)
    # `overload` import is referenced by the decorators on stubs; those should still surface
    # at the module level. The key assertion is that the impl's body reference to `BAR`
    # is attributed to the implementation function, not lost.
    impl_sites = [s for s in sites if s.from_id == "python:function:demo.foo"]
    assert len(impl_sites) == 1
    assert impl_sites[0].kind == "name"


@pytest.mark.pyright
def test_extractor_does_not_turn_closure_local_into_module_reference(
    tmp_path: Path,
    pyright_langserver: str,
) -> None:
    source = textwrap.dedent(
        """
        def outer():
            token = object()

            def inner():
                return token

            return inner
        """,
    ).lstrip()
    path = tmp_path / "demo.py"
    path.write_text(source, encoding="utf-8")

    with PyrightSession(tmp_path, executable=pyright_langserver) as resolver:
        result = extract_with_stats(
            source,
            str(path),
            module_prefix_path="demo.py",
            reference_resolver=resolver,
        )

    references = [edge for edge in result.edges if edge["kind"] == "references"]
    assert not any(
        edge["from_id"] == "python:function:demo.outer.<locals>.inner"
        and edge["to_id"] == "python:module:demo"
        for edge in references
    )


def test_safety_net_drops_duplicate_non_overload_definitions(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Two non-overload defs sharing a qualname (rare but legal Python) must not crash the run.

    Example pattern in the wild: `@singledispatch.register` users frequently write
    `def _(arg): ...` repeatedly at the same scope. First-wins, stderr-logged,
    counter bumped, and the host never sees a duplicate id.
    """
    source = (
        "def helper(x):\n"
        "    return x + 1\n"
        "\n"
        "def helper(x):\n"  # redefinition shadows the first; same qualname.
        "    return x * 2\n"
    )
    result = extract_with_stats(source, "demo.py")
    function_entities = [e for e in result.entities if e["kind"] == "function"]
    assert len(function_entities) == 1, "duplicate ids deduped at the emit boundary"
    assert function_entities[0]["id"] == "python:function:demo.helper"
    # First-wins: source range points at the first def (line 1), not the second.
    assert function_entities[0]["source"]["source_range"]["start_line"] == 1
    assert result.stats.duplicate_entities_dropped_total == 1
    err = capsys.readouterr().err
    assert "python:function:demo.helper" in err
    assert "demo.py" in err


def test_safety_net_drops_contains_edges_to_dropped_entities() -> None:
    """A contains edge pointing at a dropped duplicate must also be dropped."""
    source = "def helper(x):\n    return x\n\ndef helper(x):\n    return x\n"
    result = extract_with_stats(source, "demo.py")
    # Both defs are top-level so contains-edges would be (module → helper) twice;
    # after dedup, exactly one survives.
    contains_edges = [e for e in result.edges if e["kind"] == "contains"]
    assert contains_edges == [
        {
            "kind": "contains",
            "from_id": "python:module:demo",
            "to_id": "python:function:demo.helper",
        },
    ]
