# Plugin Host + Rust Plugin Adversarial-Input Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `loomweave analyze` over a hostile or pathological source tree cannot hang, crash the host, or wedge the run record — closing the hardening gap left open when the Rust plugin shipped on-by-default into rc4.

**Architecture:** Extend the existing CLI-side `PluginWatchdog` (analyze.rs) to cover the two uncovered lifecycle phases — handshake and shutdown — with per-phase wall-clock deadlines, by splitting `PluginHost::spawn` into launch and handshake halves. Fix exit-signal classification (SIGABRT finding; no OOM double-report on watchdog kill). In the Rust plugin, prevent stack-overflow aborts with a pre-parse depth scan (mini-lexer: bracket-depth cap + prefix-operator-run cap), pin the parse stack to a fixed 16 MiB thread, and cap file size — over-cap files degrade to warning findings, never crash. Document semantics in ADR-050; prove everything with an extended fixture plugin and a hostile-corpus e2e.

**Tech Stack:** Rust (edition 2024, 1.88 pin, clippy pedantic `-D warnings`), syn 2.0.117, std::thread + Arc<Mutex<Child>> watchdog (no tokio in the blocking path), bash e2e per `tests/e2e/` conventions.

**Branch:** `feat/plugin-host-hardening` (worktree `.claude/worktrees/plugin-host-hardening`), merges `--no-ff` into `rc4`.

**Ticket:** clarion-7bc08e05c0 (P1).

---

## Decisions baked into this plan

