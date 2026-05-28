//! Shared project snapshot: entity/subsystem/finding counts + index staleness.
//!
//! One function, two callers: the `clarion hook session-start` subcommand and
//! the MCP `clarion://context` resource. Infallible by design — every failure
//! folds into the snapshot (zero counts, `Staleness::Unknown`) so the fail-soft
//! hook never has to handle an error. Degrade, but don't go quiet: a real query
//! failure is `tracing::warn!`-logged before it folds, so a populated index
//! reporting 0 leaves a trace (run with `RUST_LOG=warn`).

use std::path::Path;
use std::time::SystemTime;

use rusqlite::Connection;
use serde::Serialize;

/// Freshness of the `.clarion/` index relative to the source files Clarion
/// ingested. See the plan's Decision Point (b) for the algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Staleness {
    /// No completed analyze run has ever been recorded.
    NeverAnalyzed,
    /// At least one ingested source file is newer than the latest run.
    Stale,
    /// No ingested source file is newer than the latest run.
    Fresh,
    /// Could not determine (stat/parse/IO error) — degrade, don't fail (and log).
    Unknown,
}

/// Counts + freshness for one Clarion project, safe to serialize into the MCP
/// resource or print from the hook.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSnapshot {
    pub db_present: bool,
    pub entity_count: i64,
    pub subsystem_count: i64,
    pub finding_count: i64,
    pub staleness: Staleness,
    /// Latest run `completed_at` (ISO-8601) if any, else `None`.
    pub last_analyzed_at: Option<String>,
}

/// Build a snapshot from an already-open migrated `Connection`.
///
/// `db_present` is always `true` here (the caller opened the connection); the
/// `false` case is produced by the caller when the db file is missing.
#[must_use]
pub fn project_snapshot(conn: &Connection, project_root: &Path) -> ProjectSnapshot {
    let entity_count = scalar_count(conn, "SELECT COUNT(*) FROM entities");
    let subsystem_count = scalar_count(
        conn,
        "SELECT COUNT(*) FROM entities WHERE kind = 'subsystem'",
    );
    let finding_count = scalar_count(conn, "SELECT COUNT(*) FROM findings");

    let last_analyzed_at = latest_completed_run(conn);
    let staleness = compute_staleness(conn, project_root, last_analyzed_at.as_deref());

    ProjectSnapshot {
        db_present: true,
        entity_count,
        subsystem_count,
        finding_count,
        staleness,
        last_analyzed_at,
    }
}

/// A missing-database snapshot: all zeros, `NeverAnalyzed`, no timestamp.
#[must_use]
pub fn missing_db_snapshot() -> ProjectSnapshot {
    ProjectSnapshot {
        db_present: false,
        entity_count: 0,
        subsystem_count: 0,
        finding_count: 0,
        staleness: Staleness::NeverAnalyzed,
        last_analyzed_at: None,
    }
}

fn scalar_count(conn: &Connection, sql: &str) -> i64 {
    match conn.query_row(sql, [], |row| row.get::<_, i64>(0)) {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(error = %err, sql, "clarion snapshot count query failed; reporting 0");
            0
        }
    }
}

fn latest_completed_run(conn: &Connection) -> Option<String> {
    match conn.query_row(
        "SELECT completed_at FROM runs \
         WHERE completed_at IS NOT NULL AND status = 'completed' \
         ORDER BY completed_at DESC LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(s) => Some(s),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(err) => {
            tracing::warn!(error = %err, "clarion latest-completed-run query failed");
            None
        }
    }
}

fn compute_staleness(
    conn: &Connection,
    project_root: &Path,
    last_analyzed_at: Option<&str>,
) -> Staleness {
    let Some(run_iso) = last_analyzed_at else {
        return Staleness::NeverAnalyzed;
    };
    let Some(run_time) = parse_iso8601_to_systemtime(run_iso) else {
        return Staleness::Unknown;
    };

    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT source_file_path FROM entities \
         WHERE source_file_path IS NOT NULL",
    ) else {
        return Staleness::Unknown;
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
        return Staleness::Unknown;
    };

    let mut saw_any_file = false;
    for rel in rows.flatten() {
        let abs = if Path::new(&rel).is_absolute() {
            std::path::PathBuf::from(&rel)
        } else {
            project_root.join(&rel)
        };
        match abs.metadata().and_then(|m| m.modified()) {
            Ok(mtime) => {
                saw_any_file = true;
                if mtime > run_time {
                    return Staleness::Stale;
                }
            }
            Err(_) => return Staleness::Unknown,
        }
    }
    if saw_any_file {
        Staleness::Fresh
    } else {
        Staleness::Unknown
    }
}

