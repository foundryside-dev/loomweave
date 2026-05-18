# Self-Sufficiency & Derivation Review — Clarion v0.1 Layered Design Set

Reviewer: muna-wiki-management:self-sufficiency-reviewer
Date: 2026-04-17

**Overall Rating:** CONDITIONAL PASS

The three-layer docset is unusually disciplined — each requirement carries a stable ID, a verification method, and a `See:` pointer, and each system-design section carries an `Addresses:` header. Forward traceability is strong and backward justification is almost complete. However, the `Addresses:` headers undercount: roughly 20% of requirements have no section claiming them, even when the content is present in prose. Detailed-design is mostly grounded but carries a handful of implementation details (dirty-tree policy, legacy alias handling, generated SQL columns) that do not anchor to a system-design mechanism. Terminology drift around briefing detail-level bounds and around the SARIF translator's role is the main consistency risk.

---

## 1. Forward traceability (requirements → system-design)

**Overall: strong, with gaps in the `Addresses:` headers, not the content.**

The following requirements are addressed in prose but **not listed in any `Addresses:` header**, meaning the docset's mechanical audit tool (if one were written against headers) would flag them as orphans:

- **REQ-BRIEFING-02** (controlled vocabulary for patterns/antipatterns) — Content lives in §10 (Prompt-injection containment, point 3) but §10's `Addresses:` lists only NFR-SEC-01..05 + REQ-CONFIG-05. REQ-BRIEFING-02's `See:` line points to "§3 (Data Model, Briefing), §10 (Security)" — neither claims it.
- **REQ-BRIEFING-06** (detail levels with token ceilings) — Discussed in §3 Entity Briefing and §8 "Token budgeting per response"; neither lists it in `Addresses:`.
- **REQ-FINDING-02** (namespaced rule IDs) — Handled in detailed-design §7 but no system-design section lists it. Its `See:` line points to "§9 (Integrations, Rule-ID round-trip)"; §9's `Addresses:` omits it.
- **NFR-OBSERV-01 / -02 / -03** (structured logs, stats.json, Prometheus metrics) — Referenced as "See: §5 (Policy Engine, Observability)" and "§9 (HTTP Read API)"; neither section lists them. §5 has a brief "Observability" paragraph that names stats.json but doesn't cover log rotation or the metrics endpoint catalog.
- **NFR-COMPAT-01 / -02 / -03** (Filigree schema pin, Wardline REGISTRY pin, Anthropic SDK pin) — §9 mentions schema pin in one paragraph; REGISTRY pin is in §2; SDK pin has no system-design home at all. None of the three are in any `Addresses:` header.
- **CON-RUST-01** — No `Addresses:` header claims it. §12 ADR-001 and §2 core/plugin split cover it, but the constraint itself is not mechanically traceable.
- **NG-01, NG-02, NG-04, NG-05, NG-06, NG-07** — Named in §1 prose ("What Clarion is NOT") but only NG-03 appears in an `Addresses:` header.

**Verdict:** Every requirement's *content* is addressed somewhere; the `Addresses:` headers are the load-bearing trace apparatus per the preamble and they under-list. This is a mechanical gap, not a design gap, but it matters because requirements.md says IDs are "load-bearing" and presumably a reverse-traceability query would miss these.

## 2. Backward justification (system-design → requirements)

**Overall: strong.** Each major mechanism traces cleanly.

Minor free-floating items:

- §2 Plugin packaging / pipx specifics: anchors to NFR-OPS-04 (listed). Fine.
- §3 Identity reconciliation — claims to address REQ-INTEG-WARDLINE-06 and does.
- §4 "Why not a single giant transaction" — a design rationale without a requirement; acceptable as explanatory.
- §4 Writer-actor vs. shadow-DB (ADR-011) — ADR only; no specific REQ. Acceptable since it's a decision, not a capability.
- §5 Prompt caching strategy (four segments) — traces to CON-ANTHROPIC-01; fine.
- §9 `metadata` nesting convention (`metadata.clarion.*`, `metadata.wardline_properties.*`) — traces loosely via REQ-FINDING-03 and CON-FILIGREE-01 but the specific nesting rule isn't in any requirement.
- §10 Threat-model table rows — "Personal API key charged when committing team DB" and "DB tampering" have no requirement backing. Defensible as threat-modeling practice, but the reader cannot verify they're in scope by ID lookup.

No major free-floating mechanisms.

## 3. Detail ↔ design (detailed-design → system-design)

**Overall: strong, with a few grounded-in-nothing details.**

Items in detailed-design that have no system-design anchor:

