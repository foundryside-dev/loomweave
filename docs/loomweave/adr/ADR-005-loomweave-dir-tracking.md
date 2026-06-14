# ADR-005: `.loomweave/` Directory Git-Tracking Policy

**Status**: Accepted; amended by ADR-041, ADR-046; **`loomweave.db` tracking
reversed by C1 (weft-d822a7de2d), 2026-06-08; `config.json` stub removed by
C-11(a) (weft-da23c1f6bd), 2026-06-13**

> **C1 reversal (weft-d822a7de2d), 2026-06-08:** `loomweave.db` is **no longer
> committed by default — it is `.gitignore`d.** The original decision (below)
> committed the DB so small teams could share briefings/guidance. Two facts that
> postdate it overturn that default: (1) tracking a file that mutates on every
> `analyze`/`scan` leaves a permanently dirty working tree, which **blocks legis
> from signing** the project (legis refuses to sign a dirty tree); and (2) the DB
> is a *regenerable orientation cache* — `loomweave analyze` rebuilds the
> structural graph with **no LLM calls**, so the expensive part is only the lazy
> summary cache, which is acceptably machine-local. Sharing summaries across a
> team becomes a future **opt-in** (`storage.commit_db: true`, the inverse of the
> old opt-out), not the default. The `GITIGNORE_CONTENTS` template in
> `crates/loomweave-cli/src/install.rs` remains the source of truth and now lists
> `loomweave.db`. Sections below are kept for the historical decision and read
> with this reversal applied.

> **ADR-046 amendment:** the directory tracked by this policy moved from
> `.loomweave/` to `.weft/loomweave/` (Weft store consolidation, clean break).
> The tracked-vs-ignored split below is unchanged — only the parent path. Read
> every `.loomweave/` path below as `.weft/loomweave/`.
>
> **C-11(a) amendment (weft-da23c1f6bd), 2026-06-13:** `loomweave install` no
> longer writes `.weft/loomweave/config.json`. The old
> `{schema_version,last_run_id}` stub had no live reader; schema state lives in
> SQLite, and run metadata lives under `runs/`.

**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: `loomweave install` must write a `.gitignore` inside `.loomweave/` that
separates committed analysis state from volatile per-run artefacts. Sprint 1 WP1
Task 5 is the authoring trigger; before this ADR, the rules were only proposed
in `docs/implementation/sprint-1/wp1-scaffold.md §UQ-WP1-04`.

## Summary

`.loomweave/loomweave.db` is **`.gitignore`d** (C1 reversal — a regenerable
cache that would otherwise dirty the tree on every run). WAL sidecars, the
shadow-DB intermediate, `tmp/`, `logs/`, and per-run raw LLM request/response
logs (`runs/*/log.jsonl`) are `.gitignore`d. `loomweave.yaml` lives at the
project root and is tracked under the user's existing repo-root `.gitignore`,
not under `.loomweave/.gitignore` (it's a user-edited config, not analysis
state). No `.loomweave/config.json` stub is written.

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
(the literal file lives at `crates/loomweave-cli/src/install.rs` —
`GITIGNORE_CONTENTS` — which is the source of truth; the v0.1 baseline has since
grown the `ephemeral.port` (ADR-044), `embeddings.db` (ADR-040), `instance_id`,
and `*.lock` entries):

```
loomweave.db
ephemeral.port
*-wal
*-shm
*.db-wal
*.db-shm
*.shadow.db
*.db.new
embeddings.db
instance_id
*.lock
tmp/
logs/
runs/*/log.jsonl
```

### Tracked

- ~~`.loomweave/loomweave.db`~~ — **reversed by C1 (weft-d822a7de2d): now
  Excluded** (see below). The DB is a regenerable orientation cache, and tracking
  a file that mutates every run dirtied the tree and blocked legis signing.
- `.loomweave/.gitignore` itself — this file.
- `.loomweave/runs/<run_id>/config.yaml` — the snapshot of `loomweave.yaml` at run
  time. Material for provenance replay.
- `.loomweave/runs/<run_id>/stats.json` — run statistics.
- `.loomweave/runs/<run_id>/partial.json` — present only for partial runs;
  material for `--resume`.

### Excluded

