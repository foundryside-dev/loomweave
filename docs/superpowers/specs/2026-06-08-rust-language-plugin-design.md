# Rust language plugin — design

- **Status:** draft (brainstormed 2026-06-08; revised after 6-agent design review 2026-06-08)
- **Branch:** `feat/rust-plugin-spec` (based on `rc3`)
- **Plugin id:** `rust` · **language:** `rust` · **extensions:** `["rs"]`
- **Backend:** `syn` (syntactic; build-free is a design-derived rule — see §1)
- **Identity scheme:** **ADR-049** (qualname canonicalization + backend choice)
- **Scope:** v2.0+ second language plugin (CLAUDE.md marks non-Python languages as v2.0+)

## 1. Scope & motivation

A first-party Rust **language plugin** — the syntactic sibling of the existing
pyright-backed Python plugin. It extracts entities and edges from `.rs` source
and reports them to the plugin host over the existing JSON-RPC 2.0 protocol.

Motivating use case: **Loomweave indexes its own Rust workspace.** Today the
core ships a Python plugin but cannot analyse the Rust crates that make up
Loomweave itself; this closes that gap and makes the tool dogfoodable on its
own source.

**Hard constraint:** `analyze` stays **network-free and credential-free** — the
inherited, non-negotiable CLAUDE.md "Local-first" posture (the only required
egress is the LLM provider during `summary`; `analyze` runs with no
credentials). From that this design *derives* its own operating rule:
**build-free and Cargo-metadata-free** — the plugin must never invoke `cargo`,
perform `Cargo.lock` registry resolution, download a toolchain, or require the
target project to compile. ("Build-free" is this design's coinage, not a
CLAUDE.md term — the binding inherited constraint is network/credential-free;
`cargo metadata`'s registry/index resolution *is* network egress, which is what
forecloses rust-analyzer.) This is why the backend is syntactic (`syn`) rather
than semantic (rust-analyzer) — see §2.2. The decision is recorded in
**ADR-049**. (ADR-002 governs only the JSON-RPC *transport*; the sandbox/posture
authority is CLAUDE.md and the host's `setrlimit`/breaker.)

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
  install` drops the compiled binary on PATH. **The manifest declares the
  operational NFRs the Python manifest declares (M6):** `expected_max_rss_mb`
  and `expected_entities_per_file`, each with a *measured* basis per ADR-035 —
  RSS is load-bearing here because §2.3 holds a whole-workspace symbol table in
  memory, so its budget can't be hand-waved.

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
- **Cost note (M4):** the table is rebuilt **fresh on every plugin spawn** — the
  init walk parses every file once, then `analyze_file` parses each again. This
  partly defeats the host's incremental/warm single-file optimization (a warm
  run still pays a whole-tree walk). For Loomweave-scale workspaces this is
  cheap; the Phase-1 plan must *quantify* the per-spawn cost and RSS (feeds the
  §2.1 manifest NFRs) and cross-reference the ADR-043/045 re-analysis lifecycle.
  If it ever matters, the init parse can be cached and reused — not a Phase 1
  concern beyond measuring it.
- **Double-parse staleness (M5):** if a file changes *between* the init walk and
  its `analyze_file`, a "Resolved" cross-file edge could point at a qualname that
  no longer exists → an FK violation (`edges` `REFERENCES … ON DELETE CASCADE`
  with `foreign_keys` ON), not a graceful drop. Mitigation: when the init-walk
  snapshot and the `analyze_file` snapshot of a target differ, **downgrade the
  cross-file edge to Inferred** (or route it via `unresolved_call_sites`) rather
  than emit a dangling Resolved edge. Settle the exact mechanism in the Phase 1b
  plan.

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
| `contains` | **1a** | Resolved | `module`→item, `module`→method, file-module→items. Purely structural — genuinely Resolved-by-construction. **Moved into Phase 1a:** ADR-026 dual-encoding requires that every entity `parent_id` have a matching `contains` edge, or the storage writer (`parent_contains_mismatch`) hard-fails the run — so `parent_id` and `contains` ship together. In 1a, methods are parented to their enclosing **module** (the impl `entity` is 1b); Phase 1b re-parents methods to impl entities (non-churning — parent_id is not the locator). |
| `imports` | 1b | Resolved (restricted) / Ambiguous | `use` statements. **Only the explicit, non-glob, uniquely-resolvable subset is Resolved** (`use a::b::C;` where `C` resolves to exactly one in-project entity via the §2.3 table). `use a::*;` globs, `pub use` re-exports, and aliases that syntax cannot uniquely resolve are emitted **Ambiguous**, never as a faked Resolved edge (H5). |
| `implements` | 1b | Resolved (restricted) / Ambiguous | `impl Trait for Type` → trait. Highest-value Rust edge ("who implements X"). Resolved when the trait path resolves to exactly one in-project trait via §2.3; aliased/re-exported trait paths that don't uniquely resolve from syntax are Ambiguous (H5). External traits → external ref, not faked. |
| `derives` | 1b | Resolved (in-project) / external-ref | `#[derive(...)]` → trait. Same resolution as `implements`. **Reality note (L4):** in practice nearly all `#[derive]` targets are `std`/external (`Debug`, `Clone`, `serde::*`) → external-ref or dropped; in-project resolved derives are rare. Captures the *invocation*, not the macro-generated impl body (§5). **Escape hatch:** if the external-target representation (open Q1) complicates it, `derives` slips to Phase 2 with no loss to the Phase-1a/1b dogfood win. |
| `calls` | 2 | Ambiguous + Inferred | same-name candidates Ambiguous; unresolved sites handed to core via `unresolved_call_sites` for query-time Inferred resolution (the same channel pyright's unresolved calls feed — core suffix-matches at query time, so syn's only true loss vs pyright is the *Resolved tier* on semantically-resolvable calls, not the calls themselves). |
| `references` | 2 | Ambiguous + Inferred | type/path mentions, same channel as `calls` |

`contains` is the only unconditionally Resolved-by-construction edge. `imports`
and `implements` are Resolved **only** on their uniquely-resolvable syntactic
subset and otherwise Ambiguous — syn gives the AST but not name resolution, so
globs/re-exports/aliases cannot be promoted to Resolved without lying to
downstream tools that treat Resolved as ground truth (H5). The genuinely hard
call/reference resolution is quarantined to Phase 2.

### 3.3 Roles & the guidance integration fix

```toml
[ontology.roles]
file_scope = ["module"]
callable   = ["function"]
```

**Scope-level ladder — NO integration fix required (review-corrected).** An
earlier draft claimed `guidance.rs` hardcodes a `module/class/function` ladder
that Rust kinds would break. The review verified this is wrong on two counts:
(1) the `scope_rank` CASE map lives in **SQL migration `0001:254-263`**, not
`guidance.rs`; (2) **no plugin/analyze/writer path ever emits `scope_level`** —
`grep` shows every `scope_level` write is in guidance/MCP/CLI paths, and
`guidance.rs:20` explicitly says "Never set the generated columns." A code
entity of an unknown kind therefore gets `scope_rank = NULL` and **degrades
gracefully** — the scope-rank index is `WHERE scope_rank IS NOT NULL`, so such
rows are simply absent from scope-ordered guidance queries, not mis-scoped or
crashed. There is nothing to patch in Phase 1.
- *Residual (deferred, not a Phase 1 task):* if guidance *sheets* are ever
  authored against Rust entities, there is an operator-vocabulary gap (Rust has
  no `class`) — that is an ADR-024 follow-up, tracked in §8, not code here.

**Wardline taint — rationale corrected (review-verified).** An earlier draft
said the taint store "filters on `kind == function`, which Rust honours
natively, so no change needed." That is **false**: the production resolver
`resolve_wardline_qualnames` (`wardline_taint.rs:68`) builds candidates through
`function_candidate` (`:53`), which **hardcodes** `format!("python:function:{qualname}")`
— a `rust:function:…` id can never match it (the `kind=="function"` filter that
draft saw is in a test helper, not the production path). The deferral *conclusion*
is unchanged but the *reason* is: Rust taint is deferred because
`wardline_aware = false` **and** because `resolve_wardline_qualnames` needs a
`plugin_id`-generic refactor before any non-Python id can resolve.

- `rule_id_prefix = "LMWV-RUST-"` (ADR-022: `LMWV-{PLUGIN_ID_UPPER}-`).
- `wardline_aware = false` — Wardline's trust vocabulary is Python-decorator
  shaped today; Rust trust-boundary semantics are deferred to a later phase.

## 4. Identity & SEI stability (risk centerpiece)

Entity IDs are `{plugin_id}:{kind}:{canonical_qualified_name}` (ADR-003,
ADR-022). Under SEI (ADR-038) that id is the **locator** — the carry-or-mint
matcher keys on it and `sei.rs:600` enforces a partial-unique index on
`current_locator WHERE status='alive'`. CLAUDE.md flags SEI stability as a
**cross-product contract** — Filigree issue associations and Wardline taint both
key on it, so an id that churns on benign edits silently breaks those bindings.
**And a *collision* is worse than churn:** the writer upserts entities
`ON CONFLICT(id) DO UPDATE` (`writer.rs:570`) with no uniqueness guard at
assembly (`entity_id.rs` validates only the grammar and the `:` ban), so two
distinct source items that produce the same qualname **silently merge** —
intra-run data loss. This section is first-class, and its scheme is **fixed by
ADR-049** (`docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md`); the
summary below is normative-by-reference to that ADR.

### 4.1 Qualname canonicalization (per ADR-049)

**The qualname may not contain a literal `:`** (the reserved separator —
`entity_id.rs:141` rejects it). Rust's native `::` is therefore illegal. The
canonical qualname is a **dot-separated, crate-rooted, impl-discriminated path**
that is **unique within a run** and **stable across benign edits**:

```
rust:struct:loomweave_core.config.Widget            # crate token closes cross-crate collisions
rust:function:loomweave_core.config.helper
rust:function:loomweave_core.config.Widget.impl#<>.render   # inherent method, carries its impl discriminator
```

- **Crate token (closes the cross-crate collision).** Leading segment is the
  crate name (read as *text* from each `Cargo.toml [package].name` within
  `project_root` — permitted; the hard constraint forbids `cargo metadata`
  registry resolution, not reading a manifest file — falling back to the dir
  holding `src/lib.rs`/`src/main.rs`). Without it, `loomweave_core::config::X`
  and `loomweave_cli::config::X` both collapse to `config.X` and the second
  overwrites the first on the 8-crate dogfood target.
- **Module path** follows `mod foo;` file boundaries. `#[path=...]`-mounted
  modules route to their **mounted** logical path (implemented — ADR-049
  Amendment 8): a targeted mount overlay (exact mounted file + mounted-subtree
  prefixes, twin mounts split by `@cfg`) with the pure filesystem derivation
  as the default for every unmounted file. Macro-wrapped mounts are invisible
  (filesystem fallback); out-of-src targets are ignored.

### 4.2 `impl` blocks and member methods (closes intra-type collisions)

`impl` blocks have no source name and get a synthesized, **source-order-independent**
discriminator; **methods carry their impl's discriminator** so same-named methods
on the same type never collide:

```
rust:impl:loomweave_core.config.Foo.impl[Display]      # trait impl — keyed by trait
rust:impl:loomweave_core.config.Foo.impl[From<i32>]    # trait generic args ARE in the key
rust:impl:loomweave_core.config.Foo.impl#<$0>          # inherent impl, positional generics (no ordinal — same-key impls merge)
rust:function:loomweave_core.config.Foo.impl[Display].fmt   # Display::fmt, distinct from…
rust:function:loomweave_core.config.Foo.impl[Debug].fmt     # …Debug::fmt
```

- **Trait impls** key by the trait path **with its concrete generic arguments**
  (`impl[From<i32>]` ≠ `impl[From<u32>]`).
- **Inherent impls** key by a **positional, De Bruijn-style** generic signature
  so renaming `<T>`→`<U>` (a benign edit) does **not** churn the id. There is
  **no source-order ordinal** (amended 2026-06-09, Option (b)): multiple inherent
  impls of the same `(type, positional-generic-signature, cfg)` **merge** into one
  `impl` entity — the union of their methods all hang off that single entity —
  making the discriminator both reorder-stable and method-set-stable. Inherent
  blocks for the same type in different files stay distinct because the defining
  file's module-path prefix already discriminates them; cfg-twin inherent (and
  trait) impls are split by the `@cfg(...)` discriminant below.