- §3 Commit-ref and dirty-tree handling (`-dirty` suffix, `CLA-INFRA-DIRTY-TREE-RUN`, `--require-clean`) — Not mentioned in system-design §4 or §6. REQ-CATALOG-07 (`first_seen_commit` / `last_seen_commit`) implies it peripherally but doesn't require the policy.
- §3 Generated SQL columns + views (`git_churn_count`, `priority`, `guidance_sheets` view) — An implementation choice with no system-design analogue.
- §3 `clarion.yaml:storage.commit_db: false` opt-out and `clarion db sync push/pull` — No system-design mechanism; NFR-OPS-03 says `.clarion/` is committable by default but does not describe the opt-out.
- §7 "How Wardline picks up the token in CI" (`CLARION_TOKEN` env, `clarion check-auth --from wardline`) — Operationally important; no system-design mechanism for it. §9 "Token auth" mentions the env var in passing but not `clarion check-auth`.
- §1 Plugin decorator-detection table rows (class-decoration, metaclass, `__init_subclass__`, dynamic dispatch refusals) — The specific `CLA-PY-ANNOTATION-AMBIGUOUS` rule ID is not named in system-design; REQ-PLUGIN-06 is generic about decorator detection.
- §9.1 "Three auto-create paths in Filigree" — named here with specific file references; system-design §9 / §11 mention them as "three auto-create paths" without enumeration.
- §10 Acceptance Ecosystem item 7 (`add_file_association` round-trip) — No system-design mechanism; a requirement might be missing here.

These are small and none are load-bearing, but they represent detail that a reader of system-design alone cannot anticipate.

## 4. Doctrine ↔ requirements (Loom axiom honoured?)

**Verdict: honoured cleanly.**

- **Solo-useful** — NFR-RELIABILITY-02 (`--no-filigree`, `--no-wardline`), CON-LOOM-01 explicit, NG-06 (no hosted service). Satisfied.
- **Pairwise-composable** — REQ-INTEG-FILIGREE-01..05 and REQ-INTEG-WARDLINE-01..06 are each independent one-to-one contracts. No "only works with all three" requirement exists.
- **Enrich-only** — CON-LOOM-01 names it explicitly; requirements never demand a sibling for Clarion's *own* data coherence. Wardline absence degrades to `confidence_basis: clarion_augmentation` (sem-preserving); Filigree absence degrades to local `findings.jsonl` (sem-preserving).

**One minor friction point:** REQ-INTEG-FILIGREE-03 (registry-backend consumption) and CON-FILIGREE-02 depend on Filigree shipping a flag. The shadow-registry fallback is documented and preserves the axiom. No violation, but it's the closest-to-load-bearing of any requirement and worth naming.

No doctrinal violations.

## 5. Self-sufficiency per layer

**requirements.md:** Stands alone for its audience (reviewer verifying scope, implementer checking obligations). One deferral worth naming: the Glossary section defers *entirely* to detailed-design Appendix B. For a reader using only requirements.md, terms like "finding", "briefing", "scan_run_id", and "guidance fingerprint" are used without definition. This is a **lazy deferral** — a 15-term mini-glossary in requirements.md would make it standalone.

**system-design.md:** Mostly self-sufficient. Two thin patches:

- §12 ADR summaries — Each row is one sentence of rationale. For P0 decisions this is under-powered: e.g., ADR-014 ("Filigree registry_backend flag") says "Four NOT-NULL foreign keys + three auto-create paths require a real interface" — a reader who doesn't know Filigree's schema cannot understand what is being asked. The row defers to detailed-design §9.1 implicitly.
- §12 also defers to authored ADR files for "context, decision, alternatives, consequences" — of 20 ADRs, only ADR-001..004 are "Accepted" (authored); the rest are "To author". System-design is the only current home for P0 decisions like ADR-014, -015, -016, -017, -018, and the one-line summaries are thinner than a P0 decision warrants.
- Glossary stub pointer — same lazy-deferral issue as requirements.md; the docset has *no* standalone glossary.

**detailed-design.md:** Stands alone as implementation reference. Appropriately heavy.

## 6. Consistency / terminology drift

**Notable drifts:**

1. **Briefing detail-level token bounds** differ across the three docs:
   - requirements.md REQ-BRIEFING-06: `short ≤100 / medium ≤400 / full ≤1,500 / exhaustive ≤3,600`
   - system-design §3: "`short` (~60 tokens), `medium` (~300), `full` (~900), `exhaustive` (~1,800)" plus qualifier "hard ceilings... are what the renderer must enforce"
   - system-design §8 Token budgeting: `summary(short) ≤100, (medium) ≤400, (full) ≤1,500`
   - detailed-design §2: `~60 / ~300 / ~900 / ~1,800`

   The gloss is "~N is typical; ≤N is ceiling" and system-design §3 *does* call this out. But the detailed-design table is unqualified, and a reader bouncing between them will see two distinct sets of numbers without immediate clarity on which is the contract.

