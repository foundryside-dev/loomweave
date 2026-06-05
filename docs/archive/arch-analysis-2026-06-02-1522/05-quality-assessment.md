# 05 — Quality Assessment

**Date:** 2026-06-02 · v1.1.0. Evidence-based, no diplomatic softening. Severity: 🔴 High / 🟡 Medium / 🟢 Low. Every item carries file:line evidence and a fix sketch. Filigree issue IDs cited where one already exists.

---

## 1. Headline quality posture

Clarion is a **well-built system carrying two distinct debts**:

1. **Change-amplification debt** — four files hold most of the operational complexity. Internally they are well-factored (named functions, inline-test discipline); the problem is *one file's worth of change-risk per touch*. This is the conventional debt the prior analyses flagged, now larger.
2. **Documentation-integrity debt** — `system-design.md` has drifted from the code across five subsystems with no reconciling ADR. In a repo whose own rules make the design doc the acceptance surface, this is a correctness-of-record defect, not a cosmetic one. It is *cheap* to fix (doc edits) and *expensive* to leave (every future contributor is misled by the canonical doc).

The encouraging signal: three prior-flagged 🔴/🟡 defects were genuinely closed this cycle (PRAGMA identity, async-blocking reqwest, pyright restart cap). The team fixes what's filed. The bottleneck is that the largest debts have *tickets but no execution*.

## 2. Metrics snapshot

| Metric | Value | Note |
|---|---|---|
| Rust src LOC | ~47,300 | 6 crates |
| Rust test LOC | ~21,400 | ~45% of src; storage tests > storage src |
| Python src / test | ~3,173 / ~3,400+ | parity-fixture gated |
| Largest files | 7,101 / 4,387 / 3,542 / 2,958 | mcp·http·analyze·host |
| Inter-crate cycles | 0 | clean DAG |
| MCP tools | 35 | doc says 8 |
| SQLite tables | 13 + FTS5 + view | doc says 6+FTS5 |
| Unsafe blocks | 2 | 1 non-test (`host.rs:602` `pre_exec` setrlimit, SAFETY-commented); 1 test-only (`plugin-fixture/main.rs:155`) |

> **Erratum (captured during analysis):** all LOC figures here are HEAD-at-capture. `http_read.rs` grew from **4,387 → 4,765** *during* the synthesis window (two Wardline-T3.4 commits, `caa2665`/`204790e`). The 4,387 figure used throughout 01–06 was exact when read; it is now ~4,765. No conclusion changes — http_read remains the #2 monolith either way.

## 3. Findings by severity

### 🔴 High

**Q1 — `clarion-mcp/src/lib.rs` is 7,101 LOC.** Holds 18 in-line tool handlers + `ServerState` + `BudgetLedger` + dispatch + tests. WS5 extracted 17 tools into `catalogue/` but stopped there. Every MCP change vector routes through this file. *Fix:* finish the split into `tools/` (one module per category) — filigree **`clarion-42cbd8a25a`** (proposed). Mechanical, behavior-preserving.

**Q2 — `clarion-cli/src/http_read.rs` is 4,387 LOC.** The whole 16-route federation surface + hand-rolled HMAC + 4 handler families in one file. *Fix:* split by route family (`files`, `linkages`, `identity`, `wardline`) + an `auth` module. No ticket yet — file one. Security-sensitive code (auth) buried in a 4K-line file is a review hazard.

**Q3 — `clarion-cli/src/analyze.rs::run_with_options` is an 836-LOC monolith** carrying `#[allow(clippy::too_many_lines)]`, grew ~266 LOC since the prior analysis. Every pipeline change vector (orphan recovery, secret scan, plugin loop, breakers, ingestion, clustering, SEI, commit) shares one scope. *Fix:* extract per-phase functions — filigree **`clarion-cb9676de57`** (→ `analyze/phase3.rs` + `analyze/mapping.rs`, proposed/unstarted).

**Q4 — `clarion-core/src/plugin/host.rs` is 2,958 LOC in one `impl`** (+678 inline-test LOC). Validation pipeline, edge pipeline, stats validation, briefing-block, subprocess ctor, stderr drain, `connect()` all one unit. *Fix:* split along lifecycle / pipeline / IO axes — filigree **`clarion-2b8811da39`** (proposed).

**Q5 — Documentation drift D1–D8 (see `04` §4).** `system-design.md` §2/§5/§6/§8/§9, `detailed-design.md` schema, and `CLAUDE.md` Layout each contradict shipped code; no ADR/errata reconciles them. **"Ahead" drift** (D1/D2/D5/D6/D7/D8) is pure doc cleanup (code wins). **"Behind" drift** was checked against `requirements.md`: D3 (§5 budget engine) is a **confirmed v1.1 deferral** (NFR-COST-01/03 → ADR-030) — mirror the notice into §5; D3a (Anthropic→OpenRouter pivot) **needs an ADR** to supersede `CON-ANTHROPIC-01`. *Fix:* a documentation-reconciliation pass (details in `06` §3). Highest-ROI work in the report: small effort, removes an active trap from the canonical record. (The one item that is *not* doc cleanup — D4a / Q12 — is broken out below and gates the release.)

### 🟡 Medium

