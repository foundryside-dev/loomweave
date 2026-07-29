# Loomweave entity ids and SEIs

Relocated verbatim from `SKILL.md` (convention C-20 budget).

## Entity IDs — the model

Every entity has an ID: `{plugin}:{kind}:{qualified_name}`
(e.g. `python:function:pkg.mod.func`, `python:class:pkg.mod.Cls`,
`python:module:pkg.mod`). Subsystems are `core:subsystem:{hash}`.

**You almost never type IDs.** Get one from `entity_find` / `entity_at`, then
**copy it verbatim** into the next tool. Don't hand-construct or guess IDs.

### `id` vs `sei` — which one to bind on

Every entity in a tool response now carries an `sei` field alongside its `id`.
They are not interchangeable:

- **`id`** is the entity's *locator* — a mutable address. It changes when the
  code is renamed or moved, and it's the right thing to feed into the next
  Loomweave tool call (above).
- **`sei`** is the entity's *durable, stable identity*. It survives renames and
  moves. **When you record a cross-tool binding** — e.g. attaching a Filigree
  issue to a Loomweave entity — **bind on the `sei`, not the `id`.** A binding
  keyed on the mutable `id` silently breaks the first time the entity moves.

`sei` is `null` when the index predates SEI support or the entity has no binding
yet; `project_status_get` and `entity_orientation_pack_get` report
`sei.populated` so you can tell which case you're in.
