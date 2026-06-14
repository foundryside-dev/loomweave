# Rust Plugin Dogfood + Scale QA Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the Rust plugin production-grade on real code at real scale with committed numbers; fix the two absorbed warts (spurious-OOM mislabel, non-hermetic install); leave a reproducible QA harness.

**Architecture:** Measurement-first QA: a fresh maturin-built venv (the proven wheel-smoke pattern) supplies the INSTALLED binary with the Rust plugin discoverable; four pinned external corpora + Loomweave itself get analyzed under `/usr/bin/time -v` with stats harvested from `runs.stats`, the findings table, and the SEI tracing line; verdicts (guard FP rate, collision count, churn, deadline/RSS headroom) land in a committed memo. Code changes are confined to the error-path fix, the hermetic-install env plumbing, and a qualname-collision harness — all TDD.

**Tech Stack:** bash measurement scripts (kept in `tests/qa/`), sqlite3, /usr/bin/time -v, maturin + uv venv, existing fixture-plugin env-flag pattern.

**Tickets:** parent `clarion-a2db86704e`; subtasks `clarion-959cb50f63` (dogfood), `clarion-f635d97620` (scale), `clarion-bee09830cf` (error UX); absorbed: `clarion-371efa3e07` (spurious OOM, fixing), `clarion-c5e3cc2818` (hermetic install).

---

## Design decisions (settled 2026-06-10, evidence in fact-finding)

| # | Decision | Call |
|---|----------|------|
| D1 | Corpora | tokio `2e7930fe58cdf29671ea71a066ebc80b5006b97d`, serde `5f0f18b9211732f2d82f73b5a43e4f5ff3701251`, rust-analyzer `587ce15e272b94d4f0a69d695e3718a02c24667e`, ripgrep `82313cf95849bfe425109ad9506a52154879b1b1` (already cloned shallow at `~/corpora/`, read-only) + Loomweave itself (dogfood). Pass per corpus: run terminal `completed`; zero qualname collisions; guard trips individually classified (real-pathology vs FP); no lifecycle-deadline findings; no OOM kill. |
| D2 | Measurement methodology | Wall + peak RSS via `/usr/bin/time -v` (its MaxRSS covers reaped children via wait4 rusage — the plugin child IS the heavy process; note as upper-bound methodology). Per-file p95 from `runs.stats` (`extractor_parse_latency_p95_ms`). Handshake duration via RUST_LOG timestamps (spawn→handshake-complete). Counts/findings via sqlite3 on each corpus store. Unresolved-call rate = unresolved_sites_total / (unresolved + resolved calls edges). SEI churn from the `SEI mint pass complete` tracing line: unchanged re-analyze MUST show minted=0, orphaned=0, carried=total; benign one-fn-body edit MUST stay confined (locator unchanged ⇒ carried). Corpus runs SEQUENTIAL (no CPU contention skewing walls). |
| D3 | Installed binary | Fresh QA venv built exactly like `tests/e2e/rust_plugin_wheel_smoke.sh` (maturin release wheels for cli + rust-plugin-dist from the WORKTREE, so Sprint-2 edges + any Sprint-3 fixes are in). User's uv-tool venv refreshed at sprint END via atomic temp+mv (loomweave-binary-rebuild memory) — it currently carries ontology 0.4.0. |
| D4 | Handshake wall | Measure rust-analyzer init-parse against the 300 s handshake deadline + 128 MiB effective RLIMIT_AS (plugin.toml `expected_max_rss_mb = 128`). Tune ONLY what numbers indict; any semantics change = ADR-050 amendment with the empirical row. |
| D5 | Spurious OOM fix (clarion-371efa3e07) | In analyze.rs handshake-failure branch: `try_wait()` BEFORE `kill()` — child already exited ⇒ classify honestly (real OOM during init stays visible); child alive and WE kill it ⇒ suppress kill-classification (the handshake-failure finding tells the story). New fixture mode `LOOMWEAVE_FIXTURE_REFUSE_HANDSHAKE` (alive process, protocol-refusal response) + TDD test asserting zero `LMWV-INFRA-PLUGIN-OOM-KILLED`. |
| D6 | Hermetic install (clarion-c5e3cc2818) | Every e2e script + QA harness exports `LOOMWEAVE_CODEX_CONFIG=$TMP/codex-config.toml` (the env override already exists in install.rs — plumbing only, no product code change). Audit all `tests/e2e/*.sh`. |
| D7 | Qualname-collision harness | New example binary `crates/loomweave-plugin-rust/examples/qualname_check.rs`: walks a path via the existing public `build_symbol_table`, prints `duplicate_ids()`, exit 1 if non-empty. No product code touched; reusable for future corpora. |
| D8 | MCP checklist depth | Committed: new `tests/e2e/dogfood_mcp_rust.sh` asserting Rust-entity MCP answers against a SELF-BUILT small fixture index (stable). Manual-with-evidence in the memo: the same checklist against the real Loomweave dogfood index (entity_find, resolve by ADR-049 qualname, callers_of, neighborhood, entity_at on a .rs file:line, dead-code w/ unresolved suppression count, subsystem membership). |
| D9 | Report home | `docs/implementation/qa/2026-06-10-rust-plugin-scale-qa.md` (non-normative archive). Operator-docs edits deferred to Sprint 4 except factual one-liners. |
| D10 | Frozen contracts | No entity_id.rs / storage/sei.rs / qualname changes; no version bumps; corpora never committed; temp venvs + corpora cleaned at exit. |

