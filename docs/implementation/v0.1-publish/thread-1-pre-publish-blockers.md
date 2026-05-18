# Thread 1 — Pre-publish blockers (program of work)

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans`. Each workstream is independently dispatchable; sequencing is described in §1.

**Status**: drafted 2026-05-18 — not yet broken into Filigree issues.
**Predecessor context**: Sprint 2 closed GREEN ([`../sprint-2/signoffs.md`](../sprint-2/signoffs.md)); the 2026-05-18 codebase archaeology ([`../../arch-analysis-2026-05-18-1244/04-final-report.md`](../../arch-analysis-2026-05-18-1244/04-final-report.md)) is the authoritative current-state snapshot.
**Goal**: take the amended-v0.1 MCP-MVP from "works on the elspeth-slice corpus on the author's box" to "publishable v0.1" — meaning *an outside operator can install it, point it at an arbitrary repo without leaking secrets, and find their way to first value in five minutes*.

**Scope discipline**: this is *only* Thread 1 (pre-publish operational/security blockers). Two adjacent threads exist and are NOT in this program:

- **Thread 2** — reconcile `CON-FILIGREE-02` ("Filigree `registry_backend` is a hard v0.1 dependency") against the 2026-05 scope amendment (deferred WP9-B + WP10 to v0.2). One-day editorial pass in `requirements.md` + amendment memo; out of scope here.
- **Thread 3** — dogfood pass: `clarion analyze` + reproduce the arch-analysis findings via the 7 MCP tools against Clarion / Filigree / Wardline themselves. Out of scope here.

---

## 1. Workstream map and sequencing

Four workstreams. **A** is the only one with significant engineering weight; **B/C** are mostly docs + packaging; **D** is the verification gate that proves A/B/C composed correctly.

```
A  WP5 pre-ingest secret scanner ─────────────────────┐
B  Operator-facing entry surface ─────────────────────┤
C  Distribution mechanics + clarion install --force ──┤
                                                      ↓
                          D  External-operator smoke test (gate)
