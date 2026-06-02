## clarion-core / plugin-host

**Location:** `crates/clarion-core/`

**Responsibility:** Provides the canonical entity-ID assembler, the synchronous JSON-RPC plugin-host supervisor (discover, spawn, validate, enforce, reap language-plugin subprocesses), and the shared wire-surface error-code vocabularies for the HTTP federation API and MCP tool envelope.

---

### Key Components

| File | LOC | Role |
|---|---|---|
| `src/plugin/host.rs` | 2958 | `PluginHost<R,W>` supervisor — spawn/connect, handshake, four-stage per-entity validation pipeline, edge pipeline, stats/unresolved-call-site validation, briefing-block application, idempotent shutdown, bounded stale-frame drain (`read_response_matching` / `MAX_DRAIN_FRAMES=16`) |
| `src/plugin/manifest.rs` | 1152 | TOML parser for `plugin.toml`; ADR-022 reserved-kind/rule-prefix gating; `validate_for_v0_1()` capability gate; `SignatureManifest` / `SignatureKindSchema` blocks (ADR-038 REQ-C-01); `RESERVED_ENTITY_KINDS = ["file", "subsystem", "guidance"]` at line 28 |
| `src/plugin/mock.rs` | 876 | `#[cfg(test)] pub(crate)` in-process mock plugin driver — test-only, no release binary footprint |
| `src/plugin/protocol.rs` | 875 | Typed JSON-RPC 2.0 envelopes; five method structs (`initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`); `JsonRpcVersion` newtype; `ProtocolError` with `MAX_PROTOCOL_ERROR_FIELD_BYTES=4 KiB` truncation-on-deserialise; `AnalyzeFileStats` with Pyright latency fields; `UnresolvedCallSite`; `EdgeConfidence` enum |
| `src/plugin/discovery.rs` | 667 | `discover()` / `discover_on_path()` — `$PATH` scanner for `clarion-plugin-<suffix>` executables; three-step manifest lookup (neighbor → install-prefix → symlink-resolved-pipx); dedup by directory |
| `src/plugin/limits.rs` | 572 | `ContentLengthCeiling` (8 MiB default), `EntityCountCap` (500k default), `PathEscapeBreaker` (>10 / 60s), `apply_prlimit_as` / `apply_prlimit_nofile_nproc` (Linux-only `setrlimit`), `effective_rss_mib`, limit constants, five `FINDING_*` subcode constants |
| `src/entity_id.rs` | 596 | `entity_id(plugin_id, kind, qname) -> Result<EntityId, EntityIdError>` per ADR-003/ADR-022; `validate_kind_grammar` shared helper; ~450 LOC cross-language byte-for-byte fixture parity tests covering entity + `contains`/`calls`/`references` edge shapes |
| `src/errors.rs` | 221 | `HttpErrorCode` (SCREAMING_SNAKE, frozen federation contract per ADR-034/ADR-037) + `McpErrorCode` (kebab-case, MCP tool envelope) — two independent closed error-code sets with pinned wire strings verified by inline tests |
| `src/plugin/breaker.rs` | 360 | `CrashLoopBreaker` (>3 crashes / 60s per ADR-002/UQ-WP2-10) — caller-driven; owned by the analyze run loop in `clarion-cli`, not by `PluginHost` |
| `src/plugin/host_findings.rs` | 273 | `HostFinding` struct + 10 `CLA-INFRA-*` subcode constants + constructor functions; `oom_killed` is `pub` (called from CLI after child reap) |
| `src/plugin/jail.rs` | 260 | `jail(root, candidate)` + `jail_to_string(root, candidate)` — canonicalise-then-starts-with membership check; TOCTOU by design (documented at `jail.rs:67-72`) |
| `src/plugin/transport.rs` | 569 | LSP-style `Content-Length:`-framed read/write; `read_frame(reader, ceiling)` rejects before consuming body on oversize; `MAX_HEADER_LINE_BYTES=8 KiB` |
| `src/plugin/mod.rs` | 51 | Facade re-exports; `mock` gated behind `#[cfg(test)] pub(crate)` |
| `src/lib.rs` | 51 | Crate-root `pub use` facade; re-export policy comment at lines 3-7 |

---

### Dependencies

