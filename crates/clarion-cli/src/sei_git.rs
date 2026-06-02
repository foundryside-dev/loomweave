//! `GitRenameSource` implementations — the typed git-rename seam (REQ-C-05).
//!
//! The SEI matcher consumes a typed, locator-level git-rename signal
//! (`{old_locator, new_locator}`), never "Clarion's git code". Two suppliers
//! implement the same trait, with a shared file→locator translation:
//!
//! - [`ShellGitRenameSource`] — the v1 concrete supplier: shells
//!   `git diff --name-status -M` for *file* renames and translates each into the
//!   locator renames it implies, by substituting the renamed file's module prefix
//!   across the current run's locator set.
//! - [`LegisGitRenameSource`] — the WS9 supplier: reads `legis`'s
//!   `GET /git/renames?rev_range=…` over HTTP and feeds the *same* translation,
//!   so `legis` becomes the first external supplier with no change to the matcher
//!   (SEI spec §6 / REQ-C-05). Enrich-only: never required, fail-soft to empty.
//!
//! ## Window mismatch (the WS9 load-bearing fact)
//! The two suppliers observe **different rename windows**:
//! - `ShellGitRenameSource` diffs the **working tree against `HEAD`** (`git diff -M
//!   HEAD`, empty base) → *uncommitted* renames: the "rename a file, then
//!   re-analyze before commit" flow `analyze` depends on today.
//! - `legis`'s endpoint serves only **committed** renames over a rev-range
//!   (`git log -M`). In the working-tree flow it returns empty.
//!
//! So [`select_git_rename_source`] is **capability-aware**: for the working-tree
//! window (empty base) the shell source is the authority regardless of `legis`;
//! `legis` is selected only for a committed rev-range when configured AND
//! reachable. This guarantees Clarion-with-`legis` is never *worse* than
//! Clarion-without (the enrich-only invariant, loom.md §5). The matcher is
//! fail-closed anyway — a rename is only a hint, confirmed by byte-identical body
//! hash — so neither window choice can cause a *false* carry, only a missed one.
//! The gap (legis lacks a working-tree rename surface; Clarion does not yet drive
//! a committed rev-range re-index) is surfaced in `docs/federation/contracts.md`.
//!
//! ## Scope (v1, honest framing)
//! - Path→module translation is Python-shaped (strip the extension, drop a
//!   trailing `/__init__`, map separators to `.`). That matches the v1 Python
//!   plugin; the seam is what keeps this from calcifying.
//! - No git, not a repo, a git error, or an unreachable `legis` ⇒ an empty signal
//!   (best-effort): the move case (identical body + signature) carries the load.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clarion_storage::{GitRename, GitRenameSource};

/// How long to wait on a `legis` HTTP probe/read before giving up and degrading
/// to an empty signal. Short on purpose: `legis` is enrich-only, so a slow or
/// dead peer must never stall an `analyze` run. This is a reqwest **total**
/// request deadline (connection establishment included), so a black-hole host is
/// bounded, not infinite. On the committed-base path the bound is *sequential* —
/// [`legis_reachable`]'s `/health` probe then [`LegisGitRenameSource::fetch_renames`]'s
/// read — so a dead peer degrades to empty after at most `2 × LEGIS_HTTP_TIMEOUT`.
/// (The default working-tree path never reaches `legis` at all; see
/// [`select_git_rename_source`].) The connection-refused degrade is covered by
/// `legis_source_unreachable_degrades_to_empty`; the timeout-firing case is left
/// to this by-construction bound rather than a deliberately-slow test.
const LEGIS_HTTP_TIMEOUT: Duration = Duration::from_secs(3);

/// v1 `GitRenameSource`: shells git, translates file renames to locator renames
/// over the current run's locator set.
pub(crate) struct ShellGitRenameSource {
    repo_root: PathBuf,
    /// Every locator present in the current run. A file rename only yields a
    /// locator rename for current locators whose qualname sits under the new
    /// module (the renamed-to file).
    current_locators: Vec<String>,
}

impl ShellGitRenameSource {
    pub(crate) fn new(repo_root: impl Into<PathBuf>, current_locators: Vec<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            current_locators,
        }
    }

    /// Run `git diff --name-status -M <base>` in the repo and return its stdout,
    /// or `None` on any failure (not a repo, git missing, non-zero exit).
    fn run_git_diff(&self, base: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["diff", "--name-status", "-M"])
            .arg(base)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }
}

