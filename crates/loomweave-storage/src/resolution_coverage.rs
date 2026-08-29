//! Per-file call / reference resolution coverage (clarion-3e517d4aff).
//!
//! A language plugin whose resolver can fail transiently (the Python plugin's
//! pyright session times out, crashes, or trips its restart cap and stays
//! disabled for the rest of the run) reports the coverage it actually achieved
//! for each analysed file. The host persists that claim here so that:
//!
//! - the incremental skip re-dispatches a byte-identical file whose last
//!   analysis was `degraded && transient` ([`files_needing_resolution_redispatch`]),
//!   collateral files first and self-inflicted ones last, and gives up after
//!   [`MAX_REDISPATCH_ATTEMPTS`] consecutive degraded runs so one pathological
//!   file cannot make every incremental run pay the full re-dispatch cost;
//! - the MCP caller-navigation surface can name a degraded index instead of
//!   asserting `traversal_complete: true` over a hole
//!   ([`degraded_call_coverage_file_count`]); and
//! - `doctor` can report the residual ([`degraded_resolution_coverage_summary`])
//!   and, under `--fix`, re-arm files that exhausted the budget
//!   ([`reset_exhausted_redispatch_budget`]).
//!
//! Rows are keyed by the core `file` entity id and replaced in the same per-file
//! transaction as the file's anchored edges; the vanished-file prune drops them.

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// Consecutive transient-degraded runs after which a byte-identical file stops
/// forcing re-dispatch. It stays `degraded` on the read surface; a content
/// change, `--no-incremental`, or `doctor --fix`
/// ([`reset_exhausted_redispatch_budget`]) re-arms it.
pub const MAX_REDISPATCH_ATTEMPTS: i64 = 3;

/// One resolution facet's persisted coverage claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FacetCoverageRecord {
    /// `true` when the plugin reported less evidence than the file holds.
    pub degraded: bool,
    /// Plugin-defined machine token naming the degradation.
    pub reason: Option<String>,
    /// Whether re-dispatching the unchanged file could recover coverage.
    pub transient: bool,
    /// The degradation was caused by an earlier file's failure, not this
    /// file's content (the resolver was already disabled when it arrived).
    pub collateral: bool,
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

    /// Whether every transient degradation on this file is collateral — i.e.
    /// nothing suggests the file itself is what breaks the resolver.
    #[must_use]
    pub fn is_collateral_only(&self) -> bool {
        let self_inflicted =
            |facet: &FacetCoverageRecord| facet.degraded && facet.transient && !facet.collateral;
        !self_inflicted(&self.calls) && !self_inflicted(&self.references)
    }
}

/// One file the incremental partition must re-dispatch even though its bytes
/// are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RedispatchCandidate {
    /// Canonical-absolute `source_file_path` (the key `previously_analyzed_files`
    /// uses).
    pub source_file_path: String,
    /// `true` when the file's own resolution failed (timeout / crash on this
    /// file); `false` when it was collateral or has no coverage record yet.
    /// Self-inflicted files are dispatched LAST so they can only poison what
    /// follows them.
    pub self_inflicted: bool,
}

