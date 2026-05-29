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
///
/// Freshness is computed by stat-ing only the files already recorded in
/// `entities.source_file_path`, so it detects *modified* ingested files but is
/// blind to files *added* (not yet ingested, so absent from the table) or
/// *deleted* since the last run. A repo that gained or lost source files
/// without touching any ingested file can therefore still report [`Fresh`]; the
/// verdict is a best-effort nudge, not a guarantee. Added/removed-file
/// detection is tracked as a `release:1.1` follow-up.
///
/// [`Fresh`]: Staleness::Fresh
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Staleness {
    /// No completed analyze run has ever been recorded.
    NeverAnalyzed,
    /// At least one ingested source file is newer than the latest run. (Does
    /// not account for added/removed files — see the type-level note.)
    Stale,
    /// No ingested source file is newer than the latest run. (Does not account
    /// for added/removed files — see the type-level note.)
    Fresh,
    /// A completed run exists, but no ingested entity has a resolvable
    /// `source_file_path` to stat — there is *nothing to compare against*, so
    /// freshness is neither Fresh nor Stale. A normal outcome (e.g. a project
    /// whose only entities are subsystems), distinct from [`Unknown`]: no query
    /// or stat failed, so it never sets `degraded`.
    ///
    /// [`Unknown`]: Staleness::Unknown
    NoSourcePaths,
    /// Could not determine because a query/parse/stat *failed* — degrade, don't
    /// fail (and log). Strictly the error fold: "nothing to compare" is
    /// [`NoSourcePaths`], not `Unknown`.
    ///
    /// [`NoSourcePaths`]: Staleness::NoSourcePaths
    Unknown,
}

/// Counts + freshness for one Clarion project, safe to serialize into the MCP
/// resource or print from the hook.
///
/// Fields are private and read through accessors so the documented invariant —
/// `db_present == false` implies zero counts and [`Staleness::NeverAnalyzed`] —
/// cannot be violated: the only ways to build one are the three constructors in
/// this module ([`project_snapshot`], [`missing_db_snapshot`],
/// [`unreadable_db_snapshot`]), each of which upholds it. No external caller can
/// assemble a `db_present: false` snapshot carrying non-zero counts
/// (clarion-e0a4937d89). Serialization is unaffected — serde uses the field
/// names regardless of visibility, so the wire shape is identical.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSnapshot {
    db_present: bool,
    entity_count: i64,
    subsystem_count: i64,
    finding_count: i64,
    staleness: Staleness,
    /// Latest run `completed_at` (ISO-8601) if any, else `None`.
    last_analyzed_at: Option<String>,
    /// `true` when this snapshot was produced from a *failure* rather than a
    /// healthy read: at least one backing SQL query failed unexpectedly and was
    /// folded to a safe default (a count to `0`, the run lookup to `None`, or
    /// the staleness scan to [`Staleness::Unknown`]), or the snapshot was built
    /// by a caller's reader-pool fallback. Lets an MCP consumer distinguish
    /// "machinery broke" from a genuinely empty-but-present index, which
    /// otherwise serialize byte-identically (`db_present: true`, all counts `0`,
    /// `staleness: unknown`). Environmental staleness (a missing/unstat-able
    /// source file folding to `Unknown`) is *not* degradation — that is a normal
    /// outcome signalled by `staleness` itself, not a DB-machinery failure.
    degraded: bool,
}

impl ProjectSnapshot {
    /// Whether a readable `.clarion/clarion.db` was found. When `false`, every
    /// count is `0` and `staleness` is [`Staleness::NeverAnalyzed`].
    #[must_use]
    pub fn db_present(&self) -> bool {
        self.db_present
    }

    /// Total entity rows (subsystems included — see [`subsystem_count`]).
    ///
    /// [`subsystem_count`]: ProjectSnapshot::subsystem_count
    #[must_use]
    pub fn entity_count(&self) -> i64 {
        self.entity_count
    }

    /// Entities of kind `subsystem` — a *subset* of [`entity_count`], not a
    /// disjoint category.
    ///
    /// [`entity_count`]: ProjectSnapshot::entity_count
    #[must_use]
    pub fn subsystem_count(&self) -> i64 {
        self.subsystem_count
    }

    /// Total finding rows.
    #[must_use]
    pub fn finding_count(&self) -> i64 {
        self.finding_count
    }

    /// Index freshness verdict.
    #[must_use]
    pub fn staleness(&self) -> Staleness {
        self.staleness
    }

    /// Latest run `completed_at` (ISO-8601) if any.
    #[must_use]
    pub fn last_analyzed_at(&self) -> Option<&str> {
        self.last_analyzed_at.as_deref()
    }

