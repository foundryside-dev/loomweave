# Clarion v0.1 — Executive Review Synthesis

**Panel date:** 2026-04-17
**Synthesiser:** Plan Review Synthesiser Agent
**Inputs:** 10 specialist reviews (structure, doc-critique, editorial, self-sufficiency, link-integrity, architecture-critique, ADR review, debt catalog, STRIDE threat model, doctrine reader-panel)
**Target artefact:** `/home/john/clarion/docs/` — 5,670 lines across 17 markdown files, docs-only pre-implementation
**Decision this review gates:** design-complete → begin Clarion v0.1 implementation

---

## 1. Headline verdict

**Conditional-go with rework gates.** The docset is unusually disciplined — stable IDs, explicit non-goals, layered derivation, honest ADRs — and the three-layer design is fundamentally sound enough to build from. But Clarion v0.1 as currently scoped is roughly six products in one release, resting on eight unwritten P0 ADRs, an unbenchmarked $15 cost model, a cross-repo Filigree dependency that does not exist yet, and a default-off HTTP auth posture that a security tool cannot ship. Implementation can start on the catalog + Python plugin + SQLite writer core; everything touching Filigree `registry_backend`, the Wardline→Filigree bridge, and the summary-cache cost model should be gated behind the rework below.

---

## 2. The decision-maker's punch list

Ten items, ranked by convergent severity × reversibility. "Convergence" counts distinct reviewers who surfaced the same issue from different analytical angles — the strongest signal in a multi-panel review.

