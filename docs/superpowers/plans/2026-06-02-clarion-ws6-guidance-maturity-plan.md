# WS6 — Guidance Maturity — Design & Delivery Plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:subagent-driven-development` or
> `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Date:** 2026-06-02
**Status:** Design + delivery plan (the design is folded in; this is not a bare task list)
**Workstream:** WS6 of the Clarion first-class program — **Wave 6**. Code-intelligence half;
ungated/concurrent.
**Goal:** Bring the guidance system from "schema baseline" to "mature" — the authoring CLI, the
anti-poisoning propose→promote lifecycle, Wardline-derived generation, staleness signals, and
team import/export.

**Inputs / authorities:**
- `docs/clarion/1.0/system-design.md` §7 (Guidance System — the design this implements)
- `docs/clarion/1.0/requirements.md` REQ-GUIDANCE-*, REQ-BRIEFING-05, NFR-SEC-02
- `docs/clarion/adr/ADR-024-guidance-schema-vocabulary.md` (the `scope_level`/`scope_rank`/
  `pinned`/`provenance` vocabulary)
- `docs/superpowers/specs/2026-06-02-clarion-ws5-mcp-catalogue-design.md` §6 (the WS5/WS6 boundary)
- Ground truth: the guidance schema exists (migration 0001) and `guidance_fingerprint` feeds the
  summary-cache key (`clarion-storage/src/cache.rs`); the **authoring CLI, the propose/promote
  lifecycle, the `CLA-FACT-GUIDANCE-*` staleness findings, Wardline-derived generation, and
  import/export are NOT built.** Verify the exact composition state before building.

---

## 0. What's built vs. what WS6 builds

**Built:** the `guidance` entity kind + schema (`match_rules`, `scope_level`/`scope_rank`,
`pinned`, `provenance`, `expires`); guidance composition feeding the summary-cache key.

**WS6 builds (the maturity layer):** CLI authoring · the MCP propose→observation→promote
anti-poisoning flow · Wardline-derived auto-generation tested against real Wardline output ·
staleness signals as reviewable findings · import/export. **Verify the composition baseline first**
— ground-truth what composition exists so you extend it, not duplicate it.

## 1. Design decisions

**1.1 Two authoring surfaces, one boundary (WS5/WS6).**
- **Human authoring → CLI.** `clarion guidance create/edit/show/list/delete/promote` — direct,
  operator-driven sheet management.
- **Agent suggestion → MCP, mediated.** `propose_guidance(entity_id, content, rules?)` produces a
  Filigree **observation**, NOT a sheet; `promote_guidance(obs_id)` (operator action, CLI or MCP)
  turns an observation into a sheet. This is the **NFR-SEC-02 anti-poisoning defence**: a single
  compromised LLM call cannot poison every future prompt, because promotion requires operator
  action. WS5 ships `guidance_for` (READ); WS6 owns these WRITE/lifecycle tools.

**1.2 Wardline-derived guidance — generated, overridable, tested against reality.**
On every `analyze` with `wardline.yaml` present, auto-generate per-tier / per-boundary-contract /
per-annotation-group sheets (`provenance: wardline_derived`, `pinned: true`). User-edited overrides
preserved by ID (`provenance: wardline_derived_overridden`). The gap today is that this must be
**tested against real Wardline output**, not just fixtures — wire a conformance test against an
actual `wardline.yaml` + fingerprint.

**1.3 Staleness as reviewable findings (no auto-expiry).** Emit, per `analyze`:
- `CLA-FACT-GUIDANCE-CHURN-STALE` — aggregate git-churn over matched entities since
  `authored_at`/`reviewed_at` exceeds threshold (50; **20 for `pinned`** — asymmetric on purpose,
  pinned sheets shape output most). `confidence: 0.7, confidence_basis: heuristic`.
- `CLA-FACT-GUIDANCE-ORPHAN` — explicit-entity `match_rules` pointing at a deleted entity.
- `CLA-FACT-GUIDANCE-EXPIRED` — past `expires` (excluded from composition, kept in store).
- `CLA-FACT-GUIDANCE-STALE` — `wardline.yaml` ↔ derived-guidance drift.
Surfaced via `clarion guidance list --stale`/`--expired` + the findings. **Auto-expiry is NOT the
design** — the signal pushes operators to review; the decision stays human.

**1.4 Import/export.** `clarion guidance export --to <dir>` / `import <dir>` for team sharing
(deterministic, diff-friendly).

## 2. Owner-decision (flag, don't pre-empt)
- **D5 — lifecycle depth.** How much lands in the first WS6 pass vs. later. *Recommendation:* ship
  CLI authoring + propose/promote + the four staleness findings + import/export (all reuse existing
  finding/observation machinery); **defer** the in-browser staleness-review UI (the semi-dynamic
  wiki, NG-13) to a later pass. Confirm before sequencing T4/T5.

## 3. Tasks

- [ ] **T1 — CLI authoring.** `clarion guidance create --match <…> --scope-level <…>` / `edit <id>`
  (in `$EDITOR`) / `show <id>` / `list [--for-entity <id>] [--stale] [--expired]` / `delete <id>`.
  New `crates/clarion-cli/src/guidance.rs` + `cli.rs` subcommand. Test-first on match-rule parsing
  + scope-rank ordering.
- [ ] **T2 — propose→promote lifecycle (MCP).** `propose_guidance` → Filigree observation (not a
  sheet); `promote_guidance(obs_id)` → sheet (operator action). Test the anti-poisoning property:
  a proposed sheet is NOT composed into prompts until promoted.
- [ ] **T3 — Wardline-derived generation.** Generate/regenerate derived sheets each analyze;
  preserve user overrides by ID. **Conformance test against a real `wardline.yaml` + fingerprint**,
  not only synthetic fixtures.
- [ ] **T4 — staleness findings.** The four `CLA-FACT-GUIDANCE-*` findings (§1.3) + the
  `list --stale/--expired` filters + churn-eager cache invalidation tie-in (ADR-007). Rule-catalogue
  entries. Test the asymmetric pinned threshold.
- [ ] **T5 — import/export.** Deterministic export/import; round-trip test.
- [ ] **T6 — docs.** `clarion-workflow` skill + guidance docs; rule catalogue.

## 4. Hard boundaries — do NOT
- Do NOT build `guidance_for` (READ) — that's WS5 (Wave 4). WS6 owns authoring + lifecycle + write.
- Do NOT build the semi-dynamic wiki / in-browser editing (NG-13) — out of scope (D5 recommendation).
- Do NOT auto-expire or auto-delete sheets — staleness is a signal, the decision stays human.
- Do NOT edit Accepted ADRs; do NOT touch archived docs.

## 5. Method & gates
- Ungated/concurrent. Soft relationship to WS5 (`guidance_for` read) and WS1 (a sheet's findings
  carry `sei` when bindings exist) — neither blocks WS6 authoring.
- superpowers:executing-plans / subagent-driven-development; TDD on match-rule parsing, the
  anti-poisoning property, and the asymmetric staleness threshold. Verify the composition baseline
  before building. All ADR-023 Rust gates green; Python gates if Wardline-derived emission touches
  the plugin.
- Invariants: opt-in (guidance is enrich; no base-path cost), fail-closed (anti-poisoning;
  no silent auto-expiry), enrich-only.

## 6. Definition of done (Wave 6 / WS6)
- `clarion guidance` CLI complete (create/edit/show/list/delete/promote).
- propose→observation→promote lifecycle live; proposed sheets provably NOT composed until promoted.
- Wardline-derived generation stable, override-preserving, tested against REAL Wardline output.
- The four `CLA-FACT-GUIDANCE-*` staleness findings emit + are reviewable; no auto-expiry.
- Import/export round-trips deterministically.
- All CI gates green. D5's deferred items (review UI) logged with a trigger, not silently dropped.
