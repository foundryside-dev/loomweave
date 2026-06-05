# Loomweave Comprehensive Read-Only Audit

Date: 2026-06-04
Repository: `/home/john/loomweave`
Branch observed: `ws6-guidance-maturity` ahead of `origin/ws6-guidance-maturity` by 2
Dirty state observed: untracked `.loomweave/loomweave.lock`

## Method

This was a static, source-and-document audit. Session startup checks were run with
`filigree session-context` and `git status --short --branch`.

The requested subagent API did not expose literal `enable_write_tools=false` or
`enable_mcp_tools=false` fields. Each subagent prompt carried those constraints
explicitly, including strict no-edit/no-MCP instructions and the instruction not
to use escaped double quotes in tool arguments.

Subagent execution:

| Role | Status |
| --- | --- |
| Architecture Critic | Completed |
| Systems Thinker | Completed |
| Rust and Python Implementation Engineer | Completed |
| Quality Engineer | Completed |
| Static Tools Analyst | Completed |
| Security Architect | Blocked by external platform usage limit before report |
| MCP and CLI Specialist | Blocked by external platform usage limit before report |

Because two reports were externally blocked, the coordinator performed a local
read-only security and MCP/CLI synthesis from the same source evidence. No build,
test, formatter, analyzer, or server command was run, because the audit was
constrained to read-only investigation.

## Critical Findings

None found in the completed read-only audit.

## High Findings

### H1. Combined plugin item cap excludes edges and findings

Severity: High

Locations:
- [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:978), lines 978-1087
- [limits.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/limits.rs:109), lines 109-130
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:3478), lines 3478-3569

Evidence: `EntityCountCap` documents ADR-021 as a combined cap for entities,
edges, and findings. The host calls `try_admit(1)` only in the entity loop, then
`process_edges` explicitly says edges do not participate in the cap. The CLI
accumulates accepted edges before writing them.

Impact: A buggy or hostile plugin can emit very large valid edge/finding sets
bounded mainly by frame size and memory, violating the documented resource
contract and risking memory growth or storage spam.

Remediation: Rename the cap to an item cap or keep the current name with updated
semantics, then charge accepted entities, accepted edges, and retained findings
against the same run-level counter. Apply admission before accepting each batch.

Acceptance test: Configure a tiny cap, emit one entity and enough valid edges or
findings to exceed it, and assert the host emits the cap finding and stops before
persisting the excess items.

### H2. macOS plugin limit cfgs appear inconsistent

Severity: High

Locations:
- [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:48), lines 48-59
- [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:594), lines 594-610
- [limits.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/limits.rs:301), lines 301-326

Evidence: The `pre_exec` block is compiled for Linux or macOS, but `host.rs`
imports several limit symbols only on Linux and `effective_max_nproc` is compiled
only for Linux/tests. The macOS path therefore appears to reference symbols that
are not imported or defined.

Impact: Loomweave can fail to compile on macOS targets despite macOS being named
in the resource-limit path and release governance history.

Remediation: Align cfgs. Either make the `pre_exec` block Linux-only or import
and define macOS-safe helpers, splitting Linux-only `nproc` behavior from the
portable address-space/file-descriptor limits.

Acceptance test: Run `cargo check --workspace --all-targets --target x86_64-apple-darwin`
or restore an equivalent macOS CI leg and prove `loomweave-core` builds.

### H3. File entities are not yet the canonical graph/source anchor

Severity: High

Locations:
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:3514), lines 3514-3540
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:3853), lines 3853-3933
- [writer.rs](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:571), lines 571-573

Evidence: Analyze now creates `core:file:*` records, but plugin entities still
derive `source_file_id` from the module entity. Storage still permits both
`file` and `module` as source anchors.

Impact: Consumers see split semantics: file entities exist, while navigation,
source anchoring, and identity still attach through module entities.

Remediation: Make `core:file:*` the canonical `source_file_id`, parent module
entities under the file entity, emit file-to-module containment, populate file
metadata, and then tighten storage validation to file anchors only.

Acceptance test: Analyze a one-file fixture and assert the file entity exists,
the module parent is the file, the function parent chain resolves to the file,
`source_file_id` is the file id, and required file metadata is present.

### H4. Resume is a re-walk, not checkpoint recovery

Severity: High

Locations:
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:321), lines 321-328
- [commands.rs](/home/john/loomweave/crates/loomweave-storage/src/commands.rs:157), lines 157-164
- [writer.rs](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:439), lines 439-447

Evidence: Storage comments state `--resume` is a re-emit-without-flip path, not
incremental checkpoint recovery. No durable phase/file checkpoint path was found.

Impact: A killed or failed large run repeats completed work instead of resuming
from the last successful phase/file.

Remediation: Add durable phase/file checkpoints and make `--resume` consult them
to skip completed work. If this narrower behavior is intentional, update the
requirements/design through an ADR instead of leaving the contract ambiguous.

