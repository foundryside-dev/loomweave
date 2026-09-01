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
| Status | first-party, 1.x | first-party, 1.x |
| Source backend | `pyright` (type-resolved) | `syn` (parse-only, in-project symbol table) |
| Ontology version | 0.13.0 | 0.9.0 |
| Wardline-aware | **yes** (`wardline:*` trust tags) | no |
| **Entity kinds** | `function`, `class`, `module` | `module`, `struct`, `enum`, `trait`, `function`, `impl`, `type_alias`, `const`, `static`, `macro` |
| **Structural edges** | `contains`, `calls`, `references`, `imports` | `contains`, `calls`, `references`, `imports` |
| `calls` targets | functions **and classes** (instantiation `Name(...)` is a call to the class, ADR-059) | functions only (`Type::new()` resolves; tuple-struct `Type(..)` is an unresolved site) |
| **Relation edges** | `inherits_from`, `decorates` | `implements`, `derives` |
| Call/ref resolution tiers | `resolved` / `ambiguous` / `inferred` (pyright) | `resolved` (in-project only; external targets dropped) |
| **Categorisation / reachability-root tags** | **yes** — see below | **yes** — see below (ADR-054) |
| Dead-code analysis (`entity_dead_list`) | **works** | **works** (lib/bin roots, ADR-054) |
| Summaries (`entity_summary_get`) | on-demand, any entity | on-demand, any entity |

## Categorisation & reachability-root tags

These `entity_tags` drive the faceted views and most dead-code roots. Rust also
contributes typed provenance on uniquely resolved public re-export edges to the
dead-code root set; that edge-only signal deliberately does not appear in tag
facets such as `entity_exported_api_list`.

**Python emits:** `entry-point`, `exported-api`, `public-surface`, `test`,
`data-model`, `http-route`, `cli-command`, `framework-handler`, and the
Wardline-derived `wardline:external_boundary` / `wardline:trusted`. Notable: a
module that declares no `__all__` gets its non-underscore module-level
defs/classes tagged `public-surface` — a lower-confidence reachability root than
a declared `exported-api` (ADR-053 / clarion-4ec50f3d92), so a Python codebase is
not over-reported as dead just because it does not exhaustively declare `__all__`.
When `__all__` explicitly re-exports imported names, including names added by a
literal `__all__ += [...]`, the module entity is tagged `exported-api`; local
functions/classes listed in `__all__` remain the exported entities themselves,
so the module is not double-counted as a second public surface. Main-guard
targets, common runner wrappers (`asyncio.run`, `anyio.run`, `typer.run`), and
module-level command candidates using `sys.argv` or `argparse` are tagged
`cli-command` without `framework-handler`; decorator/framework-dispatched CLI
functions still carry `framework-handler`.

Plainweave uses `entry-point`, `http-route`, `exported-api`, and `cli-command`
as its intent-coverage denominator. If re-analysis lowers a Plainweave coverage
ratio by adding previously invisible public surfaces, that can be the accurate
result: Plainweave's incomplete-denominator warning was honest, requirements are
right, and Loomweave now sees more doors.

### Classifier support and completeness

An empty tag page is meaningful only when the latest completed analysis proves
that every active source plugin declares the tag and that enumeration was
complete. `entity_tag_list` and the tag shortcuts therefore return a
`classification` object with schema `loomweave.classification.v1`:

| `state` | Meaning |
|---|---|
| `supported` | Every active source plugin declares the requested tag. |
| `partial` | At least one active plugin declares it and at least one does not. |
| `unsupported` | No active plugin declares it. |
| `unavailable` | No valid latest-run evidence exists, or no plugin matched source files. |

`state` describes declaration support; `complete` describes the evidence for
this particular enumeration. A result is complete only when the latest run has
complete plugin discovery and source walking, every active plugin has status
`complete` with zero degraded files, and the response is the whole enumeration:
offset zero, `returned == total`, and `page.truncated`, `scope_truncated`, and
`scan_truncated` all false.

