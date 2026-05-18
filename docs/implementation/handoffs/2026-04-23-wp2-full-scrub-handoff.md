# Clarion Sprint 1 WP2 — Full Scrub Before WP3 (handoff prompt)

This file is the starting prompt for a fresh Claude Code session that will
do a **second full scrub of WP2** before the project moves on to WP3
(Python plugin). Paste it as the user's first message. It is self-contained.

Supersedes the two prior WP2 handoffs (`2026-04-18-wp2-plugin-host-handoff.md`
and `2026-04-19-wp2-tasks-4-to-9-handoff.md`), which covered initial
implementation. WP2 is now code-complete; the question this session
answers is *"is it actually ready to sign off A.2 and move to WP3?"*

---

# Continue Clarion Sprint 1 WP2 — full review + fix pass

You are picking up WP2 (plugin protocol + hybrid authority) **after code
is in place and two review passes have already landed fixes**. The sprint
lead called the landing "very wobbly" and asked for another scrub before
WP3 begins. Your job is to find what's still wrong, fix what you can
fix cheaply, and file what's worth filing — then leave the user with a
clean call on whether A.2 can tick.

Do not assume the prior reviews were exhaustive. They were time-boxed and
biased toward what the agents-of-the-moment happened to look for. A
third perspective usually turns up real issues the first two missed.

## Working directory + branch

- Directory: `/home/john/clarion`
- Branch: `sprint-1/wp2-plugin-host`
- Current HEAD: `a1cc3be` (after the four P1 review-2 bugs landed)
- Merge base with `main`: `ad8d4ce` (WP1 merge commit)

21 commits already on this branch; 153 tests passing; every ADR-023 gate
(clippy pedantic, fmt, nextest, doc, deny) green on every commit.

## What's already shipped (don't redo)

WP2 implementation (Tasks 1–9) is complete. The most recent fixes are
the four P1 review-2 bugs closed on 2026-04-23:

| SHA | Issue | What it fixed |
|---|---|---|
| `5c5c3ee` | clarion-b6d7e077fd | RawEntity string fields bounded to 4 KiB (host RAM DoS) |
| `0fcc57f` | clarion-64b53d174e | Reap child on `PluginHost::spawn` handshake failure (no zombies) |
| `37b56d9` | clarion-f56dc6ee43 | `clarion analyze` exits non-zero on FailRun |
| `a1cc3be` | clarion-978c8d6f15 | CrashLoopBreaker wired into analyze.rs + one-plugin-crash no longer tanks run |

The last one introduced a behavioural change worth flagging to reviewers:
`RunOutcome::SoftFailed` (plugin crashed, other plugins' entities still
commit) vs `RunOutcome::HardFailed` (writer-actor error, rollback). See
`crates/clarion-cli/src/analyze.rs` §run-outcome. This split is new and
warrants scrutiny — it widens the CommitRun/FailRun semantics and
implicitly extends the L3 writer-actor contract.

## Scope of this scrub

WP2 covers:

- **L4** — JSON-RPC transport (Content-Length framing, typed protocol),
  `crates/clarion-core/src/plugin/transport.rs` + `protocol.rs`
- **L5** — `plugin.toml` manifest parser/validator per ADR-022,
  `crates/clarion-core/src/plugin/manifest.rs`
- **L6** — Core-enforced minimums per ADR-021 §Layer 2 (path jail,
  Content-Length ceiling, entity cap, `RLIMIT_AS`),
  `jail.rs` + `limits.rs`
- **L9** — Plugin discovery convention (PATH + neighbour plugin.toml),
  `discovery.rs`
- **Host supervisor** — spawn, handshake, analyze_file pipeline,
  ontology + identity enforcement, path-escape breaker, `host.rs`
- **Crash-loop breaker** — ADR-002 + UQ-WP2-10 (>3 crashes/60s),
  `breaker.rs`
- **Analyze CLI wiring** — discover → walk → per-plugin spawn →
  writer-actor, `crates/clarion-cli/src/analyze.rs`

