//! `index_diff`: deterministic index freshness / drift report (clarion-326b01ffd0).
//!
//! Answers "what changed since the last analyze, and is this checkout newer
//! than the graph?" without an agent hand-rolling git + mtime checks.
//!
//! **Git posture (per the issue's design fork).** Clarion persists no
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
use std::process::Command;
use std::time::SystemTime;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use clarion_storage::{StorageError, normalize_source_path};

/// Default per-list cap so a pathological repo cannot produce an unbounded
/// packet. Overridable via the `limit` argument.
pub(crate) const DEFAULT_MAX_ENTRIES: usize = 200;

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

/// Build a constrained `git` invocation for read-only freshness checks.
///
/// `index_diff` may be pointed at arbitrary project directories. Even without
/// a shell, Git honors repository-local configuration; notably, `git status`
/// can execute a `core.fsmonitor` helper from `.git/config`. Override that
/// execution-capable knob on every invocation so a freshness query remains
/// read-only for attacker-prepared worktrees.
fn git_command(project_root: &Path) -> Command {
    let mut command = Command::new("git");
    sanitize_git_environment(&mut command);
    command
        .arg("-C")
        .arg(project_root)
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.untrackedCache=false"]);
    command
}

fn sanitize_git_environment(command: &mut Command) {
    const GIT_ENV_KEYS: &[&str] = &[
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_SYSTEM",
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_WORK_TREE",
    ];

    for key in GIT_ENV_KEYS {
        command.env_remove(key);
    }
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("GIT_CONFIG_KEY_") || key.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(key.as_ref());
        }
    }

    // Keep system/global config out of this read-only probe. Repository-local
    // config is still loaded by Git, so the command-line overrides above remain
    // the critical protection against `.git/config` fsmonitor helpers.
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .env("GIT_CONFIG_SYSTEM", null_device());
}

#[cfg(not(windows))]
fn null_device() -> &'static str {
    "/dev/null"
}

#[cfg(windows)]
fn null_device() -> &'static str {
    "NUL"
}

