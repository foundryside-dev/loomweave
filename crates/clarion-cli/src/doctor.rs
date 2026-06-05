//! `clarion doctor [--fix]` — verify (and optionally repair) the installed
//! agent-orientation surfaces.
//!
//! Three surfaces are checked, each owned by an existing installer module:
//! the `clarion-workflow` skill pack ([`crate::skill_pack`]), the `SessionStart`
//! hook ([`crate::hooks_settings`]), and the Claude Code `.mcp.json` MCP
//! registration ([`crate::mcp_registration`]), plus the local
//! Clarion/Filigree/Wardline binding files ([`crate::integration_bindings`]).
//! The repair for each is that module's idempotent installer, so
//! `doctor --fix` and `clarion install` converge to the same state.
//!
//! Output is a per-surface ✓/⚠/✗ report followed by the index snapshot (reused
//! verbatim from the session-start hook). [`run`] returns whether every surface
//! is healthy *after* any repairs; the caller maps an unhealthy result to a
//! non-zero exit so `doctor` is usable as a CI / pre-commit gate.
//!
//! Severity is deliberate. The Loom three-way integration bindings are an
//! *enrich-only* surface (per `docs/suite/loom.md` §5): a Clarion-solo or
//! Clarion+Filigree-only project is first-class, so their absence is a
//! **warning** (surfaced, suggests `--fix`) and never a problem that fails the
//! gate. Only a genuinely broken state — an unparseable config file, or a
//! `--fix` repair that errors or does not converge — is a problem.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

use crate::hooks_settings::HookState;
use crate::integration_bindings::BindingState;
use crate::mcp_registration::McpState;
use crate::skill_pack::SkillPackState;
use crate::{hook, hooks_settings, integration_bindings, mcp_registration, skill_pack};

