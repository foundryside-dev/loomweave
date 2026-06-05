# ADR-022: Core/Plugin Ontology Ownership Boundary

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: data-layer companion to Principle 3 (plugin owns ontology; core owns algorithms); specifies what the core validates without interpreting

## Summary

Plugins own language-specific ontology — entity kinds, edge kinds, tag vocabulary, and rule-ID prefixes — declared in the startup manifest. The core owns the *shape* the ontology fits into: the `Entity`/`Edge`/`Finding` record structs, the identifier grammar, a small reserved-kind namespace produced by core-owned algorithms, and the manifest-acceptance contract. The core never interprets a plugin-declared kind beyond validating its shape. Algorithms that consume plugin-declared vocabulary (clustering, prompt dispatch, neighbour queries) operate on named strings supplied at configuration time, not on a core-baked taxonomy.

## Context

Principle 3 is stated in prose in `requirements.md:37`: "plugin owns ontology; core owns algorithms." REQ-CATALOG-02 and REQ-CATALOG-03 bind that principle to entity and edge kinds respectively; REQ-FINDING-02 binds it to rule-ID namespacing. None of those requirements names the inverse question: *what is the core permitted to do with a plugin-declared kind, and what must it refuse?*

The design review and the 2026-04-17 panel synthesis identified three sites where the boundary needs to be explicit and is not:

1. **Clustering (ADR-006)** runs on a subgraph filtered by edge kind (default `imports` + `calls` for the Python plugin). If the core hardcodes those literals, it has baked Python-specific assumptions into a "language-agnostic" algorithm.
2. **Prompt dispatch (system-design §8)** chooses a template per entity kind. A `match entity.kind { "function" => …, "class" => … }` dispatch in core code forces a core release every time a plugin adds a kind.
3. **Core-reserved kinds.** `file`, `subsystem`, and `guidance` entities (`detailed-design.md:228-230`) are produced by core-owned algorithms: file discovery, Leiden clustering (ADR-006), guidance composition. A plugin emitting a `file` entity would be claiming authority the core already has. Conversely, the four core-reserved edge kinds (`contains`, `guides`, `emits_finding`, `in_subsystem`; `detailed-design.md:261`) carry semantics fixed across all plugins — a plugin cannot redefine `contains` to mean something other than structural containment.

Without an explicit decision, every downstream ADR re-litigates "should the core know about this kind?" and drift accumulates. ADR-014's `registry_backend: loomweave` mode depends on the `file` entity kind being core-owned. ADR-006 needs "clustering operates on a named edge subgraph" to be true. ADR-021 needs the manifest to be a validatable contract the core can enforce. Those three ADRs share an unstated premise; this ADR states it.

## Decision

The boundary is drawn as follows.

### Core owns (ontology-shape)

- **Record structs.** `Entity`, `Edge`, `Finding` as defined in `detailed-design.md:187-309`. `kind` and `rule_id` are `String` fields; no core `enum` enumerates valid values.
- **Identifier grammar.** Kind strings must match `[a-z][a-z0-9_]*`. Rule-ID prefixes must match `LMWV-[A-Z]+(-[A-Z0-9]+)+`. A malformed identifier is rejected at `initialize` with `LMWV-INFRA-MANIFEST-MALFORMED` (causes the plugin to fail to start).
- **Reserved entity kinds.** `file`, `subsystem`, `guidance`. These carry `plugin_id: core` only. A plugin manifest declaring any of them in its `kinds` list is rejected at `initialize` with `LMWV-INFRA-MANIFEST-RESERVED-KIND`.
- **Reserved edge kinds.** `contains`, `guides`, `emits_finding`, `in_subsystem`. Semantics are fixed across all plugins and the core. Plugins *may emit* edges of these kinds (e.g., a Python `module` `contains` a `function`); plugins *may not redefine* them. A manifest that attaches non-structural semantics to `contains` (e.g., asserting it is commutative) is rejected.
- **Rule-ID namespace registry.** `LMWV-INFRA-*` is core-only (pipeline/infrastructure findings). `LMWV-FACT-*` is shared — any plugin or the core may emit factual findings under it. `LMWV-{PLUGIN_ID_UPPERCASE}-*` is reserved to that plugin (`LMWV-PY-*` for the Python plugin, `LMWV-JAVA-*` for a future Java plugin). A plugin emitting a rule ID outside its namespace is refused at RPC with `LMWV-INFRA-RULE-ID-NAMESPACE`.
- **Emission acceptance.** Entities whose `kind` is not declared in the emitting plugin's manifest are rejected at `analyze_file` notification with `LMWV-INFRA-PLUGIN-UNDECLARED-KIND`. Edges whose `kind` is neither plugin-declared nor core-reserved are rejected likewise.

