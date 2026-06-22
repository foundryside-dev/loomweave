//! Index-integrity diagnosis and surgical repair (clarion-abda98c869 recovery
//! surface for `loomweave doctor --fix`).
//!
//! Two related corruptions are detected; the common, recoverable one is repaired:
//!
//! * **Stale file entities** — a `core:file:*` entity whose source path no longer
//!   exists on disk (the file was deleted, renamed, or converted file↔package).
//!   `entities` is cumulative and never run-pruned, so such rows linger until an
//!   analyze run's SEI orphan pass retires them; until then their dangling
//!   `contains` edges can violate the parent/contains invariant.
//! * **Parent/contains mismatches** — the `LMWV-INFRA-PARENT-CONTAINS-MISMATCH`
//!   invariant (ADR-026 decision 2) the writer enforces at `CommitRun`. A
//!   file→package refactor (`m.py` → `m/__init__.py`, *same* module qualname)
//!   leaves a stale file entity whose `contains` edge competes with the new file's,
//!   and the run aborts at phase3 before the orphan pass can clean up.
//!
//! Repair removes each stale file entity and every entity anchored to it
//! (`source_file_id`); their edges and tags cascade away (`ON DELETE CASCADE`,
//! `foreign_keys = ON`). This is the same retirement the analyze SEI pass
//! performs, applied proactively so a corrupted index becomes analysable again
//! without a full rebuild. The delete runs under `defer_foreign_keys = ON` so the
//! removal set need not be ordered; the deferred check at commit guarantees no
//! surviving row is left dangling. Mismatches not attributable to a stale file
//! (genuine writer corruption) are surfaced as *residual* for a full re-analyze.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// A `core:file:*` entity whose source path no longer exists on disk.
#[derive(Debug, Clone)]
pub struct StaleFileEntity {
    pub id: String,
    /// Best-effort display path (relative to the project root).
    pub path: String,
}

/// One violation of the parent/contains dual-encoding invariant.
#[derive(Debug, Clone)]
pub struct ParentContainsMismatch {
    pub detail: String,
}

/// The read-only integrity verdict for an index.
#[derive(Debug, Default)]
pub struct IntegrityReport {
    pub stale_file_entities: Vec<StaleFileEntity>,
    pub parent_contains_mismatches: Vec<ParentContainsMismatch>,
}

impl IntegrityReport {
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.stale_file_entities.is_empty() && self.parent_contains_mismatches.is_empty()
    }
}

/// Outcome of a [`repair_integrity`] pass.
#[derive(Debug)]
pub struct RepairReport {
    /// Number of stale (vanished-from-disk) file entities removed.
    pub removed_file_entities: usize,
    /// Total entities removed (the stale files plus everything anchored to them).
    pub removed_entities_total: usize,
    /// Integrity re-check after the repair. A non-healthy residual means
    /// corruption that surgical orphan-removal cannot fix — a full re-analyze
    /// (`loomweave analyze --no-incremental`) is required.
    pub residual: IntegrityReport,
}

/// Read-only integrity check. `project_root` resolves `core:file:*` entity paths
/// to decide whether a file still exists on disk.
///
/// # Errors
///
/// Returns [`crate::error::StorageError::Sqlite`] on any query failure.
pub fn check_integrity(conn: &Connection, project_root: &Path) -> Result<IntegrityReport> {
    Ok(IntegrityReport {
        stale_file_entities: stale_file_entities(conn, project_root)?,
        parent_contains_mismatches: parent_contains_mismatches(conn)?,
    })
}

