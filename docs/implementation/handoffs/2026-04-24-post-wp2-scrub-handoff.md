# Clarion Sprint 1 — post-WP2-scrub handoff

This file is the starting prompt for the next Claude Code session after
the WP2 phase-3 scrub closed out on 2026-04-24. Paste it as the user's
first message; it is self-contained.

Supersedes `2026-04-23-wp2-full-scrub-handoff.md`. That scrub is
complete: WP2 code-complete, 21 outstanding issues closed, 5 remaining
all legitimately FILE-ONLY with documented deferral rationale.

---

# Continue Clarion Sprint 1 — WP2 signoff close + WP3 kickoff

WP2 is code-complete and ready to tick. WP3 (Python plugin) is the
next work package. This session's job is the human-gated A.2 signoff
close AND/OR starting WP3 — your choice depending on what the user
wants to do first.

Do not assume the WP2 scrub was exhaustive beyond what's claimed here.
It was time-boxed and focused on what the reviewers flagged; a fresh
perspective on WP3 will inevitably surface things WP2 missed.

## Working directory + branch

- Directory: `/home/john/clarion`
- Branch: `sprint-1/wp2-plugin-host`
- Current HEAD: `7c0e396` (last WP2 scrub commit — world-writable dir
  refusal)
- Merge base with `main`: `ad8d4ce`
- 45 commits on this branch total (21 original WP2 + triage doc + 23
  scrub commits across two push-through rounds)
- 174 tests passing; every ADR-023 gate green on every commit (fmt,
  clippy pedantic, nextest, doc -D warnings, deny)

## What happened in the scrub (do not redo)

Six parallel reviewers (rust-code-reviewer, threat-analyst,
unsafe-auditor, test-suite-reviewer, coverage-gap-analyst,
bug-triage-specialist) swept `ad8d4ce..a1cc3be`. Triage doc at
`docs/superpowers/handoffs/2026-04-23-wp2-scrub-findings.md`. 21
issues closed across the scrub:

### First round — blocking items (session 1)

1. `b30638c` — `spawn_blocking` JoinError no longer bypasses
   CommitRun/FailRun (plugin-task panic → `runs.status='failed'`).
2. `7f8fc9a` — E2E crash-loop breaker trip test (A.2.7).
3. `3e0ea44` — host-level `EntityCapExceeded` test T9 (A.2.3).
4. `ad054bd` — PATH scrubbed in skipped-run test.
5. `669e89e` — `protocol::make_{request,notification}` → `pub(crate)`.
6. `d890a73` — `walk_dir` logs + counts skipped entries.
7. `483db6e` — T3 + T6 exact-count assertions, T8c + T8d for id/kind
   oversize.
8. `26f14aa` — `t9_handshake_failure_…_returns_err_promptly` rename.

### Second round — FILE-ONLY follow-ups (session 2)

Cluster A deserialisation ceilings:

9. `288defe` — `ContentLengthCeiling::unbounded()` gated behind
   `#[cfg(test)]`; fixture uses `DEFAULT` (8 MiB).
10. `855803e` — `plugin.toml` capped at 64 KiB; `[integrations.*]`
    capped at 64 entries; `DiscoveryError::ManifestTooLarge` variant.
11. `7d97c66` — `ProtocolError.message/.data` truncated at 4 KiB via
    custom Deserialize (`MAX_PROTOCOL_ERROR_FIELD_BYTES`).
12. `7b5db34` — `RawEntity.extra` / `RawSource.extra` bounded by
    serialised size (`MAX_ENTITY_EXTRA_BYTES = 64 KiB`); T8e.

Cluster B + protocol hardening:

13. `84b5778` — documented that `DiscoveredPlugin.executable` is
    raw-PATH (neighbour-manifest constraint); no behaviour change.
14. `6b0fa3a` — structural double-shutdown guard (`terminated: bool`);
    T8f.
15. `5fb5666` — `FINDING_NON_UTF8_PATH` at wire boundary; T8g.
16. `1ac32b1` — `ontology_version` validated at handshake; stored on
    `PluginHost`; pub `ontology_version()` getter (ADR-007 / WP6 prep).
17. `769a177` — **drain-until-match** helper
    `read_response_matching` replaces single-read in all three
    response sites (handshake, analyze_file, do_shutdown);
    `MAX_DRAIN_FRAMES = 16` bounds the budget.
18. `bd92600` — `RLIMIT_NOFILE=256` + `RLIMIT_NPROC=32` applied in
    pre_exec; `apply_prlimit_nofile_nproc`.

Threat C1 (user-green-lit):

