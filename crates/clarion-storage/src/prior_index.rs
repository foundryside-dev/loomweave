//! Prior-index retention (Wave 0 / WS3).
//!
//! `sei_prior_index` (migration 0004) is a side table holding the previous
//! successful run's `locator -> body_hash (+ signature)` snapshot. It is
//! SHAPE-INDEPENDENT — there is no `sei` column — so it ships before the
//! suite-wide SEI identity standard locks. Two later consumers read it: the
//! incremental-analysis fast path (skip unchanged files) and the SEI matcher
//! (detect vanished locators / compare bodies for move + rename). Neither is
//! wired in Wave 0; this module only builds and rewrites the snapshot.
//!
//! The snapshot is rewritten as a FULL REPLACE after each successful run (see
//! [`replace_prior_index`] and `WriterCmd::UpsertPriorIndex`). `entities` is a
//! cumulative, never-pruned table, so the run pipeline accumulates the current
//! run's `(locator, body_hash)` pairs as it inserts entities and hands the
//! whole set here — the table is never derived by querying `entities`.
//!
//! `signature` is reserved for the WS1 matcher and stays `None`/NULL in Wave 0,
//! because `entities.signature` does not exist until the WS1 migration adds it.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use crate::Result;
use crate::error::StorageError;

/// One row of the prior-index snapshot: a locator and the body hash (and, from
/// WS1 onward, signature) it carried at the last successful run.
///
/// `signature` is `None` in Wave 0 (the matcher input arrives with WS1). The
/// `recorded_at` timestamp is supplied by the writer at flush time (the run's
/// completion timestamp), not stored on the struct, so one consistent stamp
/// covers a whole snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorIndexEntry {
    /// The entity's full id string (`plugin:kind:qualname`).
    pub locator: String,
    /// `entities.content_hash` at prior-run time. Required: an entity with no
    /// body hash cannot be move-matched and is omitted from the snapshot.
    pub body_hash: String,
    /// Reserved for the WS1 matcher; always `None` in Wave 0.
    pub signature: Option<String>,
}

/// Upsert one prior-index row (`INSERT OR REPLACE` on the `locator` PK).
/// `recorded_at` is the ISO-8601 UTC stamp written to the row (the run's
/// completion time).
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if the statement fails.
pub fn upsert_prior_index_entry(
    conn: &Connection,
    entry: &PriorIndexEntry,
    recorded_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sei_prior_index (locator, body_hash, signature, recorded_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(locator) DO UPDATE SET \
            body_hash   = excluded.body_hash, \
            signature   = excluded.signature, \
            recorded_at = excluded.recorded_at",
        params![entry.locator, entry.body_hash, entry.signature, recorded_at],
    )?;
    Ok(())
}

