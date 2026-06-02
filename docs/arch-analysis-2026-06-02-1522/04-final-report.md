# 04 — Final Report: Clarion Architecture Analysis

**Date:** 2026-06-02 · **Branch:** `feat/road-to-first-class` · **Version:** 1.1.0
**Scope:** Entire Rust workspace (6 crates) + Python language plugin.
**Method:** Tree archaeology + reconciliation against the canonical design ladder (`requirements.md` → `system-design.md` → `detailed-design.md` → 34 ADRs). Eight `codebase-explorer` subagents (one per subsystem, each anchored to a `system-design` section, drift-hunting), seeded by two prior analyses. One `analysis-validator` adversarially checked the load-bearing drift claims.
**Validator status:** Catalog → **NEEDS-REVISION** (5 drift claims clean-verified; 1 strawman + 3 numeric errors corrected inline). This report and `05`/`06` reflect the corrected facts.

---

## 1. Executive Summary

Clarion is a **single-binary Rust code-archaeology tool** (6 crates) plus a **wire-isolated Python plugin**. It ingests a codebase through out-of-tree language plugins, persists a typed entity/edge/subsystem graph in embedded SQLite, and serves that graph to consult-mode LLM agents over two read surfaces: a **35-tool MCP stdio server** and a **16-route authenticated HTTP read API** for Loom-federation. The reference Python plugin drives `pyright` as an LSP subprocess for type-resolved edges.

The architecture is **structurally clean** — 8 cohesive subsystems, a no-cycle inter-crate DAG, ~47K Rust src LOC, a test corpus roughly half the size of source. The hard engineering lives where it should: untrusted-plugin subprocess supervision, single-writer-actor storage discipline, and a deterministic analysis pipeline. The "road to first class" branch has *added* substantial capability since v1.0 — Stable Entity Identity (SEI, ADR-038), a Wardline taint store (ADR-036), WS5 faceted MCP shortcuts (19→35 tools), and incremental re-index — and has *retired* several previously-flagged defects.

**The single dominant finding of this analysis is documentation drift.** The code is ahead of `system-design.md` in five sections (§2, §5, §6, §8, §9), and in this repo's own precedence rule **a design doc that contradicts the code is a bug.** None of these drifts is a code correctness defect; all are a maintenance-and-trust hazard for the next reader, who is explicitly told (CLAUDE.md) to treat `system-design.md` as the acceptance surface.

The clearest **change-amplification debt** is unchanged in *kind* from prior analyses but worse in *degree*: four files now concentrate the operational complexity — `mcp/lib.rs` (7,101), `http_read.rs` (4,387), `analyze.rs` (3,542, with an 836-LOC monolith function), `host.rs` (2,958). Split tickets exist for all four; none is taken.

---

## 2. What changed since the 2026-05 analyses (the delta)

| Area | Then (2026-05-22) | Now (2026-06-02) |
|---|---|---|
| MCP tools | 19 | **35** (WS5 faceted + honest-empty shortcuts) |
| `mcp/lib.rs` | 4,703 LOC | **7,101** |
| `analyze.rs` | 2,549 LOC | **3,542** |
| Entity identity | `plugin:kind:qualname` only | **+ SEI** (ADR-038, blake3 surrogate; qualname demoted to locator) |
| Storage tables | ~9 | **13 + FTS5 + view**, 6 migrations; `sei_bindings`, `sei_lineage`, `sei_prior_index`, `wardline_taint_facts` |
| Wardline | probe only | **+ taint-fact store** (ADR-036), read-by-SEI, HTTP `/api/wardline/*` |
| Re-index | full re-walk | **+ Wave-2 incremental** file-hash skip |
| `application_id`/`user_version` | absent (flagged) | **implemented + enforced** ✅ |
| Blocking reqwest in async MCP | present (flagged) | **wrapped in `spawn_blocking`** ✅ |
| Pyright restart cap | reset per recycle (flagged) | **shared across recycles** (`PyrightRunState`) ✅ |

