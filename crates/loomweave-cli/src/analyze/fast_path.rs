//! The no-indexed-changes fast path (clarion-78d75e45c9).
//!
//! Every commit moves the freshness oracle's commit clock, so a docs-only or
//! CI-only commit makes the index read `stale` and — through the git-sync and
//! `SessionStart` hooks — costs a full incremental run: the secret-scan walk,
//! plugin dispatch, the clustering pass, the SEI mint. On a shared checkout
//! that was 12–148 runs a day, most of them over a tree that had not
//! structurally changed. When the ONLY drift signal is the commit clock and
//! the committed diff since the analyzed commit touches nothing the index
//! ingests, the index at HEAD *is* the index at the analyzed commit; the run
//! records that (a completed run row at HEAD carrying the base run's stats,
//! tagged `fast_path`) and returns in about a second.
//!
//! Conservative by construction: any in-place modification, staged indexed
//! change, untracked source, or observation blindness disables it (the oracle
//! returns `None`), as does an unknown commit range (`git diff` fails after a
//! rewrite that dropped the analyzed commit), and `--no-incremental` bypasses
//! it entirely. Beyond ingested extensions, the analyzer's own inputs count as
//! indexed scope: `loomweave.yaml`, the secrets baseline, and `.env` sidecars
//! (the pre-ingest scanner reads them).

use std::collections::BTreeSet;
use std::path::Path;

use loomweave_core::hardened_git_command;
use rusqlite::{Connection, OpenFlags};

/// Non-source inputs whose change must run the full pipeline.
const CONFIG_SENTINELS: [&str; 2] = ["loomweave.yaml", ".weft/loomweave/secrets-baseline.yaml"];

/// What the fast path established, for the run row and the completion line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FastPathEvidence {
    pub(crate) analyzed_commit: String,
    pub(crate) head_commit: String,
    /// Paths the committed range touched — none of them indexed scope.
    pub(crate) paths_changed: usize,
    /// The base run whose stats this run carries forward.
    pub(crate) base_run_id: String,
    pub(crate) base_stats: serde_json::Value,
}

/// `Some` when the run may be settled without a walk. See the module docs.
pub(crate) fn no_indexed_changes(project_root: &Path, db_path: &Path) -> Option<FastPathEvidence> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let drift = loomweave_mcp::commit_only_drift(&conn, project_root)?;
    let output = hardened_git_command(project_root)
        .args([
            "diff",
            "--name-only",
            "-z",
            &format!("{}..{}", drift.analyzed_commit, drift.head_commit),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let paths: Vec<&str> = stdout.split('\0').filter(|p| !p.is_empty()).collect();
    if paths_touch_indexed_scope(paths.iter().copied(), &drift.ingested_extensions) {
        return None;
    }
    let (base_run_id, base_stats) = latest_completed_run_stats(&conn)?;
    Some(FastPathEvidence {
        analyzed_commit: drift.analyzed_commit,
        head_commit: drift.head_commit,
        paths_changed: paths.len(),
        base_run_id,
        base_stats,
    })
}

/// Whether any committed path is something the pipeline ingests or reads:
/// an ingested extension (case-insensitive, matching the tree walk), a
/// config sentinel, or a `.env*` sidecar anywhere in the tree.
pub(crate) fn paths_touch_indexed_scope<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    ingested_extensions: &BTreeSet<String>,
) -> bool {
    let exts: BTreeSet<String> = ingested_extensions
        .iter()
        .map(|e| e.to_ascii_lowercase())
        .collect();
    paths.into_iter().any(|path| {
        if CONFIG_SENTINELS.contains(&path) {
            return true;
        }
        let file_name = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if file_name.starts_with(".env") {
            return true;
        }
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e.to_ascii_lowercase()))
    })
}

fn latest_completed_run_stats(conn: &Connection) -> Option<(String, serde_json::Value)> {
    let (id, stats): (String, String) = conn
        .query_row(
            "SELECT id, stats FROM runs \
             WHERE status = 'completed' AND completed_at IS NOT NULL \
             ORDER BY completed_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    let stats = serde_json::from_str(&stats).ok()?;
    Some((id, stats))
}

/// The stats a fast-path run records: the base run's stats carried forward
/// (the index content is that run's, so its coverage still describes it),
/// with the per-run insertion counters zeroed and a `fast_path` block naming
/// the base and the settled commit range.
pub(crate) fn fast_path_stats(evidence: &FastPathEvidence) -> serde_json::Value {
    let mut stats = evidence.base_stats.clone();
    if !stats.is_object() {
        stats = serde_json::json!({});
    }
    let object = stats.as_object_mut().expect("object ensured above");
    object.insert("entities_inserted".to_owned(), serde_json::json!(0));
    object.insert("edges_inserted".to_owned(), serde_json::json!(0));
    object.insert(
        "fast_path".to_owned(),
        serde_json::json!({
            "reason": "no_indexed_changes",
            "base_run_id": evidence.base_run_id,
            "from_commit": evidence.analyzed_commit,
            "to_commit": evidence.head_commit,
            "paths_changed": evidence.paths_changed,
        }),
    );
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exts(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|e| (*e).to_owned()).collect()
    }

    #[test]
    fn only_ingested_extensions_sentinels_and_env_sidecars_count_as_indexed_scope() {
        let py = exts(&["py"]);
        assert!(!paths_touch_indexed_scope(
            ["README.md", "docs/a.md", ".github/ci.yml"],
            &py
        ));
        assert!(paths_touch_indexed_scope(["README.md", "src/app.py"], &py));
        // Case-insensitive like the tree walk.
        assert!(paths_touch_indexed_scope(["src/APP.PY"], &py));
        assert!(paths_touch_indexed_scope(["loomweave.yaml"], &py));
        assert!(paths_touch_indexed_scope(
            [".weft/loomweave/secrets-baseline.yaml"],
            &py
        ));
        assert!(paths_touch_indexed_scope(["deploy/.env.production"], &py));
        // A `.env` mention inside a directory name is not a sidecar.
        assert!(!paths_touch_indexed_scope([".envs/notes.txt"], &py));
        // No ingested extensions: only sentinels can match.
        assert!(!paths_touch_indexed_scope(["src/app.py"], &exts(&[])));
        assert!(!paths_touch_indexed_scope(std::iter::empty(), &py));
    }

    #[test]
    fn fast_path_stats_carry_the_base_forward_and_zero_the_run_counters() {
        let evidence = FastPathEvidence {
            analyzed_commit: "aaa".to_owned(),
            head_commit: "bbb".to_owned(),
            paths_changed: 2,
            base_run_id: "run-1".to_owned(),
            base_stats: serde_json::json!({
                "entities_inserted": 42,
                "edges_inserted": 7,
                "classifier_coverage": {"schema": "x"},
            }),
        };
        let stats = fast_path_stats(&evidence);
        assert_eq!(stats["entities_inserted"], 0);
        assert_eq!(stats["edges_inserted"], 0);
        assert_eq!(stats["classifier_coverage"]["schema"], "x");
        assert_eq!(stats["fast_path"]["reason"], "no_indexed_changes");
        assert_eq!(stats["fast_path"]["base_run_id"], "run-1");
        assert_eq!(stats["fast_path"]["paths_changed"], 2);
    }
}
