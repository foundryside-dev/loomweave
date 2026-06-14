//! `loomweave doctor [--fix]` — verify (and optionally repair) the installed
//! agent-orientation surfaces.
//!
//! Several surfaces are checked, each owned by an existing installer module:
//! the `loomweave-workflow` skill pack ([`crate::skill_pack`]), the `SessionStart`
//! hook ([`crate::hooks_settings`]), the Claude Code `.mcp.json` MCP
//! registration ([`crate::mcp_registration`]), the `CLAUDE.md` / `AGENTS.md`
//! agent-orientation block ([`crate::instructions`]), and the local
//! Loomweave/Filigree/Wardline binding files ([`crate::integration_bindings`]).
//! The repair for each is that module's idempotent installer, so
//! `doctor --fix` and `loomweave install` converge to the same state.
//!
//! Output is a per-surface ✓/⚠/✗ report followed by the index snapshot (reused
//! verbatim from the session-start hook). [`run`] returns whether every surface
//! is healthy *after* any repairs; the caller maps an unhealthy result to a
//! non-zero exit so `doctor` is usable as a CI / pre-commit gate.
//!
//! Severity is deliberate. The Weft three-way integration bindings are an
//! *enrich-only* surface (per `docs/suite/weft.md` §5): a Loomweave-solo or
//! Loomweave+Filigree-only project is first-class, so their absence is a
//! **warning** (surfaced, suggests `--fix`) and never a problem that fails the
//! gate. A genuinely broken state — an unparseable config file, a `--fix` repair
//! that errors or does not converge, or a git-tracked runtime `loomweave.db`
//! (which dirties the tree and blocks legis signing, C1 / weft-d822a7de2d) — is
//! a problem that fails the gate.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use loomweave_federation::config::{McpConfig, ProviderSelection, select_provider_with_env};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

use loomweave_storage::StorageError;
use loomweave_storage::schema::{CURRENT_SCHEMA_VERSION, verify_user_version};

use crate::hooks_settings::HookState;
use crate::instructions::InstructionsState;
use crate::integration_bindings::BindingState;
use crate::mcp_registration::McpState;
use crate::skill_pack::SkillPackState;
use crate::{
    hook, hooks_settings, instructions, integration_bindings, mcp_registration, skill_pack,
};

/// Run `loomweave doctor`. Returns `Ok(true)` iff every orientation surface is
/// healthy after any requested repairs.
///
/// # Errors
///
/// Returns an error only if the target directory does not exist or cannot be
/// canonicalised. Per-surface repair failures are reported as problems (they do
/// not abort the run), so one broken surface never hides the others.
pub fn run(path: &Path, fix: bool, json_output: bool) -> Result<bool> {
    if !path.exists() {
        bail!(
            "target directory does not exist: {}. Create it first or pass a valid --path.",
            path.display()
        );
    }
    let project_root = path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise --path {}", path.display()))?;

    if json_output {
        let report = json_report(&project_root, fix);
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(report.ok);
    }

    println!("loomweave doctor{}", if fix { " --fix" } else { "" });

    let mut tally = Tally::default();
    tally += check_skill(&project_root, fix);
    tally += check_hook(&project_root, fix);
    tally += check_mcp(&project_root, fix);
    tally += check_instructions(&project_root, fix);
    tally += check_integration_bindings(&project_root, fix);
    tally += check_db_tracked(&project_root, fix);
    tally += check_gitignore_current(&project_root, fix);
    tally += check_loomweave_dir(&project_root);
    println!("--- llm ---");
    tally += check_llm_provider(&project_root);

    println!("--- index ---");
    for line in hook::snapshot_report(&project_root) {
        println!("{line}");
    }

    if tally.problems == 0 && tally.warnings == 0 {
        println!("All orientation surfaces healthy.");
    } else if tally.problems == 0 {
        let plural = if tally.warnings == 1 { "" } else { "s" };
        println!(
            "{} warning{plural}; no problems (run with --fix to wire optional surfaces).",
            tally.warnings
        );
    } else {
        let suffix = if fix {
            "."
        } else {
            " (run with --fix to repair)."
        };
        let plural = if tally.problems == 1 { "" } else { "s" };
        println!("{} problem{plural} found{suffix}", tally.problems);
    }
    // Only problems fail the gate; warnings are advisory (enrich-only surfaces).
    Ok(tally.problems == 0)
}

#[derive(Debug, Serialize)]
struct DoctorJsonReport {
    ok: bool,
    checks: Vec<DoctorJsonCheck>,
    next_actions: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DoctorJsonCheck {
    id: &'static str,
    status: &'static str,
    fixed: bool,
    message: String,
}

impl DoctorJsonCheck {
    fn ok(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: "ok",
            fixed: false,
            message: message.into(),
        }
    }

    fn warning(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: "warning",
            fixed: false,
            message: message.into(),
        }
    }

    fn problem(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: "problem",
            fixed: false,
            message: message.into(),
        }
    }

    fn fixed(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: "fixed",
            fixed: true,
            message: message.into(),
        }
    }
}

fn json_report(project_root: &Path, fix: bool) -> DoctorJsonReport {
    let mut checks = vec![
        check_loomweave_dir_json(project_root),
        check_index_freshness_json(project_root),
        check_plugin_availability_json(),
        check_skill_json(project_root, fix),
        check_hook_json(project_root, fix),
        check_mcp_json(project_root, fix),
        check_instructions_json(project_root, fix),
        check_http_config_json(project_root),
        check_filigree_url_json(project_root),
        check_llm_provider_json(project_root),
        check_sei_population_json(project_root),
        check_wardline_taint_capability_json(project_root),
        check_mcp_hygiene_json(),
        check_integration_bindings_json(project_root, fix),
        check_db_tracked_json(project_root, fix),
        check_gitignore_current_json(project_root, fix),
    ];
    let next_actions: Vec<String> = checks
        .iter()
        .filter(|check| check.status == "problem" || check.status == "warning")
        .map(|check| match check.id {
            "skill.pack" => {
                "Run `loomweave doctor --fix` or `loomweave install --skills`.".to_owned()
            }
            "hook.session_start" => {
                "Run `loomweave doctor --fix` or `loomweave install --hooks`.".to_owned()
            }
            "instructions.block" => {
                "Run `loomweave doctor --fix` or `loomweave install --instructions`.".to_owned()
            }
            "mcp.registration" | "integration.bindings" => {
                "Run `loomweave doctor --fix`.".to_owned()
            }
            "db.tracked" => {
                "Run `loomweave doctor --fix` or `git rm --cached .weft/loomweave/loomweave.db` \
                 to stop the regenerable index dirtying the tree."
                    .to_owned()
            }
            ".weft/loomweave.schema" => {
                "Run `loomweave install` + `loomweave analyze <project>` to create or \
                 rebuild the index. If the DB is corrupt, remove `.weft/loomweave/loomweave.db` \
                 first."
                    .to_owned()
            }
            "index.freshness" => {
                "Run `loomweave analyze <project>` to refresh the index.".to_owned()
            }
            "llm.provider" => {
                "Run `loomweave config check` to see the effective LLM state; to enable live \
                 summaries set llm_policy.enabled: true + allow_live_provider: true and supply the \
                 provider credential. See \
                 https://github.com/foundryside-dev/loomweave/blob/main/docs/operator/openrouter.md."
                    .to_owned()
            }
            "plugin.availability" => {
                "Install a Loomweave language plugin (the Python plugin ships with `pip install \
                 loomweave`)."
                    .to_owned()
            }
            "gitignore.current" => {
                "Run `loomweave doctor --fix` or `loomweave install` to rewrite \
                 `.weft/loomweave/.gitignore` to the current template."
                    .to_owned()
            }
            _ => format!("Review doctor check `{}`.", check.id),
        })
        .collect();
    let ok = checks.iter().all(|check| check.status != "problem");
    // Keep ordering stable even when future checks append conditionally.
    checks.shrink_to_fit();
    DoctorJsonReport {
        ok,
        checks,
        next_actions,
    }
}