| # | Issue | Convergence | Cost | Blocking? |
|---|---|---|---|---|
| **1** | **Author the 8 P0 ADRs** (006 clustering, 007 cache key, 011 writer-actor, 013 secret scanner, 014 registry backend, 015 Wardline emission, 016 observation transport, 017 severity/dedup, 018 identity reconciliation) as standalone files. Plus one missing ADR: core/plugin ontology boundary. | 4 reviewers: `07-adr-review.md`, `08-debt.md` C1, `04-self-sufficiency.md` Issue 3, `02-doc-critique.md` (dual-table hazard) | 3–5 days | **YES** — first implementer will litigate each in PR threads otherwise |
| **2** | **Resolve the Filigree `registry_backend` dependency.** Either author the Filigree-side `RegistryProtocol` design jointly with Filigree maintainers, or drop the "Clarion owns structural truth" framing and document shadow-registry mode as the v0.1 shape. | 3 reviewers: `06-architecture-critique.md` §1+§6, `08-debt.md` C2, `11-doctrine-panel-synthesis.md` 2.7 + Dev persona | 1–2 days to rewrite, 1–2 weeks if Filigree patch is in scope | **YES** — the headline product claim rests on it |
| **3** | **Flip HTTP API default from `auth: none` to authenticated-or-not-listening** (Unix domain socket mode 0600, or auto-minted token on first `clarion serve`). A security tool cannot ship with loopback-is-trusted as the default posture. | 1 reviewer primary (`09-threat-model.md` T-02, risk score 9), but reinforced by `06-architecture-critique.md` §7 observability gap | 2–4 hours design change, small implementation | **YES** — reputational blocker on first public demo |
| **4** | **Write ADR on the Wardline→Filigree bridge (ADR-015, ADR-017)** and decide whether Clarion is the permanent bridge or a temporary one with a retirement condition on Wardline's native emitter. | 3 reviewers: `06-architecture-critique.md` §5, `08-debt.md` C5, `11-doctrine-panel-synthesis.md` 2.1 (the "triangle") | 1 day | **YES** — per `loom.md` §4 pairwise rule this is a doctrine violation without a written retirement condition |
| **5** | **Benchmark or downgrade the cost model.** `$15 ± 50%` (NFR-COST-01), ≥95% cache hit rate (NFR-COST-02), ≤60 min wall-clock (NFR-PERF-01) are currently speculation treated as acceptance criteria. Run a throw-away spike on `elspeth-slice`, or mark each as "hypothesis, to be validated before GA." | 2 reviewers: `06-architecture-critique.md` §6, `08-debt.md` C4 + M9 | 2–3 days (spike) or 30 min (downgrade language) | Should-fix; blocks only if these stay as contractual acceptance gates |
| **6** | **Core-enforced plugin boundary**: path jail (refuse plugin-returned paths outside project root), per-run entity-count cap, per-message Content-Length ceiling, per-plugin RSS limit via `ulimit` on spawn. Turn "trusted plugin" from doctrine into enforced minimum. | 2 reviewers: `09-threat-model.md` T-01/T-08/T-11/T-12 (compound critical), `06-architecture-critique.md` §3 | 1–2 days design, 2–3 days implementation | **YES** — mitigates three critical risk-9/risk-6 threats |
| **7** | **Broaden `loom.md` §5 failure test or explicitly scope it.** Current test ("does removing a sibling change the *meaning* of the remaining product's own data") catches semantic drift but not initialization coupling (the `wardline.core.registry.REGISTRY` direct Python import) or pipeline-stage dependencies (the SARIF triangle). Add ~200 words in §5 naming these v0.1 asterisks with retirement conditions. | 4 reviewers in effect: `11-doctrine-panel-synthesis.md` 2.2 + 2.3 (4 of 5 personas), `06-architecture-critique.md` §8, `04-self-sufficiency.md` §4 friction point | 2–3 hours | **YES** for doctrine credibility; the enrichment axiom is the suite's load-bearing claim |
| **8** | **Collapse the dual ADR tables** (system-design §12 + detailed-design §11). The docset itself admits they must be kept in sync manually — this is the warning, not the remedy. Pick one canonical home and cross-reference. While there, fix the "ADR-005 through ADR-013" phrase (`05-link-integrity.md` MEDIUM defect): both documents undercount by 7 entries. | 3 reviewers: `02-doc-critique.md` must-fix #1, `05-link-integrity.md`, `04-self-sufficiency.md` Issue 3 | 1 hour | Should-fix; this is the highest-probability silent-drift hazard in the docset |
| **9** | **Complete the Phase-7 rule-ID catalogue** in detailed-design §5. Preamble promises exhaustive coverage; actual §5 lists 3 rules while 20+ `CLA-*` IDs are scattered across all three documents. Add §5.1 with rule ID, phase, severity, kind, description. Fix the `# ... etc.` stub at detailed-design.md:128 while there. Reconcile `CLA-INFRA-PARSE-ERROR` vs `CLA-PY-PARSE-ERROR` namespace inconsistency (`04-self-sufficiency.md` Issue 7). | 2 reviewers: `02-doc-critique.md` detailed-design must-fix #1+#3, `04-self-sufficiency.md` Issue 7 | 3–4 hours | Should-fix; implementer of the finding emission layer currently has to grep three documents |
| **10** | **Inline a 15-term mini-glossary in both requirements.md and system-design.md** (finding, briefing, entity, edge, scan_run_id, guidance_fingerprint, knowledge_basis, tier, manifest, run_id, scope lens, writer-actor, pre-ingest redaction, capability probe, EntityId). Resolves the "glossary is a pointer-only stub" defect in both upper layers. | 3 reviewers: `02-doc-critique.md` system-design must-fix #2, `04-self-sufficiency.md` Issue 2, `01-structure.md` §7 | 1 hour | Nice-to-have; reduces cross-layer bouncing for every subsequent reader |

Items 1–4 and 6–7 are the blocking set. Items 5, 8, 9, 10 are should-fix but not blocking. Total estimated rework: **6–9 days of focused writing** if cost-model validation runs in parallel; **2–3 weeks** if it blocks.

---

## 3. Cross-cutting themes

Five themes surface in three or more reviews from independent angles. Each is more important than any single finding it comprises.

### 3.1 The Wardline → Clarion → Filigree triangle

`06-architecture-critique.md` §1+§5, `09-threat-model.md` TB-5+TB-6, and `11-doctrine-panel-synthesis.md` findings 2.1+2.2 (four of five reader personas) converge on the same shape: in v0.1, Wardline findings reach Filigree *only* through Clarion's SARIF translator, and Clarion's Python plugin *directly imports* `wardline.core.registry.REGISTRY` at startup. The first is a pipeline dependency that makes the Wardline+Filigree pair no longer pairwise-composable. The second is categorically tighter than the HTTP/file couplings it sits next to in the briefing's data-flow table. The architecture critic reads it as over-scope; the threat modeller reads it as a supply-chain blast-radius multiplier; the reader panel reads it as a doctrine violation that the §5 failure test is worded too narrowly to catch. All three are correct and describe the same structural fact.