Acceptance test: Kill a run after early phases complete, resume it, and assert
completed phases/files and provider calls are not repeated.

### H5. Analyze buffers whole plugin output before writer backpressure applies

Severity: High

Locations:
- [requirements.md](/home/john/loomweave/docs/loomweave/1.0/requirements.md:548), lines 548-554
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:668), lines 668-700
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:785), lines 785-884
- [writer.rs](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:974), lines 974-989

Evidence: Requirements describe streaming so the core can commit entities
incrementally. Implementation runs blocking plugin work, collects all entities,
edges, unresolved sites, and stats, then sends records to the writer afterward.

Impact: Large repositories can consume excessive memory, and a late plugin
failure loses completed file output for that plugin despite the writer-actor
design.

Remediation: Stream per-file or per-batch plugin results through a bounded
channel to the writer. Preserve per-file progress and apply backpressure during
extraction instead of after full collection.

Acceptance test: Use a fake plugin that emits many files then fails; assert
earlier batches are durable, the run records failure, and memory stays bounded.

### H6. Python plugin advertises Wardline awareness while semantic extraction is absent

Severity: High

Locations:
- [plugin.toml](/home/john/loomweave/plugins/python/plugin.toml:22), lines 22-40
- [wardline_probe.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/wardline_probe.py:38), lines 38-84
- [server.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/server.py:144), lines 144-155
- [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:179), lines 179-194
- [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:903), lines 903-930

Evidence: The manifest sets `wardline_aware = true`, but the ontology exposes
only `function`, `class`, `module` and `contains`, `calls`, `references`,
`imports`. The probe reports package availability/version; decorator handling
extends entity spans but does not emit Wardline tags, groups, annotations, or
decorator edges.

Impact: Downstream guidance/federation consumers can infer Wardline enrichment is
enabled when no usable semantic signal is emitted.

Remediation: Implement Wardline/decorator extraction, or downgrade the manifest
capability claim until the signal is actually produced.

Acceptance test: Analyze fixtures with direct, factory, stacked, and aliased
Wardline decorators and assert emitted annotation metadata and ordered
decorator edges; also assert explicit degraded behavior when the Wardline
vocabulary is unavailable.

### H7. Stale analyze run rows can persist and leak into project status

Severity: High

Locations:
- [analyze_runs.rs](/home/john/loomweave/crates/loomweave-mcp/src/analyze_runs.rs:11), lines 11-16
- [analyze_runs.rs](/home/john/loomweave/crates/loomweave-mcp/src/analyze_runs.rs:166), lines 166-202
- [lib.rs](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2005), lines 2005-2028
- [writer.rs](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:344), lines 344-363

Evidence: MCP cancel explicitly defers supervising-process crash reconciliation
to future `owner_pid`/`heartbeat_at` work. `project_status` reports the latest
raw `runs.status`, and writer cleanup marks failure only on normal writer
shutdown paths.

Impact: Operators and agents can see `running` after the owner process is gone,
making index freshness and recovery decisions unreliable.

Remediation: Add durable `runs.owner_pid` and `heartbeat_at`, reconcile stale
running rows on analyze startup/status/project-status reads, and mark abandoned
rows terminal with reason and completion time.

Acceptance test: Seed a `running` row with a dead owner PID and stale heartbeat;
`project_status_get` and `analyze_status` should both report an abandoned/failed
terminal state and update the row.

### H8. Guidance invalidates summaries, but summary generation ignores guidance

Severity: High

Locations:
- [ADR-007-summary-cache-key.md](/home/john/loomweave/docs/loomweave/adr/ADR-007-summary-cache-key.md:28), lines 28-40
- [ADR-030-on-demand-summary-scope.md](/home/john/loomweave/docs/loomweave/adr/ADR-030-on-demand-summary-scope.md:58), lines 58-63
- [summary.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:425), lines 425-454
- [summary.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:510), lines 510-532
- [guidance.rs](/home/john/loomweave/crates/loomweave-storage/src/guidance.rs:443), lines 443-452
- [guidance.rs](/home/john/loomweave/crates/loomweave-cli/src/guidance.rs:272), lines 272-282

Evidence: ADRs require `guidance_fingerprint` because summaries are
guidance-conditioned. The summary read path hard-codes `guidance-empty`, and the
prompt builder receives only entity/source fields. Guidance writes eagerly
invalidate matching summaries.

Impact: Summaries can appear fresh under the cache contract while being
generated without the institutional guidance that supposedly affects them.

Remediation: Compose applicable guidance during summary input construction,
hash it into the cache key, include it in the prompt, and make guidance mutation
plus affected-summary invalidation atomic or persist pending invalidation.

Acceptance test: Cache a summary, create/edit a matching guidance sheet, then
request the summary with a recording LLM provider. The request should be a cache
miss with a changed fingerprint and guidance content in the prompt.

### H9. Runtime-scope-blind imports can fabricate circular-import and clustering facts

Severity: High

