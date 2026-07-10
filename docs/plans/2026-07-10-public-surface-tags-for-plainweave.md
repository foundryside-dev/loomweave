# Public Surface Tags for Plainweave Coverage Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Do not claim or close tracker issues unless the user explicitly re-authorizes it.

**Goal:** Make Loomweave's persisted public-surface catalog accurate enough for Plainweave's coverage denominator by exposing and tag-classifying at least `entry-point`, `http-route`, `exported-api`, and `cli-command`, while preserving the attribution that Plainweave is honestly reporting Loomweave's catalog completeness.

**Architecture:** Keep the existing tag architecture: language plugins emit open `RawEntity.tags`, `loomweave-cli` normalizes them, `loomweave-storage` persists them in `entity_tags`, and consumers such as Plainweave read the SQLite catalog. The implementation adds the missing MCP shortcut exposure for `exported-api` and `cli-command`, tightens Python extraction for explicit module exports and manual/argparse CLI entry points, leaves the existing Rust tag derivation intact, and validates Loomweave capability against synthetic and live evidence corpora. Do not turn these into entity kinds; the current `entity_tags` model is the right denominator contract.

**Tech Stack:** Rust workspace (`cargo fmt`, `cargo clippy`, `cargo nextest`, MCP stdio E2E), Python plugin (`ast`, pyright-backed extractor, `pytest`, ruff, mypy), SQLite `entity_tags`, Plainweave CLI `plainweave intent coverage`, Wardline gate.

**Prerequisites:**
- Start from `/home/john/loomweave` on a feature branch. The planning pass observed an unrelated pre-existing dirty file: `packaging/rust-plugin-dist/Cargo.lock`. Preserve it unless the owner explicitly asks otherwise.
- `plugins/python/.venv` exists and has the plugin dev dependencies. If missing, run `uv sync --project plugins/python --locked --extra dev`.
- For targeted Python red/green test loops, pass `--no-cov`. `plugins/python/pyproject.toml` enables coverage globally, so selected-test runs are otherwise a bad oracle. The full Python suite in the final gate still runs with coverage.
- After the final Python ontology bump, force-refresh the editable Python plugin install before any cross-repo validation. Plugin discovery reads the installed shared-data `plugin.toml`, so editing the source manifest is not enough if `plugins/python/.venv/share/loomweave/plugins/python/plugin.toml` is stale.
- `sqlite3`, `plainweave`, and `wardline` are available on `PATH`.
- The validation repos exist: `/home/john/scrappack` and `/home/john/scrappack-engine-phase-tasks-1-2`.
- Do not mutate `/home/john/scrappack/.plainweave/plainweave.db` without explicit owner approval; it was dirty before this plan.
- Validation repos are evidence corpora, not product requirements. A corpus may legitimately lack HTTP routes, explicit `__all__` exports, or CLI commands; absence in one corpus is source reality, not a Loomweave acceptance failure. The acceptance target is Loomweave's ability to emit, persist, and expose the four tag classes when source evidence exists.
- Current baseline from read-only inspection:
  - `/home/john/scrappack/.weft/loomweave/loomweave.db`: `entry-point=4`, `public-surface=100`, `cli-command=0`, `exported-api=0`, `http-route=0`.
  - `/home/john/scrappack-engine-phase-tasks-1-2/.weft/loomweave/loomweave.db`: `entry-point=6`, `http-route=13`, `framework-handler=13`, `public-surface=253`, `cli-command=0`, `exported-api=0`.
  - `/home/john/scrappack-engine-phase-tasks-1-2` is not a Plainweave project today; `plainweave intent coverage --json` returns `NOT_FOUND` for `.plainweave/plainweave.db`. Its validation must use Loomweave SQLite unless the owner initializes Plainweave there.

---

## Ground Truth From the Live Repo

- Python tag emission lives in `plugins/python/src/loomweave_plugin_python/extractor.py`:
  - `_CLI_DECORATOR_NAMES = {"command", "group", "callback"}` at line 1230.
  - `_function_tags` adds `entry-point` for module-level `main`, `http-route` for route decorators, and `cli-command` for click/typer-style decorators at lines 1347-1368.
  - `_module_surface_tag` tags direct `__all__` function/class members as `exported-api` and no-`__all__` public defs/classes as `public-surface` at lines 1371-1392.
  - `_build_module_entity` currently attaches no tags to module entities at lines 344-363.
- Python tests already cover decorator HTTP routes and direct `__all__` exports in `plugins/python/tests/test_extractor.py` around lines 1230-1481.
- Rust tag emission already covers the four required tag classes in `crates/loomweave-plugin-rust/src/root_tags.rs`:
  - `exported-api`, `entry-point`, `http-route`, `cli-command` are inserted at lines 121-142.
  - Tests for HTTP route and clap-derived CLI tags live in `crates/loomweave-plugin-rust/tests/root_tags.rs` lines 350-388.
- The host/storage path is already correct:
  - `RawEntity.tags` is typed and defaults empty in `crates/loomweave-core/src/plugin/host.rs` lines 143-147.
  - `map_entity_to_record` normalizes tags with `normalised_entity_tags` in `crates/loomweave-cli/src/analyze.rs` lines 5905-5924.
  - `entity_tags` is the persisted table in `crates/loomweave-storage/migrations/0001_initial_schema.sql` lines 64-72.
  - The writer deletes/reinserts tags per entity in `crates/loomweave-storage/src/writer.rs` lines 775-784.
- MCP shortcuts currently expose `entity_entry_point_list` and `entity_http_route_list` but not `entity_exported_api_list` or `entity_cli_command_list`:
  - aliases in `crates/loomweave-mcp/src/lib.rs` lines 149-152.
  - tool definitions in `crates/loomweave-mcp/src/lib.rs` lines 669-687.
  - dispatch in `crates/loomweave-mcp/src/lib.rs` lines 1788-1803.
  - shortcut implementations in `crates/loomweave-mcp/src/catalogue/shortcuts.rs` lines 628-684.
  - exact tool-order tests in `crates/loomweave-mcp/src/lib.rs` lines 6308-6442 and E2E pinned names in `tests/e2e/sprint_2_mcp_surface.sh` lines 510-546.
- Plainweave already does the honest thing:
  - `PUBLIC_SURFACE_TAGS = {"exported-api", "entry-point", "http-route", "cli-command"}` in `/home/john/plainweave/src/plainweave/loomweave_adapter.py` line 18.
  - It marks the denominator incomplete when any tag class is absent in `/home/john/plainweave/src/plainweave/loomweave_adapter.py` lines 196-223.
  - Current `/home/john/scrappack` Plainweave output has `denominator_complete=false`, absent tags `cli-command`, `exported-api`, `http-route`, and a degraded note naming the Loomweave catalog gap.

