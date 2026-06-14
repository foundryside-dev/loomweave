//! Git-ignored embeddings sidecar (`.weft/loomweave/embeddings.db`) for `WS5b`
//! semantic search (ADR-040).
//!
//! Embeddings are large and rebuildable, so they must **not** bloat the
//! committed `.weft/loomweave/loomweave.db` (ADR-005). They live in a separate `SQLite`
//! file, keyed by `(entity_id, content_hash, model_id)` so they invalidate on
//! content change exactly like the summary cache. Because the file is a private,
//! rebuildable cache (git-ignored), it carries its own self-contained schema
//! (`CREATE TABLE IF NOT EXISTS`) rather than a row in the committed-DB
//! migration chain, and it is exempt from the `application_id` foreign-DB guard.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Result, StorageError};

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS entity_embeddings (
    entity_id        TEXT NOT NULL,
    content_hash     TEXT NOT NULL,
    model_id         TEXT NOT NULL,
    dim              INTEGER NOT NULL,
    vec              BLOB NOT NULL,
    cost_usd         REAL NOT NULL DEFAULT 0,
    tokens_input     INTEGER NOT NULL DEFAULT 0,
    created_at       TEXT NOT NULL,
    last_accessed_at TEXT NOT NULL,
    PRIMARY KEY (entity_id, content_hash, model_id)
);";

/// Cache key for one entity's embedding under a given model — mirrors the
/// summary cache's content-hash invalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingKey {
    pub entity_id: String,
    pub content_hash: String,
    pub model_id: String,
}

/// A stored embedding row returned to the cosine-scan path.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEmbedding {
    pub entity_id: String,
    pub content_hash: String,
    pub vector: Vec<f32>,
}

/// Handle on the sidecar embeddings database.
pub struct EmbeddingStore {
    conn: Connection,
}

/// The conventional sidecar path for a project: `<root>/.weft/loomweave/embeddings.db`.
#[must_use]
pub fn embeddings_db_path(project_root: &Path) -> PathBuf {
    loomweave_core::store::store_dir(project_root).join("embeddings.db")
}

impl EmbeddingStore {
    /// Open (creating if absent) the sidecar at `path` and ensure its schema.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Open the conventional `<root>/.weft/loomweave/embeddings.db` sidecar.
    pub fn open_in_store_dir(project_root: &Path) -> Result<Self> {
        Self::open(&embeddings_db_path(project_root))
    }

    /// Insert or replace one entity's embedding vector.
    pub fn upsert(
        &self,
        key: &EmbeddingKey,
        vector: &[f32],
        cost_usd: f64,
        tokens_input: u32,
        now: &str,
    ) -> Result<()> {
        let blob = encode_vector(vector);
        let dim = i64::try_from(vector.len()).unwrap_or(i64::MAX);
        self.conn.execute(
            "INSERT INTO entity_embeddings \
                 (entity_id, content_hash, model_id, dim, vec, cost_usd, tokens_input, \
                  created_at, last_accessed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
             ON CONFLICT(entity_id, content_hash, model_id) DO UPDATE SET \
                 dim = excluded.dim, vec = excluded.vec, cost_usd = excluded.cost_usd, \
                 tokens_input = excluded.tokens_input, last_accessed_at = excluded.last_accessed_at",
            params![
                key.entity_id,
                key.content_hash,
                key.model_id,
                dim,
                blob,
                cost_usd,
                tokens_input,
                now,
            ],
        )?;
        Ok(())
    }

