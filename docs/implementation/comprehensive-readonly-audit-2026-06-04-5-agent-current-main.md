# Clarion Comprehensive Read-Only Audit - 2026-06-04 - 5 Agent Current Main

Repository: `/home/john/clarion`

Branch observed at session start: `main...origin/main`

Dirty state observed at session start: clean

Mode: strictly read-only source audit plus this requested markdown artifact. Five specialized subagents were dispatched as read-only explorer reviewers. Each prompt included `enable_write_tools=false`, `enable_mcp_tools=false`, no file edits, no generated files, no mutating commands, no MCP tools, and no escaped double quotes in tool arguments. The subagent API schema did not expose literal `enable_write_tools` or `enable_mcp_tools` fields, so those constraints were enforced in the task prompts.

Verification: source inspection only. No build, test, formatter, scanner, server, or CI commands were run because those can write caches, build output, or runtime artifacts.

## Reviewer Coverage

- Architecture Critic: crate boundaries, Loom doctrine, federation coupling, SARIF and Filigree integration paths.
- Systems Thinker: analyze lifecycle, failure propagation, stale graph risks, MCP analyze supervision.
- Rust and Python Engineer: plugin host, Python extractor/Pyright integration, JSON-RPC/LSP framing, implementation idioms.
- Quality Engineer: release gates, CI coverage, migration guard, perf harness coverage.
- Security Architect: HTTP auth defaults, SARIF import, MCP framing, Filigree endpoint resolution, plugin trust boundaries.

## Critical

### C-01: Release gate passes without the published migration immutability marker

Locations:

- [/home/john/clarion/scripts/check-migration-retirement.py:70](/home/john/clarion/scripts/check-migration-retirement.py:70), lines 70-74
- [/home/john/clarion/.github/workflows/release.yml:65](/home/john/clarion/.github/workflows/release.yml:65), lines 65-68
- `/home/john/clarion/crates/clarion-storage/migrations/published_build.txt`, absent on the audited checkout

Evidence: the release workflow invokes `scripts/check-migration-retirement.py`, but the guard returns success when `published_build.txt` is absent, printing that the policy is still pre-trigger. The marker file is absent in the current checkout.

Impact: a release can pass without pinning the published migration baseline. After a tag cut, future in-place edits to `0001_initial_schema.sql` can remain in the guard's pre-trigger path, defeating the immutability rule the release marker is supposed to activate.

Remediation:

- Add a strict mode, for example `--require-marker`, that fails when `published_build.txt` is absent.
- Invoke strict mode from the release workflow on tag pushes.
- Keep the current permissive behavior only for pre-release/local development if needed.
- Add self-tests for absent marker, empty marker, missing ref, matching migration, and changed migration.
- Commit `crates/clarion-storage/migrations/published_build.txt` at the intended published tag-cut baseline.

Acceptance test: run the release verify guard in tag/release mode without `published_build.txt`; it must fail before build or publish jobs can run.

## High

### H-01: Failed or partial analyzes can publish mixed-generation graph state

Locations:

- [/home/john/clarion/crates/clarion-storage/src/writer.rs:1](/home/john/clarion/crates/clarion-storage/src/writer.rs:1), lines 1-8
- [/home/john/clarion/crates/clarion-storage/migrations/0001_initial_schema.sql:31](/home/john/clarion/crates/clarion-storage/migrations/0001_initial_schema.sql:31), lines 31-55
- [/home/john/clarion/crates/clarion-storage/migrations/0001_initial_schema.sql:81](/home/john/clarion/crates/clarion-storage/migrations/0001_initial_schema.sql:81), lines 81-97
- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:1440](/home/john/clarion/crates/clarion-cli/src/analyze.rs:1440), lines 1440-1486
- [/home/john/clarion/crates/clarion-storage/src/writer.rs:1276](/home/john/clarion/crates/clarion-storage/src/writer.rs:1276), lines 1276-1303

Evidence: the writer commits batches during a run, and `entities`/`edges` are cumulative tables without a visible generation scope. Soft-failed runs intentionally commit healthy-plugin entities and edges while marking the run failed. Hard-fail rollback only covers the currently open transaction, not previously committed batches.

Impact: MCP and HTTP readers query cumulative graph tables, so a failed or partial analyze can leave readers with a hybrid graph: some current-run data, some last-good data, and a failed latest run. Agents can trust a graph that has no coherent successful generation boundary.

