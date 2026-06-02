# Clarion v1.0 — Requirements vs Implementation Gap Analysis

**Date:** 2026-05-24 (live north-star — amendments applied as we work)
**Scope:** 125 requirement IDs from `docs/clarion/1.0/requirements.md` audited against shipped code in `crates/`, `plugins/python/`, `tests/`, and `.github/workflows/`.
**Inventory:** 51 REQ-* · 28 NFR-* · 8 CON-* · 25 NG-* · 13 cross-cutting (REQ-INTEG-*).
**Status:** Supporting context only — not normative. See [`docs/clarion/1.0/README.md`](../README.md) for canonical-truth precedence.

This document is the working north-star for v1.0 publish-ready cleanup. It evolves as amendments land. It tells you, today, where shipped code agrees with the spec, where it diverges by intentional deferral, where the spec drifted away from shipped behaviour, and where there is genuine uncovered scope to backfill before publish.

## 0. Amendments applied this session

### Session 1 — MCP/HTTP surface narrowing (2026-05-24 morning)

| # | Action | Effect |
|---|--------|--------|
| 1 | Added "v1.0 ships 8-tool MVP subset" blockquote to `REQ-MCP-02`; full catalogue documented as v0.2 target. | `REQ-MCP-02` **P0 → Deferred**. |
| 2 | Added "Deferred to v0.2" blockquote to `REQ-MCP-03` citing amendment §3+§4 + ADR-030. | `REQ-MCP-03` **P0 → Deferred**. |
| 3 | Added "v1.0 ships ADR-014 file-registry subset" blockquote to `REQ-HTTP-01`. | `REQ-HTTP-01` **P0 → Deferred**. |
| 4 | Added "Deferred to v0.2" blockquote to `REQ-HTTP-02`. | `REQ-HTTP-02` **P0 → Deferred**. |
| 5 | "v1.0 ships subset" notes at `system-design.md` §8 Tool catalogue / §8 Exploration-elimination / §9 HTTP Endpoints. | Closes design-doc half of surface drift. |

### Session 2 — broader amendment bundle (2026-05-24 afternoon)

| # | Action | Effect |
|---|--------|--------|
| 6 | "Deferred to v0.2" blockquote on `REQ-MCP-01` (cursor session model). | **P1 Missing → Deferred** |
| 7 | "v1.0 ships intrinsic bounds" blockquote on `REQ-MCP-04` (per-tool token budgets are v0.2). | **P2 Partial → Satisfied** |
| 8 | "Deferred to v0.2" blockquote on `REQ-MCP-05` (write-effect tool consent gates). | **P1 Missing → Deferred** |
| 9 | "Deferred to v0.2" blockquote on `REQ-MCP-06` (session persistence). | **P1 Missing → Deferred** |
| 10 | "Deferred to v0.2" blockquote on `REQ-ARTEFACT-01` (B.4 removed). | already Deferred; now blockquoted |
| 11 | "Deferred to v0.2" blockquote on `REQ-ARTEFACT-02` (B.5 removed). | already Deferred; now blockquoted |
| 12 | "Deferred to v0.2" blockquote on `REQ-CONFIG-02/03/04` (WP6 narrowed). | already Deferred; now blockquoted |
| 13 | Section-level "whole subsystem deferred" note on `REQ-GUIDANCE-*` (WP7 deferred). | -01 **Partial → Deferred**; -02..-06 **Missing → Deferred** |
| 14 | Per-row "Deferred to v0.2" blockquotes on `REQ-BRIEFING-01/02/04/05/06` (WP6 narrowed per ADR-030). | -01 **Partial → Deferred**; -02/04/05/06 **Missing → Deferred** |
| 15 | "Deferred to v0.2" blockquote on `NFR-PERF-01` (60-min target needs Phases 4–6). | **P1 Missing → Deferred** |
| 16 | "Deferred to v0.2" blockquote on `NFR-COST-01` ($15 elspeth needs Phases 4–6). | **P1 Missing → Deferred** |
| 17 | "Deferred to v0.2" blockquote on `NFR-COST-03` (preflight needs Phase 0). | **P1 Missing → Deferred** |
| 18 | `system-design.md` UX-modes table: catalog-artefacts row flipped from "Shipped" to "v0.2 (deferred)". | Closes the stale `system-design.md:103` claim. |

### Session 3 — checklist phase 1 (2026-05-24 evening)

| # | Action | Effect |
|---|--------|--------|
| 19 | "Deferred to v0.2" blockquote on `REQ-INTEG-WARDLINE-02` (WP9-B manifest + overlay ingest). | **Missing → Deferred** |
| 20 | "Deferred to v0.2" blockquote on `REQ-INTEG-WARDLINE-03` (WP9-B fingerprint ingest). | **Missing → Deferred** |
| 21 | "Deferred to v0.2" blockquote on `REQ-INTEG-WARDLINE-04` (WP9-B exceptions ingest). | **Missing → Deferred** |
| 22 | "Deferred to v0.2" blockquote on `REQ-INTEG-WARDLINE-05` (WP10 SARIF baseline ingest for translator). | **Missing → Deferred** |
| 23 | "Deferred to v0.2" blockquote on `REQ-INTEG-WARDLINE-06` (three-scheme identity oracle, depends on -03/-04). | **Missing → Deferred** |
| 24 | `CHANGELOG.md` known-limitations: appended Wardline state-file ingest line after the REGISTRY-import asterisk; enumerates -02..-06 and names WP9-B + WP10 as v0.2 landing surfaces. | Closes the release-notes drift surface. |
| 25 | `NFR-SEC-03` verification line: replaced stale `serve.rs` line numbers (1457/1495/1547/1579/1614) with eight test-name citations covering opt-in refusal, no-auth refusal, env-missing refusal, HMAC + wrong-secret, bearer + wrong-token + batch variant, and `_capabilities` carve-out. | Verification citations are now name-stable, not line-fragile. |