---

## Task 1: Expose `exported-api` and `cli-command` as MCP Shortcut Tools

**Files:**
- Modify: `crates/loomweave-mcp/src/lib.rs`
- Modify: `crates/loomweave-mcp/src/catalogue/shortcuts.rs`
- Test: `crates/loomweave-mcp/tests/catalogue_tools.rs`
- Test: `tests/e2e/sprint_2_mcp_surface.sh`
- Docs: none in this task

**Step 1: Write the failing MCP shortcut tests**

In `crates/loomweave-mcp/tests/catalogue_tools.rs`:
- Extend the tool loop in `categorisation_shortcuts_are_honest_empty` to include:
  - `"find_exported_apis"`
  - `"find_cli_commands"`
- Add a new test after `find_tests_lights_up_when_test_tag_is_present`:

```rust
#[tokio::test]
async fn public_surface_shortcuts_light_up_when_tags_are_present() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:function:api.exported", "function", "api.py", Some((1, 2)));
    insert_entity(&conn, "python:function:cli.main", "function", "cli.py", Some((1, 8)));
    insert_tag(&conn, "python:function:api.exported", "exported-api");
    insert_tag(&conn, "python:function:cli.main", "cli-command");
    drop(conn);
    let state = state_for(project.path(), &db);

    let exported = call_tool(&state, "find_exported_apis", json!({})).await;
    assert_eq!(exported["result"]["page"]["total"], 1, "{exported}");
    assert_eq!(
        exported["result"]["entities"][0]["id"],
        "python:function:api.exported"
    );

    let cli = call_tool(&state, "find_cli_commands", json!({})).await;
    assert_eq!(cli["result"]["page"]["total"], 1, "{cli}");
    assert_eq!(cli["result"]["entities"][0]["id"], "python:function:cli.main");
}
```

**Why this test:** It proves the shortcuts are real tag-backed queries and not hardcoded empty responses.

**Step 2: Run the red tests**

Run:
```bash
cargo nextest run -p loomweave-mcp categorisation_shortcuts_are_honest_empty public_surface_shortcuts_light_up_when_tags_are_present
```

Expected output:
```text
FAIL ... categorisation_shortcuts_are_honest_empty
FAIL ... public_surface_shortcuts_light_up_when_tags_are_present
unknown tool: find_exported_apis
```

**Step 3: Implement the shortcuts**

In `crates/loomweave-mcp/src/lib.rs`:
- Add aliases beside the existing categorisation aliases:

```rust
("find_exported_apis", "entity_exported_api_list"),
("find_cli_commands", "entity_cli_command_list"),
```

- Add two `ToolDefinition` entries immediately after `entity_http_route_list`:

```rust
ToolDefinition {
    name: "entity_exported_api_list",
    description: "Entities tagged `exported-api`, optional `scope`. Honest-empty when export categorisation is not emitted. Bounded.",
    input_schema: scope_page_schema(false),
},
ToolDefinition {
    name: "entity_cli_command_list",
    description: "Entities tagged `cli-command`, optional `scope`. Honest-empty when CLI categorisation is not emitted. Bounded.",
    input_schema: scope_page_schema(false),
},
```

- Add dispatch arms immediately after `entity_http_route_list`:

```rust
"entity_exported_api_list" => match self.tool_find_exported_apis(arguments).await {
    Ok(value) => value,
    Err(response) => return response.to_json_rpc(id),
},
"entity_cli_command_list" => match self.tool_find_cli_commands(arguments).await {
    Ok(value) => value,
    Err(response) => return response.to_json_rpc(id),
},
```

- Update exact tool-order assertions:
  - `tools.len()` from `46` to `48`.
  - Insert `entity_exported_api_list` and `entity_cli_command_list` after `entity_http_route_list`.
  - Increment the following indices by 2.

In `crates/loomweave-mcp/src/catalogue/shortcuts.rs`, add methods after `tool_find_http_routes`:

```rust
/// `find_exported_apis(scope?)` - entities tagged as exported APIs
/// (honest-empty when the `exported-api` tag is not emitted).
pub(crate) async fn tool_find_exported_apis(
    &self,
    arguments: &serde_json::Map<String, Value>,
) -> std::result::Result<Value, ParamError> {
    self.categorisation_shortcut(
        arguments,
        "exported-api",
        "no entity is tagged as an exported API in this index",
    )
    .await
}

/// `find_cli_commands(scope?)` - entities tagged as CLI commands
/// (honest-empty when the `cli-command` tag is not emitted).
pub(crate) async fn tool_find_cli_commands(
    &self,
    arguments: &serde_json::Map<String, Value>,
) -> std::result::Result<Value, ParamError> {
    self.categorisation_shortcut(
        arguments,
        "cli-command",
        "no entity is tagged as a CLI command in this index",
    )
    .await
}
```

In `tests/e2e/sprint_2_mcp_surface.sh`, add the two canonical tool names after `entity_http_route_list`:

```python
"entity_exported_api_list",
"entity_cli_command_list",
```

**Step 4: Run the green tests**

Run:
```bash
cargo nextest run -p loomweave-mcp categorisation_shortcuts_are_honest_empty public_surface_shortcuts_light_up_when_tags_are_present tools_list_fits_the_context_budget tools_list_exposes_exact_docstrings server_instructions_enumerate_every_tool
cargo nextest run -p loomweave-mcp server_instructions_fit_truncating_clients
```

Expected output:
```text
PASS ... categorisation_shortcuts_are_honest_empty
PASS ... public_surface_shortcuts_light_up_when_tags_are_present
PASS ... tools_list_fits_the_context_budget
PASS ... tools_list_exposes_exact_docstrings
PASS ... server_instructions_enumerate_every_tool
PASS ... server_instructions_fit_truncating_clients
```

If `tools_list_fits_the_context_budget` fails by a small amount, shorten only the two new descriptions first. Do not raise the 23,500-byte budget for two simple tag shortcuts.

**Step 5: Commit**

```bash
git add crates/loomweave-mcp/src/lib.rs crates/loomweave-mcp/src/catalogue/shortcuts.rs crates/loomweave-mcp/tests/catalogue_tools.rs tests/e2e/sprint_2_mcp_surface.sh
git commit -m "feat(mcp): expose exported API and CLI command tag shortcuts"
```

**Definition of Done:**
- [ ] `find_exported_apis` and `find_cli_commands` aliases resolve.
- [ ] `entity_exported_api_list` and `entity_cli_command_list` appear in `tools/list` and initialize instructions.
- [ ] Shortcut tests prove both honest-empty and tag-present behavior.
- [ ] Tool-list budget still passes without raising the budget.
- [ ] E2E pinned tool inventory includes both canonical names.

