# B.5* - Python plugin: `references` edges via pyright

**Status**: READY-FOR-IMPLEMENTATION after five-reviewer panel revisions
**Anchoring design**: inherits [B.4* calls edges](./b4-calls-edges.md) except where this memo says otherwise.
**Accepted ADRs**: [ADR-026](../../clarion/adr/ADR-026-containment-wire-and-edge-identity.md), [ADR-027](../../clarion/adr/ADR-027-ontology-version-semver.md), [ADR-028](../../clarion/adr/ADR-028-edge-confidence-tiers.md)
**Predecessor**: [B.4* - `calls` edges](./b4-calls-edges.md)
**Filigree umbrella**: `clarion-b0cedfd2bb`

---

## 1. Scope

B.5* adds plugin-emitted `references` edges: source-anchored, static mentions
of an in-project entity that are not invocation edges.

Changes in scope:

- The Python plugin emits `references` edges with `confidence` in
  {`resolved`, `ambiguous`}; no scan-time `inferred` edges, per ADR-028
  Decision 3.
- Every emitted `references` edge carries both `source_byte_start` and
  `source_byte_end`, per ADR-026 Decision 3 and ADR-028's `calls` /
  `references` confidence contract.
- Calls do not double-emit as references. B.5* ships
  `references_minus_calls`; consumers can union `calls` + `references` when
  they want "all static mentions."
- Ontology bumps from `0.4.0` to `0.5.0`; package patch bumps from `0.1.3`
  to `0.1.4`.

Out of scope:

- Virtual entities for builtins, stdlib, site-packages, typeshed, or
  third-party packages. Pyright can resolve `int`, but Clarion has no persisted
  target entity for it today.
- Subclassing: `class B(A)` is future `inherits_from`, not `references`.
- Decorators: future `decorates`.
- Import dependency edges: future `imports`. Imported names can still be
  reference sites when used elsewhere and resolved to in-project entities.
- Variable/property targets. `Foo.attr` can reference the class `Foo`; the
  `attr` member itself is ignored unless it maps to an existing module, class,
  or function entity.
- Query-time inferred references. B.6 owns inferred traversal behavior.

## 2. Locked Surfaces From B.4*

Inherited unchanged:

- `AnalyzeFileResult` remains `{entities, edges, stats}`.
- `RawEdge` remains `{kind, from_id, to_id, source_byte_start,
  source_byte_end, confidence, properties, extra}`.
- `EdgeConfidence` remains ADR-028's lowercase enum:
  `resolved`, `ambiguous`, `inferred`.
- Pyright is already pinned through `plugins/python/pyproject.toml` and
  `[capabilities.runtime.pyright]` in `plugin.toml`.

Required B.5* correction to the live tree:

- `crates/clarion-storage/src/writer.rs` currently lists anchored edge kinds as
  `calls`, `imports`, `decorates`, `inherits_from`. Add `references`, and
  tighten the anchored source-range contract so both endpoints are required,
  not merely one endpoint.

## 3. Design Decisions

### D1 - What Constitutes A `references` Edge?

A `references` edge is a source-anchored mention of an in-project module,
class, or function entity where the mention is not already represented by a
more specific edge kind.

Included source-site forms:

- Name reads: `CONST_REF = world`, `handler = Foo`.
- Type annotations: variable annotations, parameter annotations, return
  annotations, and nested/subscripted annotation names such as `list[Foo]`.
- Attribute bases: `Foo.attr` references the in-project base symbol `Foo`.
  The `attr` token is emitted only if it maps to a known module/class/function
  entity, which will usually be false until property/variable entities exist.
- `isinstance` / `issubclass` class-argument expressions.
- `getattr` / `hasattr` / `setattr` first-argument expressions when the first
  argument is a static in-project symbol. Constant attribute-name strings are
  not independent targets under the current ontology.
- Imported symbols when later uses resolve to in-project entities.

Excluded source-site forms:

- Calls: `world()` emits `calls`, not `references`.
- Subclass bases: `class B(A)` is future `inherits_from`.
- Decorator expressions: future `decorates`.
- Import-statement dependency edges: future `imports`.
- External targets: builtins, stdlib, site-packages, typeshed stubs, and source
  files outside `project_root` are counted as skipped, but no edge row is
  emitted.

#### Reference Site Ownership

Every reference site carries an explicit lexical owner used as `from_id`:

- Module-body site -> module entity id.
- Class-body site -> class entity id, unless the site is inside a method,
  nested function, or nested class.
- Function-body and function-signature annotation site -> innermost function
  entity id.