```

**A** ships in parallel with **B** and **C**: the scanner is a `clarion-core`-internal change; the docs / packaging touch repo-root + CI + plugin manifests. No file-level collisions expected.

**D** is the publish gate. Until D is green on a fresh VM, no `v0.1` tag.

**Effort estimate** (single engineer, agentic velocity): **A** ≈ 6–9 working days; **B** ≈ 1–2 days; **C** ≈ 2–4 days (depending on chosen distribution path); **D** ≈ 1 day. Total: ~3 weeks elapsed if serialised, ~2 weeks if **B/C** run in parallel behind **A**.

**Filigree umbrellas to create** before kickoff:

| Umbrella | Title | Labels |
|---|---|---|
| WP5 umbrella | `WP5 — Pre-ingest secret scanner (ADR-013)` | `release:v0.1`, `sprint:3`, `wp:5`, `adr:013`, `tier:a` |
| Repo-UX umbrella | `Publish-prep — operator-facing entry surface` | `release:v0.1`, `sprint:3`, `docs`, `tier:a` |
| Distribution umbrella | `Publish-prep — distribution + install ergonomics` | `release:v0.1`, `sprint:3`, `tier:a` |
| Publish gate | `Publish gate — external-operator smoke test` | `release:v0.1`, `sprint:3`, `tier:a` |

The L-* arch-analysis items already in flight (L-1 `--force`, L-7 blank actor, etc.) fold into Workstream C as named subtasks (§4); do **not** double-file.

---

## 2. Workstream A — WP5 Pre-ingest secret scanner

### A.0 Spec source

[ADR-013](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) fully specifies behaviour, rule set, baseline format, override semantics, and plugin-boundary interaction. It is Accepted; do not re-litigate. The `system-design.md` §10 paragraph this ADR formalises was retained when ADR-013 was authored.

The requirements floor:

- `NFR-SEC-01` — pre-ingest scan + block + baseline whitelist
- `NFR-SEC-04` — security events as findings (audit surface)
- `NFR-OPS-01` / `NFR-OPS-04` — single-binary distribution (rules out the Python `detect-secrets` embed; ADR-013 §Alternative 1 is the dispositive read)

### A.1 Crate layout decision

**Decision required at A.1**: does the scanner ship as a new sibling crate `crates/clarion-scanner/`, or as a `clarion-core::secret_scanner` module?

**Recommendation**: new sibling crate `crates/clarion-scanner/`. Reasons:
1. ADR-013 calls out the implementation as `clarion_scanner` crate (line 40); use the name the ADR uses.
2. The rule set is data-heavy (regex tables + entropy tuning) and benefits from being a leaf crate with no `tokio` / `rusqlite` deps.
3. Keeps `clarion-core/src/plugin/host.rs` from growing further (currently 3 126 LOC; arch-analysis §5.4 A-3 named this an accepted-but-watched risk).

The CLI consumes it via `clarion-cli/src/analyze.rs` directly; the writer-actor never sees it (findings flow through the existing `HostFinding` → `WriterCmd::InsertEntity` path with `properties.briefing_blocked = "secret_present"` plus a `CLA-SEC-SECRET-DETECTED` finding row).

### A.2 Task breakdown

Tasks are sized to fit a single agent pass each (≤ ~500 LOC plus tests). They sequence top-to-bottom; tasks 1 and 2 can parallelise if dispatched to two agents.

#### Task 1 — Rule set + pattern registry

**Files**:
- Create: `crates/clarion-scanner/Cargo.toml` (workspace member)
- Create: `crates/clarion-scanner/src/lib.rs`, `src/patterns.rs`, `src/entropy.rs`
- Modify: `Cargo.toml` (workspace root — add `crates/clarion-scanner`)

**Scope**: implement the named-credential regex table from ADR-013 lines 35–38 (AWS, GitHub, Anthropic, OpenAI, Stripe, Slack, JWT, private-key headers, contextual-credential names). Implement Shannon entropy over a byte slice. Public surface:

```rust
pub struct Scanner { /* compiled regex set + entropy thresholds */ }
pub struct Detection {
    pub rule_id: &'static str,        // e.g. "AwsAccessKeyId", "HighEntropyBase64"
    pub category: SecretCategory,     // for the CLA-SEC- finding mapping
    pub byte_offset: usize,
    pub line_number: u32,
    pub matched_len: usize,           // never persist the literal bytes
    pub hashed_secret: [u8; 20],      // sha1, baseline-compat
}
impl Scanner {
    pub fn new() -> Self;             // default thresholds per ADR-013
    pub fn scan_bytes(&self, buf: &[u8]) -> Vec<Detection>;
}
```

**Tests**: one positive + one negative fixture per rule (AWS access key, GitHub PAT, RSA private-key header, etc.). For high-entropy detection: a 32-char base64-looking string passes; a UUID fails. Fixtures live in `crates/clarion-scanner/tests/fixtures/`.

**Exit**: `cargo test -p clarion-scanner` green; clippy `-D warnings` clean; the crate compiles with no `tokio` / `rusqlite` / `serde_norway` deps (assert via `cargo tree -p clarion-scanner | head -30`).

#### Task 2 — Baseline parser (`.clarion/secrets-baseline.yaml`)

**Files**:
- Create: `crates/clarion-scanner/src/baseline.rs`
- Modify: `crates/clarion-scanner/src/lib.rs` (re-export)

**Scope**: parse the `detect-secrets` v1.x baseline schema (ADR-013 lines 60–68). Required schema fields: `version` (must equal `"1.0"`), `results` (map of relative-path → list of `{type, hashed_secret, line_number, is_secret, justification}`). The `justification` field is required (ADR-013 line 71). Missing → emit `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION` (rule constant in `clarion-scanner` consumed by the CLI).

Use `serde_norway` (already a workspace dep; replaces `serde_yaml` per commit `9ffc5c8`). Match-then-suppress at `Detection` granularity, keyed on `(file_path, hashed_secret, line_number)`. Provide:

```rust
pub fn load_baseline(path: &Path) -> Result<Baseline, BaselineError>;
pub fn suppress(detections: Vec<Detection>, baseline: &Baseline, file: &Path) -> SuppressionResult;
```

Where `SuppressionResult { allowed: Vec<Detection>, suppressed: Vec<Detection> }` — both retained so the CLI can emit `CLA-SEC-SECRET-DETECTED` for unsuppressed *and* a `CLA-INFRA-SECRET-BASELINE-MATCH` info-level finding when a baseline entry actually fires (audit surface per `NFR-SEC-04`).

**Tests**: fixture baseline file + scanner output → asserts allowed/suppressed partitioning; fixture missing-justification → returns the expected `BaselineError` variant; baseline path absent → returns an empty baseline (no error).

**Exit**: `cargo test -p clarion-scanner` green including the baseline module; baseline round-trip test (parse → serialise → parse) byte-identical.

#### Task 3 — CLI wiring: pre-ingest hook in `analyze::run`

**Files**:
- Modify: `crates/clarion-cli/src/analyze.rs`
- Modify: `crates/clarion-cli/Cargo.toml` (add `clarion-scanner` dep)
- Create: `crates/clarion-cli/src/secret_scan.rs` (orchestration module — keep `analyze.rs` from growing further per arch-analysis H-1)

**Scope**: between the source-tree walk (currently `collect_source_files` at `analyze.rs:182`) and the per-plugin processing loop, insert a pre-ingest scan pass. For each file in `source_files`:

1. Read the file buffer (already needed for `analyze_file` RPC dispatch; this is the natural place — do **not** re-read in the plugin pass).
2. Run `Scanner::scan_bytes`; apply baseline suppression.
3. If `allowed` non-empty:
   - Mark the file `briefing_blocked: secret_present` in a `BTreeMap<PathBuf, BlockReason>` carried through to the per-plugin pass.
   - Accumulate `CLA-SEC-SECRET-DETECTED` findings (one per detection, severity `error`).
4. Files in this map still go to the plugin (structural extraction runs, ADR-013 line 46) but the entities emitted carry `properties.briefing_blocked = "secret_present"`.

The `briefing_blocked` flag plumbs through `RawEntity.extra` → `EntityRecord.properties_json` → the `entities.properties` column. The MCP `summary` tool already reads `entities.properties` for cache lookup (see `clarion-mcp/src/lib.rs:1010`); add the block check there in Task 5.

**Tests**: integration test in `crates/clarion-cli/tests/analyze.rs` — fixture project with a `.env` containing a fake AWS key. Assert:
- `analyze` exits 0 (`SoftFailed` — partial success path; arch-analysis H-1 coverage gap).
- `entities` table has rows for the `.env` file's structural entities.
- Those entities' `properties_json` contains `"briefing_blocked":"secret_present"`.
- `findings` table has one `CLA-SEC-SECRET-DETECTED` row referencing that file.

**Exit**: integration test green; the new `secret_scan.rs` module is < 250 LOC; `analyze.rs` net growth ≤ 30 LOC (delegate to the new module).

#### Task 4 — Override semantics: `--allow-unredacted-secrets`

**Files**:
- Modify: `crates/clarion-cli/src/cli.rs` (clap definition)
- Modify: `crates/clarion-cli/src/analyze.rs` + `secret_scan.rs`

**Scope**: implement the override path per ADR-013 lines 74–82.

- TTY: prompt with the detection list; require the operator to type the literal string `yes-i-understand`.
- Non-TTY: require both `--allow-unredacted-secrets` AND `--confirm-allow-unredacted-secrets=yes-i-understand`. Anything else → exit non-zero with `CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED` to stderr (do **not** silently bypass).
- When the override fires for file `F`: F's entities are NOT marked `briefing_blocked`; emit one `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` finding per affected file (severity `error`, audit surface).
- Record `{override_used: true, files_affected: [...]}` in `runs.stats` (the existing `runs.stats` text column).

**TTY detection**: use `std::io::IsTerminal` on stdin; this is in `std` since Rust 1.70 and the workspace's `rust-toolchain.toml` is well past that.

**Tests**:
- Non-TTY override-confirmed: secret-bearing fixture + both flags → no `briefing_blocked`, one `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` finding per file.
- Non-TTY override-unconfirmed: only `--allow-unredacted-secrets` → exit code 78 (`EX_CONFIG` per `sysexits.h`; pick once, document in `cli.rs`); `CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED` finding NOT persisted (run never started); stderr contains the rule-ID for the operator to grep.
- TTY path: separate `expectrl`-style test or skip with a `#[ignore]` and document; TTY behaviour is verified manually in Workstream D.