Locations:
- [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:454), lines 454-526
- [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:104), lines 104-132
- [requirements.md](/home/john/loomweave/docs/loomweave/1.0/requirements.md:564), lines 564-569

Evidence: `_ImportEdgeCollector` visits the whole AST and emits every import as
a module-level resolved `imports` edge. It does not track `if TYPE_CHECKING`,
function-local imports, or a type-only/scope property. Circular-import SCCs use
all import edges without filtering.

Impact: Type-only and function-local imports can be treated as runtime imports,
fabricating SCCs, inflating coupling, and misleading subsystem clustering.

Remediation: Track import context in AST traversal. Suppress type-only/function
local imports from runtime algorithms, or emit `type_only` and `scope`
properties and filter them in circular-import/coupling/clustering queries.

Acceptance test: A fixture where `b.py` imports `a.py` only under
`if TYPE_CHECKING:` must not produce a circular-import cycle.

### H10. Dead-code reachability ignores ambiguous call candidates beyond `to_id`

Severity: High

Locations:
- [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:432), lines 432-448
- [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:324), lines 324-331
- [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:738), lines 738-757

Evidence: Ambiguous calls store only `candidate_ids[0]` in `to_id`; the full set
is stored in `properties.candidates`. Dead-code reachability selects only
`from_id, to_id`, so candidates beyond the first are invisible.

Impact: A target that is known to be reachable as an ambiguous candidate can be
reported as dead, contradicting the conservative fail-toward-live policy.

Remediation: Reuse existing ambiguous-candidate expansion in storage helpers, or
parse `properties.candidates` in dead-code adjacency for `calls` edges.

Acceptance test: Seed an ambiguous call with candidates `maybe_a` and `maybe_b`;
both must be excluded from `entity_dead_list`.

### H11. Duplicate-definition dedup is not shared by call/reference resolution

Severity: High

Locations:
- [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:356), lines 356-365
- [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:861), lines 861-889
- [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:970), lines 970-982
- [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1008), lines 1008-1038

Evidence: Entity extraction first-wins duplicate definitions and suppresses
dropped duplicate bodies. Pyright indexing separately collects all definitions
and builds `by_id` with a dict comprehension, allowing later duplicates to
overwrite earlier ones.

Impact: Calls/references from a dropped duplicate body can be attributed to the
surviving entity id with source ranges outside the stored entity span.

Remediation: Centralize duplicate-disposition logic and share it across entity,
call-site, and reference-site collection, or switch to a consistently documented
last-wins model with explicit duplicate confidence.

Acceptance test: Two same-name functions where only the dropped duplicate calls
`callee()` must not produce a `calls` edge from the surviving entity to `callee`.

### H12. Non-authoritative unresolved-call results can leave stale inferred-edge anchors

Severity: High

Locations:
- [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:247), lines 247-276
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4141), lines 4141-4164
- [unresolved.rs](/home/john/loomweave/crates/loomweave-storage/src/unresolved.rs:20), lines 20-29
- [query.rs](/home/john/loomweave/crates/loomweave-storage/src/query.rs:961), lines 961-975
- [summary.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:188), lines 188-199

Evidence: On Pyright unavailable/timeout/crash, `resolve_calls` reports
unresolved totals but returns an empty unresolved-site list. The analyzer only
clears all callers when the listed sites are authoritative. Reads fetch
unresolved rows by caller id only, while inference keys cache entries by current
caller content hash.

Impact: Old unresolved-site rows can survive a changed caller body and feed a
new inferred-edge prompt/cache key.

Remediation: Filter unresolved-site reads by current `caller_content_hash`, and
clear or mark stale caller rows when call resolution is non-authoritative. MCP
inference should reject rows whose stored hash differs from the current entity
hash.

Acceptance test: Analyze with one unresolved site, change the caller body,
simulate Pyright unavailable, then request inferred dispatch. It must not prompt
on or materialize the stale site.

### H13. Release verify no longer mirrors CI static guards

Severity: High

Locations:
- [ci.yml](/home/john/loomweave/.github/workflows/ci.yml:48), lines 48-74
- [release.yml](/home/john/loomweave/.github/workflows/release.yml:26), lines 26-29
- [release.yml](/home/john/loomweave/.github/workflows/release.yml:62), lines 62-117

Evidence: `release.yml` says the verify job must mirror CI. CI includes release
governance static guard, pyright pin lockstep, Wardline version bounds, and
entity-cap lockstep checks that are absent from the release verify job.

Impact: A tag/manual release path can build artifacts from a commit that would
fail CI-only release-safety checks.

Remediation: Add the missing guard steps to `release.yml` or centralize verify
logic into a shared script/reusable workflow used by both CI and release.

Acceptance test: Intentionally break the pyright pin or entity-cap ADR/code
lockstep; both CI and release verify should fail before artifact build/publish.

## Medium Findings

### M1. Python call resolution mixes AST byte offsets with LSP UTF-16 positions

Severity: Medium

Locations:
- [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:370), lines 370-463
- [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1202), lines 1202-1218
- [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1384), lines 1384-1389

