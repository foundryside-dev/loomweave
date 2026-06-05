# RC1 Quality Assessment

## Overall Quality Posture

Loomweave's quality posture is strong for a release candidate: important
boundaries are tested, the source-of-truth ladder is explicit, and most
high-risk runtime behavior has focused coverage. Remaining concerns are
concentrated in drift-prone duplicated facts, large files, release-gate
mismatches, and a few intentionally permissive or platform-shaped behaviors.

## Maintainability

### Strengths

- Workspace boundaries are clear and map to product responsibilities.
- Storage writes are centralized in one actor.
- Protocol and API shapes are typed and test-pinned.
- Documentation names authoritative sources and demotes implementation history
  to supporting context.

### Concerns

- `crates/loomweave-mcp/src/lib.rs`, `crates/loomweave-core/src/plugin/host.rs`,
  `crates/loomweave-cli/src/analyze.rs`, `crates/loomweave-core/src/llm_provider.rs`,
  and `crates/loomweave-cli/src/http_read.rs` are large enough that changes need
  local regression tests and careful review.
- Release/publish history still contains old arch-analysis concepts. Current
  work should use this RC1 report and live source, not old H/L labels.
- Some constants are duplicated instead of derived or drift-tested.

## Correctness

### Strengths

- SQLite migrations are exercised through real temp databases.
- Writer actor tests cover run lifecycle and graph write behavior.
- HTTP read API tests pin path traversal rejection, briefing-block
  non-disclosure, storage error envelopes, batching, resolve results, auth, and
  limits.
- MCP tests cover metadata, graph queries, LLM cache/accounting, coalescing,
  hallucinated targets, Filigree drift/caps.
- Python plugin tests cover server protocol, AST extraction, Pyright
  timeouts/restarts/caps, and Wardline import/version states.

### Concerns

- `EntityCountCap` semantics are ambiguous for edges/findings.
- `summary_cache.entity_id` lacks a foreign key while
  `inferred_edge_cache.caller_entity_id` has one.
- Installed-entrypoint Python smoke tests can skip when the plugin is not
  installed editable.
- Pyright-marked tests skip if `pyright-langserver` is unavailable; CI must
  install the plugin venv before relying on them.

## Operability

### Strengths

- `install` creates and repairs local project state predictably.
- `analyze` records explicit terminal run states.
- `serve` supervises MCP and optional HTTP together.
- Release scripts and operator docs distinguish dry runs, artifacts, signing,
  smoke tests, and governance checks.

### Concerns

- `ReaderPool::open` defers SQLite file validation until first read.
- External operator smoke is checklist-based with no dated result artifact
  found in the tree.
- Phase 3 perf evidence is useful but directional because the temporary corpus
  is not committed.

## Documentation Quality

### Strengths

- ADR precedence and requirement ID stability are explicit.
- Federation contract docs have normative fixtures and security notes.
- Operator docs exist for OpenRouter, coding-agent providers, secret scanning,
  release governance, and HTTP read API.

### Concerns

- `CHANGELOG.md` contains the `UNAUTHORIZED`/`UNAUTHENTICATED` mismatch.
- Historical implementation documents can still mention old arch-analysis
  finding labels. Treat them as historical notes, not current evidence.

## Quality Recommendations

1. Treat the large files as high-risk change zones; require narrow tests for
   every behavior change.
2. Add drift tests for duplicate plugin metadata and Wardline bounds.
3. Resolve the core cap semantics mismatch with one explicit test for
   edge-heavy plugin output.
4. Either require all named E2E scripts in CI/release or record dated manual
   evidence for those kept outside CI.
5. Fix the changelog error-code mismatch before release.