/// Replace the coverage row for `source_file_id`.
///
/// `redispatch_attempts` counts consecutive runs in which the file stayed
/// transient-degraded: it increments when both the prior row and the new claim
/// need re-dispatch, and resets to zero otherwise.
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
    let prior: Option<(i64, i64, i64, i64, i64)> = conn
        .query_row(
            "SELECT calls_status = 'degraded', calls_transient, \
                    references_status = 'degraded', references_transient, \
                    redispatch_attempts \
             FROM source_file_resolution_coverage WHERE source_file_id = ?1",
            params![source_file_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let prior_needed_redispatch = prior.is_some_and(
        |(calls_degraded, calls_transient, refs_degraded, refs_transient, _)| {
            (calls_degraded == 1 && calls_transient == 1)
                || (refs_degraded == 1 && refs_transient == 1)
        },
    );
    let attempts = if coverage.needs_redispatch() && prior_needed_redispatch {
        prior.map_or(1, |(_, _, _, _, attempts)| attempts.saturating_add(1))
    } else {
        0
    };
    conn.execute(
        "INSERT INTO source_file_resolution_coverage ( \
            source_file_id, calls_status, calls_reason, calls_transient, calls_collateral, \
            references_status, references_reason, references_transient, references_collateral, \
            redispatch_attempts, run_id, updated_at \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT(source_file_id) DO UPDATE SET \
            calls_status = excluded.calls_status, \
            calls_reason = excluded.calls_reason, \
            calls_transient = excluded.calls_transient, \
            calls_collateral = excluded.calls_collateral, \
            references_status = excluded.references_status, \
            references_reason = excluded.references_reason, \
            references_transient = excluded.references_transient, \
            references_collateral = excluded.references_collateral, \
            redispatch_attempts = excluded.redispatch_attempts, \
            run_id = excluded.run_id, \
            updated_at = excluded.updated_at",
        params![
            source_file_id,
            coverage.calls.status(),
            coverage.calls.reason,
            i64::from(coverage.calls.transient),
            i64::from(coverage.calls.collateral),
            coverage.references.status(),
            coverage.references.reason,
            i64::from(coverage.references.transient),
            i64::from(coverage.references.collateral),
            attempts,
            run_id,
            updated_at,
        ],
    )?;
    Ok(())
}

/// Every file the incremental partition must re-dispatch even though its bytes
/// are unchanged. Two populations:
///
/// 1. Files whose last recorded coverage is `degraded && transient` on either
///    facet and whose `redispatch_attempts` is still under
///    [`MAX_REDISPATCH_ATTEMPTS`] — the resolver failed and a re-run can
///    plausibly recover. Self-inflicted ones (the file's own resolution timed
///    out / crashed) are flagged so the host dispatches them last.
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
pub fn files_needing_resolution_redispatch(conn: &Connection) -> Result<Vec<RedispatchCandidate>> {
    let mut out = Vec::new();
    let mut transient = conn.prepare(
        "SELECT e.source_file_path, \
                ((c.calls_status = 'degraded' AND c.calls_transient = 1 \
                  AND c.calls_collateral = 0) \
              OR (c.references_status = 'degraded' AND c.references_transient = 1 \
                  AND c.references_collateral = 0)) \
         FROM source_file_resolution_coverage c \
         JOIN entities e ON e.id = c.source_file_id \
         WHERE e.source_file_path IS NOT NULL \
           AND c.redispatch_attempts < ?1 \
           AND ((c.calls_status = 'degraded' AND c.calls_transient = 1) \
             OR (c.references_status = 'degraded' AND c.references_transient = 1)) \
         ORDER BY e.source_file_path",
    )?;
    for row in transient.query_map(params![MAX_REDISPATCH_ATTEMPTS], |row| {
        Ok(RedispatchCandidate {
            source_file_path: row.get::<_, String>(0)?,
            self_inflicted: row.get::<_, i64>(1)? == 1,
        })
    })? {
        out.push(row?);
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
                           WHERE u.source_file_id = f.id) \
         ORDER BY f.source_file_path",
    )?;
    for row in uncovered.query_map([], |row| row.get::<_, String>(0))? {
        out.push(RedispatchCandidate {
            source_file_path: row?,
            self_inflicted: false,
        });
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

/// `doctor`-facing counts over the coverage table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolutionCoverageSummary {
    /// Files whose `calls` facet is degraded.
    pub degraded_calls: u64,
    /// Files whose `references` facet is degraded.
    pub degraded_references: u64,
    /// Files with a transient degradation still under the re-dispatch budget —
    /// the next `analyze` re-dispatches these automatically.
    pub transient: u64,
    /// Files with a transient degradation that exhausted
    /// [`MAX_REDISPATCH_ATTEMPTS`] — still degraded, no longer re-dispatched
    /// until their bytes change.
    pub exhausted: u64,
}

/// Summary for `doctor` (see [`ResolutionCoverageSummary`]).
///
/// # Errors
///
/// Returns [`crate::StorageError::Sqlite`] if the query fails.
pub fn degraded_resolution_coverage_summary(
    conn: &Connection,
) -> Result<ResolutionCoverageSummary> {
    let to_u64 = |value: Option<i64>| u64::try_from(value.unwrap_or(0)).unwrap_or(u64::MAX);
    conn.query_row(
        "SELECT \
            SUM(CASE WHEN calls_status = 'degraded' THEN 1 ELSE 0 END), \
            SUM(CASE WHEN references_status = 'degraded' THEN 1 ELSE 0 END), \
            SUM(CASE WHEN ((calls_status = 'degraded' AND calls_transient = 1) \
                        OR (references_status = 'degraded' AND references_transient = 1)) \
                      AND redispatch_attempts < ?1 THEN 1 ELSE 0 END), \
            SUM(CASE WHEN ((calls_status = 'degraded' AND calls_transient = 1) \
                        OR (references_status = 'degraded' AND references_transient = 1)) \
                      AND redispatch_attempts >= ?1 THEN 1 ELSE 0 END) \
         FROM source_file_resolution_coverage",
        params![MAX_REDISPATCH_ATTEMPTS],
        |row| {
            Ok(ResolutionCoverageSummary {
                degraded_calls: to_u64(row.get(0)?),
                degraded_references: to_u64(row.get(1)?),
                transient: to_u64(row.get(2)?),
                exhausted: to_u64(row.get(3)?),
            })
        },
    )
    .map_err(Into::into)
}

/// Reset `redispatch_attempts` to 0 for every row that has exhausted
/// [`MAX_REDISPATCH_ATTEMPTS`] on a transient-degraded facet (calls or
/// references), so the next incremental `analyze` re-dispatches it. Rows
/// still under budget, complete, or permanently degraded (syntax error /
/// site cap) are untouched — this only re-arms files the budget itself gave
/// up on; it does not wipe the failure counter of anything still under it,
/// so a chronically flaky file cannot dodge the anti-thrash budget by having
/// `doctor --fix` run periodically. Returns the number of rows reset.
///
/// # Errors
///
/// Returns [`crate::StorageError::Sqlite`] if the update fails.
pub fn reset_exhausted_redispatch_budget(conn: &Connection) -> Result<u64> {
    let rows = conn.execute(
        "UPDATE source_file_resolution_coverage \
         SET redispatch_attempts = 0 \
         WHERE redispatch_attempts >= ?1 \
           AND ((calls_status = 'degraded' AND calls_transient = 1) \
             OR (references_status = 'degraded' AND references_transient = 1))",
        params![MAX_REDISPATCH_ATTEMPTS],
    )?;
    Ok(u64::try_from(rows).unwrap_or(u64::MAX))
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
            collateral: false,
        }
    }

    fn collateral() -> FacetCoverageRecord {
        FacetCoverageRecord {
            degraded: true,
            reason: Some("pyright_poisoned".to_owned()),
            transient: true,
            collateral: true,
        }
    }

    fn paths(candidates: &[RedispatchCandidate]) -> Vec<(&str, bool)> {
        candidates
            .iter()
            .map(|c| (c.source_file_path.as_str(), c.self_inflicted))
            .collect()
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
        assert_eq!(paths(&files), vec![("/p/a.py", true)]);
        assert_eq!(
            degraded_resolution_coverage_summary(&conn).unwrap(),
            ResolutionCoverageSummary {
                degraded_calls: 1,
                degraded_references: 1,
                transient: 1,
                exhausted: 0,
            }
        );
    }

    #[test]
    fn collateral_files_are_not_self_inflicted() {
        let conn = migrated_conn();
        insert_entity(
            &conn,
            "core:file:trouble.py",
            "core",
            "file",
            "/p/trouble.py",
            None,
            None,
        );
        insert_entity(
            &conn,
            "core:file:victim.py",
            "core",
            "file",
            "/p/victim.py",
            None,
            None,
        );
        let trouble = SourceFileResolutionCoverage {
            calls: degraded(true),
            references: degraded(true),
        };
        let victim = SourceFileResolutionCoverage {
            calls: collateral(),
            references: collateral(),
        };
        assert!(!trouble.is_collateral_only());
        assert!(victim.is_collateral_only());
        upsert_source_file_resolution_coverage(&conn, "core:file:trouble.py", &trouble, "r", "t")
            .unwrap();
        upsert_source_file_resolution_coverage(&conn, "core:file:victim.py", &victim, "r", "t")
            .unwrap();
        let files = files_needing_resolution_redispatch(&conn).unwrap();
        assert_eq!(
            paths(&files),
            vec![("/p/trouble.py", true), ("/p/victim.py", false)]
        );
    }

    #[test]
    fn redispatch_budget_exhausts_after_consecutive_degraded_runs() {
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
        let transient = SourceFileResolutionCoverage {
            calls: degraded(true),
            references: FacetCoverageRecord::default(),
        };
        let attempts = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT redispatch_attempts FROM source_file_resolution_coverage \
                 WHERE source_file_id = 'core:file:a.py'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        // Run 1 degrades: attempts=0 (first sighting), still re-dispatched.
        upsert_source_file_resolution_coverage(&conn, "core:file:a.py", &transient, "r1", "t")
            .unwrap();
        assert_eq!(attempts(&conn), 0);
        assert_eq!(files_needing_resolution_redispatch(&conn).unwrap().len(), 1);
        // Runs 2..=N keep degrading: the counter climbs until the budget is spent.
        for run in 2..=(MAX_REDISPATCH_ATTEMPTS + 1) {
            upsert_source_file_resolution_coverage(
                &conn,
                "core:file:a.py",
                &transient,
                &format!("r{run}"),
                "t",
            )
            .unwrap();
        }
        assert_eq!(attempts(&conn), MAX_REDISPATCH_ATTEMPTS);
        assert!(
            files_needing_resolution_redispatch(&conn)
                .unwrap()
                .is_empty(),
            "an exhausted file must stop forcing re-dispatch"
        );
        let summary = degraded_resolution_coverage_summary(&conn).unwrap();
        assert_eq!((summary.transient, summary.exhausted), (0, 1));
        assert_eq!(
            degraded_call_coverage_file_count(&conn).unwrap(),
            1,
            "still degraded"
        );
        // A clean run resets the counter; a later degradation starts over.
        upsert_source_file_resolution_coverage(
            &conn,
            "core:file:a.py",
            &SourceFileResolutionCoverage::default(),
            "ok",
            "t",
        )
        .unwrap();
        assert_eq!(attempts(&conn), 0);
        upsert_source_file_resolution_coverage(&conn, "core:file:a.py", &transient, "again", "t")
            .unwrap();
        assert_eq!(attempts(&conn), 0);
        assert_eq!(files_needing_resolution_redispatch(&conn).unwrap().len(), 1);
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
        assert_eq!(paths(&files), vec![("/p/hole.py", false)]);

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
            ResolutionCoverageSummary::default()
        );
        assert_eq!(degraded_call_coverage_file_count(&conn).unwrap(), 0);
    }

    fn exhaust(conn: &Connection, id: &str) {
        let transient = SourceFileResolutionCoverage {
            calls: degraded(true),
            references: FacetCoverageRecord::default(),
        };
        for run in 0..=MAX_REDISPATCH_ATTEMPTS {
            upsert_source_file_resolution_coverage(conn, id, &transient, &format!("r{run}"), "t")
                .unwrap();
        }
    }

    fn attempts(conn: &Connection, id: &str) -> i64 {
        conn.query_row(
            "SELECT redispatch_attempts FROM source_file_resolution_coverage \
             WHERE source_file_id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn reset_exhausted_redispatch_budget_zeroes_only_rows_at_or_over_the_budget() {
        let conn = migrated_conn();
        for (id, path) in [
            ("core:file:a.py", "/p/a.py"),
            ("core:file:b.py", "/p/b.py"),
            ("core:file:c.py", "/p/c.py"),
        ] {
            insert_entity(&conn, id, "core", "file", path, None, None);
        }
        exhaust(&conn, "core:file:a.py");
        // b.py: transient, one degraded re-run, still under budget.
        let transient = SourceFileResolutionCoverage {
            calls: degraded(true),
            references: FacetCoverageRecord::default(),
        };
        upsert_source_file_resolution_coverage(&conn, "core:file:b.py", &transient, "r1", "t")
            .unwrap();
        upsert_source_file_resolution_coverage(&conn, "core:file:b.py", &transient, "r2", "t")
            .unwrap();
        // c.py: permanently degraded (content-determined), never counted.
        let permanent = SourceFileResolutionCoverage {
            calls: FacetCoverageRecord::default(),
            references: degraded(false),
        };
        upsert_source_file_resolution_coverage(&conn, "core:file:c.py", &permanent, "r1", "t")
            .unwrap();
        assert_eq!(attempts(&conn, "core:file:a.py"), MAX_REDISPATCH_ATTEMPTS);
        assert_eq!(attempts(&conn, "core:file:b.py"), 1);
        assert_eq!(attempts(&conn, "core:file:c.py"), 0);

        assert_eq!(reset_exhausted_redispatch_budget(&conn).unwrap(), 1);

        assert_eq!(
            attempts(&conn, "core:file:a.py"),
            0,
            "exhausted row re-armed"
        );
        assert_eq!(
            attempts(&conn, "core:file:b.py"),
            1,
            "under-budget row untouched"
        );
        assert_eq!(
            attempts(&conn, "core:file:c.py"),
            0,
            "permanent row untouched"
        );
        let (status, transient_flag): (String, i64) = conn
            .query_row(
                "SELECT references_status, references_transient \
                 FROM source_file_resolution_coverage WHERE source_file_id = 'core:file:c.py'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((status.as_str(), transient_flag), ("degraded", 0));
    }

    #[test]
    fn reset_exhausted_redispatch_budget_makes_the_file_eligible_for_redispatch_again() {
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
        exhaust(&conn, "core:file:a.py");
        assert!(
            files_needing_resolution_redispatch(&conn)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            degraded_resolution_coverage_summary(&conn)
                .unwrap()
                .exhausted,
            1
        );

        assert_eq!(reset_exhausted_redispatch_budget(&conn).unwrap(), 1);

        assert_eq!(
            paths(&files_needing_resolution_redispatch(&conn).unwrap()),
            vec![("/p/a.py", true)]
        );
        let summary = degraded_resolution_coverage_summary(&conn).unwrap();
        assert_eq!((summary.transient, summary.exhausted), (1, 0));
    }

    #[test]
    fn reset_exhausted_redispatch_budget_returns_zero_when_nothing_is_exhausted() {
        let conn = migrated_conn();
        assert_eq!(reset_exhausted_redispatch_budget(&conn).unwrap(), 0);
        insert_entity(
            &conn,
            "core:file:a.py",
            "core",
            "file",
            "/p/a.py",
            None,
            None,
        );
        upsert_source_file_resolution_coverage(
            &conn,
            "core:file:a.py",
            &SourceFileResolutionCoverage::default(),
            "r1",
            "t",
        )
        .unwrap();
        assert_eq!(reset_exhausted_redispatch_budget(&conn).unwrap(), 0);
        assert_eq!(attempts(&conn, "core:file:a.py"), 0);
    }
}
