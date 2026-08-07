//! Run-lifecycle repair helpers.

use rusqlite::{Connection, params};

use crate::Result;

/// Running rows older than this heartbeat window are considered abandoned.
///
/// The value is deliberately conservative: normal analyze runs should refresh
/// `heartbeat_at` at run open/resume and at writer batch boundaries. A 24-hour
/// gap is far beyond expected local analyze duration while still preventing
/// dead rows from poisoning status forever.
const STALE_RUNNING_HEARTBEAT_SQL: &str = "-24 hours";

/// Mark stale `running` rows as failed.
///
/// This is idempotent and safe to call from analyze startup or diagnostic read
/// paths. It uses the heartbeat rather than probing `owner_pid` so behavior is
/// portable across Unix/macOS/Windows and testable without process tricks.
///
/// # Errors
///
/// Returns `SQLite` errors from the underlying `UPDATE`.
pub fn mark_stale_running_runs_failed(conn: &Connection) -> Result<usize> {
    let failure_stats = serde_json::json!({
        "failure_reason": "analyze run abandoned: stale heartbeat",
    })
    .to_string();
    let changed = conn.execute(
        "UPDATE runs \
            SET status = 'failed', \
                completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                stats = ?1, \
                owner_pid = NULL \
          WHERE status = 'running' \
            AND ( \
                heartbeat_at IS NULL \
                OR heartbeat_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2) \
            )",
        params![failure_stats, STALE_RUNNING_HEARTBEAT_SQL],
    )?;
    Ok(changed)
}

/// Mark every `running` row failed because its builder is provably gone.
///
/// The heartbeat-window variant above ([`mark_stale_running_runs_failed`]) is
/// deliberately conservative — 24 hours — because it has no liveness evidence
/// beyond the row itself. This variant has the opposite contract: **the
/// caller must have already proved no builder is alive** before calling, and
/// must hold that proof for the duration of the call. Concretely,
/// `loomweave-mcp`'s worktree bootstrap calls this while *holding* the
/// per-worktree analyze lock (`<repository-store>/worktrees/<stable-id>.lock`):
/// a live analyze holds that lock exclusively from before its `BeginRun`
/// insert until after its final transaction lands, so any `running` row that
/// exists while another process holds the lock was abandoned by a builder
/// that died uncleanly (OOM-kill, `kill -9`, reboot) — no heartbeat wait
/// needed.
///
/// # Errors
///
/// Returns `SQLite` errors from the underlying `UPDATE`.
pub fn mark_abandoned_running_runs_failed(conn: &Connection) -> Result<usize> {
    let failure_stats = serde_json::json!({
        "failure_reason": "analyze run abandoned: no live process holds the analyze lock",
    })
    .to_string();
    let changed = conn.execute(
        "UPDATE runs \
            SET status = 'failed', \
                completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                stats = ?1, \
                owner_pid = NULL \
          WHERE status = 'running'",
        params![failure_stats],
    )?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_migrated_db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open sqlite");
        crate::schema::apply_migrations(&mut conn).expect("apply migrations");
        conn
    }

    #[test]
    fn abandoned_repair_fails_every_running_row_and_leaves_terminal_rows_alone() {
        let conn = open_migrated_db();
        for (id, status) in [
            ("r-run", "running"),
            ("r-done", "completed"),
            ("r-old-fail", "failed"),
        ] {
            conn.execute(
                "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
                 VALUES (?1, '2026-01-01T00:00:00.000Z', NULL, '{}', '{}', ?2)",
                params![id, status],
            )
            .expect("seed run row");
        }

        let changed = mark_abandoned_running_runs_failed(&conn).expect("repair");
        assert_eq!(changed, 1, "exactly the running row is repaired");

        let (repaired_status, completed_at, repaired_reason): (String, Option<String>, String) =
            conn.query_row(
                "SELECT status, completed_at, stats FROM runs WHERE id = 'r-run'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read repaired row");
        assert_eq!(repaired_status, "failed");
        assert!(completed_at.is_some(), "a repaired row is terminal");
        assert!(
            repaired_reason.contains("no live process holds the analyze lock"),
            "the failure reason must name the liveness evidence: {repaired_reason}"
        );

        let done_status: String = conn
            .query_row("SELECT status FROM runs WHERE id = 'r-done'", [], |row| {
                row.get(0)
            })
            .expect("read completed row");
        assert_eq!(done_status, "completed");
    }
}
