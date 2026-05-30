# Clarion Sprint 1 WP2 — Plugin host + hybrid authority (handoff prompt)

This file is the starting prompt for a fresh Claude Code session that will
execute WP2. It is designed to be pasted directly as the user's first
message, or referenced by path. It is self-contained; the new session
should not need prior conversation context to pick up cleanly.

---

# Continue Clarion Sprint 1 WP2 execution

I'm handing off WP2 (plugin host + hybrid authority) after completing WP1
(scaffold + storage). You are continuing the same sprint with the same
discipline. This prompt is the full handoff.

## Working directory + branch

- Directory: `/home/john/clarion`
- Branch: `sprint-1/wp2-plugin-host` (already checked out; off `main` tip `ad8d4ce` which is the WP1 merge commit)
- `main` has WP1 already merged via `--no-ff` so `git log --first-parent main` shows WP-level boundaries.
- Do NOT amend existing commits; stack new commits on top.

## The project

Clarion is a code-archaeology tool, one of four products in the Loom suite
(Clarion, Filigree, Wardline, proposed Shuttle). Read `CLAUDE.md` at repo
root for the doctrine, ADR precedence rules, and the Loom federation axiom.

Sprint 1 ships the walking-skeleton across WP1 (scaffold + storage — DONE),
WP2 (plugin host — in progress), WP3 (Python plugin — blocked-by WP2).
You are executing WP2.

## What WP1 delivered (already on `main`)

- Cargo workspace (edition 2024, rustc 1.95) with three crates: `clarion-core`, `clarion-storage`, `clarion-cli`.
- Tooling floor per **ADR-023**: `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml` (pedantic), `deny.toml`, `.github/workflows/ci.yml`, workspace `[lints]`.
- `clarion-core`: `entity_id()` assembler (L2 3-segment format per ADR-003 + ADR-022), `EntityIdError`, `LlmProvider` trait + `NoopProvider` stub.
- `clarion-storage`: SQLite schema migration `0001_initial_schema.sql` (L1 per detailed-design §3 — tables, FTS5, triggers, generated columns, `guidance_sheets` view), `ReaderPool` (deadpool-sqlite), `Writer` actor (L3 per ADR-011 — `tokio::task::spawn_blocking`, bounded mpsc, per-N batch COMMIT, WriterCmd enum with oneshot acks, `WriterProtocol` error variant for contract violations).
- `clarion-cli`: `clarion install` (creates `.clarion/{clarion.db,config.json,.gitignore}` + `clarion.yaml`; ADR-005 governs the `.gitignore` contents), `clarion analyze` (Sprint 1 stub — BeginRun → CommitRun with status `skipped_no_plugins`; WP2 replaces this stub).
- 48 tests pass, all 7 ADR-023 gates green (build dev + release, fmt, clippy pedantic `-D warnings`, nextest, doc, deny).

**Key exported API that WP2 will consume**:

- `clarion_storage::{Writer, DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, EntityRecord, RunStatus, WriterCmd}`.
- `Writer::send_wait<T, F>(|ack| WriterCmd::...) -> Result<T, StorageError>` — canonical async helper for enqueuing commands. The WP1 review loop made this the preferred pattern over hand-rolling oneshot channels.
- `WriterCmd::InsertEntity { entity: Box<EntityRecord>, ack }` — note the `Box` on the entity; it is required by clippy's `large_enum_variant` and is part of the L3 locked-in shape.
- `clarion_core::{entity_id, EntityId, EntityIdError, LlmProvider, NoopProvider}`.
- `clarion_storage::StorageError` variants: `Sqlite(#[from] rusqlite::Error)`, `Pool*(#[from] deadpool)`, `PragmaInvariant(String)`, `Migration{version, source}`, `Io(#[from] io::Error)`, `WriterGone`, `WriterProtocol(String)`, `WriterNoResponse`. `StorageError` is `!Sync`; bridge to `anyhow` via `.map_err(|e| anyhow::anyhow!("{e}"))`.

## The spec

Canonical WP2 spec: `docs/implementation/sprint-1/wp2-plugin-host.md`

Outline (9 tasks):

