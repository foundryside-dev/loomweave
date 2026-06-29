# PDR-0007: Dispose of the stale `weft/legis-conformance` branch; carry its intent forward as a tracked gap

- **Date:** 2026-06-29
- **Status:** accepted (within the grant — kill a stale/orphaned work-item; owner-authorized the branch deletion explicitly)
- **PRD:** none (orphaned branch triaged during repo cleanup, not a PRD-scoped bet)
- **Tracker:** clarion-0715faa9d6 (new — the carried-forward conformance-golden gap, open); obsolete branch commit `9c30ce0` recorded there for reference

## Context

A repo-hygiene cleanup swept the remote branches. Most were cleanly merged (PRs
#53/#54/#74/#75/#76/#78) and deleted as routine housekeeping. One branch,
`origin/weft/legis-conformance` (single unique commit `9c30ce0`,
"test(conformance): legis git-rename consumer reaches the bar", 2026-06-26, **no
PR**), needed a real disposition call: was it unmerged work that still needs
landing?

Investigation (verified, not assumed): the branch adds a byte-pinned cross-member
conformance test + vendored golden for the legis→loomweave git-rename seam. But it
was cut **before PR #73** (`7804ccf`, re-point the consumer `/git/renames` →
`/git/rename-feed`, now in main). It tests the now-**deleted** `parse_legis_rename_json`
against the obsolete `/git/renames` array shape. A throwaway cherry-pick onto main
applied textually but **failed to compile** (`cannot find function
parse_legis_rename_json`; main has `parse_legis_rename_feed_json`). So the branch's
implementation is obsolete and unmergeable.

Crucially, the branch's *intent* is still unmet: main itself documents the gap at
`sei_git.rs:699-700` — "The durable fix (a shared two-way conformance vector
pinning the canonical keys) is **deferred — it needs an agreed vector home**." main
has good *unit* coverage of the new parser (`parse_legis_rename_feed_json` +
`classify_legis_rename_feed_json` silent-under-carry), but no shared byte-pinned
producer/consumer golden.

## Options

1. **Merge the branch** — rejected: doesn't compile against main; tests deleted code.
2. **Keep the branch as a reference, file the gap** — viable but leaves a
   non-compiling orphan on origin as latent confusion.
3. **Delete the branch, file the gap, preserve the commit ref in the issue**
   (chosen) — removes the obsolete code, carries the intent forward as a tracked,
   rewrite-shaped task, and keeps `9c30ce0` recoverable (issue body + git reflog).

## The call

**Option 3.** Deleted `origin/weft/legis-conformance`; filed **clarion-0715faa9d6**
(P2 task, labels federation/conformance/loomweave/sei) specifying the *rewrite*:
freeze a golden from legis's real `GET /git/rename-feed` (the new
`{committed:[…]}` envelope), drive the real `parse_legis_rename_feed_json`, and
byte-pin the blob sha1 — explicitly flagging the cross-member "agreed vector home"
(legis vendoring the identical bytes) as **owner-gated/outward-facing**. The
obsolete commit `9c30ce0` is named in the issue as a starting template.

The branch deletion itself was owner-authorized in-session (the standing
"remote-branch deletion needs explicit OK" rule was satisfied); the unique commit
is not lost (merged-branch commits are in main; `9c30ce0` is in the reflog + the
issue).

## Reversal trigger

Reopen this disposition only if the deleted branch turns out to have carried
unique, still-valid work beyond `9c30ce0` — i.e. `git show 9c30ce0` (or the issue
body) reveals content that is NOT obsolete against the `/git/rename-feed` repoint.
Verified false at decision time (the single commit tests the deleted parser). The
forward work itself is now governed by clarion-0715faa9d6, not by this branch.
