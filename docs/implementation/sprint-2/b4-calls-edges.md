# B.4* — Python plugin: `calls` edges via pyright + confidence tiers (Sprint 2 amended / Tier B)

**Status**: IMPLEMENTED — Sprint 2 amended Tier-B B.4* work-package design closed; see §9 exit criteria and [B.4* gate results](./b4-gate-results.md)
**Anchoring design**: [system-design.md §4 (Plugin host / analyze pipeline)](../../clarion/v0.1/system-design.md), [detailed-design.md §"Python plugin specifics — call graph precision"](../../clarion/v0.1/detailed-design.md), [scope-amendment-2026-05.md](./scope-amendment-2026-05.md)
**Accepted ADRs**: [ADR-002](../../clarion/adr/ADR-002-plugin-transport-json-rpc.md), [ADR-003](../../clarion/adr/ADR-003-entity-id-scheme.md), [ADR-007](../../clarion/adr/ADR-007-summary-cache-key.md), [ADR-022](../../clarion/adr/ADR-022-core-plugin-ontology.md), [ADR-023](../../clarion/adr/ADR-023-tooling-baseline.md), [ADR-024](../../clarion/adr/ADR-024-guidance-schema-vocabulary.md), [ADR-026](../../clarion/adr/ADR-026-containment-wire-and-edge-identity.md), [ADR-027](../../clarion/adr/ADR-027-ontology-version-semver.md), [ADR-028](../../clarion/adr/ADR-028-edge-confidence-tiers.md)
**Predecessor**: [B.3 — `contains` edges](./b3-contains-edges.md)
**Successor**: B.5* (`references` edges), B.6 (WP8 MCP surface)
**Filigree umbrella**: `clarion-2d2d1d27b5`

---

## 1. Scope

B.4* introduces the first plugin-emitted edge kind that requires non-AST resolution — function/method `calls` extracted via pyright, tagged with per-edge confidence tiers from ADR-028. It is the first work package since B.3 to add structural surface area to a non-trivial third-party dependency (pyright) and the first to ship a per-row epistemic-strength field to consumers.

Three things change in lockstep:

- **Plugin emits `calls` edges** with `confidence` ∈ {`resolved`, `ambiguous`}. No `inferred`-tier edges are emitted at scan time (ADR-028 §"Decision 3" — inferred is lazy at MCP query time).
- **Pyright integration**: the plugin spawns a pyright subprocess at `initialize` and drives type-aware call-site resolution against it. The integration strategy is panel-resolved in Q1 below.
- **Storage gains `edges.confidence`** column + dispatching index; the writer-actor enforces a per-kind confidence contract.

**Out of scope** (deferred):