- **`#[cfg]` twins:** because §5 keeps all cfg variants visible, two same-path
  items on mutually-exclusive cfgs would collide; each appends a normalized
  `@cfg(<predicate>)` discriminant. Items with a unique path or no cfg get no
  suffix (common case unchanged).
- The `[ ] # < > @` characters are permitted by `entity_id.rs` (only `:` is
  banned); the §7 fixture audits them against downstream consumers (Wardline
  expr parser, Filigree glob, guidance `match_rules`).

### 4.3 Stability **and uniqueness** invariants (dedicated tests)

> **Stability:** reordering two `impl Foo` blocks, adding/removing a method in
> one, or renaming a generic type parameter **must not change the entity ID of
> any other entity** (and the renamed-generic entity's own id is unchanged).
>
> **Uniqueness:** over the full set the plugin emits in one run, **no two
> entities may share a locator.** This is an asserted invariant, not left to the
> writer's silent `ON CONFLICT` merge.

Methods are the `callable` entities that `calls`-edges, summaries, Filigree
associations, and Wardline taint all point at. A churned or collided method id
breaks every cross-product binding on that type. Both invariants get dedicated
regression tests over a corpus containing every collision pair ADR-049 enumerates
(§7).

### 4.4 SEI signatures (shipped in Phase 1a)

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
  it is not read as double-counting. The flip side — two same-path items on
  mutually-exclusive cfgs would collide on the locator — is closed by the
  `@cfg(...)` discriminant in §4.2 / ADR-049.

## 6. Phasing

Each phase is independently useful and mergeable to the active rcN branch.
**Phase 1 is split at the identity gate** (review H3): the qualname scheme is a
cross-product SEI contract baked into `fixtures/entity_id.json`, so it must be
proven collision-free *before* any edge keys on it.

- **Phase 1a — identity foundation (the gate). *(Implemented 2026-06-09.)***
  Crate-root discovery + `mod`-tree traversal + the ADR-049 qualname scheme
  (crate token, impl/trait method discriminator, positional generics, `@cfg`
  twins, inherent-impl merge — Option (b), no ordinal); the §2.3 init-time symbol table that the same
  walk feeds; a starter entity set (`module`, `struct`, `function`); **the
  `contains` edge** (pulled in from 1b — ADR-026 dual-encoding requires it to
  accompany `parent_id`, else the storage writer fails the run; SEI signatures
  also landed here); and the §4.3 **stability *and* uniqueness** tests. **Exit
  gate: proven zero-collision over Loomweave's own workspace** (built: a
  symbol-table uniqueness test over `crates/`, 2,836 entities). *Known residual:
  a full `loomweave analyze` CLI→writer E2E awaits the install-path PATH staging
  (a deferred cross-cutting follow-up); the writer's dual-encoding check is
  guarded by a unit test mirroring it + a host-integration roundtrip.*
