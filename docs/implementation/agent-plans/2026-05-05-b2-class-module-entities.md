# B.2 — Python plugin: class + module entity emission — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the Python plugin (Sprint-1 function-only) to also emit `class` and `module` entities, with parse_status on module entities, ontology-version lockstep bump to 0.2.0.

**Architecture:** Per-kind builder functions (`_build_function_entity`, `_build_class_entity`, `_build_module_entity`) dispatched by `match` from `_walk`; module entity prepended in `extract()` before `_walk`; degraded module entity emitted on SyntaxError; top-level `__init__.py` skipped with stderr to avoid empty entity-ID. Wire shape gains one optional field on module entities (`parse_status`) carried through `RawEntity.extra` via serde-flatten.

**Tech Stack:** Python 3.11+, ast module, TypedDict + Literal + NotRequired (typing), pytest, mypy --strict, ruff. Rust-side: rusqlite + tempfile (tests already in place).

**Spec:** [`docs/implementation/sprint-2/b2-class-module-entities.md`](../../implementation/sprint-2/b2-class-module-entities.md). All Q1–Q4 panel-resolved decisions and §10 implementation-phase decisions are pulled through into the steps below.

**Filigree umbrella:** `clarion-daa9b13ce2` (P2 feature; sprint:2/wp:3/release:v0.1/tier:b).

---

## File map

Files this plan touches:

| File | Role | Tasks |
|---|---|---|
| `plugins/python/src/clarion_plugin_python/extractor.py` | Add TypedDict shapes; add `_build_module_entity`, `_build_class_entity`; rename `_build_entity` → `_build_function_entity`; switch `_walk` to `match` dispatch; emit module entity (prepended); skip top-level `__init__.py` | 1, 2 |
| `plugins/python/tests/test_extractor.py` | Add 11+ new tests for module / class / source-range / __init__.py-skip; rename two empty-file tests | 1, 2, 7 |
| `plugins/python/plugin.toml` | Add `class`, `module` to `entity_kinds`; bump `ontology_version` to 0.2.0 | 3 |
| `plugins/python/src/clarion_plugin_python/server.py` | Bump `ONTOLOGY_VERSION` constant to 0.2.0 | 3 |
| `plugins/python/src/clarion_plugin_python/__init__.py` | Bump package `__version__` to 0.1.1 (patch — different concept from ontology_version) | 3 |
| `fixtures/entity_id.json` | Add 5 new rows: 2 module + 3 class | 4 |
| `plugins/python/tests/test_round_trip.py` | Replace exact-total assertions with by-kind invariants | 5 |
| `tests/e2e/sprint_1_walking_skeleton.sh` | Update entity-count assertion: 1 → 2 (now includes module entity) | 6 |
| `docs/implementation/sprint-1/wp3-python-plugin.md` | Forward-pointer to this plan / B.2 design (already exists per commit `371af5a`; Task 8 verifies) | 8 |

---

## Task 1: TypedDict shapes + module entity emission + SyntaxError + top-level `__init__.py` skip

**Files:**
- Modify: `plugins/python/src/clarion_plugin_python/extractor.py`
- Modify: `plugins/python/tests/test_extractor.py`

This is the meatiest task. It introduces the TypedDict wire-shape, the `_module_source_range` helper, the `_build_module_entity` builder, the SyntaxError-degraded-emission branch, and the top-level `__init__.py` skip — all behind failing tests written first. The `_walk` loop is left function-only here; class emission lands in Task 2.

- [ ] **Step 1.1: Write failing test for `_module_source_range` helper (no trailing newline + CRLF)**

Append to `plugins/python/tests/test_extractor.py`:

```python
def test_module_source_range_no_trailing_newline() -> None:
    """File ending without `\\n` still produces correct end_line.

    `"a\\nb"` has one newline → end_line = 2.
    """
    from clarion_plugin_python.extractor import _module_source_range

    rng = _module_source_range("a\nb")
    assert rng == {"start_line": 1, "start_col": 0, "end_line": 2, "end_col": 0}


def test_module_source_range_crlf() -> None:
    """CRLF-terminated file produces same end_line as LF (count('\\n') handles both)."""
    from clarion_plugin_python.extractor import _module_source_range

    rng = _module_source_range("a\r\nb\r\n")
    # Two `\n`s → end_line = 3 (one past the last terminator).
    assert rng == {"start_line": 1, "start_col": 0, "end_line": 3, "end_col": 0}


def test_module_source_range_empty_string() -> None:
    """Empty source → end_line = 1 (count is 0; +1)."""
    from clarion_plugin_python.extractor import _module_source_range

    rng = _module_source_range("")
    assert rng == {"start_line": 1, "start_col": 0, "end_line": 1, "end_col": 0}
```

- [ ] **Step 1.2: Run tests to verify they fail**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_module_source_range_no_trailing_newline plugins/python/tests/test_extractor.py::test_module_source_range_crlf plugins/python/tests/test_extractor.py::test_module_source_range_empty_string -v`

Expected: FAIL with `ImportError: cannot import name '_module_source_range'`.

- [ ] **Step 1.3: Add TypedDict imports + `_module_source_range` helper to extractor.py**

In `plugins/python/src/clarion_plugin_python/extractor.py`, replace the imports block (lines 45-53) and append the new TypedDicts + helper. The replacement block:

```python
from __future__ import annotations

import ast
import sys
from pathlib import PurePosixPath
from typing import Any, Literal, NotRequired, TypedDict

from clarion_plugin_python.entity_id import entity_id
from clarion_plugin_python.qualname import reconstruct_qualname

_PLUGIN_ID = "python"


class SourceRange(TypedDict):
    start_line: int
    start_col: int
    end_line: int
    end_col: int


class EntitySource(TypedDict):
    file_path: str
    source_range: SourceRange


class RawEntity(TypedDict):
    """Wire shape matching the Rust host's RawEntity contract.

    `parse_status` is set on module entities only and rides through the
    host's `serde(flatten) extra` map. Class and function entities omit
    it; the field is `NotRequired` to keep mypy --strict happy.
    """

    id: str
    kind: str  # "function" | "class" | "module"; not narrowed to keep extension cheap.
    qualified_name: str
    source: EntitySource
    parse_status: NotRequired[Literal["ok", "syntax_error"]]


