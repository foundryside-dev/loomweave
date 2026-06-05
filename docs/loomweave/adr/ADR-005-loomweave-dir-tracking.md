# ADR-005: `.loomweave/` Directory Git-Tracking Policy

**Status**: Accepted; amended by ADR-041
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: `loomweave install` must write a `.gitignore` inside `.loomweave/` that
separates committed analysis state from volatile per-run artefacts. Sprint 1 WP1
Task 5 is the authoring trigger; before this ADR, the rules were only proposed
in `docs/implementation/sprint-1/wp1-scaffold.md §UQ-WP1-04`.

## Summary

`.loomweave/loomweave.db` and `.loomweave/config.json` are committed. WAL sidecars,
the shadow-DB intermediate, `tmp/`, `logs/`, and per-run raw LLM request/response
logs (`runs/*/log.jsonl`) are `.gitignore`d. `loomweave.yaml` lives at the project
root and is tracked under the user's existing repo-root `.gitignore`, not under
`.loomweave/.gitignore` (it's a user-edited config, not analysis state).

## Context

`.loomweave/` mixes artefact kinds that want different tracking posture:

- **Shared analysis state** (entities, edges, briefings, guidance) — diff-friendly
  via `loomweave db export --textual`; solo-developer and small-team cases benefit
  from having briefings versioned alongside the code they describe
  (`detailed-design.md §3 File layout`).
- **Runtime write-ahead files** (`*-wal`, `*-shm`) — SQLite bookkeeping that is
  process-local and meaningless on a different machine.
- **Shadow DB** (`loomweave.db.new`, `*.shadow.db`) — ADR-011's `--shadow-db`
  intermediate; deleted on successful atomic rename, would leak as junk
  otherwise.
- **Per-run LLM bodies** (`runs/<run_id>/log.jsonl`) — raw request/response
  bodies for audit. May contain source excerpts fine to ship to Anthropic
  but not appropriate to commit to a public repo.
- **Scratch** (`tmp/`, `logs/`) — volatile by definition.

Without this ADR, `loomweave install` has no normative place to look up the rules,
and every developer's install produces their own variant `.gitignore` by accident.

## Decision

`loomweave install` writes `.loomweave/.gitignore` with the following contents
(verbatim — the literal file lives at
`crates/loomweave-cli/src/install.rs` and ships as the v0.1 baseline):

```
*-wal
*-shm
*.db-wal
*.db-shm
*.shadow.db
*.db.new
tmp/
logs/
runs/*/log.jsonl
```

### Tracked

- `.loomweave/loomweave.db` — the main analysis store. SQLite diffs poorly; the
  `loomweave db export --textual` + `loomweave db merge-helper` pattern (detailed
  design §3 File layout) handles the team case.
- `.loomweave/config.json` — small, human-readable internal state (schema
  version, last run IDs).
- `.loomweave/.gitignore` itself — this file.
- `.loomweave/runs/<run_id>/config.yaml` — the snapshot of `loomweave.yaml` at run
  time. Material for provenance replay.
- `.loomweave/runs/<run_id>/stats.json` — run statistics.
- `.loomweave/runs/<run_id>/partial.json` — present only for partial runs;
  material for `--resume`.

### Excluded

- All SQLite WAL + SHM sidecars.
- All shadow-DB intermediates.
- `tmp/` and `logs/` (volatile scratch).
- `runs/*/log.jsonl` (raw LLM bodies — audit-local, not commit-appropriate).

### Out of scope for `.loomweave/.gitignore`

- `loomweave.yaml` (the user-edited config) lives at the *project root*, not
  inside `.loomweave/`. Its tracking is governed by the project's own repo-root
  `.gitignore`, which is the user's concern. Default posture: tracked.

### Opt-out for users who don't want the DB committed

`loomweave.yaml:storage.commit_db: false` (post-Sprint-1 knob; WP6 authors the
full `loomweave.yaml` schema). When false, Loomweave writes an additional
`.loomweave/.gitignore` line excluding `loomweave.db`, and emits
`loomweave db sync push/pull` commands. Not implemented in Sprint 1; the knob
is documented here so the future change has a home.

## Alternatives Considered

### Alternative 1: commit everything

**Pros**: no ignore list to maintain.

**Cons**: WAL sidecars break repos (they're process-local binary files); raw
LLM bodies may contain material the user does not want public.

**Why rejected**: blast radius of a single `git push` with `runs/*/log.jsonl`
committed is unbounded.

### Alternative 2: commit nothing

**Pros**: simplest — `.loomweave/` becomes entirely machine-local.

**Cons**: loses the "shared analysis state" benefit — briefings and guidance
are derived outputs that are expensive to rebuild. Small teams especially
benefit from having them versioned alongside the code.

**Why rejected**: the "enterprise rigor at lack of scale" posture favours
committing analytic state for small-team workflows. Users who want machine-local
analysis only opt out via `storage.commit_db: false`.

### Alternative 3: commit the DB but use git-lfs by default

**Pros**: keeps small-git-diff UX (LFS handles the binary file).

**Cons**: requires git-lfs installed on every developer machine; makes `loomweave
install` a multi-tool setup; adds failure modes (lfs server availability, large
file policy). v0.1 target workflows are solo/small-team where the straight-commit
path works; LFS is a v0.2+ knob.

**Why rejected**: premature infrastructure for the v0.1 audience.

## Consequences

### Positive

- Every `loomweave install` produces the same `.gitignore`. Ends per-developer
  drift on "what should be committed."
- WAL sidecars cannot accidentally land in a commit.
- Raw LLM bodies stay local to the developer that ran the analysis.
- `--shadow-db` intermediates (ADR-011) are excluded by the same list, so
  users adopting that mode don't discover an ignore gap post-hoc.

### Negative

- Committed SQLite DBs diff poorly by default. Mitigation: the
  `loomweave db export --textual` / merge-helper path (detailed-design §3) is
  the documented escape hatch.
- Adding a new excluded pattern requires either a Loomweave release or a
  user-side `.loomweave/.gitignore` edit. The post-v0.1 plan is to keep this
  file tool-owned; users adding their own ignores put them in the repo-root
  `.gitignore`, not here.

### Neutral

- `storage.commit_db: false` is a defined but unimplemented opt-out. Sprint 1
  ships with the commit-the-DB default only.

## Related Decisions

- [ADR-011](./ADR-011-writer-actor-concurrency.md) — names the shadow-DB
  intermediate; this ADR excludes it from git.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — cross-tool references
  rely on `loomweave.db` being available to readers (Filigree, Wardline); the
  commit-by-default posture keeps those references resolvable across machines.

## References

- [detailed-design.md §3 File layout](../v0.1/detailed-design.md#file-layout) —
  the prose version of this decision, now superseded by this ADR as the
  normative source.
- [wp1-scaffold.md UQ-WP1-04](../../implementation/sprint-1/wp1-scaffold.md) —
  the sprint-local resolution this ADR formalises.
