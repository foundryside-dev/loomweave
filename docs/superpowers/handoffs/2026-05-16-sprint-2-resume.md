# Clarion Sprint 2 — Resume Handoff (post-amendment)

This file is the starting prompt for the next Claude Code session that
opens the resumed Sprint 2. Paste it as the user's first message; it is
self-contained.

Supersedes [`2026-04-30-sprint-2-kickoff.md`](./2026-04-30-sprint-2-kickoff.md)
for execution sequencing; the original kickoff remains the historical
record of what Sprint 2 was originally scoped to do.

---

# Begin Clarion Sprint 2 (resumed under scope amendment 2026-05-16)

Sprint 2 was opened 2026-04-30 with seven Tier B boxes (B.1–B.7) plus
warmup carryover. Two boxes shipped (B.2 class+module entities, one
warmup bug `clarion-5e03cfdd21`). On 2026-05-16, a planning conversation
amended the sprint scope: WP4 catalog/rendering work (B.1, B.4, B.5) is
deferred to v0.2 in favour of the **MVP MCP surface** (pyright-based
calls extraction with confidence tiers, the seven-tool MCP server, and
the Filigree entity_associations binding). The amendment shipped three
new ADRs (028/029/030) and a scope-amendment memo; this handoff is the
implementation kick-off for the amended scope.