### Plugin owns (ontology-vocabulary)

- All entity kinds other than the three core-reserved ones.
- All edge kinds, including its declared use of the four core-reserved ones (plugin lists `contains` in its `edge_kinds` to signal that it emits containment edges, and binds itself to the core's semantics).
- All tag vocabulary and per-kind properties.
- Plugin-specific rule IDs under its assigned namespace (`LMWV-PY-STRUCTURE-001`, etc.).
- Prompt template selection. The plugin receives `build_prompt(entity_id, query_type, context, segments[])` and returns rendered segments. Core never inspects `entity.kind` to choose a template.

### Core validates shape without interpreting

Concrete implementation rules, each defending Principle 3 at a specific site:

- **Clustering (ADR-006)** takes its subgraph by edge-kind *name* from configuration, defaulting to the set named by the plugin (`imports`, `calls` for Python). The algorithm works on any named subset; a future Java plugin substituting `java_uses` for `imports` needs no core change.
- **Prompt dispatch (§8)** is plugin-driven. Core sends `(entity_id, query_type)`; the plugin returns pre-rendered segments. There is no core-side kind-switch.
- **Neighbour queries.** `neighbors(id, edge_kind=…, direction=…)` accepts any string; the core joins on `edges.kind = ?` without verifying `?` is semantically meaningful for the target plugin.
- **`file`-kind ownership (ADR-014).** `file` entities are minted by the core's file-discovery pass; plugins receive file entities via their `file_list` RPC and produce source entities whose `parent_id` chains lead to a `file` entity. Plugins never emit entities of kind `file` themselves.

## Alternatives Considered

### Alternative 1: Core-defined entity-kind enum

Core ships a closed `EntityKind` enum (`Function`, `Class`, `Module`, …) covering the expected languages.

**Pros**: compile-time type-safety; stronger IDE support in core code; pattern-match dispatch feels natural.

**Cons**: every new plugin or new language-level abstraction requires a core release. The design's stated goal (`system-design.md:126-128`) — "adding a language must not require upstream changes to the core" — fails at the data model. Python adding a language-level concept Loomweave wants to represent (pattern-matching bindings, structural protocols) forces a core release. Third-party plugin authorship dies the moment the upstream enum is missing the author's concept.

**Why rejected**: centralisation drift at the data-model level (`weft.md` §5 failure test). A closed enum is the stealth-monolith pattern applied to ontology.

### Alternative 2: No validation — trust the plugin entirely

Core stores whatever the plugin emits; no manifest-time or RPC-time checks on `kind` or `rule_id`.

**Pros**: zero validation cost; plugin authors aren't fighting a schema.

**Cons**: typos survive to production (`functon` instead of `function`); diagnosis reduces to post-hoc SQL. Rule-ID namespacing collapses — a plugin could emit `LMWV-INFRA-*` findings and break the pipeline-vs-analysis distinction. ADR-021's plugin authority model loses its data-layer half; a compromised plugin could mint synthetic `file` or `subsystem` entities that look authoritative but aren't.

**Why rejected**: shape-validation is O(kinds) at `initialize` and O(1) per emission — cheap. The downstream benefit (every emission is a contract check) is not.

### Alternative 3: Core-defined superset — union of supported languages

Core ships a schema listing every known language's kinds; plugins select from it. Vocabulary is shared across plugins for interoperability.

**Pros**: cross-plugin queries on a shared kind name ("find all functions across Python and Java") work by name.

**Cons**: a shared name implies equivalent semantics, and it does not hold. Python's `class` and Java's `class` differ on MRO, metaclasses, `__init_subclass__`, nominal vs structural typing — the questions Loomweave asks about classes are language-specific. False sharing produces worse outputs than no sharing (operators see "67 classes aggregated across languages" and can't reason about the count). Upstream maintenance of the superset reintroduces Alternative 1's core-release-per-language problem.

**Why rejected**: false economy. For the v0.2+ cross-plugin case, the right answer is a per-pair mapping table, not a forced common vocabulary.

### Alternative 4: Pure plugin ownership — no core-reserved kinds

Every kind is plugin-owned, including `file`, `subsystem`, `guidance`. Plugins coordinate on naming by convention.

**Pros**: maximum purity; zero core-owned ontology surface.

**Cons**: `file` crosses plugin boundaries (one codebase, many plugins; cross-plugin queries on "which entities live in this file" require shared file identity). `subsystem` is output of core-owned Leiden clustering — the algorithm produces subsystem entities, no plugin does. `guidance` is a core-composed construct. Pushing these into a plugin creates arbitrary ownership ("which plugin owns `file`?") with no principled answer. ADR-014's `registry_backend: loomweave` protocol requires `file` to be core-owned; shifting it to a plugin breaks the Filigree integration that ADR commits to.

**Why rejected**: the core-algorithm / plugin-ontology split *is* the decision being made; a zero-reserved-kinds rule makes the split impossible to draw at all.

## Consequences

### Positive

- Principle 3 is no longer only prose. It is a data-model invariant the core enforces at manifest acceptance and at every RPC. Manifest-time rejection surfaces plugin bugs the moment they appear.
- Adding a language is authoring a plugin. Zero core changes required. The Python plugin is the reference implementation; adding Java or Rust needs no upstream coordination.
- Clustering (ADR-006), prompt dispatch (§8), and neighbour queries are plugin-generic by construction. Their test surface is the shape-not-semantics contract, not a per-language matrix.
- Rule-ID namespacing (REQ-FINDING-02) becomes enforceable. The core refuses cross-namespace emissions, so the `LMWV-INFRA-PARSE-ERROR` vs `LMWV-PY-PARSE-ERROR` drift noted in `04-self-sufficiency.md` Issue 7 becomes a validation failure rather than a convention.
- ADR-014's file-kind ownership is principled, not incidental. `registry_backend: loomweave` works *because* `file` is core-owned; this ADR states that explicitly.

### Negative

- Storage is permissive at the string level. Typos are caught at manifest-acceptance, not compile time; IDE support for kind strings in plugin code is whatever the plugin's host language provides.
- Cross-plugin edge correlation is unsolved for v0.1: a Python `imports` edge and a JavaScript `imports` edge are different kinds under this decision. v0.2 needs a per-pair mapping convention if multi-language projects become a supported shape. Out-of-scope for v0.1 (single Python plugin).
- The reserved-kind list is effectively closed. Adding a new core-reserved kind in a future release (e.g., `commit`, `policy_bundle`) is a breaking change for plugins that happen to have declared that identifier. Mitigation: reserved namespace is documented prominently in plugin-authoring docs; additions go through a deprecation cycle.

### Neutral

- The manifest-acceptance contract is one more thing plugin authors satisfy, but it aligns with ADR-002's subprocess transport (validation at `initialize` is the natural checkpoint) and ADR-021's plugin authority model (same enforcement surface).
- Clustering's default edge-kind set (`imports`, `calls`) is configured in the plugin's manifest for v0.1, not in the core. ADR-006 picks the algorithm; this ADR guarantees the algorithm accepts any named subset the plugin supplies.

## Related Decisions

- [ADR-002](./ADR-002-plugin-transport-json-rpc.md) — the RPC surface validates shape, not semantics, at `initialize` and at every `file_analyzed` notification. This ADR names exactly which shape rules apply.
- [ADR-003](./ADR-003-entity-id-scheme.md) — entity IDs are `{plugin_id}:{kind}:{canonical_qualified_name}`; this ADR constrains what `{kind}` can legitimately be and who owns each namespace.
- [ADR-006](./ADR-006-clustering-algorithm.md) — clustering operates on a named edge subgraph (`imports`, `calls`); this ADR guarantees the core treats edge kinds as strings, not as a fixed vocabulary, which is what makes ADR-006's configurable `edge_types` work for non-Python plugins.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — the `file` entity kind is core-owned; ADR-014's `registry_backend: loomweave` protocol relies on this being a first-class decision rather than an incidental one.
- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — the process-layer plugin authority model has its data-layer counterpart here; manifest acceptance is the joint enforcement point for both.

## References

- [Loomweave v0.1 requirements §Design principles](../v0.1/requirements.md) (line 37) — Principle 3.
- [REQ-CATALOG-02, REQ-CATALOG-03](../v0.1/requirements.md) — plugin-declared kinds; reserved edge set.
- [Loomweave v0.1 system design §2](../v0.1/system-design.md) (lines 120-234) — core/plugin responsibility split; plugin manifest contract.
- [Loomweave v0.1 detailed design §1, §2](../v0.1/detailed-design.md) (lines 56-136, 187-309) — manifest shape; Entity/Edge/Finding structs; core-reserved edge kinds at line 261; core-minted `file`/`subsystem`/`guidance` IDs at lines 228-230.
- [Weft doctrine §5](../../suite/weft.md) — centralisation-drift failure test; a closed core-defined kind enum qualifies.
- [Panel synthesis — self-sufficiency review](../../implementation/v0.1-reviews/panel-2026-04-17/04-self-sufficiency.md) — Issue 7 (rule-ID inconsistency) is the empirical case this ADR's namespace rule resolves.