2. **SARIF translator framing** — requirements.md REQ-FINDING-04 calls it a "general-purpose SARIF → Filigree translator" (permanent). System-design §9 calls it "general-purpose" and notes v0.2 adds a native Wardline POST. Detailed-design §7 calls the translator permanent but also calls the v0.1 Wardline path a "workaround" and v0.1 decision "Option A". Slight inconsistency about whether "Clarion-side ownership of Wardline-specific SARIF mapping" is a workaround or the v0.1 design.

3. **"scan_source" values** — system-design §9 lists "`clarion`, `wardline`, `cov`, `sec`" as suite-wide reserved. Detailed-design §7 lists same plus says "no registry" / "free-form". Requirements.md REQ-INTEG-FILIGREE-04 uses `scan_source: "clarion"`. Consistent, but the "reserved namespace" claim has no canonical source.

4. **Briefing `knowledge_basis` values** — system-design §3 mentions `StaticOnly / RuntimeInformed / HumanVerified` (CamelCase enum). Requirements.md REQ-BRIEFING-04 uses `static_only | runtime_informed | human_verified` (snake_case). Detailed-design §2 uses CamelCase Rust enum but the wire shape is unspecified. Minor, but a plugin author reading only requirements.md would implement the wrong casing.

5. **Python plugin error codes drift** — requirements.md REQ-ANALYZE-06 lists `CLA-PY-PARSE-ERROR, CLA-INFRA-PLUGIN-CRASH, CLA-INFRA-LLM-ERROR, CLA-INFRA-BUDGET-WARNING`. System-design §6 table uses the same. Detailed-design §10 "Error surfaces" lists `CLA-INFRA-PARSE-ERROR` (INFRA, not PY) among others — namespace inconsistency with REQ-FINDING-02 which reserves `CLA-PY-*` for Python-plugin structural findings.

6. **NG-25 vs system-design §2 "v0.2 generalisation"** — detailed-design §1 Observe-vs-enforce says "v0.2 adds `wardline annotations descriptor --format yaml`". NG-25 in requirements.md says the same. System-design §9 lists it as a Wardline-side v0.2 prerequisite. Consistent.

## 7. Issues summary

**Issue 1: Missing `Addresses:` headers for 8+ requirements**
- Location: System-design §3 (no REQ-BRIEFING-02 / -06), §5 (no NFR-OBSERV-*), §9 (no NFR-COMPAT-*), §1 (no NG-01/02/04/05/06/07 or CON-RUST-01).
- Fix: Add these REQ IDs to existing `Addresses:` headers. Mechanical change; content already there.

**Issue 2: Glossary is a pointer-only stub in the two upper layers**
- Location: requirements.md Glossary; system-design.md §Glossary.
- Fix: Inline a 15-term mini-glossary covering the concepts each layer uses (finding, briefing, entity, edge, scan_run_id, guidance fingerprint, knowledge_basis, tier, manifest, run_id, scope lens, writer-actor, pre-ingest redaction, capability probe, EntityId).

**Issue 3: ADR one-liners are thin for P0 decisions**
- Location: system-design §12; ADR-014 through ADR-018 especially.
- Fix: For "To author" P0 ADRs (014–018), expand to a short paragraph per ADR (context, decision, alternatives rejected) until the standalone ADR file is written, so system-design is self-sufficient meanwhile.

**Issue 4: Briefing token-bound drift between layers**
- Location: requirements.md REQ-BRIEFING-06, system-design §3 + §8, detailed-design §2.
- Fix: In detailed-design §2 table, append the hard ceilings in the same column ("typical ~60 / ceiling ≤100") so the contract is visible at point of use.

**Issue 5: Free-floating implementation details in detailed-design**
- Location: detailed-design §3 (dirty-tree policy, `--require-clean`, `clarion db sync push/pull`, `storage.commit_db: false`), §7 (`clarion check-auth --from wardline`).
- Fix: Either add a one-line mention in the corresponding system-design section (§4 for dirty-tree and commit-db opt-out, §9 for CI auth check) or append a requirement.

