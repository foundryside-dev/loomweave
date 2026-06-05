# Wave 3 — WS9 `legis` governance consumption (execution plan)

**Date:** 2026-06-02
**Branch:** `feat/wave3-legis-governance` (stacked on `feat/wave2-dossier-participation`)
**Prompt:** `docs/superpowers/prompts/2026-06-02-wave-3-execution.md`
**Closes:** GOVERNED PARADISE — `legis` adds IRAP-grade governance keyed to Loomweave's
stable identity, as an OPT-IN layer invisible to a solo project. Does **not** gate core paradise.

## Gate check — DONE, both conditions met

1. **Wave 2 complete (core paradise).** Verified in code last wave: `resolve` / `resolve_sei` /
   `lineage` live, dossier-participation surface pinned, incremental skip + orphan guard shipped.
   The wave-2 commits are the parent of this branch.
2. **`legis` exists.** The prompt's "design-ready, NOT implemented" line is **stale**. `legis` at
   `/home/john/legis` is now a running Python/FastAPI subsystem: `src/legis/` (git surface, checks,
   enforcement, policy grammar, audit store, identity), passing tests, an HTTP read API. Smoke-tested:
   `GET /health → {status: ok, service: legis}`, `GET /git/renames?rev_range=… → 200 [RenameEvidence]`.
   The user's goal explicitly said "check legis," so the owner knows it shipped. **Gate opens — build.**

## Ground truth (verified in code, both sides)

**Loomweave (the seam already exists, Wave 1):**
- `loomweave-storage/src/sei.rs`: `GitRename { old_locator, new_locator }` + the typed `GitRenameSource`
  trait (`fn renames_since(&self, base_commit: &str) -> Vec<GitRename>`). The matcher is **fail-closed**:
  a rename is only a *hint*; the carry is confirmed by byte-identical body hash, so an over- or
  under-broad rename signal can never cause a *false* carry — only a missed/relabeled one.
- `loomweave-cli/src/sei_git.rs`: `ShellGitRenameSource` (v1 concrete). The translation
  `parse_git_rename_lines` → `path_to_module` → `file_renames_to_locator_renames` are **shared free
  functions** — reusable by any provider.
- `analyze.rs:1107-1115`: builds `ShellGitRenameSource` and calls `.renames_since("")`.
  **`""` = compare working tree to `HEAD`** (`git diff -M HEAD`) → the **uncommitted** "rename a file,
  re-analyze before commit" dev flow.

**legis (the planned provider, now real):**
- `src/legis/git/surface.py` `GitSurface.renames(rev_range)` → `RenameEvidence{commit_sha, old_path,
  new_path, similarity}` via `git log -M --diff-filter=R` over a **committed rev-range**.
- `src/legis/api/app.py` exposes `GET /git/renames?rev_range=…`. Docstring: "WP-6.3 re-exposes this
  surface to Loomweave's matcher." `docs/federation/sei-conformance.md` claims the §6 seam (REQ-L-02).
- legis is a **pull-only polling consumer** of Loomweave's `resolve_sei` + `lineage`; for REQ-L-01 it
  accepts **Option 3** (store its own lineage snapshot hash, detect divergence on re-read) → it wants
  **no Loomweave-side hash-chain / signature**.

## The load-bearing design fact — window mismatch (drives the deliverable shape)

`analyze` depends on the **working-tree-vs-HEAD** rename window (empty base). legis's `/git/renames`
serves only **committed** renames over a rev-range. In Loomweave's canonical dev flow legis returns
**empty** → naively flipping the operative source to legis would be **enrich-NEGATIVE** (silently lose
the renamed-and-edited uncommitted case, relabel `locator_changed`→`moved`). That violates both the
prompt's enrich-only invariant and the unstated corollary *Loomweave-with-legis must not be worse than
Loomweave-without*.

**Resolution (honest, regression-free):** build the `LegisGitRenameSource` seam + degrade path and
make the selector **capability-aware on the base window** — for the empty (working-tree) window the
operative source stays `ShellGitRenameSource`; legis is selected only for a non-empty committed
rev-range (a re-index path the current pipeline does not yet drive). The seam is **proven and tested**;
the live pipeline is **unchanged → zero regression**; the gap is **surfaced with a recommendation**
(DoD explicitly blesses "gap surfaced with recommendation"). The matcher's fail-closed property means
neither choice risks a false carry.

## Workstream A — audit-spine consumption contract (doc + verify only)

- legis reads Loomweave's **existing** Wave-1 surface (`resolve_sei`, `lineage`) as its audit spine.
  No new endpoint; no Loomweave-side integrity machinery (REQ-L-01 Option 3).
- Verify reachability of the surfaces legis relies on; pin the consumption contract in
  `docs/federation/contracts.md` (consumer obligations, two-axis status, honest-degrade flag,
  orphan→governance-gap reliance). Fill an additive gap **only if** legis surfaces a blocking one —
  batch lineage / push surface are "informational/future" per legis's own conformance notes, so do
  **not** build them speculatively.

## Workstream B — git-rename provider swap (REQ-C-05) — the real code, TDD

1. **RED:** tests in `sei_git.rs` (+ a small mock) for:
   - translation parity: identical file-rename input → identical `Vec<GitRename>` through both the
     shell-parse path and the legis-JSON-parse path (shared translation fn).
   - `LegisGitRenameSource` parses legis `/git/renames` JSON → `(old_path,new_path)` pairs → locators.
   - **degrade:** legis absent / URL unset / unreachable → empty signal, and the selector falls back
     to `ShellGitRenameSource` (enrich-only). Loomweave-without-legis is byte-identical to today.
   - capability: empty base → selector returns shell even when legis is configured (no regression).
2. **GREEN:** add `reqwest.workspace = true` to loomweave-cli; implement `LegisGitRenameSource`
   (`base_url`, `current_locators`) behind `GitRenameSource`; add `resolve_git_rename_source(...)`
   with enrich-only discipline mirroring `filigree_url.rs`. Thread `legis_url: Option<String>` into
   `AnalyzeOptions` + a `--legis-url` CLI flag (default `None` → shell).

## Workstream C — trust vocabulary carried verbatim (verify, no adjudication)

- Loomweave carries `declared_tier` / `wardline_groups` exactly as Wardline emits them (the v1.0
  `WardlineMeta` ingestion concept). WS9 adds **no** trust adjudication and **no** policy/attestation
  engine — attestations live in `legis`, keyed on Loomweave's SEI. Verify nothing regresses; note the
  posture in the contract. Any incompleteness in v1.0 carriage is a **pre-existing gap**, not WS9's job.

## Invariants held (weft.md §5)

- **Opt-in:** `--legis-url` unset → behavior identical to today. Solo Loomweave untouched.
- **Fail-closed:** legis unreachable → empty signal → shell fallback → matcher confirms by body hash.
- **Enrich-only:** legis may supply the git signal; it is never *required*; it never moves identity
  authority out of Loomweave; Loomweave never adjudicates trust (one judge: Wardline analyses, legis governs).

## Definition of done

- Audit-spine contract pinned; legis reads SEI+lineage over the existing surface; no integrity machinery.
- `LegisGitRenameSource` behind the typed interface; `ShellGitRenameSource` the fallback; tests prove
  identical `GitRename` output for identical input + clean legis-absent degrade.
- Window-semantics gap surfaced with a recommendation (legis working-tree rename surface, or Loomweave
  rev-range re-index driving) — not papered over.
- Trust vocab verbatim; no adjudication added.
- Loomweave solo + Loomweave-without-legis both fully functional. All ADR-023 gates green. Code review.
