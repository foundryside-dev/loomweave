## 6. clarion-plugin-fixture

**Location:** `crates/clarion-plugin-fixture/`
**LOC:** 187 source (3 `lib.rs` + 184 `main.rs`); 0 in-crate tests — exercised externally by 931 LOC of test code in `clarion-core` and `clarion-cli`.
**Crate type / role:** Binary crate (`[[bin]] name = "clarion-plugin-fixture"`) plus a stub `lib.rs` whose only job is to let Cargo resolve the workspace member cleanly. Functionally a **test-only protocol-reference plugin**: a minimal, correct implementation of the L4 JSON-RPC wire contract used to drive the plugin host's subprocess code paths without needing the Python plugin on `PATH`.

### Responsibility

This crate owns the *smallest valid implementation* of the Clarion plugin protocol — just enough to (a) prove `clarion-core::plugin::host` can spawn a child, negotiate `initialize`/`initialized`/`analyze_file`/`shutdown`/`exit`, and round-trip framed JSON-RPC, and (b) deterministically misbehave on demand (RLIMIT_AS exhaustion via `CLARION_FIXTURE_EXCEED_RLIMIT_AS`) so the host's OOM-kill and crash-loop-breaker paths can be tested end-to-end. It is intentionally not a real analyzer: every `analyze_file` returns the same single hard-coded widget entity regardless of the input file's content. The crate's "public surface" is therefore (1) the binary itself, consumed by `cargo nextest` via `CARGO_BIN_EXE_clarion-plugin-fixture`, and (2) the fixture identity tuple (`plugin_id=fixture`, kind `widget`, qualname `demo.sample`, rule-ID prefix `CLA-FIXTURE-`) that tests assert against verbatim.

### Key components

- `src/main.rs:23-123` — the JSON-RPC dispatch loop: blocking `read_frame` → parse → match on `method` → either no-op (notification) or send a typed result envelope. Uses `clarion-core` framing and request/response types directly so the wire shape stays in lockstep with the host.
- `src/main.rs:67-76` — `initialize` handler: emits `InitializeResult { name, version, ontology_version: "0.1.0", capabilities: {} }`.
- `src/main.rs:77-115` — `analyze_file` handler: extracts `file_path` from params, returns one entity (`fixture:widget:demo.sample`, `kind=widget`, `qualified_name=demo.sample`, `source.file_path=<input>`), no edges, default stats.
- `src/main.rs:48-60` — notification dispatch for `initialized` (becomes-ready, no reply) and `exit` (process-exits-0, no reply).
- `src/main.rs:78-83, 137-184` — the `CLARION_FIXTURE_EXCEED_RLIMIT_AS` escape hatch: on Unix, repeatedly `mmap_anonymous` doubling-size regions with `PROT_NONE` to blow past `RLIMIT_AS`, then `SIGKILL` self via `nix::sys::signal::kill`. The mappings are held but never dereferenced (documented `SAFETY` comment).
- `src/main.rs:125-135` — `send_result` helper: wraps a `Value` in `ResponseEnvelope { jsonrpc, id, payload: Result(...) }`, serialises, frames via `write_frame`, flushes.
- `src/lib.rs:1-3` — three-line doc-only stub explaining why the lib target exists.

### Public interface (outbound)

The plugin speaks the L4 JSON-RPC protocol on stdin/stdout. Methods implemented:

- **`initialize`** (request) — returns `InitializeResult` with `ontology_version=0.1.0`, empty capabilities. `src/main.rs:68-76`
- **`initialized`** (notification) — accepted, no-op. `src/main.rs:51-53`
- **`analyze_file`** (request) — accepts `AnalyzeFileParams`, returns one canned entity. `src/main.rs:77-115`
- **`shutdown`** (request) — returns empty `ShutdownResult`. `src/main.rs:116-119`
- **`exit`** (notification) — `process::exit(0)`. `src/main.rs:54-56`

