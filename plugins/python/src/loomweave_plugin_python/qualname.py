"""L7 qualname reconstruction matching Python's ``__qualname__`` semantics.

Python's ``__qualname__`` is only bound at runtime, after the function or
class definition has been executed; Loomweave's static analyser has to
reconstruct the same string from the AST and the chain of parent scopes.

Rules (CPython language reference, "``__qualname__``"):

- Module-level function/class: qualname == name.
- Class-nested (class body contains a function/class): qualname prepends
  the enclosing class names joined by ``.`` with no separator marker.
- Function-nested (function body contains a function/class): qualname
  prepends ``parent.<locals>.`` — the ``<locals>`` marker distinguishes a
  closure from a method.

The L7 lock-in (``wp3-python-plugin.md §L7``) is that Loomweave reconstructs
the same bare Python ``__qualname__`` semantics that Wardline stores in its
``FingerprintEntry.qualified_name`` field. Loomweave entity names prepend the
dotted module path elsewhere; ADR-018 requires cross-product joins to translate
between those shapes instead of comparing strings directly.

``FunctionDef``, ``AsyncFunctionDef``, and ``ClassDef`` are all emitted
entities (class entities since WP3); every one of the three also acts as
a parent scope for qualname reconstruction.
"""

from __future__ import annotations

import ast

Scope = ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef


def reconstruct_qualname(node: Scope, parents: list[ast.AST]) -> str:
    """Return Python's ``__qualname__`` for ``node`` given its AST parent chain.

    ``parents`` is ordered from outermost (typically the ``ast.Module``) to
    the immediate parent. Non-scope ancestors (e.g. ``Module``,
    ``ast.If`` bodies, ``ast.With`` bodies) are skipped — they do not
    contribute to ``__qualname__``.
    """
    name = node.name
    for ancestor in reversed(parents):
        if isinstance(ancestor, (ast.FunctionDef, ast.AsyncFunctionDef)):
            name = f"{ancestor.name}.<locals>.{name}"
        elif isinstance(ancestor, ast.ClassDef):
            name = f"{ancestor.name}.{name}"
    return name
