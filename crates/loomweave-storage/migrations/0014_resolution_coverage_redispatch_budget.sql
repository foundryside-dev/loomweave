-- Migration 0014: collateral flag + re-dispatch budget on the per-file
-- resolution coverage claim (clarion-3e517d4aff follow-through).
--
-- 0013 made a transient-degraded file re-dispatch on every incremental run.
-- On a project where one pathological file exhausts pyright's restart budget
-- part-way through a run, every file dispatched AFTER it is degraded as
-- collateral -- and re-dispatching that whole set behind the same file next
-- run reproduces the same poison, so nothing converges and each incremental
-- analyze pays the full cost forever.
--
-- Two additive columns per facet + one counter fix that:
-- - `*_collateral`: the plugin says the degradation was caused by an EARLIER
--   file's failure (resolver already disabled when this file arrived), not by
--   this file's own content. The host dispatches collateral files first and
--   self-inflicted ones last, so the troublemaker can only poison what
--   follows it.
-- - `redispatch_attempts`: consecutive runs in which the file stayed
--   transient-degraded. Past the budget the file is still reported degraded
--   on the read surface, but no longer forces re-dispatch until its bytes
--   change (or `--no-incremental`).

BEGIN;

ALTER TABLE source_file_resolution_coverage
ADD COLUMN calls_collateral INTEGER NOT NULL DEFAULT 0 CHECK (calls_collateral IN (0, 1));
ALTER TABLE source_file_resolution_coverage
ADD COLUMN references_collateral INTEGER NOT NULL DEFAULT 0
    CHECK (references_collateral IN (0, 1));
ALTER TABLE source_file_resolution_coverage
ADD COLUMN redispatch_attempts INTEGER NOT NULL DEFAULT 0 CHECK (redispatch_attempts >= 0);

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (14, '0014_resolution_coverage_redispatch_budget', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