### 3.2 Unauthored P0 ADRs = pre-implementation debt

`07-adr-review.md`, `08-debt.md` C1, and `04-self-sufficiency.md` Issue 3 independently flag that eight of the sixteen backlog ADRs are P0 and load-bearing, yet exist as one-sentence summaries inside mirrored tables in two documents. The ADR reviewer calls them "decided at one-sentence resolution"; the debt cataloguer prices them as "very high interest rate"; the self-sufficiency reviewer notes system-design is the only current home for P0 decisions like ADR-014 through ADR-018 and that one-sentence rows are under-powered for P0 rigour. These are the same finding seen three ways. The dual-table structure compounds the problem: every ADR change must land in two places.

### 3.3 Scope over-ambition at the v0.1 boundary

`06-architecture-critique.md` §1 (explicit: "Clarion v0.1 is trying to ship roughly six products in one release"), `08-debt.md` pattern observation ("debt estimates assume stated v0.1 scope; if scope contracts, several major items drop to minor"), and the recurring acknowledgement across `07-adr-review.md` and `09-threat-model.md` that v0.1 is carrying infrastructure (registry-backend displacement, SARIF bridge, MCP server, HTTP auth, Wardline-derived guidance) that any of its siblings might reasonably own. The architecture critic's realistic-minimum ("catalog + Python plugin + SQLite + local finding writer") is the same scope envelope the debt cataloguer arrives at by cost-accounting and the threat modeller arrives at by risk-surface minimisation.

### 3.4 Glossary / vocabulary drift across layers

`04-self-sufficiency.md` Issues 4+6+7, `02-doc-critique.md` system-design issue "Glossary stub", and `03-editorial.md` §3 each flag a different face of the same problem: the docset has no standalone glossary; briefing token bounds differ between layers (≤100/400/1500/3600 in requirements vs ~60/300/900/1800 in system-design and detailed-design); `knowledge_basis` casing drifts between snake_case on the wire and CamelCase in Rust types; `CLA-INFRA-PARSE-ERROR` and `CLA-PY-PARSE-ERROR` are used interchangeably in violation of the namespace contract in REQ-FINDING-02. Each drift is individually minor. As a class, they mean a plugin author reading only requirements.md implements the wrong shape.

### 3.5 Cost-model fiction

`06-architecture-critique.md` §6 (the "load-bearing assumptions" table) and `08-debt.md` C4 + M9 converge explicitly. $15 ± 50% per run, ≥95% cache-hit-after-stabilisation, ≤60 min wall-clock, four-segment prompt caching, and ±50% preflight estimator are all speculation currently styled as acceptance criteria. The architecture critic ranks "first real user sees $40+" as the most likely reason an early customer abandons the tool; the debt cataloguer ranks C4 + M9 as the pair that together make the whole cost story fiction. A half-day spike against elspeth-slice would displace three speculative numbers simultaneously.

---

## 4. Where reviewers disagreed

The panel is unusually coherent — most divergences are one reviewer seeing further than another, not contradicting. The real disagreements:

**Scope verdict.** `06-architecture-critique.md` concludes scope is over-ambitious and recommends cutting registry displacement, SARIF translator, MCP consult, HTTP auth, and Wardline-derived guidance to v0.2. `11-doctrine-panel-synthesis.md` finds the *doctrine* sound and well-defended — and §9 explicitly states the documents are "doing their job" for the adopter persona. The tension is real but resolves cleanly: the doctrine is sound; the v0.1 implementation scope cited in `briefing.md` exceeds what the doctrine's failure test can cleanly validate. Both findings stand.

**ADR-001 (Rust) severity.** `07-adr-review.md` verdict is "accept-with-amendments" (Rust genuinely fits, the author-directive framing is honest). `06-architecture-critique.md` calls it "20 lines of alternatives analysis" for the load-bearing language choice and flags resume-driven-design smell as borderline. Resolution: the ADR reviewer is scoped to decision-record rigour, the architecture critic to engineering weight-of-evidence. The right action covers both — amend ADR-001 to cite the actual requirements (single-binary distribution, subprocess supervision, SQLite ergonomics) and drop the "not subject to alternatives analysis" phrase in system-design §12, which the ADR reviewer and architecture critic both name as the defensive slip.