---

## Task 2: Tag Explicit Python Re-Export Modules as `exported-api`

**Files:**
- Modify: `plugins/python/src/loomweave_plugin_python/extractor.py`
- Modify: `plugins/python/plugin.toml`
- Modify: `plugins/python/src/loomweave_plugin_python/server.py`
- Test: `plugins/python/tests/test_extractor.py`
- Test: `plugins/python/tests/test_package.py`
- Test: `plugins/python/tests/test_server.py`
- Docs: none in this task

**Step 1: Write failing extractor tests**

In `plugins/python/tests/test_extractor.py`, add tests near the existing `__all__` tests:

```python
def test_dunder_all_reexport_module_is_exported_api_surface() -> None:
    source = """\
from pkg.impl import Thing, do_work

__all__ = ["Thing", "do_work"]
"""
    entities, _ = extract(source, "api.py")
    by_id = {e["id"]: e for e in entities}

    module = by_id["python:module:api"]
    assert "exported-api" in module["tags"]
    assert module["exported_names"] == ["Thing", "do_work"]


def test_empty_dunder_all_does_not_tag_module_exported_api() -> None:
    source = """\
__all__ = []
"""
    entities, _ = extract(source, "api.py")
    module = next(e for e in entities if e["id"] == "python:module:api")

    assert "tags" not in module or "exported-api" not in module["tags"]
    assert module["exported_names"] == []


def test_dunder_all_local_definition_does_not_double_count_module() -> None:
    source = """\
__all__ = ["exported_fn"]


def exported_fn():
    return 1
"""
    entities, _ = extract(source, "api.py")
    by_id = {e["id"]: e for e in entities}

    module = by_id["python:module:api"]
    function = by_id["python:function:api.exported_fn"]
    assert "exported-api" in function["tags"]
    assert "tags" not in module or "exported-api" not in module["tags"]
    assert module["exported_names"] == ["exported_fn"]
```

**Why this test:** Direct function/class exports are already covered; the validation repo has an adapter module (`rustfang/engine/oracle.py`) that exports imported names via `__all__`. Loomweave needs a persisted public-surface row for that exported module so Plainweave can include the exported API denominator honestly. It must not tag ordinary modules just because `__all__` lists local functions or classes, because Plainweave counts every public-surface-tagged entity in the denominator and would double-count the module plus the local entity.

**Step 2: Run the red tests**

Run:
```bash
plugins/python/.venv/bin/pytest --no-cov plugins/python/tests/test_extractor.py::test_dunder_all_reexport_module_is_exported_api_surface plugins/python/tests/test_extractor.py::test_empty_dunder_all_does_not_tag_module_exported_api plugins/python/tests/test_extractor.py::test_dunder_all_local_definition_does_not_double_count_module -q
```

Expected output:
```text
FAILED ... KeyError: 'tags'
FAILED ... KeyError: 'exported_names'
FAILED ... KeyError: 'exported_names'
```

**Step 3: Implement module export metadata**

In `plugins/python/src/loomweave_plugin_python/extractor.py`:
- Add the metadata field to the local `RawEntity` `TypedDict`; `typing.NotRequired` is already imported in this file:

```python
# Explicit __all__ names observed on module entities. Stored through the host
# RawEntity extra/properties path; omitted unless __all__ is declared.
exported_names: NotRequired[list[str]]
```

- Change `_build_module_entity` to accept `exported_names: set[str] | None = None` and `local_export_entity_names: set[str] | None = None`.
- If `exported_names is not None`, attach `entity["exported_names"] = sorted(exported_names)`.
- Tag the module entity `exported-api` only when `__all__` contains names that are not module-level local function/class entities. This is the re-export/imported-name case Plainweave needs; ordinary local exports are already represented by their function/class entities and must not be double-counted at module level.
- Compute `exported_names = _module_export_names(tree)` and `local_export_entity_names = _module_level_exportable_names(tree)` once in `extract_with_stats`; pass `exported_names` to both `_build_module_entity` and `_WalkState`, and pass `local_export_entity_names` only to `_build_module_entity`.

Implementation sketch:

```python
def _build_module_entity(
    source: str,
    dotted_module: str,
    file_path: str,
    parse_status: Literal["ok", "syntax_error", "too_complex"],
    docstring: str | None = None,
    exported_names: set[str] | None = None,
    local_export_entity_names: set[str] | None = None,
) -> RawEntity:
    entity: RawEntity = {
        "id": entity_id(_PLUGIN_ID, "module", dotted_module),
        "kind": "module",
        "qualified_name": dotted_module,
        "source": {
            "file_path": file_path,
            "source_range": _module_source_range(source),
        },
        "parse_status": parse_status,
    }
    tags: set[str] = set()
    if exported_names is not None:
        entity["exported_names"] = sorted(exported_names)
        reexported_names = exported_names - (local_export_entity_names or set())
        if reexported_names:
            tags.add("exported-api")
    _attach_optional_entity_metadata(entity, docstring=docstring, tags=tags)
    return entity


def _module_level_exportable_names(tree: ast.Module) -> set[str]:
    return {
        statement.name
        for statement in tree.body
        if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
    }
```

In `extract_with_stats`, replace the current module build with:

```python
exported_names = _module_export_names(tree)
local_export_entity_names = _module_level_exportable_names(tree)
module_entity = _build_module_entity(
    source,
    dotted_module,
    file_path,
    "ok",
    ast.get_docstring(tree),
    exported_names,
    local_export_entity_names,
)
...
walk_state = _WalkState(
    seen_ids={module_entity["id"]},
    file_path=file_path,
    exported_names=exported_names,
    wardline_vocabulary=wardline_vocabulary,
)
```

Leave syntax-error and too-complex module entities untagged because no reliable AST-level `__all__` evidence exists in those cases.

**Step 4: Bump Python plugin ontology version**

This is an additive tag-schema change. Bump `0.9.0` to `0.10.0` in:
- `plugins/python/plugin.toml` line 58, and add a comment saying `0.10.0` adds module-level `exported-api` for explicit `__all__` re-export/imported-name surfaces without double-counting local function/class exports.
- `plugins/python/src/loomweave_plugin_python/server.py` line 37.
- `plugins/python/tests/test_package.py` line 50.
- `plugins/python/tests/test_server.py` line 90.

**Step 5: Run the green tests and ontology guard**

