//! Wardline taint-fact store (SP9, ADR-036). Dedicated per-entity table;
//! `wardline_json` is opaque (stored/returned verbatim). Resolution is the
//! exact tier: Wardline pre-composes its dotted qualname to byte-match
//! Clarion's `canonical_qualified_name`, so resolution is a direct existence
//! lookup of `python:function:<qualname>`. Heuristic tier is Flow B B.2.

use std::collections::HashSet;

use rusqlite::{Connection, params};

use crate::query::existing_entity_ids;
use crate::{Result, StorageError};

/// Resolution of a Wardline qualname against Clarion's entity catalog.
///
/// Exact tier only at 1.1. The Heuristic tier is Flow B B.2
/// (clarion-ca2d26ffbe), which extends THIS enum (e.g. a
/// `Heuristic { entity_id, alternatives }` variant) and must not reimplement
/// resolution. Keeping this a sum type means an illegal combination — a
/// confidence without an id, or alternatives on an exact hit — is
/// unrepresentable rather than merely undocumented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Byte-exact match: the pre-composed qualname maps to exactly one entity.
    Exact { entity_id: String },
    /// No entity matched.
    None,
}

impl Resolution {
    /// Borrow the resolved entity id, if any.
    #[must_use]
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Resolution::Exact { entity_id } => Some(entity_id),
            Resolution::None => Option::None,
        }
    }

    /// Consume into the resolved entity id, if any.
    #[must_use]
    pub fn into_entity_id(self) -> Option<String> {
        match self {
            Resolution::Exact { entity_id } => Some(entity_id),
            Resolution::None => Option::None,
        }
    }
}

/// Build the candidate entity id for a Wardline pre-composed qualname.
/// Taint facts are function/method-scoped (request §3); methods are
/// `python:function:` in Clarion's ontology (ADR-022, fixture-confirmed).
fn function_candidate(qualname: &str) -> String {
    format!("python:function:{qualname}")
}

/// Resolve one pre-composed Wardline qualname to a Clarion entity id (exact
/// tier). Returns `Exact` with the id when the entity exists, else `None`.
pub fn resolve_wardline_qualname(conn: &Connection, qualname: &str) -> Result<Resolution> {
    let resolved = resolve_wardline_qualnames(conn, std::slice::from_ref(&qualname.to_owned()))?;
    Ok(resolved
        .into_iter()
        .next()
        .map_or(Resolution::None, |(_, r)| r))
}

/// Batch resolve. Returns `(qualname, Resolution)` pairs in input order.
pub fn resolve_wardline_qualnames(
    conn: &Connection,
    qualnames: &[String],
) -> Result<Vec<(String, Resolution)>> {
    let candidates: Vec<String> = qualnames.iter().map(|q| function_candidate(q)).collect();
    let found: HashSet<String> = existing_entity_ids(conn, &candidates)?;
    Ok(qualnames
        .iter()
        .zip(candidates)
        .map(|(qualname, candidate)| {
            let resolution = if found.contains(&candidate) {
                Resolution::Exact {
                    entity_id: candidate,
                }
            } else {
                Resolution::None
            };
            (qualname.clone(), resolution)
        })
        .collect())
}

/// A single taint fact to persist. `wardline_json` is opaque to Clarion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintFact {
    pub entity_id: String,
    pub wardline_json: String,
    pub scan_id: Option<String>,
    pub content_hash_at_compute: Option<String>,
    pub updated_at: String,
}

/// Upsert one taint fact (per-entity replace). Idempotent on `entity_id`.
/// Runs on the writer-actor's connection (Task 3) outside any run transaction.
pub fn upsert_taint_fact(conn: &Connection, fact: &TaintFact) -> Result<()> {
    conn.execute(
        "INSERT INTO wardline_taint_facts \
            (entity_id, wardline_json, scan_id, content_hash_at_compute, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(entity_id) DO UPDATE SET \
            wardline_json = excluded.wardline_json, \
            scan_id = excluded.scan_id, \
            content_hash_at_compute = excluded.content_hash_at_compute, \
            updated_at = excluded.updated_at",
        params![
            fact.entity_id,
            fact.wardline_json,
            fact.scan_id,
            fact.content_hash_at_compute,
            fact.updated_at,
        ],
    )?;
    Ok(())
}

/// A fetched taint fact joined with the entity's containing-file path. The
/// freshness signal (`current_content_hash`) is NOT stored here: the read
/// surface derives it live from `source_file_path` via
/// [`crate::query::current_file_hash`], because the stored
/// `entities.content_hash` is a span-scoped, LF-normalized hash for function
/// entities and would be wrong for the whole-file freshness contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintFactRow {
    pub entity_id: String,
    pub wardline_json: String,
    /// The containing file's stored path; the read surface derives the live
    /// `current_content_hash` from it (see `query::current_file_hash`). `None`
    /// only if the entity row has no `source_file_path`.
    pub source_file_path: Option<String>,
}