/// Run `git` read-only against `project_root` and collect HEAD + dirty-tree
/// facts. Blocking; call from a `spawn_blocking` context.
pub(crate) fn gather_git_facts(project_root: &Path) -> GitFacts {
    let inside = git_command(project_root)
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
        let out = git_command(project_root).args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    };

    let head_commit = run(&["rev-parse", "HEAD"]);
    // `%cI` is strict ISO-8601 (RFC3339) with the committer's UTC offset.
    let head_committed_at = run(&["log", "-1", "--format=%cI", "HEAD"]);
    // Read status raw: porcelain's leading X-column space is significant, so it
    // must NOT be trimmed off the front (the `run` closure trims the whole
    // blob, which would shift every column left and corrupt the path).
    let dirty = git_command(project_root)
        .args(["status", "--porcelain=v1"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| parse_porcelain(&String::from_utf8_lossy(&out.stdout)))
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

/// Parse `git status --porcelain=v1` output into per-path entries. Renames
/// (`R  old -> new`) collapse to the new path; git's C-style quoting of paths
/// with special bytes is decoded (see [`unquote_c_path`]).
fn parse_porcelain(out: &str) -> Vec<DirtyEntry> {
    out.lines()
        .filter_map(|line| {
            if line.len() <= 3 {
                return None;
            }
            let status = line[..2].trim().to_owned();
            let rest = line[3..].trim();
            let path = rest.rsplit(" -> ").next().unwrap_or(rest);
            Some(DirtyEntry {
                status,
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
struct FileDrift {
    modified: Vec<Value>,
    missing: Vec<Value>,
    statted: usize,
    stat_failures: usize,
}

/// Stat each indexed file once: classify as modified (mtime newer than the
/// analyze time), missing (gone from disk), or fresh. `analyzed_time` is `None`
/// when the run timestamp could not be parsed — files are still statted for
/// existence, but none are flagged modified (the mtime channel is blind).
fn compute_file_drift(
    project_root: &Path,
    state: &IndexState,
    analyzed_time: Option<SystemTime>,
) -> FileDrift {
    let mut drift = FileDrift::default();
    for file in &state.files {
        let abs = absolute(project_root, &file.source_file_path);
        match std::fs::metadata(&abs) {
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
    drift
}

/// Assemble the `index_diff` report from index state + git facts. Stats the
/// indexed files (cheap IO); correlates git dirty paths against indexed paths.
pub(crate) fn build_report(
    project_root: &Path,
    state: &IndexState,
    git: &GitFacts,
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
            "notes": ["no completed analyze run; run `clarion analyze` first"],
        });
    };
    let analyzed_time = parse_rfc3339(analyzed_at);
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

    // Per-file drift: stat each indexed file once.
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

    // Dirty working-tree files, flagged when they touch an indexed path.
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

    let drift_detected = commit_mismatch == Some(true)
        || (commit_mismatch.is_none() && head_newer_than_analyze == Some(true))
        || !file_drift.modified.is_empty()
        || !file_drift.missing.is_empty()
        || dirty_indexed_count > 0;

    // Verdict: drift if any signal fired; fresh if we could observe state and
    // nothing fired; unknown only when every observation channel was blind
    // (no files statted AND git gave us no HEAD comparison).
    let could_observe =
        file_drift.statted > 0 || commit_mismatch.is_some() || head_newer_than_analyze.is_some();
    let overall = if drift_detected {
        "drift"
    } else if could_observe {
        "fresh"
    } else {
        "unknown"
    };

    let (modified, modified_omitted) = cap_list(file_drift.modified, cap);
    let (missing, missing_omitted) = cap_list(file_drift.missing, cap);
    let (dirty, dirty_omitted) = cap_list(dirty, cap);

    let mut notes = vec![
        "when both commits are known, current_commit != analyzed_commit is the \
         primary staleness signal; HEAD committer date remains a fallback diagnostic"
            .to_owned(),
        "added (never-indexed) source files are not enumerated here beyond the \
         git dirty set; a new commit still flips head_newer_than_analyze"
            .to_owned(),
    ];
    if file_drift.stat_failures > 0 {
        notes.push(format!(
            "{} indexed file(s) could not be stat-ed (permission/IO); \
             excluded from the modified/missing sets",
            file_drift.stat_failures
        ));
    }

    json!({
        "overall": overall,
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
    fn parse_porcelain_handles_modified_and_rename() {
        let out = " M src/lib.rs\nR  old.rs -> new.rs\n?? untracked.py\n";
        let entries = parse_porcelain(out);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status, "M");
        assert_eq!(entries[0].rel_path, "src/lib.rs");
        assert_eq!(entries[1].status, "R");
        assert_eq!(entries[1].rel_path, "new.rs");
        assert_eq!(entries[2].status, "??");
        assert_eq!(entries[2].rel_path, "untracked.py");
    }

    #[test]
    fn parse_porcelain_skips_blank_and_short_lines() {
        assert!(parse_porcelain("\n \nM\n").is_empty());
    }

    #[test]
    fn parse_porcelain_decodes_c_quoted_non_ascii_path() {
        // git quotes `café.py` (and emits its UTF-8 bytes as octal escapes)
        // under the default core.quotePath=true.
        let out = " M \"caf\\303\\251.py\"\n";
        let entries = parse_porcelain(out);
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

    #[cfg(unix)]
    fn git_ok(project_root: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(project_root)
            .args(args)
            .output()
            .is_ok_and(|out| out.status.success())
    }

    #[test]
    #[cfg(unix)]
    fn gather_git_facts_disables_repo_configured_fsmonitor() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        if !git_ok(dir.path(), &["init"]) {
            eprintln!("skipping fsmonitor regression: git init failed");
            return;
        }

        let marker = dir.path().join("fsmonitor.marker");
        let hook = dir.path().join("fsmonitor-poc.sh");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\nprintf executed > {}\n", marker.display()),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&hook).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook, perms).unwrap();
        assert!(git_ok(
            dir.path(),
            &["config", "core.fsmonitor", hook.to_str().unwrap()]
        ));

        // Prove this Git build/repo setup would execute the configured helper
        // without the production mitigation, then assert `gather_git_facts` does
        // not execute it. Older Git builds that do not invoke fsmonitor here are
        // not useful for this regression and are treated as a no-op environment.
        let _ = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["status", "--porcelain=v1"])
            .output();
        if !marker.exists() {
            eprintln!("skipping fsmonitor regression: git status did not invoke fsmonitor");
            return;
        }
        std::fs::remove_file(&marker).unwrap();

        let facts = gather_git_facts(dir.path());
        assert!(facts.available);
        assert!(facts.is_repo);
        assert!(
            !marker.exists(),
            "gather_git_facts must not execute repo-configured core.fsmonitor"
        );
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
    fn missing_indexed_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("gone.py").to_string_lossy().into_owned();
        let state = state_with_file(&abs, 4, "2999-01-01T00:00:00.000Z");
        let report = build_report(
            dir.path(),
            &state,
            &git_facts(None, &[]),
            DEFAULT_MAX_ENTRIES,
        );
        assert_eq!(report["drift_detected"], true);
        let missing = report["missing_files"].as_array().unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0]["path"], abs);
        assert_eq!(missing[0]["indexed_entities"], 4);
    }
}