/// Classification of the tracked-index DB health, shared by the text and JSON
/// renderers so they can never diverge.
enum IndexDbHealth {
    /// DB is absent (legitimate intermediate state: install-before-analyze).
    Absent,
    /// DB file is present but could not be opened or probed — corrupt, wrong
    /// format, permission error, or locked.
    Unreadable(String),
    /// DB opens cleanly but its `user_version` is newer than this build.
    FutureSchema { found: u32, current: u32 },
    /// DB opens and its schema version is within range of this build.
    Healthy,
}

/// Classify the index DB at the canonical store path into one of four states.
/// Uses `Connection::open_with_flags` with `SQLITE_OPEN_READ_ONLY` so the
/// check never creates or mutates the DB (unlike `Connection::open`, which
/// creates the file on success).
fn classify_index_db_health(project_root: &Path) -> IndexDbHealth {
    let db_path = loomweave_core::store::db_path(project_root);
    if !db_path.exists() {
        return IndexDbHealth::Absent;
    }
    let conn =
        match Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => conn,
            Err(err) => return IndexDbHealth::Unreadable(err.to_string()),
        };
    // `open_with_flags(READ_ONLY)` lazily succeeds even on a non-SQLite file
    // ("NOT A SQLITE DB"); the corruption only surfaces at first read.
    // `verify_user_version` issues `PRAGMA user_version` — a cheap single-page
    // read that serves double duty as the corruption probe.
    match verify_user_version(&conn) {
        Ok(()) => IndexDbHealth::Healthy,
        Err(StorageError::FutureUserVersion { found, current }) => {
            IndexDbHealth::FutureSchema { found, current }
        }
        Err(err) => IndexDbHealth::Unreadable(err.to_string()),
    }
}

/// JSON-path check for tracked-index DB health.  Expands the former
/// existence-only check with four distinct states: absent (warning),
/// unreadable (problem), future-schema (problem), healthy (ok).
fn check_loomweave_dir_json(project_root: &Path) -> DoctorJsonCheck {
    match classify_index_db_health(project_root) {
        IndexDbHealth::Healthy => DoctorJsonCheck::ok(
            ".weft/loomweave.schema",
            format!(
                ".weft/loomweave store database is present and readable (schema v{CURRENT_SCHEMA_VERSION})"
            ),
        ),
        IndexDbHealth::Absent => DoctorJsonCheck::warning(
            ".weft/loomweave.schema",
            "no index — run `loomweave install` + `loomweave analyze`",
        ),
        IndexDbHealth::Unreadable(detail) => DoctorJsonCheck::problem(
            ".weft/loomweave.schema",
            format!("index exists but is unreadable: {detail}"),
        ),
        IndexDbHealth::FutureSchema { found, current } => DoctorJsonCheck::problem(
            ".weft/loomweave.schema",
            format!(
                "index schema v{found} is newer than this build (current v{current}); \
                 the database was written by a newer Loomweave build"
            ),
        ),
    }
}

/// Text-path twin of [`check_loomweave_dir_json`]: contributes to the `Tally`
/// so problems fail the gate and warnings are surfaced.
fn check_loomweave_dir(project_root: &Path) -> Tally {
    match classify_index_db_health(project_root) {
        IndexDbHealth::Healthy => ok(&format!(
            "index DB present and readable (schema v{CURRENT_SCHEMA_VERSION})"
        )),
        IndexDbHealth::Absent => warn(
            "no index — run `loomweave install` + `loomweave analyze`",
            Some("loomweave install --path . && loomweave analyze ."),
        ),
        IndexDbHealth::Unreadable(detail) => problem(
            &format!("index exists but is unreadable: {detail}"),
            Some(
                "check permissions; if corrupt, remove .weft/loomweave/loomweave.db and re-analyze",
            ),
        ),
        IndexDbHealth::FutureSchema { found, current } => problem(
            &format!(
                "index schema v{found} is newer than this build (current v{current}); \
                 the database was written by a newer Loomweave build"
            ),
            Some("upgrade loomweave to match or exceed the schema version of the database"),
        ),
    }
}

/// Whether the regenerable runtime DB is committed to git.
///
/// `loomweave.db` mutates on every `analyze`/`scan`; tracking it leaves a
/// permanently-dirty work tree that blocks legis signing (C1 / weft-d822a7de2d).
/// ADR-005 was reversed (`b7a1b30`) so a fresh `install` gitignores it, but a
/// template change cannot untrack an already-committed db — this is the detector
/// for that residual.
#[derive(Debug, PartialEq, Eq)]
enum DbTrackedState {
    /// Healthy: the db is not in the git index (untracked, ignored, absent, the
    /// store lives outside the repo, or this is not a git work tree).
    Untracked,
    /// The db is committed/staged — dirties the tree and blocks signing.
    Tracked,
}

/// Ask git whether `<store_dir>/loomweave.db` is tracked. `ls-files
/// --error-unmatch` exits 0 only when the pathspec matches a tracked file, so a
/// non-success exit (untracked, ignored, absent, outside the repo, not a repo,
/// or git missing) all fold to [`DbTrackedState::Untracked`] — nothing to fix.
fn db_tracked_state(project_root: &Path) -> DbTrackedState {
    let db = loomweave_core::store::db_path(project_root);
    let Ok(rel) = db.strip_prefix(project_root) else {
        // Store dir is outside the repo — this repo cannot be tracking it.
        return DbTrackedState::Untracked;
    };
    let tracked = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(rel)
        .output()
        .is_ok_and(|out| out.status.success());
    if tracked {
        DbTrackedState::Tracked
    } else {
        DbTrackedState::Untracked
    }
}

/// `--fix` self-heal: `git rm --cached` the runtime db (and its WAL/SHM
/// sidecars), removing them from the index while keeping the working-tree files.
/// `--ignore-unmatch` makes the sidecars optional.
fn git_untrack_db(project_root: &Path) -> Result<()> {
    let store = loomweave_core::store::store_dir(project_root);
    let rel = store
        .strip_prefix(project_root)
        .context("store dir is outside the project root; cannot git rm --cached")?;
    let status = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rm", "--cached", "-q", "--ignore-unmatch", "--"])
        .arg(rel.join("loomweave.db"))
        .arg(rel.join("loomweave.db-wal"))
        .arg(rel.join("loomweave.db-shm"))
        .status()
        .context("run git rm --cached")?;
    if !status.success() {
        bail!("git rm --cached exited with {status}");
    }
    Ok(())
}

