# 01 — Discovery Findings (Clarion)

> Methodology note: produced from source (Rust + Python), `Cargo.toml`,
> `plugin.toml`, `pyproject.toml`, `clarion.yaml`, `.mcp.json`, migration SQL,
> e2e shell scripts, and per-crate test files only. No design docs, ADRs, or
> sprint/WP narrative were read while writing this document.

## 1. One-Paragraph Pitch (Inferred From Code)

Clarion is a Rust-implemented code-archaeology toolchain: a `clarion`
binary (`crates/clarion-cli/src/main.rs`) that (a) installs a per-project
`.clarion/clarion.db` SQLite store with embedded migrations
(`install.rs` + `crates/clarion-storage/migrations/0001_initial_schema.sql`),
(b) walks a source tree, discovers `clarion-plugin-*` executables on `$PATH`,
spawns each as a JSON-RPC subprocess, ingests entities/edges they extract, and
persists them through a writer-actor (`analyze.rs`, `clarion-storage/writer.rs`),
and (c) `clarion serve` runs as a stdio MCP server exposing **19**
agent-facing tools (`crates/clarion-mcp/src/lib.rs::list_tools`; original
discovery said "twenty" — corrected during validation against the actual
`ToolDefinition` registry; see §4 of `02-subsystem-catalog.md`) plus an Axum
HTTP read API on `/api/v1/files*` (`crates/clarion-cli/src/http_read.rs:347`).
Out-of-tree language plugins follow the protocol defined in
`crates/clarion-core/src/plugin/protocol.rs`; the in-repo reference plugin
(`plugins/python/`) extracts module/class/function entities plus
`contains`/`calls`/`references`/`imports` edges using a `pyright` language-server
session for type-resolved call/reference targets. Audience is consult-mode
LLM agents and developers needing structured navigation of a codebase.

## 2. Repository Layout

```
clarion/
├── Cargo.toml                       Rust workspace, resolver "3", 6 members
├── clarion.yaml                     User-edited runtime config (LLM, integrations, serve.http)
├── .mcp.json                        Registers clarion + filigree as stdio MCP servers
├── rust-toolchain.toml              channel = stable, profile = minimal
├── crates/
│   ├── clarion-core/      ~11.7k LOC src,  325 LOC tests  — domain types, plugin host
│   ├── clarion-storage/   ~3.2k  LOC src, 4.86k LOC tests — SQLite layer, writer-actor
│   ├── clarion-cli/       ~6.8k  LOC src, 6.4k  LOC tests — `clarion` binary
│   ├── clarion-mcp/       ~6.6k  LOC src, 2.2k  LOC tests — MCP protocol surface
│   ├── clarion-scanner/   ~880   LOC src, 655   LOC tests — pre-ingest secret scanner
│   └── clarion-plugin-fixture/ 187 LOC    — test-only Rust plugin
├── plugins/python/
│   ├── plugin.toml                  Plugin manifest (plugin_id=python, ontology v0.6.0)
│   ├── pyproject.toml               name=clarion-plugin-python, requires-python>=3.11
│   ├── src/clarion_plugin_python/   3028 LOC across 11 modules
│   └── tests/                       9 pytest files, 3440 LOC
├── fixtures/entity_id.json          Cross-language byte-for-byte parity fixture
├── tests/
│   ├── e2e/                         4 bash scripts (sprint_1, sprint_2_mcp, phase3, wp5_secret_scan)
│   └── perf/                        b8_scale_test driver + elspeth_mini corpus (~150 Python files)
├── scripts/                         CI helpers (b4 gate, governance, migration retirement, version lockstep)
├── .pre-commit-config.yaml          ruff, ruff-format, mypy --strict for plugins/python
└── .github/workflows/               ci.yml + release.yml
```

Total Rust source: ~29k LOC across 6 crates (largest single files:
`clarion-mcp/src/lib.rs` 4703, `clarion-core/src/plugin/host.rs` 2935,
`clarion-cli/src/analyze.rs` 2549, `clarion-core/src/llm_provider.rs` 2467).
Total Python source: 3028 LOC, dominated by `pyright_session.py` 1406 and
`extractor.py` 932.

## 3. Technology Stack

- **Languages:**
  - Rust: edition `2024`, MSRV `1.88`, toolchain channel `stable`
    (`Cargo.toml:17`, `rust-toolchain.toml`).
  - Python: `>=3.11` (`plugins/python/pyproject.toml:10`); ruff target
    `py311`; mypy `python_version = "3.11"` with `strict = true`.

- **Rust dependencies (key, from root `Cargo.toml` workspace.dependencies):**
  - `tokio` 1 — multi-thread runtime + sync primitives (`rt-multi-thread`,
    `macros`, `net`, `sync`, `time`).
  - `rusqlite` 0.31 with `bundled` SQLite — embedded DB engine.
  - `deadpool-sqlite` 0.8 (`rt_tokio_1`) — async reader pool.
  - `axum` 0.7 + `tower` 0.5 + `tower-http` 0.6 — HTTP read API.
  - `reqwest` 0.12 with `rustls-tls-native-roots`, `blocking`, `json` —
    outbound HTTP (Filigree, OpenRouter).
  - `clap` 4 (`derive`) — CLI parser.
  - `serde`/`serde_json`/`serde_norway` (YAML) — config + wire formats.
  - `tracing` + `tracing-subscriber` (`env-filter`) — structured logs.
  - `thiserror` + `anyhow` — typed library errors / binary errors.
  - `blake3`, `sha1`, `sha2` — content/secret hashing.
  - `xgraph` 2.0.0 — used by `clarion-cli/src/clustering.rs` for Leiden
    community detection (`xgraph::graph::algorithms::leiden_clustering`).
  - `regex` 1, `ignore` 0.4 — secret-scanner patterns and file walking.
  - `nix` 0.28 (`resource`) — `setrlimit(2)` for plugin sandboxing.
  - `which` 6 — locate plugins on `$PATH`.
  - `cargo-deny` policy file (`deny.toml`) checked in CI.

