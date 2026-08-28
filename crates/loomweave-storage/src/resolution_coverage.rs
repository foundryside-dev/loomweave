//! Per-file call / reference resolution coverage (clarion-3e517d4aff).
//!
//! A language plugin whose resolver can fail transiently (the Python plugin's
//! pyright session times out, crashes, or trips its restart cap and stays
//! disabled for the rest of the run) reports the coverage it actually achieved
//! for each analysed file. The host persists that claim here so that:
//!
//! - the incremental skip re-dispatches a byte-identical file whose last
//!   analysis was `degraded && transient` ([`files_needing_resolution_redispatch`]),
//! - the MCP caller-navigation surface can name a degraded index instead of
//!   asserting `traversal_complete: true` over a hole
//!   ([`degraded_call_coverage_file_count`]), and
//! - `doctor` can report the residual ([`degraded_resolution_coverage_summary`]).
//!
//! Rows are keyed by the core `file` entity id and replaced in the same per-file
//! transaction as the file's anchored edges; the vanished-file prune drops them.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// One resolution facet's persisted coverage claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FacetCoverageRecord {
    /// `true` when the plugin reported less evidence than the file holds.
    pub degraded: bool,
    /// Plugin-defined machine token naming the degradation.
    pub reason: Option<String>,
    /// Whether re-dispatching the unchanged file could recover coverage.
    pub transient: bool,
}

impl FacetCoverageRecord {
    fn status(&self) -> &'static str {
        if self.degraded {
            "degraded"
        } else {
            "complete"
        }
    }
}

/// Coverage claim for one analysed source file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFileResolutionCoverage {
    pub calls: FacetCoverageRecord,
    pub references: FacetCoverageRecord,
}

impl SourceFileResolutionCoverage {
    /// Whether either facet is degraded.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.calls.degraded || self.references.degraded
    }

    /// Whether either facet is degraded *and* transient — the shape the
    /// incremental partition must re-dispatch.
    #[must_use]
    pub fn needs_redispatch(&self) -> bool {
        (self.calls.degraded && self.calls.transient)
            || (self.references.degraded && self.references.transient)
    }
}

/// Replace the coverage row for `source_file_id`.
///
/// # Errors
///
/// Returns [`crate::StorageError::Sqlite`] if the upsert fails.
pub fn upsert_source_file_resolution_coverage(
    conn: &Connection,
    source_file_id: &str,
    coverage: &SourceFileResolutionCoverage,
    run_id: &str,
    updated_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO source_file_resolution_coverage ( \
            source_file_id, calls_status, calls_reason, calls_transient, \
            references_status, references_reason, references_transient, \
            run_id, updated_at \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT(source_file_id) DO UPDATE SET \
            calls_status = excluded.calls_status, \
            calls_reason = excluded.calls_reason, \
            calls_transient = excluded.calls_transient, \
            references_status = excluded.references_status, \
            references_reason = excluded.references_reason, \
            references_transient = excluded.references_transient, \
            run_id = excluded.run_id, \
            updated_at = excluded.updated_at",
        params![
            source_file_id,
            coverage.calls.status(),
            coverage.calls.reason,
            i64::from(coverage.calls.transient),
            coverage.references.status(),
            coverage.references.reason,
            i64::from(coverage.references.transient),
            run_id,
            updated_at,
        ],
    )?;
    Ok(())
}

