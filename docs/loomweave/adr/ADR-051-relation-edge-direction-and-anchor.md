# ADR-051: Relation Edge Direction and Anchor Semantics (`inherits_from`, `decorates`)

**Status**: Accepted
**Date**: 2026-06-10
**Deciders**: john@foundryside.dev
**Context**: clarion-43416be550 makes the Python plugin the first emitter of the `inherits_from` and `decorates` edge kinds. ADR-026 locked their *source-range* contract (anchored: `source_byte_start/end` MUST be `Some`) but never specified edge **direction**, and its decision-3 table cell for `decorates` ("the decoration target location") reads differently from its own rationale prose ("the `@decorator_name` line"). The historic design-ladder sketch named the kind `decorated_by` — the opposite reading. The first emitter forces both questions; this ADR records the answers so the second emitter (a future language plugin) cannot re-derive them differently.

## Summary

`inherits_from` runs subclass → base (`from_id` = the declaring subclass, `to_id` = the resolved base class). `decorates` runs decorator → decorated (`from_id` = the resolved decorator entity, `to_id` = the decorated entity) — the kind name is read as a sentence, superseding the `decorated_by` spelling in the pre-0.8.0 design ladder. Both kinds anchor the **reduced dotted path token** of the base/decorator expression (Rust `implements`/`derives` parity: the path, not the whole statement; factory-call arguments excluded), resolving ADR-026's table-cell wording in favour of its rationale prose. Both kinds resolve to **precise entities only** — no module-id coarse fallback — with a class-kind filter on `inherits_from` targets and a self-edge drop on both.

## Context

- ADR-026 decision 3 fixes nullability per kind but not direction. Every shipped anchored kind so far happened to run *declaring entity → resolved target* (`calls`, `imports`, `references`, `implements`, `derives`), a convention written nowhere normative.
- The Python relation token sits in the *decorated* entity's declaration (`@deco` above `def handler`), so a "declaring entity → target" reading would produce `decorated_by` semantics (decorated → decorator). The ontology kind that shipped in `ANCHORED_EDGE_KINDS` (`crates/loomweave-storage/src/writer.rs`) is spelled `decorates`, and the headline consumer question is "what handlers does `@app.route` decorate" — a forward traversal from the decorator.
- Ambiguous anchored edges carry `properties.candidates` (ADR-028 reading: alternative best-guess endpoints). For an inverted kind the candidates are necessarily *from*-side.

## Decision

1. **Direction.** `inherits_from`: `from_id` = subclass, `to_id` = base class. `decorates`: `from_id` = decorator entity (function or class), `to_id` = decorated entity. Edge kinds are read as sentences: `from KIND to` must parse as English. `decorates` therefore inverts relative to the site that anchors it; the anchor's file is the *to*-side entity's file. The `decorated_by` name in pre-0.8.0 requirements/design sketches is superseded.
2. **Anchor.** The byte span is the reduced dotted path token of the base/decorator expression: `Name` and `Attribute` anchor their own span (`helpers.Base` anchors `helpers.Base`); `Subscript` reduces to its value (`Generic[T]` → `Generic`); decorator factory `Call`s reduce to their callee (`@app.route("/x")` anchors `app.route`); the `@` sigil and call arguments are excluded. This matches the Rust `implements`/`derives` precedent (the trait *path* token) and resolves ADR-026's `decorates` table cell ("the decoration target location") in favour of its rationale prose ("the `@decorator_name` line" — i.e. the decorator path token at the decoration site).
3. **Resolution discipline.** Relation sites resolve to precise entity positions only — the module-id coarse fallback used by `references` is disabled (an aliased base resolving to an assignment yields *no* edge, not a `class inherits_from module` fact). `inherits_from` targets are filtered to `class`-kind entities (Rust parity: `resolve_trait_path` filters on `rust:trait:`). Self-edges (`from_id == to_id`) are dropped for both kinds. Expressions with no stable path token (call bases like `class X(make())`, lambdas, conditionals) emit no site: the call's *result* is the base, and anchoring the callee would assert a false inheritance fact.
4. **Ambiguity payload.** Ambiguous relation edges carry `properties.candidates` like `references`; for `decorates` the candidates are alternative **from-side** decorator entities (direction-inverted relative to every other kind). Consumers generalising candidate expansion across kinds must branch on this.

