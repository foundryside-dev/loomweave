//! Crate-local error type wrapping `rusqlite::Error` per UQ-WP1-06.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("connection-pool error: {0}")]
    Pool(#[from] deadpool_sqlite::PoolError),

    #[error("pool build error: {0}")]
    PoolBuild(#[from] deadpool_sqlite::CreatePoolError),

    #[error("pool interact error: {0}")]
    PoolInteract(#[from] deadpool_sqlite::InteractError),

    #[error("PRAGMA invariant violated: {0}")]
    PragmaInvariant(String),

    #[error(
        "LMWV-INFRA-STORAGE-FOREIGN-DB: refusing to open SQLite file with \
         application_id={application_id:#010x}; Loomweave databases carry \
         application_id=0x4C4D5756 (\"LMWV\")"
    )]
    ForeignDatabase { application_id: u32 },

    #[error(
        "LMWV-INFRA-STORAGE-FUTURE-DB: refusing to open SQLite file with \
         user_version={found} (greater than current schema version {current}); \
         the database was written by a newer Loomweave build"
    )]
    FutureUserVersion { found: u32, current: u32 },

    #[error(
        "LMWV-INFRA-STORAGE-UNMIGRATED-DB: refusing to open an unmigrated SQLite \
         file (user_version=0 — no Loomweave schema applied). This is an empty or \
         externally-created file, not a Loomweave index. Run `loomweave install \
         --path <project>` then `loomweave analyze <project>` to build the index"
    )]
    UnmigratedIndex,

    #[error("migration {version} failed: {source}")]
    Migration {
        version: u32,
        #[source]
        source: rusqlite::Error,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid query: {0}")]
    InvalidQuery(String),

    /// A row that exists in storage failed an integrity check on read (e.g. a
    /// blob that the write path stores byte-verbatim no longer re-parses).
    /// This is a server-side fault — Loomweave's stored state is damaged, NOT a
    /// malformed client request — so it must map to 5xx and be logged, never to
    /// a client 4xx. Reachable only via storage corruption or an out-of-band
    /// write that bypasses the validated write path.
    #[error("storage integrity failure: {0}")]
    Corruption(String),

    #[error("invalid source path: {0}")]
    InvalidSourcePath(String),

    #[error("channel closed — writer actor has exited")]
    WriterGone,

    #[error("writer protocol violation: {0}")]
    WriterProtocol(String),

    #[error("writer actor returned no response")]
    WriterNoResponse,
}

pub type Result<T> = std::result::Result<T, StorageError>;

impl StorageError {
    /// `true` iff the underlying rusqlite error is a foreign-key
    /// constraint violation (`SQLite` extended code 787). The MCP envelope
    /// layer uses this to mark such failures `retryable=false`, because
    /// a deterministic FK violation against the same row will recur on
    /// retry and re-burn LLM tokens (clarion-df58379de4).
    #[must_use]
    pub fn is_foreign_key_violation(&self) -> bool {
        match self {
            Self::Sqlite(rusqlite::Error::SqliteFailure(err, _)) => {
                err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn is_foreign_key_violation_detects_sqlite_fk_breach() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; \
             CREATE TABLE parent (id TEXT PRIMARY KEY); \
             CREATE TABLE child (id TEXT PRIMARY KEY, parent_id TEXT REFERENCES parent(id));",
        )
        .unwrap();
        let raw = conn
            .execute(
                "INSERT INTO child (id, parent_id) VALUES ('c1', 'absent')",
                [],
            )
            .expect_err("FK violation should fail the insert");
        let err: StorageError = raw.into();
        assert!(
            err.is_foreign_key_violation(),
            "expected FK classifier to recognise SQLITE_CONSTRAINT_FOREIGNKEY (787), got {err}"
        );
    }

    #[test]
    fn is_foreign_key_violation_rejects_other_constraint_breaches() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (id TEXT PRIMARY KEY)", [])
            .unwrap();
        conn.execute("INSERT INTO t VALUES ('a')", []).unwrap();
        let raw = conn
            .execute("INSERT INTO t VALUES ('a')", [])
            .expect_err("PK collision should fail the insert");
        let err: StorageError = raw.into();
        assert!(
            !err.is_foreign_key_violation(),
            "PK collision should not be classified as FK violation: {err}"
        );
    }

    #[test]
    fn is_foreign_key_violation_returns_false_for_non_sqlite_errors() {
        let err = StorageError::WriterGone;
        assert!(!err.is_foreign_key_violation());
    }
}
