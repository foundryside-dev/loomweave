//! Shared project snapshot: entity/subsystem/finding counts + index staleness.
//!
//! One function, two callers: the `loomweave hook session-start` subcommand and
//! the MCP `loomweave://context` resource. Infallible by design — every failure
//! folds into the snapshot (zero counts, `Staleness::Unknown`) so the fail-soft
//! hook never has to handle an error. Degrade, but don't go quiet: a real query
//! failure is `tracing::warn!`-logged before it folds, so a populated index
//! reporting 0 leaves a trace (run with `RUST_LOG=warn`).

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::Connection;
use serde::Serialize;

/// Freshness of the `.loomweave/` index relative to the source files Loomweave
/// ingested. See the plan's Decision Point (b) for the algorithm.
///
/// Freshness combines two passes over the files recorded in
/// `entities.source_file_path` (clarion-e687941a8c):
///
/// 1. **Structural drift** — added / removed / renamed source files. Adding or
///    removing a directory entry bumps the *parent directory's* mtime, so a
///    watched source directory whose mtime is newer than the latest run means
///    its file set changed since analyze, even when no ingested file's own
///    mtime did. This is a conservative nudge: unrelated churn in a source
///    directory (Python's `__pycache__`, an editor's swap/backup file, a
///    `.DS_Store`) also bumps its mtime and can therefore report [`Stale`]
///    when no tracked source actually changed. The watch set is the *direct
///    parents* of ingested files, so an addition/removal in any directory that
///    is not such a parent goes undetected — always including the project root
///    itself, which is deliberately never watched (`analyze` writes `.loomweave/`
///    under it, which would otherwise wedge every check to a permanent Stale).
/// 2. **In-place modification** — an ingested file edited since the run. This
///    needs one `stat` per file and is bounded by `MAX_MODIFICATION_STAT_FILES`
///    so `loomweave hook session-start` stays cheap on large repos
///    (clarion-93465ff89e); the structural pass runs first and short-circuits
///    the common "repo changed" case before any file is stat-ed.
///
/// The verdict is a best-effort nudge, not a guarantee.
///
/// [`Fresh`]: Staleness::Fresh
/// [`Stale`]: Staleness::Stale
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Staleness {
    /// No completed analyze run has ever been recorded.
    NeverAnalyzed,
    /// The index is out of date: a watched source directory's mtime is newer
    /// than the latest run (a file was added / removed / renamed), an ingested
    /// file was modified or deleted since the run, or both. See the type-level
    /// note for the conservative-nudge caveat.
    Stale,
    /// No structural drift in a watched directory and no ingested file newer
    /// than (or missing since) the latest run. Subject to the bounded
    /// modification scan and the unwatched-project-root caveat in the
    /// type-level note.
    Fresh,
    /// The mtime/structural passes found every ingested file fresh, but the
    /// working tree contains untracked source of an already-indexed file type
    /// that the index has never seen — e.g. a brand-new top-level module the
    /// structural pass cannot reach (the unwatched-project-root blind spot;
    /// clarion-26c7e52027). Detected via a hardened, ignore-aware
    /// `git ls-files --others` scoped to ingested extensions (ADR-045); the raw
    /// signal is on [`ProjectSnapshot::worktree_dirty`]. Returned in place of
    /// [`Fresh`] only when that worktree signal is positive — so it never fires
    /// outside a git work tree, and a non-source untracked file never triggers it.
    ///
    /// [`Fresh`]: Staleness::Fresh
    StaleWorktree,
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

/// Counts + freshness for one Loomweave project, safe to serialize into the MCP
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
    /// The git commit HEAD pointed at when the latest completed run was analyzed
    /// (`runs.analyzed_at_commit`), if Loomweave captured one — `None` for a run
    /// analyzed outside a git work tree, or before WS9 began recording it.
    /// Surfaced so the `loomweave://context` resource and the session-start
    /// banner can state *which commit* the index reflects. It is descriptive, not
    /// a freshness signal: a [`Staleness::Fresh`] verdict is only ever fresh
    /// relative to the ingested source files' mtimes — never a claim that HEAD or
    /// the working tree still matches this commit (clarion-26c7e52027). The
    /// `project_status_get` tool already reports the same value as `git_sha`.
    ///
    /// [`Fresh`]: Staleness::Fresh
    indexed_at_commit: Option<String>,
    /// Whether the working tree holds untracked source of an already-indexed file
    /// type that the index does not reflect — the signal behind
    /// [`Staleness::StaleWorktree`] (clarion-26c7e52027, ADR-045). `Some(true)` =
    /// un-indexed source present; `Some(false)` = a git work tree with none;
    /// `None` = not a git work tree, git unavailable, or nothing ingested to scope
    /// against (the check is moot). Computed via a hardened, hash-free
    /// `git ls-files --others --exclude-standard` filtered to ingested file
    /// extensions, so an untracked non-source file (a scratch `notes.txt`) never
    /// flags it, and the untrusted-corpus posture is preserved (no working-tree
    /// hashing — see [`loomweave_core::list_untracked_files`]).
    worktree_dirty: Option<bool>,
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
    /// `true` when the in-place modification scan stopped at
    /// [`MAX_MODIFICATION_STAT_FILES`] without finding drift: the index has more
    /// ingested files than the per-check `stat` cap, so a [`Staleness::Fresh`]
    /// verdict on this snapshot is only proven for the files that were scanned —
    /// an edit beyond the cap may go unnoticed until the next analyze. A
    /// consumer on a very large repo can read this to know a `Fresh` result is
    /// bounded rather than exhaustive (clarion-e687941a8c). Always `false` for a
    /// `Stale`/`Unknown`/`NeverAnalyzed`/`NoSourcePaths` verdict.
    ///
    /// [`Fresh`]: Staleness::Fresh
    scan_truncated: bool,
}

