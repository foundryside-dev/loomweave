# Language support: what each plugin covers

Loomweave's structural graph is produced by language **plugins**, one per
language, each a subprocess the core launches over JSON-RPC (ADR-002). v1.x ships
two first-party plugins. They do **not** cover the same surface — this page is the
single place to see what each emits, so a missing edge kind or an unavailable
tool reads as *expected* rather than *broken*.

The MCP read tools, summaries, SEI identity, findings, and the pre-ingest secret
scanner are **plugin-agnostic** — they work the same regardless of which plugin
produced an entity. The differences below are entirely in what the plugins
*extract and tag*.

## At a glance

| Capability | Python (`loomweave-plugin-python`) | Rust (`loomweave-plugin-rust`) |
|---|---|---|
| Status | first-party, v1.0 | first-party, 1.x |
| Source backend | `pyright` (type-resolved) | `syn` (parse-only, in-project symbol table) |
| Ontology version | 0.9.0 | 0.5.0 |
| Wardline-aware | **yes** (`wardline:*` trust tags) | no |
| **Entity kinds** | `function`, `class`, `module` | `module`, `struct`, `enum`, `trait`, `function`, `impl`, `type_alias`, `const`, `static`, `macro` |
| **Structural edges** | `contains`, `calls`, `references`, `imports` | `contains`, `calls`, `references`, `imports` |
| **Relation edges** | `inherits_from`, `decorates` | `implements`, `derives` |
| Call/ref resolution tiers | `resolved` / `ambiguous` / `inferred` (pyright) | `resolved` (in-project only; external targets dropped) |
| **Categorisation / reachability-root tags** | **yes** — see below | **none today** |
| Dead-code analysis (`entity_dead_list`) | **works** | **unavailable** (no roots — see below) |
| Summaries (`entity_summary_get`) | on-demand, any entity | on-demand, any entity |

## Categorisation & reachability-root tags

These `entity_tags` drive the dead-code and faceted views. They are what makes
`entity_dead_list`, `entity_entry_point_list`, `entity_http_route_list`, etc.
return data.

**Python emits:** `entry-point`, `exported-api`, `public-surface`, `test`,
`data-model`, `http-route`, `cli-command`, `framework-handler`, and the
Wardline-derived `wardline:external_boundary` / `wardline:trusted`. Notable: a
module that declares no `__all__` gets its non-underscore module-level
defs/classes tagged `public-surface` — a lower-confidence reachability root than
a declared `exported-api` (ADR-053 / clarion-4ec50f3d92), so a Python codebase is
not over-reported as dead just because it does not exhaustively declare `__all__`.

**Rust emits none of these today.** The plugin extracts entities and edges but no
categorisation tags. Consequently `entity_dead_list` on a **pure-Rust** index is
**signal-unavailable**: the dead-code engine excludes a plugin's entities when it
emits no reachability roots (rather than false-flagging the entire crate dead).
The structural tools (`entity_find`, `entity_callers_list`,
`entity_neighborhood_get`, the edge surfaces) are unaffected. Adding the Rust
root model (visibility → `exported-api`, `fn main`/bin → `entry-point`,
`#[test]` → `test`, route/CLI attribute macros → handlers) is tracked in
**clarion-05fdd0490e**. See [rust-known-limitations.md](./rust-known-limitations.md)
for the full list of what Rust analysis does and does not resolve.

## Mixed-language repositories

A repo with both Python and Rust is analysed by both plugins in one pass; each
file is routed to the plugin that claims its extension. Dead-code reachability
runs over the union, so in a mixed repo Python's roots can make Python entities
reachable while Rust entities remain in the "no roots for this plugin" exclusion
until the Rust root model lands. The low-confidence dead-code advisory's lever
copy is Python-centric today (it names `__all__`); making it language-aware is
folded into clarion-05fdd0490e.

## Other languages

Java and TypeScript are v2.0+ scope. Because plugins are subprocesses speaking a
stable JSON-RPC contract (ADR-002) with a manifest-declared ontology (ADR-022),
a new language is an additive plugin, not a core change.
