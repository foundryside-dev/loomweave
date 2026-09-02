# loomweave-plugin-python

The Python language plugin for [Loomweave](../../README.md). Extracts Python
entities from source files and serves them to the Loomweave core over the
JSON-RPC protocol defined in [WP2 L4](../../docs/implementation/sprint-1/wp2-plugin-host.md#l4--json-rpc-method-set--content-length-framing).

**Status**: Python structural extractor. It emits modules, classes, functions,
`contains`, `calls`, `references`, `imports`, and versioned entity signatures
for Stable Entity Identity (SEI) matching. It also reads Wardline's NG-25
trust-vocabulary descriptor without importing Wardline and emits source-observed
Wardline decorator metadata/tags on decorated entities when a descriptor is
available.

## Install (development)

```bash
python -m venv .venv
source .venv/bin/activate
pip install -e '.[dev]'
```

This places `loomweave-plugin-python` on your `$PATH` and installs the
dev-time toolchain (`ruff`, `mypy`, `pytest`, `pytest-cov`, `pre-commit`).

## ADR-023 tooling gates

Every commit must pass all four:

```bash
ruff check plugins/python
ruff format --check plugins/python
mypy --strict plugins/python
pytest plugins/python
```

CI runs the same four gates in the `python-plugin` job.

## Interpreter discovery

`pyright-langserver` type-checks against whatever `python` it finds — the
plugin points it at the project's own interpreter deterministically, rather
than trusting whatever happened to be first on the launching process's
`PATH` (clarion-5cf9643de9). `discover_project_interpreter`
(`src/loomweave_plugin_python/interpreter.py`) walks a fixed order and stops
at the first usable candidate — a **regular** file (`Path.is_file()`) that
passes `os.access(path, os.X_OK)`:

| Rung | Source | Pinned? |
|---|---|---|
| 1 | `LOOMWEAVE_PYTHON_INTERPRETER` env var names an executable file | yes |
| 2 | `<project_root>/.venv/bin/python` — skipped when the file is tracked by the repository | yes |
| 3 | `$VIRTUAL_ENV/bin/python` | yes |
| 4 | `$CONDA_PREFIX/bin/python` | yes |
| 5 | first `python` / `python3` on `PATH` | no |
| 6 | nothing found | no |

An empty env value counts as unset at every rung. The returned path is
absolute and lexically normalised but **never symlink-resolved** — a venv's
`bin/python` is typically a symlink to the base interpreter, and pyright
needs the symlink path to stay inside the venv's `site-packages`.

This order is a cross-language contract shared with the Rust host
(`crates/loomweave-core/src/plugin/interpreter.rs`); the host runs the same
discovery and, for a `[capabilities.runtime.pyright]` plugin, exports its
pinned answer to the child as `LOOMWEAVE_PYTHON_INTERPRETER` (unless the
operator already set it). The plugin's own discovery therefore sees the
host's choice at rung 1 whenever the host found one.

When discovery lands on rung 5 or 6 (no project-owned interpreter), an
otherwise-`complete` calls/references facet is honestly demoted to
`degraded` with reason `interpreter_unpinned` — cross-module call/reference
targets may be missing because pyright resolved against a guessed
interpreter, even though nothing else went wrong. Fix it by creating
`.venv` in the project root or setting `LOOMWEAVE_PYTHON_INTERPRETER` to the
project's interpreter. See
[ADR-058](../../docs/loomweave/adr/ADR-058-project-interpreter-discovery.md)
for the full design and the `interpreter_unpinned` coverage semantics.

## Design references

- [WP3 plan](../../docs/implementation/sprint-1/wp3-python-plugin.md) — task
  ledger, lock-ins, and UQ resolutions.
- [ADR-003](../../docs/loomweave/adr/ADR-003-entity-id-scheme.md) — 3-segment
  `EntityId` format this plugin produces.
- [ADR-018](../../docs/loomweave/adr/ADR-018-identity-reconciliation.md) —
  cross-product identity join with Wardline.
- [ADR-022](../../docs/loomweave/adr/ADR-022-core-plugin-ontology.md) —
  manifest schema and ontology-boundary enforcement.
- [ADR-023](../../docs/loomweave/adr/ADR-023-tooling-baseline.md) — the four
  Python gates and the `pre-commit` setup.
