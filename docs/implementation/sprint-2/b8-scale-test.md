# B.8 - Elspeth scale-test rollback playbook

**Status**: PRE-WRITTEN ROLLBACK PLAYBOOK - implementation begins after B.6 and B.7
**Predecessors**: B.4* calls edges, B.5* references edges, B.6 MCP surface, B.7 entity associations binding
**Filigree umbrella**: `clarion-6222134e0d`
**Reason this exists now**: B.4* Task 12 / Q7 requires B.8 yellow/red options to be written before B.5* begins, so the full-scale test has a scope playbook instead of a late-sprint improvisation.

## 1. B.8 purpose

B.8 is the Sprint 2 full-system validation pass:

- Run `clarion analyze` against the elspeth validation corpus.
- Start the MCP surface from B.6.
- Verify an agent can navigate real code with `entity_at`, `find_entity`, `callers_of`, `execution_paths_from`, `summary`, `issues_for`, and `neighborhood`.
- Record scale evidence for wall-clock time, memory, store size, edge counts, confidence distribution, and MCP query latency.

B.8 does not reopen B.4* by default. It only reopens B.4* if full-scale evidence contradicts the B.4* week-2 gate result in a way that makes calls edges unusable or unsafe.

## 2. Outcome bands

**Green**: the elspeth run completes inside the v0.1 scale envelope, MCP smoke checks return useful bounded responses, and calls edges include resolved rows plus bounded ambiguous rows without pathological `ambiguous_edges_total` or `unresolved_call_sites_total` growth.

**Yellow**: the run completes, but one or more signals are outside the comfortable envelope: wall-clock is materially above projection, memory pressure is high but not fatal, MCP traversal latency is high on call-heavy entities, ambiguous-edge volume is too noisy for default workflows, or summary cost exceeds the current cost hypothesis while still being containable.

**Red**: the run does not complete, the store is unusable, MCP smoke checks cannot answer basic navigation questions, calls-edge extraction dominates the run beyond sprint repair, or confidence semantics are violated in persisted data.

## 3. Yellow options

Choose the smallest mitigation that makes a re-run informative, then re-run the affected slice and record the evidence.

- Add per-file or per-function call-resolution caching keyed by file content hash and pyright pin. This preserves B.4* semantics and mainly helps repeat analyze paths.
- Add measured parallelism with multiple pyright sessions only after recording RSS and init overhead. Parallelism must be re-gated because it trades wall-clock for memory and process pressure.
- Narrow B.8 acceptance to the representative elspeth slice for Sprint 2 close, while opening a follow-up optimization issue for full elspeth before v0.1 GA. This is allowed only if MCP navigation quality is demonstrated on the slice.
- Add query-side caps for `confidence >= ambiguous` traversals in MCP responses. This keeps resolved-default behavior intact and makes noisy ambiguous expansions explicit to callers.
- Defer summary prewarming or broad summary smoke tests if calls/references navigation is healthy but LLM cost is the yellow signal. B.8 can close on navigation evidence while summary-cost calibration moves to a follow-up.

## 4. Red options

Red is a scope decision, not an in-sprint tuning chore. Pick one path explicitly and update the sprint tracker before implementation resumes.

- Ship v0.1 MCP navigation without scan-time `calls` edges: keep `edges.confidence` and ADR-028 semantics, but remove `calls` from the Python plugin manifest and mark `callers_of` / `execution_paths_from` as unavailable or degraded until the redesign lands.
- Ship a narrowed calls mode: resolved-only direct calls, no ambiguous emission, with the limitation documented in the MCP tool descriptions and B.8 closeout. This preserves a useful graph but gives up the B.4* uncertainty signal for v0.1.
- Re-design the resolver as AST-first with pyright only for selected ambiguous sites. This is a new design pass, not a B.8 fix; expect a new B.4*-narrowed issue and an implementation schedule extension.
- Defer the full elspeth proof and close only a slice demo if Sprint 2 must preserve a partial milestone. This requires an explicit scope amendment because the current Sprint 2 close condition names B.8.

## 5. Evidence B.8 must preserve

Every B.8 closeout, including yellow or red, must record:

- Corpus file count, function count, and total LOC basis.
- Analyze wall-clock, peak RSS if available, DB size, entity count, edge count, calls-edge count by confidence, and unresolved-call-site count.
- MCP smoke-check transcript or fixture output for each of the seven tools.
- Whether the chosen outcome changed the B.4* assumptions, and if so which rollback option was selected.
