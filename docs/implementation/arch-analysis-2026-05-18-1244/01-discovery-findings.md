# 01 — Discovery Findings

**Repository:** `/home/john/clarion`
**Branch:** `sprint-2/b8-scale-test` (11 commits ahead of `main`)
**Tags present:** `v0.1-sprint-1`, `v0.1-sprint-2`
**Discovery pass run:** 2026-05-18
**Methodology:** SME Agent Protocol — metadata → structure → routers → sampling → quantitative. No depth-reads on sibling-subsystem files; that's the next pass.

---

## 1. Repository organisation

### 1.1 Top-level layout (Confidence: High — `ls /home/john/clarion`)

```
/home/john/clarion
├── Cargo.toml          workspace root (5-member virtual workspace)
├── Cargo.lock
├── CLAUDE.md           project doctrine + filigree workflow
├── rust-toolchain.toml
├── rustfmt.toml
├── clippy.toml
├── deny.toml           cargo-deny config (ADR-023 floor)
├── crates/             5 Rust crates (workspace members)
├── plugins/python/     Python plugin (installed via `pip install -e plugins/python[dev]`)
├── tests/              cross-crate e2e + perf harnesses
│   ├── e2e/            two bash scripts: Sprint-1 walking skeleton + Sprint-2 MCP surface
│   └── perf/           B.5 reference-scale smoke + B.8 scale-test harness + result snapshots
├── fixtures/           single file — `entity_id.json` (cross-language L2 parity proof)
├── scripts/            (not deep-read in this pass)
├── docs/               doctrine + design + ADRs + implementation plans (79 .md files)
└── target/             cargo build artifacts (gitignored)
```

### 1.2 Organisation principle (Confidence: High — `Cargo.toml` workspace + crate-name semantics)

Clarion is organised as a **plugin-extensible monolith with a layered Rust core**:

- **Layer-by-crate**: `clarion-core` (domain types + plugin host) → `clarion-storage` (SQLite persistence) → `clarion-mcp` (read surface) → `clarion-cli` (binary glue). The CLI depends transitively on every other crate; `clarion-mcp` and `clarion-storage` depend on `clarion-core`; `clarion-storage` does not depend on `clarion-mcp`. No circular references at the crate level.
- **Plugin-extensible**: language plugins are *out-of-process subprocesses* spoken to over Content-Length-framed JSON-RPC. `crates/clarion-plugin-fixture/` is a Rust test plugin; `plugins/python/` is the real Python plugin. This is enforced by ADR-021 (plugin authority — hybrid) and ADR-022 (core/plugin ontology ownership).
- **Per-product documentation**: `docs/clarion/` is product-scoped; `docs/suite/` is Loom-federation-scoped; `docs/implementation/sprint-{1,2}/` is execution-scoped. No documentation lives next to source modules.

The principle "the core never invents kinds" (ADR-022) is the load-bearing organisational rule: the plugin owns segment 1 (plugin_id) and segment 3 (canonical qualified name) of every entity ID; the core owns segment 2 (kind) only at the schema level.

---

## 2. Technology stack

### 2.1 Rust toolchain (Confidence: High — `rust-toolchain.toml` + `Cargo.toml`)

- **Edition**: 2024 (workspace-wide)
- **MSRV**: 1.85 (`Cargo.toml:8`)
- **Resolver**: 3
- **Lint floor** (workspace lints, `Cargo.toml:13–22`):
  - `unsafe_code = "deny"` (downgraded from `forbid` per documented exception: `CommandExt::pre_exec` calling `setrlimit(2)` in the forked plugin child)
  - `clippy::pedantic = warn` at `priority = -1`, with three pragmatic allows: `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`

### 2.2 Workspace dependencies (Confidence: High — `Cargo.toml` `[workspace.dependencies]`)

Heavy hitters by responsibility:

