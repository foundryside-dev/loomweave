# ADR-027: Ontology Version Semver Policy

**Status**: Accepted
**Date**: 2026-05-05
**Deciders**: qacona@gmail.com
**Context**: ADR-022 establishes that plugins own their ontology vocabulary (entity kinds, edge kinds, tag vocabulary, rule-ID prefixes), declared in the manifest under `[ontology].ontology_version`. ADR-022 does not define what numeric changes to that field mean. Sprint-2 B.2 bumped `ontology_version` 0.1.0 → 0.2.0 (added `class` and `module` entity kinds). Sprint-2 B.3 bumps 0.2.0 → 0.3.0 (added `contains` edge kind). Without an explicit policy, the version becomes a monotonic counter — consumers cannot tell "additive kind" from "breaking schema change", and future contributors invent inconsistent rules under deadline pressure. This ADR locks the semver semantics.

## Summary

`[ontology].ontology_version` in a plugin's `plugin.toml` follows semver:

- **MAJOR** bump — incompatible vocabulary change. A previously-emitted kind is removed or its semantics redefined; a property's type changes incompatibly; a kind's parent-relationship constraint changes. Consumers reading historical data from before the bump cannot trust their interpretation of the new emissions.
- **MINOR** bump — additive vocabulary expansion. A new entity kind, a new edge kind, a new property on an existing kind, a new tag vocabulary entry. Pre-bump consumers see a superset of what they saw before; their existing queries continue to work.
- **PATCH** bump — clarification or bug fix without wire-shape change. Documentation correction in the manifest comments, fixing an emitter bug that produced wrong values for an existing kind, refining property semantics in a backward-compatible way (e.g., a new optional sub-field).

This ADR clarifies ADR-022 — it does not amend or supersede it. Each plugin owns its own `ontology_version` independent of siblings.

## Context

### Why the policy was implicit

ADR-022 was written when only one plugin existed and only one entity kind shipped. The manifest field `[ontology].ontology_version` was sized for ADR-007's summary-cache invalidation — "bump when the entity/edge/rule set shifts" — but the original framing did not anticipate the ambiguity of repeated additive bumps.

Sprint-2 surfaced the ambiguity twice:

1. **B.2** (2026-05-05) bumped `ontology_version` 0.1.0 → 0.2.0 when `class` and `module` joined the entity-kind set. The bump policy (additive ⇒ MINOR) was inferred from convention; B.2's design doc (`docs/implementation/sprint-2/b2-class-module-entities.md` §5) chose 0.2.0 by analogy to package-version semver, not because ADR-022 specified it.
2. **B.3** (2026-05-05) bumps 0.2.0 → 0.3.0 for the addition of the `contains` edge kind — same shape: additive expansion, MINOR bump. The B.3 design doc cites this ADR for the rule.

The B.3 panel review (architecture-critic, solution-architect) identified the policy gap: "If `decorates`, `inherits_from`, `calls`, `imports` each get B.3.x bumps, ontology_version becomes a monotonic counter with no semantic meaning — consumers can't tell 'additive kind' from 'breaking schema change'." This ADR closes that gap.

### The relationship to package version

A plugin's package version (Python: `__version__`) and its ontology version are deliberately decoupled. The package version tracks code releases — bug fixes to the extractor, refactors that don't change emission, dependency bumps. The ontology version tracks declared vocabulary. They evolve at different cadences:

- B.2 bumped package `__version__` 0.1.0 → 0.1.1 (PATCH — no API break, only emission shape grew) AND `ontology_version` 0.1.0 → 0.2.0 (MINOR — new kinds).
- B.3 bumps package `__version__` 0.1.1 → 0.1.2 (PATCH — no API break) AND `ontology_version` 0.2.0 → 0.3.0 (MINOR — new edge kind).

This decoupling is intentional. A reader who pins the package version is pinning the implementation; a reader who pins the ontology version is pinning the contract. ADR-007's summary-cache key uses neither version directly (the cache key 5-tuple is `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`); ontology_version influences cache invalidation only insofar as new entity kinds get new entity IDs that miss the cache by component-1 organically.

### Why not amend ADR-022 directly