Remediation:

- Introduce an index generation boundary. Preferred: write each analyze into run-scoped or shadow tables and atomically publish a `current_index_generation` only on completed runs.
- Alternatively, add generation columns and make default reader views filter to the latest completed generation.
- Keep failed-run artifacts queryable through an explicit diagnostic surface rather than the default consult graph.
- Add tests for plugin crash, soft failure, and hard failure: default MCP/HTTP reads must remain pinned to the previous completed generation.

Acceptance test: analyze a fixture, run a second analyze that commits one batch and then soft-fails, then assert `entity_find`, graph traversal, and HTTP linkages use the previous completed generation unless an explicit failed-run diagnostic mode is requested.

### H-02: Unauthenticated loopback HTTP API exposes protected routes by default

Locations:

- [/home/john/clarion/crates/clarion-federation/src/config.rs:346](/home/john/clarion/crates/clarion-federation/src/config.rs:346), lines 346-374
- [/home/john/clarion/crates/clarion-cli/src/http_read/auth.rs:97](/home/john/clarion/crates/clarion-cli/src/http_read/auth.rs:97), lines 97-103
- [/home/john/clarion/crates/clarion-cli/src/http_read.rs:141](/home/john/clarion/crates/clarion-cli/src/http_read.rs:141), lines 141-153
- [/home/john/clarion/crates/clarion-cli/src/http_read.rs:302](/home/john/clarion/crates/clarion-cli/src/http_read.rs:302), lines 302-309

Evidence: `validate_auth_trust` explicitly returns success for loopback binds even when no bearer token or HMAC secret is configured. The auth middleware falls through to the route when both secrets are absent. Startup warns that any local process can read the catalogue. The route group can also include Wardline taint writes when `wardline_taint_write` is enabled.

Impact: loopback is not a user boundary on shared developer machines, CI runners, containers with shared networking, or compromised local processes. This can expose the structural graph and, when Wardline writes are enabled, allow local data poisoning.

Remediation:

- Require bearer or HMAC auth for all protected HTTP routes, including loopback.
- If compatibility is necessary, add an explicit break-glass flag such as `allow_unauthenticated_loopback: true`, default it to false, and make startup output unmistakable.
- Keep `/api/v1/_capabilities` unauthenticated if pre-auth probing is required, but do not expose file, entity, linkage, guidance, finding, or Wardline write routes without auth.
- Add tests that loopback without credentials refuses protected routes unless the break-glass flag is set.

Acceptance test: start the HTTP read API on `127.0.0.1` with no token or identity secret and request a protected route; the response must be `401` unless the explicit unauthenticated-loopback override is present.

### H-03: SARIF import can poison Filigree with out-of-project paths

Locations:

- [/home/john/clarion/crates/clarion-cli/src/sarif.rs:91](/home/john/clarion/crates/clarion-cli/src/sarif.rs:91), lines 91-93
- [/home/john/clarion/crates/clarion-cli/src/sarif.rs:124](/home/john/clarion/crates/clarion-cli/src/sarif.rs:124), lines 124-137
- [/home/john/clarion/crates/clarion-cli/src/sarif.rs:188](/home/john/clarion/crates/clarion-cli/src/sarif.rs:188), lines 188-205
- [/home/john/clarion/crates/clarion-cli/src/sarif.rs:221](/home/john/clarion/crates/clarion-cli/src/sarif.rs:221), lines 221-226

Evidence: `normalize_sarif_uri` relativizes absolute `file://` URIs under `project_root`, but preserves absolute paths outside `project_root`. The test suite currently asserts preservation of `/tmp/other/src/a.py`. The preserved path is sent as the finding `path` in the Filigree scan-results request.

Impact: untrusted SARIF can register findings against arbitrary host absolute paths or files outside the target project, polluting Filigree's file registry and misleading triage.

Remediation:

- Parse SARIF artifact URIs with a URL/path API.
- Reject absolute paths that cannot be canonicalized under `project_root`.
- Preserve the original external URI only in metadata, not as the finding path.
- Emit a skipped-finding count or warning for out-of-project SARIF locations.
- Add tests for `file:///tmp/outside.py`, `file://localhost/...`, relative paths, URL-encoded paths, and Windows-style paths.

Acceptance test: importing SARIF with `file:///tmp/other/src/a.py` against `/home/john/project` must not POST a finding whose `path` is `/tmp/other/src/a.py`.