Evidence: AST `col_offset`/`end_col_offset` are byte offsets. LSP ranges are
UTF-16 character offsets. The code compares and converts these as if they were
the same coordinate system.

Impact: Non-ASCII text before a call on the same line can produce unresolved or
incorrectly anchored call edges.

Remediation: Normalize call-site matching to a single coordinate system,
preferably LSP UTF-16 positions for Pyright matching plus a UTF-16-aware
position-to-byte converter.

Acceptance test: Add a plugin test with non-ASCII text before a call; assert the
call resolves and the byte span slices exactly to the callee expression.

### M2. Closure-local references can become false module references

Severity: Medium

Locations:
- [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:632), lines 632-668
- [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:602), lines 602-624

Evidence: The reference collector suppresses only names bound in the current
scope. Names bound in an enclosing function can be collected in an inner
function; if Pyright resolves them to a local assignment instead of an indexed
entity, target fallback can become the module entity.

Impact: Common closure patterns can create misleading `references` edges to a
module, polluting neighborhoods and dependency interpretation.

Remediation: Track enclosing-scope bindings and suppress non-entity local
references, or map them to the containing function if that is the intended graph
model. Reserve module fallback for true module-level definitions/imports.

Acceptance test: For `outer -> inner -> return token`, where `token` is an outer
local, assert no `references` edge targets the module for `token`.

### M3. Finding-list cap is silent after 5,000 rows and applied before filters

Severity: Medium

Locations:
- [lib.rs](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:345), lines 345-347
- [inspection.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/inspection.rs:30), lines 30-31
- [inspection.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/inspection.rs:179), lines 179-235

Evidence: `entity_finding_list` fetches `LIMIT 5000`, then checks whether the
already-capped vector reached 5,000. The 5,001st row is never fetched, so
`scan_truncated` cannot become true. Filters run after this cap.

Impact: Older critical findings beyond the newest 5,000 can be missed while the
tool reports an apparently complete filtered result.

Remediation: Push filters and pagination into SQL with exact count/has-more, or
fetch `FINDINGS_SCAN_CAP + 1` before filtering and report truncation honestly.

Acceptance test: Seed 5,001 findings for an entity, including an older critical
finding beyond the cap; a severity filter should either return it or report
truncation.

### M4. Analyze heartbeat can falsely look wedged during one long file

Severity: Medium

Locations:
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:3484), lines 3484-3494
- [lib.rs](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2796), lines 2796-2798
- [lib.rs](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2846), lines 2846-2850
- [analyze.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/analyze.rs:136), lines 136-160

Evidence: Progress heartbeat is written at phase/file boundaries, but not while
`host.analyze_file(file)` is in flight. MCP treats heartbeat age over 30 seconds
as unobserved while file timeout can be longer.

Impact: A healthy slow file can look like a wedged plugin, prompting premature
operator cancellation.

Remediation: Add periodic heartbeat while a file is in flight, or make
staleness relative to configured file timeout and expose a clearer
`working_on_file` state.

Acceptance test: A fixture plugin sleeps longer than 30 seconds but less than
the file timeout; `analyze_status` must not report stale/wedged during valid
work.

### M5. Normative federation batch fixture is not exercised

Severity: Medium

Locations:
- [contracts.md](/home/john/loomweave/docs/federation/contracts.md:236), lines 236-310
- [serve.rs](/home/john/loomweave/crates/loomweave-cli/tests/serve.rs:195), lines 195-234

Evidence: The contract marks `fixtures/post-api-v1-files-batch.json` as
normative. The conformance test loads other fixtures but not that batch fixture.

Impact: Sibling tools can rely on a documented wire contract that drifts from
implementation without tests noticing.

Remediation: Add `post-api-v1-files-batch.json` to fixture-driven conformance.

Acceptance test: Change only the batch fixture’s expected response shape; the
fixture-conformance test should fail.

### M6. Wardline qualname fixture is copied into tests instead of executed

Severity: Medium

Locations:
- [contracts.md](/home/john/loomweave/docs/federation/contracts.md:791), lines 791-805
- [wardline_taint.rs](/home/john/loomweave/crates/loomweave-storage/src/wardline_taint.rs:346), lines 346-377

Evidence: The test states expected values were copied from
`wardline-qualname-normalization.json`, then hard-codes them. New fixture
vectors would not automatically run.

Impact: Normative fixture drift can be missed.

Remediation: Parse the fixture JSON directly in storage/MCP reconciliation
tests.

Acceptance test: Add a new trap vector to the fixture; tests should fail until
implementation supports it.

### M7. Entity git provenance columns are never populated

Severity: Medium

Locations:
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:3908), lines 3908-3933
- [writer.rs](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:487), lines 487-528

Evidence: Core file records set `first_seen_commit` and `last_seen_commit` to
`None`, and the writer preserves incoming values rather than repairing them.

Impact: Catalog history/churn questions cannot be answered even though columns
exist.

