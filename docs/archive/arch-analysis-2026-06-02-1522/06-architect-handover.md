# 06 — Architect Handover

**Date:** 2026-06-02 · Branch `feat/road-to-first-class` · v1.1.0
**For:** the system architect / release owner taking the "road to first class" branch forward.
**Supersedes for current state:** the `2026-05-20-2124` (RC1) and `2026-05-22-1924` (from-scratch) analyses — both pre-date the SEI/Wardline-taint/WS5 work and are stale on counts. Their structural narrative still reads true; their numbers do not.

## 1. Read first

1. `04-final-report.md` — the release-state call and the validated drift register.
2. `05-quality-assessment.md` — debt by severity, with filigree IDs and ROI ranking.
3. `02-subsystem-catalog.md` — code geography (full backing in `temp/catalog-*.md`).
4. `03-diagrams.md` — the drift map is the one-glance summary.

## 2. The headline for an architect

The code is **structurally sound and maturing** (three prior defects closed; SEI/incremental/taint added with the same failure-containment discipline the rest of the system shows). It is **not** carrying hidden correctness landmines. The two things that need an architect's decision are:

- **Documentation integrity.** `system-design.md` is the stated acceptance surface, and it now contradicts the code in five sections. This is a governance problem more than an engineering one: the canonical record lies to the next contributor. **Decide who owns the reconciliation and when** (recommendation: before this branch merges to `main`, because the branch is what introduced most of the drift).
- **One verifiable release gap, not a vague roadmap call.** I checked the unbuilt surfaces against `requirements.md` (which outranks `system-design.md`). The §5 budget engine is **already a documented v1.1 deferral** (NFR-COST-01/03 → ADR-030) — settled, just mirror the notice into §5. The four §6 phase-7 findings have **no baselined REQ** — safe to cut. What is *not* settled is **D4a**: `REQ-ANALYZE-06` is baselined ("no silent fallbacks; findings visible in store + Filigree") and the code makes plugin `HostFinding`s **log-only** (`analyze.rs:626`). **Verify whether failure findings are persisted before any release claim** — if not, that is an unmet baselined requirement. Separately, **D3a**: the Anthropic→OpenRouter provider pivot silently contradicts `CON-ANTHROPIC-01` and needs an ADR to supersede it.

## 3. Drift reconciliation plan (the #1 recommended action)

All drift is **doc-side** (code wins, per CLAUDE.md precedence). For each, the resolution is a doc edit, not a code change — *except* where the architect chooses to build the missing feature instead.

| ID | Section | Action | Owner decision needed? |
|---|---|---|---|
| D1 | §2 sync vs async host | Rewrite §2 supervision to describe the synchronous host; write a short ADR recording the sync decision and *why* (testability via in-process mock). | No — code is correct |
| D2 | §2 Python parser | Replace "tree-sitter + LibCST / TYPE_CHECKING / alias_of / unresolved entities" with the `ast`-only reality. | No |
| D3 | §5 policy/budget engine | **Deferral confirmed** — `NFR-COST-01`/`NFR-COST-03` (which §5 Addresses) are already "Deferred to v1.1 per ADR-030" in `requirements.md`. Just mirror that deferral notice into §5 + fix the provider list (4, no Anthropic). | No — req settles it |
| D3a | `CON-ANTHROPIC-01` superseded | Code went OpenRouter + CLI, dropping the Anthropic-only constraint + 4-segment caching. **Write an ADR** recording the provider pivot (and its cost-caching tradeoff) to supersede the constraint. | **Yes** |
| D4 | §6 phase-7 findings | The 4 unbuilt `CLA-FACT-*` have no baselined REQ found → **safe to cut/defer**. Document the 3 shipped SEI/incremental phases; note `CLA-FACT-CLUSTERING-WEAK-MODULARITY` ships. | No (cut is safe) |
| D4a | `REQ-ANALYZE-06` tension | **Verify before any release claim.** REQ-ANALYZE-06 (baselined: no silent fallbacks; findings visible in store + Filigree) vs. log-only `HostFinding`s (`analyze.rs:626`). If failure findings aren't persisted, this is an **unmet baselined requirement = release gap**, not doc cleanup. | **Yes — gap check** |
| D5 | §8 "8-tool subset" | Replace with the 35-tool catalogue (categories in `02` §6). | No |
| D6 | §9 `entities/resolve` | Mark deferred; **cross-link §9 to `docs/federation/contracts.md` as the authoritative wire surface.** | No |
| D7 | detailed-design schema | Regenerate the schema table from the 6 migration files (13 tables + FTS5 + view; add `entities.signature`). | No |
| D8 | CLAUDE.md Layout | Add `clarion-mcp` + `clarion-scanner`; bump 5→6 crates, v1.0.0→v1.1.0. | No |