- `loomweave.db` (C1 reversal, weft-d822a7de2d) — the index DB. A regenerable
  orientation cache: `loomweave analyze` rebuilds the structural graph with no
  LLM calls, and the only expensive content (the lazy summary cache) is
  acceptably machine-local. Committing it left a permanently dirty tree (it
  mutates on every `analyze`/`scan`), which blocked legis from signing the
  project. Teams that want to share briefings opt **in** via
  `storage.commit_db: true` (see the opt-in note below).
- All SQLite WAL + SHM sidecars.
- All shadow-DB intermediates.
- `tmp/` and `logs/` (volatile scratch).
- `runs/*/log.jsonl` (raw LLM bodies — audit-local, not commit-appropriate).
- `ephemeral.port` (ADR-044) — the read-API live port discovery file, present
  only while `serve` runs and rewritten per bind.
- `embeddings.db` (ADR-040) — the semantic-search sidecar; large and rebuildable.
- `instance_id` and `*.lock` — the per-project `serve` fingerprint and the
  analyze advisory lock (`loomweave.lock`, fs2). Both are process-/machine-local
  runtime state, never durable (clarion-7381e6382d).

### Out of scope for `.loomweave/.gitignore`

- `loomweave.yaml` (the user-edited config) lives at the *project root*, not
  inside `.loomweave/`. Its tracking is governed by the project's own repo-root
  `.gitignore`, which is the user's concern. Default posture: tracked.

### Opt-in for teams who *do* want the DB committed (C1 reversal)

Post-C1 the default is **ignored**, so the knob inverts: `loomweave.yaml:
storage.commit_db: true` is the opt-**in** for teams that want briefings/guidance
versioned alongside the code. When true, Loomweave omits the `loomweave.db` line
from the generated `.gitignore` (and the team accepts the dirty-tree / legis
consequence, or commits via a checkpointed snapshot). Still unimplemented — the
knob is documented here so the future change has a home. (Before C1 this was the
inverse `commit_db: false` opt-*out*; the commit-the-DB posture was the default.)

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

**Why rejected** (originally): the "enterprise rigor at lack of scale" posture
favoured committing analytic state for small-team workflows. Users who wanted
machine-local analysis only opted out via `storage.commit_db: false`.

> **Superseded for `loomweave.db` by C1 (weft-d822a7de2d):** this alternative is
> now the chosen posture *for the DB* — it is machine-local by default. The
> "expensive to rebuild" con is narrower than it read in 2026-04: the structural
> graph regenerates from `loomweave analyze` with no LLM calls, and only the lazy
> summary cache carries real cost. The decisive new factor (not in view at the
> original decision) is that a committed, ever-mutating DB blocks legis signing.
> the `runs/` provenance metadata remains tracked; the old `config.json` stub
> is no longer written.

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

- ~~Committed SQLite DBs diff poorly by default.~~ Moot post-C1: the DB is no
  longer committed by default. A fresh checkout has no index until `loomweave
  install`/`analyze` rebuilds it (cheap — no LLM calls); the lazy summary cache
  is re-paid per machine unless a team opts into `commit_db: true`.
- Adding a new excluded pattern requires either a Loomweave release or a
  user-side `.loomweave/.gitignore` edit. The post-v0.1 plan is to keep this
  file tool-owned; users adding their own ignores put them in the repo-root
  `.gitignore`, not here.

### Neutral

- `storage.commit_db` is a defined but unimplemented knob. Post-C1 its sense is
  inverted: `true` is the opt-**in** to commit the DB; the default (DB ignored)
  needs no knob.

## Related Decisions

- [ADR-011](./ADR-011-writer-actor-concurrency.md) — names the shadow-DB
  intermediate; this ADR excludes it from git.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — cross-tool references
  rely on `loomweave.db` being available to readers (Filigree, Wardline). Post-C1
  the DB is no longer committed, so a reader on a fresh checkout resolves
  references against a locally-rebuilt index (`loomweave analyze`) rather than a
  pulled one; the structural graph it depends on regenerates with no LLM calls.

## References

- [detailed-design.md §3 File layout](../v0.1/detailed-design.md#file-layout) —
  the prose version of this decision, now superseded by this ADR as the
  normative source.
- [wp1-scaffold.md UQ-WP1-04](../../implementation/sprint-1/wp1-scaffold.md) —
  the sprint-local resolution this ADR formalises.
