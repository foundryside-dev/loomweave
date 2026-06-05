# Loomweave Comprehensive Read-Only Audit - 2026-06-04

Repository: `/home/john/loomweave`

Branch observed at session start: `ws6-guidance-maturity...origin/ws6-guidance-maturity`, ahead by 1. Initial tracked tree was clean. A later `git status` showed unrelated documentation changes; this report did not modify those files.

Mode: read-only audit plus this requested markdown artifact. Seven specialist subagents were dispatched as read-only explorers and instructed to avoid MCP tools, write tools, file edits, generated files, formatting, installs, staging, commits, and mutating commands. The subagent tool schema did not expose literal `enable_write_tools=false` or `enable_mcp_tools=false` fields, so those constraints were enforced in each subagent prompt.

Verification: source inspection only. No build, test, clippy, mypy, pytest, ruff, cargo metadata, or audit commands were run because they can write caches or artifacts. One parent `project_status_get` call against the Loomweave dogfood MCP timed out and was not used as evidence.

## Reviewer Coverage

- Architecture Critic: crate boundaries, Weft doctrine, plugin ontology, federation/storage coupling.
- Systems Thinker: graph lifecycle, feedback loops, freshness, read/write boundaries.
- Python Engineer: Python plugin extractor, Pyright integration, typing and Python idioms.
- Quality Engineer: CI/release gates, test structure, perf evidence, coverage posture.
- Security Architect: HTTP auth, path confinement, plugin trust boundary, release supply chain.
- Static Tools Analyst: static extraction, call/reference soundness, SEI, Wardline taint, SCC/Tarjan.
- MCP and CLI Specialist: CLI, MCP stdio, tool dispatch, HTTP/API semantic alignment.

## Critical

None found.

## High

### H-01: Scan-time edge graph is cumulative and can preserve stale topology

Locations:

- [/home/john/loomweave/crates/loomweave-storage/migrations/0001_initial_schema.sql:81](/home/john/loomweave/crates/loomweave-storage/migrations/0001_initial_schema.sql:81), lines 81-97
- [/home/john/loomweave/docs/loomweave/adr/ADR-026-containment-wire-and-edge-identity.md:85](/home/john/loomweave/docs/loomweave/adr/ADR-026-containment-wire-and-edge-identity.md:85), lines 85-91
- [/home/john/loomweave/crates/loomweave-storage/src/writer.rs:738](/home/john/loomweave/crates/loomweave-storage/src/writer.rs:738), lines 738-789

The `edges` table primary key is `(kind, from_id, to_id)`, and `insert_edge` uses `INSERT OR IGNORE`. This makes re-analysis idempotent, but it does not retract edges that disappeared from source, and it does not update metadata when the same triple gets a new source range, confidence, or properties.

Impact: MCP graph traversal, HTTP linkages, coupling, circular imports, guidance, and downstream federation consumers can read old graph relationships as current truth after source changes.

Remediation:

- Add per-source-file replacement semantics before inserting current anchored edges, scoped by `source_file_id` and edge kind.
- For cross-file call/import/reference edges, invalidate by source file, then insert the current file's emitted edges.
- Replace `INSERT OR IGNORE` with upsert/update behavior when the triple is still present but metadata changes.
- Add a regression that analyzes a fixture, removes or relocates a call, re-analyzes, and asserts the old edge is gone and source metadata updates.

### H-02: Index freshness ignores the persisted analyzed commit

Locations:

- [/home/john/loomweave/crates/loomweave-storage/migrations/0007_run_analyzed_commit.sql:1](/home/john/loomweave/crates/loomweave-storage/migrations/0007_run_analyzed_commit.sql:1), lines 1-13
- [/home/john/loomweave/crates/loomweave-mcp/src/index_diff.rs:203](/home/john/loomweave/crates/loomweave-mcp/src/index_diff.rs:203), lines 203-250
- [/home/john/loomweave/crates/loomweave-mcp/src/tools/status.rs:282](/home/john/loomweave/crates/loomweave-mcp/src/tools/status.rs:282), lines 282-287

Loomweave persists `runs.analyzed_at_commit`, but MCP freshness and status surfaces still describe `analyzed_commit`/`git_sha` as null and rely on weaker time-based heuristics.

