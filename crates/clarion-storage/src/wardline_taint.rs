//! Wardline taint-fact store (SP9, ADR-036). Dedicated per-entity table;
//! `wardline_json` is opaque (stored/returned verbatim). Resolution is the
//! exact tier: Wardline pre-composes its dotted qualname to byte-match
//! Clarion's `canonical_qualified_name`, so resolution is a direct existence
//! lookup of `python:function:<qualname>`. Heuristic tier is Flow B B.2.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use crate::query::existing_entity_ids;
use crate::{Result, StorageError};

/// Resolution confidence for a qualname → entity lookup. Exact tier only at
/// 1.1; `Heuristic` is reserved for Flow B B.2 (clarion-ca2d26ffbe) which
/// extends THIS resolver — it must not reimplement resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionConfidence {
    Exact,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub entity_id: Option<String>,
    pub confidence: ResolutionConfidence,
    /// Other entity IDs that matched. Always empty in the exact tier.
    pub alternatives: Vec<String>,
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
    Ok(resolved.into_iter().next().map_or_else(
        || Resolution {
            entity_id: None,
            confidence: ResolutionConfidence::None,
            alternatives: Vec::new(),
        },
        |(_, r)| r,
    ))
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
                Resolution {
                    entity_id: Some(candidate),
                    confidence: ResolutionConfidence::Exact,
                    alternatives: Vec::new(),
                }
            } else {
                Resolution {
                    entity_id: None,
                    confidence: ResolutionConfidence::None,
                    alternatives: Vec::new(),
                }
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

/// A fetched taint fact joined with the entity's CURRENT content hash.
/// `current_content_hash` is the freshness signal Wardline compares against
/// the `content_hash_at_compute` stamped inside `wardline_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintFactRow {
    pub entity_id: String,
    pub wardline_json: String,
    pub current_content_hash: Option<String>,
    pub exists: bool,
}

/// Fetch taint facts for a set of already-resolved entity ids. Returns one
/// row per input id; `exists: false` (and `wardline_json: ""`) when no fact
/// is stored. `current_content_hash` is read from `entities.content_hash`.
pub fn get_taint_facts(conn: &Connection, entity_ids: &[String]) -> Result<Vec<TaintFactRow>> {
    let mut rows = Vec::with_capacity(entity_ids.len());
    for entity_id in entity_ids {
        let fetched = conn
            .query_row(
                "SELECT f.wardline_json, e.content_hash \
                   FROM wardline_taint_facts f \
                   JOIN entities e ON e.id = f.entity_id \
                  WHERE f.entity_id = ?1",
                params![entity_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(StorageError::from)?;
        match fetched {
            Some((wardline_json, current_content_hash)) => rows.push(TaintFactRow {
                entity_id: entity_id.clone(),
                wardline_json,
                current_content_hash,
                exists: true,
            }),
            None => rows.push(TaintFactRow {
                entity_id: entity_id.clone(),
                wardline_json: String::new(),
                current_content_hash: None,
                exists: false,
            }),
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(conn: &Connection, ids: &[&str]) {
        conn.execute_batch("CREATE TABLE entities (id TEXT PRIMARY KEY);")
            .unwrap();
        for id in ids {
            conn.execute("INSERT INTO entities (id) VALUES (?1)", params![id])
                .unwrap();
        }
    }

    #[test]
    fn resolves_fixture_vectors_exact() {
        let conn = Connection::open_in_memory().unwrap();
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
            assert_eq!(r.confidence, ResolutionConfidence::Exact, "{qualname}");
            assert_eq!(
                r.entity_id.as_deref(),
                Some(format!("python:function:{qualname}").as_str()),
                "{qualname}"
            );
            assert!(r.alternatives.is_empty());
        }
    }

    #[test]
    fn unknown_qualname_resolves_none() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn, &["python:function:auth.tokens.TokenManager.verify"]);
        let r = resolve_wardline_qualname(&conn, "auth.tokens.does_not_exist").unwrap();
        assert_eq!(r.confidence, ResolutionConfidence::None);
        assert_eq!(r.entity_id, None);
    }

    #[test]
    fn batch_preserves_input_order_and_mixed_results() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn, &["python:function:a.b.c"]);
        let qs = vec!["a.b.c".to_owned(), "x.y.z".to_owned(), "a.b.c".to_owned()];
        let out = resolve_wardline_qualnames(&conn, &qs).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].1.confidence, ResolutionConfidence::Exact);
        assert_eq!(out[1].1.confidence, ResolutionConfidence::None);
        assert_eq!(out[2].1.confidence, ResolutionConfidence::Exact);
    }

    fn seed_with_hash(conn: &Connection, id: &str, hash: Option<&str>) {
        conn.execute(
            "INSERT INTO entities (id, content_hash) VALUES (?1, ?2)",
            params![id, hash],
        )
        .unwrap();
    }

    fn create_tables(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE entities (id TEXT PRIMARY KEY, content_hash TEXT); \
             CREATE TABLE wardline_taint_facts ( \
                entity_id TEXT PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE, \
                wardline_json TEXT NOT NULL, scan_id TEXT, \
                content_hash_at_compute TEXT, updated_at TEXT NOT NULL);",
        )
        .unwrap();
    }

    #[test]
    fn upsert_then_fetch_roundtrips_verbatim() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn);
        seed_with_hash(&conn, "python:function:a.b.c", Some("deadbeef"));
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
        assert!(rows[0].exists);
        assert_eq!(rows[0].wardline_json, blob, "blob stored verbatim");
        assert_eq!(rows[0].current_content_hash.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn upsert_replaces_per_entity() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn);
        seed_with_hash(&conn, "python:function:a.b.c", None);
        let mk = |json: &str| TaintFact {
            entity_id: "python:function:a.b.c".to_owned(),
            wardline_json: json.to_owned(),
            scan_id: None,
            content_hash_at_compute: None,
            updated_at: "t".to_owned(),
        };
        upsert_taint_fact(&conn, &mk(r#"{"v":1}"#)).unwrap();
        upsert_taint_fact(&conn, &mk(r#"{"v":2}"#)).unwrap();
        let rows = get_taint_facts(&conn, &["python:function:a.b.c".to_owned()]).unwrap();
        assert_eq!(rows[0].wardline_json, r#"{"v":2}"#);
    }

    #[test]
    fn fetch_absent_entity_reports_not_exists() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn);
        let rows = get_taint_facts(&conn, &["python:function:missing".to_owned()]).unwrap();
        assert!(!rows[0].exists);
        assert_eq!(rows[0].wardline_json, "");
    }
}