The Rust + Python plugin stack is the implementation vehicle. Do NOT
rewrite the workspace; the existing crates and plugin protocol already
do what the MVP needs (B.2 shipped two new entity kinds with zero
protocol changes — that's the proof). Forward on the existing work.

## Required reading (in order)

1. **The scope amendment** — [`docs/implementation/sprint-2/scope-amendment-2026-05.md`](../../implementation/sprint-2/scope-amendment-2026-05.md). This is the source of truth for what Sprint 2 ships now. Read it cover-to-cover before doing anything else.
2. **The three new ADRs** —
   - [ADR-028 — Edge confidence tiers](../../clarion/adr/ADR-028-edge-confidence-tiers.md)
   - [ADR-029 — Entity associations binding](../../clarion/adr/ADR-029-entity-associations-binding.md)
   - [ADR-030 — On-demand summary scope](../../clarion/adr/ADR-030-on-demand-summary-scope.md)
3. **The original Sprint 2 kickoff** — [`2026-04-30-sprint-2-kickoff.md`](./2026-04-30-sprint-2-kickoff.md). Read sections "Sprint 1 in one paragraph" (so this handoff stays self-contained on the walking-skeleton contract) and "Carryover" (unfixed warmup bugs are still ready work).
4. **The B.3 design** — [`docs/implementation/sprint-2/b3-contains-edges.md`](../../implementation/sprint-2/b3-contains-edges.md). Implementation pending; this is the next thing to actually code.

Skim only:
- [`v0.1-plan.md`](../../implementation/v0.1-plan.md) — has the original WP definitions; the resequence memo is now the authoritative source for what ships in v0.1. The plan is architectural reference, not execution map.
- Earlier ADRs (001–027) — load on demand.
- `sprint-1/signoffs.md` — the lock-ins (L1–L9) are unchanged; Sprint 2 reads against them.

## Working directory + branch

- Directory: `/home/john/clarion`
- Branch: continue on `sprint-2/b3-design` (the in-flight branch). The scope-amendment artifacts (ADRs 028/029/030, scope memo, plan + ADR-README updates) were authored under a `worktree-sprint-2+mvp-reset-additions` worktree; merge that into `sprint-2/b3-design` before starting implementation work, or rebase the amendments on top of B.3 impl as you go.
- Latest commit on `sprint-2/b3-design`: `5c510f1` (B.3 design doc).
- The `sprint-2/b2-design` branch is closed; B.2 work merged.

## Sprint 1 in one paragraph (so this handoff is self-contained)

`clarion analyze` against a fixture project with a single
`def hello(): ...` Python file persists exactly one entity row,
`python:function:demo.hello`, with `kind="function"`, into
`.clarion/clarion.db`. The pipeline is: Rust plugin host discovers
the editable Python plugin on `$PATH`, spawns it, completes a
JSON-RPC handshake, sends one `analyze_file` request, receives one
entity, and writes it through the writer-actor. Every stage has both
positive and negative tests. CI runs three jobs (rust, python-plugin,
walking-skeleton); all green at sprint close. Tagged `v0.1-sprint-1`.

## Sprint 2 status snapshot (2026-05-16)

| Box | Status | Filigree |
|---|---|---|
| Original B.1 (multi-file dispatch) | **deferred to v0.2** | — |
| Original B.2 (class + module entities) | **shipped** | `clarion-daa9b13ce2` closed |
| Original B.3 (`contains` edges) | **design committed, impl pending** | `clarion-39bc17bde8` approved |
| Original B.4 (catalog.json) | **deferred to v0.2** | — |
| Original B.5 (per-subsystem markdown) | **deferred to v0.2** | — |
| Original B.6 (elspeth demo) | **renamed to B.8, moved to end** | `clarion-6222134e0d` (new) |
| Original B.7 (no-Filigree-changes invariant) | **voided** — broken on purpose by new B.7 | — |
| **NEW B.4*** — calls + pyright + confidence | not started | `clarion-2d2d1d27b5` |
| **NEW B.5*** — references | not started | `clarion-b0cedfd2bb` |
| **NEW B.6** — WP8 MCP surface (7 tools) | not started | `clarion-e2a3672cc9` |
| **NEW B.7** — WP9-A entity_associations binding | not started | `clarion-73ab0da435` |
| **NEW B.8** — elspeth scale-test | not started | `clarion-6222134e0d` |

## Execution sequencing

```
warmup (optional) → B.3 impl → B.4* → B.5*       B.7 (parallel)
                                       ↓           ↓
                                       B.6 (consumes both)
                                          ↓
                                       B.8 scale-test (sprint close)
```

### Step 0 — Warmup (optional but recommended)

Pick one of the unfixed warmup bugs. Both are P2, small, in code you
will touch later, and will get you back into the Rust workspace:

- `clarion-ed5017139f` — `clarion install` leaves partial `.clarion/` on failure. Fix: cleanup or atomic move.
- `clarion-b5b1029f5a` — `reader_pool` flaky 100ms sleep. Fix: replace with deterministic synchronisation primitive.

Skip if you want to dive into B.3.

### Step 1 — B.3 implementation (`clarion-39bc17bde8`)

Design is committed at [`docs/implementation/sprint-2/b3-contains-edges.md`](../../implementation/sprint-2/b3-contains-edges.md). Implementation follows the §"Locked surfaces" and §"Design decisions Q1–Q6" sections directly. Key shape:

- Python plugin emits `contains` edges + adds `parent_id` field to `RawEntity` (dual encoding per Q2).
- Writer-actor enforces the parent_id ↔ contains-edge consistency invariant (`CLA-INFRA-PARENT-CONTAINS-MISMATCH`).
- Schema migration drops `edges.id`, promotes `(kind, from_id, to_id)` to natural PK (ADR-026 + ADR-024 edit-in-place policy).
- Ontology version bumps `0.2.0 → 0.3.0` (MINOR per ADR-027).

Update filigree status as you go: `clarion-39bc17bde8` is currently `approved`; transition to `building` when impl starts.

### Step 2 — B.4* calls + pyright + confidence (`clarion-2d2d1d27b5`)

This is the load-bearing box. Read **ADR-028** carefully. Three things to figure out before writing code:

**Decision A — which pyright path?** Three viable strategies (scope-amendment §5):
1. Pyright-as-LSP + per-symbol `callHierarchy` queries (accurate, slow).
2. AST walk (`ast` / `libcst` / `ruff_python_ast`) + pyright as type oracle for ambiguous sites.
3. `pycg` or similar pre-built call-graph extractor.

Decide via a design doc (`docs/implementation/sprint-2/b4-calls-pyright.md`, following the B.2/B.3 panel-resolution format). The decision shapes everything downstream.

**Decision B — week-2 go/no-go.** At the end of week 2 of B.4* implementation, run the chosen path against elspeth-slice (or a comparable corpus) and measure call-edge extraction wall-clock time. Gate per scope-amendment §5:
- Green (<5 min): proceed.
- Yellow (5–30 min): document cost; consider optimisation.
- Red (>30 min): pause, redesign with the engineer panel.

This gate's purpose is to discover scale-bound design problems in week 2, not week 5 staring down B.8.

**Decision C — storage layout for inferred edges.** Same `edges` table with `confidence='inferred'` rows, or separate `inferred_edges_cache` table? ADR-028 §"Alternative 4" defers this to B.4* implementation. Pick during implementation; document in the B.4* design doc.

The schema migration for `confidence` column lands under ADR-024's edit-in-place policy (no consumer has read an edge row yet; Clarion is unpublished).

### Step 3 — B.5* references (`clarion-b0cedfd2bb`)

Mechanically similar to B.4*: pyright's cross-reference index is the resolution engine; same three-tier confidence model. Write `b5-references.md` design doc; smaller surface than B.4* (pyright cross-refs are well-trodden).

### Step 4 — B.7 entity_associations binding (`clarion-73ab0da435`)

Can start in parallel with B.4*/B.5* — different repo (Filigree), different code path. Two halves:

**Filigree-side**:
- New migration: `entity_associations` table per ADR-029 §"Decision 1".
- New MCP tools registered: `add_entity_association`, `remove_entity_association`, `list_entity_associations`.
- Target Filigree release: 2.1.0 (Filigree is currently at `release/2.0.1`).
- Coordinate with the Filigree-side release plan; this is one PR on the Filigree repo.

**Clarion-side**:
- HTTP client for Filigree's existing API (use `reqwest`; auth per ADR-012 / Filigree's existing UDS+token scheme).
- New MCP tool: `issues_for(entity_id, include_contained=true)` — implementation in the new `clarion-mcp` crate (see B.6 below).

