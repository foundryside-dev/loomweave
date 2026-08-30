# ADR-059: Class Instantiation Is a `calls` Edge to the Class

**Status**: Accepted
**Date**: 2026-08-31
**Deciders**: john@foundryside.dev
**Context**: clarion-e5224c3aff (salvage worklist A2). On a downstream Python index the graph held **zero** `calls` edges whose target was any `python:class:*` entity: `entity_callers_list` on a class always returned `[]` with `traversal_complete: true`, 2,235 constructor sites sat in `entity_unresolved_call_sites`, and 75 test files whose only link to the source tree is instantiation had no edge into it. The Python plugin's calls pass looked pyright's outgoing-call targets up in a *function-only* position index, so a class target — which is exactly what pyright reports for `Name(...)` — was silently dropped as unresolved.

## Summary

`Name(...)` (or `pkg.Name(...)`) resolving to a class entity emits an anchored `calls` edge from the enclosing function to the **class entity** (`to_id = python:class:…`), at `resolved` confidence, anchored on the callee token pyright reports. No new edge kind is introduced. The Python plugin ontology bumps 0.12.0 → 0.13.0 (MINOR, ADR-027: semantic widening of an existing kind's target set) so unchanged files re-dispatch and pick up the edges on the next incremental run.

## Context

- pyright's `callHierarchy/outgoingCalls` reports every instantiation as a call whose target item **is the class** (`SymbolKind.Class`, selection range on the class name) — never its `__init__`, whether the class defines one, inherits one, or gets one synthesised by `@dataclass`. That is the type checker's own model: calling a class object invokes its metaclass `__call__`, which is what `__init__`/`__new__` hang off.
- The writer's `ANCHORED_EDGE_KINDS` (ADR-026), the `entity_callers_list` reader, the neighborhood/execution-path traversals, and the dead-code reachability walk all key on `kind = 'calls'` and are agnostic about the target's kind. Nothing downstream assumed "calls target is a function"; the asymmetry lived only in the plugin's lookup.
- The Rust plugin already resolves `Type::new()` to the associated *function* (a real function entity), and drops tuple-struct constructor calls `Type(..)` as unresolved (parse-only, no type inference — see `docs/operator/rust-known-limitations.md`). Python's situation is different: pyright *does* resolve the target, we were discarding the answer.

## Decision

1. **Target.** An instantiation is a `calls` edge whose `to_id` is the class entity. `from_id` is the enclosing function (module-level instantiations, like every other module-level call, remain out of the calls pass).
2. **No `constructs` kind.** A distinct edge kind was considered and rejected (Alternative 1). "Function *calls* class" reads as a sentence (ADR-051's kind-name discipline) and matches pyright's own model.
3. **Anchor and confidence.** Unchanged from function-target calls: anchored on the token range pyright reports (`resolved`; `ambiguous` with `properties.candidates` when pyright returns several targets for one range).
4. **Ontology bump.** 0.12.0 → 0.13.0. The bump is what makes the change land on an existing index without `--no-incremental`: the analyzer treats an ontology-version change as "every file changed" for that plugin.
5. **`__init__` is not the target.** Consumers wanting "what runs when this class is instantiated" navigate class → `contains` → `__init__`; the graph does not synthesise a second edge.

## Alternatives Considered

### Alternative 1: A distinct `constructs` edge kind

**Pros**: Lets a consumer distinguish "instantiates" from "calls a function" without inspecting the target kind.
**Cons**: New kind in the writer's `ANCHORED_EDGE_KINDS`, both plugin manifests, the MCP `entity_call_site_list` schema (`kind` enum — inside the 22 KB `tools/list` budget), the callers/neighborhood/dead-code readers (which would each need to union the new kind in), and Rust parity work for a kind Rust cannot yet emit.
**Why rejected**: Every consumer question we have ("who uses this class", "is this class reachable", "which tests exercise it") is answered by `calls`; the target kind is already on the row for anyone who wants the distinction. The cost is all plumbing, the benefit is a label.

### Alternative 2: Target the class's `__init__` (or `__new__`) function entity

**Pros**: Every `calls` target stays a function.
**Cons**: pyright does not report `__init__` as the target, so the plugin would have to re-derive it — including for inherited and dataclass-synthesised initialisers, where there is no in-project `__init__` entity to point at (the edge would have to be dropped or coarsened to the class anyway). `entity_callers_list(class)` would still be empty.
**Why rejected**: It answers the wrong question (the consumer asks about the class, not its initialiser) and cannot be emitted uniformly.

## Consequences

### Positive

- `entity_callers_list` on a class returns its instantiation sites; test → class linkage exists for the "only instantiates" test-file population.
- Classes reached only by instantiation stop depending on the coarse unresolved-name suppression to avoid being reported dead; they are now reachable by a real edge.
- Nested classes instantiated inside a function no longer fall through to the "containing function" fallback, which had been attributing the call to whatever function *declared* the class.

### Negative

- Consumers that assumed "calls target ⇒ function" (none in-tree) must inspect the target kind.
- `unresolved_call_sites_total` drops on re-analysis for reasons unrelated to plugin resolution quality; the run-stats delta across the 0.12.0 → 0.13.0 boundary is not comparable.

### Neutral

- The Rust plugin is unchanged; its tuple-struct constructor gap remains documented in `rust-known-limitations.md`.
- Instances invoked via `__call__` (`obj()`) are a separate, existing mechanism (`_dunder_call_dispatches`) and are not widened here.

## Related Decisions

- **Related to**: [ADR-026](./ADR-026-containment-wire-and-edge-identity.md) (anchored `calls` contract), [ADR-027](./ADR-027-ontology-version-semver.md) (the MINOR bump), [ADR-028](./ADR-028-edge-confidence-tiers.md) (confidence tiers), [ADR-051](./ADR-051-relation-edge-direction-and-anchor.md) (kind-name-as-sentence discipline), [ADR-053](./ADR-053-public-surface-reachability-root.md) (dead-code roots this edge now feeds).

## References

- `plugins/python/src/loomweave_plugin_python/pyright_session.py` (`_target_id_from_call`, now consulting `entity_by_name_position`) — the implementation.
- `plugins/python/tests/test_pyright_session.py::test_pyright_session_resolves_class_instantiation_as_call_to_class` — the pin (local, imported, inherited-`__init__`, `@dataclass`).