Per the project's editorial conventions (`CLAUDE.md`), Accepted ADRs are immutable except for status changes and supersession links. ADR-027 clarifies a portion of ADR-022 that ADR-022 left implicit; the cleanest record of that clarification is a new ADR that ADR-022's `Related Decisions` section can later cite (when ADR-022 is next amended for an unrelated reason).

## Decision

### MAJOR bump — incompatible vocabulary change

A MAJOR bump (X.0.0 → (X+1).0.0) is required when ANY of the following changes:

1. A previously-declared entity kind is removed from `entity_kinds`.
2. A previously-declared edge kind is removed from `edge_kinds`.
3. The semantics of an existing kind change in a way pre-bump consumers cannot accommodate. Examples: redefining `function` to include lambdas (when previously they were excluded); changing `contains` from one-to-many to many-to-many (a child entity gains multiple parents).
4. A property on an existing kind changes type incompatibly. Example: `parse_status` changes from string to integer.
5. A property previously documented as optional becomes required, OR a previously-required property becomes optional (this changes the consumer's null-safety obligations).

Every MAJOR bump SHOULD be accompanied by a change-log entry in the plugin's release notes naming the affected kinds and the migration path (if any).

### MINOR bump — additive vocabulary expansion

A MINOR bump (X.Y.0 → X.(Y+1).0) is required when ANY of the following changes AND no MAJOR-bump condition applies:

1. A new entity kind is added to `entity_kinds`.
2. A new edge kind is added to `edge_kinds`.
3. A new optional property is added to an existing kind.
4. A new tag is added to the kind's tag vocabulary.
5. A new rule-ID is registered under the plugin's namespace.

Pre-bump consumers see a strict superset of what they saw before; their existing queries continue to work; new queries against new kinds gracefully return empty results when run against pre-bump data.

### PATCH bump — clarification or bug fix without wire-shape change

A PATCH bump (X.Y.Z → X.Y.(Z+1)) is required when:

1. The manifest comments are clarified (no field-set or value change).
2. An emitter bug is fixed such that values previously emitted incorrectly are now emitted correctly. Example: a `function` entity's `start_line` was off-by-one; the fix is a PATCH because the kind set is unchanged and the wire shape is unchanged. Consumers that had defensive workarounds for the bug should drop them; consumers that relied on the buggy values must adapt (this is the only case where a PATCH bump can require consumer adaptation, and the change-log entry MUST name the buggy values).
3. A new optional sub-field is added inside an existing property (the property's overall structure is unchanged from a typed-deserialise perspective).

PATCH bumps do NOT add new top-level kinds, properties, or rule IDs. Adding any of those is MINOR.

### Independence across plugins

Each plugin owns its own `ontology_version` independently. The Python plugin's bump 0.2.0 → 0.3.0 has no implication for a hypothetical Java plugin's `ontology_version`. Cross-plugin reconciliation (when v0.2+ adds it) operates on per-pair vocabulary mapping, not on shared version numbers (see ADR-018 for the cross-product identity reconciliation precedent).

### Lockstep with `server.py::ONTOLOGY_VERSION`

For Python plugins specifically, the manifest `[ontology].ontology_version` and the Python module constant `ONTOLOGY_VERSION` MUST remain numerically equal. The lockstep is enforced manually today (see B.2 design §5, B.3 design §7); a CI lint guard is filed as filigree `clarion-8befae708b` (P3 follow-up) and is the canonical mechanical check. This invariant is plugin-implementation-specific (other plugin runtimes may not have an analogous constant); the policy here applies only to the manifest field.

## Alternatives Considered

### Alternative 1: Single monotonic counter

`ontology_version` is just an integer (or a single-component "1", "2", "3"). Bump on any change.

**Why rejected**: defeats the purpose of having a version field. Consumers cannot tell "additive" from "breaking" without reading the changelog. The handshake validator at the Rust host (`crates/clarion-core/src/plugin/protocol.rs`) already validates non-empty; it can also enforce semver shape with no extra cost.

### Alternative 2: ABI-style four-part version (MAJOR.MINOR.PATCH.BUILD)

Add a build counter for bug fixes that don't even rise to PATCH.

**Why rejected**: PATCH already covers bug fixes. The four-part version is over-engineering for a manifest field that ships once per release.

### Alternative 3: No policy — let plugin authors choose

Document that the field exists; let each plugin pick its own version semantics.

**Why rejected**: produces exactly the drift this ADR forecloses. Any cross-plugin tooling (a future "show all plugins' ontology versions" CLI command, a multi-plugin handshake check) would have to special-case each plugin's convention.

### Alternative 4: Tie to the host's protocol_version

`ontology_version` follows the host's `protocol_version` field bumping rules.

**Why rejected**: protocol version tracks the JSON-RPC envelope shape; ontology version tracks the plugin's vocabulary. Conflating them couples two evolution paths that should remain independent (a host protocol bump shouldn't force every plugin's manifest to also bump; a plugin adding a new kind shouldn't require a host release).

## Consequences

### Positive

- **Consumers can act on the version field.** A pre-cache reader observing `ontology_version` change from 0.2.0 to 0.3.0 (MINOR) can confidently re-use cached results for kinds that existed in 0.2.0; only new-kind queries miss the cache. A change to 1.0.0 (MAJOR) signals "everything previously cached needs review."
- **Future contributors don't drift.** B.4 (catalog rendering) doesn't bump ontology_version (no kind set change). B.3.1 (a hypothetical bug-fix PATCH for `contains` edge emission) doesn't get conflated with B.4 (a downstream consumer with no manifest impact).
- **Cross-plugin tooling has a stable contract.** When v0.2 introduces a Java plugin, its ontology_version follows the same rules; multi-plugin "list manifests" commands can present versions uniformly.

### Negative

- **One more rule for plugin authors.** PATCH-vs-MINOR judgment calls for borderline cases (e.g., "I added an optional property — but is it really optional, or is it a kind change in disguise?") require thinking. The policy section above covers the common cases; ambiguous cases SHOULD escalate to ADR review.
- **Lockstep with `ONTOLOGY_VERSION` constant requires manual discipline.** The lint guard is filed but not yet implemented. Until it is, a divergence is caught only at handshake (the host validates non-empty, not semver shape).

### Neutral

- The MAJOR-bump dispensation for "previously-required property becomes optional" is conservative — some readers might tolerate that change if their parser treats every field as optional. The conservative read protects readers that don't.

## Related Decisions

- [ADR-007](./ADR-007-summary-cache-key.md) — summary-cache key 5-tuple. `ontology_version` is NOT in the key; new entity kinds invalidate caches via component-1 (entity_id) organically. ADR-027's policy means the cache invalidation pattern is predictable: MINOR bumps invalidate only new-kind cache rows; MAJOR bumps invalidate all rows for the affected kinds.
- [ADR-018](./ADR-018-identity-reconciliation.md) — cross-product identity reconciliation. ADR-027's "each plugin owns its own version" rule mirrors ADR-018's "Wardline owns its qualnames" rule: per-product sovereignty.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — plugin owns ontology vocabulary. ADR-027 names the version-bumping semantics that ADR-022 left implicit. ADR-027 clarifies, does not supersede.
- [ADR-024](./ADR-024-guidance-schema-vocabulary.md) — guidance schema vocabulary rename + edit-in-place migration. ADR-024 renamed schema field names (priority → scope_level / scope_rank, etc.) — that was a MAJOR-equivalent change at the schema layer (which has its own migration policy independent of plugin manifests). ADR-027 covers only the plugin manifest.
- [ADR-026](./ADR-026-containment-wire-and-edge-identity.md) — containment wire shape. B.3's wire-shape addition (top-level `edges` field on `AnalyzeFileResult`) does not bump ontology_version directly; it bumps because B.3 adds a new edge kind (`contains`). The two events coincide in B.3 but are independent in general.

## References

- [B.2 design doc](../../implementation/sprint-2/b2-class-module-entities.md) §5 — first ontology_version bump cite.
- [B.3 design doc](../../implementation/sprint-2/b3-contains-edges.md) §7 — second ontology_version bump cite.
- [Sprint-2 kickoff handoff](../../superpowers/handoffs/2026-04-30-sprint-2-kickoff.md) §"`ontology_version` bump policy" — the original ambiguous formulation that motivated this ADR.
- [Filigree clarion-8befae708b](.) — CI lint guard for plugin.toml ↔ server.py ONTOLOGY_VERSION drift (P3 follow-up; not blocking).
- [Semver 2.0.0](https://semver.org) — the broader convention this ADR adopts.