Three prior 🔴/🟡 concerns are now genuinely resolved — credit where due. The codebase is maturing, not stagnating.

---

## 3. The system in code

### 3.1 Shape
A CLI with two architectural modes. `install`/`analyze`/`hook`/`doctor` are one-shot; `serve` is a long-running supervisor running an MCP stdio server (current-thread runtime) and an Axum HTTP read API (multi-thread runtime) over **one shared `ReaderPool`** (identity proved with `Arc::ptr_eq`). Either thread crashing kills the binary — no per-surface restart.

### 3.2 The hard parts (well-engineered)
- **Plugin host** (`host.rs`): generic-over-IO synchronous supervisor with an in-process mock; 4-stage per-entity validation (field-size → ontology → identity-recompute → path-jail); drop-with-finding vs kill-with-error asymmetry; two breakers at two scopes; `setrlimit` via `pre_exec`; detached stderr-drain ring buffer; per-frame ceiling rejection *before* body-consume.
- **Storage** (`writer.rs`): every mutation through one bounded-mpsc actor on `spawn_blocking`; per-run super-transaction, batch-commit every 50; wire-contract enforcement at the writer boundary (edge-kind tables + parent↔contains bijection abort) so caller bugs cannot corrupt graph shape.
- **SEI** (`sei.rs`, ADR-038): fail-closed `rebind_or_mint` matcher (trivial carry → git-rename carry → exact-one-vanished move → fail-closed mint) turns the entity ID into a stable cross-tool key resilient to rename/move.
- **Determinism**: seeded Leiden clustering with a hand-rolled fallback; entity IDs are pure functions gated by a cross-language byte-for-byte parity fixture.

### 3.3 Wire surfaces

| Surface | Where | Auth |
|---|---|---|
| `clarion` CLI | `main.rs`/`cli.rs` | n/a |
| Plugin JSON-RPC | `core/plugin/protocol.rs` (Content-Length over pipes) | process boundary = trust boundary |
| MCP server | `mcp/lib.rs` (stdio, framing autodetect) | caller-trusted |
| HTTP read API (16 routes) | `cli/http_read.rs` | **HMAC-SHA256 → bearer → loopback-WARN**, constant-time |
| OpenRouter | `core/llm_provider.rs` | API key |
| Filigree | `mcp/filigree.rs` | bearer + actor header |
| Pyright LSP | `plugins/python/.../pyright_session.py` | none |

---

## 4. Drift register (validated)

| # | Doc claim | Code reality | Sev | Resolve in |
|---|---|---|---|---|
| D1 | §2: async/tokio host, mpsc backpressure, streaming, `file_list` RPC | fully synchronous host; batch `analyze_file`; no `file_list` | 🔴 | doc (write ADR/errata) |
| D2 | §2 Python: tree-sitter + LibCST, `TYPE_CHECKING` exclusion, `alias_of`, `unresolved` entities | CPython `ast` only; none of those exist | 🔴 | doc |
| D3 | §5: AnthropicProvider, 4-segment cache_control, async cost trait, `cost_report`, budget findings | 4 providers (no Anthropic), flat payload, sync `estimate_tokens`, no budget engine | 🔴 | doc + roadmap decision |
| D4 | §6: 4 phase-7 `CLA-FACT-*` findings; phases 0/2/4–7 | unimplemented; 3 SEI/incremental phases undocumented (1 other CLA-FACT *does* ship) | 🔴 | doc + roadmap decision |
| D5 | §8: "v1.0 ships 8-tool subset"; shortcuts "deferred to v1.1" | 35 tools ship | 🔴 | doc |
| D6 | §9: `GET /api/v1/entities/resolve` shipped | does not exist (deferred per `contracts.md`); 16 routes live; §9 not cross-linked to `contracts.md` | 🟡 | doc |
| D7 | `detailed-design.md:611-760`: 6 tables + FTS5 | 13 tables + FTS5 + view across 6 migrations; `entities.signature` undocumented | 🟡 | doc |
| D8 | `CLAUDE.md` Layout: 4 crates / v1.0.0 | 6 crates / v1.1.0 (`clarion-mcp`, `clarion-scanner` omitted) | 🟡 | doc |