Anchoring documents to read-with:
- `docs/implementation/sprint-1/wp2-plugin-host.md` — the WP doc
- `docs/implementation/sprint-1/signoffs.md` §A.2 — the gate
- `docs/clarion/adr/ADR-002-crash-loop-breaker.md`
- `docs/clarion/adr/ADR-021-plugin-authority-hybrid.md`
- `docs/clarion/adr/ADR-022-core-plugin-ontology.md`
- `docs/clarion/adr/ADR-023-tooling-baseline.md`
- `docs/clarion/v0.1/requirements.md` — REQ / NFR / CON IDs WP2 addresses
- `docs/clarion/v0.1/system-design.md` §§4–6 (host, plugin protocol, limits)
- `docs/clarion/v0.1/detailed-design.md` §§4–5 (wire schemas, rule catalogues)

## Open WP2 review-2 tail at session start

14 issues labelled `wp:2` still open — pull the current list yourself
with `filigree list --label=wp:2 --status=open`. Summary at handoff
time:

- **P1** — `clarion-9dee2d24c3` the WP2 work-package issue itself
  (closes when A.2 signoffs tick)
- **P2** — `clarion-a287217267` RawEntity.extra / RawSource.extra Maps
  still unbounded (properties_json DoS). Twin of the closed
  b6d7e077fd — the suggested fix in its body is a bound on the total
  serialised size of `extra` per entity.
- **P3 (12 issues)** — mix of design gaps, missing tests, and polish:
  - breaker surface gaps: JailError::Io/NonUtf8Path don't tick the
    PathEscapeBreaker; `ContentLengthCeiling::unbounded()` is `pub`
  - TOCTOU / shadow risk: `jail()` returns canonicalised path at one
    moment; discovery dedupes by canonical but stores raw
  - unbounded deserialisation: `ProtocolError.message/.data`;
    manifest reads have no size cap; `integrations` table keys
  - missing double-shutdown guard on `PluginHost::shutdown()`
  - missing tests: analyze_file JSON-RPC error response; no
    `initialized` notification after capability refusal; path-escape
    count pinning
  - poisoned-inbox drain/discard strategy
- **P4** — `clarion-c850c27f33` `entities.len() as u64` unchecked cast

These are a starting map, not a ceiling. Expect to find more.

## A.2 signoff status

`docs/implementation/sprint-1/signoffs.md` §A.2 has 12 ticks (A.2.1
through A.2.12). None are currently ticked. Part of this session's job
is to audit the signoffs ladder against the code and mark which are
ready to lock. *Don't* tick them yourself without the user's explicit
go-ahead — the `locked on <YYYY-MM-DD>` stamp is a sprint-level
commitment and takes a human in the loop.

## Methodology

Run in three phases. Commit-and-close as you go; don't batch.

### Phase 1 — Independent reviewer sweep (parallel)

Dispatch these reviewers in a **single message** (parallel) — they see
the same code but from different angles, and finding overlap is signal.

1. **`axiom-rust-engineering:rust-code-reviewer`** on the four WP2
   source modules: `plugin/host.rs`, `plugin/transport.rs`,
   `plugin/jail.rs`, `plugin/limits.rs`. Also `crates/clarion-cli/src/analyze.rs`.
   Ask specifically about: error handling integrity, API surface, async
   correctness, lifetime soundness. *Do not* ask for style nits —
   clippy pedantic already caught those.

2. **`ordis-security-architect:threat-analyst`** on the host ↔ plugin
   trust boundary. Scope: what can a malicious/buggy plugin do to the
   host that isn't already mitigated by the four P1 fixes? The prior
   review caught the RawEntity RAM DoS; this one should look for the
   next layer (deserialisation bombs in `extra` maps, symlink races in
   discovery, pipe-write-flood causing host read-buffer growth, etc.).

3. **`axiom-rust-engineering:unsafe-auditor`** on the single `unsafe`
   block in `host.rs::spawn` (the `pre_exec` closure calling
   `apply_prlimit_as`). Verify the SAFETY comment is accurate,
   async-signal-safety holds, and no allocation or Rust drop runs
   across the fork/exec boundary.

