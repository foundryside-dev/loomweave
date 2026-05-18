# Validation — `04-final-report.md`

**Validator:** axiom-system-archaeologist:analysis-validator
**Date:** 2026-05-18
**Target:** `docs/arch-analysis-2026-05-18-1244/04-final-report.md`
**Status:** **NEEDS_REVISION (warnings)** — structurally complete, all required sections present, internally consistent on priority mapping; multiple stale numeric facts at the §1 / §5 / §6 level that drifted between the catalog pass and the report write-up. None of the drifts invalidate the report's architectural conclusions, but the report explicitly bills itself as evidence-anchored, and the numbers cited in the executive summary and the deltas section must match the working tree.

---

## Summary

The report covers every contract-required section (executive summary; architecture at a glance; per-subsystem walkthrough for all 6 subsystems; cross-cutting concerns table; prioritised observations & risks; Sprint-2 deltas; recommended follow-ups; confidence + limitations + audit trail). Risk priorities in §5 and recommendation priorities in §7 are mapped one-to-one (3 High → items 1–3; 5 Medium → items 4–8; 8 Low → items 9–16). The §8 audit-trail file list matches the actual workspace contents (1 coordination + 1 discovery + 1 catalog + 1 diagrams + 1 report + 6 section-*.md + 2 validation-*.md). Cross-cutting concerns enumerate 14 entries grounded in ADR or doctrine citations.

The report is fit for an architect handover at the level of structure, narrative, and prioritisation. The blocker is a cluster of stale numbers that the report inherited from the catalog pass and that drifted further before the report was written.

---

## Findings

### Critical

None. No claim is contradicted by primary evidence in a way that changes a conclusion. The drifts below are warnings, not blockers.

### Warnings

**W-1. `lib.rs` LOC stale by ~90 lines.**
Report §1 ("Standout strengths") and §2 ("Read surface") cite `clarion-mcp/src/lib.rs` at **2 623 LOC**. Working tree on `sprint-2/b8-scale-test` HEAD shows **2 712 LOC** (`wc -l crates/clarion-mcp/src/lib.rs`). The catalog footnoted the prior drift (2 620 → 2 623); two further commits on this branch (`7317a91` clippy-explicit Map; `87036b1` reservation-poison fix; `363bb0a` inferred-target pre-filter) appear to have pushed the file past 2 700 since the catalog pass. Recommend re-running `wc -l` against the working tree and updating §1, §2 table, §3.3, and §6 "drift from 2620 to 2623" parenthetical.

**W-2. "11 commits ahead of `main`" is now 17.**
Report header line 4 and §6 paragraph "Current branch (`sprint-2/b8-scale-test`) vs. `main`". Verified by `git rev-list --count main..HEAD` → **17**. The discovery doc (01) also says 11 — the figure was correct at the start of the analysis and stale by the time the report was written; four additional commits landed on the branch between discovery and report-writing: `caa6459` (B.8 raw artifacts), `f7bb63f` (CLAUDE.md refresh), `7317a91` (clippy), `87036b1` (poison fix), `363bb0a` (pre-filter inferred). Two of those (`87036b1`, `363bb0a`) are substantive MCP source-code changes that should plausibly join the §6 list of "substantive source changes" if the report wants to remain accurate at the commit level.

**W-3. `git diff --stat main..HEAD` numbers are stale.**
Report §6 reports "45 files changed, 23 209 insertions, 59 deletions". Verified `git diff --stat main..HEAD | tail -1` → **59 files changed, 25 650 insertions, 85 deletions**. Same root cause as W-2 — the same five commits landed after the figures were captured.

**W-4. Spot-check M-1 line-number is wrong.**
Report §5 M-1 cites `clarion-mcp/src/lib.rs:2381` as the location of `reference_neighbors`'s `conn.prepare(`. Verified: the only `conn.prepare(` site in the crate is at **line 2470**; the `reference_neighbors` fn itself begins at **line 2455**. The *claim* M-1 makes ("the only `conn.prepare(` site in the crate") is correct — only the line number is stale. The catalog also stated `reference_neighbors` is at "lib.rs:2363–2400" / "line 2378", which is also stale by the same +80 lines as W-1. Recommend either re-running grep at report-emit time or footnoting the line-number-as-of-commit. The Concern-level claim survives.