Run:
```bash
plugins/python/.venv/bin/pytest --no-cov plugins/python/tests/test_extractor.py::test_dunder_all_reexport_module_is_exported_api_surface plugins/python/tests/test_extractor.py::test_empty_dunder_all_does_not_tag_module_exported_api plugins/python/tests/test_extractor.py::test_dunder_all_local_definition_does_not_double_count_module plugins/python/tests/test_package.py::test_manifest_declares_current_v1_ontology_only plugins/python/tests/test_server.py::test_initialize_roundtrip -q
python scripts/check-python-ontology-version.py
```

Expected output:
```text
5 passed
Python plugin ontology_version matches: 0.10.0
```

**Step 6: Commit**

```bash
git add plugins/python/src/loomweave_plugin_python/extractor.py plugins/python/plugin.toml plugins/python/src/loomweave_plugin_python/server.py plugins/python/tests/test_extractor.py plugins/python/tests/test_package.py plugins/python/tests/test_server.py
git commit -m "feat(python): tag explicit module exports as exported API surfaces"
```

**Definition of Done:**
- [ ] Explicit `__all__` re-export/imported names tag the module entity `exported-api`.
- [ ] Explicit `__all__` local function/class names tag the local entities only and do not tag the module `exported-api`.
- [ ] Empty explicit `__all__` remains an explicit empty surface, not an exported API.
- [ ] Existing direct function/class `__all__` behavior is unchanged.
- [ ] Python ontology version lockstep passes.
- [ ] Incremental re-analysis will refresh stale tag rows because the ontology marker changed.

---

## Task 3: Tag Python Manual and Argparse CLI Entry Points as `cli-command`

**Files:**
- Modify: `plugins/python/src/loomweave_plugin_python/extractor.py`
- Modify: `plugins/python/plugin.toml`
- Modify: `plugins/python/src/loomweave_plugin_python/server.py`
- Test: `plugins/python/tests/test_extractor.py`
- Test: `plugins/python/tests/test_package.py`
- Test: `plugins/python/tests/test_server.py`
- Docs: none in this task

**Step 1: Write failing extractor tests**

In `plugins/python/tests/test_extractor.py`, add tests near `test_categorisation_tags_and_docstrings_are_emitted`:

```python
def test_main_guard_target_is_cli_command_and_entry_point() -> None:
    source = """\
def run():
    print("interactive")


if __name__ == "__main__":
    run()
"""
    entities, _ = extract(source, "cli.py")
    run = next(e for e in entities if e["id"] == "python:function:cli.run")

    assert "entry-point" in run["tags"]
    assert "cli-command" in run["tags"]
    assert "framework-handler" not in run["tags"]


def test_sys_argv_dispatch_function_is_cli_command() -> None:
    source = """\
import sys


def main():
    args = sys.argv[1:]
    return args
"""
    entities, _ = extract(source, "tool.py")
    main = next(e for e in entities if e["id"] == "python:function:tool.main")

    assert "entry-point" in main["tags"]
    assert "cli-command" in main["tags"]


def test_argparse_function_is_cli_command() -> None:
    source = """\
import argparse


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args()
"""
    entities, _ = extract(source, "gate.py")
    main = next(e for e in entities if e["id"] == "python:function:gate.main")

    assert "entry-point" in main["tags"]
    assert "cli-command" in main["tags"]


def test_cli_parsing_helper_is_not_cli_command_without_command_candidate() -> None:
    source = """\
import argparse


def parse_options():
    parser = argparse.ArgumentParser()
    return parser.parse_args([])
"""
    entities, _ = extract(source, "helper.py")
    helper = next(e for e in entities if e["id"] == "python:function:helper.parse_options")

    assert "cli-command" not in helper.get("tags", [])


def test_nested_cli_parser_is_not_cli_command() -> None:
    source = """\
import argparse


def main():
    def parse_options():
        return argparse.ArgumentParser().parse_args([])
    return parse_options()
"""
    entities, _ = extract(source, "nested.py")
    nested = next(e for e in entities if e["qualified_name"].endswith("main.parse_options"))

    assert "cli-command" not in nested.get("tags", [])


def test_class_cli_parser_method_is_not_cli_command() -> None:
    source = """\
import sys


class CliConfig:
    def parse_options(self):
        return sys.argv[1:]
"""
    entities, _ = extract(source, "settings.py")
    method = next(e for e in entities if e["qualified_name"].endswith("CliConfig.parse_options"))

    assert "cli-command" not in method.get("tags", [])
```

**Why this test:** `/home/john/scrappack/rustfang/playtest_cli.py` uses manual `sys.argv` dispatch; `/home/john/scrappack-engine-phase-tasks-1-2/rustfang/tuning/run_gate.py` uses `argparse`; `/home/john/scrappack/probe_a/cli.py` exposes `run()` through a main guard. None are click/typer decorators, so the current `_CLI_DECORATOR_NAMES` path cannot see them.

**Step 2: Run the red tests**

Run:
```bash
plugins/python/.venv/bin/pytest --no-cov plugins/python/tests/test_extractor.py::test_main_guard_target_is_cli_command_and_entry_point plugins/python/tests/test_extractor.py::test_sys_argv_dispatch_function_is_cli_command plugins/python/tests/test_extractor.py::test_argparse_function_is_cli_command plugins/python/tests/test_extractor.py::test_cli_parsing_helper_is_not_cli_command_without_command_candidate plugins/python/tests/test_extractor.py::test_nested_cli_parser_is_not_cli_command plugins/python/tests/test_extractor.py::test_class_cli_parser_method_is_not_cli_command -q
```

Expected output:
```text
FAILED ... assert 'entry-point' in ...
FAILED ... assert 'cli-command' in ...
FAILED ... assert 'cli-command' in ...
FAILED ... assert 'cli-command' in ...
3 passed
```

**Step 3: Implement the AST helpers**

In `plugins/python/src/loomweave_plugin_python/extractor.py`:
- Add `cli_guard_targets: set[str] = field(default_factory=set)` to `_WalkState`.
- Add helper functions after `_module_export_names`:

```python
def _main_guard_targets(tree: ast.Module) -> set[str]:
    targets: set[str] = set()
    for statement in tree.body:
        if not isinstance(statement, ast.If) or not _is_main_guard(statement.test):
            continue
        for child in statement.body:
            call: ast.Call | None = None
            if isinstance(child, ast.Expr) and isinstance(child.value, ast.Call):
                call = child.value
            elif isinstance(child, ast.Return) and isinstance(child.value, ast.Call):
                call = child.value
            if isinstance(call, ast.Call) and isinstance(call.func, ast.Name):
                targets.add(call.func.id)
    return targets


def _is_main_guard(node: ast.expr) -> bool:
    return (
        isinstance(node, ast.Compare)
        and isinstance(node.left, ast.Name)
        and node.left.id == "__name__"
        and len(node.ops) == 1
        and isinstance(node.ops[0], ast.Eq)
        and len(node.comparators) == 1
        and isinstance(node.comparators[0], ast.Constant)
        and node.comparators[0].value == "__main__"
    )


def _function_uses_cli_parsing(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    for child in ast.walk(node):
        if isinstance(child, ast.Attribute) and child.attr == "argv":
            if isinstance(child.value, ast.Name) and child.value.id == "sys":
                return True
        if isinstance(child, ast.Call):
            func = child.func
            if isinstance(func, ast.Attribute) and func.attr in {"parse_args", "parse_known_args"}:
                return True
            if (
                isinstance(func, ast.Attribute)
                and func.attr == "ArgumentParser"
                and isinstance(func.value, ast.Name)
                and func.value.id == "argparse"
            ):
                return True
            if isinstance(func, ast.Name) and func.id == "ArgumentParser":
                return True
    return False
```

