# Wave 2 — execution plan (WS4 dossier participation + T3.1 incremental skip)

**Date:** 2026-06-02
**Status:** In execution
**Branch:** `feat/wave2-dossier-participation` (stacked on `feat/wave1-sei-authority`)
**Source of truth:** program design §4 (Wave 2) + §5 invariants + D3; integrated delivery
plan Phase 3 (T3.1, T3.2); Wardline dossier design (`/home/john/wardline/.../2026-06-01-wardline-weft-entity-dossier-design.md`).
**Filigree:** WS4 = `clarion-4bccfb5f44`; T3.1 = `clarion-a96573d734`.

This wave **closes core paradise**: `dossier(entity)` becomes achievable by the Wardline
assembler over Loomweave's HTTP surface, and stays correct after a function is renamed.

## Gate (verified in code, not just the plan)
- **Wave 1 (WS1):** `/api/v1/identity/resolve`, `:batch`, `sei/:sei`, `lineage/:sei`,
  `validate_locator` (REQ-F-02), `sei_bindings` + `ux_sei_alive_locator`, conformance oracle — all present.
- **Wave 0 (WS2+WS3):** `/api/v1/entities/:id/callers`/`callees` (+batch), `prior_index.rs`
  + `sei_prior_index` populated each run.
- Both live on the linear stack (`main` is an ancestor). "Merged" ⇒ stable/won't-shift, which a
  linear stack satisfies. Proceed.

## Framing: Loomweave serves, it does not assemble
The dossier is assembled by Wardline. Loomweave's Wave 2 job is (a) guarantee every slice the
assembler needs is HTTP-reachable + pin it, and (b) ship the incremental skip deferred from Wave 1.
**Do NOT** build a Loomweave-owned dossier envelope or proxy sibling data.

---

## T3.1 — Incremental analysis (skip unchanged files) + orphan guard  [the real code; TDD]

### Design facts (verified)
- `module` entities carry a **whole-file blake3** `content_hash` (analyze.rs `content_hash_for_entity`,
  line 2548). Non-module entities carry a normalized line-span hash.
- Module entities are in the prior index (`sei_prior_index`: locator → body_hash + signature).
- `entities` is **cumulative, never pruned**; edges are **`INSERT OR IGNORE`** (additive). There is
  **no per-run full-replace sweep** → skipping a file leaves its entities + edges intact in the DB.
  Skip is therefore pure speed, no semantic change — *provided* the SEI orphan guard includes skipped
  locators.
- The SEI matcher's `current_locators` is consumed by exactly two functions: `rebind_or_mint`
  (cases 2a/2b vanish-detection) and `orphaned_bindings`. Both must see the union.

### Mechanism
1. **`whole_file_hash(path) -> Option<String>` helper** (analyze.rs) — `blake3::hash(fs::read(path))`.
   Used by BOTH `content_hash_for_entity`'s module branch AND the skip check, so the comparison is
   byte-identical. (Advisor blind-spot E.) Fail toward re-analysis when `None`.
2. **`previously_analyzed_files(conn) -> HashMap<file_path, whole_file_hash>`** (query.rs) — join
   `sei_prior_index` to `entities` on locator, filter `kind='module'`, return
   `{source_file_path → body_hash}` (last-run-scoped, NOT cumulative entities).
3. **`prior_locators_by_file(conn) -> HashMap<file_path, Vec<locator>>`** (query.rs) — prior-index
   locators grouped by their entity's `source_file_path` — the skipped entities' locators.