- **Phase 1b — remaining entities + the resolving edges.** The other 7 entity
  kinds + the `impl` entity (re-parenting methods off the module onto their
  impl); `imports`/`implements` (Resolved on the uniquely-resolvable subset,
  else Ambiguous — §3.2 H5) + `derives`; the full `loomweave analyze`→writer
  E2E. (`contains` and SEI signatures already shipped in 1a.) Loomweave indexes
  its own workspace end-to-end. No fuzzy resolution beyond the declared
  Ambiguous envelope.
- **Phase 2 — `calls` + `references`.** Same-name candidates as Ambiguous;
  unresolved sites reported to core via the existing `unresolved_call_sites`
  channel for query-time Inferred resolution. This is where the genuinely hard,
  bug-prone resolution lives, quarantined from the Phase 1 dogfooding win.
- **Phase 3 (optional, later) — rust-analyzer enrichment.** Upgrade
  Ambiguous/Inferred **edge confidence** to Resolved using rust-analyzer as an
  opt-in semantic backend (gated, never on the build-free `analyze` path).
  **Additivity is bounded (review H4):** it is strictly additive *only* for
  edge-confidence on already-existing entities. rust-analyzer also reveals
  macro-generated entities (§5) as **new** entity ids — that changes the SEI
  entity *set* (churn, not free enrichment), so any such entity must go through
  the same ADR-049 qualname + SEI + parity-fixture contract. RA must reproduce
  §4 byte-for-byte or it forks every id.

