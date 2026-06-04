# Validation Report — 04/05/06 synthesized architecture docs

**Validator:** independent analysis-validator (fresh eyes)
**Date:** 2026-06-02 · Branch `feat/road-to-first-class` · v1.1.0
**Documents under test:**
- `04-final-report.md`
- `05-quality-assessment.md`
- `06-architect-handover.md`

**Source of truth:** `02-subsystem-catalog.md` (validated + corrected) and `temp/validation-catalog.md`.
**Scope:** Faithfulness to the corrected catalog + internal consistency across the three docs + filigree-ID reality + no invented claims. This is a structural/evidentiary validation, not a re-derivation against live HEAD.

## Overall status: **APPROVED-WITH-WARNINGS**

The three docs are internally consistent, faithful to the corrected catalog, cite only real filigree tickets, and reintroduce none of the four corrected catalog errors. One warning (a point-in-time LOC for `http_read.rs` that the working tree outgrew *during* the synthesis window) and one trivial metric undercount (`unsafe` blocks) are the only blemishes. Neither changes any conclusion. **Not blocking.**

---

## Checklist

| # | Check | Result |
|---|-------|--------|
| 2 | **Filigree IDs are real** (6 cited) | ✅ PASS — all 6 resolve; titles match doc descriptions |
| 1 | **Drift-register fidelity** (corrected errors stay dead) | ✅ PASS — all 6 corrected items handled correctly |
| 4 | **Internal consistency** across 04/05/06 (+01/03) | ✅ PASS — 35 tools / 16 routes / 6 crates / 13 tables all agree |
| 3 | **No invented claims** | ✅ PASS — zero invented; 1 stale (http_read LOC), 1 undercount (unsafe) |

---

## Check 2 — Filigree IDs (HIGHEST VALUE) — PASS

Every cited ticket resolves to a real issue via `mcp__filigree__get_issue`, and each title matches the description the docs attach to it:

| ID cited | Doc usage | Real issue title | Status | Match |
|---|---|---|---|---|
| `clarion-42cbd8a25a` | split mcp/lib.rs (Q1, 06 §6) | "Split clarion-mcp/src/lib.rs into tools/ subdir" | proposed | ✅ |
| `clarion-cb9676de57` | split analyze.rs (Q3) | "Split analyze.rs run_with_options into analyze/phase3.rs + analyze/mapping.rs" | proposed | ✅ |
| `clarion-2b8811da39` | split host.rs (Q4, catalog) | "Split plugin/host.rs validation from transport" | proposed | ✅ |
| `clarion-1f6241b329` | wardline registry migration (Q6) | "Migrate Python plugin off direct wardline.core.registry import → NG-25 descriptor" | open | ✅ |
| `clarion-141e9c08c8` | extract clarion-llm (Q7) | "Extract clarion-llm crate from clarion-core" | proposed | ✅ |
| `clarion-f9027d2187` | runs.owner_pid + heartbeat_at (Q9) | "runs.owner_pid + heartbeat_at columns" | proposed | ✅ |

The docs describe `cb9676de57` as "→ analyze/phase3.rs + analyze/mapping.rs, proposed" — verbatim accurate against the real title and status. `1f6241b329` is described "open/ready/P2" — real issue is open, P2, is_ready=true. **No hallucinated tickets.**

---

## Check 1 — Drift-register fidelity (corrected errors stay dead) — PASS

The 04 §4 drift table (D1–D8), 05 Q5/Q12, 06 §3 D-table, and the 03 drift map were checked against the six corrections mandated by `validation-catalog.md`. **All corrections are honored; no corrected error reappears:**

