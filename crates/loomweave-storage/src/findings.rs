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

/// Rule-scoped variant of [`sweep_stale_findings`]: retire stale (`run_id` !=
/// current), `open`, Filigree-unlinked findings whose `rule_id` is in
/// `rule_ids`, leaving every other rule's rows alone. For rule families whose
/// producer is a FULL pass on every run regardless of the incremental file
/// skip — the pre-ingest secret scan (`LMWV-SEC-*` + the baseline-match audit
/// fact) re-walks every source file and sidecar each run — `run_id != current`
/// already unambiguously means "looked, no longer detected", so these rows can
/// be retired on incremental runs the general sweep must skip
/// (weft-7256739b31 / dogfood-4 B10: stale secret findings accumulated across
/// incremental re-analyses). Same lifecycle preservation as the general sweep.
///
/// `examined_source_files` bounds the sweep to files the producer actually
/// re-examined this run (L3). The "full pass" is full only over the
/// CURRENTLY-installed plugins' extension union, so uninstalling/disabling a
/// plugin between runs silently drops its files from the scan with no walk
/// error — and the caller's `source_walk_skipped_entries == 0` gate cannot see
/// that scope shrinkage. Without this bound a vanished-from-scope file's
/// still-valid finding would be retired as "looked, clean" when it was never
/// looked at again. A finding is retired only when its anchor entity's
/// `source_file_path` is in this set (canonical-absolute strings, the form
/// entities store). An empty set retires nothing — the conservative default
/// when no file was examined.
///
/// # Errors
///
/// Returns [`crate::error::StorageError::Sqlite`] if the statement fails.
pub fn sweep_stale_findings_for_rules(
    conn: &Connection,
    current_run_id: &str,
    rule_ids: &[&str],
    examined_source_files: &[&str],
) -> Result<usize> {
    if rule_ids.is_empty() || examined_source_files.is_empty() {
        return Ok(0);
    }
    let rule_placeholders = std::iter::repeat_n("?", rule_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let file_placeholders = std::iter::repeat_n("?", examined_source_files.len())
        .collect::<Vec<_>>()
        .join(", ");
    // Retire a stale secret finding only when its anchor entity's source file
    // was among those re-examined this run (L3 scope-shrinkage guard).
    let sql = format!(
        "DELETE FROM findings \
         WHERE status = 'open' \
           AND filigree_issue_id IS NULL \
           AND run_id <> ? \
           AND rule_id IN ({rule_placeholders}) \
           AND entity_id IN ( \
               SELECT id FROM entities \
               WHERE source_file_path IN ({file_placeholders}) \
           )"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params = std::iter::once(current_run_id)
        .chain(rule_ids.iter().copied())
        .chain(examined_source_files.iter().copied())
        .collect::<Vec<_>>();
    let deleted = stmt.execute(rusqlite::params_from_iter(params))?;
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

    /// Like [`insert_finding`] but with an explicit `rule_id`, for the
    /// rule-scoped sweep tests.
    fn insert_finding_with_rule(conn: &Connection, id: &str, run_id: &str, rule_id: &str) {
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
                ?1, 'loomweave', '0', ?2, ?3, 'defect', 'WARN', \
                'python:function:x', '[]', 'm', '[]', '{}', \
                '[]', '[]', 'open', NULL, 't', 't' \
             )",
            params![id, run_id, rule_id],
        )
        .unwrap();
    }

    #[test]
    fn rule_scoped_sweep_retires_only_named_rules() {
        // weft-7256739b31: the scoped sweep retires stale rows of the named
        // rules and must not touch a stale row of any OTHER rule (those rows
        // belong to the gated full-pass sweep — an incrementally-skipped file's
        // still-reproducing findings live there).
        let conn = migrated_conn();
        insert_finding_with_rule(
            &conn,
            "core:finding:secret-stale",
            "run-1",
            "LMWV-SEC-SECRET-DETECTED",
        );
        insert_finding_with_rule(
            &conn,
            "core:finding:secret-fresh",
            "run-2",
            "LMWV-SEC-SECRET-DETECTED",
        );
        insert_finding_with_rule(
            &conn,
            "core:finding:other-stale",
            "run-1",
            "LMWV-PY-SYNTAX-ERROR",
        );
        let deleted = sweep_stale_findings_for_rules(
            &conn,
            "run-2",
            &[
                "LMWV-SEC-SECRET-DETECTED",
                "LMWV-INFRA-SECRET-BASELINE-MATCH",
            ],
            // The single seeded entity's file was re-examined this run.
            &["/x.py"],
        )
        .unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(
            ids(&conn),
            ["core:finding:other-stale", "core:finding:secret-fresh"]
        );
    }

    #[test]
    fn rule_scoped_sweep_preserves_findings_whose_file_was_not_re_examined() {
        // L3 scope-shrinkage guard: a plugin uninstalled between runs drops its
        // files from the scan with no walk error. A stale secret finding on such
        // a file ("never looked again") must survive — only findings whose
        // anchor file WAS re-examined this run may be retired.
        let conn = migrated_conn();
        // Second entity, anchored to a file that is NOT in this run's scan scope
        // (e.g. owned by an uninstalled plugin's extension).
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, source_file_path, properties, \
              content_hash, created_at, updated_at) \
             VALUES ('rust:module:y', 'rust', 'module', 'y', 'y', '/y.rs', \
                     '{}', 'h', 't', 't')",
            [],
        )
        .unwrap();
        // Both findings are stale (run-1) secret rows.
        insert_finding_with_rule(
            &conn,
            "core:finding:examined",
            "run-1",
            "LMWV-SEC-SECRET-DETECTED",
        );
        // Re-point the second finding to the out-of-scope entity.
        insert_finding_with_rule(
            &conn,
            "core:finding:unexamined",
            "run-1",
            "LMWV-SEC-SECRET-DETECTED",
        );
        conn.execute(
            "UPDATE findings SET entity_id = 'rust:module:y' \
             WHERE id = 'core:finding:unexamined'",
            [],
        )
        .unwrap();

        // This run examined only /x.py (the .rs scope shrank away).
        let deleted = sweep_stale_findings_for_rules(
            &conn,
            "run-2",
            &["LMWV-SEC-SECRET-DETECTED"],
            &["/x.py"],
        )
        .unwrap();
        assert_eq!(deleted, 1, "only the re-examined file's stale finding is retired");
        assert_eq!(
            ids(&conn),
            ["core:finding:unexamined"],
            "the un-examined file's finding survives (never looked again ≠ looked, clean)"
        );
    }

    #[test]
    fn rule_scoped_sweep_with_no_examined_files_is_a_no_op() {
        // The conservative default: if the scan examined nothing, retire nothing
        // (an empty examined set must never sweep the whole rule family).
        let conn = migrated_conn();
        insert_finding_with_rule(&conn, "core:finding:x", "run-1", "LMWV-SEC-SECRET-DETECTED");
        assert_eq!(
            sweep_stale_findings_for_rules(&conn, "run-2", &["LMWV-SEC-SECRET-DETECTED"], &[])
                .unwrap(),
            0
        );
        assert_eq!(ids(&conn), ["core:finding:x"]);
    }

    #[test]
    fn rule_scoped_sweep_preserves_lifecycle_rows() {
        // Filigree-linked / non-open rows are operator decisions: preserved even
        // when stale and rule-matched, exactly like the general sweep.
        let conn = migrated_conn();
        insert_finding(
            &conn,
            "core:finding:linked",
            "run-1",
            "open",
            Some("clarion-sf-1"),
        );
        conn.execute(
            "UPDATE findings SET rule_id = 'LMWV-SEC-SECRET-DETECTED' \
             WHERE id = 'core:finding:linked'",
            [],
        )
        .unwrap();
        let deleted = sweep_stale_findings_for_rules(
            &conn,
            "run-2",
            &["LMWV-SEC-SECRET-DETECTED"],
            &["/x.py"],
        )
        .unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(ids(&conn), ["core:finding:linked"]);
    }

    #[test]
    fn rule_scoped_sweep_with_no_rules_is_a_no_op() {
        let conn = migrated_conn();
        insert_finding_with_rule(&conn, "core:finding:x", "run-1", "LMWV-SEC-SECRET-DETECTED");
        assert_eq!(
            sweep_stale_findings_for_rules(&conn, "run-2", &[], &["/x.py"]).unwrap(),
            0
        );
    }
}