## 7. Testing

- **syn-extraction unit tests** over Rust fixture snippets — one per entity
  kind and per Phase 1 edge kind, including the edge cases the review surfaced:
  trait impls for generic types, multiple inherent impls, glob `use a::*` and
  `pub use` re-exports (assert they land Ambiguous, not faked Resolved — H5),
  cfg-gated duplicate items, nested modules.
- **Entity-ID parity (non-vacuous — H1):** satisfy `fixtures/entity_id.json`
  byte-for-byte. The fixture is **extended before implementation** with rows
  exercising the crate token, trait-impl-method (`impl[Trait].m`),
  inherent-impl-method, multiple-inherent-impl merge (Option (b)), positional-generic
  stability, and a `@cfg` twin — the language-neutral assembler makes the lone
  trivial row pass today, so it must grow to actually gate the ADR-049 scheme.
- **Identity stability & uniqueness** (`crates/loomweave-plugin-rust/tests/identity_stability.rs`
  and `…/identity_uniqueness.rs`): the §4.3 invariants — stability (reorder
  impls / mutate method-set / rename a generic param → no *other* id changes,
  renamed-generic id itself unchanged) **and** uniqueness (zero duplicate
  locators over a corpus containing every ADR-049 collision pair). The
  uniqueness corpus is the load-bearing regression for B1/B2.
