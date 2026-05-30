# RC1 Subsystem Catalog

Each subsystem uses the archaeologist catalog contract: Location,
Responsibility, Key Components, Dependencies, Patterns Observed, Concerns, and
Confidence.

## 1. Clarion Core

**Location:** `crates/clarion-core`

**Responsibility:** Shared runtime contracts and enforcement: entity ID
assembly, plugin manifest/protocol/transport/discovery/supervision, resource
and path safety checks, host findings, test mocks, and LLM provider adapters.

**Key Components:** `src/lib.rs`, `src/entity_id.rs`,
`src/plugin/manifest.rs`, `src/plugin/protocol.rs`, `src/plugin/host.rs`,
`src/llm_provider.rs`.

**Dependencies:** consumed by most workspace crates and the fixture plugin;
depends on `serde`, `serde_json`, `toml`, `reqwest`, `tracing`, `nix`, `which`,
and process/file APIs.

**Patterns Observed:** facade exports stable caller-facing types; boundary
validation happens at manifest, frame, path, field-size, and LLM response
ingress; malformed plugin data becomes findings while path/resource breakers
stop the plugin.

**Concerns:** `EntityCountCap` semantics drift between comments and host edge
processing; memory limits are Linux-only; path jail is canonicalization-time,
not open-file proof; `plugin/host.rs` and `llm_provider.rs` have broad blast
radius.

**Confidence:** High.

## 2. Clarion Plugin Fixture

**Location:** `crates/clarion-plugin-fixture`

**Responsibility:** Minimal binary test plugin that speaks Clarion JSON-RPC
framing over stdin/stdout and gives `PluginHost::spawn` a real subprocess
target.

**Key Components:** `src/main.rs`, `src/lib.rs`, `Cargo.toml`, and
`crates/clarion-core/tests/fixtures/plugin.toml`.

**Dependencies:** consumed by core subprocess tests; depends on `clarion-core`,
`serde_json`, and Unix-only `nix` features for memory-limit testing.

**Patterns Observed:** reuses production framing/protocol types; fails closed
on invalid input; emits deterministic single-entity output.

**Concerns:** manifest declares an edge kind but the fixture emits no accepted
edge; bad input terminates instead of returning JSON-RPC errors; memory-limit
stress coverage is platform-shaped.

**Confidence:** High.

## 3. Clarion Storage

**Location:** `crates/clarion-storage`

**Responsibility:** SQLite persistence for Clarion's graph, runs, findings,
search indexes, LLM/query caches, writer serialization, and pooled reads.

**Key Components:** `migrations/0001_initial_schema.sql`, `src/schema.rs`,
`src/commands.rs`, `src/writer.rs`, `src/reader.rs`, `src/query.rs`,
`src/cache.rs`, `src/unresolved.rs`.

**Dependencies:** consumed by CLI, MCP, and HTTP read paths; depends on
`clarion-core`, `rusqlite`, `deadpool-sqlite`, `tokio`, `serde`,
`serde_json`, `blake3`, `tracing`, and `thiserror`.

**Patterns Observed:** single writer actor serializes all durable mutation;
read pool handles query concurrency; SQLite constraints and writer protocol
checks share invariant enforcement; tests use real temp SQLite databases.

**Concerns:** `summary_cache.entity_id` has no FK while
`inferred_edge_cache.caller_entity_id` has one; `ReaderPool::open` defers DB
validation until first read; query-time writes can force active run
transactions to commit.

**Confidence:** High.

## 4. Clarion CLI, Analyze, Serve, And HTTP Read API

**Location:** `crates/clarion-cli`

**Responsibility:** Owns the `clarion` binary: project install, analyze
orchestration, pre-ingest secret scanning, MCP stdio serving, and federation
HTTP read API.

**Key Components:** `src/main.rs`, `src/install.rs`, `src/analyze.rs`,
`src/secret_scan/*`, `src/serve.rs`, `src/http_read.rs`.

**Dependencies:** consumed by operators, CI/E2E scripts, federation consumers,
and MCP clients; depends on `clarion-core`, `clarion-storage`, `clarion-mcp`,
`clarion-scanner`, `clap`, `axum`, `tokio`, `tower`, `rusqlite`, `ignore`,
`serde`, and `xgraph`.

**Patterns Observed:** explicit run terminal states; plugin execution isolated
in `spawn_blocking`; HTTP API uses closed envelopes, body/concurrency/timeout
layers, ETags, and single-connection batch lookup; `analyze` skips `.env`
loading.

**Concerns:** HTTP HMAC is locally implemented; `_capabilities` is
intentionally unauthenticated, so bind-address and non-loopback trust remain
load-bearing; `analyze.rs` and `http_read.rs` should not absorb unrelated
future concerns.

**Confidence:** High.

## 5. Clarion Scanner