**Issue 6: `knowledge_basis` casing drift**
- Location: requirements.md REQ-BRIEFING-04 (snake_case), system-design §3 + detailed-design §2 (CamelCase).
- Fix: Name the wire format once, canonically. Likely `static_only | runtime_informed | human_verified` on the wire and `KnowledgeBasis::StaticOnly` internally; call this out in system-design §3.

**Issue 7: Parse-error rule-ID namespace inconsistency**
- Location: detailed-design §10 "Error surfaces" uses `CLA-INFRA-PARSE-ERROR`; requirements.md REQ-ANALYZE-06 uses `CLA-PY-PARSE-ERROR`.
- Fix: Pick one. REQ-FINDING-02 says `CLA-INFRA-*` is for pipeline failures and `CLA-PY-*` for Python-plugin structural findings — a parse error is Python-plugin emitted, so `CLA-PY-PARSE-ERROR` is correct and detailed-design §10 is the error.

**Issue 8: SARIF translator framing**
- Location: detailed-design §9.2 frames Clarion-side SARIF translator ownership as "Option A (recommended for v0.1)" i.e. a workaround; system-design §9 and requirements.md REQ-FINDING-04 frame it as permanent.
- Fix: Reconcile. The permanent-feature framing is the one that should prevail (it is consistent with Loom federation — Clarion owns a suite-wide utility).

## 8. Strengths

1. **Stable ID discipline is genuine.** Every requirement has a stable ID, a rationale, a verification method, and a forward pointer. The `Addresses:` / `See:` symmetry is the right mechanism; it just needs tightening.
2. **Loom doctrine is load-bearing throughout.** CON-LOOM-01 is cited in integration requirements, referenced in system-design §1 and §11, and the enrich-only posture drives degraded modes (NFR-RELIABILITY-02). The axiom is not decorative.
3. **Revision-history appendix in detailed-design is exemplary.** The Rev 2→3→4→5 change tables let a reader reconstruct why any given decision has its current shape. This is rare in design docs.
4. **Explicit Non-Goals with renamed-deferral IDs.** NG-14 (rename tracking), NG-17 (triage-feedback loop), NG-25 (annotation descriptor) — each is a specific, traceable deferral rather than a vague "future work".

## 9. Deferral audit

**Total cross-references found in requirements.md + system-design.md:** 21 substantive

**Acceptable (13):**
- requirements.md → ADR-001 file (for a locked author directive)
- system-design.md → detailed-design for SQL schemas, crate picks, full YAML, rule-ID enumeration (by design of the layering)
- system-design.md §12 → authored ADR files in ../adr/ (once written)
- requirements.md → loom.md §5 (reaching to doctrine, appropriate)
- system-design.md §11 → loom.md §3, §6 (doctrine citation)
- detailed-design.md → loom.md §3 (unification caveat)
- Detailed-design pointers to integration-recon.md and design-review.md (historical reviews)
- system-design.md §12 → detailed-design §11 ADR table (parallel table; navigation)
- Multiple sibling-ADR cross-refs (ADR-008 → ADR-014 supersession)

**Lazy (8):**
- requirements.md "Glossary" → detailed-design Appendix B. Requirements is used without the vocabulary it depends on.
- system-design.md "Glossary (pointer)" → detailed-design Appendix B. Same problem at the mid-layer.
- system-design.md §10 "Operator-guidance documentation lives in the detailed-design §10 for procedural depth." — Operator guidance is exactly the thing a system-design reader needs to know is enforceable; pointing away from it in the security section is a content deferral.
- system-design.md §11 "full detail in detailed-design §11" for prerequisites — the prerequisite *list* is in both places, but the detailed-design version has ADR citations and recon file references the system-design version lacks. Reader cannot act on a prerequisite without bouncing.
- system-design.md §12 ADR summaries → detailed-design §11 ADR backlog + authored ADR files. For "To author" P0 ADRs, *both* targets are inadequate (one is a summary, one is a backlog entry).
- requirements.md CON-RUST-01 → ADR-001 and system-design §12 for rationale. The rationale ("primary author's directive") is in requirements.md, but the "consequences accepted" list is not — a reader verifying whether the consequences are acceptable must leave.
- requirements.md NFR-SEC-02 → system-design §10 for the layered-defence mechanisms. Appropriate, but the NFR-SEC-02 rationale says "Layered defence is required because no single mechanism is sufficient" without naming the layers. A reader evaluating whether NFR-SEC-02 is adequately specified cannot, from requirements alone.
- requirements.md NG-07 → loom.md ("Shuttle's territory"). Mostly fine, but the federation implication (Clarion must not drift toward change execution even if operators ask for it) is not stated in requirements.md itself.

---

*Word count: ~1,480 (within 1,500 limit).*