/// Load the whole prior-index snapshot, keyed by locator. Called once at the
/// start of a re-index by the (Wave 1) incremental fast path and the SEI
/// matcher; no Wave-0 caller consumes it yet.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if the query fails.
pub fn load_prior_index(conn: &Connection) -> Result<HashMap<String, PriorIndexEntry>> {
    let mut stmt = conn.prepare("SELECT locator, body_hash, signature FROM sei_prior_index")?;
    let rows = stmt.query_map([], |row| {
        Ok(PriorIndexEntry {
            locator: row.get::<_, String>(0)?,
            body_hash: row.get::<_, String>(1)?,
            signature: row.get::<_, Option<String>>(2)?,
        })
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let entry = row.map_err(StorageError::from)?;
        out.insert(entry.locator.clone(), entry);
    }
    Ok(out)
}

/// Empty the prior-index snapshot. Used inside [`replace_prior_index`] and as
/// the explicit-reset primitive (a full `.clarion/` wipe via `install --force`
/// removes the DB entirely, so this is for in-place resets).
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if the statement fails.
pub fn clear_prior_index(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM sei_prior_index", [])?;
    Ok(())
}

/// Replace the entire prior-index snapshot with `entries`, atomically: a single
/// transaction clears the table and inserts every entry, so a mid-flush failure
/// rolls back to the previous snapshot rather than leaving a half-cleared one.
/// After this returns the table is EXACTLY `entries` (stale rows from the prior
/// run removed) — the Wave-0 contract for "rewrite the snapshot after a
/// successful run".
///
/// Runs on the writer-actor connection in autocommit context (the
/// `query_time_write` path commits any open run batch first), so opening a
/// fresh `unchecked_transaction` here is safe.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if any statement fails; the transaction is
/// dropped (rolled back) without commit on error.
pub fn replace_prior_index(
    conn: &Connection,
    entries: &[PriorIndexEntry],
    recorded_at: &str,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    clear_prior_index(&tx)?;
    for entry in entries {
        upsert_prior_index_entry(&tx, entry, recorded_at)?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::apply_migrations;

    /// In-memory connection with the real schema applied (so the table shape
    /// comes from migration 0004, never a hand-written DDL that could drift).
    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        conn
    }

    fn entry(locator: &str, body_hash: &str) -> PriorIndexEntry {
        PriorIndexEntry {
            locator: locator.to_owned(),
            body_hash: body_hash.to_owned(),
            signature: None,
        }
    }

    #[test]
    fn upsert_then_load_roundtrips() {
        let conn = migrated_conn();
        upsert_prior_index_entry(&conn, &entry("python:function:a.b.c", "hash-abc"), "t0").unwrap();
        let loaded = load_prior_index(&conn).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.get("python:function:a.b.c"),
            Some(&entry("python:function:a.b.c", "hash-abc"))
        );
    }

    #[test]
    fn upsert_is_idempotent_latest_body_hash_wins() {
        // Re-recording a locator with a changed body hash must leave exactly one
        // row carrying the latest hash — the prior index tracks the LAST run, so
        // a stale hash surviving would mis-feed the matcher.
        let conn = migrated_conn();
        upsert_prior_index_entry(&conn, &entry("python:function:f", "old"), "t0").unwrap();
        upsert_prior_index_entry(&conn, &entry("python:function:f", "new"), "t1").unwrap();
        let loaded = load_prior_index(&conn).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["python:function:f"].body_hash, "new");
    }

    #[test]
    fn signature_is_persisted_when_present() {
        // The column is reserved for WS1 but must round-trip if ever set, so a
        // later matcher reading it back gets exactly what was written.
        let conn = migrated_conn();
        let with_sig = PriorIndexEntry {
            locator: "python:function:g".to_owned(),
            body_hash: "h".to_owned(),
            signature: Some(r#"{"v":1}"#.to_owned()),
        };
        upsert_prior_index_entry(&conn, &with_sig, "t0").unwrap();
        let loaded = load_prior_index(&conn).unwrap();
        assert_eq!(
            loaded["python:function:g"].signature.as_deref(),
            Some(r#"{"v":1}"#)
        );
    }

    #[test]
    fn clear_empties_the_snapshot() {
        let conn = migrated_conn();
        upsert_prior_index_entry(&conn, &entry("python:function:a", "h"), "t0").unwrap();
        clear_prior_index(&conn).unwrap();
        assert!(load_prior_index(&conn).unwrap().is_empty());
    }

    #[test]
    fn replace_makes_the_snapshot_equal_the_new_set_and_drops_stale_rows() {
        // The load-bearing WS3 behaviour: the snapshot after a run must be
        // EXACTLY that run's entities. Seed a prior snapshot {a, b}, then replace
        // with {a (changed), c} — b is stale and must vanish, a must update, c is
        // new. A naive per-row upsert without the clear would leave b behind.
        let conn = migrated_conn();
        replace_prior_index(
            &conn,
            &[
                entry("python:function:a", "a0"),
                entry("python:function:b", "b0"),
            ],
            "t0",
        )
        .unwrap();

        replace_prior_index(
            &conn,
            &[
                entry("python:function:a", "a1"),
                entry("python:function:c", "c0"),
            ],
            "t1",
        )
        .unwrap();

        let loaded = load_prior_index(&conn).unwrap();
        let mut locators: Vec<&str> = loaded.keys().map(String::as_str).collect();
        locators.sort_unstable();
        assert_eq!(locators, ["python:function:a", "python:function:c"]);
        assert_eq!(loaded["python:function:a"].body_hash, "a1");
        assert!(
            !loaded.contains_key("python:function:b"),
            "stale row from the prior snapshot must be removed by replace"
        );
    }

    #[test]
    fn replace_with_empty_set_clears_the_snapshot() {
        let conn = migrated_conn();
        replace_prior_index(&conn, &[entry("python:function:a", "h")], "t0").unwrap();
        replace_prior_index(&conn, &[], "t1").unwrap();
        assert!(load_prior_index(&conn).unwrap().is_empty());
    }
}