impl ProjectSnapshot {
    /// Whether a readable `.loomweave/loomweave.db` was found. When `false`, every
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

    /// The commit the latest completed run was analyzed at, if captured — see the
    /// field note. `None` when never analyzed, analyzed outside a git repo, or the
    /// `analyzed_at_commit` column is NULL.
    #[must_use]
    pub fn indexed_at_commit(&self) -> Option<&str> {
        self.indexed_at_commit.as_deref()
    }

    /// Whether the working tree holds untracked source the index has not seen —
    /// see the field note. `None` outside a git work tree / with nothing ingested.
    #[must_use]
    pub fn worktree_dirty(&self) -> Option<bool> {
        self.worktree_dirty
    }

    /// `true` when this snapshot was folded from a backing-query failure — see
    /// the field-level note for the precise contract.
    #[must_use]
    pub fn degraded(&self) -> bool {
        self.degraded
    }

    /// `true` when a [`Staleness::Fresh`] verdict rests on a modification scan
    /// that hit the per-check `stat` cap — see the field-level note.
    ///
    /// [`Fresh`]: Staleness::Fresh
    #[must_use]
    pub fn scan_truncated(&self) -> bool {
        self.scan_truncated
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

    let (last_analyzed_at, indexed_at_commit) = latest_completed_run(conn, &mut degraded);
    let mut scan_truncated = false;
    let mut staleness = compute_staleness(
        conn,
        project_root,
        last_analyzed_at.as_deref(),
        &mut degraded,
        &mut scan_truncated,
    );

    // Worktree-source detection (clarion-26c7e52027, ADR-045): the mtime/structural
    // passes cannot see un-indexed source in a brand-new top-level directory (the
    // unwatched-project-root blind spot), so a hardened, ignore-aware
    // `git ls-files --others` scoped to ingested extensions catches it. Best-effort
    // and never degrades — `None` outside a git work tree. When the index is
    // otherwise Fresh but such source exists, the honest verdict is StaleWorktree.
    let worktree_dirty = compute_worktree_dirty(conn, project_root);
    if staleness == Staleness::Fresh && worktree_dirty == Some(true) {
        staleness = Staleness::StaleWorktree;
    }

    ProjectSnapshot {
        db_present: true,
        entity_count,
        subsystem_count,
        finding_count,
        staleness,
        last_analyzed_at,
        indexed_at_commit,
        worktree_dirty,
        degraded,
        scan_truncated,
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
        indexed_at_commit: None,
        worktree_dirty: None,
        degraded: false,
        scan_truncated: false,
    }
}

/// A degraded snapshot for a database that *is* present but could not be read
/// or serialized (the MCP `loomweave://context` reader-pool / serialize-error
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
        indexed_at_commit: None,
        worktree_dirty: None,
        degraded: true,
        scan_truncated: false,
    }
}