/// Canonical-absolute `source_file_path`s (the key `previously_analyzed_files`
/// uses) of every file the incremental partition must re-dispatch even though
/// its bytes are unchanged. Two populations:
///
/// 1. Files whose last recorded coverage is `degraded && transient` on either
///    facet — the resolver failed and a re-run can plausibly recover.
/// 2. Files with NO coverage row (indexed before this table existed) that look
///    like the failure shape: they own at least one callable-looking entity
///    (anything but the `file` / module rows) yet carry zero outgoing `calls`
///    edges AND zero unresolved call sites. A genuinely call-free file pays one
///    re-dispatch, after which its `complete` row keeps it skippable.
///
/// The second population is the bootstrap that heals an index analysed by a
/// pre-fix binary without an operator `--no-incremental` pass.
///
/// # Errors
///
/// Returns [`crate::StorageError::Sqlite`] if either query fails.
pub fn files_needing_resolution_redispatch(conn: &Connection) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    let mut transient = conn.prepare(
        "SELECT e.source_file_path \
         FROM source_file_resolution_coverage c \
         JOIN entities e ON e.id = c.source_file_id \
         WHERE e.source_file_path IS NOT NULL \
           AND ((c.calls_status = 'degraded' AND c.calls_transient = 1) \
             OR (c.references_status = 'degraded' AND c.references_transient = 1))",
    )?;
    for row in transient.query_map([], |row| row.get::<_, String>(0))? {
        out.insert(row?);
    }
    let mut uncovered = conn.prepare(
        "SELECT f.source_file_path \
         FROM entities f \
         WHERE f.plugin_id = 'core' AND f.kind = 'file' \
           AND f.source_file_path IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM source_file_resolution_coverage c \
                           WHERE c.source_file_id = f.id) \
           AND EXISTS (SELECT 1 FROM entities x \
                       WHERE x.source_file_id = f.id \
                         AND x.plugin_id <> 'core' \
                         AND x.parent_id IS NOT NULL \
                         AND x.parent_id <> f.id) \
           AND NOT EXISTS (SELECT 1 FROM edges ce \
                           WHERE ce.source_file_id = f.id AND ce.kind = 'calls') \
           AND NOT EXISTS (SELECT 1 FROM entity_unresolved_call_sites u \
                           WHERE u.source_file_id = f.id)",
    )?;
    for row in uncovered.query_map([], |row| row.get::<_, String>(0))? {
        out.insert(row?);
    }
    Ok(out)
}

/// Number of source files whose last analysis reported degraded `calls`
/// coverage. Non-zero means the call graph has holes the read surface must
/// name.
///
/// # Errors
///
/// Returns [`crate::StorageError::Sqlite`] if the query fails.
pub fn degraded_call_coverage_file_count(conn: &Connection) -> Result<u64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM source_file_resolution_coverage WHERE calls_status = 'degraded'",
        [],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(u64::MAX))
}

