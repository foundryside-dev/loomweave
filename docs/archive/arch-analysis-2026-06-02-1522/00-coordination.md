# 00 — Coordination Plan

## Analysis Configuration

- **Scope:** Entire Rust workspace (`crates/`, 6 crates) + Python language plugin (`plugins/python/`). Branch `feat/road-to-first-class`, workspace version **1.1.0**.
- **Deliverables:** **Option C — Architect-Ready.** `01-discovery` → `02-catalog` → `03-diagrams` → `04-final-report` → `05-quality-assessment` → `06-architect-handover`.
- **Strategy:** **PARALLEL** — 8 independent subsystems, ~47K Rust src LOC (+~21K test) + ~3.2K Python, loosely coupled (no inter-crate cycles per prior art). Fan out one `codebase-explorer` per subsystem.
- **Time constraint:** None stated.
- **Complexity estimate:** High (operationally subtle subprocess supervision, writer-actor, MCP tool surface), but structurally a clean DAG.

## Reframe (per advisor) — this is a DELTA analysis, not a from-scratch map

This codebase has a full design ladder (`docs/clarion/1.0/{requirements,system-design,detailed-design}.md` + 34 ADRs) **and** two prior arch-analyses:

- `docs/arch-analysis-2026-05-22-1924/` — thorough 7-subsystem structural archaeology, done **deliberately without design docs** (2026-05-22).
- `docs/implementation/arch-analysis-2026-05-20-2124/` — RC1-era release-readiness pass with quality + handover docs (2026-05-20).

Therefore the structural map already exists. **The value of this run is the delta:**

1. **Reconcile current code against the canonical design** (`system-design.md` §1–§11 `Addresses:` headers ARE the authoritative decomposition). In this repo a doc that contradicts the code is a *bug*, not merely a finding.
2. **Hunt doc-vs-code drift.** Already seeded: CLAUDE.md says 5 crates / v1.0.0; reality is **6 crates** (`clarion-mcp`, `clarion-scanner` undocumented in the layout list) / **v1.1.0**.
3. **Quality assessment + architect handover (debt roadmap)** — not provided by the design docs; the justification for Option C.

**Code has grown materially since the 05-22 analysis** (MCP `lib.rs` 4703→7101, `analyze.rs` 2549→3542; new modules: `catalogue/`, `sei`, `wardline_taint`, `prior_index`, `index_diff`, `scan_results`, `snapshot`, `wardline_reconcile`, `sei_git`, `hooks_settings`; MCP tools 19→~35). Explorers REFRESH against current source, not redo blind.

## Subsystem partition (anchored to system-design sections)

| # | Subsystem | Files | Design anchor |
|---|-----------|-------|---------------|
| 1 | core / plugin host | `clarion-core/src/plugin/*`, `entity_id.rs`, `errors.rs` | §2 Core/Plugin |
| 2 | policy / LLM | `clarion-core/src/llm_provider.rs` | §5 Policy Engine |
| 3 | storage | `clarion-storage/src/*` | §4 Storage, §3 Data Model |
| 4 | analysis pipeline | `clarion-cli/src/{analyze,clustering,sei_git,analyze_lock,run_lifecycle}.rs` | §6 Pipeline |
| 5 | CLI surfaces + federation HTTP | `clarion-cli/src/{serve,http_read,install,hook,hooks_settings,mcp_registration,doctor,instance,config,secret_scan*}.rs` | §1, §9 Integrations |
| 6 | MCP consult surface | `clarion-mcp/src/*` | §7 Guidance, §8 MCP |
| 7 | secret scanner + fixture | `clarion-scanner/src/*`, `clarion-plugin-fixture/src/*` | §10 Security |
| 8 | Python plugin | `plugins/python/src/**` | §2 Python specifics |

## Validation gates

Multi-subsystem (≥3) ⇒ MUST spawn `analysis-validator` after the catalog and after the final report. Reports land in `temp/validation-*.md`. BLOCK status halts deliverable sign-off.

## Execution Log

- 2026-06-02 15:22 — Created workspace `docs/arch-analysis-2026-06-02-1522/`.
- 2026-06-02 — User selected **Option C (Architect-Ready)**.
- 2026-06-02 — Clarion self-index empty (`never_analyzed`); Rust bulk has no Rust extractor → traditional tree archaeology.
- 2026-06-02 — Holistic scan: 6 crates, ~47K Rust src LOC, Python plugin ~3.2K. Workspace v1.1.0.
- 2026-06-02 — Advisor checkpoint: reframed to delta/drift analysis anchored to system-design §1–§11; read prior art (05-22 final report, 05-20 handover).
- 2026-06-02 — Strategy fixed: PARALLEL, 8 focused explorers (drift-hunting + quality), then validate.
- 2026-06-02 — 8 `codebase-explorer` agents completed; partials in `temp/catalog-*.md`. Headline finding: doc drift across system-design §2/§5/§6/§8/§9 + detailed-design + CLAUDE.md.
- 2026-06-02 — Wrote `01-discovery`, `02-catalog`, `03-diagrams`.
- 2026-06-02 — **Validation gate 1** (`analysis-validator` on `02`): NEEDS-REVISION → corrected inline (ADR-013 GCP strawman dropped; provider count 4; detailed-design "6 tables+FTS5"; CLAUDE.md "4 crates"; wardline ×4; phase-7 reframed). 5/9 drift claims clean-verified; no BLOCK. Report: `temp/validation-catalog.md`.
- 2026-06-02 — Wrote `04-final-report`, `05-quality-assessment`, `06-architect-handover`.
- 2026-06-02 — **Validation gate 2** (`analysis-validator` on `04/05/06`): APPROVED-WITH-WARNINGS. All 6 cited filigree IDs verified real; no reintroduced errors; no invented claims. Fixed 2 blemishes (http_read.rs LOC erratum 4,387→4,765 grew mid-analysis; unsafe-block count 1→2). Report: `temp/validation-final-report.md`.
- 2026-06-02 — **Requirements check (advisor-driven):** split "behind" drift (D3/D4) against `requirements.md` (outranks system-design). D3 §5 budget engine = **confirmed v1.1 deferral** (NFR-COST-01/03 → ADR-030); D3a Anthropic→OpenRouter pivot = **superseded CON-ANTHROPIC-01, needs ADR**; D4a `REQ-ANALYZE-06` (no silent fallbacks) vs log-only `HostFinding`s = **possible release gap, verify**. Folded into 04 §4, 05 Q5/Q12/priority-0, 06 §2/§3/queue.
- 2026-06-02 — **COMPLETE.** All 6 Architect-Ready deliverables produced + validated.

## Outcome

Architect-Ready analysis delivered. Dominant finding: **documentation-integrity debt** — `system-design.md` drifted from shipped code in 5 sections with no reconciling ADR (all doc-side bugs under CLAUDE.md precedence). Code is structurally sound and maturing (3 prior defects closed; SEI/ADR-038 + Wardline-taint/ADR-036 + WS5 35-tool surface added). Top recommendation (`06` §3): a ~1-day doc-reconciliation pass before merge to `main`, plus a deferred-vs-abandoned ruling on the §5 policy engine and §6 phase-7 findings. Four of the next five debts already have filigree tickets needing scheduling, not analysis.