Remediation: Thread the analyzed commit into entity construction and writer
update semantics for first/last seen values.

Acceptance test: Run over two commits and assert new, unchanged, and changed
entities have correct first/last commit values.

### M8. Recoverable source-walk failures are log-only

Severity: Medium

Location:
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4270), lines 4270-4321

Evidence: `collect_source_files` warns and increments a local skipped counter
for walk errors, but the return value is only a file list. The skipped count and
paths do not become durable stats or findings.

Impact: Analysis can be incomplete while durable outputs look clean.

Remediation: Return source-walk error counts/details into run stats and persist
a finding anchored to the project or path.

Acceptance test: Analyze a fixture with an unreadable/skipped path and assert
durable stats/findings report the skipped source-walk failure.

### M9. Federation HMAC is hand-rolled and has no freshness component

Severity: Medium

Locations:
- [auth.rs](/home/john/loomweave/crates/loomweave-cli/src/http_read/auth.rs:71), lines 71-110
- [auth.rs](/home/john/loomweave/crates/loomweave-cli/src/http_read/auth.rs:122), lines 122-185

Evidence: HMAC-SHA256 and constant-time comparison are implemented locally. The
canonical message signs method, path/query, and body hash only; it has no
timestamp, nonce, replay cache, or expiry window.

Impact: Local crypto primitives increase review burden, and a captured signed
request remains valid for the lifetime of the shared secret.

Remediation: Replace local primitives with `hmac` and `subtle`, normalize
signature decoding before comparison, add timestamp/nonce fields, enforce a
bounded skew window, and store recent nonces per component identity.

Acceptance test: Valid signatures, same-length wrong signatures, wrong-length
signatures, malformed hex, and missing headers all return the same envelope
class. Replaying the same signed request/nonce should fail.

### M10. Decorator, inheritance, globals, protocol, and package ontology remains absent

Severity: Medium

Locations:
- [plugin.toml](/home/john/loomweave/plugins/python/plugin.toml:31), lines 31-47
- [requirements.md](/home/john/loomweave/docs/loomweave/1.0/requirements.md:556), lines 556-577

Evidence: Requirements name protocols, globals, modules, packages, and edges
such as `inherits_from`, `decorated_by`, `uses_type`, and `alias_of`. The live
plugin ontology declares only `function`, `class`, `module`, and
`contains`/`calls`/`references`/`imports`.

Impact: Python framework and Wardline semantics encoded in decorators, bases,
types, and package exports are absent or approximated.

Remediation: Implement the missing ontology or amend v1.0 requirements to make
the limitation explicit and honest to consumers.

Acceptance test: Fixtures for `@app.route`, stacked decorators, class
decorators, `class Child(Base)`, module globals, and package re-exports emit the
documented shapes or return explicit missing-signal notes.

## Low Findings

### L1. Plugin handshake-failure test does not prove zombie reaping

Severity: Low

Locations:
- [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:656), lines 656-666
- [host_subprocess.rs](/home/john/loomweave/crates/loomweave-core/tests/host_subprocess.rs:185), lines 185-233

Evidence: Production code kills/waits on handshake failure, but the test comment
states it verifies only prompt error return and non-hanging behavior, not zombie
reaping.

Remediation: Add a Unix-only test seam or controlled `/proc` assertion for the
reap behavior.

Acceptance test: Removing `child.wait()` from the handshake-failure path should
fail the new test.

### L2. Python protocol reader lets malformed non-ASCII headers escape `ProtocolError`

Severity: Low

Locations:
- [server.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/server.py:74), lines 74-117
- [server.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/server.py:294), lines 294-300

Evidence: `read_frame` decodes headers with `line.decode("ascii")`; a
non-ASCII byte raises `UnicodeDecodeError`, while `main` catches only
`ProtocolError`.

Remediation: Catch `UnicodeDecodeError` during header decoding and convert it to
`ProtocolError`.

Acceptance test: Feed a malformed non-ASCII header to the server entrypoint and
assert it returns the protocol-error exit path without corrupting stdout.

### L3. Guidance create can race and overwrite a concurrent sheet

Severity: Low

Locations:
- [guidance.rs](/home/john/loomweave/crates/loomweave-cli/src/guidance.rs:228), lines 228-234
- [guidance.rs](/home/john/loomweave/crates/loomweave-storage/src/guidance.rs:156), lines 156-204

Evidence: CLI create performs a non-atomic existence check before a low-level
upsert. The source comment acknowledges a concurrent create can overwrite the
earlier sheet.

Remediation: Add an insert-only create primitive and reserve upsert for edit or
import paths.

Acceptance test: Race two creates with the same computed id; one succeeds and
the other fails without modifying the first row.

### L4. Capped graph scans use unordered `LIMIT`

Severity: Low

Locations:
- [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:104), lines 104-119
- [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:741), lines 741-744

Evidence: Circular-import and dead-code adjacency scans use `LIMIT ?1` without
`ORDER BY`.