- **Async runtime**: `tokio 1` (`rt-multi-thread, macros, sync, time`)
- **Storage**: `rusqlite 0.31` (bundled), `deadpool-sqlite 0.8` (reader pool per ADR-011)
- **HTTP**: `reqwest 0.12` (blocking + JSON + `rustls-tls-native-roots`; no native OpenSSL)
- **Serialisation**: `serde 1`, `serde_json 1`, `toml 0.8`, `serde_norway 0.9` (YAML — replaces unsound `serde_yaml`, see commit `9ffc5c8`)
- **CLI / errors**: `clap 4` (derive), `anyhow 1`, `thiserror 1`
- **Hashing / IDs**: `blake3 1.8.5`, `uuid 1` (v4)
- **Observability**: `tracing 0.1` + `tracing-subscriber 0.3` (`env-filter`)
- **POSIX limits**: `nix 0.28` (`resource` feature only) — used by `apply_prlimit_as` / `apply_prlimit_nofile_nproc`
- **Configuration helper**: `dotenvy 0.15` (loaded in `main.rs` before tracing init)
- **Test scaffolding**: `assert_cmd 2`, `tempfile 3`

### 2.3 Python plugin toolchain (Confidence: High — `plugins/python/pyproject.toml`)

- **Build backend**: `hatchling`
- **Python**: `>=3.11`, targets 3.11 + 3.12
- **Runtime deps** (intentionally tiny — the plugin is a stdlib-first ast walker):
  - `packaging>=24` — version-range comparison for Wardline pin
  - `pyright==1.1.409` — pinned exact-version for cross-reference resolution (`pyright_session.py`, the largest Python file at 1251 lines)
- **Dev deps**: `pytest 8+`, `pytest-cov 5+`, `ruff 0.6+`, `mypy 1.11+`, `pre-commit 3.8+`
- **Lint/format/typecheck**: `ruff select = ["ALL"]` with pragmatic ignores (D, COM812, ISC001, CPY, ANN401, TRY003); `mypy --strict`; `max-complexity = 15` to match Rust clippy
- **Script entry**: `clarion-plugin-python = "clarion_plugin_python.__main__:main"`
- **Manifest packaging**: hatch shared-data routes `plugin.toml` → `share/clarion/plugins/python/plugin.toml` so the core's L9 PATH discovery finds it.

### 2.4 Plugin manifest (`plugins/python/plugin.toml`)

The plugin declares its capabilities at install time (Confidence: High — full file read):

- `plugin_id = "python"`, `protocol_version = "1.0"`
- `expected_max_rss_mb = 2048` (clamped against core 2 GiB ceiling per ADR-021 §2b)
- `expected_entities_per_file = 5000`
- `wardline_aware = true`, `reads_outside_project_root = false`
- `[ontology] entity_kinds = ["function", "class", "module"]`, `edge_kinds = ["contains", "calls", "references"]`, `ontology_version = "0.5.0"` (B.5* MINOR bump per ADR-027)
- `rule_id_prefix = "CLA-PY-"` (ADR-022 grammar)
- Wardline pin: `min_version = "1.0.0", max_version = "2.0.0"`

---

## 3. Entry points

### 3.1 Single binary: `clarion` (Confidence: High — `crates/clarion-cli/src/main.rs:1–25`)

```rust
match cli.command {
    Command::Install { force, path } => install::run(&path, force),
    Command::Analyze { path } => block_on(analyze::run(path)),
    Command::Serve { path, config } => serve::run(&path, config.as_deref()),
}
```

Three CLI subcommands (`crates/clarion-cli/src/cli.rs:1–45`):

- `clarion install [--path PATH] [--force]` — initialise `.clarion/clarion.db` against the current schema migration.
- `clarion analyze [PATH]` — discover plugins via L9 `$PATH` convention, walk source tree, spawn plugin processes, persist entities/edges via writer-actor.
- `clarion serve [--path PATH] [--config PATH]` — **new in Sprint 2** — run the MCP stdio server (`serve.rs:1–90`). Reads `clarion.yaml`, wires the LLM provider (OpenRouter, Recording, or disabled), wires the optional Filigree HTTP client, and dispatches to `clarion_mcp::serve_stdio_with_state_on_runtime`.

A `.env` file is loaded from CWD or any ancestor *before* tracing init (commit `dc9bf41`); existing process env vars win per dotenvy default.

### 3.2 Plugin process entry: `clarion-plugin-python`

`plugins/python/src/clarion_plugin_python/__main__.py` (15 lines, all of it shown):

```python
from clarion_plugin_python.server import main
if __name__ == "__main__":
    sys.exit(main())
```

