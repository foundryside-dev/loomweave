# Clarion Sprint 1 WP2 — Remaining Tasks 4–9 (handoff prompt)

This file is the starting prompt for a fresh Claude Code session that will
execute WP2 Tasks 4–9 (core-enforced minimums, plugin discovery, plugin-host
supervisor, crash-loop breaker, analyze wiring, E2E smoke). Paste it as the
user's first message. It is self-contained.

Supersedes `2026-04-18-wp2-plugin-host-handoff.md` which covered all of WP2
and was written before Tasks 1–3 landed.

---

# Continue Clarion Sprint 1 WP2 — Tasks 4 through 9

You are picking up WP2 (plugin protocol + hybrid authority) after Tasks 1–3
landed in the previous session. Tasks 4–9 remain. Continue the same
discipline: subagent-driven-development, one commit per task, ADR-023 gates
clean on every commit.

## Working directory + branch

- Directory: `/home/john/clarion`
- Branch: `sprint-1/wp2-plugin-host`
- Current HEAD: `bd56cd5` (pre-Task-4 hardening)
- Merge base with `main`: `ad8d4ce` (WP1 merge commit)

5 commits already on this branch, test suite at 74 passing, clippy + fmt
clean at every commit.

## What's already shipped (don't redo)

| SHA | Summary | Scope |
|---|---|---|
| `c2cb07a` | Task 1 L5 manifest parser + validator | `plugin/manifest.rs`, `plugin/mod.rs`, `lib.rs`, adds `toml` workspace dep |
| `4b20244` | Task 2 L4 Content-Length transport + typed protocol | `plugin/transport.rs`, `plugin/protocol.rs` |
| `f3ca3ab` | Task 2 fix round (null-params → `{}`, double-payload rejection, EINTR retry, flush-on-write, 8 KiB header cap, trim fix) | same 2 files |
| `b27594d` | Task 3 in-process mock plugin (`#[cfg(test)]`-gated) | `plugin/mock.rs`, `plugin/mod.rs` |
| `bd56cd5` | Pre-Task-4 hardening: `plugin_id` split from `name`, PathBuf→String in protocol params, mock state guards, mock tick() inbox preservation | 5 files |

## What remains (Tasks 4–9 per plan spec)

Plan spec: `docs/implementation/sprint-1/wp2-plugin-host.md` §6 Task ledger.

- **Task 4** — Core-enforced minimums (L6): `plugin/jail.rs`, `plugin/limits.rs`. Path jail, Content-Length ceiling (8 MiB), entity-count cap (500k), `apply_prlimit_as` (2 GiB RSS, Linux-only).
- **Task 5** — Plugin discovery (L9): `plugin/discovery.rs`. PATH scan for `clarion-plugin-*` binaries + neighbouring `plugin.toml`.
- **Task 6** — Plugin-host supervisor: `plugin/host.rs`. Spawn subprocess, handshake, request-response loop, jail enforcement on response paths, ontology boundary enforcement, identity-mismatch rejection, shutdown+exit on drop. **Largest task.**
- **Task 7** — Crash-loop breaker: `plugin/breaker.rs`. Parameters per ADR-002 + ADR-021 §Layer 3 (>3 crashes/60s general; >10 path-escapes/60s path-escape sub-breaker).
- **Task 8** — Wire `clarion analyze` to plugin host: modify `clarion-cli/src/analyze.rs`. Discover, spawn, iterate files by extension, persist entities via WP1's writer-actor.
- **Task 9** — WP2 E2E smoke test: `clarion-cli/tests/wp2_e2e.rs`. `clarion install` + `clarion analyze fixture_dir/` produces a completed run with persisted mock-plugin entities.

## Methodology

Use the `superpowers:subagent-driven-development` skill. For each task:

1. Dispatch an implementer subagent (general-purpose, sonnet) with the full
   task text from the plan + context + gotchas (see per-task notes below).
2. When implementer reports DONE, dispatch a spec-compliance reviewer
   (general-purpose, sonnet). Verify each task bullet maps to a concrete
   test function. Don't trust the implementer's report.