- **Python dependencies (`plugins/python/pyproject.toml`):**
  - Runtime: `packaging>=24`, `pyright==1.1.409` (pinned exact for the
    type-resolved call/reference probe).
  - Dev: `pytest>=8.0`, `pytest-cov>=5.0`, `ruff>=0.6`, `mypy>=1.11`,
    `pre-commit>=3.8`.

- **External processes / services:**
  - **SQLite** — single file at `.clarion/clarion.db`; WAL mode forced by
    `crates/clarion-storage/src/pragma.rs:17`; one migration `0001_initial_schema.sql`.
  - **Plugin subprocesses** — spawned by `PluginHost::spawn` over
    stdin/stdout JSON-RPC (`crates/clarion-core/src/plugin/host.rs`),
    discovered by `discover()` scanning `$PATH` for `clarion-plugin-*`
    (`crates/clarion-core/src/plugin/discovery.rs`).
  - **pyright language server** — invoked as a subprocess by the Python
    plugin (`plugins/python/src/clarion_plugin_python/pyright_session.py:7-9`
    imports `select`, `subprocess`).
  - **MCP stdio transport** — `clarion serve` exposes JSON-RPC 2.0 over
    stdin/stdout with `protocolVersion = "2025-11-25"`
    (`crates/clarion-mcp/src/lib.rs:40`); reads Content-Length frames via
    `clarion_core::plugin::transport`.
  - **HTTP read API** — bound by `serve.http.bind` (default `127.0.0.1:9111`
    per `clarion.yaml`); Axum router at `http_read.rs:347-369` with
    `ConcurrencyLimitLayer(64)` + 10s timeout + `LoadShedLayer` +
    `CatchPanicLayer` + body-size limit. Six routes total.
  - **Filigree** — outbound HTTP via `reqwest` in
    `crates/clarion-mcp/src/filigree.rs`; reads entity-association reverse
    lookups; transport sibling, not vendored.
  - **LLM providers** — pluggable via `clarion-core/src/llm_provider.rs`:
    `OpenRouterProvider` (HTTPS to `openrouter.ai/api/v1`),
    `CodexCliProvider` and `ClaudeCliProvider` (subprocess execution of
    `codex` / `claude` binaries), `RecordingProvider` (fixture replay).
  - **Wardline** — Python-side import probe only
    (`plugins/python/src/clarion_plugin_python/wardline_probe.py`):
    `importlib.import_module("wardline.core.registry")`, version-pin
    check against `[1.0.0, 2.0.0)` declared in `plugin.toml:54-56`.

- **Build/test tooling:**
  - **Pre-commit** (`.pre-commit-config.yaml`): ruff fix, ruff-format,
    mypy `--strict` on `plugins/python/{src,tests}`.
  - **Workspace lints** (`Cargo.toml:19-30`): `unsafe_code = "deny"`,
    clippy `pedantic = "warn"`, with three pragmatic allows.
  - **cargo-deny** policy at `deny.toml`.
  - **GitHub Actions** at `.github/workflows/{ci,release}.yml` (not read for
    contents, only existence).
  - **Scripts** under `scripts/`: `check-workspace-version-lockstep.py`,
    `check-python-ontology-version.py`, `check-migration-retirement.py`,
    `check-github-release-governance.py`, `b4-gate-run.sh`.

## 4. Entry Points

- **`clarion` binary** — `crates/clarion-cli/src/main.rs`; `clap` parser at
  `src/cli.rs:7`. Three subcommands:
  - `Install { force, path }` → `install::run(&path, force)`
    (`install.rs:1-50`) creates `.clarion/clarion.db`, `.clarion/config.json`,
    `.clarion/.gitignore`, and `<path>/clarion.yaml`.
  - `Analyze { path, config, allow_unredacted_secrets, … }` → runs on a
    multi-thread tokio runtime, performs `secret_scan` gate, then
    `analyze::run_with_options` (`analyze.rs:70`+).
  - `Serve { path, config }` → `serve::run(&path, config.as_deref())`
    (`serve.rs:20`).
- **`clarion serve`** is the MCP server. It supervises *two* concurrent
  servers in one process: an stdio JSON-RPC loop on a dedicated thread with
  a per-thread current-thread tokio runtime
  (`serve.rs:96-110, 126-130`), and the optional Axum HTTP read server on a
  second thread (`serve.rs:55-71, http_read.rs::spawn`). Both share one
  `ReaderPool`; identity is asserted via `Arc::ptr_eq` (`serve.rs:63-71`).
- **`clarion-plugin-fixture`** — `crates/clarion-plugin-fixture/src/main.rs`;
  test-only binary that speaks the JSON-RPC plugin protocol, returns one
  hard-coded `fixture:widget:demo.sample` entity per `analyze_file`, and
  honours an `CLARION_FIXTURE_EXCEED_RLIMIT_AS` env hook to provoke OOM
  paths (`main.rs:78, 137-178`).