`server.py` (285 lines) speaks the L4 five-method JSON-RPC protocol: `initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`. `MAX_CONTENT_LENGTH = 8 MiB` matches the host's ADR-021 §2b ceiling. `stdout_guard.py` (62 lines) replaces stdout with a strict transport channel so stray `print()` cannot corrupt the wire.

### 3.3 Test fixture plugin: `clarion-plugin-fixture`

`crates/clarion-plugin-fixture/src/main.rs` (128 lines) — Rust binary that speaks the same L4 protocol as the Python plugin; consumed only by `wp2_e2e` integration tests.

### 3.4 Integration-test entry points

- `tests/e2e/sprint_1_walking_skeleton.sh` — runs the README §3 demo verbatim; asserts SQLite returns Python module/function entities, source ranges, content hashes, and resolved+ambiguous call edges and references.
- `tests/e2e/sprint_2_mcp_surface.sh` — exists (not deep-read this pass); presumably exercises the seven MCP tools.
- `tests/perf/b8_scale_test/driver.py` — 816-line scale-test harness against the `elspeth` corpus (and `tests/perf/elspeth_mini/`).

---

## 4. Subsystem identification

The 6-candidate hypothesis is **confirmed**. Refined per crate evidence below.

### Subsystem A — `clarion-core` (~3 100 LOC actual code, 10 686 LOC including tests)

- **Location**: `crates/clarion-core/src/`
- **One-sentence responsibility**: Owns the canonical entity-ID format, the plugin host (subprocess supervisor + JSON-RPC peer), the LLM-provider abstraction, and the jail/limits ceilings that all other crates use.
- **Primary modules** (Confidence: High — `ls` + per-file LOC):
  - `entity_id.rs` (610 LOC) — three-segment ID assembler (`{plugin_id}:{kind}:{canonical_qualified_name}`); cross-validated against `fixtures/entity_id.json` (L2 parity proof).
  - `llm_provider.rs` (948 LOC) — `LlmProvider` trait, `OpenRouterProvider`, `RecordingProvider` (test-mode), prompt templates for leaf-summary and inferred-calls (`LEAF_SUMMARY_PROMPT_TEMPLATE_ID`, `INFERRED_CALLS_PROMPT_VERSION`).
  - `plugin/` submodule (~7 400 LOC across 9 files):
    - `host.rs` (3126 LOC) — supervisor; the single largest file in the codebase
    - `manifest.rs` (1508 LOC) — `plugin.toml` parser + validator (ADR-021/ADR-022)
    - `protocol.rs` (846 LOC) — JSON-RPC 2.0 typed envelopes
    - `mock.rs` (897 LOC, `#[cfg(test)]`) — in-process mock plugin for unit tests
    - `discovery.rs` (637 LOC) — `$PATH` scanning for `clarion-plugin-*` executables
    - `transport.rs` (568 LOC) — Content-Length framing
    - `limits.rs` (552 LOC) — entity-cap, frame-oversize, RSS, prlimit application
    - `breaker.rs` (360 LOC) — crash-loop circuit breaker (ADR-002 §UQ-WP2-10)
    - `jail.rs` (253 LOC) — path-jail enforcement (ADR-021 §2a)
- **External interface**: Rust-only — re-exports a curated facade through `lib.rs` (47 lines; see commit `ticket clarion-29acbcd042` policy note). Implementation types stay accessible via `clarion_core::plugin::transport::*` and siblings.

### Subsystem B — `clarion-storage` (~1 950 LOC actual code, 5 453 LOC with tests)

- **Location**: `crates/clarion-storage/src/`
- **One-sentence responsibility**: Writer-actor + reader-pool over a single SQLite database at `.clarion/clarion.db`, with all writes funnelled through one `tokio::task` per ADR-011.
- **Modules**:
  - `writer.rs` (817 LOC) — writer-actor command loop; owns the sole write `rusqlite::Connection`; enforces edge contract (`enforce_edge_contract`, called out at `writer.rs:411` in ADR-031).
  - `query.rs` (569 LOC) — read-side helpers for MCP tools (graph navigation: `call_edges_from`, `contained_entity_ids`, `entity_at_line`, `find_entities`, etc.).
  - `commands.rs` (183 LOC) — `WriterCmd`, `EntityRecord`, `EdgeRecord`, `InferredCallEdgeRecord`, `RunStatus` — typed enums at the command boundary.
  - `cache.rs` (251 LOC) — `SummaryCacheKey` (5-tuple per ADR-007), `InferredEdgeCacheKey`, `summary_cache_lookup`, `inferred_edge_cache_lookup`.
  - `unresolved.rs` (50 LOC) — unresolved call-site bookkeeping (B.4*).
  - `schema.rs` (118 LOC) — migration runner.
  - `pragma.rs` (45 LOC) — PRAGMA block (WAL, busy_timeout, etc.).
  - `reader.rs` (88 LOC) — `deadpool-sqlite` pool wrapper.
  - `error.rs` (48 LOC) — `StorageError` taxonomy.