**Exit**: integration tests green; the override surface has both happy-path and the "footgun absent confirmation" path covered.

#### Task 5 — MCP-side awareness of `briefing_blocked`

**Files**:
- Modify: `crates/clarion-mcp/src/lib.rs` (the `summary` tool dispatch path)
- Modify: `crates/clarion-mcp/tests/storage_tools.rs`

**Scope**: when `summary(id)` is called on an entity whose `properties.briefing_blocked == "secret_present"`, return an envelope shape (do **not** invoke the LLM, do **not** consume budget):

```json
{
  "entity_id": "python:function:demo.foo|function",
  "summary": null,
  "briefing_blocked": "secret_present",
  "remediation": "File flagged by pre-ingest secret scan. Fix the secret or whitelist via .clarion/secrets-baseline.yaml; ADR-013."
}
```

This is the consult-mode-agent surface for "the absence of a summary is policy, not pipeline failure" (ADR-013 line 49). The four already-existing `issues_unavailable` envelopes in `clarion-mcp::filigree` are the precedent for this envelope shape.

**Tests**: storage-tools test with a fixture entity flagged `briefing_blocked` → `summary` tool returns the envelope; no row is added to `summary_cache`; the budget ledger is untouched.

**Exit**: storage-tools test green; manual verification that `RecordingProvider` is NOT invoked on a blocked entity (assert no fixture file is created in the recording-mode test setup).