- **Python plugin** — console script `clarion-plugin-python` defined in
  `pyproject.toml:33`; `clarion_plugin_python.__main__:main` (15 LOC at
  `__main__.py`) installs `stdout_guard` and delegates to
  `clarion_plugin_python.server.main` (`server.py` 296 LOC). Plugin manifest
  ships via wheel `shared-data` to `share/clarion/plugins/python/plugin.toml`
  (`pyproject.toml:38-44`) for the host's install-prefix discovery fallback.
- **Library entry** — `crates/clarion-core/src/lib.rs` re-exports a facade of
  domain types (`EntityId`, `Manifest`, `PluginHost`, `LlmProvider`,
  `LeafSummaryPromptInput`, etc.) per its module-doc "Re-export policy".
- **Storage library entry** — `crates/clarion-storage/src/lib.rs` exposes
  `Writer`, `ReaderPool`, the `WriterCmd` enum, cache helpers, and twenty-odd
  query functions.

## 5. Public Wire Surfaces

1. **Plugin JSON-RPC 2.0 protocol** (stdin/stdout per plugin subprocess).
   - Defined in `crates/clarion-core/src/plugin/protocol.rs` (875 LOC) +
     `transport.rs` (LSP-style `Content-Length: N\r\n\r\n<body>` framing).
   - Five methods: `initialize`, `initialized` (notification),
     `analyze_file`, `shutdown`, `exit` (notification).
   - `analyze_file` returns `{entities: [...], edges: [...], stats: {…}}`;
     entity shape carries `id, kind, qualified_name, source.{file_path,
     source_range, …}`; edges carry `kind, src_id, dst_id, confidence,
     properties` plus `unresolved_call_sites` for query-time inference.
   - Frame ceiling: `ContentLengthCeiling::DEFAULT` = 8 MiB
     (`limits.rs` per the ADR-021 §2b comment, also mirrored on the
     plugin side in `plugins/python/.../server.py:48`).
   - `initialize` returns
     `{name, version, ontology_version, capabilities}`; the host validates
     the returned ontology against `manifest.ontology.entity_kinds /
     edge_kinds` (host.rs module docs §Enforcement pipeline step 1).

