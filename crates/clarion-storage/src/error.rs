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