Any unknown method, malformed frame, or unparseable JSON causes `process::exit(1)` (`src/main.rs:33-39, 46, 57, 64, 97, 120`).

Companion manifest (consumed by host, not shipped by this crate): `crates/clarion-core/tests/fixtures/plugin.toml` declares `plugin_id="fixture"`, `language="fixture"`, `extensions=["mt"]`, `entity_kinds=["widget"]`, `edge_kinds=["uses"]`, `rule_id_prefix="CLA-FIXTURE-"`.

### Dependencies

- **Inbound (who consumes the binary):**
  - `crates/clarion-core/tests/host_subprocess.rs` — direct subprocess test of `PluginHost::spawn`; locates the binary via `CARGO_BIN_EXE_clarion-plugin-fixture` (`host_subprocess.rs:30`) with a `cargo build` fallback (`:84-94`). Asserts entity id `fixture:widget:demo.sample` (`:162`).
  - `crates/clarion-cli/tests/wp2_e2e.rs` — declares `clarion-plugin-fixture` as a `[dev-dependencies]` entry (`clarion-cli/Cargo.toml:43`) so nextest exports the env var, symlinks the binary into a synthetic plugin dir, and drives the full `clarion analyze` CLI through it. Tests: `wp2_e2e_smoke_fixture_plugin_round_trip` (line 135), `wp2_rlimit_as_oom_kill_is_reported_as_host_finding` (line 259, uses `CLARION_FIXTURE_EXCEED_RLIMIT_AS`), `wp2_crash_in_one_plugin_does_not_prevent_other_plugins_from_running` (line 323), `wp2_crash_loop_breaker_trips_and_skips_remaining_plugins` (line 471).
- **Outbound (what this calls):**
  - `clarion-core::plugin::transport` — `read_frame` / `write_frame` / `Frame` for length-prefixed framing.
  - `clarion-core::plugin::limits::ContentLengthCeiling::DEFAULT` (8 MiB per ADR-021 referenced in source comment, `main.rs:30-32`).
  - `clarion-core::plugin` request/response DTOs: `AnalyzeFileParams`, `AnalyzeFileResult`, `AnalyzeFileStats`, `InitializeResult`, `ShutdownResult`, `JsonRpcVersion`, `ResponseEnvelope`, `ResponsePayload`.
  - `serde_json` for ad-hoc `Value` parsing of the inbound request envelope.
  - `nix` (Unix only, `mman` + `signal` features) — only for the deliberate-OOM probe.
- **External services:** None at runtime. Communicates only over inherited stdin/stdout with its parent (the plugin host).

### Internal architecture

The binary is a single synchronous loop in `main()` (`main.rs:23-123`); there are no modules, threads, async runtime, or background tasks. State machine is implicit and minimal: the loop accepts notifications and requests in any order after the host has spoken — there is no explicit "before-initialize" guard, which is acceptable because the host always sends `initialize` first and the fixture's responses are stateless. The shape is `loop { read_frame → parse Value → branch on (has_id, method) → dispatch }`.

Error model is intentionally brutal: any deviation from the happy path (truncated frame, non-UTF8, missing method, unknown method, unparseable params) calls `std::process::exit(1)` with no reply. This is *desired* behaviour — `clarion-core`'s host has to handle a plugin that hangs up mid-stream, and crashing the fixture is the cheapest way to drive that path. The crash-loop-breaker test (`wp2_e2e.rs:471`) depends on this.

The `exceed_rlimit_as` path (`main.rs:137-184`) is the only piece of nontrivial logic. It pre-reserves 1024 mapping handles before the memory pressure starts (so the `Vec::push` itself does not allocate after the kernel starts refusing maps), then loops `mmap_anonymous(PROT_NONE, MAP_PRIVATE)` doubling the request size each iteration starting at 128 MiB. PROT_NONE means no physical pages are committed — only address space is consumed, which is exactly what `RLIMIT_AS` constrains. When the next request would not grow (`saturating_mul(2)` saturated) or `mmap` returns `Err`, the fixture `SIGKILL`s itself so the parent observes a signal-death, not a clean exit. This is the load-bearing detail that distinguishes OOM-kill from a controlled shutdown in the host's diagnosis.