19. `eb0a41d` — **`PluginHost::spawn` signature changed** — now takes
    `executable: &Path` separately; manifest's `plugin.executable`
    must be a bare basename matching the discovered binary; T10 +
    T11 negative tests. **This is a breaking change for any caller
    in WP3+.**

Test gaps + cleanup:

20. `89b2da0` — analyze-file-error-payload, content-length-ceiling-
    through-host, cross-plugin-id-spoof, drain-happy-path,
    no-initialized-after-refusal (T2 strengthened).
21. `b4eca5b` — typed `AnalyzeFileResult` in `analyze_file` — eliminates
    the outer Value-array clone.
22. `b3c91a7` — **stderr piped to bounded ring buffer** (64 KiB);
    `host.stderr_tail() -> Option<String>` for diagnostics; T9b.
    **Also a caller-observable change — stderr is no longer inherited.**
23. `7c0e396` — world-writable `$PATH` dirs refused with
    `DiscoveryError::WorldWritableDir`; T8.

## WP2 signoff ladder (§A.2) status at handoff

All 12 boxes have production code AND a discriminating test. You can
tick them pending the human-gate rules below.

| Item | Proof that's in place |
|---|---|
| A.2.1 | `transport_*` tests + end-to-end via T1 |
| A.2.2 | `manifest.rs` tests (30+ positive/negative) |
| A.2.3 | T9 (entity cap host-level) + `content_length_ceiling_surfaces_through_plugin_host` + T5/T6 (jail + breaker) + `apply_prlimit_linux_returns_ok` |
| A.2.4 | `discovery.rs` T1–T8 + T10/T11 spawn-safety |
| A.2.5 | T3 with pinned count |
| A.2.6 | T4 + `cross_plugin_plugin_id_spoof_is_rejected` |
| A.2.7 | `breaker_*` + `wp2_crash_loop_breaker_trips_and_skips_remaining_plugins` |
| A.2.8 | `wp2_e2e_smoke_fixture_plugin_round_trip` |
| A.2.9 | **DOC TASK — not done** (see below) |
| A.2.10 | manifest negative tests for grammar + reserved prefix |
| A.2.11 | manifest negative tests for reserved kinds |
| A.2.12 | T2 with strengthened no-initialized-sent assertion |

## The 5 remaining WP2 open issues — **do not reopen**

Each has an explicit trigger or deferral rationale from the scrub's
advisor pass. Re-litigating them is time wasted.

| ID | Why it stays open |
|---|---|
| `clarion-9dee2d24c3` P1 | WP2 umbrella — human gate; closes when A.2 ticks |
| `clarion-48c5d06578` P3 | Explicit WP4 trigger: "flag when Task 6 writes its own supervisor read loop" |
| `clarion-928349b60f` P3 | Explicit WP4 trigger: becomes critical when briefing-serving code reads `AcceptedEntity.source_file_path` |
| `clarion-35688034f0` P3 | Deferred: touches every I/O path, needs cross-module design pass. Half-fixing is worse than not fixing — do in its own session when someone has the design bandwidth |
| `clarion-c0977ac293` P4 | Deferred: deliberately hard (requires a subprocess that allocates past limit); unit tests + code review of `reap_and_classify_exit` sufficient for Sprint 1 |

## Your job this session — two options

### Option A: A.2 signoff close (tight, ~30 min)

Complete the human-gate steps so WP2 is formally locked.

1. **A.2.9 doc walk-through**: read
   `docs/implementation/sprint-1/wp2-plugin-host.md §5` and verify
   every UQ-WP2-* is marked resolved. Where one isn't, either update
   the doc or surface the gap. UQ-WP2-10 specifically should read
   "resolved by ADR-002"; UQ-WP2-11 should read "resolved by identity
   check / T4".
2. **Tick A.2.1–A.2.12** in
   `docs/implementation/sprint-1/signoffs.md`. Each tick gets a
   `locked on 2026-04-24` (or current date) stamp where appropriate
   (L4, L5, L6, L9 are the load-bearing lock-ins).
3. **Close the umbrella** `clarion-9dee2d24c3` with a pointer to the
   signoff commit.
4. Make one commit: `docs(wp2): lock A.2 signoffs; close WP2 umbrella`.

Do NOT tick A.3 or A.4 — those belong to WP3 and the demo respectively.

### Option B: Start WP3 (Python plugin)

Anchor doc: `docs/implementation/sprint-1/wp3-python-plugin.md`.

This is the bigger slice. WP3 builds an editable Python package at
`plugins/python/` that speaks the Sprint-1 JSON-RPC protocol. Key
lock-ins:

- **L7**: qualname reconstruction per
  `docs/clarion/v0.1/detailed-design.md §§4–5` (module-level, nested,
  class, async, nested-class). Shared test fixture at
  `/fixtures/entity_id.json` must pass byte-for-byte in both Rust and
  Python — this is the L2+L7 alignment proof (A.3.4).
- **L8**: Wardline probe returns the three documented states
  (`absent`, `enabled`, `version_out_of_range`).
- ADR-023 Python gates: `ruff check`, `ruff format --check`,
  `mypy --strict`, `pytest` all green.

You will need Option A done first OR done alongside — A.3 tests
exercise the full host↔plugin pipeline and will surface any latent
WP2 bug.

### Caller-observable WP2 changes WP3 must know

The scrub changed several surfaces that WP3 will touch:

1. **`PluginHost::spawn` signature**: now takes `executable: &Path` as
   a third argument (the discovered binary path). Manifest's
   `plugin.executable` must be a bare basename that matches the
   discovered filename, or `HostError::Spawn` fires before exec. WP3's
   Python plugin must declare `executable = "clarion-plugin-python"`
   — no paths.

2. **Plugin's stderr is piped, not inherited.** The Python plugin's
   stderr is captured into a 64 KiB ring buffer, accessible via
   `host.stderr_tail()`. Print-debugging from the Python plugin during
   tests won't show up in the test runner's stderr unless someone
   asks for the tail. `tracing::warn!` in the host IS still on stdout
   via `tracing_subscriber::fmt::init()`.

3. **`ontology_version` must be present and non-empty** in the
   `initialize` response. Python plugin's `initialize` handler needs
   to return a valid semver string. The host validates and stores it;
   WP6 will consume it via `host.ontology_version()`.

4. **Drain-until-match** on response reads. If the Python plugin
   accidentally sends a response twice (or sends unsolicited frames
   between analyze_file calls), the host will log `warn!` and drain
   up to `MAX_DRAIN_FRAMES = 16` stale frames before failing. Don't
   write tests that rely on the host aborting on the first mismatched
   id — that's not current behaviour.

5. **Resource limits on the plugin child**: `RLIMIT_AS` from manifest's
   `expected_max_rss_mb` (min of that and 2 GiB default), plus fixed
   `RLIMIT_NOFILE = 256`, `RLIMIT_NPROC = 32`. The Python plugin boot
   path (`python3 -m clarion_plugin_python`) must fit — CPython alone
   takes ~30 MiB, plus imports. Realistic RSS ceiling for the Python
   plugin is 256 MiB+ (set in `plugin.toml`).

6. **Entity field caps**: `MAX_ENTITY_FIELD_BYTES = 4 KiB` per scalar
   (`id`, `kind`, `qualified_name`, `source.file_path`);
   `MAX_ENTITY_EXTRA_BYTES = 64 KiB` for `extra` / `source.extra`
   serialised. A Python plugin emitting a huge docstring in `extra`
   WILL have the entity dropped with `FINDING_ENTITY_FIELD_OVERSIZE`.
   WP3 tests should assert on normal-size entities only.

7. **`[integrations.*]` capped at 64 entries**; `plugin.toml` capped at
   64 KiB. Python plugin's manifest should be ~2 KiB, well under.

8. **world-writable `$PATH` dirs are refused**. In tests that use
   TempDir on a 0o700 home, this isn't an issue — but CI systems that
   drop plugins in a world-writable dir will now fail discovery with
   `DiscoveryError::WorldWritableDir`.

## Files of interest

### WP2 (what's in place, don't edit unless fixing a bug)

- `crates/clarion-core/src/plugin/host.rs` (~2400 lines; T1–T11 tests
  plus 4 new coverage-gap tests)
- `crates/clarion-core/src/plugin/protocol.rs` (ProtocolError has
  custom Deserialize now; `AnalyzeFileResult` is typed)
- `crates/clarion-core/src/plugin/discovery.rs` (world-writable check,
  size caps)
- `crates/clarion-core/src/plugin/manifest.rs` (integrations cap)
- `crates/clarion-core/src/plugin/limits.rs` (NOFILE/NPROC)
- `crates/clarion-cli/src/analyze.rs` (JoinError handled;
  `DiscoveredPlugin.executable` threaded into spawn)

### WP3 (what you'll create)

- `plugins/python/` — editable Python package; `pyproject.toml` with
  `ruff`, `mypy`, `pytest` configured per ADR-023