- In `extract_with_stats`, compute `cli_guard_targets = _main_guard_targets(tree)` and pass it to `_WalkState`.
- Change `_function_tags` signature to accept `cli_guard_targets: set[str]`.
- Inside `_function_tags`, after the existing module-level `main` rule, only apply manual parsing heuristics to module-level command candidates. A command candidate is either a module-level function named `main` or a module-level function directly called from an `if __name__ == "__main__"` guard. Do not tag arbitrary helper functions, nested functions, or class methods just because they parse argv.

```python
if module_level and node.name in cli_guard_targets:
    tags.update({"entry-point", "cli-command"})

command_candidate = module_level and (
    node.name == "main" or node.name in cli_guard_targets
)
if command_candidate and _function_uses_cli_parsing(node):
    tags.add("cli-command")
```

- Keep `framework-handler` only for decorator/framework-dispatched CLI functions. Manual `sys.argv`/`argparse` commands are public surfaces, not framework handlers.

**Step 4: Bump Python plugin ontology version**

This is another additive tag-schema change. Bump `0.10.0` to `0.11.0` in:
- `plugins/python/plugin.toml`, adding a comment that `0.11.0` adds manual main-guard, `sys.argv`, and argparse `cli-command` tagging.
- `plugins/python/src/loomweave_plugin_python/server.py`.
- `plugins/python/tests/test_package.py`.
- `plugins/python/tests/test_server.py`.

**Step 5: Run the green tests and Python quality checks**

Run:
```bash
plugins/python/.venv/bin/pytest --no-cov plugins/python/tests/test_extractor.py::test_main_guard_target_is_cli_command_and_entry_point plugins/python/tests/test_extractor.py::test_sys_argv_dispatch_function_is_cli_command plugins/python/tests/test_extractor.py::test_argparse_function_is_cli_command plugins/python/tests/test_extractor.py::test_cli_parsing_helper_is_not_cli_command_without_command_candidate plugins/python/tests/test_extractor.py::test_nested_cli_parser_is_not_cli_command plugins/python/tests/test_extractor.py::test_class_cli_parser_method_is_not_cli_command plugins/python/tests/test_package.py::test_manifest_declares_current_v1_ontology_only plugins/python/tests/test_server.py::test_initialize_roundtrip -q
python scripts/check-python-ontology-version.py
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
```

Expected output:
```text
8 passed
Python plugin ontology_version matches: 0.11.0
All checks passed!
Success: no issues found
```

**Step 6: Commit**

```bash
git add plugins/python/src/loomweave_plugin_python/extractor.py plugins/python/plugin.toml plugins/python/src/loomweave_plugin_python/server.py plugins/python/tests/test_extractor.py plugins/python/tests/test_package.py plugins/python/tests/test_server.py
git commit -m "feat(python): tag manual CLI entry points"
```

**Definition of Done:**
- [ ] Main-guard target functions are tagged `entry-point` and `cli-command`.
- [ ] Module-level command candidates using `sys.argv` dispatch or `argparse` are tagged `cli-command`.
- [ ] Helper functions, nested functions, and class methods are not tagged `cli-command` solely because they parse argv.
- [ ] Decorator-based CLI behavior remains unchanged.
- [ ] Manual CLI tags do not get `framework-handler`.
- [ ] Python ontology version lockstep passes.
- [ ] `ruff` and `mypy --strict` pass for the Python plugin.

---

## Task 4: Add an E2E Catalog Fixture Covering All Four Public-Surface Tags

**Files:**
- Modify: `tests/e2e/sprint_2_mcp_surface.sh`
- Test: `tests/e2e/sprint_2_mcp_surface.sh`
- Docs: none in this task

**Step 1: Extend the E2E fixture source**

The heredoc in `tests/e2e/sprint_2_mcp_surface.sh` is line-sensitive. Existing assertions include an `entity_at` call for `{"file":"demo.py","line":10}` that resolves to `python:function:demo.hello`, and later source/excerpt/caller/path assertions also reference `demo.hello`. Preferred implementation: insert the new fixture code after the existing `hello()` block so `hello` stays on the same line. If the implementer moves `hello`, they must update every affected assertion in the same commit, including `entity_at`, source/excerpt, caller/path, neighborhood, and any expected line numbers.

Add a sibling implementation module before the existing `demo.py` heredoc:

```bash
cat > demo_impl.py <<'PY'
def exported_api():
    return 42
PY
```

In the heredoc that writes `demo.py`, add all four required tag classes without external runtime dependencies. To preserve the existing `hello` line assertion, append this block after the current `hello()` definition and before the existing dispatch/caller fixture code. The `__all__` entry must name an imported symbol, not a local function, so the E2E proves the module-level re-export path from Task 2:

```python
import argparse
from demo_impl import exported_api

__all__ = ["exported_api"]

class Router:
    def get(self, path):
        def wrap(fn):
            return fn
        return wrap

router = Router()

@router.get("/health")
def health():
    return exported_api()

def cli():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args([])

if __name__ == "__main__":
    cli()
```

Keep the existing demo functions unless a name collision makes an assertion ambiguous, and verify that the existing `entity_at` assertion still points at `demo.hello` before adding new public-surface assertions.

**Step 2: Add failing SQLite assertions**

In the Python driver inside `tests/e2e/sprint_2_mcp_surface.sh`, after opening the SQLite connection and before `conn.close()`, add:

```python
tag_counts = dict(
    conn.execute(
        """
        SELECT tag, COUNT(*)
        FROM entity_tags
        WHERE tag IN ('entry-point', 'http-route', 'exported-api', 'cli-command')
        GROUP BY tag
        """
    ).fetchall()
)
assert tag_counts.get("entry-point", 0) >= 1, tag_counts
assert tag_counts.get("http-route", 0) >= 1, tag_counts
assert tag_counts.get("exported-api", 0) >= 1, tag_counts
assert tag_counts.get("cli-command", 0) >= 1, tag_counts

module_export = conn.execute(
    """
    SELECT e.kind
    FROM entities e
    JOIN entity_tags t ON t.entity_id = e.id
    WHERE e.id = 'python:module:demo'
      AND e.kind = 'module'
      AND t.tag = 'exported-api'
    """
).fetchone()
assert module_export == ("module",), module_export
```

