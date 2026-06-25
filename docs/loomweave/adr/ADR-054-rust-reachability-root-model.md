# ADR-054: Rust Reachability-Root Tag Model — Visibility, Entry-Points, Tests, Handlers

**Status**: Accepted
**Date**: 2026-06-25
**Deciders**: john@foundryside.dev
**Context**: clarion-05fdd0490e. Sibling to ADR-053 (the Python `public-surface`
fallback); coordinates with ADR-049 (Rust qualname canonicalization) for the
`@bin(<name>)` / `@cfg(...)` namespace segments this model reads. Closes the
limitation recorded as PDR-0012 ("binary/lib roots unsupported").

## Context

The Rust language plugin (`crates/loomweave-plugin-rust`) extracts entities and
edges but emits **zero categorisation / reachability-root tags**. The dead-code
engine (`loomweave-mcp` `catalogue/shortcuts.rs`) excludes a plugin's entire
entity set from the survey when that plugin emits no root tags — a deliberate
honesty posture (signal-*unavailable* beats false-flagging an entire crate
dead, `dead_code_candidate_set` → `plugins_with_root_tags`). The consequence is
that `entity_dead_list` / `find_dead_code` simply **does not work for Rust**, and
the faceted surfaces (`entity_entry_point_list`, `entity_http_route_list`, …)
return nothing for Rust entities.

This is the Rust counterpart of ADR-053, but it is **net-new, not a port**.
Python needed a PEP 8 *inference* (`public-surface`) because `__all__` is
optional and usually absent. Rust's visibility is **explicit in the grammar**
(`pub`), so there is no inference gap to paper over — the plugin simply needs to
read the visibility, entry-point, and test signals already present in the AST
and emit the root vocabulary the engine already consumes.

The engine side is ready. `DEAD_CODE_ROOT_TAGS` already contains
`entry-point` / `exported-api` / `test` / `http-route` / `cli-command` /
`framework-handler`; the per-plugin no-roots exclusion is keyed on
`entity_tags`, not on a plugin name, so it **auto-lifts** the moment Rust emits
any root tag — no new MCP root plumbing is required. The wire already carries
`tags: Vec<String>` on every plugin entity (`loomweave-core` `plugin/host.rs`).

## The grounding principle: error-cost asymmetry (fail-toward-live)

Reachability roots exist to stop *live* code being reported *dead*. The two
error directions are not symmetric:

- **Over-rooting** (tag something that is actually intra-crate-reachable) → the
  item merely reads **live** → we under-report some dead code. Safe.
- **Under-rooting** (miss a genuine external-API root) → real API reads **dead**
  → a false positive that erodes trust in the whole signal. This is the exact
  failure ADR-053 fought.

So every judgement call below resolves toward rooting. Precision (Cargo.toml
target parsing, full re-export resolution, method-level rooting) is deferred
where it would only *narrow* the root set, because narrowing is the unsafe
direction and the safe default already covers the case.

## Decision

The Rust plugin emits four reachability-root tag classes, derived from the
`syn` AST with no cross-file resolution (increment 1). All are computed
per-item during the existing recursive item walk (`extract.rs` `walk_items`),
which already carries the enclosing module path and attribute list.

### 1. `exported-api` — external public surface (lib targets)

An item is `exported-api` iff **all** hold:

- its visibility is `pub` **without restriction** — `syn::Visibility::Public`.
  `pub(crate)` / `pub(super)` / `pub(in path)` are **not** `exported-api`: their
  reachability is intra-crate and statically analysable, so the normal
  call/import reachability handles them (and missing them only over-reports, the
  safe direction is already covered by leaving them out — they are reachable
  from a rooted caller if used);
- **every enclosing module is `pub`** (the visibility chain reaches the crate
  root). A `pub fn` inside a private `mod` is *not* part of the crate's external
  surface; a `pub fn` inside a `pub mod` is. The walk threads a single
  `ancestors_all_pub` boolean; the file-root module is the crate boundary and
  counts as public;