Impact: branch switches or checkouts to older commits can leave the index stale without being reported as stale. Agents may trust an index built against a different commit.

Remediation:

- Select `analyzed_at_commit` in `read_index_state`.
- Carry it through `IndexState`, `latest_run`, `index_diff`, and `project_status`.
- Compute freshness primarily as `current_commit != analyzed_at_commit` when both are known; retain mtime/date checks as secondary diagnostics.
- Add tests for switching to a different but older HEAD.

### H-03: HTTP call-graph defaults include inferred edges by default

Locations:

- [/home/john/loomweave/crates/loomweave-cli/src/http_read/linkages.rs:78](/home/john/loomweave/crates/loomweave-cli/src/http_read/linkages.rs:78), lines 78-88
- [/home/john/loomweave/docs/loomweave/adr/ADR-028-edge-confidence-tiers.md:90](/home/john/loomweave/docs/loomweave/adr/ADR-028-edge-confidence-tiers.md:90), lines 90-96
- [/home/john/loomweave/docs/federation/contracts.md:343](/home/john/loomweave/docs/federation/contracts.md:343), lines 343-345

`parse_max_confidence(None)` defaults to `all`, which admits inferred edges. ADR-028 makes resolved-only the safe default for graph traversal, with weaker tiers opt-in. The HTTP contract currently documents the lower-precedence default as `all`.

Impact: an HTTP client omitting `confidence` can unknowingly receive ambiguous and persisted inferred edges as if they were baseline graph truth.

Remediation:

- Change HTTP default from `all` to `resolved`.
- Keep explicit `confidence=all` for callers that truly want all persisted tiers.
- Update `docs/federation/contracts.md` and fixtures.
- Add HTTP regression tests proving omitted `confidence` excludes ambiguous/inferred edges.

### H-04: MCP exposes mutating tools without a read-only/write-tool gate

Locations:

- [/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:297](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:297), lines 297-327
- [/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:355](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:355), lines 355-382
- [/home/john/loomweave/crates/loomweave-mcp/src/tools/analyze.rs:40](/home/john/loomweave/crates/loomweave-mcp/src/tools/analyze.rs:40), lines 40-75

The MCP server advertises mutating or process-spawning tools such as `analyze_start`, `analyze_cancel`, `propose_guidance`, and `promote_guidance`. There is no Loomweave config gate that lets an operator expose only read-only MCP tools.

Impact: a deployment that intends read-only consult mode cannot enforce that boundary at the server. Clients also cannot reliably distinguish pure read tools from tools that spawn processes, write SQLite, create observations, or invalidate summaries.

Remediation:

- Add tool metadata such as `read_only`, `writes_local_state`, `spawns_process`, and `may_call_llm`.
- Add config such as `serve.mcp.enable_write_tools = false` and default it conservatively for consult-only mode.
- Filter both `tools/list` and `tools/call` by policy.
- Add tests that disabled write tools are neither advertised nor callable.

### H-05: Pyright call resolution indexes overload stubs instead of implementations

Locations:

- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:863](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:863), lines 863-940
- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1000](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1000), lines 1000-1058

The extractor skips `@overload` stubs before emitting the implementation entity, but `_collect_entities` in `pyright_session.py` does not apply the same policy. It records the first duplicate entity ID, which can be the overload stub.

Impact: calls inside overloaded implementation bodies can be missed because the call resolver maps the entity ID to a stub body with no real calls.

Remediation:

- Share or duplicate the extractor overload-skip policy in `_collect_entities`.
- Prefer building the Pyright index from the same emitted entity set the plugin returns.
- Add a Pyright-backed regression where an overloaded implementation calls a helper and must emit `implementation -> helper`.

### H-06: Pyright degradation findings are recorded but dropped from the plugin wire result

Locations:

- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:247](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:247), lines 247-252
- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:308](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:308), lines 308-320
- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:938](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:938), lines 938-951
- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/server.py:218](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/server.py:218), lines 218-230
- [/home/john/loomweave/crates/loomweave-core/src/plugin/protocol.rs:364](/home/john/loomweave/crates/loomweave-core/src/plugin/protocol.rs:364), lines 364-397

Pyright records findings for timeout, cap, and process degradation paths, but `AnalyzeFileResult`/server response does not carry those findings to the host.