/// Summary for `doctor`: `(degraded_calls, degraded_references, transient)`
/// file counts, where `transient` counts files the next `analyze` will
/// re-dispatch automatically.
///
/// # Errors
///
/// Returns [`crate::StorageError::Sqlite`] if the query fails.
pub fn degraded_resolution_coverage_summary(conn: &Connection) -> Result<(u64, u64, u64)> {
    let row: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT \
                SUM(CASE WHEN calls_status = 'degraded' THEN 1 ELSE 0 END), \
                SUM(CASE WHEN references_status = 'degraded' THEN 1 ELSE 0 END), \
                SUM(CASE WHEN (calls_status = 'degraded' AND calls_transient = 1) \
                           OR (references_status = 'degraded' AND references_transient = 1) \
                         THEN 1 ELSE 0 END) \
             FROM source_file_resolution_coverage",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            },
        )
        .optional()?;
    let (calls, references, transient) = row.unwrap_or((0, 0, 0));
    let to_u64 = |value: i64| u64::try_from(value).unwrap_or(u64::MAX);
    Ok((to_u64(calls), to_u64(references), to_u64(transient)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::apply_migrations;

    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        conn
    }

    fn insert_entity(
        conn: &Connection,
        id: &str,
        plugin: &str,
        kind: &str,
        path: &str,
        source_file_id: Option<&str>,
        parent_id: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, parent_id, source_file_id, \
              source_file_path, properties, content_hash, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, '{}', 'h', 't', 't')",
            params![id, plugin, kind, path, parent_id, source_file_id, path],
        )
        .unwrap();
    }

    fn degraded(transient: bool) -> FacetCoverageRecord {
        FacetCoverageRecord {
            degraded: true,
            reason: Some("pyright_timeout".to_owned()),
            transient,
        }
    }

    #[test]
    fn upsert_replaces_prior_claim_for_the_same_file() {
        let conn = migrated_conn();
        let first = SourceFileResolutionCoverage {
            calls: degraded(true),
            references: FacetCoverageRecord::default(),
        };
        upsert_source_file_resolution_coverage(&conn, "core:file:a.py", &first, "r1", "t1")
            .unwrap();
        assert_eq!(degraded_call_coverage_file_count(&conn).unwrap(), 1);

        let healed = SourceFileResolutionCoverage::default();
        upsert_source_file_resolution_coverage(&conn, "core:file:a.py", &healed, "r2", "t2")
            .unwrap();
        assert_eq!(degraded_call_coverage_file_count(&conn).unwrap(), 0);
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM source_file_resolution_coverage",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "upsert must not duplicate the row");
    }

    #[test]
    fn redispatch_selects_transient_degraded_files_only() {
        let conn = migrated_conn();
        insert_entity(
            &conn,
            "core:file:a.py",
            "core",
            "file",
            "/p/a.py",
            None,
            None,
        );
        insert_entity(
            &conn,
            "core:file:b.py",
            "core",
            "file",
            "/p/b.py",
            None,
            None,
        );
        insert_entity(
            &conn,
            "core:file:c.py",
            "core",
            "file",
            "/p/c.py",
            None,
            None,
        );
        let transient = SourceFileResolutionCoverage {
            calls: degraded(true),
            references: FacetCoverageRecord::default(),
        };
        let permanent = SourceFileResolutionCoverage {
            calls: FacetCoverageRecord::default(),
            references: degraded(false),
        };
        upsert_source_file_resolution_coverage(&conn, "core:file:a.py", &transient, "r", "t")
            .unwrap();
        upsert_source_file_resolution_coverage(&conn, "core:file:b.py", &permanent, "r", "t")
            .unwrap();
        upsert_source_file_resolution_coverage(
            &conn,
            "core:file:c.py",
            &SourceFileResolutionCoverage::default(),
            "r",
            "t",
        )
        .unwrap();

        let files = files_needing_resolution_redispatch(&conn).unwrap();
        assert_eq!(files, HashSet::from(["/p/a.py".to_owned()]));
        assert_eq!(
            degraded_resolution_coverage_summary(&conn).unwrap(),
            (1, 1, 1)
        );
    }

    #[test]
    fn redispatch_bootstraps_uncovered_files_that_look_like_the_failure_shape() {
        let conn = migrated_conn();
        // `hole.py`: a function-bearing file with no coverage row, no calls
        // edges and no unresolved sites — the pre-fix failure shape.
        insert_entity(
            &conn,
            "core:file:hole.py",
            "core",
            "file",
            "/p/hole.py",
            None,
            None,
        );
        insert_entity(
            &conn,
            "python:module:hole",
            "python",
            "module",
            "/p/hole.py",
            Some("core:file:hole.py"),
            Some("core:file:hole.py"),
        );
        insert_entity(
            &conn,
            "python:function:hole.f",
            "python",
            "function",
            "/p/hole.py",
            Some("core:file:hole.py"),
            Some("python:module:hole"),
        );
        // `ok.py`: same shape but it DOES carry a calls edge — healthy.
        insert_entity(
            &conn,
            "core:file:ok.py",
            "core",
            "file",
            "/p/ok.py",
            None,
            None,
        );
        insert_entity(
            &conn,
            "python:module:ok",
            "python",
            "module",
            "/p/ok.py",
            Some("core:file:ok.py"),
            Some("core:file:ok.py"),
        );
        insert_entity(
            &conn,
            "python:function:ok.g",
            "python",
            "function",
            "/p/ok.py",
            Some("core:file:ok.py"),
            Some("python:module:ok"),
        );
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence, properties, source_file_id) \
             VALUES ('calls', 'python:function:ok.g', 'python:function:hole.f', \
             'resolved', '{}', 'core:file:ok.py')",
            [],
        )
        .unwrap();
        // `empty.py`: only a module row (no callable-looking entity) — a
        // module with no symbols legitimately has no calls; not re-dispatched.
        insert_entity(
            &conn,
            "core:file:empty.py",
            "core",
            "file",
            "/p/empty.py",
            None,
            None,
        );
        insert_entity(
            &conn,
            "python:module:empty",
            "python",
            "module",
            "/p/empty.py",
            Some("core:file:empty.py"),
            Some("core:file:empty.py"),
        );

        let files = files_needing_resolution_redispatch(&conn).unwrap();
        assert_eq!(files, HashSet::from(["/p/hole.py".to_owned()]));

        // Once a coverage row exists the bootstrap heuristic no longer applies.
        upsert_source_file_resolution_coverage(
            &conn,
            "core:file:hole.py",
            &SourceFileResolutionCoverage::default(),
            "r",
            "t",
        )
        .unwrap();
        assert!(
            files_needing_resolution_redispatch(&conn)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn summary_is_zero_on_an_empty_table() {
        let conn = migrated_conn();
        assert_eq!(
            degraded_resolution_coverage_summary(&conn).unwrap(),
            (0, 0, 0)
        );
        assert_eq!(degraded_call_coverage_file_count(&conn).unwrap(), 0);
    }
}