#### Task 6 — Rule catalogue entries in `detailed-design.md`

**Files**:
- Modify: `docs/clarion/v0.1/detailed-design.md` (§5 rule catalogue)

**Scope**: append rule rows for `CLA-SEC-SECRET-DETECTED`, `CLA-SEC-UNREDACTED-SECRETS-ALLOWED`, `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION`, `CLA-INFRA-SECRET-BASELINE-MATCH`, `CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED`. Each row: rule-ID, severity, category, one-sentence description, one-sentence remediation, ADR pointer.

This makes the WP9-B Filigree-emission story (deferred to v0.2) able to round-trip these IDs without a separate spec pass.

**Exit**: design-doc lint passes (no broken cross-references); ADR-013 retains the canonical authority on *behaviour*, the design doc carries the *catalogue*.

#### Task 7 — Documentation: operator surface

**Files**:
- Create: `docs/operator/secret-scanning.md`
- Modify: `docs/operator/README.md` (add link)

**Scope**: operator-facing doc — what gets blocked, how to whitelist via `.clarion/secrets-baseline.yaml`, what the override does, how to find the audit trail (`findings` table queries, future Filigree integration). One page, ≤ 250 lines. Link from the top-level README (Workstream B Task 1).

**Exit**: a non-engineer can read this doc and resolve a baseline false-positive without reading ADR-013.

### A.3 Workstream A exit criteria

All tasks green; in addition:
- `cargo test --workspace --all-features` passes.
- Existing CI gates (ADR-023 floor) unchanged in pass status.
- A new E2E test under `tests/e2e/` (parallels `sprint_1_walking_skeleton.sh`) runs `clarion install && clarion analyze` against a fixture project containing one known secret and asserts: exit 0, entities present, briefing_blocked flagged, finding persisted.
- The Sprint-1 walking-skeleton E2E continues to pass (no regression on the clean-fixture path).

---

## 3. Workstream B — Operator-facing entry surface

### B.1 Top-level repo README

**Files**:
- Create: `README.md` at repo root.

**Scope**: there is no top-level README currently. The reader-ladder under `docs/suite/briefing.md` → `docs/suite/loom.md` → `docs/clarion/v0.1/README.md` assumes the reader already knows to start there. A first-time visitor (PyPI / crates.io / GitHub front page) has no entry point.

The README must answer, in order:

1. **What this is** — one paragraph. Use the briefing's framing ("Clarion is a code-archaeology tool…") but compress to ~80 words.
2. **What it does today** — bullet list of the 7 MCP tools and what each answers, with one example invocation each.
3. **Quick start** — `clarion install && clarion analyze && clarion serve`, with the expected stdout shapes. Link to the operator tutorial (Task B.2).
4. **Status** — explicit "v0.1 — Python only; structural + on-demand LLM summarisation; Filigree finding emission deferred to v0.2." Quote the scope: don't oversell.
5. **Project layout** — three-sentence map (Rust workspace + Python plugin + docs) with links to `docs/clarion/v0.1/README.md` for the design ladder and `docs/clarion/adr/README.md` for the ADR index.
6. **Contributing** — pointer to `CLAUDE.md` and the test commands (ADR-023 floor).

**Length target**: ≤ 200 lines. No installation instructions deeper than `cargo install ...` + `pipx install clarion-plugin-python` (Workstream C delivers the actual commands).

**Exit**: a developer who has never seen the repo can answer "is this for me?" in 60 seconds.

### B.2 Getting-started tutorial

**Files**:
- Create: `docs/operator/getting-started.md`
- Modify: `docs/operator/README.md` (index)

**Scope**: a single-flow tutorial: install Clarion, run against a tiny example repo provided in `examples/quickstart-repo/` (or use `crates/clarion-plugin-fixture`'s test inputs — pick one, document), connect a consult-mode agent over MCP, ask one question, see a real answer. Includes:

- Prerequisite versions (Rust toolchain per `rust-toolchain.toml`; Python 3.11+; `pyright-langserver` 1.1.409 — pinned in the Python plugin manifest).
- Required env vars for live LLM calls (`OPENROUTER_API_KEY`). Note that `clarion analyze` works without the LLM (structural-only); summarisation requires the key.
- The seven MCP tool names with one example each.
- Troubleshooting: plugin not discovered → check `$PATH`; secret block fires → link to `secret-scanning.md`.

**Exit**: a fresh operator runs through the tutorial start-to-finish in ≤ 15 minutes and gets a non-trivial answer from an agent.

### B.3 Workstream B exit criteria

- Top-level `README.md` present and reviewed.
- Getting-started tutorial walked end-to-end by someone who did NOT write it (the publish-gate operator in Workstream D, on a fresh VM).

---

## 4. Workstream C — Distribution + install ergonomics

### C.1 `clarion install --force` (arch-analysis L-1)

**Files**:
- Modify: `crates/clarion-cli/src/install.rs`, `src/cli.rs`

**Scope**: the `--force` flag is declared in the clap definition (`cli.rs:17–18`) and rejected at runtime (`install.rs:87–92`). Wire it up: when set, remove existing `.clarion/` *atomically* (rename-to-tmpdir + remove tmpdir, never partial deletes), then proceed. Refuse if `.clarion/clarion.db` shows a `runs` row with `status='running'` (someone else is using this DB) unless `--force --force` (double-force) is passed — the operator owns the override.

This closes Filigree issue `clarion-2d178ddda0` (P3, ready).

**Exit**: integration test in `crates/clarion-cli/tests/install.rs` exercises the three paths (no `.clarion/` → install; `.clarion/` present without `--force` → exit non-zero with helpful message; `.clarion/` present with `--force` → atomic replace, success).

### C.2 Distribution decision

**Decision required at C.2**: which packaging path ships v0.1? Three viable options; pick one and ADR it.

| Option | Rust binary | Python plugin |
|---|---|---|
| (a) Source-only — `cargo install --git` + `pipx install --editable git+https://…` | repo URL | repo URL |
| (b) GitHub Releases — pre-built binaries per platform; plugin sdist attached | `gh release download` + `mv to ~/.cargo/bin/` | `pipx install ./clarion-plugin-python-*.tar.gz` |
| (c) Public registries — `cargo install clarion-cli` (crates.io) + `pipx install clarion-plugin-python` (PyPI) | crates.io | PyPI |

**Recommendation**: (b) for the v0.1 publish, (c) for v0.2 once names are reserved and the publish cadence is established. Reasons:
- (a) burns ten minutes of cargo compile on every new install — bad first impression.
- (c) requires name reservation on crates.io + PyPI, and version-bump discipline; not free.
- (b) ships in a day with `cargo-dist` or a hand-written GH Actions workflow; the install command is one `curl | tar` away from being scriptable.

If (b) is chosen, file an ADR (ADR-032 candidate: "v0.1 distribution via GitHub Releases; promote to public registries at v0.2"). The ADR is short — half a page.

**Files** (if (b) chosen):
- Create: `.github/workflows/release.yml`
- Create: `docs/clarion/adr/ADR-032-v0.1-distribution.md` (or reuse the next free ADR number — check the index)

**Workflow shape**:
- Trigger: `push: tags: ['v0.1*']`.
- Matrix: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin` minimum. Windows is out of v0.1 scope (no requirement; `setrlimit` is Unix-only — `clarion-core::plugin::limits`).
- Build the `clarion` binary; build the Python plugin sdist (`python -m build --sdist plugins/python`).
- Attach both to the GH release; auto-generate release notes from `git log v0.1-sprint-2..HEAD` filtered to merge commits.

**Exit**: a tag `v0.1.0` push produces a GH release with downloadable artifacts; a hand-test on a fresh VM (Workstream D) installs from those artifacts successfully.

### C.3 Plugin auto-discovery affordance

**Files**:
- Modify: `docs/operator/getting-started.md` (Workstream B Task 2)
- Possibly: `crates/clarion-cli/src/cli.rs` (add a `clarion doctor` subcommand — optional, see below)

**Scope**: plugin discovery walks `$PATH` looking for `clarion-plugin-*` executables (ADR-002 / L9 convention). For someone running `pipx install clarion-plugin-python`, the plugin lands in `~/.local/bin/` — which is `$PATH` on most Linux but not always on macOS, and is silent when missing. Two paths:

- Document the `$PATH` requirement crisply in the tutorial. Cheap; punts the problem.
- Add a `clarion doctor` subcommand that prints discovered plugins and a yes/no for "found a Python plugin." Spends a day; the operator gets a self-diagnosis path.

**Recommendation**: tutorial only for v0.1; `clarion doctor` is a v0.2 nice-to-have. Document the failure mode (zero plugins discovered → `SkippedNoPlugins`, which currently exits 0 — verify and note this).

### C.4 Workstream C exit criteria

- `clarion install --force` lands and tests green.
- Distribution decision recorded as an ADR; the chosen path is exercised end-to-end (a release is produced).
- Tutorial documents the installation path that the chosen distribution implies.

---

## 5. Workstream D — Publish gate: external-operator smoke test

### D.1 Test setup

**Files**:
- Create: `tests/e2e/external-operator-smoke.md` (manual checklist) — and/or `.github/workflows/external-operator-smoke.yml` if automatable.

**Scope**: spin up a fresh VM (or container — `ubuntu:24.04` is a fair proxy). Use **only** the installation instructions in `README.md` and `docs/operator/getting-started.md`. No `git clone`, no `cargo build` from source.

Steps:

1. Install Rust binary per Workstream C's chosen path.
2. Install Python plugin per the same.
3. `clarion install` against a small public Python project (suggestion: `requests==2.32.x` source tarball — ~7k LOC, well-behaved, no secrets).
4. `clarion analyze` — assert exit 0, non-empty entities count.
5. `clarion serve` in one shell; connect a consult-mode agent via MCP in another (Claude Desktop or `mcptool` CLI — pick one, document).
6. Ask the agent three pre-scripted questions:
   - "List the top-level modules in this project."
   - "What calls `requests.get`?"
   - "Summarise `requests.sessions.Session.send`." (forces a live LLM call → exercises OpenRouter path + budget + cache.)
7. Re-run `analyze` to confirm idempotency (re-walk doesn't double-emit; existing entities updated).
8. Plant a `.env` file containing `AKIA0123456789ABCDEF` in the test project; re-run `analyze`; assert the WP5 block fires + finding persists.

### D.2 Acceptance

The smoke test passes if all eight steps complete without operator improvisation. Any step that requires reading source code (rather than the docs) is a Workstream B bug to fix before publish.

### D.3 Workstream D exit criteria

- Smoke test executed at least once on each platform the release workflow produces artifacts for.
- The operator who runs it is NOT the author. Recruit a second engineer or use a fresh agent session with no prior repo context.
- Any deviations between docs and reality become Workstream B tickets; cycle until clean.

---

## 6. What this program does NOT cover

To prevent scope creep mid-execution, the following are explicitly OUT:

- **WP9-B / WP10** (findings emission to Filigree, registry_backend, SARIF translator) — Thread 2; v0.2 in the amended plan.
- **WP4 phases beyond Phase 1** (clustering, Phase-7 `CLA-*` cross-cutting rules, Phase-8 entity-set diff) — v0.2.
- **WP7 guidance system** — v0.2.
- **Multi-language plugins** — `NG-15`, v0.2+.
- **MCP `summary(id)` module/subsystem aggregation** — `ADR-030` defers to v0.2; v0.1 ships leaf-only.
- **The L-3 through L-8 arch-analysis items** — in flight as separate Filigree issues; pick them up opportunistically but do not block the publish on them. L-1 is named here because it is install-ergonomic; the rest are quality polish, not gate items.

---

## 7. Filigree seeding (suggested first commands)

```bash
# Workstream A — WP5 umbrella + 7 task issues
filigree create --type=work_package --title="WP5 — Pre-ingest secret scanner (ADR-013)" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,tier:a" --priority=P1

# Then per task; example:
filigree create --type=task --title="WP5 Task 1 — Scanner crate + rule set + entropy" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,crate:scanner"
# repeat for tasks 2–7

# Workstream B
filigree create --type=task --title="Publish-prep: top-level README" --labels="release:v0.1,sprint:3,docs"
filigree create --type=task --title="Publish-prep: getting-started tutorial" --labels="release:v0.1,sprint:3,docs"

# Workstream C
filigree create --type=task --title="Publish-prep: clarion install --force" --labels="release:v0.1,sprint:3,crate:cli"
# (Folds in clarion-2d178ddda0; close that with a forward-pointer.)
filigree create --type=task --title="Publish-prep: choose v0.1 distribution path + ADR + release workflow" \
  --labels="release:v0.1,sprint:3"

# Workstream D
filigree create --type=task --title="Publish gate: external-operator smoke test on fresh VM" \
  --labels="release:v0.1,sprint:3,tier:a" --priority=P1
```

Set the dependencies:

```bash
# A blocks D
filigree add-dep <D-id> --blocked-by <A-umbrella-id>
# B blocks D
filigree add-dep <D-id> --blocked-by <B-readme-id> --blocked-by <B-tutorial-id>
# C blocks D
filigree add-dep <D-id> --blocked-by <C-distribution-id>
# C --force task supersedes the standalone L-1
filigree close clarion-2d178ddda0 --reason="superseded by Publish-prep --force task <new-id>"
```

---

## 8. References

- [ADR-013 — Pre-ingest secret scanner with LLM-dispatch block](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md)
- [ADR-007 — Summary cache key (briefing_blocked semantics)](../../clarion/adr/ADR-007-summary-cache-key.md)
- [ADR-021 — Plugin authority hybrid (path-jail upstream of scanner)](../../clarion/adr/ADR-021-plugin-authority-hybrid.md)
- [ADR-023 — Tooling baseline (CI floor every PR must clear)](../../clarion/adr/ADR-023-tooling-baseline.md)
- [Requirements — NFR-SEC-01, NFR-SEC-04, NFR-OPS-01, NFR-OPS-04](../../clarion/v0.1/requirements.md)
- [System design — §10 Security, pre-ingest redaction paragraph](../../clarion/v0.1/system-design.md)
- [v0.1-plan.md — WP5 scope](../v0.1-plan.md#wp5--pre-ingest-secret-scanner)
- [Sprint-2 scope amendment — explicit WP5 deferral rationale, "production deployment against unknown corpora gates on this returning"](../sprint-2/scope-amendment-2026-05.md)
- [Arch analysis final report — §7 follow-ups #9 (L-1) and §5.3 L-2 (closed)](../../arch-analysis-2026-05-18-1244/04-final-report.md)