Impact: the graph can lose calls/references without durable evidence explaining the index is incomplete. Query-time recovery also has less context when unresolved call-site details are missing.

Remediation:

- Extend `AnalyzeFileResult` or `AnalyzeFileStats` with bounded plugin findings.
- Validate size and schema in the host.
- Persist them as `LMWV-PY-*` findings with file/entity anchors.
- Add tests for Pyright unavailable, timeout, cap-exceeded, and poisoned-process paths.

### H-07: Source files are scanned and dispatched before a jail-safe open

Locations:

- [/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4946](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4946), lines 4946-4972
- [/home/john/loomweave/crates/loomweave-cli/src/secret_scan.rs:393](/home/john/loomweave/crates/loomweave-cli/src/secret_scan.rs:393), lines 393-399
- [/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:859](/home/john/loomweave/crates/loomweave-core/src/plugin/host.rs:859), lines 859-874
- [/home/john/loomweave/crates/loomweave-core/src/plugin/jail.rs:86](/home/john/loomweave/crates/loomweave-core/src/plugin/jail.rs:86), lines 86-129

The walker disables symlink following, but later secret scanning reads `fs::read(file)`, and plugin dispatch sends the path to the plugin before returned source paths are jail-checked. A safer `safe_open` helper exists but is not used before scanner reads or plugin dispatch.

Impact: in a writable workspace, a symlink swap after walking can make Loomweave read or hand a plugin an out-of-tree file before later output validation rejects paths.

Remediation:

- Use `safe_open(project_root, file)` or an openat-style confined read before secret scanning.
- Revalidate immediately before `host.analyze_file`.
- Prefer passing plugins a stable verified copy or canonical jailed path.
- Treat jail-open failures as skipped files with audit findings.

## Medium

### M-01: Briefing-blocked neighbor identities leak through linkage endpoints

Locations:

- [/home/john/loomweave/crates/loomweave-cli/src/http_read/linkages.rs:133](/home/john/loomweave/crates/loomweave-cli/src/http_read/linkages.rs:133), lines 133-151
- [/home/john/loomweave/crates/loomweave-cli/src/http_read/linkages.rs:204](/home/john/loomweave/crates/loomweave-cli/src/http_read/linkages.rs:204), lines 204-214
- [/home/john/loomweave/docs/federation/contracts.md:390](/home/john/loomweave/docs/federation/contracts.md:390), lines 390-398

The handlers check the queried entity for `briefing_blocked` but do not filter returned neighbor IDs. The contract explicitly says neighbors are not filtered.

Impact: a sibling client can enumerate blocked entity IDs and relationship shape via visible neighbors, even when direct content/file access is refused.

Remediation:

- Apply visibility filtering to every returned neighbor.
- Return `blocked_neighbor_count` aggregates when graph density matters.
- Align MCP and HTTP policy docs.
- Add visible-to-blocked and blocked-to-visible linkage tests.

### M-02: Function/class LSP name positions use first substring match

Locations:

- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1018](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1018), lines 1018-1026
- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1066](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1066), lines 1066-1074

`line_text.find(child.name)` can match text before the declaration name, such as the `f` in `def f():`.

Impact: Pyright call hierarchy and definition lookups can miss or mis-map short names and repeated names, producing wrong or missing edges.

Remediation:

- Locate declaration-name tokens structurally with `tokenize`, anchored after `def`, `async def`, or `class`.
- Add regressions for `def f():`, `def d(d):`, lowercase class names, and call/reference resolution to those symbols.

### M-03: Reference lookup cache ignores source position

Locations:

- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:500](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:500), lines 500-531
- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1130](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1130), lines 1130-1135

The cache key is `(from_id, kind, lexeme)`, omitting line/character or binding identity.

Impact: identical names under one owner can resolve to different bindings after imports, shadowing, or reassignment, but the cache can collapse them into one target.

Remediation:

- Include line/character or byte span in the cache key.
- Or cache only after proving the same lexical binding.
- Add tests for import shadowing and local rebinding within the same owner.

### M-04: Fallback call target containment chooses the outermost function

Location:

- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1539](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/pyright_session.py:1539), lines 1539-1549