## Alternatives Considered

### Alternative 1: `decorated_by` (decorated → decorator), keeping the declaring-entity-as-`from` convention

**Pros**: Uniform "the anchor lives in `from_id`'s file" invariant across all anchored kinds; matches the pre-0.8.0 design-ladder sketch.
**Cons**: The shipped ontology kind in `ANCHORED_EDGE_KINDS` is spelled `decorates`; a `decorated_by`-directed edge under that name reads backwards everywhere it is displayed; the headline query traverses from the decorator.
**Why rejected**: The kind-name-as-sentence rule is the only convention a reader can check without a spec in hand. Renaming the ontology kind instead would touch the writer contract and Rust-era ontology for a cosmetic gain.

### Alternative 2: Keep the module-id coarse fallback for relation sites (full `references` parity)

**Pros**: One uniform resolution envelope; aliased bases still produce *some* edge.
**Cons**: Mints semantically false facts (`class inherits_from module`, `module decorates function`) at `resolved` confidence; Rust's relation kinds drop unresolvable targets rather than coarsen them.
**Why rejected**: Relation kinds are stronger semantic claims than `references`; a wrong relation edge is worse than a missing one. Drops are observable (`unresolved_reference_sites_total`).

### Alternative 3: Anchor the whole decoration/base clause (statement-level span)

**Pros**: Simpler extraction (no path reduction); covers factory arguments.
**Cons**: Breaks Rust parity (trait *path* token); a consumer highlighting the anchor would highlight argument lists that are not part of the relation; ADR-026's rationale names the token, not the clause.
**Why rejected**: The path token IS the edge's textual occurrence (ADR-026 §rationale).

## Consequences

### Positive

- "What subclasses X" / "what does `@app.route` decorate" are answerable for Python with the same edge-set discipline Rust ships (`implements` + `derives`) — the launch-parity asymmetry closes.
- Direction and anchor are recorded normatively before a second emitter exists; the per-kind table in ADR-026 gains an authoritative reading.

### Negative

- `decorates` breaks two implicit invariants other kinds satisfy: the anchor is in the *to*-side entity's file, and ambiguous `candidates` are *from*-side. Both are latent traps for generic edge consumers; this ADR is the machine-checkable-adjacent record of them.
- Aliased decorators/bases and call bases produce no edge at all (precise-entity discipline). Coverage is honest but narrower than runtime truth.
- Disciplined-away resolutions (kind-filtered, self-edge-dropped) are counted in `unresolved_reference_sites_total`, indistinguishable from genuine pyright misses in run stats.

### Neutral

- Relation sites ride the existing reference-resolution machinery and counters (`reference_sites_total` etc.) and the `MAX_REFERENCE_SITES_PER_FILE` cap; the resolution-envelope audit (clarion-e9cfde2773) should enumerate these boundaries.
- Stacking order and decorator arguments are not represented; `(kind, from_id, to_id)` identity (ADR-026 decision 2) dedupes repeated decorators.

## Related Decisions

- **Related to**: [ADR-026](./ADR-026-containment-wire-and-edge-identity.md) (source-range contract this ADR completes with direction + anchor reading), [ADR-028](./ADR-028-edge-confidence-tiers.md) (confidence tiers; relation edges are `resolved`/`ambiguous` only), [ADR-027](./ADR-027-ontology-version-semver.md) (the 0.7.0 → 0.8.0 MINOR bump), [ADR-022](./ADR-022-core-plugin-ontology.md) (plugin-owned ontology).

## References

- `plugins/python/src/loomweave_plugin_python/extractor.py` (`_relation_anchor`, `_site_for_relation`) and `pyright_session.py` (`_merge_reference_site`, `_filter_relation_candidates`) — the first implementation.
- `crates/loomweave-plugin-rust/src/edges.rs` (`implements_edge`, `derives_edge`) — the path-token anchor precedent.
- Filigree clarion-43416be550 (this change), clarion-e9cfde2773 (resolution-envelope audit), clarion-12dd19c6a1 (duplicate-qualname freeze; first-wins attribution also shapes relation edges).
