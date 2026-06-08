# Rust language plugin — design

- **Status:** draft (brainstormed 2026-06-08)
- **Branch:** `feat/rust-plugin-spec` (based on `rc3`)
- **Plugin id:** `rust` · **language:** `rust` · **extensions:** `["rs"]`
- **Backend:** `syn` (syntactic, build-free)
- **Scope:** v2.0+ second language plugin (CLAUDE.md marks non-Python languages as v2.0+)

## 1. Scope & motivation

A first-party Rust **language plugin** — the syntactic sibling of the existing
pyright-backed Python plugin. It extracts entities and edges from `.rs` source
and reports them to the plugin host over the existing JSON-RPC 2.0 protocol.

Motivating use case: **Loomweave indexes its own Rust workspace.** Today the
core ships a Python plugin but cannot analyse the Rust crates that make up
Loomweave itself; this closes that gap and makes the tool dogfoodable on its
own source.

**Hard constraint (inherited, non-negotiable):** `analyze` stays
**build-free, Cargo-metadata-free, network-free, and credential-free.** The
plugin must never invoke `cargo`, read `Cargo.lock` resolution, download a
toolchain, or require the target project to compile. This is why the backend is
syntactic (`syn`) rather than semantic (rust-analyzer) — see §2.2 and ADR-002
sandbox posture.

## 2. Architecture

### 2.1 Crate home & build model

- **Workspace member** `crates/loomweave-plugin-rust`, producing the binary
  `loomweave-plugin-rust`. Built by the existing `cargo build --workspace
  --bins`, exactly like `crates/loomweave-plugin-fixture`.
- **Lockstep-versioned** with the workspace (the `scripts/check-*.py` version
  guards and the CI floor, `cargo-deny`, pinned toolchain, and pedantic clippy
  all extend to it for free). No second toolchain, no separate release cadence.
  This is the deliberate divergence from `plugins/python/`'s out-of-workspace
  layout — a first-party Rust plugin has no need for the release independence
  that justified Python's separate uv project.
- **Depends on `loomweave-core` directly.** It calls
  `loomweave_core::entity_id()` to assemble IDs rather than reimplementing the
  3-segment assembler the way `entity_id.py` had to. It must satisfy the
  existing cross-language parity fixture `fixtures/entity_id.json` (the Rust
  assembler and any plugin-side ID construction must agree byte-for-byte).
- **Manifest** `crates/loomweave-plugin-rust/plugin.toml`, mirroring the Python
  manifest's structure (see §3, §4). `executable = "loomweave-plugin-rust"`
  (bare basename per ADR-021 — the host refuses any path component). `loomweave
  install` drops the compiled binary on PATH.

### 2.2 Backend: `syn`

- Parse each file with `syn` (full-fidelity typed AST) using `proc-macro2`
  spans for `source_byte_start` / `source_byte_end` on every entity and edge.
- **Why syntactic, not rust-analyzer:** rust-analyzer would give resolved
  calls/references and trait/type info (full symmetry with pyright), but it
  needs a buildable project + Cargo metadata, is RAM-heavy and slow to index,
  and pulls a network/toolchain dependency into `analyze` — violating the §1
  hard constraint. The confidence ladder (§3.2) lets rust-analyzer arrive
  *later* as strictly-additive enrichment (Phase 3) instead of a prerequisite.

### 2.3 Resolution model — the load-bearing piece

The host protocol is **per-file**: `analyze_file(path)` returns that file's
final entities and edges. The Python plugin's per-file calls are nonetheless
cross-file-aware because **pyright pre-indexes the whole project**, so even the
first `analyze_file` can resolve a symbol defined in another file. Plain
per-file `syn` has **no such index** — so structural edges that cross files
(`implements` to a trait in another module, `contains` across `mod foo;` file
boundaries, `imports`) cannot be resolved per-call without help.

**Design:** at `initialize` (which carries `project_root` in
`InitializeParams`), the plugin walks the project tree and builds an in-memory
**project symbol table** — one cheap `syn` parse per `.rs` file, mapping every
declared path to its entity ID. Each subsequent `analyze_file` resolves its
structural edges against that *complete* table. This is what earns Phase 1 its
"Resolved-by-construction" claim; without it those edges would be Ambiguous.

- **In-project targets → `Resolved`.**
- **Out-of-project targets** (`std`, `core`, external crates like `serde`) are
  **not faked into edges.** They are dropped, or recorded as an external
  reference marker — honestly, never as a dangling in-project edge. Most
  `derives` targets (§3.2) fall here.