`_containing_function_id` scans `index.functions` in insertion order. Since `_collect_entities` inserts outer functions before nested functions, fallback can return the outer function for a range inside a nested callable.

Impact: when Pyright returns a range inside a nested function but not exactly at the name position, an edge can target the enclosing function.

Remediation:

- Choose the smallest containing span.
- Or iterate candidate functions deepest-first.
- Add nested-function fallback tests where the target range sits inside the nested body.

### M-05: Caller-supplied Wardline SEI can override server-resolved locator identity

Location:

- [/home/john/loomweave/crates/loomweave-cli/src/http_read/wardline.rs:233](/home/john/loomweave/crates/loomweave-cli/src/http_read/wardline.rs:233), lines 233-267

The write path resolves locator SEIs, then lets `fact.sei` win if supplied.

Impact: a bad client can store a taint fact for locator A under locator B's SEI, causing misattribution after rename or SEI-based reads.

Remediation:

- When both caller-supplied and server-resolved SEIs exist, require equality.
- Reject conflicts with a typed error or store an explicit conflict finding.
- Add tests for matching, missing, and conflicting SEIs.

### M-06: Wardline SARIF absolute `file://` URI normalization can corrupt paths

Location:

- [/home/john/loomweave/crates/loomweave-cli/src/sarif.rs:91](/home/john/loomweave/crates/loomweave-cli/src/sarif.rs:91), lines 91-103

URI stripping checks `file://` before `file:///` and then trims all leading slashes.

Impact: `file:///home/john/project/src/a.py` can become `home/john/project/src/a.py`, preventing correct finding-to-file/entity reconciliation.

Remediation:

- Parse SARIF artifact URIs with URL/path APIs.
- Relativize against `project_root`.
- Reject or preserve unresolved absolute paths explicitly.
- Add tests for POSIX absolute file URIs and relative SARIF URIs.

### M-07: Read surfaces mutate run lifecycle through the reader pool

Locations:

- [/home/john/loomweave/crates/loomweave-storage/src/reader.rs:1](/home/john/loomweave/crates/loomweave-storage/src/reader.rs:1), lines 1-7
- [/home/john/loomweave/crates/loomweave-storage/src/reader.rs:134](/home/john/loomweave/crates/loomweave-storage/src/reader.rs:134), lines 134-145
- [/home/john/loomweave/crates/loomweave-mcp/src/tools/status.rs:173](/home/john/loomweave/crates/loomweave-mcp/src/tools/status.rs:173), lines 173-199
- [/home/john/loomweave/crates/loomweave-mcp/src/tools/analyze.rs:253](/home/john/loomweave/crates/loomweave-mcp/src/tools/analyze.rs:253), lines 253-272

Status/read paths can mark stale running analyses failed through a pool documented as read-only.

Impact: observing status mutates durable state, weakening the single-writer/read-surface boundary and making operational reasoning harder.

Remediation:

- Move stale-run reconciliation behind an explicit writer/maintenance command.
- Or rename/split the pool to document and isolate write-capable maintenance reads.
- Add tests around the intended mutation boundary.

### M-08: ReaderPool serve validation does not enforce Loomweave database identity

Locations:

- [/home/john/loomweave/crates/loomweave-storage/src/pragma.rs:26](/home/john/loomweave/crates/loomweave-storage/src/pragma.rs:26), lines 26-42
- [/home/john/loomweave/crates/loomweave-storage/src/reader.rs:81](/home/john/loomweave/crates/loomweave-storage/src/reader.rs:81), lines 81-91
- [/home/john/loomweave/crates/loomweave-cli/src/serve.rs:70](/home/john/loomweave/crates/loomweave-cli/src/serve.rs:70), lines 70-74

Write-side startup enforces SQLite `application_id`, but `loomweave serve` reader validation checks only schema readability/version, not database identity.

Impact: a mispointed or foreign SQLite file can pass serve startup and later fail as confusing empty or broken queries.

Remediation:

- Add read-side validation for `application_id` and compatible `user_version` without mutating legacy zero-ID files.
- Or perform an explicit identity check before opening serve readers.
- Add `ReaderPool::open_validated` tests for foreign application IDs and future schema versions.

### M-09: Binary crate has absorbed core architectural responsibilities

Locations:

