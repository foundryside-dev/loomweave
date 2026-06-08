//! Stale-finding sweep (clarion-87c1eba2bd / ADR-048).
//!
//! Mirrors the entity prior-index diff (`prior_index.rs`) for findings. The
//! content-keyed finding upsert (`writer::write_finding_row`, ADR-047) refreshes
//! a *reproduced* finding's `run_id` to the current run, but nothing retires a
//! finding that a later run stopped emitting. A finding whose code was fixed (or
//! deleted — `entities` is cumulative, so the `findings → entities` cascade never
//! fires) therefore lingers forever.
//!
//! The diff signal is already established by ADR-047: a reproduced finding carries
//! the current `run_id`; a finding that did NOT reproduce keeps its prior `run_id`.
//! [`sweep_stale_findings`] deletes the latter — but ONLY when it is still
//! transient (`status = 'open'`) and unlinked (`filigree_issue_id IS NULL`). A
//! finding that carries a Filigree issue id or a non-`open` status
//! (`acknowledged` / `suppressed` / `promoted_to_issue`) represents an operator
//! decision and is owned by the Filigree-side unseen / soft-archive lifecycle
//! (ADR-029 / ADR-047) — never by this local sweep. The two paths are disjoint by
//! construction: this sweep touches only `filigree_issue_id IS NULL` rows.
//!
//! Correctness depends on the CALLER gating the sweep to a clean full pass — a
//! `Completed`, non-`--resume`, fully-walked run (no incrementally-skipped files
//! AND no source-walk errors), non-`--no-sei` — so that `run_id <> current`
//! unambiguously means "the current run walked this finding's file and no longer
//! reproduces it." A file the run never read (skipped or walk-errored) keeps its
//! prior `run_id` without having been re-examined and must not be swept. See the
//! call site in `loomweave-cli/src/analyze.rs` for the full gate and its rationale.

use rusqlite::{Connection, params};

use crate::Result;