1. Manifest parser (L5) — `plugin.toml` schema per ADR-022.
2. JSON-RPC transport (L4) — Content-Length framing + method set per ADR-002.
3. In-process mock plugin (test harness).
4. Core-enforced minimums (L6) — path jail drop-not-kill first offense + >10/60s sub-breaker, 8 MiB Content-Length ceiling, 500k per-run entity cap, 2 GiB `RLIMIT_AS` per ADR-021.
5. Plugin discovery (L9).
6. Plugin-host supervisor.
7. Crash-loop breaker.
8. Wire `clarion analyze` to use the plugin host (replaces the Sprint 1 stub).
9. WP2 end-to-end smoke test.

Anchoring ADRs: **ADR-002** (plugin transport JSON-RPC over stdio), **ADR-021** (plugin authority hybrid — declared capabilities + core-enforced minimums), **ADR-022** (core/plugin ontology ownership boundary).

Sign-off ladder: `docs/implementation/sprint-1/signoffs.md` Tier A.2. Tier A.1 is done; do NOT touch it. Your closure target is A.2.1 through A.2.9 with `locked on <date>` stamps for L4, L5, L6, L9.

## Plan status — WRITE ONE FIRST

Unlike WP1, there is **no plan file yet** under `docs/superpowers/plans/`.
Your first step is to write one using the `superpowers:writing-plans`
skill, using the WP2 spec (`docs/implementation/sprint-1/wp2-plugin-host.md`)
as input. Target path:
`docs/superpowers/plans/2026-04-18-wp2-plugin-host.md` (or whatever date
you write it on).

The WP1 plan (`docs/superpowers/plans/2026-04-18-wp1-scaffold-storage.md`)
is a reference for the shape — 10 tasks with per-task files, full verbatim
code blocks, commit messages, and ADR-023 gate requirements inline. Match
that shape.

After writing, optionally run `axiom-planning:plan-review` for a reality /
risk / complexity / convention-alignment check before execution. WP2's
surface is wider than WP1's (subprocess management, frame parsing, jail
enforcement) so a plan review is probably worth the tokens.

Then execute the plan using `superpowers:subagent-driven-development`
exactly as WP1 did — fresh implementer subagent per task, two-stage review
(spec compliance first, then code quality) after each, chore(wp2) fix
commits stacked without amending. See the "Execution protocol" section
below.

## Filigree state

- **WP2 issue**: `clarion-9dee2d24c3`, status `defined`, `is_ready: true`.
  Labels: `adr:002 adr:021 adr:022 release:v0.1 sprint:1 wp:2`.
  Blocks: WP3 (`clarion-cd84959ee9`) and Sprint 1 close (`clarion-30ca615264`).
- Transition path at WP2 start: `defined` → `executing` via
  `mcp__filigree__claim_issue` or `update_issue status=executing`.
