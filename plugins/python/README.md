# clarion-plugin-python

The Python language plugin for [Clarion](../../README.md). Extracts Python
entities from source files and serves them to the Clarion core over the
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

This places `clarion-plugin-python` on your `$PATH` and installs the
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

## Design references

- [WP3 plan](../../docs/implementation/sprint-1/wp3-python-plugin.md) — task
  ledger, lock-ins, and UQ resolutions.
- [ADR-003](../../docs/clarion/adr/ADR-003-entity-id-scheme.md) — 3-segment
  `EntityId` format this plugin produces.
- [ADR-018](../../docs/clarion/adr/ADR-018-identity-reconciliation.md) —
  cross-product identity join with Wardline.
- [ADR-022](../../docs/clarion/adr/ADR-022-core-plugin-ontology.md) —
  manifest schema and ontology-boundary enforcement.
- [ADR-023](../../docs/clarion/adr/ADR-023-tooling-baseline.md) — the four
  Python gates and the `pre-commit` setup.
