-- Migration 0008: runs.owner_pid + runs.heartbeat_at (H7 stale-running
-- reconciliation).
--
-- `owner_pid` is diagnostic ownership for the process that opened/resumed the
-- run. `heartbeat_at` is the cross-platform freshness signal readers can use
-- to identify abandoned `running` rows after an unclean process death.

BEGIN;

ALTER TABLE runs ADD COLUMN owner_pid INTEGER;
ALTER TABLE runs ADD COLUMN heartbeat_at TEXT;

CREATE INDEX ix_runs_running_heartbeat
    ON runs(status, heartbeat_at)
    WHERE status = 'running';

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (8, '0008_run_owner_heartbeat', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