- **Schema**: single migration at `crates/clarion-storage/migrations/0001_initial_schema.sql` (289 lines). Per the migration's own comment block, the file is edit-in-place under ADR-024 until an external operator builds a `.clarion/clarion.db` from a published build. The 2026-05-18 edits added `CHECK` constraints on closed core-owned vocabularies (`findings.{kind,severity,status}`, `runs.status`) per ADR-031; `entities.kind` and `edges.kind` deliberately do *not* gain CHECKs because their vocabularies are plugin-extensible.
- **External interface**: `WriterCmd` enum + `ReaderPool` + query helpers, exposed via `lib.rs` re-exports (40 LOC).

### Subsystem C — `clarion-mcp` (~3 200 LOC actual code, 4 917 LOC with tests) — *Sprint-2 new*

- **Location**: `crates/clarion-mcp/src/`
- **One-sentence responsibility**: Speaks MCP protocol revision `2025-11-25` over stdio; serves seven storage-backed read tools to consult-mode LLM agents; integrates the LLM provider for on-demand summaries and inferred call edges; integrates Filigree as an enrichment source.
- **Modules**:
  - `lib.rs` (2617 LOC) — `ServerState`, tool dispatch, the seven `ToolDefinition`s, dispatch glue to `clarion-storage` query helpers and `clarion-core::LlmProvider`. Massive single file; could plausibly subdivide.
  - `config.rs` (352 LOC) — `McpConfig`, `LlmConfig`, `ProviderSelection`, `select_provider_with_env`. YAML via `serde_norway` (the safe `serde_yaml` replacement).
  - `filigree.rs` (238 LOC) — `FiligreeHttpClient`, `EntityAssociation`, `EntityAssociationsResponse`, `FiligreeLookup` trait (allows mocking).
- **Seven tools** (from `lib.rs::list_tools`):
  1. `entity_at` — innermost entity containing (file, line)
  2. `find_entity` — FTS search on id/name/short_name/summary
  3. `callers_of` — incoming call edges; default confidence `resolved`
  4. `execution_paths_from` — bounded calls-only paths; max_depth ≤ 8, default 3
  5. `summary` — on-demand cached leaf summary (ADR-030 narrows to leaf scope for v0.1)
  6. `issues_for` — Filigree enrichment (enrich-only per Loom doctrine; unavailable envelope on Filigree downtime)
  7. `neighborhood` — one-hop graph view (callers, callees, container, contained, references)
- **External interface**: MCP stdio protocol; consumed by LLM agent clients via `clarion serve`.

### Subsystem D — `clarion-cli` (~1 740 LOC actual code, 3 275 LOC with tests)

- **Location**: `crates/clarion-cli/src/`
- **One-sentence responsibility**: Glue binary; `clap`-driven subcommand dispatch (`install`, `analyze`, `serve`) wiring `clarion-core` (plugin host) + `clarion-storage` (writer-actor + pool) + `clarion-mcp` (server).
- **Modules**:
  - `main.rs` (33 LOC) — entrypoint; loads `.env`, parses CLI, dispatches.
  - `cli.rs` (45 LOC) — `clap` derive structs.
  - `analyze.rs` (1436 LOC) — orchestrates plugin discovery, source-tree walk, plugin handshake, per-file dispatch, writer-actor flushing, crash-loop accounting, error mapping to `FailRun` vs `SkippedNoPlugins`.
  - `serve.rs` (136 LOC) — MCP server wiring (LLM provider build, Filigree client build, runtime + reader pool + writer wiring, stdio loop).
  - `install.rs` (168 LOC) — `.clarion/` bootstrap + migration apply.
  - `stats.rs` (small) — `P95Accumulator` helper for analyze-time latency reporting.