- [/home/john/loomweave/crates/loomweave-cli/src/main.rs:1](/home/john/loomweave/crates/loomweave-cli/src/main.rs:1), lines 1-22
- [/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:24](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:24), lines 24-48
- [/home/john/loomweave/crates/loomweave-cli/src/clustering.rs:8](/home/john/loomweave/crates/loomweave-cli/src/clustering.rs:8), lines 8-58

`loomweave-cli` privately owns analysis orchestration, clustering, config mapping, and HTTP serving glue.

Impact: other surfaces must shell out to `loomweave analyze` instead of linking reusable analysis logic. Boundaries become harder to defend as Loomweave adds languages and surfaces.

Remediation:

- Extract analysis orchestration and clustering into a library crate such as `loomweave-analyze` or a clearly bounded `loomweave-core::analysis`.
- Leave `loomweave-cli` as a thin command wrapper.
- Move HTTP read serving to a crate whose ownership matches the served API surface.

### M-10: Plugin ontology boundary is weakened by hardcoded kind semantics

Locations:

- [/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:119](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:119), lines 119-127
- [/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4730](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4730), lines 4730-4755
- [/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4799](/home/john/loomweave/crates/loomweave-cli/src/analyze.rs:4799), lines 4799-4828
- [/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2854](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2854), lines 2854-2863

Rust-side paths still treat names such as `module` and `function` as special semantic buckets, even though requirements put ontology ownership at the plugin boundary.

Impact: new languages or plugin kinds can store rows but silently miss hashing, unresolved-call tracking, source matching, or failure reporting behavior.

Remediation:

- Move these semantics into manifest-declared roles such as `file_scope`, `callable`, `syntax_degraded_module`, and rule mappings.
- Keep Python-specific failure subcodes emitted by the Python plugin or a Python adapter.
- Add manifest-role tests with a non-Python fixture plugin.

### M-11: Federation crate depends on storage row shape

Locations:

- [/home/john/loomweave/crates/loomweave-federation/Cargo.toml:12](/home/john/loomweave/crates/loomweave-federation/Cargo.toml:12), lines 12-16
- [/home/john/loomweave/crates/loomweave-federation/src/scan_results.rs:13](/home/john/loomweave/crates/loomweave-federation/src/scan_results.rs:13), lines 13-17
- [/home/john/loomweave/crates/loomweave-federation/src/scan_results.rs:92](/home/john/loomweave/crates/loomweave-federation/src/scan_results.rs:92), lines 92-101

`loomweave-federation` consumes `loomweave_storage::FindingForEmitRow` directly.

Impact: storage projection refactors cross a federation boundary unnecessarily, making the external contract layer depend on persistence internals.

Remediation:

- Define federation DTOs in `loomweave-federation`.
- Map storage rows to DTOs in `loomweave-cli` or `loomweave-storage`.
- Keep storage schema evolution behind the storage crate.

### M-12: Release required-check guard does not require the macOS CI job

Locations:

- [/home/john/loomweave/.github/workflows/ci.yml:103](/home/john/loomweave/.github/workflows/ci.yml:103), lines 103-141
- [/home/john/loomweave/scripts/check-github-release-governance.py:44](/home/john/loomweave/scripts/check-github-release-governance.py:44), lines 44-50

CI defines a macOS Rust job to catch platform-specific `-D warnings`, but the release governance guard only requires `Rust`, `Python plugin`, and `Sprint 1 walking skeleton (end-to-end)`.

Impact: a release PR can satisfy the documented governance gate while macOS is red or not required.

Remediation:

- Add `Rust (aarch64-apple-darwin)` to `REQUIRED_STATUS_CHECKS`.
- Update the governance docs and ruleset example.
- Extend self-test fixtures so a ruleset missing macOS fails.

### M-13: Performance/scale gates validate stale evidence, not live behavior

Locations:

- [/home/john/loomweave/.github/workflows/ci.yml:161](/home/john/loomweave/.github/workflows/ci.yml:161), lines 161-162
- [/home/john/loomweave/scripts/check-b4-gate-result.py:41](/home/john/loomweave/scripts/check-b4-gate-result.py:41), lines 41-59
- [/home/john/loomweave/tests/perf/b5_reference_scale_smoke.py:153](/home/john/loomweave/tests/perf/b5_reference_scale_smoke.py:153), lines 153-258