def _module_source_range(source: str) -> SourceRange:
    """Whole-file cover for module entities (Q4 resolution, B.2 §3 Q4).

    Uniform formula regardless of `parse_status`: `end_line = source.count('\\n') + 1`,
    `end_col = 0`. The `end_col = 0` value is a sentinel for module entities
    only — it means "end-of-file," NOT "column 0 of the last line." Class
    and function entities use real `ast.*.end_col_offset` data; consumers
    must not infer column semantics by analogy across kinds.
    """
    return {
        "start_line": 1,
        "start_col": 0,
        "end_line": source.count("\n") + 1,
        "end_col": 0,
    }
```

Note the removed `_KIND = "function"` constant — per Task 2's per-kind builder split, kinds become inline. Task 1 leaves the existing `_build_entity` referring to `"function"` directly inline (one-line edit). Apply that:

In `_build_entity` (was lines 129-153), replace `_KIND` references with the literal `"function"`:

```python
def _build_entity(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    parents: list[ast.AST],
    dotted_module: str,
    file_path: str,
) -> RawEntity:
    python_qualname = reconstruct_qualname(node, parents)
    qualified_name = f"{dotted_module}.{python_qualname}" if dotted_module else python_qualname
    end_line = node.end_lineno if node.end_lineno is not None else node.lineno
    end_col = node.end_col_offset if node.end_col_offset is not None else node.col_offset
    return {
        "id": entity_id(_PLUGIN_ID, "function", qualified_name),
        "kind": "function",
        "qualified_name": qualified_name,
        "source": {
            "file_path": file_path,
            "source_range": {
                "start_line": node.lineno,
                "start_col": node.col_offset,
                "end_line": end_line,
                "end_col": end_col,
            },
        },
    }
```

The return annotation tightens from `dict[str, Any]` to `RawEntity` (parse_status remains absent — that's fine because it's `NotRequired`).

Tighten `extract`'s return annotation from `list[dict[str, Any]]` to `list[RawEntity]` and `_walk`'s `out` parameter likewise. Replace `entities: list[dict[str, Any]] = []` with `entities: list[RawEntity] = []`.

- [ ] **Step 1.4: Run helper tests to verify pass**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_module_source_range_no_trailing_newline plugins/python/tests/test_extractor.py::test_module_source_range_crlf plugins/python/tests/test_extractor.py::test_module_source_range_empty_string -v`

Expected: PASS (3 passed).

- [ ] **Step 1.5: Run mypy --strict to verify TypedDict shapes**

Run: `plugins/python/.venv/bin/mypy --strict plugins/python`

Expected: PASS (no errors). If mypy complains about `_build_entity` returning `dict[...]` literal not matching `RawEntity`, the literal needs no change because TypedDict is structurally compatible — but mypy may want the inner `source_range` cast. If errors appear, it's a real type-mismatch; fix by tightening the return literal.

- [ ] **Step 1.6: Run all existing extractor tests to confirm no regression**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py -v`

Expected: all 13 existing tests PASS plus 3 new helper tests = 16 PASS.

- [ ] **Step 1.7: Write failing test for module entity always-emitted**

Append to `plugins/python/tests/test_extractor.py`:

```python
def test_module_entity_emitted_for_every_call() -> None:
    """Q1: every analyze produces exactly one module entity."""
    entities = extract("def hello():\n    pass\n", "demo.py")
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
    entities = extract("", "empty.py")
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
    entities = extract("", "pkg/__init__.py")
    assert len(entities) == 1
    module = entities[0]
    assert module["id"] == "python:module:pkg"
    assert module["qualified_name"] == "pkg"
```

- [ ] **Step 1.8: Run tests to verify they fail**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_module_entity_emitted_for_every_call plugins/python/tests/test_extractor.py::test_module_entity_for_empty_file plugins/python/tests/test_extractor.py::test_module_entity_for_init_py_collapses_to_package -v`

Expected: FAIL — current code returns `[]` for empty files and emits no module entity for the function case.

- [ ] **Step 1.9: Add `_build_module_entity` and prepend it in `extract()`**

Add to `extractor.py` before `_walk`:

```python
def _build_module_entity(
    source: str,
    dotted_module: str,
    file_path: str,
    parse_status: Literal["ok", "syntax_error"],
) -> RawEntity:
    """Build the per-file module entity (Q1 + Q4 resolutions)."""
    return {
        "id": entity_id(_PLUGIN_ID, "module", dotted_module),
        "kind": "module",
        "qualified_name": dotted_module,
        "source": {
            "file_path": file_path,
            "source_range": _module_source_range(source),
        },
        "parse_status": parse_status,
    }
```

Modify `extract()` to prepend the module entity *before* `_walk` runs, on the success path:

```python
def extract(
    source: str,
    file_path: str,
    *,
    module_prefix_path: str | None = None,
) -> list[RawEntity]:
    prefix_source = module_prefix_path if module_prefix_path is not None else file_path
    dotted_module = module_dotted_name(prefix_source)

    # Top-level __init__.py would resolve to "" — entity_id() rejects that
    # (crates/clarion-core/src/entity_id.rs:97-101). Skip with stderr.
    if not dotted_module:
        sys.stderr.write(
            f"clarion-plugin-python: skipping {file_path}: "
            f"top-level __init__.py has no package name\n",
        )
        return []

    try:
        tree = ast.parse(source)
    except SyntaxError as exc:
        sys.stderr.write(
            f"clarion-plugin-python: skipping {file_path}: syntax error at "
            f"line {exc.lineno}: {exc.msg}\n",
        )
        return [_build_module_entity(source, dotted_module, file_path, "syntax_error")]

    entities: list[RawEntity] = [_build_module_entity(source, dotted_module, file_path, "ok")]
    _walk(tree, [tree], dotted_module, file_path, entities)
    return entities
```