`lib.rs` exists solely so the crate works as a workspace member with a binary target (3 lines, doc comment only).

### Patterns observed

- **Stub lib + real bin** (`lib.rs:1-3` + `Cargo.toml:12-14`) — workspace-member compatibility trick.
- **Untyped envelope parse, typed payload re-parse** (`main.rs:37-44, 93-98`) — read the whole frame as `serde_json::Value` to inspect `id`/`method` before committing to a typed struct, so unknown/malformed messages can be rejected without spurious deserialisation errors.
- **Crash-on-anomaly as a feature** (`main.rs:33-46, 57, 97, 120`) — every error path is `process::exit(1)`. Tests want this behaviour.
- **Hard-coded fixture identity** (`main.rs:101-108`) — `"fixture:widget:demo.sample"` is the ground-truth string tests assert on; do not change without updating both call sites listed under Inbound.
- **Environment-flag-driven fault injection** (`main.rs:78-83`) — `CLARION_FIXTURE_EXCEED_RLIMIT_AS` toggles the OOM path; default behaviour is benign.
- **PROT_NONE address-space probe** (`main.rs:137-178`) — exhausts virtual address space cheaply (no page commits) to trip a specific kernel limit deterministically.
- **Pre-reserved Vec to avoid allocation under memory pressure** (`main.rs:142-145`).

### Concerns / Smells / Risks

- **`process::exit(1)` everywhere with no diagnostic output.** Reasonable for a test fixture (the host is the system under test), but if a future test asserts on stderr or a specific exit code other than 1, every error branch will need disambiguating. No `eprintln!` anywhere. `main.rs:33-46, 57, 97, 120`.
- **No "initialize must come first" sequencing check.** A misbehaving host could call `analyze_file` before `initialize` and the fixture would happily respond. This is fine today because the host always sequences correctly, but the fixture is not a conformance checker — do not use it as one.
- **`unwrap()` on `serde_json::to_value(InitializeResult)`** (`main.rs:75`) and other result serialisations — defensible because the types are `Serialize`-derived, but pedantically these are crash points.
- **Unix-gated OOM path with a `cfg(not(unix))` arm that just `exit(1)`s** (`main.rs:78-83`) — Windows CI would silently lose coverage of the RLIMIT_AS branch. Acceptable since OOM-kill semantics are Unix-specific.
- **The manifest lives in another crate's test tree** (`clarion-core/tests/fixtures/plugin.toml`), not in this crate. Discoverable only by `grep`. Two callers (`host_subprocess.rs` and `wp2_e2e.rs`) construct their own variants of it. A `tests/fixtures/plugin.toml` here, re-used by both, would be cleaner; not urgent.
- **Hard-coded `version = "0.1.0"` and `ontology_version = "0.1.0"`** in the binary (`main.rs:71-72`) do not pick up the workspace version — if version-handshake logic ever depends on these, they will drift from `Cargo.toml`. Currently the host does not validate them.
- **No in-crate tests.** Justified: the fixture *is* the test apparatus for two upstream test files. Counting test coverage of this crate in isolation is meaningless.

### Confidence: High

Read all 187 source lines end-to-end, the companion manifest in `clarion-core/tests/fixtures/plugin.toml`, and grepped both consumer tests (`host_subprocess.rs`, `wp2_e2e.rs`) for every reference to `clarion-plugin-fixture`, `fixture:widget:demo`, and `CLARION_FIXTURE_EXCEED_RLIMIT_AS` to confirm the protocol surface and the four named test cases. Inbound dep edges verified via `Cargo.toml:43` of `clarion-cli`. Outbound dep edges verified by walking every `use clarion_core::` import in `main.rs`.