### H-04: MCP-launched analyzes are not recoverable after `serve` crashes

Locations:

- [/home/john/clarion/crates/clarion-mcp/src/analyze_runs.rs:1](/home/john/clarion/crates/clarion-mcp/src/analyze_runs.rs:1), lines 1-16
- [/home/john/clarion/crates/clarion-mcp/src/tools/analyze.rs:169](/home/john/clarion/crates/clarion-mcp/src/tools/analyze.rs:169), lines 169-187
- [/home/john/clarion/crates/clarion-mcp/src/tools/analyze.rs:232](/home/john/clarion/crates/clarion-mcp/src/tools/analyze.rs:232), lines 232-248
- [/home/john/clarion/crates/clarion-mcp/src/lib.rs:3541](/home/john/clarion/crates/clarion-mcp/src/lib.rs:3541), lines 3541-3550
- [/home/john/clarion/crates/clarion-storage/src/runs.rs:7](/home/john/clarion/crates/clarion-storage/src/runs.rs:7), lines 7-19

Evidence: MCP supervision lives in an in-memory registry. The module explicitly says supervising-process crash reconciliation is out of scope. If a run is absent from the registry, status/cancel fall back to DB terminal status handling; `map_run_status` maps any DB status other than completed/skipped/cancelled to failed. Storage can mark stale rows abandoned only after a 24-hour heartbeat window.

Impact: if `clarion serve` dies while a child `clarion analyze` continues running, a restarted MCP server cannot adopt, cancel, or accurately report the live child. Operators lose observability and control even though the analyze process may still own the lock and continue writing.

Remediation:

- Persist supervision metadata: process group id, owner pid, progress path, command identity, project root, and heartbeat.
- On `serve` startup and `analyze_status`, distinguish `running_owned`, `running_orphaned`, `stale_running`, and terminal failed states.
- Allow safe cancellation of orphaned runs only after validating the persisted process group still belongs to the recorded Clarion analyze command/project.
- Lower or make configurable the stale heartbeat threshold for interactive MCP status, while preserving conservative repair for portability.

Acceptance test: start an MCP analyze, terminate `serve` while the child continues, restart `serve`, and assert `analyze_status` reports an orphaned live run and `analyze_cancel` can terminate it safely.

### H-05: Release dry run does not exercise release-only publish steps

Locations:

- [/home/john/clarion/.github/workflows/release.yml:11](/home/john/clarion/.github/workflows/release.yml:11), lines 11-15
- [/home/john/clarion/.github/workflows/release.yml:309](/home/john/clarion/.github/workflows/release.yml:309), lines 309-315
- [/home/john/clarion/.github/workflows/release.yml:329](/home/john/clarion/.github/workflows/release.yml:329), lines 329-361
- [/home/john/clarion/.github/workflows/release.yml:376](/home/john/clarion/.github/workflows/release.yml:376), lines 376-423
- [/home/john/clarion/docs/operator/v1.0-release-governance.md:186](/home/john/clarion/docs/operator/v1.0-release-governance.md:186), lines 186-196

Evidence: the operator governance doc requires a manual `release.yml` run before tag creation. The workflow's manual dispatch path validates verify/build/artifact plumbing, but the actual `release` job is gated to tag pushes. Signing, signature verification, release notes generation, and GitHub Release creation therefore remain unexercised by the documented dry run.

Impact: release-only failures can surface only after the tag path starts, including signing identity assumptions, release note generation, staged artifact selection, and GitHub Release creation.

Remediation:

- Add a workflow-dispatch release dry-run mode that downloads artifacts, stages assets, generates notes for a supplied candidate tag, and either dry-runs signing or verifies dispatch-compatible signing identity.
- Skip only the final public release creation in dry-run mode.
- Make the operator doc require that dry-run release job path, not only build artifact inspection.
- Add static workflow checks that the dry-run and tag paths share the same staging/notes/signing logic.

Acceptance test: run `workflow_dispatch` with a candidate tag/ref and assert the workflow executes staging, notes generation, and signing verification steps without publishing a public release.

## Medium

### M-01: MCP JSON-line framing can read an unbounded line

Locations:

