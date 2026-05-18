# Validation Report — 03-diagrams.md

**Document:** `/home/john/clarion/docs/arch-analysis-2026-05-18-1244/03-diagrams.md`
**Validator:** analysis-validator (independent pass)
**Date:** 2026-05-18
**Status:** APPROVED

---

## Summary

Five C4-inspired diagrams (Context, Container, two sequences, one component zoom). All Mermaid blocks parse cleanly; captions accurately describe what each diagram shows. Spot-checks against the catalog and source code confirm every substantive claim. All six subsystems from the catalog appear in at least one diagram. Loom-doctrine relationships (Filigree enrich-only, Wardline soft-import asterisk) are represented in the L1 view. ADR-007 5-tuple cache, edge-contract validator, writer-actor, and the host validator pipeline each have visible representation.

No critical issues. Two minor notes recorded as informational; neither blocks progression.

---

## Findings

### Critical

None.

### Warnings

None.

### Spot-checks (all PASS)

1. **Storage → Core dependency labelling (Diagram 2).**
   The user's framing of the question pointed at `STORAGE → CORE` labelled "writer + readers". Re-reading the Mermaid source: the `"writer + readers"` label is actually on `STORAGE -->|"writer + readers"| DB` (line 99), not on `STORAGE --> CORE` (line 93). The latter is unlabelled, which is correct shorthand — the catalog records the dep as `EdgeConfidence` only (`crates/clarion-storage/src/query.rs:6`, `commands.rs:14`), and the diagram's prose at line 105 explicitly states "`storage` → `core` (one symbol, `EdgeConfidence`)". **PASS — no misleading label exists.**

2. **`SoftFailed` "partial work kept" (Diagram 3).**
   Verified at `crates/clarion-cli/src/analyze.rs:478–509`. The `SoftFailed` branch sends `WriterCmd::CommitRun { status: RunStatus::Failed, stats_json, … }` where `stats_json` includes `entities_inserted`, `edges_inserted`, etc. The writer-actor folds the `UPDATE runs SET status='failed'` into the open entity transaction (per the comment at line 479–482 and the catalog's storage section), so accepted entities from healthy plugins persist alongside the failure marker. The diagram's "partial work kept" caption is accurate. **PASS.**

3. **ADR-007 5-tuple cache key (Diagram 4).**
   Verified at `crates/clarion-mcp/src/lib.rs:1077–1083`. `SummaryCacheKey` is materialised with exactly the five fields shown: `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`. `prompt_template_id` is set to `LEAF_SUMMARY_PROMPT_TEMPLATE_ID`, defined at `crates/clarion-core/src/llm_provider.rs:10` as `"leaf-v1"`. The diagram's annotation `LEAF_SUMMARY_PROMPT_TEMPLATE_ID = "leaf-v1"` matches. (The context-line citation `1010-1016` in the user's request actually points at `InferredEdgeCacheEntry`, not `SummaryCacheKey` — the diagram itself doesn't cite line numbers and shows the correct 5-tuple, so this is irrelevant to the diagram's correctness.) **PASS.**

4. **Five validator steps, kill paths on 3 and 4 only (Diagram 5).**
   Verified at `crates/clarion-core/src/plugin/host.rs:1031–1198`.
   - Step 0 field-size (lines 1103–1107): `continue` only — drops record.
   - Step 1 ontology declared-kind (1110–1114): `continue` only.
   - Step 2 identity (1117–1133): `continue` only.
   - Step 3 jail (1135–1164): `continue` on under-threshold escape; `return Err(HostError::PathEscapeBreakerTripped)` at line 1160 after `do_shutdown` — **kill path confirmed**.
   - Step 4 entity cap (1166–1179): `return Err(HostError::EntityCapExceeded(e))` at line 1178 after `do_shutdown` — **kill path confirmed**.
   Steps 0–2 have no `return Err` path; only `continue`. The diagram's claim "steps 3 and 4 are the only ones with kill paths" is exact. **PASS.**

