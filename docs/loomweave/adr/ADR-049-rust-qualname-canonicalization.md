# ADR-049: Rust plugin qualname canonicalization and syntactic backend

**Status**: Accepted
**Date**: 2026-06-08
**Deciders**: john@foundryside.dev (with Claude)
**Context**: The forthcoming first-party Rust language plugin (`crates/loomweave-plugin-rust`, design spec `docs/superpowers/specs/2026-06-08-rust-language-plugin-design.md`) must mint entity ids of the form `{plugin}:{kind}:{qualname}` (ADR-003). Under SEI (ADR-038) that id is the **locator** — the carry-or-mint matcher keys on it, and `sei.rs:600` enforces a partial-unique index on `current_locator WHERE status='alive'`. A multi-agent design review (2026-06-08) verified two locator-collision families in the draft qualname scheme that the writer would silently merge (`writer.rs:570` is `ON CONFLICT(id) DO UPDATE`), plus a benign-edit churn case. Because SEI is a cross-product contract (Filigree associations, Wardline taint key on it) and the scheme has no Python precedent, it is settled here before any code lands.
**Relates to**: [ADR-003](./ADR-003-entity-id-scheme.md) (the `{plugin}:{kind}:{qualname}` scheme this canonicalizes for Rust), [ADR-022](./ADR-022-core-plugin-ontology.md) (`rule_id_prefix`, plugin-owned ontology), [ADR-028](./ADR-028-edge-confidence-tiers.md) (the confidence ladder the syntactic backend is honest about), [ADR-038](./ADR-038-sei-token-and-signature.md) (the qualname is the SEI *locator*; this ADR keeps it collision-free within a run), [ADR-002](./ADR-002-plugin-transport-json-rpc.md) (JSON-RPC transport only).

## Summary

Two decisions:

1. **Qualname canonicalization (the load-bearing one).** A Rust entity's `canonical_qualified_name` is a **dot-separated, crate-rooted, impl-discriminated path** that is (a) **unique within a run** — no two distinct source items collapse to one locator — and (b) **stable across benign edits** — reordering items, adding/removing a sibling, or renaming a generic type parameter does not churn an unrelated entity's id. This directly closes the cross-crate collision (no crate token) and the intra-type collisions (missing method/impl discriminator) the review found.

2. **Syntactic backend (`syn`), recorded.** The plugin parses with `syn`, not rust-analyzer. The binding reason is the project-wide **network-free / credential-free** posture (CLAUDE.md "Local-first"): `cargo metadata` / registry resolution is network egress, and a buildable-project requirement pulls a toolchain into `analyze`. rust-analyzer is admissible **later** only as strictly-additive *edge-confidence* enrichment (ADR-028 ladder), and only if it reproduces this ADR's qualname byte-for-byte.

## Context

### The locator must be collision-free within a run

Verified against source on 2026-06-08:

- `crates/loomweave-storage/src/writer.rs:570` upserts entities `ON CONFLICT(id) DO UPDATE SET …` — a second entity with the same id **overwrites** the first's byte range, content hash, and parent. There is no uniqueness guard at id assembly: `crates/loomweave-core/src/entity_id.rs` validates the `[a-z][a-z0-9_]*` grammar for `plugin_id`/`kind` only and rejects a literal `:` in the qualname (line 141) — nothing else. So a colliding qualname is *silent intra-run data loss*, not an error.
- `crates/loomweave-storage/src/sei.rs:600` enforces a partial unique index on `current_locator WHERE status='alive'`. The SEI matcher (ADR-038) carries-or-mints by reading the prior binding **keyed on the locator**; two alive entities sharing a locator break that index and confuse the matcher.

The draft scheme (spec §4.1/§4.2 as first written) collided in idiomatic Rust:

| Colliding source | Draft locator (both) | Why it collides |
|---|---|---|
| `loomweave_core::config::X` + `loomweave_cli::config::X` | `rust:struct:config.X` | no crate token (8-crate dogfood target) |
| `impl Display for Foo { fn fmt }` + `impl Debug for Foo { fn fmt }` | `rust:function:…Foo.fmt` | method qualname had no impl/trait discriminator |
| `impl From<i32> for Foo` + `impl From<u32> for Foo` | impl: `…Foo.impl[From]` | trait's generic arg omitted from key |
| two inherent `impl Foo {}` blocks | `…Foo.impl#` | empty generic sig, no ordinal |
| `#[cfg(unix)] fn f` + `#[cfg(windows)] fn f` | `…mod.f` | §5 all-cfg-visible policy makes this *guaranteed* |

And one benign-edit **churn** case: the generic type-parameter *name* was in the key (`impl#<T>`), so renaming `<T>`→`<U>` — a no-op edit — churned the id, orphaning bindings.

### What rust-analyzer would cost the analyze path