- [/home/john/clarion/crates/clarion-mcp/src/lib.rs:2188](/home/john/clarion/crates/clarion-mcp/src/lib.rs:2188), lines 2188-2195
- [/home/john/clarion/crates/clarion-mcp/src/lib.rs:2222](/home/john/clarion/crates/clarion-mcp/src/lib.rs:2222), lines 2222-2239
- [/home/john/clarion/crates/clarion-core/src/plugin/limits.rs:70](/home/john/clarion/crates/clarion-core/src/plugin/limits.rs:70), lines 70-71

Evidence: stdio frames beginning with `{`, `[`, or whitespace are routed to JSON-line compatibility mode. `read_json_line_frame` calls `read_until` into a `Vec` with no byte ceiling. Content-Length framing has an 8 MiB ceiling, but JSON-line framing does not.

Impact: a malicious or buggy MCP client can send a very large newline-free line and force unbounded memory growth in the MCP server.

Remediation:

- Apply `ContentLengthCeiling::DEFAULT` or a dedicated MCP line cap to JSON-line mode.
- Return a typed frame-too-large error before appending beyond the cap.
- Consider removing JSON-line compatibility from production stdio if Content-Length framing is the supported protocol.
- Add tests for oversized line, newline-free EOF, whitespace-prefixed JSON, and normal JSON-line requests.

Acceptance test: feed a JSON-line request larger than the configured cap; `read_stdio_frame` must reject it without allocating the full line.

### M-02: Pyright LSP response framing lacks bounded validation

Locations:

- [/home/john/clarion/plugins/python/src/clarion_plugin_python/pyright_session.py:879](/home/john/clarion/plugins/python/src/clarion_plugin_python/pyright_session.py:879), lines 879-902
- [/home/john/clarion/crates/clarion-core/src/plugin/transport.rs:166](/home/john/clarion/crates/clarion-core/src/plugin/transport.rs:166), lines 166-175

Evidence: `_read_message` reads Pyright headers, parses `Content-Length` with `int`, reads that many bytes, and passes the body to `json.loads`. It has no maximum content length and does not turn malformed/negative/oversized length or invalid JSON into a controlled poisoned-transport path. The Rust host/plugin transport has explicit size validation.

Impact: a faulty or compromised language server can trigger excessive reads/allocation or generic handler failure instead of a bounded Pyright restart/disable finding.

Remediation:

- Add `MAX_LSP_CONTENT_LENGTH`.
- Reject missing, duplicate, non-integer, negative, and oversized `Content-Length` as `LspTransportClosedError` or a dedicated poison error.
- Catch JSON decode failures and record a Pyright poison-frame finding before restart/disable.
- Add fake-langserver tests for malformed headers, oversized frames, truncated body, and invalid JSON.

Acceptance test: a fake Pyright server returning `Content-Length: 999999999` must not cause the plugin to allocate/read that body; it should record a controlled degradation finding.

### M-03: Pyright target-file indexing can turn one bad referenced file into a request failure

Locations:

- [/home/john/clarion/plugins/python/src/clarion_plugin_python/pyright_session.py:606](/home/john/clarion/plugins/python/src/clarion_plugin_python/pyright_session.py:606), lines 606-628
- [/home/john/clarion/plugins/python/src/clarion_plugin_python/pyright_session.py:917](/home/john/clarion/plugins/python/src/clarion_plugin_python/pyright_session.py:917), lines 917-923
- [/home/john/clarion/plugins/python/src/clarion_plugin_python/pyright_session.py:931](/home/john/clarion/plugins/python/src/clarion_plugin_python/pyright_session.py:931), lines 931-940
- [/home/john/clarion/plugins/python/src/clarion_plugin_python/server.py:197](/home/john/clarion/plugins/python/src/clarion_plugin_python/server.py:197), lines 197-201

Evidence: `server.py` catches `OSError` and `UnicodeDecodeError` when reading the directly analyzed file. Pyright target resolution calls `_function_index_for_path` for referenced internal files, and that helper performs `read_text(encoding="utf-8")` without the same containment.

Impact: a non-UTF-8 or otherwise unreadable in-project target can bubble out through call/reference resolution and become a generic handler failure for the current file, rather than a localized unresolved edge or degraded finding.

Remediation:

- Wrap `_function_index_for_path` file reads and parsing in a typed degraded-index path.
- Cache unavailable/degraded target indexes to avoid repeated failures.
- Treat bad target files as unresolved/external for call/reference purposes and record a bounded finding.
- Add regressions with invalid UTF-8 target files and unreadable target files.