impl GitRenameSource for ShellGitRenameSource {
    fn renames_since(&self, base_commit: &str) -> Vec<GitRename> {
        // An empty base means "compare the working tree to HEAD".
        let base = if base_commit.is_empty() {
            "HEAD"
        } else {
            base_commit
        };
        let Some(stdout) = self.run_git_diff(base) else {
            return Vec::new();
        };
        let file_renames = parse_git_rename_lines(&stdout);
        file_renames_to_locator_renames(&file_renames, &self.current_locators)
    }
}

/// Parse `git diff --name-status -M` output, returning `(old_path, new_path)`
/// for every rename (`R<score>\told\tnew`). Non-rename lines are ignored.
fn parse_git_rename_lines(stdout: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let mut cols = line.split('\t');
        let Some(status) = cols.next() else { continue };
        if !status.starts_with('R') {
            continue;
        }
        if let (Some(old), Some(new)) = (cols.next(), cols.next())
            && !old.is_empty()
            && !new.is_empty()
        {
            out.push((old.to_owned(), new.to_owned()));
        }
    }
    out
}

/// Map a project-relative source path to its Python module qualname:
/// `a/b/c.py` → `a.b.c`; `a/b/__init__.py` → `a.b`; non-Python paths → `None`.
fn path_to_module(path: &str) -> Option<String> {
    let path = path.trim_start_matches("./");
    let stem = path
        .strip_suffix(".py")
        .or_else(|| path.strip_suffix(".pyi"))?;
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    if stem.is_empty() {
        return None;
    }
    Some(stem.replace(['/', '\\'], "."))
}

/// Translate file renames into locator renames over the current locator set.
///
/// For a rename `old_path → new_path` with modules `old_mod`/`new_mod`, any
/// current locator `{plugin}:{kind}:{qualname}` whose qualname is `new_mod` or
/// is prefixed by `new_mod.` maps to the candidate old locator with `new_mod`
/// rewritten to `old_mod`. The matcher then confirms the carry only if the old
/// binding's body hash is byte-identical (the rename predicate), so an
/// over-broad candidate here cannot cause a false carry — it is a *hint*, fail-
/// closed at the matcher.
fn file_renames_to_locator_renames(
    file_renames: &[(String, String)],
    current_locators: &[String],
) -> Vec<GitRename> {
    let mut out = Vec::new();
    for (old_path, new_path) in file_renames {
        let (Some(old_mod), Some(new_mod)) = (path_to_module(old_path), path_to_module(new_path))
        else {
            continue;
        };
        if old_mod == new_mod {
            continue;
        }
        for locator in current_locators {
            let mut segs = locator.splitn(3, ':');
            let (Some(plugin), Some(kind), Some(qualname)) =
                (segs.next(), segs.next(), segs.next())
            else {
                continue;
            };
            let new_qualname = if qualname == new_mod {
                Some(old_mod.clone())
            } else {
                qualname
                    .strip_prefix(&format!("{new_mod}."))
                    .map(|rest| format!("{old_mod}.{rest}"))
            };
            if let Some(old_qualname) = new_qualname {
                out.push(GitRename {
                    old_locator: format!("{plugin}:{kind}:{old_qualname}"),
                    new_locator: locator.clone(),
                });
            }
        }
    }
    out
}

/// The WS9 `GitRenameSource`: reads `legis`'s `GET /git/renames` over HTTP and
/// feeds the *same* file→locator translation as the shell source. `legis` owns
/// the git interface (SEI §6 / REQ-C-05); this supplier moves the signal behind
/// it with no change to the matcher. Enrich-only — any failure degrades to an
/// empty signal (the move case carries identity without git).
pub(crate) struct LegisGitRenameSource {
    /// `legis`'s read-API base URL (e.g. `http://127.0.0.1:8615`).
    base_url: String,
    /// Every locator present in the current run (same role as in the shell
    /// source — bounds which locators a file rename can imply).
    current_locators: Vec<String>,
}

impl LegisGitRenameSource {
    pub(crate) fn new(base_url: impl Into<String>, current_locators: Vec<String>) -> Self {
        Self {
            base_url: base_url.into(),
            current_locators,
        }
    }
}

impl GitRenameSource for LegisGitRenameSource {
    fn renames_since(&self, base_commit: &str) -> Vec<GitRename> {
        // The working-tree window (empty base) is precisely what `legis`'s
        // committed-rev-range endpoint cannot answer. Return empty rather than
        // issue a request that can only come back empty — and so that
        // `select_git_rename_source` callers that reach here for the wrong window
        // pay no network cost. The move case carries identity without git.
        if base_commit.is_empty() {
            return Vec::new();
        }
        let Some(body) = self.fetch_renames(base_commit) else {
            return Vec::new();
        };
        let file_renames = parse_legis_rename_json(&body);
        file_renames_to_locator_renames(&file_renames, &self.current_locators)
    }
}