- **Integration tests** (`crates/clarion-cli/tests/`):
  - `install.rs`, `analyze.rs`, `serve.rs` — per-subcommand black-box tests via `assert_cmd`.
  - `wp1_e2e.rs`, `wp2_e2e.rs` — WP-level walking-skeleton tests; `wp2_e2e` consumes the `clarion-plugin-fixture` binary on disk.

### Subsystem E — `clarion-plugin-fixture` (131 LOC, 2 source files)

- **Location**: `crates/clarion-plugin-fixture/src/`
- **One-sentence responsibility**: Test-only Rust plugin speaking the L4 protocol; lets WP2 tests verify the host without bringing Python into the loop.
- **Modules**: `main.rs` (128 LOC) — full process entry; `lib.rs` (3 LOC) — re-export shim.
- **External interface**: stdin/stdout JSON-RPC, identical to Python plugin.

### Subsystem F — `plugins/python` (Python plugin, 2670 LOC)

- **Location**: `plugins/python/src/clarion_plugin_python/`
- **One-sentence responsibility**: Python language plugin — extracts module/class/function entities and `contains` + `calls` + `references` edges from a Python source tree; uses `pyright-langserver` for cross-reference resolution; speaks L4 JSON-RPC to the Rust host.
- **Modules** (by size):
  - `pyright_session.py` (1251 LOC) — long-running `pyright-langserver` LSP client; the heaviest single file.
  - `extractor.py` (744 LOC) — AST-based entity extraction; **modified on this branch** (+98 LOC, B.8 `@overload`-stub deduplication fix).
  - `server.py` (285 LOC) — JSON-RPC dispatch loop.
  - `entity_id.py` (75 LOC) — cross-language entity-ID assembler (parity-tested against `fixtures/entity_id.json`).
  - `reference_resolver.py` (69 LOC) — references-edge collection (B.5*).
  - `call_resolver.py` (64 LOC) — calls-edge resolution (B.4*).
  - `stdout_guard.py` (62 LOC) — keeps stray prints off the wire.
  - `wardline_probe.py` (56 LOC) — L8 Wardline-presence probe, reported in handshake `capabilities`.
  - `qualname.py` (46 LOC) — Python-flavoured canonical qualname per ADR-018.
  - `__main__.py` (15 LOC) / `__init__.py` (3 LOC).
- **External interface**: stdin/stdout JSON-RPC, console-script `clarion-plugin-python` on `$PATH`, manifest at `share/clarion/plugins/python/plugin.toml`.

---

## 5. Cross-cutting concerns

(Confidence: High where ADRs back the claim; Medium where inferred from imports.)