/// Fetch taint facts for a set of already-resolved entity ids. Returns ONLY
/// the rows that have a stored fact — an id with no fact is simply absent
/// from the result (the sole consumer keys by `entity_id` and treats a miss
/// as "no fact"), so there is no absent-row sentinel to misread as JSON.
/// Resolves in one `IN (...)` query (chunked) rather than N point lookups,
/// matching the batched resolution side. The caller derives the live
/// whole-file freshness hash from `source_file_path`; this function does NOT
/// read the filesystem.
pub fn get_taint_facts(conn: &Connection, entity_ids: &[String]) -> Result<Vec<TaintFactRow>> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::with_capacity(entity_ids.len());
    for chunk in entity_ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT f.entity_id, f.wardline_json, e.source_file_path \
               FROM wardline_taint_facts f \
               JOIN entities e ON e.id = f.entity_id \
              WHERE f.entity_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let fetched = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok(TaintFactRow {
                entity_id: row.get::<_, String>(0)?,
                wardline_json: row.get::<_, String>(1)?,
                source_file_path: row.get::<_, Option<String>>(2)?,
            })
        })?;
        for row in fetched {
            rows.push(row.map_err(StorageError::from)?);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::apply_migrations;

    /// In-memory connection with the REAL schema applied (entities +
    /// `wardline_taint_facts` from migration 0003 + `schema_migrations`). Tests
    /// build from the migration runner so they cannot drift from the DDL.
    ///
    /// Applies `apply_read_pragmas` so `foreign_keys` is ON, matching every
    /// production connection (`pragma::apply_{read,write}_pragmas` both enable
    /// it). Without this the FK cascade is silently inert in-memory and a
    /// cascade test would false-pass. The write-side pragma set is NOT used
    /// here: its `journal_mode = WAL` invariant check rejects an in-memory DB
    /// (which reports `memory`), and WAL is irrelevant to these tests.
    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        crate::pragma::apply_read_pragmas(&conn).unwrap();
        conn
    }

    /// Insert a full, valid `entities` row (mirrors the column list of
    /// `tests/writer_actor.rs::seed_entity_row`). `source_file_path` is the
    /// column the fetch tests vary (the read surface derives the live freshness
    /// hash from it), so it is the sole parameter besides the id.
    fn insert_entity(conn: &Connection, id: &str, source_file_path: Option<&str>) {
        conn.execute(
            "INSERT INTO entities ( \
                id, plugin_id, kind, name, short_name, properties, \
                content_hash, source_file_path, created_at, updated_at \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                "python",
                "function",
                id,
                id.rsplit('.').next().unwrap_or(id),
                "{}",
                "deadbeef",
                source_file_path,
                "2026-05-31T00:00:00.000Z",
                "2026-05-31T00:00:00.000Z",
            ],
        )
        .unwrap();
    }

    fn seed(conn: &Connection, ids: &[&str]) {
        for id in ids {
            insert_entity(conn, id, None);
        }
    }

    #[test]
    fn resolves_fixture_vectors_exact() {
        let conn = migrated_conn();
        // expected_entity_id values copied verbatim from
        // fixtures/wardline-qualname-normalization.json qualified_name_vectors.
        seed(
            &conn,
            &[
                "python:function:auth.tokens.TokenManager.verify",
                "python:function:auth.tokens.refresh.<locals>.helper",
                "python:function:pkg.sub.mod.Outer.Inner.method",
                "python:function:lib.foo.Service.handle",
                "python:function:myns.pkg.mod.widget",
            ],
        );
        for qualname in [
            "auth.tokens.TokenManager.verify",
            "auth.tokens.refresh.<locals>.helper",
            "pkg.sub.mod.Outer.Inner.method",
            "lib.foo.Service.handle",
            "myns.pkg.mod.widget",
        ] {
            let r = resolve_wardline_qualname(&conn, qualname).unwrap();
            assert_eq!(
                r,
                Resolution::Exact {
                    entity_id: format!("python:function:{qualname}"),
                },
                "{qualname}"
            );
        }
    }

    #[test]
    fn unknown_qualname_resolves_none() {
        let conn = migrated_conn();
        seed(&conn, &["python:function:auth.tokens.TokenManager.verify"]);
        let r = resolve_wardline_qualname(&conn, "auth.tokens.does_not_exist").unwrap();
        assert_eq!(r, Resolution::None);
    }

    #[test]
    fn batch_preserves_input_order_and_mixed_results() {
        let conn = migrated_conn();
        seed(&conn, &["python:function:a.b.c"]);
        let qs = vec!["a.b.c".to_owned(), "x.y.z".to_owned(), "a.b.c".to_owned()];
        let out = resolve_wardline_qualnames(&conn, &qs).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].1.entity_id(), Some("python:function:a.b.c"));
        assert_eq!(out[1].1, Resolution::None);
        assert_eq!(out[2].1.entity_id(), Some("python:function:a.b.c"));
    }

    #[test]
    fn upsert_then_fetch_roundtrips_verbatim() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:a.b.c", Some("/abs/pkg/mod.py"));
        let blob =
            r#"{"schema_version":"wardline-taint-1","taint":{"actual_return":"EXTERNAL_RAW"}}"#;
        upsert_taint_fact(
            &conn,
            &TaintFact {
                entity_id: "python:function:a.b.c".to_owned(),
                wardline_json: blob.to_owned(),
                scan_id: Some("scan-1".to_owned()),
                content_hash_at_compute: Some("deadbeef".to_owned()),
                updated_at: "2026-05-31T00:00:00.000Z".to_owned(),
            },
        )
        .unwrap();
        let rows = get_taint_facts(&conn, &["python:function:a.b.c".to_owned()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].wardline_json, blob, "blob stored verbatim");
        // The row carries the entity's stored path; the read surface derives
        // the live freshness hash from it (NOT from entities.content_hash).
        assert_eq!(rows[0].source_file_path.as_deref(), Some("/abs/pkg/mod.py"));
    }

    #[test]
    fn upsert_replaces_per_entity() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:a.b.c", None);
        let mk = |json: &str| TaintFact {
            entity_id: "python:function:a.b.c".to_owned(),
            wardline_json: json.to_owned(),
            scan_id: None,
            content_hash_at_compute: None,
            updated_at: "t".to_owned(),
        };
        // Multi-key blobs whose contents differ only by key ORDER: a naive
        // overwrite that compared parsed JSON (or kept the first write) would
        // pass with a single-key blob but fail here. The store keeps bytes
        // verbatim, so the second write's exact byte order must win.
        let first = r#"{"schema_version":"wardline-taint-1","taint":{"a":1,"b":2}}"#;
        let second = r#"{"schema_version":"wardline-taint-1","taint":{"b":2,"a":1}}"#;
        upsert_taint_fact(&conn, &mk(first)).unwrap();
        upsert_taint_fact(&conn, &mk(second)).unwrap();
        let rows = get_taint_facts(&conn, &["python:function:a.b.c".to_owned()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].wardline_json, second,
            "overwrite must keep the latest write's exact bytes (key order included)"
        );
    }

    #[test]
    fn fetch_absent_entity_is_omitted() {
        let conn = migrated_conn();
        // An id with no stored fact is simply absent from the result — no
        // sentinel row, so no empty-string masquerading as verbatim JSON.
        let rows = get_taint_facts(&conn, &["python:function:missing".to_owned()]).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn fetch_returns_only_present_rows_for_mixed_input() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:present", None);
        upsert_taint_fact(
            &conn,
            &TaintFact {
                entity_id: "python:function:present".to_owned(),
                wardline_json: r#"{"v":1}"#.to_owned(),
                scan_id: None,
                content_hash_at_compute: None,
                updated_at: "t".to_owned(),
            },
        )
        .unwrap();
        let rows = get_taint_facts(
            &conn,
            &[
                "python:function:present".to_owned(),
                "python:function:absent".to_owned(),
            ],
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity_id, "python:function:present");
    }

    #[test]
    fn fetch_empty_input_returns_empty() {
        let conn = migrated_conn();
        assert!(get_taint_facts(&conn, &[]).unwrap().is_empty());
    }

    #[test]
    fn deleting_entity_cascades_to_taint_fact() {
        // The FK `wardline_taint_facts.entity_id → entities.id` is declared
        // `ON DELETE CASCADE` (migration 0003). This guards that contract —
        // and is only meaningful because `migrated_conn` enables
        // `foreign_keys` (production parity); with the pragma OFF the row
        // would survive and this test would silently false-pass.
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:a.b.c", None);
        upsert_taint_fact(
            &conn,
            &TaintFact {
                entity_id: "python:function:a.b.c".to_owned(),
                wardline_json: r#"{"v":1}"#.to_owned(),
                scan_id: None,
                content_hash_at_compute: None,
                updated_at: "t".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(
            get_taint_facts(&conn, &["python:function:a.b.c".to_owned()])
                .unwrap()
                .len(),
            1
        );

        conn.execute(
            "DELETE FROM entities WHERE id = ?1",
            params!["python:function:a.b.c"],
        )
        .unwrap();

        assert!(
            get_taint_facts(&conn, &["python:function:a.b.c".to_owned()])
                .unwrap()
                .is_empty(),
            "deleting the entity must cascade-delete its taint fact"
        );
    }
}