- **SEI signatures** (`…/tests/sei_signatures.rs`): assert the §4.4 per-kind
  signature objects are emitted and stable.
- **Degraded parse (M3):** on `syn::parse_file` failure the plugin emits a
  single `module` entity flagged degraded + a Warning finding — never an empty
  list or a panic (Python's established pattern). A malformed-`.rs` test asserts
  this.
- **Host-integration test:** plugin handshake → `analyze_file` → `shutdown`,
  modelled on the existing fixture-plugin host integration tests.
- **E2E (golden-snapshot, not subset — H2):** the acceptance gate is a
  golden-snapshot test over a **small vendored fixture workspace** pinning the
  exact entity-id set and edge set, so any id change, dropped edge, or two
  entities merging to one id **fails** (a monotonic "expected appears" subset
  check cannot detect a collision). The dogfood run over the real Loomweave
  workspace is a **smoke** test, not the derives-edges acceptance gate — note
  that the dogfood graph is derive-dense and §5 makes macro impls invisible, so
  `implements`/`derives` edges will be sparse there by design. E2E lives at
  `crates/loomweave-cli/tests/` (e.g. alongside `wp2_e2e.rs`), not a top-level
  `tests/e2e/` directory.
- All Phase 1 work clears the CLAUDE.md CI floor (`fmt`, `clippy -D warnings`,
  `build`, `nextest`, `cargo doc -D warnings`, `cargo deny`).

## 8. Open questions / deferred

**Resolved during review (2026-06-08):**

- **ADR for the qualname scheme & backend — done.** The qualname canonicalization
  (cross-product SEI contract) *and* the syntactic-vs-semantic backend choice are
  now settled in **ADR-049** (`docs/loomweave/adr/ADR-049-rust-qualname-canonicalization.md`).
  §4 is normative-by-reference to it. This closes the prior open question.

**Resolved during Phase 1b (2026-06-09; authoritative record is ADR-049 + the
Phase-1b commits — noted here only to close the loop):**

- **External-target representation (Q1) — drop-external (D1).** Out-of-project
  `implements`/`imports` targets are dropped, not faked as external refs.
- **Init-walk entity-set consistency (D2/D3) — host seen-entity-set gate.** The
  host gates emitted entities against the seen-entity set rather than the plugin
  re-deriving the file-discovery/skip-list logic.
- **Inherent-impl ordinal — resolved by merge (Option (b)).** No source-order
  ordinal; same-`(type, positional-generics, cfg)` inherent impls merge into one
  `impl` entity (see the ADR-049 item-path discrimination section).

**Deferred (NOT walked / NOT implemented in Phase 1b):**

- **trait BODY items are NOT walked as entities.** Only the `trait` item itself
  is emitted; trait methods and associated consts/types inside the trait body
  are deferred (the trait-item walk discards the body).
- **`#[path = "…"]` module-file overrides — IMPLEMENTED** (ADR-049
  Amendment 8, post-Phase-1b): module-path derivation consults the `#[path]`
  mount overlay first and falls back to the default file path for unmounted
  files (per §4.1).
- **Host `RLIMIT_STACK`/`RLIMIT_CPU` + `syn` recursion-depth hardening — tracked
  SEPARATELY.** This is a host-hardening item protecting *all* plugins from
  adversarial/pathological inputs, not Rust-specific, and is out of Phase 1b
  scope.

**Still open (settle in the Phase 1 plan):**

- The Phase 1b cross-file-edge staleness mechanism (M5 in §2.3) — downgrade-to-
  Inferred vs. route-via-`unresolved_call_sites`.

**Documented known-limitations (not blockers; carry into Phase 1 docs):**

- **Subsystem attribution is Python-seeded (M1).** Subsystem-membership queries
  (`query.rs:1247/1285/1327/1366`) hardcode `kind='module'`; Rust *does* use kind
  `module`, but membership is seeded by Python qualnames, so a Rust-only project
  gets no subsystem attribution until those queries are made plugin-agnostic.
  Pre-existing architectural constraint, not a Rust-plugin defect.
- **`entity_at` span-ordering tiebreak (M2).** `query.rs:618-623/665-670` hardcode
  a `function/class/module` CASE as a span-containment **tiebreaker** (after
  span-length, before `id ASC`); Rust `struct`/`impl`/`trait` fall to `ELSE 3`,
  ordered after equal-span function/class/module. Result stays **deterministic**
  — a minor `entity_at` mis-prioritization, polish only, distinct from the
  `scope_rank` non-issue in §3.3.
- Wardline trust-boundary semantics for Rust (`wardline_aware`) — future phase;
  also requires the `resolve_wardline_qualnames` `plugin_id`-generic refactor
  noted in §3.3.
- Guidance-sheet operator vocabulary for Rust (no `class`) — ADR-024 follow-up
  if/when Rust guidance sheets are authored (§3.3).