CI checks freshness of prior B.4 evidence but does not run even a synthetic perf corpus.

Impact: Pyright query-volume, latency, or scale regressions can pass until the freshness window expires.

Remediation:

- Add a lightweight CI or nightly synthetic perf job with fixed thresholds.
- Or add an optional execution mode to `check-b4-gate-result.py`.
- Fail on skipped-reference caps, excessive request/function ratio, and p95 latency drift.

### M-14: Python CI/release dependency path is not lock/hash/audit enforced

Locations:

- [/home/john/loomweave/.github/workflows/ci.yml:149](/home/john/loomweave/.github/workflows/ci.yml:149), lines 149-182
- [/home/john/loomweave/.github/workflows/release.yml:124](/home/john/loomweave/.github/workflows/release.yml:124), lines 124-137
- [/home/john/loomweave/.github/workflows/release.yml:248](/home/john/loomweave/.github/workflows/release.yml:248), lines 248-260
- [/home/john/loomweave/deny.toml:3](/home/john/loomweave/deny.toml:3), lines 3-7

Rust dependencies are covered by `cargo deny`, but Python CI/release installs from PyPI via pip without lock enforcement, hash checking, or Python vulnerability audit.

Impact: dependency compromise or confusion can affect package build outputs that later get signed and published.

Remediation:

- Use `uv sync --locked` or `pip install --require-hashes` from a committed lock file in CI and release.
- Add `pip-audit` or equivalent.
- Generate SBOM/provenance for Python artifacts before signing.

### M-15: MCP cancellation notifications are ignored and cannot interrupt long calls

Locations:

- [/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:811](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:811), lines 811-814
- [/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2035](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2035), line 2035
- [/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:380](/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:380), line 380
- [/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:597](/home/john/loomweave/crates/loomweave-mcp/src/tools/summary.rs:597), line 597

The server drops JSON-RPC notifications before method handling and processes stdio frames sequentially, while summary/inferred paths can await long LLM calls.

Impact: client cancellation cannot stop token-spending or long-running calls, and the server cannot observe cancellation until the request finishes.

Remediation:

- Handle `notifications/cancelled` by request ID.
- Run tool calls as cancellable tasks.
- Pass cancellation tokens into LLM and inference paths.
- Keep the stdio reader able to process notifications while a tool runs.

### M-16: MCP `confidence` argument treats wrong JSON types as the default

Location:

- [/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2235](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:2235), lines 2235-2244

`arguments.get("confidence").and_then(Value::as_str)` treats `null`, numbers, arrays, and objects like an omitted field and defaults to resolved.

Impact: malformed client requests silently succeed with semantics the client may not have intended.

Remediation:

- Default only when the field is absent.
- Accept only string enum values.
- Reject all other JSON types with `-32602`.
- Add tests for null, number, object, array, and unknown string.

### M-17: `TYPE_CHECKING` import classification is too broad for boolean expressions

Locations:

- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:501](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:501), lines 501-511
- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:579](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/extractor.py:579), lines 579-586

The predicate treats any boolean expression containing `TYPE_CHECKING` as type-only.

Impact: `if TYPE_CHECKING or runtime_flag:` can run at runtime, but imports inside the block are marked `type_only=True`, weakening import-edge semantics.

Remediation:

- Treat only bare `TYPE_CHECKING` and `typing.TYPE_CHECKING` as type-only by default.
- For boolean expressions, accept only conservative forms whose runtime truth is solely type-checking.
- Add tests for `TYPE_CHECKING or runtime_flag`, `TYPE_CHECKING and flag`, and `not TYPE_CHECKING`.

## Low

### L-01: HTTP tracing records raw `X-Weft-Component` authentication material

Locations:

- [/home/john/loomweave/crates/loomweave-cli/src/http_read.rs:609](/home/john/loomweave/crates/loomweave-cli/src/http_read.rs:609), lines 609-632
- [/home/john/loomweave/crates/loomweave-cli/src/http_read/auth.rs:135](/home/john/loomweave/crates/loomweave-cli/src/http_read/auth.rs:135), lines 135-141

The request span records the raw `x-weft-component` header. For HMAC auth, that header includes the signature.