- the file is **not a binary-target file**. Binary targets route to a
  `<crate>@bin(<name>)` module-path root (ADR-049 / `scope.rs`), which can never
  collide with a real module — so `module_path` containing `@bin(` is a reliable
  "this is a bin target" discriminator. `pub` in a bin is internal; the real
  entry is `fn main` (rooted separately).

Applies to the leaf value/type item kinds (`function`, `struct`, `enum`,
`trait`, `type_alias`, `const`, `static`, `macro`). **Module entities are not
tagged, and are excluded from dead-code candidacy engine-side** (a new
`DEAD_CODE_CONTAINER_KINDS = ["module"]` in `loomweave-mcp`): a module is the
*containment spine* rooted at the always-live crate root, so it is never "dead"
in any actionable sense — you remove its contents, not the namespace.
Reachability proper runs over call+import edges only, and the Rust plugin emits
no module-targeting `imports` edges (its import edges target items), so without
this exclusion **every** Rust module would read as dead and dominate the
candidate set (the dogfood confirmed: a 3-of-7 over-report tripping the
LOW-confidence band, vs. the clean 1-of-5 once modules are excluded). The
exclusion is kind-based and language-agnostic — it also closes the same latent
over-report for any never-imported Python module.

**Accepted imprecision (documented, fail-toward-live):** a *pure-binary* crate
(`src/main.rs` with no sibling `src/lib.rs`) routes its files to the **bare**
crate root, not `@bin(...)` (`scope.rs`: "main.rs IS its canonical crate root").
So a pure-bin crate's `pub` items are indistinguishable from a lib's at the
module-path level and will receive `exported-api`. This over-roots (their pub is
really internal) — the safe direction. Precise lib-vs-bin classification from
`Cargo.toml` `[lib]`/`[[bin]]` targets is deferred; it would only *remove* roots.

### 2. `entry-point` — program entry

An item is `entry-point` iff any hold:

- it is a module-level `fn main` (covers both the lib+bin `@bin` root and the
  pure-bin bare root);
- it carries a runtime-entry attribute macro: `#[tokio::main]`,
  `#[actix_web::main]`, `#[async_std::main]` (last path segment `main` under a
  known async-runtime path);
- it carries `#[no_mangle]` or `#[export_name = "…"]` — an FFI / C-ABI export
  reachable only from outside the Rust call graph.

### 3. `test` — test / bench entry

An item is `test` iff any hold:

- it carries `#[test]` or `#[bench]`;
- it is under a `#[cfg(test)]` ancestor module (the walk threads an
  `under_cfg_test` boolean, set when descending into a module whose attrs carry
  a literal `cfg(test)` predicate).

Test items are roots (they are entry points the harness invokes) and are
excluded from `app_only` reachability by the engine, exactly as Python's `test`
tag.

### 4. `allow-dead-code` — explicit author "keep" assertion

An item carrying `#[allow(dead_code)]` is tagged `allow-dead-code`, a **new
additive entry in `DEAD_CODE_ROOT_TAGS`**. `#[allow(dead_code)]` is the author
explicitly suppressing rustc's own dead-code lint — an "I am keeping this on
purpose" assertion. Rooting it is fail-toward-live and consistent with rustc's
own behaviour (it will not warn). It is the lowest-confidence class (an explicit
suppression, not a structural surface); the provenance lives in the distinct tag
value, per the ADR-053 precedent.

### Provenance by tag value, not by plumbing

As in ADR-053, the declared-vs-inferred distinction lives in the **tag value**
(`exported-api` = declared `pub` surface; `entry-point`/`test` = structural;
`allow-dead-code` = explicit suppression), not in new wire fields. For
reachability the union is what matters, so all four simply join the root set.

### Advisory copy is language-aware

`dead_code_no_roots_envelope` and the LOW-confidence advisory in `shortcuts.rs`
currently name `__all__` and Python decorators as the levers. That is correct
only while a Rust-only index hits the *no-roots* exclusion. **Once Rust emits
roots**, the advisory can fire for Rust corpora and MUST name Rust levers (`pub`
an item, add a `[[bin]]` / `fn main`, add `#[test]`) instead of `__all__`. The
lever phrasing is sourced per-plugin / per-language so a Rust corpus is never
handed Python-only advice. This ships **with** the roots, not after.

### Ontology bump

