# WS7 — Multi-Language Plugin Support — Design & Delivery Plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:subagent-driven-development` or
> `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Date:** 2026-06-02
**Status:** Design + delivery plan (design folded in; not a bare task list)
**Workstream:** WS7 of the Loomweave first-class program — **Wave 7**. Code-intelligence half;
ungated/concurrent.
**Goal:** Make "any language" real, not aspirational — publish the plugin protocol as an external
spec, ship a conformance harness, validate distribution with a real out-of-tree plugin, and build a
second-language plugin. **Other languages are *other producers*, not a core rewrite.**

**Inputs / authorities:**
- `docs/loomweave/1.0/system-design.md` §2 (Core / Plugin Architecture — the protocol this publishes)
- `docs/loomweave/adr/ADR-022-core-plugin-ontology.md` (core stays language-agnostic; plugin owns
  ontology), ADR-027 (ontology-version semver), ADR-033 (v1.0 distribution via GitHub Releases),
  ADR-026 (containment wire + edge identity), ADR-038 (manifest `signature_schemas`)
- Ground truth: the protocol IS implemented (`loomweave-core/src/plugin/{manifest,host,discovery}.rs`)
  and `loomweave-plugin-fixture` exists — but there is **NO external protocol documentation** and no
  out-of-tree conformance/distribution validation. Confirm before building.

---

## 0. The four pieces, and why three are autonomous

| Piece | What | Gate |
|---|---|---|
| **1. Publish the protocol spec** | the L4 JSON-RPC contract + manifest, as a versioned external doc | none |
| **2. Conformance harness** | generalise `loomweave-plugin-fixture` into a suite a new author runs | none |
| **3. Distribution validation** | prove ADR-033 with a real out-of-tree plugin | none |
| **4. Second-language plugin** | a TS/Go/Rust plugin proving the core is language-agnostic | **D2** |

Pieces 1–3 are **D2-independent and deliver value on their own** (a documented, conformance-tested,
distributable protocol). Piece 4 waits on D2 (which language). So WS7's first milestone is "the
protocol is publishable and provable"; the second plugin is a follow-on, not a blocker.

## 1. Design decisions

**1.1 Publish the protocol as a versioned external spec.** Document, from the implementation
(`manifest.rs`/`host.rs`), the full L4 contract: the lifecycle (`initialize` → manifest;
`file_list`; `analyze_file` streaming `entity`/`edge`/`finding` notifications; `build_prompt`), the
Content-Length JSON-RPC framing, and every manifest field (`plugin_id`, `kinds`, `edge_kinds`,
`tags`, `capabilities` + per-capability `confidence_basis`, `supported_rule_ids`, `prompt_templates`,
`signature_schemas` (ADR-038), `ontology_version` (ADR-027)). Live at `docs/plugin-protocol/` (or
`docs/federation/`) as the authority an external author implements against. **Version it** (ADR-027
semver) so plugins can pin.

**1.2 Conformance harness — the plugin oracle.** Generalise `loomweave-plugin-fixture` into a
reusable suite a new plugin author runs against the core: manifest validity, entity/edge/finding
emission shapes, streaming framing, `build_prompt` rendering, the ADR-026 containment/edge-identity
contract. Analogous to the SEI conformance oracle — "conformant" is proven, not claimed. Structural
compatibility is necessary, not sufficient.

**1.3 Distribution validation (ADR-033).** Validate the real path end-to-end with an **out-of-tree**
plugin (not the in-repo fixture): GitHub-Release asset → `pipx install <asset>` (or the
language-appropriate equivalent) → `~/.config/loomweave/plugins.toml` registration → discovery →
analyze. This is what proves a third party can actually ship a plugin.

**1.4 Core stays language-agnostic (ADR-022 — load-bearing).** WS7 adds NO fixed kinds, no
hardcoded "function/class", no language-specific core logic. A second language must work purely
through the manifest the core already accepts. **If you find yourself editing the core to teach it
about a language, you have violated ADR-022 — stop.** Other languages are *other producers*; the
Python AST analyzer stays Python (North Star — no rewrite).

## 2. Owner-decision (flag, don't pre-empt)
- **D2 — which second language** (TypeScript / Go / Rust), customer-demand-driven. elspeth is
  Python, so the validating customer does not force it. *Recommendation:* ship pieces 1–3 first
  (the publishable, provable protocol) and treat the second plugin as a follow-on gated on D2 —
  pick by the first non-Python customer's need; absent that, TypeScript (broadest ecosystem reach).
  Confirm before T4.

## 3. Tasks

- [ ] **T1 — publish the protocol spec.** From the implementation, write the versioned external
  `docs/plugin-protocol/` spec (lifecycle, framing, every manifest field, the reserved core edge
  kinds, ontology-version semver). Cross-check it against `manifest.rs`/`host.rs` so doc = reality.
- [ ] **T2 — conformance harness.** Generalise `loomweave-plugin-fixture` into a runnable conformance
  suite + a "your plugin conforms" report. Test-first: a deliberately-broken fixture fails the
  right check; the Python plugin passes.
- [ ] **T3 — distribution validation (ADR-033).** Drive the full out-of-tree install→register→
  discover→analyze path with a minimal real plugin; document the author workflow. Surface any gap
  in ADR-033's path additively (do not edit the immutable ADR).
- [ ] **T4 — second-language plugin (gated on D2).** Build a minimal but real plugin in the chosen
  language: manifest, entity/edge extraction for that language, conformance-suite green, distributed
  via the validated path. Proves the core needed no change (ADR-022).
- [ ] **T5 — docs.** Author-facing "write a Loomweave plugin" guide referencing the protocol spec +
  conformance harness; `loomweave-workflow`/README updates.

## 4. Hard boundaries — do NOT
- Do NOT add language-specific logic, fixed kinds, or hardcoded concepts to the core (ADR-022). A
  second language works through the manifest or it is a core bug, not a core feature.
- Do NOT rewrite the Python analyzer or fold other languages into it — other producers, not a rewrite.
- Do NOT let the second-language plugin (T4/D2) block pieces 1–3 — they ship independently.
- Do NOT edit Accepted ADRs (ADR-022/026/027/033); surface gaps additively. Do NOT touch archived docs.

## 5. Method & gates
- Ungated/concurrent. T1–T3 autonomous; T4 gated on D2.
- superpowers:executing-plans / subagent-driven-development; TDD on the conformance suite (broken
  fixture → right failure; Python plugin → pass). Verify the protocol implementation before
  documenting it (doc must equal `manifest.rs`/`host.rs`). All ADR-023 Rust gates green; Python
  gates for any plugin-side or fixture work; the second-language plugin runs its own toolchain's gates.
- Invariants: ADR-022 (core agnostic), enrich-only, conformance proven not assumed.

## 6. Definition of done (Wave 7 / WS7)
- The plugin protocol is published as a versioned external spec that equals the implementation.
- A conformance harness exists; the Python plugin passes; a broken fixture fails the right check.
- Distribution (ADR-033) is validated end-to-end with a real out-of-tree plugin + an author workflow doc.
- (On D2) a second-language plugin is built, conformance-green, and distributed — with **zero core
  changes** (ADR-022 proven).
- All CI gates green. If T4 defers pending D2, it is logged with the decision, not silently dropped.
