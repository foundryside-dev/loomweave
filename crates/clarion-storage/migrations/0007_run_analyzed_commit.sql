-- Migration 0007: runs.analyzed_at_commit (WS9 / SEI §6 — REQ-C-05).
--
-- Records the git HEAD commit a run analyzed against, so the *next* run can
-- query renames over the committed window `<prior_commit>..HEAD` and select the
-- `legis` git-rename provider (which serves committed renames via `git log -M`).
-- Without a stored prior commit the analyze pipeline only ever had the
-- working-tree window (empty base), which the capability-aware selector routes
-- to the shell source — so `legis` was never operatively consulted.
--
-- Nullable: NULL when the corpus is not a git repo, when `git rev-parse HEAD`
-- fails, or for rows written before this migration. The committed window is
-- skipped whenever the prior commit is absent, so a NULL degrades cleanly to
-- the pre-WS9 working-tree-only behaviour.

-- Wrapped in a single transaction (mirroring 0002) so the ALTER and the
-- migration record commit together; an interruption mid-way must not leave the
-- column in place without the schema_migrations.version=7 row (the next startup
-- would rerun the ALTER and die on a duplicate column).
BEGIN;

ALTER TABLE runs ADD COLUMN analyzed_at_commit TEXT;

-- Record the migration inside the same transaction (defence-in-depth: the
-- runner's INSERT OR IGNORE in apply_one then no-ops). Matches 0001/0002.
INSERT INTO schema_migrations (version, name, applied_at)
VALUES (7, '0007_run_analyzed_commit', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