Remediation: Add deterministic ordering such as
`ORDER BY from_id, to_id, kind, source_byte_start, source_byte_end`. For
dead-code, consider returning unavailable/degraded when edge scan truncates.

Acceptance test: Seed more edges than the cap in randomized insertion order and
assert repeated runs return identical truncated output.

### L5. MCP crate owns non-MCP helper surfaces

Severity: Low

Locations:
- [lib.rs](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:149), lines 149-490
- [lib.rs](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:648), lines 648-970
- [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:38), lines 38-44

Evidence: `loomweave-mcp` carries tool registry, server state, dispatch, resources,
diagnostics, analyze registry, and utility clients; CLI analyze imports helpers
from `loomweave_mcp`.

Remediation: Move shared federation/config/scan-result helpers to a narrower
crate or CLI-owned module and split MCP registry/state/dispatch/resource code
into focused modules.

Acceptance test: CLI analyze no longer depends on `loomweave_mcp` for non-MCP
helpers, and MCP dispatch delegates to focused modules with behavior preserved.

## Recommended Remediation Order

1. Fix resource-limit correctness first: H1, H2, and H13. These are release and
   platform safety issues with clear acceptance tests.
2. Fix graph correctness issues that can actively mislead agents: H9, H10, H11,
   H12, M1, and M2.
3. Reconcile the design/implementation contracts: H3, H4, H5, H6, M10.
4. Repair stale-state and cache semantics: H7, H8, M3, M4, L3.
5. Harden security and conformance coverage: M5, M6, M8, M9, L1, L2, L4.
6. Treat L5 as a refactoring follow-up after the behavioral issues are tracked.

## Verification Not Run

No dynamic gates were run due to the strict read-only audit constraint:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo build --workspace --bins`
- `cargo nextest run --workspace --all-features`
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`
- `cargo deny check`
- Python ruff/mypy/pytest gates
- E2E scripts
- macOS target checks

## Residual Risk

This audit is broad but static. Some findings are contract-confirmed by source
and docs; others, especially macOS compile behavior and dynamic Pyright/LSP edge
cases, should be reproduced with targeted tests before implementation planning.
Tracker deduplication was not performed, so several findings may correspond to
already-open Filigree work.

## Remediation Addendum

Date: 2026-06-04

This addendum was added after the read-only audit moved into implementation.
The original findings above remain unchanged as the audit trail. The current
worktree contains remediations for the listed findings, with targeted
regressions and broad gates run afterward.

### Remediated Findings