/// JSON-path twin of [`check_db_tracked`].
fn check_db_tracked_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match db_tracked_state(project_root) {
        DbTrackedState::Untracked => {
            DoctorJsonCheck::ok("db.tracked", "runtime loomweave.db is not git-tracked")
        }
        DbTrackedState::Tracked => {
            let what = "loomweave.db is git-tracked — it mutates on every analyze/scan, dirtying \
                        the work tree and blocking legis signing (ADR-005 reversed)";
            if !fix {
                return DoctorJsonCheck::problem("db.tracked", what);
            }
            match git_untrack_db(project_root) {
                Ok(()) if db_tracked_state(project_root) == DbTrackedState::Untracked => {
                    DoctorJsonCheck::fixed(
                        "db.tracked",
                        format!("{what} — untracked (git rm --cached)"),
                    )
                }
                Ok(()) => DoctorJsonCheck::problem(
                    "db.tracked",
                    format!("{what} — repair did not converge"),
                ),
                Err(err) => {
                    DoctorJsonCheck::problem("db.tracked", format!("{what} — repair failed: {err}"))
                }
            }
        }
    }
}

/// Health of the Loomweave-owned `.weft/loomweave/.gitignore` relative to the
/// canonical template. When the template evolves (e.g. C1 reversed ADR-005 to
/// *ignore* `loomweave.db`), a project initialised by an older binary keeps a
/// stale file: `doctor --fix` must detect and rewrite it, not green over it.
#[derive(Debug, PartialEq, Eq)]
enum GitignoreState {
    /// On-disk bytes match the current template (or there is no store dir to
    /// manage — that gap is owned by `check_loomweave_dir`).
    Current,
    /// The store dir exists but `.gitignore` is absent.
    Missing,
    /// `.gitignore` exists but its bytes differ from the current template.
    Stale,
}

/// Classify `<store_dir>/.gitignore` against [`crate::install::GITIGNORE_CONTENTS`].
/// A full-file byte compare is correct because the file is wholly Loomweave-owned
/// (written verbatim into the private store dir) — there is no user content to
/// merge. When the store dir is absent there is nothing to manage (that gap is
/// `check_loomweave_dir`'s), so report [`GitignoreState::Current`].
fn gitignore_state(project_root: &Path) -> GitignoreState {
    let store = loomweave_core::store::store_dir(project_root);
    if !store.is_dir() {
        return GitignoreState::Current;
    }
    match fs::read_to_string(store.join(".gitignore")) {
        Ok(contents) if contents == crate::install::GITIGNORE_CONTENTS => GitignoreState::Current,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => GitignoreState::Missing,
        // Drifted bytes, or unreadable for any other reason: rewrite under `--fix`.
        Ok(_) | Err(_) => GitignoreState::Stale,
    }
}

/// `--fix` repair: rewrite the Loomweave-owned `.gitignore` to the canonical
/// template via the shared installer writer, so `install` and `doctor --fix`
/// converge on byte-identical output.
fn repair_gitignore(project_root: &Path) -> Result<()> {
    crate::install::write_gitignore(&loomweave_core::store::store_dir(project_root))
}

/// JSON-path twin of [`check_gitignore_current`].
fn check_gitignore_current_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match gitignore_state(project_root) {
        GitignoreState::Current => DoctorJsonCheck::ok(
            "gitignore.current",
            "loomweave .gitignore matches the current template",
        ),
        state => {
            let what = gitignore_what(&state);
            if !fix {
                // Loomweave-owned regenerable file: drift is advisory, never a
                // gate failure (mirrors the enrich-only surfaces).
                return DoctorJsonCheck::warning("gitignore.current", what);
            }
            match repair_gitignore(project_root) {
                Ok(()) if gitignore_state(project_root) == GitignoreState::Current => {
                    DoctorJsonCheck::fixed("gitignore.current", format!("{what} — fixed"))
                }
                Ok(()) => DoctorJsonCheck::warning(
                    "gitignore.current",
                    format!("{what} — repair did not converge"),
                ),
                Err(err) => DoctorJsonCheck::warning(
                    "gitignore.current",
                    format!("{what} — repair failed: {err}"),
                ),
            }
        }
    }
}

/// Human-readable description of a non-`Current` gitignore state.
fn gitignore_what(state: &GitignoreState) -> &'static str {
    match state {
        GitignoreState::Missing => "loomweave .gitignore is missing",
        GitignoreState::Stale => {
            "loomweave .gitignore is stale (does not match the current template)"
        }
        GitignoreState::Current => unreachable!("Current is handled before gitignore_what"),
    }
}

fn check_index_freshness_json(project_root: &Path) -> DoctorJsonCheck {
    let lines = hook::snapshot_report(project_root);
    if lines
        .iter()
        .any(|line| line.to_ascii_lowercase().contains("may be stale"))
    {
        DoctorJsonCheck::warning("index.freshness", lines.join("\n"))
    } else {
        DoctorJsonCheck::ok("index.freshness", lines.join("\n"))
    }
}

fn check_plugin_availability_json() -> DoctorJsonCheck {
    // Use the same discovery path as `loomweave analyze` (`$PATH` *and* the running
    // binary's directory), so doctor agrees with analyze about which plugins are
    // visible. A manual `$PATH`-only scan here would report a co-located
    // PyPI/venv-installed plugin as missing even though analyze can drive it.
    let mut ids = Vec::new();
    let mut errs = Vec::new();
    for result in loomweave_core::plugin::discover() {
        match result {
            Ok(plugin) => ids.push(plugin.manifest.plugin.plugin_id),
            Err(err) => errs.push(err.to_string()),
        }
    }

    if !ids.is_empty() {
        let plural = if ids.len() == 1 { "" } else { "s" };
        DoctorJsonCheck::ok(
            "plugin.availability",
            format!(
                "{} language plugin{plural} discovered: {}",
                ids.len(),
                ids.join(", ")
            ),
        )
    } else if !errs.is_empty() {
        DoctorJsonCheck::warning(
            "plugin.availability",
            format!("plugin discovery reported errors: {}", errs.join("; ")),
        )
    } else {
        DoctorJsonCheck::warning(
            "plugin.availability",
            "no loomweave language plugin discovered (on PATH or alongside the loomweave binary)",
        )
    }
}

