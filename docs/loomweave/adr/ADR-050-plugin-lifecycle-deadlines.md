# ADR-050: Plugin Lifecycle Deadlines and Abnormal-Exit Classification

**Status**: Accepted
**Date**: 2026-06-10
**Deciders**: john@foundryside.dev (with Claude)
**Context**: rc4 shipped the Rust language plugin on-by-default (merge `e53fc91`) with its adversarial-input hardening explicitly deferred (ticket clarion-7bc08e05c0). Two failure families were open: a plugin that *hangs* during `initialize` or `shutdown` escapes every existing deadline and wedges the project (the runs row stays `running`, the fail-fast analyze advisory lock then refuses every later analyze); and a plugin that *aborts* on hostile input (syn 2.x has no recursion limit — a nested-bracket bomb overflows the parse stack and SIGABRTs the process) was either unclassified or, when the watchdog itself killed a hung plugin, double-reported as an OOM kill.
**Amends**: [ADR-021](./ADR-021-plugin-authority-hybrid.md) — corrects the "Per-plugin CPU cap" non-defence rationale (see §3).

## Summary

Three decisions, all landed on `feat/plugin-host-hardening`:

1. **Lifecycle deadlines (host side).** The CLI-side `PluginWatchdog` (analyze.rs) becomes phase-aware and covers the **entire** plugin lifecycle with three wall-clock deadlines: handshake **300 s**, per-file **120 s** (pre-existing, unchanged), shutdown **10 s** — each env-overridable, each documented under the ADR-035 constant discipline. A hung `initialize` can no longer wedge the run record and the analyze lock; a hung `shutdown` can no longer hang analyze after the work completed.
2. **Abnormal-exit classification.** SIGABRT (signal 6) gets its own finding, `LMWV-INFRA-PLUGIN-ABORTED` (ERROR) — the stack-overflow / explicit-`abort()` signature. The SIGKILL/SIGSEGV → `LMWV-INFRA-PLUGIN-OOM-KILLED` classification (ADR-021 §2d) is **gated on "no lifecycle deadline fired"**, so the watchdog's own SIGKILL stops double-reporting a single timeout as TIMEOUT + OOM.
3. **Rust-plugin parse guards (prevention, not just containment).** The Rust plugin pre-scans every in-scope file with a minimal byte-oriented lexer — bracket-depth cap **128**, prefix-operator-run cap **1024** — and caps file size at **10 MiB** (checked via `fs::metadata`, never read). Over-cap files **degrade** to one `module` entity (`parse_status` `"depth_limit"` / `"file_too_large"`) plus one warning finding (`LMWV-RUST-DEPTH-LIMIT` / `LMWV-RUST-FILE-TOO-LARGE`); the init-time symbol-table walk silently skips them. Every syn parse runs on a dedicated thread with a **pinned 16 MiB stack**, so crash thresholds stop depending on the inherited environment stack.

An explicit **rejection of `RLIMIT_CPU`** is recorded in §3, superseding ADR-021's stated rationale for that non-defence.

## Context

The plugin host (ADR-002, ADR-021) supervises each language plugin as a subprocess speaking Content-Length-framed JSON-RPC. Before this ADR, the only liveness control was the per-file watchdog: armed around each `analyze_file` exchange, 120 s default. Two lifecycle phases were uncovered:

- **Handshake.** `PluginHost::spawn` ran the blocking `initialize` round-trip internally, *before* the CLI could create the watchdog — and a plugin may legitimately do whole-repo work inside `initialize` (the Rust plugin builds its project symbol table there). A plugin that hung in `initialize` blocked `analyze` forever. Worse, the failure is *sticky*: the runs row stays `running`, and the analyze advisory lock (`analyze_lock.rs`, fs2) fails fast on a held lock while the stale-run sweep runs only *after* lock acquisition — so one hung handshake wedged every subsequent analyze of the project.
- **Shutdown.** The watchdog was stopped and joined *before* `host.shutdown()`, whose synchronous `read_response_matching` blocks with no deadline. A plugin that completed all its work and then went silent at `shutdown` hung analyze indefinitely — after the entities were already durable.

Separately, exit classification had two defects, both verified empirically:

- Deeply nested Rust input makes syn 2.0.117 recurse without limit; the Rust guard page turns the overflow into a SIGABRT (exit 134, never SIGSEGV). The terminal path was already sound (pipe EOF → `PluginRunError` → SoftFailed → `CommitRun(Failed)`) but the signal was unclassified — operators saw a generic transport error.
- The reap path emitted `LMWV-INFRA-PLUGIN-OOM-KILLED` unconditionally for signal 9. The watchdog kills with SIGKILL, so every watchdog timeout *also* produced a spurious OOM finding.

Measured syn 2.0.117 first-crash depths at an 8 MiB stack (probe generators preserved in `parse_guard.rs` tests): nested `mod` 337, `if` 407, blocks 480, arrays 687, parens 760, unary `!` 2386. Left-associative binary chains parse iteratively (safe at 20 000). A byte-depth scan catches every bracket bomb (floor 337 ≫ cap 128) but misses unary bombs (bracket depth ~1) — hence the separate prefix-run cap. A real lexer (not a naive byte count) is required because banner comments (`/****…*/`) and string literals would otherwise false-positive.

## Decision

### §1 Lifecycle deadlines

`PluginHost::spawn` is split into `spawn_unhandshaken` (launch only) plus the existing `spawn` wrapper (launch + handshake), so the CLI can interpose its watchdog before the first blocking read. `run_plugin_blocking` uses `spawn_unhandshaken`, wraps the child in `Arc<Mutex<Child>>`, starts the watchdog **immediately**, and arms it per phase. The watchdog (`PluginWatchdog`, 50 ms poll) records the **first** expired `WatchdogPhase` (`Handshake` / `File` / `Shutdown`) and kills the child, which unblocks the host's synchronous read.

| Phase | Default | Env override | ADR-035 basis |
|---|---|---|---|
| Handshake | **300 s** (`DEFAULT_PLUGIN_HANDSHAKE_TIMEOUT`) | `LOOMWEAVE_PLUGIN_HANDSHAKE_TIMEOUT_MS` | A plugin may do whole-repo work in `initialize`; at syn's ~40 MB/s parse throughput 300 s covers a ~10 M-LOC repo. Scales with repo size, not per-file work. |
| Per-file | **120 s** (`DEFAULT_PLUGIN_FILE_TIMEOUT`, pre-existing) | `LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS` | Healthy single-file extraction completes well under a second; minutes of silence is a hang. |
| Shutdown | **10 s** (`DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT`) | `LOOMWEAVE_PLUGIN_SHUTDOWN_TIMEOUT_MS` | After the file loop there is no legitimate work left; shutdown is pure exit etiquette. |

Retune triggers are documented at each constant (raise if a legitimate repo / analyzer / plugin trips it in practice).

**Timeout outcomes differ by phase:**

- **Handshake / per-file timeout** → run terminal (`failed`), reason names the phase and the budget, and a `LMWV-PY-TIMEOUT` finding (ERROR) is persisted with `metadata.phase` = `"handshake"` / `"file"` plus `plugin_id` and `timeout_ms`. (The historical `LMWV-PY-` rule-id is plugin-agnostic in practice; renaming it is out of scope.) `spawn_unhandshaken`'s contract — caller owns kill+reap on handshake failure, `Child::Drop` does not reap on Unix — is preserved CLI-side via `reap_and_classify_exit`; no zombie survives any path.
- **Shutdown timeout** → kill + reap + `LMWV-INFRA-PLUGIN-SHUTDOWN-TIMEOUT` **warning** finding (`metadata.plugin_id`, `timeout_ms`); the run outcome is **unchanged** — a completed run stays `completed`. The entities are durable and the data is whole; only the plugin's exit etiquette failed. Because it rides the Ok path, a shutdown timeout never ticks the crash-loop breaker.

The `plugin_limits.*` `loomweave.yaml` surface documented by ADR-021 remains unimplemented; the env override follows the existing `LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS` precedent. The yaml surface is a filed follow-up, not this ADR.

### §2 Abnormal-exit classification

`reap_and_classify_exit` waits on the child (5 s reap backstop) and classifies the exit status:

- **Signal 6 (SIGABRT)** → `LMWV-INFRA-PLUGIN-ABORTED` (ERROR), `metadata.signal = 6`, message naming the stack-overflow / explicit-abort signature. Never suppressed: the watchdog kills with SIGKILL, so an abort is always the plugin's own.
- **Signal 9 / 11 (SIGKILL / SIGSEGV)** → `LMWV-INFRA-PLUGIN-OOM-KILLED` (ADR-021 §2d), **unless** a lifecycle deadline fired for this child (`suppress_kill_classification`) — in that case the SIGKILL is the host's own and the timeout finding is the root cause. The 9 ∥ 11 → OOM conflation itself (a genuine SIGSEGV is not an OOM) is a known refinement, filed as follow-up.
- Other signals / non-zero exits: warn log, no finding — the cause is ambiguous without more bookkeeping.

### §3 `RLIMIT_CPU` rejected; ADR-021 non-defence rationale corrected

ADR-021 declined a per-plugin CPU cap with the rationale "plugin crash-loop breaker already catches runaway loops." That rationale is **false for hung plugins**: the breaker only observes *crashed* plugins (processes that exited); a plugin blocked idle on a read, or spinning inside one request, never crosses the breaker at all. The corrected position, which this ADR records:

- `RLIMIT_CPU` cannot cover **blocked-idle** hangs (a deadlocked or wedged plugin consumes no CPU and the limit never fires) — the most damaging observed failure mode, since it wedged the run record and the analyze lock.
- `RLIMIT_CPU` *would* endanger legitimately CPU-heavy plugin work (pyright's long whole-project passes).
- A **wall-clock** deadline covers both the busy-loop and the blocked-idle case, with per-phase budgets that match each phase's legitimate cost envelope.

So the non-defence stands — there is still no `RLIMIT_CPU` — but on the corrected grounds above; the liveness control is §1's wall-clock deadlines. ADR-021's non-defence list carries a pointer here.

### §4 Rust-plugin parse guards

`crates/loomweave-plugin-rust/src/parse_guard.rs` (constants carry ADR-035 basis/override/retune comments; retune on a syn major bump or a `PARSE_STACK_BYTES` change):

| Guard | Constant | Value | Basis |
|---|---|---|---|
| Bracket depth | `MAX_BRACKET_DEPTH` | 128 | Lowest measured syn first-crash depth is 337 (nested mods, 8 MiB stack); 128 sits ≥4x under every measured floor at the 16 MiB pinned stack. |
| Prefix-operator run (`!`, `&`, `*`, `-`) | `MAX_PREFIX_RUN` | 1024 | Unary bombs carry bracket depth ~1 so the depth cap misses them; measured first-crash run 2386 at 8 MiB (~4.7 k at 16 MiB). |
| File size | `MAX_FILE_BYTES` | 10 MiB | Manifest declares `expected_max_rss_mb = 128`; a multi-MiB file's string + token stream + AST can blow `RLIMIT_AS` → avoidable child kill. Checked via `fs::metadata` **before** reading. |
| Parse stack | `PARSE_STACK_BYTES` | 16 MiB | Pinning makes the crash threshold independent of the inherited environment stack; doubles the common 8 MiB default, putting every measured floor ≥4x above the scan caps. |

- **`scan_source`** is a minimal byte-oriented O(n) zero-allocation lexer, not a parser: it skips line comments, *nesting* block comments, string literals, raw strings (`r#"…"#`, `br…`, `cr…`), and char literals, and disambiguates a char literal from a lifetime tick — so banner comments and bracket-laden strings cannot false-positive. Misclassification is safe by construction: a false positive degrades the file to a visible warning finding; a false negative is contained by §1+§2's floor (the SIGABRT is terminal and now classified).
- **Analyze path** (`serve.rs::analyze_one_file`): size check → scan → clean parse. A violation degrades to exactly one `module` entity with `parse_status` `"depth_limit"` / `"file_too_large"` plus one **warning** finding `LMWV-RUST-DEPTH-LIMIT` / `LMWV-RUST-FILE-TOO-LARGE` whose message names the measured depth/run/bytes and the cap — mirroring the existing `LMWV-RUST-SYNTAX-ERROR` degraded shape, under the manifest's `syntax_degraded_module` role. The plugin never crashes; the rest of the project analyzes normally.
- **Symbol-table walk** (`symbol_table.rs`): the same size+scan checks `continue` past bombs (silent skip, matching the existing parse-error skip semantics) — a successful `initialize` over a bomb-bearing project is itself proof the walk cannot overflow.
- **Pinned stack** (`with_pinned_stack`): every production syn parse — and the recursive AST walk that consumes the `!Send` `syn::File` — runs on a scoped thread named `syn-parse` with a 16 MiB stack. On a failed thread spawn (EAGAIN under `RLIMIT_NPROC`) it falls back to parsing inline on the caller's stack with a once-only warning; the scan caps still apply.

## Alternatives Considered

### Alternative 1: `RLIMIT_CPU` at spawn

Apply a CPU-time rlimit in the forked child alongside the existing `RLIMIT_AS`.

**Pros**: zero host-side threading; kernel-enforced; catches busy-loops even if the watchdog thread itself misbehaves.

**Cons**: cannot fire for blocked-idle hangs (no CPU consumed) — the exact failure that wedges the run/lock; a single budget cannot distinguish a hung plugin from pyright's legitimately long CPU-heavy passes; SIGXCPU classification adds another ambiguous exit signature.

**Why rejected**: covers the less-damaging half of the hang space while risking the flagship legitimate workload. See §3.

### Alternative 2: floor-only (containment without the Rust-plugin guards)

Ship §1+§2 and let hostile files SIGABRT the plugin: the path is terminal and now classified.

**Pros**: no lexer to maintain; no false-positive surface.

**Cons**: one hostile file fails the **entire run** (SoftFailed → `failed`) and, repeated, trips the crash-loop breaker — instead of costing one degraded entity. An attacker (or a build artifact) holding one pathological file would deny analysis of the whole tree.

**Why rejected**: degrade-per-file preserves the rest of the corpus; the e2e acceptance gate (`tests/e2e/hostile_corpus_rust.sh`) requires a hostile tree to complete with `runs.status = 'completed'`.

### Alternative 3: `loomweave.yaml` `plugin_limits.*` config surface for the deadlines

Implement the ADR-021-documented (but never-built) `plugin_limits.*` yaml block and hang the three deadlines off it.

**Pros**: one discoverable operator surface; consistent with ADR-021's documented intent.

**Cons**: the yaml surface does not exist anywhere yet (its only code mention is a "lands in WP6" comment); building it inside a hardening change couples an emergency liveness fix to a config-plumbing project.

**Why rejected (deferred)**: env overrides follow the existing `LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS` precedent; the yaml surface is a filed follow-up that can subsume the env vars later without breaking them.

## Consequences

### Positive

- A hostile or pathological source tree cannot hang `analyze`, crash the host, or wedge the run record / analyze advisory lock. Every lifecycle phase has a deadline; every abnormal exit has a classification; every guard rejection is a visible finding.
- A hung `initialize` no longer bricks the project until manual intervention — the acute pre-ADR failure.
- Watchdog kills no longer masquerade as OOM events; SIGABRT (the hostile-input signature) is distinctly visible, so "plugin was attacked / hit a parser bug" and "plugin ran out of memory" are separable in compat reports.
- The Rust plugin survives bracket bombs, unary bombs, and oversize files at the cost of one degraded module entity + one WARN finding each, and the crash-loop breaker stays untripped (proved end-to-end by `tests/e2e/hostile_corpus_rust.sh` and `crates/loomweave-plugin-rust/tests/parse_guard_e2e.rs`).
- Fixture-driven integration coverage: `loomweave-plugin-fixture` gained env-triggered misbehaviors (`LOOMWEAVE_FIXTURE_HANG_AT_INITIALIZE` / `HANG_AT_SHUTDOWN` / `SPIN_AT_ANALYZE` / `ABORT_AT_ANALYZE`) exercised by `crates/loomweave-cli/tests/analyze_hardening.rs`.

### Negative

- **Trickle residual**: a hostile-but-not-hung plugin can still consume each *full* deadline — up to 300 s at handshake plus 120 s per file plus 10 s at shutdown — before each kill. The run is bounded, not fast. Accepted: ADR-021's threat model already trusts plugin *provenance*; these deadlines bound damage, they do not police throughput.
- The scan caps can false-positive legitimately machine-generated code (a real 129-deep bracket nest or a 1025-long operator run). The cost is one degraded file with a visible WARN finding naming the cap — not silent data loss, and not a crashed run. The caps carry retune triggers.
- The per-run-per-plugin crash-loop breaker granularity is unchanged: timeout-killed plugins still tick it (by design — repeated hangs *should* disable the plugin for the run), and the breaker still cannot see cross-run patterns.
- The SIGKILL ∥ SIGSEGV → OOM conflation is narrowed (watchdog kills suppressed) but not resolved; a genuine SIGSEGV still reports as OOM. Filed as a follow-up alongside the `plugin_limits.*` yaml surface and the Python-plugin deep-recursion characterization.

### Neutral

- `LMWV-PY-TIMEOUT` is reused for handshake timeouts (with `metadata.phase`) rather than minting a Rust-agnostic rule-id; the name is historical and the severity table already treats it plugin-agnostically.
- `PluginHost::spawn` remains the one-call convenience for tests and non-CLI callers; `spawn_unhandshaken` is additive, with the kill+reap-on-handshake-failure contract documented on both.
- The host-side floor (§1+§2) protects against **every** plugin, including future third-party ones; §4 is Rust-plugin-internal and other plugins are free to adopt analogous guards.

## Weft vocabulary verdict (per ADR index acceptance rule)

New finding subcodes: `LMWV-INFRA-PLUGIN-ABORTED`, `LMWV-INFRA-PLUGIN-SHUTDOWN-TIMEOUT`, `LMWV-RUST-DEPTH-LIMIT`, `LMWV-RUST-FILE-TOO-LARGE`. All four are new **values inside already-registered namespaces** — the core-owned `LMWV-INFRA-` host-finding family (ADR-021/ADR-017) and the Rust plugin's manifest-declared `LMWV-RUST-` prefix (ADR-022 rule-ID namespacing). No sibling product uses these identifiers; the `rule_id` field they ride is a managed term since ADR-017. Verdict: **`no clash`** — no `glossary.md` change required (model: ADR-040, ADR-049).

## Related Decisions

- [ADR-002](./ADR-002-plugin-transport-json-rpc.md) — the subprocess transport whose blocking reads these deadlines bound; the crash-loop breaker that timeout kills (but not shutdown timeouts) tick.
- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — **amended by this ADR**: the "Per-plugin CPU cap" non-defence rationale is corrected (§3); the §2d OOM classification is gated on "not a watchdog kill" (§2). The Layer-2 enforcement floor itself is unchanged.
- [ADR-017](./ADR-017-severity-and-dedup.md) — severity vocabulary the new subcodes map into (ABORTED/timeout → ERROR; shutdown-timeout and the `LMWV-RUST-*` guards → WARN).
- [ADR-022](./ADR-022-core-plugin-ontology.md) — the `rule_id_prefix` discipline under which `LMWV-RUST-DEPTH-LIMIT` / `LMWV-RUST-FILE-TOO-LARGE` are plugin-owned.
- [ADR-035](./ADR-035-operational-tuning-discipline.md) — basis/override/retune discipline applied to every constant this ADR introduces.
- [ADR-049](./ADR-049-rust-qualname-canonicalization.md) — the syn-backend decision that makes the recursion-limit gap (and therefore §4) the Rust plugin's to own.

## References

- Implementation plan: `docs/superpowers/plans/2026-06-10-plugin-host-hardening.md` (ticket clarion-7bc08e05c0; decisions D1–D9, empirical syn crash-depth probe).
- `crates/loomweave-cli/src/analyze.rs` — `PluginWatchdog` / `WatchdogPhase`, the three deadline constants, `reap_and_classify_exit`.
- `crates/loomweave-core/src/plugin/host.rs` — `spawn_unhandshaken` / `spawn` split; `host_findings.rs` — `FINDING_PLUGIN_ABORTED`.
- `crates/loomweave-plugin-rust/src/parse_guard.rs` — caps, lexer, `with_pinned_stack`, probe-mirroring tests.
- `tests/e2e/hostile_corpus_rust.sh` — hostile-corpus acceptance gate.