- Nested function/class sites use their own entity id once that node owns the
  reference lexically.
- Lambda bodies are not entities in v0.1; their sites use the nearest enclosing
  module/class/function entity.
- Syntax-error files emit only the degraded module entity and no references,
  matching the existing extractor behavior.

This ownership is part of the source-site object, not inferred later from byte
ranges. Tests must prove module-level `CONST_REF = world` uses the module
entity and `def f(x: Foo) -> Foo` uses the function entity.

### D2 - Pyright LSP Traffic Shape

Decision: **AST site enumeration + per-site `textDocument/definition`**, with
`textDocument/typeDefinition` as a narrow fallback when `definition` returns no
in-project target for annotation/type-expression positions.

Why this shape:

- B.4*'s `callHierarchy/outgoingCalls` is function-anchored and returns call
  sites. It does not expose arbitrary non-call mentions.
- `textDocument/references` is target-oriented: from a symbol position it
  returns declaration/reference locations for that symbol. It proves pyright
  has a cross-reference index, but it does not directly answer "what target
  does this source site mention?"
- `textDocument/documentSymbol` returns declarations, including variables and
  class members, but not RHS/use reference sites.
- `hover` is display text, not a stable target identifier.
- A local probe against pinned pyright 1.1.409 showed:
  - `CONST_REF = world` resolves by `definition` to the in-project function.
  - `x: Foo`, `-> Foo`, and `Foo.attr` at `Foo` resolve by `definition` to
    the in-project class.
  - `x: int` and `-> int` resolve to bundled typeshed `builtins.pyi`.
  - stdlib symbols may resolve to both stubs and external source; all
    outside-project targets are filtered out.

Traffic envelope:

- One `didOpen` and one `didClose` per analyzed file, sharing B.4*'s
  `PyrightSession` lifecycle.
- One AST walk per file to collect `ReferenceSite` objects and call-callee
  suppression ranges.
- One `definition` request per unique `(from_id, site kind, source lexeme)`
  lookup in the file; repeated same-owner references reuse the first result.
- At most one `typeDefinition` fallback for annotation/type positions when
  `definition` returns no target. If `definition` resolves only outside
  `project_root`, the site is counted as external and the fallback is skipped.
- B.4*'s gate result is not sufficient by itself because references can
  outnumber function entities. B.5* adds per-run reference counters and a
  B.5 reference scale smoke before close; B.8 remains the full-scale arbiter.
  The recorded B.5 smoke result lives in
  [b5-gate-results.md](./b5-gate-results.md).

Target filter and mapping:

- Persist only targets whose resolved URI is under `project_root` and maps to a
  known Clarion module, class, or function entity.
- Pyright definition locations are mapped through a shared entity declaration
  index, not B.4*'s function-only index. The index includes:
  - module entity by source file path;
  - class entity by class name span;
  - function entity by function name span.
- If pyright returns multiple in-project entity targets for one site, emit
  `confidence="ambiguous"`.
- If no in-project entity target remains after filtering, emit no edge and
  increment the skipped/unresolved reference stats.

Offset rule:

- Source byte ranges on `references` edges are computed from the AST reference
  token span, using Python's byte-offset semantics for `col_offset` /
  `end_col_offset`.
- LSP query positions are converted to pyright's expected line/character units
  from the same source. Non-ASCII tests are required for text before the token
  and inside the token's line.

### D3 - Dedup And Ambiguous Candidate Policy

Storage natural key remains `(kind, from_id, to_id)`. The plugin dedups
before returning `RawEdge`s so ordinary repeated references do not inflate the
writer's `dropped_edges_total`.

For duplicate `(kind, from_id, to_id)` rows:

- Keep the earliest `(source_byte_start, source_byte_end)`.
- If any duplicate row is ambiguous, the surviving row is ambiguous.
- Candidate sets are merged, deduped, and sorted. `properties.candidates`
  contains the full sorted candidate set, including the chosen `to_id`, matching
  the B.4* implementation behavior.
- The chosen `to_id` is the sorted first candidate id. It is a deterministic
  storage key, not a "best guess."

### D4 - Overlap With `calls`

A reference candidate is suppressed when its source range is contained within
the AST callee expression of a `Call` node. This is stricter than exact byte
range equality and prevents double emission for `world()`, `Foo.attr()`, and
`handlers[key]()`.

Non-call mentions of callable objects remain references. Example:
`CONST_REF = world` emits `references`; `return world()` emits only `calls`.

### D5 - Ontology Version Bump