1. **ADR-013 GCP NOT drift** — Refuted item is *absent* from the D1–D8 table (D-table runs D1–D8 with no ADR-013/GCP row). 03 diagram states it explicitly: *"ADR-013's GCP-rule 'drift' was a validation strawman — doc and code agree — and has been dropped."* 05 Q14 frames it as a 🟢 doc-clarity nit ("no design doc enumerates the 12 secret-scan rules"), not drift. ✅
2. **Provider count = 4** — D3 says "4 providers (no Anthropic)"; 04 §1/§3 says "4 providers"; 06 D3 "fix the provider list (4, no Anthropic)". The erroneous "three providers" never appears. ✅
3. **detailed-design = "6 tables + FTS5"** — D7 says "`detailed-design.md:611-760`: 6 tables + FTS5"; 05 metrics "doc says 6+FTS5". No "7 tables" / "1 migration" claim survives. ✅
4. **CLAUDE.md = "4 crates"** — D8 says "CLAUDE.md Layout: 4 crates / v1.0.0 (`clarion-mcp`, `clarion-scanner` omitted)"; 06 D8 "Add `clarion-mcp` + `clarion-scanner`; bump 5→6 crates". The erroneous "5 crates" never appears as the doc-side claim. ✅
5. **Wardline routes ×4** — 04 §3.3 "16 routes"; catalog and 04 enumerate "wardline×4" / "/api/wardline/* ×4". The erroneous "×3" never appears. Verified against source: lines 497/498/502/512 = 4 wardline routes. ✅
6. **Four phase-7 findings = "unimplemented," CLA-FACT-CLUSTERING-WEAK-MODULARITY ships** — D4 says "unimplemented; … (1 other CLA-FACT *does* ship)"; 05 Q12 "four of five §6 `CLA-FACT-*` findings are unimplemented"; 06 D4 "note `CLA-FACT-CLUSTERING-WEAK-MODULARITY` ships". No "absent workspace-wide" phrasing survives. Verified against source: the 4 named findings = 0 matches in crates/plugins; `CLA-FACT-CLUSTERING-WEAK-MODULARITY` present at `analyze.rs:50`. ✅

---

## Check 4 — Internal consistency — PASS

Counts agree across all three docs (and 01/03):

| Count | 04 | 05 | 06 | 01/03 | Source-verified |
|---|---|---|---|---|---|
| MCP tools | 35 | 35 | 35 | 35 | ✅ 35 dispatch arms in lib.rs |
| HTTP routes | 16 | (16 via D-table) | 16 | 16 | ✅ 16 production routes |
| Crates | 6 | 6 | 6 (5→6 in D8) | 6 | ✅ 6 Cargo.toml members |
| SQLite tables | 13+FTS5+view | 13+FTS5+view | 13+FTS5+view | 13 | (catalog-verified) |
| mcp/lib.rs LOC | 7,101 | 7,101 | 7,101 | 7,101 | ✅ 7,101 |
| analyze.rs LOC | 3,542 | 3,542 | 3,542 | 3,542 | ✅ 3,542 |
| host.rs LOC | 2,958 | 2,958 | 2,958 | 2,958 | ✅ 2,958 |
| llm_provider LOC | — | (Q7) 2,500 | 2,500 | 2,500 | ✅ 2,500 |
| http_read LOC | 4,387 | 4,387 | 4,387 | 4,387 | ⚠ now 4,765 (see warning) |

All three docs are mutually consistent. The one number that no longer matches live source (`http_read.rs`) is consistently 4,387 across *all* docs — i.e. the inconsistency is doc-vs-current-HEAD, not doc-vs-doc.

---

## Check 3 — No invented claims — PASS (1 stale, 1 undercount)

Spot-checked the falsifiable claims in 04/05/06 that are not verbatim in catalog 02. None contradicts source ("invented" = contradicts source). Findings:

- **WARNING (stale, not invented): `http_read.rs` = 4,387 LOC.** Real value is now **4,765** (4,435 non-blank). Traced via git: at HEAD~2 (`a57760f`, 2026-06-02 09:49) the file was *exactly* 4,387. Two commits — `caa2665` (15:44, Wardline T3.4 read-by-SEI) and `204790e` (15:51, T3.4 review minors) — grew it during the synthesis window (docs written 15:50–15:53). So 4,387 was **accurate when captured** and is now stale by ~378 lines. Blast radius is bounded: of 9 highest-risk files wc-l'd, only `http_read.rs` drifted; 7,101 / 3,542 / 2,958 / 2,500 / 1,427 / 1,727 / 1,211 all still match. No conclusion changes (the "split this ~4–5K-LOC file; auth buried inside" finding holds at 4,387 or 4,765). **Recommend a one-line errata: http_read.rs grew to 4,765 LOC after the read snapshot.**
- **MINOR (undercount, not invented): 05 metrics row "Unsafe blocks: 1 (documented)."** Source has **2** non-test `unsafe` blocks: `host.rs:602` (the `pre_exec`/setrlimit one the doc names) and `plugin-fixture/src/main.rs:155` (test-only fixture binary). "1 documented unsafe in the supervisor" is defensible, but the bare metric "1" undercounts by one. Cosmetic; does not affect any finding.
- **Delta-table "then" values (19 tools, 4,703, 2,549 LOC, ~9 tables) in 04 §2** are explicitly labeled as 2026-05-22 prior-analysis figures (historical), not current-state claims — out of scope to chase, not invented.
- All other numbers (35, 16, 6, 13, the four largest current LOCs, `application_id=0x434C524E`, batch-commit-every-50, 25-file pyright recycle, 12 named scanner rules) trace to the catalog or were source-verified here.

---

## Confidence Assessment

**High.** The critical check (6 filigree IDs) was verified directly against the live tracker; all resolve with matching titles/status. Drift-fidelity was checked line-by-line against the six mandated corrections in `validation-catalog.md` and against source for the two falsifiable cases (phase-7 findings, wardline route count). Internal-count consistency is a mechanical cross-read of four documents plus source counts. The single discrepancy (http_read LOC) was root-caused to two specific commits with timestamps inside the synthesis window.

## Risk Assessment

- **If APPROVED-WITH-WARNINGS ships as-is:** the only downstream cost is a reader citing 4,387 for `http_read.rs` when it is now 4,765 — immaterial to the "split it / auth-buried" conclusion. No ticket, no architecture decision, no correctness claim turns on it.
- **No hallucinated-ticket risk** — the highest-value defect class is clean. A handover that pointed an architect at a non-existent ticket would have been BLOCK-worthy; that did not occur.
- **No reintroduced-error risk** — every catalog correction propagated correctly; the docs do not relitigate the ADR-013 strawman or the miscounts.

## Information Gaps

- I did **not** read the 8 `temp/catalog-*.md` partials end-to-end; for Check 3 I therefore applied "invented = contradicts source," verifying falsifiable numbers against source rather than flagging on absence-from-02 alone.
- HTTP wire conformance and the federation fixture suite were not executed (consistent with the docs' own stated limitation).
- SQLite table count (13+FTS5+view) was taken from the already-validated catalog, not re-counted from migrations here.

## Caveats

- This validates *faithfulness and consistency*, not the *technical soundness* of the drift findings themselves (that was the prior `validation-catalog.md` pass, which I treated as authoritative).
- The deferred-vs-abandoned ruling for D3/D4 is correctly surfaced **by the docs** as an architect decision (06 §2); it is not a validation defect and is not raised as one.
- "Working tree moved under the analysis" is an inherent hazard of analyzing an actively-committed branch; the docs already note (06 §8) uncommitted edits at session start. The http_read growth is the same class of hazard and warrants the same one-line caveat.

## Recommended (non-blocking) fixes

1. Add an errata line (04 §3.3 / §5 / 05 metrics / 06 §5): `http_read.rs` was 4,387 LOC at the read snapshot (HEAD~2 `a57760f`); two mid-session Wardline-T3.4 commits grew it to **4,765**. Conclusions unchanged.
2. 05 metrics: change "Unsafe blocks: 1 (documented)" → "2 (1 in supervisor `host.rs`, 1 in test-only fixture; both SAFETY-commented)" — or qualify as "1 in the supervisor."