Additive tag-vocabulary change: Rust plugin `ontology_version` **0.5.0 →
0.6.0**, in lockstep across the four locations that carry it —
`crates/loomweave-plugin-rust/plugin.toml`, its byte-identical wheel-data copy
(`packaging/rust-plugin-dist/wheel-data/.../plugin.toml`, guarded by
`scripts/check-rust-plugin-manifest-lockstep.py`), `serve.rs`'s `initialize`
response, and the `docs/operator/language-support.md` table.

## Increment 2 (implemented): framework handlers + `pub`-method rooting

A focused framework-attribute survey (a 6-agent taxonomy sweep across the Rust
web / CLI / FFI / RPC / test ecosystems) produced a precision-first, collision-
aware detection set. The Rust plugin now also emits:

- **`http-route`** (+ `framework-handler` companion) — actix-web / ntex / rocket
  route attribute macros, matched on the attribute's last path segment (`get`,
  `post`, `put`, `patch`, `delete`, `head`, `options`, `trace`, `connect`,
  `route`). Cross-crate last-segment collisions are benign (every match means a
  route) and over-rooting is fail-toward-live.
- **`cli-command`** (+ `framework-handler`) — clap / structopt CLI derives,
  matched on the `#[derive(...)]` list (`Parser`, `Subcommand`, `Args`,
  `ValueEnum`, `StructOpt`).
- **`entry-point`** also from pyo3 FFI host exports (`#[pyfunction]` / `#[pyfn]`
  / `#[pyclass]` / `#[pymodule]`) and proc-macro entry points (`#[proc_macro]` /
  `#[proc_macro_derive]` / `#[proc_macro_attribute]`) — items reached from a
  non-Rust host or the compiler.
- **`test`** also from the std-replacement runners `#[rstest]` / `#[test_case]`
  / `#[quickcheck]`.
- **`pub`-method rooting** — a `pub` method of an inherent `impl` whose enclosing
  module chain is `pub` (lib target) is `exported-api`. Trait-impl methods carry
  inherited visibility (no `pub`) so the rule leaves them unrooted (see the
  residual below). The pub-chain + cfg(test) gating is inherited from the
  enclosing module; a method named `main` is never an entry point.

**Critical tag-semantics fact (verified against `shortcuts.rs`):**
`framework-handler` is in `DEAD_CODE_EXCLUDED_TAGS`, **not** `DEAD_CODE_ROOT_TAGS`
— it excludes the *tagged entity itself* from dead-code candidacy but does NOT
root its callees. So `http-route` / `cli-command` (the real roots) carry
`framework-handler` only as the self-exclusion companion, exactly as the Python
plugin does. FFI host exports map to `entry-point` (a real root) precisely so
their callees ARE traversed.

The collision survey's one **catastrophic** finding is honoured: a bare
last-segment match on `serde` (from `typetag::serde`) would match every
`#[serde(...)]`, so no such match exists; CLI detection is derive-gated and the
deferred FFI frameworks that need it use full-path matching.

This is a further additive ontology change: Rust plugin `ontology_version`
**0.6.0 → 0.7.0**.

## Still deferred (second-pass extensions)

- **`pub use` re-export resolution** — a privately-defined item re-exported
  `pub` is part of the API surface. Resolving the re-export target needs the
  cross-file symbol table (the resolver). The common facade case (`pub use
  internal::Thing` where `Thing` is itself `pub`) is **already covered** by
  `Thing`'s own `exported-api` tag at its definition; only a `pub(crate)` item
  re-exported `pub` is under-rooted, a narrow residual.
- **Trait-impl-method rooting** — a method reached only via trait dispatch
  (the Rust framework-dispatch case) has no inbound call edge; the pub rule does
  not root it (inherited visibility). `framework-handler` on a `#[handler]` would
  exclude the handler but not its private callees — the documented under-rooting
  residual this model's error-cost asymmetry otherwise fights. Left as an open
  follow-up rather than silently promoting handlers to `entry-point`.
