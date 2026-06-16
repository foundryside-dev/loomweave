# ADR-045: Worktree-Source Staleness via Hardened `git ls-files --others`

**Status**: Accepted
**Date**: 2026-06-06
**Deciders**: qacona@gmail.com
**Context**: clarion-26c7e52027 (dogfood: `staleness:"fresh"` lied while
un-indexed top-level modules sat in the working tree) and its follow-up
clarion-d9cf8bcfa9. Builds on ADR-013/ADR-021 (untrusted-corpus posture) and the
`hardened_git` helper (clarion-4b5a8aff54).

## Summary

`project_snapshot` (the `loomweave://context` resource, the `loomweave hook
session-start` banner, and `project_status_get`) now reports a third "needs
re-analyze" signal beyond the mtime/structural passes: **worktree-source
drift**. When the index is otherwise mtime-fresh but the working tree contains an
**untracked source file of an already-indexed type**, the verdict becomes
`Staleness::StaleWorktree` and the snapshot carries `worktree_dirty: Some(true)`.

Detection uses a **hardened, ignore-aware, hash-free** `git ls-files --others
--exclude-standard`, scoped to the file extensions Loomweave has actually
ingested. It is fail-soft: `worktree_dirty` is `None` outside a git work tree,
when git is unavailable, or when nothing is ingested.

## Context

The mtime/structural freshness passes (ADR note in `snapshot.rs`) watch the
*direct parent directories of ingested files*, and deliberately never watch the
project root (`analyze` writes `.loomweave/` under it, which would wedge every
check to a permanent `Stale`). The documented consequence: a brand-new
**top-level** directory of source the index has never seen is invisible — it
reports `Fresh`. That is the exact dogfood failure: new specimen modules added to
a tree, `project_status_get` still says `fresh`, and an agent trusts it and gives
wrong "what calls X" answers.

Catching un-indexed worktree source requires looking at the working tree. The
untrusted-corpus posture forbids the obvious tool: `git status` must **hash**
working-tree content to detect modifications, which runs a repo-controlled
`filter.<drv>.clean` selected by `$GIT_DIR/info/attributes` — a code-execution
vector no git config can disable (see `hardened_git` module docs). That is why
the SEI rename diff and `index_diff` use `git diff --cached` and a stat-based
per-file scan, never `git status`.

## Decision

Use `git ls-files --others --exclude-standard` through `hardened_git_command`,
exposed as `loomweave_core::list_untracked_files`.

1. **Safe under the untrusted-corpus posture.** `ls-files --others` *enumerates*
   untracked, non-ignored paths; it never computes blob hashes of working-tree
   content, so the `filter.clean` vector is never triggered. This is verified
   empirically, not by reasoning alone: `hardened_git::tests::
   ls_files_others_does_not_run_clean_filter` booby-traps a repo with `* filter=pwn`
   in `$GIT_DIR/info/attributes` and a repo-local `filter.pwn.clean` that would
   create a marker file, then asserts the marker does **not** appear after the
   call. The hardened command also sets `core.fsmonitor=false` and
   `GIT_OPTIONAL_LOCKS=0`, so no fsmonitor program runs and the index is not
   written.

2. **No false-positives.** A naive "any untracked file ⇒ dirty" would flag a
   scratch `notes.txt` and make a genuinely-fresh index look dirty. The signal is
   therefore **scoped to the file extensions present in `entities.source_file_path`**
   — only an untracked file whose extension Loomweave actually ingests counts. An
   untracked `notes.txt` never flags; an untracked `hub.py` (when `.py` is
   indexed) does. `--exclude-standard` further drops `.gitignore`d paths, so
   build dirs, virtualenvs, and the ignored `.loomweave/` sidecars never appear.

3. **Verdict + field.** When mtime/structural say `Fresh` and the worktree signal
   is positive, the verdict is `Staleness::StaleWorktree` (serialized
   `"stale_worktree"`); `worktree_dirty: Option<bool>` carries the raw signal on
   every snapshot and in `project_status_get`. `StaleWorktree` is treated as
   "stale" by orientation consumers; the session-start banner names the remedy
   (`loomweave analyze`).

4. **Fail-soft.** Any git failure, a non-repo working directory, or an empty
   ingested-extension set yields `worktree_dirty: None` and leaves the
   mtime-derived verdict unchanged. Detection never sets `degraded` (a missing
   git binary is environmental) and never errors — `project_snapshot` stays
   infallible.

### What it does NOT cover (deliberate scope)

- **Untracked-source enumeration runs git at session start.** It is hash-free and
  ignore-pruned (comparable to `git status` minus hashing), and fail-soft, but it
  is the first git invocation in the session-start snapshot path. Accepted for the
  honesty win.
- **Modified-but-unstaged edits to *tracked* indexed files** remain the job of the
  stat-based mtime pass (→ `Stale`) and `index_diff_get`'s `diff --cached`; they
  are not what `ls-files --others` reports.
- **Mid-serve committable snapshots** are still `loomweave db backup`'s job
  (ADR-005 note; clarion-cdee445ed8), unrelated to this verdict.

## Consequences

- `Staleness` gains a `StaleWorktree` variant — a wire-vocabulary addition to
  `loomweave://context` and `project_status_get` (`"stale_worktree"`). Consumers
  that switch on `staleness` must handle it; `orientation` treats it as stale.
- `ProjectSnapshot` gains `worktree_dirty: Option<bool>`, surfaced on the context
  resource and `project_status_get`.
- `loomweave_core` gains `list_untracked_files`, the only sanctioned untracked
  probe, carrying the security contract + the empirical test.
- The session-start banner gives a concrete `StaleWorktree` line instead of only
  the `Fresh` caveat when un-indexed source is present in a git repo.

## Alternatives Considered

- **`git status --porcelain`** — rejected: hashes the working tree, re-opening the
  `filter.clean` RCE the whole `hardened_git` posture exists to close.
- **commit-mismatch (`rev-parse HEAD` vs `analyzed_at_commit`) and/or
  `diff --cached` only** — rejected as the *sole* signal: both report "clean" for
  the reported untracked-new-file case, i.e. misleadingly-clean — the original bug
  wearing a new field name.
- **Watching the project root's mtime** — rejected: the root is poisoned by
  `.loomweave/` writes and by unrelated top-level churn (editor temp files,
  `.DS_Store`), trading a false-negative for frequent false-positives.
- **Prose-only honest banner (no detection)** — shipped first as the conservative
  mitigation (clarion-26c7e52027) and retained for the non-git case; this ADR adds
  real detection where a git work tree makes it safe and accurate.