- `plugins/python/clarion_plugin_python/` — the module
- `plugins/python/tests/` — `test_qualname.py`,
  `test_wardline_probe.py`, `test_server.py`, `test_round_trip.py`
- Shared fixture used by both Rust and Python:
  `fixtures/entity_id.json` (should already exist from WP1 — A.1.4)

### Anchoring documents

- `docs/implementation/sprint-1/wp3-python-plugin.md` — the WP doc
- `docs/implementation/sprint-1/signoffs.md §A.3` — the gate
- `docs/implementation/sprint-1/README.md §4` — lock-in table
- `docs/clarion/adr/ADR-001-language-plugin-boundary.md`
- `docs/clarion/adr/ADR-002-crash-loop-breaker.md`
- `docs/clarion/adr/ADR-003-entity-id-format.md`
- `docs/clarion/adr/ADR-007-summary-cache-keying.md` — you'll produce
  `ontology_version` but not consume it yet
- `docs/clarion/adr/ADR-023-tooling-baseline.md` — Python tooling gates
- `docs/clarion/v0.1/detailed-design.md §§4–5` — qualname rules
- `docs/clarion/v0.1/requirements.md` — REQ-/NFR- IDs WP3 addresses

## Methodology

Same as the prior sessions. Not negotiable.

### Phase 1 — Plan + brainstorm (before writing code)

Invoke the appropriate skills:
- `superpowers:brainstorming` if WP3's shape is genuinely open (the
  WP doc does most of this but you may have fresh angles)
- `superpowers:writing-plans` after brainstorming, to produce a
  concrete task list
- `axiom-planning:review-plan` if the plan is non-trivial

The WP3 doc is detailed; you probably don't need brainstorm. Go
straight to plan + review if comfortable.

### Phase 2 — TDD implementation

One commit per task. Each commit:
1. Failing test first (TDD discipline).
2. Minimum code to pass.
3. Gate run: all ADR-023 gates green before commit.
4. Commit message cites the filigree issue ID.

ADR-023 Rust gates (same as WP2):
```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo deny check
```

ADR-023 Python gates (new for WP3):
```
ruff check plugins/python/
ruff format --check plugins/python/
mypy --strict plugins/python/
pytest plugins/python/
```

### Phase 3 — Demo script + close

- Run the README §3 demo script end-to-end
  (`docs/implementation/sprint-1/README.md §3`).
- Tick A.3.1–A.3.10 in the signoff ladder with a `locked on <date>`
  stamp where appropriate.
- Tick A.4.1–A.4.3 (end-to-end walking skeleton).
- Close the WP3 umbrella issue and the Sprint 1 close issue.

## Session hygiene

- **filigree workflow**: MCP tools are in `.mcp.json`. Prefer
  `mcp__filigree__*` over CLI.
- **Do not reopen the 5 remaining WP2 issues** — each has a documented
  deferral rationale. If WP3 work genuinely uncovers a reason to
  revisit (e.g. Python plugin hangs in a way only `read_frame`
  deadline would fix), file a new issue and reference the old one.
- **Commit discipline**: one logical fix per commit, `git add
  <specific files>` (not `git add -A`). The WP2 scrub had one
  accidental mis-stage from `-A`; the commit stayed but I flagged it
  to the user.
- **Never skip hooks** (`--no-verify`). If a pre-commit fails, fix
  the root cause.
- **ADR-023 gates on every commit**. Not every N commits.
- **Never invent new ADRs** in this session. If WP3 work surfaces a
  design-level decision, file an issue and let the user decide.
- **Respect the rename-over-stub policy** (CLAUDE.md): if anything
  moves, use `git mv`.

## Starting checklist

1. `git status && git log --oneline main..HEAD | head -10` — confirm
   branch state matches this doc (HEAD should be `7c0e396`).
2. `filigree list --status=open --json | jq -r '.[] | "\(.id) P\(.priority) \(.type) \(.title)"' | head -30` —
   see what's ready. WP2 tail (5 items) is legitimately open; expect
   `clarion-cd84959ee9` (WP3 umbrella) ready.
3. `cargo nextest run --workspace --all-features 2>&1 | tail -5` —
   confirm the 174-test green baseline.
4. Read this doc in full. Read the WP3 doc + signoffs §A.3 + ADR-023's
   Python section.
5. Ask the user: "A.2 close, WP3 start, or both in parallel?" The
   answer determines your first task list.
6. If they want both: tick A.2 as Task 1 (small commit), then pivot to
   WP3. Don't conflate the two.

Good luck. Assume WP2 is correct until proven otherwise — the scrub
was thorough but not infallible, and WP3 tests will find anything it
missed.