/// Retire findings the current run no longer reproduces: delete every `open`,
/// Filigree-unlinked finding whose `run_id` is not `current_run_id`. Returns the
/// number of rows deleted.
///
/// Lifecycle preservation (acceptance contract): findings carrying a
/// `filigree_issue_id` or a non-`open` status are NEVER deleted, even when stale.
///
/// This is a raw DELETE with no run-state or transaction management; it runs on
/// the writer actor via the query-time-write path (`WriterCmd::SweepStaleFindings`),
/// post-`CommitRun`, exactly like the prior-index flush.
///
/// # Errors
///
/// Returns [`crate::error::StorageError::Sqlite`] if the statement fails.
pub fn sweep_stale_findings(conn: &Connection, current_run_id: &str) -> Result<usize> {
    let deleted = conn.execute(
        "DELETE FROM findings \
         WHERE status = 'open' \
           AND filigree_issue_id IS NULL \
           AND run_id <> ?1",
        params![current_run_id],
    )?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::apply_migrations;

    /// In-memory connection with the real schema applied, so the `findings` table
    /// shape (and its CHECK constraints on `kind`/`severity`/`status`) come from
    /// migration 0001, never a hand-written DDL that could drift. The single
    /// anchor entity every finding references is seeded here (`foreign_keys` is ON
    /// — production parity, so the `findings.entity_id`/`run_id` FKs are enforced).
    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, source_file_path, properties, \
              content_hash, created_at, updated_at) \
             VALUES ('python:function:x', 'python', 'function', 'x', 'x', '/x.py', \
                     '{}', 'h', 't', 't')",
            [],
        )
        .unwrap();
        conn
    }

    /// Insert a minimal finding row. Only the columns the sweep predicates on
    /// (`status`, `filigree_issue_id`, `run_id`) vary; the rest are fixed valid
    /// values that satisfy the NOT NULL + CHECK constraints. The referenced `run`
    /// row is seeded on demand (`INSERT OR IGNORE`) so the `run_id` FK resolves.
    fn insert_finding(
        conn: &Connection,
        id: &str,
        run_id: &str,
        status: &str,
        filigree_issue_id: Option<&str>,
    ) {
        conn.execute(
            "INSERT OR IGNORE INTO runs (id, started_at, config, stats, status) \
             VALUES (?1, 't', '{}', '{}', 'completed')",
            params![run_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO findings ( \
                id, tool, tool_version, run_id, rule_id, kind, severity, \
                entity_id, related_entities, message, evidence, properties, \
                supports, supported_by, status, filigree_issue_id, \
                created_at, updated_at \
             ) VALUES ( \
                ?1, 'loomweave', '0', ?2, 'LMWV-TEST-RULE', 'defect', 'WARN', \
                'python:function:x', '[]', 'm', '[]', '{}', \
                '[]', '[]', ?3, ?4, 't', 't' \
             )",
            params![id, run_id, status, filigree_issue_id],
        )
        .unwrap();
    }

    fn ids(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT id FROM findings ORDER BY id").unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect()
    }

    #[test]
    fn retires_open_unlinked_stale_finding() {
        // The core behaviour: an open, Filigree-unlinked finding from a prior run
        // (run_id != current) is retired.
        let conn = migrated_conn();
        insert_finding(&conn, "core:finding:stale", "run-1", "open", None);
        let deleted = sweep_stale_findings(&conn, "run-2").unwrap();
        assert_eq!(deleted, 1);
        assert!(ids(&conn).is_empty());
    }

    #[test]
    fn preserves_reproduced_finding_at_current_run() {
        // A finding the current run re-emitted carries the current run_id (the
        // upsert set it) and must survive.
        let conn = migrated_conn();
        insert_finding(&conn, "core:finding:fresh", "run-2", "open", None);
        let deleted = sweep_stale_findings(&conn, "run-2").unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(ids(&conn), ["core:finding:fresh"]);
    }

    #[test]
    fn preserves_stale_finding_carrying_filigree_issue_id() {
        // A stale finding promoted to / linked to a Filigree issue is an operator
        // decision owned by the Filigree lifecycle — never swept locally.
        let conn = migrated_conn();
        insert_finding(
            &conn,
            "core:finding:linked",
            "run-1",
            "open",
            Some("clarion-sf-abc123"),
        );
        let deleted = sweep_stale_findings(&conn, "run-2").unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(ids(&conn), ["core:finding:linked"]);
    }

    #[test]
    fn preserves_stale_findings_with_non_open_status() {
        // acknowledged / suppressed / promoted_to_issue are operator decisions:
        // preserved even when stale and Filigree-unlinked.
        let conn = migrated_conn();
        insert_finding(&conn, "core:finding:ack", "run-1", "acknowledged", None);
        insert_finding(&conn, "core:finding:sup", "run-1", "suppressed", None);
        insert_finding(
            &conn,
            "core:finding:promo",
            "run-1",
            "promoted_to_issue",
            None,
        );
        let deleted = sweep_stale_findings(&conn, "run-2").unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(
            ids(&conn),
            ["core:finding:ack", "core:finding:promo", "core:finding:sup"]
        );
    }

    #[test]
    fn sweeps_only_stale_open_unlinked_in_a_mixed_set() {
        // One pass over a realistic mix: only the open+unlinked+stale row goes.
        let conn = migrated_conn();
        insert_finding(&conn, "core:finding:a-stale-open", "run-1", "open", None);
        insert_finding(&conn, "core:finding:b-fresh-open", "run-2", "open", None);
        insert_finding(
            &conn,
            "core:finding:c-stale-linked",
            "run-1",
            "open",
            Some("clarion-sf-1"),
        );
        insert_finding(
            &conn,
            "core:finding:d-stale-sup",
            "run-1",
            "suppressed",
            None,
        );
        let deleted = sweep_stale_findings(&conn, "run-2").unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(
            ids(&conn),
            [
                "core:finding:b-fresh-open",
                "core:finding:c-stale-linked",
                "core:finding:d-stale-sup",
            ]
        );
    }

    #[test]
    fn empty_table_sweeps_nothing() {
        let conn = migrated_conn();
        assert_eq!(sweep_stale_findings(&conn, "run-1").unwrap(), 0);
    }
}
