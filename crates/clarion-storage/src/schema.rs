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

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_initial_schema",
        sql: include_str!("../migrations/0001_initial_schema.sql"),
    },
    Migration {
        version: 2,
        name: "0002_briefing_blocked",
        sql: include_str!("../migrations/0002_briefing_blocked.sql"),
    },
    Migration {
        version: 3,
        name: "0003_wardline_taint_facts",
        sql: include_str!("../migrations/0003_wardline_taint_facts.sql"),
    },
    Migration {
        version: 4,
        name: "0004_sei_prior_index",
        sql: include_str!("../migrations/0004_sei_prior_index.sql"),
    },
    Migration {
        version: 5,
        name: "0005_sei",
        sql: include_str!("../migrations/0005_sei.sql"),
    },
    Migration {
        version: 6,
        name: "0006_wardline_taint_sei",
        sql: include_str!("../migrations/0006_wardline_taint_sei.sql"),
    },
    Migration {
        version: 7,
        name: "0007_run_analyzed_commit",
        sql: include_str!("../migrations/0007_run_analyzed_commit.sql"),
    },
];

/// Highest migration version known to this build. Mirrored into the
/// `SQLite` `user_version` header (STO-02) so a future-built database is
/// refused at open instead of silently corrupting state.
pub const CURRENT_SCHEMA_VERSION: u32 = 7;

const _CURRENT_SCHEMA_VERSION_MATCHES_LAST_MIGRATION: () = {
    // Compile-time check: `CURRENT_SCHEMA_VERSION` must equal the highest
    // version in `MIGRATIONS`. If a new migration is added without bumping
    // the constant (or vice versa), this assertion fails to compile.
    assert!(
        MIGRATIONS[MIGRATIONS.len() - 1].version == CURRENT_SCHEMA_VERSION,
        "CURRENT_SCHEMA_VERSION must equal the highest MIGRATIONS[].version"
    );
};

/// Apply every migration not already recorded in `schema_migrations`.
///
/// The first migration creates the `schema_migrations` table itself, so the
/// initial lookup tolerates its absence.
///
/// After all pending migrations apply, the `SQLite` header `user_version` is
/// written to [`CURRENT_SCHEMA_VERSION`]. A `user_version` strictly greater
/// than [`CURRENT_SCHEMA_VERSION`] at entry is refused via
/// [`verify_user_version`] (closes STO-02 forward-incompatibility check).
///
/// # Errors
///
/// Returns [`StorageError::FutureUserVersion`] if the database was written
/// by a newer Clarion build.
/// Returns [`StorageError::Migration`] with the failing version on SQL error
/// during apply. Returns [`StorageError::Sqlite`] on bookkeeping failures.
pub fn apply_migrations(conn: &mut Connection) -> Result<()> {
    verify_user_version(conn)?;
    let applied = read_applied_versions(conn)?;
    for m in MIGRATIONS {
        if applied.contains(&m.version) {
            tracing::debug!(version = m.version, "migration already applied");
            continue;
        }
        apply_one(conn, m)?;
    }
    apply_user_version(conn)?;
    Ok(())
}