**Direct-import coupling severity.** Reader panel persona Marcus (p2) did not flag the `wardline.core.registry.REGISTRY` direct import; four of five other personas did. This is consistent with Marcus's declared blind spot (decision-maker, not data-flow tracker) — not a genuine disagreement, but recorded here because the control persona's silence should not be read as absolution.

**"Auth: none" default.** `09-threat-model.md` rates it a critical risk-9. `06-architecture-critique.md` observability gap notes it in passing without flagging as blocking. `07-adr-review.md` does not touch it (no ADR yet exists). The threat model is the specialist domain here; its priority should prevail.

No reviewer directly contradicted another. Conflicts are scope conflicts, not fact conflicts.

---

## 5. What's strong (don't bury this)

Every review surfaced something load-bearing that the docset does well. Enumerated so the edit pass doesn't erode them:

- **Stable ID discipline is real.** Every requirement carries a stable ID, a rationale, a verification method, a `See:` pointer. The `Addresses:` / `See:` symmetry is the right mechanism; `04-self-sufficiency.md` §8 finding 1.
- **Explicit non-goals with deferral IDs (NG-01 through NG-25).** Each non-goal is specific and traceable, not vague future-work. `04-self-sufficiency.md` §8 finding 4 names NG-14 (rename tracking), NG-17 (triage-feedback loop), NG-25 (annotation descriptor) as exemplary.
- **Revision-history appendix (detailed-design Appendix D).** The Rev 2→3→4→5 change tables let any reader reconstruct why a decision has its current shape. `04-self-sufficiency.md` §8 finding 3 calls it "rare in design docs."
- **§5 enrichment / failure-test formulation in `loom.md`.** All five reader personas accepted the central thesis as sincere and precisely expressed; adversarial readers (Priya, Sam, Dev) each explicitly credited the author for naming the stealth-monolith failure mode rather than hand-waving past it. `11-doctrine-panel-synthesis.md` §11.
- **Navigation spine post-restructure (commit dfb9d95).** All five READMEs load-bearing; four personas find the right document in <10 seconds. `01-structure.md` rates findability 4.5/5.
- **Link graph integrity.** 67 relative links verified, zero broken. One real defect (the "ADR-005 through ADR-013" undercount). `05-link-integrity.md`.
- **ADR internal consistency.** The four authored ADRs are coherent as a set; `Related Decisions` links are reciprocal; ADR-002 correctly inherits Rust and isolates the plugin boundary so an ADR-001 reversal wouldn't cascade. `07-adr-review.md` opening.
- **ADR-004 (Filigree-native intake) is genuinely evidence-forced.** The only ADR in the set driven by external reality (integration-recon), not preference; alternatives are real, both honestly rejected. `07-adr-review.md`.
- **Loom axiom is cited, not decorative.** CON-LOOM-01 appears in integration requirements, references in system-design §1+§11, drives degraded modes (NFR-RELIABILITY-02). `04-self-sufficiency.md` §4.
- **Honest register discipline across layers.** Doctrine → orientation → requirements → design → implementation-reference; each document declares its audience and stays in lane. `03-editorial.md` overall verdict.

These are not consolation prizes. Several are rare enough that losing them during the rework would be a regression.

---

## 6. Three questions the author must answer before writing code

The panel cannot decide these for you. They are commitments, not reviews.

**Q1. Is v0.1 the catalog + Python plugin + SQLite + local finding writer, or is it the full triangle-integrating fabric the briefing describes?**
Every critical debt item (`08-debt.md` C1–C5), the architecture critic's scope verdict, and the doctrine panel's triangle concern reduce to this choice. Picking the smaller scope makes items 4, 5, and much of 1 non-blocking. Picking the larger scope means several acceptance criteria currently stand on speculation. You cannot defer this: every downstream ADR depends on it.

**Q2. Is the Filigree `registry_backend` flag a Clarion v0.1 commitment or a v0.2 aspiration?**
This is the load-bearing pairwise-composability question (`loom.md` §4) and the integration-recon gap that kept recurring across `06`, `08`, and the doctrine panel. If v0.1, you need Filigree-maintainer buy-in and a cross-repo PR plan; if v0.2, the briefing's "Clarion owns structural truth" language needs to be rewritten as "Clarion shadows the file mapping until the Filigree flag lands," and the doctrine documents need to name the deferral honestly.