**Location:** `crates/clarion-scanner`

**Responsibility:** Core-owned pre-ingest scanner: detects secret-like byte
ranges, returns redacted metadata plus SHA-1 hashes, and applies
detect-secrets-style baselines.

**Key Components:** `src/lib.rs`, `src/patterns.rs`, `src/baseline.rs`,
`src/entropy.rs`.

**Dependencies:** consumed by CLI pre-ingest scanner; depends on `regex`,
`serde`, `serde_norway`, `sha1`, and `thiserror`.

**Patterns Observed:** byte-oriented scanning preserves offsets for non-UTF-8
input; literal secrets are not exported; baseline suppression keys on file
path, rule, line, hash, and `is_secret = false`.

**Concerns:** high-entropy detection is intentionally broad and relies on
baselines for lockfile/git-SHA false positives; contextual comment suppression
only handles `#` comments.

**Confidence:** High.

## 6. Clarion MCP Consult Surface

**Location:** `crates/clarion-mcp`

**Responsibility:** Exposes Clarion's MCP JSON-RPC/tool surface for code-graph
lookup, graph traversal, summaries, inferred calls, subsystem membership, and
optional Filigree issue enrichment.

**Key Components:** tool catalog, `ServerState`, LLM prompt/cache/accounting
path, `config.rs`, `filigree.rs`.

**Dependencies:** consumed by MCP clients and CLI `serve`; depends on
`clarion-storage`, `clarion-core`, optional Filigree HTTP, `blake3`, `reqwest`,
`rusqlite`, `serde`, `serde_json`, `serde_norway`, `time`, `tokio`, and
`tracing`.

**Patterns Observed:** uniform tool envelope; live providers require explicit
opt-in; blocking LLM and Filigree work uses `spawn_blocking`; inferred
dispatches coalesce concurrent cold requests.

**Concerns:** unreadable/non-UTF8 source returns empty LLM excerpt rather than
hard error; one shared token ledger gates both summary and inferred-edge calls;
Filigree HTTP error bodies can appear in MCP envelope error strings.

**Confidence:** High.

## 7. Python Language Plugin

**Location:** `plugins/python`

**Responsibility:** Clarion's v1.0 Python language plugin: JSON-RPC stdio
server, AST entity/edge extraction, Pyright-backed call/reference resolution,
and fail-soft Wardline compatibility reporting.

**Key Components:** `plugin.toml`, `pyproject.toml`, `server.py`,
`extractor.py`, `pyright_session.py`, `wardline_probe.py`, `stdout_guard.py`.

**Dependencies:** consumed by Clarion plugin host and discovery; depends on
Python stdlib AST/JSON/subprocess/select/pathlib, `packaging`, pinned
`pyright==1.1.409`, optional Wardline import, and `pyright-langserver`.

**Patterns Observed:** syntax errors emit degraded module entities; missing
Pyright returns unresolved stats/findings; caps include 8 MiB frames, stdout
guard, per-file Pyright timeout, reference-site cap, and project-local external
filtering.

**Concerns:** Pyright pin is duplicated in `pyproject.toml` and `plugin.toml`;
Wardline bounds are duplicated in `plugin.toml` and server constants; Wardline
probe catches `ImportError` only; installed-entrypoint smoke can skip when the
package is not installed editable.

**Confidence:** High for source shape and local behavior; medium for live
runtime health because plugin tests were not executed in this pass.

## 8. Release, Governance, And Federation Evidence

**Location:** `docs/clarion/1.0`, `docs/clarion/adr`, `docs/federation`,
`docs/operator`, `.github/workflows`, `scripts`, `tests/e2e`, `tests/perf`.

**Responsibility:** Defines the v1.0 release contract, federation HTTP read
contract, governance gates, release workflows, operator evidence, manual
publish checks, and scale/perf evidence.

**Key Components:** v1.0 requirements/system/detailed design, ADR-033,
ADR-034, federation contracts and fixtures, release workflows, governance
guard scripts, E2E/perf artifacts.

**Dependencies:** used by release maintainers, Filigree's `ClarionRegistry`,
MCP clients, consult-mode agents, and external operators; depends on GitHub
Releases/Actions/rulesets, Dependabot, cosign, SLSA generator, cargo/nextest,
Python tooling, sqlite3, pyright, and optional Filigree/Wardline.

**Patterns Observed:** layered docs with explicit precedence; federation is
enrich-only, not shared runtime; closed wire contracts with normative fixtures;
fail-closed auth for non-loopback HTTP; governance-as-code for release policy.

**Concerns:** live GitHub repository governance is documented as still
permissive; release workflows do not run every E2E script named in local
instructions; external-operator smoke lacks a dated result artifact;
`CHANGELOG.md` has an auth-code mismatch.

**Confidence:** High for documentation/infrastructure shape; medium for live
release posture.
