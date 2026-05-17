# B.2 — Python plugin: class + module entity emission (Sprint 2 / WP3 feature-complete subset)

**Status**: DRAFT — Sprint 2 Tier-B B.2 work-package design
**Anchoring design**: [detailed-design.md §1 (Plugin implementation — Python specifics)](../../clarion/v0.1/detailed-design.md#1-plugin-implementation-detail), [system-design.md §6 (Guidance composition; clustering)](../../clarion/v0.1/system-design.md#6-guidance-system)
**Accepted ADRs**: [ADR-007](../../clarion/adr/ADR-007-summary-cache-key.md), [ADR-018](../../clarion/adr/ADR-018-identity-reconciliation.md), [ADR-021](../../clarion/adr/ADR-021-plugin-authority-hybrid.md), [ADR-022](../../clarion/adr/ADR-022-core-plugin-ontology.md), [ADR-023](../../clarion/adr/ADR-023-tooling-baseline.md)
**Predecessor**: [WP3 Sprint-1 baseline](../sprint-1/wp3-python-plugin.md) (function-only extraction)
**Successor**: B.3 — `contains` edges (planned)
**Sprint-2 kickoff handoff**: [`docs/superpowers/handoffs/2026-04-30-sprint-2-kickoff.md`](../../superpowers/handoffs/2026-04-30-sprint-2-kickoff.md) §"What's in scope for Sprint 2" Tier B row B.2

---

## 1. Scope

B.2 extends the Python plugin to emit two additional entity kinds beyond Sprint 1's function-only baseline:

- **`class`** — every `ast.ClassDef` becomes one entity. Nested classes nest. Class methods continue to be emitted as `function` entities (no separate `method` kind, per `detailed-design.md:67`).
- **`module`** — every analyzed `.py` file produces exactly one module entity. Files that fail to parse still produce a module entity (with `parse_status: "syntax_error"`); files that parse successfully (including empty files) produce one with `parse_status: "ok"`. Top-level `__init__.py` files (where `module_dotted_name` would resolve to `""`) are skipped with a stderr line — emitting them would invalidate the entity-ID grammar (see Q1 resolution).

**Out of scope** (deferred to B.3 or later):

- `contains` edges (B.3).
- `parent_id` field on the wire (B.3).
- Imports, calls, decorated_by, inherits_from edges (later WP3-feature-complete sprints).
- `package` entity kind (later — requires multi-file context the per-file `analyze_file` RPC doesn't carry).
- `decorator`, `global`, `protocol`, `enum`, `typed_dict`, `type_alias` entity kinds (later).
- All `CLA-PY-*` findings including `CLA-PY-SYNTAX-ERROR` (the `parse_status` property is set on the module entity; the matching finding emission lands when WP4's finding pipeline activates).

## 2. Locked surfaces from Sprint 1 (B.2 reads and writes against these)

These are caller-observable surfaces locked at `v0.1-sprint-1` close. B.2 must not change them; if a change is genuinely needed, write an ADR amendment per the kickoff-handoff convention.

- **Wire shape** (`crates/clarion-core/src/plugin/host.rs:132-154`): `RawEntity { id, kind, qualified_name, source: RawSource, extra }` with `#[serde(flatten)] extra`. `RawSource { file_path, extra }` likewise. New top-level fields ride through `extra` non-breakingly.
- **L7 qualname format**: `{dotted_module}.{__qualname__}`. `<locals>` marker only at function-parent boundaries. `module_dotted_name` strips `src/` prefix, drops `.py`, collapses `pkg/__init__.py` → `pkg`.
- **L4 JSON-RPC method set**: `initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`. B.2 changes none of these.
- **L5 manifest schema**: B.2 amends only `[ontology].entity_kinds` (adds `"class"`, `"module"`) and `[ontology].ontology_version` (`0.1.0` → `0.2.0`). No structural change.
- **L8 Wardline pin**: unchanged.
- **`extract()` signature** (`extractor.py:86-99`): `extract(source: str, file_path: str, *, module_prefix_path: str | None = None) -> list[dict[str, Any]]`. B.2 keeps the signature; the return-type annotation tightens to a `TypedDict` form (see §4).
- **`ServerState`** (`server.py:60-66`): unchanged.
- **`ONTOLOGY_VERSION` constant** in `server.py`: bumps `0.1.0` → `0.2.0` in lockstep with `plugin.toml::ontology_version`. The bump is the L5 handshake-validation signal that the entity-kind set has shifted; **note that ADR-007's summary-cache key is the 5-tuple `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)` and `ontology_version` is NOT a cache-key component** (this corrects an imprecise framing in the kickoff handoff — verified at `docs/clarion/adr/ADR-007-summary-cache-key.md:10`). Cache invalidation when the kind set expands happens organically: new module/class entity_ids miss the cache by component-1 of the 5-tuple.

## 3. Design decisions (Q1–Q4 panel-resolved)

Each design decision below was taken to a five-reviewer panel (systems thinker, solution architect, architecture critic, Python engineer, quality engineer) for consensus before locking. Reviewer reasoning is summarised inline; the full transcripts are in this branch's commit messages and conversation log.

### Q1 — Module entity emission policy

**Decision**: always emit one module entity per `analyze_file` call (even empty / syntax-error / comment-only files), with a `parse_status` property carried via `RawEntity.extra` (`{"ok" | "syntax_error"}` — empty files are `"ok"` since `ast.parse("")` succeeds). Skip top-level `__init__.py` (where `module_dotted_name` would resolve to `""`) with a one-line stderr message.

**Why**:
- Module is `leaf: false` in the kind catalogue (`detailed-design.md:74`) — a structural container tied to *files*, not content. Suppressing module entities for empty / broken files would break referential integrity for B.3's `contains` edges (every function/class entity must contain into a module entity that exists) and force B.4 catalog rendering and B.5 per-subsystem markdown to write defensive null-parent guards.
- An empty `__init__.py` is a load-bearing Python semantic artifact (package marker). Omitting it from the catalog passes verification but fails validation (the user asks "what packages exist?" and gets a wrong answer).
- Syntax-error files are exactly what an archaeologist most needs to see ("why is this file broken?"). The `parse_status` property keeps the truth honest without dropping the entity.
- Top-level `__init__.py` would crash the entity-ID assembler (`crates/clarion-core/src/entity_id.rs:97-101` rejects empty `canonical_qualified_name`). Skipping with a stderr line matches the existing syntax-error posture (`extractor.py:103-106`).

**Supersedes**: UQ-WP3-11 (`wp3-python-plugin.md:322-327` — "empty `.py` file → empty entities array"). UQ-WP3-11 was scoped to *function* extraction at Sprint-1 close. B.2 introduces module as a new kind; for module entities specifically, the always-emit policy applies. The function-extraction part of UQ-WP3-11 still holds: an empty file produces zero *function* entities.

### Q2 — `parent_id` wire field

**Decision**: defer to B.3. B.2 entities are flat — no `parent_id` at the wire.

**Why**:
- No requirement cites parent_id-on-wire for B.2. Adding it now is gold-plating against a non-existent consumer; no Sprint-2 read path traverses entity hierarchy.
- Forces every existing function-entity test to gain a `parent_id` assertion for a field with no validating consumer (VER-without-VAL trap).
- Creates dual-source-of-truth reconciliation debt: `parent_id` and `contains` edges encode the same one-to-many fact. B.3 introduces both together (or, per the architecture critic's stronger reading of ADR-022:52, `contains` edges only with core-side parent_id derivation — that question stays open for B.3 to resolve).
- ADR-022:52 is ambiguous on whether plugins emit parent_id directly or core derives it from `contains` edges. The ambiguity is fine to leave unresolved here because B.2 emits neither.

**B.3 guardrail (forward-looking)**: `parent_id ≠ qualified_name.rsplit('.', 1)[0]` for at least one fixture case. Decorator-emitted entities and conditional defs may produce qualnames where the parent isn't a literal prefix; B.3 must not assume string-split derivation.

### Q3 — Extractor code organization

**Decision**: per-kind builder functions dispatched by AST type from `_walk` using `match`.

```python
def _walk(node, parents, dotted_module, file_path, out):
    for child in ast.iter_child_nodes(node):
        match child:
            case ast.FunctionDef() | ast.AsyncFunctionDef():
                out.append(_build_function_entity(child, parents, dotted_module, file_path))
            case ast.ClassDef():
                out.append(_build_class_entity(child, parents, dotted_module, file_path))
        _walk(child, [*parents, child], dotted_module, file_path, out)
```

`_build_module_entity` is called from `extract()` *before* `_walk` runs (one module entity prepended), not from inside `_walk`.

**Why**:
- The three kinds have different field sets — `Module` lacks `lineno`/`end_lineno`; class has no `<locals>` semantics; functions need decorator handling later. A single polymorphic `_build_entity(kind=...)` would hide three different field shapes behind a `kind` argument, replacing `_walk`'s explicit dispatch with internal type-checking.
- B.3 will add per-kind outgoing edges (function → calls/decorated_by; class → inherits_from/decorated_by; module → imports). Per-kind builders make that a localised addition; one fat builder forces a conditional cascade at the riskiest place to introduce bugs.
- Strategy/dispatch dict is over-engineering at three kinds.

**Renames in B.2**: `_build_entity` → `_build_function_entity`. The module-level constant `_KIND = "function"` is removed; per-builder kind is inline (`"function"` / `"class"` / `"module"`). Don't defer the rename to B.3 — doing it under pressure during edge-emission is worse than the one-line change now.

**Shared helpers**: extract `_module_source_range(source: str) -> SourceRange` (Q4) and any qualname-assembly logic shared between classes and functions as free functions called by all builders. Otherwise per-kind builders silently diverge on shared concerns.

### Q4 — Module entity `source_range`

**Decision**: `{"start_line": 1, "start_col": 0, "end_line": source.count("\n") + 1, "end_col": 0}` for **all** module entities, regardless of `parse_status`. The source string is available to `extract()` whether or not it parses, so the formula applies uniformly.

**Why**:
- `source_range` is required by every consumer that reads entity ranges (storage, briefing, finding attachment). Omitting it forces every reader to special-case module entities — propagates exception handling indefinitely.
- Computing exact `end_col` (option b) requires re-reading source for `splitlines()[-1]` length and triples the test surface (BOM, CRLF, no-trailing-newline) for zero analytic benefit. No consumer attaches to module-level column precision.
- Zero-range `{1,0,1,0}` is a lie (the module isn't a one-line zero-column point).
- The whole-file cover answers "does this finding fall inside the module entity?" with `start_line ≤ finding_line ≤ end_line` — clean for finding attachment in WP4.

**Documented sentinel convention** (hidden-invariant warning from QA): for **module** entities specifically, `end_col = 0` means "end-of-file sentinel," not "column zero of last line." Class and function entities' `end_col` is real column data from `ast.ClassDef.end_col_offset` / `ast.FunctionDef.end_col_offset`. B.4 catalog rendering and B.5 per-subsystem markdown must not infer column semantics by analogy across kinds. This convention is documented in the extractor docstring + this design doc.

## 4. Wire shape additions

`RawEntity.extra` carries one new field for module entities:

- `parse_status: "ok" | "syntax_error"` — rides through `extra` via serde flatten; host stores into `properties_json` (subject to `MAX_ENTITY_EXTRA_BYTES`, currently 64 KiB). Empty/comment-only files use `"ok"` because they parse successfully and emit a whole-file module entity with zero function/class children.

No structural wire-shape change at the host. No top-level field addition.

The plugin's `RawEntity` TypedDict (Python side) is introduced in B.2 to replace the current `dict[str, Any]` return type. This sets up mypy-strict catching kind/source mismatches at build time. Shape:

```python
class SourceRange(TypedDict):
    start_line: int
    start_col: int
    end_line: int
    end_col: int

class EntitySource(TypedDict):
    file_path: str
    source_range: SourceRange

class RawEntity(TypedDict):
    id: str
    kind: str  # currently "function" | "class" | "module"; not narrowed to keep extension cheap
    qualified_name: str
    source: EntitySource
    # parse_status is on module entities only; modelled as NotRequired
    parse_status: NotRequired[Literal["ok", "syntax_error"]]
```

`extract()` returns `list[RawEntity]`. The host's `serde(flatten) extra` map absorbs `parse_status` without Python-side wire negotiation.

## 5. Manifest + version-bump lockstep

In a single commit:

| File | Change |
|---|---|
| `plugins/python/plugin.toml` | `[ontology].entity_kinds = ["function", "class", "module"]`; `[ontology].ontology_version = "0.2.0"` |
| `plugins/python/src/clarion_plugin_python/server.py` | `ONTOLOGY_VERSION = "0.2.0"` |
| `plugins/python/src/clarion_plugin_python/__init__.py` | bump `__version__` (the package version) — separate decision; see §10 |

**Lint guard suggestion** (from Python-engineer review): a small shell assertion in the CI `python-plugin` job that greps `[ontology].ontology_version` from `plugin.toml` and `ONTOLOGY_VERSION = "..."` from `server.py` and fails if they disagree. Out of scope for B.2 itself but worth filing as a follow-up.

## 6. Test plan

Test pyramid (per QA review): ~70% unit / ~25% integration / ~5% e2e by test count.

### Unit tests (~15–20 in `plugins/python/tests/`)

New tests in `test_extractor.py`:

- `test_module_entity_emitted_for_every_call` — one module entity per `extract()`, `kind == "module"`, `id == "python:module:<dotted>"`.
- `test_module_entity_for_empty_file` — one module entity, `parse_status == "ok"` (empty source parses), `source_range == {1,0,1,0}`.
- `test_module_entity_for_syntax_error_file` — one module entity, `parse_status == "syntax_error"`, no function/class entities.
- `test_module_entity_for_init_py_collapses_to_package` — `pkg/__init__.py` produces `python:module:pkg`.
- `test_top_level_init_py_skipped_with_stderr` — top-level `__init__.py` (no parent package) produces `[]` and one stderr line.
- `test_class_entity_simple` — `class Foo: pass` → one class entity at `python:class:<dotted>.Foo` plus one module entity.
- `test_class_entity_nested` — `class A: class B: pass` → two class entities (`A`, `A.B`) plus one module entity.
- `test_class_method_emitted_as_function` — `class Foo: def bar(self): pass` → one class entity (`Foo`) and one function entity (`Foo.bar`); class methods continue as `function` kind.
- `test_class_in_function_qualname` — `def f(): class C: pass` → class entity at `f.<locals>.C`.
- `test_async_class_method` — async functions inside classes still emit as `function` kind.
- `test_class_source_range_from_ast` — class entity uses `node.lineno`/`end_lineno` (real column data, not the module sentinel).

Renames:

- `test_empty_file_yields_zero_entities` → `test_empty_file_yields_one_module_entity` (per Q1 resolution; documents the semantic shift). New assertion: length 1, kind `module`, `parse_status == "ok"`.
- `test_whitespace_only_file_yields_zero_entities` → `test_whitespace_only_file_yields_one_module_entity`.

New test in `test_extractor.py` for the helper:

- `test_module_source_range_no_trailing_newline` — file ending without `\n` still produces correct `end_line`.
- `test_module_source_range_crlf` — CRLF-terminated file produces same `end_line` as LF (Python's `count('\n') + 1` handles this naturally).

### Integration tests

- `crates/clarion-core/tests/wp2_e2e_*.rs` — host-side: a fixture file with `def hello():` and `class Foo:` produces three entities (one module, one function, one class) with the right kinds and IDs. The existing `wp2_e2e_smoke_fixture_plugin_round_trip` test will need updating; the test should assert on entity-kinds-set rather than exact count.

### End-to-end

- `tests/e2e/sprint_1_walking_skeleton.sh` — currently asserts exactly 1 entity (`python:function:demo.hello|function`). Under B.2, the demo file produces 2 entities: `python:module:demo` + `python:function:demo.hello`. Update to assert exactly 2 entities with both kinds present (per QA recommendation — `≥1 of each kind` is too loose; would mask double-emit regressions).

### Cross-language fixture (`fixtures/entity_id.json`)

Grow with module + class rows for byte-for-byte parity:

- `python:module:pkg.mod` from `("python", "module", "pkg.mod")`.
- `python:module:pkg` from `("python", "module", "pkg")` (`__init__.py` collapse case).
- `python:class:pkg.mod.Foo` from `("python", "class", "pkg.mod.Foo")`.
- `python:class:pkg.mod.Foo.Bar` (nested class).
- `python:class:pkg.mod.f.<locals>.C` (class-in-function).

Both `crates/clarion-core/src/entity_id.rs::tests::shared_fixture_byte_for_byte_parity` and `plugins/python/tests/test_entity_id.py::test_matches_shared_fixture` consume the same file; divergence fails CI on both sides.

### Round-trip self-test

`plugins/python/tests/test_round_trip.py:128` currently has a loop asserting `entity["kind"] == "function"` for every entity. Under B.2 this fails. Replace with by-kind invariants:

```python
function_entities = [e for e in entities if e["kind"] == "function"]
module_entities = [e for e in entities if e["kind"] == "module"]
class_entities = [e for e in entities if e["kind"] == "class"]

# Invariants — no exact totals (those become merge-conflict generators).
assert len(module_entities) == 1, "exactly one module entity per analyzed file"
assert all(e["source"]["file_path"] == str(target) for e in entities)
assert any(e["qualified_name"] == "clarion_plugin_python.extractor.extract" for e in function_entities)
```

## 7. Implementation task ledger

### Task 1 — TypedDict shapes for `RawEntity` + `_build_module_entity`

Files:
- Modify `plugins/python/src/clarion_plugin_python/extractor.py` — add TypedDict imports, define `SourceRange`/`EntitySource`/`RawEntity`, change `extract` return annotation, add `_module_source_range` helper, add `_build_module_entity`.

Steps:
- Failing test: `test_module_source_range_no_trailing_newline`.
- Implement `_module_source_range`.
- Failing test: `test_module_entity_emitted_for_every_call`.
- Add `entities.append(_build_module_entity(...))` at top of `extract()` after `ast.parse` succeeds, *before* `_walk`.
- Failing test: `test_module_entity_for_syntax_error_file`.
- Modify the `except SyntaxError` branch in `extract()` to emit a degraded module entity with `parse_status: "syntax_error"` instead of returning `[]`.
- Failing test: `test_top_level_init_py_skipped_with_stderr`.
- Detect empty `module_dotted_name` and skip with stderr.
- Verify all existing function-entity tests still pass (the rename to `_build_function_entity` happens in Task 2, not yet here).
- Commit: `feat(wp3): module entity emission with parse_status (B.2 Q1)`.

### Task 2 — Per-kind builder split + `_build_class_entity`

Files:
- Modify `plugins/python/src/clarion_plugin_python/extractor.py` — rename `_build_entity` → `_build_function_entity`; add `_build_class_entity`; switch `_walk` to `match` dispatch; remove `_KIND` module-level constant.

Steps:
- Failing test: `test_class_entity_simple`.
- Rename `_build_entity` → `_build_function_entity`. Inline the kind literal `"function"`.
- Add `_build_class_entity` (reuses `reconstruct_qualname`; uses `node.lineno`/`end_lineno` directly — Module-style sentinel does NOT apply here).
- Switch `_walk` to `match` statement.
- Run all unit tests; expect green.
- Failing tests: `test_class_entity_nested`, `test_class_in_function_qualname`, `test_class_method_emitted_as_function`, `test_class_source_range_from_ast`.
- Verify pass.
- Commit: `feat(wp3): class entity emission + per-kind builders (B.2 Q3)`.

### Task 3 — Manifest + ONTOLOGY_VERSION lockstep bump

Files:
- Modify `plugins/python/plugin.toml`.
- Modify `plugins/python/src/clarion_plugin_python/server.py`.

Steps:
- Update `plugin.toml::[ontology].entity_kinds` to `["function", "class", "module"]`; `ontology_version` to `"0.2.0"`.
- Update `server.py::ONTOLOGY_VERSION = "0.2.0"`.
- Verify `pytest plugins/python` green; the `test_initialize_roundtrip` test will pick up the new ontology_version automatically (it asserts the constant, not a literal).
- Commit: `feat(wp3): ontology v0.2.0 — entity_kinds += class, module (B.2)`.

### Task 4 — Cross-language fixture parity (`fixtures/entity_id.json`)

Files:
- Modify `fixtures/entity_id.json` — add 5 new rows (per §6).

Steps:
- Add module + class fixture rows.
- Run `cargo nextest run -p clarion-core entity_id::tests::shared_fixture_byte_for_byte_parity`.
- Run `pytest plugins/python/tests/test_entity_id.py::test_matches_shared_fixture`.
- Both green.
- Commit: `test(wp3): cross-language fixture parity for class + module entities (B.2)`.

### Task 5 — Round-trip self-test update

Files:
- Modify `plugins/python/tests/test_round_trip.py`.

Steps:
- Replace exact-total assertions with by-kind invariants per §6.
- Verify pass.
- Commit: `test(wp3): round-trip by-kind invariants (B.2)`.

### Task 6 — Walking-skeleton e2e update

Files:
- Modify `tests/e2e/sprint_1_walking_skeleton.sh`.

Steps:
- Update entity-count assertion: now expects exactly 2 entities (`python:module:demo` + `python:function:demo.hello`).
- Run e2e locally; expect pass.
- Commit: `test(wp3): walking skeleton expects module + function entities (B.2)`.

### Task 7 — Existing-test renames

Files:
- Modify `plugins/python/tests/test_extractor.py`.

Steps:
- `test_empty_file_yields_zero_entities` → `test_empty_file_yields_one_module_entity` with updated assertion (length 1, kind module, parse_status "ok").
- `test_whitespace_only_file_yields_zero_entities` → same shape.
- Verify pass.
- Commit: `test(wp3): rename empty-file tests for B.2 module-emit semantics`.

### Task 8 — Documentation lock + close

Files:
- Modify `docs/implementation/sprint-1/wp3-python-plugin.md` — add forward-pointer to this design doc near §1 "out of scope for Sprint 1" mentioning that classes + modules are realised in Sprint 2 / B.2.
- Verify all ADR-023 gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo build --workspace --bins`, `cargo nextest run --workspace --all-features`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`, `cargo deny check`, `ruff check`, `ruff format --check`, `mypy --strict`, `pytest`, `bash tests/e2e/sprint_1_walking_skeleton.sh`. All green.
- Commit: `docs(wp3): forward-pointer Sprint-1 → B.2 + close design`.

## 8. Filigree umbrella + tracking

A single B.2 umbrella issue is filed at `clarion-daa9b13ce2` with labels `sprint:2`, `wp:3`, `release:v0.1`, `tier:b`.

Sub-tasks (Tasks 1–8 above) live as inline checklist items rather than separate filigree issues. The kickoff handoff convention: "the WP3 feature-complete WP doc plus `system-design.md §6` and `detailed-design.md §5` are detailed enough that an inline task list (no `/docs/superpowers/plans/` file) is often the right move."

## 9. Exit criteria

B.2 is done for Sprint 2 when all of:

- Every analyzed `.py` file produces exactly one module entity (verified by walking-skeleton e2e + unit + round-trip self-test).
- Every `ast.ClassDef` produces exactly one class entity with correct L7 qualname (including nesting and class-in-function).
- Class methods continue as `function` entities; no behavioural change for existing function emission.
- `plugin.toml::[ontology].entity_kinds` and `server.py::ONTOLOGY_VERSION` move in lockstep to `0.2.0`.
- Cross-language fixture (`fixtures/entity_id.json`) has module + class rows; both Rust and Python tests pass.
- All ADR-023 gates green on the closing commit.
- This design doc reviewed and approved by user.

## 10. Implementation-phase decisions (resolved post-spec-review)

- **`__version__` package bump**: `clarion_plugin_python.__version__` moves from `0.1.0` to `0.1.1` (patch). The package version tracks code releases; the ontology version tracks declared kind set. They are different concepts — no breaking API change ships in B.2, so the patch bump is correct.
- **Top-level `__init__.py` skip — stderr line wording**: `clarion-plugin-python: skipping <path>: top-level __init__.py has no package name\n`. Matches the existing syntax-error skip convention at `extractor.py:103-106` (`clarion-plugin-python: skipping <path>: <reason>\n`).
- **Lint guard for ontology-version drift**: deferred as a follow-up — filigree [`clarion-8befae708b`](../../../.filigree/) (P3 task; not blocking B.2). B.2 ships with the two values agreeing by inspection; the lint guard is risk insurance against future drift.

## 11. Panel-review record

The four design questions (Q1 emission policy, Q2 parent_id wire, Q3 code organization, Q4 module source_range) were each taken to a five-reviewer panel (systems-thinker, solution-architect, architecture-critic, Python-engineer, quality-engineer) before being locked here. Panel verdicts:

| Q | Decision | Vote pattern | Key minority view (if any) |
|---|---|---|---|
| Q1 | (d) always emit + parse_status property + skip top-level __init__.py | architecture-critic proposed (d) extending my (a); 2× (a), 1× (b), 1× (c) before reconciliation | solution-architect's (b) was driven by UQ-WP3-11 lock-in concern — addressed by the supersession rationale |
| Q2 | (b) defer to B.3 — flat entities in B.2 | 4× (b), 1× (d) "never on wire"; reconciled to (b) | architecture-critic's (d) "parent_id NEVER on wire" stays open for B.3 to resolve |
| Q3 | (b) per-kind builders + match dispatch | 5× (b) unanimous | none |
| Q4 | (a) `{1,0,last_line,0}` whole-file cover | 5× (a) unanimous | none |

Reviewer transcripts and verbatim verdicts are in the brainstorming conversation log on this branch.