**Net effect after session 2 + checklist phase 1:** REQ Missing list now 4 (REQ-ANALYZE-03, REQ-CATALOG-07, REQ-PLUGIN-05, REQ-PLUGIN-06); the 5 REQ-INTEG-WARDLINE rows flipped Missing → Deferred when checklist 1.1–1.5 added the row-level blockquotes (2026-05-24 sweep). NFR Missing stays at 6 (NFR-OBSERV-01..04, NFR-COMPAT-01, NFR-COMPAT-02; plus NFR-OPS gaps). The list of genuine v1.0-publish-blocking code work is now small and actionable — §5 below.

---

## 1. Executive summary

Clarion v1.0 is a credible release for its **stated minimum viable surface** — entity ingestion, plugin host, secret scanning, Phase-3 clustering, persistent storage, HMAC-authenticated HTTP read API for Filigree federation, and a small MCP tool set. The initial audit read it as materially narrower than `requirements.md` because the requirement doc had not been updated to reflect the 2026-05-16 Sprint-2 scope amendment. As of §0 amendments 1–4, the MCP and HTTP requirement rows now correctly document the v1.0 subset.

After session 1's amendments, three patterns describe the remaining work:

1. **Doc-drift cluster (text-only fixes, not code).** The Sprint-2 amendment deferred WP6 (LLM pipeline), WP7 (guidance authoring), WP9-B (Filigree finding emission), WP10 (SARIF translator), narrowed WP8 (MCP) to 8 tools, and removed the catalog-artefact boxes B.4/B.5. `REQ-FINDING-03..06` and `REQ-INTEG-FILIGREE-01..05` carry the "Deferred to v0.2" blockquote correctly; `REQ-MCP-02/03` and `REQ-HTTP-01/02` now carry it. **Still missing the blockquote:** `REQ-ARTEFACT-01/02`, `REQ-CONFIG-02/03/04`, `REQ-GUIDANCE-01..06`, `REQ-BRIEFING-01/02/04/05/06`, `REQ-MCP-04/05/06`. These all describe surface that the amendment deferred but whose requirement rows still read as v1.0 contract — the obvious next bundle (see §5 action #1b).
2. **Whole pipeline phases absent (architectural deferral).** `clarion analyze` ends at Phase 3. Phase 0 (dry-run) and Phases 4–6 (LLM summarisation) drive `NFR-PERF-01`, `NFR-COST-01/03`, `REQ-BRIEFING-04/05`, the budget gate, and the preflight estimator. Summarisation happens lazily through MCP `summary` only per ADR-030. Once #1b lands, all of these reclassify Deferred — the work itself is genuinely v0.2.
3. **Targeted code gaps outside any deferral.** A small set of v1.0-contract items that nobody amended out: `REQ-ANALYZE-03` (`--resume`), `REQ-CATALOG-04/07` (file git metadata, HEAD-SHA capture), `REQ-PLUGIN-05/06` (Python plugin import policy + decorator edges), `NFR-COMPAT-01` (Filigree schema-pin CI), `NFR-COMPAT-02` (Wardline probe pins wrong symbol), `NFR-OBSERV-01..04` (JSON logs / stats.json file / Prometheus / compat-report finding). These are the *actual* code work to publish v1.0 — once the amendments collapse the doc drift, this list is the punch list.

**Disciplined areas (genuinely strong, no action):** plugin host (manifest enforcement, framing, jail, crash-loop breaker, structured host findings), secret scanner, Phase-3 Leiden clustering with seeded determinism, summary-cache 5-tuple key, HMAC + ADR-034 hardening surface, all 8 CON-* honoured (one with the minor `CodexCli` provider drift noted in §5), all 25 NG-* honoured (no scope creep into linter / file-watcher / multi-branch / SARIF / wiki territory).

**Disciplined areas (genuinely strong):** plugin host (manifest enforcement, framing, jail, crash-loop breaker, structured host findings), secret scanner, Phase-3 Leiden clustering with seeded determinism, summary-cache 5-tuple key, HMAC + ADR-034 hardening surface, all 8 CON-* honoured (one with the minor `CodexCli` provider drift noted in §5), all 25 NG-* honoured (no scope creep into linter / file-watcher / multi-branch / SARIF / wiki territory).

**Verdict tally** (after session 2 amendments):

| Category | Total | Satisfied | Partial | Missing | Deferred | Δ from initial audit |
|----------|------:|----------:|--------:|--------:|---------:|---------------------|
| REQ-* (functional, non-INTEG) | 40 | 13 | 9 | 4 | 14 | Sat +1, Partial −4, Missing −4, Deferred +7 |
| REQ-INTEG-* (cross-product) | 11 | 1 | 0 | 0 | 10 | Missing −5, Deferred +5 (checklist 1.1–1.5 sweep) |
| NFR-* | 28 | 5 | 12 | 6 | 5 | Missing −3, Deferred +3 |
| CON-* | 8 | 6 | 1 | 0 | 1 | unchanged |
| NG-* (inverted: scope creep) | 25 | 25 honoured | 0 | 0 drift | — | unchanged |
| **TOTAL** | **112** | **50** | **22** | **10** | **30** | |

(Counts in §6 RTM rows are authoritative; the headline table sums them.)

**Working plan:** §5 action #1c is done (checklist phase 1, 2026-05-24). The doc-drift workstream is complete; what's left is genuine code — §5 actions 2–7, tracked in [`v1.0-publish-checklist-2026-05-24.md`](./v1.0-publish-checklist-2026-05-24.md) phases 2–6. Once those empty out, the docs and the binary tell the same story and v1.0 is publish-ready.

---

## 2. Methodology

Each requirement ID was traced via three sources, in this order:

1. **Requirement text** — `docs/clarion/1.0/requirements.md`, including the per-requirement `Verification:` line.
2. **Design trace bridge** — `**Addresses**:` headers in `docs/clarion/1.0/system-design.md` (twelve sections covering §2–§11; bridge density is good — every functional requirement in this audit had a navigable target).
3. **Implementation** — corresponding code under `crates/clarion-core/`, `crates/clarion-storage/`, `crates/clarion-cli/`, `crates/clarion-scanner/`, `crates/clarion-mcp/`, `plugins/python/`, plus `.github/workflows/` and `tests/`.

Each verdict was cross-checked against in-flight tracked work in Filigree (`get-ready` + `list-issues --status in_progress`, snapshot at `/tmp/clarion_gap_filigree_ready.json`) to avoid re-reporting known follow-ups as discoveries. Verdicts cite specific `file:line` evidence. Non-goals (NG-*) were checked **inverted** — confirming the codebase does *not* implement them — to detect scope creep.

Verdict vocabulary:

- **Satisfied** — code implements the requirement; verification test or behavioural artefact exists.
- **Partial** — mechanism exists but is undermeasured, contract-divergent, or covers only part of the named surface.
- **Missing** — no implementation; not deferred by any written carve-out.
- **Deferred** — scope explicitly carved out of v1.0 by the Sprint-2 amendment, a Backlog ADR, or a `> **Deferred to v0.2**` blockquote in `requirements.md`.

`Tracked` column values: `new` (file an issue), `tracked:<keyword>` (existing Filigree row covers this; merge into that work), `deferred:scope-amendment` (don't file).

Audit was performed by six concurrent subagents (REQ slices A/B/C; NFR slices D/E/F) plus a seventh CON+NG audit (G), capped at three in flight, synthesised here.

---

## 3. Headline findings, by priority

### 3.1 P0 — Resolved by session 1 amendments

The initial audit flagged four P0 surface-mismatch findings: `REQ-MCP-02`, `REQ-MCP-03`, `REQ-HTTP-01`, `REQ-HTTP-02`. Investigation of the [Sprint-2 scope amendment](../../implementation/sprint-2/scope-amendment-2026-05.md) showed that all four were explicitly narrowed by the 2026-05-16 amendment (Box B.6 named the 8-tool MVP MCP surface; [ADR-014](../adr/ADR-014-filigree-registry-backend.md) defined the file-registry HTTP subset) but the requirement rows had never received the "Deferred to v0.2" blockquote that `REQ-FINDING-03..06` carries. **Session 1 added those blockquotes (see §0).** No code changes required for these four rows. The P0 list is now empty.

The next-bundle of probably-same-shape rows is documented as §5 action #1b — they should be inspected against the amendment and either amended or escalated to genuine work, individually.

### 3.2 P1 — Genuine v1.0 code gaps (post amendment sweep)

After session 2, the P1 list is much shorter and entirely "real code work" — no remaining doc-drift hidden as severity.

**Observability foundations (NFR-OBSERV-01..04).** Not deferred by any envelope. `tracing` initialised in plain text at `crates/clarion-cli/src/main.rs:69-76` with no `.json()`, no file sink, no rotation. The install template's `.gitignore` excludes `runs/*/log.jsonl` — a path nothing writes to. Stats live only in the SQLite `runs.stats` column; no `runs/<run_id>/stats.json` file lands. No Prometheus `/api/v1/metrics` endpoint *(this one will reclassify Deferred once REQ-HTTP-01 is propagated through NFR-OBSERV-03; it's part of the broader HTTP surface)*. No `CLA-INFRA-SUITE-COMPAT-REPORT` emitter. Three of four are direct v1.0 code work; one (`-03`) probably joins HTTP-01's deferral.

**Schema/version compat (NFR-COMPAT-01/02).** Genuinely outside any deferral. NFR-COMPAT-01 = Filigree schema-pin CI test missing (one job). NFR-COMPAT-02 = Wardline probe pins `wardline.__version__` instead of `REGISTRY_VERSION`; named graceful-degradation findings have no emitter.

**Python plugin gaps (REQ-PLUGIN-05/06).** TYPE_CHECKING block exclusion / `src.`-prefix canonicalisation / `python:unresolved:*` placeholders / `alias_of` edges / `decorated_by` / `inherits_from` / `uses_type` edges all absent. Not in any defer list — these are the v1.0 contract for the Python plugin's ontology coverage.

**Three targeted REQ-* rows outside any envelope.** `REQ-ANALYZE-03` (`--resume` CLI flag + checkpoint reader), `REQ-CATALOG-07` (HEAD-SHA capture into `first_seen_commit`/`last_seen_commit` — columns exist, populated as `None`), `REQ-CATALOG-04` (file-entity git metadata).

**Ops gaps (NFR-OPS-03/04).** `clarion db export --textual` + `db merge-helper` don't exist (NFR-OPS-03 — either build them or narrow the requirement); PyPI publish + SLSA-over-sdist outstanding (NFR-OPS-04, tracked under `clarion-f530101222`).

### 3.3 P1 — Targeted gaps outside any deferral envelope

- **REQ-ANALYZE-03 — `--resume`** has no CLI flag (`cli.rs:30`), no checkpoint reader, no `checkpoints.jsonl` consumer. Crash mid-run leaves `runs.status='running'` rows; rerun starts from scratch.
- **REQ-CATALOG-07 — `first_seen_commit` / `last_seen_commit`** columns exist in `commands.rs:66` but every call-site in `analyze.rs` writes `None` (lines 813, 1619, 1692, 2156, 2348, 2417). No HEAD-SHA capture. Point-in-time queries return NULL.
- **REQ-CATALOG-04 — file-entity git metadata** writes only `language` + `briefing_blocked`. `git_churn_count`, `git_last_modified`, `git_authors`, `size_bytes`, `line_count`, `mime_type` never populated (`analyze.rs:1544`).

### 3.4 P2 — Implemented-but-unmeasured (no benchmarks, no recorded runs)

`find tests/ -name 'bench*'` returns nothing; no `criterion` dependency anywhere. The following all have correct-shape mechanisms in code and lack only a measurement artefact:

- NFR-PERF-02 (≤100 ms initialize, ≤50 ms p95 hot-cache summary): mechanisms correct (`lib.rs:176,256,788,1339-1367`), no harness.
- NFR-SCALE-01 (elspeth ±20% entity count, no OOM): `EntityCountCap=500k` wired (`limits.rs:115`), no recorded elspeth run.
- NFR-SCALE-02 (`.clarion/clarion.db` ≤2 GB): no size reporter, no measured run.
- NFR-SCALE-03 (16-reader pool, no exhaustion): wired (`serve.rs:46`), saturation tested for max_size=1/2 only; no combined-load test.
- NFR-COST-02 (≥95% summary-cache hit rate after 3 runs): mechanism correct (`cache.rs:48-110`), no `cache_hit_rate` aggregation in `stats.json`.

Each could flip to Satisfied with a measurement task that does not require new product code.

### 3.5 Documentation drift (text-only fixes)

Status legend: ✅ done this session · ⏳ still open.

- ✅ `system-design.md` §8 Tool catalogue + §8 Exploration-elimination + §9 HTTP endpoints — each now carries a "v1.0 ships subset" note.
- ✅ `REQ-MCP-02 / REQ-MCP-03 / REQ-HTTP-01 / REQ-HTTP-02` — deferral blockquotes added.
- ⏳ `requirements.md` still lacks "Deferred to v0.2" blockquotes on `REQ-ARTEFACT-01/02`, `REQ-CONFIG-02/03/04`, `REQ-GUIDANCE-01..06`, and probably `REQ-MCP-04/05/06` + `REQ-BRIEFING-04/05` — see §5 action #1b for the bundle.
- ⏳ `system-design.md:103` still calls catalog artefacts "Shipped" though `grep -r catalog.json crates/` is empty; pair this fix with `REQ-ARTEFACT-*` amendments.
- ⏳ `NFR-SEC-03` verification cites stale `serve.rs:798-803`; actual locations are `tests/serve.rs:1160,1194,1251,1285,1317`. Tracked under the ADR-034 refresh chain (clarion-7913f950d7, clarion-272b5bc1ec). Recommend replacing line citations with test-name citations to survive future refactors.
- ⏳ `CHANGELOG.md` known-limitations enumerates the Wardline REGISTRY import asterisk but not Wardline state-file ingest (manifest/overlay/fingerprint/exceptions/SARIF baseline). Add to release notes.

### 3.6 Quick-close wins (issues already done in HEAD)

Per the standing "close already-done tickets" authorisation in user memory:

- **`clarion-a4fb59a96a` — rollback runbook** — `docs/operator/v1.0-release-rollback.md` exists (141 lines).
- **`clarion-42f4fee904` — loopback trust banner** — emitted at `crates/clarion-cli/src/http_read.rs:244-248`; documented at `docs/operator/clarion-http-read-api.md:58-73`.

Both can transition through the full workflow to `closed`.

### 3.7 One CON drift (governance, not severity)

`CON-ANTHROPIC-01` says "Anthropic-only LLM provider in v0.1." `crates/clarion-mcp/src/config.rs:95-99` enumerates `OpenRouter`, `CodexCli`, `ClaudeCli`. `CodexCli` (`crates/clarion-cli/src/serve.rs:11`) shells out to OpenAI's Codex CLI — a non-Anthropic vendor surface. The constraint's prompt-caching rationale arguably doesn't apply to CLI shell-outs, which is probably why the drift went unflagged. Recommend either (a) amend the constraint to recognise local-CLI providers as a separate category, or (b) gate `CodexCli` behind a default-off cargo feature.

---

## 4. Strongest areas (no action required)

- **Plugin host (REQ-PLUGIN-01..04, REQ-FINDING-01/02, NFR-SEC-01/05).** Content-Length framing (`transport.rs:113,287`), manifest enforcement (`host.rs:897,1043`), undeclared-kind drop tests (`host.rs:1586,2679`), crash-loop breaker (`breaker.rs:16`), entity cap and OOM kill (`limits.rs:115`), path-escape jail. Findings vocabulary (`Defect | Fact | Classification | Metric | Suggestion`) round-trips through a CHECK-constrained `findings` table; per-plugin `rule_id_prefix` works as designed. `.clarion/.gitignore` shipped by `install.rs:97`.
- **Phase-3 Leiden clustering (REQ-CATALOG-05).** Seeded determinism asserted by `tests/analyze.rs:730 analyze_phase3_is_deterministic_across_two_runs`. Subsystem entities and `in_subsystem` edges flow through the writer-actor; e2e at `tests/e2e/phase3_subsystems.sh`.
- **Summary-cache 5-tuple key (REQ-BRIEFING-03).** PK = `entity_id + content_hash + prompt_template_id + model_tier + guidance_fingerprint` exactly per ADR-007 (`cache.rs:9-72`; `migrations/0001_initial_schema.sql:151-164`). Round-trip tested.
- **HMAC + ADR-034 federation surface (REQ-HTTP-03, NFR-SEC-03).** Six tests at `tests/serve.rs:1160..1342` cover loopback default, non-loopback-without-auth refusal, HMAC-required path, legacy bearer path, identity-env missing refusal. Code in `http_read.rs:179-251,389-498`.
- **Secret scanner (NFR-SEC-01).** `crates/clarion-scanner/` + `crates/clarion-cli/src/secret_scan.rs` (574 LOC) with baseline justification + dedicated e2e at `tests/e2e/wp5_secret_scan.sh`.
- **All 25 NG-* honoured.** No rule engine, no taint, no SARIF, no file watchers, no multi-branch, no wiki UI, no rename detection, no coverage ingestion, no second plugin, no plugin hash-pinning. The codebase shows real scope discipline.
- **7 of 8 CON-* satisfied** (`CON-ANTHROPIC-01` partial drift noted in §3.7; CON-FILIGREE-01 properly Deferred).

---

## 5. Recommended next actions

Sorted by leverage. Status legend: ✅ done · ⏳ pending. Each item names a target and a "why."

| # | Status | Action | Target | Why |
|---|--------|--------|--------|-----|
| 1a | ✅ | Amendment pass on REQ-MCP-02/03 + REQ-HTTP-01/02 + system-design §8/§9 notes. | done 2026-05-24 | Closed surface-vs-doc P0 gap. |
| 1b | ✅ | Amendment bundle on `REQ-MCP-01/04/05/06`, `REQ-ARTEFACT-01/02`, `REQ-CONFIG-02/03/04`, `REQ-GUIDANCE-01..06` (section header), `REQ-BRIEFING-01/02/04/05/06`, `NFR-PERF-01`, `NFR-COST-01/03` + retract `system-design.md:103` "Shipped" claim. | done 2026-05-24 | 17 rows; Missing list dropped from 19 → 4. |
| 1c | ✅ | **Final amendment sweep.** `REQ-INTEG-WARDLINE-02..06` row-level blockquotes added; `CHANGELOG.md` known-limitations now enumerates Wardline state-file ingest alongside the REGISTRY-import asterisk; `NFR-SEC-03` verification swapped from stale line numbers to test-name citations. Done 2026-05-24 (checklist phase 1). | done | Closed the last amendment-deferred ambiguity; doc-drift workstream complete. |
| 2 | ⏳ | Backfill the three uncovered REQ rows: `REQ-ANALYZE-03` (`--resume`), `REQ-CATALOG-07` (HEAD-SHA capture into `first_seen_commit` / `last_seen_commit`), `REQ-CATALOG-04` (git metadata on file entities). | v1.0 publish-ready | None deferred by any carve-out; storage substrate already shaped. |
| 3 | ⏳ | Python plugin: implement `REQ-PLUGIN-05` (TYPE_CHECKING exclusion, src-prefix canonicalisation, `python:unresolved:*` placeholders, `alias_of` edges) + `REQ-PLUGIN-06` (`decorated_by`/`inherits_from`/`uses_type` edges). | v1.0 publish-ready | Not in any defer list; core ontology surface advertised at v1.0. |
| 4 | ⏳ | Wardline probe — pin `REGISTRY_VERSION` (not `__version__`), emit `CLA-INFRA-WARDLINE-REGISTRY-ADDITIVE-SKEW` and `-MIRRORED` findings on version drift. | v1.0 publish-ready | Cheap fix, named in detailed-design.md:1169-1170, currently invisible. |
| 5 | ⏳ | Add Filigree schema-pin CI job (`NFR-COMPAT-01`). | v1.0 publish-ready | One job; closes a P1 with low effort. |
| 6 | ⏳ | Observability foundations: JSON-formatted tracing layer with file sink + rotation; `runs/<run_id>/stats.json` file emission alongside the SQLite column; `CLA-INFRA-SUITE-COMPAT-REPORT` emitter. `NFR-OBSERV-03` (Prometheus `/api/v1/metrics`) probably becomes Deferred — depends on broader HTTP surface already deferred via REQ-HTTP-01. | v1.0 publish-ready | Three NFR-OBSERV-* rows; one cohesive workstream. |
| 7 | ⏳ | Add `clarion db export --textual` + `clarion db merge-helper` subcommands, or narrow `NFR-OPS-03` to reflect the commit-by-default-only scope. | v1.0 publish-ready | Currently the text overpromises. |
| 8 | ⏳ | Publish `clarion-plugin-python` to PyPI and extend SLSA provenance to the sdist (existing ticket `clarion-f530101222`). | v1.0 publish-ready | Closes `pipx install clarion-plugin-python` per `NFR-OPS-04`. |
| 9 | ⏳ | Decide on `CON-ANTHROPIC-01` × `CodexCliProvider`: either amend constraint or gate behind feature flag. | ADR or constraint amendment | Governance, not severity, but currently invisible drift. |
| 10 | ⏳ | Close already-done Filigree issues per standing authorisation: `clarion-a4fb59a96a` (rollback runbook), `clarion-42f4fee904` (loopback banner). | Immediate | Both ship in HEAD; tickets are stale. |
| 11 | ⏳ | Backlog: elspeth-scale measurement task — cargo bench harness for MCP latency, single recorded elspeth run capturing entity count + DB size + cache hit rate + stats.json. | post-publish, before 1.1 | Flips five P2 "implemented but unmeasured" verdicts to Satisfied without product code. |

---

## 6. Full RTM

### 6.1 REQ-* (functional)

| ID | Verdict | Evidence | Gap | Sev | Tracked |
|----|---------|----------|-----|-----|---------|
| REQ-ANALYZE-01 | Partial | `analyze.rs:54,485` | Phases 4–8 absent; no phase log | P2 | deferred:scope-amendment |
| REQ-ANALYZE-02 | Partial | `analyze.rs:308`; writer serial | No LLM parallelism | — | deferred:scope-amendment |
| REQ-ANALYZE-03 | **Missing** | `cli.rs:30` no flag | No `--resume`; no checkpoint reader | P1 | new |
| REQ-ANALYZE-04 | Built (v1.1) | deletion findings in the SEI mint pass (`analyze.rs` `emit_deletion_findings`); `--no-sei` disables | — (was: Phase-7 entity-set diff) | — | built:v1.1 |
| REQ-ANALYZE-05 | Built (v1.1) | `analyze.rs` `emit_tier_subsystem_findings` (tier × subsystem, function→subsystem resolution); conditional on Wardline ingest | — (was: Phase-7 `CLA-*` rules) | — | built:v1.1 |
| REQ-ANALYZE-06 | Partial | `breaker.rs:16`, `host_findings.rs`, `limits.rs` | Named rules `CLA-PY-PARSE-ERROR`/`-TIMEOUT`/`CLA-INFRA-LLM-ERROR`/`-BUDGET-WARNING` absent | P2 | new |
| REQ-ANALYZE-07 | Partial | `tests/analyze.rs:730` | No `clarion db export --textual`; whole-catalog byte-id not verified | P3 | new |
| REQ-ARTEFACT-01 | Deferred | no `catalog.json` emit | Doc drift: requirement lacks blockquote | — | deferred:scope-amendment |
| REQ-ARTEFACT-02 | Deferred | no per-subsystem markdown | Doc drift: requirement lacks blockquote | — | deferred:scope-amendment |
| REQ-BRIEFING-01 | Deferred (✅ amended §0 #14) | `llm_provider.rs:762-789` ships 4-field on-demand summary | rich 9-field `EntityBriefing` v0.2 per ADR-030 | — | deferred:scope-amendment |
| REQ-BRIEFING-02 | Deferred (✅ amended §0 #14) | no impl | controlled vocab v0.2 with briefing pipeline | — | deferred:scope-amendment |
| REQ-BRIEFING-03 | Satisfied | `cache.rs:9-72`; `migrations/0001_initial_schema.sql:151-164` | TTL pruner missing (sub-gap) | — | — |
| REQ-BRIEFING-04 | Deferred (✅ amended §0 #14) | no `knowledge_basis` computation | depends on WP7 guidance + WP9-B triage | — | deferred:scope-amendment |
| REQ-BRIEFING-05 | Deferred (✅ amended §0 #14) | no triage-into-briefing path | depends on WP9-B + WP7 | — | deferred:scope-amendment |
| REQ-BRIEFING-06 | Deferred (✅ amended §0 #14) | no detail levels | per ADR-030 (briefing pipeline) | — | deferred:scope-amendment |
| REQ-CATALOG-01 | Satisfied | `analyze.rs:382,794`; e2e | — | — | — |
| REQ-CATALOG-02 | Satisfied | `host.rs:897`; `plugin.toml:38` | Positive "novel kind round-trip" test implicit | P3 | new |
| REQ-CATALOG-03 | Satisfied | `migrations/0001_initial_schema.sql:81`; `host.rs:1043` | `guides`/`emits_finding` reserved; no producer | — | deferred:scope-amendment |
| REQ-CATALOG-04 | Partial | `analyze.rs:1544` | git_churn/last_modified/authors/size/lines/mime never populated | P1 | new |
| REQ-CATALOG-05 | Satisfied | `analyze.rs:657`; `clustering.rs`; `tests/analyze.rs:730` | — | — | — |
| REQ-CATALOG-06 | Satisfied | `entity_id.rs`; `qualname.py` | Move-without-rename test missing | P3 | new |
| REQ-CATALOG-07 | **Missing** | `commands.rs:66`; call-sites write `None` | No HEAD-SHA capture | P1 | new |
| REQ-CONFIG-01 | Partial | `config.rs:16` | No `~/.config/clarion/defaults.yaml` merge; no `version:` field | P2 | new |
| REQ-CONFIG-02 | Deferred | no `profile`/`budget`/`default`/`deep` enum | WP6 deferral | — | deferred:scope-amendment |
| REQ-CONFIG-03 | Deferred | no dry-run estimator | WP6 + WP11 | — | deferred:scope-amendment |
| REQ-CONFIG-04 | Deferred | no `LlmPolicyConfig` | WP6 | — | deferred:scope-amendment |
| REQ-CONFIG-05 | Partial | `secret_scan.rs:574`; CLI flag | No `analysis.include`/`exclude` globs in config schema | P2 | new |
| REQ-FINDING-01 | Satisfied | `commands.rs:113-133`; `migrations/0001_initial_schema.sql:104-135` | — | — | — |
| REQ-FINDING-02 | Satisfied | `manifest.rs`; `limits.rs:37-54`; `pyright_session.py:34-41` | — | — | — |
| REQ-FINDING-03 | Deferred | scope-amendment §3-4 | WP9-B v1.1 | — | deferred:scope-amendment |
| REQ-FINDING-04 | Deferred | requirements.md:332 | WP10 SARIF translator | — | deferred:scope-amendment |
| REQ-FINDING-05 | Deferred | requirements.md:342 | scan_run_id mapping | — | deferred:scope-amendment |
| REQ-FINDING-06 | Deferred | requirements.md:352 | mark_unseen dedup | — | deferred:scope-amendment |
| REQ-GUIDANCE-01 | Deferred (✅ amended §0 #13) | view substrate exists; no behaviour | WP7 deferred whole | — | deferred:scope-amendment |
| REQ-GUIDANCE-02 | Deferred (✅ amended §0 #13) | `lib.rs:38 EMPTY_GUIDANCE_FINGERPRINT` placeholder | composition algo v0.2 | — | deferred:scope-amendment |
| REQ-GUIDANCE-03 | Deferred (✅ amended §0 #13) | no CLI / no `propose_guidance` MCP | WP7 authoring surface v0.2 | — | deferred:scope-amendment |
| REQ-GUIDANCE-04 | Deferred (✅ amended §0 #13) | no wardline_derived emitter | WP7 deferred | — | deferred:scope-amendment |
| REQ-GUIDANCE-05 | Deferred (✅ amended §0 #13) | no churn-vs-guidance code | WP7 deferred | — | deferred:scope-amendment |
| REQ-GUIDANCE-06 | Deferred (✅ amended §0 #13) | no `guidance export/import` CLI | WP7 deferred | — | deferred:scope-amendment |
| REQ-HTTP-01 | Deferred (✅ amended §0 #3) | `http_read.rs:364-372` ships ADR-014 subset | broader catalogue v0.2 per amendment §4 | — | deferred:scope-amendment |
| REQ-HTTP-02 | Deferred (✅ amended §0 #4) | `:resolve` handles file_path scheme only | multi-scheme oracle v0.2 (depends on WP9-B Wardline ingest) | — | deferred:scope-amendment |
| REQ-HTTP-03 | Satisfied | `http_read.rs:179-251,389-498`; `tests/serve.rs:1160..1342` | — | — | tracked:adr-034-refresh |
| REQ-HTTP-04 | Partial | `http_read.rs:735-816` | Uses `ETag` per-file vs spec'd `X-Clarion-State` run-level | P2 | new |
| REQ-MCP-01 | Deferred (✅ amended §0 #6) | `lib.rs:187-211` no cursor/breadcrumb state | cursor session model v0.2 (B.6 narrowed surface) | — | deferred:scope-amendment |
| REQ-MCP-02 | Deferred (✅ amended §0 #1) | `lib.rs:52-129` ships 8-tool MVP subset per amendment B.6 | broader catalogue v0.2 | — | deferred:scope-amendment |
| REQ-MCP-03 | Deferred (✅ amended §0 #2) | no `find_entry_points` etc.; depends on Phase-7 pre-compute | shortcuts v0.2 per ADR-030 + amendment §4 | — | deferred:scope-amendment |
| REQ-MCP-04 | Satisfied (✅ amended §0 #7) | `http_read.rs` execution_edge_cap; `lib.rs:74,93` bound 8-tool subset intrinsically | per-tool token budgets v0.2 (catalogue-deferred) | — | — |
| REQ-MCP-05 | Deferred (✅ amended §0 #8) | no write-effect tools in 8-tool MVP surface | consent gate v0.2 with catalogue | — | deferred:scope-amendment |
| REQ-MCP-06 | Deferred (✅ amended §0 #9) | `lib.rs:162-185` no session persistence | session model v0.2 with cursor (REQ-MCP-01) | — | deferred:scope-amendment |
| REQ-PLUGIN-01 | Satisfied | `transport.rs:113,287,42,57-63`; `server.py:73,119` | — | — | — |
| REQ-PLUGIN-02 | Satisfied | `manifest.rs:283,43`; `plugin.toml:38`; tests | Tags/prompt_templates absent (WP6) | — | — |
| REQ-PLUGIN-03 | Partial | `host.rs:815`; `server.py:179,238` | `build_prompt` RPC + `file_list` absent (ADR-030 narrowed) | — | deferred:scope-amendment |
| REQ-PLUGIN-04 | Partial | `plugin.toml`; extractor + resolvers | `protocol`/`global` kinds, `decorated_by`/`inherits_from`/`uses_type`/`alias_of` edges absent | P2 | new |
| REQ-PLUGIN-05 | **Missing** | `reference_resolver.py:4` only type-imports TYPE_CHECKING | No exclusion / src-prefix / placeholders / aliases | P1 | new |
| REQ-PLUGIN-06 | **Missing** | no `decorated_by` in `plugin.toml:40`; no emit-site | Decorator detection entirely absent | P1 | new |

### 6.2 REQ-INTEG-* (cross-product)

| ID | Verdict | Evidence | Gap | Sev | Tracked |
|----|---------|----------|-----|-----|---------|
| REQ-INTEG-FILIGREE-01 | Deferred | requirements.md:600 | WP9-B | — | deferred:scope-amendment |
| REQ-INTEG-FILIGREE-02 | Deferred | requirements.md:610 | WP9-B observation emission | — | deferred:scope-amendment |
| REQ-INTEG-FILIGREE-03 | Deferred | requirements.md:620 | Registry-backend consumption | — | deferred:scope-amendment |
| REQ-INTEG-FILIGREE-04 | Deferred | requirements.md:630 | `scan_source` ns + schema pin | — | deferred:scope-amendment |
| REQ-INTEG-FILIGREE-05 | Deferred | requirements.md:640 | Capability probe | — | deferred:scope-amendment |
| REQ-INTEG-WARDLINE-01 | Satisfied | `wardline_probe.py:35-56`; `plugin.toml:55-56`; tests | Fail-soft only; no mirror module, no `MIRRORED` finding | P2 | tracked:clarion-88d2ef40b6 |
| REQ-INTEG-WARDLINE-02 | Deferred | no `wardline.yaml` reader | WP9-B Wardline-config ingest | P2 | deferred:scope-amendment (row-level blockquote in `requirements.md:702`) |
| REQ-INTEG-WARDLINE-03 | Deferred | `entities.wardline_json` always `None` | WP9-B | P2 | deferred:scope-amendment (row-level blockquote in `requirements.md:712`) |
| REQ-INTEG-WARDLINE-04 | Deferred | no `wardline.exceptions.json` reader | WP9-B | P2 | deferred:scope-amendment (row-level blockquote in `requirements.md:722`) |
| REQ-INTEG-WARDLINE-05 | Deferred | no `clarion sarif import` | WP10 | P2 | deferred:scope-amendment (row-level blockquote in `requirements.md:732`) |
| REQ-INTEG-WARDLINE-06 | Deferred | no resolve oracle for wardline schemes | Depends on -03/-04 | P2 | deferred:scope-amendment (row-level blockquote in `requirements.md:742`) |

### 6.3 NFR-*

| ID | Verdict | Evidence | Gap | Sev | Tracked |
|----|---------|----------|-----|-----|---------|
| NFR-SEC-01 | Satisfied | `clarion-scanner/src/lib.rs`; `secret_scan.rs:212-263`; tests | — | — | — |
| NFR-SEC-02 | Deferred | no `<file_content trusted="false">`, no schema validation | ADR-009 Backlog + WP6/WP7 | — | deferred:scope-amendment |
| NFR-SEC-03 | Satisfied | `tests/serve.rs:1160..1317`; HMAC helper :2231 | Verification line citations stale | — | tracked:adr-034-refresh |
| NFR-SEC-04 | Partial | `secret_scan/findings.rs:17`; `secret_scan.rs:36`; `breaker.rs:16` | `CLA-INFRA-TOKEN-STORAGE-DEGRADED`, `CLA-INFRA-BRIEFING-INVALID`, `CLA-SEC-VOCABULARY-CANDIDATE-NOVEL` absent | P2 | deferred:scope-amendment |
| NFR-SEC-05 | Satisfied | `install.rs:97`; `tests/install.rs:40,45` | — | — | — |
| NFR-RELIABILITY-01 | Partial | `pragma.rs:17-30`; writer-actor; `analyze_lock.rs` | No `--resume`; no SIGKILL+reopen test | P2 | partly deferred; tracked:STO-04 (clarion-ee22d1d72c) |
| NFR-RELIABILITY-02 | Missing | no `--no-filigree`/`--no-wardline` flags; no `findings.jsonl` fallback | WP9-B | P2 | deferred:scope-amendment |
| NFR-RELIABILITY-03 | Partial | `host_findings.rs:17-76`; `breaker.rs:16`; `analyze.rs:1355,2279` | LLM rate/budget/schema-invalid emitters absent; no PRAGMA integrity_check in e2e | P2 | partly deferred; tracked:STO-04 |
| NFR-PERF-01 | Deferred (✅ amended §0 #15) | `analyze.rs:482-495` ends at Phase 3 | 60-min target needs Phases 4–6 per ADR-030 | — | deferred:scope-amendment |
| NFR-PERF-02 | Partial | `lib.rs:176,256,788,1339-1367` | No benchmark harness | P2 | tracked:briefing_blocked index (clarion-bdabfd6bca) |
| NFR-PERF-03 | Missing | no 20-turn token-budget assertion | Detail levels absent (BRIEFING-06) | P2 | new |
| NFR-SCALE-01 | Partial | `limits.rs:115-165` `EntityCountCap=500k` | No recorded elspeth run | P2 | new |
| NFR-SCALE-02 | Missing | no DB-size assertion or fixture | — | P3 | new |
| NFR-SCALE-03 | Partial | `reader.rs:46-49`; `serve.rs:46`; `tests/reader_pool.rs` | No combined-load saturation test | P3 | new |
| NFR-COST-01 | Deferred (✅ amended §0 #16) | per-call cost captured; no run-level budget gate | $15 elspeth target needs Phases 4–6 per ADR-030 | — | deferred:scope-amendment |
| NFR-COST-02 | Partial | `cache.rs:48-110`; `lib.rs:788-792` | No `cache_hit_rate` aggregation; no repeat-run measurement | P2 | new |
| NFR-COST-03 | Deferred (✅ amended §0 #17) | no preflight/dry-run code | Phase 0 deferred per ADR-030 | — | deferred:scope-amendment |
| NFR-OPS-01 | Satisfied | `.github/workflows/release.yml:163,180,306,391` | Matrix 3 of 5 targets | P2 | new |
| NFR-OPS-02 | Satisfied | no telemetry import; CHANGELOG | — | — | — |
| NFR-OPS-03 | Partial | `install.rs:78-97` | No `clarion db export --textual` or `db merge-helper` subcommand | P1 | new |
| NFR-OPS-04 | Partial | `pyproject.toml`; `release.yml:241` | Not on PyPI; SLSA covers rust only | P1 | tracked:slsa-python-sdist (clarion-f530101222) |
| NFR-OBSERV-01 | Partial | `main.rs:69-76` plain-text tracing | No JSON, no file sink, no rotation, no per-run log | P1 | new |
| NFR-OBSERV-02 | Partial | `analyze.rs:174,527,563,670`; `writer.rs:234,328` | `runs/<run_id>/stats.json` file never written | P1 | new |
| NFR-OBSERV-03 | **Missing** | no Prometheus surface | — | P1 | new |
| NFR-OBSERV-04 | **Missing** | no `CLA-INFRA-SUITE-COMPAT-REPORT` emitter | — | P1 | new |
| NFR-COMPAT-01 | **Missing** | no Filigree schema-pin CI job | — | P1 | new |
| NFR-COMPAT-02 | Partial | `wardline_probe.py:35`; `plugin.toml:50`; tests | Pins `__version__` not `REGISTRY_VERSION`; no `MIRRORED`/`ADDITIVE-SKEW` findings; no mirror fallback | P1 | new |
| NFR-COMPAT-03 | Missing | no Anthropic SDK dependency | LLM path deferred | P3 | deferred:scope-amendment |

### 6.4 CON-*

| ID | Verdict | Evidence | Gap | Sev | Tracked |
|----|---------|----------|-----|-----|---------|
| CON-LOOM-01 | Satisfied | no cross-product mediator; `filigree.rs` is read-only client | — | — | — |
| CON-FILIGREE-01 | Deferred | no `scan-results` POST | WP9-B | — | deferred:scope-amendment |
| CON-FILIGREE-02 | Satisfied | no `RegistryProtocol` impl; shadow-registry only | — | — | — |
| CON-WARDLINE-01 | Satisfied | `wardline_probe.py:38-43` direct import | Asterisk per loom.md §5 | — | — |
| CON-ANTHROPIC-01 | Partial | `mcp/src/config.rs:95-99`; `cli/src/serve.rs:11` | `CodexCliProvider` is non-Anthropic vendor surface | Med | new (recommend ticket) |
| CON-LOCAL-01 | Satisfied | CLI-only; LLM is only network egress | — | — | — |
| CON-RUST-01 | Satisfied | trivially | — | — | — |
| CON-SQLITE-01 | Satisfied | `rusqlite` + `deadpool-sqlite` | — | — | — |

### 6.5 NG-* (inverted: all 25 honoured)

All 25 non-goals returned the expected absence pattern. No drift detected. The codebase ships no rule engine, no taint, no SARIF export, no file watchers, no multi-branch analysis, no rename detection, no wiki UI, no second language plugin, no coverage ingestion, no advanced git analysis, no plugin hash-pinning, no triage-feedback loop, no Wardline HTTP state-pull, no BAR awareness, no Wardline annotation descriptor, no `EntityAlias`, no Phase-7 cross-cutting analyses, no incremental analysis, no Filigree server-side dedup. The two named asterisks (`docs/suite/loom.md` §5: Wardline REGISTRY import, Wardline pipeline coupling) remain exactly where the federation axiom documents them with the named retirement conditions still standing.

---

## 7. Appendix: how to reproduce this audit

Source artefacts:

- Per-agent partial reports: `/tmp/clarion_gap_{A,B,C,D,E,F,G}.md` (transient; regenerated each run).
- In-flight Filigree snapshot: `/tmp/clarion_gap_filigree_ready.json` and `…_inprogress.json`.
- Trace bridge: `**Addresses**:` headers in `docs/clarion/1.0/system-design.md` lines 38, 122, 243, 389, 482, 575, 664, 749, 846, 1056, 1139.

The audit was executed by seven concurrent subagents (general-purpose, capped at three in flight) over `docs/clarion/1.0/requirements.md` + `docs/clarion/1.0/system-design.md` + `docs/clarion/1.0/detailed-design.md` + the workspace source. Verdicts cite specific `file:line` evidence so each row is independently re-verifiable.