The `dotted_module` resolution moves *above* the `ast.parse` so the top-level `__init__.py` path is checked first; SyntaxError now emits a degraded module entity instead of `[]`. The existing UQ-WP3-02 stderr message is preserved verbatim.

- [ ] **Step 1.10: Run module-entity tests to verify pass**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_module_entity_emitted_for_every_call plugins/python/tests/test_extractor.py::test_module_entity_for_empty_file plugins/python/tests/test_extractor.py::test_module_entity_for_init_py_collapses_to_package -v`

Expected: PASS (3 passed).

- [ ] **Step 1.11: Write failing test for syntax-error file**

Append to `plugins/python/tests/test_extractor.py`:

```python
def test_module_entity_for_syntax_error_file(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Q1: syntax-error file emits one module entity with parse_status='syntax_error'."""
    entities = extract("def :", "broken.py")
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
```

Note: this requires importing pytest at the top of the test file. Update the existing TYPE_CHECKING import:

```python
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    pass
```

(Existing `if TYPE_CHECKING: import pytest` is removed since `pytest.CaptureFixture` is now needed at runtime in the new test even though the existing test only uses it as an annotation.)

Actually — re-check: the existing `test_syntax_error_yields_empty_list_and_logs_to_stderr` (line 62) uses `capsys: pytest.CaptureFixture[str]` as an annotation only and the file already has `import pytest` under `if TYPE_CHECKING`. Annotations are deferred (`from __future__ import annotations` is at the top of the file), so the existing TYPE_CHECKING-guarded import works for both the existing and the new test. **No import change needed.**

- [ ] **Step 1.12: Run test to verify it fails**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_module_entity_for_syntax_error_file -v`

Expected: FAIL — current `extract()` already has the syntax-error branch returning `[_build_module_entity(...)]`, so this should actually PASS at this point. If it fails, fix the SyntaxError branch in extract() per Step 1.9.

If the test passes here (likely because Step 1.9 already wired the syntax-error branch), proceed.

- [ ] **Step 1.13: Update existing `test_syntax_error_yields_empty_list_and_logs_to_stderr`**

The existing test (line 62-69) asserts `result == []` — this is now wrong. Replace it with a name that better describes the new behavior, OR delete it since `test_module_entity_for_syntax_error_file` covers the same ground.

Replace the existing test at line 62-69 with:

```python
def test_syntax_error_emits_degraded_module_entity_and_logs_to_stderr(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """UQ-WP3-02 + B.2 Q1: SyntaxError files now emit a degraded module entity (was: empty list)."""
    result = extract("def :", "broken.py")
    assert len(result) == 1
    assert result[0]["kind"] == "module"
    assert result[0].get("parse_status") == "syntax_error"
    captured = capsys.readouterr()
    assert "broken.py" in captured.err
```

- [ ] **Step 1.14: Run test to verify pass**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_syntax_error_emits_degraded_module_entity_and_logs_to_stderr -v`

Expected: PASS.

- [ ] **Step 1.15: Write failing test for top-level `__init__.py` skip**

Append:

```python
def test_top_level_init_py_skipped_with_stderr(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Top-level `__init__.py` (no package name) returns [] + one stderr line.

    `module_dotted_name("__init__.py")` returns "" (the empty stem case).
    Emitting an entity with empty qualified_name would crash the entity-ID
    assembler at crates/clarion-core/src/entity_id.rs:97-101.
    """
    entities = extract("def helper():\n    pass\n", "__init__.py")
    assert entities == []
    captured = capsys.readouterr()
    assert "__init__.py" in captured.err
    assert "top-level __init__.py has no package name" in captured.err
```

- [ ] **Step 1.16: Run test to verify pass**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_top_level_init_py_skipped_with_stderr -v`

Expected: PASS — Step 1.9 added the skip. If FAIL, verify the skip predicate runs *before* `ast.parse` and that the stderr message matches verbatim.

- [ ] **Step 1.17: Update the now-stale `test_empty_file_yields_zero_entities` and `test_whitespace_only_file_yields_zero_entities`**

These tests (lines 13-19) now contradict the new always-emit policy. Rename + update assertions:

Replace `test_empty_file_yields_zero_entities` with:

```python
def test_empty_file_yields_one_module_entity() -> None:
    """B.2 Q1 supersession of Sprint-1 UQ-WP3-11: empty file produces one module entity, not [].

    The function-extraction part of UQ-WP3-11 still holds: zero *function* entities.
    """
    entities = extract("", "empty.py")
    assert len(entities) == 1
    assert entities[0]["kind"] == "module"
    assert entities[0].get("parse_status") == "ok"
    function_entities = [e for e in entities if e["kind"] == "function"]
    assert function_entities == []
```

Replace `test_whitespace_only_file_yields_zero_entities` with:

```python
def test_whitespace_only_file_yields_one_module_entity() -> None:
    """Whitespace + comment-only file → one module entity (parse_status='ok'), zero functions."""
    entities = extract("\n\n# just a comment\n", "empty.py")
    assert len(entities) == 1
    assert entities[0]["kind"] == "module"
    assert entities[0].get("parse_status") == "ok"
```

- [ ] **Step 1.18: Run all extractor tests**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py -v`

Expected: all PASS. The `test_module_level_function`, `test_class_method`, `test_nested_function_emits_both_outer_and_inner`, `test_async_function`, `test_nested_class_method`, `test_src_prefix_stripped`, `test_init_py_collapsed_to_package_name`, `test_module_prefix_path_decouples_file_path_and_dotted_prefix` tests will now each return one *additional* entity (the module entity). They may need updates: review each `assert len(entities) == 1` and change to `assert len([e for e in entities if e["kind"] == "function"]) == 1` *if* the test asserted exact count. Specifically check:

  - `test_module_level_function` (line 22) — `assert len(entities) == 1` → use kind-filtered count.
  - `test_class_method` (line 34) — `assert len(entities) == 1` → use kind-filtered count.
  - `test_nested_function_emits_both_outer_and_inner` (line 40) — uses `ids = {e["id"] for e in entities}` then asserts an exact set; module ID must be added to expected set OR filter to function entities only.
  - `test_async_function` (line 49) — `assert len(entities) == 1` → use kind-filtered count.
  - `test_nested_class_method` (line 55) — `assert len(entities) == 1` → use kind-filtered count.
  - `test_init_py_collapsed_to_package_name` (line 78) — asserts `entities[0]` directly — must skip the module entity OR filter.
  - `test_source_range_end_fields_populated` (line 109) — same — `entities[0]` may now be the module entity (prepended), so assertion fails.

Update each by filtering `function_entities = [e for e in entities if e["kind"] == "function"]` and asserting on that subset where the test was about function-extraction semantics. This propagates the spec change into the existing test bodies.

For `test_nested_function_emits_both_outer_and_inner`, the cleanest update is:

```python
def test_nested_function_emits_both_outer_and_inner() -> None:
    entities = extract("def outer():\n    def inner():\n        pass\n", "demo.py")
    function_ids = {e["id"] for e in entities if e["kind"] == "function"}
    assert function_ids == {
        "python:function:demo.outer",
        "python:function:demo.outer.<locals>.inner",
    }
```

For each `entities[0]` access, change to `next(e for e in entities if e["kind"] == "function")`.

- [ ] **Step 1.19: Run all extractor tests after the function-test filter updates**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py -v`

Expected: all PASS.

- [ ] **Step 1.20: Run mypy --strict + ruff**

Run:
```bash
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
```

Expected: all PASS. If ruff format complains, run `ruff format plugins/python` to auto-fix and re-run check.

- [ ] **Step 1.21: Commit**

```bash
git add plugins/python/src/clarion_plugin_python/extractor.py plugins/python/tests/test_extractor.py
git commit -m "$(cat <<'EOF'
feat(wp3): module entity emission with parse_status (B.2 Q1)

Per B.2 design (docs/implementation/sprint-2/b2-class-module-entities.md
§3 Q1, §3 Q4), every analyzed .py file now produces exactly one module
entity with whole-file source_range and a parse_status property. Empty
and syntax-error files no longer return []; they emit a degraded module
entity. Top-level __init__.py is skipped with stderr (the entity-ID
assembler rejects empty qualified_name).

Adds TypedDict wire shapes (RawEntity, EntitySource, SourceRange) so
mypy --strict catches kind/source mismatches at build time.

Q1 supersedes the function-only part of Sprint-1 UQ-WP3-11; zero
*function* entities for empty files still holds.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Per-kind builder split + `_build_class_entity`

**Files:**
- Modify: `plugins/python/src/clarion_plugin_python/extractor.py`
- Modify: `plugins/python/tests/test_extractor.py`

Refactor the dispatcher and add class entity emission. After this task, all three kinds (function, class, module) ship.

- [ ] **Step 2.1: Write failing test for simple class entity**

Append to `test_extractor.py`:

```python
def test_class_entity_simple() -> None:
    """`class Foo: pass` → one class entity + one module entity."""
    entities = extract("class Foo:\n    pass\n", "demo.py")
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
```

- [ ] **Step 2.2: Run to verify fail**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_class_entity_simple -v`

Expected: FAIL — `_walk` does not currently emit class entities.

- [ ] **Step 2.3: Rename `_build_entity` → `_build_function_entity`; add `_build_class_entity`; switch `_walk` to `match`**

In `extractor.py`:

1. Rename `_build_entity` to `_build_function_entity` (signature, return type, and contents unchanged from Step 1.3).
2. Add `_build_class_entity` next to it:

```python
def _build_class_entity(
    node: ast.ClassDef,
    parents: list[ast.AST],
    dotted_module: str,
    file_path: str,
) -> RawEntity:
    """Build a class entity. Uses real ast.end_lineno/end_col_offset (not the module sentinel).

    Class methods continue to emit as `function` entities (per detailed-design.md:67);
    no separate `method` kind. Nested classes nest in the qualname per
    `reconstruct_qualname` (no `<locals>` between class names).
    """
    python_qualname = reconstruct_qualname(node, parents)
    qualified_name = f"{dotted_module}.{python_qualname}" if dotted_module else python_qualname
    end_line = node.end_lineno if node.end_lineno is not None else node.lineno
    end_col = node.end_col_offset if node.end_col_offset is not None else node.col_offset
    return {
        "id": entity_id(_PLUGIN_ID, "class", qualified_name),
        "kind": "class",
        "qualified_name": qualified_name,
        "source": {
            "file_path": file_path,
            "source_range": {
                "start_line": node.lineno,
                "start_col": node.col_offset,
                "end_line": end_line,
                "end_col": end_col,
            },
        },
    }
```

3. Replace `_walk`'s body with a `match` dispatch:

```python
def _walk(
    node: ast.AST,
    parents: list[ast.AST],
    dotted_module: str,
    file_path: str,
    out: list[RawEntity],
) -> None:
    for child in ast.iter_child_nodes(node):
        match child:
            case ast.FunctionDef() | ast.AsyncFunctionDef():
                out.append(_build_function_entity(child, parents, dotted_module, file_path))
            case ast.ClassDef():
                out.append(_build_class_entity(child, parents, dotted_module, file_path))
        _walk(child, [*parents, child], dotted_module, file_path, out)
```

The `match` ignores other child types — same behavior as the old `isinstance` chain. Classes-in-functions and classes-in-classes both descend correctly because the recursion happens unconditionally after the match.

- [ ] **Step 2.4: Run simple-class test to verify pass**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py::test_class_entity_simple -v`

Expected: PASS.

- [ ] **Step 2.5: Write failing tests for nested class + class-in-function + async + class source-range**

Append to `test_extractor.py`:

```python
def test_class_entity_nested() -> None:
    """`class A: class B: pass` → two class entities (A, A.B) + one module entity."""
    entities = extract("class A:\n    class B:\n        pass\n", "demo.py")
    class_ids = {e["id"] for e in entities if e["kind"] == "class"}
    assert class_ids == {
        "python:class:demo.A",
        "python:class:demo.A.B",
    }


def test_class_in_function_qualname() -> None:
    """`def f(): class C: pass` → class entity at f.<locals>.C (function-parent gets <locals>)."""
    entities = extract("def f():\n    class C:\n        pass\n", "demo.py")
    class_ids = {e["id"] for e in entities if e["kind"] == "class"}
    function_ids = {e["id"] for e in entities if e["kind"] == "function"}
    assert class_ids == {"python:class:demo.f.<locals>.C"}
    assert function_ids == {"python:function:demo.f"}


def test_class_method_emitted_as_function() -> None:
    """Class methods continue as function-kind (no separate method kind)."""
    entities = extract(
        "class Foo:\n    def bar(self):\n        pass\n",
        "demo.py",
    )
    class_ids = {e["id"] for e in entities if e["kind"] == "class"}
    function_ids = {e["id"] for e in entities if e["kind"] == "function"}
    assert class_ids == {"python:class:demo.Foo"}
    assert function_ids == {"python:function:demo.Foo.bar"}


def test_async_class_method() -> None:
    """`async def` inside a class still emits as function-kind."""
    entities = extract(
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
    entities = extract("class A:\n    pass\n", "demo.py")
    cls = next(e for e in entities if e["kind"] == "class")
    sr = cls["source"]["source_range"]
    # Class body extends past the header line.
    assert sr["end_line"] == 2
    # Real column data, not the module sentinel 0.
    assert sr["end_col"] > 0
```

- [ ] **Step 2.6: Run new class tests to verify pass**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py -v -k class_`

Expected: all class tests PASS. The `match` dispatch + `_build_class_entity` plus existing `reconstruct_qualname` (which already handles nested classes per `qualname.py:42-46`) cover the cases.

- [ ] **Step 2.7: Run all extractor tests**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py -v`

Expected: all PASS.

- [ ] **Step 2.8: Run mypy + ruff**

Run:
```bash
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
```

Expected: all PASS.

- [ ] **Step 2.9: Commit**

```bash
git add plugins/python/src/clarion_plugin_python/extractor.py plugins/python/tests/test_extractor.py
git commit -m "$(cat <<'EOF'
feat(wp3): class entity emission + per-kind builders (B.2 Q3)

Per B.2 design §3 Q3, splits `_build_entity` into `_build_function_entity`
and `_build_class_entity` and switches `_walk` to a `match` dispatch.
Class methods continue as function-kind (no separate method kind).
Nested classes and classes-in-functions reuse `reconstruct_qualname`,
producing the same L7 strings Wardline annotations would.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Manifest + ONTOLOGY_VERSION + package __version__ lockstep bump

**Files:**
- Modify: `plugins/python/plugin.toml`
- Modify: `plugins/python/src/clarion_plugin_python/server.py`
- Modify: `plugins/python/src/clarion_plugin_python/__init__.py`

§10 resolution: package `__version__` → `0.1.1` (patch); ontology_version → `0.2.0` (matches the kind-set expansion). The two are different concepts.

- [ ] **Step 3.1: Update `plugin.toml`**

In `plugins/python/plugin.toml`, replace the `[ontology]` block (lines 27-37):

Old:
```toml
[ontology]
# Sprint 1 narrow scope: functions only (WP3 §1). Classes, modules,
# decorators are WP3-feature-complete kinds.
entity_kinds = ["function"]
edge_kinds = []
# Per ADR-022: uppercase `CLA-{PLUGIN_ID_UPPER}-`. Reserved at parse
# against the CLA-INFRA-* and CLA-FACT-* namespaces.
rule_id_prefix = "CLA-PY-"
# Feeds ADR-007 cache keying; bump when the entity/edge/rule set shifts.
ontology_version = "0.1.0"
```

New:
```toml
[ontology]
# Sprint 2 B.2: classes + modules join the kind set. Decorators, edges,
# imports, calls remain WP3-feature-complete (later Sprint 2+).
entity_kinds = ["function", "class", "module"]
edge_kinds = []
# Per ADR-022: uppercase `CLA-{PLUGIN_ID_UPPER}-`. Reserved at parse
# against the CLA-INFRA-* and CLA-FACT-* namespaces.
rule_id_prefix = "CLA-PY-"
# Bumps when the entity/edge/rule set shifts. NOTE: ADR-007's summary-cache
# key is the 5-tuple (entity_id, content_hash, prompt_template_id,
# model_tier, guidance_fingerprint) — ontology_version is handshake-validation,
# NOT a cache-key component. New module/class entity_ids miss the cache by
# component-1 of the 5-tuple organically.
ontology_version = "0.2.0"
```

- [ ] **Step 3.2: Update `server.py` constant**

In `plugins/python/src/clarion_plugin_python/server.py:35`:

Old: `ONTOLOGY_VERSION = "0.1.0"`

New: `ONTOLOGY_VERSION = "0.2.0"`

- [ ] **Step 3.3: Update package `__version__`**

In `plugins/python/src/clarion_plugin_python/__init__.py`:

Old: `__version__ = "0.1.0"`

New: `__version__ = "0.1.1"`

- [ ] **Step 3.4: Run handshake test**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_server.py -v -k initialize`

Expected: any test that asserts `ONTOLOGY_VERSION` reads it as `"0.2.0"` and PASSES (the existing test asserts the constant, not a literal). If a test asserts the literal `"0.1.0"`, update it — but verify by inspection first.

If `test_server.py` doesn't exist, run the full plugin test suite instead:

```bash
plugins/python/.venv/bin/pytest plugins/python -v
```

Expected: all PASS.

- [ ] **Step 3.5: Run mypy + ruff**

Run:
```bash
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
```

Expected: all PASS.

- [ ] **Step 3.6: Commit**

```bash
git add plugins/python/plugin.toml plugins/python/src/clarion_plugin_python/server.py plugins/python/src/clarion_plugin_python/__init__.py
git commit -m "$(cat <<'EOF'
feat(wp3): ontology v0.2.0 — entity_kinds += class, module (B.2)

plugin.toml::[ontology].entity_kinds gains "class" and "module";
ontology_version bumps 0.1.0 → 0.2.0 in lockstep with
server.py::ONTOLOGY_VERSION. Package __version__ patches 0.1.0 → 0.1.1
(no breaking API change — the wire-shape addition is non-breaking via
serde-flatten).

Lint guard for drift between plugin.toml and server.py is filed as
follow-up clarion-8befae708b (P3 task; not blocking B.2).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Cross-language fixture parity (`fixtures/entity_id.json`)

**Files:**
- Modify: `fixtures/entity_id.json`

The fixture already has one `python:module:pkg.submodule` row (line 79) labeled "future Python plugin kind". Sprint-2 B.2 makes module + class real; expand to cover the cases the design doc §6 listed.

- [ ] **Step 4.1: Inspect current fixture and update the "future" comment**

Read `fixtures/entity_id.json`. The existing module entry is:

```json
{
  "description": "module entity (future Python plugin kind)",
  "plugin_id": "python",
  "kind": "module",
  "canonical_qualified_name": "pkg.submodule",
  "expected_entity_id": "python:module:pkg.submodule"
}
```

Update its description from `"future Python plugin kind"` → `"module entity (B.2)"`.

- [ ] **Step 4.2: Add 4 new rows after the existing module row**

Add (insert after the existing module row, before the `core:file` rows):

```json
{
  "description": "module entity: __init__.py collapse case (B.2)",
  "plugin_id": "python",
  "kind": "module",
  "canonical_qualified_name": "pkg",
  "expected_entity_id": "python:module:pkg"
},
{
  "description": "class entity: simple module-level (B.2)",
  "plugin_id": "python",
  "kind": "class",
  "canonical_qualified_name": "pkg.mod.Foo",
  "expected_entity_id": "python:class:pkg.mod.Foo"
},
{
  "description": "class entity: nested class chains names with no locals separator (B.2)",
  "plugin_id": "python",
  "kind": "class",
  "canonical_qualified_name": "pkg.mod.Foo.Bar",
  "expected_entity_id": "python:class:pkg.mod.Foo.Bar"
},
{
  "description": "class entity: class-in-function gets <locals> at function boundary (B.2)",
  "plugin_id": "python",
  "kind": "class",
  "canonical_qualified_name": "pkg.mod.f.<locals>.C",
  "expected_entity_id": "python:class:pkg.mod.f.<locals>.C"
}
```

Mind JSON commas — the existing module row currently ends with a comma before the next row; preserve syntactic validity.

- [ ] **Step 4.3: Validate JSON syntax**

Run: `python3 -c 'import json; json.load(open("/home/john/clarion/fixtures/entity_id.json"))'`

Expected: no output (valid JSON).

- [ ] **Step 4.4: Run Rust + Python parity tests**

Run:
```bash
cargo nextest run -p clarion-core entity_id::tests::shared_fixture_byte_for_byte_parity
plugins/python/.venv/bin/pytest plugins/python/tests/test_entity_id.py::test_matches_shared_fixture -v
```

Expected: both PASS.

- [ ] **Step 4.5: Commit**

```bash
git add fixtures/entity_id.json
git commit -m "$(cat <<'EOF'
test(wp3): cross-language fixture parity for class + module entities (B.2)

Adds 4 new rows to fixtures/entity_id.json: __init__.py module collapse,
simple class, nested class, class-in-function. The existing
`pkg.submodule` module row's "future" annotation is updated since B.2
makes module a real Python-plugin kind.

Both crates/clarion-core/src/entity_id.rs::tests::shared_fixture_byte_for_byte_parity
and plugins/python/tests/test_entity_id.py::test_matches_shared_fixture
consume this file; divergence fails CI on both sides.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Round-trip self-test by-kind invariants

**Files:**
- Modify: `plugins/python/tests/test_round_trip.py`

The current test (lines 124-129) asserts `entity["kind"] == "function"` for every entity. Under B.2, the plugin's own `extractor.py` will analyse to a module entity + several function entities — that loop must be replaced with by-kind invariants.

- [ ] **Step 5.1: Replace exact-kind assertion with by-kind invariants**

In `plugins/python/tests/test_round_trip.py`, replace lines 117-129 (the `entities = response[...]` block through the per-entity `assert entity["kind"] == "function"` loop):

Old (lines ~115-129):
```python
        entities = response["result"]["entities"]
        ids = {e["id"] for e in entities}
        # Public extractor API must be present.
        assert "python:function:clarion_plugin_python.extractor.module_dotted_name" in ids
        assert "python:function:clarion_plugin_python.extractor.extract" in ids
        # Private walker is a FunctionDef too, so it emits.
        assert "python:function:clarion_plugin_python.extractor._walk" in ids
        assert "python:function:clarion_plugin_python.extractor._build_entity" in ids

        # Every entity should carry kind="function" and the absolute
        # source.file_path we sent (project_root relativisation only affects
        # the qualified_name prefix, not source.file_path).
        for entity in entities:
            assert entity["kind"] == "function"
            assert entity["source"]["file_path"] == str(target)
```

New:
```python
        entities = response["result"]["entities"]
        function_entities = [e for e in entities if e["kind"] == "function"]
        module_entities = [e for e in entities if e["kind"] == "module"]
        class_entities = [e for e in entities if e["kind"] == "class"]
        function_ids = {e["id"] for e in function_entities}

        # Invariants — no exact totals (those become merge-conflict generators
        # the moment someone adds a private helper to extractor.py).
        assert len(module_entities) == 1, "exactly one module entity per analyzed file"
        assert module_entities[0]["id"] == "python:module:clarion_plugin_python.extractor"
        assert module_entities[0].get("parse_status") == "ok"

        # Public extractor API must be present.
        assert "python:function:clarion_plugin_python.extractor.module_dotted_name" in function_ids
        assert "python:function:clarion_plugin_python.extractor.extract" in function_ids
        # Private walker is a FunctionDef too, so it emits.
        assert "python:function:clarion_plugin_python.extractor._walk" in function_ids
        # B.2 renamed `_build_entity` → `_build_function_entity` and added
        # `_build_class_entity` + `_build_module_entity` (and `_module_source_range`).
        assert "python:function:clarion_plugin_python.extractor._build_function_entity" in function_ids
        assert "python:function:clarion_plugin_python.extractor._build_class_entity" in function_ids
        assert "python:function:clarion_plugin_python.extractor._build_module_entity" in function_ids

        # Extractor has no top-level classes (module-level functions only),
        # so class_entities should be empty for this specific target.
        assert class_entities == []

        # Every entity carries the absolute source.file_path we sent
        # (project_root relativisation only affects the qualified_name prefix).
        for entity in entities:
            assert entity["source"]["file_path"] == str(target)
```

- [ ] **Step 5.2: Reinstall plugin so the binary picks up Task 1+2 changes**

The round-trip test invokes the *installed* `clarion-plugin-python` binary. Editable install (`pip install -e`) makes the *source* changes take effect immediately, but verify:

Run: `plugins/python/.venv/bin/pip install -e plugins/python[dev]` (idempotent reinstall — no-op if up to date).

- [ ] **Step 5.3: Run round-trip test**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_round_trip.py -v`

Expected: PASS.

If FAIL with "binary not found," check `which clarion-plugin-python` from the venv. If FAIL with mismatched IDs, the rename in Task 2 (`_build_entity` → `_build_function_entity`) wasn't applied or didn't reach the installed binary; reinstall and re-run.

- [ ] **Step 5.4: Commit**

```bash
git add plugins/python/tests/test_round_trip.py
git commit -m "$(cat <<'EOF'
test(wp3): round-trip by-kind invariants (B.2)

Replaces the per-entity `kind == "function"` assertion with per-kind
filters and adds invariants for the new module entity and the renamed
private helpers (`_build_function_entity`, `_build_class_entity`,
`_build_module_entity`). Exact total counts are deliberately NOT
asserted — they generate merge conflicts every time someone adds a
private helper to extractor.py, with no analytic value.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Walking-skeleton e2e expects 2 entities (module + function)

**Files:**
- Modify: `tests/e2e/sprint_1_walking_skeleton.sh`

The script's `EXPECTED` assertion (line 73) expects exactly `python:function:demo.hello|function`. Under B.2 the demo file produces 2 rows; update.

- [ ] **Step 6.1: Update the EXPECTED assertion**

In `tests/e2e/sprint_1_walking_skeleton.sh`, replace lines 71-78 (the `RESULT=$(...)` block and the `if`):

Old:
```bash
RESULT=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select id, kind from entities order by id;")
EXPECTED="python:function:demo.hello|function"

if [ "$RESULT" != "$EXPECTED" ]; then
    log "DB contents:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select * from entities;" >&2 || true
    fail "expected exactly '$EXPECTED', got '$RESULT'"
fi

log "PASS: walking skeleton persisted $RESULT"
```

New:
```bash
RESULT=$(sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select id, kind from entities order by id;")
# B.2 (Sprint 2): every analyzed file emits a module entity in addition to
# its function/class entities. The demo file `def hello(): return "world"`
# produces exactly two rows.
EXPECTED="python:function:demo.hello|function
python:module:demo|module"

if [ "$RESULT" != "$EXPECTED" ]; then
    log "DB contents:"
    sqlite3 "$DEMO_DIR/.clarion/clarion.db" "select * from entities;" >&2 || true
    fail "expected exactly:\n$EXPECTED\ngot:\n$RESULT"
fi

log "PASS: walking skeleton persisted module + function entities"
```

The newline inside `EXPECTED` matches sqlite3's default output format (one row per line, alphabetic order: `python:function:demo.hello` < `python:module:demo`).

- [ ] **Step 6.2: Build a fresh clarion binary so the script picks up nothing of Rust changes (none in this plan, but the venv has the new Python plugin)**

The script does `cargo build --workspace --release` itself; ensure the editable plugin install in the venv reflects Task 1+2:

Run: `plugins/python/.venv/bin/pip install -e plugins/python[dev]` (sanity check; should be no-op).

- [ ] **Step 6.3: Run the e2e script**

Run: `bash tests/e2e/sprint_1_walking_skeleton.sh`

Expected: ends with `[walking-skeleton] PASS: walking skeleton persisted module + function entities`.

If FAIL with "got: python:function:demo.hello|function" only (no module entity), the venv has a stale install — `pip install -e plugins/python[dev]` and re-run.

- [ ] **Step 6.4: Commit**

```bash
git add tests/e2e/sprint_1_walking_skeleton.sh
git commit -m "$(cat <<'EOF'
test(wp3): walking skeleton expects module + function entities (B.2)

Per B.2 design §6, the demo file now produces 2 entities (module + function).
EXPECTED is the literal sqlite3 output of `select id, kind from entities
order by id;` — alphabetic ordering puts function before module.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Existing-test renames absorbed elsewhere

**Files:** none (all renames already happened in Task 1).

The B.2 design §7 Task 7 lists the empty-file test renames separately for organisational clarity, but Task 1's Step 1.17 absorbed them (it's logically the same file edit cycle). Verify:

- [ ] **Step 7.1: Confirm renames landed**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_extractor.py -v -k "empty_file or whitespace"`

Expected: tests `test_empty_file_yields_one_module_entity` and `test_whitespace_only_file_yields_one_module_entity` PASS; the old `*_yields_zero_entities` names are GONE (no test collected under those names).

- [ ] **Step 7.2: No commit needed**

Skip — the work is already in Task 1's commit.

---

## Task 8: Documentation lock + close (full ADR-023 gate sweep)

**Files:**
- Verify: `docs/implementation/sprint-1/wp3-python-plugin.md` (forward-pointer should already exist from commit `371af5a`).
- Run: ADR-023 gate suite end-to-end.
- Close: filigree umbrella `clarion-daa9b13ce2`.

- [ ] **Step 8.1: Verify forward-pointer exists in the Sprint-1 WP3 doc**

Run: `grep -n "B.2" /home/john/clarion/docs/implementation/sprint-1/wp3-python-plugin.md | head -5`

Expected: at least one match referencing `b2-class-module-entities.md` or "Sprint 2 / B.2". If none, append the forward-pointer near §1 "out of scope":

```markdown
> **Sprint 2 update (2026-04-30):** B.2 realises the deferred class + module entity kinds — see [`docs/implementation/sprint-2/b2-class-module-entities.md`](../sprint-2/b2-class-module-entities.md). Decorators and the remaining kinds in §1's deferred list stay deferred to later WP3-feature-complete sprints.
```

(Already added in commit `371af5a` per session history; the verification is just a sanity step.)

- [ ] **Step 8.2: Run all Rust gates**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

Expected: all PASS. If clippy fails on a new warning introduced by the toolchain since Sprint 1, fix at the call site (don't add `#[allow]`).

- [ ] **Step 8.3: Run all Python gates**

Run:
```bash
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
```

Expected: all PASS.

- [ ] **Step 8.4: Run e2e walking-skeleton**

Run: `bash tests/e2e/sprint_1_walking_skeleton.sh`

Expected: PASS.

- [ ] **Step 8.5: Push branch**

Run: `git push`

Expected: pushes `sprint-2/b2-design` (or whichever branch the work landed on) to origin.

- [ ] **Step 8.6: Open PR**

Run:
```bash
gh pr create --base main --head sprint-2/b2-design --title "feat(wp3): B.2 — Python plugin emits class + module entities" --body "$(cat <<'EOF'
## Summary

Sprint 2 / Tier B / B.2: Python plugin extends from function-only emission to also emit `class` and `module` entities.

- Module entity prepended on every analyze (parse_status `"ok"` | `"syntax_error"`); top-level `__init__.py` skipped with stderr.
- Per-kind builders (`_build_function_entity`, `_build_class_entity`, `_build_module_entity`) dispatched by `match` from `_walk`.
- Wire shape gains one optional field via serde-flatten — non-breaking host change.
- `plugin.toml::ontology_version` and `server.py::ONTOLOGY_VERSION` move in lockstep `0.1.0` → `0.2.0`. Package `__version__` patches `0.1.0` → `0.1.1`.
- Cross-language fixture (`fixtures/entity_id.json`) gains 4 rows (1 module collapse case + 3 class cases).
- Walking-skeleton e2e now expects 2 entities (module + function).

Spec: `docs/implementation/sprint-2/b2-class-module-entities.md`. All Q1–Q4 panel-resolved decisions and §10 implementation-phase decisions implemented as designed.

Filigree umbrella: clarion-daa9b13ce2 (P2 feature, sprint:2/wp:3/release:v0.1/tier:b).

Lint guard for ontology_version drift between plugin.toml and server.py is filed as follow-up clarion-8befae708b (P3, not blocking).

## Test plan

- [ ] All ADR-023 gates green (fmt, clippy, build, nextest, doc, deny; ruff, ruff-format, mypy --strict, pytest).
- [ ] Cross-language fixture parity passes on Rust + Python sides.
- [ ] Walking-skeleton e2e (`bash tests/e2e/sprint_1_walking_skeleton.sh`) PASSES.
- [ ] Round-trip self-test PASSES with by-kind invariants.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 8.7: Close filigree umbrella**

After PR merges:

Run: `filigree update clarion-daa9b13ce2 --status=delivered` (or whichever transition is valid for the feature type — check with `filigree transitions clarion-daa9b13ce2`).

Then close: `filigree close clarion-daa9b13ce2 --reason="B.2 shipped in PR #N"`.

---

## Self-review

**Spec coverage** — every requirement in `docs/implementation/sprint-2/b2-class-module-entities.md` is implemented somewhere:

- §1 Scope (class + module entity emission) — Tasks 1, 2 ✓
- §2 Locked surfaces (no L4/L5/L7/L8 changes) — preserved across all tasks ✓
- §3 Q1 (always-emit module + parse_status + top-level __init__.py skip) — Task 1 ✓
- §3 Q2 (no parent_id on wire) — implicit in the wire-shape definition (Task 1's TypedDict has no parent_id field) ✓
- §3 Q3 (per-kind builders + match dispatch) — Task 2 ✓
- §3 Q4 (whole-file source_range with end_col=0 sentinel) — Task 1 (Step 1.3 + 1.9) ✓
- §4 Wire shape additions (RawEntity/EntitySource/SourceRange TypedDicts; parse_status NotRequired) — Task 1 Step 1.3 ✓
- §5 Manifest + version bump lockstep — Task 3 ✓
- §6 Test plan (unit + cross-language fixture + e2e + round-trip) — Tasks 1, 2, 4, 5, 6 ✓
- §7 Implementation task ledger — direct mapping to Tasks 1–8 ✓
- §8 Filigree umbrella tracking — Task 8 Step 8.7 ✓
- §9 Exit criteria — Task 8 (full ADR-023 sweep) ✓
- §10 Implementation-phase decisions (__version__ → 0.1.1, stderr wording, lint guard deferred) — Task 3 + Task 1 Step 1.9 + filigree clarion-8befae708b reference ✓
- §11 Panel-review record — citation only; no implementation change ✓

**Type consistency** — `RawEntity`/`EntitySource`/`SourceRange` defined once in Task 1 Step 1.3; referenced consistently in Task 2 (`_build_class_entity` returns `RawEntity`). `_build_function_entity` rename used consistently across Tasks 2 + 5. `parse_status` literal values `"ok"` and `"syntax_error"` used consistently across Tasks 1 (helper, build, tests), 5 (round-trip), and the spec.

**Placeholder scan** — no TODO, TBD, "fill in later," "similar to Task N," or "implement appropriately" patterns. Every step that changes code shows the exact code.

---

## Open follow-ups (post-B.2)

- `clarion-8befae708b` (P3 task) — CI lint guard for plugin.toml ↔ server.py ontology_version drift.
- B.3 will resolve the ADR-022:52 ambiguity on `parent_id` derivation (plugin emits vs. core derives from `contains` edges).
- B.3 fixture guardrail (B.2 §3 Q2): `parent_id ≠ qualified_name.rsplit('.', 1)[0]` for at least one fixture case (decorator-emitted or conditional-def case).
