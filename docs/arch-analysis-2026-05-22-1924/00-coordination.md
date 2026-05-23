# Coordination Plan — Clarion Architecture Analysis

**Workspace:** `docs/arch-analysis-2026-05-22-1924/`
**Date:** 2026-05-22
**Operator:** Claude (system-archaeologist skill)
**Source instruction:** "Full from-scratch analysis, do not read existing documentation."

## Analysis Configuration

- **Scope:** Entire repo source tree — `crates/` (Rust workspace) and `plugins/python/` (Python plugin). Excludes `target/`, `.venv/`, `.git/`, caches.
- **Deliverables (Option A — Full Analysis):**
  - `01-discovery-findings.md` — holistic assessment
  - `02-subsystem-catalog.md` — per-subsystem entries
  - `03-diagrams.md` — C4 architecture diagrams
  - `04-final-report.md` — synthesized executive narrative
- **Strategy:** PARALLEL per-subsystem explorers, then sequential validation + diagrams + report.
  - Rationale: 7 candidate subsystems are loosely coupled (separate crates + a plugin), well-suited to independent exploration.
- **Tier:** Standard (not ultralarge). LOC ~50K, file count ~85 source files, subsystems = 7.
- **Constraint:** **Do not consult existing design docs** (`docs/clarion/**`, `docs/suite/**`, ADRs, sprint READMEs, CLAUDE.md design content). Findings must derive from code only. CLAUDE.md operational sections (tooling, build commands) remain in-scope as orientation; design narrative there is treated as "existing documentation" and avoided.
- **Complexity:** Medium. Plugin host / RPC transport / federation HTTP API are non-trivial; storage actor model and entity-ID assembly need careful inspection.

## Subsystem Partition (initial candidates)

1. **clarion-core** — Plugin host, transport, manifest, jail/limits, breaker, entity-ID assembler, LLM provider.
2. **clarion-storage** — Writer-actor + reader-pool over SQLite (schema, pragma, query, cache, unresolved).
3. **clarion-cli** — `clarion` binary (`install`, `analyze`, `serve`, `secret-scan`), clustering, HTTP read API, run lifecycle, stats.
4. **clarion-mcp** — MCP server façade (filigree client, config).
5. **clarion-scanner** — Secret/entropy/pattern scanner with baseline support.
6. **clarion-plugin-fixture** — Test fixture plugin (also a reference impl of the protocol).
7. **plugins/python** — Python language plugin (function/class entity extraction, qualname, Wardline probe).

## Execution Log

- 2026-05-22 19:24 — Created workspace `docs/arch-analysis-2026-05-22-1924/`.
- 2026-05-22 19:24 — Scale check: 6 crates (~44K Rust LOC), Python plugin (~6.5K LOC), 121 md files. Standard tier confirmed.
- 2026-05-22 19:25 — User selected **Option A — Full Analysis**.
- 2026-05-22 19:25 — Wrote this coordination plan. Strategy: PARALLEL per-subsystem explorers.
- Next: holistic discovery sweep (entry points, build graph, dependency edges) → `01-discovery-findings.md`.