Per ADR-027, adding `references` is a MINOR ontology bump:
`0.4.0 -> 0.5.0`.

Lockstep updates:

- `plugins/python/plugin.toml`: `edge_kinds = ["contains", "calls",
  "references"]`; `ontology_version = "0.5.0"`.
- `plugins/python/src/clarion_plugin_python/server.py`:
  `ONTOLOGY_VERSION = "0.5.0"`.
- `plugins/python/src/clarion_plugin_python/__init__.py` and
  `plugins/python/pyproject.toml`: package `0.1.3 -> 0.1.4`.

### D6 - Pyright Provisioning And Failure Modes

No new dependency. B.5* uses the pinned pyright 1.1.409 installed by B.4*.

Failure behavior:

| Case | Finding / observable | Behavior |
|---|---|---|
| pyright unavailable | `CLA-PY-PYRIGHT-UNAVAILABLE` | no calls or references from pyright; site counters record skipped/unresolved sites |
| pyright install failure | `CLA-PY-PYRIGHT-INSTALL-FAILURE` | same |
| init timeout | `CLA-PY-PYRIGHT-INIT-TIMEOUT` | same |
| subprocess crash, restart under cap | `CLA-PY-PYRIGHT-RESTART` | retry future work through restarted session |
| restart cap exceeded | `CLA-PY-PYRIGHT-POISON-FRAME` | session disabled; both calls and references suppressed for remaining files |
| per-reference lookup timeout | `CLA-PY-REFERENCE-RESOLUTION-TIMEOUT` | current site skipped; run continues |

The same `PyrightSession` disabled state suppresses both call and reference
resolution. Plugin findings remain resolver/extractor test observables in B.5*;
persistence as ADR-004 findings is future work, matching B.4*.

## 4. Wire, Stats, And Type Shape

No new edge envelope or field. B.5* adds rows like:

```python
{
    "kind": "references",
    "from_id": "python:function:demo.annotated",
    "to_id": "python:class:demo.Foo",
    "source_byte_start": 123,
    "source_byte_end": 126,
    "confidence": "resolved",
}
```

Python type additions:

- `ReferenceSite`: owner id, source byte range, LSP query position, kind tag
  (`name`, `annotation`, `attribute_base`, etc.), and call-suppression metadata.
- `ReferencesRawEdge`: `kind: Literal["references"]`, same anchored/confidence
  fields as `CallsRawEdge`.
- `ReferenceResolutionResult`: edges, reference counters, pyright latency
  samples, findings.
- `ExtractionStats`: neutral aggregate replacing `ExtractResult.stats:
  CallResolutionResult`. It merges call and reference counters, latency samples,
  and Python-internal findings without widening every edge through unsafe casts.

Rust `AnalyzeFileStats` grows serde-defaulted fields:

- `reference_sites_total`
- `references_resolved_total`
- `references_skipped_external_total`
- `references_skipped_cap_total`
- `unresolved_reference_sites_total`
- existing `unresolved_call_sites_total`
- existing `pyright_query_latency_ms`, now documented as all pyright LSP query
  latency samples, not only call-hierarchy latency.

`clarion analyze` aggregates these into `runs.stats` with the same names plus
the existing `pyright_query_latency_p95_ms`.

## 5. Storage, Host, And Consumer Contracts

No migration is expected. Existing `edges.confidence` and
`ix_edges_kind_confidence` are sufficient.

Writer requirements:

- `references` is an anchored edge kind.
- Anchored edges require both `source_byte_start` and `source_byte_end`.
- Anchored scan-time `confidence=inferred` is rejected.
- Accepted ambiguous references increment `ambiguous_edges_total`.

Host requirements:

- Before the manifest bump, a `references` edge is dropped with
  `CLA-INFRA-PLUGIN-UNDECLARED-EDGE-KIND`.
- After the manifest bump, `process_edges` accepts `references` exactly as it
  accepts other declared edge kinds.

B.6 consumer handoff:

- `neighborhood` may traverse `references`.
- `execution_paths_from` must not treat `references` as executable flow.
- Ambiguous references expand as `to_id union properties.candidates`.
- Default confidence filtering remains ADR-028's resolved-only default.

## 6. Scale Guard

B.5* adds `MAX_REFERENCE_SITES_PER_FILE` (default 2000). If a file exceeds the
cap, reference resolution for that file is skipped, `references_skipped_cap_total`
increments by the skipped site count, and a `CLA-PY-REFERENCE-SITE-CAP` warning
is emitted. Calls still run.

B.5 reference scale smoke:

- Use the existing `tests/perf/synthetic` and/or `tests/perf/elspeth_mini`
  corpus from B.4*.
- Record: file count, reference site count, pyright request count, p95 latency,
  cap skips, skipped external count, and projected elspeth-full request count.
- Green: no cap skips on the synthetic corpus and projected request count is not
  more than 5x B.4*'s function-query count without a written mitigation.
- Mitigations: per-file content-hash cache, lower source-site inclusion scope,
  or a target-first cross-reference redesign if per-site lookup proves too
  expensive.
- The implemented mitigation is a per-file repeated-lookup cache keyed by
  `(from_id, site kind, source lexeme)`, plus an external-definition
  short-circuit for annotation fallback.

## 7. Implementation Task Ledger

### Task 1 - Design memo and panel lock

RED: panel reviewers find unresolved design gaps.

GREEN: this memo records the five-reviewer verdicts and reconciliations in
Section 10.

Acceptance: file committed with no TODO/TBD placeholders and Filigree comment
linking the final design.

Commit: `docs(wp3): design B.5* references edges via pyright`

### Task 2 - Writer and host contract for `references`

RED matrix:

| Case | Observable |
|---|---|
| `references` before manifest declaration | host finding `CLA-INFRA-PLUGIN-UNDECLARED-EDGE-KIND` |
| no source range | writer error `CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT` + `dropped_edges_total += 1` |
| start-only range | same |
| end-only range | same |
| `confidence=inferred` | writer error `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT` + `dropped_edges_total += 1` |
| resolved with both endpoints | row persisted, no counters |
| ambiguous with both endpoints | row persisted, `ambiguous_edges_total += 1` |
| duplicate logical edge | plugin-level dedup test; writer dedupe behavior remains unchanged |

GREEN: add `references` to anchored kinds, tighten both-endpoint enforcement,
and preserve B.4* calls behavior.

Acceptance: storage and host tests pass; every negative case asserts the named
code plus the relevant counter/finding channel.

Commit: `feat(storage): enforce references edge contract (B.5* Task 2)`

### Task 3 - Reference site collection and type surface

RED tests:

- module-level `CONST_REF = world` produces a `ReferenceSite` owned by the
  module entity.
- function annotations `def f(x: Foo) -> Foo` produce sites owned by the
  function entity.
- class-body annotation is owned by the class entity.
- nested/subscripted annotation `list[Foo]` records the `Foo` token.
- call-callee containment suppresses `world()` and `Foo.attr()`.
- subclass and decorator expressions are excluded.
- non-ASCII before the token and on the token line produce correct byte ranges.

GREEN: add `ReferenceSite`, entity declaration index inputs, call-suppression
ranges, and `ExtractionStats` without invoking pyright yet.

Acceptance: pure extractor tests pass under mypy strict with no broad casts.

Commit: `feat(wp3): collect reference sites with lexical owners (B.5* Task 3)`

### Task 4 - Pyright reference resolver

RED tests:

- resolved name read: `CONST_REF = world`.
- resolved annotation target: `Foo`.
- class target mapping via the shared entity index.
- external/builtin `int`, stdlib, site-packages, and typeshed targets produce
  no edge and increment skipped/unresolved stats.
- duplicate references keep earliest byte range and merge candidates.
- ambiguous result is deterministic and has full sorted `properties.candidates`.
- `CLA-PY-PYRIGHT-UNAVAILABLE`, `CLA-PY-PYRIGHT-INSTALL-FAILURE`,
  `CLA-PY-PYRIGHT-INIT-TIMEOUT`, `CLA-PY-PYRIGHT-RESTART`,
  `CLA-PY-PYRIGHT-POISON-FRAME`, `CLA-PY-REFERENCE-RESOLUTION-TIMEOUT`,
  and `CLA-PY-REFERENCE-SITE-CAP` each have a targeted negative test.

GREEN: extend `PyrightSession` with `resolve_references(file_path, sites)` and
the shared entity index. Reuse lifecycle, stderr drain, and restart behavior.

Acceptance: pyright tests pass when pyright is installed; skip markers remain
honest when it is absent.

Commit: `feat(wp3): resolve references via pyright definitions (B.5* Task 4)`

### Task 5 - Extractor/server/Rust stats integration

RED tests:

- no-op reference resolver emits no references and preserves entities,
  contains, and calls.
- fake resolver appends a references edge and stats/finding data.
- one analyze result can carry both call and reference latency/counters.
- server serializes new stats fields; Rust `AnalyzeFileStats` deserializes
  missing fields to zero for older plugins.
