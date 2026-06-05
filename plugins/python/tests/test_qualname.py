"""Unit tests for L7 qualname reconstruction (WP3 Task 3).

Each test parses a short source snippet, locates the target FunctionDef/
AsyncFunctionDef, and asserts ``reconstruct_qualname`` returns the same
string that Python itself would put on ``node.__qualname__`` at runtime.

The golden strings are taken from the Python language reference
(``__qualname__`` semantics) — if one of these drifts, Wardline's
cross-product join (ADR-018) breaks.
"""

from __future__ import annotations

import ast

from loomweave_plugin_python.qualname import reconstruct_qualname


def _parse(source: str) -> ast.Module:
    return ast.parse(source)


def test_module_level_function() -> None:
    tree = _parse("def hello():\n    pass\n")
    func = tree.body[0]
    assert isinstance(func, ast.FunctionDef)
    assert reconstruct_qualname(func, [tree]) == "hello"


def test_module_level_async_function() -> None:
    tree = _parse("async def aloha():\n    pass\n")
    func = tree.body[0]
    assert isinstance(func, ast.AsyncFunctionDef)
    assert reconstruct_qualname(func, [tree]) == "aloha"


def test_nested_function_uses_locals_separator() -> None:
    tree = _parse("def outer():\n    def inner():\n        pass\n")
    outer = tree.body[0]
    assert isinstance(outer, ast.FunctionDef)
    inner = outer.body[0]
    assert isinstance(inner, ast.FunctionDef)
    assert reconstruct_qualname(inner, [tree, outer]) == "outer.<locals>.inner"


def test_class_method_omits_locals_separator() -> None:
    tree = _parse("class Foo:\n    def bar(self):\n        pass\n")
    foo = tree.body[0]
    assert isinstance(foo, ast.ClassDef)
    bar = foo.body[0]
    assert isinstance(bar, ast.FunctionDef)
    assert reconstruct_qualname(bar, [tree, foo]) == "Foo.bar"


def test_nested_class_method_chains_class_names() -> None:
    """UQ-WP3-01: `class A: class B: def c(): ...` yields `A.B.c`."""
    tree = _parse("class Outer:\n    class Inner:\n        def method(self):\n            pass\n")
    outer = tree.body[0]
    assert isinstance(outer, ast.ClassDef)
    inner = outer.body[0]
    assert isinstance(inner, ast.ClassDef)
    method = inner.body[0]
    assert isinstance(method, ast.FunctionDef)
    assert reconstruct_qualname(method, [tree, outer, inner]) == "Outer.Inner.method"


def test_function_in_class_method() -> None:
    """`class Foo: def bar(): def inner(): ...` yields `Foo.bar.<locals>.inner`."""
    source = "class Foo:\n    def bar(self):\n        def inner():\n            pass\n"
    tree = _parse(source)
    foo = tree.body[0]
    assert isinstance(foo, ast.ClassDef)
    bar = foo.body[0]
    assert isinstance(bar, ast.FunctionDef)
    inner = bar.body[0]
    assert isinstance(inner, ast.FunctionDef)
    assert reconstruct_qualname(inner, [tree, foo, bar]) == "Foo.bar.<locals>.inner"


def test_class_in_function_in_class_method() -> None:
    """`class Foo: def bar(): class Local: def meth(): ...` yields `Foo.bar.<locals>.Local.meth`.

    The `<locals>` appears once — between the function parent `bar` and the
    class parent `Local`. Class parents don't add their own `<locals>`.
    """
    source = (
        "class Foo:\n"
        "    def bar(self):\n"
        "        class Local:\n"
        "            def meth(self):\n"
        "                pass\n"
    )
    tree = _parse(source)
    foo = tree.body[0]
    assert isinstance(foo, ast.ClassDef)
    bar = foo.body[0]
    assert isinstance(bar, ast.FunctionDef)
    local = bar.body[0]
    assert isinstance(local, ast.ClassDef)
    meth = local.body[0]
    assert isinstance(meth, ast.FunctionDef)
    assert reconstruct_qualname(meth, [tree, foo, bar, local]) == "Foo.bar.<locals>.Local.meth"


def test_overloaded_method_gets_regular_qualname() -> None:
    """UQ-WP3-07: @typing.overload methods are regular defs with decorators.

    The decorator does not change __qualname__; each overload and the final
    implementation all share ``Foo.bar``.
    """
    source = (
        "from typing import overload\n"
        "class Foo:\n"
        "    @overload\n"
        "    def bar(self, x: int) -> int: ...\n"
        "    @overload\n"
        "    def bar(self, x: str) -> str: ...\n"
        "    def bar(self, x):\n"
        "        pass\n"
    )
    tree = _parse(source)
    foo = tree.body[1]  # index 0 is the `from typing import overload`
    assert isinstance(foo, ast.ClassDef)
    for bar_def in foo.body:
        assert isinstance(bar_def, ast.FunctionDef)
        assert reconstruct_qualname(bar_def, [tree, foo]) == "Foo.bar"


def test_deeply_nested_function() -> None:
    source = "def a():\n    def b():\n        def c():\n            pass\n"
    tree = _parse(source)
    a = tree.body[0]
    assert isinstance(a, ast.FunctionDef)
    b = a.body[0]
    assert isinstance(b, ast.FunctionDef)
    c = b.body[0]
    assert isinstance(c, ast.FunctionDef)
    assert reconstruct_qualname(c, [tree, a, b]) == "a.<locals>.b.<locals>.c"