/// Surgically remove stale file entities (and everything anchored to them), then
/// re-check. Idempotent: a healthy index is left untouched.
///
/// # Errors
///
/// Returns [`crate::error::StorageError::Sqlite`] on any query/transaction
/// failure, including a deferred foreign-key violation at commit (which rolls the
/// repair back, leaving the index unchanged).
pub fn repair_integrity(conn: &mut Connection, project_root: &Path) -> Result<RepairReport> {
    let stale = stale_file_entities(conn, project_root)?;
    let removed_file_entities = stale.len();
    let mut removed_entities_total = 0usize;

    if !stale.is_empty() {
        let tx = conn.transaction()?;
        // Defer FK checks to commit so the removal set need not be topologically
        // ordered; cascade still fires for edges/tags as each entity is deleted.
        tx.execute_batch("PRAGMA defer_foreign_keys = ON;")?;

        // Stage the removal set (stale file entities + everything anchored to
        // them via `source_file_id`) so the null-out and delete share one set.
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS __lw_to_delete (id TEXT PRIMARY KEY); \
             DELETE FROM __lw_to_delete;",
        )?;
        let mut to_delete: BTreeSet<String> = BTreeSet::new();
        {
            let mut child_stmt = tx.prepare("SELECT id FROM entities WHERE source_file_id = ?1")?;
            for file in &stale {
                to_delete.insert(file.id.clone());
                let kids = child_stmt.query_map([&file.id], |row| row.get::<_, String>(0))?;
                for kid in kids {
                    to_delete.insert(kid?);
                }
            }
            let mut ins = tx.prepare("INSERT OR IGNORE INTO __lw_to_delete (id) VALUES (?1)")?;
            for id in &to_delete {
                ins.execute([id])?;
            }
        }

        // Null the four NO-ACTION foreign-key columns into `entities(id)` that do
        // NOT cascade, where a SURVIVING row would otherwise be left pointing at a
        // deleted entity (`edges.from_id`/`to_id`, tags, taint, caches all cascade
        // and need no handling). `source_file_id` always names a file entity, so
        // nulling it drops only stale provenance; a dangling `parent_id` on a
        // survivor is corruption itself (a moved child whose old parent vanished)
        // and is cleared so a re-analyze can re-establish it.
        tx.execute_batch(
            "UPDATE entities SET parent_id = NULL \
               WHERE parent_id IN (SELECT id FROM __lw_to_delete) \
                 AND id NOT IN (SELECT id FROM __lw_to_delete); \
             UPDATE entities SET source_file_id = NULL \
               WHERE source_file_id IN (SELECT id FROM __lw_to_delete) \
                 AND id NOT IN (SELECT id FROM __lw_to_delete); \
             UPDATE edges SET source_file_id = NULL \
               WHERE source_file_id IN (SELECT id FROM __lw_to_delete); \
             UPDATE entity_unresolved_call_sites SET source_file_id = NULL \
               WHERE source_file_id IN (SELECT id FROM __lw_to_delete);",
        )?;

        removed_entities_total = tx.execute(
            "DELETE FROM entities WHERE id IN (SELECT id FROM __lw_to_delete)",
            [],
        )?;
        tx.execute_batch("DROP TABLE __lw_to_delete;")?;
        tx.commit()?;
    }

    Ok(RepairReport {
        removed_file_entities,
        removed_entities_total,
        residual: check_integrity(conn, project_root)?,
    })
}

/// File entities whose source path is gone from disk. The canonical path is the
/// `core:file:<relpath>` id (ADR-003); `source_file_path` is a fallback.
fn stale_file_entities(conn: &Connection, project_root: &Path) -> Result<Vec<StaleFileEntity>> {
    let mut stmt = conn.prepare("SELECT id, source_file_path FROM entities WHERE kind = 'file'")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut stale = Vec::new();
    for row in rows {
        let (id, source_path) = row?;
        if !file_entity_exists(&id, source_path.as_deref(), project_root) {
            let path = source_path
                .or_else(|| id.strip_prefix("core:file:").map(ToOwned::to_owned))
                .unwrap_or_else(|| id.clone());
            stale.push(StaleFileEntity { id, path });
        }
    }
    Ok(stale)
}

/// Does the file backing a `core:file:*` entity still exist on disk?
fn file_entity_exists(id: &str, source_path: Option<&str>, project_root: &Path) -> bool {
    if let Some(rel) = id.strip_prefix("core:file:")
        && project_root.join(rel).exists()
    {
        return true;
    }
    if let Some(path) = source_path {
        let raw = Path::new(path);
        if raw.is_absolute() && raw.exists() {
            return true;
        }
        if project_root.join(path).exists() {
            return true;
        }
    }
    false
}

/// Both directions of the parent/contains dual-encoding invariant (mirrors the
/// writer's `CommitRun` check, ADR-026 decision 2), collecting *all* violations.
fn parent_contains_mismatches(conn: &Connection) -> Result<Vec<ParentContainsMismatch>> {
    let mut out = Vec::new();

    // Direction 1: every entity.parent_id has a matching `contains` edge from it.
    let mut s1 = conn.prepare(
        "SELECT e.id, e.parent_id, ce.from_id \
         FROM entities e \
         LEFT JOIN edges ce ON ce.kind = 'contains' AND ce.to_id = e.id \
         WHERE e.parent_id IS NOT NULL \
           AND (ce.from_id IS NULL OR ce.from_id != e.parent_id)",
    )?;
    let r1 = s1.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in r1 {
        let (eid, parent, ce_from) = row?;
        out.push(ParentContainsMismatch {
            detail: format!(
                "entity {eid:?} declares parent_id={parent:?} but no matching `contains` \
                 edge exists (closest contains.from_id={ce_from:?})"
            ),
        });
    }

    // Direction 2: every `contains` edge has a child whose parent_id matches.
    let mut s2 = conn.prepare(
        "SELECT ce.from_id, ce.to_id, e.parent_id \
         FROM edges ce \
         JOIN entities e ON e.id = ce.to_id \
         WHERE ce.kind = 'contains' \
           AND (e.parent_id IS NULL OR e.parent_id != ce.from_id)",
    )?;
    let r2 = s2.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in r2 {
        let (from, to, parent) = row?;
        out.push(ParentContainsMismatch {
            detail: format!(
                "contains edge ({from:?} -> {to:?}) has no matching child parent_id \
                 (child.parent_id={parent:?})"
            ),
        });
    }

    Ok(out)
}