/// Refuse to operate on a database whose `user_version` is strictly greater
/// than [`CURRENT_SCHEMA_VERSION`].
///
/// Equal or less is accepted: equal means the schema is current, less means
/// either a fresh DB (`user_version=0`) or a DB awaiting in-flight migrations
/// — both are handled by [`apply_migrations`]. The writer-actor calls this
/// directly (without invoking the migration runner) so a forward-incompatible
/// file is rejected at `Writer::spawn` time.
///
/// # Errors
///
/// Returns [`StorageError::FutureUserVersion`] when `user_version >
/// CURRENT_SCHEMA_VERSION`. Returns [`StorageError::Sqlite`] if the PRAGMA
/// query fails.
pub fn verify_user_version(conn: &Connection) -> Result<()> {
    let raw: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    // SQLite stores user_version as a 32-bit integer; rusqlite returns i64.
    // Negative values are unreachable in normal use (we only set u32 values);
    // clamp via `try_from` so an out-of-range value surfaces explicitly
    // rather than silently truncating.
    let found = u32::try_from(raw).map_err(|_| {
        StorageError::PragmaInvariant(format!(
            "PRAGMA user_version returned out-of-range value {raw}; expected 0..=u32::MAX"
        ))
    })?;
    if found > CURRENT_SCHEMA_VERSION {
        return Err(StorageError::FutureUserVersion {
            found,
            current: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Write `PRAGMA user_version = CURRENT_SCHEMA_VERSION`. Idempotent — writing
/// the same value is cheap (it touches the `SQLite` header page). Called after
/// the migration runner has applied every pending migration.
fn apply_user_version(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!("PRAGMA user_version = {CURRENT_SCHEMA_VERSION};"))?;
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

    fn entities_has_briefing_blocked(conn: &Connection) -> bool {
        conn.prepare("SELECT 1 FROM pragma_table_xinfo('entities') WHERE name = 'briefing_blocked'")
            .and_then(|mut stmt| stmt.exists([]))
            .unwrap_or(false)
    }

    #[test]
    fn briefing_blocked_is_added_by_an_upgrade_migration_not_the_initial() {
        // An existing v1 database (created before briefing_blocked) must gain
        // the column on upgrade. If the column lives in the already-applied
        // initial migration, existing DBs at schema_migrations.version=1 skip
        // it forever and project_status hits `no such column`. Reproduce: apply
        // only the initial migration, confirm the column is absent, then run
        // the full migration runner and confirm an upgrade migration adds it.
        let mut conn = Connection::open_in_memory().unwrap();
        apply_one(&mut conn, &MIGRATIONS[0]).expect("apply initial migration");
        assert!(
            !entities_has_briefing_blocked(&conn),
            "briefing_blocked must not be defined by the initial migration (0001)"
        );

        apply_migrations(&mut conn).expect("apply pending migrations");
        assert!(
            entities_has_briefing_blocked(&conn),
            "an upgrade migration must add briefing_blocked to an existing v1 DB"
        );
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, i64::from(CURRENT_SCHEMA_VERSION));
    }

    #[test]
    fn migration_0002_rolls_back_the_column_when_a_later_statement_fails() {
        // 0002 must apply its ALTER + CREATE INDEX atomically. If the column is
        // added under autocommit and a later statement fails, the DB is left
        // with `briefing_blocked` present but no schema_migrations.version=2
        // row — the next startup reruns the ALTER and dies on duplicate column,
        // blocking upgrade. Inject that failure: squat the index name so 0002's
        // CREATE INDEX fails on its second statement, then prove (on a fresh
        // connection, reading committed state) that the ALTER did not survive.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("rollback.db");
        {
            let mut conn = Connection::open(&db).unwrap();
            apply_one(&mut conn, &MIGRATIONS[0]).expect("apply initial migration");
            // Squat the index name 0002 will try to create.
            conn.execute_batch("CREATE INDEX ix_entities_briefing_blocked ON entities(id);")
                .expect("pre-create colliding index");
            apply_one(&mut conn, &MIGRATIONS[1])
                .expect_err("0002 must fail when its CREATE INDEX collides");
            // Drop conn here → any open transaction rolls back on close.
        }
        let conn = Connection::open(&db).unwrap();
        assert!(
            !entities_has_briefing_blocked(&conn),
            "0002's ALTER must roll back when a later statement fails; \
             the column survived, so the migration is not atomic"
        );
        // The version-2 row must roll back *with* the column — proving the two
        // commit together. If the column rolled back but a version row had been
        // recorded, the next upgrade would skip 0002 forever.
        let v2_present: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 2)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !v2_present,
            "the version-2 row must roll back together with the column"
        );
    }

    #[test]
    fn migration_0002_records_its_own_version_row() {
        // apply_one writes an INSERT OR IGNORE version row as a fallback, which
        // would mask a migration that forgot to record itself. Exercise 0002's
        // SQL directly (bypassing apply_one) so the in-transaction
        // `INSERT INTO schema_migrations` is load-bearing: this fails if that
        // line is dropped, reopening the duplicate-column upgrade bug.
        let mut conn = Connection::open_in_memory().unwrap();
        apply_one(&mut conn, &MIGRATIONS[0]).expect("apply initial migration");
        conn.execute_batch(MIGRATIONS[1].sql)
            .expect("apply 0002 SQL directly");
        let recorded: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations \
                 WHERE version = 2 AND name = '0002_briefing_blocked')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            recorded,
            "migration 0002 must record its own schema_migrations row inside its transaction"
        );
    }

    fn table_exists(conn: &Connection, table: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            params![table],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .unwrap()
        .is_some()
    }

    #[test]
    fn sei_prior_index_is_added_by_an_upgrade_migration_not_the_initial() {
        // The prior-index side table (0004) must arrive on an existing DB via an
        // upgrade migration, not the initial schema — same discipline as 0002's
        // briefing_blocked. Apply only 0001, confirm the table is absent, then
        // run the full runner and confirm it appears with user_version bumped.
        let mut conn = Connection::open_in_memory().unwrap();
        apply_one(&mut conn, &MIGRATIONS[0]).expect("apply initial migration");
        assert!(
            !table_exists(&conn, "sei_prior_index"),
            "sei_prior_index must not be defined by the initial migration (0001)"
        );

        apply_migrations(&mut conn).expect("apply pending migrations");
        assert!(
            table_exists(&conn, "sei_prior_index"),
            "an upgrade migration must add sei_prior_index to an existing v1 DB"
        );
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, i64::from(CURRENT_SCHEMA_VERSION));
    }

    #[test]
    fn sei_prior_index_has_no_sei_column() {
        // Wave-0 invariant: the prior-index table is SHAPE-INDEPENDENT — it must
        // ship before SEI lock and therefore must NOT carry any `sei` column.
        // Identity lives in a later table; this guards against a refactor that
        // smuggles identity into the side table.
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).expect("apply migrations");
        let has_sei: bool = conn
            .prepare("SELECT 1 FROM pragma_table_xinfo('sei_prior_index') WHERE name = 'sei'")
            .and_then(|mut stmt| stmt.exists([]))
            .unwrap();
        assert!(
            !has_sei,
            "sei_prior_index must be shape-independent (no `sei` column) so it ships pre-lock"
        );
    }

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
