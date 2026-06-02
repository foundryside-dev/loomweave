# The entity model

Clarion's job is to turn a tree of source files into a queryable graph of
**entities** and **edges**. Everything an agent asks is answered against that
graph.

## Entities

An entity is a named thing Clarion extracted from the source: a function, a
class, a module, or a clustered **subsystem**. Every entity carries a stable
**entity ID** with three colon-separated segments:

```
{plugin_id}:{kind}:{canonical_qualified_name}
```

For example:

```
python:function:auth.tokens.refresh
python:class:http.client.Session
python:module:auth.tokens
```

The contract (ADR-003, ADR-022) is deliberate: the **language plugin owns
segments 1 and 3** — the plugin id and the canonical qualified name — and the
**core never invents kinds**. That keeps identity reproducible across runs and
portable across tools: a sibling product can bind an issue or a finding to
`python:function:auth.tokens.refresh` and trust it still names the same function
after a rename or move.

| Kind | What it is |
| --- | --- |
| `function` | A function or method, addressed by its L7 qualified name |
| `class` | A class definition |
| `module` | A source module (file-level) |
| `subsystem` | A cluster of modules, produced by Clarion's clustering pass |

## Edges

Entities are connected by typed edges. Clarion extracts four:

| Edge | Meaning |
| --- | --- |
| `contains` | Structural nesting — a module contains its classes and functions |
| `calls` | One function invokes another (execution flow) |
| `references` | A symbol mentions another without calling it (not execution flow) |
| `imports` | Module-to-module import (`imports_in` = who imports me, `imports_out` = what I import) |

`calls` edges carry a **confidence**. The default surface is `resolved` —
unambiguous static edges. Ambiguous static candidates and LLM-inferred edges are
excluded unless a query explicitly asks for them, so a clean answer stays clean.

## Why `scope_excludes` matters

Static analysis has blind spots — calls made through an attribute receiver, for
instance, can't always be resolved statically. Rather than silently dropping
them, Clarion's traversal results carry a `scope_excludes` block naming exactly
what was **not** searched. An empty `callers` list with
`scope_excludes: ["attribute-receiver-calls"]` means "none found among the
edges I can see," never "guaranteed nobody calls this." That distinction is what
lets an agent reason safely about a negative result.

## Subsystems

Beyond the raw graph, Clarion clusters modules into **subsystems** and persists
them as first-class entities. `subsystem_members(id)` lists a subsystem's
modules; `subsystem_of(id)` is the reverse — given any entity, find the
subsystem it belongs to (a function resolves through its containing module).

## Storage

The whole graph is persisted to a project-local SQLite database at
`.clarion/clarion.db`, written by a single writer-actor with a reader pool
(ADR-011). There is no mandatory cloud component: Clarion is local-first, and the
only network egress is the LLM provider during `summary(id)` calls.