    /// `true` when this snapshot was folded from a backing-query failure — see
    /// the field-level note for the precise contract.
    #[must_use]
    pub fn degraded(&self) -> bool {
        self.degraded
    }
}

/// Build a snapshot from an already-open migrated `Connection`.
///
/// `db_present` is always `true` here (the caller opened the connection); the
/// `false` case is produced by the caller when the db file is missing.
#[must_use]
pub fn project_snapshot(conn: &Connection, project_root: &Path) -> ProjectSnapshot {
    // Accumulates any SQL-machinery failure folded below into a wire-visible
    // `degraded` flag, so the consumer can tell a broken read from an empty one.
    let mut degraded = false;

    let entity_count = scalar_count(conn, "SELECT COUNT(*) FROM entities", &mut degraded);
    let subsystem_count = scalar_count(
        conn,
        "SELECT COUNT(*) FROM entities WHERE kind = 'subsystem'",
        &mut degraded,
    );
    let finding_count = scalar_count(conn, "SELECT COUNT(*) FROM findings", &mut degraded);

    let last_analyzed_at = latest_completed_run(conn, &mut degraded);
    let staleness = compute_staleness(
        conn,
        project_root,
        last_analyzed_at.as_deref(),
        &mut degraded,
    );

    ProjectSnapshot {
        db_present: true,
        entity_count,
        subsystem_count,
        finding_count,
        staleness,
        last_analyzed_at,
        degraded,
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
        degraded: false,
    }
}

/// A degraded snapshot for a database that *is* present but could not be read
/// or serialized (the MCP `clarion://context` reader-pool / serialize-error
/// fallback): `db_present: true`, all counts `0`, [`Staleness::Unknown`], no
/// timestamp, and `degraded: true` so a consumer never mistakes the zero counts
/// for a genuinely empty index. The single construction site for this case,
/// replacing the inline struct literal that the private fields now forbid
/// (clarion-e0a4937d89).
#[must_use]
pub fn unreadable_db_snapshot() -> ProjectSnapshot {
    ProjectSnapshot {
        db_present: true,
        entity_count: 0,
        subsystem_count: 0,
        finding_count: 0,
        staleness: Staleness::Unknown,
        last_analyzed_at: None,
        degraded: true,
    }
}

/// Run a scalar `COUNT(*)` query. On failure, log, fold to `0`, and set
/// `*degraded` so the caller can mark the whole snapshot as a degraded read.
fn scalar_count(conn: &Connection, sql: &str, degraded: &mut bool) -> i64 {
    match conn.query_row(sql, [], |row| row.get::<_, i64>(0)) {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(error = %err, sql, "clarion snapshot count query failed; reporting 0");
            *degraded = true;
            0
        }
    }
}

/// Look up the latest completed run's `completed_at`. `QueryReturnedNoRows` is a
/// normal "never analyzed" outcome and does *not* degrade; any other error is a
/// machinery failure that folds to `None` and sets `*degraded`.
fn latest_completed_run(conn: &Connection, degraded: &mut bool) -> Option<String> {
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
            *degraded = true;
            None
        }
    }
}

