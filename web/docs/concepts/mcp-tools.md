# MCP consult tools

Clarion is a **consult-mode** tool. It does not write your code or run your
tests; it answers questions about the codebase so a coding agent doesn't have to
re-derive the same structural facts on every turn. `clarion serve` exposes those
answers as MCP tools.

## The consult loop

Without Clarion, an agent answering "who calls this, and what does it touch?"
greps the tree, opens files, and reconstructs the call graph from scratch —
every time, burning context. With Clarion, the same question is a couple of tool
calls against a graph that was already built once by `clarion analyze`:

```text
agent ──▶ entity_at(file, line)        ──▶ "which entity is here?"
      ──▶ neighborhood(id)             ──▶ callers, callees, imports, refs
      ──▶ execution_paths_from(id, n)  ──▶ bounded call paths
      ──▶ summary(id)                  ──▶ one-paragraph explanation (lazy LLM)
```

Each answer is structured, paginated where needed, and carries enough metadata
(confidence, `scope_excludes`, freshness) for the agent to know how much to
trust it.

## Core tool families

Clarion exposes a 39-tool MCP surface. Start with the navigation and briefing
tools, then reach for catalogue shortcuts when you need a targeted structural
query:

| Tool | What it answers |
| --- | --- |
| `entity_at(file, line)` | "Which entity covers this source location?" |
| `find_entity(pattern)` | "Find entities whose name or summary matches X." |
| `callers_of(id)` | "Who calls this function?" |
| `execution_paths_from(id, max_depth)` | "Show up to N hops of call paths from here." |
| `summary(id)` | "Give me a one-paragraph summary." (lazy LLM, cached) |
| `issues_for(id)` | "What Filigree issues are attached to this entity?" |
| `neighborhood(id)` | "Show callers, callees, container, contained, references, imports in one hop." |
| `subsystem_members(id)` | "Which entities belong to this subsystem?" |
| `source_for_entity(id)` | "Show the indexed source span and context." |
| `orientation_pack(id or file/line)` | "Give me the entity, context, neighbors, paths, issues, and freshness in one packet." |
| `guidance_for(id)` | "Which guidance sheets apply to this entity?" |
| `find_dead_code(scope?)` / `search_semantic(query)` | "Run advanced reachability or semantic-search queries when their inputs are available." |

See the [MCP tool reference](../reference/mcp-tools.md) for parameters and the
shape of each response.

## A broader, growing catalogue

The running server also exposes analyze lifecycle tools, freshness checks,
faceted search, guidance/finding inspection, source/call-site evidence, and
exploration-elimination shortcuts. Connect an MCP client to a live
`clarion serve` to see the full, current `tools/list`.

## Enrich-only by design

`issues_for(id)` reaches into Filigree, a sibling Loom product, to attach issues
to an entity. That binding is strictly **enrich-only**: if Filigree is
unavailable, the tool returns an `unavailable` envelope instead of failing the
call, and Clarion's own answers never depend on it. This is the Loom federation
axiom in practice — a sibling may *add* information to Clarion's view, but is
never *required* for Clarion to make sense.