| # | Decision | Choice | Evidence |
|---|----------|--------|----------|
| D1 | Hang/spin containment | Host-side wall-clock deadlines via the existing `PluginWatchdog`, extended to handshake + shutdown phases. **No RLIMIT_CPU.** | Watchdog exists (analyze.rs:4241-4320, arm/disarm at 4401-4403). GAP 1: `PluginHost::spawn` runs the blocking handshake (host.rs:575) at analyze.rs:4349, *before* watchdog creation at 4366 — and the Rust plugin parses the whole repo inside `initialize` (serve.rs:104). GAP 2: watchdog is stopped (4519-4520) before `host.shutdown()` (4545), whose `read_response_matching` blocks forever. A hung handshake **wedges the project**: runs row stays `running`, the fail-fast advisory lock (analyze_lock.rs:63-77) blocks all later analyzes, and the stale-run sweep runs only after lock acquisition (analyze.rs:378-387). RLIMIT_CPU cannot cover blocked-idle hangs and risks pyright's legitimately long CPU-heavy runs; a wall-clock kill contains busy-loops too. |
| D2 | Deadline values | Handshake: 300 s default, env `LOOMWEAVE_PLUGIN_HANDSHAKE_TIMEOUT_MS`. Per-file: 120 s (existing, unchanged). Shutdown: 10 s default, env `LOOMWEAVE_PLUGIN_SHUTDOWN_TIMEOUT_MS`. ADR-035 constant discipline (basis/override/retune) at each constant. | Mirrors `DEFAULT_PLUGIN_FILE_TIMEOUT`/`plugin_file_timeout()` (analyze.rs:3070-3087). Handshake budget must scale differently from per-file: Rust plugin symbol-table build is whole-repo. `plugin_limits.*` yaml surface is documented in ADR-021 but **unimplemented** (only hit: breaker.rs:8 "lands in WP6") — env override is the existing precedent; yaml surface is a filed follow-up, not this plan. |
| D3 | Stack-overflow floor | Already terminal (SIGABRT → pipe EOF → `PluginRunError` → SoftFailed → `CommitRun(Failed)`). Add classification: signal 6 → new `FINDING_PLUGIN_ABORTED`; gate `oom_killed` on `!timed_out` so the watchdog's own SIGKILL stops double-reporting TIMEOUT+OOM. | Empirical: deep nesting SIGABRTs (exit 134), never SIGSEGV — Rust guard page + abort. Terminal path verified end-to-end (transport.rs:126 → analyze.rs:4409 → 951-998 → 1105-1113 → 1556-1566). Double-report verified: timeout finding at 4564-4579 + unconditional `oom_killed` for signal 9 at 4687-4691. |
| D4 | Stack-overflow prevention (Rust plugin) | Pre-parse depth scan with a minimal lexer (skips comments/strings): **bracket-depth cap 128** + **prefix-operator-run cap 1024**; over-cap → degraded entity + `LMWV-RUST-DEPTH-LIMIT` warning, symbol-table walk skips. Plus: run all syn parses on a dedicated thread with **16 MiB fixed stack** so crash thresholds stop depending on the inherited environment stack. | syn 2.0.117 has **no recursion limit** — always aborts. Measured first-crash depths at 8 MiB: mods 337, ifs 407, blocks 480, arrays 687, parens 760, unary `!` 2386; left-assoc binary chains parse iteratively (safe at 20 000). Byte-depth scan catches every bracket bomb (floor 337 ≫ cap 128) but **misses** unary bombs (byte-depth 1) — hence the prefix-run cap (2386 ≫ 1024, and ~4.7 k at 16 MiB). Lexer needed because banner comments (`/****…*/`) and string literals would false-positive a naive byte scan. Plugin parses on its main thread today with zero stack config (no `thread::`/`stack_size` hits in plugin-rust src). |
| D5 | Oversize-file cap (Rust plugin) | 10 MiB per `.rs` file; over-cap → degraded entity + `LMWV-RUST-FILE-TOO-LARGE` warning, symbol-table walk skips. Check via `fs::metadata` before reading. | Plugin reads files itself (`read_to_string` unbounded, serve.rs:197; RPC carries only `file_path`, protocol.rs:359-364). Manifest `expected_max_rss_mb = 128` — a multi-MB file's string+tokens+AST can blow RLIMIT_AS → avoidable child kill. |
| D6 | Test fixture | Extend `loomweave-plugin-fixture` with env-triggered misbehaviors: `LOOMWEAVE_FIXTURE_HANG_AT_INITIALIZE`, `LOOMWEAVE_FIXTURE_HANG_AT_SHUTDOWN`, `LOOMWEAVE_FIXTURE_SPIN_AT_ANALYZE`, `LOOMWEAVE_FIXTURE_ABORT_AT_ANALYZE` (pattern: existing `LOOMWEAVE_FIXTURE_EXCEED_RLIMIT_AS`, main.rs:78-83). | Fixture today can only self-OOM; hang tests use ad-hoc Python scripts (tests/analyze.rs HANGING_PLUGIN_SCRIPT). Handshake/shutdown/spin/abort scenarios need a real subprocess plugin. |
| D7 | Shutdown-timeout policy | Kill + reap + `LMWV-INFRA-PLUGIN-SHUTDOWN-TIMEOUT` **warning** finding; run outcome unchanged (a completed run stays `completed` — entities are durable, the data is whole; only the plugin's exit etiquette failed). | Existing code already treats shutdown failure as best-effort (analyze.rs:4544-4555 logs warn + kill, keeps `work_result` Ok). |
| D8 | ADR | New **ADR-050 “Plugin lifecycle deadlines and abnormal-exit classification”**; ADR-021 README row gets “amended by ADR-050”, and ADR-021's “What is NOT in Layer 2” CPU-cap rationale (“breaker already catches runaway loops” — false for *hung* plugins) is corrected by reference. Rust-plugin caps documented in ADR-050 §plugin-guards + ADR-035-style constant comments. | ADR-021:66-71 declines a CPU cap; no ADR defines plugin timeouts (ADR-002 contains none despite ADR-021's attribution). Index convention + next number 050 verified (README.md:7-64). |
| D9 | Out of scope (file as follow-up tickets, with dependency where needed) | (a) `plugin_limits.*` loomweave.yaml config surface (ADR-021-documented, unimplemented); (b) Python-plugin deep-recursion characterization (ast/pyright behavior on nested input); (c) SIGSEGV-vs-OOM conflation refinement beyond the timed_out gate. | Critic findings; Python initialize is light (server.py:143-160, pyright lazy at first analyze_file) so the handshake deadline is uncontroversial for it. |

## File structure

