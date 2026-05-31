//! `clarion doctor [--fix]` — verify (and optionally repair) the installed
//! agent-orientation surfaces.
//!
//! Three surfaces are checked, each owned by an existing installer module:
//! the `clarion-workflow` skill pack ([`crate::skill_pack`]), the `SessionStart`
//! hook ([`crate::hooks_settings`]), and the `.mcp.json` MCP registration
//! ([`crate::mcp_registration`]). The repair for each is that module's
//! idempotent installer, so `doctor --fix` and `clarion install` converge to
//! the same state.
//!
//! Output is a per-surface ✓/✗ report followed by the index snapshot (reused
//! verbatim from the session-start hook). [`run`] returns whether every surface
//! is healthy *after* any repairs; the caller maps an unhealthy result to a
//! non-zero exit so `doctor` is usable as a CI / pre-commit gate.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::hooks_settings::HookState;
use crate::mcp_registration::McpState;
use crate::skill_pack::SkillPackState;
use crate::{hook, hooks_settings, mcp_registration, skill_pack};

/// Run `clarion doctor`. Returns `Ok(true)` iff every orientation surface is
/// healthy after any requested repairs.
///
/// # Errors
///
/// Returns an error only if the target directory does not exist or cannot be
/// canonicalised. Per-surface repair failures are reported as problems (they do
/// not abort the run), so one broken surface never hides the others.
pub fn run(path: &Path, fix: bool) -> Result<bool> {
    if !path.exists() {
        bail!(
            "target directory does not exist: {}. Create it first or pass a valid --path.",
            path.display()
        );
    }
    let project_root = path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise --path {}", path.display()))?;

    println!("clarion doctor{}", if fix { " --fix" } else { "" });

    let mut problems = 0usize;
    problems += check_skill(&project_root, fix);
    problems += check_hook(&project_root, fix);
    problems += check_mcp(&project_root, fix);

    println!("--- index ---");
    for line in hook::snapshot_report(&project_root) {
        println!("{line}");
    }

    if problems == 0 {
        println!("All orientation surfaces healthy.");
    } else {
        let suffix = if fix {
            "."
        } else {
            " (run with --fix to repair)."
        };
        let plural = if problems == 1 { "" } else { "s" };
        println!("{problems} problem{plural} found{suffix}");
    }
    Ok(problems == 0)
}

/// Print one healthy line and return 0.
fn ok(line: &str) -> usize {
    println!("  ✓ {line}");
    0
}

/// Print one problem line (plus an optional fix hint) and return 1.
fn problem(line: &str, fix_hint: Option<&str>) -> usize {
    println!("  ✗ {line}");
    if let Some(hint) = fix_hint {
        println!("      fix: {hint}");
    }
    1
}

fn check_skill(project_root: &Path, fix: bool) -> usize {
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

fn check_hook(project_root: &Path, fix: bool) -> usize {
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

fn check_mcp(project_root: &Path, fix: bool) -> usize {
    match mcp_registration::mcp_entry_state(project_root) {
        McpState::Present => ok(".mcp.json clarion serve entry present"),
        McpState::Unparseable => problem(
            ".mcp.json is not parseable JSON — fix it by hand, then re-run",
            None,
        ),
        state => {
            let what = match state {
                McpState::Missing => ".mcp.json has no clarion serve entry",
                McpState::Stale => ".mcp.json clarion entry targets a different project",
                McpState::Present | McpState::Unparseable => unreachable!(),
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
