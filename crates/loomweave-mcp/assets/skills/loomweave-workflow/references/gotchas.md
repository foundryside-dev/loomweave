# Loomweave gotchas and launch

Relocated verbatim from `SKILL.md` (convention C-20 budget).

## Gotchas (read before hunting for a subsystem)

- **To find a package's subsystem, search the package NAME with `kind`.**
  Subsystems are *named after* their dominant package (e.g. `mypkg`), so
  `entity_find {"pattern":"subsystem"}` returns nothing. Search the package name
  and pass `{"kind":"subsystem"}` to return only subsystem entities, then call
  `subsystem_member_list`. (`entity_find` accepts an optional `kind` filter —
  `"subsystem"`, `"function"`, `"class"`, `"module"`, …; omit it for no filter.)
- **To go from an entity to its subsystem, use `entity_subsystem_get`.**
  `entity_neighborhood_get` does **not** return the entity's subsystem. Call
  `entity_subsystem_get {"id": "<entity-id>"}` — it accepts any entity (a function/class
  resolves through its containing module) and returns the subsystem plus the
  module it resolved through. `subsystem_member_list` is the forward direction.
- **`entity_find` is paginated** (~20/page, `next_cursor`); a broad concept word
  now matches docstring/identifier substrings too, so it can return many hits —
  narrow the pattern (or add a `kind` filter) rather than paging if you can.
- **`entity_callers_list` and `subsystem_member_list` are bounded** (`limit`
  default 50, max 100, plus a numeric-offset `cursor`). Each response carries
  `next_cursor`
  (null when exhausted) and an explicit `truncated` flag — re-call with
  `{"cursor": "<next_cursor>"}` to walk the full set. An empty page on a non-null
  cursor means you paged past the end.
- **`entity_neighborhood_get` caps each bucket independently** with one
  per-bucket `limit`
  and reports a `truncated` **map** (`{callers, callees, contained,
  references_in, references_out, imports_in, imports_out, relations_in,
  relations_out}`) — it has **no cursor**. When a bucket is `truncated:true`,
  switch to that relation's dedicated cursor-paginated tool (e.g.
  `entity_callers_list`, `entity_relation_list`) for the complete set;
  `entity_neighborhood_get` is a one-hop overview, not a paging surface.
- **Relation direction reads as a sentence** (`from KIND to`, ADR-051):
  `entity_relation_list` with `direction: "in"` on a class answers "what
  subclasses / implements / derives this"; `direction: "out"` on a *decorator*
  answers "what does this decorate" (the decorator is the FROM side — inverted
  from where the `@decorator` line sits). Each entry carries the anchoring
  file/line/line-text so you can see the declaration behind the edge.

## Launch

`loomweave serve --path <dir>` where `<dir>` contains `.weft/loomweave/loomweave.db`
(built by `loomweave analyze <dir>`). In an MCP client the tools appear as
`mcp__loomweave__entity_find`, etc. — exactly the names registered in
`tools/list` and used throughout this skill.

**Legacy aliases.** Pre-1.0 docs and transcripts may use retired names
(find_entity, callers_of, neighborhood, subsystem_of, summary, …). The server's
rename shim still accepts them on raw JSON-RPC `tools/call`, but they are NOT
in `tools/list`, so an MCP client cannot call them — always use the registered
names above.

Besides the tools, the server exposes a `loomweave://context` **resource** — live
entity/subsystem/finding counts and index freshness as JSON, a lightweight read
when you only want the numbers (`project_status_get` is the fuller tool-based view).
