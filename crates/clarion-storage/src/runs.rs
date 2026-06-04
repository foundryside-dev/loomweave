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
            AND heartbeat_at IS NOT NULL \
            AND heartbeat_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2)",
        params![failure_stats, STALE_RUNNING_HEARTBEAT_SQL],
    )?;
    Ok(changed)
}