- **Entity-ID format** — ADR-003 + ADR-022. Three colon-segments. Cross-validated by `fixtures/entity_id.json` against both the Rust `entity_id.rs` (610 LOC) and Python `entity_id.py` (75 LOC). This is the L2 byte-for-byte parity proof.
- **JSON-RPC L4 protocol** — ADR-002 + commit `b0a12a6`. Content-Length framing, five methods (`initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`), 8 MiB cap on both sides. Speakers: `clarion-core::plugin::transport`, `clarion-plugin-fixture` main, `clarion-plugin-python::server`.
- **Ontology version semver** — ADR-027. Plugin manifest's `[ontology].ontology_version` follows MAJOR/MINOR/PATCH; B.5* was a MINOR bump (additive `references` edge kind) to `0.5.0`.
- **Edge confidence tiers** — ADR-028. `resolved` / `ambiguous` / `inferred`. MCP queries default to `>= resolved`; inferred edges are computed lazily at query time via LLM dispatch.
- **Summary cache key** — ADR-007. 5-tuple: (entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint). `ontology_version` is explicitly NOT a cache-key component (it's handshake-validation only).
- **Loom federation doctrine** — `docs/suite/loom.md` §3–§5. Solo-useful + pairwise-composable + enrich-only. Two named v0.1 asterisks survive: Wardline→Filigree pipeline coupling via Clarion; Python plugin's `wardline.core.registry.REGISTRY` import.
- **Tooling baseline (ADR-023)** — `cargo fmt`, `cargo clippy -D warnings`, `cargo nextest`, `RUSTDOCFLAGS="-D warnings" cargo doc`, `cargo deny check`; ruff + ruff-format + mypy --strict + pytest. CI at `.github/workflows/ci.yml` has three jobs: `rust`, `python-plugin`, `walking-skeleton` (the last depends on the first two).
- **Schema-validation policy (ADR-031, NEW 2026-05-18)** — `CHECK` constraints required on closed core-owned TEXT-enum columns, forbidden on plugin-extensible ones. Writer-actor remains the canonical validator; CHECK is defense-in-depth.

---

## 6. Documentation surface

(Confidence: High — `find docs -name '*.md'` returned 79 files; full file paths inventoried but not deep-read.)

### 6.1 Suite-level doctrine (`docs/suite/`, 4 files)

- `loom.md` — federation axiom, §3–§5 are load-bearing
- `briefing.md` — 5-minute intro
- `glossary.md` — cross-product term registry (gate for ADR acceptance)
- `README.md`

### 6.2 Clarion product docs (`docs/clarion/`)

- `v0.1/requirements.md` — REQ-/NFR-/CON-/NG- baselined IDs
- `v0.1/system-design.md` — §2–§11 each with `Addresses:` headers naming requirement IDs
- `v0.1/detailed-design.md` — schemas, rule catalogues, appendices (modified on this branch, +15 lines)
- `v0.1/plans/v0.1-scope-commitments.md` — pre-implementation scope memo
- `v0.1/reviews/` — `panel-2026-04-17/` (4 files) + `pre-restructure/` (2 files). Supporting context, not normative.

### 6.3 ADRs (`docs/clarion/adr/`, 25 Accepted + 1 README)

Authored ADRs span 001–031 with gaps (008–010, 019–020 are tracked as backlog inside system-design §12 / detailed-design §11). All Accepted. ADR-031 was added on this branch (2026-05-18).

### 6.4 Implementation plans (`docs/implementation/`)

- `v0.1-plan.md` — 11 work packages in dependency order
- `sprint-1/` — `README.md`, `signoffs.md`, `wp1-scaffold.md`, `wp2-plugin-host.md`, `wp3-python-plugin.md` (closed at `v0.1-sprint-1`)
- `sprint-2/` — 12 files covering B.2, B.3, B.4*, B.5*, B.6, B.7, B.8 design + results + signoffs + OpenRouter swap memo + scope amendment memo. Sprint 2 closed at `v0.1-sprint-2` (initially RED on B.8, now GREEN after repair rerun captured in `b8-results.md`).

### 6.5 Operator docs (`docs/operator/`, 2 files)

- `README.md` + `openrouter.md` (provider-swap operator guide)

### 6.6 Handoffs (`docs/superpowers/`, 11 files)

8 dated handoff memos + 3 sprint plans. The most recent (`2026-05-16-sprint-2-resume.md`, `2026-05-03-skeleton-audit.md`) are the live carryover into the current branch.

### 6.7 Top-level

`docs/README.md` + `docs/clarion/README.md` + `docs/clarion/v0.1/README.md` + `docs/implementation/README.md` form the navigation tree.

---

## 7. What's changed since Sprint 1

(Confidence: High — `git log --oneline v0.1-sprint-1..HEAD` + `git diff --stat main..HEAD`.)

### 7.1 Whole-of-sprint-2 deltas (commits since `v0.1-sprint-1`)

A **new top-level crate (`clarion-mcp`)** plus heavy growth in `clarion-cli` (the `serve` subcommand), `clarion-storage` (graph query helpers, LLM cache tables, unresolved call sites, source ranges), and the Python plugin (calls + references edges). Notable commit chains:

- **B.2 (class + module entities)** — `e53191d` merge
- **B.3 (contains edges, first edge kind)** — `f9bd31e` merge, ontology `0.3.0`
- **B.4* (calls edges + confidence tiers)** — `837d965` merge, ontology `0.4.0`
- **B.5* (references edges via pyright)** — `e988a83` merge, ontology `0.5.0`
- **B.6 (seven-tool MCP surface)** — `ed64a16` merge, including `b0a12a6` (scaffold), `7c13b73` (`clarion serve`), `8b8ecdc` (graph query helpers), `7f6a51f` (storage-backed tools), `e964118` (MCP/LLM config), `5a9f218` (LLM cache tables), `6fba66b` (WP6 LLM provider surface), `5627686` (on-demand summary tool), `5dc6e23` (unresolved call sites), `dcf6a30` (inferred call-edge dispatch), `16634ae` (Filigree association contract test), `29d3865` (`issues_for`), `5588ed8` (full-surface e2e), `d1ebca4` (LLM budget reserve), `9ffc5c8` (replace unsound YAML parser → `serde_norway`), `fa5b7cb` (wire MCP LLM paths)
- **OpenRouter provider swap** — `35be4db` merge, `4af69fd` (replace Anthropic with OpenRouter), `a53d2e4` (operator docs), `ab6b1dd` (strict-JSON path for B.8 green rerun)
- **B.8 scale test** — `5a396a5` (plan + harness), `80a6af9` (heavy-sample steady-state), `ad2ef80` (RED results), `b87bc1d` (GREEN rerun on full elspeth), `ffdfd79` (signoff revised to GREEN)
- **Sprint hygiene** — `a80c31a` (gitignore `.env`), `dc9bf41` (load `.env` before tracing), `29f0426` (skip `@overload` stubs to prevent UNIQUE collision), `c7ec1dd` (ADR-031 CHECK clauses), `0cb61b4` (manifest rule-ID grammar fix)

### 7.2 Current-branch (`sprint-2/b8-scale-test`) deltas against `main` (11 commits)

`git diff --stat main..HEAD` shows 45 files changed, 23 209 insertions, 59 deletions. Substantive non-result-data changes:

- **`crates/clarion-core/src/llm_provider.rs`** — +193 lines (OpenRouter strict-JSON path)
- **`crates/clarion-core/src/plugin/manifest.rs`** — +10 lines (ADR-022 rule-ID quantifier fix)
- **`crates/clarion-mcp/src/lib.rs`** — +171 lines (B.8 follow-up)
- **`crates/clarion-mcp/tests/storage_tools.rs`** — +184 lines
- **`crates/clarion-storage/migrations/0001_initial_schema.sql`** — +33 lines (ADR-031 CHECK clauses)
- **`crates/clarion-storage/tests/schema_apply.rs`** — +148 lines (new test verifying CHECK enforcement)
- **`docs/clarion/adr/ADR-031-schema-validation-policy.md`** — +426 lines (new ADR)
- **`plugins/python/src/clarion_plugin_python/extractor.py`** — +98 lines (`@overload`-stub skip)
- **`plugins/python/tests/test_extractor.py`** — +184 lines

The bulk of the +23 209 line delta is **B.8 scale-test result artifacts** (`tests/perf/b8_scale_test/results/*/mcp-driver-output*.json`) — JSON drivers' captured outputs from the elspeth corpus run, plus the 816-line `driver.py` and 227-line `test_driver.py` harness. These are checked-in evidence, not source code.

### 7.3 Working tree (not yet committed)

`git status` shows 7 modified files + 1 new file (`docs/clarion/adr/ADR-031-schema-validation-policy.md`) + the result snapshot `tests/perf/b8_scale_test/results/2026-05-18T0017Z/`. The B.8 GREEN rerun results are partially committed and partially still in the working tree.

---

## 8. Open questions for the deeper per-subsystem pass

1. **`clarion-mcp/src/lib.rs` is 2 617 LOC in one file.** Is this a single coherent concern (tool dispatch) or a god-file that subdivides? The subsystem-catalog pass should examine internal structure — is there a `ServerState` impl, a per-tool handler set, and a transport loop, all in one file? If so, refactor candidates may surface.
2. **`clarion-core/src/plugin/host.rs` is 3 126 LOC.** Same question — supervisor logic vs handshake vs analyze-file orchestration. Sprint-1 lock-ins anchor this; understand which.
3. **`clarion-mcp` ↔ `clarion-storage` coupling.** `clarion-mcp::lib.rs` imports ~20 symbols from `clarion-storage` (`CallEdgeMatch`, `EntityRow`, `InferredCallEdgeRecord`, `ReaderPool`, etc.). Is the MCP layer a thin dispatch over storage query helpers, or does it also marshal/transform substantially? This affects the catalog's responsibility statement for `clarion-storage::query`.
4. **LLM-provider dispatch path.** `serve.rs` wires `OpenRouterProvider` | `RecordingProvider` | disabled. `dcf6a30` and `5627686` added inferred-edge and summary dispatch. The catalog pass should confirm what's actually cached vs lazy-computed and where the 5-tuple cache key is materialised (writer.rs vs lib.rs vs cache.rs).
5. **Filigree integration shape.** `filigree.rs` is 238 LOC; `issues_for` exists; ADR-029 governs the binding contract. The catalog should verify the "enrich-only" axiom is upheld at the code level — does any code path on the Clarion side *require* a Filigree response to succeed?
6. **Schema migration governance.** Single migration file (289 LOC), edit-in-place per ADR-024 until an external operator ships a published build. Currently three documented edit events (initial, 2026-05-03 ADR-024 rename, 2026-05-18 ADR-031 CHECKs). When does the retirement trigger fire? Catalog should record current state.
7. **`tests/perf/elspeth_mini/`** is a 3-file corpus checked in for harness self-tests. Confirm this is fixture data (not Clarion source); none of the discovery reads suggest it's wired into the build.
8. **Sprint-1 deferred items** (per `CLAUDE.md`) — six WP2 items, four WP1 review-2 P2 bugs, one WP9 amendment — are these still open Filigree issues, or has Sprint 2 closed some? The catalog pass may want to map deferred items to current crate state.

---

## Confidence summary

| Section | Confidence | Evidence |
|---|---|---|
| §1 Repository organisation | High | `ls` + `Cargo.toml` workspace members + ADR-022 |
| §2 Technology stack | High | `Cargo.toml`, `pyproject.toml`, `plugin.toml` read in full |
| §3 Entry points | High | `main.rs`, `cli.rs`, `__main__.py`, `serve.rs` read |
| §4 Subsystem identification | High for crate boundaries, Medium for "responsibility" claims (lib.rs facades read, internal implementations sampled by size only) | per-crate `src/` listings, `lib.rs` heads read, LOC counts per file |
| §5 Cross-cutting concerns | High where ADR-cited; Medium where inferred | ADR index, ADR-031, ADR-027, ADR-028, ADR-007 read |
| §6 Documentation surface | High (inventory only; no depth reads) | `find docs -name '*.md'` |
| §7 Sprint-2 deltas | High | `git log v0.1-sprint-1..HEAD`, `git diff --stat main..HEAD`, `git status` |
| §8 Open questions | N/A (questions, not findings) | n/a |

---

## Risk Assessment

- **Risk: file-size god-files** — Two source files exceed 2 600 LOC (`clarion-core/src/plugin/host.rs` 3126, `clarion-mcp/src/lib.rs` 2617). Catalog pass needs to confirm whether internal structure makes them coherent or whether they should be flagged for refactoring.
- **Risk: discovery undercounts plugin-side complexity** — `pyright_session.py` is 1 251 LOC of LSP-client logic; this is single-file in the plugin but represents a substantial dependency on an external process. The catalog pass for subsystem F must give this its own treatment.
- **Risk: Sprint-2 docs not yet fully inventoried** — 12 sprint-2 .md files exist; only `signoffs.md` was read in any depth this pass. The deeper pass should at least skim each B.* design doc for subsystem-level claims.

## Information Gaps

- LLM-provider dispatch internals (how the 5-tuple cache key is constructed and where the LRU/TTL logic lives).
- Whether `tests/e2e/sprint_2_mcp_surface.sh` actually executes all seven MCP tools or a subset.
- The `scripts/` directory contents (not enumerated this pass).
- Wardline / Filigree sibling-repo source — none of this is vendored; cross-product claims rely on doctrine + manifest pins only.

## Caveats

- LOC numbers include test code unless explicitly stated. The "actual code" subtotals exclude `tests/` subdirectories but include `#[cfg(test)]` modules in `src/` (e.g., `mock.rs` 897 LOC counts in `clarion-core`).
- This pass cited line ranges from file heads only; deeper file scrutiny is deferred to the per-subsystem catalog pass.
- No assessment of code *quality* is made here. That is a `axiom-system-architect:assess-architecture` concern, not a discovery concern.