Acceptance test: analyzing `a.py` that references `b.py`, where `b.py` is invalid UTF-8, should still return `a.py` entities and a degradation finding instead of failing the whole request.

### M-04: Repo-local `.filigree/ephemeral.port` can steer outbound Filigree calls

Locations:

- [/home/john/clarion/crates/clarion-federation/src/filigree_url.rs:53](/home/john/clarion/crates/clarion-federation/src/filigree_url.rs:53), lines 53-87
- [/home/john/clarion/crates/clarion-federation/src/filigree_url.rs:89](/home/john/clarion/crates/clarion-federation/src/filigree_url.rs:89), lines 89-97
- [/home/john/clarion/crates/clarion-cli/src/serve.rs:48](/home/john/clarion/crates/clarion-cli/src/serve.rs:48), lines 48-63
- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:3477](/home/john/clarion/crates/clarion-cli/src/analyze.rs:3477), lines 3477-3484

Evidence: Filigree URL resolution prefers `<project_root>/.filigree/ephemeral.port` over the configured port. The port file is read from the target project tree and used by both `serve` and `analyze` when building Filigree clients.

Impact: when Clarion analyzes an untrusted project with Filigree enabled and a token configured, project-local content can redirect bearer-authenticated calls to an attacker-chosen local port. The connection stays loopback, but the token and request body can still be exposed to a local listener.

Remediation:

- Treat the port file as trusted runtime state only after ownership/permission checks.
- Require an authenticated discovery handshake before sending bearer-token requests to an ephemeral-port endpoint.
- Consider ignoring repo-contained `.filigree/ephemeral.port` when analyzing untrusted projects unless an explicit flag enables ethereal discovery.
- Record the resolved URL source in emission stats for every outbound call.

Acceptance test: place `.filigree/ephemeral.port` in a project with unsafe permissions or mismatched ownership; Clarion must ignore it or fail closed before sending authenticated requests.

### M-05: SARIF import bypasses the live Filigree endpoint resolver

Locations:

- [/home/john/clarion/crates/clarion-cli/src/sarif.rs:17](/home/john/clarion/crates/clarion-cli/src/sarif.rs:17), lines 17-25
- [/home/john/clarion/crates/clarion-cli/src/serve.rs:48](/home/john/clarion/crates/clarion-cli/src/serve.rs:48), lines 48-63
- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:3477](/home/john/clarion/crates/clarion-cli/src/analyze.rs:3477), lines 3477-3484

Evidence: `serve` and `analyze` call `resolve_filigree_url` and prefer the live ethereal port. `sarif.rs` builds `FiligreeHttpClient` directly from the static config.

Impact: `clarion sarif import` is the least reliable federation path in dogfood/ethereal mode. It can post to a stale configured port even when the live dashboard port is available.

Remediation:

- Use `clarion_federation::filigree_url::resolve_filigree_url` in `sarif.rs` before constructing the client.
- Surface the configured URL, resolved URL, and source in logs/errors.
- Share outbound Filigree client construction between analyze and SARIF import to avoid drift.

Acceptance test: with config base URL on a stale port and `.filigree/ephemeral.port` pointing to a test Filigree server, `clarion sarif import` should post to the resolved live endpoint.

### M-06: Root-level added source files can produce a false fresh index verdict

Locations:

- [/home/john/clarion/crates/clarion-mcp/src/snapshot.rs:24](/home/john/clarion/crates/clarion-mcp/src/snapshot.rs:24), lines 24-35
- [/home/john/clarion/crates/clarion-mcp/src/snapshot.rs:383](/home/john/clarion/crates/clarion-mcp/src/snapshot.rs:383), lines 383-398
- [/home/john/clarion/crates/clarion-mcp/src/snapshot.rs:412](/home/john/clarion/crates/clarion-mcp/src/snapshot.rs:412), lines 412-430

Evidence: snapshot freshness watches direct parent directories of already ingested files but deliberately excludes the project root. It also checks only files already present in `entities.source_file_path`. A new top-level source file is neither an existing entity nor in a watched parent directory.

Impact: `clarion://context` and session-start freshness can report fresh while the graph is missing a new top-level source file.

Remediation:

- Store analyzed source roots or an analyzed file inventory.
- Watch root-level source additions while explicitly excluding `.clarion/` and other generated directories.
- Or compute freshness from the same ignore/plugin-extension walk used by analyze, bounded and reported as truncated when necessary.