A focused doc pass closes D1, D2, D5, D6, D7, D8 in roughly a day. D3/D4 need a 30-minute roadmap ruling first.

## 4. Architecture guardrails (unchanged, still load-bearing)

- No shared runtime / registry / mediator across Loom products. Clarion stays solo-useful; federation **enriches**, never defines, Clarion semantics.
- Plugin subprocesses are untrusted — the 4-stage validation, jail, setrlimit, breakers, and entity cap are the trust boundary. Do not weaken to "make a plugin easier."
- All SQLite mutation stays centralized through the writer actor; wire-contract enforcement at the writer boundary is non-negotiable.
- MCP and HTTP response envelopes stay closed and fixture-backed.
- Source→LLM flow stays behind pre-ingest secret scan, live-provider opt-in, source-hash verification, and token budgeting.
- **New:** the SEI is now the cross-tool binding key; treat `rebind_or_mint`'s fail-closed bias as a guardrail — minting a fresh SEI on ambiguity is correct; never "guess" a rebind to avoid a mint.

## 5. Highest-risk files (require focused tests on change)

| File | LOC | Review rule |
|---|---|---|
| `clarion-mcp/src/lib.rs` | 7,101 | MCP envelope/tool tests for any response-shape or LLM-behavior change. Split first if touched heavily. |
| `clarion-cli/src/http_read.rs` | 4,387 | Federation contract tests + security review for auth/path/limits. Auth code buried here — review with care. |
| `clarion-cli/src/analyze.rs` | 3,542 | Focused tests for pipeline / run-state / SEI / clustering changes. |
| `clarion-core/src/plugin/host.rs` | 2,958 | Plugin-boundary tests for protocol/path/resource changes. |
| `clarion-core/src/llm_provider.rs` | 2,500 | Provider/accounting tests for usage, JSONL parsing, live calls. |
| `clarion-storage/src/sei.rs` | 1,143 | Matcher tests for carry/move/mint + orphan transitions; watch memory at scale. |
| `plugins/python/.../pyright_session.py` | 1,427 | Pyright timeout/cap/target-mapping tests. |

## 6. Recommended work queue (maps to filigree)

0. **Verify D4a (REQ-ANALYZE-06 gap)** — confirm plugin/failure `HostFinding`s are persisted to the store + emitted to Filigree, not just logged (`analyze.rs:626`). If not persisted, this is an unmet baselined requirement; gate the release on it. *No ticket — create one (P1 if confirmed a gap).*
1. **Doc reconciliation D1–D8 + D3a ADR** (`05` Q5) — file an issue; do before merge to `main`. Includes mirroring the ADR-030 v1.1 deferral notice into §5 and writing an ADR for the Anthropic→OpenRouter pivot (D3a). *No existing ticket — create one.*
2. **Split `mcp/lib.rs`** — `clarion-42cbd8a25a` (start it).
3. **Split `analyze.rs run_with_options`** — `clarion-cb9676de57` (start it).
4. **Retire Wardline asterisk #2** — `clarion-1f6241b329` (prereq met per `loom.md §5`).
5. **Extract `clarion-llm`** — `clarion-141e9c08c8` (also adds the missing provider-contract test).
6. **MCP-launched stale-run reconciliation + owner_pid/heartbeat** — `clarion-f9027d2187` (`05` Q9).
7. **SEI matcher memory bound at elspeth scale** (`05` Q8) — *no ticket; create one before the next large-corpus run.*
8. **Split `http_read.rs` + isolate `auth`** (`05` Q2) — *no ticket; create one.*

## 7. Verification to run before any release claim on this branch

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
bash tests/e2e/sprint_1_walking_skeleton.sh
bash tests/e2e/sprint_2_mcp_surface.sh
bash tests/e2e/phase3_subsystems.sh
bash tests/e2e/wp5_secret_scan.sh
```

## 8. Handoff risks

- This analysis read source as of the working tree on `feat/road-to-first-class`; the branch had uncommitted doc edits in `docs/superpowers/**` at session start (unrelated to code).
- Drift findings are code-vs-doc; the architect owns the deferred-vs-abandoned ruling for D3/D4 — the analysis cannot infer intent from code.
- HTTP wire conformance was checked against `contracts.md` text, not by executing the fixture suite — run the federation contract tests before trusting the 16-route surface against the contract.
- The three "resolved since prior" items (PRAGMA identity, async reqwest, pyright cap) were verified in source this session; re-confirm if the branch rebases.