## Execution notes
- Worktree: `/home/john/loomweave/.claude/worktrees/rust-plugin-scale-qa` (rc4 @ 48bdafd, Sprint 2 edges IN — record in report).
- `cargo build --workspace --bins` before any nextest (stale-binary hazard).
- Main-checkout files `.mcp.json`, `loomweave.yaml` are dirty from a concurrent agent — never commit them.
- Corpora are READ-ONLY except the benign-edit churn probe, which edits ONE function body and `git checkout -- .` restores it after.

### Task 1: Spurious-OOM fix + refuse-handshake fixture (TDD)
**Files:** `crates/loomweave-plugin-fixture/src/main.rs` (new env flag), `crates/loomweave-cli/src/analyze.rs` (handshake-failure branch try_wait-before-kill), `crates/loomweave-cli/tests/analyze_hardening.rs` (new test)
- [ ] Red: fixture gains `LOOMWEAVE_FIXTURE_REFUSE_HANDSHAKE` (reply to initialize with a JSON-RPC error, then keep running/reading); test `handshake_refusal_no_spurious_oom_finding` asserts run terminal, child reaped, `finding_count("LMWV-INFRA-PLUGIN-OOM-KILLED") == 0`, and a handshake-failure surface present (whatever the branch emits — assert the actual finding/failure_reason). Run → expect OOM-finding assertion FAIL.
- [ ] Green: in the handshake-failure branch, capture `child.try_wait()` before `kill()`; suppress kill-classification iff the child was still alive when we killed it OR the watchdog fired (existing flag). Comment cites clarion-371efa3e07.
- [ ] Full `cargo nextest run -p loomweave-cli` + clippy + fmt. Commit `fix(analyze): handshake refusal no longer mislabeled as OOM kill (clarion-371efa3e07)`.

### Task 2: Hermetic e2e install (audit + plumb)
**Files:** every `tests/e2e/*.sh` that runs `loomweave install`
- [ ] Grep all e2e scripts for `install`; for each, export `LOOMWEAVE_CODEX_CONFIG` to a script-local temp path if not already; verify each script still passes. Commit `fix(e2e): hermetic install — no script mutates ~/.codex/config.toml (clarion-c5e3cc2818)`.

### Task 3: Qualname-collision harness
**Files:** `crates/loomweave-plugin-rust/examples/qualname_check.rs`
- [ ] Example binary: arg = path; `build_symbol_table(path)`; print count + each duplicate id; exit non-zero on collisions. Smoke it on the worktree itself (expect zero). Commit `test(plugin-rust): qualname_check example for external-corpus collision sweeps`.

### Task 4: QA venv + measurement harness
**Files:** `tests/qa/run_corpus_qa.sh` (new, committed), QA venv under `/tmp/lw-qa-venv` (NOT committed)
- [ ] Build release wheels (maturin) for cli + rust-plugin-dist from the worktree; `uv venv /tmp/lw-qa-venv`; install both wheels; verify `loomweave-plugin-rust` + manifest land per discovery chain.
- [ ] `tests/qa/run_corpus_qa.sh <corpus-path> <out-dir>`: hermetic env (LOOMWEAVE_CODEX_CONFIG temp), `install --path`, `/usr/bin/time -v analyze` with RUST_LOG=info captured, sqlite3 harvest (entities/edges per kind, findings by rule_id+subcode, runs.stats json), SEI line capture, unchanged re-analyze (churn), benign-edit probe + restore, qualname_check run. Emits one JSON/markdown block per corpus.
- [ ] Commit the script only.

### Task 5: Corpus runs (SEQUENTIAL) + dogfood index
- [ ] Run harness over: ripgrep, serde, tokio, rust-analyzer (ascending size; watch the rust-analyzer handshake wall + RSS), each into `/tmp/lw-qa-results/<name>/`.
- [ ] Dogfood: run QA-venv `loomweave install --path /home/john/loomweave && analyze` (real store, advisory-lock-safe), then the MCP checklist with evidence (D8 manual half) + new committed `tests/e2e/dogfood_mcp_rust.sh` (D8 committed half, fixture-index based — model on sprint_2_mcp_surface.sh).
- [ ] Guard-trip classification: for every LMWV-RUST-DEPTH-LIMIT / FILE-TOO-LARGE / SYNTAX-ERROR finding across corpora, inspect the file and classify real-pathology vs false-positive (parallel agents OK — read-only judgment work).

### Task 6: Verdicts → fixes/tuning → report
- [ ] If guard FP rate non-zero: tune cap (ADR-050 amendment with the empirical row) or accept-with-rationale; TDD any cap change in parse_guard.rs.
- [ ] If deadline/RSS indicted: ADR-050 amendment + plugin.toml/limits change as evidence dictates.
- [ ] Write `docs/implementation/qa/2026-06-10-rust-plugin-scale-qa.md`: methodology, per-corpus table, SEI churn, MCP checklist evidence, error-path UX before/after, decisions. Commit.

### Task 7: Gates + merge + closeout
- [ ] Full CLAUDE.md floor (bins-before-nextest), all check-*.py, all e2e smokes incl. hostile_corpus_rust.sh + rust_plugin_wheel_smoke.sh + new dogfood_mcp_rust.sh.
- [ ] Merge `--no-ff` → rc4 (re-merge rc4 first if moved), push, post-merge verify, close tickets with `--reason`, refresh user's uv-tool venv (atomic mv), remove worktree, clean `/tmp/lw-qa-*` + decide ~/corpora retention (cheap, keep with a note? delete — prompt says clean up).