**W-5. Sprint-2 deltas listing presented as 7 items but worth bullet count check.**
Report §6 paragraph "Whole-of-Sprint-2" lists six bullets B.2–B.8 plus "OpenRouter swap" — actually seven bullets. The narrative claim above the bullets ("Six merged work-package landings since `v0.1-sprint-1`") undercounts by one. Either the narrative count is wrong (should be seven) or "OpenRouter swap" is being mentally excluded (in which case the list should say so explicitly). Trivial to fix.

### Spot-check results

| # | Claim | Source line | Result |
|---|---|---|---|
| 1 | §5 H-1: `analyze.rs:478–509` is the `SoftFailed` branch that folds `UPDATE runs SET status='failed'` into the open entity transaction | `crates/clarion-cli/src/analyze.rs:478–509` | **CONFIRMED** — `RunOutcome::SoftFailed { reason }` arm at 478, `CommitRun { status: RunStatus::Failed, ... }` at 499–501, `"CommitRun(Failed) — soft fail"` context at 508. |
| 2 | §5 M-1: `clarion-mcp/src/lib.rs:2381` is the only `conn.prepare(` site in the crate | `grep -n 'conn.prepare(' crates/clarion-mcp/src/lib.rs` | **PARTIAL** — exactly one `conn.prepare(` site (claim survives); line number is **2470**, not 2381 (W-4). |
| 3 | §4: ADR-031 added six CHECK clauses | `crates/clarion-storage/migrations/0001_initial_schema.sql` | **CONFIRMED** — `grep -cE 'CHECK\s*\(' ...sql` → **6**: `edges.confidence` (90), `findings.kind` (108), `findings.severity` (112), `findings.status` (125), `summary_cache.stale_semantic` (153), `runs.status` (201). |
| 4 | §1: no god-files. The two largest source files are coherent, banded, internally documented; `host.rs` ~3 126 LOC, `lib.rs` ~2 623 LOC | `wc -l crates/clarion-core/src/plugin/host.rs crates/clarion-mcp/src/lib.rs` | **PARTIAL** — `host.rs` 3 126 ✓; `lib.rs` actually **2 712** (W-1). The "no god-files" architectural judgment is unaffected. |
| 5 | §6: 11 commits ahead of main | `git rev-list --count main..HEAD` | **WRONG** — actually **17** (W-2). |
| 6 | §1: 24 462 Rust LOC across 5 crates; 5 629 Python; 78 markdown docs | `find` + `wc -l` | **PARTIAL** — Rust now **24 727** (drift +265, same root cause as W-1/W-2); Python 5 629 ✓; markdown actually **91** files in `docs/` tree (`find docs -name '*.md' \| wc -l`). The "78 markdown docs" figure in §1 also disagrees with discovery doc 01 §1.1 which says "79 .md files" — the two source documents already disagreed before the report was written. |

### Notes (non-blocking)

**N-1. §3.3 "Three source files" claim.**
"One Rust crate, three source files (`lib.rs` 2 623 + `config.rs` 352 + `filigree.rs` 238)" — correct count, stale LOC for `lib.rs` (W-1). Same drift, no extra issue.