CLAUDE.md ("Local-first"): the only required network egress is the LLM provider during `summary`; `analyze` runs with no credentials. rust-analyzer needs Cargo metadata (registry/index resolution = network) and a buildable project (toolchain). That is the binding exclusion. RAM/index cost is real but secondary. ("Build-free" is this design's own derived rule, not an inherited CLAUDE.md term — the inherited terms are network-free and credential-free.)

## Decision

### 1. Qualname canonicalization

A Rust entity's `canonical_qualified_name` is `<crate>.<module-path>.<item-path>`, dot-separated, where:

**Crate token (closes B1).** The leading segment is the crate name (underscored, e.g. `loomweave_core`). Crate roots are discovered by reading each `Cargo.toml`'s `[package].name` **as text** within `project_root` (permitted — the hard constraint forbids running `cargo metadata`/registry resolution, not reading a manifest file), falling back to the directory containing `src/lib.rs` / `src/main.rs`. A virtual workspace with no member resolves crate-by-nearest-manifest.

**Module path.** The `mod` tree from crate root to the item, honoring `#[path = "…"]` and file-module (`mod foo;` → `foo.rs`/`foo/mod.rs`) boundaries. Inline `mod` blocks nest normally.

**Item path with impl discrimination (closes B2).**

- Free items: `<crate>.<mods>.<name>` (function, struct, enum, trait, type_alias, const, static, macro).
- `impl` blocks get a synthesized, source-order-independent discriminator appended to the target type:
  - **Trait impl:** `…<Type>.impl[<TraitPath-with-concrete-generics>]` — the trait's concrete generic arguments are **part of the key** (`impl[From<i32>]`, `impl[From<u32>]` are distinct).
  - **Inherent impl:** `…<Type>.impl#<concrete-generic-signature>`, with a **positional, De Bruijn-style** rendering of generic parameters so a param *rename* does not churn (`impl<T> Foo<T>` and `impl<U> Foo<U>` render identically). Multiple inherent impls with an identical signature get a stable **ordinal** discriminant (`#0`, `#1`, …) assigned by source order *within a file*, and — because two inherent blocks for the same type in different files is legal — disambiguated further by the defining file's module path already present in the prefix.
  - The `[`, `]`, `#`, `<`, `>` characters are permitted by `entity_id.rs` (only `:` is rejected). A fixture row MUST exercise each so downstream consumers (Wardline expr parser, Filigree glob, guidance `match_rules`) are audited against them (review finding L5).
- **Methods carry their impl's discriminator:** a method is `…<Type>.impl[<Trait>].<method>` / `…<Type>.impl#<sig>.<method>`, never the bare `…<Type>.<method>`. This is what makes `Display::fmt` and `Debug::fmt` on the same type distinct.

**`#[cfg]` collision policy (closes the guaranteed B2 case).** Because §5 of the spec keeps *all* cfg variants visible, two same-path items gated on mutually-exclusive cfgs would collide. Each cfg-gated item that shares a path with a sibling appends a canonicalized `@cfg(<predicate>)` discriminant derived from its `#[cfg(...)]` attribute (normalized: sorted, whitespace-stripped). An item with no cfg, or a unique path, gets no `@cfg` suffix — so the common case is unchanged and only genuine cfg-twins pay the discriminator.

**Uniqueness is an invariant, not a hope.** The plugin asserts, over its emitted set per run, that **no two entities share a locator**. This is enforced by a dedicated uniqueness test class (below), not left to the writer's silent merge.

### 2. Syntactic backend

`syn` (full-fidelity typed AST, `proc-macro2` spans). rust-analyzer is excluded from the `analyze` path for the network/credential reason above and admitted only as a future, gated, strictly-additive Phase-3 *edge-confidence* enrichment that MUST reproduce §1's qualname byte-for-byte (any newly-revealed macro entity goes through this same locator + SEI + parity-fixture contract — it is *not* free additivity; review finding H4).

## Consequences

- **Parity fixture.** `fixtures/entity_id.json` is extended with Rust rows exercising the crate token, trait-impl-method, inherent-impl-method, multiple-inherent-impl ordinal, positional-generic stability, and a `@cfg` twin — *before* implementation, so the byte-parity gate is non-vacuous (review finding H1).
- **Test classes.** Two dedicated test files: `identity_stability.rs` (reorder impls / mutate method-set / rename a generic param → no *other* id changes, and the renamed-generic id is itself unchanged) and `identity_uniqueness.rs` (a corpus containing every collision pair in the Context table asserts zero duplicate locators).
- **Init-time symbol table.** The crate-root + `mod`-tree discovery this ADR requires is the same walk the spec's §2.3 project symbol table performs at `initialize` (`InitializeParams::project_root`, `protocol.rs:328`); the two share one traversal.
- **SEI.** The locator stays stable across the benign edits ADR-038's matcher must tolerate (reorder, sibling add/remove, generic-param rename), so carry-not-mint holds for unaffected entities; no `entities`/`sei_bindings` shape change is implied.
- **No core change required for assembly.** The plugin calls `loomweave_core::entity_id()`; this ADR constrains only the qualname *string* the plugin hands it. The grammar/colon rules in `entity_id.rs` are unchanged.
- **Supersedes nothing.** ADR-003's scheme stands; this is the Rust-specific canonicalization of its opaque qualname segment, mirroring how the Python plugin owns its dotted qualnames.

## Weft vocabulary verdict (per ADR index acceptance rule)

No new cross-product-visible term is introduced. "locator", "SEI", and the `{plugin}:{kind}:{qualname}` shape are all pre-registered (ADR-003, ADR-038, `docs/suite/glossary.md`). The Rust qualname canonicalization is plugin-internal detail behind the opaque locator — consumers MUST NOT parse it (the opacity discipline of ADR-003/ADR-038). Verdict: **`no clash`** — nothing new to register.