**Why this test:** It proves a real `loomweave analyze` run persists the four tag classes Plainweave uses for the denominator, not only unit-test fixtures. The explicit `python:module:demo` assertion proves the new re-export-module behavior instead of accidentally satisfying `exported-api` through a local function/class export.

**Step 3: Add failing MCP calls**

Where the E2E calls categorisation tools, call the four public-surface shortcuts and assert at least one result:

```python
for tool_name in [
    "entity_entry_point_list",
    "entity_http_route_list",
    "entity_exported_api_list",
    "entity_cli_command_list",
]:
    write_frame(proc, {
        "jsonrpc": "2.0",
        "id": f"public-surface-{tool_name}",
        "method": "tools/call",
        "params": {"name": tool_name, "arguments": {}},
    })
    envelope = assert_tool_ok(read_frame(proc))
    assert envelope["result"]["page"]["total"] >= 1, (tool_name, envelope)
```

**Step 4: Run the red E2E before implementation tasks are complete**

If run before Tasks 1-3, this should fail:

```bash
CARGO_BUILD=0 bash tests/e2e/sprint_2_mcp_surface.sh
```

Expected output:
```text
[mcp-surface] FAIL ...
unknown tool: entity_exported_api_list
```

or:

```text
AssertionError: {'entry-point': ..., 'http-route': ..., 'exported-api': ...}
```

**Step 5: Run the green E2E after Tasks 1-3**

Run:
```bash
bash tests/e2e/sprint_2_mcp_surface.sh
```

Expected output:
```text
[mcp-surface] building loomweave (release) ...
[mcp-surface] running: loomweave analyze .
[mcp-surface] driving MCP stdio requests ...
[mcp-surface] ok
```

**Step 6: Commit**

```bash
git add tests/e2e/sprint_2_mcp_surface.sh
git commit -m "test(e2e): cover all public-surface tag classes"
```

**Definition of Done:**
- [ ] E2E creates a demo catalog with `entry-point`, `http-route`, `exported-api`, and `cli-command`.
- [ ] E2E proves the tag rows exist in SQLite.
- [ ] E2E proves all four MCP shortcut tools return tag-backed results.
- [ ] The pinned tool-name list remains exact and green.

---

## Task 5: Document Plainweave Attribution and Denominator Semantics

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `docs/operator/language-support.md`
- Modify: `crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md`
- Test: documentation grep checks and existing MCP budget tests

**Step 1: Update user-facing docs**

In `CHANGELOG.md` under `## [Unreleased]`, add:

```markdown
### Added

- **Plainweave public-surface denominator completeness.** Loomweave now exposes
  MCP shortcuts and plugin tags for the four public-surface classes Plainweave
  uses for intent coverage: `entry-point`, `http-route`, `exported-api`, and
  `cli-command`.

### Changed

- **Coverage honesty over higher-looking ratios.** Expanding these tags can lower
  Plainweave coverage on real projects by adding previously invisible public
  surfaces to the denominator. That is the desired result: Plainweave's prior
  degraded output was correct; Loomweave now sees more doors.
```

In `docs/operator/language-support.md`:
- Update the Python ontology version shown in the language-support table from `0.9.0` to `0.11.0`.
- Update the Python tag section to mention:
  - explicit `__all__` re-export/imported-name module entities get `exported-api`, while local functions/classes listed in `__all__` remain the tagged public-surface entities and do not cause module-level double-counting;
  - main-guard targets and module-level command candidates using `sys.argv` or `argparse` get `cli-command`;
  - lower coverage after re-analysis is an accuracy improvement when previously invisible public surfaces enter the denominator.
- Leave the Rust limitations section intact; it already says Rust emits `exported-api`, `entry-point`, `http-route`, and `cli-command`.

In `crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md`:
- Add `entity_exported_api_list` and `entity_cli_command_list` to the categorisation shortcut table near `entity_entry_point_list` / `entity_http_route_list`.
- Keep the descriptions short enough for skill readability; `tools/list` budget is governed by `crates/loomweave-mcp/src/lib.rs`, not this skill file.

**Step 2: Run doc and budget checks**

Run:
```bash
rg -n "Plainweave|0.11.0|entry-point|http-route|exported-api|cli-command|sees more doors" CHANGELOG.md docs/operator/language-support.md crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md
cargo nextest run -p loomweave-mcp tools_list_fits_the_context_budget server_instructions_fit_truncating_clients
```

Expected output:
```text
CHANGELOG.md:...
docs/operator/language-support.md:...
crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md:...
PASS ... tools_list_fits_the_context_budget
PASS ... server_instructions_fit_truncating_clients
```

**Step 3: Commit**

```bash
git add CHANGELOG.md docs/operator/language-support.md crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md
git commit -m "docs: explain Plainweave public-surface denominator honesty"
```

**Definition of Done:**
- [ ] Docs explicitly say Plainweave was correct to report incomplete source-denominator coverage.
- [ ] Docs explicitly say lower coverage after re-analysis can be accurate and desired.
- [ ] Docs use the required attribution: Plainweave good; requirements good; Loomweave must see all doors.
- [ ] MCP context-budget tests still pass.

---

## Task 6: Validate Loomweave Public-Surface Capability

**Files:**
- Create: `docs/implementation/2026-07-10-public-surface-tags-plainweave-validation.md`
- Test: live validation commands against `/home/john/scrappack`
- Test: live validation commands against `/home/john/scrappack-engine-phase-tasks-1-2`

**Step 1: Write the validation report stub**

Create `docs/implementation/2026-07-10-public-surface-tags-plainweave-validation.md` with sections:

```markdown
# Public Surface Tags for Plainweave Validation - 2026-07-10

## Summary

Plainweave was already honestly reporting denominator incompleteness. This
validation checks that Loomweave now emits, persists, and exposes public-surface
tags when source evidence exists. Scrappack corpus-specific absences are evidence
observations, not Loomweave acceptance criteria.

## /home/john/scrappack

## /home/john/scrappack-engine-phase-tasks-1-2

## Attribution

Plainweave good. Requirements good. Loomweave must see all doors.
```

**Step 2: Build and preflight the exact Loomweave + Python plugin artifacts**