/// Whether the working tree holds untracked source of an already-indexed file
/// type — the [`ProjectSnapshot::worktree_dirty`] signal (clarion-26c7e52027,
/// ADR-045). Fail-soft: `None` when nothing is ingested (no extensions to scope
/// against, so the check is moot), the project is not a git work tree, or git is
/// unavailable. Never sets `degraded` — a missing git binary is environmental,
/// not a DB-machinery failure.
///
/// Scoping to ingested extensions is what keeps this honest: a hardened
/// `git ls-files --others --exclude-standard` lists every untracked, non-ignored
/// path, but only those whose extension Loomweave actually ingests count — so an
/// untracked `notes.txt` never flags a fresh index dirty, while an untracked
/// `hub.py` (the dogfood scenario) does.
fn compute_worktree_dirty(conn: &Connection, project_root: &Path) -> Option<bool> {
    let exts = ingested_source_extensions(conn);
    if exts.is_empty() {
        return None;
    }
    let untracked = loomweave_core::list_untracked_files(project_root)?;
    Some(untracked.iter().any(|rel| {
        Path::new(rel)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| exts.contains(ext))
    }))
}

/// The distinct file extensions among ingested `source_file_path`s (lowercased by
/// nothing — git and the filesystem are case-sensitive on the platforms we
/// target). Fail-soft to an empty set on any query error, which makes
/// [`compute_worktree_dirty`] return `None` (treat the scope as unknown).
fn ingested_source_extensions(conn: &Connection) -> BTreeSet<String> {
    let mut exts = BTreeSet::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT source_file_path FROM entities \
         WHERE source_file_path IS NOT NULL",
    ) else {
        return exts;
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
        return exts;
    };
    for rel in rows.flatten() {
        if let Some(ext) = Path::new(&rel).extension().and_then(|ext| ext.to_str()) {
            exts.insert(ext.to_owned());
        }
    }
    exts
}

/// Run a scalar `COUNT(*)` query. On failure, log, fold to `0`, and set
/// `*degraded` so the caller can mark the whole snapshot as a degraded read.
fn scalar_count(conn: &Connection, sql: &str, degraded: &mut bool) -> i64 {
    match conn.query_row(sql, [], |row| row.get::<_, i64>(0)) {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(error = %err, sql, "loomweave snapshot count query failed; reporting 0");
            *degraded = true;
            0
        }
    }
}

/// Look up the latest completed run's `completed_at` and `analyzed_at_commit`.
/// `QueryReturnedNoRows` is a normal "never analyzed" outcome and does *not*
/// degrade; any other error is a machinery failure that folds to `(None, None)`
/// and sets `*degraded`. `analyzed_at_commit` is independently nullable (a run
/// analyzed outside a git work tree), so it is `None` even on the happy path when
/// the column was never populated.
fn latest_completed_run(
    conn: &Connection,
    degraded: &mut bool,
) -> (Option<String>, Option<String>) {
    match conn.query_row(
        "SELECT completed_at, analyzed_at_commit FROM runs \
         WHERE completed_at IS NOT NULL AND status = 'completed' \
         ORDER BY completed_at DESC LIMIT 1",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    ) {
        Ok((completed_at, analyzed_at_commit)) => (Some(completed_at), analyzed_at_commit),
        Err(rusqlite::Error::QueryReturnedNoRows) => (None, None),
        Err(err) => {
            tracing::warn!(error = %err, "loomweave latest-completed-run query failed");
            *degraded = true;
            (None, None)
        }
    }
}