**Inbound (who depends on this):**
- `crates/clarion-cli/src/analyze.rs` — principal consumer: `discover`, `parse_manifest`, `PluginHost::spawn`, `CrashLoopBreaker`, `AcceptedEntity`/`AcceptedEdge`/`AnalyzeFileOutcome`, `HostFinding`, `BriefingBlockReason`, `EdgeConfidence`, direct `entity_id` call for `core:file:*` and `core:subsystem:*` IDs
- `crates/clarion-cli/src/serve.rs`, `secret_scan.rs` — `BriefingBlockReason`, LLM-provider types, `HttpErrorCode`, `McpErrorCode`
- `crates/clarion-storage/src/writer.rs` — `EdgeConfidence` (via facade), plus a **facade leak**: line 537 reaches `clarion_core::plugin::manifest::RESERVED_ENTITY_KINDS` directly through the module path
- `crates/clarion-mcp/src/lib.rs` — `McpErrorCode`, entity-ID types
- `crates/clarion-plugin-fixture/src/main.rs` — speaks the L4 JSON-RPC protocol against the host
- Integration tests: `clarion-cli/tests/wp2_e2e.rs`, `clarion-mcp/tests/storage_tools.rs`

**Outbound (what this calls):**
- `std::process::Command`, `std::io::{BufRead,Write}`, `std::fs::canonicalize`, `std::thread`, `std::sync::{Arc,Mutex}` — no async runtime in the plugin-host or entity-ID layers
- External crates: `serde`/`serde_json`, `thiserror`, `toml`, `tracing`, `nix` (Linux `setrlimit`), `which` (via discovery), `reqwest` (LLM provider — `llm_provider.rs`, separate subsystem), `tempfile` (test-only)
- Plugin subprocesses over stdin/stdout pipes; stderr piped to detached drain thread → 64 KiB ring buffer

---

### Patterns Observed

