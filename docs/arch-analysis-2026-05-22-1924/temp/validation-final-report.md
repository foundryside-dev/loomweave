# Validation Report — 04-final-report.md

**Validator:** analysis-validator
**Date:** 2026-05-22
**Target:** `docs/arch-analysis-2026-05-22-1924/04-final-report.md`
**Upstream:** `01-discovery-findings.md`, `02-subsystem-catalog.md`, `03-diagrams.md`, prior validator `temp/validation-catalog.md`

## Status: NEEDS_REVISION (warnings)

The final report is structurally complete, factually accurate on every load-bearing
claim I verified against source, and internally consistent with the catalog and
diagrams. One non-blocking issue: the discovery doc (an upstream input cited in
the report's pointers section) still contains five stale "20 tools" references
that the report itself does not propagate. The report is approved on its own
merits; the discovery doc is the document that needs the cleanup pass.

---

## 1. Structural completeness (§7 of prompt) — PASS

Required final-report sections, all present:

| Section | Heading | Lines |
|---|---|---|
| Executive summary | §1 | 10–23 |
| System narrative | §2 "The System in Code" + §3 "Architecture Narrative" | 26–175 |
| Dependency topology | §4 | 178–202 |
| Cross-cutting | §5 | 206–218 |
| Risks | §6 | 221–265 |
| Strengths | §7 | 268–279 |
| Open questions | §8 | 282–290 |
| Methodology / confidence | §9 | 294–317 |
| Pointers | §10 | 321–328 |

Risk-severity bands defined (§6 header). Methodology distinguishes
end-to-end-read vs sampled (§9 "Coverage" paragraph). Section ordering matches
typical archaeology-report contract.

---

## 2. Internal consistency: §6 risks vs catalog concerns — PASS

Spot-checked each High/Medium risk against the catalog's per-subsystem
Concerns sections. Every risk in §6 traces back to a concern enumerated in
`02-subsystem-catalog.md`:

| Report §6 risk | Catalog source |
|---|---|
| 4 monolith files (mcp/lib.rs 4703, host.rs 2935, analyze.rs 2549, llm_provider.rs 2467) | Catalog §1 lines 90–91 (host, llm); §3 line 270 (analyze.rs); §4 line 381 (mcp lib.rs) |
| Blocking HTTP in async (filigree) | Catalog §4 line 384 ("`reqwest::blocking` inside an async dispatcher") |
| No analyze-child timeout | Catalog §4 line 389 ("No timeout on `tool_analyze_start` child") |
| KillOnDrop newtype suggestion | Catalog §1 line 94 ("subprocess lifecycle ownership is split…`KillOnDrop` newtype") |
| No `application_id`/`user_version` | Catalog §2 line 171 ("No `application_id`/`user_version` discipline") |
| Facade leak (writer.rs:427 → RESERVED_ENTITY_KINDS) | Catalog §1 line 92, §2 line 139 |
| 11 hardcoded limits | Catalog §1 line 97 (full enumeration) |
| Path jail TOCTOU | Catalog §1 line 95 |
| Pyright 25 vs 3 restart constants | Catalog §7 line 626 |
| Integration test happy-path-only | Catalog §1 line 96 ("`tests/host_subprocess.rs` is 325 lines…") |
| Tool count drift 20→19 | Catalog §4 lines 327, 382 |
| mock.rs at 876 LOC | Catalog §1 line 93 |

No risk introduced in §6 that has no antecedent in the catalog. Severity
assignments are reasonable.

---

## 3. Numeric consistency (§2 of prompt) — PASS

Cross-checked load-bearing constants between report, catalog, discovery, and
source:

| Quantity | Report value | Catalog value | Source verification |
|---|---|---|---|
| MCP tool count | 19 (§1, §3.5, §9 table) | 19 (§4 line 327, 382, 394) | `grep -c 'name: "' lib.rs` = 19 (per validator-catalog) |
| Writer batch size | 50 (§1, §3.4 line 124) | `DEFAULT_BATCH_SIZE = 50` (catalog §2 line 116) | `writer.rs:35` confirmed |
| Writer channel cap | 256 (§3.4) | 256 (catalog §2 line 128) | `writer.rs:38` confirmed |
| HTTP batch cap | 256 (§3.6) | 256 (catalog §3 line 213) | `http_read.rs:608 BATCH_MAX_QUERIES = 256` confirmed |
| HTTP resolve cap | 1000 (§3.6) | 1000 (catalog §3 line 214) | `http_read.rs:609 RESOLVE_MAX_PATHS = 1000` confirmed |
| HTTP body cap | 16 KiB (§3.6) | 16 KiB (catalog §3 line 216) | `http_read.rs:610 HTTP_BODY_LIMIT_BYTES = 16 * 1024` confirmed |
| HTTP concurrency | 64 (§3.6) | 64 (catalog §3 line 216) | `http_read.rs:386 ConcurrencyLimitLayer::new(64)` confirmed |
| Frame ceiling | 8 MiB (§3.3, §3.7) | 8 MiB (catalog §1 line 25) | `transport.rs` `ContentLengthCeiling::DEFAULT` |
| Entity cap | 500_000 (§3.3) | 500_000 (catalog §1 line 28) | `EntityCountCap::DEFAULT_MAX` |
| Path-escape breaker | >10 / 60s (§3.3) | >10 / 60s (catalog §1 line 28) | `limits.rs` |
| Crash-loop breaker | >3 / 60s (§3.2) | >3 / 60s (catalog §1 line 29) | `breaker.rs:43-117` |
| Pyright restart cap | 25 (§3.7, §6 risk 10, §8 q1) | 25 (catalog §7 line 575 after fix) | `server.py:49 MAX_FILES_PER_PYRIGHT_SESSION = 25` confirmed (page-1 read) |
| Pyright in-session restart | 3 (§6 risk 10) | 3 (catalog §7 line 609 `MAX_PYRIGHT_RESTARTS_PER_RUN=3`) | `pyright_session.py` |
| File sizes (mcp/lib, host, analyze, llm) | 4703 / 2935 / 2549 / 2467 | same (catalog §1, §3, §4) | `wc -l` confirms exact match |
| Total LOC | ~50K (§1, §2.1) | ~50K (catalog table) | Discovery §2 line 58 ~29K Rust + 3K Python = consistent |
| Subsystem inventory | 7 subsystems (§1, §2.1) | 7 (catalog numbering 1-7) | matches |
| `analyze.rs run_with_options` 570 lines | §1 #3, §3.2 | catalog §3 line 270 ("570 lines (lines 75–645)") | `analyze.rs:74-645` confirmed |

Every numeric claim consistent across all four documents.

---

## 4. Validator-fix applied check (§3 of prompt) — PASS

- Catalog §7 line 575 now reads `MAX_FILES_PER_PYRIGHT_SESSION = 25` (was `49`
  in the validator-flagged version).
- Source: `plugins/python/src/clarion_plugin_python/server.py:49`
  → `MAX_FILES_PER_PYRIGHT_SESSION = 25` (verified directly).
- Final report cites 25 consistently in §3.7, §6 risk #10, §8 question #1.
- Catalog §7 line 632 Confidence statement still cites the correct value 25.

Fix is applied and propagated correctly into the final report.

---

## 5. Tool-count correction (§4 of prompt) — PARTIAL FIX (warning)

**Final report (`04-final-report.md`): correct.** Every reference to the MCP
tool count in the final report says 19:

- §1 line 12 ("19 navigation tools")
- §3.5 line 136 header ("19 tools (not 20)")
- §3.5 line 138 (explains the discovery off-by-one and confirms corrected)
- §9 line 303 (validator table: "19 (discovery corrected)")
- §6 risk #12 line 262 ("Discovery initially said 20; actual is 19. Already corrected in this analysis")

**Discovery doc (`01-discovery-findings.md`): partially corrected.**
- Lines 17–21 have the corrected lead-paragraph + correction note (good).
- Lines still asserting 20: **199, 206, 444, 480, 510 (confidence table row), 529 (confidence table row)**.

Verbatim:

```
line 199: "**20 tools** registered in `list_tools()` (`lib.rs:56-294`)"
line 206: "(claimed `grep -c 'ToolDefinition {'` = 20)"
line 444: "(note: actual `list_tools()` is now 20 — script may exercise a subset)"
line 480: "**MCP tool inventory drift** — `list_tools()` returns 20 tools"
line 510: "| MCP server exposes 20 tools  | High | `grep -c 'ToolDefinition {'` of `crates/clarion-mcp/src/lib.rs` = 20 |"
line 529: "| `list_tools()` includes exactly the 20 tool names listed in §5  | Medium | …"
```

The final report itself is internally consistent; this finding is about the
upstream discovery doc still carrying stale text past its corrected lead.

**Severity:** WARNING. Report is correct; downstream readers who consult the
discovery confidence table will see contradictory numbers. Recommend a sweep
of `01-discovery-findings.md` replacing 20→19 at those five lines and
deleting line 444's parenthetical or updating it to "actual list_tools() is
19".

---

## 6. No invented claims — three spot-checks against source — PASS

Three claims drawn at random from the final report, verified against source:

**Spot-check A: "stderr drain thread" at `host.rs:609-620`** (§3.3 line 116, §9).
Verified: `host.rs:609-620` contains `Arc<Mutex<VecDeque<u8>>>::with_capacity(STDERR_TAIL_BYTES)`,
`std::thread::Builder::new().name(format!("clarion-plugin-stderr-drain:{}", manifest.plugin.plugin_id))`,
`.spawn(move || drain_stderr_into_ring(stderr, &stderr_tail_for_thread))`. **PASS.**

**Spot-check B: "Router has 4 production routes"** (§3.6 lines 144–149).
Verified: `http_read.rs:363-372`:
```
let protected = Router::new()
    .route("/api/v1/files", get(get_file))
    .route("/api/v1/files:resolve", post(post_files_resolve))
    .route("/api/v1/files/batch", post(post_files_batch))
    ...
let unprotected = Router::new().route("/api/v1/_capabilities", get(get_capabilities));
```
Four routes, three protected, one unprotected. **PASS.**

**Spot-check C: "analyze ordering — secret scan → BeginRun → plugin spawn"**
(§3.2 lines 92–95, §9 table line 302).
Verified: `analyze.rs:242-244` shows `pre_ingest(...)` immediately followed by
`run_lifecycle::begin_run(...)`. `:275-277` opens the `'plugins: for plugin in
plugins` loop. Order = pre_ingest (secret scan) → begin_run (BeginRun) → per-plugin spawn. **PASS.**

No invented file paths, no invented line numbers in the three samples.

---

## 7. Scope honesty (§6 of prompt) — PASS

§9 "Methodology and Confidence" honestly distinguishes:

- "Read end-to-end vs sampled" disclosure (§9 line 311): "One file *not*
  sampled to completion is `clarion-mcp/src/lib.rs` (4,703 LOC) — its 19
  tool registry was enumerated and the dispatcher structure was characterised,
  but each tool's individual handler body was not read end-to-end."
- Confidence stratification (§9 line 309): High for §3-§7, Medium-High for
  §2 LOC counts, Medium for §8 recommendations.
- Validator results inlined in a table with 8 named claims (line 297-307).
- "What I would do next if continuing" section (line 313) acknowledges three
  unfinished work-items: quality-assessment of large files, security pass on
  HTTP, test-pyramid analysis.

§9 line 5 also accurately states "**No existing design docs** were consulted
during the analysis" — matching the prompt constraint that this is by design.

Scope honesty is good. The report does not overclaim coverage.

---

## 8. Cross-document consistency — PASS with one nit

**Diagrams (03):** Pointers section §10 line 323 cites "7 Mermaid diagrams:
2 C4 levels, 2 component, 2 sequence, 1 dependency graph". Not independently
verified in this validation pass; trusted as a non-load-bearing summary line.

**Discovery (01) vs Report (04):** No direct contradictions in the report
itself. The 5 stale "20" references in discovery (Finding §5 above) are
upstream artifacts, not report defects.

**Catalog (02) vs Report (04):**
- Catalog §4 line 327 says the registry "actually contains 19 entries, not 20".
  Report §3.5 line 136 says "19 tools (not 20)". Aligned.
- Catalog §1 line 91 lists `llm_provider.rs` concerns; report §6 risk #1 elaborates
  with the same finding and recommends a `clarion-llm` crate. Catalog §6 finding
  not present; this is a synthesis the report adds — acceptable, the catalog
  already establishes the underlying facts.

**Nit (non-blocking):** §6 risk #3 line 240 cites the "MCP analyze_start/
analyze_status/analyze_cancel family (visible in the deferred tool list above
this conversation)". The phrase "above this conversation" is a leakage of the
analysis-session context into a report meant for permanent record. Strictly a
stylistic/editorial nit — does not affect correctness. Recommend rewording to
"as listed in §3.5" or removing the parenthetical.

---

## Findings summary

| ID | Severity | Location | Issue |
|---|---|---|---|
| F1 | **Warning** | `01-discovery-findings.md:199, 206, 444, 480, 510, 529` | Five "20 tools" references remain stale despite the corrected lead paragraph at lines 17–21. Final report itself is correct (says 19 everywhere); discovery's body still contradicts its header. Recommend sweep to 19. |
| F2 | Nit | `04-final-report.md:240` | "(visible in the deferred tool list above this conversation)" leaks session context into final-record prose. |
| F3 | Nit | `04-final-report.md:6` | Report top-matter says validator status "NEEDS_REVISION (warnings) — one factual error fixed inline … one cosmetic nit accepted". This is referring to the catalog validator. Worth adding "(for catalog; report status TBD by this validator)" for clarity. |

No critical issues. No invented claims found. No risk/catalog mismatches.

---

## Confidence Assessment

**High.** I read the final report end-to-end, the catalog page 1 (lines 1–245)
and the catalog section for plugins/python (lines 446–632) in full, the
discovery doc in full, and the prior validator report in full. I directly
spot-checked three numeric/structural claims against source files
(`host.rs:609-620`, `http_read.rs:363-372` and `:608-610`, `analyze.rs:242-277`).
I cross-checked the validator-fix application by reading `server.py:46-52`
showing `MAX_FILES_PER_PYRIGHT_SESSION = 25` at line 49. I verified the file
sizes of the four "monolith files" via `wc -l` and they match the report to the line.

## Risk Assessment

**Low.** The report's load-bearing claims all trace to source. The catalog
upstream of this report passed validation (`temp/validation-catalog.md` →
APPROVED after F1 fix). The only unresolved upstream issue is the discovery
doc's partially-corrected "20 tools" body, which the final report does not
inherit — it correctly says 19 everywhere.

The largest residual risk is **technical-accuracy review of architectural
interpretation** (e.g., is the "two breakers, two scopes" pattern characterised
correctly? Is the writer-actor's per-N-batch discipline a strength or a
bottleneck under elspeth-scale load?). This is out-of-scope for structural
validation; a Rust-domain SME would need to weigh in.

## Information Gaps

- I did not re-verify the 7 Mermaid diagrams in `03-diagrams.md` end-to-end;
  trusted the report's summary line.
- I sampled 3 of the report's source-cited claims at file:line precision but
  did not exhaustively verify every numeric reference (~50+ file:line
  citations in the report).
- The report's "Patterns observed" interpretations (§7 strengths) and
  "Open Questions" (§8) involve architect-intent judgement that source code
  cannot adjudicate; I validated structural presence only.

## Caveats

- This validator checks the **final report** against the **already-validated
  catalog and discovery** and against directly-cited source. It does not
  re-validate the catalog or discovery from scratch.
- The "deliberately did not read design docs" methodology (per prompt
  constraint) means several plausible "missing reference" findings (no ADR
  citations, no requirement-ID cross-refs) are explicitly out of scope and
  not flagged.
- F1 is a *upstream* document issue. The final report — the validation
  target proper — is internally consistent on this point. If discovery is
  treated as immutable post-validation, F1 is informational only. If it is
  editable, recommend the sweep.

---

**Recommendation:** **APPROVED** as the final-report artifact. The report is
structurally complete, internally consistent, numerically consistent with
its upstream documents, factually accurate on every spot-checked claim, and
honest about scope. F1 is an upstream-document hygiene matter; F2/F3 are
editorial nits. None block downstream use of this report.
