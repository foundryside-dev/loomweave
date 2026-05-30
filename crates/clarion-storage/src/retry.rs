//! `BEGIN IMMEDIATE` with bounded `SQLITE_BUSY` retry (gap-register STO-05).
//!
//! Two distinct concerns motivate this, and `PRAGMA busy_timeout` addresses
//! neither completely:
//!
//! 1. **Deferred vs. immediate.** A plain `BEGIN` starts a *deferred*
//!    transaction that does not take the write lock until the first write.
//!    When that write needs to upgrade a read lock to a write lock and another
//!    connection already holds the write lock, `SQLite` returns `SQLITE_BUSY`
//!    *immediately* — the busy handler is **not** invoked for lock upgrades,
//!    because waiting could deadlock. `BEGIN IMMEDIATE` acquires the write lock
//!    up front, where `busy_timeout` *is* honored, so contention surfaces as a
//!    clean wait-then-retry at transaction start rather than a mid-statement
//!    failure that leaves a half-open transaction.
//!
//! 2. **Beyond the timeout.** `busy_timeout` caps how long `SQLite` blocks on a
//!    single lock attempt. Under sustained cross-process contention a writer
//!    can still see `SQLITE_BUSY` after the timeout elapses; an application
//!    retry with backoff gives the holder more chances to drain.
//!
//! Single-writer, single-process Clarion does not contend today; this helper
//! exists so the writer is correct the moment cross-process writers land
//! (STO-01 / V11-STO-01).

use std::time::Duration;

use rusqlite::{Connection, ErrorCode};

use crate::error::Result;

/// Bounded retry schedule for acquiring a write transaction.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total number of `BEGIN IMMEDIATE` attempts (>= 1).
    pub max_attempts: u32,
    /// Backoff before the second attempt; doubles each subsequent retry.
    pub initial_backoff: Duration,
    /// Upper bound the exponential backoff is clamped to.
    pub max_backoff: Duration,
}

impl RetryPolicy {
    /// The policy the writer actor uses: a handful of attempts with short,
    /// capped backoff. This sits *on top of* `PRAGMA busy_timeout=5000`, so the
    /// effective tolerance is the timeout plus these retries.
    #[must_use]
    pub fn writer_default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(200),
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::writer_default()
    }
}

/// `true` if `err` is a `SQLite` busy/locked error worth retrying.
fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.code == ErrorCode::DatabaseBusy || e.code == ErrorCode::DatabaseLocked
    )
}

/// Open a write transaction with `BEGIN IMMEDIATE`, retrying on a busy/locked
/// database according to `policy`.
///
/// On success the connection holds an open `IMMEDIATE` transaction; the caller
/// is responsible for the matching `COMMIT`/`ROLLBACK`. On a non-busy error the
/// helper returns immediately without retrying. After `policy.max_attempts`
/// busy results it returns the last busy error.
///
/// # Errors
///
/// Returns [`crate::StorageError::Sqlite`] with the underlying busy error after
/// exhausting retries, or the first non-busy error encountered.
pub fn begin_immediate(conn: &Connection, policy: &RetryPolicy) -> Result<()> {
    begin_immediate_inner(conn, policy, |_| {})
}

/// Test seam for [`begin_immediate`]: `on_busy` is invoked with the
/// 1-based attempt number that just failed, immediately before the backoff
/// sleep. Tests use it to release a competing lock deterministically (no
/// wall-clock guessing). Production callers use [`begin_immediate`], whose hook
/// is a no-op.
pub(crate) fn begin_immediate_inner(
    conn: &Connection,
    policy: &RetryPolicy,
    mut on_busy: impl FnMut(u32),
) -> Result<()> {
    let attempts = policy.max_attempts.max(1);
    let mut backoff = policy.initial_backoff;
    for attempt in 1..=attempts {
        match conn.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => return Ok(()),
            Err(err) if is_busy(&err) && attempt < attempts => {
                on_busy(attempt);
                if !backoff.is_zero() {
                    std::thread::sleep(backoff);
                }
                backoff = (backoff * 2).min(policy.max_backoff);
            }
            Err(err) => return Err(err.into()),
        }
    }
    // Unreachable: the loop either returns Ok, returns the final Err on the
    // last attempt, or retries. Kept as a defensive fallback.
    unreachable!("begin_immediate loop must return within max_attempts")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        // Disable SQLite's own busy handler so contention surfaces as an
        // immediate SQLITE_BUSY and the application retry loop is what we test.
        conn.busy_timeout(Duration::from_millis(0))
            .expect("busy_timeout");
        conn
    }

    /// Two connections against the same on-disk DB, busy handler disabled.
    fn shared_pair(path: &std::path::Path) -> (Connection, Connection) {
        let open = || {
            let c = Connection::open(path).expect("open");
            c.busy_timeout(Duration::from_millis(0))
                .expect("busy_timeout");
            c
        };
        (open(), open())
    }

    #[test]
    fn begin_immediate_succeeds_with_no_contention() {
        let conn = busy_conn();
        begin_immediate(&conn, &RetryPolicy::writer_default()).expect("begin");
        // The transaction is open: a write succeeds and commits cleanly.
        conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1); COMMIT")
            .expect("write inside immediate tx");
    }

    #[test]
    fn begin_immediate_exhausts_and_returns_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contended.db");
        let (holder, contender) = shared_pair(&path);

        // Holder takes the write lock and never releases it during the call.
        holder
            .execute_batch("BEGIN IMMEDIATE; CREATE TABLE t (x)")
            .expect("holder acquires write lock");

        let policy = RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        };
        let err = begin_immediate(&contender, &policy)
            .expect_err("contender must fail while the lock is held");
        assert!(
            matches!(&err, crate::StorageError::Sqlite(e) if is_busy(e)),
            "expected a busy/locked error, got {err:?}"
        );
    }

    #[test]
    fn begin_immediate_retries_then_succeeds_when_lock_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contended.db");
        let (holder, contender) = shared_pair(&path);

        holder
            .execute_batch("BEGIN IMMEDIATE; CREATE TABLE t (x)")
            .expect("holder acquires write lock");

        // Deterministic, single-threaded: when the first attempt fails busy,
        // release the holder's lock from inside the retry hook so the next
        // attempt finds the database free. No threads, no wall-clock races.
        let mut released = false;
        let policy = RetryPolicy {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        };
        begin_immediate_inner(&contender, &policy, |attempt| {
            if attempt == 1 && !released {
                holder.execute_batch("COMMIT").expect("release holder lock");
                released = true;
            }
        })
        .expect("contender should acquire the lock after it is released");
        assert!(released, "the retry hook should have fired at least once");
    }
}