```
crates/loomweave-core/src/plugin/host.rs              # split spawn → spawn_unhandshaken + spawn wrapper
crates/loomweave-plugin-fixture/src/main.rs           # 4 new env-triggered misbehaviors
crates/loomweave-cli/src/analyze.rs                   # watchdog phases, handshake/shutdown deadlines,
                                                      #   reap classification (timed_out gate, SIGABRT), consts
crates/loomweave-cli/tests/analyze_hardening.rs       # NEW: hang/spin/abort integration tests (fixture-driven)
crates/loomweave-plugin-rust/src/parse_guard.rs       # NEW: depth scan (mini-lexer), size cap, pinned-stack parse
crates/loomweave-plugin-rust/src/serve.rs             # wire guard into analyze_one_file
crates/loomweave-plugin-rust/src/symbol_table.rs      # wire guard into init walk (skip)
crates/loomweave-plugin-rust/src/extract.rs           # degraded path: depth_limit / file_too_large subcodes
crates/loomweave-plugin-rust/tests/parse_guard_e2e.rs # NEW: generated-bomb integration tests
docs/loomweave/adr/ADR-050-plugin-lifecycle-deadlines.md  # NEW
docs/loomweave/adr/README.md                          # index rows (050 + 021 amended-by)
tests/e2e/hostile_corpus_rust.sh                      # NEW: acceptance-gate e2e
```

**Hard constraints (from CLAUDE.md / sprint brief):** no edits to `entity_id.rs` / `storage/sei.rs`; no version bumps; `unsafe` only in pre_exec/setrlimit paths with SAFETY comments; generated hostile fixtures are generated in-test, never checked-in blobs; clippy pedantic `-D warnings`.

---

### Task 1: Fixture-plugin misbehaviors

**Files:** Modify: `crates/loomweave-plugin-fixture/src/main.rs`

The fixture's dispatch loop (main.rs:23-123) handles `initialize` / `analyze_file` / `shutdown` / `initialized` / `exit`. Add four env-gated behaviors, checked once at startup like `LOOMWEAVE_FIXTURE_EXCEED_RLIMIT_AS` (main.rs:78-83):

- [ ] **Step 1.1:** Add behavior flags + helpers near the existing env gate:

```rust
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// Park forever without consuming CPU (hang simulation). The host's watchdog
/// must kill us; sleeping in a loop survives spurious wakeups.
fn hang_forever() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
```

- [ ] **Step 1.2:** In the dispatch loop: on receiving `initialize`, if `env_flag("LOOMWEAVE_FIXTURE_HANG_AT_INITIALIZE")`, call `hang_forever()` *before* sending the response. On receiving `shutdown`, if `env_flag("LOOMWEAVE_FIXTURE_HANG_AT_SHUTDOWN")`, `hang_forever()` before responding. On the first `analyze_file`: if `env_flag("LOOMWEAVE_FIXTURE_SPIN_AT_ANALYZE")`, busy-loop (`loop { std::hint::spin_loop(); }`) before responding; if `env_flag("LOOMWEAVE_FIXTURE_ABORT_AT_ANALYZE")`, `std::process::abort()` (raises SIGABRT — the real stack-overflow signature).

- [ ] **Step 1.3:** `cargo build -p loomweave-plugin-fixture && cargo clippy -p loomweave-plugin-fixture --all-targets -- -D warnings` — green. Commit: `test(fixture): env-triggered hang/spin/abort misbehaviors for hardening tests`.