impl LegisGitRenameSource {
    /// GET `legis`'s renames for the committed range `<base_commit>..HEAD`,
    /// returning the raw JSON body, or `None` on any failure (build/connect/
    /// non-2xx/read) — fail-soft, never propagated into the run.
    fn fetch_renames(&self, base_commit: &str) -> Option<String> {
        let url = legis_renames_url(&self.base_url, base_commit);
        let client = reqwest::blocking::Client::builder()
            .timeout(LEGIS_HTTP_TIMEOUT)
            .build()
            .ok()?;
        let resp = client.get(&url).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.text().ok()
    }
}

/// Build the `legis` renames URL for the committed range `<base_commit>..HEAD`.
/// A commit-ish base contains only `[A-Za-z0-9._/~^-]`, all query-safe, so no
/// percent-encoding is required.
fn legis_renames_url(base_url: &str, base_commit: &str) -> String {
    format!(
        "{}/git/renames?rev_range={}..HEAD",
        base_url.trim_end_matches('/'),
        base_commit
    )
}

/// Parse `legis`'s `GET /git/renames` JSON (`[{old_path, new_path, …}]`) into
/// `(old_path, new_path)` pairs. Entries missing/empty in either path are
/// skipped; a non-array or unparseable body yields an empty list (fail-soft).
/// The shape mirrors `legis`'s `RenameEvidence` dataclass.
fn parse_legis_rename_json(body: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        if let (Some(old), Some(new)) = (
            item.get("old_path").and_then(serde_json::Value::as_str),
            item.get("new_path").and_then(serde_json::Value::as_str),
        ) && !old.is_empty()
            && !new.is_empty()
        {
            out.push((old.to_owned(), new.to_owned()));
        }
    }
    out
}

/// True if `legis` answers a `GET {base_url}/health` with a 2xx inside the
/// timeout. Any build/connect/status failure ⇒ `false` (treated as absent).
fn legis_reachable(base_url: &str) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(LEGIS_HTTP_TIMEOUT)
        .build()
    else {
        return false;
    };
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    client
        .get(&url)
        .send()
        .is_ok_and(|r| r.status().is_success())
}

/// Capability-aware, enrich-only selection of the git-rename supplier (REQ-C-05,
/// loom.md §5). `legis` is chosen ONLY when it is configured, the window is a
/// committed rev-range (`!base.is_empty()`), AND it is reachable; in every other
/// case — `legis` absent/unset/unreachable, or the working-tree window the shell
/// source alone can answer — the shell source is the authority. The
/// `base.is_empty()` check short-circuits *before* any network probe, so the
/// default working-tree path issues no HTTP and is byte-identical to pre-WS9
/// behaviour. See this module's "Window mismatch" note.
pub(crate) fn select_git_rename_source(
    project_root: &Path,
    legis_url: Option<String>,
    base: &str,
    current_locators: Vec<String>,
) -> Box<dyn GitRenameSource> {
    if let Some(url) = legis_url
        && !base.is_empty()
        && legis_reachable(&url)
    {
        return Box::new(LegisGitRenameSource::new(url, current_locators));
    }
    Box::new(ShellGitRenameSource::new(
        project_root.to_path_buf(),
        current_locators,
    ))
}

