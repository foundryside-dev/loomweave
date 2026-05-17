//! Read-only connection pool wrapping `deadpool-sqlite` per ADR-011.
//!
//! Readers take a connection from the pool, run a query, and drop it. The
//! pool caps concurrent connections (default 16 per ADR-011 §Reader pool).
//! WAL mode lets readers see the committed snapshot at the moment they
//! open; writes become visible only after the next checkpoint or a fresh
//! connection.

use std::path::Path;

use deadpool_sqlite::{Config, Pool, Runtime};

use crate::error::Result;
use crate::pragma;

/// A read-only connection pool backed by `deadpool-sqlite`.
pub struct ReaderPool {
    pool: Pool,
}

impl ReaderPool {
    /// Open a pool against an existing `SQLite` file.
    ///
    /// The database file must already exist and already have migrations
    /// applied — callers should run [`crate::schema::apply_migrations`] on
    /// a write connection first.
    ///
    /// # Errors
    ///
    /// Returns [`crate::StorageError::PoolBuild`] if `deadpool-sqlite`
    /// cannot build the pool — typically because `max_size` is zero or
    /// the runtime is not configured. The `SQLite` file itself is NOT
    /// validated here; connections open lazily on the first
    /// [`Self::with_reader`] call, and file-level errors (path missing,
    /// permission denied) surface there instead.
    pub fn open(db_path: impl AsRef<Path>, max_size: usize) -> Result<Self> {
        let mut cfg = Config::new(db_path.as_ref());
        cfg.pool = Some(deadpool_sqlite::PoolConfig::new(max_size));
        let pool = cfg.create_pool(Runtime::Tokio1)?;
        Ok(Self { pool })
    }

    /// Acquire a reader and run a blocking closure on it.
    ///
    /// Read-side PRAGMAs are applied on every acquisition — cheap and
    /// guarantees `busy_timeout` + `foreign_keys` are always on.
    ///
    /// The closure must be `'static`: captures must be owned or cloned
    /// into the closure (borrowed references from the caller's scope
    /// will not compile). This is a consequence of `deadpool_sqlite`'s
    /// `interact()` submitting the closure to a blocking task pool.
    ///
    /// # Errors
    ///
    /// Returns one of:
    ///
    /// - [`crate::StorageError::Pool`] if the pool cannot acquire a
    ///   connection (most commonly: pool exhausted, acquire timeout).
    /// - [`crate::StorageError::PoolInteract`] if the closure panics or
    ///   the interact task is aborted. The pool recycles poisoned
    ///   connections automatically; subsequent calls remain usable.
    /// - Whatever the closure itself returns on query failure (typically
    ///   [`crate::StorageError::Sqlite`]).
    pub async fn with_reader<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let obj = self.pool.get().await?;
        obj.interact(move |conn| -> Result<T> {
            pragma::apply_read_pragmas(conn)?;
            f(conn)
        })
        .await?
    }

    /// Number of callers currently waiting for a connection.
    ///
    /// Exposed so tests can assert that a queued reader has actually reached
    /// the pool's wait-list, replacing wall-clock sleep heuristics with a
    /// deterministic poll. Not part of the stable API; the deadpool
    /// `Status` shape is an implementation detail.
    #[doc(hidden)]
    #[must_use]
    pub fn waiting_count(&self) -> usize {
        self.pool.status().waiting
    }
}
