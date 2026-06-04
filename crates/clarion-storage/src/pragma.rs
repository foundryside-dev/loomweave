//! PRAGMAs applied at connection open per ADR-011 §`SQLite` PRAGMAs.
//!
//! # Operational tuning discipline (per ADR-035, in-flight at v1.0 tag-cut)
//!
//! - **Stated basis**: ADR-011 §`SQLite` PRAGMAs fixes the durability +
//!   concurrency posture (`journal_mode=WAL` + `synchronous=NORMAL` +
//!   `busy_timeout=5000` + `wal_autocheckpoint=1000` + `foreign_keys=ON`).
//!   The `application_id=0x434C524E` ("CLRN") and `user_version` PRAGMAs
//!   close gap STO-02 from `docs/implementation/v1.0-tag-cut/gap-register.md`:
//!   they give `.clarion/clarion.db` a self-identifying on-disk header so
//!   `file(1)` / `sqlite3 .dbinfo` / a future migration runner can refuse
//!   foreign or forward-incompatible files.
//! - **Override surface**: **recompile-only.** None of these PRAGMAs are
//!   user-tunable at runtime — they encode the storage contract. Any
//!   override is a source-code change reviewed against the ADR that
//!   established the value.
//! - **Retune trigger**: the on-disk format identifier (`application_id`)
//!   may only change under a new ADR that supersedes ADR-035; the
//!   `user_version` is bumped automatically by the migration runner
//!   (`schema::apply_migrations`) and otherwise treated as immutable.

use rusqlite::Connection;

use crate::error::{Result, StorageError};

/// `SQLite` `application_id` header value identifying Clarion databases.
///
/// ASCII "CLRN" — picked so `file(1)` / `sqlite3 .dbinfo` distinguishes a
/// Clarion DB from any other `SQLite` file. Set lazily on first open of a
/// fresh (`application_id = 0`) file. Refusing to open a file with any
/// other non-zero value is how we close STO-02.
pub const CLARION_APPLICATION_ID: u32 = 0x434C_524E;

/// Apply the write-side PRAGMA set: WAL, `synchronous=NORMAL`, `busy_timeout`,
/// `wal_autocheckpoint`, `foreign_keys`. Called on the writer's connection once,
/// immediately after open.
///
/// Also enforces the `application_id` identity check (STO-02): a file whose
/// `application_id` is neither `0` (fresh / legacy) nor
/// [`CLARION_APPLICATION_ID`] is rejected with
/// [`StorageError::ForeignDatabase`]. A zero value is upgraded in place
/// (`PRAGMA application_id = ...`) so re-opens recognise the file.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if any PRAGMA statement fails.
/// Returns [`StorageError::PragmaInvariant`] if WAL mode is not
/// confirmed after the `PRAGMA journal_mode = WAL` command.
/// Returns [`StorageError::ForeignDatabase`] if the file carries a
/// non-zero `application_id` that is not Clarion's.
pub fn apply_write_pragmas(conn: &Connection) -> Result<()> {
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StorageError::PragmaInvariant(format!(
            "expected WAL journal mode, got {mode:?} — \
             ADR-011's synchronous=NORMAL durability posture requires WAL"
        )));
    }
    enforce_application_id(conn)?;
    conn.execute_batch(concat!(
        "PRAGMA synchronous = NORMAL;",
        "PRAGMA busy_timeout = 5000;",
        "PRAGMA wal_autocheckpoint = 1000;",
        "PRAGMA foreign_keys = ON;",
    ))?;
    Ok(())
}

/// Apply the read-side PRAGMA set: `busy_timeout` + `foreign_keys`. Readers do not
/// set `journal_mode` (WAL is a database-level mode set by the first writer).
///
/// # Errors
///
/// Returns [`crate::error::StorageError::Sqlite`] if any PRAGMA statement fails.
pub fn apply_read_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(concat!(
        "PRAGMA busy_timeout = 5000;",
        "PRAGMA foreign_keys = ON;",
    ))?;
    Ok(())
}

/// Validate the database identity for read-only surfaces without mutating the
/// header. Accepts legacy zero-id files, accepts Clarion's id, and rejects any
/// other non-zero `application_id`.
pub fn validate_application_id_for_read(conn: &Connection) -> Result<()> {
    let raw: i64 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let id = u32::try_from(raw).map_err(|_| {
        StorageError::PragmaInvariant(format!(
            "PRAGMA application_id returned out-of-range value {raw}; expected 0..=u32::MAX"
        ))
    })?;
    match id {
        0 => Ok(()),
        id if id == CLARION_APPLICATION_ID => Ok(()),
        other => Err(StorageError::ForeignDatabase {
            application_id: other,
        }),
    }
}

/// Read `application_id`; on `0` set it to [`CLARION_APPLICATION_ID`]; on
/// [`CLARION_APPLICATION_ID`] continue; on any other value refuse with
/// [`StorageError::ForeignDatabase`].
///
/// `SQLite` stores `application_id` as a signed 32-bit integer; rusqlite
/// surfaces it as `i64`. We read into `i64` and reinterpret via
/// `as u32` so values with the high bit set (negative `i32`) still compare
/// against [`CLARION_APPLICATION_ID`] correctly.
fn enforce_application_id(conn: &Connection) -> Result<()> {
    let raw: i64 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    // i64 -> u32: SQLite caps application_id at 32 bits. Truncating cast is
    // the documented round-trip; values outside u32 should not reach us.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let current = raw as u32;
    match current {
        0 => {
            conn.execute_batch(&format!(
                "PRAGMA application_id = {CLARION_APPLICATION_ID};"
            ))?;
            Ok(())
        }
        id if id == CLARION_APPLICATION_ID => Ok(()),
        other => Err(StorageError::ForeignDatabase {
            application_id: other,
        }),
    }
}
