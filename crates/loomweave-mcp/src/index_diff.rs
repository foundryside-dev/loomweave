//! `index_diff`: deterministic index freshness / drift report (clarion-326b01ffd0).
//!
//! Answers "what changed since the last analyze, and is this checkout newer
//! than the graph?" without an agent hand-rolling git + mtime checks.
//!
//! **This module is the project's ONE freshness oracle** (convention C-12,
//! weft-4165f1ed71). [`compute_freshness`] produces the authoritative verdict;
//! `index_diff_get` reports it in full, and every other surface that answers
//! the freshness question — `project_status_get`, the orientation pack, the
//! `loomweave://context` resource, and the `SessionStart` hook (all via
//! [`crate::snapshot::project_snapshot`]) — DERIVES its answer from this same
//! code path. Dogfood-4 B1 was two detectors disagreeing at the same instant:
//! the snapshot's former mtime/structural detector watched the *parents* of
//! ingested paths (which put `$HOME` in the watch set whenever a project-anchor
//! entity carried the project root as its source path) while this module said
//! fresh. There is no second detector anymore.
//!
//! **Git posture (per the issue's design fork).** Loomweave persists no
//! analyze-time commit SHA — `project_status` reports `git_sha: null` and the
//! analyze write path never captures HEAD. Rather than reverse that stance to
//! populate one side of a comparison, `index_diff` reads git *at query time*,
//! *read-only*, and *fail-soft*: a missing git binary or a non-repo working
//! directory degrades to `git.available=false` with a reason, never an error.
//! `analyzed_commit` is reported `null` (honest, matching `project_status`),
//! and the HEAD-vs-analyze staleness signal compares HEAD's committer date
//! against the run's completion time — which holds even when source mtimes are
//! ambiguous.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::SystemTime;

use loomweave_core::hardened_git_command;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use loomweave_storage::{StorageError, normalize_source_path};

/// Default per-list cap so a pathological repo cannot produce an unbounded
/// packet. Overridable via the `limit` argument.
pub(crate) const DEFAULT_MAX_ENTRIES: usize = 200;

/// Upper bound on per-file `stat` syscalls in one freshness check — a backstop
/// against pathological repositories. In-place modification detection
/// inherently needs one `stat` per ingested file, and the `SessionStart` hook
/// runs this at the top of every agent session (clarion-93465ff89e). Beyond
/// the cap the verdict is bounded rather than exhaustive — recorded on
/// [`FileDrift::stat_scan_truncated`] so a consumer can tell. Sized well above
/// realistic targets (the elspeth corpus, ~425k LOC, is a few thousand files).
const MAX_FILE_STAT_SCAN: usize = 20_000;

/// Git facts gathered at query time, outside the reader. Every field is
/// best-effort; see the module docstring for the fail-soft contract.
pub(crate) struct GitFacts {
    available: bool,
    is_repo: bool,
    head_commit: Option<String>,
    head_committed_at: Option<String>,
    dirty: Vec<DirtyEntry>,
    reason: Option<String>,
}

struct DirtyEntry {
    status: String,
    rel_path: String,
}