fn check_skill_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match skill_pack::skill_pack_state(project_root) {
        SkillPackState::UpToDate => {
            DoctorJsonCheck::ok("skill.pack", "skill pack up to date (.claude + .agents)")
        }
        state => {
            let what = match state {
                SkillPackState::Missing => "missing or incomplete",
                SkillPackState::Drifted => "drifted from the bundled copy",
                SkillPackState::UpToDate => unreachable!(),
            };
            if !fix {
                return DoctorJsonCheck::problem("skill.pack", format!("skill pack {what}"));
            }
            match skill_pack::install_skill_pack(project_root) {
                Ok(_) if skill_pack::skill_pack_state(project_root) == SkillPackState::UpToDate => {
                    DoctorJsonCheck::fixed(
                        "skill.pack",
                        format!("skill pack {what}; reinstalled .claude + .agents"),
                    )
                }
                Ok(_) => DoctorJsonCheck::problem(
                    "skill.pack",
                    format!("skill pack {what}; repair did not converge"),
                ),
                Err(err) => DoctorJsonCheck::problem(
                    "skill.pack",
                    format!("skill pack {what}; repair failed: {err}"),
                ),
            }
        }
    }
}

fn check_hook_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match hooks_settings::session_start_hook_state(project_root) {
        HookState::Present => DoctorJsonCheck::ok(
            "hook.session_start",
            "SessionStart hook present (.claude/settings.json)",
        ),
        HookState::Unparseable => DoctorJsonCheck::problem(
            "hook.session_start",
            ".claude/settings.json is not parseable JSON",
        ),
        state => {
            let what = match state {
                HookState::Missing => "SessionStart hook missing",
                HookState::Stale => "SessionStart hook stale (wrong project or old form)",
                HookState::Present | HookState::Unparseable => unreachable!(),
            };
            if !fix {
                return DoctorJsonCheck::problem("hook.session_start", what);
            }
            match hooks_settings::install_session_start_hook(project_root) {
                Ok(_)
                    if hooks_settings::session_start_hook_state(project_root)
                        == HookState::Present =>
                {
                    DoctorJsonCheck::fixed("hook.session_start", format!("{what}; fixed"))
                }
                Ok(_) => DoctorJsonCheck::problem(
                    "hook.session_start",
                    format!("{what}; repair did not converge"),
                ),
                Err(err) => DoctorJsonCheck::problem(
                    "hook.session_start",
                    format!("{what}; repair failed: {err}"),
                ),
            }
        }
    }
}

fn check_mcp_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match mcp_registration::mcp_entry_state(project_root) {
        McpState::Present => DoctorJsonCheck::ok(
            "mcp.registration",
            ".mcp.json loomweave serve entry present",
        ),
        McpState::Unparseable => {
            DoctorJsonCheck::problem("mcp.registration", ".mcp.json is not parseable JSON")
        }
        McpState::UntrustedCommand => {
            let cmd = mcp_registration::loomweave_entry_command(project_root)
                .unwrap_or_else(|| "<unknown>".to_owned());
            let what = format!(
                ".mcp.json loomweave entry uses an unrecognized command {cmd:?} (not the loomweave \
                 executable); doctor will not auto-replace it"
            );
            if !fix {
                return DoctorJsonCheck::problem("mcp.registration", what);
            }
            // `--fix` repairs args but never the command; the entry stays
            // UntrustedCommand and is surfaced as an advisory warning.
            let _ = mcp_registration::install_mcp_entry(project_root);
            DoctorJsonCheck::warning(
                "mcp.registration",
                format!("{what}; left the command in place for you to review"),
            )
        }
        state => {
            let what = match state {
                McpState::Missing => ".mcp.json has no loomweave serve entry",
                McpState::Stale => ".mcp.json loomweave entry is stale or not runtime-discovered",
                McpState::Present | McpState::Unparseable | McpState::UntrustedCommand => {
                    unreachable!()
                }
            };
            if !fix {
                return DoctorJsonCheck::problem("mcp.registration", what);
            }
            match mcp_registration::install_mcp_entry(project_root) {
                Ok(_) if mcp_registration::mcp_entry_state(project_root) == McpState::Present => {
                    DoctorJsonCheck::fixed(
                        "mcp.registration",
                        format!("{what}; merged loomweave serve entry"),
                    )
                }
                Ok(_) => DoctorJsonCheck::problem(
                    "mcp.registration",
                    format!("{what}; repair did not converge"),
                ),
                Err(err) => DoctorJsonCheck::problem(
                    "mcp.registration",
                    format!("{what}; repair failed: {err}"),
                ),
            }
        }
    }
}

fn check_http_config_json(project_root: &Path) -> DoctorJsonCheck {
    let Some(config) = read_loomweave_yaml(project_root) else {
        return DoctorJsonCheck::warning("http.config", "loomweave.yaml is absent or unparseable");
    };
    let enabled = config
        .get("serve")
        .and_then(|serve| serve.get("http"))
        .and_then(|http| http.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true);
    if !enabled {
        return DoctorJsonCheck::warning(
            "http.config",
            "HTTP serve config is disabled or incomplete",
        );
    }
    // ADR-044: prefer the live published port over the (now usually absent)
    // static bind. A running serve publishes .weft/loomweave/ephemeral.port.
    let resolution =
        loomweave_federation::loomweave_url::resolve_loomweave_url(None, project_root, |name| {
            std::env::var(name).ok()
        });
    if let Some(url) = resolution.resolved_url {
        if resolution.source == loomweave_federation::loomweave_url::SOURCE_EPHEMERAL_PORT
            && !http_health_reachable(&url)
        {
            return DoctorJsonCheck::warning(
                "http.config",
                format!(
                    "stale HTTP read-API port metadata in .weft/loomweave/ephemeral.port: \
                     {url}/health is not reachable; start `loomweave serve` or ignore this \
                     persisted port when .mcp.json launches the stdio runtime"
                ),
            );
        }
        return DoctorJsonCheck::ok(
            "http.config",
            format!("HTTP read API published on {url} ({})", resolution.source),
        );
    }
    let bind = config
        .get("serve")
        .and_then(|serve| serve.get("http"))
        .and_then(|http| http.get("bind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if bind.trim().is_empty() {
        DoctorJsonCheck::ok(
            "http.config",
            "HTTP enabled; read-API port auto-selected and published to .weft/loomweave/ephemeral.port while serving",
        )
    } else {
        DoctorJsonCheck::ok(
            "http.config",
            format!("HTTP configured on {bind} (auto-published while serving)"),
        )
    }
}

fn http_health_reachable(base_url: &str) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(250))
        .build()
    else {
        return false;
    };
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    client
        .get(url)
        .send()
        .is_ok_and(|response| response.status().is_success())
}

