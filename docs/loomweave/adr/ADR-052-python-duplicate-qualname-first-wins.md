# ADR-052: Python Duplicate-Qualname Semantics — First-Wins, Frozen with an Additive Evolution Clause

**Status**: Accepted
**Date**: 2026-06-11
**Deciders**: john@foundryside.dev
**Context**: clarion-12dd19c6a1. The Python extractor resolves same-id collisions at the emit boundary (`plugins/python/src/loomweave_plugin_python/extractor.py`, `_walk`): PEP-484 `@overload` stubs are recognised and dropped pre-emission (the implementation, last in source order, emits normally — clarion-e29402d1ba); **every other** duplicate is deduplicated **first-wins** with a stderr line per drop and a `duplicate_entities_dropped_total` bump. This catches patterns that are not corner cases: `@property` getter/setter (and deleter) triples, `@singledispatch` users writing `def _(...)` repeatedly, conditional `def` (platform-switch `if`/`else`), and manual redefinition. Rust faced the analogous collision problem (inherent-impl twins, cfg twins) and resolved it in ADR-049 with declaration-derived discriminants plus identity_uniqueness/dogfood_uniqueness test gates — explicitly **rejecting source-order ordinals** as churn-prone. Backwards compatibility for Python entity ids starts at public launch, so the semantic must be decided and frozen now even if enrichment ships later.

## Summary

First-wins is the frozen launch semantic: the first definition in source order owns the bare qualname id; later same-id definitions are dropped entirely (entity, `contains` edge, and the whole subtree — no recursion, so nothing inside a dropped body is emitted or attributed). The freeze carries an **additive evolution clause**: any future surfacing of later duplicates MUST keep the bare qualname on the first definition and give later definitions new declaration-derived discriminated ids — never re-keying the survivor and never source-order ordinals. That makes the enrichment deferrable without an id-shape break.

## Context

- The collision is structural, not exotic. `@property`/`@x.setter` pairs share the Python qualname `C.x`; both `def`s are live at runtime (the `property` descriptor holds both), yet only one entity id `python:function:{module}.C.x` exists in the id scheme (ADR-003 has no discriminant position for them today).
- Two independent layers walk the same AST and must agree: the extractor's emit boundary (`extractor.py` `_walk`, `seen_ids`) and the pyright session's function index (`pyright_session.py` `_collect_entities`, `seen_ids`). Both apply the identical first-wins skip *including recursion suppression*, and call sites are collected per-function from each surviving function's own AST node. Consequence (verified, pinned by test): a dropped definition's body call/reference sites are **absent**, never mis-attributed to the surviving definition. Incoming resolutions that pyright points at a dropped definition's declaration position find no index entry and surface as *unresolved*, not as the survivor.
- ADR-049's amendment history is the controlling precedent for the evolution question: a source-order ordinal was dropped from the Rust dialect as self-contradictory ("assigned by source order" cannot be "source-order-independent"); the surviving discriminants are declaration-derived (`@cfg(...)`, self-type/trait paths).

## Decision

1. **First-wins is the frozen v1 semantic**, for functions and classes alike. The first definition in source order owns the bare qualname id. A later same-id definition drops: its entity, its `contains` edge, and its entire subtree (recursion suppressed — nested entities would carry a `parent_id` the host never sees, and body call/reference sites would mis-attach). Each drop writes one stderr line and bumps `ExtractionStats.duplicate_entities_dropped_total`. `@overload` stubs remain a separate, earlier rule (semantic recognition, implementation-emits).
2. **Attribution invariant.** The emit boundary and the pyright function index apply the same first-wins rule in the same source order, so the surviving entity's call/reference edges come only from its own body. A dropped definition's body is invisible — counted, never collided, never mis-attributed. This is a contract both walkers must preserve; it is pinned by unit and pyright-marked tests.
3. **Additive evolution clause (the compatibility freeze).** If later duplicates are ever surfaced as entities, the change MUST be additive: the first definition keeps the bare qualname forever; later definitions gain new ids with a **declaration-derived** discriminant (e.g. a decorator-derived suffix for `@x.setter` / `@x.deleter`, mirroring Rust's `@cfg(...)` shape). Source-order ordinals are rejected now (ADR-049 precedent: inserting or reordering a duplicate would churn every later twin's id and SEI). Under this clause the enrichment is purely additive — existing ids, SEI bindings, and Wardline/Filigree associations survive — so it can ship any time after launch without a breaking decision.
4. **Uniqueness gates (Rust parity).** Unit pins for the three real-world shapes (property setter/getter, `@singledispatch` `def _` sequences, conditional `def`) plus a dogfood test running the extractor over `plugins/python` itself asserting zero duplicate ids emitted across the whole tree (drops counted, never collided) — the Python analogs of `identity_uniqueness.rs` / `dogfood_uniqueness.rs`.

## Alternatives Considered

### Alternative 1: discriminate setters now (`…C.x@setter`)

**Pros**: setter bodies visible in the launch graph; properties are ubiquitous.
**Cons**: an id-shape change to the Python dialect days before the compatibility freeze — ontology bump, fixture re-vendor, Wardline/Filigree SEI consumers all move in lockstep. The additive evolution clause (decision 3) buys the same end state later at zero breakage, which is the whole point of deciding now.

### Alternative 2: last-wins

**Pros**: matches runtime shadowing for conditional `def` and manual redefinition.
**Cons**: wrong for properties (both `def`s are live; "last" is arbitrary there); inverts the already-shipped behavior, churning content hashes and source spans for every existing store; the one case where "last is the real one" genuinely holds (`@overload`) is already handled by the dedicated stub rule.

### Alternative 3: merge duplicates into one entity (Rust inherent-impl precedent)

**Pros**: nothing dropped; one id covers all definitions.
**Cons**: the Rust merge applies to impl *blocks* — containers whose methods re-parent onto the merged entity. Python duplicate `def`s are leaf functions with distinct bodies and spans; a merged entity would need a fabricated span (or a span union covering unrelated lines) and would attribute both bodies' calls to one function — precisely the mis-attribution decision 2 exists to prevent.

## Consequences

- A `@x.setter` body is invisible in the v1 graph (its calls/references too). This is a known, counted gap — observable per run via `duplicate_entities_dropped_total` — not silent corruption: nothing mis-attributes, ids never collide at the host's `UNIQUE(entities.id)`.
- Surfacing setters/deleters later is a pre-approved additive enrichment under decision 3: new discriminated ids appear, no existing id changes.
- Both walkers (`extractor.py`, `pyright_session.py`) carry the first-wins rule; any third walker added later must apply it identically or break the attribution invariant — the dogfood gate will catch an emitted-id collision, the unit pins catch attribution drift.