Federation §5 audit is in ADR-029; cite it if any reviewer questions the cross-product coupling.

**Pre-emptive read**: `clarion-889200006a` (ADR-018 amendment trigger — Wardline `FingerprintEntry` qualname divergence). The first cross-product join will surface it; addressing it during B.7 is cheaper than after.

### Step 5 — B.6 WP8 MCP surface (`clarion-e2a3672cc9`)

New crate: `clarion-mcp`. Use `rmcp` (the Rust MCP SDK) or whatever the Rust ecosystem has settled on by the time this work starts.

Seven tools (per ADR-028 §"Decision 2" + ADR-029 §"Decision 2" + ADR-030 §"Decision 1"):
- `entity_at(file, line)` — innermost containing entity (uses `ix_entities_source_file` + line-range query)
- `find_entity(pattern)` — qualname glob/regex via `entity_fts` (already in schema)
- `callers_of(entity_id, confidence: Resolved)` — default-resolved (load-bearing per ADR-028 §"Decision 2")
- `execution_paths_from(entity_id, max_depth: 3, confidence: Resolved)` — DFS over calls edges
- `summary(entity_id)` — on-demand, cached on 5-tuple (ADR-007 + ADR-030)
- `issues_for(entity_id, include_contained: true)` — HTTP to Filigree (ADR-029)
- `neighborhood(entity_id, confidence: Resolved)` — one-hop callers + callees + container + contained

MCP auth: UDS default per ADR-012; falls back to TCP+token. Same discipline as the HTTP read API (also part of WP8 in `v0.1-plan.md` — defer the HTTP read API to v0.2 unless a sibling tool actually needs it during scale-test).

### Step 6 — B.8 elspeth scale-test (`clarion-6222134e0d`)

Sprint 2 closes when B.8 ships. Scope per scope-amendment §3:
- `clarion analyze` runs end-to-end against elspeth-slice (~50 files initially; full ~425k LOC if feasible).
- `clarion serve` exposes the seven MCP tools.
- Consult-mode agent navigates the corpus via the MCP server.
- Measure: cost per `summary(id)` call, latency per MCP query, cache hit rate on second-pass.
- Re-scoped WP11 spike: per-query cost (the honest metric for on-demand) rather than per-run cost.