Build the updated Rust binary and refresh the Python plugin install after the final `0.11.0` ontology bump. This is not optional: Loomweave plugin discovery scans `$PATH` first, and the Python plugin manifest is installed as shared data. A stale installed `plugin.toml` can make validation prove the wrong artifact.

Run:
```bash
cd /home/john/loomweave
cargo build --workspace --bins
uv sync --project plugins/python --locked --extra dev
plugins/python/.venv/bin/python -m pip install --quiet --force-reinstall -e 'plugins/python[dev]'
PATH="/home/john/loomweave/plugins/python/.venv/bin:/home/john/loomweave/target/debug:$PATH"
python - <<'PY'
import shutil

print("loomweave =", shutil.which("loomweave"))
print("loomweave-plugin-python =", shutil.which("loomweave-plugin-python"))
PY
rg -n 'ontology_version = "0.11.0"|version = "1.4.1"' plugins/python/.venv/share/loomweave/plugins/python/plugin.toml
```

Expected output:
```text
loomweave = /home/john/loomweave/target/debug/loomweave
loomweave-plugin-python = /home/john/loomweave/plugins/python/.venv/bin/loomweave-plugin-python
plugins/python/.venv/share/loomweave/plugins/python/plugin.toml:...:version = "1.4.1"
plugins/python/.venv/share/loomweave/plugins/python/plugin.toml:...:ontology_version = "0.11.0"
```

If either executable resolves anywhere else, stop and fix the PATH/install before mutating validation catalogs.

Use this exact PATH for every validation command below:

```bash
export PATH="/home/john/loomweave/plugins/python/.venv/bin:/home/john/loomweave/target/debug:$PATH"
```

**Step 3: Re-analyze `/home/john/scrappack` as one live evidence corpus**

This validation mutates `/home/john/scrappack/.weft/loomweave/loomweave.db`. Record the repo status and the pre-analysis tag counts before running `loomweave analyze`, then record the post-analysis status and counts immediately after. If the repo already has local changes, record them plainly in the validation report before mutating the catalog.

Run:
```bash
cd /home/john/scrappack
git status --short
stat -c '%s %Y %n' .weft/loomweave/loomweave.db
sha256sum .weft/loomweave/loomweave.db
sqlite3 -readonly .weft/loomweave/loomweave.db "SELECT tag, COUNT(*) FROM entity_tags WHERE tag IN ('entry-point','http-route','exported-api','cli-command','public-surface','framework-handler') GROUP BY tag ORDER BY tag;"
loomweave analyze .
git status --short
stat -c '%s %Y %n' .weft/loomweave/loomweave.db
sha256sum .weft/loomweave/loomweave.db
sqlite3 -readonly .weft/loomweave/loomweave.db "SELECT tag, COUNT(*) FROM entity_tags WHERE tag IN ('entry-point','http-route','exported-api','cli-command','public-surface','framework-handler') GROUP BY tag ORDER BY tag;"
sqlite3 -readonly .weft/loomweave/loomweave.db "SELECT e.id, group_concat(t.tag, ',') FROM entities e JOIN entity_tags t ON t.entity_id=e.id WHERE t.tag IN ('entry-point','http-route','exported-api','cli-command') GROUP BY e.id ORDER BY e.id LIMIT 40;"
plainweave intent coverage --json --max-surfaces 5
```

Expected SQLite output includes non-zero counts for the tag classes whose source evidence exists in this corpus. For `/home/john/scrappack`, the acceptance signal is currently the newly visible CLI surfaces, not all four tag classes:
```text
entry-point|...
cli-command|...
public-surface|...
```

`http-route` and `exported-api` may remain absent for `/home/john/scrappack` if the project has no such source evidence. If absent, record that as source reality, not a Loomweave or Plainweave defect. Do not treat Scrappack as the proof target for every tag class; the E2E fixture and engine-phase repo cover the missing Loomweave functionality where source evidence exists.

For `plainweave intent coverage --json`, expected shape:
```json
{
  "ok": true,
  "data": {
    "coverage": {
      "public_surface_tags": ["cli-command", "entry-point", "exported-api", "http-route"],
      "present_tags": ["cli-command", "entry-point"],
      "complete": false
    },
    "adapter": {
      "degraded": [
        {"code": "public_surface_tags_incomplete"}
      ]
    }
  }
}
```

If Scrappack happens to gain source evidence for all four tag classes later, `complete` should become `true` and the `public_surface_tags_incomplete` degraded note should disappear. Otherwise, `complete=false` is still honest because Plainweave's current completeness flag is tag-class-presence based.

**Step 4: Re-analyze `/home/john/scrappack-engine-phase-tasks-1-2` as one live evidence corpus**

This validation mutates `/home/john/scrappack-engine-phase-tasks-1-2/.weft/loomweave/loomweave.db`. Record before/after status and tag counts even if Plainweave itself is out of scope because `.plainweave/plainweave.db` is absent.

Run:
```bash
cd /home/john/scrappack-engine-phase-tasks-1-2
git status --short
stat -c '%s %Y %n' .weft/loomweave/loomweave.db
sha256sum .weft/loomweave/loomweave.db
sqlite3 -readonly .weft/loomweave/loomweave.db "SELECT tag, COUNT(*) FROM entity_tags WHERE tag IN ('entry-point','http-route','exported-api','cli-command','public-surface','framework-handler') GROUP BY tag ORDER BY tag;"
loomweave analyze .
git status --short
stat -c '%s %Y %n' .weft/loomweave/loomweave.db
sha256sum .weft/loomweave/loomweave.db
sqlite3 -readonly .weft/loomweave/loomweave.db "SELECT tag, COUNT(*) FROM entity_tags WHERE tag IN ('entry-point','http-route','exported-api','cli-command','public-surface','framework-handler') GROUP BY tag ORDER BY tag;"
sqlite3 -readonly .weft/loomweave/loomweave.db "SELECT e.id, group_concat(t.tag, ',') FROM entities e JOIN entity_tags t ON t.entity_id=e.id WHERE t.tag IN ('entry-point','http-route','exported-api','cli-command') GROUP BY e.id ORDER BY e.id LIMIT 40;"
plainweave intent coverage --json --max-surfaces 5
```

Expected SQLite output includes non-zero counts for:
```text
cli-command|...
entry-point|...
exported-api|...
http-route|13
```

Expected `plainweave` output today:
```json
{
  "ok": false,
  "error": {
    "code": "NOT_FOUND",
    "message": "Plainweave project is not initialized"
  }
}
```

Record this as out of scope unless the owner initializes Plainweave there. The Loomweave catalog validation still counts for acceptance item 3 because it verifies the source denominator rows that Plainweave would consume.

**Step 5: Preserve baseline and after counts in the report**