2. **MCP stdio JSON-RPC server** (`clarion serve`).
   - Same `Content-Length` framing (`clarion-mcp/src/lib.rs` imports
     `clarion_core::plugin::{ContentLengthCeiling, Frame, TransportError}`).
   - `MCP_PROTOCOL_VERSION = "2025-11-25"` (`lib.rs:40`).
   - **19 tools** registered in `list_tools()` (`lib.rs:56-294` — corrected from this doc's initial "20"; see §4 of `02-subsystem-catalog.md` for the enumerated registry):
     `entity_at`, `project_status`, `analyze_start`, `analyze_status`,
     `analyze_cancel`, `find_entity`, `source_for_entity`, `entity_context`,
     `call_sites`, `callers_of`, `execution_paths_from`,
     `execution_paths_ranked`, `summary`, `summary_preview_cost`,
     `issues_for`, `orientation_pack`, `index_diff`, `neighborhood`,
     `subsystem_members`, plus one I have not independently counted past
     line 254 (claimed `grep -c 'ToolDefinition {'` = 20).
   - Backed by `ServerState` (built in `serve.rs:131`) holding a
     `ReaderPool`, optional summary-LLM `Writer` + `LlmProvider`, optional
     `FiligreeHttpClient`.

3. **HTTP read API** — Axum router in
   `crates/clarion-cli/src/http_read.rs:347-369`. Six routes:
   - `GET  /api/v1/files`          → `get_file` (line 670)
   - `POST /api/v1/files:resolve`  → `post_files_resolve` (line 887)
   - `POST /api/v1/files/batch`    → `post_files_batch` (line 815)
   - `GET  /api/v1/_capabilities`  → `get_capabilities` (line 1005;
     unprotected — outside the bearer middleware)
   - Plus two test-only routes inside `#[cfg(test)]` modules (`/x`, `/boom`)
     that are not part of the public surface.
   - Bearer-token middleware on the `protected` group
     (`http_read.rs:376 require_http_identity`); HMAC variant exists
     (`require_hmac_identity` line 405). Tower stack: `CatchPanic` →
     `HandleError` → 10s `TimeoutLayer` → `RequestBodyLimitLayer` →
     `LoadShedLayer` → `ConcurrencyLimitLayer(64)`.

4. **CLI** — `crates/clarion-cli/src/cli.rs` (64 LOC). `clap` derive macros.
   Flags of note on `analyze`: `--allow-unredacted-secrets` (requires
   `--confirm-allow-unredacted-secrets <TOKEN>` non-interactively),
   `--allow-no-plugins` for dry runs.

5. **Outbound HTTP — Filigree reverse-association lookup.**
   `crates/clarion-mcp/src/filigree.rs::FiligreeHttpClient` (`reqwest`
   blocking) calls Filigree's HTTP API for entity-association reverse
   lookup; request shape decoded into `EntityAssociationsResponse`
   (`filigree.rs:11-22`). Auth via `token_env` (`clarion.yaml:integrations.
   filigree.token_env`).

6. **Outbound HTTP — OpenRouter LLM** in `clarion-core/src/llm_provider.rs`
   (`OpenRouterProvider`); URL/attribution from `clarion.yaml:llm_policy.openrouter`.

7. **Subprocess providers** — `CodexCliProvider`, `ClaudeCliProvider` in the
   same file run external `codex` / `claude` binaries with stdin-piped
   prompts and structured output.

## 6. Candidate Subsystems

### clarion-core — `crates/clarion-core/`
- **LOC:** ~11669 src across 13 `.rs` files; 325 LOC integration test.
- **Source files (one-line roles):**
  - `src/lib.rs` (50) — facade re-exports per the documented "Re-export policy".
  - `src/entity_id.rs` (596) — 3-segment ID assembler + grammar (ADR-003/022 per code comment).
  - `src/llm_provider.rs` (2467) — `LlmProvider` trait, `OpenRouterProvider`, `CodexCliProvider`, `ClaudeCliProvider`, `RecordingProvider`, prompt builders, `CachingModel`.
  - `src/plugin/mod.rs` (52) — submodule wiring.
  - `src/plugin/protocol.rs` (875) — JSON-RPC envelopes + typed params/results.
  - `src/plugin/transport.rs` (569) — Content-Length framing.
  - `src/plugin/manifest.rs` (1119) — `plugin.toml` parser + validator.
  - `src/plugin/discovery.rs` (667) — `$PATH` scan + manifest lookup.
  - `src/plugin/host.rs` (2935) — supervisor, ontology/identity/jail/cap pipeline.
  - `src/plugin/host_findings.rs` (NOT measured separately) — finding subcodes.
  - `src/plugin/jail.rs` (~) — path-jail (`canonicalize` + `starts_with`).
  - `src/plugin/limits.rs` (572) — Content-Length / entity-cap / path-escape breakers + `RLIMIT_AS`/`RLIMIT_NOFILE`/`RLIMIT_NPROC` via `nix`.
  - `src/plugin/breaker.rs` (360) — crash-loop breaker (>3 crashes / 60s).
  - `src/plugin/mock.rs` (876) — `#[cfg(test)]` mock plugin for host unit tests.
- **Outbound deps:** `serde`, `serde_json`, `tempfile`, `thiserror`, `toml`, `tracing`, `nix`, `which`, `reqwest` (LLM HTTP). No internal Clarion crate deps.

### clarion-storage — `crates/clarion-storage/`
- **LOC:** ~3218 src; 4858 LOC tests.
- **Source files:**
  - `src/lib.rs` (43) — re-exports.
  - `src/writer.rs` (1074) — `Writer` actor, `mpsc::Sender<WriterCmd>` API, batch commits (`DEFAULT_BATCH_SIZE = 50`), `commits_observed` counter (`Arc<AtomicUsize>` per writer.rs:50 docs).
  - `src/reader.rs` — `ReaderPool` via `deadpool-sqlite`.
  - `src/query.rs` (1160) — read-side query helpers: `call_edges_from`, `subsystem_members`, `entity_at_line`, `find_entities`, `resolve_file_catalog_entry`, etc.
  - `src/schema.rs` — migration runner; embedded `0001_initial_schema.sql` (293 LOC SQL).
  - `src/pragma.rs` — WAL+`synchronous=NORMAL`+`busy_timeout=5000`+`wal_autocheckpoint=1000`+`foreign_keys=ON` PRAGMAs.
  - `src/commands.rs` — `WriterCmd` enum (`EntityRecord`, `EdgeRecord`, `FindingRecord`, `InferredCallEdgeRecord`, `RunStatus`).
  - `src/cache.rs` — LLM `SummaryCache`/`InferredEdgeCache` helpers.
  - `src/unresolved.rs` — `UnresolvedCallSiteRecord` + replace-by-caller.
  - `src/error.rs` — `StorageError`.
- **Outbound deps:** `clarion-core` (path dep), `rusqlite`, `deadpool-sqlite`, `tokio`, `serde`, `serde_json`, `blake3`, `tracing`, `thiserror`.

### clarion-cli — `crates/clarion-cli/`
- **LOC:** ~6790 src; 6394 LOC tests (5 test files + `wp1_e2e.rs` + `wp2_e2e.rs`).
- **Source files:**
  - `src/main.rs` (78) — binary entry, runtime construction, .env hygiene exclusion for `analyze`.
  - `src/cli.rs` (64) — clap definitions.
  - `src/install.rs` — `.clarion/` initialiser + `clarion.yaml` stub.
  - `src/analyze.rs` (2549) — analyze pipeline: discovery → spawn → walk → `analyze_file` per file → writer commands → clustering.
  - `src/serve.rs` (326) — `clarion serve` orchestrator (stdio MCP thread + HTTP thread).
  - `src/http_read.rs` (1532) — Axum HTTP read API, bearer + HMAC middleware, tower stack.
  - `src/clustering.rs` (510) — Leiden / weighted-components clustering over `xgraph::Graph`.
  - `src/config.rs` — `AnalyzeConfig` / `ClusteringConfig` YAML loader.
  - `src/instance.rs` — `InstanceId` UUID newtype, persisted to `.clarion/instance_id`.
  - `src/run_lifecycle.rs` — `runs` row lifecycle: `recover_preexisting_running_runs`, `begin_run`.
  - `src/secret_scan.rs` + `src/secret_scan/{anchors,baseline,files,findings}.rs` — pre-ingest secret-scan gate wrapping `clarion-scanner`.
  - `src/stats.rs` — `P95Accumulator` and stat helpers.
- **Outbound deps:** `clarion-core`, `clarion-mcp`, `clarion-scanner`, `clarion-storage` (all path deps); `anyhow`, `axum`, `blake3`, `clap`, `dotenvy`, `ignore`, `rusqlite`, `serde`, `serde_json`, `serde_norway`, `sha2`, `time`, `tokio`, `tower`, `tower-http`, `tracing`, `tracing-subscriber`, `uuid`, `xgraph`. Dev-only: `clarion-plugin-fixture`, `assert_cmd`, `tempfile`, `sha1`.

### clarion-mcp — `crates/clarion-mcp/`
- **LOC:** ~6595 src across 3 files; 2233 LOC integration test.
- **Source files:**
  - `src/lib.rs` (4703) — `ToolDefinition` list, `ServerState`, JSON-RPC dispatch (`handle_json_rpc`, `handle_tool_call`), stdio loop (`serve_stdio*`), in-process analyze supervisor for `analyze_{start,status,cancel}`.
  - `src/config.rs` (1600) — `McpConfig` YAML loader; LLM/Filigree/serve.http config; provider selection; validation rules (deprecated-provider rejection, Filigree port-conflict, etc.).
  - `src/filigree.rs` — `FiligreeHttpClient` + reverse-association lookup with `FiligreeLookup` trait; `reqwest::blocking`.
- **Outbound deps:** `clarion-core`, `clarion-storage` (path deps); `blake3`, `reqwest`, `rusqlite`, `serde`, `serde_json`, `serde_norway`, `thiserror`, `time`, `tokio`, `tracing`.

### clarion-scanner — `crates/clarion-scanner/`
- **LOC:** ~881 src across 4 files; 655 LOC tests.
- **Source files:**
  - `src/lib.rs` — `Detection`, `SecretCategory`, `HashedSecret` (detect-secrets-compatible SHA-1).
  - `src/patterns.rs` — `Scanner`, `PatternMeta` regex catalogue.
  - `src/entropy.rs` — `EntropyTuning`, high-entropy filter.
  - `src/baseline.rs` — `Baseline`, `BaselineEntry`, `SuppressionResult`, `load_baseline`.
- **Outbound deps:** `regex`, `serde`, `serde_norway`, `sha1`, `thiserror`. **No internal Clarion deps** — this is a pure scanner library.

### clarion-plugin-fixture — `crates/clarion-plugin-fixture/`
- **LOC:** 187 across 2 files.
- **Source files:**
  - `src/main.rs` (185) — minimal JSON-RPC plugin: returns one `fixture:widget:demo.sample` entity per `analyze_file`; honours `CLARION_FIXTURE_EXCEED_RLIMIT_AS` env var to exercise the host's `RLIMIT_AS` OOM-kill path via `nix::sys::mman::mmap_anonymous` (the only `unsafe` block in the crate).
  - `src/lib.rs` — trivial.
- **Outbound deps:** `clarion-core` (path); `serde_json`; `nix` (unix-only, with `mman`, `signal` features).

### plugins/python — `plugins/python/`
- **LOC:** 3028 src across 11 modules; 3440 LOC tests across 9 files.
- **Source files (`src/clarion_plugin_python/`):**
  - `__init__.py` (3) — package metadata.
  - `__main__.py` (15) — entry point, installs `stdout_guard` then calls `server.main()`.
  - `server.py` (296) — JSON-RPC server loop, dispatch for `initialize`/`initialized`/`analyze_file`/`shutdown`/`exit`; constants `ONTOLOGY_VERSION = "0.6.0"`, `MAX_CONTENT_LENGTH = 8 MiB`, `MAX_FILES_PER_PYRIGHT_SESSION = 25`.
  - `extractor.py` (932) — AST walker emitting module/class/function entities plus `imports`/`calls`/`references` candidate edges.
  - `pyright_session.py` (1406) — wraps `pyright-langserver` over `subprocess` + `select`; type-resolved call & reference resolution.
  - `call_resolver.py` (65) — `CallResolutionResult`, `CallsRawEdge`, `Finding`, `UnresolvedCallSite` dataclasses.
  - `reference_resolver.py` (70) — `ReferenceResolutionResult`, `ReferenceSite`.
  - `entity_id.py` (75) — Python-side mirror of the 3-segment ID assembler.
  - `qualname.py` (48) — L7 qualname reconstruction (dotted module + `__qualname__`).
  - `stdout_guard.py` (62) — replaces `sys.stdout` so accidental writes don't corrupt JSON-RPC frames (called from `__main__.py`).
  - `wardline_probe.py` (56) — fail-soft `importlib` probe for `wardline.core.registry` + version-pin gate against `[1.0.0, 2.0.0)`.
- **Outbound deps (runtime):** `packaging>=24`, `pyright==1.1.409`. **No HTTP, no network at all in the plugin process.**

## 7. Cross-Cutting Concerns Observed

- **Error handling:**
  - Library crates use `thiserror` for typed error enums
    (`clarion-storage/src/error.rs`, `clarion-mcp/src/filigree.rs:31-50`,
    `clarion-core/src/plugin/limits.rs::BreakerState`, etc.).
  - Binary code uses `anyhow` for top-level error propagation
    (`crates/clarion-cli/src/main.rs:13`,
    `crates/clarion-cli/src/serve.rs:8`).
  - The `runs` table has a `recover_preexisting_running_runs` step
    (`run_lifecycle.rs:5-28`) that marks any leftover `status='running'`
    rows as `failed` on next start — explicit crash-recovery discipline.

- **Logging / tracing:** `tracing` + `tracing-subscriber` with
  `EnvFilter::try_from_default_env()`; defaults to `info` if no `RUST_LOG`
  (`main.rs:73`). HTTP read API installs a *separate* `Dispatch` writing to
  stderr to keep panic traces off the MCP stdout
  (`http_read.rs:31-38`).

- **Async runtime:**
  - `analyze` builds a **multi-thread** runtime (`main.rs:36-38`).
  - `serve`'s MCP stdio loop runs a **current-thread** runtime on a
    dedicated OS thread (`serve.rs:126-130`), with the HTTP server running
    in its own thread (`http_read.rs::spawn`). One `ReaderPool` is shared
    across both and verified by `Arc::ptr_eq` (`serve.rs:63-71`).

- **Configuration loading:**
  - `clarion.yaml` is parsed by `serde_norway` (YAML). `clarion-mcp/src/config.rs::McpConfig::from_yaml_str` runs an alias-collision check (`llm` vs `llm_policy`) then `validate()` enforces invariants such as "no Anthropic provider", "Filigree actor non-blank when enabled", "Filigree port-conflict ban", "no zero-port".
  - `clarion-cli/src/config.rs` separately parses analyze-time config (clustering, etc.).
  - Note `analyze` deliberately **does not** load `.env` files
    (`main.rs:23-25, 19-22`): `.env` is treated as in-tree source and
    scanned by the secret scanner before plugin spawn.

- **Schema / migrations:**
  - Embedded via `include_str!` in
    `clarion-storage/src/schema.rs:18-22`. One migration so far:
    `0001_initial_schema.sql` (293 lines of SQL). Tracking table:
    `schema_migrations`.
  - PRAGMA invariant check: `apply_write_pragmas` rejects connection if
    `journal_mode` is not `WAL` after `PRAGMA journal_mode = WAL`
    (`pragma.rs:17-21`).

- **Security boundaries:**
  - **Plugin jail** (`crates/clarion-core/src/plugin/jail.rs`): every
    `file_path` from a plugin response is `canonicalize()`d and asserted
    `starts_with(project_root)`. Violations trip the `PathEscapeBreaker`
    (>10 escapes / 60s → kill).
  - **Resource caps** (`limits.rs:11-15` table): `ContentLengthCeiling` 8 MiB,
    entity cap 500k per run, `RLIMIT_AS` via
    `CommandExt::pre_exec`-applied `setrlimit(2)` (the *only* allowed
    `unsafe` in the workspace, justified at `Cargo.toml:21-24`).
  - **Crash-loop breaker** (`breaker.rs:1-14`): >3 plugin crashes / 60s →
    refuse further spawns.
  - **Pre-ingest secret scan** (`clarion-cli/src/secret_scan.rs:1-8`):
    runs before any plugin spawn; analyze aborts with exit code 78
    unless `--allow-unredacted-secrets` is set with the right confirm
    token; emits structured findings written to storage.
  - **HTTP bearer auth + HMAC option**
    (`http_read.rs:376-510`); `/api/v1/_capabilities` is the only
    unprotected route. `LoadShedLayer` + `ConcurrencyLimitLayer(64)`
    + 10s `TimeoutLayer` for DoS resistance.
  - **stdout guard on the Python plugin side**
    (`plugins/python/src/clarion_plugin_python/stdout_guard.py`) — any
    accidental `print()` is redirected so JSON-RPC framing is never
    corrupted.

- **Test harness shape:**
  - Per-crate `tests/*.rs` integration tests + inline `#[cfg(test)]` units.
  - Cross-language fixture at `fixtures/entity_id.json` consumed by both
    `tests/test_entity_id.py` and (presumably, not verified) a Rust test.
  - Bash-level e2e in `tests/e2e/` (4 scripts).
  - Performance harness in `tests/perf/b8_scale_test/` with timestamped
    results directories (latest: `2026-05-18T1138Z-phase3/`).

## 8. Test Corpus Shape

**Per-crate Rust tests** (`crates/*/tests/*.rs`):
- `clarion-core/tests/host_subprocess.rs` (325) — T1 happy-path subprocess
  integration; spawns the `clarion-plugin-fixture` binary via `PluginHost::spawn`.
- `clarion-storage/tests/writer_actor.rs` (2440) — round-trip insert,
  per-N-batch commit cadence, `FailRun` rollback.
- `clarion-storage/tests/schema_apply.rs` (901) — migration 0001 produces
  every table, index, trigger.
- `clarion-storage/tests/query_helpers.rs` (1124) — read-side query helpers.
- `clarion-storage/tests/reader_pool.rs` — reader-pool concurrency.
- `clarion-storage/tests/llm_cache.rs` — LLM cache helper tests.
- `clarion-cli/tests/install.rs` — `clarion install` integration.
- `clarion-cli/tests/analyze.rs` (1456) — Sprint-1 `clarion analyze`.
- `clarion-cli/tests/serve.rs` (3075) — MCP serve over real sockets/pipes.
- `clarion-cli/tests/secret_scan.rs` (917, `#![cfg(unix)]`) — secret-scan gate.
- `clarion-cli/tests/wp1_e2e.rs` — README §3 demo-script smoke.
- `clarion-cli/tests/wp2_e2e.rs` (606) — full walking-skeleton pipeline.
- `clarion-mcp/tests/storage_tools.rs` (2233) — storage-backed MCP tool tests.
- `clarion-scanner/tests/scanner.rs` (655) — pattern + baseline tests.

**Python plugin tests** (`plugins/python/tests/`, 9 files, 3440 LOC):
`test_entity_id.py`, `test_extractor.py`, `test_package.py`,
`test_pyright_session.py`, `test_qualname.py`, `test_round_trip.py`
(plugin analyses its own source), `test_server.py` (subprocess JSON-RPC),
`test_stdout_guard.py`, `test_wardline_probe.py`.

**End-to-end shell scripts** (`tests/e2e/`):
- `sprint_1_walking_skeleton.sh` — `install` → `analyze` → sqlite assertions
  on entities, edges, references.
- `sprint_2_mcp_surface.sh` — analyze + `clarion serve`, sends framed
  MCP JSON-RPC for "eight" navigation tools (note: actual `list_tools()` is
  now 19 — script may exercise a subset), with a local HTTP fake of
  Filigree's reverse-association route.
- `phase3_subsystems.sh` — clustering: subsystem entities, membership
  edges, deterministic signature across clean project copies, MCP
  `subsystem_members` tool.
- `wp5_secret_scan.sh` — pre-ingest scanner smoke.
- Plus `external-operator-smoke.md` (procedure doc, not a script).

**Perf corpus**: `tests/perf/b8_scale_test/` with a `derive-elspeth-corpus.sh`
script, `driver.py`, `test_driver.py`, and persisted result directories
(latest `2026-05-18T1138Z-phase3/`). `tests/perf/elspeth_mini/` is a
~150-file Python corpus checked in to seed perf runs.

## 9. Open Questions (For Per-Subsystem Phase)

1. **Writer-actor concurrency** — `writer.rs:50` exposes `commits_observed`
   as an `Arc<AtomicUsize>` and explicitly says "Read this field before
   dropping the Writer". What invariants does the actor enforce around
   `BeginRun` / `FailRun` / `CommitRun`? How does it interact with
   `recover_preexisting_running_runs` on next start?
2. **Two-thread `serve` topology** — why are MCP stdio and HTTP read each
   on a dedicated thread with their *own* tokio runtime configurations
   (`current_thread` vs the implicit Axum runtime), and what guarantees the
   `Arc::ptr_eq` check buys at `serve.rs:63-71`?
3. **Plugin jail bypasses** — `jail.rs` follows symlinks. Are there file
   classes (e.g. symlinks in the project root, hard links across
   filesystems, special files in `/proc`) that the current `canonicalize +
   starts_with` rule under-handles?
4. **Crash-loop & path-escape thresholds** — `breaker.rs` hard-codes
   >3 crashes / 60s; `limits.rs` PathEscapeBreaker hard-codes >10 / 60s.
   Are these surfaced through `clarion.yaml` or are they baked in?
5. **Identity check correctness** — `host.rs` step 2 recomputes
   `entity_id(plugin_id, kind, qualified_name)` and compares against the
   returned string. What happens if the plugin emits an entity ID whose
   `canonical_qualified_name` contains a colon (forbidden) vs unicode edge
   cases (NFC/NFD)?
6. **MCP tool inventory drift** — `list_tools()` returns 19 tools; the
   `sprint_2_mcp_surface.sh` script comments mention "eight". Does the
   e2e cover the full surface, or only a Sprint-2 subset?
7. **LLM provider sandboxing** — `ClaudeCliProvider` and `CodexCliProvider`
   shell out to local CLI binaries. How are timeouts, stdout
   capture, and permission modes (e.g. Codex `sandbox: read-only` in
   `clarion.yaml`) actually enforced inside `llm_provider.rs`?
8. **HTTP auth modes** — bearer is wired (`require_http_identity` line 376)
   and HMAC handler exists (`require_hmac_identity` line 405). Which is on
   today, how is the secret material loaded, and is HMAC behind a feature
   gate or a config setting?
9. **Schema evolution path** — only one migration is in tree
   (`0001_initial_schema.sql`, 293 lines). What is the migration-author
   workflow, and how does `scripts/check-migration-retirement.py` gate it?
10. **Clustering reproducibility** — `clustering.rs` uses `xgraph`'s Leiden
    implementation with a seed in `ClusterConfig`. Does
    `phase3_subsystems.sh` actually exercise byte-for-byte determinism, or
    only signature-level determinism via `cluster_hash`?
11. **Wardline coupling shape** — the plugin imports `wardline.core.registry`
    only inside `wardline_probe.py` (the manifest declares
    `wardline_aware = true` in `plugin.toml:24`). Does any production
    code path *consume* the probe result, or is it purely declarative
    at v1.0?

## 10. Confidence Statement

| Claim                                                                                  | Confidence | Evidence |
|----------------------------------------------------------------------------------------|------------|----------|
| Workspace has six Rust crates                                                          | High       | `Cargo.toml:3-10` |
| `clarion` binary has three subcommands: install / analyze / serve                       | High       | `crates/clarion-cli/src/cli.rs:12-62` |
| MCP server exposes 19 tools                                                            | High       | `ToolDefinition` enumeration of `crates/clarion-mcp/src/lib.rs:56-257` = 19 (the `grep -c 'ToolDefinition {'` shortcut counted the struct declaration plus 19 instances; corrected during catalog validation) |
| HTTP read API has 4 functional routes + 2 test-only                                    | High       | `crates/clarion-cli/src/http_read.rs:347-356,1374-1433` |
| MCP protocol version = `"2025-11-25"`                                                  | High       | `crates/clarion-mcp/src/lib.rs:40` |
| Plugin JSON-RPC framing is LSP-style `Content-Length`                                  | High       | `crates/clarion-core/src/plugin/transport.rs:1-9` |
| SQLite is WAL with `synchronous=NORMAL`, `busy_timeout=5000`                           | High       | `crates/clarion-storage/src/pragma.rs:17-30` |
| Writer-actor is a single tokio task owning one write connection                        | High       | `crates/clarion-storage/src/writer.rs:1-12, lib.rs:1-5` |
| Plugin discovery is `$PATH` scan for `clarion-plugin-*` with neighbor + install-prefix fallback | High | `crates/clarion-core/src/plugin/discovery.rs:1-40` |
| Python plugin uses `pyright==1.1.409` as a subprocess language server                  | High       | `plugins/python/pyproject.toml:21`, `pyright_session.py:7-11` |
| `unsafe_code = "deny"` workspace-wide with one documented exception in plugin host    | High       | `Cargo.toml:20-25` + `crates/clarion-core/src/plugin/host.rs` module doc §Memory limit |
| Crash-loop breaker triggers >3 crashes in 60s                                          | High       | `crates/clarion-core/src/plugin/breaker.rs:1-7` |
| `clarion serve` runs MCP stdio + HTTP in two threads sharing one `ReaderPool`          | High       | `crates/clarion-cli/src/serve.rs:53-80` |
| Pre-ingest secret scan blocks analyze with exit code 78 by default                     | High       | `crates/clarion-cli/src/secret_scan.rs:1-12`, `main.rs:42-47` |
| Wardline integration is import-probe-only on the Python side                           | High       | `plugins/python/src/clarion_plugin_python/wardline_probe.py` (whole file) |
| `clarion-scanner` is dependency-free of other Clarion crates                           | High       | `crates/clarion-scanner/Cargo.toml` (no `clarion-*` paths) |
| Filigree is reached via outbound `reqwest`; no inbound Filigree surface in this repo   | High       | `crates/clarion-mcp/src/filigree.rs:1-12` |
| `clustering.rs` uses `xgraph`'s Leiden community detection                             | High       | `crates/clarion-cli/src/clustering.rs:5-6` |
| Single schema migration so far (`0001_initial_schema.sql`)                             | High       | `ls crates/clarion-storage/migrations/`, `schema.rs:18-22` |
| Total Rust source ~29k LOC                                                             | Medium     | `wc -l` per crate src trees; line counts include comments + blanks |
| Total Python source 3028 LOC                                                           | High       | `wc -l plugins/python/src/clarion_plugin_python/*.py` |
| `list_tools()` includes exactly the 19 tool names listed in §5                         | High       | Direct enumeration of the `ToolDefinition` registry during catalog validation. The "one more past line 254" was a miscount — the literal `ToolDefinition {` at the struct declaration leaked into the original `grep -c`. |
| HMAC inbound auth is wired but possibly behind a config gate                           | Medium     | `require_hmac_identity` exists at `http_read.rs:405` but I did not trace what selects it vs bearer |
| `analyze.rs` calls `Command::new` on the plugin path                                    | Medium     | Inferred from `analyze.rs` module doc + `clarion_core::AcceptedEntity` imports; did not read the spawn line directly. The `Command::new` call lives inside `PluginHost::spawn` in `host.rs`, not in `analyze.rs`. |
| Cross-language fixture `fixtures/entity_id.json` is consumed by Rust tests too         | Low        | Only Python consumer confirmed (`test_entity_id.py` per its docstring); Rust side asserted by spec language but not greped in this pass |

---

## Confidence Assessment (overall)

High overall. The §1–§8 claims are mostly directly traceable to a file:line
or to a `Cargo.toml` / `pyproject.toml` field. The discovery sweep read or
sampled every src `.rs` in `clarion-core/src/plugin/` and the `lib.rs`/entry
of every other crate; every Python module head; every `Cargo.toml`; the
HTTP router definition; the MCP `list_tools()` table; the writer/pragma/schema
heads; the e2e scripts; and `plugin.toml`.

## Risk Assessment

- **Greatest risk to downstream catalog work:** the two giant files
  (`clarion-mcp/src/lib.rs` at 4703 LOC and `clarion-core/src/plugin/host.rs`
  at 2935 LOC) were only sampled. Per-subsystem explorers should read them
  end-to-end before claiming completeness on MCP tooling or host
  enforcement semantics.
- **Resolved during validation:** the tool count was originally reported as
  20 in this doc, derived via `grep -c 'ToolDefinition {'`. Catalog
  validation enumerated the actual `ToolDefinition` registry at
  `clarion-mcp/src/lib.rs:56-257` and confirmed **19** distinct production
  tools. The grep counted the struct declaration plus 19 instances.
- **External dependency surface I did not read:** the actual `Command::new`
  for plugin subprocess spawn lives inside `host.rs` (sampled only at the
  module doc), not `analyze.rs`. Behaviour around `pre_exec`, descriptor
  closing, and env-var passing was not traced beyond the module-doc
  promise.

## Information Gaps

- No reading of the migration SQL (`0001_initial_schema.sql`) — schema
  shape (tables, indexes, FK graph) is therefore an open question.
- No tracing of `analyze::run_with_options`'s control flow past its
  imports + signature — concurrency model of "Pattern A buffering" not
  verified, only quoted from the module doc.
- `target/` build artifacts not inspected; binary sizes / link surface
  unknown.
- `.github/workflows/ci.yml` contents not read; only filename observed.
- Wardline-side code: not in this repo; no claims made beyond what the
  Python `wardline_probe` proves about imports.

## Caveats

- This document deliberately reports the system as evidenced by code,
  not by intent. Where the code names ADRs, work packages, or sprints in
  comments, the IDs are quoted but not validated against the (un-read)
  design docs.
- LOC figures include comments and blanks; they are coarse "depth
  indicators" only.
- The Python `__pycache__/` count in the file tree was suppressed from
  module counts but does indicate the test suite has been executed on
  this machine.