/// Parse a strict RFC3339 UTC timestamp (the format
/// `strftime('%Y-%m-%dT%H:%M:%fZ','now')` writes into `runs.completed_at`) to a
/// `SystemTime`. Returns `None` on any deviation.
fn parse_iso8601_to_systemtime(iso: &str) -> Option<SystemTime> {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    let odt = OffsetDateTime::parse(iso, &Rfc3339).ok()?;
    Some(SystemTime::from(odt))
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use clarion_storage::{pragma, schema};

    use super::{Staleness, project_snapshot};

    // `apply_write_pragmas` enforces ADR-011's WAL journal-mode invariant, which
    // an in-memory connection cannot satisfy (`journal_mode=memory`). Back the
    // test db with a file in a `TempDir`, matching the canonical pattern in
    // `clarion-storage`'s own integration tests. The `TempDir` is returned so the
    // caller keeps it alive for the connection's lifetime.
    fn migrated_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clarion.db");
        let mut conn = Connection::open(path).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
        (dir, conn)
    }

    fn insert_entity(conn: &Connection, id: &str, kind: &str, source_file_path: Option<&str>) {
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, source_file_path, created_at, updated_at) \
             VALUES (?1, 'python', ?2, ?3, ?3, '{}', ?4, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            rusqlite::params![id, kind, id, source_file_path],
        )
        .unwrap();
    }

    #[test]
    fn counts_entities_subsystems_and_findings() {
        let (_dir, conn) = migrated_conn();
        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        insert_entity(&conn, "python:function:a.f", "function", Some("a.py"));
        insert_entity(&conn, "core:subsystem:abc", "subsystem", None);
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('run1', '2026-01-01T00:00:00.000Z', '2026-01-02T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO findings \
             (id, tool, tool_version, run_id, rule_id, kind, severity, entity_id, \
              related_entities, message, evidence, properties, supports, supported_by, status, created_at, updated_at) \
             VALUES ('f1','clarion','1.0','run1','R1','defect','WARN','python:module:a', \
                     '[]','m','{}','{}','[]','[]','open','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, std::path::Path::new("/nonexistent-root"));
        assert!(snap.db_present);
        assert_eq!(snap.entity_count, 3);
        assert_eq!(snap.subsystem_count, 1);
        assert_eq!(snap.finding_count, 1);
        // No source files exist under /nonexistent-root, but there IS a completed
        // run, so staleness degrades to Unknown (stat failure folds to Unknown).
        assert_eq!(snap.staleness, Staleness::Unknown);
    }

    #[test]
    fn never_analyzed_when_no_completed_run() {
        let (_dir, conn) = migrated_conn();
        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
        assert_eq!(snap.staleness, Staleness::NeverAnalyzed);
        assert!(snap.last_analyzed_at.is_none());
    }

    #[test]
    fn fresh_when_all_sources_older_than_run() {
        let (_dir, conn) = migrated_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.py");
        std::fs::write(&src, "x = 1\n").unwrap();

        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, dir.path());
        assert_eq!(snap.staleness, Staleness::Fresh, "{snap:?}");
        assert_eq!(
            snap.last_analyzed_at.as_deref(),
            Some("2099-01-01T00:00:00.000Z")
        );
    }

    #[test]
    fn stale_when_a_source_is_newer_than_run() {
        let (_dir, conn) = migrated_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.py");
        std::fs::write(&src, "x = 1\n").unwrap();

        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2000-01-01T00:00:00.000Z', '2000-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, dir.path());
        assert_eq!(snap.staleness, Staleness::Stale, "{snap:?}");
    }
}