In the validation report, include:
- The artifact preflight: resolved `loomweave`, resolved `loomweave-plugin-python`, and the installed Python plugin manifest lines proving ontology `0.11.0`.
- Baseline counts from this plan's prerequisites, plus the immediate before-analysis counts from each validation repo.
- Immediate after-analysis counts from each validation repo.
- Before and after `git status --short` output for each validation repo, to record unrelated tracked changes.
- Before and after `.weft/loomweave/loomweave.db` size/mtime and SHA-256 for each validation repo. `.weft/` is gitignored, so `git status --short` is not catalog-mutation evidence.
- At least three concrete example rows per tag class where available in the corpus.
- New `cli-command` examples from Scrappack and `exported-api` / `http-route` examples from the engine-phase repo where available.
- The exact Plainweave degraded/complete status for `/home/john/scrappack`.
- The explicit out-of-scope reason for Plainweave coverage in `/home/john/scrappack-engine-phase-tasks-1-2` if still uninitialized.

**Step 6: Commit**

```bash
git add docs/implementation/2026-07-10-public-surface-tags-plainweave-validation.md
git commit -m "docs: record Plainweave public-surface validation"
```

**Definition of Done:**
- [ ] Artifact preflight proves validation used `/home/john/loomweave/target/debug/loomweave` and `/home/john/loomweave/plugins/python/.venv/bin/loomweave-plugin-python` with installed ontology `0.11.0`.
- [ ] `/home/john/scrappack` validation was run and recorded.
- [ ] `/home/john/scrappack-engine-phase-tasks-1-2` validation was run, or the exact out-of-scope reason was recorded.
- [ ] Both validation repos have before/after tag counts, before/after `git status --short`, and before/after DB size/mtime/SHA-256 recorded, acknowledging `.weft/loomweave/loomweave.db` mutation despite `.weft/` being gitignored.
- [ ] The report treats corpus-specific absence of a tag class as source reality, not as failure of Loomweave functionality.
- [ ] The report distinguishes Loomweave denominator incompleteness from Plainweave behavior.
- [ ] The report preserves the required attribution sentence.

---

## Task 7: Final Verification Gate

**Files:**
- Test only; no source or docs changes expected unless a gate fails.

**Step 1: Run targeted tests**

Run:
```bash
plugins/python/.venv/bin/pytest --no-cov plugins/python/tests/test_extractor.py plugins/python/tests/test_package.py plugins/python/tests/test_server.py
python scripts/check-python-ontology-version.py --self-test
python scripts/check-python-ontology-version.py
cargo nextest run -p loomweave-mcp categorisation_shortcuts_are_honest_empty public_surface_shortcuts_light_up_when_tags_are_present tools_list_fits_the_context_budget tools_list_exposes_exact_docstrings server_instructions_enumerate_every_tool server_instructions_fit_truncating_clients
cargo nextest run -p loomweave-plugin-rust root_tags
bash tests/e2e/sprint_2_mcp_surface.sh
rg -n "Plainweave|0.11.0|entry-point|http-route|exported-api|cli-command|sees more doors" CHANGELOG.md docs/operator/language-support.md crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md
```

Expected output:
```text
plugins/python ... passed
Python plugin ontology_version matches: 0.11.0
PASS ... loomweave-mcp targeted tests
PASS ... loomweave-plugin-rust root_tags
[mcp-surface] ok
CHANGELOG.md:...
docs/operator/language-support.md:...
crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md:...
```

**Step 2: Run formatting, lint, and broad gates**

Run:
```bash
cargo fmt --all -- --check
python scripts/check-migration-retirement.py --self-test
python scripts/check-migration-retirement.py
python scripts/check-workspace-version-lockstep.py
python scripts/check-pyright-pin-lockstep.py --self-test
python scripts/check-pyright-pin-lockstep.py
python scripts/check-wardline-version-bounds.py --self-test
python scripts/check-wardline-version-bounds.py
python scripts/check-entity-cap-lockstep.py --self-test
python scripts/check-entity-cap-lockstep.py
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
deps="$(cargo tree -p loomweave-core --edges normal --prefix none)"; ! grep -qE '^reqwest v' <<<"$deps"
cargo nextest run --workspace --all-features --no-tests=pass
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
uv sync --project plugins/python --locked --extra dev
uv export --project plugins/python --locked --extra dev --no-emit-project --format requirements.txt --output-file /tmp/loomweave-python-dev-requirements.txt
uv run --project plugins/python --extra dev pip-audit -r /tmp/loomweave-python-dev-requirements.txt
uv run --project plugins/python --extra dev python scripts/check-b4-gate-result.py --run-b5-smoke
uv run --project plugins/python --extra dev ruff check plugins/python
uv run --project plugins/python --extra dev ruff format --check plugins/python
uv run --project plugins/python --extra dev mypy --strict plugins/python
uv run --project plugins/python --extra dev pytest plugins/python
CARGO_BUILD=0 bash tests/e2e/sprint_1_walking_skeleton.sh
CARGO_BUILD=0 bash tests/e2e/wp5_secret_scan.sh
CARGO_BUILD=0 bash tests/e2e/sprint_2_mcp_surface.sh
CARGO_BUILD=0 bash tests/e2e/phase3_subsystems.sh
wardline scan . --fail-on ERROR
```

Expected output:
```text
cargo fmt ... clean
cargo clippy ... clean
cargo build ... finished
cargo nextest ... passed
cargo doc ... clean
cargo deny ... clean
All checks passed!
Success: no issues found
[e2e] sprint_1 / wp5 / sprint_2 / phase3 passed
wardline: no ERROR findings
```

If `wardline scan . --fail-on ERROR` exits 1, run the Wardline explain/fix/rescan loop and fix findings at the boundary, not at the sink. If it exits 2, treat that as a Wardline tool/configuration error and resolve it before handoff; do not record it as a clean scan or waive it silently.

**Step 3: Check working tree scope**

Run:
```bash
git status --short
```

Expected output should include only the intended commits' clean state, or known pre-existing unrelated changes such as:
```text
 M packaging/rust-plugin-dist/Cargo.lock
```

Do not stage or revert unrelated pre-existing changes.

**Definition of Done:**
- [ ] Python targeted tests, full Python suite, Rust, MCP, E2E, lockstep, dependency-audit, doc/deny, and Wardline gates pass.
- [ ] The final gate re-checks the public-facing doc copy after all implementation and validation edits.
- [ ] Validation evidence exists and references both target repos or explains the engine Plainweave out-of-scope condition.
- [ ] Final handoff states that a lower Plainweave ratio after this change is an accuracy improvement when the denominator expands.
- [ ] No unrelated dirty worktree changes were staged, committed, or reverted.
