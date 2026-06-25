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
| Ontology version | 0.9.0 | 0.6.0 |
| Wardline-aware | **yes** (`wardline:*` trust tags) | no |
| **Entity kinds** | `function`, `class`, `module` | `module`, `struct`, `enum`, `trait`, `function`, `impl`, `type_alias`, `const`, `static`, `macro` |
| **Structural edges** | `contains`, `calls`, `references`, `imports` | `contains`, `calls`, `references`, `imports` |
| **Relation edges** | `inherits_from`, `decorates` | `implements`, `derives` |
| Call/ref resolution tiers | `resolved` / `ambiguous` / `inferred` (pyright) | `resolved` (in-project only; external targets dropped) |
| **Categorisation / reachability-root tags** | **yes** — see below | **yes** — see below (ADR-054) |
| Dead-code analysis (`entity_dead_list`) | **works** | **works** (lib/bin roots, ADR-054) |
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

**Rust emits** (ADR-054, clarion-05fdd0490e): `exported-api`, `entry-point`,
`test`, and `allow-dead-code`, derived from Rust's explicit semantics rather than
inferred — so `entity_dead_list` now **works** on a pure-Rust index.

- `exported-api` — an unrestricted `pub` value/type item whose whole enclosing
  module chain is `pub` (the visibility chain reaches the crate's external
  surface), in a **library** target. `pub(crate)`/`pub(super)`/`pub(in …)` are
  intra-crate, not external API, and are excluded; a **binary** target's `pub`
  items are internal (their entry is `fn main`), detected via the `@bin(…)`
  module-path root. A `macro_rules!` is `exported-api` when it carries
  `#[macro_export]`.
- `entry-point` — `fn main`; a runtime-entry attribute (`#[tokio::main]` /
  `#[actix_web::main]` / `#[async_std::main]`); an FFI export (`#[no_mangle]` /
  `#[export_name]`).
- `test` — `#[test]` / `#[bench]`, or any item under a `#[cfg(test)]` module.
- `allow-dead-code` — an item carrying `#[allow(dead_code)]` /
  `#[expect(dead_code)]` (an explicit author keep-signal; the lowest-confidence
  root class).

Not yet emitted by Rust (tracked, increment 2): framework-attribute handlers
(`http-route` / `cli-command` / `framework-handler` from axum/actix/rocket/clap
attributes), `pub use` re-export resolution, and `pub`-method rooting of `pub`
types. A `pub(crate)` item re-exported `pub` is therefore under-rooted today (a
narrow, fail-toward-live residual). The structural tools (`entity_find`,
`entity_callers_list`, `entity_neighborhood_get`, the edge surfaces) are
unaffected. See [rust-known-limitations.md](./rust-known-limitations.md) for the
full list of what Rust analysis does and does not resolve.

## Mixed-language repositories

A repo with both Python and Rust is analysed by both plugins in one pass; each
file is routed to the plugin that claims its extension. Dead-code reachability
runs over the union of both plugins' roots. The low-confidence dead-code advisory
and the no-roots envelope are **language-aware** (ADR-054): the lever copy is
sourced from the plugins actually present, so a Rust corpus is handed Rust levers
(`pub` an item, add a `[[bin]]` / `fn main`, `#[test]`) and a Python corpus is
handed Python levers (`__all__`, decorators) — never the other language's advice.

## Other languages

Java and TypeScript are v2.0+ scope. Because plugins are subprocesses speaking a
stable JSON-RPC contract (ADR-002) with a manifest-declared ontology (ADR-022),
a new language is an additive plugin, not a core change.