- **Lower-prevalence frameworks** — wasm-bindgen / napi / uniffi / cxx FFI,
  poem/salvo `#[handler]`, rocket `#[catch]`, tarpc / jsonrpsee, argh / gumdrop /
  bpaf. Several need full-path matching to stay collision-safe; deferred to a
  second pass. Builder-pattern frameworks (axum `Router::route`, warp filters,
  tonic codegen) register routes via runtime calls, not attributes, so a
  parse-only (`syn`) extractor cannot detect them — a permanent limitation, not a
  deferral.

## Empirical result (dogfood, the loomweave workspace itself)

Analysed a copy of the loomweave Rust workspace (181 files, **4231 entities**,
5755 edges) with the increment-1+2 plugin (acceptance criterion AC#6):

- `entity_dead_list` is **available and plausible**: **357 dead / 2384 analysed
  ≈ 15%** → **moderate** confidence (well inside the plausible band; nowhere near
  the >25% "implausible" band ADR-053 fought for Python). The no-roots exclusion
  lifts; `find_entry_points` / `find_http_routes` light up for Rust.
- **Trait-impl-method rooting deferral is vindicated by the data:** of the 357
  candidates only **8** are `impl` methods. Trait-heavy real Rust does *not*
  pathologically inflate the dead share, so deferring trait-method rooting was
  the right call, not a hidden weakness.
- The candidate set is dominated by **private value items** — `const` (124),
  `struct` (113), `enum` (25) — reached through *value references* (a const read
  in an expression, a struct used as a field type) that the Rust plugin does not
  yet emit as reachability edges, AND the dead-code adjacency counts only
  call+import (not `references`). So these read as dead though they are used: a
  **pre-existing reference-extraction + reachability-coverage gap surfaced (not
  caused) by making Rust analysable.** It is the natural next accuracy lever
  (the Rust analogue of ADR-053's method-rooting follow-up) and is tracked
  separately; it does not block this model, whose headline number is already
  honest and plausible.

## Alternatives considered

### Alternative 1: reuse `exported-api` for the `#[allow(dead_code)]` keep-signal

**Pros**: one fewer tag, no `DEAD_CODE_ROOT_TAGS` entry. **Cons**: conflates an
external-API claim with an explicit local suppression — an agent inspecting the
tag could no longer tell a public export from a privately-kept dead item. The
distinct tag costs one const entry and keeps provenance legible (the ADR-053
reasoning). Rejected.

### Alternative 2: classify lib vs bin from `Cargo.toml` targets now

**Pros**: precise `exported-api` suppression for pure-bin crates. **Cons**:
larger (parse `[lib]`/`[[bin]]`/`[[example]]`/`[[bench]]`, thread target kind
into extraction) and it only *removes* roots — the unsafe direction. The
`@bin(...)` module-path discriminator already handles the lib+bin and multi-bin
cases for free; the residual (pure-bin over-rooting) is fail-toward-live. Left as
a precision follow-up, not built.

### Alternative 3: require the full `pub` visibility chain via type resolution

The chosen model approximates the pub-chain with a threaded `ancestors_all_pub`
boolean over the lexical module nesting. A fully precise model would resolve
re-exports and `pub use` paths to compute true external reachability. That is the
deferred re-export work; the lexical approximation is safe (it can only
over-root via the pure-bin case, never under-root a lexically-public item).

## Consequences

- `entity_dead_list` / `find_dead_code` becomes **available for Rust**: a
  Rust-only index is surveyed instead of wholesale-excluded, and the
  `plugins_without_roots` exclusion stops withholding Rust entities
  automatically (no engine change beyond the advisory copy).
- The faceted surfaces (`entity_entry_point_list`, `entity_test_list`, etc.)
  light up for Rust with no read-side change — they are plugin-agnostic queries
  over `entity_tags`.
- Mixed Python+Rust repos get a unified root set across both plugins; a Rust
  `pub` API reached only from a sibling crate or a test stays live via its
  `exported-api` / `test` root.
- A Rust corpus that genuinely has little rooted surface still gets an **honest,
  language-correct** advisory (Rust levers, not `__all__`).
- Increment 2's additions (handler tags, re-export resolution, method rooting)
  are all additive: new tags join `DEAD_CODE_ROOT_TAGS`; no existing tag
  semantics change. The Rust ontology bumps again then.