**N-2. §4 cross-cutting concerns: edge-ontology row labels three sites of duplication.**
"Hard-coded in `clarion-storage::writer.rs:394–401`; declared in `plugin.toml:38`; documented in ADR — **3-place duplication, no cross-check**." Catalog §clarion-storage §Concerns (per the report's own internal reference) backs this. Not validated against `writer.rs` line numbers in this pass — flagged as a candidate spot-check for any deeper validation.

**N-3. §8 audit-trail listing omits dot-prefix grep-friendliness but otherwise matches.**
The listing labels `00-coordination.md` "coordination plan + execution log" — workspace `ls` confirms file exists. Sub-tree files (`temp/section-*.md`, `temp/validation-*.md`) all present.

**N-4. §5.4 Doctrine-accepted risks (A-1 … A-4) are present and explicitly mark themselves "do not fix".**
This is the right framing for the federation asterisks and the file-size acceptances; matches discovery doc §5 and catalog §clarion-core §A-3. Worth keeping.

**N-5. Information gaps section (§8) is candid and matches the actual scope of the pass.**
"Test coverage is not depth-read", "external siblings are not vendored", "B.8 result data not inspected" — all consistent with what the discovery and catalog passes actually did. Good limitation discipline.

**N-6. Recommendations §7 ordering is risk-priority-first within tier and then by §5 letter.**
Item 1 = H-1, item 16 = L-8. Consistent. No "high" recommendation without a matching §5 High; no §5 High without a matching item.

**N-7. Coverage check: every subsystem in the catalog has a §3 subsection in the report.**
A (`clarion-core`) → §3.1; B (`clarion-storage`) → §3.2; C (`clarion-mcp`) → §3.3; D (`clarion-cli`) → §3.4; E (`clarion-plugin-fixture`) → §3.5; F (`plugins/python`) → §3.6. No subsystem from the catalog is silently omitted. Every catalog "Concerns" entry (read at the heading level) is folded into §5 either as a numbered risk or as one of the doctrine-accepted A-* items.

**N-8. No silent omission of cross-cutting concerns from discovery / catalog.**
The 14-row table in §4 covers entity-ID, JSON-RPC L4, plugin authority, ontology ownership, edge confidence, edge ontology, ontology semver, summary cache key, migration governance, schema-validation policy, federation axiom, Filigree bindings, summary scope, tooling baseline. Discovery §5 has 8 concerns — all 8 are in §4. Catalog cross-references are honoured.

---

## Confidence Assessment

- **Structural compliance:** High — every contract-required section is present in the correct order, properly cross-referenced to source artifacts.
- **Internal consistency:** High — risk-priority ↔ recommendation-priority mapping is one-to-one and complete.
- **Cross-document coverage:** High — every subsystem in the catalog has a walkthrough entry; every catalog concern entry maps to a §5 risk or §5.4 acceptance.
- **Numerical accuracy:** Medium — 5 of 6 spot-checks revealed stale figures inherited from the catalog or drifted further before report emit. The drifts are all "snapshot in motion" artefacts (LOC, commit count, diff stats) rather than misreadings of source.
- **Conclusion durability:** High — none of the stale numbers, if corrected, would change a §5 priority assignment or a §7 recommendation.

## Risk Assessment

- **Risk that downstream readers cite stale figures:** Medium. The report is explicitly the synthesis layer; a Sprint-3 plan citing "11 commits ahead", "lib.rs 2 623 LOC", or "45 files changed" will be wrong on day one of execution.
- **Risk that the file-size acceptance (A-4) shifts:** Low-Medium. The catalog argued `lib.rs` at 2 620 is coherent; at 2 712 the same argument still holds, but the file is on a measurable growth trajectory (+92 LOC across five commits on this branch alone). If §5.4 A-4 is to remain "do not fix", a "monitored, not accepted indefinitely" footnote would help.

## Information Gaps

- The report's "audit trail" §8 lists `temp/` contents in shape but does not record a hash or modification time. If the report is consumed weeks later, the validator artefacts may have drifted again.
- Several catalog line-number citations (e.g. `writer.rs:394–401` for `STRUCTURAL_EDGE_KINDS`, `lib.rs:1010–1016` for the 5-tuple cache key, `lib.rs:1180–1316` for `BudgetLedger.blocked`) were not independently re-verified in this validation pass — they may be subject to the same drift as W-1/W-4.
- The §6 narrative "Six merged work-package landings" / actually seven bullets discrepancy (W-5) suggests the deltas section was edited under time pressure.

## Caveats

- This is a structural-compliance validation, not a technical-accuracy validation. The architectural judgments in the report (e.g. "boundary discipline is real", "plugin separation is a process boundary not a trait abstraction", "no god-files") were not independently re-derived — they were checked for internal coherence with discovery and catalog, not against the source code as a quality assessment. A technical critique should go through `axiom-system-architect:assess-architecture` as the report itself notes.
- The line-number / LOC drift findings are inherently snapshot-bound; any document of this kind written against a live working tree carries the same risk. Recommend the analysis workspace pin a commit hash in each document's header so future readers can re-execute the spot-checks deterministically.

---

## Recommendation

**Status: NEEDS_REVISION (warnings).** Five low-effort edits unblock APPROVED:

1. Re-run `wc -l crates/clarion-mcp/src/lib.rs` and update §1 (Standout strengths), §2 (Read surface table), §3.3 (first sentence), §6 ("from 2620 to 2623" parenthetical).
2. Re-run `git rev-list --count main..HEAD` and update the header line 4 and §6 first sentence of the "Current branch" paragraph.
3. Re-run `git diff --stat main..HEAD | tail -1` and update §6 "45 files changed, 23 209 insertions, 59 deletions".
4. Re-grep `conn.prepare(` and update the §5 M-1 line number (currently `:2381`, should be `:2470`).
5. Reconcile "Six merged work-package landings" with the seven bullets that follow — either say "Seven" or split the OpenRouter swap out of the merged-WP list explicitly.

Pin a commit hash in the report header (e.g. `Commit: 363bb0a`) so future readers can re-derive every numeric claim deterministically. Then re-validate — expected pass.
