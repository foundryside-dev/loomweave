# WP3 — Python Plugin v0.1 Baseline (Sprint 1)

**Status**: DRAFT — blocked-by WP2
**Anchoring design**: [detailed-design.md §1 (Plugin implementation — Python specifics)](../../clarion/v0.1/detailed-design.md#1-plugin-implementation-detail), [system-design.md §2](../../clarion/v0.1/system-design.md#2-core--plugin-architecture)
**Accepted ADRs**: [ADR-018](../../clarion/adr/ADR-018-identity-reconciliation.md), [ADR-022](../../clarion/adr/ADR-022-core-plugin-ontology.md), [ADR-023](../../clarion/adr/ADR-023-tooling-baseline.md)
**Predecessor**: [WP2](./wp2-plugin-host.md).
**Blocks**: the Sprint 1 walking-skeleton demo.

---

## 1. Scope (Sprint 1 narrow)

WP3 in Sprint 1 ships the smallest real Python plugin that makes the walking skeleton
pass. It extracts **functions only** (module-level and class-method), produces
canonical names that match WP1's L2 format exactly, speaks WP2's JSON-RPC protocol,
and makes one real pass at the Wardline `REGISTRY` integration surface (even if the
pass is a graceful-no-op probe).

Feature-completeness for the Python plugin — every entity kind, every edge, every
`CLA-PY-*` rule — is **not** Sprint 1. That's the WP3-feature-complete sprint,
reached after WP4 exists and can consume richer plugin output.

**In scope for Sprint 1:**

- Python package `clarion-plugin-python` installable via `pip install -e
  plugins/python/` from the monorepo.
- `plugin.toml` manifest matching WP2's L5 schema.
- `clarion-plugin-python` executable (entry point) on `$PATH` after install.
- JSON-RPC server speaking WP2's L4 method set: `initialize`, `initialized`,
  `analyze_file`, `shutdown`, `exit`.
- `ast`-based function extraction: for a `.py` file, emit one entity per
  module-level function and per class method, each with:
  - `kind: "function"` (declared in `[ontology].entity_kinds` per ADR-022).
  - `id` — the 3-segment `EntityId` (L2) = `python:function:{qualified_name}`
    where `{qualified_name}` is the dotted module-relative name produced per
    L7. Format matches WP1's Rust `entity_id()` byte-for-byte via the shared
    fixture (Task 5).
  - `qualified_name` — the language-native dotted name per L7 (Python-side
    producer of the `canonical_qualified_name` segment).
  - `module_path` — the source-relative file path (entity **property**, not
    part of the ID per ADR-003).
  - `source_range` — line/column start and end.
- L7 qualname production format (see §2).
- L8 Wardline `REGISTRY` direct-import probe — attempt import, capture version,
  log outcome, continue regardless (graceful no-op if Wardline absent; see
  UQ-WP3-03 for the "fully wire" vs "probe-only" decision).
- `sys.stdout` discipline: plugin initialisation swaps `sys.stdout` for a
  JSON-RPC-only writer to prevent stray `print()` from corrupting framing
  (WP2 UQ-WP2-08 resolution).
- Round-trip self-test: plugin runs against its own source tree and asserts
  specific expected entities appear.

**Explicitly out of scope for Sprint 1:**

- Classes, decorators, module entities — deferred to WP3 feature-complete. _Sprint 2 / B.2 realises the **classes + module entities** subset; see [`../sprint-2/b2-class-module-entities.md`](../sprint-2/b2-class-module-entities.md). Decorators and the remaining kinds remain deferred to later WP3-feature-complete sprints._
- Imports edge extraction — deferred.
- Calls edge extraction — deferred.
- All edge kinds — deferred. _Sprint 2 / B.3 realises the first edge kind (**contains**), the dual-encoded `parent_id` field, the per-kind source-range contract, and the writer-actor's parent/contains consistency check; see [`../sprint-2/b3-contains-edges.md`](../sprint-2/b3-contains-edges.md) and the ADRs it locks (ADR-026, ADR-027). Calls/imports/decorates/inherits_from remain deferred to later WP3-feature-complete sprints._
- Dynamic imports (`importlib`, `__import__`) — deferred; deliberately not solved
  in v0.1 per the out-of-scope list in `../v0.1-plan.md`.
- All `CLA-PY-*` findings — WP4 consumes, WP3-feature-complete emits.
- Type-inference, dataflow, taint — NG-05, never in scope for Clarion.
- Multi-file incremental analysis — WP6/WP7 territory.

## 2. Lock-in callouts

### L7 — Python qualified-name production format

**What locks**: the exact function producing the `canonical_qualified_name`
segment (the third segment of L2's 3-segment `EntityId`) per ADR-003. The
plugin prepends `python:function:` to form the full ID; WP1's Rust
`entity_id()` must produce byte-identical output from the same inputs (proven
via the shared fixture — Task 5).

**Format** (Sprint 1 subset — only functions; extend in later sprints as more kinds
ship):

- Module-level function `def hello(): ...` in `demo.py` → qualified name
  `demo.hello` → full `EntityId` `python:function:demo.hello`.
- Nested function `def outer(): def inner(): ...` in `demo.py` → qualified
  name `demo.outer.<locals>.inner` (matches Python's own `__qualname__`
  combined with the module dotted path).
- Class method `class Foo: def bar(self): ...` in `demo.py` → qualified name
  `demo.Foo.bar`.
- Async function → same as sync; the `async` keyword does not change the name.
- Lambdas → `<lambda>` — but Sprint 1 skips lambdas; they're deferred.

The rule "module dotted path + Python's own `__qualname__` semantics, joined
by `.`" is the spec. Sprint 1's implementation reconstructs `__qualname__`
from the AST (Python's `__qualname__` is only available at runtime
post-definition, which this plugin's static analyser isn't doing). Tests
assert against hand-written expectations derived from Python's documented
`__qualname__` rules. Module dotted path is derived from the root-relative
`module_path` via `detailed-design.md §1` Python normalisation (including
`src/` prefix stripping; UQ-WP3-05).

**Why now**: `↗` ADR-018 — Wardline stores qualnames produced by the same rule
and uses this segment (not the full 3-segment `EntityId`) as its Clarion-side
join key. Divergence means Filigree's triage state can't join Clarion's
entities to Wardline's annotations. This is the single most important
cross-product alignment in Sprint 1.

**Downstream impact**:
- WP1's L2 `entity_id()` concatenates `python`, `function`, and this
  segment. Shared fixture (Task 5) is the byte-for-byte parity proof.
- `↗` Wardline must produce the same qualnames. If Wardline's rule diverges, either
  Wardline changes or Clarion adds translation. ADR-018 names direct-production as
  the default; WP3's tests are the executable spec for what "match Wardline" means.

**Mitigation if Wardline diverges**: discovered via the first real cross-check (either
a Sprint 2 WP9 test against real Wardline annotations, or a manual spot-check during
this WP). Response is documented in ADR-018 — the translator route — not a WP3
change.

**Divergence found at Sprint 1 close (2026-04-28)**: Wardline's
`FingerprintEntry` (`wardline/src/wardline/manifest/models.py:86-97`)
stores `(module: str, qualified_name: str)` as **separate fields** —
`module` is the source file path (e.g. `demo.py`) and `qualified_name`
is Python's bare `__qualname__` (e.g. `Foo.bar`). Clarion's L7 emits a
single combined `{dotted_module}.{__qualname__}` string. The two
encodings carry the same information but are not byte-equal — joining
requires a translator that composes
`f"{module_dotted_name(wardline.module)}.{wardline.qualified_name}"` on
the Wardline side using Clarion's `module_dotted_name` rules. Sprint 1
does not exercise the join (the L8 probe verifies presence + version
only), so no Sprint-1 code path is broken. Tracked in
**`clarion-889200006a`** for ADR-018 amendment when WP9 attempts the
first real join.

### L8 — Wardline `REGISTRY` import + version-pin protocol

**What locks**: the import path (`from wardline.core.registry import REGISTRY`) and
the version-pin syntax used in the plugin's `plugin.toml` (or a dedicated
`wardline_compat` field).

**Symbol verification** (re-checked at Sprint 1 close 2026-04-28): both symbols
remain present in the Wardline source and the in-range probe returns
`enabled` against `pip install -e /home/john/wardline`:

- `wardline.core.registry.REGISTRY` — declared at
  `wardline/src/wardline/core/registry.py:55` as a `MappingProxyType[str, RegistryEntry]`.
- `wardline.__version__` — re-exported from `wardline/src/wardline/__init__.py:3`
  (sourced from `wardline._version`); current value `1.0.0`.

UQ-WP3-03 resolves to "fully wire" (no stub-only fallback).

**Sprint 1 pin approach**:

- Manifest field: `[integrations.wardline]` section with `min_version = "1.0.0"`
  and `max_version = "2.0.0"` (semver half-open range). Updated from the
  pre-sprint placeholder `0.1.0`/`0.2.0` to admit the actual current
  Wardline 1.x; 2.0.0 is exclusive so a future major bump triggers an
  explicit re-pin rather than silent drift.
- Plugin startup probe:
  1. Attempt `import wardline.core.registry`. If `ImportError`, record `wardline
     absent` in the handshake response's `capabilities` field and proceed.
  2. If import succeeds, read `wardline.__version__`. If outside declared range,
     record `wardline version out of range` and proceed with the integration
     disabled (no hard fail — WP3 still emits entities, just without the
     REGISTRY cross-check).
  3. If in range, capture `REGISTRY` for later use. Sprint 1 does not actually
     call into `REGISTRY` for any entity — that's WP3-feature-complete work.
     Sprint 1 only proves the import + version-pin handshake works end-to-end.

**Why now**: the *protocol* is what locks. Once the plugin ships to any user who
also has Wardline installed, changing the probe shape or the version-pin syntax is
visible behaviour. The `REGISTRY` symbol itself is already named in ADR-018;
WP3 commits the plugin's consumption pattern.

**Cross-product implication**: `↗` **Wardline should not rename
`wardline.core.registry.REGISTRY` or drop `wardline.__version__` without a
coordinated Clarion-side release**. ADR-018 already states this; WP3 makes it a
real dependency. Same-author note: this is within-scope Wardline discipline, not
cross-team coordination — but the constraint is real.

**Sprint 1 decision** (resolved — see UQ-WP3-03 in §5): fully wire the
import. Symbol existence was verified pre-sprint
(`wardline/src/wardline/core/registry.py:55`,
`wardline/src/wardline/__init__.py:3`), so the probe runs against a real
`pip install wardline` in the dev venv rather than stubbing behind a
`CLARION_WARDLINE_ENABLED` env var. This locks L8 completely — the
exercised lock-in is the honest one.

## 3. File decomposition

```
/plugins/python/
  pyproject.toml                  # package metadata, entry-point, [tool.ruff], [tool.mypy], [tool.pytest]
  plugin.toml                     # L5 manifest
  .pre-commit-config.yaml         # ADR-023: ruff-check, ruff-format, mypy hooks
  README.md                       # install + dev notes
  src/
    clarion_plugin_python/
      __init__.py
      py.typed                    # PEP 561 marker so downstream mypy picks up stubs
      __main__.py                 # entry point; runs the JSON-RPC server loop
      server.py                   # JSON-RPC framing + dispatch
      extractor.py                # ast visitor producing entities (L7)
      qualname.py                 # __qualname__ reconstruction (L7)
      entity_id.py                # L2 3-segment EntityId assembler matching WP1
      wardline_probe.py           # L8 import probe
      stdout_guard.py             # sys.stdout discipline (WP2 UQ-WP2-08)
  tests/
    test_qualname.py              # matches L7 __qualname__ expectations
    test_entity_id.py             # matches WP1 shared fixture byte-for-byte
    test_extractor.py             # ast → entities
    test_server.py                # JSON-RPC framing + dispatch
    test_wardline_probe.py        # probe behaviour with and without wardline
    test_round_trip.py            # plugin runs against its own source
    fixtures/                     # .py files with expected-entity YAML
```

The plugin is its own pip-installable package — deliberately separate from the Rust
workspace. This is Sprint 1's commitment to "plugins are independent artefacts,
not core-vendored code" per ADR-022.

## 4. External dependencies being locked

### Cross-product dependency locked by Sprint 1

- **Wardline** — import path `wardline.core.registry.REGISTRY` and version attribute
  `wardline.__version__`. See L8.

### Python-side package dependencies

Minimal. `pyproject.toml` declares:

- `python_requires = ">=3.11"` (UQ-WP3-04 — resolved: 3.11).
- No runtime deps beyond the standard library for Sprint 1. `ast`, `json`, `sys`,
  `os`, `pathlib` are all stdlib. Task 6 adds `packaging` for Wardline version
  comparisons.
- Dev deps (per ADR-023 tooling baseline): `pytest`, `pytest-cov`, `ruff`
  (lint + format; strict config), **`mypy`** (`--strict` from day 1), and
  `pre-commit` (hooks for ruff-check, ruff-format, mypy). All wired into CI
  via a separate GitHub Actions job that installs the plugin editable and
  runs `ruff check`, `ruff format --check`, `mypy --strict`, and `pytest`.
- Optional dep: `wardline` (declared in `[project.optional-dependencies] integrations`).
  The plugin works without Wardline; declaring it optional allows `pip install
  clarion-plugin-python[integrations]` to pull Wardline when desired.

## 5. Unresolved questions

- **UQ-WP3-01** — **Qualname for nested class methods**: ~~open~~ —
  **resolved as "follow ``__qualname__`` exactly"**. `class A: class B: def c():`
  produces `A.B.c` (class parents chain with `.`, no `<locals>` marker).
  `qualname.reconstruct_qualname` tests cover this directly
  (`test_nested_class_method_chains_class_names`) plus the harder
  class-in-function-in-class case (`Foo.bar.<locals>.Local.meth`) where
  `<locals>` appears once, only at the function-parent boundary.
  **Resolved**: Task 3 / `plugin.qualname`.
- **UQ-WP3-02** — **Syntax-error handling**: ~~open~~ — **resolved as
  "skip + stderr log"** per the original proposal. `extract()` catches
  `SyntaxError` from `ast.parse`, writes one line to `sys.stderr`
  (`clarion-plugin-python: skipping <path>: syntax error at line N: <msg>`),
  and returns `[]`. The run continues; WP4 may later attach a finding.
  `test_syntax_error_yields_empty_list_and_logs_to_stderr` is the
  discriminating test. **Resolved**: Task 4 / `plugin.extractor`.
- **UQ-WP3-03** — **Fully wire Wardline import in Sprint 1 or stub?** ~~open~~
  — **resolved as "fully wire"**. Pre-sprint symbol check (see L8) confirmed
  `wardline.core.registry.REGISTRY` and `wardline.__version__` exist in the
  current Wardline source (`src/wardline/core/registry.py:55` and
  `src/wardline/__init__.py:3`). The probe can run against a real
  `pip install wardline` in the dev venv, which is the honest lock-in.
  **Resolved**: Task 6.
- **UQ-WP3-04** — **Minimum Python version**: **Resolved — 3.11**. Picked
  for `ast.unparse` availability and better error messages; 3.12 raises the
  install barrier without a Sprint 1 payoff. Clarion users are developers
  with reasonable Python versions available. **Resolved**: Task 1.
- **UQ-WP3-05** — **Module-path normalisation**: ~~open~~ — **resolved
  as plugin-side relativisation** (diverges from original proposal). The
  host sends absolute paths (WP2's CLI canonicalises `project_root`
  and walks via `entry.path()` — see
  `crates/clarion-cli/src/analyze.rs`), so the plugin captures
  `project_root` from the `initialize` handshake and relativises
  incoming `file_path` values against it when deriving the dotted-module
  prefix for `qualified_name`. `source.file_path` emitted on the wire
  stays absolute so the host's path jail canonicalise-and-compare works.
  `extract(source, file_path, *, module_prefix_path=...)` decouples the
  two paths. **Resolved**: Task 7 / `plugin.server._resolve_module_path`.
- **UQ-WP3-06** — **`__init__.py` handling**: ~~open~~ — **resolved as
  "collapse to package name for dotted prefix; keep literal file_path"**.
  `pkg/__init__.py` produces `module_dotted_name == "pkg"` (not
  `pkg.__init__`), so entities emit `qualified_name = "pkg.package_helper"`.
  `source.file_path` stays as the literal `pkg/__init__.py` — the file
  is unambiguous even when the module name collapses. `test_init_py_
  collapsed_to_package_name` is the discriminating test. **Resolved**:
  Task 4 / `plugin.extractor.module_dotted_name`.
- **UQ-WP3-07** — **`typing.overload` / protocol methods**: ~~open~~ —
  **resolved as "regular function entities"**. Overloaded methods are
  `FunctionDef`s with a decorator list — the extractor emits each one
  as a separate entity with the same `qualified_name`, matching Python's
  own `__qualname__` behaviour. A future `CLA-PY-OVERLOAD` rule can add
  semantic annotation in a later sprint. `test_overloaded_method_gets_
  regular_qualname` covers three overloads + the implementation.
  **Resolved**: Task 3 / `plugin.qualname`.
- **UQ-WP3-08** — **Byte-for-byte `EntityId` parity strategy**: ~~open~~ —
  **resolved as "shared JSON fixture file"**. `fixtures/entity_id.json`
  at the repo root has 20 rows covering module-level functions, class
  methods, `<locals>`-marked nested functions, core file/subsystem
  entities, and hypothetical go/rust plugin IDs. Both
  `crates/clarion-core/src/entity_id.rs::tests::shared_fixture_byte_for_byte_parity`
  and `plugins/python/tests/test_entity_id.py::test_matches_shared_fixture`
  consume the same file and assert byte-equal output; divergence fails
  CI on both sides in lockstep. **Resolved**: Task 5 / `fixtures/entity_id.json`.
- **UQ-WP3-09** — **Plugin logging destination**: ~~open~~ — **resolved
  as "stderr for diagnostics"**. `extractor` writes syntax-error and
  read-error messages to `sys.stderr` via `sys.stderr.write`. The host
  captures stderr into a bounded 64 KiB ring buffer (WP2 scrub commit
  `b3c91a7`, resolving UQ-WP2-07); diagnostics are surfaced via
  `host.stderr_tail()`. `.clarion/logs/` as a persistent log destination
  is a Sprint 2+ decision. **Resolved**: Task 2 + Task 4 / `plugin.server`,
  `plugin.extractor`.
- **UQ-WP3-10** — **Testing + tooling infrastructure**: ~~"pytest + ruff;
  mypy adoption deferred until the plugin grows enough to benefit."~~ —
  **reopened 2026-04-18 and re-resolved by
  [ADR-023](../../clarion/adr/ADR-023-tooling-baseline.md)**. The deferred
  framing was the canonical tell for unexamined tech debt: every Python
  module written without mypy would be a module to retrofit later. ADR-023
  adopts `pytest`, `ruff` (strict `select = ["ALL"]` config minus pragmatic
  excludes), **`mypy --strict` from day 1**, and **`pre-commit`** wiring
  ruff-check + ruff-format + mypy into every `git commit`. CI runs the same
  four gates as a separate job. **Resolved**: Task 1.
- **UQ-WP3-11** — **Empty `.py` file response**: ~~open~~ — **resolved
  as "empty entities array"**. `extract("", ...)` returns `[]`. The host
  accepts an empty array without tripping any cap or alert.
  `test_empty_file_yields_zero_entities` and
  `test_whitespace_only_file_yields_zero_entities` cover the edge cases.
  **Resolved**: Task 4 / `plugin.extractor`.
- **UQ-WP3-12** — **`initialize` response identity**: ~~open~~ —
  **resolved as "match the manifest exactly"**. The handshake returns
  `{name: "clarion-plugin-python", version: "0.1.0", ontology_version:
  "0.1.0", capabilities: {...}}` — every field populated from the
  package `__version__` + the `ONTOLOGY_VERSION` module constant in
  `plugin.server`. Cross-check against manifest happens on the host side
  (WP2 scrub commit `1ac32b1` validates `ontology_version` non-empty).
  `test_initialize_roundtrip` is the discriminating test. **Resolved**:
  Task 2 / `plugin.server.handle_initialize`.

## 6. Task ledger

### Task 1 — Python package skeleton + ADR-023 tooling baseline

**Files**:
- Create `/plugins/python/pyproject.toml` (package metadata + `[tool.ruff]` strict config + `[tool.mypy]` `strict = true` + `[tool.pytest.ini_options]`)
- Create `/plugins/python/.pre-commit-config.yaml` (ruff-check, ruff-format, mypy hooks)
- Create `/plugins/python/src/clarion_plugin_python/__init__.py`
- Create `/plugins/python/src/clarion_plugin_python/py.typed` (PEP 561 marker)
- Create `/plugins/python/src/clarion_plugin_python/__main__.py`
- Create `/plugins/python/README.md`
- Create `/plugins/python/tests/__init__.py`
- Extend `/.github/workflows/ci.yml` with a `python-plugin` job running ruff + mypy + pytest

Steps:

- [ ] Write `pyproject.toml` with `project.name = "clarion-plugin-python"`, `requires-python = ">=3.11"` (UQ-WP3-04), `project.scripts.clarion-plugin-python = "clarion_plugin_python.__main__:main"`, no runtime deps, dev deps `pytest`, `pytest-cov`, `ruff`, `mypy`, `pre-commit` (ADR-023).
- [ ] Configure `[tool.ruff]` with `target-version = "py311"`, `line-length = 100`, `select = ["ALL"]`, pragmatic excludes per ADR-023 (`D` docstring lints relaxed; `COM812`/`ISC001` to avoid format conflict; per-file-ignores for `tests/` and fixtures). `[tool.ruff.format]` matches defaults.
- [ ] Configure `[tool.mypy]` with `strict = true`, `python_version = "3.11"`, `warn_unused_configs = true`. Add `[[tool.mypy.overrides]]` entries for any third-party modules without stubs (Sprint 1: none yet; Task 6 may add `packaging` once it's pulled in).
- [ ] Configure `[tool.pytest.ini_options]` with `testpaths = ["tests"]`, `addopts = "--strict-markers --cov=clarion_plugin_python --cov-report=term-missing"`.
- [ ] Write `.pre-commit-config.yaml` with hooks for `ruff check --fix`, `ruff format`, and `mypy` (using `additional_dependencies` to install stubs mypy needs inside the hook env).
- [ ] Write `py.typed` as an empty file — PEP 561 marker making the package's own type hints visible to downstream mypy consumers.
- [ ] Write `__main__.py` with a typed `def main() -> int:` that writes `clarion-plugin-python 0.1.0\n` to `sys.stderr` and returns 0 (so `pip install -e .` produces a verifiable binary with full type coverage).
- [ ] `pip install -e plugins/python[dev]` (dev extras) and verify locally:
  - `which clarion-plugin-python` returns a path and running it exits 0.
  - `ruff check plugins/python` passes.
  - `ruff format --check plugins/python` passes.
  - `mypy --strict plugins/python` passes (Sprint 1's tiny surface makes this trivial; the discipline is set for every subsequent task).
  - `pytest plugins/python` passes (no tests yet — an empty test discovery returning "no tests ran" is the expected Task-1 shape).
- [ ] `pre-commit install` and `pre-commit run --all-files` passes.
- [ ] Extend `.github/workflows/ci.yml` with a `python-plugin` job that installs Python 3.11, runs `pip install -e plugins/python[dev]`, and executes the same four gates (`ruff check`, `ruff format --check`, `mypy --strict`, `pytest`).
- [ ] Commit: `feat(wp3): Python plugin package skeleton + ADR-023 tooling baseline`.

### Task 2 — JSON-RPC server loop + stdout discipline

**Files**:
- Create `/plugins/python/src/clarion_plugin_python/stdout_guard.py`
- Create `/plugins/python/src/clarion_plugin_python/server.py`
- Modify `/plugins/python/src/clarion_plugin_python/__main__.py`

Steps:

- [ ] In `stdout_guard.py`: on import, replace `sys.stdout` with an object whose `.buffer` is the real stdout bytes stream and whose `.write` raises. Provide a context manager `jsonrpc_output()` that yields the real bytes stream. This enforces WP2 UQ-WP2-08.
- [ ] In `server.py`: implement Content-Length frame read/write from `sys.stdin.buffer` / the real stdout bytes stream. Implement a dispatch loop handling `initialize`, `initialized`, `analyze_file`, `shutdown`, `exit` by method name; each dispatches to a handler function.
- [ ] Handlers for `initialize` (returns `{"name": "clarion-plugin-python", "version": "0.1.0", "ontology_version": "0.1.0"}`, UQ-WP3-12) and `shutdown` (returns `null`, then the next loop iteration exits on `exit`). `analyze_file` returns `{"entities": []}` for now; filled in later tasks.
- [ ] Failing integration test in `test_server.py`: spin up the server in a subprocess, send an `initialize` frame, receive a response with the expected shape.
- [ ] Implement and verify. Commit: `feat(wp3): L4-compatible JSON-RPC server + stdout guard`.

### Task 3 — Qualname reconstruction (L7)

**Files**:
- Create `/plugins/python/src/clarion_plugin_python/qualname.py`
- Create `/plugins/python/tests/test_qualname.py`

Steps:

- [ ] Failing tests in `test_qualname.py`: module-level function, nested function (`outer.<locals>.inner`), class method (`Foo.bar`), nested class method (`Outer.Inner.method`), async function (same as sync). Include UQ-WP3-01 and UQ-WP3-07 cases.
- [ ] Implement `qualname.py`: `def reconstruct(node: ast.AST, parents: list[ast.AST]) -> str` — walks the parent chain; adds `.<locals>.` between function-parent and child; joins with `.` for class-parent.
- [ ] Run tests; expect pass.
- [ ] Commit: `feat(wp3): L7 qualname reconstruction matching __qualname__ semantics`.

### Task 4 — Extractor (ast → entities)

**Files**:
- Create `/plugins/python/src/clarion_plugin_python/extractor.py`
- Create `/plugins/python/src/clarion_plugin_python/entity_id.py`
- Create `/plugins/python/tests/test_extractor.py`
- Create `/plugins/python/tests/fixtures/` with sample `.py` files + expected-entity YAML

Steps:

- [ ] In `entity_id.py`, implement the 3-segment assembler per ADR-003: `def entity_id(plugin_id: str, kind: str, canonical_qualified_name: str) -> str`. Sprint 1's byte-for-byte match with WP1's Rust is covered by Task 5's shared fixture.
- [ ] In `extractor.py`, `def extract(source: str, module_path: str) -> list[dict]`:
  - `ast.parse(source)`; on `SyntaxError`, log to stderr and return `[]` (UQ-WP3-02).
  - Walk the tree; for each `ast.FunctionDef` / `ast.AsyncFunctionDef`, emit one entity.
  - Each entity: `{id: "python:function:...", kind: "function", qualified_name: ..., module_path: ..., source_range: {start_line, start_col, end_line, end_col}}`. `id` is built by `entity_id("python", "function", qualified_name)`.
- [ ] Failing tests: fixture `empty.py` → 0 entities (UQ-WP3-11); fixture `simple.py` with module-level `def hello()` → one entity with `id == "python:function:simple.hello"`; fixture `nested.py` with class methods and nested functions; fixture `syntax_error.py` → 0 entities + stderr log.
- [ ] Implement; run; expect pass.
- [ ] Commit: `feat(wp3): function extractor with L2 EntityId production`.

### Task 5 — Shared `EntityId` fixture (WP1 parity)

**Files**:
- Create `/fixtures/entity_id.json` at repo root
- Modify `/crates/clarion-core/src/entity_id.rs` tests to consume it
- Modify `/plugins/python/tests/test_entity_id.py` to consume it

Steps:

- [ ] Write the fixture: JSON array of objects each with `plugin_id`, `kind`, `canonical_qualified_name`, and `expected_entity_id` fields. At least 20 rows covering the representative cases from ADR-003 (both plugin-emitted `python:function:*` rows and core-reserved `core:file:*`/`core:subsystem:*` rows).
- [ ] Add a test in `clarion-core` that loads the fixture at test time, runs `entity_id()` for each row, asserts equality. Same-shaped test in `plugins/python` (the Python side concatenates its three segments; assertion is on the final string).
- [ ] Run both test suites; expect pass in both.
- [ ] Commit: `test(wp3): shared EntityId fixture (UQ-WP3-08 resolution)`.

### Task 6 — Wardline probe (L8)

**Files**:
- Create `/plugins/python/src/clarion_plugin_python/wardline_probe.py`
- Create `/plugins/python/tests/test_wardline_probe.py`

Steps:

- [ ] Failing tests:
  - `test_probe_absent`: stub `sys.modules` to raise `ImportError` for `wardline.core.registry`; probe returns `{"status": "absent"}`.
  - `test_probe_present_in_range`: stub to return a module with `__version__ = "0.1.5"` and a `REGISTRY` attr; probe returns `{"status": "enabled", "version": "0.1.5"}`.
  - `test_probe_out_of_range`: stub to return `__version__ = "0.3.0"` (above `max_version`); probe returns `{"status": "version_out_of_range", "version": "0.3.0"}`.
- [ ] Implement `probe(min_version: str, max_version: str) -> dict` using `importlib.import_module` in a `try/except ImportError`. Use `packaging.version.Version` for comparison (add `packaging` as a dep — first runtime dep; accepted because the alternative is hand-parsing semver).
- [ ] Wire probe result into the `initialize` handshake response's `capabilities` field.
- [ ] Run; expect pass.
- [ ] Commit: `feat(wp3): L8 Wardline REGISTRY probe with version pinning`.

### Task 7 — `plugin.toml` manifest + `analyze_file` end-to-end

**Files**:
- Create `/plugins/python/plugin.toml`
- Modify `/plugins/python/src/clarion_plugin_python/server.py` `analyze_file` handler

Steps:

- [ ] Write `plugin.toml` matching WP2 L5 schema: `[plugin]` (name, `plugin_id = "python"`, version, protocol_version, executable, `language = "python"`, `extensions = ["py"]`), `[capabilities.runtime]` per ADR-021 §Layer 1 (`expected_max_rss_mb = 512`, `expected_entities_per_file = 5000`, `wardline_aware = true`, `reads_outside_project_root = false`), `[ontology]` (kinds = `["function"]`, edge_kinds = `[]`, `rule_id_prefix = "CLA-PY-"`, `ontology_version = "0.1.0"`), `[integrations.wardline]` (`min_version = "0.1.0"`, `max_version = "0.2.0"`). The Wardline-specific values in `[integrations.wardline]` flow from the `wardline_aware = true` declaration.
- [ ] Arrange installation to place `plugin.toml` where WP2's discovery (L9) finds it: at install-prefix `share/clarion/plugins/clarion-plugin-python/plugin.toml`. Using `tool.setuptools` or `hatch` data-file declarations in `pyproject.toml`. Verify after `pip install -e .` the file is discoverable.
- [ ] Modify `analyze_file` handler: read the requested path, run `extractor.extract()`, return `{"entities": [...]}`.
- [ ] Commit: `feat(wp3): plugin.toml manifest + analyze_file wired to extractor`.

### Task 8 — Round-trip self-test

**Files**:
- Create `/plugins/python/tests/test_round_trip.py`

Steps:

- [ ] Test: spawn the installed plugin as a subprocess; complete handshake; call `analyze_file` on `plugins/python/src/clarion_plugin_python/extractor.py`; assert the returned entities include specific expected ones (the `extract` function itself, etc.); shutdown cleanly.
- [ ] Commit: `test(wp3): round-trip self-test against plugin's own source`.

### Task 9 — Sprint 1 walking-skeleton end-to-end

**Files**:
- Create `/tests/e2e/sprint_1_walking_skeleton.sh` (shell integration test)

Steps:

- [ ] Write the test as a shell script or `bats` test that runs the [README §3 demo script](./README.md#3-demo-script-sprint-1-close-proof) commands in sequence, asserting each step's expected output.
- [ ] Run locally end-to-end.
- [ ] Wire into CI (if CI exists; if not, document the manual run).
- [ ] Commit: `test(sprint-1): walking-skeleton end-to-end demo script`.

## 7. ADR triggers

None in Sprint 1. ADR-018 and ADR-022 are already Accepted. **However**, WP3 stress-
tests the L8 version-pin protocol; if the probe's shape turns out to be insufficient
(e.g., semver-range is the wrong abstraction), a ADR-018 amendment may be needed —
not a new ADR.

## 8. Exit criteria

WP3 is done for Sprint 1 when all of:

- The walking-skeleton demo script (README §3) passes end-to-end on a clean machine.
- L7 (qualname) and L8 (Wardline probe) each have passing positive and negative tests.
- The shared `EntityId` fixture (`fixtures/entity_id.json`) passes in both the
  Rust (`clarion-core::entity_id`) and Python (`test_entity_id.py`) test suites.
- Round-trip self-test passes.
- Every UQ-WP3-* is marked resolved in §5.
- `pip install -e plugins/python[dev]` works on a clean Python 3.11 venv and
  `clarion-plugin-python` is on `$PATH`.
- **ADR-023 gates green** (all four): `ruff check plugins/python`,
  `ruff format --check plugins/python`, `mypy --strict plugins/python`, and
  `pytest plugins/python` all pass on the WP3 closing commit.
- **`pre-commit run --all-files` passes** on the WP3 closing commit.
- **GitHub Actions `python-plugin` job green** on the WP3 PR.

See also [`signoffs.md` Tier A](./signoffs.md#tier-a--sprint-1-close-walking-skeleton).
