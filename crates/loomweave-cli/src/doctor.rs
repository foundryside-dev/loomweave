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
// The git call sites use `hardened_git_command` (clarion-9202f4acec); this is
// for spawning our OWN binary in `repair_classifier_analysis`, which is not git
// and must not inherit the git hardening wrapper.
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use loomweave_core::{ClassifierCoverage, PluginCoverageStatus};
use loomweave_federation::config::{McpConfig, ProviderSelection, select_provider_with_env};
use loomweave_storage::{
    ExternalSqliteCompatibility, ExternalSqliteCompatibilityStatus, LatestClassifierCoverage,
    ResolutionCoverageSummary, external_sqlite_compatibility, latest_classifier_coverage,
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;

use loomweave_core::hardened_git_command;
use loomweave_storage::StorageError;
use loomweave_storage::schema::{
    CURRENT_SCHEMA_VERSION, reject_unmigrated_for_read, verify_user_version,
};

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
    tally += check_git_hooks(&project_root, fix);
    tally += check_mcp(&project_root, fix);
    tally += check_instructions(&project_root, fix);
    tally += check_integration_bindings(&project_root, fix);
    tally += check_db_tracked(&project_root, fix);
    tally += check_gitignore_current(&project_root, fix);
    tally += check_loomweave_dir(&project_root);
    tally += emit_json_check_text(&check_external_sqlite_json(&project_root));
    let instance_id = check_http_instance_id_json(&project_root, fix);
    let (enumeration, tags) = check_classifier_json(&project_root, fix);
    tally += emit_json_check_text(&enumeration);
    tally += emit_json_check_text(&tags);
    tally += emit_json_check_text(&check_http_authentication_json(&project_root));
    tally += emit_json_check_text(&instance_id);
    tally += check_index_integrity(&project_root, fix);
    tally += emit_json_check_text(&check_resolution_coverage_json(&project_root, fix));
    tally += emit_json_check_text(&check_runs_json(&project_root, fix));
    if let Some(check) = check_worktree_stores_json(&project_root) {
        tally += emit_json_check_text(&check);
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
    #[serde(skip)]
    next_action: Option<String>,
}

impl DoctorJsonCheck {
    fn ok(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: "ok",
            fixed: false,
            message: message.into(),
            details: None,
            next_action: None,
        }
    }

    fn warning(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: "warning",
            fixed: false,
            message: message.into(),
            details: None,
            next_action: None,
        }
    }

    fn problem(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: "problem",
            fixed: false,
            message: message.into(),
            details: None,
            next_action: None,
        }
    }

    fn fixed(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: "fixed",
            fixed: true,
            message: message.into(),
            details: None,
            next_action: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    fn with_next_action(mut self, next_action: impl Into<String>) -> Self {
        self.next_action = Some(next_action.into());
        self
    }

    fn mark_fixed(mut self, message: impl Into<String>) -> Self {
        self.status = "fixed";
        self.fixed = true;
        self.message = message.into();
        self.next_action = None;
        self
    }
}