/// Run `git` read-only against `project_root` and collect HEAD + dirty-tree
/// facts. Blocking; call from a `spawn_blocking` context.
pub(crate) fn gather_git_facts(project_root: &Path) -> GitFacts {
    // Hardened against the untrusted corpus (clarion-4b5a8aff54): no
    // repo-controlled program runs while gathering git facts. `git status` below
    // is the most reliable fsmonitor/clean-filter trigger of all, so the
    // hardening is load-bearing here.
    let inside = hardened_git_command(project_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    let (available, is_repo, reason) = match inside {
        Ok(out) if out.status.success() => {
            let is_repo = String::from_utf8_lossy(&out.stdout).trim() == "true";
            let reason = (!is_repo).then(|| "not inside a git work tree".to_owned());
            (true, is_repo, reason)
        }
        Ok(_) => (true, false, Some("not a git repository".to_owned())),
        Err(err) => (false, false, Some(format!("git unavailable: {err}"))),
    };
    if !is_repo {
        return GitFacts {
            available,
            is_repo: false,
            head_commit: None,
            head_committed_at: None,
            dirty: Vec::new(),
            reason,
        };
    }

    let run = |args: &[&str]| -> Option<String> {
        let out = hardened_git_command(project_root)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    };

    let head_commit = run(&["rev-parse", "HEAD"]);
    // `%cI` is strict ISO-8601 (RFC3339) with the committer's UTC offset.
    let head_committed_at = run(&["log", "-1", "--format=%cI", "HEAD"]);
    // Dirty signal via `git diff --cached` (STAGED changes, index vs HEAD), NOT
    // `git status` (clarion-4b5a8aff54): `git status` must hash working-tree
    // content to report unstaged modifications, which executes a repo-controlled
    // `filter.<name>.clean` selected by `$GIT_DIR/info/attributes` — a source no
    // config flag can disable on an untrusted corpus. `--cached` compares only
    // stored objects (index + HEAD), so it never hashes the working tree. The
    // cost is honest and bounded: unstaged working-tree modifications and
    // untracked files are NOT enumerated here. Unstaged modifications to *indexed*
    // files are still caught by the stat-based `compute_file_drift`
    // (`modified_since_analyze`); only never-staged/never-indexed changes go
    // unreported, which the report notes already disclaim.
    let dirty = hardened_git_command(project_root)
        .args(["diff", "--cached", "--name-status", "-M", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| parse_name_status(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default();

    GitFacts {
        available,
        is_repo: true,
        head_commit,
        head_committed_at,
        dirty,
        reason: None,
    }
}

/// Parse `git diff --cached --name-status -M HEAD` output into per-path entries.
/// Each line is `<STATUS>\t<path>` (e.g. `M\tsrc/lib.rs`, `A\tnew.py`) or, for
/// renames/copies, `<STATUS>\t<old>\t<new>` — which collapse to the new path. The
/// status is reduced to its leading letter (`R096` → `R`). git's C-style quoting
/// of paths with special bytes is decoded (see [`unquote_c_path`]).
fn parse_name_status(out: &str) -> Vec<DirtyEntry> {
    out.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let raw = cols.next()?;
            let code = raw.chars().next()?;
            // Rename/copy lines carry two paths (old, new); report the new path.
            let path = if matches!(code, 'R' | 'C') {
                let _old = cols.next()?;
                cols.next()?
            } else {
                cols.next()?
            };
            if path.is_empty() {
                return None;
            }
            Some(DirtyEntry {
                status: code.to_string(),
                rel_path: unquote_c_path(path),
            })
        })
        .collect()
}

/// Decode git's C-style path quoting back to a real path. With the default
/// `core.quotePath=true`, git renders any path containing a control byte, a
/// backslash, a double-quote, or a non-ASCII byte as a double-quoted string
/// with C escapes: `\\`, `\"`, `\t`/`\n`/`\r`/`\a`/`\b`/`\f`/`\v`, and `\NNN`
/// octal *byte* escapes (e.g. `"\303\251.py"` for `é.py`). Octal escapes are
/// emitted one per UTF-8 byte, so they must be reassembled into bytes before
/// the result is decoded — `trim_matches('"')` alone would leave
/// `\303\251.py` literal and never correlate against an indexed path.
///
/// A path with no special bytes is emitted bare (no surrounding quotes) and is
/// returned unchanged. Best-effort and panic-free: an unrecognised escape keeps
/// its literal `\x`, a dangling trailing backslash is preserved, and invalid
/// UTF-8 decodes lossily.
fn unquote_c_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    // Bare (unquoted) paths pass through untouched.
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return raw.to_owned();
    }
    let inner = &bytes[1..bytes.len() - 1];
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] != b'\\' {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        // Consume the backslash; decode the escape that follows.
        i += 1;
        let Some(&escape) = inner.get(i) else {
            // Dangling trailing backslash — keep it verbatim.
            out.push(b'\\');
            break;
        };
        match escape {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            b'0'..=b'7' => {
                // Up to three octal digits → one byte (git emits \000..\377).
                let mut value: u8 = 0;
                let mut digits = 0;
                while digits < 3 && inner.get(i).is_some_and(|b| (b'0'..=b'7').contains(b)) {
                    value = value.wrapping_mul(8).wrapping_add(inner[i] - b'0');
                    i += 1;
                    digits += 1;
                }
                out.push(value);
                continue; // `i` already advanced past the octal run.
            }
            other => {
                // Unrecognised escape: preserve `\` + the char.
                out.push(b'\\');
                out.push(other);
            }
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Index state read from the DB inside the reader.
pub(crate) struct IndexState {
    pub analyzed_at: Option<String>,
    pub analyzed_commit: Option<String>,
    pub latest_run: Option<Value>,
    pub files: Vec<IndexedFile>,
    pub plugin_stats: Value,
}

pub(crate) struct IndexedFile {
    pub source_file_path: String,
    pub entity_count: i64,
}

/// Read the freshness-relevant index state: the latest *completed* run, the
/// distinct indexed source files with their entity counts, and the aggregate
/// plugin skip/drop counters from that run's stats.
pub(crate) fn read_index_state(conn: &rusqlite::Connection) -> Result<IndexState, StorageError> {
    let latest = conn
        .query_row(
            "SELECT id, started_at, completed_at, status, stats, analyzed_at_commit FROM runs \
             WHERE status = 'completed' AND completed_at IS NOT NULL \
             ORDER BY completed_at DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .ok();

    let (analyzed_at, analyzed_commit, latest_run, plugin_stats) = match latest {
        Some((id, started_at, completed_at, run_status, stats_json, analyzed_commit)) => {
            let parsed_stats = serde_json::from_str::<Value>(&stats_json).unwrap_or(Value::Null);
            let run = json!({
                "id": id,
                "started_at": started_at,
                "completed_at": completed_at,
                "status": run_status,
                "analyzed_at_commit": analyzed_commit,
            });
            (
                completed_at,
                analyzed_commit,
                Some(run),
                plugin_stats_subset(&parsed_stats),
            )
        }
        None => (None, None, None, Value::Null),
    };

    let mut stmt = conn.prepare(
        "SELECT source_file_path, COUNT(*) FROM entities \
         WHERE source_file_path IS NOT NULL \
         GROUP BY source_file_path ORDER BY source_file_path",
    )?;
    let files = stmt
        .query_map([], |row| {
            Ok(IndexedFile {
                source_file_path: row.get(0)?,
                entity_count: row.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(IndexState {
        analyzed_at,
        analyzed_commit,
        latest_run,
        files,
        plugin_stats,
    })
}

/// Pull just the per-run aggregate skip/drop/unresolved counters that bear on
/// "what did the plugins fail to fully resolve?". Per-file failure lists are
/// not retained in v0.1 (wipe-and-rerun model), so this is the honest surface.
fn plugin_stats_subset(stats: &Value) -> Value {
    let pick = |key: &str| stats.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "dropped_edges_total": pick("dropped_edges_total"),
        "imports_skipped_external_total": pick("imports_skipped_external_total"),
        "references_skipped_external_total": pick("references_skipped_external_total"),
        "references_skipped_cap_total": pick("references_skipped_cap_total"),
        "unresolved_call_sites_total": pick("unresolved_call_sites_total"),
        "unresolved_reference_sites_total": pick("unresolved_reference_sites_total"),
    })
}

fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    OffsetDateTime::parse(s, &Rfc3339)
        .ok()
        .map(SystemTime::from)
}

#[derive(Default)]
pub(crate) struct FileDrift {
    modified: Vec<Value>,
    missing: Vec<Value>,
    statted: usize,
    stat_failures: usize,
    /// Recorded `source_file_path`s that stat as DIRECTORIES — e.g. the
    /// synthetic project-anchor entity, whose path is the project root. A
    /// directory's mtime changes on any direct child create/delete and is not
    /// a file-modification signal: treating it as one wedged lacuna's
    /// staleness flag permanently (weft-4165f1ed71). Skipped + counted.
    skipped_non_files: usize,
    /// `true` when the per-file stat scan stopped at [`MAX_FILE_STAT_SCAN`]
    /// without finding drift: a no-drift verdict is then proven only for the
    /// scanned prefix.
    pub(crate) stat_scan_truncated: bool,
}

/// Stat each indexed file once: classify as modified (mtime newer than the
/// analyze time), missing (gone from disk), or fresh. `analyzed_time` is `None`
/// when the run timestamp could not be parsed — files are still statted for
/// existence, but none are flagged modified (the mtime channel is blind).
/// Bounded by [`MAX_FILE_STAT_SCAN`]; non-file paths (directories) are skipped
/// and counted, never treated as modified.
fn compute_file_drift(
    project_root: &Path,
    state: &IndexState,
    analyzed_time: Option<SystemTime>,
) -> FileDrift {
    let mut drift = FileDrift::default();
    for file in state.files.iter().take(MAX_FILE_STAT_SCAN) {
        let abs = absolute(project_root, &file.source_file_path);
        match std::fs::metadata(&abs) {
            Ok(meta) if !meta.is_file() => drift.skipped_non_files += 1,
            Ok(meta) => {
                drift.statted += 1;
                let mtime = meta.modified().ok();
                if let (Some(mtime), Some(analyzed)) = (mtime, analyzed_time)
                    && mtime > analyzed
                {
                    drift.modified.push(json!({
                        "path": file.source_file_path,
                        "indexed_entities": file.entity_count,
                    }));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                drift.missing.push(json!({
                    "path": file.source_file_path,
                    "indexed_entities": file.entity_count,
                }));
            }
            Err(_) => drift.stat_failures += 1,
        }
    }
    if state.files.len() > MAX_FILE_STAT_SCAN {
        drift.stat_scan_truncated = true;
        tracing::warn!(
            indexed_files = state.files.len(),
            cap = MAX_FILE_STAT_SCAN,
            "index freshness: ingested-file count exceeds the stat cap; in-place edits \
             beyond the cap may go unnoticed until the next analyze"
        );
    }
    drift
}

/// The single freshness verdict vocabulary, shared by `index_diff_get`'s
/// `overall` field and (via mapping) the snapshot's `staleness` (C-12,
/// weft-4165f1ed71).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FreshnessOverall {
    /// No completed analyze run exists.
    NeverAnalyzed,
    /// A drift signal fired: commit mismatch, HEAD newer than the analyze,
    /// a modified/missing indexed file, or a staged change touching an
    /// indexed path.
    Drift,
    /// No drift signal fired, but the working tree holds UNTRACKED source of
    /// an already-indexed file type the index has never seen (ADR-045,
    /// hardened `git ls-files --others` scoped to ingested extensions).
    StaleWorktree,
    /// State was observable and nothing fired.
    Fresh,
    /// Every observation channel was blind (no file statted successfully and
    /// git offered no HEAD comparison).
    Unknown,
}

impl FreshnessOverall {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NeverAnalyzed => "never_analyzed",
            Self::Drift => "drift",
            Self::StaleWorktree => "stale_worktree",
            Self::Fresh => "fresh",
            Self::Unknown => "unknown",
        }
    }
}

/// The authoritative freshness verdict + every signal that fed it. Computed
/// once here; rendered by `index_diff_get` and mapped by the snapshot.
pub(crate) struct FreshnessVerdict {
    pub(crate) overall: FreshnessOverall,
    pub(crate) drift_detected: bool,
    pub(crate) commit_mismatch: Option<bool>,
    pub(crate) head_newer_than_analyze: Option<bool>,
    /// `false` when a completed run's timestamp failed to parse (a
    /// data/machinery fault the snapshot reports as `degraded`); `true`
    /// otherwise (including never-analyzed, where there is nothing to parse).
    pub(crate) analyzed_time_parsed: bool,
    pub(crate) file_drift: FileDrift,
    /// Staged-vs-HEAD entries (`{path, status, indexed}`), pre-rendered.
    pub(crate) dirty: Vec<Value>,
    pub(crate) dirty_indexed_count: usize,
    /// Untracked-source signal (ADR-045): `None` outside a git work tree /
    /// with nothing ingested to scope against.
    pub(crate) untracked_source: Option<bool>,
}

/// Compute the project's ONE freshness verdict (C-12, weft-4165f1ed71) from
/// index state + git facts + the untracked-source signal. Every surface that
/// answers "is the index fresh?" goes through here.
pub(crate) fn compute_freshness(
    project_root: &Path,
    state: &IndexState,
    git: &GitFacts,
    untracked_source: Option<bool>,
) -> FreshnessVerdict {
    let analyzed_time = state.analyzed_at.as_deref().and_then(parse_rfc3339);
    let analyzed_time_parsed = state.analyzed_at.is_none() || analyzed_time.is_some();

    let commit_mismatch = match (git.head_commit.as_deref(), state.analyzed_commit.as_deref()) {
        (Some(current), Some(analyzed)) => Some(current != analyzed),
        _ => None,
    };
    // HEAD-vs-analyze by committer date — independent of source mtimes.
    let head_newer_than_analyze = match (
        git.head_committed_at.as_deref().and_then(parse_rfc3339),
        analyzed_time,
    ) {
        (Some(head), Some(analyzed)) => Some(head > analyzed),
        _ => None,
    };

    // Per-file drift: stat each indexed file once (bounded, dir-skipping).
    let file_drift = compute_file_drift(project_root, state, analyzed_time);

    // Indexed source paths (absolute) normalized to canonical project-relative
    // form, so a git-relative dirty path matches regardless of the project_root
    // shape (`.` vs absolute) or symlinks. A raw join + string-eq would never
    // match (clarion-326b01ffd0 review).
    let indexed_rel: BTreeSet<String> = state
        .files
        .iter()
        .filter_map(|f| normalize_source_path(project_root, &f.source_file_path).ok())
        .collect();

    // Staged-vs-HEAD changes (clarion-4b5a8aff54: no worktree hashing), flagged
    // when they touch an indexed path. Unstaged/untracked changes are not in this
    // set; unstaged edits to indexed files surface via `file_drift` above.
    let mut dirty = Vec::new();
    let mut dirty_indexed_count = 0usize;
    for entry in &git.dirty {
        let indexed = normalize_source_path(project_root, &entry.rel_path)
            .ok()
            .is_some_and(|rel| indexed_rel.contains(&rel));
        if indexed {
            dirty_indexed_count += 1;
        }
        dirty.push(json!({
            "path": entry.rel_path,
            "status": entry.status,
            "indexed": indexed,
        }));
    }

    let drift_signal = commit_mismatch == Some(true)
        || (commit_mismatch.is_none() && head_newer_than_analyze == Some(true))
        || !file_drift.modified.is_empty()
        || !file_drift.missing.is_empty()
        || dirty_indexed_count > 0;

    // Verdict: drift if any signal fired; stale_worktree if otherwise clean but
    // un-indexed untracked source exists; fresh if we could observe state and
    // nothing fired; unknown only when every observation channel was blind.
    let could_observe = file_drift.statted > 0
        || commit_mismatch.is_some()
        || head_newer_than_analyze.is_some()
        || untracked_source.is_some();
    let overall = if state.analyzed_at.is_none() {
        FreshnessOverall::NeverAnalyzed
    } else if drift_signal {
        FreshnessOverall::Drift
    } else if untracked_source == Some(true) {
        FreshnessOverall::StaleWorktree
    } else if could_observe {
        FreshnessOverall::Fresh
    } else {
        FreshnessOverall::Unknown
    };

    FreshnessVerdict {
        overall,
        // `drift_detected` is the boolean a gate reads: true for BOTH drift and
        // the untracked-source case — an index that does not reflect the
        // working tree must never read as a bare `false` here.
        drift_detected: drift_signal || overall == FreshnessOverall::StaleWorktree,
        commit_mismatch,
        head_newer_than_analyze,
        analyzed_time_parsed,
        file_drift,
        dirty,
        dirty_indexed_count,
        untracked_source,
    }
}

/// Whether the working tree holds untracked source of an already-indexed file
/// type — the ADR-045 signal (clarion-26c7e52027). Fail-soft: `None` when
/// nothing is ingested (no extensions to scope against, so the check is moot),
/// the project is not a git work tree, or git is unavailable.
///
/// Scoping to ingested extensions is what keeps this honest: a hardened
/// `git ls-files --others --exclude-standard` lists every untracked,
/// non-ignored path, but only those whose extension Loomweave actually ingests
/// count — an untracked `notes.txt` never flags a fresh index dirty, while an
/// untracked `hub.py` (the dogfood scenario) does.
pub(crate) fn compute_untracked_source(
    conn: &rusqlite::Connection,
    project_root: &Path,
) -> Option<bool> {
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

/// The distinct file extensions among ingested `source_file_path`s. Fail-soft
/// to an empty set on any query error, which makes [`compute_untracked_source`]
/// return `None` (treat the scope as unknown).
fn ingested_source_extensions(conn: &rusqlite::Connection) -> BTreeSet<String> {
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

/// Assemble the `index_diff` report from index state + git facts. Stats the
/// indexed files (cheap IO); correlates git dirty paths against indexed paths.
/// The verdict itself comes from [`compute_freshness`] — the one oracle every
/// freshness surface shares (C-12, weft-4165f1ed71).
pub(crate) fn build_report(
    project_root: &Path,
    state: &IndexState,
    git: &GitFacts,
    untracked_source: Option<bool>,
    cap: usize,
) -> Value {
    let git_json = json!({
        "available": git.available,
        "is_repo": git.is_repo,
        "current_commit": git.head_commit,
        "head_committed_at": git.head_committed_at,
        "reason": git.reason,
    });

    // No completed run: nothing to diff against.
    let Some(analyzed_at) = state.analyzed_at.as_deref() else {
        return json!({
            "overall": "never_analyzed",
            "drift_detected": false,
            "analyzed_at": Value::Null,
            "analyzed_commit": Value::Null,
            "git": git_json,
            "notes": ["no completed analyze run; run `loomweave analyze` first"],
        });
    };

    let verdict = compute_freshness(project_root, state, git, untracked_source);
    let FreshnessVerdict {
        overall,
        drift_detected,
        commit_mismatch,
        head_newer_than_analyze,
        analyzed_time_parsed: _,
        file_drift,
        dirty,
        dirty_indexed_count,
        untracked_source,
    } = verdict;

    let stat_scan_truncated = file_drift.stat_scan_truncated;
    let skipped_non_files = file_drift.skipped_non_files;
    let stat_failures = file_drift.stat_failures;
    let (modified, modified_omitted) = cap_list(file_drift.modified, cap);
    let (missing, missing_omitted) = cap_list(file_drift.missing, cap);
    let (dirty, dirty_omitted) = cap_list(dirty, cap);

    let mut notes = vec![
        "this is the AUTHORITATIVE freshness verdict (one oracle, C-12); \
         project_status_get / orientation / the session-start banner derive their \
         staleness from this same computation"
            .to_owned(),
        "when both commits are known, current_commit != analyzed_commit is the \
         primary staleness signal; HEAD committer date remains a fallback diagnostic"
            .to_owned(),
        "added (never-indexed) source files are not enumerated here beyond the \
         git dirty set and the untracked_source flag; a new commit still flips \
         head_newer_than_analyze"
            .to_owned(),
        "dirty_files lists STAGED changes (index vs HEAD); unstaged working-tree \
         modifications are not enumerated (untrusted-corpus hardening). Unstaged \
         edits to indexed files still surface in modified_since_analyze; untracked \
         SOURCE files surface via untracked_source (ignore-aware ls-files scoped to \
         ingested extensions)."
            .to_owned(),
    ];
    if stat_failures > 0 {
        notes.push(format!(
            "{stat_failures} indexed file(s) could not be stat-ed (permission/IO); \
             excluded from the modified/missing sets"
        ));
    }
    if skipped_non_files > 0 {
        notes.push(format!(
            "{skipped_non_files} recorded source path(s) stat as directories (e.g. the \
             project anchor); a directory mtime is not a modification signal and is \
             excluded"
        ));
    }

    json!({
        "overall": overall.as_str(),
        "drift_detected": drift_detected,
        "analyzed_at": analyzed_at,
        "analyzed_commit": state.analyzed_commit,
        "latest_run": state.latest_run,
        "git": git_json,
        "commit_mismatch": commit_mismatch,
        "head_newer_than_analyze": head_newer_than_analyze,
        "indexed_files": state.files.len(),
        "modified_since_analyze": modified,
        "missing_files": missing,
        "dirty_files": dirty,
        "dirty_indexed_count": dirty_indexed_count,
        // ADR-045 untracked-source signal, folded into the single verdict:
        // `true` + an otherwise-clean index → overall "stale_worktree".
        "untracked_source": untracked_source,
        "file_stat_scan_truncated": stat_scan_truncated,
        "skipped_non_file_paths": skipped_non_files,
        // Per-run aggregate plugin skip/drop counters; per-file failure lists
        // are not retained in v0.1 (wipe-and-rerun).
        "plugin_resolution": state.plugin_stats,
        // Entity-level add/remove/change diff needs a retained prior-run
        // snapshot; v0.1 keeps only the current graph (wipe-and-rerun).
        "entity_diff": {
            "available": false,
            "reason": "v0.1 retains only the current run's graph; no prior-run snapshot to diff against",
        },
        "omitted": {
            "modified_since_analyze": modified_omitted,
            "missing_files": missing_omitted,
            "dirty_files": dirty_omitted,
        },
        "notes": notes,
    })
}

fn absolute(project_root: &Path, path: &str) -> String {
    if Path::new(path).is_absolute() {
        path.to_owned()
    } else {
        project_root.join(path).to_string_lossy().into_owned()
    }
}

fn cap_list(mut list: Vec<Value>, cap: usize) -> (Vec<Value>, usize) {
    if list.len() > cap {
        let omitted = list.len() - cap;
        list.truncate(cap);
        (list, omitted)
    } else {
        (list, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_name_status_handles_modified_added_and_rename() {
        // `git diff --cached --name-status -M HEAD` format: tab-separated, with a
        // second path column for renames/copies.
        let out = "M\tsrc/lib.rs\nA\tnew.py\nR096\told.rs\tnew.rs\n";
        let entries = parse_name_status(out);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status, "M");
        assert_eq!(entries[0].rel_path, "src/lib.rs");
        assert_eq!(entries[1].status, "A");
        assert_eq!(entries[1].rel_path, "new.py");
        assert_eq!(entries[2].status, "R");
        assert_eq!(entries[2].rel_path, "new.rs");
    }

    #[test]
    fn parse_name_status_skips_blank_and_malformed_lines() {
        // Blank lines and a status with no path column yield nothing.
        assert!(parse_name_status("\n\nM\n").is_empty());
    }

    #[test]
    fn parse_name_status_decodes_c_quoted_non_ascii_path() {
        // git quotes `café.py` (and emits its UTF-8 bytes as octal escapes)
        // under the default core.quotePath=true.
        let out = "M\t\"caf\\303\\251.py\"\n";
        let entries = parse_name_status(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "M");
        assert_eq!(
            entries[0].rel_path, "café.py",
            "octal-escaped UTF-8 bytes must decode to the real path"
        );
    }

    #[test]
    fn unquote_c_path_handles_escapes_quotes_and_bare_paths() {
        // Bare path: returned untouched.
        assert_eq!(unquote_c_path("src/lib.rs"), "src/lib.rs");
        // Quoted with an escaped quote and backslash.
        assert_eq!(unquote_c_path(r#""a\"b\\c.py""#), "a\"b\\c.py");
        // Quoted with a tab escape.
        assert_eq!(unquote_c_path(r#""a\tb.py""#), "a\tb.py");
        // Octal byte escapes reassemble into a multi-byte UTF-8 char.
        assert_eq!(unquote_c_path(r#""\360\237\232\200.py""#), "🚀.py");
        // A leading-quote-only string is not a valid quoted path → unchanged.
        assert_eq!(unquote_c_path("\"unterminated"), "\"unterminated");
    }

    fn git_facts(head_committed_at: Option<&str>, dirty: &[(&str, &str)]) -> GitFacts {
        GitFacts {
            available: true,
            is_repo: true,
            head_commit: Some("deadbeef".to_owned()),
            head_committed_at: head_committed_at.map(str::to_owned),
            dirty: dirty
                .iter()
                .map(|(status, path)| DirtyEntry {
                    status: (*status).to_owned(),
                    rel_path: (*path).to_owned(),
                })
                .collect(),
            reason: None,
        }
    }

    fn git_facts_with_commit(
        head_commit: &str,
        head_committed_at: Option<&str>,
        dirty: &[(&str, &str)],
    ) -> GitFacts {
        let mut facts = git_facts(head_committed_at, dirty);
        facts.head_commit = Some(head_commit.to_owned());
        facts
    }

    fn state_with_file(path: &str, entity_count: i64, analyzed_at: &str) -> IndexState {
        IndexState {
            analyzed_at: Some(analyzed_at.to_owned()),
            analyzed_commit: None,
            latest_run: Some(json!({"id": "run-1", "status": "completed"})),
            files: vec![IndexedFile {
                source_file_path: path.to_owned(),
                entity_count,
            }],
            plugin_stats: json!({}),
        }
    }

    #[test]
    fn never_analyzed_when_no_completed_run() {
        let dir = tempfile::tempdir().unwrap();
        let state = IndexState {
            analyzed_at: None,
            analyzed_commit: None,
            latest_run: None,
            files: Vec::new(),
            plugin_stats: Value::Null,
        };
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[]),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["overall"], "never_analyzed");
        assert_eq!(report["drift_detected"], false);
    }

    #[test]
    fn clean_fresh_graph_reports_no_drift() {
        // AC: clean repo + fresh graph → no drift. The file's mtime is "now";
        // an analyze timestamp far in the future keeps it un-modified.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        let abs = dir.path().join("a.py").to_string_lossy().into_owned();
        let state = state_with_file(&abs, 3, "2999-01-01T00:00:00.000Z");
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[]),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["overall"], "fresh");
        assert_eq!(report["drift_detected"], false);
        assert_eq!(report["modified_since_analyze"], json!([]));
        assert_eq!(report["analyzed_commit"], Value::Null);
    }

    #[test]
    fn different_current_commit_is_stale_even_when_head_is_older() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        let abs = dir.path().join("a.py").to_string_lossy().into_owned();
        let mut state = state_with_file(&abs, 3, "2999-01-01T00:00:00.000Z");
        state.analyzed_commit = Some("newer-indexed-commit".to_owned());
        let report = build_report(
            dir.path(),
            &state,
            &git_facts_with_commit(
                "older-checked-out-commit",
                Some("2000-01-01T00:00:00+00:00"),
                &[],
            ),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["analyzed_commit"], "newer-indexed-commit");
        assert_eq!(report["git"]["current_commit"], "older-checked-out-commit");
        assert_eq!(report["commit_mismatch"], true);
        assert_eq!(report["head_newer_than_analyze"], false);
        assert_eq!(report["overall"], "drift");
    }

    #[test]
    fn modified_source_file_is_flagged_with_path_and_entity_impact() {
        // AC: a modified source file is flagged with path + indexed entity impact.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        let abs = dir.path().join("a.py").to_string_lossy().into_owned();
        let state = state_with_file(&abs, 5, "2000-01-01T00:00:00.000Z");
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[]),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["overall"], "drift");
        assert_eq!(report["drift_detected"], true);
        let modified = report["modified_since_analyze"].as_array().unwrap();
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0]["path"], abs);
        assert_eq!(modified[0]["indexed_entities"], 5);
    }

    #[test]
    fn head_newer_than_analyze_is_stale_even_when_mtimes_are_not() {
        // AC: a HEAD newer than the last analyze is reported stale even if source
        // mtimes are ambiguous. analyzed_at is in 2500 (so the just-written file's
        // 2026 mtime does NOT flag it modified), but HEAD committed in 2600.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        let abs = dir.path().join("a.py").to_string_lossy().into_owned();
        let state = state_with_file(&abs, 2, "2500-01-01T00:00:00.000Z");
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(Some("2600-01-01T00:00:00+00:00"), &[]),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["head_newer_than_analyze"], true);
        assert_eq!(report["overall"], "drift");
        assert_eq!(
            report["modified_since_analyze"],
            json!([]),
            "the drift signal comes from the commit clock, not mtime"
        );
    }

    #[test]
    fn dirty_file_touching_indexed_path_drives_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        let abs = dir.path().join("a.py").to_string_lossy().into_owned();
        let state = state_with_file(&abs, 1, "2999-01-01T00:00:00.000Z");
        // Git reports a.py dirty (project-relative); it joins to the indexed abs.
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[("M", "a.py"), ("??", "untracked.txt")]),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["drift_detected"], true);
        assert_eq!(report["dirty_indexed_count"], 1);
        let dirty = report["dirty_files"].as_array().unwrap();
        assert_eq!(dirty.len(), 2);
        let a = dirty.iter().find(|d| d["path"] == "a.py").unwrap();
        assert_eq!(a["indexed"], true);
        let u = dirty.iter().find(|d| d["path"] == "untracked.txt").unwrap();
        assert_eq!(u["indexed"], false);
    }

    #[test]
    fn untracked_source_flips_an_otherwise_fresh_report_to_stale_worktree() {
        // C-12 (weft-4165f1ed71): the ADR-045 untracked-source signal is part
        // of the ONE freshness verdict — an otherwise-clean index with
        // un-indexed untracked source must not read "fresh".
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        let abs = dir.path().join("a.py").to_string_lossy().into_owned();
        let state = state_with_file(&abs, 3, "2999-01-01T00:00:00.000Z");
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[]),
            Some(true),
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["overall"], "stale_worktree", "{report}");
        assert_eq!(
            report["drift_detected"], true,
            "a boolean gate must not read an unreflected working tree as clean: {report}"
        );
        assert_eq!(report["untracked_source"], true);
    }

    #[test]
    fn directory_valued_source_path_is_skipped_not_modified() {
        // weft-4165f1ed71: the lacuna project-anchor entity records the project
        // ROOT DIRECTORY as its source path. A directory mtime bumps on any
        // direct child create/delete — it is not a file-modification signal and
        // must never produce drift.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        let abs = dir.path().join("a.py").to_string_lossy().into_owned();
        let root_as_path = dir.path().to_string_lossy().into_owned();
        let state = IndexState {
            analyzed_at: Some("2000-01-01T00:00:00.000Z".to_owned()),
            analyzed_commit: None,
            latest_run: Some(json!({"id": "run-1", "status": "completed"})),
            files: vec![
                IndexedFile {
                    // The directory: its mtime ("now") is far newer than the
                    // 2000 analyze time, but it must be skipped, not flagged.
                    source_file_path: root_as_path,
                    entity_count: 1,
                },
                IndexedFile {
                    source_file_path: abs.clone(),
                    entity_count: 3,
                },
            ],
            plugin_stats: json!({}),
        };
        // The real file IS newer than the 2000 run — drift comes from the FILE,
        // never the directory.
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[]),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        let modified = report["modified_since_analyze"].as_array().unwrap();
        assert_eq!(modified.len(), 1, "{report}");
        assert_eq!(modified[0]["path"], abs);
        assert_eq!(report["skipped_non_file_paths"], 1, "{report}");
    }

    #[test]
    fn file_stat_scan_caps_and_reports_truncation() {
        // One real, old file repeated past the cap: the scan stops at the cap
        // (so the no-drift verdict is bounded) and says so in-band.
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.py");
        std::fs::write(&old, "x = 1\n").unwrap();
        let abs = old.to_string_lossy().into_owned();
        let state = IndexState {
            analyzed_at: Some("2999-01-01T00:00:00.000Z".to_owned()),
            analyzed_commit: None,
            latest_run: Some(json!({"id": "run-1", "status": "completed"})),
            files: (0..=super::MAX_FILE_STAT_SCAN)
                .map(|_| IndexedFile {
                    source_file_path: abs.clone(),
                    entity_count: 1,
                })
                .collect(),
            plugin_stats: json!({}),
        };
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[]),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["overall"], "fresh", "{report}");
        assert_eq!(report["file_stat_scan_truncated"], true, "{report}");
    }

    #[test]
    fn missing_indexed_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("gone.py").to_string_lossy().into_owned();
        let state = state_with_file(&abs, 4, "2999-01-01T00:00:00.000Z");
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[]),
            None,
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["drift_detected"], true);
        let missing = report["missing_files"].as_array().unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0]["path"], abs);
        assert_eq!(missing[0]["indexed_entities"], 4);
    }

    /// REGRESSION (clarion-4b5a8aff54): `gather_git_facts` gathers facts against
    /// an untrusted served corpus. It must not execute any repo-controlled helper
    /// — not `core.fsmonitor`, and not a `filter.<name>.clean` selected by ANY
    /// attribute source (in-tree `.gitattributes`, `$GIT_DIR/info/attributes`
    /// which no config flag disables, or `core.attributesFile`). It avoids
    /// `git status` (which would hash the worktree) in favour of `git diff
    /// --cached`, so staged changes are still reported.
    #[cfg(unix)]
    #[test]
    fn gather_git_facts_does_not_execute_repo_controlled_helpers() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_path_buf();
        // Raw `git` is fine here: it builds the trusted fixture repo. The
        // assertion below exercises the hardened production path.
        let run_git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?} failed");
        };

        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "t@t"]);
        run_git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("auth.py"), "def login():\n    return 1\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-qm", "init"]);
        // Dirty the tree so `git status` must re-hash working-tree content.
        run_git(&["mv", "auth.py", "authn.py"]);
        std::fs::write(repo.join("authn.py"), "def login():\n    return 2\n").unwrap();

        let make_payload = |name: &str, marker: &Path| {
            let p = repo.join(name);
            std::fs::write(
                &p,
                format!("#!/bin/sh\necho fired > {}\ncat\n", marker.display()),
            )
            .unwrap();
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&p, perms).unwrap();
            p
        };
        let fsmonitor_marker = repo.join("FSMONITOR_FIRED");
        let filter_marker = repo.join("FILTER_FIRED");
        let fsmonitor_payload = make_payload("fsmonitor.sh", &fsmonitor_marker);
        let filter_payload = make_payload("filter.sh", &filter_marker);

        run_git(&[
            "config",
            "core.fsmonitor",
            &fsmonitor_payload.display().to_string(),
        ]);
        // `filter=evil` assigned from all three attribute sources at once.
        std::fs::write(repo.join(".gitattributes"), "* filter=evil\n").unwrap();
        std::fs::write(repo.join(".git/info/attributes"), "* filter=evil\n").unwrap();
        std::fs::write(repo.join("extra-attrs"), "* filter=evil\n").unwrap();
        run_git(&[
            "config",
            "core.attributesFile",
            &repo.join("extra-attrs").display().to_string(),
        ]);
        run_git(&[
            "config",
            "filter.evil.clean",
            &filter_payload.display().to_string(),
        ]);

        let facts = gather_git_facts(&repo);

        assert!(
            !fsmonitor_marker.exists(),
            "repo-local core.fsmonitor executed during gather_git_facts"
        );
        assert!(
            !filter_marker.exists(),
            "repo-local filter.*.clean executed during gather_git_facts"
        );
        assert!(facts.is_repo, "repo must still be recognized");
        assert!(
            facts.dirty.iter().any(|e| e.rel_path == "authn.py"),
            "dirty reporting must still work; got {:?}",
            facts.dirty.iter().map(|e| &e.rel_path).collect::<Vec<_>>()
        );
    }
}