/// Upper bound on per-file `stat` syscalls in one staleness check — a backstop
/// against pathological repositories. In-place modification detection
/// inherently needs one `stat` per ingested file, and `loomweave hook
/// session-start` runs at the top of every agent session, so an unbounded scan
/// is O(files) syscalls per session start (clarion-93465ff89e). Structural
/// drift (added / removed / renamed files) is detected first and *exhaustively*
/// from directory mtimes — O(dirs) ≪ O(files) — which also short-circuits the
/// common "repo changed since analyze" case before any file is stat-ed. Only a
/// genuinely-fresh repo falls through to the bounded per-file scan; if it
/// exceeds this cap the overflow is logged and pure in-place edits to files
/// past the cap may report [`Staleness::Fresh`] until the next analyze. Sized
/// well above realistic targets (the elspeth corpus, ~425k LOC, is a few
/// thousand files) so no real project is sampled — the cap only bites a
/// pathological monorepo.
const MAX_MODIFICATION_STAT_FILES: usize = 20_000;

fn compute_staleness(
    conn: &Connection,
    project_root: &Path,
    last_analyzed_at: Option<&str>,
    degraded: &mut bool,
    scan_truncated: &mut bool,
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

    let Some((files, dirs)) = ingested_files_and_dirs(conn, project_root, degraded) else {
        // A query/prepare failure already set `degraded` and folds to Unknown.
        return Staleness::Unknown;
    };

    // (1) Structural drift: any watched source directory newer than the run
    // means a file was added / removed / renamed in it. Exhaustive over dirs
    // and far cheaper than the per-file scan, so it runs first.
    if directory_structural_drift(&dirs, run_time) {
        return Staleness::Stale;
    }

    // (2) In-place modification: one stat per ingested file, bounded.
    match file_modification_drift(&files, run_time, scan_truncated) {
        Some(staleness) => staleness,
        None => {
            if files.is_empty() {
                // A completed run with no resolvable source file to stat:
                // nothing to compare, NOT an error. Kept distinct from the
                // error folds so `Unknown` means strictly "a query/parse/stat
                // failed" (clarion-22add08e98).
                Staleness::NoSourcePaths
            } else {
                Staleness::Fresh
            }
        }
    }
}

/// Resolve every distinct ingested `source_file_path` to an absolute path, and
/// collect the distinct parent directories to watch for structural drift. The
/// project root itself is deliberately excluded from the watch set: `analyze`
/// writes `.loomweave/loomweave.db` under it, so the root's mtime is always newer
/// than the run and would wedge every check to a permanent false [`Stale`]
/// (the footgun the type-level note records). Returns `None` only on a
/// query/prepare failure, having set `*degraded`.
///
/// [`Stale`]: Staleness::Stale
fn ingested_files_and_dirs(
    conn: &Connection,
    project_root: &Path,
    degraded: &mut bool,
) -> Option<(Vec<PathBuf>, BTreeSet<PathBuf>)> {
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT source_file_path FROM entities \
         WHERE source_file_path IS NOT NULL",
    ) {
        Ok(stmt) => stmt,
        Err(err) => {
            tracing::warn!(error = %err, "loomweave staleness source-path query failed");
            *degraded = true;
            return None;
        }
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
        *degraded = true;
        return None;
    };

    let mut files = Vec::new();
    let mut dirs = BTreeSet::new();
    for rel in rows.flatten() {
        let abs = if Path::new(&rel).is_absolute() {
            PathBuf::from(&rel)
        } else {
            project_root.join(&rel)
        };
        if let Some(parent) = abs.parent()
            && parent != project_root
        {
            dirs.insert(parent.to_path_buf());
        }
        files.push(abs);
    }
    Some((files, dirs))
}

/// `true` if any watched directory's mtime is newer than the run, or a watched
/// directory is gone (a removed package) — both are structural drift. Other
/// dir-stat errors are environmental and skipped (best-effort, never degrade).
fn directory_structural_drift(dirs: &BTreeSet<PathBuf>, run_time: SystemTime) -> bool {
    dirs.iter()
        .any(|dir| match dir.metadata().and_then(|m| m.modified()) {
            Ok(mtime) => mtime > run_time,
            Err(err) => err.kind() == ErrorKind::NotFound,
        })
}