- The init-time walk reads source **within** `project_root` only
  (`reads_outside_project_root = false`). It should honour the same
  ignore/skip-lists the host applies where practical (e.g. a custom
  `[loomweave].store_dir`); a superset symbol table is harmless for resolution
  but the file *entities* must match the host's analysed set.
- **Cost note:** the init walk parses every file once, then `analyze_file`
  parses each again. For Loomweave-scale workspaces this is cheap. If it ever
  matters, the init parse can be cached and reused. Not a Phase 1 concern.

## 3. Ontology

### 3.1 Entity kinds

```
function, struct, enum, trait, impl, module, type_alias, const, static, macro
```

All ten ship in Phase 1. Entity kinds are open per-plugin strings — the host
manifest validator only checks the grammar `[a-z][a-z0-9_]*`, that none are
reserved, and that declared roles reference declared kinds (verified in
`crates/loomweave-core/src/plugin/manifest.rs`). The fixture plugin's `widget`
kind proves arbitrary kinds are accepted.

### 3.2 Edge kinds

| Edge | Phase | Confidence | Notes |
|------|-------|-----------|-------|
| `contains` | 1 | Resolved | `mod`→item, `impl`→method, file-module→items |
| `imports` | 1 | Resolved | `use` statements (in-project targets) |
| `implements` | 1 | Resolved | `impl Trait for Type` → trait. Highest-value Rust edge ("who implements X") |
| `derives` | 1 | Resolved (in-project) | `#[derive(...)]` → trait. Same resolution as `implements`; external derives → external ref, not faked. **Escape hatch:** if external-target representation complicates it, slips to Phase 2. |
| `calls` | 2 | Ambiguous + Inferred | same-name candidates Ambiguous; unresolved sites handed to core via `unresolved_call_sites` for query-time Inferred resolution |
| `references` | 2 | Ambiguous + Inferred | type/path mentions, same channel as `calls` |

Phase 1 edges are all syntactically explicit and resolve against the §2.3
symbol table → no fuzzy heuristics in the first cut. The genuinely hard
call/reference resolution is quarantined to Phase 2.

### 3.3 Roles & the guidance integration fix

```toml
[ontology.roles]
file_scope = ["module"]
callable   = ["function"]
```

**Required integration fix (Phase 1 task, not hand-waved):**
`crates/loomweave-storage/src/guidance.rs` hardcodes a `module / class /
function` scope-level ladder. The Rust kinds have no `class`, so guidance
scoping must either (a) map the Rust kinds onto the existing ladder via the
roles table, or (b) take a small tolerance patch so an unknown kind degrades
gracefully instead of mis-scoping. The Wardline taint store filters on
`kind == "function"`, which Rust honours natively (taint-relevant entities are
functions), so no change needed there.

- `rule_id_prefix = "LMWV-RUST-"` (ADR-022: `LMWV-{PLUGIN_ID_UPPER}-`).
- `wardline_aware = false` — Wardline's trust vocabulary is Python-decorator
  shaped today; Rust trust-boundary semantics are deferred to a later phase.

## 4. Identity & SEI stability (risk centerpiece)

Entity IDs are `{plugin_id}:{kind}:{canonical_qualified_name}` (ADR-003,
ADR-022). CLAUDE.md flags SEI stability as a **cross-product contract** —
Filigree issue associations and Wardline taint both key on the entity ID, so an
ID that churns on benign edits silently breaks those bindings. This section is
first-class, not a footnote.

### 4.1 Qualname canonicalization

**The qualname may not contain a literal `:`** (the reserved segment
separator — `entity_id.rs` rejects it at assembly). Rust's native `::` paths
are therefore illegal. Canonical scheme: **dot-separated module path**,
mirroring the Python plugin's dotted qualnames.

```
rust:function:mymod.helper
rust:struct:mymod.Widget
rust:function:mymod.Widget.render        # inherent method
```

### 4.2 Anonymous `impl` blocks

`impl` blocks have no source name and must be given a synthesized, stable
identity. **Generic arguments are part of the key** — `impl<T> Foo<T>` and
`impl Foo<i32>` are distinct impls on the same target type.

```
rust:impl:mymod.Foo.impl[Display]        # trait impl — keyed by trait
rust:impl:mymod.Foo.impl#<T>             # inherent impl<T> Foo<T>
rust:impl:mymod.Foo.impl#i32             # inherent impl Foo<i32>
```

