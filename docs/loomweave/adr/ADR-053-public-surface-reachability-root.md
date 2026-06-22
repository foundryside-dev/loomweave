# ADR-053: `public-surface` Reachability Root — PEP 8 Fallback for No-`__all__` Library Modules

**Status**: Accepted
**Date**: 2026-06-22
**Deciders**: john@foundryside.dev
**Context**: clarion-4ec50f3d92. Dead-code reachability over-reports dead when
the reachability **root set is under-covered**. The Python plugin derived the
`exported-api` root tag *only* from a module's `__all__` (`extractor.py`
`_module_export_names` / `_function_tags` / `_class_tags`). A codebase that does
not exhaustively declare `__all__` therefore emits zero `exported-api` roots for
~all of its public surface, so that surface reads as unreachable whenever it is
invoked through paths static analysis cannot follow (framework dispatch,
dependency injection, a CLI, or tests). Dogfood evidence (`~/elspeth`, a Python
**web app with a CLI component**, much of whose service/business code is reached
through framework dispatch rather than direct static calls): `__all__` is
declared in only 67 / 505 modules (13%); `entity_dead_list app_only=true`
reported **64%** unreachable (6976/10861) — the analyzer itself flagged this as
implausible (LOW CONFIDENCE). Separately, the
LOW-confidence advisory and the no-roots envelope told users to *"configure
entry-point roots"*, naming a config knob that does not exist (`loomweave.yaml`
has no roots section; the `roots` arg is a *mode*, `explicit`/`auto`, not a root
*set*).

## Summary

Two changes, both Loomweave-owned and structural:

1. **`public-surface` root class.** When a module declares **no `__all__` at
   all**, the Python plugin tags its non-underscore module-level defs/classes
   `public-surface` — a reachability root of *lower confidence* than a declared
   `exported-api`. `exported-api` stays reserved for names a module explicitly
   lists in `__all__`; the provenance distinction (declared vs PEP 8-inferred) is
   preserved in the tag itself rather than conflated. A module *with* `__all__`
   (including an empty `__all__ = []`) emits no `public-surface`, so well-declared
   corpora are byte-identical to before. `public-surface` joins
   `DEAD_CODE_ROOT_TAGS` in `loomweave-mcp`, so it is a root in both `explicit`
   and `auto` modes.

2. **Advisory points at the real levers.** The LOW-confidence advisory and the
   no-roots envelope no longer name a "configure roots" knob. They recruit the
   *actual* levers: declare `__all__`, add entry-point/cli-command/http-route
   decorators — and note that public module-level defs/classes are auto-tagged
   `public-surface` when a module has no `__all__`.

This is an additive ontology change: Python plugin `ontology_version`
0.8.0 → 0.9.0.

## Context

- `exported-api` previously carried an implicit claim: "the author declared this
  public in `__all__`." Reusing that tag for the no-`__all__` PEP 8 fallback
  would erase the declared-vs-inferred distinction — an honesty regression the
  codebase otherwise guards carefully (the dead-code summary already separates
  `roots_mode` explicit/auto and a `roots_confidence: derived` marker). A
  distinct tag keeps the provenance legible to any agent inspecting the graph.
- PEP 8 is the grounding: absent `__all__`, a module's public surface *is* its
  non-underscore module-level names. So the fallback is not a guess; it is the
  language's own convention for "public," applied only where the author left the
  surface implicit. Treating that public surface as a reachability root is a
  fail-toward-live posture for code invoked from outside the static call graph —
  which is the common case for both libraries (consumed by callers not in the
  tree) and applications (handlers reached through framework dispatch / DI / a
  CLI), so the heuristic is not library-specific.
- The `None` vs empty-set distinction in `_module_export_names` is load-bearing.
  `__all__ = []` is an **explicit empty** public surface (no roots); the
  *absence* of `__all__` is what triggers the fallback. Both plain and annotated
  (`__all__: list[str] = [...]`) assignments count as a declaration, so an
  annotated `__all__` is never mistaken for an absent one.