4. **`ordis-quality-engineering:test-suite-reviewer`** on
   `crates/clarion-core/src/plugin/*/tests` and
   `crates/clarion-core/tests/host_subprocess.rs` +
   `crates/clarion-cli/tests/wp2_e2e.rs` + `.../analyze.rs`. Look for:
   sleepy assertions, test interdependence, brittle ordering, tests
   that prove the mock rather than the code under test.

5. **`ordis-quality-engineering:coverage-gap-analyst`** on the WP2
   modules. Map test-to-source; identify untested error paths. Compare
   against the signoff ladder — every A.2.x item should correspond to
   at least one concrete test.

6. **`axiom-sdlc-engineering:bug-triage-specialist`** — read the 14
   open WP2 issues, cluster by root cause, identify any that are
   duplicates of each other or symptoms of a common design gap. The
   goal is to *not* play whack-a-mole — if 3 P3 findings all trace to
   "deserialisation has no size limits anywhere," that's one fix with
   three test cases, not three fixes.

Give each reviewer the commit range `ad8d4ce..a1cc3be` as its scope.
They should report in `CONFIDENT / PLAUSIBLE / SPECULATIVE` severity
buckets per the SME Agent Protocol. Ignore SPECULATIVE unless it
clusters with another reviewer's CONFIDENT.

### Phase 2 — Synthesise and prioritise

After all six reviewers return, you (the main agent) synthesise:

- Which findings do multiple reviewers raise? (high-confidence real
  issues)
- Which findings are already filed? (look up by description match)
- Which are duplicates of the P3 tail above?
- What's the *new* surface — findings nobody filed yet?

Produce a short triage doc (`docs/superpowers/handoffs/2026-04-23-wp2-scrub-findings.md`).
Columns: finding, source (reviewer name), severity, existing issue ID
or "new", proposed disposition (fix / file / defer / dismiss). Keep it
to one page.

### Phase 3 — Fix the right subset; file the rest

For each "fix" item in the triage doc:

1. Claim the issue in filigree (`filigree update <id> --status=fixing`
   with severity/root_cause as required by the workflow).
2. Write a failing test first (TDD discipline — the existing T-series
   is your template).
3. Implement the fix.
4. Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
   `cargo fmt --all -- --check`, `cargo nextest run --workspace --all-features`,
   `cargo doc --no-deps --all-features`, `cargo deny check` — **every
   gate must be green before commit**, per ADR-023.
5. One commit per issue. Commit message cites the filigree ID.
6. Close the filigree issue with the commit SHA in `fix_verification`.

For each "file" item: create a new filigree issue with
`--label=sprint:1 --label=wp:2` and a clear suggested-fix section.

For each "defer" item: leave a note in the triage doc explaining why
Sprint 1 doesn't need it. The user reads this to gut-check the
deferrals.

**Do not batch commits.** The previous sessions used
subagent-driven-development with one-commit-per-task — keep the same
discipline. A scrub fix that touches three modules and closes two
issues becomes two commits, one per issue.

## Specific areas where I'd look hard

Not a requirements list — pointers based on what I know is thin:

- **Deserialisation size limits.** The RawEntity fix covered four
  named fields, but serde_json reads the whole frame body into a
  `Value` tree first (`host.rs::analyze_file`, `entities_raw` line
  512). A 6 MiB frame that's 100% nested objects still gets parsed
  before per-field bounds fire. Is that OK? What's the actual memory
  cost of serde_json parsing a 6 MiB pathological payload?

- **Discovery dedup + symlink semantics.** `discover()` finds
  `clarion-plugin-*` binaries on PATH. If two PATH entries resolve to
  the same canonical file, what happens? If a symlink changes between
  discovery and spawn, what happens? If a PATH entry is a world-
  writable directory, can an attacker inject a `clarion-plugin-*` and
  have it picked up?