- **Generic-over-IO supervisor with in-process mock** — `PluginHost<R: BufRead, W: Write>` (`host.rs:407`). `connect()` is the seam for in-process tests; `spawn()` wires `BufReader<ChildStdout>` / `BufWriter<ChildStdin>`. `mock.rs` (876 LOC, test-only) drives the full pipeline without a subprocess.
- **Bounded stale-frame drain (new vs prior)** — `read_response_matching` (`host.rs:1227-1270`) reads up to `MAX_DRAIN_FRAMES=16` frames looking for the expected JSON-RPC `id`, discarding stale or unparseable frames with a `tracing::warn`. Addresses two named historical vulnerabilities: `clarion-c08586a2da` (pre-baked frames defeating kill path) and `clarion-ff2831eec0` (partial-commit lever via stale frames after entities committed).
- **Drop-with-finding vs kill-with-error asymmetry** — `host.rs:886-998`. Steps 0–2 (field-size, ontology, identity) and step 3-open (jail miss below threshold) drop the entity + emit a finding. Step 3-trip (path-escape breaker) and step 4 (entity-cap) kill the plugin and return `HostError::*`. Edges (`process_edges`, `host.rs:1040-1083`) use drop-only, never kill.
- **Pre-frame size ceiling with no-body-consume rejection** — `transport.rs:67-71` returns `FrameTooLarge` before touching the body bytes, preventing a hostile `Content-Length` from forcing `read_to_end` into a multi-MiB allocation.
- **`pre_exec` resource-limit application** — `host.rs:592-608`. Linux-only; calls only `setrlimit(2)` inside the forked child, async-signal-safe per POSIX.1-2017 §2.4.3. Applies `RLIMIT_AS` (manifest hint or 2 GiB), `RLIMIT_NOFILE` (256), `RLIMIT_NPROC` (32 default; 4096 for pyright-enabled plugins — `host.rs:103-112`).
- **Idempotent shutdown via `terminated` flag** — `host.rs:1129-1134`. Flag set *before* the shutdown exchange begins (`do_shutdown` line 1187) so a mid-exchange pipe break does not cause spurious `BrokenPipe` on a defensive double-`shutdown()`.
- **SEI signature passthrough (new vs prior)** — `RawEntity.signature: Option<serde_json::Value>` (`host.rs:145`); `Manifest.signature: Option<SignatureManifest>` (`manifest.rs:145`). Per ADR-038 REQ-C-01: core stores verbatim, compares by string equality, never parses — the plugin owns the shape. `SignatureManifest.schema_version` voids cached comparisons on bump.
- **Newtype wire pins** — `JsonRpcVersion` (`protocol.rs:72-91`) serialises/deserialises only to `"2.0"`; `EntityId` constructible only via `entity_id()` / `FromStr::from_str` / custom `Deserialize` (`entity_id.rs:51-60`) so a deserialized arbitrary string cannot smuggle a malformed ID past the serde boundary.
- **Caller-driven vs host-driven breaker asymmetry** — `CrashLoopBreaker` (`breaker.rs`) lives in the caller (`analyze.rs`), policing the fleet of plugins across one run; `PathEscapeBreaker` (`limits.rs`) lives inside `PluginHost`, policing a single plugin's path emissions.
- **Cross-language byte-for-byte fixture parity** — `entity_id.rs:371-596` consumes `../../fixtures/entity_id.json` (same file as Python's `test_entity_id.py`) and asserts identical assembly for entities + three edge wire shapes.
- **Dual error-code vocabularies** — `errors.rs`: `HttpErrorCode` (SCREAMING_SNAKE, frozen ADR-034/ADR-037 federation contract) and `McpErrorCode` (kebab-case MCP tool envelope) are independent closed enums with pinned wire-string tests. The module explicitly documents the MCP→HTTP narrowing relationship as maintainer reference without code coupling.

---

### DRIFT

1. **`system-design.md §2` describes `tokio::process::Child` and `tokio::sync::mpsc` backpressure; the implementation is fully synchronous.**
   - Doc claim (`system-design.md:155`): "Plugin supervision uses `tokio::process::Child` with explicit `wait()` to reap zombies; SIGPIPE is ignored ... backpressure via a bounded `tokio::sync::mpsc` (default 100 messages) prevents a runaway plugin from OOMing the core."
   - Code reality: `PluginHost` uses `std::process::Command` (`host.rs:569`), not tokio. There is no async runtime, no mpsc channel, no SIGPIPE handling in this crate. The concurrency model is one thread per host (the calling thread for RPC, one detached thread for stderr drain). The design doc is pre-implementation; the implementation chose synchronous-over-BufRead/Write for simplicity.
   - **Severity: known divergence.** The CLAUDE.md does not list this as a named deviation. The WP2 architecture comment in `host.rs:1-28` makes no reference to tokio. The synchronous model is documented internally (`host.rs:8-9`) but not reconciled in the design doc.

2. **`system-design.md §2` (plugin lifecycle sequence, line 167) includes `clarion_version` in `initialize` params; the implementation sends only `protocol_version` and `project_root`.**
   - Doc claim (`system-design.md:167`): `initialize { project_root, clarion_version }`
   - Code reality: `InitializeParams { protocol_version: String, project_root: String }` (`protocol.rs:318-327`). No `clarion_version` field.
   - **Severity: minor** — the wire contract is defined by the code, not the sequence diagram. No consumers rely on the doc's `clarion_version` field.

3. **`system-design.md §2` (plugin lifecycle, line 171-174) describes `file_list(include, exclude)` and streaming entity/edge/finding notifications; the implementation uses batch `analyze_file` request/response.**
   - Doc claim (`system-design.md:171-185`): core sends `file_list(...)`; plugin returns `[files...]`; `analyze_file` produces "stream of file_analyzed notifications: { entity } | { edge } | { finding }"; `analyze_file` uses streaming notifications for incremental commit with mpsc backpressure.
   - Code reality: there is no `file_list` RPC. `analyze_file` is a strict request/response returning `AnalyzeFileResult { entities: Vec<Value>, edges: Vec<Value>, stats }` (`protocol.rs:433-447`). All per-file entities arrive in one response frame. Finding emission is via `HostFinding` accumulation + `take_findings()`, not a streaming notification.
   - **Severity: significant.** The file-discovery responsibility sits in `clarion-cli/analyze.rs`, not the plugin. The streaming-notification model described in the doc is not implemented; the batch-per-file design is what shipped. This is the largest divergence between §2 and the code.

4. **`system-design.md §2` manifest description lists `tags`, `capabilities` (boolean flags per capability), `supported_rule_ids`, and `prompt_templates` fields; none of these appear in `Manifest`.**
   - Doc claim (`system-design.md:193-198`): manifest has `kinds`, `tags`, `capabilities` (with `confidence_basis`), `supported_rule_ids`, `prompt_templates`.
   - Code reality: `Manifest` has `plugin`, `capabilities` (a `CapabilitiesRuntime` struct with `expected_max_rss_mb`, `expected_entities_per_file`, `wardline_aware`, `reads_outside_project_root`, optional `pyright`), `ontology` (with `entity_kinds`, `edge_kinds`, `rule_id_prefix`, `ontology_version`), `integrations`, `signature`. No `tags`, no `confidence_basis`, no `supported_rule_ids`, no `prompt_templates`.
   - **Severity: design-doc staleness.** The manifest schema is the ADR-022 implementation shape, which is more minimal and more precisely specified than the early §2 sketch.

5. **`errors.rs` (`HttpErrorCode`, `McpErrorCode`) is absent from the prior catalog (May 2026).**
   - New module added between the two analysis snapshots. `lib.rs:15` re-exports both types. Referenced by ADR-037 and ADR-034. Not a drift against design docs — both ADRs are Accepted — but represents a new crate responsibility (shared wire-surface error vocabularies) not captured in the prior analysis.

6. **`RESERVED_ENTITY_KINDS` facade leak persists.** `clarion-storage/src/writer.rs:537` uses `clarion_core::plugin::manifest::RESERVED_ENTITY_KINDS` directly, bypassing the `lib.rs` facade. The `lib.rs:3-7` policy comment explicitly designates the facade as the supported surface; this constant is not in it. Unchanged from the prior catalog.

---

### Quality Concerns / Debt

- **🔴 High — `host.rs` at 2958 LOC is the largest file in the workspace.** The four-stage entity validation pipeline, edge pipeline, stats/unresolved-call-site validation, briefing-block reconciliation, subprocess constructor with stderr drainer, and in-process `connect()` all live in one `impl PluginHost<R,W>` block. The module would split cleanly along lifecycle / pipeline / IO axes; the single-block shape makes per-step contracts harder to reason about and harder to test in isolation. The test section starts at line 1280 (678 LOC of inline tests). **Fix sketch**: extract a `ValidationPipeline` struct for steps 0–4 + edge processing; `PluginHost` becomes a thin lifecycle wrapper. `host.rs:1-2958`.

- **🔴 High — Three significant system-design.md §2 divergences are undocumented.** The tokio/async → sync-over-BufRead replacement, the file_list elimination, and the streaming-notification → batch-request/response pivot are each substantive architectural changes that ship without a reconciling ADR or a §2 errata note. Future contributors reading §2 get a materially wrong picture of the concurrency model and the plugin protocol shape. **Fix sketch**: retract the §2 sequence diagram's streaming/mpsc/tokio claims; add a note that the implementation chose synchronous batch-per-file over the draft spec; or write a narrowly-scoped ADR superseding the affected §2 claims. `docs/clarion/1.0/system-design.md:155-185`.

- **🟡 Med — `RESERVED_ENTITY_KINDS` facade leak in `clarion-storage`.** `clarion-storage/src/writer.rs:537` accesses `clarion_core::plugin::manifest::RESERVED_ENTITY_KINDS` via an internal module path not in the `lib.rs` facade. The facade's re-export policy comment (`lib.rs:3-7`) identifies the facade as the supported surface; this direct reach pins the internal module path as semi-public. **Fix sketch**: add `pub use manifest::RESERVED_ENTITY_KINDS` to `plugin/mod.rs` and re-export it through `lib.rs`, or expose `Manifest::is_reserved_kind(kind: &str) -> bool`. `clarion-storage/src/writer.rs:537`.

- **🟡 Med — Subprocess lifecycle ownership is unenforceable.** `PluginHost::spawn` returns `(Self, std::process::Child)` (`host.rs:528-532`). `Child::Drop` does not `waitpid` on Unix; the contract that the caller must reap is documented (`host.rs:653-659`) but not enforced by the type system. A future consumer that drops `Child` on a happy path leaves a zombie. **Fix sketch**: introduce a `KillOnDrop(Child)` newtype that calls `kill()` + `wait()` in `Drop`; `spawn` returns `(Self, KillOnDrop)`.

- **🟡 Med — All limit constants are hard-coded with no operator tunability.** Eleven constants across `host.rs` (`MAX_ENTITY_FIELD_BYTES=4 KiB`, `MAX_ENTITY_EXTRA_BYTES=64 KiB`, `MAX_UNRESOLVED_CALLEE_EXPR_BYTES=512`, `STDERR_TAIL_BYTES=64 KiB`, `MAX_DRAIN_FRAMES=16`), `transport.rs` (`MAX_HEADER_LINE_BYTES=8 KiB`), `limits.rs` (`ContentLengthCeiling::DEFAULT=8 MiB`, `EntityCountCap::DEFAULT_MAX=500k`, `DEFAULT_MAX_RSS_MIB=2 GiB`, `DEFAULT_MAX_NOFILE=256`, `DEFAULT_MAX_NPROC=32`), and `breaker.rs` (`DEFAULT_THRESHOLD=3`, `DEFAULT_WINDOW=60s`). `breaker.rs:7` and `limits.rs:63` acknowledge that the config surface lands in WP6; the concern is pre-logged, but "WP6" remains aspirational post-1.0. **Fix sketch**: expose these as `clarion.yaml` fields with the current values as defaults; plumb them from config through the analyze run to `PluginHost::new_inner` and the breaker constructors.

- **🟢 Low — Integration test coverage for failure modes is happy-path only.** `tests/host_subprocess.rs` (325 LOC) tests one happy-path subprocess walkthrough. The host's many failure modes (each `HostError` variant, each `CLA-INFRA-*` subcode, the kill paths, the Pyright `RLIMIT_NPROC` bump, `SIGPIPE` on a dead plugin stdin) are covered only by inline `#[cfg(test)]` unit tests against `MockPlugin` — no integration-level coverage of the real subprocess for error paths. **Fix sketch**: add integration tests in `tests/` for at least the path-escape-breaker and entity-cap kill paths against `clarion-plugin-fixture`.

- **🟢 Low — The path jail is TOCTOU by design but the warning is comment-only.** `jail.rs:67-72` documents that `jail_to_string` is a membership proof at canonicalization time, not a durable file handle. No `#[deprecated]` or `#[must_use]` annotation enforces the caveat. Any future caller that opens the returned path without re-checking is silently unsound. **Fix sketch**: add a `/// # TOCTOU warning` doc section to `jail_to_string` and consider returning a `JailedPath` newtype that carries the canonical root so callers can cheaply re-assert on open.

---

### Confidence

**High.** Read end-to-end: `lib.rs` (51 LOC), `errors.rs` (221 LOC), `entity_id.rs` (596 LOC), `plugin/mod.rs` (51 LOC), `plugin/breaker.rs` (360 LOC), `plugin/limits.rs` (572 LOC), `plugin/host_findings.rs` (273 LOC), `plugin/jail.rs` (first 120 LOC + public API), `plugin/transport.rs` (first 100 LOC), `plugin/protocol.rs` (100% — 875 LOC), `plugin/manifest.rs` (100% — 300 LOC shown, full structs + validation reviewed). `plugin/host.rs`: read lines 1–1277 end-to-end (the entire `impl` blocks, all public methods, the `read_response_matching` helper, `apply_briefing_block`, `process_edges`, `process_stats`, the spawn constructor, the stderr drain); lines 1278–2958 are the inline test section (sampled `compliant_manifest()`, `calls_manifest()`, `pyright_manifest()` fixtures at 1295–1355 and confirmed the `#[cfg(test)]` split). `plugin/discovery.rs`: read first 50 LOC (discovery doc + public types). `plugin/mock.rs`: not read (876 LOC, test-only, `#[cfg(test)] pub(crate)`). Verified the `RESERVED_ENTITY_KINDS` facade leak by grepping all crates. Cross-checked `system-design.md §2` (lines 120–238) against the code for every protocol claim. Verified `errors.rs` is new since prior catalog by confirming absence from `catalog-clarion-core.md`. ADR-034, ADR-037, ADR-038 referenced by code comments; ADR content not read end-to-end but cross-validated by manifest field names and error-code wire strings.