fn compute_staleness(
    conn: &Connection,
    project_root: &Path,
    last_analyzed_at: Option<&str>,
    degraded: &mut bool,
) -> Staleness {
    let Some(run_iso) = last_analyzed_at else {
        return Staleness::NeverAnalyzed;
    };
    let Some(run_time) = parse_iso8601_to_systemtime(run_iso) else {
        // A run timestamp we can't parse is a data/machinery fault, not an
        // environmental one — mark degraded alongside the Unknown verdict.
        *degraded = true;
        return Staleness::Unknown;
    };

    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT source_file_path FROM entities \
         WHERE source_file_path IS NOT NULL",
    ) else {
        *degraded = true;
        return Staleness::Unknown;
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
        *degraded = true;
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
        // A completed run with no resolvable source file to stat: nothing to
        // compare, NOT an error. Kept distinct from the error folds above so
        // `Unknown` means strictly "a query/parse/stat failed" (clarion-22add08e98).
        Staleness::NoSourcePaths
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

    // `compute_staleness` has two error folds to `Unknown` — (a) the run
    // timestamp fails to parse; (c) a stat/`modified()` error — plus one
    // non-error fold, (b) a completed run exists but no entity has a resolvable
    // source_file_path (`saw_any_file == false`), which is its own
    // `NoSourcePaths` variant (clarion-22add08e98). `counts_entities_subsystems_and_findings`
    // and `stat_failure_unknown_is_not_degraded` cover (c); the two tests below
    // lock (a) → Unknown+degraded and (b) → NoSourcePaths (never degraded).

    #[test]
    fn unknown_and_degraded_when_run_timestamp_unparseable() {
        // (a) An unparseable `completed_at` is a data/machinery fault: Unknown
        // staleness AND degraded. (`completed_at` is plain TEXT — no format
        // CHECK — so a garbage value is insertable.)
        let (_dir, conn) = migrated_conn();
        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2026-01-01T00:00:00.000Z', 'not-a-timestamp', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
        assert_eq!(snap.staleness, Staleness::Unknown, "{snap:?}");
        assert!(
            snap.degraded,
            "an unparseable run timestamp is a machinery fault: {snap:?}"
        );
        // The raw (unparseable) value is still surfaced verbatim as last_analyzed_at.
        assert_eq!(snap.last_analyzed_at.as_deref(), Some("not-a-timestamp"));
    }

    #[test]
    fn no_source_paths_when_no_entity_has_a_source_file() {
        // (b) The realistic case: a completed run exists, but every entity is
        // subsystem-only (NULL source_file_path), so the DISTINCT scan returns
        // no rows and `saw_any_file` stays false. That is NOT an error fold to
        // Unknown — it is its own `NoSourcePaths` verdict, and never degraded.
        let (_dir, conn) = migrated_conn();
        insert_entity(&conn, "core:subsystem:abc", "subsystem", None);
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2026-01-01T00:00:00.000Z', '2026-01-02T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
        assert_eq!(snap.staleness, Staleness::NoSourcePaths, "{snap:?}");
        assert!(
            !snap.degraded,
            "no-resolvable-source-files is not a failure, never degraded: {snap:?}"
        );
        assert_eq!(
            snap.last_analyzed_at.as_deref(),
            Some("2026-01-02T00:00:00.000Z")
        );
    }

    #[test]
    fn no_source_paths_serializes_to_snake_case() {
        // The new wire value is `"no_source_paths"` (serde rename_all =
        // "snake_case"); pin it so the clarion://context / project_status
        // vocabulary can't drift silently.
        let json = serde_json::to_value(Staleness::NoSourcePaths).unwrap();
        assert_eq!(json, serde_json::Value::String("no_source_paths".into()));
    }

    #[test]
    fn healthy_empty_index_is_not_degraded() {
        let (_dir, conn) = migrated_conn();
        let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
        // All counts 0 and staleness NeverAnalyzed, but every query succeeded —
        // this is a genuinely empty index, NOT a degraded read.
        assert!(
            !snap.degraded,
            "healthy empty index must not be degraded: {snap:?}"
        );
        assert!(snap.db_present);
    }

    #[test]
    fn healthy_populated_index_is_not_degraded() {
        let (_dir, conn) = migrated_conn();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        let snap = project_snapshot(&conn, dir.path());
        assert_eq!(snap.staleness, Staleness::Fresh, "{snap:?}");
        assert!(
            !snap.degraded,
            "fresh healthy read must not be degraded: {snap:?}"
        );
    }

    #[test]
    fn degraded_when_a_count_query_fails() {
        let (_dir, conn) = migrated_conn();
        // Simulate machinery failure: drop a table a count query depends on so
        // the `findings` COUNT(*) errors and folds to 0 via scalar_count.
        conn.execute("DROP TABLE findings", []).unwrap();
        let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
        assert!(
            snap.degraded,
            "a failed count query must mark the snapshot degraded: {snap:?}"
        );
        // The fold itself still produces a safe 0 — degraded is the ONLY signal
        // that distinguishes this from a real empty index.
        assert_eq!(snap.finding_count, 0);
        assert!(snap.db_present);
    }

    #[test]
    fn stat_failure_unknown_is_not_degraded() {
        // Environmental Unknown (a recorded source file that no longer exists on
        // disk) is a normal outcome signalled by `staleness`, NOT a DB-machinery
        // failure — it must leave `degraded` false so the two stay distinct.
        let (_dir, conn) = migrated_conn();
        insert_entity(&conn, "python:module:a", "module", Some("gone.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2026-01-01T00:00:00.000Z', '2026-01-02T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        let snap = project_snapshot(&conn, std::path::Path::new("/nonexistent-root"));
        assert_eq!(snap.staleness, Staleness::Unknown, "{snap:?}");
        assert!(
            !snap.degraded,
            "stat-failure Unknown is environmental, not degraded: {snap:?}"
        );
    }

    #[test]
    fn degraded_field_serializes() {
        let (_dir, conn) = migrated_conn();
        let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["degraded"], serde_json::Value::Bool(false));
    }
}