4. **Skip in the plugin file loop:** before dispatch, partition `plugin_files` into `changed`
   (current `whole_file_hash` ≠ prior, or path absent from prior) and `skipped` (equal). Dispatch
   only `changed`. For each skipped file: emit a `skipped_unchanged` progress event, count it,
   collect its prior locators (for the union) AND its prior-index entries (for the rebuild — see #6).
5. **Orphan guard (union):** thread `retained_locators: HashSet<String>` (skipped-file locators) into
   `run_sei_mint_pass`. Inside, `current_locators = analyzed_descriptor_locators ∪ retained_locators`,
   passed to BOTH `rebind_or_mint` and `orphaned_bindings`. The carry/mint loop still iterates only the
   analyzed `descriptors` (skipped entities' bindings are untouched at an unchanged locator).
6. **Prior-index WRITE side (advisor blind-spot A):** append the skipped files' prior-index entries
   (`{locator, body_hash, signature}` from `load_prior_index`) to `prior_index_entries` before the
   `replace_prior_index` rebuild — otherwise the rebuild blanks skipped files out of the next run's
   index and the skip decays after one run.
7. **`skipped_files: N`** added to `stats.json` (both run-success branches).
8. **Full-skip (zero-dispatch) path:** when every file is unchanged, the plugin is skipped entirely;
   the union + prior-index rebuild + stats still account for all entities.
9. **`--no-incremental`** flag on `AnalyzeOptions` (default-on skip). Mirrors `--no-sei` wiring.
10. **`last_seen_commit`** for skipped entities is intentionally NOT bumped (skip = don't touch);
    documented as the one acceptable observable difference (a re-analyze refreshes it).

### Tests (RED before GREEN — the crux)
- **T-RED (orphan guard):** wire the naive analyzed-only locator set FIRST; write the regression test
  (analyze, change ONE file, re-analyze) and watch it FAIL (unchanged-file entities falsely orphaned);
  THEN add the union and watch it pass. This proves the guard is load-bearing.
- **T-decay (prior-index write side):** thrice-unchanged re-run asserts `skipped_files` stays maximal
  every run (not just run 2).
- **T-skip-basic:** changed file is re-analyzed, unchanged files skipped; `skipped_files` correct.
- **T-full-skip:** all-unchanged re-run dispatches zero files, run still commits, stats correct.
- **T-no-incremental:** `--no-incremental` forces full re-analysis (skipped_files = 0).
- Unit: `whole_file_hash` parity with the module content_hash; `previously_analyzed_files` /
  `prior_locators_by_file` queries.

---

## T3.2 / WS4 — Dossier participation surface  [contract + verification, light code]

### The surface (each verified HTTP-reachable)
| Dossier section | Loomweave surface | HTTP | Status |
|---|---|---|---|
| identity (entity_id, content_hash, **content axis**) | `resolve(locator)` | POST `/api/v1/identity/resolve` (+`:batch`) | ✅ Wave 1 |
| identity (**identity axis**: alive/orphaned + lineage) | `resolve_sei(sei)` | GET `/api/v1/identity/sei/:sei`, `/lineage/:sei` | ✅ Wave 1 |
| linkages: callers/callees | — | GET `/api/v1/entities/:id/callers`/`callees` (+batch) | ✅ Wave 0 |
| linkages: scc_peers | subsystem clustering (≠ true SCC) | — | ⚠ graceful-degrade + recommendation |
| file context | file catalog | GET `/api/v1/files` (+`:resolve`, `/batch`) | ✅ existing |
| work (Filigree associations) | **Filigree's own** `/api/entity-associations` (ADR-029) | — | ✅ read DIRECTLY, not via Loomweave |

### Two-axis freshness (explicit, neither inferred from the other)
- **Content axis:** `resolve(locator)` → `content_hash`; the assembler hash-compares its stored
  fact's write-time hash against this.
- **Identity axis:** `resolve_sei(sei)` → `alive: true|false` + `lineage`; orphaned/superseded surfaced
  honestly. A rename flips the locator but `resolve_sei` on the carried SEI stays alive.

### Filigree associations — the "GAP" resolved (enrich-only)
The Wardline dossier design (§4, §9, §6.1) reads Filigree associations **directly** from Filigree's
own `GET /api/entity-associations?entity_id=…` (ADR-029, frozen). Loomweave's `issues_for` is MCP-only,
but that is **not** a dossier gap: making Loomweave serve Filigree associations would make Loomweave a
Filigree **proxy/aggregator** — a direct violation of the enrich-only axiom (weft.md §5) and the Wave 2
hard boundary ("do NOT aggregate Filigree issues into a Loomweave object"). **Recommendation:** Loomweave
provides only the **join key** (the SEI, via `resolve`); the assembler keys Filigree's own endpoint on
it. No Loomweave endpoint added.

### scc_peers
Loomweave has subsystem clustering (`subsystem_members`/`subsystem_of_entity`), not strongly-connected-
component peers. Surfacing subsystem peers under the dossier's `scc_peers[]` would be a semantic
mismatch. The dossier degrades gracefully on partial linkages (callers/callees carry the load-bearing
"fix locus / responsible boundary" synthesis). **Recommendation:** expose a thin subsystem-peers HTTP
route as a follow-up only if Wardline confirms it wants subsystem peers (not true SCC) there — named,
not silently dropped.

### Deliverables
- `docs/superpowers/specs/2026-06-02-loomweave-dossier-participation.md` (the spec above, expanded).
- Pin every depended-on endpoint in `docs/federation/contracts.md`.
- e2e demonstration (extend `tests/serve.rs`): renamed-function fixture → the full set of assembler
  HTTP calls succeeds, SEI carried, facts not orphaned, freshness stamped.

---

## Definition of done
- Incremental skip works; `skipped_files` in stats; orphan-guard regression test passes
  (unchanged-file entities keep their SEI, not orphaned); prior-index decay test passes.
- Participation spec written; every depended-on endpoint HTTP-reachable + pinned (or gap surfaced
  with recommendation — Filigree-direct + scc_peers).
- `dossier(entity)` achievable over Loomweave's HTTP surface for a renamed-function fixture.
- All ADR-023 Rust gates green. (Python untouched.)
- Code review requested; honest core-paradise statement surfaced. Do NOT proceed into the parallel band / WS9.