5. **60-second in-flight coalescer timeout (Diagram 4 caption).**
   Verified at `crates/clarion-mcp/src/lib.rs:912` — `tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv())` inside `coalesced_inferred_dispatch` (declared at line 894). The `inferred_inflight: HashMap<InferredEdgeCacheKey, broadcast::Sender<InferredDispatchOutcome>>` field is at lines 175–176. **PASS.**

### Coverage check (PASS)

| Catalog subsystem | Appears in |
|---|---|
| A `clarion-core` | Diagrams 2 (CORE), 3 (CORE), 5 (entire diagram) |
| B `clarion-storage` | Diagrams 2 (STORAGE), 3 (WRITER + DB), 4 (READER, CACHE, WRITER) |
| C `clarion-mcp` | Diagrams 2 (MCP), 4 (MCP) |
| D `clarion-cli` | Diagrams 2 (CLI), 3 (CLI) |
| E `clarion-plugin-fixture` | Diagram 2 (FIXTURE), Diagram 3 (as alternative to PLUGIN) |
| F Python plugin | Diagrams 1 (PYRIGHT external + implicit), 2 (PYPLUGIN), 3 (PLUGIN) |

Loom doctrine surfaces (Diagram 1): Filigree as solid `sibling`-styled box with "enrich-only" edge label; Wardline as solid sibling with a dashed "import probe at handshake (asterisk: loom.md §5)" edge — matches `docs/suite/loom.md` §5's named v0.1 asterisk treatment.

Architecturally load-bearing concepts (all visible):
- **ADR-007 5-tuple cache** — Diagram 4, explicit 5-tuple shown in `SELECT by 5-tuple` step.
- **Edge-contract validator** — Diagram 3, `InsertEdge * N (enforce_edge_contract)` step.
- **Writer-actor (ADR-011)** — Diagrams 2, 3, 4 (singleton WRITER participant in both sequences).
- **Host validator pipeline** — Diagram 5 (entire diagram).

### Notes (informational, non-blocking)

1. **Diagram 3 plugin-loop step ordering is slightly idealised.** The diagram shows the `loop per file in plugin extensions` nested inside `loop per plugin`, with `host.shutdown / kill / reap` happening after the inner loop completes. In `clarion-cli/src/analyze.rs` the actual control flow uses a `BatchResult`-returning helper and per-plugin spawn-then-collect pattern; the diagram's nesting reads as a clean conceptual model rather than a literal call-graph. The diagram's caption doesn't claim line-fidelity, so this is acceptable shorthand for an L3 sequence.

2. **Diagram 5 collapses edge/stats post-processing into terminal nodes.** Steps `EDGES` (process_edges, drop-only) and `STATS` (process_stats) are shown as siblings of the entity validator chain, attached to `OUTCOME` rather than to the accepted-entity exit. The host actually invokes `process_edges` and `process_stats` after the per-entity loop completes (`host.rs:1190–1191`). The diagram's edge layout is a fair simplification; the design-notes prose underneath ("same drop-on-violation discipline is applied to edges, but with no kill paths") sets expectations correctly.

---

## Confidence Assessment

**High.** All five substantive spot-checks resolved cleanly against source code at the cited locations. Coverage is complete. The diagrams are unusually well-aligned with the catalog and source — captions are accurate, none of the labels overstate what's shown, and the L1/L2/L3 + sequence breakdown is conventionally correct C4 usage.

## Risk Assessment

**Low.** No claims that would mislead a downstream consumer. The minor "notes" above are stylistic — the diagrams are read as conceptual models, not literal call traces, and their captions are consistent with that contract.

## Information Gaps

None blocking. The diagrams deliberately omit a `clarion-mcp::lib.rs` component view and a writer-`WriterCmd` zoom; both omissions are explicitly justified in the "Coverage notes" table at the end of the document and adequately substituted by catalog content.

## Caveats

- This validation checks structural and factual fidelity against the catalog and source spot-checks; it does not re-render the Mermaid blocks. The document attests the blocks were validated through the Mermaid renderer during authoring.
- Architectural quality of the chosen abstractions (e.g. is a "Container" view the right level for `clarion-mcp`?) is out of scope for structural validation. Refer to `axiom-system-architect:architecture-critic` if such review is desired.

---

**Final status: APPROVED** — proceed to next phase.