- **Writer-actor contract.** The Reading-A′ change uses
  `CommitRun(Failed)` where FailRun was the documented path for
  "something went wrong." Is `CommitRun(Failed)` actually guaranteed
  to commit the open entity transaction? (It works in the test, but
  I want the writer author to confirm the contract.) Does the WP1
  writer-actor doc need updating to reflect this usage?

- **Shutdown timing.** `PluginHost::shutdown()` sends `shutdown` +
  `exit` then returns. The CLI's `run_plugin_blocking` calls
  `child.wait()` afterward. If the plugin doesn't actually exit
  (hangs on a bad signal handler), `wait()` blocks forever. There's
  no timeout. Sprint 1 might accept that; I'd at least note it.

- **JSON-RPC error response on analyze_file.** Issue `clarion-e190f1e72b`
  flags there's no test for this. What actually happens in the code?
  Trace `host.rs::analyze_file` when the response payload is
  `ResponsePayload::Error` — the code converts it to
  `HostError::Protocol(e)`, which the CLI classifies as "transport/
  protocol error" and treats as a plugin crash. That's correct but
  untested.

- **ADR-021 §Layer 3 coverage.** The path-escape sub-breaker exists,
  the crash-loop breaker exists. What about the ADR-021 §Layer 3
  *general* crash-escalation language? Read ADR-021 §3 and check if
  anything is declared but unimplemented.

## Exit criteria

The user wants to know: "is WP2 ready to ship and does A.2 tick?"

To answer that, by end of session you should have:

1. A findings triage doc in `docs/superpowers/handoffs/` with every
   new reviewer finding categorised.
2. All "fix" items landed as individual commits with the ADR-023 gates
   green. Expect somewhere between 3 and 10 new commits, depending on
   what the reviewers surface.
3. All "file" items in filigree.
4. A short summary message to the user:
   - What you found beyond the existing P2/P3 tail
   - What you fixed this session
   - What you filed but didn't fix (and why)
   - Your call on A.2: "ready to tick" vs "still has N blockers, specifically X, Y, Z"
   - Your call on whether WP3 can start now or should wait

Do **not** tick A.2 signoffs yourself. That's a human gate. Your job
is to recommend.

## Session hygiene

- **filigree workflow**: `bug` type uses states `triage → confirmed
  [requires severity] → fixing [requires root_cause] → verifying
  [requires fix_verification] → closed`. Required field per transition
  is enforced; `filigree validate <id>` tells you what's missing.
  Severity enum: `critical, major, minor, cosmetic`.
- **Commit discipline**: one logical fix per commit. Commit message
  cites the filigree ID verbatim (e.g. `Closes clarion-abcdef1234`).
- **Never skip hooks** (`--no-verify`). If pre-commit fails, fix the
  root cause and re-commit.
- **ADR-023 gates on every commit** — not every N commits. If a
  commit fails clippy pedantic, the fix is either a real code change
  or an `#[allow(clippy::...)]` with a one-line justification comment.
- **Never invent new ADRs** in this session. If a finding points at a
  design-level gap, file the issue and let the user decide whether it
  needs an ADR.
- **No doc restructuring.** Read docs; don't rewrite them. If a spec
  file actually contradicts the code, file an issue; don't silently
  edit the spec to match.
- **Respect the rename-over-stub policy** (CLAUDE.md): if anything
  moves, use `git mv`; don't leave redirect stubs behind.

## Starting checklist

1. `git status && git log --oneline main..HEAD | head -25` — confirm
   branch state matches this doc.
2. `filigree list --label=wp:2 --status=open --json | jq .` — get the
   current open tail.
3. `cargo nextest run --workspace --all-features` — confirm green
   baseline (expect 153 passing).
4. Read the three most-recent commit bodies: `git log --format=%B
   -n3`. Internalise the RunOutcome split and the breaker wiring
   before reviewers start flagging them.
5. Dispatch Phase 1 reviewers in parallel.

Good luck. Assume the code is wrong until proven otherwise.