Acceptance test: after analyzing a project with nested Python files, add `new_top_level.py` at the project root; freshness should report stale.

### M-07: Ambiguous inbound call queries scan all ambiguous call edges

Locations:

- [/home/john/clarion/crates/clarion-storage/src/query.rs:884](/home/john/clarion/crates/clarion-storage/src/query.rs:884), lines 884-925
- [/home/john/clarion/crates/clarion-cli/src/http_read/linkages.rs:132](/home/john/clarion/crates/clarion-cli/src/http_read/linkages.rs:132), lines 132-150
- [/home/john/clarion/crates/clarion-mcp/src/tools/graph.rs:340](/home/john/clarion/crates/clarion-mcp/src/tools/graph.rs:340), lines 340-357

Evidence: for `max_confidence >= ambiguous`, `call_edges_targeting` selects every ambiguous `calls` edge with properties and filters `candidate_ids` in Rust. HTTP and MCP callers materialize full vectors before aggregating/filtering/paginating.

Impact: one inbound query for a hot target on a graph with many ambiguous calls becomes global work over the shared reader pool, causing avoidable latency and potential head-of-line blocking.

Remediation:

- Normalize ambiguous call candidates into an indexed table keyed by candidate target.
- Push filtering, limit, offset, and counts into SQL.
- Add scan caps and honest truncation metadata for legacy rows until normalization lands.

Acceptance test: seed many ambiguous call edges with only one candidate pointing to the target; an inbound query should use an indexed candidate lookup and avoid scanning all ambiguous edges.

### M-08: Filigree emission happens before the local run terminal commit

Locations:

- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:1120](/home/john/clarion/crates/clarion-cli/src/analyze.rs:1120), lines 1120-1146
- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:1220](/home/john/clarion/crates/clarion-cli/src/analyze.rs:1220), lines 1220-1229
- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:1477](/home/john/clarion/crates/clarion-cli/src/analyze.rs:1477), lines 1477-1486
- [/home/john/clarion/crates/clarion-storage/src/writer.rs:1168](/home/john/clarion/crates/clarion-storage/src/writer.rs:1168), lines 1168-1183

Evidence: Phase 8 emits findings to Filigree before `CommitRun` so the emission outcome can be stored in `stats.json`. The local terminal update and final commit can still fail after an external POST succeeds.

Impact: Filigree can hold findings for a Clarion run that did not durably reach the matching terminal state. This does not make Filigree load-bearing, but it propagates stale/inconsistent state outward.

Remediation:

- Use an outbox pattern: commit the run locally with emission status `pending`, then emit after commit.
- Persist post-commit emission results in a run event table or outbox table rather than requiring them inside the terminal `stats.json`.
- On startup, reconcile pending/failed outbox entries.

Acceptance test: force `CommitRun` failure after a successful Filigree POST in a controlled test; the outbox should either prevent the POST before commit or preserve a reconcileable pending state.

### M-09: B8 perf harness tests are not part of visible CI

Locations:

- [/home/john/clarion/tests/perf/b8_scale_test/test_driver.py:5](/home/john/clarion/tests/perf/b8_scale_test/test_driver.py:5), lines 5-17
- [/home/john/clarion/.github/workflows/ci.yml:180](/home/john/clarion/.github/workflows/ci.yml:180), lines 180-181
- [/home/john/clarion/.github/workflows/ci.yml:192](/home/john/clarion/.github/workflows/ci.yml:192), lines 192-193

Evidence: `tests/perf/b8_scale_test/test_driver.py` is a pytest suite for the B8 driver. CI runs the B4/B5 performance gate script and `pytest plugins/python`, but does not visibly run `pytest tests/perf/b8_scale_test`.

Impact: perf harness parsing, manifest handling, and summary logic can drift while CI still reports green on the smoke gate.

Remediation:

- Add a CI step running `uv run --project plugins/python --extra dev pytest tests/perf/b8_scale_test`.
- Or fold those tests into the existing B4/B5 performance validation script.
- Keep this narrow so it validates harness logic without running the expensive scale benchmark.

Acceptance test: intentionally break `driver.parse_tool_response`; CI should fail in the B8 harness test step.

## Low

### L-01: Python plugin server inbound headers lack count and total-byte caps