    /// Look up a single cached vector (skip-unchanged check during analyze).
    pub fn get_vector(
        &self,
        entity_id: &str,
        content_hash: &str,
        model_id: &str,
    ) -> Result<Option<Vec<f32>>> {
        let blob: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT vec FROM entity_embeddings \
                 WHERE entity_id = ?1 AND content_hash = ?2 AND model_id = ?3",
                params![entity_id, content_hash, model_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        blob.map(|b| decode_vector(&b)).transpose()
    }

    /// All stored vectors for `model_id`, bounded by `cap`. Returns
    /// `(rows, truncated)`. The caller intersects against the current entity
    /// `content_hash` set so stale rows are never used (freshness).
    pub fn vectors_for_model(
        &self,
        model_id: &str,
        cap: usize,
    ) -> Result<(Vec<StoredEmbedding>, bool)> {
        let limit = i64::try_from(cap.saturating_add(1)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT entity_id, content_hash, vec FROM entity_embeddings \
             WHERE model_id = ?1 ORDER BY entity_id LIMIT ?2",
        )?;
        let mut rows = stmt.query(params![model_id, limit])?;
        let mut out = Vec::new();
        let mut truncated = false;
        while let Some(row) = rows.next()? {
            if out.len() >= cap {
                truncated = true;
                break;
            }
            let entity_id: String = row.get(0)?;
            let content_hash: String = row.get(1)?;
            let blob: Vec<u8> = row.get(2)?;
            out.push(StoredEmbedding {
                entity_id,
                content_hash,
                vector: decode_vector(&blob)?,
            });
        }
        Ok((out, truncated))
    }
}

/// Encode a vector as little-endian f32 bytes.
fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Decode little-endian f32 bytes; a non-multiple-of-4 length is stored
/// corruption (the write path always stores whole f32s).
fn decode_vector(bytes: &[u8]) -> Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(StorageError::Corruption(format!(
            "embedding blob length {} is not a multiple of 4",
            bytes.len()
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> EmbeddingStore {
        let dir = tempfile::tempdir().expect("tempdir");
        EmbeddingStore::open(&dir.path().join("embeddings.db")).expect("open")
        // dir drops, but the open Connection keeps the file handle valid for the
        // test's lifetime on Linux.
    }

    fn key(id: &str) -> EmbeddingKey {
        EmbeddingKey {
            entity_id: id.to_owned(),
            content_hash: "h1".to_owned(),
            model_id: "m".to_owned(),
        }
    }

    #[test]
    fn upsert_then_get_round_trips_the_vector() {
        let s = store();
        s.upsert(
            &key("e1"),
            &[1.0, -2.5, 3.0],
            0.01,
            7,
            "2026-01-01T00:00:00.000Z",
        )
        .expect("upsert");
        let got = s.get_vector("e1", "h1", "m").expect("get");
        assert_eq!(got, Some(vec![1.0, -2.5, 3.0]));
        assert_eq!(s.get_vector("e1", "OTHER", "m").expect("get"), None);
    }

    #[test]
    fn upsert_replaces_on_conflict() {
        let s = store();
        s.upsert(&key("e1"), &[1.0], 0.0, 1, "t1").expect("upsert");
        s.upsert(&key("e1"), &[9.0], 0.0, 1, "t2").expect("upsert");
        assert_eq!(s.get_vector("e1", "h1", "m").expect("get"), Some(vec![9.0]));
    }

    #[test]
    fn vectors_for_model_filters_and_bounds() {
        let s = store();
        s.upsert(&key("e1"), &[1.0, 0.0], 0.0, 1, "t").expect("u1");
        s.upsert(&key("e2"), &[0.0, 1.0], 0.0, 1, "t").expect("u2");
        s.upsert(
            &EmbeddingKey {
                entity_id: "e3".to_owned(),
                content_hash: "h1".to_owned(),
                model_id: "other-model".to_owned(),
            },
            &[1.0, 1.0],
            0.0,
            1,
            "t",
        )
        .expect("u3");

        let (rows, truncated) = s.vectors_for_model("m", 10).expect("scan");
        assert_eq!(rows.len(), 2, "only the two 'm' rows");
        assert!(!truncated);

        let (rows, truncated) = s.vectors_for_model("m", 1).expect("scan capped");
        assert_eq!(rows.len(), 1);
        assert!(truncated);
    }
}