All eight are **doc-side bugs** in this repo's precedence model (code wins). D3/D4 additionally pose a *roadmap* question: are the unbuilt features deferred or abandoned? The doc should say which.

---

## 5. Risks & smells (consolidated; full list in `05`)

### 🔴 High
1. **Four monolith files** concentrate change risk: `mcp/lib.rs` 7,101, `http_read.rs` 4,387, `analyze.rs` 3,542 (`run_with_options` 836 LOC), `host.rs` 2,958. Split tickets `clarion-42cbd8a25a`, `clarion-cb9676de57`, `clarion-2b8811da39` exist; none started.
2. **Documentation drift (D1–D8)** — `system-design.md` actively misleads on five subsystems; no ADR/errata reconciles the sync pivot, the unbuilt policy engine, or the shipped 35-tool surface.

### 🟡 Medium
3. **Wardline federation asterisk still live** — `wardline_probe.py:38` imports `wardline.core.registry` directly; the Wardline-side retirement prerequisite (NG-25) is met per `loom.md §5`; migration ticket `clarion-1f6241b329` is open/ready.
4. **`clarion-llm` not extracted** — `reqwest`/`tempfile`/`which` sit in the plugin-supervisor crate solely for `llm_provider.rs` (`clarion-141e9c08c8`).
5. **SEI matcher loads all alive bindings into a HashMap** at re-index start — unbounded at elspeth scale.
6. **`analyze_runs.rs` lacks stale-`running`-row reconciliation** on supervisor crash — can block future `analyze_start`.
7. **Codex provider cost accounting blind** (`cost_usd` hardcoded 0.0); malformed Codex JSONL under-reports tokens.
8. **Wardline taint writer-actor in the HTTP runtime** has no separate health-check surface.

### 🟢 Low
9. Facade-bypass leak (`writer.rs:537` → `core::plugin::manifest::RESERVED_ENTITY_KINDS`).
10. No design doc enumerates all 12 secret-scan rules.
11. `query.rs` (1,727) / `writer.rs` (1,211) approaching split-me size.

---

## 6. Strengths worth naming

Generic-over-IO supervisor + in-process mock; wire-contract enforcement at the writer boundary; cross-language parity fixture; per-frame ceiling before body-consume; two breakers at two scopes; secret-scanner baseline that won't mask drift; deterministic seeded clustering with fallback; SEI's fail-closed matcher; honest-empty MCP shortcuts (an empty result is explicitly distinguished from "signal absent"). This is a system that has thought hard about adversarial-plugin and partial-failure scenarios — and the new SEI/incremental work extends that discipline rather than bolting on shortcuts.

---

## 7. Confidence & coverage

**Overall: High.** All 8 subsystems analyzed; every load-bearing module read at least partially; `host.rs` impl, all storage/migration files, the 35-arm MCP dispatch, and both scanner crates read end-to-end. The three largest files (`mcp/lib.rs`, `http_read.rs`, `analyze.rs`) were enumerated and sampled (not 100% end-to-end). Drift claims were independently validated; corrections from that pass are folded into all artifacts. HTTP wire conformance assessed against `contracts.md` text, not by executing the fixture suite.

## 8. Pointers
- Per-subsystem detail: `02-subsystem-catalog.md` (+ full backing in `temp/catalog-*.md`).
- Diagrams (7 Mermaid incl. drift map): `03-diagrams.md`.
- Quality/debt deep-dive: `05-quality-assessment.md`.
- Architect handover + remediation roadmap: `06-architect-handover.md`.
- Validation: `temp/validation-catalog.md`.