This makes **supported-zero** explicit. For example, `http-route` with
`state: "supported"`, `complete: true`, and `matches: 0` means the active
plugins could classify routes and found none. Zero with `partial`,
`unsupported`, `unavailable`, or `complete: false` is not proof of absence.
Likewise, a nonzero page with `complete: false` proves only that the returned
entities matched; it does not prove that the denominator is exhaustive.

Each analysis run stores the evidence behind this decision in
`runs.stats.classifier_coverage` (`loomweave.classifier-coverage.v1`). Per-plugin
status is `complete`, `degraded`, `failed`, or `not-applicable`; the record also
names the declared tags, matched/analyzed/retained/degraded file counts, plugin
and ontology versions, discovery errors, and source-walk omissions. Run
`loomweave doctor --format json` and inspect `classifier.enumeration` and
`classifier.tags` when classification is incomplete.

**Rust emits** (ADR-054, clarion-05fdd0490e): `exported-api`, `entry-point`,
`test`, `allow-dead-code`, `http-route`, and `cli-command` (the last two with a
`framework-handler` companion), derived from Rust's explicit semantics rather
than inferred — so `entity_dead_list` now **works** on a pure-Rust index.

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
- `test` — `#[test]` / `#[bench]`, the std-replacement runners (`#[rstest]`,
  `#[test_case]`, `#[quickcheck]`), or an item whose `cfg` constraints prove it
  cannot exist with `test=false`, directly or through an inline module or impl.
  This includes `cfg(test)`, `cfg(all(test, ...))`, and equivalent nested
  `not` predicates. `cfg(any(test, feature = "x"))` and ordinary
  `cfg_attr(test, ...)` are not test-only because they can exist in app builds.
- `allow-dead-code` — an item carrying `#[allow(dead_code)]` /
  `#[expect(dead_code)]`, plus lexical children of an inline module or impl
  carrying one of those attributes (an explicit author keep-signal; the
  lowest-confidence root class). A nested `warn` / `deny` / `forbid` cancels
  the inherited keep signal. An attribute on a struct does not jump to a
  separate sibling impl, matching rustc lint scope.
- `http-route` (+ `framework-handler`) — actix-web / ntex / rocket route
  attribute macros (`#[get("/")]`, `#[post]`, …, `#[route]`).
- `cli-command` (+ `framework-handler`) — clap / structopt CLI derives
  (`#[derive(Parser)]` / `Subcommand` / `Args` / `ValueEnum` / `StructOpt`).
- `entry-point` also covers pyo3 FFI host exports (`#[pyfunction]` /
  `#[pyclass]` / `#[pymodule]`) and proc-macro entry points (`#[proc_macro]` /
  `#[proc_macro_derive]` / `#[proc_macro_attribute]`) — items reached from a
  non-Rust host or the compiler.
- `pub` methods of `pub` types in **library** targets (inherent `impl` blocks)
  are `exported-api` (same lib/bin and pub-chain gating as regular `pub` items).

Not yet emitted by Rust (tracked second-pass extensions): trait-impl-method
rooting (a method reached only via
trait dispatch — `framework-handler` excludes the *handler* but not its private
callees, the documented under-rooting residual); and the lower-prevalence
frameworks (wasm-bindgen / napi / uniffi / cxx FFI, poem/salvo `#[handler]`,
rocket `#[catch]`, tarpc/jsonrpsee, argh/gumdrop). Builder-pattern frameworks
(axum `Router::route`, warp filters, tonic codegen) register routes via runtime
calls, not attributes, so a parse-only extractor cannot detect them. The
structural tools (`entity_find`, `entity_callers_list`,
`entity_neighborhood_get`, the edge surfaces) are unaffected. See
[rust-known-limitations.md](./rust-known-limitations.md) for the full list of
what Rust analysis does and does not resolve.

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