- CLI `runs.stats` aggregates reference counters and keeps existing B.4*
  call counters.

GREEN: wire `reference_resolver` through extractor and server, add stats
fields in Rust, and aggregate them in CLI.

Acceptance: Python extractor/server tests, Rust protocol tests, and CLI stats
tests pass.

Commit: `feat(wp3): emit references edges and aggregate stats (B.5* Task 5)`

### Task 6 - Manifest bump, fixture parity, round-trip, e2e, scale smoke

RED tests:

- fixture parity expects resolved and ambiguous references rows.
- round-trip self-analysis expects at least one resolved references edge.
- walking skeleton expects:
  - at least one `CONST_REF = world` reference;
  - at least one annotation reference to an in-project `Foo`;
  - no `references` row for the `world()` call callee range;
  - no persisted reference to `int`;
  - existing resolved/ambiguous `calls` checks remain green;
  - `dropped_edges_total == 0`;
  - `unresolved_call_sites_total == 0`;
  - reference stats are nonzero where expected.
- B.5 reference scale smoke records the counters from Section 6 in
  [b5-gate-results.md](./b5-gate-results.md).

GREEN: update versions, manifests, fixtures, tests, and e2e script.

Acceptance: parity tests, round-trip, walking skeleton, and scale smoke pass.

Commit: `test(wp3): references fixture parity and e2e coverage (B.5* Task 6)`

### Task 7 - ADR-023 gates and closeout

Run and record:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo nextest run --workspace --all-features`
- `cargo doc --workspace --all-features --no-deps`
- `cargo deny check`
- `plugins/python/.venv/bin/ruff check plugins/python`
- `plugins/python/.venv/bin/ruff format --check plugins/python`
- `plugins/python/.venv/bin/mypy --strict plugins/python`
- `plugins/python/.venv/bin/pytest plugins/python`
- `tests/e2e/sprint_1_walking_skeleton.sh`

Acceptance: all gates pass on the closing commit; Filigree
`clarion-b0cedfd2bb` is closed with verification notes; PR is opened.

Commit: `chore(wp3): close B.5* references edges`

## 8. Exit Criteria

B.5* is done when all are true:

- Python plugin emits resolved references for in-project non-call mentions.
- Ambiguous references are deterministic, candidate-safe, and expandable by B.6.
- Repeated references dedup to earliest range and merged candidates.
- Calls do not double-emit as references inside call-callee expressions.
- `references` is enforced as an anchored edge kind by the writer.
- Reference stats surface in `AnalyzeFileResult.stats` and `runs.stats`.
- Ontology version is `0.5.0`; Python package version is `0.1.4`.
- Cross-language fixture parity includes references rows.
- Round-trip self-test sees at least one resolved references edge in plugin
  source.
- Walking skeleton persists references rows while B.4* calls assertions remain
  green.
- B.5 reference scale smoke is recorded.
- ADR-023 gates pass.

## 9. Open Questions For Later Work

- Builtin/stdlib entity strategy. Durable references to `int`, `str`, or
  third-party types need explicit virtual-entity ontology and source policy.
- Variable/property entities. Attribute member targets need a variable/property
  entity kind before they can be represented honestly.
- Import dependency semantics. B.5* should not preempt future `imports` edges.

## 10. Panel-Review Record

Five reviewers ran in parallel on the first draft and all returned
ACCEPT-WITH-REVISION.

| Reviewer | Verdict | Reconciliation |
|---|---|---|
| architecture | ACCEPT-WITH-REVISION | Added lexical owner rules, split site collection from pyright resolution, tightened both-endpoint range tests, pinned full-candidate `properties.candidates`, and added reference stats. |
| quality | ACCEPT-WITH-REVISION | Expanded Task 2 RED matrix with exact observables, added named pyright negative tests, strengthened e2e assertions, and added per-task acceptance blocks. |
| reality | ACCEPT-WITH-REVISION | Verified pyright 1.1.409 provider support and behavior; qualified traffic cost, external target filtering, documentSymbol/references claims, and attribute handling. |
| systems | ACCEPT-WITH-REVISION | Added B.5 scale guard, explicit failure-mode matrix, ambiguous dedup merge rule, B.6 consumer invariants, and call-callee containment suppression. |
| python-engineering | ACCEPT-WITH-REVISION | Added entity declaration index requirement, AST/LSP offset rules, non-ASCII tests, mypy-strict stats/types, and expanded extractor test seams. |

This section is the authoritative panel record; raw reviewer transcripts live in
the conversation log.