3. When spec review passes, dispatch a code-quality reviewer
   (`superpowers:code-reviewer`, sonnet). If issues are CRITICAL or
   IMPORTANT, send the same implementer a fix round via `SendMessage`
   to the agent's ID. Re-review after fixes.
4. When quality review approves (even with follow-up observations), mark
   the task complete and move on. File P3/P4 follow-ups as filigree
   observations with `mcp__filigree__observe`.

Each task ends with one commit. Commit message template:
`feat(wp2): <one-line task summary>`. Fix rounds use `fix(wp2): ...`.

## Doctrine (read before touching code)

- **`docs/suite/loom.md`** — Loom federation axiom. Any "wouldn't it be
  easier if we just added X" proposal must pass the §5 failure test.
- **`docs/implementation/sprint-1/wp2-plugin-host.md`** — the plan you are
  executing. §6 Task ledger is your scope boundary. §L5, L6, L9 are the
  lock-ins.
- **`docs/clarion/adr/ADR-002-plugin-transport-json-rpc.md`** — transport.
- **`docs/clarion/adr/ADR-021-plugin-authority-hybrid.md`** — the four
  core-enforced minimums. §Layer 1 is the manifest runtime sub-block
  (already implemented in Task 1). §Layer 2 is what Task 4 implements.
- **`docs/clarion/adr/ADR-022-core-plugin-ontology.md`** — identifier
  grammar, reserved kinds, rule-ID namespace. Task 6's ontology-boundary
  enforcement cites this.
- **`docs/implementation/sprint-1/signoffs.md`** — Tier A A.2.1–A.2.12.
  Every box should tick when WP2 is done. A.2.3 (L6 locked) and A.2.5–
  A.2.7 (ontology enforcement, identity-mismatch, crash-loop) are the key
  gates for Tasks 4–7. A.2.8 is Task 8's gate.

## Build / test commands (ADR-023 gates — run on every commit)

```bash
cargo nextest run -p clarion-core                                       # unit tests
cargo nextest run --workspace --all-features                            # full suite
cargo fmt --all -- --check                                              # formatting
cargo clippy --workspace --all-targets --all-features -- -D warnings    # lint (pedantic level warn)
cargo deny check                                                        # advisories/licenses/bans
cargo doc --no-deps --all-features                                      # doc build
```

Expected current state: 74 tests, all green, all gates clean.

## Filigree backlog (read these BEFORE starting each task)

13 tickets remain open. They split into "do now" vs "trigger when". Query
the full list with `mcp__filigree__list_issues --status=open` or the CLI
equivalent; titles here for orientation:

**Task 6 triggers (act on when Task 6 lands):**
- `clarion-e46503831c` P3 — `Manifest.integrations` leaks `toml::Value`. Revisit if Task 6 forwards integrations through the handshake. If not, defer.
- `clarion-29acbcd042` P4 — crate-root re-exports too broad. Once `PluginHost` becomes the façade, prune `lib.rs` re-exports down to what external consumers (CLI crate) actually need.

**Task 5 triggers:**
- `clarion-fa35cad487` P4 — extension values not validated. When Task 5 or Task 8 implements extension-to-file-matching, decide the format grammar (`"py"` vs `".py"` vs `"*.py"`) and add parser validation.

**Related observation (P3):**
- `clarion-obs-cc9fe7d44c` — **important for Task 6**. The mock's `tick()` preserves the full failing frame on dispatch error so subsequent ticks re-attempt it. This works for the mock (caller controls lifecycle) but Task 6's real supervisor must not copy the pattern unchanged — a plugin that emits a permanently-bad frame would make the supervisor loop forever. Real supervisor should advance past unrecoverable frames and/or count per-frame failures.