- Test entities are excluded from `public-surface` (they are already roots via
  the `test` tag and are not a library's public API); underscore-prefixed and
  non-module-level (nested defs, methods) names are excluded by the PEP 8
  definition itself.

## Decision

1. **`public-surface` is a new, additive reachability-root tag.** Emitted by the
   Python plugin (`_module_surface_tag`) for non-underscore, non-test,
   module-level functions/classes in modules that declare no `__all__`. Added to
   `DEAD_CODE_ROOT_TAGS`. `exported-api` semantics are unchanged (declared
   `__all__` members only).
2. **Lower-confidence-by-class, not by separate plumbing.** `public-surface` is
   an inferred root; `exported-api` is a declared root. The distinction lives in
   the tag value (and is documented), so a consumer can weight them differently
   without new wire fields. For dead-code reachability the union is what matters,
   so both simply join the root set.
3. **Advisory copy names only real levers.** No shipped copy may name a config
   knob that does not exist. The LOW-confidence advisory and the no-roots
   envelope name `__all__` and the decorator-derived root tags.
4. **Ontology bump 0.8.0 → 0.9.0** (additive tag-vocabulary change), in lockstep
   across `plugin.toml` `[ontology].ontology_version` and `server.ONTOLOGY_VERSION`.

## Empirical result (dogfood, `~/elspeth`)

Measured on the full live index (44k entities, deps resolved, tests present) by
injecting the validated heuristic onto an identical graph and comparing
`entity_dead_list app_only=true`:

- **Before**: 64% dead (6946/10896) — reproduces the ticket's documented 64%.
- **After** (`public-surface` roots): **48% dead** (5697/11786) — ~1,250 entities
  rescued, a 16-percentage-point / ~25%-relative reduction.

(Two notes on the figures. (1) The ticket's Context cites 64% as 6976/10861 from
the *original* run on the then-current live index; the Before row here is a fresh
re-measurement on a backup taken later, so the raw counts differ slightly from
index drift while the rounded share is the same 64%. (2) The `analysed`
denominator grows Before→After, 10896→11786: injecting `public-surface` roots
gives previously-rootless modules a root, so their entities move out of the
"not analysed / plugins-without-roots" exclusion and into the surveyed set —
i.e. more of the corpus becomes honestly analysable, which is part of the
improvement, not a counting artefact.)

The fix is a real, material accuracy improvement but does **not** by itself drop
elspeth out of the >25% "implausible" band. elspeth is a web app whose service
and business logic is largely reached through framework dispatch / DI the static
call graph cannot follow, and a large body of code (public *methods* of public
classes, nested helpers) is reachable only through those paths or through tests —
which `app_only` excludes by design and which this module-level heuristic does
not root. The reworded advisory now flags exactly this case honestly.

## Alternatives Considered

### Alternative 1: reuse `exported-api` for the no-`__all__` fallback

**Pros**: zero Rust changes; PEP 8 says absent `__all__` the public surface *is*
the non-underscore module-level names, so they are arguably "exported API."
**Cons**: conflates *declared* exports with *inferred* public surface — an agent
inspecting the tag can no longer tell which, eroding a provenance signal the rest
of the dead-code surface works to preserve. The distinct tag costs one
`DEAD_CODE_ROOT_TAGS` entry and an ontology bump, which is cheap.

### Alternative 2: add a real `loomweave.yaml` root-declaration surface

**Pros**: a project could declare arbitrary roots explicitly.
**Cons**: much larger (config schema + parsing + threading a root set into the
`find_dead_code` query path) and orthogonal to the actual gap, which is *default*
out-of-the-box coverage for libraries. The advisory reword fully removes the
phantom-knob dishonesty without it. Left as a possible future lever, not built.

### Alternative 3: also root public methods of public/exported classes (now)

**Pros**: would rescue more of elspeth's surface and likely push app_only below
the band.
**Cons**: out of this ticket's written "module-level" scope, and a materially
larger correctness surface (MRO, overrides, `@property`, dunder methods). Deferred
to a follow-up so the scoped, verified fix can land cleanly.

## Consequences

- Corpora (apps or libraries) with sparse/absent `__all__` get reachability roots
  for their public surface out of the box; their dead-code numbers become
  materially more trustworthy without any per-project configuration.
- A module that adopts `__all__` later loses its `public-surface` tags for that
  module (the declaration becomes authoritative) and gains `exported-api` for the
  listed names — the intended graduation from inferred to declared roots.
- `app_only=true` on a genuinely test-exercised library remains LOW CONFIDENCE by
  design; the advisory now says so and names the levers. The next structural
  lever (rooting public methods of public classes) is a tracked follow-up.
- Any future surfacing of method-level public roots is additive: a new tag
  (or `public-surface` extended to methods) joins `DEAD_CODE_ROOT_TAGS`; no
  existing tag semantics change.