Impact: logs can contain signed request material. Timestamp and nonce are separate, but combined logs may increase replay risk inside the freshness window.

Remediation:

- Do not log raw auth headers.
- Record only derived booleans or non-secret component kind.
- Add a scrub list for `authorization`, `x-weft-component`, `x-weft-timestamp`, and `x-weft-nonce`.

### L-02: Python coverage is reported but not gated

Locations:

- [/home/john/loomweave/plugins/python/pyproject.toml:97](/home/john/loomweave/plugins/python/pyproject.toml:97), lines 97-104
- [/home/john/loomweave/.github/workflows/ci.yml:181](/home/john/loomweave/.github/workflows/ci.yml:181), lines 181-182

`pytest-cov` is configured for reporting, but no `--cov-fail-under` threshold gates CI.

Impact: parser, Pyright, and protocol coverage can regress while CI stays green.

Remediation:

- Set an initial threshold at the measured current baseline.
- Ratchet deliberately after targeted tests land.

### L-03: Release governance docs omit tag-ruleset requirement enforced by code

Locations:

- [/home/john/loomweave/scripts/check-github-release-governance.py:363](/home/john/loomweave/scripts/check-github-release-governance.py:363), lines 363-386
- [/home/john/loomweave/docs/operator/v1.0-release-governance.md:12](/home/john/loomweave/docs/operator/v1.0-release-governance.md:12), lines 12-28

The script enforces an active ruleset for `refs/tags/v*`, but the operator checklist omits it.

Impact: operators following the doc can configure every listed control and still fail the executable guard.

Remediation:

- Add the tag-ruleset requirement and a minimal REST/ruleset shape to the doc.
- Add a lockstep assertion so new required controls must be documented.

### L-04: Wardline probe is a dead integration surface with stale meaning

Locations:

- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/wardline_probe.py:1](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/wardline_probe.py:1), lines 1-22
- [/home/john/loomweave/plugins/python/src/loomweave_plugin_python/server.py:140](/home/john/loomweave/plugins/python/src/loomweave_plugin_python/server.py:140), lines 140-150
- [/home/john/loomweave/plugins/python/plugin.toml:22](/home/john/loomweave/plugins/python/plugin.toml:22), lines 22-27

The probe claims startup Wardline discovery, but the server does not call it and the plugin handshake returns empty capabilities.

Impact: the plugin remains Weft-safe and not Wardline-dependent, but dead probe code and stale wording make federation state harder to audit.

Remediation:

- Remove the unused probe/tests until Wardline-aware extraction lands.
- Or wire a fail-soft descriptor-read probe into `initialize` and update docs/ADR state.

### L-05: Advertised MCP schemas are stricter than runtime dispatch

Locations:

- [/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:160](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:160), lines 160-171
- [/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:835](/home/john/loomweave/crates/loomweave-mcp/src/lib.rs:835), lines 835-845
- [/home/john/loomweave/crates/loomweave-mcp/src/tools/graph.rs:83](/home/john/loomweave/crates/loomweave-mcp/src/tools/graph.rs:83), line 83

Tool schemas advertise `additionalProperties: false`, but runtime dispatch generally validates only object shape and lets handlers ignore unknown keys.

Impact: misspelled or safety-looking arguments can be silently ignored despite a strict advertised schema.

Remediation:

- Add central JSON Schema validation against `list_tools()` before dispatch.
- Or replace map-based parsing with typed `Deserialize` structs using `deny_unknown_fields`.

## Notable Non-Findings

- SCC/Tarjan cycle logic looked sound in the inspected scope. The implementation handles target-only nodes, self-edges, deterministic ordering, and truncation tests around `/home/john/loomweave/crates/loomweave-mcp/src/catalogue/shortcuts.rs:855`.
- Subsystem clustering normalizes community/member order before hashing and output in the inspected paths.
- No evidence was found that Loomweave introduces mandatory Weft sibling dependencies; the federation concerns above are enrichment/policy-boundary issues, not mandatory-centralization issues.

## Verification Gaps

- No test/build/lint gates were run due the requested read-only audit mode.
- The parent Loomweave dogfood MCP status call timed out after 120 seconds and was not used as evidence.
- Findings should be converted to Filigree issues or implementation tasks before remediation work starts.