/// True if `path` is inside a git work tree (used to skip the git probe
/// entirely on non-repo corpora, avoiding a spurious subprocess per run).
pub(crate) fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rename_lines_and_ignores_others() {
        let out = "M\tsrc/keep.py\n\
                   R096\tsrc/auth.py\tsrc/authn.py\n\
                   A\tsrc/new.py\n\
                   R100\tpkg/a/old.py\tpkg/a/new.py\n";
        assert_eq!(
            parse_git_rename_lines(out),
            vec![
                ("src/auth.py".to_owned(), "src/authn.py".to_owned()),
                ("pkg/a/old.py".to_owned(), "pkg/a/new.py".to_owned()),
            ]
        );
    }

    #[test]
    fn path_to_module_handles_python_shapes() {
        assert_eq!(path_to_module("a/b/c.py").as_deref(), Some("a.b.c"));
        assert_eq!(path_to_module("./a/b.py").as_deref(), Some("a.b"));
        assert_eq!(path_to_module("a/b/__init__.py").as_deref(), Some("a.b"));
        assert_eq!(path_to_module("a/b/c.pyi").as_deref(), Some("a.b.c"));
        assert_eq!(path_to_module("README.md"), None);
    }

    #[test]
    fn translates_file_rename_to_locator_renames_for_module_members() {
        let renames = vec![("auth.py".to_owned(), "authn.py".to_owned())];
        let current = vec![
            "python:function:authn.login".to_owned(),
            "python:class:authn.Session".to_owned(),
            "python:module:authn".to_owned(),
            "python:function:other.unrelated".to_owned(),
        ];
        let got = file_renames_to_locator_renames(&renames, &current);
        assert!(got.contains(&GitRename {
            old_locator: "python:function:auth.login".to_owned(),
            new_locator: "python:function:authn.login".to_owned(),
        }));
        assert!(got.contains(&GitRename {
            old_locator: "python:class:auth.Session".to_owned(),
            new_locator: "python:class:authn.Session".to_owned(),
        }));
        // The module entity itself (qualname == new module) maps too.
        assert!(got.contains(&GitRename {
            old_locator: "python:module:auth".to_owned(),
            new_locator: "python:module:authn".to_owned(),
        }));
        // Unrelated module is untouched.
        assert!(!got.iter().any(|r| r.new_locator.contains("other")));
    }

    #[test]
    fn no_locator_rename_when_module_unchanged() {
        // A rename whose path maps to the same module (e.g. case-only on a
        // case-insensitive checkout) yields nothing.
        let renames = vec![("a/b.py".to_owned(), "a/b.py".to_owned())];
        let current = vec!["python:function:a.b.f".to_owned()];
        assert!(file_renames_to_locator_renames(&renames, &current).is_empty());
    }

    // ── WS9: LegisGitRenameSource (REQ-C-05 / SEI §6) ──────────────────────────

    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    /// A one-thread `legis` mock: accepts up to `max_conns` connections, reads
    /// each request's first line, and replies 200 with health JSON for
    /// `/health`, else `renames_json`. Returns `(addr, join_handle)`.
    fn spawn_legis_mock(
        max_conns: usize,
        renames_json: &'static str,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = thread::spawn(move || {
            for _ in 0..max_conns {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut line = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let _ = reader.read_line(&mut line);
                let body = if line.contains("/health") {
                    r#"{"status":"ok","service":"legis"}"#.to_string()
                } else {
                    renames_json.to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (addr, handle)
    }

    #[test]
    fn parses_legis_rename_json_into_path_pairs() {
        let body = r#"[
          {"commit_sha":"abc","old_path":"auth.py","new_path":"authn.py","similarity":96},
          {"commit_sha":"abc","old_path":"","new_path":"x.py","similarity":0},
          {"commit_sha":"def","old_path":"pkg/a/old.py","new_path":"pkg/a/new.py","similarity":100}
        ]"#;
        assert_eq!(
            parse_legis_rename_json(body),
            vec![
                ("auth.py".to_owned(), "authn.py".to_owned()),
                ("pkg/a/old.py".to_owned(), "pkg/a/new.py".to_owned()),
            ]
        );
    }

    #[test]
    fn malformed_legis_body_yields_empty_pairs() {
        assert!(parse_legis_rename_json("not json").is_empty());
        assert!(parse_legis_rename_json(r#"{"not":"an array"}"#).is_empty());
    }

    #[test]
    fn legis_and_shell_translate_identical_file_renames_identically() {
        // The DoD's "identical GitRename output": GIVEN identical file-rename
        // input, both suppliers route through the SAME translation, so they emit
        // byte-identical locator renames. (The observation *windows* differ; the
        // translation does not.)
        let current = vec![
            "python:function:authn.login".to_owned(),
            "python:module:authn".to_owned(),
        ];
        let shell_pairs = parse_git_rename_lines("R096\tauth.py\tauthn.py\n");
        let legis_pairs = parse_legis_rename_json(
            r#"[{"old_path":"auth.py","new_path":"authn.py","similarity":96}]"#,
        );
        assert_eq!(shell_pairs, legis_pairs);
        assert_eq!(
            file_renames_to_locator_renames(&shell_pairs, &current),
            file_renames_to_locator_renames(&legis_pairs, &current),
        );
    }

    #[test]
    fn legis_source_fetches_and_translates_renames_over_http() {
        let body =
            r#"[{"commit_sha":"c","old_path":"auth.py","new_path":"authn.py","similarity":96}]"#;
        let (addr, handle) = spawn_legis_mock(1, body);
        let src = LegisGitRenameSource::new(
            format!("http://{addr}"),
            vec!["python:function:authn.login".to_owned()],
        );
        let got = src.renames_since("base123");
        handle.join().unwrap();
        assert_eq!(
            got,
            vec![GitRename {
                old_locator: "python:function:auth.login".to_owned(),
                new_locator: "python:function:authn.login".to_owned(),
            }]
        );
    }

    #[test]
    fn legis_source_empty_base_returns_empty_without_network() {
        // The working-tree window: legis cannot serve it. The source must return
        // empty WITHOUT a request — proven by pointing at an unroutable address
        // that would hang/timeout if a request were issued.
        let src = LegisGitRenameSource::new(
            "http://203.0.113.1:1".to_owned(),
            vec!["python:function:a.b".to_owned()],
        );
        assert!(src.renames_since("").is_empty());
    }

    #[test]
    fn legis_source_unreachable_degrades_to_empty() {
        // A committed base but a dead port: connect error → empty signal
        // (enrich-only fail-soft), never an error into the run.
        let src = LegisGitRenameSource::new(
            "http://127.0.0.1:1".to_owned(),
            vec!["python:function:a.b".to_owned()],
        );
        assert!(src.renames_since("base").is_empty());
    }

    #[test]
    fn selector_uses_shell_for_working_tree_window_even_when_legis_configured() {
        // base="" → shell source, regardless of legis_url, with NO network probe
        // (the unroutable URL would hang if legis_reachable were called). This is
        // the no-regression guarantee for the default analyze path.
        let tmp = std::env::temp_dir();
        let src = select_git_rename_source(
            &tmp,
            Some("http://203.0.113.1:1".to_owned()),
            "",
            vec!["python:function:a.b".to_owned()],
        );
        // Shell source on a non-repo dir → empty (and the call returned promptly).
        assert!(src.renames_since("").is_empty());
    }

    #[test]
    fn selector_uses_legis_for_committed_base_when_reachable() {
        let body =
            r#"[{"commit_sha":"c","old_path":"auth.py","new_path":"authn.py","similarity":96}]"#;
        // Two connections: the /health probe, then the /git/renames read.
        let (addr, handle) = spawn_legis_mock(2, body);
        let tmp = std::env::temp_dir();
        let src = select_git_rename_source(
            &tmp,
            Some(format!("http://{addr}")),
            "base123",
            vec!["python:module:authn".to_owned()],
        );
        let got = src.renames_since("base123");
        handle.join().unwrap();
        assert_eq!(
            got,
            vec![GitRename {
                old_locator: "python:module:auth".to_owned(),
                new_locator: "python:module:authn".to_owned(),
            }]
        );
    }

    #[test]
    fn selector_falls_back_to_shell_when_legis_absent() {
        // No legis URL, committed base: shell source. Enrich-only — Clarion
        // without legis is unchanged.
        let tmp = std::env::temp_dir();
        let src = select_git_rename_source(&tmp, None, "base123", vec![]);
        assert!(src.renames_since("base123").is_empty());
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git runs");
        assert!(status.status.success(), "git {args:?} failed");
    }

    /// THE WS9 CRUX — enrich-only must never be enrich-NEGATIVE.
    ///
    /// A working-tree (uncommitted) rename is exactly what `legis`'s committed
    /// endpoint CANNOT see — a reachable `legis` returns `[]` here. If the
    /// selector handed the working-tree window to `legis`, the shell-detectable
    /// rename would be LOST and the entity's SEI would orphan instead of carry.
    /// The capability guard (`!base.is_empty()`) keeps the shell source for this
    /// window, so Clarion-with-`legis` is never worse than Clarion-without.
    #[test]
    fn selector_keeps_working_tree_rename_even_when_a_reachable_legis_sees_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("auth.py"), "def login():\n    return 1\n").unwrap();
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-qm", "init"]);
        // Uncommitted rename: visible to `git diff -M HEAD`, invisible to a
        // committed-range query — just as a real pre-commit re-analyze would be.
        run_git(repo, &["mv", "auth.py", "authn.py"]);

        // A *reachable* legis that (correctly) reports no committed rename.
        let (addr, handle) = spawn_legis_mock(2, "[]");
        let src = select_git_rename_source(
            repo,
            Some(format!("http://{addr}")),
            "", // the operative working-tree window
            vec![
                "python:function:authn.login".to_owned(),
                "python:module:authn".to_owned(),
            ],
        );
        let got = src.renames_since("");
        // The mock may receive 0 connections (selector short-circuits before the
        // health probe), so don't join on a guaranteed accept — just drop it.
        drop(handle);

        assert!(
            got.contains(&GitRename {
                old_locator: "python:module:auth".to_owned(),
                new_locator: "python:module:authn".to_owned(),
            }),
            "the shell-detected working-tree rename must survive even with legis configured; got {got:?}"
        );
    }
}