**Q6 — Wardline federation asterisk still live.** `plugins/python/.../wardline_probe.py:38` does `importlib.import_module("wardline.core.registry")`. `loom.md §5` records the Wardline-side prerequisite (NG-25 descriptor, SP2) as **met**. Migration to the NG-25 descriptor read is filigree **`clarion-1f6241b329`** (open/ready/P2). *Fix:* execute the migration; retire asterisk #2 per the written condition. Fail-soft today (returns `absent` against a rebuilt Wardline), so not urgent — but it is the one remaining named federation coupling.

**Q7 — `clarion-llm` crate not extracted.** `reqwest`/`tempfile`/`which` are direct `clarion-core` deps *only* for `llm_provider.rs` (2,500 LOC) — a live TLS/HTTP transport living in the untrusted-plugin-supervisor crate, contradicting `core/lib.rs:1`'s "domain types, identifiers, provider traits" charter. Filigree **`clarion-141e9c08c8`** (P2). *Fix:* extract; this also forces the currently-missing cross-provider trait-contract uniformity test.

**Q8 — SEI matcher loads all alive bindings into a HashMap** at re-index start (`sei.rs`). Unbounded memory at elspeth scale (~425K LOC → potentially hundreds of thousands of bindings). *Fix:* stream/window the match, or bound by changed-file locus using `sei_prior_index`.

**Q9 — `analyze_runs.rs` has no stale-`running`-row reconciliation** when the MCP supervisor process crashes mid-analyze. A crashed run leaves a `running` row that can block future `analyze_start`. (The *CLI* path has orphan-run recovery; the *MCP-launched* path does not mirror it.) *Fix:* reconcile stale `running` rows on `serve` startup, mirroring `analyze.rs` orphan recovery. There is a related filigree task `clarion-f9027d2187` (runs.owner_pid + heartbeat_at).

**Q10 — Codex provider cost accounting is blind.** `llm_provider.rs:544` hardcodes `cost_usd = 0.0`; malformed Codex JSONL silently under-reports tokens (`:1039-1056`), so `session_token_ceiling` enforcement can diverge from true spend. *Fix:* parse Codex cost if available, or surface a "cost-unknown" flag rather than 0.0.

**Q11 — Wardline taint writer-actor runs in the HTTP runtime** (ADR-036) with no dedicated health-check surface; if it wedges, the HTTP API has no signal. *Fix:* expose its liveness via `_capabilities` or `doctor`.

**Q12 — Log-only `HostFinding`s may breach a baselined requirement (possible release gap, verify).** Plugin `HostFinding`s are logged not persisted ("Tier B persistence is future work", `analyze.rs:626`). This is in direct tension with **`REQ-ANALYZE-06`** (baselined: *no silent fallbacks — every recoverable failure emits a finding visible in `stats.json`, the store, and Filigree*). If failure findings are not persisted, REQ-ANALYZE-06 is **unmet** — a release gap, not cleanup (see `04` D4a / `06` §3). Separately, four of five §6 `CLA-FACT-*` *structural* findings are unimplemented but have **no baselined REQ** (safe to cut/defer). *Fix:* (1) verify+persist failure findings (gate release on it); (2) decide scope for the structural findings and align §6.

### 🟢 Low

**Q13 — Facade-bypass leak.** `clarion-storage/src/writer.rs:537` reaches `clarion_core::plugin::manifest::RESERVED_ENTITY_KINDS` via internal module path, bypassing the `lib.rs` facade. *Fix:* re-export through the facade or expose `Manifest::is_reserved_kind`.

**Q14 — No design doc enumerates the 12 secret-scan rules.** Readers must consult `patterns.rs`. *Fix:* table in `detailed-design.md` §10 (and document the OpenAI extended prefixes / Stripe test keys ADR-013 omits).

**Q15 — `query.rs` (1,727) and `writer.rs` (1,211, 18-variant `WriterCmd` match)** approaching split-me size; `pyright_session.py` (1,427) has four independent AST walks over the same tree. *Fix:* watch; split when the next change touches them.

**Q16 — Test coverage unevenness.** Storage and scanner are saturated; `host.rs` integration coverage is happy-path-heavy (`tests/host_subprocess.rs` ~325 LOC for one walkthrough); no unit test for `WriterCmd::ResumeRun`; no cross-provider contract test. *Fix:* targeted error-path tests for `HostError` variants and `ResumeRun`.

## 4. Debt prioritization (ROI view)

| Priority | Item | Effort | Payoff |
|---|---|---|---|
| 0 | **Q12/D4a** verify failure findings are persisted (REQ-ANALYZE-06) | Low (verify) | **Gates release** — possible unmet baselined requirement |
| 1 | **Q5** doc reconciliation (D1–D8 + D3a ADR) | Low (doc edits) | High — removes active trap from canonical record |
| 2 | **Q1** mcp/lib.rs split (`clarion-42cbd8a25a`) | Med (mechanical) | High — biggest change-risk surface |
| 3 | **Q3** analyze.rs split (`clarion-cb9676de57`) | Med | High |
| 4 | **Q6** Wardline asterisk retire (`clarion-1f6241b329`) | Med | Med — closes a federation coupling whose prereq is met |
| 5 | **Q7** clarion-llm extract (`clarion-141e9c08c8`) | Med | Med — shrinks supervisor trust surface + forces a test |
| 6 | **Q8/Q9** SEI scale + MCP run reconciliation | Med | Med — elspeth-scale + operability |
| 7 | **Q2/Q4** http_read/host split | Med | Med — review-safety |

The top item is documentation, not code: it is the cheapest fix with the highest correctness-of-record payoff, and four of the next five already have tickets that simply need to be scheduled.
