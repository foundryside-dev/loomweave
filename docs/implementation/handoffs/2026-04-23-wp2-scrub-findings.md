# WP2 scrub — findings triage (2026-04-23)

Outcome of the Phase-1 review sweep requested by
`docs/superpowers/handoffs/2026-04-23-wp2-full-scrub-handoff.md`.
Six independent reviewers (Rust reviewer, threat analyst, unsafe
auditor, test-suite reviewer, coverage-gap analyst, bug-triage
specialist) ran over commit range `ad8d4ce..a1cc3be` (HEAD
`sprint-1/wp2-plugin-host` = `a1cc3be`).

This doc captures every finding, classifies it, and commits to a
disposition. The **FIX-NOW** slate is what this session will close
before A.2 can tick; **FILE-ONLY** entries get new filigree issues for
Sprint 2+ with trigger conditions; **DISMISS** entries get closed or
skipped with a rationale.

## Summary verdict

**A.2 is not ready to tick at session start.** Three findings
contradict explicit A.2 signoff claims (A.2.3, A.2.7) and one corrupts
run-row terminal state on any plugin-task panic.

After the FIX-NOW slate (9 fixes, estimated 9 commits) and assuming a
decision on the C1 discriminator below, A.2 is ready to recommend for
tick.

**Unsafe-block audit verdict: SOUND.** The single unsafe in
`host.rs::spawn` (pre_exec setrlimit) passes async-signal-safety,
no-alloc, no-drop, no-unwind, and failure-path checks. No action.

## Disposition table