(The fixture is test infrastructure; its behaviors are proven by Tasks 4-7's red tests.)

### Task 2: Split `PluginHost::spawn` (launch vs handshake)

**Files:** Modify: `crates/loomweave-core/src/plugin/host.rs` (spawn at 437-582, handshake already `pub fn` at 682)

- [ ] **Step 2.1 (red):** In `crates/loomweave-core/tests/host_subprocess.rs`, add a test that calls the new `PluginHost::spawn_unhandshaken(manifest, root, exe)` against the fixture plugin, asserts no `initialize` has been exchanged yet (host's `next_id` untouched / fixture receives nothing — simplest observable: call `handshake()` explicitly afterwards and assert it succeeds, then `shutdown()`), and asserts `spawn` (the wrapper) still round-trips as today. Run: `cargo nextest run -p loomweave-core spawn_unhandshaken` → FAIL (method missing).
- [ ] **Step 2.2 (green):** Extract everything in `spawn` *before* the `host.handshake()` call at host.rs:575 into `pub fn spawn_unhandshaken(...) -> Result<(Self, std::process::Child), HostError>` (same signature as `spawn`). Reimplement `spawn` as: `let (mut host, mut child) = Self::spawn_unhandshaken(...)?;` + the existing handshake-with-reap-on-failure block (kill+wait on Err, host.rs:568-580 comment preserved). Doc comments: `spawn_unhandshaken` says “caller MUST call `handshake()` before any request, and owns kill+reap on handshake failure (`Child::Drop` does not reap on Unix)”.
- [ ] **Step 2.3:** `cargo nextest run -p loomweave-core` green; clippy green. Commit: `refactor(host): split spawn into spawn_unhandshaken + handshake wrapper`.

### Task 3: Watchdog phases

**Files:** Modify: `crates/loomweave-cli/src/analyze.rs` (PluginWatchdog at 4241-4320, unit test `plugin_watchdog_arm_disarm_and_severity` at ~6134)

- [ ] **Step 3.1 (red):** Extend the existing watchdog unit test: arm with a phase, expire, assert `did_time_out()` reports the phase.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WatchdogPhase { Handshake, File, Shutdown }
```

`arm(deadline)` → `arm(deadline, WatchdogPhase)`; `did_time_out() -> bool` → `timed_out_phase() -> Option<WatchdogPhase>`. Run the watchdog unit tests → FAIL.
- [ ] **Step 3.2 (green):** Store `Mutex<Option<WatchdogPhase>>` alongside the deadline; the watchdog thread records the phase it killed for. Update both existing call sites (`arm(file_timeout)` → `arm(file_timeout, WatchdogPhase::File)`; `did_time_out()` → `timed_out_phase()` mapped to the existing per-file behavior). All existing tests green. Commit: `feat(analyze): phase-aware plugin watchdog`.

### Task 4: Handshake deadline

**Files:** Modify: `crates/loomweave-cli/src/analyze.rs` (run_plugin_blocking 4333+; consts near 3067-3087; caller threading near 709). Test: `crates/loomweave-cli/tests/analyze_hardening.rs` (NEW — copy harness conventions from tests/analyze.rs's hanging-plugin test at ~5117).

- [ ] **Step 4.1 (red):** New integration test `handshake_hang_times_out_run_terminal_child_reaped`: temp project with one trivial file; fixture plugin installed as the only plugin (copy the plugin-dir setup from existing fixture-driven tests in `crates/loomweave-cli/tests/wp2_e2e.rs:276+`); env `LOOMWEAVE_FIXTURE_HANG_AT_INITIALIZE=1`, `LOOMWEAVE_PLUGIN_HANDSHAKE_TIMEOUT_MS=500`. Run analyze. Assert: (a) analyze returns Err / exit-1 path within the test timeout (≪ 3600 s — proves deadline fired); (b) `runs.status == 'failed'` and `failure_reason` mentions handshake timeout; (c) a `LMWV-PY-TIMEOUT` finding with `metadata.phase == "handshake"` persisted; (d) **no zombie**: poll `/proc/<child_pid>/` absent or state ≠ Z (copy the zombie assert from the existing reap test at analyze.rs:6261+); (e) no `LMWV-INFRA-PLUGIN-OOM-KILLED` finding (timed_out gate — red until Task 6, mark this single assert `// Task 6` and add it there if you prefer strict one-behavior-per-test). Run → FAIL (hangs / no timeout). Use a generous `#[ntest::timeout]`-equivalent or nextest's slow-timeout as backstop.
- [ ] **Step 4.2 (green):** In analyze.rs:
  - Consts (ADR-035 comment discipline — basis: Rust plugin parses the whole repo in `initialize`, ~40 MB/s syn throughput → 300 s covers ~10 M LOC; override: env; retune: if a legitimate repo trips it):

```rust
const DEFAULT_PLUGIN_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
fn plugin_handshake_timeout() -> std::time::Duration { /* mirror plugin_file_timeout(), env LOOMWEAVE_PLUGIN_HANDSHAKE_TIMEOUT_MS */ }
```

  - In `run_plugin_blocking`: replace `PluginHost::spawn` (4349) with `spawn_unhandshaken`; wrap child in `Arc<Mutex<_>>` and start the watchdog **immediately** (move the existing block at 4364-4372 up); then `watchdog.arm(handshake_timeout, WatchdogPhase::Handshake); let hs = host.handshake(); watchdog.disarm();`. On `Err`: stop+join watchdog, recover child, kill+wait (preserving host.rs's old reap-on-handshake-failure contract, now CLI-side via `reap_and_classify_exit`), and if `timed_out_phase() == Some(Handshake)` produce the timeout reason + finding (`phase: "handshake"` metadata, message “plugin {id} exceeded the handshake timeout ({n} ms) and was killed”) instead of the generic “refused handshake”. Thread `handshake_timeout`/`shutdown_timeout` params from the caller alongside `file_timeout` (resolved once near analyze.rs:709).
- [ ] **Step 4.3:** Test green; full `cargo nextest run -p loomweave-cli` green (existing spawn-error-path tests must still pass — the spawn-vs-handshake error split changes no observable reason strings except the new timeout case; fix any that assert “refused handshake” only if behavior is genuinely preserved). Commit: `feat(analyze): handshake deadline — hung initialize can no longer wedge the run/lock`.

### Task 5: Shutdown deadline

**Files:** Modify: `crates/loomweave-cli/src/analyze.rs` (watchdog stop/join at 4517-4520 currently precedes shutdown at 4544). Test: `analyze_hardening.rs`.

- [ ] **Step 5.1 (red):** Test `shutdown_hang_times_out_run_still_completes`: fixture with `LOOMWEAVE_FIXTURE_HANG_AT_SHUTDOWN=1`, `LOOMWEAVE_PLUGIN_SHUTDOWN_TIMEOUT_MS=500`. Assert: analyze exits 0; `runs.status == 'completed'`; the fixture's entity persisted; a `LMWV-INFRA-PLUGIN-SHUTDOWN-TIMEOUT` warning finding persisted; child reaped (no zombie). → FAIL (hangs).
- [ ] **Step 5.2 (green):** Reorder: keep the watchdog alive across shutdown. New sequence after the file loop: compute `timed_out_phase`-so-far → if `work_result.is_ok()`, `watchdog.arm(shutdown_timeout, WatchdogPhase::Shutdown); let _ = host.shutdown() /* Err already logged+killed today, keep */; watchdog.disarm();` → **then** stop/join watchdog and `Arc::try_unwrap` the child (move 4517-4523 below the shutdown call). If `timed_out_phase() == Some(Shutdown)`: push the warning finding (`LMWV-INFRA-PLUGIN-SHUTDOWN-TIMEOUT`, metadata plugin_id + timeout_ms) but DO NOT touch `work_result` (D7). Add the new subcode to `infra_severity` (analyze.rs:3090-3098) as `"WARN"` with a test assert beside the existing ones (6145). Shutdown timeout must NOT tick the crash-loop breaker (it rides the Ok path — assert in test: no `FINDING_DISABLED_CRASH_LOOP`, run completed).
- [ ] **Step 5.3:** Green + full crate tests + clippy. Commit: `feat(analyze): shutdown deadline — silent plugin can no longer hang analyze after work completes`.

### Task 6: Exit-classification fixes (timed_out gate + SIGABRT)

**Files:** Modify: `crates/loomweave-cli/src/analyze.rs` (`reap_and_classify_exit*` 4598-4707, call site 4582); `crates/loomweave-core/src/plugin/host_findings.rs` (new constant + constructor beside `oom_killed` at 264). Test: `analyze_hardening.rs` + unit tests near the existing reap tests (6261).

- [ ] **Step 6.1 (red):**
  - Integration test `spin_at_analyze_contained_by_file_watchdog`: fixture `LOOMWEAVE_FIXTURE_SPIN_AT_ANALYZE=1`, `LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS=500`. Assert: run terminal `failed` with timeout reason; exactly **one** `LMWV-PY-TIMEOUT` finding (`phase == "file"`); **zero** `LMWV-INFRA-PLUGIN-OOM-KILLED` findings (the gate); child reaped. → FAIL today on the zero-OOM assert (double-report) — and proves CPU-spin containment (acceptance gate 3).
  - Integration test `abort_at_analyze_classified_and_terminal`: fixture `LOOMWEAVE_FIXTURE_ABORT_AT_ANALYZE=1`. Assert: run terminal `failed`; a `LMWV-INFRA-PLUGIN-ABORTED` finding (metadata signal=6) persisted; no OOM finding; child reaped. → FAIL (no such finding exists).
- [ ] **Step 6.2 (green):**
  - `host_findings.rs`: `pub const FINDING_PLUGIN_ABORTED: &str = "LMWV-INFRA-PLUGIN-ABORTED";` + `pub fn aborted(plugin_id: &str, signal: i32) -> Self` with message “plugin {id} terminated abnormally (signal {signal}, SIGABRT — consistent with a stack overflow or explicit abort)”.
  - `reap_and_classify_exit*`: add a `suppress_kill_classification: bool` param (true when `timed_out_phase().is_some()` — the watchdog's own SIGKILL is not an OOM); signal 6 → `aborted` finding; keep 9||11 → `oom_killed` when not suppressed. Add `FINDING_PLUGIN_ABORTED` to `infra_severity` as `"ERROR"`.
  - Unit tests: extend the existing reap unit tests for the new branches (signal 6 → aborted; suppressed → no finding).
- [ ] **Step 6.3:** All green; commit: `fix(analyze): SIGABRT classified distinctly; watchdog kill no longer double-reports as OOM`.

### Task 7: Rust plugin — parse guard module (scan + caps + pinned stack)

**Files:** Create: `crates/loomweave-plugin-rust/src/parse_guard.rs`; modify `lib.rs` (mod decl).

- [ ] **Step 7.1 (red):** Unit tests in `parse_guard.rs` `#[cfg(test)]` (generators in-test, mirroring the empirical probe):

```rust
fn nested(open: &str, close: &str, n: usize, core: &str) -> String {
    format!("fn f() {{ let _ = {}{}{}; }}", open.repeat(n), core, close.repeat(n))
}
// scan_source(&nested("(", ")", 127, "1")) == Ok(())
// scan_source(&nested("(", ")", 200, "1")) == Err(GuardViolation::Depth { depth: 129, .. })  // reports cap+1 at trip point
// prefix bomb: format!("fn f() {{ let _ = {}1; }}", "!".repeat(2000)) → Err(GuardViolation::PrefixRun {..})
// banner comment with 300 '(' inside /* */ → Ok(())   // lexer skips comments
// string literal with 300 '[' → Ok(())                 // lexer skips strings
// raw string r#"((("# with brackets → Ok(())
// nested block comments /* /* */ */ → Ok(())
// parse_with_pinned_stack on nested mods depth 120 → Ok(syn::File)  // ~3 MB frames: would abort a 2 MiB thread, proves the 16 MiB pin
```

Run: `cargo nextest run -p loomweave-plugin-rust parse_guard` → FAIL (module missing).
- [ ] **Step 7.2 (green):** Implement:

```rust
//! Pre-parse guards: syn 2.x has no recursion limit — deeply nested input
//! overflows the stack and SIGABRTs the process. Empirical first-crash depths
//! at 8 MiB stack (syn 2.0.117): nested mods 337, blocks 480, parens 760,
//! unary `!` 2386. Caps below sit ≥4x under the worst case at the 16 MiB
//! pinned parse stack. Basis/override/retune per ADR-035; retune on syn major
//! bump or PARSE_STACK_BYTES change. See ADR-050.

pub const MAX_BRACKET_DEPTH: usize = 128;
pub const MAX_PREFIX_RUN: usize = 1024;
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const PARSE_STACK_BYTES: usize = 16 * 1024 * 1024;

pub enum GuardViolation { Depth { depth: usize }, PrefixRun { len: usize }, FileTooLarge { bytes: u64 } }

pub fn scan_source(src: &str) -> Result<(), GuardViolation> { /* state-machine lexer */ }

pub fn parse_with_pinned_stack(src: &str) -> Result<syn::File, syn::Error> {
    std::thread::Builder::new()
        .name("syn-parse".into())
        .stack_size(PARSE_STACK_BYTES)
        .spawn({ let src = src.to_owned(); move || syn::parse_file(&src) })
        .expect("spawn parse thread")
        .join()
        .expect("parse thread panicked")
}
```

Lexer states: `Normal`, `LineComment`, `BlockComment{depth}` (Rust block comments nest), `Str`, `RawStr{hashes}`, `Char`, lifetime-tick disambiguation (`'a` vs `'x'`: treat `'` followed by alphanumeric+no closing quote within 2 as lifetime). In `Normal`: `([{` increment depth (trip when > `MAX_BRACKET_DEPTH`), `)]}` saturating-decrement; bytes in `[b'!', b'&', b'*', b'-']` increment a consecutive-run counter (trip when > `MAX_PREFIX_RUN`), any other byte resets it. Keep it byte-oriented (`src.as_bytes()`), O(n), zero allocation. Misclassification is safe-by-construction: false positive → visible warning finding; false negative → contained by Task 4-6 floor.
- [ ] **Step 7.3:** Tests green; clippy pedantic green. Commit: `feat(plugin-rust): pre-parse depth/size guards + pinned 16MiB parse stack`.

### Task 8: Rust plugin — wire guards into analyze + symbol-table paths

**Files:** Modify: `crates/loomweave-plugin-rust/src/serve.rs` (analyze_one_file ~187-214), `extract.rs` (degraded path 232-274, parse at 123), `symbol_table.rs` (walk 84-99). Test: `crates/loomweave-plugin-rust/tests/parse_guard_e2e.rs` (NEW).

- [ ] **Step 8.1 (red):** Integration tests (tempdir + inline-generated sources, pattern from `host_integration.rs:25-31`):
  - `depth_bomb_degrades_to_finding_not_crash`: project with `bomb.rs` = 3000-deep nested parens (generated) + `ok.rs` benign. Drive the plugin (whichever harness `host_integration.rs` uses — real subprocess through `PluginHost`). Assert: plugin survives; `bomb.rs` yields exactly one module entity with `parse_status` `"depth_limit"` + one finding `subcode == "LMWV-RUST-DEPTH-LIMIT"`, severity warning; `ok.rs` extracts normally.
  - `unary_bomb_degrades`: `"!".repeat(4000)` expression file → same shape.
  - `oversize_file_degrades`: generated 11 MiB `.rs` (repeat a valid 1 KiB fn 11 000×) → `LMWV-RUST-FILE-TOO-LARGE`, `parse_status` `"file_too_large"`; file content never read into memory (size check via metadata — assert is behavioral: it completes fast and emits the finding).
  - `symbol_table_skips_bombs`: init over the bomb project → no abort, bombs contribute nothing.
  → FAIL (plugin aborts / no subcodes).
- [ ] **Step 8.2 (green):**
  - `extract.rs`: route the single production parse (123) through `parse_guard::parse_with_pinned_stack`. Extend the degraded path (232-274) so callers can supply a violation kind: new variants emitting `parse_status: "depth_limit"` / `"file_too_large"` and findings `LMWV-RUST-DEPTH-LIMIT` / `LMWV-RUST-FILE-TOO-LARGE` (mirror the `LMWV-RUST-SYNTAX-ERROR` JSON shape at 266-271, severity `"warning"`, message includes the measured depth/run/bytes and the cap).
  - `serve.rs` analyze path: before `read_to_string` (197), `fs::metadata` size check → over-cap degrade; after read, `scan_source` → violation degrade; clean → normal extraction.
  - `symbol_table.rs`: same size+scan checks in the walk (93-95) → `continue` (silent skip, matching the existing parse-Err skip semantics at 96-99, doc comment updated).
- [ ] **Step 8.3:** All plugin-rust tests green (`cargo nextest run -p loomweave-plugin-rust`), clippy green. Commit: `feat(plugin-rust): hostile input degrades to findings — depth bombs, prefix bombs, oversize files`.

### Task 9: ADR-050 + ADR-021 amendment

**Files:** Create: `docs/loomweave/adr/ADR-050-plugin-lifecycle-deadlines.md`. Modify: `docs/loomweave/adr/README.md` (row 050; ADR-021 row → “Accepted; amended by ADR-050”), `docs/loomweave/adr/ADR-021-plugin-authority-hybrid.md` (one-line note in the non-defences section pointing at ADR-050).

- [ ] **Step 9.1:** Write ADR-050 (Status: Accepted, Date: 2026-06-10, format per ADR-021's header). Content: context (rc4 shipped Rust plugin without hardening; handshake/shutdown hang gaps wedge run+lock); decision — three wall-clock lifecycle deadlines (handshake 300 s / file 120 s / shutdown 10 s; env overrides; ADR-035 discipline), exit-signal classification (6→ABORTED, 9||11→OOM gated on not-watchdog-kill), explicit **rejection of RLIMIT_CPU** (cannot cover blocked-idle hangs; pyright risk) superseding ADR-021's “breaker catches runaway loops” rationale (the breaker only sees *crashed* plugins, never hung ones), Rust-plugin parse guards (caps + empirical basis table + pinned stack); consequences (incl. the trickle-bytes residual: a hostile plugin can still consume each full deadline; the per-run-per-plugin breaker granularity unchanged). Glossary check per README.md:70-78: new subcodes `LMWV-INFRA-PLUGIN-ABORTED`, `LMWV-INFRA-PLUGIN-SHUTDOWN-TIMEOUT`, `LMWV-RUST-DEPTH-LIMIT`, `LMWV-RUST-FILE-TOO-LARGE` — no clash (verdict recorded in the ADR).
- [ ] **Step 9.2:** Commit: `docs(adr): ADR-050 plugin lifecycle deadlines; amend ADR-021 non-defence rationale`.

### Task 10: Hostile-corpus e2e (acceptance gate 1)

**Files:** Create: `tests/e2e/hostile_corpus_rust.sh` (conventions from `sprint_1_walking_skeleton.sh`: `set -euo pipefail`, REPO_ROOT/CARGO_BUILD env overrides, mktemp + trap cleanup, log/fail helpers, sqlite3 string asserts).

- [ ] **Step 10.1:** Script generates the corpus in the temp project via `python3`/printf (never checked in): (a) `deep_parens.rs` 3000-deep nesting (pre-fix this SIGABRTed the child); (b) `unary_bomb.rs` 4000 `!`; (c) `big.rs` ~12 MiB of valid repeated fns; (d) `broken.rs` syntactically invalid; (e) `benign.rs` one real fn. Run `loomweave install` + `loomweave analyze .` with the **release** binary. Assert via sqlite3 against `.weft/loomweave/loomweave.db`: `runs.status == 'completed'` (no plugin crash — guards degraded everything) and exactly one runs row (no stuck `running`); findings contain `LMWV-RUST-DEPTH-LIMIT` (×2), `LMWV-RUST-FILE-TOO-LARGE`, `LMWV-RUST-SYNTAX-ERROR`; `benign.rs` entity present; **no** `FINDING_DISABLED_CRASH_LOOP` / OOM / TIMEOUT findings (breaker state asserted: untripped); analyze exit code 0. Echo `PASS:` line.
- [ ] **Step 10.2:** Run it: `bash tests/e2e/hostile_corpus_rust.sh` → PASS. Commit: `test(e2e): hostile-corpus acceptance gate for rust-plugin hardening`.

### Task 11: Full gates + smokes (acceptance gates 4-6)

- [ ] **Step 11.1:** From the worktree root, run the full CLAUDE.md CI floor; paste real output:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
for s in scripts/check-*.py; do python3 "$s" || exit 1; done
```

- [ ] **Step 11.2:** Run the three e2e smokes + the new one: `bash tests/e2e/sprint_1_walking_skeleton.sh && bash tests/e2e/sprint_2_mcp_surface.sh && bash tests/e2e/phase3_subsystems.sh && bash tests/e2e/hostile_corpus_rust.sh`.
- [ ] **Step 11.3:** Fix anything red; only then proceed to merge (exit protocol lives outside this plan: `--no-ff` into `rc4`, push, close clarion-7bc08e05c0 with `--reason`, file D9 follow-up tickets, remove worktree).

## Self-review notes

- Acceptance-gate mapping: gate 1 → Task 10; gate 2 (hung plugin) → Tasks 4+5; gate 3 (CPU spin) → Task 6 spin test; gate 4 (Python unharmed) → Task 11 (host integration tests run in nextest); gate 5/6 → Task 11.
- The handshake-timeout finding reuses `PLUGIN_TIMEOUT_RULE_ID` (`LMWV-PY-TIMEOUT`) with `phase` metadata — the historical name is plugin-agnostic in practice (severity table analyze.rs:3095); renaming is out of scope.
- Implementers MUST re-verify cited line numbers before editing — they drift; the anchors are from a 2026-06-10 read of `feat/plugin-host-hardening` @ 510a032.