/// Scan up to [`MAX_MODIFICATION_STAT_FILES`] ingested files for in-place
/// edits. Returns `Some(Stale)` on the first file newer-than or deleted-since
/// the run, `Some(Unknown)` on a non-`NotFound` stat error (environmental, not
/// degraded), or `None` when every stat-ed file is older than the run (the
/// caller decides Fresh vs. `NoSourcePaths`). A deleted ingested file
/// (`NotFound`) is staleness, not an error (clarion-e687941a8c) — the
/// structural pass usually catches it via the parent directory first, but a
/// top-level deletion (parent is the unwatched project root) lands here.
fn file_modification_drift(
    files: &[PathBuf],
    run_time: SystemTime,
    scan_truncated: &mut bool,
) -> Option<Staleness> {
    for abs in files.iter().take(MAX_MODIFICATION_STAT_FILES) {
        match abs.metadata().and_then(|m| m.modified()) {
            Ok(mtime) if mtime > run_time => return Some(Staleness::Stale),
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => return Some(Staleness::Stale),
            Err(_) => return Some(Staleness::Unknown),
        }
    }
    // Reached only when no drift was found in the scanned prefix. If the index
    // has more files than the cap, the resulting `Fresh` verdict is bounded:
    // record it on the snapshot (not just the log) so a consumer can tell a
    // proven-fresh index from a fresh-as-far-as-scanned one (clarion-e687941a8c).
    if files.len() > MAX_MODIFICATION_STAT_FILES {
        *scan_truncated = true;
        tracing::warn!(
            ingested_files = files.len(),
            cap = MAX_MODIFICATION_STAT_FILES,
            "loomweave staleness: ingested-file count exceeds the modification-scan cap; \
             in-place edits beyond the cap may go unnoticed until the next analyze"
        );
    }
    None
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

    use loomweave_storage::{pragma, schema};

    use std::time::Duration;

    use super::{
        MAX_MODIFICATION_STAT_FILES, Staleness, file_modification_drift, project_snapshot,
    };

    // `apply_write_pragmas` enforces ADR-011's WAL journal-mode invariant, which
    // an in-memory connection cannot satisfy (`journal_mode=memory`). Back the
    // test db with a file in a `TempDir`, matching the canonical pattern in
    // `loomweave-storage`'s own integration tests. The `TempDir` is returned so the
    // caller keeps it alive for the connection's lifetime.
    fn migrated_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loomweave.db");
        let mut conn = Connection::open(path).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
        (dir, conn)
    }

    /// Set a file's or directory's mtime deterministically (no flaky sleeps).
    /// Opening a directory read-only and calling `set_modified` issues
    /// `futimens` on the dir fd, which is permitted on Linux.
    fn set_mtime(path: &std::path::Path, when: std::time::SystemTime) {
        std::fs::File::options()
            .read(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
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
             VALUES ('f1','loomweave','1.0','run1','R1','defect','WARN','python:module:a', \
                     '[]','m','{}','{}','[]','[]','open','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, std::path::Path::new("/nonexistent-root"));
        assert!(snap.db_present);
        assert_eq!(snap.entity_count, 3);
        assert_eq!(snap.subsystem_count, 1);
        assert_eq!(snap.finding_count, 1);
        // The ingested `a.py` does not exist under /nonexistent-root and sits at
        // the (unwatched) project root, so the per-file scan stats it, gets
        // NotFound, and reports the file as deleted-since-analyze → Stale
        // (clarion-e687941a8c).
        assert_eq!(snap.staleness, Staleness::Stale);
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

    #[test]
    fn fresh_within_cap_is_not_scan_truncated() {
        let (_dir, conn) = migrated_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.py");
        std::fs::write(&src, "x = 1\n").unwrap();
        set_mtime(&src, std::time::UNIX_EPOCH + Duration::from_secs(1_000_000));

        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        // Run far after the file mtime → Fresh, and only one file → exhaustive.
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, dir.path());
        assert_eq!(snap.staleness, Staleness::Fresh, "{snap:?}");
        assert!(
            !snap.scan_truncated,
            "a within-cap scan is exhaustive, not truncated: {snap:?}"
        );
    }

    #[test]
    fn scan_truncated_set_when_file_count_exceeds_cap_without_drift() {
        // Drive the bounded modification scan directly: one real, old file
        // repeated past the cap. The surplus entries are never stat-ed (the
        // scan stops at the cap), but their presence means the resulting
        // no-drift verdict is bounded — exactly the false-negative risk the
        // flag warns about (clarion-e687941a8c). Repeating one path keeps the
        // test cheap instead of materialising 20k files.
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.py");
        std::fs::write(&old, "x = 1\n").unwrap();
        // A run a year in the future: the file is unambiguously older, so every
        // scanned stat is Fresh and the loop runs to the cap.
        let run_time = std::time::SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 365);
        let files = vec![old; MAX_MODIFICATION_STAT_FILES + 1];

        let mut scan_truncated = false;
        let verdict = file_modification_drift(&files, run_time, &mut scan_truncated);
        assert_eq!(verdict, None, "no drift among the scanned prefix");
        assert!(
            scan_truncated,
            "exceeding the per-check stat cap must set scan_truncated"
        );
    }

    // `compute_staleness` folds: (a) an unparseable run timestamp → Unknown +
    // degraded; (b) a completed run with no resolvable source path →
    // NoSourcePaths (never degraded, clarion-22add08e98); (c) a *non*-NotFound
    // stat error → Unknown (environmental, never degraded). A deleted ingested
    // file (NotFound) is no longer (c) — it now reports Stale
    // (clarion-e687941a8c). `non_notfound_stat_error_folds_to_unknown_not_stale`
    // covers (c); the tests below lock (a) and (b).

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
        // "snake_case"); pin it so the loomweave://context / project_status
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
    fn deleted_top_level_source_file_reports_stale() {
        // A recorded source file that no longer exists on disk was deleted since
        // the last analyze: that is staleness, not Unknown (clarion-e687941a8c).
        // `gone.py` sits at the (unwatched) project root, so the structural pass
        // can't see it and the per-file scan's NotFound drives the verdict. A
        // deletion is environmental, so `degraded` stays false.
        let (_dir, conn) = migrated_conn();
        insert_entity(&conn, "python:module:a", "module", Some("gone.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2026-01-01T00:00:00.000Z', '2026-01-02T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        let snap = project_snapshot(&conn, std::path::Path::new("/nonexistent-root"));
        assert_eq!(snap.staleness, Staleness::Stale, "{snap:?}");
        assert!(
            !snap.degraded,
            "a deleted source file is environmental, not degraded: {snap:?}"
        );
    }

    #[test]
    fn added_file_in_watched_dir_reports_stale_via_directory_mtime() {
        // A brand-new file the last analyze never ingested is invisible to the
        // per-file scan (it is absent from `entities`), but adding it bumped its
        // parent directory's mtime — which the structural pass catches. Pin the
        // ingested file OLDER than the run and the directory NEWER, so ONLY the
        // structural pass can produce Stale: if detection regressed to files
        // alone this would wrongly report Fresh (clarion-e687941a8c).
        use super::parse_iso8601_to_systemtime;
        let (_dir, conn) = migrated_conn();
        let root = tempfile::tempdir().unwrap();
        let pkg = root.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        let a = pkg.join("a.py");
        std::fs::write(&a, "x = 1\n").unwrap();

        let run_iso = "2026-06-15T00:00:00.000Z";
        let run_time = parse_iso8601_to_systemtime(run_iso).unwrap();
        let day = std::time::Duration::from_secs(86_400);
        set_mtime(&a, run_time - day); // ingested file untouched since the run
        set_mtime(&pkg, run_time + day); // a sibling file was added after the run

        insert_entity(&conn, "python:module:pkg.a", "module", Some("pkg/a.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', ?1, ?1, '{}', '{}', 'completed')",
            rusqlite::params![run_iso],
        )
        .unwrap();

        let snap = project_snapshot(&conn, root.path());
        assert_eq!(snap.staleness, Staleness::Stale, "{snap:?}");
        assert!(
            !snap.degraded,
            "structural drift is environmental, not degraded: {snap:?}"
        );
    }

    #[test]
    fn non_notfound_stat_error_folds_to_unknown_not_stale() {
        // A stat failure that is NOT "file missing" — here ENOTDIR, a path whose
        // parent component is a regular file — is environmental machinery we
        // cannot read: it folds to Unknown, never Stale, and never sets
        // `degraded`. This is the fold a deleted file (NotFound -> Stale) is now
        // distinguished from.
        let (_dir, conn) = migrated_conn();
        let root = tempfile::tempdir().unwrap();
        // A regular file where a directory is expected: stat("blocker/child.py")
        // returns ENOTDIR, not NotFound.
        std::fs::write(root.path().join("blocker"), "not a dir\n").unwrap();
        insert_entity(&conn, "python:module:x", "module", Some("blocker/child.py"));
        // Run far in the future so the structural pass (which stats the parent
        // "blocker", a real file with a ~now mtime) finds nothing newer and falls
        // through to the per-file scan where the ENOTDIR fold happens.
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        let snap = project_snapshot(&conn, root.path());
        assert_eq!(snap.staleness, Staleness::Unknown, "{snap:?}");
        assert!(
            !snap.degraded,
            "an environmental stat error is not degraded: {snap:?}"
        );
    }

    #[test]
    fn degraded_field_serializes() {
        let (_dir, conn) = migrated_conn();
        let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["degraded"], serde_json::Value::Bool(false));
    }

    #[test]
    fn indexed_at_commit_is_surfaced_and_serialized_when_the_run_recorded_one() {
        // `project_status_get` already reports the analyzed commit as `git_sha`;
        // the snapshot (loomweave://context + the session-start banner) must carry
        // the same value so a Fresh verdict can name the commit it reflects
        // (clarion-26c7e52027).
        let (_dir, conn) = migrated_conn();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status, analyzed_at_commit) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed', 'abc123def456')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, dir.path());
        assert_eq!(snap.indexed_at_commit(), Some("abc123def456"), "{snap:?}");
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(
            json["indexed_at_commit"],
            serde_json::Value::String("abc123def456".into())
        );
    }

    #[test]
    fn indexed_at_commit_is_none_when_run_analyzed_outside_a_git_repo() {
        // `analyzed_at_commit` is independently nullable: a run outside a git work
        // tree records NULL, and the snapshot must report None — never a fabricated
        // or empty commit.
        let (_dir, conn) = migrated_conn();
        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2026-01-01T00:00:00.000Z', '2026-01-02T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
        assert_eq!(snap.indexed_at_commit(), None, "{snap:?}");
    }

    #[test]
    fn new_top_level_directory_is_a_known_fresh_blind_spot() {
        // Documents (and regression-locks) the conservative-nudge limitation the
        // honest banner now discloses (clarion-26c7e52027). The watch set is the
        // *direct parents of ingested files* and the project root is deliberately
        // unwatched, so a brand-new top-level directory full of never-ingested
        // source is invisible to BOTH the structural-drift and per-file passes —
        // the verdict stays Fresh. This is the exact dogfood scenario (new
        // specimen modules read as "fresh"). Detecting it would need working-tree
        // git, which the untrusted-corpus posture (hardened_git) blocks; until
        // then the banner tells the agent to re-analyze after adding modules.
        use super::parse_iso8601_to_systemtime;
        let (_dir, conn) = migrated_conn();
        let root = tempfile::tempdir().unwrap();
        let pkg = root.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        let a = pkg.join("a.py");
        std::fs::write(&a, "x = 1\n").unwrap();

        let run_iso = "2026-06-15T00:00:00.000Z";
        let run_time = parse_iso8601_to_systemtime(run_iso).unwrap();
        let day = std::time::Duration::from_secs(86_400);
        set_mtime(&a, run_time - day); // ingested file untouched since the run
        set_mtime(&pkg, run_time - day); // its watched parent untouched too

        insert_entity(&conn, "python:module:pkg.a", "module", Some("pkg/a.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', ?1, ?1, '{}', '{}', 'completed')",
            rusqlite::params![run_iso],
        )
        .unwrap();

        // Add a brand-new top-level package AFTER the run. Its parent is the
        // (unwatched) project root, so nothing in the watch set changed.
        let newpkg = root.path().join("newpkg");
        std::fs::create_dir(&newpkg).unwrap();
        let hub = newpkg.join("hub.py");
        std::fs::write(&hub, "y = 2\n").unwrap();
        set_mtime(&hub, run_time + day);

        let snap = project_snapshot(&conn, root.path());
        // The mtime/structural passes can't see the new top-level dir, AND this
        // tempdir is not a git work tree, so the worktree-source check returns
        // None (nothing to detect with). Verdict stays Fresh — the mtime blind
        // spot the banner caveat covers. In a GIT repo this same scenario flips to
        // StaleWorktree (see `untracked_source_in_git_repo_reports_stale_worktree`).
        assert_eq!(snap.staleness, Staleness::Fresh, "{snap:?}");
        assert_eq!(
            snap.worktree_dirty, None,
            "outside a git work tree, worktree_dirty must be None: {snap:?}"
        );
    }

    /// `git init` + commit `files` in `root`; returns `false` (caller skips) if
    /// git is unavailable on the host. Committing keeps the seeded source OUT of
    /// the untracked set so a clean baseline really is clean.
    fn git_init_with_committed(root: &std::path::Path, files: &[(&str, &str)]) -> bool {
        use std::process::Command;
        let run = |args: &[&str]| -> bool {
            Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .is_ok_and(|s| s.success())
        };
        if !run(&["init", "-q"]) {
            return false;
        }
        let _ = run(&["config", "user.email", "t@t"]);
        let _ = run(&["config", "user.name", "t"]);
        for (name, body) in files {
            std::fs::write(root.join(name), body).unwrap();
        }
        if !files.is_empty() {
            run(&["add", "."]);
            run(&["commit", "-q", "-m", "init"]);
        }
        true
    }

    #[test]
    fn untracked_source_in_git_repo_reports_stale_worktree() {
        // The dogfood scenario (clarion-26c7e52027, ADR-045): an index that is
        // mtime-fresh but with a brand-new untracked source module the structural
        // pass cannot reach. In a git work tree the hardened `ls-files --others`
        // check (scoped to ingested `.py`) catches it and the verdict is honest:
        // StaleWorktree, with worktree_dirty = Some(true).
        let (_dir, conn) = migrated_conn();
        let root = tempfile::tempdir().unwrap();
        if !git_init_with_committed(root.path(), &[("demo.py", "x = 1\n")]) {
            return; // git unavailable on host → skip (mechanism covered in core)
        }
        insert_entity(&conn, "python:module:demo", "module", Some("demo.py"));
        // Far-future run → every ingested file is mtime-fresh.
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        // Brand-new untracked source the index never saw.
        std::fs::write(root.path().join("hub.py"), "y = 2\n").unwrap();

        let snap = project_snapshot(&conn, root.path());
        assert_eq!(snap.staleness, Staleness::StaleWorktree, "{snap:?}");
        assert_eq!(snap.worktree_dirty, Some(true), "{snap:?}");
    }

    #[test]
    fn untracked_non_source_in_git_repo_stays_fresh() {
        // False-positive guard: an untracked file whose extension Loomweave does
        // not ingest (a scratch notes.txt) must NOT flag the index dirty. The
        // extension scoping is what keeps the signal honest.
        let (_dir, conn) = migrated_conn();
        let root = tempfile::tempdir().unwrap();
        if !git_init_with_committed(root.path(), &[("demo.py", "x = 1\n")]) {
            return;
        }
        insert_entity(&conn, "python:module:demo", "module", Some("demo.py"));
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        // Untracked, but NOT a source extension the index uses.
        std::fs::write(root.path().join("notes.txt"), "scratch\n").unwrap();

        let snap = project_snapshot(&conn, root.path());
        assert_eq!(
            snap.staleness,
            Staleness::Fresh,
            "an untracked non-source file must not flip the verdict: {snap:?}"
        );
        assert_eq!(snap.worktree_dirty, Some(false), "{snap:?}");
    }
}