| Finding | Status | Primary implementation points | Regression evidence |
| --- | --- | --- | --- |
| H1 | Resolved | Combined entity, edge, and finding admission is enforced in [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:982) and [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:1174); cap semantics remain documented in [limits.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/limits.rs:128). | Host cap tests in [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:2440) and [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:2524). |
| H2 | Resolved | macOS/Linux resource-limit cfgs now align in [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:52), [host.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:107), [limits.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/limits.rs:301), and [limits.rs](/home/john/loomweave/crates/loomweave-core/src/plugin/limits.rs:320). | Workspace check/build/clippy gates below compile the local targets; macOS target CI remains the stronger remote proof. |
| H3 | Resolved | Analyze maps plugin output to canonical `core:file:*` anchors in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4137) and [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4395); storage rejects module/function anchors in [writer.rs](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:590). | Anchor tests in [writer_actor.rs](/home/john/loomweave/crates/loomweave-storage/tests/writer_actor.rs:1324) and [writer_actor.rs](/home/john/loomweave/crates/loomweave-storage/tests/writer_actor.rs:1374). |
| H4 | Resolved by contract reconciliation | ADR-041 makes v1.x resume an idempotent re-emit rather than checkpoint recovery in [ADR-041-resume-is-idempotent-reemit.md](/home/john/loomweave/docs/loomweave/adr/ADR-041-resume-is-idempotent-reemit.md:12), and amends ADR-005/ADR-011 status lines. | Resume behavior test in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/tests/analyze.rs:2261). |
| H5 | Resolved | Plugin file output streams through a bounded channel in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:765), [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:776), and [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:3673); cross-file edges are queued until both endpoints are inserted in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:797) and [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:3593). | Failure-mode coverage in [analyze_failure_modes.rs](/home/john/loomweave/crates/loomweave-cli/tests/analyze_failure_modes.rs:439), plus full workspace and Phase 3 e2e tests. |
| H6 | Resolved by honest capability contract | Python Wardline capability claims and v1 ontology docs were reconciled in [plugin.toml](/home/john/loomweave/plugins/python/plugin.toml:1), [requirements.md](/home/john/loomweave/docs/loomweave/1.0/requirements.md:1), [system-design.md](/home/john/loomweave/docs/loomweave/1.0/system-design.md:1), and [detailed-design.md](/home/john/loomweave/docs/loomweave/1.0/detailed-design.md:1). | Ontology test in [test_package.py](/home/john/loomweave/plugins/python/tests/test_package.py:1). |
| H7 | Resolved | Runs now persist `owner_pid` and `heartbeat_at`; stale running rows are repaired by [runs.rs](/home/john/loomweave/crates/loomweave-storage/src/runs.rs:15), analyze startup in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:371), MCP status in [status.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/status.rs:184), and analyze status in [analyze.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/analyze.rs:261). | Tests in [storage_tools.rs](/home/john/loomweave/crates/loomweave-mcp/tests/storage_tools.rs:4292) and [analyze_lifecycle.rs](/home/john/loomweave/crates/loomweave-mcp/tests/analyze_lifecycle.rs:265). |
| H8 | Resolved | Summary inputs compose applicable guidance and hash it into the cache key in [summary.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:39), [summary.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:77), and [summary.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:500). | Prompt/cache regression in [storage_tools.rs](/home/john/loomweave/crates/loomweave-mcp/tests/storage_tools.rs:1421). |
| H9 | Resolved | Python import extraction now records `type_only` and `scope`, and MCP runtime import algorithms filter them in [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:456), [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:550), and [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:60). | Python package tests plus MCP shortcut tests. |
| H10 | Resolved | Dead-code reachability expands ambiguous call candidates from edge properties in [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:823) and [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:829). | Ambiguous candidate tests in [catalogue_tools.rs](/home/john/loomweave/crates/loomweave-mcp/tests/catalogue_tools.rs:1205). |
| H11 | Resolved | Duplicate-definition disposition now suppresses dropped duplicate bodies before call/reference collection in [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:896). | Regression in [test_extractor.py](/home/john/loomweave/plugins/python/tests/test_extractor.py:565). |
| H12 | Resolved | Unresolved-call reads require current caller content hashes in [query.rs](/home/john/loomweave/crates/loomweave-storage/src/query.rs:961) and [query.rs](/home/john/loomweave/crates/loomweave-storage/src/query.rs:984); analyze writes hash-scoped replacements in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4419). | Tests in [query_helpers.rs](/home/john/loomweave/crates/loomweave-storage/tests/query_helpers.rs:438) and [storage_tools.rs](/home/john/loomweave/crates/loomweave-mcp/tests/storage_tools.rs:3132). |
| H13 | Resolved | Release verify now mirrors CI static guards in [release.yml](/home/john/loomweave/.github/workflows/release.yml:70), [release.yml](/home/john/loomweave/.github/workflows/release.yml:78), [release.yml](/home/john/loomweave/.github/workflows/release.yml:85), and [release.yml](/home/john/loomweave/.github/workflows/release.yml:88). | Guard scripts are covered by self-tests in the release verify job and cargo deny/build gates below. |
| M1 | Resolved | Pyright/LSP matching now converts AST byte columns and LSP UTF-16 positions explicitly in [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1451), [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1461), and [pyright_session.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1471). | Regression in [test_pyright_session.py](/home/john/loomweave/plugins/python/tests/test_pyright_session.py:180). |
| M2 | Resolved | Reference collection tracks enclosing local bindings in [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:650) and [extractor.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:717). | Python extractor tests. |
| M3 | Resolved | Finding filters and pagination are pushed into SQL before the cap in [inspection.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/inspection.rs:200) and [inspection.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/inspection.rs:216). | Regression in [catalogue_tools.rs](/home/john/loomweave/crates/loomweave-mcp/tests/catalogue_tools.rs:315). |
| M4 | Resolved | Analyze progress refreshes `heartbeat_at` through live progress snapshots and writer heartbeats in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:126), [writer.rs](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:1013), and [analyze.rs](/home/john/loomweave/crates/loomweave-mcp/src/tools/analyze.rs:147). | Analyze status and stale-run tests listed under H7. |
| M5 | Resolved | The normative batch fixture is exercised by [serve.rs](/home/john/loomweave/crates/loomweave-cli/tests/serve.rs:1). | `cargo test -p loomweave-cli --test serve serve_http_responses_match_federation_fixture_contracts -- --nocapture`. |
| M6 | Resolved | Wardline qualname fixture vectors are loaded directly by storage/Python tests in [wardline_taint.rs](/home/john/loomweave/crates/loomweave-storage/src/wardline_taint.rs:1). | Full storage and Python gates. |
| M7 | Resolved | Analyze stamps entity git provenance in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:3540), with writer preserving first-seen and refreshing last-seen in [writer.rs](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:501). | Regression in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/tests/analyze.rs:3100). |
| M8 | Resolved | Source-walk failures now persist stats/findings in [analyze.rs](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4270). | `cargo test -p loomweave-cli analyze::tests::source_walk -- --nocapture`. |
| M9 | Resolved | Federation auth uses `hmac`/`subtle` plus timestamp and nonce replay checks in [auth.rs](/home/john/loomweave/crates/loomweave-cli/src/http_read/auth.rs:1), with ADR-042 documenting the contract. | `cargo test -p loomweave-cli http_read::auth::tests::hmac -- --nocapture` and `cargo test -p loomweave-cli --test serve hmac_identity -- --nocapture`. |
| M10 | Resolved by contract reconciliation | v1.0 Python ontology now explicitly limits emitted kinds/edges and defers the absent ontology in [requirements.md](/home/john/loomweave/docs/loomweave/1.0/requirements.md:1), [system-design.md](/home/john/loomweave/docs/loomweave/1.0/system-design.md:1), and [detailed-design.md](/home/john/loomweave/docs/loomweave/1.0/detailed-design.md:1). | [test_package.py](/home/john/loomweave/plugins/python/tests/test_package.py:1). |
| L1 | Resolved | Handshake-failure zombie reaping is asserted on Linux in [host_subprocess.rs](/home/john/loomweave/crates/loomweave-core/tests/host_subprocess.rs:1). | `cargo test -p loomweave-core --test host_subprocess t9 -- --nocapture`. |
| L2 | Resolved | Python protocol header decoding converts non-ASCII malformed headers to `ProtocolError` in [server.py](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/server.py:1). | [test_server.py](/home/john/loomweave/plugins/python/tests/test_server.py:1). |
| L3 | Resolved | Guidance create is now insert-only and atomic in [guidance.rs](/home/john/loomweave/crates/loomweave-storage/src/guidance.rs:1) and [guidance.rs](/home/john/loomweave/crates/loomweave-cli/src/guidance.rs:1). | [guidance_write.rs](/home/john/loomweave/crates/loomweave-storage/tests/guidance_write.rs:1). |
| L4 | Resolved | Capped graph scans use deterministic ordering before `LIMIT` in [shortcuts.rs](/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:1). | `cargo test -p loomweave-mcp scan_truncates -- --nocapture`. |
| L5 | Resolved | Shared federation/config/scan-result helpers moved to [crates/loomweave-federation](/home/john/loomweave/crates/loomweave-federation/src/lib.rs:1), with MCP retaining re-export shims and CLI importing the narrower crate. | `cargo check -p loomweave-federation --all-targets`, `cargo check -p loomweave-cli --all-targets`, and no remaining CLI references to `loomweave_mcp::config`, `loomweave_mcp::filigree`, `loomweave_mcp::filigree_url`, or `loomweave_mcp::scan_results`. |

