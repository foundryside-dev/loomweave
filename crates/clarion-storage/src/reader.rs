//! Read-only connection pool wrapping `deadpool-sqlite` per ADR-011.
//!
//! Readers take a connection from the pool, run a query, and drop it. The
//! pool caps concurrent connections (default 16 per ADR-011 §Reader pool).
//! WAL mode lets readers see the committed snapshot at the moment they
//! open; writes become visible only after the next checkpoint or a fresh
//! connection.

use std::path::Path;
use std::sync::Arc;

use deadpool_sqlite::{Config, Pool, Runtime};

use crate::error::Result;
use crate::pragma;

/// A read-only connection pool backed by `deadpool-sqlite`.
///
/// `identity` is a per-`open()` Arc that survives every `Clone` of the
/// `ReaderPool`. Two `ReaderPool` values share a `ReaderPool::identity`
/// pointer if-and-only-if they were produced by `Clone`-ing the same
/// original. Callers that need to *prove at runtime* that two pool handles
/// are the same pool (rather than coincidentally pointing at the same
/// file) use [`ReaderPool::shares_pool_with`].
#[derive(Clone)]
pub struct ReaderPool {
    pool: Pool,
    identity: Arc<()>,
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
        Ok(Self {
            pool,
            identity: Arc::new(()),
        })
    }

    /// Open a pool and eagerly validate the backing database at boot.
    ///
    /// Like [`Self::open`], but first proves the file exists and is a readable
    /// `SQLite` database, so `clarion serve` fails fast on a missing, corrupt,
    /// or unreadable DB instead of deferring the error to the first
    /// [`Self::with_reader`] call. This matters because `deadpool-sqlite` opens
    /// pool connections lazily *and with `CREATE`* — without this probe, a
    /// `serve` pointed at a missing DB would silently materialise an empty one
    /// and answer every query with zero rows.
    ///
    /// The probe is a throwaway read-only connection running
    /// `PRAGMA schema_version`, the same cheap corruption check
    /// `clarion hook session-start` uses. It also validates the Clarion
    /// `application_id` and refuses a future `user_version` without mutating
    /// legacy zero-id files. It runs *before* the pool is built so a bad DB
    /// never produces a half-live pool.
    ///
    /// It validates file-level openability and readability only; it does not
    /// prove the pool can acquire a connection under concurrent load (that
    /// still surfaces in [`Self::with_reader`] as before).
    ///
    /// # Errors
    ///
    /// Returns [`crate::StorageError::Sqlite`] if the database cannot be opened
    /// read-only (missing file, permission denied) or the probe read fails
    /// (corrupt / not a `SQLite` file), and [`crate::StorageError::PoolBuild`]
    /// if the pool itself cannot be built.
    pub fn open_validated(db_path: impl AsRef<Path>, max_size: usize) -> Result<Self> {
        let db_path = db_path.as_ref();
        let conn = rusqlite::Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        // Force a real read of the database header so a present-but-corrupt file
        // is rejected here rather than reported as an empty index later.
        conn.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?;
        pragma::validate_application_id_for_read(&conn)?;
        crate::schema::verify_user_version(&conn)?;
        drop(conn);
        Self::open(db_path, max_size)
    }

    /// Borrow the per-pool identity tag. Two `ReaderPool` clones from the
    /// same original return tags that satisfy `Arc::ptr_eq`; pools opened
    /// independently do not.
    #[must_use]
    pub fn identity(&self) -> &Arc<()> {
        &self.identity
    }

    /// Returns `true` iff `self` and `other` were produced by cloning the
    /// same original `ReaderPool` (i.e. they share the same in-process pool
    /// instance, not just the same backing file).
    #[must_use]
    pub fn shares_pool_with(&self, other: &ReaderPool) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }

    /// Acquire a reader and run a blocking closure on it.
    ///
    /// Read-side PRAGMAs are applied on every acquisition even though they
    /// persist for a connection's lifetime. This is an intentional
    /// belt-and-suspenders choice: `deadpool-sqlite` opens connections lazily,
    /// the current API does not install a post-create hook, and these two
    /// PRAGMAs are cheap compared with the queries that use the reader.
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