- `references` edges (B.5*, immediately following).
- `inferred`-tier edge emission (B.6 lazy MCP query path; ADR-028 §"Decision 3").
- Cross-file calls into stdlib or third-party packages (pyright's "no in-project entity" case → no edge emitted).
- `decorates`, `inherits_from`, `imports` edge kinds (later in WP3 per amended scope).
- Subsystem-level call aggregation, Leiden clustering on the call graph (ADR-006; v0.2).
- Async/coroutine call resolution beyond what pyright handles for free (no special-case logic).
- Pyright's `pyright_for_python_code` library mode — we drive the LSP, not the library directly.

## 2. Locked surfaces from Sprint 1 + B.2 + B.3 (B.4* reads and writes against these)

These are caller-observable surfaces locked at `v0.1-sprint-1`, B.2 close, and B.3 close. B.4* must not change them.

- **`AnalyzeFileResult` envelope** (ADR-026 / B.3 Q1): `{entities: [...], edges: [...]}`. B.4* adds new edges of `kind: "calls"` to the existing `edges` array; envelope unchanged.
- **`RawEdge` Rust shape** (B.3 + ADR-028): `{kind, from_id, to_id, source_byte_start, source_byte_end, confidence, properties, extra}`. B.4* is the first work package to *use* the `confidence` field added in ADR-028 (B.3 ships `confidence` on the wire but only ever sets `Resolved` for contains).
- **`EdgeConfidence` enum** (ADR-028): `{Resolved, Ambiguous, Inferred}`, ordered `Resolved < Ambiguous < Inferred` (lower = more trustworthy / less permissive). Serialised lowercase.
- **Per-kind source-range contract** (ADR-026 decision 3 / B.3 Q5): `calls` MUST carry `Some(source_byte_start)` + `Some(source_byte_end)`. B.4* is the first kind to actually exercise this side of the contract.
- **Writer-actor `dropped_edges_total` counter** (B.3 §6): increments on UNIQUE conflicts and on per-kind-contract rejections. B.4* extends what counts as a rejection (Q4).
- **`extract()` signature** (B.3 baseline): `extract(source, file_path, *, module_prefix_path) -> tuple[list[RawEntity], list[RawEdge]]`. Return type unchanged; `RawEdge` list gains heterogeneous kinds.
- **Plugin `_walk` dual-accumulator pattern** (B.3 baseline): `out_entities`, `out_edges` lists mutated in place. B.4* re-uses; call-site walks may live in a sibling helper, not inside `_walk` itself (Q1 dependent).
- **L4 JSON-RPC method set**: `initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`. B.4* changes none of these.
- **L5 manifest schema**: B.4* amends only `[ontology].edge_kinds` (adds `"calls"`), `[ontology].ontology_version` (`0.3.0` → `0.4.0`), and adds a new `[capabilities.runtime.pyright]` sub-table (Q2).
- **L8 Wardline pin**: unchanged.
- **`EdgeRecord` Rust POD** (B.3): gains a `confidence: EdgeConfidence` field. The writer-actor's `InsertEdge` path threads it through. Existing test helpers that construct edge records gain a defaulted-to-`Resolved` argument.

## 3. Design decisions (Q1–Q7 panel-resolved)

Each decision below was taken to a five-reviewer panel (plan-review-architecture, plan-review-quality, plan-review-reality, plan-review-systems, axiom-python-engineering:python-code-reviewer) before being locked here. Vote tallies and reconciliations are in §11.

### Q1 — Pyright integration strategy

Three viable strategies; each implies a different process model, JSON-RPC traffic shape, and performance envelope. The scope-amendment memo names all three (§5) but defers the choice to this design pass.

**Candidates**:

- **(a) Pyright-as-LSP**: spawn `pyright-langserver` as a long-lived subprocess at `initialize`; per call-site, issue LSP `callHierarchy/incomingCalls` (or its `outgoingCalls` dual) requests; close at `shutdown`. Pyright handles project-wide indexing and incrementality.
- **(b) AST walk + pyright as type oracle**: the plugin walks its own Python AST for call sites; for each unresolved site, asks pyright (still as LSP, but for `textDocument/hover` or `textDocument/typeDefinition`) about the called symbol's type. Plugin owns dispatch; pyright resolves only types.
- **(c) `pycg` + pyright escalation**: use `pycg` (pure-AST callgraph) for the bulk pass; escalate to pyright only when `pycg` flags ambiguity.

**Decision**: **(a) Pyright-as-LSP, with the LSP subprocess as a per-session resource owned by the plugin**.

**Why**:

- Pyright's `callHierarchy/incomingCalls` and `outgoingCalls` are the entry points pyright's maintainers shipped for exactly this consumer. (b) re-implements a piece of pyright (its call-site walker) inside the plugin; (c) takes on `pycg` whose last release was 2022 and whose Python-version coverage is shaky against elspeth's tooling envelope.
- The week-2 gate (§5) measures *pyright's cost at elspeth scale*. That cost dominates regardless of (a)/(b) — pyright still types the project. (a) inherits pyright's incrementality and project caching; (b) re-pays project init cost in pyright on every type-resolution request unless we hold the LSP subprocess open anyway (at which point (b) is just (a) plus an extra AST walk in the plugin).
- (b) and (c) put *call-pattern recognition* — knowing what "Protocol dispatch" or "dict-of-callables" or "decorated function" looks like — into the plugin. Pyright already knows these. Reimplementing them in the plugin is the kind of "lightweight glue" the Loom federation axiom warns against (cross-product reasoning collapse, not federation).
- LSP serialization overhead is real but bounded: typical call-hierarchy responses are kilobytes; per-file query counts are bounded by call-site count, which is bounded by file size. At elspeth's ~425k LOC, this is millions of LSP messages, not billions.

**Process model** (panel-revised):

- Plugin holds a `PyrightSession` object at `ServerState.pyright`. Lazy-initialised on first `analyze_file` (not at `initialize`, because pyright project-warm cost shouldn't be paid for plugins that get a `shutdown` before any analyze).
- The subprocess is `pyright-langserver --stdio`; the plugin speaks JSON-RPC 2.0 framed with `Content-Length` headers (same framing as the plugin's own protocol — convenient symmetry).
- **Traffic shape** (panel correction from python-engineering): per `analyze_file`, the plugin: (1) sends `textDocument/didOpen` for the file, (2) walks the AST for *function entities* (not individual call sites), (3) for each function entity, issues `textDocument/prepareCallHierarchy` once + `callHierarchy/outgoingCalls` once — pyright returns *all* outgoing call sites from that function in a single response, including byte ranges, (4) maps response ranges back to AST call-site nodes for byte-offset population, tier-tags edges (resolved if exactly one in-project target; ambiguous if N>1; emitted-nothing if zero), (5) sends `textDocument/didClose`. The cost envelope is `functions_per_file × roundtrip_latency`, not `call_sites_per_file × roundtrip_latency` — material for the week-2 gate math (Q7).
- **Per-session LSP subprocess lifecycle**: started lazily, kept alive until `shutdown`. Stderr is drained by a daemon thread started before the first LSP write; the drain thread is joined in `PyrightSession.close()` after `process.wait()`.
- **Init-hang deadline** (panel addition): the `initialize` handshake to pyright is bounded by `PYRIGHT_INIT_TIMEOUT_SECS` (default 30s). On timeout, the plugin emits `CLA-PY-PYRIGHT-INIT-TIMEOUT` and treats pyright as unavailable for the run (no `calls` edges; `unresolved_call_sites_total` += AST-counted call sites). Distinct from per-call-site timeout (5s, also bounded).
- **Crash + restart with per-run cap** (panel addition): if pyright crashes mid-session, restart count surfaces as `CLA-PY-PYRIGHT-RESTART`. Restarts capped at `MAX_PYRIGHT_RESTARTS_PER_RUN` (default 3). On cap exceeded, the *currently-analysing file* is skipped with `CLA-PY-PYRIGHT-POISON-FRAME`; subsequent files in the same run continue without pyright (treated as unavailable). This prevents a single deterministic-crash file from stalling the entire run indefinitely.
- **Init-failure (binary missing / bootstrap incomplete)** (panel addition): if pyright cannot be invoked at all (binary missing, node bootstrap failed, version negotiation failed), the plugin emits `CLA-PY-PYRIGHT-UNAVAILABLE` and continues with zero `calls` edges + `unresolved_call_sites_total` += AST-counted call sites. The run completes; the operator sees the finding.

### Q2 — Pyright provisioning

The pyright binary must be present in the plugin's runtime environment. Three options:

- **(a) Pinned `pyright` PyPI package as a `[project.dependencies]` entry**: `pip install clarion-plugin-python` brings pyright in; pyright's bootstrapper downloads node + the LSP on first run.
- **(b) Operator pre-installs pyright separately**: plugin assumes a working `pyright-langserver` on PATH or in a configurable location; refuses to start without it.
- **(c) Bundle pyright with the plugin distribution**: bake into a wheel or supplementary archive; no first-run network access.

**Decision**: **(a) pinned PyPI package, with the pin recorded in `plugin.toml` so the manifest captures the version the host expects**.

**Why**:

- (a) gives a single install path that matches the Sprint-1 plugin install story (`pip install -e plugins/python[dev]`); operators don't need a node provisioning conversation.
- (b) creates a setup-vs-runtime asymmetry that's a footgun at agent-driven analyze time: a working dev env is no guarantee an operator's environment has pyright. Manifest-level version mismatch detection becomes harder.
- (c) is heaviest — pyright + node bundled is ~50 MB per supported platform; multiplies wheel matrix; bootstrapper download already moves this off the wheel.

**Pin policy**: the pin lives in two places — `pyproject.toml` `[project.dependencies]` (pip's source of truth) and `plugin.toml` `[capabilities.runtime.pyright]` (the manifest's declaration so the host can surface mismatches as findings). The version is bumped intentionally per release of B.4* / B.5* — not via dependabot auto-bumps — because pyright's call-hierarchy output is not API-stable across minor versions (pyright is on a calendar release schedule and breaks output shapes occasionally). The pin target is whatever pyright version is current at B.4*'s implementation start.

**Manifest addition** (panel-corrected — the existing manifest uses `[capabilities.runtime]`, not `[runtime]` — so the pyright pin lives under it, not at a new top-level section):

```toml
[capabilities.runtime.pyright]
# Pyright version the plugin was tested against. Host surfaces a CLA-PY-PYRIGHT-VERSION-DRIFT
# finding (warning, not fatal) if `pyright --version` at runtime differs from this pin.
pin = "1.1.X"  # exact version set at B.4* impl start
```

**First-run hardening** (panel additions):

- After `pip install`, the `pyright` PyPI package downloads node + the LSP binary on *first invocation*, not at install time. The plugin checks `pyright-langserver --version` executability before the first LSP write. On failure (binary absent, node bootstrap failed, network egress restricted), the plugin emits `CLA-PY-PYRIGHT-INSTALL-FAILURE` and treats pyright as unavailable (Q1 init-failure path).
- The pyright subprocess is launched with an explicit `env` (passing through `PATH` and `HOME` only, plus the pyright wrapper's expected cache directory `~/.cache/pyright-python/`) so a system-installed `pyright` on `PATH` cannot shadow the managed binary.
- CI gains a cache key for `~/.cache/pyright-python/` to avoid re-downloading node on every cold run.
- Dependabot is configured to ignore the `pyright` package (or, if dependabot is not configured, a TODO comment beside the `pyproject.toml` pin documents the manual-bump policy). This prevents auto-bumps from silently breaking the two-source pin parity (`pyproject.toml` + `plugin.toml`) that `CLA-PY-PYRIGHT-VERSION-DRIFT` is designed to catch.

### Q3 — `properties.candidates` shape for ambiguous edges

ADR-028 §"Decision 1" reserves `properties.candidates` for the candidate set on `ambiguous` edges but does not specify the shape. Two candidates:

- **(a) `list[str]`** — bare entity IDs of the other candidates pyright surfaced.
- **(b) `list[{id: str, evidence?: str, model_confidence?: float}]`** — entity IDs plus optional per-candidate metadata.

**Decision**: **(a) `list[str]`**.

**Why**:

- Pyright's call-hierarchy response gives us symbol references, not per-candidate "evidence strings"; (b) would require us to invent the evidence field with no genuine source signal.
- B.6's MCP tool `callers_of(id, confidence)` expands an ambiguous edge by including each candidate as a sibling caller (per ADR-028 + the scope-amendment memo); that expansion needs only the IDs.
- Future need for richer per-candidate metadata is a properties-bag addition (`properties.candidate_metadata`), not a schema change — shape (a) doesn't paint us into a corner.
- KISS, with documented escape hatch in ADR-028.

**Shape lock** (panel-corrected — uses `NotRequired` per the project's existing TypedDict convention, not `total=False`, to match `RawEntity`'s `parent_id`/`parse_status` precedent):

```python
class CallsEdgeProperties(TypedDict):
    candidates: NotRequired[list[str]]  # other candidate to_ids when confidence == "ambiguous"
```

**Best-guess `to_id` for ambiguous edges**: the plugin picks the alphabetically-first candidate via `sorted(candidate_ids)[0]` (Unicode code-point order, locale-independent, deterministic across Python versions and OSes) as `to_id` and lists the rest in `candidates`. Alphabetical isn't a quality heuristic — it's a *stable* heuristic so re-analyze is deterministic. The MCP tool is what does the user-meaningful expansion.

**B.6 consumer constraint** (panel-added to forestall silent semantic drift): B.6's `callers_of`, `execution_paths_from`, and `neighborhood` tools MUST NOT treat the `to_id` of an `Ambiguous` edge as "pyright's best guess." It is a *stability heuristic* (alphabetical) used purely to keep the natural-key `(kind, from_id, to_id)` deterministic under re-analyze. The semantically meaningful operation in B.6 is to expand `to_id ∪ properties.candidates` into the full candidate set whenever the consumer reaches an ambiguous edge. This constraint is documented here because the consumer (B.6) lives in a different design pass; it's load-bearing for honesty.

### Q4 — Writer-actor per-kind confidence contract

The writer must enforce that confidence tiers obey the per-kind invariant from ADR-028 §"Decision 1":

- Structural kinds (`contains`, `in_subsystem`, `guides`, `emits_finding`) MUST carry `confidence = Resolved`.
- Anchored kinds (`calls`, `references`, `decorates`, `inherits_from`, `imports`) MUST carry an explicit confidence tier.
- At scan time, anchored kinds MUST NOT carry `Inferred` (inferred is lazy at MCP query time, ADR-028 §"Decision 3").

**Decision** (panel-revised — substantially simpler than the draft):

Extend the *existing* `enforce_edge_contract` function at `crates/clarion-storage/src/writer.rs:343` with a confidence check; do NOT add a parallel `enforce_edge_confidence_contract`. The existing function already dispatches by kind class and emits `CLA-INFRA-EDGE-UNKNOWN-KIND` for unknown kinds; the new confidence check folds into the same per-kind match arms:

```rust
// Inside the existing enforce_edge_contract — new arms folded in:
match (kind_class, edge.confidence) {
    (EdgeKindClass::Structural, EdgeConfidence::Resolved) => { /* fall through to existing source-range check */ }
    (EdgeKindClass::Structural, _) => return Err(violation(
        "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT",
        "structural edge must carry confidence=resolved",
    )),
    (EdgeKindClass::Anchored, EdgeConfidence::Inferred) => return Err(violation(
        "CLA-INFRA-EDGE-CONFIDENCE-CONTRACT",
        "inferred-tier edges are query-time-only at scan time",
    )),
    (EdgeKindClass::Anchored, _) => { /* fall through to existing source-range check */ }
    (EdgeKindClass::Unknown, _) => { /* existing arm emits CLA-INFRA-EDGE-UNKNOWN-KIND */ }
}
```

**`WriteOrigin` enum is DEFERRED to B.6** (panel decision, reversal of the draft):

The draft introduced a `WriteOrigin::{Scan, Query}` enum on `WriterCmd::InsertEdge` to make scan-vs-query a write-time-checked invariant, enabling B.6's MCP server to use the same `InsertEdge` command with `WriteOrigin::Query` for inferred edges. Both the architecture and systems reviewers flagged this as premature abstraction with under-specified semantics:

- B.4* itself has only one consumer (Scan). The `Query` arm ships uncovered.
- The enum's *semantic* is binary ("may this write path emit Inferred?") — a bool would communicate that more honestly than an enum.
- B.6 may discover it wants a dedicated `InsertInferredEdge` command (with different ack semantics, batching, or cache-write coupling) rather than reusing `InsertEdge`. Locking the enum shape now constrains B.6 unnecessarily.

**Resolution**: at B.4* scan time, the writer rejects `Inferred` unconditionally (the arm above) — no `WriteOrigin` parameter. B.6's design pass introduces whatever discriminator (bool, enum, new command) it determines is honest *with the actual MCP write path in hand*.

**Why the new framing**:

- Single-implementer rule. Don't ship an abstraction with one consumer.
- The check is now strictly local to scan-time — no scan-vs-query coupling.
- Violations still increment `dropped_edges_total` AND surface as findings (matches B.3's `CLA-INFRA-EDGE-SOURCE-RANGE-CONTRACT` pattern).
- The check runs per-edge, not at batch commit time — fail fast on the first malformed edge rather than aggregating to "20 edges dropped, here's a sample."

**Test coverage required for the contract** (quality-reviewer additions — all of these are required in Task 2's RED tests):

- `(contains, Ambiguous)` → rejected with `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT`; assert `dropped_edges_total == 1`; assert finding list contains the named code.
- `(contains, Inferred)` → rejected; same assertions (force-traverses the structural-with-non-resolved arm a second way).
- `(in_subsystem, Inferred)` → rejected (force-coverage of a *second* structural kind, not just `contains`).
- `(calls, Inferred)` → rejected with `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT`; assert counter + finding.
- `(unknown_kind, Resolved)` → rejected with `CLA-INFRA-EDGE-UNKNOWN-KIND`; assert counter + finding (currently-untested arm of the existing function).
- `(calls, Ambiguous)` → ACCEPTED; assert row inserted; assert `ambiguous_edges_total == 1`.
- `(calls, Resolved)` → ACCEPTED; assert row inserted; counters untouched.

### Q5 — Inferred-edge storage layout (forward-looking — locked here so B.6 inherits)

ADR-028 §"Open Questions" defers: "same `edges` table with `confidence='inferred'` rows, or separate `inferred_edges_cache` table?"

This question is forward-looking — B.4* itself never writes Inferred rows. But it's load-bearing for B.6 (the MCP query path), and resolving it now means B.4*'s schema doesn't need a follow-up migration when B.6 ships.

- **(a) Same `edges` table** with `confidence='inferred'` rows. One table, one set of indexes, one query path. Audit signal is the `confidence` column itself.
- **(b) Separate `inferred_edges_cache`** table. Keeps `edges` audit-clean ("every row here was produced by analyze"). Two tables, two index sets, query-time UNION.

**Decision**: **(a) same `edges` table with `confidence='inferred'` rows**.

**Why**:

- The cost of (a) is auditability — but the `confidence` column itself is the audit signal: any query can `WHERE confidence != 'inferred'` to get a static-analysis-only view. We didn't need a separate table to keep `Ambiguous` distinguishable; same for `Inferred`.
- The cost of (b) is duplicated indexes (the `ix_edges_kind_confidence` index would need to exist on both tables), query-time UNION, and a sympathy-bug surface: a `callers_of` query that forgets to UNION returns half the answer silently.
- B.4*'s scope is scan-time only — no Inferred-row writes happen yet — so the choice is about *future shape*, not current code. Pick the simpler one and move on; revisit if observed.

**Reversibility — explicit migration sketch** (panel revision — the draft called this "reversible" twice without supporting that claim; the architecture review flagged this as architectural work the word couldn't carry). The wire is unconditionally reversible: `confidence` stays on `RawEdge` either way. Storage layout migration cost, if we ever change our minds:

1. Author an additive migration (`0002_split_inferred_edges.sql`) creating `inferred_edges_cache` with the same columns + index as `edges` minus the `confidence` CHECK enumeration; copy `SELECT * FROM edges WHERE confidence='inferred'` into it; delete the inferred rows from `edges`.
2. Update the MCP query path to UNION over both tables for `confidence>=Inferred` queries, querying only `edges` otherwise.

Cost is O(inferred-row-count) at migration time plus the UNION refactor (one query-builder layer touch) — bounded, but not free. ADR-024's in-place edit permission retires the first time B.4*'s edge rows are persisted by a published build, so this hypothetical migration would be additive, not in-place. The `EXPLAIN QUERY PLAN` assertion on the `ix_edges_kind_confidence` index (Task 1's quality-review addition) is what makes this reversibility *cheap-to-verify*: if the index isn't being used, the (a)-vs-(b) cost balance shifts and the migration may be cheaper to perform proactively.

**Recorded as resolved in ADR-028 §"Open Questions"**: this design doc closes the "Storage location for inferred edges" open question by reference. ADR-028 gets a one-line amendment ("Resolved by B.4* design — same `edges` table; see `b4-calls-edges.md` Q5"). Task 12 carries the amendment.

### Q6 — Manifest + ontology_version bump

Mechanical, per ADR-027 precedent (mirrors B.3 Q6 exactly).

| File | Change |
|---|---|
| `plugins/python/plugin.toml` | `[ontology].edge_kinds = ["contains", "calls"]`; `[ontology].ontology_version = "0.4.0"`; NEW `[capabilities.runtime.pyright]` sub-table w/ `pin` (Q2) |
| `plugins/python/src/clarion_plugin_python/server.py` | `ONTOLOGY_VERSION = "0.4.0"` |
| `plugins/python/src/clarion_plugin_python/__init__.py` | `__version__` PATCH bump (e.g., `0.1.2` → `0.1.3`) — additive edge kind, no breaking external API |
| `plugins/python/pyproject.toml` | Add `pyright == X.Y.Z` to `[project.dependencies]` (Q2's pinned version) |

ADR-027 policy: MINOR ontology bump (additive `calls` edge kind), PATCH package bump (no API change to `extract()` signature — the return type is already `tuple[list[RawEntity], list[RawEdge]]` after B.3).

### Q7 — Week-2 go/no-go gate operationalization

The scope-amendment memo §5 names the gate but does not specify the test corpus, the run environment, or the result-recording shape. The draft locked corpus + run-env + format; both quality and systems reviewers flagged the gate as a *vibes check* until additional reproducibility hooks land. This section is substantially expanded from the draft.

**(a) Corpus**:

- A 50–100-file representative subset of elspeth-slice, vendored under `tests/perf/elspeth_mini/` (or pulled via a fixture-fetching script if license attribution requires it).
- MUST include ≥1 of each: heavy Protocol dispatch, dict-of-callables call site, decorated function chain, deeply-nested module package, third-party-typed callable.
- MUST include ≥1 **synthetic file** under `tests/perf/synthetic/` that exercises the same patterns *without* requiring elspeth access — so any contributor (and CI) can run the gate without an elspeth license.

**(b) Run environment + reproducibility**:

- Developer-laptop manually-triggered via `scripts/b4-gate-run.sh`. NOT executed on every CI run (too slow).
- The script's header declares a **calibration baseline machine** (e.g., "Apple M2 Pro, 16 GiB, macOS 14.X; reference run: 2026-MM-DD"). A scale-factor `OPERATOR_HARDWARE_RATIO` env var (default 1.0) multiplies the thresholds for operators on slower/faster machines, with the override and the rationale recorded in the result file.
- The script EXITS NON-ZERO on Red — making `scripts/b4-gate-run.sh && echo ok || echo block` a deterministic go/no-go primitive rather than human discretion.
- A separate CI step (not the gate run itself) asserts: (1) the last entry in `b4-gate-results.md` is dated within `MAX_GATE_AGE_DAYS` (default 30); (2) its `pyright_pin` field matches `[capabilities.runtime.pyright].pin` in `plugin.toml`. Stale gate is visible on every PR.

**(c) Result format** (`docs/implementation/sprint-2/b4-gate-results.md`, append-only):

Required fields per entry:
- Date, calibration machine, `OPERATOR_HARDWARE_RATIO` (1.0 if reference machine).
- `pyright_pin` (verbatim from `plugin.toml`); `clarion_commit` (sha).
- Total wall-clock; breakdown: pyright init, per-file resolution, parent-walk overhead, CLI overhead.
- Median + p95 per-file resolution time, per-file roundtrip count (pyright LSP `outgoingCalls` requests), ambiguous-edge ratio.
- Corpus stats: file-count, function-count (drives the per-function-entity LSP traffic, Q1), unresolved-call-site-count.
- **Stated extrapolation to elspeth-full** (panel-required): given mini wall-clock T_mini and mini function-count F_mini, projected wall-clock at elspeth-full's ~425k-LOC function count F_full is approximately `T_mini × (F_full / F_mini)`. The extrapolation MUST be in the result file alongside the mini result — green-on-mini-but-projects-red-on-full is the failure mode this prevents. Operator may attach a non-linear model with rationale if they have one; the linear model is the default minimum.
- Stated extrapolation to next-tier (~4M LOC) using the same model — informational; doesn't change the gate outcome but informs whether B.4*'s shape has post-v0.1 headroom.

**Gate outcomes** (per scope-amendment §5, with revised mitigation menu):

- **Green** (<5 min total wall-clock on the elspeth-mini corpus, scaled by `OPERATOR_HARDWARE_RATIO`, AND extrapolated elspeth-full projection <60 min): proceed with B.4* implementation as designed.
- **Yellow** (5–30 min mini OR extrapolated full 60–360 min): document in `b4-gate-results.md`; engineer-panel decides whether to proceed-with-mitigation or re-design. **Mitigation menu** (panel-tightened — the draft listed "parallelisation" without acknowledging its costs):
  - Per-file caching of pyright resolutions keyed on file content hash (cheap; helps re-analyze paths).
  - Per-worker parallelisation — N×wall-clock reduction at a cost of N×memory and N×pyright init overhead; the gate MUST be re-run with the mitigation applied before the panel decides.
  - Narrow the call-pattern coverage to a documented subset (e.g., skip Protocol dispatch ambiguity emission).
- **Red** (>30 min mini OR extrapolated full >360 min): pause B.4* impl. **Red is likely a scope-reduction decision, not an in-sprint redesign** (panel correction — the draft framed Red as recoverable in-week, which understates the cost). Options:
  - Defer `calls` edges out of v0.1; ship MVP MCP surface with only `contains`/`references`. Re-scope to "B.4*-narrowed."
  - Fall back to strategy (b) AST-walk-with-pyright-only-on-ambiguous — substantial redesign, NOT an in-sprint pivot; would extend Sprint 2 by ~2 weeks.

**B.8 pre-committed rollback options** (panel-added — systems-review concern): the failure-discovery latency between this week-2 gate (run on mini) and B.8 (elspeth full-scale test) spans the bulk of Sprint 2's implementation work. Before B.5* begins, the B.8 design memo MUST pre-write its own yellow/red rollback options so the same scope-reduction-vs-redesign trade-off has a written playbook when B.8 hits it. Pre-writing this is the cheap insurance against B.8 panic.

**Why this is a real gate, not a vibes check**:

- `b4-gate-run.sh` is deterministic (non-zero exit on Red).
- CI catches staleness (gate result drift vs. pin).
- Extrapolation to full + next-tier means green-on-mini can't mask cliff-on-full.
- Pre-written B.8 rollback closes the failure-discovery latency window.

## 4. Wire shape additions

Summary of all wire-shape changes B.4* introduces beyond B.3:

**`AnalyzeFileResult`** (Rust):
- No envelope change (B.3 already added `edges: Vec<RawEdge>`).
- `edges` array gains heterogeneous kinds — first non-`contains` kind to appear.

**`RawEdge`** (B.3 baseline + ADR-028 baseline + B.4* concrete use):
- No struct-shape change (B.3 + ADR-028 already added `confidence: EdgeConfidence`).
- B.4* is the first kind to set `confidence` to anything other than `Resolved` (sets `Ambiguous` on multi-candidate call sites).
- B.4* is the first kind to populate `source_byte_start` + `source_byte_end` (calls are AST-anchored; pyright surfaces call-site byte ranges, plugin passes them through).
- B.4* is the first kind to populate `properties` (with `{"candidates": ["id1", "id2", ...]}` for ambiguous edges; absent for resolved).

**Python `RawEdge` TypedDict** (B.3 baseline + B.4* additions):

```python
class RawEdge(TypedDict):
    kind: str
    from_id: str
    to_id: str
    source_byte_start: NotRequired[int]  # MUST be Some for calls (B.3 contract / ADR-026)
    source_byte_end: NotRequired[int]    # MUST be Some for calls
    confidence: NotRequired[Literal["resolved", "ambiguous", "inferred"]]
    properties: NotRequired[CallsEdgeProperties]  # narrow shape per Q3
```

`confidence` is `NotRequired` on the Python side because contains-edge emission (B.3) didn't set it; the host's `serde(default)` produces `Resolved`. For `calls`, the plugin always sets it explicitly.

## 5. Storage protocol additions

**Schema migration** (in-place edit of `0001_initial_schema.sql` per ADR-024; this IS the "last permitted edit" to the edges table called out in ADR-028 §"Decision 1"):

```sql
-- Edges table (B.3 baseline + B.4* confidence column)
CREATE TABLE edges (
    kind               TEXT NOT NULL,
    from_id            TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    to_id              TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    properties         TEXT,
    source_file_id     TEXT REFERENCES entities(id),
    source_byte_start  INTEGER,
    source_byte_end    INTEGER,
    confidence         TEXT NOT NULL DEFAULT 'resolved'
                       CHECK (confidence IN ('resolved', 'ambiguous', 'inferred')),
    PRIMARY KEY (kind, from_id, to_id)
) WITHOUT ROWID;
CREATE INDEX ix_edges_from_kind        ON edges(from_id, kind);
CREATE INDEX ix_edges_to_kind          ON edges(to_id,   kind);
CREATE INDEX ix_edges_kind             ON edges(kind);
CREATE INDEX ix_edges_kind_confidence  ON edges(kind, confidence);  -- NEW (B.4*)
```

The `ix_edges_kind_confidence` index is what makes the MCP default `WHERE confidence = 'resolved'` cheap (without it, B.6's `callers_of` would force a full scan per traversal step).

**`EdgeRecord`** gains `confidence`:

```rust
#[derive(Debug, Clone)]
pub struct EdgeRecord {
    pub kind: String,
    pub from_id: String,
    pub to_id: String,
    pub confidence: EdgeConfidence,  // NEW (B.4*)
    pub properties_json: Option<String>,
    pub source_file_id: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
}
```

**`WriterCmd::InsertEdge`** shape — UNCHANGED from B.3 (panel-reversed — the draft added a `WriteOrigin` field; Q4 deferred this to B.6 with the actual query write path in hand). B.4*'s confidence contract is enforced strictly at scan-time semantics; B.6 introduces whatever scan-vs-query discriminator (bool / enum / separate command) it determines is honest.

**Writer-actor enforcement** (Q4, panel-revised):

- The existing `enforce_edge_contract` function at `crates/clarion-storage/src/writer.rs:343` is *extended in place* (not replaced or paralleled). New arms cover: (1) structural-with-non-Resolved → reject with `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT`, (2) anchored-with-Inferred → reject with the same code. Existing arms for source-range contract + unknown-kind remain.
- Violations fail the edge (counter increments, finding emitted, run continues — single-edge failure does NOT fail the run, matching B.3's UNIQUE-conflict semantics).
- The check runs per-edge (not batched at CommitRun) — failing fast on the first malformed edge surfaces the bug to the plugin author at exactly the failing site rather than aggregating to "20 edges dropped, here's a sample."
- The `ix_edges_kind_confidence` index MUST be exercised by an `EXPLAIN QUERY PLAN` test asserting it's used for `WHERE kind=? AND confidence=?` predicates. Without this, B.6's `callers_of` default (`confidence='resolved'`) silently degrades to a full scan.

**New finding codes** (panel-expanded):

| Code | Severity | Emitted when |
|---|---|---|
| `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT` | error | structural edge has confidence != resolved, OR scan-time anchored edge has confidence == inferred |
| `CLA-PY-PYRIGHT-RESTART` | warning | pyright subprocess died and was restarted; carries restart count + last-seen exit code |
| `CLA-PY-PYRIGHT-POISON-FRAME` | warning | per-run restart cap exceeded; file currently being analyzed is skipped; subsequent files in this run continue without pyright |
| `CLA-PY-PYRIGHT-INIT-TIMEOUT` | warning | pyright `initialize` handshake exceeded `PYRIGHT_INIT_TIMEOUT_SECS` (default 30s); pyright treated as unavailable for the run |
| `CLA-PY-PYRIGHT-UNAVAILABLE` | warning | pyright cannot be invoked (binary missing, node bootstrap failed); run continues with zero `calls` edges |
| `CLA-PY-PYRIGHT-INSTALL-FAILURE` | warning | pyright-langserver executability check failed before first LSP write (distinct from `-UNAVAILABLE` — fires at install-bootstrap-time only) |
| `CLA-PY-PYRIGHT-VERSION-DRIFT` | warning | `pyright --version` at runtime differs from `[capabilities.runtime.pyright].pin` |
| `CLA-PY-CALL-RESOLUTION-TIMEOUT` | warning | per-call-site LSP query exceeded configured deadline (default 5s); edge omitted |
| `CLA-PY-CALL-SITE-UNRESOLVED` | info | per-call-site unresolved (no in-project candidates); emitted optionally — see Q4-handoff in §10 / B.6 design (decision deferred for now: counter is required, finding-per-site is an open question for B.6) |

The first one belongs to clarion-core (the writer-actor); the rest belong to the Python plugin (prefix `CLA-PY-` per ADR-022).

## 6. Observability additions

**Existing `dropped_edges_total`** (B.3 §6): increments on confidence-contract rejections (new failure mode) in addition to UNIQUE conflicts + source-range-contract rejections.

**New counters on `AnalyzeFileOutcome`**:

| Counter | Increments on |
|---|---|
| `ambiguous_edges_total` | every `Ambiguous`-confidence edge that survives the writer-actor contract checks |
| `unresolved_call_sites_total` | every call site pyright surfaced as "no in-project candidate" (i.e., emitted-nothing) |
| `pyright_query_latency_p95_ms` | per-file LSP query latency, p95 over the run |

`unresolved_call_sites_total` is the load-bearing observability addition. Without it, "we did the analysis and got no edges" is indistinguishable from "we did the analysis and there are no in-project calls" — the consult-mode agent has no way to know whether to ask the inferred-tier MCP path. Surfacing it as a counter on the run summary makes the gap visible.

**Counter-vs-finding-per-site decision for B.6 (panel-flagged handoff)**: the counter is a run-level signal. For B.6's `callers_of(entity_id)` tool to know *which* entities have unresolved sites worth querying at `confidence>=Inferred`, an entity-scoped signal is needed. Three options for B.6's design:

- (a) Per-site finding (`CLA-PY-CALL-SITE-UNRESOLVED`, info severity) — entity-anchored, queryable via `findings.entity_id`. Risk: at elspeth-full scale tens of thousands of findings flood Filigree.
- (b) Per-entity counter row in a sibling table (`entity_unresolved_call_sites`) — one row per entity that has any unresolved sites, with a count.
- (c) Synthesized "edge-with-zero-candidates" rows in the `edges` table (kind=`calls`, confidence=`inferred`, properties.unresolved=true) — fits the existing schema, queryable via existing indexes, but stretches the semantic of "edge" (it's the absence of an edge).

B.4* makes the per-run counter required; B.6's design pass MUST resolve this question before `callers_of` ships. Documented here so the handoff is explicit.

`pyright_query_latency_p95_ms` is for week-2-gate post-mortems and B.8 scale-test debugging; not load-bearing for B.4* exit. Quality-reviewer addition: the counter must have a *deterministic* unit test (feed a synthetic sample list `[10, 20, ..., 1000]`-ms to the p95 accumulator and assert the expected output) AND an end-to-end smoke test that asserts `> 0` after a real pyright roundtrip — together these catch both the "p95 math wrong" and "forgot to record samples" regression paths.

## 7. Implementation task ledger

### Task 1 — Storage schema: add `confidence` column + index

Files:
- Modify: `crates/clarion-storage/migrations/0001_initial_schema.sql` (in-place edit per ADR-024 + ADR-028 last-edit clause).
- Modify: `crates/clarion-storage/tests/schema_apply.rs` (assert `confidence` column exists, CHECK constraint enforced, index present).

Steps:
- RED: schema_apply test asserts a row with `confidence='garbage'` is rejected.
- Verify RED (test fails because column doesn't exist yet).
- Edit migration to add `confidence` column + CHECK + index.
- Verify GREEN.
- Commit: `feat(storage): add edges.confidence column + index (B.4* ADR-028)`.

### Task 2 — `EdgeRecord.confidence` + extend existing `enforce_edge_contract` (panel-revised)

Files:
- Modify: `crates/clarion-storage/src/commands.rs` (add `confidence: EdgeConfidence` to `EdgeRecord`). NOTE: `WriteOrigin` enum is DEFERRED to B.6 per Q4 panel reversal — `WriterCmd::InsertEdge` shape unchanged.
- Modify: `crates/clarion-storage/src/writer.rs` (extend the EXISTING `enforce_edge_contract` function at writer.rs:343 — do not add a parallel function; fold confidence arms into the existing per-kind match; extend `dropped_edges_total` increment to cover the new rejection cause; emit `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT` finding on violation; ensure `ix_edges_kind_confidence` index hint surfaces via `EXPLAIN QUERY PLAN`).
- Modify: `crates/clarion-storage/tests/writer_actor.rs` (full contract-test matrix; see RED steps).
- Modify: `crates/clarion-storage/tests/writer_actor.rs::make_contains_edge` helper (add `confidence: EdgeConfidence` arg, default `Resolved`).
- Add: `crates/clarion-storage/tests/schema_apply.rs` EXPLAIN QUERY PLAN assertion for `WHERE kind=? AND confidence=?` using `ix_edges_kind_confidence`.

Steps (each RED requires the assertion to fail for the right reason, per TDD discipline):
- RED-1: `(contains, Ambiguous)` rejected — assert: edge rejected, `dropped_edges_total == 1`, finding list contains `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT`.
- RED-2: `(contains, Inferred)` rejected — same assertions (covers the structural arm a second way).
- RED-3: `(in_subsystem, Inferred)` rejected — covers a second *structural kind* beyond `contains`.
- RED-4: `(calls, Inferred)` rejected with same assertions.
- RED-5: `(unknown_kind, Resolved)` rejected with `CLA-INFRA-EDGE-UNKNOWN-KIND`; counter + finding asserted (this arm exists in the current function but is untested).
- RED-6: `(calls, Ambiguous)` ACCEPTED; row inserted; assert `ambiguous_edges_total == 1`.
- RED-7: `(calls, Resolved)` ACCEPTED; row inserted; counters untouched.
- RED-8: EXPLAIN QUERY PLAN on `SELECT * FROM edges WHERE kind=? AND confidence=?` returns a plan that uses `ix_edges_kind_confidence` (not a full scan).
- Verify each RED fails before implementing; implement; verify GREEN.
- Commit: `feat(storage): EdgeRecord.confidence + extend enforce_edge_contract for confidence (B.4* ADR-028 Q4)`.

### Task 3 — Host wire shape: `confidence` on `RawEdge` + `AcceptedEdge`

Files:
- Modify: `crates/clarion-core/src/plugin/protocol.rs` (add `confidence: EdgeConfidence` to `RawEdge` with `#[serde(default)]`).
- Modify: `crates/clarion-core/src/plugin/host.rs` (`AcceptedEdge` carries `confidence`; `process_edges` threads it through to `EdgeRecord`).
- Modify: `crates/clarion-core/tests/` (extend edge round-trip test to assert confidence survives the round trip).

Steps:
- RED: host test sends a `calls`-kind RawEdge with `confidence="ambiguous"`; expects `AcceptedEdge.confidence == Ambiguous`.
- Verify RED.
- Add `confidence` field + serde-default.
- Verify GREEN.
- Commit: `feat(host): thread confidence through RawEdge → AcceptedEdge → EdgeRecord (B.4*)`.

### Task 4 — CLI: thread `confidence` through `map_edge_to_record` (panel-revised — no `WriteOrigin`)

Files:
- Modify: `crates/clarion-cli/src/analyze.rs` (extend `map_edge_to_record` to set `confidence` on the `EdgeRecord` it produces). `WriteOrigin` plumbing is DROPPED per Q4 panel reversal.

Steps:
- RED: cli integration test asserts `runs.stats` reports `ambiguous_edges_total > 0` after a run that emits at least one ambiguous edge.
- Verify RED (the counter doesn't exist yet — leads into Task 5).
- Plumb `confidence` (mechanical).
- Verify GREEN once Task 5 lands.
- Commit: `feat(cli): plumb confidence into EdgeRecord (B.4*)`.

### Task 5 — Observability: `ambiguous_edges_total`, `unresolved_call_sites_total`, `pyright_query_latency_p95_ms`

Files:
- Modify: `crates/clarion-storage/src/writer.rs` (new `Arc<AtomicUsize>` counters on `Writer`; increments in the right places).
- Modify: `crates/clarion-cli/src/analyze.rs` (snapshot counters at CommitRun; fold into `runs.stats` JSON alongside `dropped_edges_total` from B.3).
- Modify: `crates/clarion-storage/tests/writer_actor.rs` (counter-assertion tests).
- Add: a small p95 accumulator module + deterministic unit test.

Steps:
- RED: test asserts `ambiguous_edges_total` increments on each ambiguous edge accepted.
- Implement counter + increment.
- Verify GREEN.
- Repeat for `unresolved_call_sites_total` (the plugin reports this in `AnalyzeFileResult.stats`; the host folds into the per-run total).
- For `pyright_query_latency_p95_ms` (panel-required two-test split — catches the "p95 math wrong" and "forgot to record samples" regression paths independently):
  - RED: deterministic unit test — feed synthetic sample list `[10, 20, 30, ..., 1000]`-ms to the p95 accumulator; assert exact expected output (the p95-math regression test).
  - RED: smoke test — after a real PyrightSession roundtrip, assert `pyright_query_latency_p95_ms > 0` on `runs.stats` (the sample-recording regression test).
- Commit: `feat(observability): ambiguous + unresolved + pyright-latency counters w/ deterministic p95 test (B.4*)`.

### Task 6 — Pyright integration: `pyright_session.py` module (panel-revised — substantially expanded)

Files:
- NEW: `plugins/python/src/clarion_plugin_python/pyright_session.py` — owns the pyright LSP subprocess lifecycle, the JSON-RPC framing, the per-function-entity `prepareCallHierarchy` + `outgoingCalls` flow, restart-on-crash logic with per-run cap, stderr drain via daemon thread, init-hang deadline, explicit `subprocess.Popen` `env` handling.
- NEW: `plugins/python/src/clarion_plugin_python/call_resolver.py` (or co-located in `pyright_session.py`) — defines the `CallResolver` Protocol and `NoOpCallResolver` default. `extract()` accepts a `CallResolver` (no Optional) — `NoOpCallResolver` for tests that don't need pyright; `PyrightSession` for the real path.
- Modify: `plugins/python/src/clarion_plugin_python/server.py` (`ServerState.pyright: PyrightSession | None`; lazy-init on first `analyze_file`; explicit close at `shutdown` that joins the stderr drain thread).
- NEW: `plugins/python/tests/test_pyright_session.py` — integration tests against a real pyright (pytest marker `@pytest.mark.pyright`; auto-skipped via a session-scoped fixture when `pyright-langserver` isn't on PATH).
- Modify: `plugins/python/pyproject.toml` (register the two new markers — `pyright` and `slow` — under `pytest.ini_options.markers`).

Steps:
- RED: `test_pyright_session_resolves_direct_call` — feed pyright a 3-line file with one direct call; assert one resolved-tier edge returned with non-zero byte offsets.
- Verify RED. Implement minimal subprocess wrapper + per-function-entity `prepareCallHierarchy` + `outgoingCalls` (NOT per-call-site — Q1 panel correction). Verify GREEN.
- RED: `test_pyright_session_ambiguous_dict_dispatch` — `handlers[k]()` over a dict-of-callables; assert ambiguous-tier edge with `properties.candidates` non-empty.
- Verify GREEN.
- RED: `test_pyright_session_ambiguous_determinism` — run analysis twice on the same input; assert byte-identical `to_id` and `candidates` ordering (proves `sorted(candidates)[0]` deterministic across invocations).
- RED: `test_pyright_session_restart_on_crash` — kill pyright mid-session, assert next query restarts it and emits `CLA-PY-PYRIGHT-RESTART`.
- RED: `test_pyright_session_restart_cap` — force `MAX_PYRIGHT_RESTARTS_PER_RUN + 1` restarts; assert the currently-analysing file is skipped with `CLA-PY-PYRIGHT-POISON-FRAME`; assert subsequent files in the run continue with `unresolved_call_sites_total` increment.
- RED: `test_pyright_session_init_timeout` — monkeypatch the pyright handshake to hang past `PYRIGHT_INIT_TIMEOUT_SECS`; assert `CLA-PY-PYRIGHT-INIT-TIMEOUT` emitted; assert run completes with zero `calls` edges.
- RED: `test_pyright_session_unavailable_binary_missing` — run with `pyright-langserver` not on PATH; assert `CLA-PY-PYRIGHT-UNAVAILABLE` emitted; run completes with zero `calls` edges; `unresolved_call_sites_total` reflects AST call-site count.
- RED: `test_pyright_session_install_failure` — simulate node-bootstrapper failure (mock the executability check); assert `CLA-PY-PYRIGHT-INSTALL-FAILURE` emitted; behavior matches `-UNAVAILABLE`.
- RED: `test_pyright_session_call_resolution_timeout` — mock per-call-site LSP query to exceed 5s; assert `CLA-PY-PYRIGHT-CALL-RESOLUTION-TIMEOUT` emitted; edge omitted (NOT silently dropped, NOT a crashed run).
- RED: `test_pyright_session_stderr_drain` — feed pyright a working input but ensure stderr fills past the OS pipe buffer (~64 KiB); assert the run does NOT deadlock and the stderr drain thread is joined cleanly in `close()`.
- Implement each, verify GREEN.
- Commit: `feat(wp3): PyrightSession — LSP subprocess + per-function callHierarchy + crash/timeout/unavailable handling (B.4* Q1)`.

### Task 7 — Extractor: emit `calls` edges via `CallResolver` (panel-revised — Protocol shape, not Optional)

Files:
- Modify: `plugins/python/src/clarion_plugin_python/extractor.py`:
  - `extract()` signature gains `call_resolver: CallResolver = NoOpCallResolver()` (panel correction — no `Optional[PyrightSession]`; the Protocol with a no-op default eliminates dual code paths and mypy-strict null-check noise).
  - `RawEdge` TypedDict gains `confidence: NotRequired[Literal["resolved", "ambiguous", "inferred"]]` (panel-flagged: not in the original draft's task ledger; load-bearing for the wire shape).
  - Extractor's `_walk` accumulates function entities into a side list; after the walk, `call_resolver.resolve_calls(file_path, function_ids)` returns the `calls` RawEdges en masse (matches the per-function LSP traffic shape, Q1).
- Modify: `plugins/python/src/clarion_plugin_python/server.py` (`handle_analyze_file` passes `state.pyright or NoOpCallResolver()` into `extract`).
- Modify: `plugins/python/tests/test_extractor.py` (calls-emission tests; some marked `@pytest.mark.pyright`, others use `NoOpCallResolver` and need no marker).

Steps:
- RED: `test_extractor_with_noop_resolver_emits_no_calls` — control: `NoOpCallResolver` produces zero `calls` edges; existing entity + contains-edge counts unchanged.
- RED: `test_extractor_emits_resolved_calls` (with pyright) — file with `def a(): b()` plus `def b(): pass` → assert one resolved calls edge `(a, b)` with source_byte_start/end set.
- Implement.
- RED: `test_extractor_emits_ambiguous_calls_with_candidates` (with pyright) — dict-of-callables; assert one ambiguous edge with `properties.candidates`.
- RED: `test_extractor_no_edge_for_unresolved_external_call` (with pyright) — file imports stdlib `os`, calls `os.getcwd()` → assert zero calls edges AND `unresolved_call_sites_total` increments.
- RED: `test_extractor_async_call_resolves` (with pyright) — `async def a(): await b()`; assert one resolved edge (pyright handles coroutine types correctly; this is a fixture not special-case logic).
- RED: `test_extractor_decorated_callable_resolves_when_possible` (with pyright) — `@functools.wraps`-decorated `def a(): pass; def b(): a()` → assert resolved or ambiguous edge (panel-flagged edge case; fixture present, no code special-case).
- RED: `test_extractor_dunder_call_dispatch` (with pyright) — class with `__call__` method, instance called as function → assert resolved or ambiguous edge.
- Commit: `feat(wp3): extractor emits calls edges via CallResolver protocol (B.4* Q1/Q3)`.

### Task 8 — Manifest lockstep bump + pyright pin + CI cache + dependabot (panel-revised)

Files:
- Modify: `plugins/python/plugin.toml`:
  - `[ontology].edge_kinds = ["contains", "calls"]`
  - `[ontology].ontology_version = "0.4.0"`
  - Add `[capabilities.runtime.pyright]` block (panel correction — the existing manifest uses `[capabilities.runtime]`, NOT a new top-level `[runtime]`):
    ```toml
    [capabilities.runtime.pyright]
    pin = "1.1.X"  # exact version at impl start
    ```
- Modify: `crates/clarion-core/src/plugin/manifest.rs` (extend manifest parser to recognise the new `pyright` sub-table under `[capabilities.runtime]`; surface `pin` for `CLA-PY-PYRIGHT-VERSION-DRIFT` detection).
- Modify: `plugins/python/src/clarion_plugin_python/server.py` (`ONTOLOGY_VERSION = "0.4.0"`).
- Modify: `plugins/python/src/clarion_plugin_python/__init__.py` (PATCH `__version__` bump).
- Modify: `plugins/python/pyproject.toml`:
  - Add `pyright == X.Y.Z` to `[project.dependencies]`.
  - Add `[tool.pytest.ini_options].markers`: register `pyright` and `slow`.
- Modify: `.github/workflows/ci.yml` (add cache key for `~/.cache/pyright-python/` to avoid re-downloading node on cold runs).
- Modify or create: `.github/dependabot.yml` — add `ignore:` entry for `pyright` (Python ecosystem). If dependabot is not yet configured, add a TODO comment in `pyproject.toml` beside the pin documenting the manual-bump policy.
- Modify: `plugins/python/tests/test_server.py`, `tests/test_package.py` (update version-string assertions).

Steps:
- Mechanical updates.
- Run pytest; literal-asserting tests need updating.
- Verify CI cache key is correctly scoped (key includes pyright pin so a bump invalidates).
- Commit: `feat(wp3): ontology v0.4.0 — edge_kinds += calls + pyright pin + CI cache + dependabot-ignore (B.4*)`.

### Task 9 — Cross-language fixture parity for calls + properties.candidates

Files:
- Modify: `fixtures/entity_id.json` (extend with `calls_edges` array containing both a resolved and an ambiguous-with-candidates example).
- Modify: Rust + Python fixture-parity tests to consume the new array.

Steps:
- Add 3–5 fixture rows: one resolved calls edge with byte offsets; one ambiguous calls edge with `properties.candidates: ["id_a", "id_b"]`; one entity with `parent_id` matching a contains edge (B.3 carryover); etc.
- Update Rust fixture-consumer + Python fixture-consumer.
- Verify byte-for-byte parity (edges have no plugin-side ID derivation; parity is structural).
- Commit: `test(wp3): cross-language fixture parity for calls edges (B.4*)`.

### Task 10 — Round-trip + walking-skeleton e2e extensions (panel-revised — must exercise ambiguous tier end-to-end)

Files:
- Modify: `plugins/python/tests/test_round_trip.py` (assert at least one resolved calls edge appears when extractor.py analyses itself; ambiguous-edge ratio is non-pathological).
- Modify: `tests/e2e/sprint_1_walking_skeleton.sh`:
  - Extend the demo file from `def hello(): return "world"` to:
    ```python
    def world():
        return 42

    def hello():
        return world()

    DISPATCH = {"k": world}

    def via_dispatch():
        return DISPATCH["k"]()
    ```
  - Demo intentionally forces: one resolved calls edge (`hello → world`), one ambiguous calls edge (`via_dispatch → world` via dict-of-callables with `properties.candidates`).
  - Assert one calls-edge row with `confidence='resolved'`.
  - Assert one calls-edge row with `confidence='ambiguous'` AND `properties` JSON contains `candidates` field.
  - Assert `ambiguous_edges_total >= 1` (not `== 0` — the draft's value would also pass if the counter were dead code; quality-reviewer correction).
  - Assert `dropped_edges_total == 0`.
  - Assert `unresolved_call_sites_total == 0` for this in-project-only corpus.

Steps:
- Update demo fixture in the e2e script per the new shape.
- Add the sqlite queries.
- Verify e2e PASSES end-to-end with both tiers exercised.
- Commit: `test(wp3): walking skeleton exercises resolved + ambiguous calls edges (B.4*)`.

### Task 11 — Week-2 go/no-go gate run (panel-revised — reproducibility hooks required)

Files:
- NEW: `tests/perf/elspeth_mini/` corpus (50–100 files, vendored or fetched per Q7a).
- NEW: `tests/perf/synthetic/` corpus (≥1 file mirroring elspeth's patterns; runs without elspeth access — panel addition).
- NEW: `scripts/b4-gate-run.sh` — declares calibration baseline machine in header; respects `OPERATOR_HARDWARE_RATIO` env var; EXITS NON-ZERO on Red (panel correction — the draft made go/no-go a human call).
- NEW: `docs/implementation/sprint-2/b4-gate-results.md` (per Q7c spec, including the elspeth-full + next-tier extrapolation).

Steps:
- Curate the elspeth-mini corpus per Q7a representativeness checklist.
- Curate the synthetic corpus (no elspeth dependency).
- Run the gate against both; record results in `b4-gate-results.md` with extrapolation to elspeth-full.
- Engineer-panel decides per Q7 outcome: green → continue; yellow → mitigation memo; red → scope-reduction or substantial redesign.
- Commit: `perf(wp3): B.4* week-2 gate results — <green/yellow/red> (extrapolated elspeth-full: <T>)`.

### Task 11b — CI staleness check (panel addition — quality reviewer required)

Files:
- Modify: `.github/workflows/ci.yml` — add a fast-running step (NOT the gate run itself) that asserts:
  1. The most recent entry in `b4-gate-results.md` is dated within `MAX_GATE_AGE_DAYS` (default 30).
  2. Its `pyright_pin` field matches `[capabilities.runtime.pyright].pin` in `plugins/python/plugin.toml`.
- The step FAILS the PR if either assertion fails, with a message naming the pin mismatch or the stale date. Operators bumping the pyright pin in a follow-up PR are forced to re-run the gate.

Steps:
- Implement the CI check as a short shell or Python script.
- Verify it passes on the just-committed Task 11 result.
- Verify it fails on a deliberately-stale `b4-gate-results.md` (test fixture).
- Commit: `ci(wp3): assert B.4* gate result fresh + pin-matched (panel-required, B.4* Q7)`.

### Task 12 — Documentation lock + close

Files:
- Modify: `docs/implementation/sprint-2/scope-amendment-2026-05.md` (status line: "B.4* complete; see exit criteria §9 of b4-calls-edges.md").
- Modify: `docs/clarion/adr/ADR-028-edge-confidence-tiers.md` (Open-Questions section: amend "Storage location for inferred edges" with "Resolved by B.4* design Q5 — same `edges` table; sketch of additive migration if reversed inline in b4-calls-edges.md").
- NEW: `docs/implementation/sprint-2/b8-scale-test.md` (or update an existing B.8 design stub) — pre-write the yellow/red rollback options for B.8 *before B.5* begins* (panel-required handoff from Q7/Systems).
- Verify all ADR-023 gates green on the closing commit.
- Commit: `docs(wp3): close B.4* design + ADR-028 amendment + B.8 rollback pre-write (Q5/Q7 panel-required)`.

## 8. Filigree umbrella + tracking

Single B.4* umbrella `clarion-2d2d1d27b5` (existing; currently `proposed`). Move proposed → approved on design landing; approved → in_progress at Task 1 start; in_progress → done at Task 12 close. Sub-tasks Tasks 1–12 as inline checklist items.

Sister-filigree work that can parallelise: B.7 (`clarion-73ab0da435`, entity_associations binding) is filigree-side work that doesn't touch B.4*'s code paths; the operator running B.7 can do so without coordination.

## 9. Exit criteria (panel-revised)

B.4* is done for Sprint 2 when ALL of:

- Plugin emits `calls` edges with `confidence` ∈ {`resolved`, `ambiguous`} for every static call site pyright resolves to ≥1 in-project candidate.
- Plugin emits zero `calls` edges for call sites pyright cannot resolve (unresolved → `unresolved_call_sites_total` increment, NOT a synthetic edge — inferred is lazy per ADR-028 §"Decision 3").
- Plugin gracefully handles pyright init-failure, crash, timeout, version-drift; all emit the named findings without crashing the run; restart cap (`MAX_PYRIGHT_RESTARTS_PER_RUN`) prevents infinite loops.
- Writer-actor extends `enforce_edge_contract` to enforce per-kind confidence contract; structural-with-non-resolved → rejected; anchored-with-inferred-at-scan-time → rejected; both cases increment `dropped_edges_total` AND emit `CLA-INFRA-EDGE-CONFIDENCE-CONTRACT` finding. ALL contract-test matrix permutations from Task 2 (RED-1 through RED-8) pass.
- `EXPLAIN QUERY PLAN` on `WHERE kind=? AND confidence=?` uses `ix_edges_kind_confidence`.
- `plugin.toml::edge_kinds == ["contains", "calls"]`; `ontology_version == "0.4.0"`; `[capabilities.runtime.pyright].pin == X.Y.Z`.
- Pyright is a pinned `[project.dependencies]` entry in `pyproject.toml`; dependabot ignores it (or TODO comment explains manual-bump policy); CI cache key includes the pin.
- Cross-language fixture parity passes on Rust + Python sides (includes both resolved + ambiguous + candidates).
- Walking-skeleton e2e PASSES with at least one resolved calls-edge row, at least one ambiguous calls-edge row with non-empty `properties.candidates`, `ambiguous_edges_total >= 1`, `dropped_edges_total == 0`, `unresolved_call_sites_total == 0` for the demo corpus.
- Week-2 gate run recorded in `b4-gate-results.md` with extrapolation to elspeth-full; outcome is GREEN (mini + extrapolation), or YELLOW with a written mitigation that the engineer panel signs off on; `scripts/b4-gate-run.sh` exits zero. CI staleness step (Task 11b) PASSES on the just-committed result.
- `ambiguous_edges_total`, `unresolved_call_sites_total`, `pyright_query_latency_p95_ms` counters surface on `runs.stats`. `pyright_query_latency_p95_ms` has both a deterministic-math unit test and a > 0 smoke test.
- Round-trip self-test asserts at least one resolved `calls` edge for a known direct call in the plugin's own source.
- ADR-028 Open Questions amended (Q5 resolved with explicit migration sketch).
- B.8 design memo has pre-written yellow/red rollback options (Task 12 handoff to systems-review concern).
- All ADR-023 gates green on the closing commit.

## 10. Open questions for the implementation phase (lower stakes — panel-expanded)

- **Pyright LSP subprocess re-init policy on `project_root` change**: the plugin currently captures `project_root` at `initialize` once. If a future host sends multiple `initialize` calls with different roots (currently not the case), pyright would need a re-init. **Recommendation**: defer — single-init is the contract, document as such, raise if changed.
- **Per-call-site timeout default**: 5s is a guess. Tunable via `clarion.yaml`? **Recommendation**: ship hard-coded 5s; add YAML config in a follow-up if observed.
- **Pyright init-hang deadline default**: 30s. Same recommendation — hard-coded for now, configurable when observed.
- **Pyright restart cap default**: `MAX_PYRIGHT_RESTARTS_PER_RUN = 3`. Recommendation: hard-coded; tune via YAML when observed.
- **Counter-vs-finding-per-site for unresolved call sites** (panel-flagged): B.4* ships only the run-level counter. B.6's `callers_of` design MUST pick between (a) per-site finding (`CLA-PY-CALL-SITE-UNRESOLVED`), (b) sibling `entity_unresolved_call_sites` table, or (c) synthesized "edge-with-zero-candidates" rows. Documented in §6 as a B.6 handoff. **Decision deferred to B.6, not B.4*.**
- **Multi-pin upgrade ordering** (panel-flagged, systems): the plugin now carries TWO independent version pins — `[capabilities.runtime.pyright]` and `[integrations.wardline]`. When both need bumping (e.g., pyright minor break coincides with Wardline release), the operator sees independent findings with no guidance on resolve order. B.5* may add a third pin (references resolution). **Decision needed**: do we author a "Loom suite pin discipline" memo before B.5* ships? **Recommendation**: defer to immediately-pre-B.5*; document the interaction in a one-line tracking note in the scope amendment.
- **Plugin coupling-accumulation policy** (panel-flagged, architecture): the Python plugin now has three optional heavy dependencies (Wardline probe at Sprint 1, pyright at B.4*, and references-resolution-engine at B.5* — TBD). "Plugin fails gracefully when dependency absent" is implicit but not stated as policy. **Recommendation**: write a brief plugin-resilience policy memo before B.5* begins, citing both `CLA-PY-PYRIGHT-UNAVAILABLE` and the Wardline-probe absence-handling as the templates.
- **B.4 → B.4*/B.5* rescoping pattern frequency** (panel-flagged, systems, LOW): governance is fine at n=2 sequential re-scopes. If B.5* or B.6 spawns a *-variant mid-implementation, the WP boundary assumptions in `v0.1-plan.md` warrant structural reassessment before Sprint 3. **Recommendation**: monitor; flag if a third re-scope appears.
- **Cache-miss storm on concurrent inferred queries** (panel-flagged, systems, for B.6 not B.4*): a single `callers_of(id, confidence>=inferred)` cold-cache call triggers an LLM dispatch. Multiple concurrent MCP queries on the same cold caller fire N parallel LLM calls without coalescing. **Recommendation**: B.6's design pass MUST decide on a per-caller in-flight coalescing guard.
- **Async call resolution**: `await foo()` — pyright resolves this through coroutine types correctly; no special-case logic needed for B.4*. Implementation should verify with a test case (added in Task 7).
- **Method call vs. function call distinction**: B.4* treats both as `calls` (no kind split — `method_calls` is YAGNI per ADR-027 / ADR-028 spirit). Detailed-design.md mentions "method calls" as approximate; this is what the ambiguous tier captures.
- **Pyright stderr handling**: drained in a daemon thread, joined in `PyrightSession.close()`. Fatal errors surfaced as `CLA-PY-PYRIGHT-DIAGNOSTIC` warnings (optional finding, not in the required set).
- **Decorated-function calls**: covered by fixture in Task 7 (`test_extractor_decorated_callable_resolves_when_possible`).
- **`__call__` dispatch**: covered by fixture in Task 7 (`test_extractor_dunder_call_dispatch`).

## 11. Panel-review record

Five reviewers ran in parallel on the DRAFT (`docs/implementation/sprint-2/b4-calls-edges.md` at first-write):

- **architecture** (`axiom-planning:plan-review-architecture`)
- **quality** (`axiom-planning:plan-review-quality`)
- **reality** (`axiom-planning:plan-review-reality`)
- **systems** (`axiom-planning:plan-review-systems`)
- **python** (`axiom-python-engineering:python-code-reviewer`)

Reviewer verdicts and the doc changes each forced:

| Q | Draft proposal | Panel verdict | Reconciliation in this doc |
|---|---|---|---|
| Q1 | Pyright-as-LSP; per-call-site `prepareCallHierarchy` + `outgoingCalls`; restart on crash | architecture ACCEPT; reality verified LSP method names; python CRITICAL — **traffic shape wrong**; systems BLOCKING — **poison-frame restart unbounded** | adopted python's per-function-entity traffic shape (Q1 process model rewritten); added `MAX_PYRIGHT_RESTARTS_PER_RUN` cap + `CLA-PY-PYRIGHT-POISON-FRAME`; added `PYRIGHT_INIT_TIMEOUT_SECS` deadline + `CLA-PY-PYRIGHT-INIT-TIMEOUT`; added `CLA-PY-PYRIGHT-UNAVAILABLE` for binary-missing; stderr drain promoted from open-question to Task 6 spec |
| Q2 | Pinned PyPI pyright; new `[runtime.pyright]` manifest block | reality BLOCKING — **manifest path wrong** (existing structure is `[capabilities.runtime]`); architecture ACCEPT-WITH-REVISION — first-run executability check; python WARN — CI cache + dependabot-ignore + explicit subprocess `env` | corrected manifest section to `[capabilities.runtime.pyright]`; added `CLA-PY-PYRIGHT-INSTALL-FAILURE` first-run check; added CI cache key + dependabot-ignore to Task 8; documented explicit `env` for the subprocess; added pyright-vs-Wardline multi-pin drift open-question |
| Q3 | `list[str]` candidates; `total=False` TypedDict | architecture ACCEPT (with B.6 consumer caveat); python WARN — **`total=False` diverges from project convention**; quality WARN — no determinism test | switched `CallsEdgeProperties` to `NotRequired` per project convention; added explicit B.6 consumer constraint (don't treat `to_id` as "best guess"); added determinism RED test to Task 6 |
| Q4 | `enforce_edge_confidence_contract` new function + `WriteOrigin::{Scan, Query}` enum | architecture BLOCKING — **drop `WriteOrigin` (single-implementer premature abstraction)**; systems WARN — under-specified; reality BLOCKING — **name collides with existing `enforce_edge_contract`**; quality BLOCKING — **VER-without-VAL** on counter+finding emission | dropped `WriteOrigin` entirely; extend the existing `enforce_edge_contract` in place; expanded Task 2 contract-test matrix to 8 cases (RED-1..8) with explicit counter + finding assertions; B.6 introduces scan-vs-query discriminator with the real query write path in hand |
| Q5 | Same `edges` table; "reversible" framing | architecture ACCEPT-WITH-REVISION — **"reversible" overstated**; quality ACCEPT — add `EXPLAIN QUERY PLAN` assertion; systems WARN — migration plan doesn't exist | replaced "reversible" with explicit two-step migration sketch inline; added `EXPLAIN QUERY PLAN` assertion to Task 1 and Task 2 verifying `ix_edges_kind_confidence` is used |
| Q6 | Mechanical bump per ADR-027 | 5×ACCEPT — unanimous | unchanged (Task 8 absorbs the manifest-path correction from Q2) |
| Q7 | Gate corpus + manual-trigger + markdown result | quality BLOCKING — **not reproducible; gate is a vibes check**; systems BLOCKING — **no extrapolation from mini to elspeth-full**; architecture WARN — Yellow mitigation menu too thin, Red understated | substantial Q7 rewrite: `b4-gate-run.sh` exits non-zero on Red; calibration baseline + `OPERATOR_HARDWARE_RATIO`; synthetic corpus that doesn't require elspeth; extrapolation to elspeth-full + next-tier required in result file; Yellow mitigation menu acknowledges parallelisation multiplies init cost; Red framed as scope reduction not in-sprint pivot; NEW Task 11b CI staleness check; NEW Task 12 requirement to pre-write B.8 yellow/red rollback options before B.5* begins |

**Cross-cutting concerns absorbed into the doc** (not bound to one Q):

- Plugin coupling-accumulation policy memo before B.5* (§10 open question).
- `unresolved_call_sites_total` per-entity queryability gap explicitly handed off to B.6 (§6 + §10).
- Multi-pin drift discipline (pyright + Wardline + future) flagged for pre-B.5* memo (§10).
- B.4 → B.4*/B.5* rescoping pattern monitored; structural WP reassessment triggered if a third re-scope appears (§10).
- Cache-miss storm on concurrent inferred MCP queries flagged for B.6 design (§10).
- `RawEdge` TypedDict `confidence` update explicit in Task 7 (was missing from draft ledger).
- `CallResolver` Protocol + `NoOpCallResolver` default replaces `Optional[PyrightSession]` (Task 7).
- Pytest marker split: `pyright` (auto-skip when binary absent) + `slow` (gate-corpus only). Task 8.
- `pyright_query_latency_p95_ms` gets both deterministic-math unit test AND `> 0` smoke test (Task 5).
- E2e walking-skeleton extended to force ambiguous-tier exercise; bare counter assertion `== 0` rejected as dead-code-safe (Task 10).

**Reviewer raw transcripts**: the per-reviewer reports are not vendored; they live in the conversation log on the `sprint-2/b4-design` branch. The reconciliations above are this doc's authoritative record.

**Status**: DRAFT → READY-FOR-IMPLEMENTATION at this commit. Filigree `clarion-2d2d1d27b5` moves `proposed` → `approved`.