**Cleanup backlog (P3/P4, can be done anytime, not blockers):**
- `clarion-078814da2d` P3 — rule-ID prefix doc comment `*` vs `+`
- `clarion-ebd790422c` P4 — char-class duplicate in `entity_id.rs`
- `clarion-c76cd2028e` P4 — manifest.rs test boilerplate (1188 lines, mostly tests)
- `clarion-bfea7d248b` P4 — `id: i64` scope note
- `clarion-6865a6607c` P4 — `#[non_exhaustive]` annotations on `TransportError`, `ResponsePayload`, `ManifestError`
- `clarion-49803b9dd0` P4 — vacuous second assertion in mock crashing test
- `clarion-f46ebccd5d` P4 — oversize mock state diagram note
- `clarion-a2f7406889` P4 — mock `write_response` tmp Vec allocation
- `clarion-80a48c51cb` P4 — `drive_to_ready` test helper

## Per-task gotchas (read before dispatching each implementer)

### Task 4 — Core-enforced minimums

**Files**: create `plugin/jail.rs` and `plugin/limits.rs`.

Key points from the plan:

- `jail.rs` — `pub fn jail(root: &Path, candidate: &Path) -> Result<PathBuf, JailError>`. Canonicalise both via `std::fs::canonicalize` (follows symlinks per UQ-WP2-03 resolution). Assert `canonical_candidate.starts_with(canonical_root)`. Typed `JailError::EscapedRoot { offending: PathBuf }` on violation.