### Verification Run

Focused regression checks:

- `cargo test -p loomweave-core --test host_subprocess t9 -- --nocapture`
- `cargo test -p loomweave-storage --test guidance_write insert_guidance_sheet_rejects_existing_id_without_overwrite -- --nocapture`
- `cargo test -p loomweave-mcp scan_truncates -- --nocapture`
- `cargo test -p loomweave-federation -- --nocapture`
- `cargo test -p loomweave-cli http_read::auth::tests::hmac -- --nocapture`
- `cargo test -p loomweave-cli http_read::tests -- --nocapture`
- `cargo test -p loomweave-cli http_read::wardline::tests -- --nocapture`
- `cargo test -p loomweave-cli http_read::linkages::tests -- --nocapture`
- `cargo test -p loomweave-cli --test serve hmac_identity -- --nocapture`
- `cargo test -p loomweave-cli --test serve serve_http_responses_match_federation_fixture_contracts -- --nocapture`
- `cargo test -p loomweave-cli --test analyze analyze_stamps_entities_with_git_head_commit -- --nocapture`
- `cargo test -p loomweave-cli analyze::tests::source_walk -- --nocapture`
- `cargo test -p loomweave-cli --test analyze analyze_migrates_a_stale_db_instead_of_failing -- --nocapture`
- `cargo test -p loomweave-cli --test install install_applies_each_migration_exactly_once -- --nocapture`
- `cargo test -p loomweave-cli --test wp1_e2e wp1_walking_skeleton_end_to_end -- --nocapture`
- `cargo test -p loomweave-cli --test analyze_failure_modes analyze_defers_cross_file_edges_until_target_entity_batch_arrives -- --nocapture`
- `plugins/python/.venv/bin/pytest plugins/python/tests/test_package.py -q`
- `plugins/python/.venv/bin/pytest plugins/python/tests/test_server.py::test_malformed_non_ascii_header_uses_protocol_error_exit_path -q`

Broad gates:

- `plugins/python/.venv/bin/ruff check plugins/python`
- `plugins/python/.venv/bin/ruff format --check plugins/python`
- `plugins/python/.venv/bin/mypy --strict plugins/python`
- `plugins/python/.venv/bin/pytest plugins/python` (160 passed, 85% coverage)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --workspace --all-features`
- `cargo nextest run --workspace --all-features` (1073 passed, 2 skipped)
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo build --workspace --bins`
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`
- `cargo deny check` (passed with duplicate-crate/license-allowance warnings)
- `bash tests/e2e/sprint_1_walking_skeleton.sh`
- `bash tests/e2e/sprint_2_mcp_surface.sh`
- `bash tests/e2e/phase3_subsystems.sh`
