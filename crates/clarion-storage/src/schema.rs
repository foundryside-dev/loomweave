//! Schema migration runner.
//!
//! Migrations are embedded at compile time via `include_str!`. On apply, each
//! is run if not already recorded in `schema_migrations`. Running twice is a
//! no-op.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Result, StorageError};

struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "0001_initial_schema",
    sql: include_str!("../migrations/0001_initial_schema.sql"),
}];

/// Apply every migration not already recorded in `schema_migrations`.
///
/// The first migration creates the `schema_migrations` table itself, so the
/// initial lookup tolerates its absence.
///
/// # Errors
///
/// Returns [`StorageError::Migration`] with the failing version on SQL error
/// during apply. Returns [`StorageError::Sqlite`] on bookkeeping failures.
pub fn apply_migrations(conn: &mut Connection) -> Result<()> {
    let applied = read_applied_versions(conn)?;
    for m in MIGRATIONS {
        if applied.contains(&m.version) {
            tracing::debug!(version = m.version, "migration already applied");
            continue;
        }
        apply_one(conn, m)?;
    }
    Ok(())
}

fn read_applied_versions(conn: &Connection) -> Result<Vec<u32>> {
    // `.optional()?` converts only `Err(QueryReturnedNoRows)` to `Ok(None)` —
    // any other rusqlite error (DatabaseLocked, IoError, CorruptDb, ...)
    // propagates as `StorageError::Sqlite`. A bare `.ok()` here would silently
    // proceed to re-run 0001 on a locked or corrupt DB and surface as a
    // confusing "table already exists" error rather than the real cause.
    let table_exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table_exists.is_none() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let v: i64 = row?;
        let v_u32 = u32::try_from(v).map_err(|_| StorageError::Migration {
            version: 0,
            source: rusqlite::Error::IntegralValueOutOfRange(0, v),
        })?;
        out.push(v_u32);
    }
    Ok(out)
}

fn apply_one(conn: &mut Connection, m: &Migration) -> Result<()> {
    tracing::info!(version = m.version, name = m.name, "applying migration");
    conn.execute_batch(m.sql)
        .map_err(|source| StorageError::Migration {
            version: m.version,
            source,
        })?;
    // Defence in depth: the migration's own BEGIN/COMMIT has already committed,
    // including its own INSERT INTO schema_migrations. This second statement
    // handles only migrations that incorrectly omit their own record INSERT.
    // INSERT OR IGNORE is a no-op when the version already exists (normal case).
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_at) \
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![i64::from(m.version), m.name],
    )?;
    Ok(())
}

/// Count of applied migrations (for tests + install).
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if the query fails for reasons other than
/// the table not existing (in which case this returns `Ok(0)`).
pub fn applied_count(conn: &Connection) -> Result<u32> {
    // Same `.optional()?` rationale as `read_applied_versions`: only
    // `QueryReturnedNoRows` collapses to `None` (table absent → 0 migrations
    // applied). Any other rusqlite error propagates so callers see the real
    // failure (e.g. database locked) rather than a misleading 0.
    let table_exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table_exists.is_none() {
        return Ok(0);
    }
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
        row.get(0)
    })?;
    migration_count_to_u32(n)
}

fn migration_count_to_u32(n: i64) -> Result<u32> {
    u32::try_from(n).map_err(|_| StorageError::Migration {
        version: 0,
        source: rusqlite::Error::IntegralValueOutOfRange(0, n),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_count_conversion_rejects_overflow() {
        let err = migration_count_to_u32(i64::from(u32::MAX) + 1)
            .expect_err("overflowing migration count should error");
        assert!(matches!(
            err,
            StorageError::Migration {
                version: 0,
                source: rusqlite::Error::IntegralValueOutOfRange(0, _),
            }
        ));
    }
}
