## Test-only Rust fixture plugin (`clarion-plugin-fixture`)

**Location:** `crates/clarion-plugin-fixture/src/`

**Responsibility:** Protocol-compatible stand-in for a real language plugin: a minimal Rust binary speaking the same Content-Length-framed JSON-RPC 2.0 protocol on stdin/stdout as the Python plugin, used by `clarion-core`'s `host_subprocess` integration test to exercise `PluginHost::spawn` end-to-end without bringing a Python interpreter and pyright into the test loop.

**Key Components:**

- `Cargo.toml` (19 lines) — declares a single `[[bin]]` target (`clarion-plugin-fixture`, `src/main.rs`); depends on `clarion-core` (path dep, version `0.1.0-dev`) and `serde_json` from the workspace; inherits workspace `[lints]`. No library is published.
- `src/main.rs` (128 lines, full code) — the entire plugin. One blocking `loop` over `read_frame(&mut reader, ContentLengthCeiling::DEFAULT)` (`main.rs:33`); per-frame `serde_json::from_slice` to a free-form `Value` so it can branch on `id`-presence (notification vs. request) before typed deserialisation (`main.rs:37-46`). Five method branches matching the L4 protocol surface:
  - `initialize` (request) → `InitializeResult { name: "clarion-plugin-fixture", version: "0.1.0", ontology_version: "0.1.0", capabilities: {} }` (`main.rs:68-76`).
  - `initialized` (notification) → state transition only, no reply (`main.rs:50-53`).
  - `analyze_file` (request) → extracts `params.file_path` (or `""`), echoes it back inside one stub entity `{"id": "fixture:widget:demo.sample", "kind": "widget", "qualified_name": "demo.sample", "source": {"file_path": <echoed>}}`, returns `AnalyzeFileResult { entities: vec![entity], edges: vec![], stats: default }` (`main.rs:77-108`).
  - `shutdown` (request) → empty `ShutdownResult` (`main.rs:109-112`).
  - `exit` (notification) → `std::process::exit(0)` (`main.rs:54-56`).
- `src/lib.rs` (3 lines) — comment-only stub explaining the crate is binary-only; exists so Cargo resolves the workspace member cleanly.

**Dependencies:**

- Inbound: `crates/clarion-core/tests/host_subprocess.rs` is the sole consumer — it locates the binary via `CARGO_BIN_EXE_clarion-plugin-fixture`, falling back to `<target_dir>/{debug,release}/clarion-plugin-fixture`; the manifest `tests/fixtures/plugin.toml` is `include_bytes!`-embedded at compile time (`host_subprocess.rs:16`). CI's `walking-skeleton` job builds this binary as part of `cargo build --workspace --bins` so the test can find it on disk (see `CLAUDE.md` build-commands section: "wp2_e2e tests need clarion-plugin-fixture on disk").
- Outbound: `clarion-core::plugin::limits::ContentLengthCeiling` (the 8 MiB default), `clarion-core::plugin::transport::{Frame, read_frame, write_frame}` (the shared framing codec), `clarion-core::plugin::{AnalyzeFileParams, AnalyzeFileResult, AnalyzeFileStats, InitializeResult, JsonRpcVersion, ResponseEnvelope, ResponsePayload, ShutdownResult}` (the typed protocol structs); `serde_json` for the free-form `Value` pre-dispatch.

**Patterns Observed:**

- **Protocol-by-shared-types.** The fixture reuses `clarion-core`'s own protocol structs (`InitializeResult`, `AnalyzeFileResult`, `ResponseEnvelope`, …) for response serialisation — there is no parallel schema definition. A breaking change to `protocol.rs` therefore fails compilation of the fixture, not at runtime under test, which is the right ordering.
- **Same ceiling as production.** Frame reads use `ContentLengthCeiling::DEFAULT` (the ADR-021 §2b 8 MiB cap), with the source comment explicitly noting that `unbounded()` is now `#[cfg(test)]`-only (`main.rs:30-32`). The fixture lives under the same wire-cap discipline as a real plugin.
- **Fail-fast on protocol violations.** Every recoverable branch in a real plugin is `std::process::exit(1)` here — malformed frame, non-object body, missing/non-string `method`, integer-id parse failure, unknown method, params-deserialise failure (`main.rs:34, 39, 45, 57, 64, 90, 113`). Acceptable because the consumer is exclusively an integration test; the alternative would obscure protocol-violation bugs behind fixture-side error handling.
- **Notification vs. request branching on `id`-presence.** Reads the raw `Value` first, checks `id.is_some_and(|v| !v.is_null())` to decide whether the frame requires a response (`main.rs:42, 48-60`). This matches the JSON-RPC 2.0 spec and parallels the Python plugin's branching in `server.dispatch` (`server.py:239-261`).
- **Stable identity for assertions.** `plugin_id = "fixture"`, kind `"widget"`, and the literal entity ID `"fixture:widget:demo.sample"` are baked into the source — `host_subprocess.rs` asserts on this exact string, so the test signal is exact-match rather than parse-and-inspect.