| # | Finding | Source | Severity | Disposition | Notes |
|---|---------|--------|----------|-------------|-------|
| 1 | `spawn_blocking` JoinError in `analyze.rs:230` bypasses CommitRun/FailRun → `runs.status` stuck at `'running'` on plugin-task panic | Rust F1 | CRITICAL | **FIX-NOW** | Same category as 37b56d9 (exit-code decoupling); blocks A.2 |
| 2 | `EntityCapExceeded` never reached through `analyze_file` in any test | Coverage 1 | HIGH | **FIX-NOW** | A.2.3 signoff requires positive+negative tests; host wiring (`host.rs:674–685`) untested |
| 3 | Crash-loop breaker trip is mock-proves-itself (`breaker_06`) — never drives `analyze.rs` wiring added in `a1cc3be` | Test C-1 / Coverage 5 | HIGH | **FIX-NOW** | A.2.7 signoff literally states "test with `MockPlugin::new_crashing`"; existing test does not exercise production wiring |
| 4 | `analyze_without_plugins_writes_skipped_run_row` leaks parent `$PATH` | Test C-2 | HIGH | **FIX-NOW** | Any machine with `clarion-plugin-fixture` installed fails this; A.1.8 reliability bomb |
| 5 | `make_request` / `make_notification` in `protocol.rs:298,314` are `pub` with `.expect()` panic | Rust F3 | MEDIUM | **FIX-NOW** | Same shape as filed 0b1f8bc940 (`pub` footgun); 2-line visibility fix |
| 6 | `walk_dir` silently swallows per-entry I/O errors (`analyze.rs:672–674`) | Rust F4 | MEDIUM | **FIX-NOW** | Same WP1 anti-pattern as `read_applied_versions`; `warn!` + counter |
| 7 | T6 path-escape breaker test uses `any()` instead of pinning count to 10 | Test C-4 | MEDIUM | **FIX-NOW** | Bundle with existing `clarion-f45dd6056f` (T3 has the same weakness) — one commit, closes both |
| 8 | T8 oversize tests cover only `qualified_name` + `source.file_path`; `id` and `kind` untested | Test C-5 | MEDIUM | **FIX-NOW** | Two new tests, no production change |
| 9 | `t9_handshake_failure_exits_cleanly_without_hanging` claims more than it tests | Test C-3 | LOW | **FIX-NOW** | Rename + comment; no behaviour change |
| 10 | Spawn uses `manifest.plugin.executable` not `DiscoveredPlugin.executable` | Threat C1 | HIGH (debate) | **USER DECISION** | Gates A.2.4's "L9 locked" if we accept the hybrid-authority framing |
| 11 | clarion-6cde4f37d7 (JailError::Io/NonUtf8Path allegedly don't tick breaker) | prior review | — | **DISMISS** | `host.rs:658` calls `record_escape()` unconditionally after the 3-arm match; filed against older code |
| 12 | clarion-c850c27f33 (`entities.len() as u64` unchecked cast) | prior review | — | **DISMISS** | `usize as u64` is lossless on 64-bit; clippy pedantic does not flag; purely cosmetic |
| 13 | `read_frame` has no read deadline → CLI hangs forever on plugin hang | Rust F2 / Threat S1 | HIGH | **FILE-ONLY** | Requires per-op deadline + kill; design work, not quick fix. Trigger: first production run that hits a real-world plugin hang (or Sprint 2 hardening) |
| 14 | `to_string_lossy` at wire boundary silently mangles non-UTF-8 paths | Rust F5 | MEDIUM | **FILE-ONLY** | Should fail loudly as `JailError::NonUtf8Path`; small fix, but changes an error surface |
| 15 | `ontology_version` field not validated at handshake | Rust F6 | MEDIUM | **FILE-ONLY** | Retrofit cost when WP6 cache-keying lands. Trigger: WP6 kickoff |
| 16 | `stderr = Stdio::inherit()` → plugin can DoS terminal, inject log lines | Threat C2 | MEDIUM | **FILE-ONLY** | Trigger: before community plugins are enabled |
| 17 | `analyze_file` response body deep-cloned twice (`result_val.cloned()` + `from_value`) | Threat C3 | MEDIUM | **FILE-ONLY** | Structural fix: typed `AnalyzeFileResult`. Trigger: elspeth-scale ingest / WP4 |
| 18 | `do_shutdown` response-id validation loses sync with queued plugin frames → breaker-kill path is best-effort | Threat C4 | MEDIUM | **FILE-ONLY** | Should discard-until-match with frame budget. Trigger: adversarial plugin threat model |
| 19 | Response IDs not validated against outstanding set — stale frames surface as protocol errors on the next call | Threat C5 | LOW | **FILE-ONLY** | Mostly robustness; same fix as #18 |
| 20 | No `RLIMIT_NOFILE` / `RLIMIT_NPROC` — plugin can fork/open-sockets unbounded during initialize | Threat P2 | MEDIUM | **FILE-ONLY** | Clear `pre_exec` addition; defer to decide default NPROC knob |
| 21 | Discovery does not refuse world-writable PATH dirs / plugin.toml | Threat P3 | LOW | **FILE-ONLY** | Operator-env dependent; trigger = multi-tenant CI support |
| 22 | Content-Length ceiling not exercised through `PluginHost` in any test | Coverage 3 | MEDIUM | **FILE-ONLY** | Test-only; drive `MockBehaviour::Oversize` through `PluginHost::connect` and assert `HostError::Transport` |
| 23 | Cross-plugin identity fabrication (plugin A emits `id` with plugin B's prefix) not tested | Coverage 4 | MEDIUM | **FILE-ONLY** | Test only; T4 covers wrong `qualified_name`, not wrong `plugin_id` segment |
| 24 | RLIMIT_AS kill not observed end-to-end (real subprocess exceeding limit) | Coverage 6 | LOW | **FILE-ONLY** | Deliberately hard test; unit + code review sufficient for Sprint 1 |
| 25 | `HostError::Protocol` on response-id mismatch in `handshake` and `do_shutdown` untested | Coverage 7, 8 | LOW | **FILE-ONLY** | Same fix surface as #18/19 |
| 26 | clarion-a287217267 P2 (extra maps unbounded) | existing | P2 | **FILE-ONLY** | Keep; part of Cluster A. Trigger: before first non-fixture plugin |
| 27 | clarion-106ab51bc9, 920609be1f, 0b1f8bc940 (deserialisation ceilings) | existing | P3 | **FILE-ONLY** | Cluster A batch; fix together |
| 28 | clarion-928349b60f, 5164e4990b (canonical-vs-raw paths) | existing | P3 | **FILE-ONLY** | Cluster B; WP4 briefing-read trigger |
| 29 | clarion-e190f1e72b, 5578157797 (missing assertions) | existing | P3 | **FILE-ONLY** | Cluster C test-quality batch, Sprint 2 open |
| 30 | clarion-f30acbbb31 (double-shutdown guard) | existing | P3 | **FILE-ONLY** | Polish; documentary guard works today |
| 31 | clarion-48c5d06578 (poisoned-inbox drain strategy) | existing | P3 | **FILE-ONLY** | Forward-flag for WP4 supervisor |

## Cluster root-cause notes

- **Cluster A — "serde boundary with no ceiling"** (#14 omitted — that's a different class). Members: 26, 27. Fix pattern: one helper `bounded_deserialize` applied at every plugin-controlled serde entry point. Single-policy review.
- **Cluster B — "canonical-vs-raw path discipline"**. Members: 28. Fix pattern: canonicalize-once + reopen-against-pinned-root-fd for future briefing reads.
- **Cluster C — "tests are existential where they should be universal/negative"**. Members: 29, plus this session's #7 (T6 count pinning). After the T6 fix + closing f45dd6056f, the discipline is established; the remaining two issues become mechanical.

## FIX-NOW slate — ordered

Each item ships as one commit. TDD discipline: failing test first, then
code. ADR-023 gates green on every commit (fmt, clippy pedantic,
nextest, doc, deny).

1. **Finding #1** — JoinError handling in `analyze.rs`. New filigree
   issue first (CRITICAL bug). Test: inject a panic into the plugin-task
   path (simplest: a test-only seam in `run_plugin_blocking` that panics
   before return), assert `runs.status='failed'` + exit 1.
2. **Finding #3** — E2E crash-loop breaker trip test. New filigree
   issue. Mock plugin that crashes every call + 4+ input files, run
   `clarion analyze`, assert `FINDING_DISABLED_CRASH_LOOP` and subsequent
   plugins skipped.
3. **Finding #2** — `EntityCapExceeded` host-level test. New filigree
   issue. Construct `PluginHost` with `EntityCountCap::new(2)`, feed
   mock emitting 3 entities, assert `HostError::EntityCapExceeded` and
   `FINDING_ENTITY_CAP_EXCEEDED` finding present.
4. **Finding #4** — PATH leak fix in `analyze_without_plugins_...`.
   New filigree issue. Add `.env("PATH", "")` to both install and
   analyze invocations.
5. **Finding #7** — T6 + T3 count pinning. Closes existing
   f45dd6056f plus a new sibling issue for T6. Replace `any(...)` with
   `filter(...).count() == 10` in both tests.
6. **Finding #8** — T8c + T8d for `id` and `kind` oversize. New
   filigree issue. Two small additional tests mirroring T8b.
7. **Finding #5** — `pub(crate)` on `make_request`/`make_notification`.
   New filigree issue. Change visibility; update imports; no test change
   (existing tests cover the call sites).
8. **Finding #6** — `walk_dir` warn+counter. New filigree issue. Log
   skipped entries; include skip count in run summary log line.
9. **Finding #9** — rename `t9_...` test. New filigree issue. Rename
   and comment-clarify what it actually asserts.

## Disposition discipline

- Each FIX-NOW finding gets its own filigree issue created first (so
  the commit can cite it) and closed with the fix commit SHA in
  `fix_verification`.
- Each FILE-ONLY finding gets one new filigree issue per row (except
  existing ones #26–#31, which are kept open). Issues use
  `--label=sprint:1 --label=wp:2` plus any ADR labels. Trigger condition
  in the issue body.
- clarion-6cde4f37d7 closes with a comment pointing at `host.rs:658`.
- clarion-c850c27f33 closes with a comment explaining `usize as u64`
  is lossless on 64-bit (or stays open as a chore if the user wants).

## Decision needed from the user — Threat C1

**Finding #10** is the discriminating call. `host.rs:382` constructs
`Command::new(&manifest.plugin.executable)` — the host runs whatever
path the manifest names, not the `clarion-plugin-*` binary discovery
found on `$PATH`. A malicious or simply misconfigured `plugin.toml`
can put `executable = "/bin/sh"` or `executable = "python3"` and the
host will run that.

**Two framings**:

- **Operator-trust**: plugin.toml ships alongside the binary the
  operator installed; they chose both. Not an escalation beyond what
  the plugin could do anyway.
- **Hybrid-authority (ADR-021 spirit)**: core enforces minimums against
  a semi-trusted plugin. "Run the binary we discovered, not an arbitrary
  manifest path" belongs in the same minimum set as RLIMIT_AS and path
  jail.

**Ask the user before fixing**. If they want it fixed now, it's one
more commit (~50 lines, thread `DiscoveredPlugin.executable` or the
canonical path into `PluginHost::spawn`, refuse manifests where
`executable` contains `/` or differs from discovered basename). If
not, file it with "A.2.4 lock pending — requires either a fix or an
explicit ADR-021 addendum naming the exclusion."

## A.2 recommendation

Provisional, after the FIX-NOW slate lands and assuming the user
disposition on C1:

- **A.2.1–A.2.2, A.2.4–A.2.5, A.2.8, A.2.10–A.2.12**: ready to tick.
- **A.2.3**: ready after Finding #2 (EntityCapExceeded test) lands.
- **A.2.6**: ready (T4 identity-mismatch test exists; count-pinning
  weakness is test-quality, not coverage).
- **A.2.7**: ready after Finding #3 (E2E crash-loop test) lands.
- **A.2.9** (UQ-WP2-* resolved): doc-only check pending.

Do **not** tick them yourself — the user/human owns the sprint-level
lock-in stamp.
