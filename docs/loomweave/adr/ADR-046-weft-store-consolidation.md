# ADR-046: Weft Store Consolidation (`.weft/loomweave/`)

**Status**: Accepted; amends ADR-005, ADR-040, ADR-044
**Date**: 2026-06-07
**Deciders**: john@pgpl.net
**Context**: The Weft federation is consolidating each member's scattered project
dotdir into one shared convention so a project root holds a single `.weft/`
tree (`weft.toml` for operator config; `.weft/<member>/` for each member's
machine-written runtime state) instead of N sibling dotdirs (`.loomweave/`,
`.filigree/`, `.wardline/`, …). This ADR records Loomweave's side of that move.

## Summary

Loomweave's per-project store moves from `<root>/.loomweave/` to
`<root>/.weft/loomweave/` — a **clean break**: there is no fallback read of the
old `.loomweave/` location. Every consumer routes through a single helper
(`loomweave_core::store`), so the path cannot drift across the workspace.

Two adjacent decisions ride along:

1. **Operator override (`weft.toml`).** The operator-authored
   `<root>/weft.toml` may relocate the store via a member-private
   `[loomweave].store_dir` key. Loomweave reads **only its own `[loomweave]`
   table** and treats a missing or malformed `weft.toml` as absent (silent
   fallback to the default — never a hard failure; C-9c). `weft.toml` is
   **read-only** to Loomweave: `install`, `doctor`, and the CLI never write it
   (the C-4 discipline behind Gate `weft-eb3dee402f` — never add a writer to a
   shared multi-section file). No shared/cross-member `weft.toml` keys are read
   yet; those are hub-pinned and pending Loomweave's schema proposal.

2. **Sibling resolution reads `.weft/` only (clean break).** When Loomweave
   resolves a sibling's runtime state it reads the consolidated
   `<root>/.weft/<sibling>/` location **only** — Filigree's live read-API port
   at `<root>/.weft/filigree/ephemeral.port`, Wardline's trust-vocabulary
   descriptor at `<root>/.weft/wardline/vocabulary.yaml`. There is **no** fallback
   to the pre-consolidation `.<sibling>/` path. Weft is pre-launch with a
   coordinated cutover, so after launch every sibling is at `.weft/` by
   construction; a sibling found only on the legacy path means a mis-sequenced
   cutover, and silently resolving it would bind a stale dir (the lacuna-401
   wrong-but-quiet-resolve failure mode). Instead resolution folds to the
   fail-soft default — the configured URL (`source = "config"`) for Filigree, an
   absent project descriptor for Wardline — so the wire-facing `source`/status
   reports the gap loudly. **Runbook ordering:** Filigree migrates to
   `.weft/filigree/` → this build installs → downstream re-init (lacuna). This
   build must not be installed against any project until Filigree has migrated.

## Decision

### Store location

`loomweave_core::store::store_dir(project_root)` is the single source of truth:

- Default: `<project_root>/.weft/loomweave/`.
- Override: `[loomweave].store_dir` in `<project_root>/weft.toml` (a relative
  value resolves against the project root; an absolute value is used verbatim).
- Fail-soft (C-9c): a missing/unparseable `weft.toml`, an absent `[loomweave]`
  table or `store_dir` key, a wrong-typed or blank value — all fall back to the
  default. Unknown top-level tables (a sibling's section) and unknown keys in
  `[loomweave]` are ignored, so the file stays forward-compatible.

The directory's contents and git-tracking posture inherit ADR-005's amended
state — only the parent path moves. `.gitignore` and durable per-run provenance
remain tracked; `loomweave.db`, the WAL sidecars, shadow DB, `embeddings.db`,
`ephemeral.port`, `instance_id`, `*.lock`, `tmp/`, `logs/`, and
`runs/*/log.jsonl` remain ignored by `<root>/.weft/loomweave/.gitignore`.
`loomweave.yaml` stays at the project root (Loomweave's authoritative config;
`weft.toml` is enrich-only and never load-bearing — the §5 deletion test still
holds). The old `.weft/loomweave/config.json` stub is no longer written.

### Amendments to prior ADRs

- **ADR-005** (`.loomweave/` tracking policy): the tracked-vs-ignored split is
  retained verbatim; the directory is now `.weft/loomweave/`.
- **ADR-040** (semantic-search sidecar): `embeddings.db` now lives at
  `.weft/loomweave/embeddings.db`.
- **ADR-044** (read-API ephemeral port): Loomweave publishes its own port to
  `.weft/loomweave/ephemeral.port`; the loopback-only/port-only/atomic file
  contract is otherwise unchanged. Cross-product consumers that read it (e.g.
  Wardline) read the `.weft/` location only, matching this clean break.

## Consequences

### Positive

- One `.weft/` tree per project instead of N sibling dotdirs; sibling subtrees
  are co-located and each member owns exactly its own subtree.
- A single store helper eliminates the ~30 scattered `.join(".loomweave")` sites
  and the drift they invited.
- Operators get a documented, member-private way to relocate the store without
  editing Loomweave config (`weft.toml:[loomweave].store_dir`).

### Negative

- A clean break orphans any existing `.loomweave/` directory. Existing projects
  must re-init (`loomweave install` then `loomweave analyze`) under
  `.weft/loomweave/`; the old directory can be deleted. This repo's own
  committed `.loomweave/` is removed as part of this change; downstream testbeds
  (e.g. `lacuna`) need explicit re-init coverage so they are not silently
  stranded.

### Neutral

- Reading `[loomweave].store_dir` does a small TOML parse on the store-path
  resolution path. These are not hot paths (install, serve startup, analyze),
  and the read is fail-soft.

- The source-walk / secret-scan / pyright skip-lists exclude the whole `.weft/`
  dotdir (the default store and all sibling subtrees). A `[loomweave].store_dir`
  override pointing *outside* `.weft/` is **not** auto-excluded from those walks,
  so an operator who relocates the store under the source tree may see the store
  DB walked/scanned. The recommended override stays within `.weft/` (or outside
  the analyzed root); per-override skip-list wiring is deferred until a concrete
  need appears.

## Related Decisions

- [ADR-005](./ADR-005-loomweave-dir-tracking.md) — the tracking policy this ADR
  relocates.
- [ADR-040](./ADR-040-semantic-search-embeddings.md) — embeddings sidecar path.
- [ADR-044](./ADR-044-read-api-ephemeral-port-publication.md) — the ephemeral
  port file contract.

## References

- Weft federation doctrine `§5` (enrichment-not-load-bearing / deletion test)
  and the `weft.toml` / `.weft/<member>/` config-store consolidation contract.
- Gate `weft-eb3dee402f` (C-4) — never add a writer to a shared multi-section
  file; the reason `install`/`doctor` never write `weft.toml`.