- Transition at WP2 close: `executing` → `delivered` (same as WP1's path).

- **WP1 issue** `clarion-2eadcfe651` is `delivered`. Do not touch it.

- **Pending observation** `clarion-obs-67175f4486`: priority generated
  column TEXT affinity breaks lexicographic ordering for numeric
  priorities. Flagged during WP1 Task 3 review. It is a design-doc
  question (detailed-design.md §3 §737), not a WP2 task. Leave it pending
  for Sprint 1 close triage UNLESS WP2 introduces an `ORDER BY priority`
  query that forces the decision earlier — in which case, raise with the
  user before acting.

## Accepted patterns from WP1 that carry forward to WP2

These were established during WP1's review loops and should be applied
from Task 1:

1. **Cargo hygiene**
   - `cargo nextest run` needs `--no-tests=pass` in any CI / script / doc command.
   - Internal path deps pin a version: `clarion-core = { path = "...", version = "0.1.0-dev" }` to satisfy `cargo-deny`'s `wildcards = "deny"`.
   - New workspace deps go in `[workspace.dependencies]` and are referenced via `dep.workspace = true` from crate manifests.
   - `Cargo.lock` is committed.
   - No `#[allow(...)]` for pedantic warnings — fix in-code.

2. **Common pedantic resolutions used in WP1**
   - `doc_markdown`: backtick identifiers like `SQLite`, `PRAGMA`, `JSON-RPC`, type names.
   - `cast_possible_truncation`: `u32::try_from(...).expect("...")` with human-readable expect strings — never `as u32`.
   - `missing_errors_doc`: `# Errors` section on every public fallible fn.
   - `missing_panics_doc`: document or restructure.
   - `needless_pass_by_value`: `&T`.
   - `large_enum_variant`: `Box<T>` the largest variant(s); check with a test-revert under clippy rather than taking anyone's word for whether it fires.
   - Doc link resolution: fully qualify as `[crate::module::Item]` when short form doesn't resolve.

3. **Error design**
   - `#[from]` for structured error types; `String` wrappers only for semantic violations without a wrapped error (e.g. `WriterProtocol(String)` for contract violations that have no underlying library error).
   - `StorageError` is `!Sync` (deadpool's `InteractError` panic payload). Bridging to `anyhow::Error` uses `.map_err(|e| anyhow::anyhow!("{e}"))`. WP2 may add a similar boundary for the plugin host's error type — if so, consider whether `!Sync` propagates.

4. **State-machine correctness**
   - Update state BEFORE the fallible op, not after. `?` short-circuits on Err
     and skips the state update, desynchronising in-memory state from the
     external system. Task 6 fixed this in the writer actor's COMMIT path;
     watch for the same pattern in WP2's subprocess state machine (did you
     mark the child "dead" before calling `wait()`? did you mark the jail
     "tripped" before applying the kill?).

5. **Review loop discipline**
   - Every task: implementer subagent (sonnet, general-purpose) → spec
     compliance reviewer (sonnet, general-purpose, verifies against
     spec not against implementer's self-report) → if clean, code
     quality reviewer (axiom-rust-engineering:rust-code-reviewer,
     sonnet) → if issues, fix subagent → re-review. Never skip the
     re-review.
   - Spec reviewers MUST verify independently. Task 6 in WP1 caught an
     implementer self-report error ("clippy didn't flag it") only because
     the spec reviewer actually test-reverted the change and ran clippy.
   - Fix commits are `chore(wp2):` commits stacked on the `feat(wp2):`
     commit they address. Never amend.

6. **Doc markdown for verbatim SQL / JSON-RPC**: use triple-backtick fenced
   blocks in doc comments. If you put raw `{` / `}` in rustdoc, fully
   escape them with backticks or wrap in `<pre>`; otherwise rustdoc tries
   to interpret them.

7. **Gates run on every commit, not just task-end**: the 6 gates (build /
   fmt / clippy / nextest / doc / deny) go green on every `feat(wp2):`
   and every `chore(wp2):` commit. The release build gate runs at Task N
   close (mirroring WP1 Task 9's pattern).

## Lock-ins to land + reviewer hotspots

- **L4 — JSON-RPC method set + Content-Length framing (ADR-002)**: `initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`. Reviewer hotspots: Content-Length header parsing (malformed framing, oversized payloads per L6's 8 MiB ceiling), JSON-RPC error code fidelity, mid-message disconnect handling.
- **L5 — `plugin.toml` schema (ADR-022)**: reviewer hotspots: missing-required-field errors, ontology declaration shape (`[ontology].entity_kinds` drives host-side filtering).
- **L6 — core-enforced minimums (ADR-021)**: path jail (drop-not-kill on first escape; >10/60s → kill = sub-breaker), 8 MiB Content-Length ceiling, 500k per-run entity cap, 2 GiB `RLIMIT_AS`. Reviewer hotspots: each invariant needs a positive and a negative test; the sub-breaker's sliding-window logic is a timing hazard (flake-check 3x for any test that relies on wall-clock).
- **L9 — plugin discovery**: finds `clarion-plugin-*` binaries on `$PATH`, loads neighbouring `plugin.toml`. Reviewer hotspots: cross-platform `$PATH` handling (WP2 scope is Linux but don't assume POSIX everywhere in code comments), missing-manifest errors, multiple-plugins-with-same-name behaviour.

Additionally, WP2 Task 8 **wires `clarion analyze` to use the plugin host**. The current Sprint-1 stub ends with `RunStatus::SkippedNoPlugins`; after WP2 Task 8 it should use `RunStatus::Completed` when at least one plugin runs and produces output. Keep `SkippedNoPlugins` as a valid status for the no-plugins-installed case.

## Execution protocol — FULL DISCIPLINE per task

Invoke `superpowers:subagent-driven-development` at session start. Then
for each task run the full review loop. Example dispatch shapes (copied
from WP1 practice; adjust task specifics):

- **Implementer subagent**: `general-purpose`, `sonnet`. Pass the task's
  full code blocks from the plan (do not ask it to "read the plan" —
  violates the skill). Include: working dir, branch, tip commit,
  ADR-023 gate requirements, pre-answered FAQ (patterns above),
  commit-message HEREDOC, self-review checklist, structured report
  format (STATUS / COMMIT_SHA / TEST_COUNT / GATE_EXITS / DEVIATIONS /
  CONCERNS).

- **Spec compliance reviewer**: `general-purpose`, `sonnet`. Include: commit
  under review, parent commit, explicit "do not trust the implementer's
  report", required files list, required public API surface, required
  test names, required behaviour, and a ledger of all 6 (or 7 at
  task-9 close) gates to run independently. Report format with
  per-surface ✅/❌ + VERDICT.

- **Code quality reviewer**: `axiom-rust-engineering:rust-code-reviewer`,
  `sonnet`. Include: scope (two commits: feat + any prior chore), project
  context (ADR-023 floor, lock-in relevance), accepted deviations from
  implementer's report, specific questions to answer (error handling,
  state machine, shutdown discipline, jail soundness for L6, Send/Sync
  correctness for subprocess handles), strengths / issues (Critical /
  Important / Minor / Nitpick) / assessment format.

- **Fix subagent (when needed)**: `general-purpose`, `sonnet`. Pass exact
  fix instructions per reviewer finding. Stack a new `chore(wp2):`
  commit — do NOT amend. Re-run all 6 gates. Flake-check concurrency /
  timing tests 3 times. Report format same as implementer.

- **Re-review**: dispatch the same code-quality reviewer on the fix
  commit. Expect a short response (<400 words). Approve or request
  further fixes.

## Commit-message conventions (from WP1)

- `feat(wp2): <subject>` for new functionality.
- `chore(wp2): apply Task N code-review fixes` for fix commits.
- `test(wp2): ...` for test-only commits (e.g. the E2E test at Task 9 close).
- `docs(sprint-1): ...` for spec / signoff / lock-in doc updates (Task 9 or the final sign-off task).
- Each message body ends without a trailer unless the user asks for a `Co-Authored-By:` line.

## Final-commit sanity checklist at WP2 close

- All 7 ADR-023 gates exit 0 (build dev + release, fmt, clippy `-D warnings`, nextest, doc, deny).
- Every Tier A.2 box ticked in `signoffs.md` with `locked on <date>` on L4, L5, L6, L9.
- L4, L5, L6, L9 stamped in `docs/implementation/sprint-1/README.md` §4.
- Every UQ-WP2-* in `wp2-plugin-host.md` §5 marked resolved with resolving task.
- `git log --oneline main..` on the WP2 branch shows the expected commit sequence.
- Filigree: `mcp__filigree__update_issue clarion-9dee2d24c3 status=delivered`. WP3 becomes ready.

## Start here

1. Run `mcp__filigree__get_issue clarion-9dee2d24c3` to see current WP2 state.
2. Read `docs/implementation/sprint-1/wp2-plugin-host.md` end-to-end.
3. Read `docs/clarion/adr/ADR-002-plugin-transport-json-rpc.md`, `ADR-021-plugin-authority-hybrid.md`, `ADR-022-core-plugin-ontology.md`.
4. Optional: skim `docs/superpowers/plans/2026-04-18-wp1-scaffold-storage.md` to see the plan shape.
5. Invoke `superpowers:writing-plans` and produce `docs/superpowers/plans/<YYYY-MM-DD>-wp2-plugin-host.md`.
6. Optional: run `axiom-planning:plan-review` on the plan.
7. Transition Filigree: `mcp__filigree__update_issue clarion-9dee2d24c3 status=executing`.
8. Invoke `superpowers:subagent-driven-development`.
9. Dispatch Task 1 (manifest parser, L5) implementer. Work through Tasks 2-9, closing with the E2E test + signoff updates.

Good luck. WP1's 18-commit review-looped history is on `main`; your WP2
history should follow the same shape.

---

*Handoff written by the previous session on 2026-04-18. Branch
`sprint-1/wp2-plugin-host` created off the WP1 merge commit `ad8d4ce`.
48 tests passing on the branch tip at handoff time.*