- **B2 integration** (from commit `bd56cd5`): protocol params are `String`, not `PathBuf`. Jail should return `Result<String, JailError>` for wire-format use, or expose both `PathBuf` (internal) and `String` (wire) accessors. Add `JailError::NonUtf8Path` variant. Task 6 calls `jail()` on both request-side paths (before sending) and response-side paths (on each entity's `source.file_path`).

- `limits.rs` — four types:
  - `ContentLengthCeiling` with 8 MiB default (ADR-021 §2b). **Refactor** `transport.rs::read_frame` to take `&ContentLengthCeiling` instead of `max_bytes: usize`. Existing transport tests pass `usize`; update them.
  - `EntityCountCap` with 500k default (ADR-021 §2c). `try_admit(delta: usize) -> Result<(), CapExceeded>` tracks cumulative `entity + edge + finding` counts across a run.
  - `PathEscapeBreaker` — rolling counter, trips at >10 escapes in 60 seconds (ADR-021 §2a sub-breaker). Consumed by Task 6's host when `JailError::EscapedRoot` observed on a response.
  - `apply_prlimit_as(max_rss_mib: u64)` using `nix::sys::resource::setrlimit` inside `CommandExt::pre_exec`. 2 GiB default (ADR-021 §2d). Effective limit = `min(manifest.capabilities.runtime.expected_max_rss_mb, core_default)`. **`#[cfg(target_os = "linux")]`-gate** per UQ-WP2-06; on non-Linux, log-once warning.

- **New dep**: `nix = "0.28"` or `rustix` (pick one, prefer `nix` per plan §4). Add to workspace deps and clarion-core's `Cargo.toml`.

- **Deferred subcode** (plan §L6 note): `CLA-INFRA-PLUGIN-ENTITY-OVERRUN-WARNING`. Sprint 1 is one-file-per-invocation, no useful surface. Document the deferral in limits.rs; do not implement.

- **Finding subcodes** this task must emit:
  - `CLA-INFRA-PLUGIN-PATH-ESCAPE` — first-offense path escape (drop entity, plugin stays alive)
  - `CLA-INFRA-PLUGIN-DISABLED-PATH-ESCAPE` — sub-breaker tripped (kill plugin)
  - `CLA-INFRA-PLUGIN-FRAME-OVERSIZE` — Content-Length ceiling exceeded
  - `CLA-INFRA-PLUGIN-ENTITY-CAP` — entity-count cap exceeded
  - `CLA-INFRA-PLUGIN-OOM-KILLED` — plugin killed by RLIMIT_AS (detected via `WIFSIGNALED && WTERMSIG == 9`)

- **Tests**: each minimum needs ≥1 positive and ≥1 negative test per signoff A.2.3.

### Task 5 — Plugin discovery

**Files**: create `plugin/discovery.rs`.

- UQ-WP2-01 resolution (plan §L9): **PATH-based scan** for executables matching `clarion-plugin-*`, plus manifest load from either (a) next to the binary or (b) `<install-prefix>/share/clarion/plugins/<name>/plugin.toml`. Neighbor-to-binary first, then install-prefix.

- **Extension-grammar trigger**: ticket `clarion-fa35cad487`. If this task touches extension string handling, decide the format now (likely lowercase, no dot, no wildcard) and add validation to `manifest.rs::parse_manifest`. Close or update the ticket.

- Test: discovery finds a mock `clarion-plugin-*` binary on a test `$PATH` and loads its manifest from the expected location.

### Task 6 — Plugin-host supervisor (largest task)

**Files**: create `plugin/host.rs`.

This is the integration layer — Tasks 1–5 are building blocks; Task 6 wires them. Read the plan's Task 6 bullets carefully.

- **Integration test harness**: uses a real subprocess fixture (not the mock). Plan says "a tiny Rust binary in `tests/fixtures/` that speaks the protocol". Create a minimal fixture binary that handles `initialize` → response → `analyze_file` → one-entity response → `shutdown` → `exit`.

- **Entity-ID assembly**: uses `manifest.plugin.plugin_id` (NOT `name`!) per commit `bd56cd5`. Call `entity_id(manifest.plugin.plugin_id.as_str(), kind, qualified_name)`.

- **String paths**: all `project_root` / `file_path` flowing through protocol are `String`. Convert from `PathBuf` via the jail helper at boundaries (jail owns UTF-8 validation per ticket `clarion-77c6971e81`).

- **Reads-outside-project-root refusal** (ADR-021 §Layer 1, already tested at parser level): before sending `initialized`, call `manifest.validate_for_v0_1()`. On `UnsupportedCapability`, emit `CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY`, send `shutdown` + `exit`, do NOT dispatch `analyze_file`. Signoff A.2.12.

- **Ontology enforcement** (ADR-022, signoff A.2.5): drop entities whose `kind` is not in `manifest.ontology.entity_kinds`. Emit `CLA-INFRA-PLUGIN-UNDECLARED-KIND`.

- **Identity-mismatch enforcement** (UQ-WP2-11, signoff A.2.6): reconstruct `EntityId` from `(plugin_id, kind, qualified_name)` and compare against the returned `id`. Mismatch → drop + `CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH`.

- **Jail on response paths** (ADR-021 §2a): for each returned entity/edge/finding, jail each `source.file_path` and evidence anchor path. On `JailError::EscapedRoot`: drop record, emit `CLA-INFRA-PLUGIN-PATH-ESCAPE`, tick `PathEscapeBreaker`. If breaker trips: kill plugin, emit `CLA-INFRA-PLUGIN-DISABLED-PATH-ESCAPE`.

- **Inbox drain strategy** (observation `clarion-obs-cc9fe7d44c`): do NOT copy the mock's `tick()` pattern of preserving the failing frame. Real supervisor must advance past unrecoverable failing frames or the plugin will stall. Pick: (a) advance past; (b) per-frame failure count with bail-out; (c) hybrid.

- **Crate-root re-export prune** (ticket `clarion-29acbcd042`): once `PluginHost` is the façade, trim `lib.rs` re-exports down to `PluginHost`, `Manifest`, `ManifestError`, `parse_manifest`, maybe `JailError`, `LimitError`, `EntityIdError`. Leave implementation types (`Frame`, `RequestEnvelope`, `TransportError`, etc.) accessible via `clarion_core::plugin::transport::*` for tests but not at the crate root.

- **Integrations forwarding** (ticket `clarion-e46503831c`): does the supervisor need to forward `manifest.integrations` to the plugin? Check WP3 Task 6 plan. If yes, decide the type (either document `toml::Value` exposure or switch to `BTreeMap<String, BTreeMap<String, String>>`). If no, defer the ticket.

- Signoffs A.2.5, A.2.6, A.2.12 are all Task 6's gate.

### Task 7 — Crash-loop breaker

**Files**: create `plugin/breaker.rs`.

- Parameters per UQ-WP2-10 resolution: **>3 crashes in 60s** → plugin disabled, `CLA-INFRA-PLUGIN-DISABLED-CRASH-LOOP`. Hard-coded for Sprint 1; config surface deferred to WP6.

- Uses `MockPlugin::new_crashing()` — the state guards from commit `bd56cd5` ensure crash-loop tests don't silently re-initialise.

- Test: using `MockPlugin::new_crashing()`, attempt spawn/run N times in a rolling window; on the Nth failure, breaker trips and refuses further spawn attempts for the cooldown.

- Signoff A.2.7.

### Task 8 — Wire `clarion analyze`

**Files**: modify `clarion-cli/src/analyze.rs` (existing skeleton from WP1's Task 8 commit `10005e8`).

- Discover plugins via Task 5's `discovery.rs`.
- For each discovered plugin, spawn via Task 6's `PluginHost::spawn`.
- Iterate source tree, call `analyze_file` per matching file (`manifest.plugin.extensions`).
- Persist returned entities via WP1's writer-actor.
- On plugin error or cap hit: mark run as failed with diagnostic.

- Signoff A.2.8.

### Task 9 — E2E smoke test

**Files**: create `clarion-cli/tests/wp2_e2e.rs`.

- Integration test using the fixture mock-plugin binary (from Task 6) and a test `$PATH`.
- `clarion install` in a tempdir + `clarion analyze fixture_dir/` produces a completed `runs` row with the mock's expected entity persisted.

- Signoff A.2.8 (same as Task 8; this is the E2E proof).

## Exit criteria for WP2

All of:

1. Signoffs A.2.1 through A.2.12 passing. Check each box in `signoffs.md`.
2. All 9 tasks committed (5 already + 4 more = 9, plus fix rounds as needed).
3. `cargo nextest run --workspace --all-features` passes.
4. `cargo fmt --all -- --check` clean.
5. `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean.
6. `cargo deny check` clean.
7. Every UQ-WP2-* marked resolved in `wp2-plugin-host.md` §5.
8. No regressions in WP1 tests.

When WP2 is done, the user can choose to either merge to main (via
`finishing-a-development-branch`) or keep going to WP3 on a new branch.

## Pitfalls (from the first session, don't repeat)

1. **Unit structs serialize to `null`, not `{}`** — use `struct Foo {}` with braces for any wire-format empty-params type. Already burned us in Task 2 (commit `f3ca3ab` fix).

2. **`serde(flatten)` on response payload loses error fields** — if you need mutual-exclusivity between two keys, write a custom `Deserialize`. Task 2's `ResponseEnvelope` now does this.

3. **`write_frame` must flush** — without it, `BufWriter<ChildStdin>` silently deadlocks. Already fixed in Task 2.

4. **Body-read loop needs `Interrupted` retry** — EINTR can fire from signal delivery on subprocess pipes. Already fixed in Task 2.

5. **Header-line memory DoS** — `BufRead::read_line` is unbounded. Use `MAX_HEADER_LINE_BYTES = 8 * 1024` cap. Already fixed in Task 2.

6. **Reserved prefix check must run AFTER grammar check** — `CLA-INFRA-` passes the grammar `CLA-[A-Z]+(-[A-Z0-9]+)+` and must be rejected as `ReservedPrefix`, not `Malformed`. Task 1 already does this right; mirror in any grammar work Task 4/5 add.

7. **`plugin_id` ≠ `plugin.name`** — commit `bd56cd5` split these. Anywhere that needs to feed `entity_id()`, use `manifest.plugin.plugin_id`. `name` is informational / human-readable.

8. **Don't trust subagent reports** — spec reviewers must verify by reading code, not summary. The quality reviewer found 2 Critical + 4 Important issues in Task 2 that the implementer's self-review had missed.

## Current state verification

Before dispatching Task 4's first subagent, confirm the starting state:

```bash
cd /home/john/clarion
git log --oneline sprint-1/wp2-plugin-host -10
# should show: bd56cd5, b27594d, f3ca3ab, 4b20244, c2cb07a, ad8d4ce, ...

cargo nextest run -p clarion-core
# should show: 74 tests, all pass

cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
# both clean
```

If any of these fail, STOP and report — don't start Task 4 on a broken base.

---

You have everything you need. Start by dispatching the Task 4 implementer.