Locations:

- [/home/john/clarion/plugins/python/src/clarion_plugin_python/server.py:66](/home/john/clarion/plugins/python/src/clarion_plugin_python/server.py:66), lines 66-97
- [/home/john/clarion/crates/clarion-core/src/plugin/transport.rs:150](/home/john/clarion/crates/clarion-core/src/plugin/transport.rs:150), lines 150-175

Evidence: the Python plugin caps body size but reads header lines until a blank line without a header count, header line length, or total header byte cap. The Rust host transport has stricter frame validation for the normal host-to-plugin boundary.

Impact: direct invocation of the Python plugin can consume memory on oversized headers. Normal Clarion host traffic is bounded, so this is a hardening issue rather than a primary product exploit.

Remediation:

- Mirror the Rust transport's framing limits in Python.
- Add max header line length, max header count, max total header bytes, and duplicate `Content-Length` behavior.
- Add direct server tests for oversized headers.

### L-02: Generic plugin timeouts use a Python-specific rule ID

Locations:

- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:2929](/home/john/clarion/crates/clarion-cli/src/analyze.rs:2929), lines 2929-2932
- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:4178](/home/john/clarion/crates/clarion-cli/src/analyze.rs:4178), lines 4178-4185
- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:4400](/home/john/clarion/crates/clarion-cli/src/analyze.rs:4400), lines 4400-4408

Evidence: `run_plugin_blocking` is generic over plugin execution, but per-file timeout findings use `CLA-PY-TIMEOUT`.

Impact: acceptable for the current Python-only v1 release line, but future non-Python plugins would be misclassified under a Python rule ID.

Remediation:

- Rename the host-side timeout to `CLA-INFRA-PLUGIN-TIMEOUT`, or derive a plugin/language-specific rule from the manifest with a generic fallback.
- Keep the Python-specific rule only for Python-plugin-owned timeout facts.

### L-03: Filigree federation client is blocking and forces async callers into thread wrappers

Locations:

- [/home/john/clarion/crates/clarion-federation/src/filigree.rs:270](/home/john/clarion/crates/clarion-federation/src/filigree.rs:270), lines 270-275
- [/home/john/clarion/crates/clarion-cli/src/analyze.rs:3486](/home/john/clarion/crates/clarion-cli/src/analyze.rs:3486), lines 3486-3501
- [/home/john/clarion/crates/clarion-cli/src/sarif.rs:160](/home/john/clarion/crates/clarion-cli/src/sarif.rs:160), lines 160-166

Evidence: `FiligreeHttpClient` owns a `reqwest::blocking::Client`. Async callers work around nested-runtime panics by moving request lifecycle into extra OS threads.

Impact: this adds complexity and makes it easier for future federation paths to reintroduce nested-runtime bugs.

Remediation:

- Add an async Filigree client for async contexts.
- Keep a small blocking adapter only for synchronous CLI paths.
- Centralize outbound client construction and retry/error handling.

## Areas Reviewed With No Finding

- Plugin host boundary: manifest size checks, executable basename validation, path jail, content-length ceiling, stderr ring buffer, entity/edge/finding cap accounting, path-escape breaker, timeout watchdog, and host-finding persistence are present.
- Rust storage basics: writer actor, WAL-oriented reader pool, schema versioning tests, generated columns, summary cache, unresolved call sites, inferred edge cache, and migration tests have broad coverage.
- Python extractor basics: the requested `scanner/ast_primitives.py` path does not exist; the equivalent AST implementation is `plugins/python/src/clarion_plugin_python/extractor.py`. The plugin currently has structured entity/tag extraction, duplicate-definition suppression, `@overload` skipping, syntax-error degradation, and stdout guarding.
- Wardline posture: the Python plugin no longer imports `wardline.core.registry`, and `plugins/python/plugin.toml` declares `wardline_aware = false`. Wardline-derived guidance and taint storage are explicit integration paths rather than hidden plugin startup coupling.
- HTTP route input handling: route body limits, HMAC replay protection, authorization log scrubbing, project-root path canonicalization, and SQL parameter binding were reviewed and did not produce additional findings beyond the auth-default and SARIF issues above.
- LLM provider surface: no separate LLM judge surface was found. Provider paths cover OpenRouter and CLI-provider parsing, malformed structured output, retryability, MCP summary budget/cache behavior, and live-provider opt-in.