**Q3. What is the authority model for plugins?**
There is no ADR for the core/plugin ontology boundary (`07-adr-review.md`'s highest-priority missing ADR) and no ADR for plugin failure/degraded-mode semantics. There is also no signed-manifest or hash-pin story — `09-threat-model.md` ranks this the compound-critical risk — and no core-enforced path jail or resource cap. Answer: is the plugin a trusted extension that declares its own capabilities, or an untrusted input that the core validates? The v0.1 design currently assumes the former while shipping to an audience that will assume the latter.

---

## 7. What this review does not cover

Honesty clause. The panel reviewed the documents. The panel did not:

- **Run any code.** No prototype exists; every cost, latency, cache-hit, and cluster-quality number in the docset is unvalidated.
- **Test the $15/run assumption against real LLM spend.** The ±50% envelope is the design's own estimate from a hypothetical run on its own example codebase.
- **Stress-test SQLite concurrency** under the writer-actor + 16-reader pool + commit-DB workload that `06-architecture-critique.md` §6 names as the highest-rewrite-cost assumption.
- **Validate the Filigree `registry_backend` proposal with Filigree maintainers.** `08-debt.md` C2 flags this explicitly — the docset proposes a Filigree-side change with no Filigree-side owner or schedule.
- **Audit third-party dependencies.** `09-threat-model.md` §6 notes Rust deps (`tokio`, `rusqlite`, `reqwest`-family, `tree-sitter`), Python deps (libcst, detect-secrets, Wardline), and the Anthropic SDK are inherited at the class-of-risk level, not at specific-CVE level.
- **Read every backlog ADR in full.** `07-adr-review.md` notes backlog triage is MEDIUM confidence; specific backlog-ADR defects may still lurk.
- **Cover detailed-design exhaustively.** `06-architecture-critique.md` and `08-debt.md` both note detailed-design was sampled by targeted grep, not read end-to-end. Thresholds or contradictions buried in uncommented code blocks may not be surfaced.
- **Review Wardline or Filigree internals** beyond what `integration-recon.md` already captured. Claims like "Filigree has no HTTP `/api/v1/observations` endpoint today" are inherited from the recon, not re-verified against Filigree HEAD.
- **Fully exercise the reader-panel personas.** `11-doctrine-panel-synthesis.md` flags two deliberately unfilled personas (elspeth team member, new-grad contributor onboarder). The elspeth gap is the most consequential — `NFR-SCALE-01` rests entirely on elspeth's shape.

The panel's most robust findings are structural (derivation integrity, doctrine coherence, ADR rigour, link graph). The weakest are quantitative (any number with "±", "≥", "≤"). Treat the findings accordingly.

---

## 8. Recommendation

**Conditional-go.** Author the eight P0 ADRs (item 1), pick a side on Q1 + Q2 + Q3 (§6 above), and close the four blocking items (1, 2, 3, 6 from the punch list) before the first `git commit` of Rust code. Items 4, 7, 8, 9, 10 can land in parallel with early implementation. Items 5 and the cost-model validation should run as a throw-away spike against elspeth-slice in the first sprint — do not freeze `NFR-COST-*` until that spike reports. The docset is strong enough to start from; what would be a mistake is treating "docs are complete" as "design is frozen" and carrying the speculative numbers and unauthored ADRs into code. The very specific next action: **schedule a 2-day writing sprint to author ADR-006, -007, -011, -013, -014, -015, -016, -017, -018 and the new core/plugin ontology ADR**, and in that sprint answer Q1 and Q2 in the first hour — every other decision depends on them. If Q2 forces a cross-repo Filigree patch, open that conversation with Filigree maintainers *now*, not when implementation blocks.

---

*Synthesis drawn from: `01-structure.md`, `02-doc-critique.md`, `03-editorial.md`, `04-self-sufficiency.md`, `05-link-integrity.md`, `06-architecture-critique.md`, `07-adr-review.md`, `08-debt.md`, `09-threat-model.md`, `11-doctrine-panel-synthesis.md`. Approx 2,350 words.*