**Concerns:**

- **No request-id sanity on `shutdown`.** Unlike the Python plugin, the fixture doesn't gate `analyze_file` on having received `initialized` — `state.initialized` doesn't exist. This is fine for the single happy-path test it supports, but means the fixture cannot exercise the host's `-32002 NOT_INITIALIZED` error path. If a future test wanted to assert that the host *itself* sequences the handshake correctly, it would have to verify host-side state rather than fixture-side rejection.
- **`exit(1)` on any malformed frame is observable only as a non-zero process exit.** The host-side test gets no structured signal about which branch failed. For an integration test fixture this is by design; flagging because anyone running the fixture by hand against a non-test client will see opaque exits.
- **No stderr discipline.** A real plugin (Python's `stdout_guard.py`) reserves stdout strictly for framing; the fixture relies on the absence of any `eprintln!` or `println!` in its own code rather than installing a guard. For a 128-line file with `serde_json` as the only output-side dep this is fine, but worth noting as a delta from the production-plugin pattern.

**Confidence:** High — Read `main.rs` (128 lines, 100% of file), `lib.rs` (3 lines, 100%), `Cargo.toml` (19 lines, 100%); cross-verified consumer via `crates/clarion-core/tests/host_subprocess.rs` lines 3-7, 15-27, and 60-66 (binary-location strategy, fixture identity assertions, manifest constants). Cross-validated against `docs/arch-analysis-2026-05-18-1244/01-discovery-findings.md` §4 Subsystem E framing and `CLAUDE.md` layout summary. Protocol identity confirmed by the matching set of imports from `clarion_core::plugin::*` against the Python plugin's `server.py:7-19` docstring describing the same five methods and response shapes. Content-Length framing parity confirmed via the explicit `ContentLengthCeiling::DEFAULT` (8 MiB) source comment matching the Python `MAX_CONTENT_LENGTH = 8 * 1024 * 1024` at `server.py:48`.

**Information Gaps:**

- Did not read the upstream `clarion_core::plugin::transport` module to verify exactly how `read_frame` / `write_frame` interpret the ceiling; took the source comment at face value.
- Did not run `cargo build -p clarion-plugin-fixture` on the current branch to confirm the binary still compiles. Treated the unmodified `Cargo.toml` and the recent (b87bc1d) signoff record as sufficient evidence that the walking-skeleton CI job was green at sprint close.

**Caveats:**

- "Protocol-compat" here means *exact wire-shape compatibility* on the five L4 methods. The fixture does not exercise the `capabilities.wardline` probe shape, `parse_status` on module entities, `parent_id`/`contains` edges, calls/references resolution, the `stats` payload's `unresolved_call_sites`, or any of the Sprint-2 ontology surface. It is a *minimum*-shape test stand-in, not a feature-parity one.
- The fixture's `ontology_version = "0.1.0"` (`main.rs:72`) is deliberately the Sprint-1 baseline; this is the version against which the host's manifest-handshake validator is tested. It does *not* track the Python plugin's `0.5.0` and shouldn't.

**Risk Assessment:**

- *Drift between fixture and real plugins.* The fixture has been stable since Sprint 1 close and the protocol contract is enforced by shared `clarion-core` types, so the drift surface is bounded to behavioural-not-structural divergence (e.g. a real plugin adding handshake side-effects the fixture doesn't model). The host-side test exercises only the structural surface, so this is a known-acceptable gap.
- *Single-consumer dependency.* The fixture exists exclusively for `host_subprocess.rs`. If that test were retired, the fixture would become dead code; conversely, the test cannot be expanded to cover behaviours the fixture doesn't model without growing the fixture. Pre-existing carryover issue `clarion-adeff0916d` (fixture-binary self-build) tracks one known sharp edge here.
- *Build-ordering coupling.* The walking-skeleton CI job depends on `cargo build --workspace --bins` running before `cargo nextest run` so the binary is on disk when `host_subprocess.rs` looks for it. This is documented in `CLAUDE.md` and codified in `.github/workflows/ci.yml`'s `walking-skeleton` job, but is an implicit dependency that would break if a future contributor used `cargo nextest run --workspace` without the prior `cargo build`.