fn check_filigree_url_json(project_root: &Path) -> DoctorJsonCheck {
    let Some(config) = read_loomweave_yaml(project_root) else {
        return DoctorJsonCheck::warning("filigree.url", "loomweave.yaml is absent or unparseable");
    };
    let enabled = config
        .get("integrations")
        .and_then(|integrations| integrations.get("filigree"))
        .and_then(|filigree| filigree.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true);
    let url = config
        .get("integrations")
        .and_then(|integrations| integrations.get("filigree"))
        .and_then(|filigree| filigree.get("base_url"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if enabled && !url.trim().is_empty() {
        DoctorJsonCheck::ok("filigree.url", format!("Filigree URL configured as {url}"))
    } else {
        DoctorJsonCheck::warning(
            "filigree.url",
            "Filigree integration URL is disabled or missing",
        )
    }
}

/// Severity classes for the LLM-config check, shared by the text and JSON
/// paths so they never diverge.
enum LlmPosture {
    /// loomweave.yaml failed to parse/validate — serve would refuse to start.
    Broken(String),
    /// A live provider is configured but unusable (e.g. missing API key).
    Unusable(String),
    /// Healthy: a concise effective-state line, plus any advisory warnings.
    Ok {
        summary: String,
        warnings: Vec<String>,
    },
}

/// Load loomweave.yaml *typed* (so `deny_unknown_fields` + `validate()` run),
/// resolve the effective provider, and classify the posture. This is the file most
/// likely to be hand-edited wrong (agent-first-feedback §2.4); an absent file is
/// fine (built-in defaults → LLM disabled).
fn llm_posture(project_root: &Path) -> LlmPosture {
    let config_path = project_root.join("loomweave.yaml");
    let config = if config_path.exists() {
        match McpConfig::from_path(&config_path) {
            Ok(config) => config,
            Err(err) => return LlmPosture::Broken(format!("loomweave.yaml: {err}")),
        }
    } else {
        McpConfig::default()
    };

    let warnings = config.llm_warnings();
    let provider = config.llm.provider.as_str();
    match select_provider_with_env(&config, |name| std::env::var(name).ok()) {
        Err(err) => LlmPosture::Unusable(format!("live provider selected but unusable: {err}")),
        Ok(sel) => {
            let live = matches!(
                sel,
                ProviderSelection::OpenRouter { .. }
                    | ProviderSelection::CodexCli
                    | ProviderSelection::ClaudeCli
            );
            let summary = if live {
                format!(
                    "LLM live: provider={provider}, model={}",
                    config.llm.effective_model_label()
                )
            } else {
                format!("LLM not live (provider={provider}); entity_summary_get is cache-only")
            };
            LlmPosture::Ok { summary, warnings }
        }
    }
}

fn check_llm_provider_json(project_root: &Path) -> DoctorJsonCheck {
    match llm_posture(project_root) {
        LlmPosture::Broken(msg) | LlmPosture::Unusable(msg) => {
            DoctorJsonCheck::problem("llm.provider", msg)
        }
        LlmPosture::Ok { summary, warnings } if warnings.is_empty() => {
            DoctorJsonCheck::ok("llm.provider", summary)
        }
        LlmPosture::Ok { summary, warnings } => DoctorJsonCheck::warning(
            "llm.provider",
            format!("{summary}; {}", warnings.join("; ")),
        ),
    }
}

fn check_sei_population_json(project_root: &Path) -> DoctorJsonCheck {
    let db = loomweave_core::store::db_path(project_root);
    let Ok(conn) = Connection::open(&db) else {
        return DoctorJsonCheck::warning("sei.population", "loomweave.db is absent or unreadable");
    };
    let count: rusqlite::Result<i64> = conn.query_row(
        "SELECT COUNT(*) FROM sei_bindings WHERE status = 'alive'",
        [],
        |row| row.get(0),
    );
    match count {
        Ok(count) if count > 0 => {
            DoctorJsonCheck::ok("sei.population", format!("{count} alive SEI bindings"))
        }
        Ok(_) => DoctorJsonCheck::warning("sei.population", "no alive SEI bindings found"),
        Err(err) => DoctorJsonCheck::warning(
            "sei.population",
            format!("SEI population could not be checked: {err}"),
        ),
    }
}

fn check_wardline_taint_capability_json(project_root: &Path) -> DoctorJsonCheck {
    let Some(config) = read_loomweave_yaml(project_root) else {
        return DoctorJsonCheck::warning(
            "wardline.taint_store",
            "loomweave.yaml is absent or unparseable",
        );
    };
    if config
        .get("serve")
        .and_then(|serve| serve.get("http"))
        .and_then(|http| http.get("wardline_taint_write"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        DoctorJsonCheck::ok(
            "wardline.taint_store",
            "Wardline taint-store write is enabled",
        )
    } else {
        DoctorJsonCheck::warning(
            "wardline.taint_store",
            "Wardline taint-store write is not enabled",
        )
    }
}

fn check_mcp_hygiene_json() -> DoctorJsonCheck {
    DoctorJsonCheck::ok(
        "mcp.stdout_stderr_hygiene",
        "operator diagnostics are configured for stderr; MCP stdout remains protocol-only",
    )
}

fn check_instructions_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match instructions::instructions_state(project_root) {
        InstructionsState::UpToDate => DoctorJsonCheck::ok(
            "instructions.block",
            "agent-orientation block present in CLAUDE.md + AGENTS.md",
        ),
        InstructionsState::Missing => {
            let what = "agent-orientation block missing from CLAUDE.md / AGENTS.md";
            if !fix {
                // Optional surface: absence is a warning, not a gate failure.
                return DoctorJsonCheck::warning("instructions.block", what);
            }
            repair_instructions_json(project_root, what)
        }
        state => {
            let what = match state {
                InstructionsState::Drifted => {
                    "agent-orientation block drifted from the bundled copy"
                }
                InstructionsState::Malformed => {
                    "agent-orientation block malformed (dangling loomweave marker)"
                }
                InstructionsState::Duplicated => {
                    "agent-orientation block duplicated (stale split-brain copy)"
                }
                InstructionsState::UpToDate | InstructionsState::Missing => unreachable!(),
            };
            if !fix {
                return DoctorJsonCheck::problem("instructions.block", what);
            }
            repair_instructions_json(project_root, what)
        }
    }
}

fn repair_instructions_json(project_root: &Path, what: &str) -> DoctorJsonCheck {
    match instructions::install_instructions(project_root) {
        Ok(_) if instructions::instructions_state(project_root) == InstructionsState::UpToDate => {
            DoctorJsonCheck::fixed("instructions.block", format!("{what}; fixed"))
        }
        // A symlinked target was skipped (never write through a symlink) and the
        // block is still not current — name the file and the hand remedy instead
        // of an opaque non-convergence.
        Ok(report) if !report.skipped_symlinks.is_empty() => DoctorJsonCheck::problem(
            "instructions.block",
            format!(
                "{what}; repair skipped symlinked target(s) {} — replace the link with a \
                 regular file by hand, then re-run",
                joined_paths(&report.skipped_symlinks)
            ),
        ),
        Ok(_) => DoctorJsonCheck::problem(
            "instructions.block",
            format!("{what}; repair did not converge"),
        ),
        Err(err) => DoctorJsonCheck::problem(
            "instructions.block",
            format!("{what}; repair failed: {err}"),
        ),
    }
}

/// Comma-join paths for a one-line diagnostic.
fn joined_paths(paths: &[std::path::PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn check_integration_bindings_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match integration_bindings::binding_state(project_root) {
        BindingState::Present => DoctorJsonCheck::ok(
            "integration.bindings",
            "three-way integration bindings present (Loomweave + Filigree + Wardline)",
        ),
        BindingState::Unparseable => DoctorJsonCheck::problem(
            "integration.bindings",
            "three-way integration bindings are not parseable",
        ),
        BindingState::MissingOrStale => {
            let what = "three-way integration bindings missing or stale";
            if !fix {
                // Enrich-only surface: absence is a warning, not a gate failure.
                return DoctorJsonCheck::warning("integration.bindings", what);
            }
            match integration_bindings::install_bindings(project_root) {
                Ok(_)
                    if integration_bindings::binding_state(project_root)
                        == BindingState::Present =>
                {
                    DoctorJsonCheck::fixed("integration.bindings", format!("{what}; fixed"))
                }
                Ok(_) => DoctorJsonCheck::problem(
                    "integration.bindings",
                    format!("{what}; repair did not converge"),
                ),
                Err(err) => DoctorJsonCheck::problem(
                    "integration.bindings",
                    format!("{what}; repair failed: {err}"),
                ),
            }
        }
    }
}

fn read_loomweave_yaml(project_root: &Path) -> Option<Value> {
    let raw = fs::read_to_string(project_root.join("loomweave.yaml")).ok()?;
    serde_norway::from_str(&raw).ok()
}

/// Per-check severity tally for the text report. Only `problems` fail the gate;
/// `warnings` are surfaced but advisory (enrich-only / optional surfaces).
#[derive(Default)]
struct Tally {
    problems: usize,
    warnings: usize,
}

impl std::ops::AddAssign for Tally {
    fn add_assign(&mut self, rhs: Self) {
        self.problems += rhs.problems;
        self.warnings += rhs.warnings;
    }
}

/// Print one healthy line; contributes nothing to the tally.
fn ok(line: &str) -> Tally {
    println!("  ✓ {line}");
    Tally::default()
}

/// Print one warning line (plus an optional fix hint). Surfaced but advisory —
/// does not fail the gate.
fn warn(line: &str, fix_hint: Option<&str>) -> Tally {
    println!("  ⚠ {line}");
    if let Some(hint) = fix_hint {
        println!("      fix: {hint}");
    }
    Tally {
        problems: 0,
        warnings: 1,
    }
}

/// Print one problem line (plus an optional fix hint). Fails the gate.
fn problem(line: &str, fix_hint: Option<&str>) -> Tally {
    println!("  ✗ {line}");
    if let Some(hint) = fix_hint {
        println!("      fix: {hint}");
    }
    Tally {
        problems: 1,
        warnings: 0,
    }
}

/// Text-path twin of [`check_llm_provider_json`]: report the effective LLM
/// state so a human running `loomweave doctor` sees why summaries are (or are
/// not) live, instead of having to read source (agent-first-feedback §2.4).
fn check_llm_provider(project_root: &Path) -> Tally {
    match llm_posture(project_root) {
        LlmPosture::Broken(msg) | LlmPosture::Unusable(msg) => problem(
            &msg,
            Some(
                "loomweave config check  (docs: \
                 https://github.com/foundryside-dev/loomweave/blob/main/docs/operator/openrouter.md)",
            ),
        ),
        LlmPosture::Ok { summary, warnings } => {
            let tally = ok(&summary);
            if warnings.is_empty() {
                tally
            } else {
                let mut tally = tally;
                for warning in &warnings {
                    tally += warn(warning, Some("loomweave config check"));
                }
                tally
            }
        }
    }
}

fn check_skill(project_root: &Path, fix: bool) -> Tally {
    match skill_pack::skill_pack_state(project_root) {
        SkillPackState::UpToDate => ok("skill pack up to date (.claude + .agents)"),
        state => {
            let what = match state {
                SkillPackState::Missing => "missing or incomplete",
                SkillPackState::Drifted => "drifted from the bundled copy",
                SkillPackState::UpToDate => unreachable!(),
            };
            if !fix {
                return problem(
                    &format!("skill pack {what}"),
                    Some("loomweave install --skills"),
                );
            }
            match skill_pack::install_skill_pack(project_root) {
                Ok(_) if skill_pack::skill_pack_state(project_root) == SkillPackState::UpToDate => {
                    ok(&format!(
                        "skill pack {what} — fixed (reinstalled .claude + .agents)"
                    ))
                }
                Ok(_) => problem(
                    &format!("skill pack {what} — repair did not converge"),
                    None,
                ),
                Err(err) => problem(&format!("skill pack {what} — repair failed: {err}"), None),
            }
        }
    }
}

fn check_hook(project_root: &Path, fix: bool) -> Tally {
    match hooks_settings::session_start_hook_state(project_root) {
        HookState::Present => ok("SessionStart hook present (.claude/settings.json)"),
        // An unparseable settings.json is never auto-repaired — the merge
        // refuses to clobber hand-authored JSON — so report it regardless of
        // --fix and keep it counted.
        HookState::Unparseable => problem(
            ".claude/settings.json is not parseable JSON — fix it by hand, then re-run",
            None,
        ),
        state => {
            let what = match state {
                HookState::Missing => "SessionStart hook missing",
                HookState::Stale => "SessionStart hook stale (wrong project or old form)",
                HookState::Present | HookState::Unparseable => unreachable!(),
            };
            if !fix {
                return problem(what, Some("loomweave install --hooks"));
            }
            match hooks_settings::install_session_start_hook(project_root) {
                Ok(_)
                    if hooks_settings::session_start_hook_state(project_root)
                        == HookState::Present =>
                {
                    ok(&format!("{what} — fixed"))
                }
                Ok(_) => problem(&format!("{what} — repair did not converge"), None),
                Err(err) => problem(&format!("{what} — repair failed: {err}"), None),
            }
        }
    }
}

fn check_mcp(project_root: &Path, fix: bool) -> Tally {
    match mcp_registration::mcp_entry_state(project_root) {
        McpState::Present => ok(".mcp.json loomweave serve entry present"),
        McpState::Unparseable => problem(
            ".mcp.json is not parseable JSON — fix it by hand, then re-run",
            None,
        ),
        McpState::UntrustedCommand => {
            let cmd = mcp_registration::loomweave_entry_command(project_root)
                .unwrap_or_else(|| "<unknown>".to_owned());
            let what = format!(
                ".mcp.json loomweave entry uses an unrecognized command {cmd:?} (not the loomweave \
                 executable); doctor will not auto-replace it"
            );
            if !fix {
                return problem(
                    &what,
                    Some(
                        "if this is a deliberate wrapper, leave it; otherwise set `command` to \
                         `loomweave` or remove the entry — `--fix` will not clobber it",
                    ),
                );
            }
            // `--fix` corrects args/type/env but never the command, so the entry
            // stays UntrustedCommand. Warn (advisory) so the operator
            // adjudicates the wrapper rather than CI silently passing it.
            let _ = mcp_registration::install_mcp_entry(project_root);
            warn(
                &format!("{what}; left the command in place for you to review"),
                None,
            )
        }
        state => {
            let what = match state {
                McpState::Missing => ".mcp.json has no loomweave serve entry",
                McpState::Stale => ".mcp.json loomweave entry is stale or not runtime-discovered",
                McpState::Present | McpState::Unparseable | McpState::UntrustedCommand => {
                    unreachable!()
                }
            };
            if !fix {
                return problem(
                    what,
                    Some("loomweave doctor --fix  (or add the entry to .mcp.json manually)"),
                );
            }
            match mcp_registration::install_mcp_entry(project_root) {
                Ok(_) if mcp_registration::mcp_entry_state(project_root) == McpState::Present => {
                    ok(&format!("{what} — fixed (merged loomweave serve entry)"))
                }
                Ok(_) => problem(&format!("{what} — repair did not converge"), None),
                Err(err) => problem(&format!("{what} — repair failed: {err}"), None),
            }
        }
    }
}

fn check_instructions(project_root: &Path, fix: bool) -> Tally {
    match instructions::instructions_state(project_root) {
        InstructionsState::UpToDate => {
            ok("agent-orientation block present in CLAUDE.md + AGENTS.md")
        }
        // Optional surface: the same guidance ships via the MCP preamble and the
        // loomweave-workflow skill, so a missing block is advisory — never a gate
        // failure. Mirrors the integration-bindings severity model.
        InstructionsState::Missing => {
            let what = "agent-orientation block missing from CLAUDE.md / AGENTS.md";
            if !fix {
                return warn(what, Some("loomweave install --instructions"));
            }
            repair_instructions(project_root, what)
        }
        // Drifted / Malformed fail the gate: a stale or dangling block is a
        // genuinely broken state. The repair is safe because it rewrites only
        // Loomweave's own marker span.
        state => {
            let what = match state {
                InstructionsState::Drifted => {
                    "agent-orientation block drifted from the bundled copy"
                }
                InstructionsState::Malformed => {
                    "agent-orientation block malformed (dangling loomweave marker)"
                }
                InstructionsState::Duplicated => {
                    "agent-orientation block duplicated (stale split-brain copy)"
                }
                InstructionsState::UpToDate | InstructionsState::Missing => unreachable!(),
            };
            if !fix {
                return problem(what, Some("loomweave doctor --fix"));
            }
            repair_instructions(project_root, what)
        }
    }
}

/// Shared `--fix` repair for the instructions block: re-inject, then re-classify
/// to confirm convergence.
fn repair_instructions(project_root: &Path, what: &str) -> Tally {
    match instructions::install_instructions(project_root) {
        Ok(_) if instructions::instructions_state(project_root) == InstructionsState::UpToDate => {
            ok(&format!("{what} — fixed"))
        }
        // Text-path twin of the JSON branch: a skipped symlinked target is an
        // actionable hand-remedy, not an opaque non-convergence.
        Ok(report) if !report.skipped_symlinks.is_empty() => problem(
            &format!(
                "{what} — repair skipped symlinked target(s) {}",
                joined_paths(&report.skipped_symlinks)
            ),
            Some("replace the symlink with a regular file, then re-run loomweave doctor --fix"),
        ),
        Ok(_) => problem(&format!("{what} — repair did not converge"), None),
        Err(err) => problem(&format!("{what} — repair failed: {err}"), None),
    }
}

/// Text-path twin of [`check_db_tracked_json`]: surface a git-tracked runtime db
/// (the C1 analyze→sign blocker) instead of greening over it, and self-heal it
/// under `--fix`.
fn check_db_tracked(project_root: &Path, fix: bool) -> Tally {
    match db_tracked_state(project_root) {
        DbTrackedState::Untracked => ok("runtime loomweave.db is not git-tracked"),
        DbTrackedState::Tracked => {
            let what = "loomweave.db is git-tracked — it mutates on every analyze/scan, dirtying \
                        the work tree and blocking legis signing";
            if !fix {
                // A tracked regenerable db blocks the analyze→govern→sign loop —
                // a genuinely broken state, so it fails the gate (unlike the
                // enrich-only binding/instruction warnings).
                return problem(
                    what,
                    Some(
                        "git rm --cached .weft/loomweave/loomweave.db  (or loomweave doctor --fix)",
                    ),
                );
            }
            match git_untrack_db(project_root) {
                Ok(()) if db_tracked_state(project_root) == DbTrackedState::Untracked => {
                    ok(&format!("{what} — fixed (git rm --cached)"))
                }
                Ok(()) => problem(&format!("{what} — repair did not converge"), None),
                Err(err) => problem(&format!("{what} — repair failed: {err}"), None),
            }
        }
    }
}

/// Text-path twin of [`check_gitignore_current_json`]: warn (never gate-fail)
/// when the Loomweave-owned `.gitignore` is stale or missing, and rewrite it to
/// the canonical template under `--fix`.
fn check_gitignore_current(project_root: &Path, fix: bool) -> Tally {
    match gitignore_state(project_root) {
        GitignoreState::Current => ok("loomweave .gitignore matches the current template"),
        state => {
            let what = gitignore_what(&state);
            if !fix {
                return warn(what, Some("loomweave doctor --fix (or loomweave install)"));
            }
            match repair_gitignore(project_root) {
                Ok(()) if gitignore_state(project_root) == GitignoreState::Current => {
                    ok(&format!("{what} — fixed"))
                }
                // Keep repair failures as warnings: a regenerable, Loomweave-owned
                // file must never fail the gate. Surface the cause.
                Ok(()) => warn(&format!("{what} — repair did not converge"), None),
                Err(err) => warn(&format!("{what} — repair failed: {err}"), None),
            }
        }
    }
}

fn check_integration_bindings(project_root: &Path, fix: bool) -> Tally {
    match integration_bindings::binding_state(project_root) {
        BindingState::Present => {
            ok("three-way integration bindings present (Loomweave + Filigree + Wardline)")
        }
        BindingState::Unparseable => problem(
            "three-way integration bindings are not parseable — fix config files by hand, then re-run",
            None,
        ),
        BindingState::MissingOrStale => {
            let what = "three-way integration bindings missing or stale";
            if !fix {
                // Enrich-only surface: absence is a warning, not a gate failure.
                return warn(what, Some("loomweave doctor --fix"));
            }
            match integration_bindings::install_bindings(project_root) {
                Ok(_)
                    if integration_bindings::binding_state(project_root)
                        == BindingState::Present =>
                {
                    ok(&format!("{what} — fixed"))
                }
                Ok(_) => problem(&format!("{what} — repair did not converge"), None),
                Err(err) => problem(&format!("{what} — repair failed: {err}"), None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run_git(repo: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git runs")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(repo: &Path) {
        run_git(repo, &["init", "-q"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
    }

    /// Materialise the runtime DB at the canonical store path
    /// (`<root>/.weft/loomweave/loomweave.db`).
    fn write_db(root: &Path) -> std::path::PathBuf {
        let db = loomweave_core::store::db_path(root);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"SQLite format 3\0").unwrap();
        db
    }

    #[test]
    fn db_tracked_state_is_untracked_when_db_is_not_added() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        write_db(root); // present on disk, never `git add`-ed
        assert_eq!(db_tracked_state(root), DbTrackedState::Untracked);
    }

    #[test]
    fn db_tracked_state_is_tracked_when_db_is_git_added() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        write_db(root);
        run_git(root, &["add", "-f", ".weft/loomweave/loomweave.db"]);
        assert_eq!(db_tracked_state(root), DbTrackedState::Tracked);
    }

    #[test]
    fn db_tracked_state_is_untracked_outside_a_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        write_db(dir.path());
        assert_eq!(db_tracked_state(dir.path()), DbTrackedState::Untracked);
    }

    /// Materialise `<root>/.weft/loomweave/.gitignore` with the given bytes,
    /// returning its path.
    fn write_gitignore_bytes(root: &Path, bytes: &str) -> std::path::PathBuf {
        let store = loomweave_core::store::store_dir(root);
        std::fs::create_dir_all(&store).unwrap();
        let path = store.join(".gitignore");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// The pre-C1 template header (ADR-005 tracked-db model) — representative of
    /// the stale file a project initialised by an older binary still carries.
    const STALE_GITIGNORE: &str =
        "# Tracked (committed): loomweave.db, config.json\nephemeral.port\n";

    #[test]
    fn gitignore_state_current_when_bytes_match_template() {
        let dir = tempfile::tempdir().unwrap();
        write_gitignore_bytes(dir.path(), crate::install::GITIGNORE_CONTENTS);
        assert_eq!(gitignore_state(dir.path()), GitignoreState::Current);
    }

    #[test]
    fn gitignore_state_stale_when_bytes_differ() {
        let dir = tempfile::tempdir().unwrap();
        write_gitignore_bytes(dir.path(), STALE_GITIGNORE);
        assert_eq!(gitignore_state(dir.path()), GitignoreState::Stale);
    }

    #[test]
    fn gitignore_state_missing_when_store_exists_without_file() {
        let dir = tempfile::tempdir().unwrap();
        // Store dir present (e.g. via db init) but no .gitignore.
        write_db(dir.path());
        assert_eq!(gitignore_state(dir.path()), GitignoreState::Missing);
    }

    #[test]
    fn doctor_warns_then_fixes_stale_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let path = write_gitignore_bytes(root, STALE_GITIGNORE);

        // Plain doctor: surface the drift as a WARNING (never a gate failure).
        let diag = check_gitignore_current(root, false);
        assert_eq!(diag.warnings, 1, "stale .gitignore must warn");
        assert_eq!(
            diag.problems, 0,
            ".gitignore drift must never fail the gate"
        );

        // doctor --fix: rewrite to exactly the template, then re-verify clean.
        let fixed = check_gitignore_current(root, true);
        assert_eq!(fixed.problems, 0);
        assert_eq!(fixed.warnings, 0, "repaired .gitignore must verify clean");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            crate::install::GITIGNORE_CONTENTS,
            ".gitignore not rewritten to the canonical template"
        );
        assert_eq!(gitignore_state(root), GitignoreState::Current);
    }

    #[test]
    fn doctor_fix_repairs_missing_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_db(root); // store dir exists, no .gitignore
        assert_eq!(gitignore_state(root), GitignoreState::Missing);

        let fixed = check_gitignore_current(root, true);
        assert_eq!(fixed.problems, 0);
        assert_eq!(fixed.warnings, 0);
        let written =
            std::fs::read_to_string(loomweave_core::store::store_dir(root).join(".gitignore"))
                .unwrap();
        assert_eq!(written, crate::install::GITIGNORE_CONTENTS);
    }

    #[test]
    fn doctor_fix_is_noop_on_current_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let path = write_gitignore_bytes(root, crate::install::GITIGNORE_CONTENTS);

        // Pin an old mtime; a rewrite (temp+rename) would replace the inode and
        // bump it, so an unchanged mtime proves the current file was not churned.
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let t = check_gitignore_current(root, true);
        assert_eq!(t.problems, 0);
        assert_eq!(t.warnings, 0);
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(after, old, "current .gitignore must not be rewritten");
    }

    /// A representative co-resident Filigree block (shape taken from the repo's
    /// own AGENTS.md) for the doctor-entry-point C-4 coverage.
    const DOCTOR_FILIGREE_BLOCK: &str = "<!-- filigree:instructions:v3.0.0rc2:98d5c5f2 -->\n\
## Filigree Issue Tracker\n\
\n\
filigree tracks tasks for this project.\n\
<!-- /filigree:instructions -->\n";

    /// C-4 (e) via the `doctor --fix` entry point: a stale duplicate own block
    /// must be FLAGGED as a problem by `doctor` (no `--fix`) and COLLAPSED to one
    /// by `doctor --fix`. Covers the doctor surface (`check_instructions`), the
    /// twin of the `install --instructions` coverage in `instructions.rs`.
    #[test]
    fn doctor_flags_then_fixes_duplicate_own_block() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        instructions::install_instructions(root).unwrap(); // seed both files clean
        let claude = root.join("CLAUDE.md");
        let block = std::fs::read_to_string(&claude).unwrap();
        // Two well-formed copies of the (already-current) block.
        std::fs::write(&claude, format!("{block}\n{block}")).unwrap();

        // doctor (diagnose only) must flag it as a problem, not green.
        let diag = check_instructions(root, false);
        assert_eq!(diag.problems, 1, "duplicate must be flagged as a problem");

        // doctor --fix must repair it to a healthy single block.
        let fixed = check_instructions(root, true);
        assert_eq!(
            fixed.problems, 0,
            "doctor --fix must collapse the duplicate"
        );
        assert_eq!(
            instructions::instructions_state(root),
            InstructionsState::UpToDate
        );
    }

    /// C-4 (c) via the `doctor --fix` entry point: a Filigree block sandwiched
    /// between a stale Loomweave start and Loomweave's real end must survive the
    /// repair (the foreign-fence-bounded rewrite never crosses it).
    #[test]
    fn doctor_fix_preserves_sandwiched_foreign_block() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        instructions::install_instructions(root).unwrap();
        let claude = root.join("CLAUDE.md");
        let sandwiched = format!(
            "<!-- loomweave:instructions:v0:deadbeef -->\n\
             stale loomweave body\n\
             {DOCTOR_FILIGREE_BLOCK}\
             <!-- /loomweave:instructions -->\n"
        );
        std::fs::write(&claude, &sandwiched).unwrap();

        let fixed = check_instructions(root, true);
        let after = std::fs::read_to_string(&claude).unwrap();
        assert!(
            after.contains(DOCTOR_FILIGREE_BLOCK),
            "doctor --fix swallowed the sandwiched filigree block:\n{after}"
        );
        assert_eq!(
            fixed.problems, 0,
            "doctor --fix must converge on the sandwiched-foreign case"
        );
    }

    #[test]
    fn git_untrack_db_unstages_the_tracked_db_but_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        let db = write_db(root);
        run_git(root, &["add", "-f", ".weft/loomweave/loomweave.db"]);
        assert_eq!(db_tracked_state(root), DbTrackedState::Tracked);

        git_untrack_db(root).expect("untrack succeeds");

        assert_eq!(db_tracked_state(root), DbTrackedState::Untracked);
        assert!(
            db.exists(),
            "git rm --cached must keep the working-tree file"
        );
    }
}