If pyright performance under B.4* turns out to be the bottleneck, B.8 is also where you decide whether to ship anyway with documented limitations, or block on a follow-up optimisation pass.

## Working discipline (carries over from Sprint 1)

- **Walking skeleton stays green throughout.** Every PR must keep `cargo nextest run --workspace`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo fmt --check`, `cargo deny check`, `cargo doc --no-deps`, `ruff check`, `ruff format --check`, `mypy --strict`, `pytest`, and `bash tests/e2e/sprint_1_walking_skeleton.sh` green. The e2e script grows assertions per sprint; B.3 should add "at least one contains edge persisted"; B.4* adds calls-edge assertions; B.6 adds MCP-tool round-trip assertions.
- **Design before code.** Every new B.N* gets its own design doc under `docs/implementation/sprint-2/`, following the B.2/B.3 pattern: Scope, Locked surfaces, Design decisions Q1–QN with panel resolution, Test plan, References. The five-reviewer panel (systems thinker, solution architect, architecture critic, Python/Rust engineer, quality engineer) for each Q is the design discipline.
- **ADR amendments not in-place edits.** If you discover a B.4* decision needs to amend ADR-028, write a new ADR superseding it (ADR-024 edit-in-place policy is narrowing — only the pre-publication storage migrations have it; the wire-shape and confidence-tier definitions in ADR-028 are now load-bearing).
- **Filigree status hygiene.** Update issue status as you transition between design / impl / review. The original Sprint 2 left this drift (B.2 shipped but was still `proposed` until 2026-05-16); don't repeat that.
- **Observation as you go.** When you spot a code smell, missed test, or concerning pattern, fire an `observe` via Filigree's MCP (per project CLAUDE.md). Don't stop your current work; just leave the note.

## What to ask the user about, what to just do

**Just do**:
- Pick warmup or B.3 first (your call).
- Pyright extraction strategy choice in B.4* — write the design doc, run the panel, decide.
- Inferred-edge storage layout in B.4* — same.
- B.6 tool descriptions and parameter defaults — follow ADR-028/029/030; don't relitigate.
- Filigree status transitions on the issues you're working.

**Ask the user**:
- If the week-2 go/no-go gate on B.4* fires Yellow or Red.
- If federation §5 starts to feel uncomfortable on the B.7 design (some edge case ADR-029 didn't anticipate).
- If you discover a load-bearing surface that requires amending ADR-028/029/030 rather than just refining them.
- Before any commit that touches multiple crates simultaneously (the "flag-day refactor" anti-pattern from Sprint 1 carries forward).

## Tooling note

The `filigree` MCP server is configured in `.mcp.json`. Prefer the MCP
tools (`mcp__filigree__*`) over the CLI when available — faster and
structured. Run `mcp__filigree__session_context` at session start to
load the current ready / in-progress / blocked landscape.

The current Sprint 2 ready landscape (as of 2026-05-16):
- `clarion-39bc17bde8` (B.3 impl) — approved, blocked by nothing
- `clarion-2d2d1d27b5` (B.4*) — pending, blocked by B.3
- `clarion-b0cedfd2bb` (B.5*) — pending, blocked by B.4*
- `clarion-e2a3672cc9` (B.6) — pending, blocked by B.4*
- `clarion-73ab0da435` (B.7) — pending, no clarion-side blocker; coordinates with Filigree-side release
- `clarion-6222134e0d` (B.8) — pending, blocked by B.6 + B.7
- Open warmups: `clarion-ed5017139f`, `clarion-b5b1029f5a`
- Triage backlog (don't let it grow further): `clarion-fbe50aa6e1`, `clarion-523b2eebad` (P2 audit findings)

Good luck. The infrastructure is solid; the design is locked; the
work-list is short. Sprint 2 closes when an agent can ask
`callers_of(some_function_in_elspeth)` and get a useful answer.