Trait impls disambiguate cleanly by trait name. Inherent impls are keyed by
their **concrete generic signature**, never by source order or method-set.

### 4.3 The stability invariant (dedicated test)

> Reordering two `impl Foo` blocks, or adding/removing a method in one, **must
> not change the entity ID of any other method.**

Methods are the `callable` entities that `calls`-edges, summaries, Filigree
associations, and Wardline taint all point at. If a source reorder churns
method IDs, every cross-product binding on that type breaks. This invariant
gets a dedicated regression test (§7).

### 4.4 SEI signatures (Phase 1 tail)

Per ADR-038, function/struct/trait/impl entities emit a versioned `signature`
object stored verbatim by core and compared by string equality as the
move-matcher's input. Modules abstain (the move case fails closed for them, as
in Python).

```toml
[signature]
schema_version = 1

[signature.schemas.function]
v = 1
fields = ["params", "return_ann", "generics"]

[signature.schemas.struct]
v = 1
fields = ["fields"]          # or {} for unit/tuple structs

[signature.schemas.trait]
v = 1
fields = ["supertraits"]

[signature.schemas.impl]
v = 1
fields = ["target", "trait"]
```

## 5. `syn` blind spots (stated limitations)

Documented honestly so they are not mistaken for bugs:

- **Macro-generated entities are invisible.** `#[derive(...)]`, proc-macros,
  and `macro_rules!` expansions produce code `syn` never sees as expanded
  items. A real coverage gap — entities that exist only after macro expansion
  will not appear in the graph. (The `derives` edge captures the *invocation*,
  not the generated impl body.)
- **All `#[cfg(...)]` variants are visible** regardless of active
  features/platform. For code archaeology this is **desirable** — you want the
  whole graph across all configs, not just the host platform's slice. Stated so
  it is not read as double-counting.

## 6. Phasing

Each phase is independently useful and mergeable to the active rcN branch.

- **Phase 1 — entities + Resolved structural edges.** All 10 entity kinds;
  `contains` + `imports` + `implements` + `derives`; the §2.3 init-time symbol
  table; SEI signatures (§4.4). Loomweave indexes its own workspace end-to-end.
  No fuzzy resolution — correct-by-construction.
- **Phase 2 — `calls` + `references`.** Same-name candidates as Ambiguous;
  unresolved sites reported to core via the existing `unresolved_call_sites`
  channel for query-time Inferred resolution. This is where the genuinely hard,
  bug-prone resolution lives, quarantined from the Phase 1 dogfooding win.
- **Phase 3 (optional, later) — rust-analyzer enrichment.** Upgrade
  Ambiguous/Inferred edges to Resolved using rust-analyzer as an opt-in
  semantic backend (gated, never on the build-free `analyze` path). Strictly
  additive thanks to the confidence ladder.

## 7. Testing

- **syn-extraction unit tests** over Rust fixture snippets — one per entity
  kind and per Phase 1 edge kind.
- **Entity-ID parity:** satisfy `fixtures/entity_id.json` byte-for-byte.
- **Identity stability:** a dedicated regression test for the §4.3 invariant
  (reorder impls / mutate method-set → other method IDs unchanged).
- **Host-integration test:** plugin handshake → `analyze_file` → `shutdown`,
  modelled on the existing fixture-plugin host integration tests.
- **E2E smoke:** analyse a slice of the real Loomweave Rust workspace and
  assert expected entities/edges appear (extends the `tests/e2e/` pattern).
- All Phase 1 work clears the CLAUDE.md CI floor (`fmt`, `clippy -D warnings`,
  `build`, `nextest`, `cargo doc -D warnings`, `cargo deny`).

## 8. Open questions / deferred

- Exact external-target representation (drop vs. external-reference marker) for
  out-of-project `implements`/`derives`/`imports` targets — settle in the Phase
  1 plan; it is the seam where `derives` could slip to Phase 2.
- Whether the init-time symbol-table walk should share the host's file-discovery
  / skip-list logic or maintain its own (correctness is unaffected; consistency
  of the *entity* set is the concern).
- Wardline trust-boundary semantics for Rust (`wardline_aware`) — future phase.
- Whether an ADR is warranted for the syntactic-vs-semantic backend choice and
  the Rust qualname canonicalization scheme (likely yes for the latter, given
  SEI is a cross-product contract).
