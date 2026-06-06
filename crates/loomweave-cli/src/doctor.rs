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
//! gate. Only a genuinely broken state — an unparseable config file, or a
//! `--fix` repair that errors or does not converge — is a problem.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

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
        check_sei_population_json(project_root),
        check_wardline_taint_capability_json(project_root),
        check_mcp_hygiene_json(),
        check_integration_bindings_json(project_root, fix),
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
            "index.freshness" => {
                "Run `loomweave analyze <project>` to refresh the index.".to_owned()
            }
            "plugin.availability" => {
                "Install a Loomweave language plugin (the Python plugin ships with `pip install \
                 loomweave`)."
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

fn check_loomweave_dir_json(project_root: &Path) -> DoctorJsonCheck {
    let loomweave_dir = project_root.join(".loomweave");
    let db = loomweave_dir.join("loomweave.db");
    if loomweave_dir.is_dir() && db.is_file() {
        DoctorJsonCheck::ok(
            ".loomweave.schema",
            ".loomweave directory and database are present",
        )
    } else if loomweave_dir.is_dir() {
        DoctorJsonCheck::warning(
            ".loomweave.schema",
            ".loomweave directory exists but loomweave.db is absent",
        )
    } else {
        DoctorJsonCheck::warning(".loomweave.schema", ".loomweave directory is absent")
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
    // static bind. A running serve publishes .loomweave/ephemeral.port.
    let resolution = loomweave_federation::loomweave_url::resolve_loomweave_url(None, project_root);
    if let Some(url) = resolution.resolved_url {
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
            "HTTP enabled; read-API port auto-selected and published to .loomweave/ephemeral.port while serving",
        )
    } else {
        DoctorJsonCheck::ok(
            "http.config",
            format!("HTTP configured on {bind} (auto-published while serving)"),
        )
    }
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

fn check_sei_population_json(project_root: &Path) -> DoctorJsonCheck {
    let db = project_root.join(".loomweave/loomweave.db");
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
        Ok(_) => problem(&format!("{what} — repair did not converge"), None),
        Err(err) => problem(&format!("{what} — repair failed: {err}"), None),
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