/// Run `clarion doctor`. Returns `Ok(true)` iff every orientation surface is
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

    println!("clarion doctor{}", if fix { " --fix" } else { "" });

    let mut tally = Tally::default();
    tally += check_skill(&project_root, fix);
    tally += check_hook(&project_root, fix);
    tally += check_mcp(&project_root, fix);
    tally += check_integration_bindings(&project_root, fix);

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
        check_clarion_dir_json(project_root),
        check_index_freshness_json(project_root),
        check_plugin_availability_json(),
        check_skill_json(project_root, fix),
        check_hook_json(project_root, fix),
        check_mcp_json(project_root, fix),
        check_http_config_json(project_root),
        check_filigree_url_json(project_root),
        check_sei_population_json(project_root),
        check_wardline_taint_capability_json(project_root),
        check_mcp_hygiene_json(),
        check_integration_bindings_json(project_root, fix),
    ];
    let next_actions: Vec<String> = checks
        .iter()
        .filter(|check| check.status == "problem" || check.status == "warning")
        .map(|check| match check.id {
            "skill.pack" => "Run `clarion doctor --fix` or `clarion install --skills`.".to_owned(),
            "hook.session_start" => {
                "Run `clarion doctor --fix` or `clarion install --hooks`.".to_owned()
            }
            "mcp.registration" | "integration.bindings" => "Run `clarion doctor --fix`.".to_owned(),
            "index.freshness" => "Run `clarion analyze <project>` to refresh the index.".to_owned(),
            "plugin.availability" => {
                "Install a Clarion language plugin (the Python plugin ships with `pip install \
                 clarion`)."
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

fn check_clarion_dir_json(project_root: &Path) -> DoctorJsonCheck {
    let clarion_dir = project_root.join(".clarion");
    let db = clarion_dir.join("clarion.db");
    if clarion_dir.is_dir() && db.is_file() {
        DoctorJsonCheck::ok(
            ".clarion.schema",
            ".clarion directory and database are present",
        )
    } else if clarion_dir.is_dir() {
        DoctorJsonCheck::warning(
            ".clarion.schema",
            ".clarion directory exists but clarion.db is absent",
        )
    } else {
        DoctorJsonCheck::warning(".clarion.schema", ".clarion directory is absent")
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
    // Use the same discovery path as `clarion analyze` (`$PATH` *and* the running
    // binary's directory), so doctor agrees with analyze about which plugins are
    // visible. A manual `$PATH`-only scan here would report a co-located
    // PyPI/venv-installed plugin as missing even though analyze can drive it.
    let mut ids = Vec::new();
    let mut errs = Vec::new();
    for result in clarion_core::plugin::discover() {
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
            "no clarion language plugin discovered (on PATH or alongside the clarion binary)",
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
        McpState::Present => {
            DoctorJsonCheck::ok("mcp.registration", ".mcp.json clarion serve entry present")
        }
        McpState::Unparseable => {
            DoctorJsonCheck::problem("mcp.registration", ".mcp.json is not parseable JSON")
        }
        McpState::UntrustedCommand => {
            let cmd = mcp_registration::clarion_entry_command(project_root)
                .unwrap_or_else(|| "<unknown>".to_owned());
            let what = format!(
                ".mcp.json clarion entry uses an unrecognized command {cmd:?} (not the clarion \
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
                McpState::Missing => ".mcp.json has no clarion serve entry",
                McpState::Stale => ".mcp.json clarion entry is stale or not runtime-discovered",
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
                        format!("{what}; merged clarion serve entry"),
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
    let Some(config) = read_clarion_yaml(project_root) else {
        return DoctorJsonCheck::warning("http.config", "clarion.yaml is absent or unparseable");
    };
    let enabled = config
        .get("serve")
        .and_then(|serve| serve.get("http"))
        .and_then(|http| http.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true);
    let bind = config
        .get("serve")
        .and_then(|serve| serve.get("http"))
        .and_then(|http| http.get("bind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if enabled && !bind.trim().is_empty() {
        DoctorJsonCheck::ok("http.config", format!("HTTP configured on {bind}"))
    } else {
        DoctorJsonCheck::warning("http.config", "HTTP serve config is disabled or incomplete")
    }
}

fn check_filigree_url_json(project_root: &Path) -> DoctorJsonCheck {
    let Some(config) = read_clarion_yaml(project_root) else {
        return DoctorJsonCheck::warning("filigree.url", "clarion.yaml is absent or unparseable");
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

fn check_sei_population_json(project_root: &Path) -> DoctorJsonCheck {
    let db = project_root.join(".clarion/clarion.db");
    let Ok(conn) = Connection::open(&db) else {
        return DoctorJsonCheck::warning("sei.population", "clarion.db is absent or unreadable");
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
    let Some(config) = read_clarion_yaml(project_root) else {
        return DoctorJsonCheck::warning(
            "wardline.taint_store",
            "clarion.yaml is absent or unparseable",
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

fn check_integration_bindings_json(project_root: &Path, fix: bool) -> DoctorJsonCheck {
    match integration_bindings::binding_state(project_root) {
        BindingState::Present => DoctorJsonCheck::ok(
            "integration.bindings",
            "three-way integration bindings present (Clarion + Filigree + Wardline)",
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

fn read_clarion_yaml(project_root: &Path) -> Option<Value> {
    let raw = fs::read_to_string(project_root.join("clarion.yaml")).ok()?;
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
                    Some("clarion install --skills"),
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
                return problem(what, Some("clarion install --hooks"));
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
        McpState::Present => ok(".mcp.json clarion serve entry present"),
        McpState::Unparseable => problem(
            ".mcp.json is not parseable JSON — fix it by hand, then re-run",
            None,
        ),
        McpState::UntrustedCommand => {
            let cmd = mcp_registration::clarion_entry_command(project_root)
                .unwrap_or_else(|| "<unknown>".to_owned());
            let what = format!(
                ".mcp.json clarion entry uses an unrecognized command {cmd:?} (not the clarion \
                 executable); doctor will not auto-replace it"
            );
            if !fix {
                return problem(
                    &what,
                    Some(
                        "if this is a deliberate wrapper, leave it; otherwise set `command` to \
                         `clarion` or remove the entry — `--fix` will not clobber it",
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
                McpState::Missing => ".mcp.json has no clarion serve entry",
                McpState::Stale => ".mcp.json clarion entry is stale or not runtime-discovered",
                McpState::Present | McpState::Unparseable | McpState::UntrustedCommand => {
                    unreachable!()
                }
            };
            if !fix {
                return problem(
                    what,
                    Some("clarion doctor --fix  (or add the entry to .mcp.json manually)"),
                );
            }
            match mcp_registration::install_mcp_entry(project_root) {
                Ok(_) if mcp_registration::mcp_entry_state(project_root) == McpState::Present => {
                    ok(&format!("{what} — fixed (merged clarion serve entry)"))
                }
                Ok(_) => problem(&format!("{what} — repair did not converge"), None),
                Err(err) => problem(&format!("{what} — repair failed: {err}"), None),
            }
        }
    }
}

fn check_integration_bindings(project_root: &Path, fix: bool) -> Tally {
    match integration_bindings::binding_state(project_root) {
        BindingState::Present => {
            ok("three-way integration bindings present (Clarion + Filigree + Wardline)")
        }
        BindingState::Unparseable => problem(
            "three-way integration bindings are not parseable — fix config files by hand, then re-run",
            None,
        ),
        BindingState::MissingOrStale => {
            let what = "three-way integration bindings missing or stale";
            if !fix {
                // Enrich-only surface: absence is a warning, not a gate failure.
                return warn(what, Some("clarion doctor --fix"));
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