fn json_report(project_root: &Path, fix: bool) -> DoctorJsonReport {
    // Materialise project identity before an automatic analysis so the repaired
    // catalogue and every serving surface converge on one project UUID.
    let instance_id = check_http_instance_id_json(project_root, fix);
    let (classifier_enumeration, classifier_tags) = check_classifier_json(project_root, fix);
    let mut checks = vec![
        check_loomweave_dir_json(project_root),
        check_external_sqlite_json(project_root),
        classifier_enumeration,
        classifier_tags,
        check_index_integrity_json(project_root, fix),
        check_index_freshness_json(project_root),
        check_plugin_availability_json(),
        check_skill_json(project_root, fix),
        check_hook_json(project_root, fix),
        check_git_hooks_json(project_root, fix),
        check_mcp_json(project_root, fix),
        check_instructions_json(project_root, fix),
        check_http_config_json(project_root),
        check_http_authentication_json(project_root),
        instance_id,
        check_filigree_url_json(project_root),
        check_llm_provider_json(project_root),
        check_sei_population_json(project_root),
        check_resolution_coverage_json(project_root, fix),
        check_runs_json(project_root, fix),
        check_wardline_taint_capability_json(project_root),
        check_mcp_hygiene_json(),
        check_integration_bindings_json(project_root, fix),
        check_db_tracked_json(project_root, fix),
        check_gitignore_current_json(project_root, fix),
    ];
    if let Some(check) = check_worktree_stores_json(project_root) {
        checks.push(check);
    }
    let next_actions: Vec<String> = checks
        .iter()
        .filter(|check| check.status == "problem" || check.status == "warning")
        .map(|check| {
            check
                .next_action
                .clone()
                .unwrap_or_else(|| default_next_action(check.id))
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

fn default_next_action(id: &str) -> String {
    match id {
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
            "index.resolution_coverage" => {
                "Run `loomweave analyze <project>`: transient-degraded files are re-dispatched \
                 automatically. Run `loomweave doctor --fix --path <project>` to reset the \
                 re-dispatch budget of files that exhausted it (retry once no other analyze / \
                 doctor --fix holds the lock), then analyze again. Content- or \
                 environment-determined ones (syntax error / site cap / `interpreter_unpinned` \
                 — set `LOOMWEAVE_PYTHON_INTERPRETER` or create `.venv`) need the source fixed \
                 or a `--no-incremental` pass once the resolver is healthy."
                    .to_owned()
            }
            "index.runs" => {
                "Run `loomweave doctor --fix --path <project>` to mark abandoned `running` \
                 runs failed (only when no live analyze holds the lock)."
                    .to_owned()
            }
            "index.freshness" => {
                "Run `loomweave doctor --fix --path <project>` or `loomweave analyze <project>` to refresh the index.".to_owned()
            }
            "federation.sqlite_compatibility" => {
                "Rebuild the local index with this Loomweave version before allowing an external SQLite reader.".to_owned()
            }
            "classifier.enumeration" | "classifier.tags" => {
                "Run `loomweave doctor --fix --path <project>` or `loomweave analyze <project>`, then inspect the latest analysis-run diagnostics.".to_owned()
            }
            "http.authentication" => {
                "Set the configured HTTP authentication secret, or disable/reconfigure the HTTP read API.".to_owned()
            }
            "http.instance_id" => {
                "Run `loomweave doctor --fix --path <project>` to materialise a missing identity; inspect and remove a malformed identity before retrying.".to_owned()
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
            _ => format!("Review doctor check `{id}`."),
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
    /// DB opens but `user_version = 0`: no Loomweave schema was ever applied —
    /// an empty/auto-created file or an externally-produced `SQLite` file. The
    /// read path (`reject_unmigrated_for_read`) refuses it, so `serve` would too
    /// (review #8); doctor must not call this Healthy.
    Unmigrated,
    /// DB opens and its schema version is within range of this build.
    Healthy,
}

/// Classify the index DB at the canonical store path into one of four states.
/// Thin wrapper over [`classify_index_db_health_at`] — kept for the many
/// existing call sites that only ever check the current invocation's own
/// (unrouted) `db_path(project_root)`. worktree-index Task 7's additive
/// worktree-store report calls the `_at` form directly with each isolated
/// store's own resolved db path.
fn classify_index_db_health(project_root: &Path) -> IndexDbHealth {
    classify_index_db_health_at(&loomweave_core::store::db_path(project_root))
}

/// Classify the index DB at `db_path` into one of four states. Uses
/// `Connection::open_with_flags` with `SQLITE_OPEN_READ_ONLY` so the check
/// never creates or mutates the DB (unlike `Connection::open`, which creates
/// the file on success).
fn classify_index_db_health_at(db_path: &Path) -> IndexDbHealth {
    if !db_path.exists() {
        return IndexDbHealth::Absent;
    }
    let conn =
        match Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => conn,
            Err(err) => return IndexDbHealth::Unreadable(err.to_string()),
        };
    // `open_with_flags(READ_ONLY)` lazily succeeds even on a non-SQLite file
    // ("NOT A SQLITE DB"); the corruption only surfaces at first read.
    // `verify_user_version` issues `PRAGMA user_version` — a cheap single-page
    // read that serves double duty as the corruption probe.
    if let Err(err) = verify_user_version(&conn) {
        return match err {
            StorageError::FutureUserVersion { found, current } => {
                IndexDbHealth::FutureSchema { found, current }
            }
            other => IndexDbHealth::Unreadable(other.to_string()),
        };
    }
    // `verify_user_version` deliberately accepts `user_version = 0`, but the
    // read-open path rejects it (`reject_unmigrated_for_read`): a header-valid
    // empty/external SQLite file is not a Loomweave index, and `serve` refuses
    // it. Mirror that gate here so doctor never reports Healthy a DB `serve`
    // would turn away (review #8 / read-vs-doctor parity).
    match reject_unmigrated_for_read(&conn) {
        Ok(()) => IndexDbHealth::Healthy,
        Err(StorageError::UnmigratedIndex) => IndexDbHealth::Unmigrated,
        Err(err) => IndexDbHealth::Unreadable(err.to_string()),
    }
}

/// worktree-index Task 7: additive, read-only report on every
/// worktree-isolated store under `<repository-store>/worktrees/`, reusing
/// [`classify_index_db_health_at`] per store. `--fix` gains no new repair
/// power here in this task — there is no `fix` parameter to wire one to;
/// repairing a worktree-isolated store's index is out of scope.
///
/// `project_root` may be the primary checkout, a standalone project, or a
/// linked worktree itself: resolved via `WorktreeContext` so this reports
/// the SAME `<repository-store>/worktrees/` regardless of which checkout
/// `doctor` was invoked from — unlike every other check in this module,
/// which stays root-derived from the literal `--path` (see the worktree-index
/// Task 7 report for the classification rationale). Returns `None` when
/// there is nothing to report: the worktree context could not be resolved
/// (non-UTF-8 path — vanishingly rare) or no worktree store has ever been
/// created.
fn check_worktree_stores_json(project_root: &Path) -> Option<DoctorJsonCheck> {
    const ID: &str = "worktree_stores";

    let ctx = loomweave_core::worktree::WorktreeContext::resolve(project_root).ok()?;
    let worktrees_dir = ctx
        .repository_store
        .join(loomweave_cli::worktree::store::WORKTREES_DIR_NAME);
    let entries = fs::read_dir(&worktrees_dir).ok()?;
    let mut stable_ids: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| loomweave_cli::worktree::confine::matches_worktree_store_grammar(name))
        .collect();
    if stable_ids.is_empty() {
        return None;
    }
    stable_ids.sort();

    let mut stores = Vec::with_capacity(stable_ids.len());
    let mut worst = "ok";
    for stable_id in &stable_ids {
        let db_path = worktrees_dir.join(stable_id).join("loomweave.db");
        let health = classify_index_db_health_at(&db_path);
        let (status, detail) = worktree_store_health_status_detail(&health);
        if severity_rank(status) > severity_rank(worst) {
            worst = status;
        }
        stores.push(serde_json::json!({
            "stable_id": stable_id,
            "status": status,
            "detail": detail,
        }));
    }

    let message = format!(
        "{} worktree-isolated store(s) found under .weft/loomweave/worktrees/",
        stable_ids.len()
    );
    let check = match worst {
        "problem" => DoctorJsonCheck::problem(ID, message),
        "warning" => DoctorJsonCheck::warning(ID, message),
        _ => DoctorJsonCheck::ok(ID, message),
    }
    .with_details(serde_json::json!({ "stores": stores }));
    Some(if worst == "ok" {
        check
    } else {
        check.with_next_action(
            "Read-only report: `doctor --fix` does not repair worktree-isolated stores. \
             Investigate the named store(s) directly, or rebuild one with \
             `loomweave worktree analyze -- <target>`.",
        )
    })
}

/// `(status, detail)` for one worktree-isolated store's [`IndexDbHealth`],
/// used only by [`check_worktree_stores_json`]'s per-store report — kept
/// separate from [`check_loomweave_dir_json`]'s primary-store wording so
/// neither can accidentally drift the other's exact message text (both are
/// asserted verbatim by existing tests).
fn worktree_store_health_status_detail(health: &IndexDbHealth) -> (&'static str, String) {
    match health {
        IndexDbHealth::Healthy => ("ok", "healthy".to_owned()),
        IndexDbHealth::Absent => (
            "warning",
            "store directory exists but loomweave.db is absent".to_owned(),
        ),
        IndexDbHealth::Unreadable(detail) => ("problem", format!("unreadable: {detail}")),
        IndexDbHealth::Unmigrated => ("problem", "unmigrated (user_version=0)".to_owned()),
        IndexDbHealth::FutureSchema { found, current } => (
            "problem",
            format!("schema v{found} is newer than this build (current v{current})"),
        ),
    }
}

/// Ordering for picking the worst status across several per-store verdicts.
fn severity_rank(status: &str) -> u8 {
    match status {
        "problem" => 2,
        "warning" => 1,
        _ => 0,
    }
}

/// `.weft/loomweave.schema`'s check is meaningless — worse, actively
/// misleading — for a linked worktree: `db_path(project_root)` re-derives a
/// path *inside the worktree checkout* that `loomweave worktree analyze`
/// never populates by design (worktree-index isolation), so it always reads
/// `Absent` there regardless of how healthy the worktree's real, isolated
/// store is. Left unrouted, that produced the fix-loop finding-3
/// self-contradiction: `doctor` printed a hint recommending `loomweave
/// install` and `loomweave analyze` — which, followed literally, would
/// CREATE the forbidden local store — immediately above a `--- index ---`
/// section reporting healthy counts from the correct isolated store one
/// line below.
///
/// Linked worktrees therefore classify the exact
/// `WorktreeContext::store_paths` database directly. The aggregate
/// `worktree_stores` report may be absent when the namespace cannot be
/// enumerated, or healthy because a different worktree's store is healthy;
/// neither can stand in for the current checkout's database. Resolution
/// failure falls through to the legacy root-derived check.
fn linked_worktree_context(
    project_root: &Path,
) -> Option<loomweave_core::worktree::WorktreeContext> {
    loomweave_core::worktree::WorktreeContext::resolve(project_root)
        .ok()
        .filter(|ctx| ctx.kind == loomweave_core::worktree::WorktreeKind::Linked)
}

fn store_paths_for_doctor(project_root: &Path) -> (loomweave_core::worktree::StorePaths, bool) {
    let resolved = loomweave_core::worktree::WorktreeContext::resolve(project_root).ok();
    let is_linked = resolved
        .as_ref()
        .is_some_and(|ctx| ctx.kind == loomweave_core::worktree::WorktreeKind::Linked);
    let paths = resolved.map_or_else(
        || loomweave_core::worktree::StorePaths::under(&project_root.join(".weft/loomweave")),
        |ctx| ctx.store_paths,
    );
    (paths, is_linked)
}

/// JSON-path check for tracked-index DB health.  Expands the former
/// existence-only check with five distinct states: absent (warning),
/// unreadable (problem), unmigrated (problem), future-schema (problem),
/// healthy (ok). Linked worktrees use [`linked_worktree_context`] to classify
/// the current isolated database rather than a checkout-local decoy.
fn check_loomweave_dir_json(project_root: &Path) -> DoctorJsonCheck {
    if let Some(ctx) = linked_worktree_context(project_root) {
        let db_path = &ctx.store_paths.db;
        return match classify_index_db_health_at(db_path) {
            IndexDbHealth::Healthy => DoctorJsonCheck::ok(
                ".weft/loomweave.schema",
                format!(
                    "current linked worktree isolated database is healthy at {} (schema v{CURRENT_SCHEMA_VERSION})",
                    db_path.display()
                ),
            ),
            IndexDbHealth::Absent => DoctorJsonCheck::warning(
                ".weft/loomweave.schema",
                format!(
                    "current linked worktree isolated loomweave.db is absent at {}; run \
                     `loomweave worktree analyze -- <target>`",
                    db_path.display()
                ),
            ),
            IndexDbHealth::Unreadable(detail) => DoctorJsonCheck::problem(
                ".weft/loomweave.schema",
                format!(
                    "current linked worktree isolated database at {} is unreadable: {detail}",
                    db_path.display()
                ),
            ),
            IndexDbHealth::Unmigrated => DoctorJsonCheck::problem(
                ".weft/loomweave.schema",
                format!(
                    "current linked worktree isolated database at {} is unmigrated \
                     (user_version=0); rebuild it with `loomweave worktree analyze -- <target>`",
                    db_path.display()
                ),
            ),
            IndexDbHealth::FutureSchema { found, current } => DoctorJsonCheck::problem(
                ".weft/loomweave.schema",
                format!(
                    "current linked worktree isolated database at {} has schema v{found}, \
                     newer than this build (current v{current})",
                    db_path.display()
                ),
            ),
        };
    }
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
        IndexDbHealth::Unmigrated => DoctorJsonCheck::problem(
            ".weft/loomweave.schema",
            "index file is present but unmigrated (user_version=0); not a Loomweave index — \
             `serve` will refuse it. Run `loomweave install` + `loomweave analyze`"
                .to_owned(),
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
/// so problems fail the gate and warnings are surfaced. See
/// [`linked_worktree_context`] for the linked-worktree routing.
fn check_loomweave_dir(project_root: &Path) -> Tally {
    if let Some(ctx) = linked_worktree_context(project_root) {
        let db_path = &ctx.store_paths.db;
        return match classify_index_db_health_at(db_path) {
            IndexDbHealth::Healthy => ok(&format!(
                "current linked worktree isolated database is healthy at {} (schema v{CURRENT_SCHEMA_VERSION})",
                db_path.display()
            )),
            IndexDbHealth::Absent => warn(
                &format!(
                    "current linked worktree isolated loomweave.db is absent at {}",
                    db_path.display()
                ),
                Some("loomweave worktree analyze -- <target>"),
            ),
            IndexDbHealth::Unreadable(detail) => problem(
                &format!(
                    "current linked worktree isolated database at {} is unreadable: {detail}",
                    db_path.display()
                ),
                Some("check permissions; rebuild only after inspecting the isolated store"),
            ),
            IndexDbHealth::Unmigrated => problem(
                &format!(
                    "current linked worktree isolated database at {} is unmigrated (user_version=0)",
                    db_path.display()
                ),
                Some("loomweave worktree analyze -- <target>"),
            ),
            IndexDbHealth::FutureSchema { found, current } => problem(
                &format!(
                    "current linked worktree isolated database at {} has schema v{found}, newer than this build (current v{current})",
                    db_path.display()
                ),
                Some("upgrade loomweave to match or exceed the database schema version"),
            ),
        };
    }
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
        IndexDbHealth::Unmigrated => problem(
            "index file is present but unmigrated (user_version=0); not a Loomweave index — \
             `serve` will refuse it",
            Some("loomweave install --path . && loomweave analyze ."),
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

/// External-consumer compatibility is stricter than Loomweave's own migration
/// acceptance. Keep it as a separate stable check so operators can distinguish
/// "Loomweave can open this" from "a federation peer may safely read it".
fn check_external_sqlite_json(project_root: &Path) -> DoctorJsonCheck {
    const ID: &str = "federation.sqlite_compatibility";
    let db_path = loomweave_core::store::db_path(project_root);
    if !db_path.exists() {
        return DoctorJsonCheck::warning(ID, "external SQLite catalogue is absent")
            .with_details(serde_json::json!({"database_present": false}));
    }
    let conn = match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(err) => {
            return DoctorJsonCheck::problem(
                ID,
                format!("external SQLite compatibility probe could not open the index: {err}"),
            )
            .with_details(serde_json::json!({
                "database_present": true,
                "probe_error": err.to_string(),
            }));
        }
    };
    let report = match external_sqlite_compatibility(&conn) {
        Ok(report) => report,
        Err(err) => {
            return DoctorJsonCheck::problem(
                ID,
                format!("external SQLite compatibility probe failed: {err}"),
            )
            .with_details(serde_json::json!({
                "database_present": true,
                "probe_error": err.to_string(),
            }));
        }
    };
    let details = serde_json::json!({
        "database_present": true,
        "schema": report.schema,
        "compatibility": report.status,
        "reason": report.reason,
        "application_id": report.application_id,
        "user_version": report.user_version,
        "min_user_version": report.min_user_version,
        "max_user_version": report.max_user_version,
        "legacy_application_id": report.legacy_application_id,
        "missing_surface": report.missing_surface,
    });
    match report.status {
        ExternalSqliteCompatibilityStatus::Compatible if report.legacy_application_id => {
            DoctorJsonCheck::warning(
                ID,
                format!(
                    "external SQLite schema v{} is compatible via legacy application_id=0; structure does not authenticate provenance",
                    report.user_version
                ),
            )
            .with_details(details)
        }
        ExternalSqliteCompatibilityStatus::Compatible => DoctorJsonCheck::ok(
            ID,
            format!(
                "external SQLite schema {} is compatible at user_version={}",
                report.schema, report.user_version
            ),
        )
        .with_details(details),
        ExternalSqliteCompatibilityStatus::OlderSupported => DoctorJsonCheck::warning(
            ID,
            format!(
                "external SQLite user_version={} is older but supported{}",
                report.user_version,
                if report.legacy_application_id {
                    " via legacy application_id=0"
                } else {
                    ""
                }
            ),
        )
        .with_details(details),
        ExternalSqliteCompatibilityStatus::Incompatible => DoctorJsonCheck::problem(
            ID,
            format!(
                "external SQLite catalogue is incompatible: {:?} (application_id={}, user_version={})",
                report.reason, report.application_id, report.user_version
            ),
        )
        .with_details(details),
    }
}

enum ExternalSqliteReadGateError {
    Probe(String),
    Incompatible(ExternalSqliteCompatibility),
}

impl ExternalSqliteReadGateError {
    fn message(&self) -> String {
        match self {
            Self::Probe(detail) => format!("external SQLite compatibility probe failed: {detail}"),
            Self::Incompatible(report) => format!(
                "external SQLite catalogue is incompatible: {:?} (application_id={}, user_version={})",
                report.reason, report.application_id, report.user_version
            ),
        }
    }

    fn details(&self) -> Value {
        match self {
            Self::Probe(detail) => serde_json::json!({"probe_error": detail}),
            Self::Incompatible(report) => serde_json::json!({
                "schema": report.schema,
                "compatibility": report.status,
                "reason": report.reason,
                "application_id": report.application_id,
                "user_version": report.user_version,
                "missing_surface": report.missing_surface,
            }),
        }
    }
}

/// Gate a catalogue connection before any contract-specific row query. Header
/// checks and required-surface introspection happen on this same read-only
/// connection; callers may query `runs`/`sei_bindings` only after `Ok(())`.
fn validate_external_sqlite_read_gate(
    conn: &Connection,
) -> std::result::Result<(), ExternalSqliteReadGateError> {
    let report = external_sqlite_compatibility(conn)
        .map_err(|err| ExternalSqliteReadGateError::Probe(err.to_string()))?;
    if report.status == ExternalSqliteCompatibilityStatus::Incompatible {
        return Err(ExternalSqliteReadGateError::Incompatible(report));
    }
    Ok(())
}

const ENUMERATION_ID: &str = "classifier.enumeration";
const TAGS_ID: &str = "classifier.tags";

fn check_classifier_json(project_root: &Path, fix: bool) -> (DoctorJsonCheck, DoctorJsonCheck) {
    let db_path = loomweave_core::store::db_path(project_root);
    if !db_path.exists() {
        let details = serde_json::json!({
            "available": false,
            "run_id": null,
            "run_status": null,
            "reason": "external SQLite catalogue is absent",
        });
        return (
            DoctorJsonCheck::warning(
                ENUMERATION_ID,
                "classifier enumeration is unavailable because the index is absent",
            )
            .with_details(details.clone()),
            DoctorJsonCheck::warning(
                TAGS_ID,
                "active classifier declarations are unavailable because the index is absent",
            )
            .with_details(details),
        );
    }
    let conn = match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(err) => return classifier_probe_problem(&err.to_string()),
    };
    if let Err(err) = validate_external_sqlite_read_gate(&conn) {
        return classifier_external_gate_problem(&err);
    }
    let latest = match latest_classifier_coverage(&conn) {
        Ok(latest) => latest,
        Err(err) => return classifier_probe_problem(&err.to_string()),
    };
    let Some(coverage) = latest.coverage() else {
        let reason = latest
            .reason()
            .unwrap_or("classifier coverage is unavailable");
        let details = serde_json::json!({
            "available": false,
            "run_id": latest.run_id(),
            "run_status": latest.run_status(),
            "reason": reason,
        });
        if let Some(not_applicable) = classifier_not_applicable(&latest) {
            return not_applicable;
        }
        if fix && latest.run_status().is_none() && latest.run_id().is_none() {
            return match repair_classifier_analysis(project_root) {
                Ok(()) => {
                    let (enumeration, tags) = check_classifier_json(project_root, false);
                    (
                        mark_classifier_repair(enumeration, "classifier enumeration regenerated"),
                        mark_classifier_repair(tags, "active classifier declarations regenerated"),
                    )
                }
                Err(err) => {
                    let details = serde_json::json!({
                        "available": false,
                        "run_id": null,
                        "run_status": null,
                        "reason": reason,
                        "repair_error": err.to_string(),
                    });
                    (
                        DoctorJsonCheck::problem(
                            ENUMERATION_ID,
                            format!("automatic classifier analysis repair failed: {err}"),
                        )
                        .with_details(details.clone()),
                        DoctorJsonCheck::problem(
                            TAGS_ID,
                            format!("automatic classifier declaration repair failed: {err}"),
                        )
                        .with_details(details),
                    )
                }
            };
        }
        let (status, message_prefix) = match latest.run_status() {
            None if latest.run_id().is_none() => {
                ("warning", "no classifier analysis run exists yet")
            }
            Some("running") => ("warning", "latest classifier analysis is not completed"),
            _ => ("problem", "latest classifier evidence is unusable"),
        };
        let enumeration = classifier_unavailable_check(
            ENUMERATION_ID,
            status,
            format!("{message_prefix}: {reason}"),
            details.clone(),
        );
        let tags = classifier_unavailable_check(
            TAGS_ID,
            status,
            format!("active classifier declarations are unavailable: {reason}"),
            details,
        );
        return (enumeration, tags);
    };

    (
        classifier_enumeration_json(&latest, coverage),
        classifier_tags_json(&latest, coverage),
    )
}

fn repair_classifier_analysis(project_root: &Path) -> Result<()> {
    let executable = std::env::current_exe().context("resolve current Loomweave executable")?;
    let output = Command::new(executable)
        .arg("analyze")
        .arg(project_root)
        .output()
        .context("start automatic Loomweave analysis repair")?;
    if output.status.success() {
        return Ok(());
    }
    // `output()` captures both streams, so without replaying them the operator
    // sees only an exit status for a repair this command ran on their behalf --
    // the plugin error, bad path or lock contention that actually explains the
    // failure is discarded. Prefer stderr and fall back to stdout, since a
    // failing `analyze` may report through either.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let diagnostics = match (stderr.trim(), stdout.trim()) {
        ("", "") => String::from("no output"),
        ("", out) => tail_diagnostics(out),
        (err, _) => tail_diagnostics(err),
    };
    bail!(
        "`loomweave analyze` exited with {}: {diagnostics}",
        output.status
    )
}

/// Keep the last [`MAX_REPAIR_DIAGNOSTIC_LINES`] lines of a failed repair's
/// output. The cause is almost always at the end, and the whole string is
/// embedded in a JSON check detail that should stay readable.
fn tail_diagnostics(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(MAX_REPAIR_DIAGNOSTIC_LINES);
    let tail = lines[start..].join("; ");
    if start > 0 {
        format!("(last {MAX_REPAIR_DIAGNOSTIC_LINES} lines) {tail}")
    } else {
        tail
    }
}

fn mark_classifier_repair(check: DoctorJsonCheck, message: &str) -> DoctorJsonCheck {
    if check.status == "ok" {
        let healthy_message = check.message.clone();
        check.mark_fixed(format!("{message}; {healthy_message}"))
    } else {
        check
    }
}

fn classifier_external_gate_problem(
    error: &ExternalSqliteReadGateError,
) -> (DoctorJsonCheck, DoctorJsonCheck) {
    let message = error.message();
    let details = serde_json::json!({
        "available": false,
        "external_sqlite": error.details(),
    });
    (
        DoctorJsonCheck::problem(
            "classifier.enumeration",
            format!("classifier evidence unavailable: {message}"),
        )
        .with_details(details.clone()),
        DoctorJsonCheck::problem(
            "classifier.tags",
            format!("active classifier declarations unavailable: {message}"),
        )
        .with_details(details),
    )
}

fn classifier_enumeration_json(
    latest: &LatestClassifierCoverage,
    coverage: &ClassifierCoverage,
) -> DoctorJsonCheck {
    const ID: &str = "classifier.enumeration";
    let enumeration_details = serde_json::json!({
        "available": true,
        "schema": coverage.schema(),
        "run_id": latest.run_id(),
        "run_status": latest.run_status(),
        "source_walk_complete": coverage.source_walk_complete(),
        "source_walk_skipped_entries": coverage.source_walk_skipped_entries(),
        "plugin_discovery_complete": coverage.plugin_discovery_complete(),
        "plugin_discovery_errors": coverage.plugin_discovery_errors(),
        "plugin_discovery_error_samples": coverage.plugin_discovery_error_samples(),
    });
    if coverage.source_walk_complete()
        && coverage.plugin_discovery_complete()
        && coverage.source_walk_skipped_entries() == 0
    {
        DoctorJsonCheck::ok(
            ID,
            format!(
                "classifier enumeration is complete for run {}",
                latest.run_id().unwrap_or("<unknown>")
            ),
        )
        .with_details(enumeration_details)
    } else {
        DoctorJsonCheck::problem(
            ID,
            format!(
                "classifier enumeration is incomplete: source_walk_complete={}, skipped_entries={}, plugin_discovery_complete={}, discovery_errors={}",
                coverage.source_walk_complete(),
                coverage.source_walk_skipped_entries(),
                coverage.plugin_discovery_complete(),
                coverage.plugin_discovery_errors(),
            ),
        )
        .with_details(enumeration_details)
    }
}

fn classifier_tags_json(
    latest: &LatestClassifierCoverage,
    coverage: &ClassifierCoverage,
) -> DoctorJsonCheck {
    const ID: &str = "classifier.tags";
    let active_plugins: Vec<Value> = coverage
        .plugins()
        .iter()
        .filter(|plugin| plugin.matched_files() > 0)
        .map(|plugin| {
            serde_json::json!({
                "plugin_id": plugin.plugin_id(),
                "plugin_version": plugin.plugin_version(),
                "ontology_version": plugin.ontology_version(),
                "status": plugin.status(),
                "matched_files": plugin.matched_files(),
                "analyzed_files": plugin.analyzed_files(),
                "retained_files": plugin.retained_files(),
                "degraded_files": plugin.degraded_files(),
                "classifier_tags": plugin.classifier_tags(),
            })
        })
        .collect();
    let not_applicable_plugins: Vec<&str> = coverage
        .plugins()
        .iter()
        .filter(|plugin| plugin.status() == PluginCoverageStatus::NotApplicable)
        .map(loomweave_core::PluginClassifierCoverage::plugin_id)
        .collect();
    let has_failed = coverage.plugins().iter().any(|plugin| {
        plugin.matched_files() > 0 && plugin.status() == PluginCoverageStatus::Failed
    });
    let has_degraded = coverage.plugins().iter().any(|plugin| {
        plugin.matched_files() > 0 && plugin.status() == PluginCoverageStatus::Degraded
    });
    let has_empty_declaration = coverage
        .plugins()
        .iter()
        .any(|plugin| plugin.matched_files() > 0 && plugin.classifier_tags().is_empty());
    let tag_summary = if active_plugins.is_empty() {
        "no active plugins; all discovered plugins were not applicable".to_owned()
    } else {
        coverage
            .plugins()
            .iter()
            .filter(|plugin| plugin.matched_files() > 0)
            .map(|plugin| {
                format!(
                    "{}({:?})=[{}]",
                    plugin.plugin_id(),
                    plugin.status(),
                    plugin.classifier_tags().join(",")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let tag_details = serde_json::json!({
        "available": true,
        "schema": coverage.schema(),
        "run_id": latest.run_id(),
        "run_status": latest.run_status(),
        "active_plugins": active_plugins,
        "not_applicable_plugins": not_applicable_plugins,
    });
    if has_failed {
        DoctorJsonCheck::problem(
            ID,
            format!("active classifier plugin failed: {tag_summary}"),
        )
        .with_details(tag_details)
    } else if has_degraded {
        DoctorJsonCheck::warning(
            ID,
            format!("active classifier plugin degraded: {tag_summary}"),
        )
        .with_details(tag_details)
    } else if has_empty_declaration {
        DoctorJsonCheck::warning(
            ID,
            format!("active plugin declares no classifier tags: {tag_summary}"),
        )
        .with_details(tag_details)
    } else {
        DoctorJsonCheck::ok(ID, format!("active classifier tags: {tag_summary}"))
            .with_details(tag_details)
    }
}

fn classifier_probe_problem(detail: &str) -> (DoctorJsonCheck, DoctorJsonCheck) {
    let details = serde_json::json!({"available": false, "probe_error": detail});
    (
        DoctorJsonCheck::problem(
            "classifier.enumeration",
            format!("classifier coverage probe failed: {detail}"),
        )
        .with_details(details.clone()),
        DoctorJsonCheck::problem(
            "classifier.tags",
            format!("active classifier declarations could not be read: {detail}"),
        )
        .with_details(details),
    )
}

fn classifier_unavailable_check(
    id: &'static str,
    status: &str,
    message: String,
    details: Value,
) -> DoctorJsonCheck {
    let check = match status {
        "ok" => DoctorJsonCheck::ok(id, message),
        "warning" => DoctorJsonCheck::warning(id, message),
        _ => DoctorJsonCheck::problem(id, message),
    };
    check.with_details(details)
}

/// Outcome of the index-integrity check (clarion-abda98c869 recovery). Shared by
/// the text and JSON paths so they cannot drift.
enum IntegrityOutcome {
    /// No healthy index to check — the `.weft/loomweave.schema` check owns that
    /// state; integrity stays silent rather than double-reporting.
    Skipped,
    Healthy,
    /// Corruption found, `--fix` not requested.
    Found {
        stale: usize,
        mismatches: usize,
        sample: Vec<String>,
    },
    /// `--fix` ran and fully restored integrity.
    Repaired {
        removed_files: usize,
        removed_entities: usize,
    },
    /// `--fix` removed stale rows but residual corruption remains (needs a full
    /// re-analyze), or repair could not run.
    ResidualAfterFix {
        removed_files: usize,
        removed_entities: usize,
        residual: usize,
    },
    /// Opening/repairing the DB errored (e.g. busy under a running `serve`).
    Error(String),
}

/// Detect (and, under `--fix`, repair) index-integrity corruption: stale
/// vanished-from-disk file entities and the `LMWV-INFRA-PARENT-CONTAINS-MISMATCH`
/// invariant violations a file→package refactor leaves behind. Only runs on a
/// healthy, migrated index (the schema check owns the other states).
fn index_integrity_outcome(project_root: &Path, fix: bool) -> IntegrityOutcome {
    if !matches!(
        classify_index_db_health(project_root),
        IndexDbHealth::Healthy
    ) {
        return IntegrityOutcome::Skipped;
    }
    let db_path = loomweave_core::store::db_path(project_root);

    if fix {
        // This repair deletes entity rows. Nothing else serialises it against
        // a concurrent `loomweave analyze` that may be mid-way through
        // re-linking the very rows being removed -- SQLite's busy_timeout and
        // WAL bound that to lock contention rather than corruption, but the
        // interleaving is still wrong. Take the same advisory lock `analyze`
        // takes (STO-01), non-blocking, and report busy rather than racing.
        //
        // Deliberately scoped to this repair, NOT to all of `doctor --fix`:
        // `repair_classifier_analysis` spawns `loomweave analyze`, which
        // acquires this lock itself, so holding it across that repair would
        // make doctor deadlock against its own child.
        let loomweave_dir = loomweave_core::store::store_dir(project_root);
        let _analyze_lock = match crate::analyze_lock::acquire_analyze_lock(&loomweave_dir) {
            Ok(guard) => guard,
            Err(err) => return IntegrityOutcome::Error(err.to_string()),
        };
        match repair_index_integrity(&db_path, project_root) {
            Ok(report) => {
                let residual = report.residual.stale_file_entities.len()
                    + report.residual.parent_contains_mismatches.len();
                if residual == 0 {
                    IntegrityOutcome::Repaired {
                        removed_files: report.removed_file_entities,
                        removed_entities: report.removed_entities_total,
                    }
                } else {
                    IntegrityOutcome::ResidualAfterFix {
                        removed_files: report.removed_file_entities,
                        removed_entities: report.removed_entities_total,
                        residual,
                    }
                }
            }
            Err(err) => IntegrityOutcome::Error(err.to_string()),
        }
    } else {
        match check_index_integrity_readonly(&db_path, project_root) {
            Ok(report) if report.is_healthy() => IntegrityOutcome::Healthy,
            Ok(report) => {
                let sample = report
                    .stale_file_entities
                    .iter()
                    .map(|s| format!("stale file: {}", s.path))
                    .chain(
                        report
                            .parent_contains_mismatches
                            .iter()
                            .map(|m| m.detail.clone()),
                    )
                    .take(3)
                    .collect();
                IntegrityOutcome::Found {
                    stale: report.stale_file_entities.len(),
                    mismatches: report.parent_contains_mismatches.len(),
                    sample,
                }
            }
            Err(err) => IntegrityOutcome::Error(err.to_string()),
        }
    }
}

fn check_index_integrity_readonly(
    db_path: &Path,
    project_root: &Path,
) -> Result<loomweave_storage::integrity::IntegrityReport> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open index {} read-only", db_path.display()))?;
    loomweave_storage::pragma::apply_read_pragmas(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    loomweave_storage::integrity::check_integrity(&conn, project_root)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn repair_index_integrity(
    db_path: &Path,
    project_root: &Path,
) -> Result<loomweave_storage::integrity::RepairReport> {
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("open index {} for repair", db_path.display()))?;
    loomweave_storage::pragma::apply_write_pragmas(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    loomweave_storage::integrity::repair_integrity(&mut conn, project_root)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

const INTEGRITY_REBUILD_HINT: &str = "stop any running `loomweave serve`, then run `loomweave analyze --no-incremental` \
     to fully rebuild the graph";

/// Lines of a failed auto-repair's output replayed into the check detail.
const MAX_REPAIR_DIAGNOSTIC_LINES: usize = 10;

/// Text-path index-integrity check.
fn check_index_integrity(project_root: &Path, fix: bool) -> Tally {
    match index_integrity_outcome(project_root, fix) {
        IntegrityOutcome::Skipped => Tally::default(),
        IntegrityOutcome::Healthy => {
            ok("index integrity: no stale entities or parent/contains mismatches")
        }
        IntegrityOutcome::Found {
            stale,
            mismatches,
            sample,
        } => problem(
            &format!(
                "index integrity: {stale} stale file entit{} + {mismatches} parent/contains \
                 mismatch{} (e.g. {})",
                if stale == 1 { "y" } else { "ies" },
                if mismatches == 1 { "" } else { "es" },
                sample.first().map_or("—", String::as_str),
            ),
            Some("loomweave doctor --fix --path . (surgically removes stale rows)"),
        ),
        IntegrityOutcome::Repaired {
            removed_files,
            removed_entities,
        } => ok(&format!(
            "index integrity: repaired — removed {removed_files} stale file entit{} \
             ({removed_entities} entit{} total); index is now consistent",
            if removed_files == 1 { "y" } else { "ies" },
            if removed_entities == 1 { "y" } else { "ies" },
        )),
        IntegrityOutcome::ResidualAfterFix {
            removed_files,
            removed_entities,
            residual,
        } => problem(
            &format!(
                "index integrity: removed {removed_files} stale file entit{} ({removed_entities} \
                 total) but {residual} violation{} remain that surgical repair cannot fix",
                if removed_files == 1 { "y" } else { "ies" },
                if residual == 1 { "" } else { "s" },
            ),
            Some(INTEGRITY_REBUILD_HINT),
        ),
        IntegrityOutcome::Error(err) => problem(
            &format!("index integrity: check/repair failed: {err}"),
            Some("ensure no `loomweave serve` holds the database, then retry"),
        ),
    }
}

/// JSON-path twin of [`check_index_integrity`].
fn check_index_integrity_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    const ID: &str = "index.integrity";
    match index_integrity_outcome(project_root, fix) {
        IntegrityOutcome::Skipped => {
            DoctorJsonCheck::ok(ID, "no healthy index to check (see .weft/loomweave.schema)")
        }
        IntegrityOutcome::Healthy => {
            DoctorJsonCheck::ok(ID, "no stale entities or parent/contains mismatches")
        }
        IntegrityOutcome::Found {
            stale,
            mismatches,
            sample,
        } => DoctorJsonCheck::problem(
            ID,
            format!(
                "{stale} stale file entities + {mismatches} parent/contains mismatches \
                 (run with --fix to repair); examples: {}",
                sample.join("; ")
            ),
        ),
        IntegrityOutcome::Repaired {
            removed_files,
            removed_entities,
        } => DoctorJsonCheck::fixed(
            ID,
            format!(
                "repaired — removed {removed_files} stale file entities ({removed_entities} \
                 entities total); index is now consistent"
            ),
        ),
        IntegrityOutcome::ResidualAfterFix {
            removed_files,
            removed_entities,
            residual,
        } => DoctorJsonCheck::problem(
            ID,
            format!(
                "removed {removed_files} stale file entities ({removed_entities} total) but \
                 {residual} violations remain; {INTEGRITY_REBUILD_HINT}"
            ),
        ),
        IntegrityOutcome::Error(err) => {
            DoctorJsonCheck::problem(ID, format!("check/repair failed: {err}"))
        }
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
    let tracked = hardened_git_command(project_root)
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
    let status = hardened_git_command(project_root)
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
    if lines.iter().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("may be stale") || line.contains("no analysis recorded yet")
    }) {
        DoctorJsonCheck::warning("index.freshness", lines.join("\n"))
    } else {
        DoctorJsonCheck::ok("index.freshness", lines.join("\n"))
    }
}

/// Classifier evidence comes from language plugins. With none installed there is
/// nothing to classify and no repair to perform: `analyze` would only record
/// `skipped_no_plugins`. Return the pair of warnings `plugin.availability`
/// already implies, rather than a problem `--fix` can never clear — which would
/// otherwise pin `doctor --fix` to exit 1 on every plugin-less machine (CI
/// runners, and fresh installs before the operator adds a plugin).
///
/// `None` when the missing plugin does not explain the state. Only two states
/// qualify: no run at all, or a run that skipped for want of plugins. A `failed`
/// run, or a `completed` run whose coverage is missing or malformed, is real
/// broken evidence and must keep failing closed — uninstalling a plugin must not
/// launder it into "not applicable".
fn classifier_not_applicable(
    latest: &LatestClassifierCoverage,
) -> Option<(DoctorJsonCheck, DoctorJsonCheck)> {
    if !matches!(latest.run_status(), None | Some("skipped_no_plugins"))
        || any_language_plugin_discovered()
    {
        return None;
    }
    let reason = "no language plugin is installed, so no classifier evidence can exist";
    let details = serde_json::json!({
        "available": false,
        "run_id": latest.run_id(),
        "run_status": latest.run_status(),
        "reason": reason,
    });
    Some((
        classifier_unavailable_check(
            ENUMERATION_ID,
            "warning",
            format!("classifier enumeration is not applicable: {reason}"),
            details.clone(),
        ),
        classifier_unavailable_check(
            TAGS_ID,
            "warning",
            format!("active classifier declarations are not applicable: {reason}"),
            details,
        ),
    ))
}

/// Whether any language plugin is visible to the same discovery path `analyze`
/// uses. Errors are treated as "not discovered": a plugin that cannot be loaded
/// cannot produce classifier evidence either.
fn any_language_plugin_discovered() -> bool {
    loomweave_core::plugin::discover()
        .into_iter()
        .any(|result| result.is_ok())
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

fn check_git_hooks_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    use crate::git_hooks::GitHookState;
    match crate::git_hooks::git_sync_hook_state(project_root) {
        GitHookState::Present => DoctorJsonCheck::ok(
            "hook.git_sync",
            "git-sync hooks present (post-checkout/post-merge)",
        ),
        GitHookState::NoGitDir => DoctorJsonCheck::ok(
            "hook.git_sync",
            "no git repository; git-sync hooks not applicable",
        ),
        state => {
            let what = match state {
                GitHookState::Missing => "git-sync git hooks missing",
                GitHookState::Stale => "git-sync git hooks stale (outdated or partial block)",
                GitHookState::Present | GitHookState::NoGitDir => unreachable!(),
            };
            // Same severity posture as the text surface: warn, do not gate.
            if !fix {
                return DoctorJsonCheck::warning("hook.git_sync", what);
            }
            match crate::git_hooks::install_git_sync_hooks(project_root) {
                Ok(Some(_))
                    if crate::git_hooks::git_sync_hook_state(project_root)
                        == GitHookState::Present =>
                {
                    DoctorJsonCheck::fixed("hook.git_sync", format!("{what}; fixed"))
                }
                Ok(_) => DoctorJsonCheck::problem(
                    "hook.git_sync",
                    format!("{what}; repair did not converge"),
                ),
                Err(err) => DoctorJsonCheck::problem(
                    "hook.git_sync",
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
    // `serve` resolves both the inherited config and the published port through
    // the worktree context. Mirror that routing here: a linked checkout reads
    // its primary project's config and publishes under its isolated store.
    let worktree_ctx = loomweave_core::worktree::WorktreeContext::resolve(project_root).ok();
    let config_path = worktree_ctx.as_ref().map_or_else(
        || project_root.join("loomweave.yaml"),
        loomweave_core::worktree::WorktreeContext::config_path,
    );
    let port_path = worktree_ctx.as_ref().map_or_else(
        || project_root.join(".weft/loomweave/ephemeral.port"),
        |ctx| ctx.store_paths.port.clone(),
    );
    let Some(config) = read_loomweave_yaml_at(&config_path) else {
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
        loomweave_federation::loomweave_url::resolve_loomweave_url_at(None, &port_path, |name| {
            loomweave_core::dotenv::var(name)
        });
    if let Some(url) = resolution.resolved_url {
        if resolution.source == loomweave_federation::loomweave_url::SOURCE_EPHEMERAL_PORT
            && !http_health_reachable(&url)
        {
            return DoctorJsonCheck::warning(
                "http.config",
                format!(
                    "stale HTTP read-API port metadata in {}: \
                     {url}{HTTP_LIVENESS_PATH} is not reachable; start `loomweave serve` or \
                     ignore this persisted port when .mcp.json launches the stdio runtime",
                    port_path.display()
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
            format!(
                "HTTP enabled; read-API port auto-selected and published to {} while serving",
                port_path.display()
            ),
        )
    } else {
        DoctorJsonCheck::ok(
            "http.config",
            format!("HTTP configured on {bind} (auto-published while serving)"),
        )
    }
}

fn load_mcp_config_for_doctor(project_root: &Path) -> std::result::Result<McpConfig, String> {
    let path = loomweave_core::worktree::WorktreeContext::resolve(project_root).map_or_else(
        |_| project_root.join("loomweave.yaml"),
        |ctx| ctx.config_path(),
    );
    if path.exists() {
        McpConfig::from_path(&path).map_err(|err| err.to_string())
    } else {
        Ok(McpConfig::default())
    }
}

fn check_http_authentication_json(project_root: &Path) -> DoctorJsonCheck {
    const ID: &str = "http.authentication";
    let config = match load_mcp_config_for_doctor(project_root) {
        Ok(config) => config,
        Err(err) => {
            return DoctorJsonCheck::problem(
                ID,
                format!("HTTP authentication discovery cannot parse loomweave.yaml: {err}"),
            )
            .with_details(serde_json::json!({
                "config_valid": false,
                "protected_routes": "unavailable",
            }))
            .with_next_action(
                "Repair `loomweave.yaml` syntax and validation errors, then run `loomweave doctor` again.",
            );
        }
    };
    let http = &config.serve.http;
    if !http.enabled {
        return DoctorJsonCheck::ok(ID, "HTTP read API is disabled; protected routes are absent")
            .with_details(serde_json::json!({
                "config_valid": true,
                "http_enabled": false,
                "protected_routes": "none",
                "secret_configured": false,
                "secret_present": false,
            }));
    }

    let identity_secret_present = http.identity_token_env.as_deref().is_some_and(|name| {
        loomweave_core::dotenv::var(name).is_some_and(|value| !value.trim().is_empty())
    });
    let bearer_secret_present =
        loomweave_core::dotenv::var(&http.token_env).is_some_and(|value| !value.trim().is_empty());
    let configured_mode = if http.identity_token_env.is_some() {
        "hmac"
    } else if bearer_secret_present {
        "bearer"
    } else {
        "none"
    };
    let secret_present = match configured_mode {
        "hmac" => identity_secret_present,
        "bearer" => bearer_secret_present,
        _ => false,
    };
    let details = serde_json::json!({
        "config_valid": true,
        "http_enabled": true,
        "protected_routes": configured_mode,
        "secret_configured": http.identity_token_env.is_some() || configured_mode == "bearer",
        "secret_present": secret_present,
        "loopback": http.is_loopback_bind(),
    });
    if let Err(err) = http.validate_auth_trust(loomweave_core::dotenv::var) {
        let check = DoctorJsonCheck::problem(
            ID,
            format!("HTTP authentication is configured but unusable: {err}"),
        )
        .with_details(details);
        if let Some(secret_env) = http
            .identity_token_env
            .as_deref()
            .filter(|_| !identity_secret_present)
        {
            return check.with_next_action(format!(
                "Set ${secret_env} to a non-empty HMAC secret, then run `loomweave doctor` again."
            ));
        }
        return check.with_next_action(format!(
            "Set ${} to a non-empty bearer secret, then run `loomweave doctor` again.",
            http.token_env
        ));
    }
    match configured_mode {
        "hmac" => DoctorJsonCheck::ok(ID, "HTTP protected routes use HMAC authentication")
            .with_details(details),
        "bearer" => DoctorJsonCheck::ok(ID, "HTTP protected routes use bearer authentication")
            .with_details(details),
        _ => DoctorJsonCheck::ok(
            ID,
            "HTTP read API is enabled on loopback without authentication",
        )
        .with_details(details),
    }
}

/// Validate the exact instance-ID leaf that `serve` will load. Linked
/// worktrees route through `WorktreeContext::store_paths` so doctor never
/// validates (or repairs) a decoy store under the linked checkout itself.
fn check_http_instance_id_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    const ID: &str = "http.instance_id";
    let (store_paths, is_linked) = store_paths_for_doctor(project_root);
    let path = store_paths.instance_id.clone();
    let db_path = store_paths.db.clone();
    let scope = if is_linked {
        "current linked worktree isolated"
    } else {
        "project"
    };
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if fix && db_path.exists() {
                return match crate::instance::load_or_create(&path) {
                    Ok(instance_id) => DoctorJsonCheck::fixed(
                        ID,
                        format!("{scope} instance ID materialised: {instance_id}"),
                    )
                    .with_details(serde_json::json!({
                        "present": true,
                        "valid": true,
                        "instance_id": instance_id.to_string(),
                    })),
                    Err(err) => DoctorJsonCheck::problem(
                        ID,
                        format!("{scope} instance ID repair failed: {err}"),
                    )
                    .with_details(serde_json::json!({
                        "present": false,
                        "valid": false,
                        "instance_id": null,
                    })),
                };
            }
            let message = if is_linked {
                format!(
                    "{scope} instance_id is not materialised at {}",
                    path.display()
                )
            } else {
                "project instance ID is not materialised yet".to_owned()
            };
            return DoctorJsonCheck::warning(ID, message)
            .with_details(serde_json::json!({
                "present": false,
                "valid": null,
                "instance_id": null,
            }))
            .with_next_action(if db_path.exists() {
                "Run `loomweave doctor --fix`; serving and federation consumers require a project instance ID."
            } else if is_linked {
                "Build the current isolated store with `loomweave worktree analyze -- <target>` before materialising its instance ID."
            } else {
                "Run `loomweave install --path <project>` before materialising a project instance ID."
            });
        }
        Err(err) => {
            let message = if is_linked {
                format!(
                    "{scope} instance ID at {} is unreadable: {err}",
                    path.display()
                )
            } else {
                format!("project instance ID is unreadable: {err}")
            };
            let next_action = if is_linked {
                format!(
                    "Restore read access to `{}` and inspect it before replacing any data.",
                    path.display()
                )
            } else {
                "Restore read access to `.weft/loomweave/instance_id` and inspect it before replacing any data."
                    .to_owned()
            };
            return DoctorJsonCheck::problem(ID, message)
                .with_details(serde_json::json!({
                    "present": true,
                    "valid": false,
                    "instance_id": null,
                }))
                .with_next_action(next_action);
        }
    };
    match uuid::Uuid::parse_str(raw.trim()) {
        Ok(instance_id) => {
            DoctorJsonCheck::ok(ID, format!("{scope} instance ID is valid: {instance_id}"))
                .with_details(serde_json::json!({
                    "present": true,
                    "valid": true,
                    "instance_id": instance_id.to_string(),
                }))
        }
        Err(err) => {
            let message = if is_linked {
                format!(
                    "{scope} instance ID at {} is malformed; expected a UUID: {err}",
                    path.display()
                )
            } else {
                format!("project instance ID is malformed; expected a UUID: {err}")
            };
            let next_action = if is_linked {
                format!(
                    "Remove the malformed `{}`; `loomweave serve` will create a valid replacement.",
                    path.display()
                )
            } else {
                "Remove the malformed `.weft/loomweave/instance_id`; `loomweave serve` will create a valid replacement."
                    .to_owned()
            };
            DoctorJsonCheck::problem(ID, message)
                .with_details(serde_json::json!({
                    "present": true,
                    "valid": false,
                    "instance_id": null,
                }))
                .with_next_action(next_action)
        }
    }
}

/// The one route the read API serves unauthenticated, by design, so siblings can
/// probe it pre-auth (`http_read/linkages.rs`). That makes it the only sound
/// liveness target: every other route may legitimately answer 401/403 on a
/// perfectly healthy server, and a probe that reads those as "dead" is worse
/// than no probe at all.
const HTTP_LIVENESS_PATH: &str = "/api/v1/_capabilities";

/// Whether a Loomweave read API is answering at `base_url`.
///
/// This previously probed `/health`, a route the read API has never registered,
/// so a LIVE server was reported as unreachable stale port metadata for every
/// operator with HTTP enabled (clarion-7ad374bac4). The test fake answered any
/// path with 200, so nothing caught it.
fn http_health_reachable(base_url: &str) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(250))
        .build()
    else {
        return false;
    };
    let url = format!("{}{HTTP_LIVENESS_PATH}", base_url.trim_end_matches('/'));
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
    match select_provider_with_env(&config, loomweave_core::dotenv::var) {
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
    if !db.exists() {
        return DoctorJsonCheck::warning("sei.population", "loomweave.db is absent");
    }
    let Ok(conn) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return DoctorJsonCheck::warning("sei.population", "loomweave.db is absent or unreadable");
    };
    if let Err(err) = validate_external_sqlite_read_gate(&conn) {
        return DoctorJsonCheck::problem(
            "sei.population",
            format!("SEI population unavailable: {}", err.message()),
        )
        .with_details(serde_json::json!({
            "external_sqlite": err.details(),
        }));
    }
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

/// Reset the re-dispatch budget for every file that exhausted it
/// (`doctor --fix`, clarion-7f527d3d32). Returns the number of rows reset
/// and the coverage summary re-read on the *same* connection right after
/// the update, so the report cannot pair the reset count with counts from
/// before it. The re-read is fallible independently of the reset: once the
/// `UPDATE` has committed, a failure to re-read must not turn a successful
/// repair into a "repair failed" verdict — the caller reports it as such.
fn repair_resolution_redispatch_budget(
    db_path: &Path,
) -> Result<(u64, Result<ResolutionCoverageSummary, String>)> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("open index {} for repair", db_path.display()))?;
    loomweave_storage::pragma::apply_write_pragmas(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    let count = loomweave_storage::reset_exhausted_redispatch_budget(&conn)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let after =
        loomweave_storage::degraded_resolution_coverage_summary(&conn).map_err(|e| e.to_string());
    Ok((count, after))
}

/// What the `--fix` half of [`check_resolution_coverage_json`] did.
enum RedispatchBudgetReset {
    /// Dry run, or `--fix` with no exhausted file to reset.
    NotAttempted,
    /// Another `analyze` / `doctor --fix` held the lock (`WouldBlock`);
    /// nothing ran. Carries the lock path. This is the ONLY lock outcome that
    /// is transient — a lock file that cannot be opened or a filesystem that
    /// refuses advisory locks never reaches this variant (it is a `problem`).
    LockContended(std::path::PathBuf),
    /// The reset committed `count` rows. `after` is the post-reset summary,
    /// or why it could not be re-read (the reset still happened).
    ///
    /// A reset only re-arms the counter: the files are still degraded on
    /// disk until the next `analyze` re-dispatches them, so the check stays
    /// a `warning` (with `fixed: true` and an explicit next action) rather
    /// than `fixed`, which would drop it from the report's `next_actions`.
    Reset {
        count: u64,
        after: Result<ResolutionCoverageSummary, String>,
    },
}

/// Files whose last analysis reported degraded call / reference resolution
/// (clarion-3e517d4aff). Each is a call-graph hole; transient ones are
/// re-dispatched by the next `analyze` automatically; content- or
/// environment-determined ones (syntax error, per-file site cap,
/// `interpreter_unpinned`) persist until the source or the resolver
/// environment changes.
///
/// Under `--fix`, files that exhausted the re-dispatch budget
/// ([`loomweave_storage::MAX_REDISPATCH_ATTEMPTS`] consecutive transient
/// failures) get their budget reset so the next incremental `analyze`
/// re-dispatches them (clarion-7f527d3d32). Rows still under budget are left
/// alone — they already re-dispatch, and wiping their counter would let a
/// chronically flaky file dodge the anti-thrash budget.
///
/// Severity: lock contention with a concurrent `analyze` / `doctor --fix` is
/// transient and retriable, so it is a `warning`. A lock that cannot be
/// taken for any other reason (sentinel unopenable, filesystem without
/// advisory locks) and an actual repair failure (the update itself errors)
/// are `problem`s — persistent, needing an operator, never "just retry".
fn check_resolution_coverage_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    const ID: &str = "index.resolution_coverage";
    let db = loomweave_core::store::db_path(project_root);
    if !db.exists() {
        return DoctorJsonCheck::warning(ID, "loomweave.db is absent");
    }
    let Ok(conn) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return DoctorJsonCheck::warning(ID, "loomweave.db is absent or unreadable");
    };
    if let Err(err) = validate_external_sqlite_read_gate(&conn) {
        return DoctorJsonCheck::problem(
            ID,
            format!("resolution coverage unavailable: {}", err.message()),
        )
        .with_details(serde_json::json!({
            "external_sqlite": err.details(),
        }));
    }
    let before = match loomweave_storage::degraded_resolution_coverage_summary(&conn) {
        Ok(summary) => summary,
        Err(err) => {
            return DoctorJsonCheck::warning(
                ID,
                format!("resolution coverage could not be checked: {err}"),
            );
        }
    };

    let reset = if fix && before.exhausted > 0 {
        // Release the read-only handle before opening the write connection.
        drop(conn);
        // Same STO-01 advisory lock `analyze` takes, scoped to this repair
        // only (see `index_integrity_outcome` for why not all of `--fix`).
        let loomweave_dir = loomweave_core::store::store_dir(project_root);
        match crate::analyze_lock::try_acquire_analyze_lock(&loomweave_dir) {
            Ok(crate::analyze_lock::TryAnalyzeLock::Acquired(_guard)) => {
                match repair_resolution_redispatch_budget(&db) {
                    Ok((count, after)) => RedispatchBudgetReset::Reset { count, after },
                    // The lock WAS held and the write itself failed: a real
                    // repair failure, correctly a problem.
                    Err(err) => {
                        return DoctorJsonCheck::problem(
                            ID,
                            format!("resolution re-dispatch budget repair failed: {err}"),
                        );
                    }
                }
            }
            Ok(crate::analyze_lock::TryAnalyzeLock::Held { lock_path }) => {
                RedispatchBudgetReset::LockContended(lock_path)
            }
            // Not contention: the sentinel could not be opened (permissions,
            // read-only or missing store dir) or the filesystem refused the
            // advisory lock (NFS without lockd — unsupported for the store,
            // see CLAUDE.md). Retrying will not help; say so.
            Err(err) => {
                return DoctorJsonCheck::problem(
                    ID,
                    format!(
                        "{} file(s) exhausted the re-dispatch budget but the analyze lock \
                         could not be taken: {err:#} (not lock contention — check the \
                         permissions and filesystem of {}; advisory locks are required, \
                         NFS is unsupported)",
                        before.exhausted,
                        loomweave_dir.display()
                    ),
                )
                .with_details(serde_json::json!({
                    "exhausted_files": before.exhausted,
                    "analyze_lock_error": format!("{err:#}"),
                }))
                .with_next_action(format!(
                    "Make {} writable on a filesystem that supports advisory locks (not \
                     NFS), then re-run `loomweave doctor --fix --path <project>`.",
                    loomweave_dir.display()
                ));
            }
        }
    } else {
        RedispatchBudgetReset::NotAttempted
    };

    render_resolution_coverage_check(fix, &before, reset)
}

/// Pure half of [`check_resolution_coverage_json`]: turn the pre-reset
/// summary plus what `--fix` did into the report.
///
/// A reset is reported as `warning` + `fixed: true`, never as status
/// `fixed`: `json_report` builds `next_actions` from `problem` / `warning`
/// checks only, and the reset files stay degraded on disk until the next
/// `analyze` re-dispatches them — an agent that reads only `next_actions`
/// must still be told to run it. Status `fixed` is reserved for a repair
/// that leaves the checked condition healthy in the same step (compare
/// `repair_index_integrity`, `mark_classifier_repair`).
///
/// Every count in the report comes from ONE read: `before` when no reset
/// ran, the post-reset re-read when one did. A reset whose re-read failed
/// must not fall back to `before` — that would print the exhausted count the
/// reset just zeroed next to `reset_redispatch_budget_files`, contradicting
/// itself. It reports the reset count, says the current counts are
/// unavailable, and carries the pre-reset counts under an explicit
/// `before_reset` key instead.
fn render_resolution_coverage_check(
    fix: bool,
    before: &ResolutionCoverageSummary,
    reset: RedispatchBudgetReset,
) -> DoctorJsonCheck {
    const ID: &str = "index.resolution_coverage";
    let (summary, reset_count, lock_contended) = match reset {
        RedispatchBudgetReset::NotAttempted => (before, None, None),
        RedispatchBudgetReset::LockContended(err) => (before, None, Some(err)),
        RedispatchBudgetReset::Reset {
            count,
            after: Ok(ref after),
        } => (after, Some(count), None),
        RedispatchBudgetReset::Reset {
            count,
            after: Err(err),
        } => {
            return reset_pending_analyze(
                count,
                format!(
                    "reset the re-dispatch budget for {count} exhausted file(s); they will \
                     re-dispatch on the next incremental analyze — current coverage counts \
                     could not be re-read after the reset ({err}); re-run doctor for them"
                ),
                serde_json::json!({
                    "reset_redispatch_budget_files": count,
                    "max_redispatch_attempts": loomweave_storage::MAX_REDISPATCH_ATTEMPTS,
                    "post_reset_summary_error": err,
                    "before_reset": coverage_summary_details(before),
                }),
            );
        }
    };

    if summary.degraded_calls == 0 && summary.degraded_references == 0 {
        return DoctorJsonCheck::ok(
            ID,
            "every analysed file reports complete call and reference resolution",
        );
    }
    let calls = summary.degraded_calls;
    let references = summary.degraded_references;
    let transient = summary.transient;
    let exhausted = summary.exhausted;
    let persistent = calls
        .max(references)
        .saturating_sub(transient)
        .saturating_sub(exhausted);

    // `reset_redispatch_budget_files` is present iff a reset ran — absent
    // (not null) on a dry run, a lock-contended run, or a --fix with nothing
    // exhausted, so "was anything reset" is a key-presence check.
    let mut details = coverage_summary_details(summary);
    details.insert(
        "max_redispatch_attempts".into(),
        loomweave_storage::MAX_REDISPATCH_ATTEMPTS.into(),
    );
    if let Some(count) = reset_count {
        details.insert("reset_redispatch_budget_files".into(), count.into());
    }
    let details = Value::Object(details);

    let summary_clause = format!(
        "{calls} file(s) with degraded call resolution, {references} with degraded reference \
         resolution ({transient} transient, re-dispatched by the next analyze; {exhausted} \
         exhausted the re-dispatch budget; {persistent} content- or environment-determined: \
         syntax error / site cap / interpreter_unpinned)"
    );

    if let Some(count) = reset_count {
        reset_pending_analyze(
            count,
            format!(
                "reset the re-dispatch budget for {count} exhausted file(s); they re-dispatch \
                 on the next incremental analyze, which has not run yet ({summary_clause})"
            ),
            details,
        )
    } else if let Some(lock_path) = lock_contended {
        DoctorJsonCheck::warning(
            ID,
            format!(
                "{exhausted} file(s) exhausted the re-dispatch budget but the reset could not \
                 run: another analyze / doctor --fix holds the lock on {} (retry once it \
                 finishes — transient, not a repair failure)",
                lock_path.display()
            ),
        )
        .with_details(details)
    } else if fix {
        DoctorJsonCheck::warning(
            ID,
            format!(
                "{summary_clause}; --fix ran but no file had exhausted the re-dispatch \
                 budget, so nothing was reset"
            ),
        )
        .with_details(details)
    } else {
        DoctorJsonCheck::warning(ID, summary_clause).with_details(details)
    }
}

/// The four per-file counts of a [`ResolutionCoverageSummary`] as report
/// details.
/// A committed budget reset that still needs `analyze` to take effect: the
/// `--fix` action ran (`fixed: true`) but the condition persists (`warning`),
/// so `json_report` keeps it in `next_actions` with the follow-up spelled out.
fn reset_pending_analyze(count: u64, message: String, details: Value) -> DoctorJsonCheck {
    let mut check = DoctorJsonCheck::warning("index.resolution_coverage", message)
        .with_details(details)
        .with_next_action(format!(
            "Run `loomweave analyze <project>`: the {count} file(s) whose re-dispatch budget \
             was just reset are still degraded until that run re-dispatches them."
        ));
    check.fixed = true;
    check
}

fn coverage_summary_details(summary: &ResolutionCoverageSummary) -> serde_json::Map<String, Value> {
    let mut details = serde_json::Map::new();
    details.insert("degraded_calls_files".into(), summary.degraded_calls.into());
    details.insert(
        "degraded_references_files".into(),
        summary.degraded_references.into(),
    );
    details.insert("transient_files".into(), summary.transient.into());
    details.insert("exhausted_files".into(), summary.exhausted.into());
    details
}

/// `runs` rows left `running` by a builder that died uncleanly (OOM-kill,
/// `kill -9`, reboot) — clarion-5cf9643de9 aside. `analyze` itself only
/// sweeps rows whose heartbeat is >24 h old (`mark_stale_running_runs_failed`);
/// a fresher abandoned row poisons `project_status_get` / the hook snapshot
/// until then. The analyze lock is the liveness proof: every `analyze` holds
/// it from before `BeginRun` to after its last transaction, so if `doctor`
/// can take it, no builder is alive and every `running` row is abandoned.
///
/// Root-derived like its sibling checks: both the DB and the analyze-lock
/// directory come from `project_root` via `loomweave_core::store`, so a linked
/// worktree resolves to the repository store rather than a store of its own
/// (clarion-f8b577dc48).
fn check_runs_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    const ID: &str = "index.runs";
    let db = loomweave_core::store::db_path(project_root);
    if !db.exists() {
        return DoctorJsonCheck::warning(ID, "loomweave.db is absent");
    }
    let Ok(conn) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return DoctorJsonCheck::warning(ID, "loomweave.db is absent or unreadable");
    };
    if let Err(err) = validate_external_sqlite_read_gate(&conn) {
        return DoctorJsonCheck::problem(ID, format!("runs unavailable: {}", err.message()))
            .with_details(serde_json::json!({ "external_sqlite": err.details() }));
    }
    let running = match running_runs(&conn) {
        Ok(rows) => rows,
        Err(err) => return DoctorJsonCheck::warning(ID, format!("runs could not be read: {err}")),
    };
    if running.is_empty() {
        return DoctorJsonCheck::ok(ID, "no analyze run is recorded as running");
    }
    drop(conn);
    let loomweave_dir = loomweave_core::store::store_dir(project_root);
    let details = serde_json::json!({ "running_rows": running.len(), "runs": running });
    match crate::analyze_lock::try_acquire_analyze_lock(&loomweave_dir) {
        Ok(crate::analyze_lock::TryAnalyzeLock::Held { .. }) => DoctorJsonCheck::ok(
            ID,
            format!(
                "{} running run(s); an analyze holds the analyze lock, so they are live",
                running.len()
            ),
        )
        .with_details(details),
        Ok(crate::analyze_lock::TryAnalyzeLock::Acquired(_guard)) if fix => {
            match repair_abandoned_runs(&db) {
                Ok(count) => runs_repair_outcome(count, &running),
                Err(err) => {
                    DoctorJsonCheck::problem(ID, format!("abandoned run repair failed: {err}"))
                }
            }
        }
        Ok(crate::analyze_lock::TryAnalyzeLock::Acquired(_guard)) => DoctorJsonCheck::warning(
            ID,
            format!(
                "{} run(s) recorded as running but no analyze holds the lock (abandoned)",
                running.len()
            ),
        )
        .with_details(details)
        .with_next_action(
            "Run `loomweave doctor --fix --path <project>` to mark the abandoned runs failed.",
        ),
        Err(err) => DoctorJsonCheck::problem(
            ID,
            format!(
                "{} running run(s) and the analyze lock could not be taken: {err:#}",
                running.len()
            ),
        )
        .with_details(details),
    }
}

/// Pure half of [`check_runs_json`]'s `--fix` arm: turn the repair's row count
/// into the report.
///
/// `count == 0` is NOT a repair. The `running` rows were read before the
/// analyze lock was taken, so a run that finished in that window is gone by
/// the time the UPDATE lands — nothing was abandoned and nothing was marked.
/// Reporting that as `fixed` with "marked 0 abandoned running run(s) failed"
/// tells an operator (and `json_report`'s tally) that doctor repaired
/// something it did not touch, on a run that was healthy all along. It is an
/// `ok` with the reason named, matching the sibling "no analyze run is
/// recorded as running" early return.
fn runs_repair_outcome(count: usize, running: &[Value]) -> DoctorJsonCheck {
    const ID: &str = "index.runs";
    if count == 0 {
        return DoctorJsonCheck::ok(
            ID,
            "no analyze run is recorded as running (the run completed before repair)",
        );
    }
    DoctorJsonCheck::fixed(
        ID,
        format!("marked {count} abandoned running run(s) failed (no live analyze holds the lock)"),
    )
    .with_details(serde_json::json!({ "repaired_rows": count, "runs": running }))
}

fn running_runs(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, started_at, owner_pid, heartbeat_at FROM runs WHERE status = 'running' ORDER BY started_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, String>(0)?,
            "started_at": row.get::<_, String>(1)?,
            "owner_pid": row.get::<_, Option<i64>>(2)?,
            "heartbeat_at": row.get::<_, Option<String>>(3)?,
        }))
    })?;
    rows.collect()
}

fn repair_abandoned_runs(db_path: &Path) -> Result<usize> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("open index {} for repair", db_path.display()))?;
    loomweave_storage::pragma::apply_write_pragmas(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    loomweave_storage::mark_abandoned_running_runs_failed(&conn).map_err(|e| anyhow::anyhow!("{e}"))
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

/// The healthy-state message for the instructions check, redirect-aware (C-20).
/// Naming both files under a redirect would claim a block in CLAUDE.md that the
/// installer deliberately keeps out of it.
fn instructions_present_message(project_root: &Path) -> String {
    if instructions::claude_md_redirects_to_agents_md(project_root) {
        "agent-orientation block present in AGENTS.md (CLAUDE.md redirects to it)".to_owned()
    } else {
        "agent-orientation block present in CLAUDE.md + AGENTS.md".to_owned()
    }
}

fn check_instructions_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match instructions::instructions_state(project_root) {
        InstructionsState::UpToDate => DoctorJsonCheck::ok(
            "instructions.block",
            instructions_present_message(project_root),
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
                // C-20 inversion: under a redirect the block's ABSENCE from
                // CLAUDE.md is health, so the defect is a leftover and the
                // repair is migration — `install_instructions` migrates rather
                // than re-injects, so `--fix` cannot churn here.
                InstructionsState::RedirectStale => {
                    "CLAUDE.md redirects to AGENTS.md but still carries a Loomweave \
                     instruction block"
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
    read_loomweave_yaml_at(&project_root.join("loomweave.yaml"))
}

fn read_loomweave_yaml_at(config_path: &Path) -> Option<Value> {
    let raw = fs::read_to_string(config_path).ok()?;
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

/// Render one of the shared JSON diagnostic results on the human surface.
/// Keeping severity and wording in a single result prevents text/JSON drift.
fn emit_json_check_text(check: &DoctorJsonCheck) -> Tally {
    match check.status {
        "ok" | "fixed" => ok(&format!("{}: {}", check.id, check.message)),
        "warning" => warn(&format!("{}: {}", check.id, check.message), None),
        "problem" => problem(&format!("{}: {}", check.id, check.message), None),
        _ => problem(
            &format!("{}: unknown doctor status {}", check.id, check.status),
            None,
        ),
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

fn check_git_hooks(project_root: &Path, fix: bool) -> Tally {
    use crate::git_hooks::GitHookState;
    match crate::git_hooks::git_sync_hook_state(project_root) {
        GitHookState::Present => ok("git-sync hooks present (post-checkout/post-merge)"),
        // Not a git repo: git-sync has nowhere to live and that is fine.
        GitHookState::NoGitDir => ok("no git repository; git-sync hooks not applicable"),
        state => {
            let what = match state {
                GitHookState::Missing => "git-sync git hooks missing",
                GitHookState::Stale => "git-sync git hooks stale (outdated or partial block)",
                GitHookState::Present | GitHookState::NoGitDir => unreachable!(),
            };
            // Freshness enrichment, not a correctness requirement: a project
            // installed before `git init` (or one that never re-ran install)
            // must not gate-fail doctor. Warn with the repair nudge instead.
            if !fix {
                return warn(what, Some("loomweave install --hooks"));
            }
            match crate::git_hooks::install_git_sync_hooks(project_root) {
                Ok(Some(_))
                    if crate::git_hooks::git_sync_hook_state(project_root)
                        == GitHookState::Present =>
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
        InstructionsState::UpToDate => ok(&instructions_present_message(project_root)),
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
                // C-20 inversion — see the JSON twin: absence in a redirecting
                // CLAUDE.md is health, so `--fix` migrates instead of injecting.
                InstructionsState::RedirectStale => {
                    "CLAUDE.md redirects to AGENTS.md but still carries a Loomweave \
                     instruction block"
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

    #[cfg(unix)]
    fn install_fsmonitor_payload(root: &Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let marker = root.join("fsmonitor-fired");
        let hook = root.join("fsmonitor-hook.sh");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\nprintf fired >> '{}'\n", marker.display()),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&hook).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook, perms).unwrap();
        run_git(root, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
        marker
    }

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

    #[cfg(unix)]
    #[test]
    fn db_tracked_state_does_not_run_repo_fsmonitor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        let marker = install_fsmonitor_payload(root);
        write_db(root);

        assert_eq!(db_tracked_state(root), DbTrackedState::Untracked);
        assert!(
            !marker.exists(),
            "db tracking probe must not run repo-configured fsmonitor"
        );
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

    /// Open the canonical store DB and stamp `PRAGMA user_version = version`,
    /// creating a real (header-valid) `SQLite` file at the store path.
    fn write_db_with_user_version(root: &Path, version: u32) {
        let db = loomweave_core::store::db_path(root);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {version};"))
            .unwrap();
    }

    #[test]
    fn classify_absent_when_no_db_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            classify_index_db_health(dir.path()),
            IndexDbHealth::Absent
        ));
    }

    #[test]
    fn classify_healthy_at_current_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        write_db_with_user_version(dir.path(), CURRENT_SCHEMA_VERSION);
        assert!(matches!(
            classify_index_db_health(dir.path()),
            IndexDbHealth::Healthy
        ));
    }

    #[test]
    fn classify_unmigrated_when_user_version_is_zero() {
        // A header-valid SQLite file that no migration ever stamped — the empty
        // file the read pool would auto-create, or an externally-produced DB.
        // The read path (`reject_unmigrated_for_read`) refuses it, so doctor
        // must NOT report Healthy; it must mirror that refusal (review #8).
        let dir = tempfile::tempdir().unwrap();
        write_db_with_user_version(dir.path(), 0);
        assert!(
            matches!(
                classify_index_db_health(dir.path()),
                IndexDbHealth::Unmigrated
            ),
            "an unmigrated (user_version=0) index `serve` refuses must classify Unmigrated, \
             not Healthy"
        );
        // And it must surface as a gate-failing problem in both renderers.
        let json = check_loomweave_dir_json(dir.path());
        assert_eq!(json.status, "problem");
        let tally = check_loomweave_dir(dir.path());
        assert_eq!(
            tally.problems, 1,
            "unmigrated index must fail the doctor gate"
        );
    }

    #[test]
    fn classify_future_schema_when_user_version_exceeds_build() {
        let dir = tempfile::tempdir().unwrap();
        write_db_with_user_version(dir.path(), CURRENT_SCHEMA_VERSION + 1);
        assert!(matches!(
            classify_index_db_health(dir.path()),
            IndexDbHealth::FutureSchema { .. }
        ));
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

    #[cfg(unix)]
    #[test]
    fn git_untrack_db_does_not_run_repo_fsmonitor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        let db = write_db(root);
        run_git(root, &["add", "-f", ".weft/loomweave/loomweave.db"]);
        let marker = install_fsmonitor_payload(root);

        git_untrack_db(root).expect("untrack succeeds");

        assert!(db.exists(), "git rm --cached must keep the file");
        assert!(
            !marker.exists(),
            "db untrack repair must not run repo-configured fsmonitor"
        );
    }

    // --- index.resolution_coverage: dry run / --fix / lock-held / no-op ---

    /// A real migrated index at the canonical store path (schema-current, so
    /// the external-SQLite read gate admits it).
    fn migrated_db(root: &Path) -> std::path::PathBuf {
        let db = loomweave_core::store::db_path(root);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let mut conn = Connection::open(&db).unwrap();
        loomweave_storage::schema::apply_migrations(&mut conn).unwrap();
        db
    }

    fn transient_degraded_calls() -> loomweave_storage::SourceFileResolutionCoverage {
        loomweave_storage::SourceFileResolutionCoverage {
            calls: loomweave_storage::FacetCoverageRecord {
                degraded: true,
                reason: Some("pyright_timeout".to_owned()),
                transient: true,
                collateral: false,
            },
            references: loomweave_storage::FacetCoverageRecord::default(),
        }
    }

    /// Drive one file through enough consecutive degraded runs to exhaust
    /// `MAX_REDISPATCH_ATTEMPTS`.
    fn exhaust_one_file(conn: &Connection, source_file_id: &str) {
        let degraded = transient_degraded_calls();
        for run in 0..=loomweave_storage::MAX_REDISPATCH_ATTEMPTS {
            loomweave_storage::upsert_source_file_resolution_coverage(
                conn,
                source_file_id,
                &degraded,
                false,
                &format!("r{run}"),
                "t",
            )
            .unwrap();
        }
    }

    /// One transient-degraded row still under budget (attempts == 1).
    fn under_budget_file(conn: &Connection, source_file_id: &str) {
        let degraded = transient_degraded_calls();
        for run in 1..=2 {
            loomweave_storage::upsert_source_file_resolution_coverage(
                conn,
                source_file_id,
                &degraded,
                false,
                &format!("r{run}"),
                "t",
            )
            .unwrap();
        }
    }

    fn redispatch_attempts(conn: &Connection, source_file_id: &str) -> i64 {
        conn.query_row(
            "SELECT redispatch_attempts FROM source_file_resolution_coverage \
             WHERE source_file_id = ?1",
            rusqlite::params![source_file_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn resolution_coverage_dry_run_reports_warning_and_does_not_touch_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = migrated_db(root);
        let conn = Connection::open(&db).unwrap();
        exhaust_one_file(&conn, "core:file:stuck.py");
        let before = redispatch_attempts(&conn, "core:file:stuck.py");
        assert_eq!(before, loomweave_storage::MAX_REDISPATCH_ATTEMPTS);

        let check = check_resolution_coverage_json(root, false);
        assert_eq!(check.status, "warning", "{}", check.message);
        assert!(!check.fixed);
        let details = check.details.as_ref().unwrap();
        assert_eq!(details["exhausted_files"], 1);
        assert!(
            details.get("reset_redispatch_budget_files").is_none(),
            "dry run must not report a reset: {details}"
        );
        assert!(!check.message.contains("nothing was reset"));
        assert_eq!(redispatch_attempts(&conn, "core:file:stuck.py"), before);
    }

    #[test]
    fn resolution_coverage_fix_resets_the_exhausted_budget() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = migrated_db(root);
        let conn = Connection::open(&db).unwrap();
        exhaust_one_file(&conn, "core:file:stuck.py");

        let check = check_resolution_coverage_json(root, true);
        // The counter is re-armed but the file is still degraded until the
        // next analyze runs: an action was taken (`fixed`), the condition
        // persists (`warning`), and the follow-up is spelled out so
        // `json_report` keeps it in `next_actions`.
        assert_eq!(check.status, "warning", "{}", check.message);
        assert!(check.fixed);
        assert!(
            check.message.contains("has not run yet"),
            "must say analyze is still pending: {}",
            check.message
        );
        let next = check.next_action.as_deref().expect("explicit next action");
        assert!(
            next.contains("loomweave analyze") && next.contains('1'),
            "next action must name analyze and the reset count: {next}"
        );
        let details = check.details.as_ref().unwrap();
        assert_eq!(details["reset_redispatch_budget_files"], 1);
        assert_eq!(
            details["exhausted_files"], 0,
            "summary re-read after the reset"
        );
        assert_eq!(details["transient_files"], 1);
        assert_eq!(redispatch_attempts(&conn, "core:file:stuck.py"), 0);
    }

    #[test]
    fn resolution_coverage_fix_lock_that_cannot_be_opened_is_a_problem_not_contention() {
        // A lock sentinel that cannot be opened (here: a directory squatting
        // on its path, standing in for permission denied / NFS without
        // lockd) is persistent. It must not be narrated as "another analyze
        // holds the lock, retry" — that is a problem needing an operator.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = migrated_db(root);
        let conn = Connection::open(&db).unwrap();
        exhaust_one_file(&conn, "core:file:stuck.py");
        std::fs::create_dir(loomweave_core::store::store_dir(root).join("loomweave.lock")).unwrap();

        let check = check_resolution_coverage_json(root, true);
        assert_eq!(check.status, "problem", "{}", check.message);
        assert!(!check.fixed);
        assert!(
            check.message.contains("not lock contention")
                && !check.message.contains("retry once it finishes"),
            "must not be graded as transient contention: {}",
            check.message
        );
        let details = check.details.as_ref().unwrap();
        assert_eq!(details["exhausted_files"], 1);
        assert!(
            details["analyze_lock_error"]
                .as_str()
                .unwrap()
                .contains("open analyze lock file"),
            "{details}"
        );
        assert!(details.get("reset_redispatch_budget_files").is_none());
        assert!(check.next_action.is_some());
        assert_eq!(
            redispatch_attempts(&conn, "core:file:stuck.py"),
            loomweave_storage::MAX_REDISPATCH_ATTEMPTS,
            "repair must not have run"
        );
    }

    #[test]
    fn resolution_coverage_fix_is_a_warning_not_a_problem_when_the_analyze_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = migrated_db(root);
        let conn = Connection::open(&db).unwrap();
        exhaust_one_file(&conn, "core:file:stuck.py");

        let held =
            crate::analyze_lock::acquire_analyze_lock(&loomweave_core::store::store_dir(root))
                .unwrap();
        let check = check_resolution_coverage_json(root, true);
        assert_eq!(
            check.status, "warning",
            "lock contention is transient, must not fail the gate: {}",
            check.message
        );
        assert!(!check.fixed);
        assert!(
            check.message.contains("retry") && check.message.contains("lock"),
            "{}",
            check.message
        );
        let details = check.details.as_ref().unwrap();
        assert!(details.get("reset_redispatch_budget_files").is_none());
        assert_eq!(details["exhausted_files"], 1);
        assert_eq!(
            redispatch_attempts(&conn, "core:file:stuck.py"),
            loomweave_storage::MAX_REDISPATCH_ATTEMPTS,
            "repair must not have run"
        );

        drop(held);
        let check = check_resolution_coverage_json(root, true);
        assert!(check.fixed, "{}", check.message);
        assert_eq!(check.status, "warning", "{}", check.message);
        assert_eq!(redispatch_attempts(&conn, "core:file:stuck.py"), 0);
    }

    #[test]
    fn resolution_coverage_fix_with_nothing_exhausted_is_a_legible_noop() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = migrated_db(root);
        let conn = Connection::open(&db).unwrap();
        under_budget_file(&conn, "core:file:flaky.py");
        assert_eq!(redispatch_attempts(&conn, "core:file:flaky.py"), 1);

        let check = check_resolution_coverage_json(root, true);
        assert_eq!(check.status, "warning", "{}", check.message);
        assert!(!check.fixed);
        assert!(
            check.message.contains("nothing was reset"),
            "--fix with nothing exhausted must say so: {}",
            check.message
        );
        let details = check.details.as_ref().unwrap();
        assert!(details.get("reset_redispatch_budget_files").is_none());
        assert_eq!(details["transient_files"], 1);
        assert_eq!(details["exhausted_files"], 0);
        assert_eq!(redispatch_attempts(&conn, "core:file:flaky.py"), 1);
    }

    #[test]
    fn resolution_coverage_fix_whose_post_reset_reread_fails_does_not_report_stale_counts() {
        // The reset committed but the summary could not be re-read: the
        // report must not print the pre-reset exhausted count (now zero on
        // disk) beside `reset_redispatch_budget_files`.
        let before = ResolutionCoverageSummary {
            degraded_calls: 1,
            degraded_references: 0,
            transient: 0,
            exhausted: 1,
        };
        let check = render_resolution_coverage_check(
            true,
            &before,
            RedispatchBudgetReset::Reset {
                count: 1,
                after: Err("database is locked".to_owned()),
            },
        );
        assert_eq!(check.status, "warning", "{}", check.message);
        assert!(check.fixed);
        assert!(
            check
                .next_action
                .as_deref()
                .is_some_and(|next| next.contains("loomweave analyze")),
            "reset still needs analyze: {:?}",
            check.next_action
        );
        assert!(
            check.message.contains("reset the re-dispatch budget for 1")
                && check.message.contains("could not be re-read")
                && check.message.contains("database is locked"),
            "{}",
            check.message
        );
        assert!(
            !check.message.contains("exhausted the re-dispatch budget;"),
            "must not narrate pre-reset counts as current: {}",
            check.message
        );
        let details = check.details.as_ref().unwrap();
        assert_eq!(details["reset_redispatch_budget_files"], 1);
        assert_eq!(details["post_reset_summary_error"], "database is locked");
        assert!(
            details.get("exhausted_files").is_none() && details.get("transient_files").is_none(),
            "current counts are unknown and must be absent, not stale: {details}"
        );
        assert_eq!(details["before_reset"]["exhausted_files"], 1);
        assert_eq!(details["before_reset"]["degraded_calls_files"], 1);
    }

    #[test]
    fn resolution_coverage_headline_names_the_environment_determined_reason_too() {
        // The residual bucket (`persistent`) is everything that is neither
        // transient nor budget-exhausted, which since ADR-058 includes
        // `interpreter_unpinned` (`transient == false`, environment- not
        // content-determined). Calling the whole bucket "content-determined:
        // syntax error / site cap" told an operator staring at an unpinned
        // interpreter to go fix their source. The headline must offer the same
        // three reasons `default_next_action("index.resolution_coverage")`
        // does, or the two halves of the same check disagree.
        let before = ResolutionCoverageSummary {
            degraded_calls: 3,
            degraded_references: 0,
            transient: 0,
            exhausted: 0,
        };
        let check =
            render_resolution_coverage_check(false, &before, RedispatchBudgetReset::NotAttempted);
        assert_eq!(check.status, "warning", "{}", check.message);
        assert!(
            check.message.contains(
                "3 content- or environment-determined: syntax error / site cap / \
                          interpreter_unpinned"
            ),
            "{}",
            check.message
        );
        assert!(
            !check.message.contains("content-determined: syntax error"),
            "the old wording mislabelled interpreter_unpinned rows: {}",
            check.message
        );
        assert!(
            default_next_action("index.resolution_coverage").contains("interpreter_unpinned"),
            "headline and next action must name the same reasons"
        );
    }

    #[test]
    fn resolution_coverage_fix_reread_succeeds_on_the_write_connection() {
        // The re-read rides the same connection as the UPDATE, so the
        // reported counts are the post-reset ones even though no second
        // read-only handle is ever opened.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = migrated_db(root);
        let conn = Connection::open(&db).unwrap();
        exhaust_one_file(&conn, "core:file:stuck.py");

        let (count, after) = repair_resolution_redispatch_budget(&db).unwrap();
        assert_eq!(count, 1);
        let after = after.unwrap();
        assert_eq!((after.exhausted, after.transient), (0, 1));
    }

    #[test]
    fn resolution_coverage_fix_on_an_empty_coverage_table_is_a_safe_noop() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        migrated_db(root);
        let check = check_resolution_coverage_json(root, true);
        assert_eq!(check.status, "ok", "{}", check.message);
        assert!(!check.fixed);
        assert!(check.details.is_none());
    }

    // --- index.runs: dry run / --fix / lock-held ---

    fn seed_run(conn: &Connection, id: &str, status: &str, owner_pid: Option<i64>) {
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status, owner_pid) \
             VALUES (?1, '2026-01-01T00:00:00.000Z', NULL, '{}', '{}', ?2, ?3)",
            rusqlite::params![id, status, owner_pid],
        )
        .unwrap();
    }

    fn run_status(conn: &Connection, id: &str) -> String {
        conn.query_row(
            "SELECT status FROM runs WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn runs_check_is_ok_when_nothing_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let db = migrated_db(dir.path());
        let conn = Connection::open(&db).unwrap();
        seed_run(&conn, "done", "completed", None);
        let check = check_runs_json(dir.path(), false);
        assert_eq!(check.status, "ok", "{}", check.message);
    }

    #[test]
    fn runs_dry_run_warns_and_lists_abandoned_rows_without_touching_them() {
        let dir = tempfile::tempdir().unwrap();
        let db = migrated_db(dir.path());
        let conn = Connection::open(&db).unwrap();
        seed_run(&conn, "stuck", "running", Some(99_999_999));
        let check = check_runs_json(dir.path(), false);
        assert_eq!(check.status, "warning", "{}", check.message);
        assert!(!check.fixed);
        let details = check.details.as_ref().unwrap();
        assert_eq!(details["running_rows"], 1);
        assert_eq!(details["runs"][0]["id"], "stuck");
        assert_eq!(details["runs"][0]["owner_pid"], 99_999_999);
        assert_eq!(run_status(&conn, "stuck"), "running");
    }

    #[test]
    fn runs_fix_marks_every_running_row_failed_while_holding_the_analyze_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = migrated_db(dir.path());
        let conn = Connection::open(&db).unwrap();
        seed_run(&conn, "stuck-a", "running", Some(1));
        seed_run(&conn, "stuck-b", "running", None);
        seed_run(&conn, "done", "completed", None);
        let check = check_runs_json(dir.path(), true);
        assert_eq!(check.status, "fixed", "{}", check.message);
        assert!(check.fixed);
        assert_eq!(check.details.as_ref().unwrap()["repaired_rows"], 2);
        assert_eq!(run_status(&conn, "stuck-a"), "failed");
        assert_eq!(run_status(&conn, "stuck-b"), "failed");
        assert_eq!(run_status(&conn, "done"), "completed");
    }

    #[test]
    fn runs_repair_that_marked_nothing_is_ok_not_fixed() {
        // The `running` rows are read BEFORE the analyze lock is taken, so a
        // run that completed in that window leaves zero rows for the UPDATE.
        // The race is not deterministically reproducible through
        // `check_runs_json` (there is no seam between the read and the lock),
        // so the outcome half is tested directly.
        let running = vec![serde_json::json!({ "id": "raced" })];
        let none = runs_repair_outcome(0, &running);
        assert_eq!(none.status, "ok", "{}", none.message);
        assert!(!none.fixed, "nothing was repaired");
        assert!(
            none.message.contains("completed before repair"),
            "{}",
            none.message
        );
        assert!(
            !none.message.contains("marked 0"),
            "must not narrate a no-op as a repair: {}",
            none.message
        );
        assert!(none.details.is_none());

        let repaired = runs_repair_outcome(2, &running);
        assert_eq!(repaired.status, "fixed", "{}", repaired.message);
        assert!(repaired.fixed);
        assert!(
            repaired.message.contains("marked 2"),
            "{}",
            repaired.message
        );
        let details = repaired.details.as_ref().unwrap();
        assert_eq!(details["repaired_rows"], 2);
        assert_eq!(details["runs"][0]["id"], "raced");
    }

    #[test]
    fn runs_check_is_ok_not_warning_when_a_live_analyze_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = migrated_db(dir.path());
        let conn = Connection::open(&db).unwrap();
        seed_run(&conn, "live", "running", Some(std::process::id().into()));
        let store = loomweave_core::store::store_dir(dir.path());
        let _guard = match crate::analyze_lock::try_acquire_analyze_lock(&store).unwrap() {
            crate::analyze_lock::TryAnalyzeLock::Acquired(guard) => guard,
            crate::analyze_lock::TryAnalyzeLock::Held { .. } => {
                panic!("lock free in a fresh tempdir")
            }
        };
        for fix in [false, true] {
            let check = check_runs_json(dir.path(), fix);
            assert_eq!(check.status, "ok", "fix={fix}: {}", check.message);
            assert!(check.message.contains("analyze lock"), "{}", check.message);
        }
        assert_eq!(run_status(&conn, "live"), "running");
    }
}
