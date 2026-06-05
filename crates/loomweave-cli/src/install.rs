//! `loomweave install` — initialise .loomweave/ in the target directory.
//!
//! Creates:
//! - `.loomweave/loomweave.db`        (migrated)
//! - `.loomweave/config.json`       (internal state stub)
//! - `.loomweave/.gitignore`        (UQ-WP1-04 rules; ADR-005)
//! - `<path>/loomweave.yaml`        (user-edited config stub at project root;
//!   see detailed-design.md §File layout)
//!
//! A bare `loomweave install` (no flags) does everything: init + MCP config +
//! skills + hooks + local Weft integration bindings. If `.loomweave/` already
//! exists, init is skipped and the idempotent components are still applied.
//! Pass `--force` to wipe and reinitialise `.loomweave/`. Component flags and
//! `--all` are still accepted for explicit partial installs.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use loomweave_storage::{pragma, schema};

const CONFIG_JSON_STUB: &str = r#"{
    "schema_version": 1,
    "last_run_id": null
}
"#;

// NOTE: Do not use `\` line-continuation here — Rust strips both the newline
// AND all leading whitespace on the continuation line, producing flat (and
// therefore broken) YAML. Use raw newlines + explicit indentation.
const LOOMWEAVE_YAML_STUB: &str = "# loomweave.yaml — user-edited config.
# Do not delete this file: loomweave serve reads MCP, LLM, and integration
# settings from here when present.
version: 1
llm_policy:
  enabled: false
  provider: openrouter
  allow_live_provider: false
  openrouter:
    endpoint_url: https://openrouter.ai/api/v1
    api_key_env: OPENROUTER_API_KEY
    attribution:
      referer: https://github.com/foundryside-dev/loomweave
      title: Loomweave
  codex_cli:
    executable: codex
    model: null
    profile: null
    sandbox: read-only
    timeout_seconds: 300
  claude_cli:
    executable: claude
    model: null
    permission_mode: plan
    tools: []
    timeout_seconds: 300
    max_turns: 2
    no_session_persistence: true
    exclude_dynamic_system_prompt_sections: true
  model_id: anthropic/claude-sonnet-4.6
  session_token_ceiling: 1000000
  max_inferred_edges_per_caller: 8
  cache_max_age_days: 180
integrations:
  filigree:
    enabled: false
    base_url: http://127.0.0.1:8766
    actor: loomweave-mcp
    token_env: FILIGREE_API_TOKEN
    timeout_seconds: 5
serve:
  mcp:
    enable_write_tools: false
  http:
    enabled: false
    # The read-API port is auto-selected per project (deterministic, with an
    # ephemeral fallback) and published to .loomweave/ephemeral.port while
    # serving. Set `bind:` explicitly only to pin a fixed port (ADR-044).
";

const GITIGNORE_CONTENTS: &str = "\
# Loomweave .gitignore — ADR-005 tracked-vs-excluded list.
# Tracked (committed): loomweave.db, config.json, .gitignore itself.
# Excluded (ignored): WAL sidecars, shadow DB, per-run logs, tmp scratch,
#   the read-API live port discovery file.

# Read-API live port discovery file (ADR-044): present only while serve runs,
# rewritten per bind, loopback-only — a runtime artifact, never committed.
ephemeral.port

# SQLite write-ahead files never belong in the repo.
*-wal
*-shm
*.db-wal
*.db-shm

# Shadow DB intermediate (ADR-011 --shadow-db).
*.shadow.db
*.db.new

# Semantic-search embeddings sidecar (ADR-040): large + rebuildable, never
# committed (keeps loomweave.db unbloated). WAL files are covered by *.db-wal/-shm.
embeddings.db

# Scratch / temp space.
tmp/

# Per-run log directories (see detailed-design §File layout). The run dir
# metadata (config.yaml, stats.json, partial.json) is tracked; only the
# raw LLM request/response log is excluded.
logs/
runs/*/log.jsonl
";

/// A single component selected by a partial `loomweave install`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallComponent {
    ClaudeCode,
    Codex,
    Skills,
    CodexSkills,
    Hooks,
}

/// What `loomweave install` should do, resolved from the CLI flags.
///
/// Modeled as an enum rather than three independent bools so the derived and
/// illegal states the bool form allowed are unrepresentable: `init_loomweave` is
/// no longer a peer field that can contradict an explicit component request,
/// and the do-nothing `{false, false, false}` state (which PR #21 had to guard
/// against at the `run()` entry) cannot be produced by
/// [`InstallPlan::from_components`]
/// at all (clarion-c6b8dc27f3). Component booleans are derived on demand via
/// [`init_loomweave`](Self::init_loomweave) / [`skills`](Self::skills) /
/// [`hooks`](Self::hooks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPlan {
    /// Component flags without `--all`: apply the named components and do NOT
    /// initialise `.loomweave/`. `from_components` only constructs this when at
    /// least one component is present.
    Components {
        claude_code: bool,
        codex: bool,
        skills: bool,
        codex_skills: bool,
        hooks: bool,
    },
    /// No flags or `--all`: initialise `.loomweave/` + every integration.
    All,
}

impl InstallPlan {
    /// Resolve the CLI flags into a plan. `--all` wins; otherwise any of
    /// the component flags selects [`Components`](Self::Components); no flag
    /// selects [`All`](Self::All) so that a naked `loomweave install` does
    /// everything. Never yields a do-nothing plan.
    #[must_use]
    pub fn from_components(all: bool, components: &[InstallComponent]) -> Self {
        if all || components.is_empty() {
            Self::All
        } else {
            Self::Components {
                claude_code: components.contains(&InstallComponent::ClaudeCode),
                codex: components.contains(&InstallComponent::Codex),
                skills: components.contains(&InstallComponent::Skills),
                codex_skills: components.contains(&InstallComponent::CodexSkills),
                hooks: components.contains(&InstallComponent::Hooks),
            }
        }
    }

    /// Whether to initialise `.loomweave/` (the index). True for `All`.
    #[must_use]
    pub fn init_loomweave(self) -> bool {
        matches!(self, Self::All)
    }

    /// Whether to install the Claude Code MCP config.
    #[must_use]
    pub fn claude_code(self) -> bool {
        matches!(
            self,
            Self::All
                | Self::Components {
                    claude_code: true,
                    ..
                }
        )
    }

    /// Whether to install the Codex MCP config.
    #[must_use]
    pub fn codex(self) -> bool {
        matches!(self, Self::All | Self::Components { codex: true, .. })
    }

    /// Whether to install the `loomweave-workflow` skill pack for Claude Code.
    #[must_use]
    pub fn skills(self) -> bool {
        matches!(self, Self::All | Self::Components { skills: true, .. })
    }

    /// Whether to install the `loomweave-workflow` skill pack for Codex.
    #[must_use]
    pub fn codex_skills(self) -> bool {
        matches!(
            self,
            Self::All
                | Self::Components {
                    codex_skills: true,
                    ..
                }
        )
    }

    /// Whether to install the `SessionStart` hook.
    #[must_use]
    pub fn hooks(self) -> bool {
        matches!(self, Self::All | Self::Components { hooks: true, .. })
    }
}

/// Run the `install` subcommand.
///
/// # Errors
///
/// Returns an error if `.loomweave/` already exists without `--force`, if the
/// target directory cannot be canonicalised, or if any filesystem or database
/// operation fails.
pub fn run(
    path: &Path,
    force: bool,
    plan: InstallPlan,
    codex_config_path: Option<&Path>,
) -> Result<()> {
    if !path.exists() {
        bail!(
            "target directory does not exist: {}. Create it first or pass a valid --path.",
            path.display()
        );
    }
    let project_root = path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise --path {}", path.display()))?;

    // `from_components` cannot produce a do-nothing plan, but a hand-built
    // `Components { skills: false, hooks: false }` still could, so keep a
    // defensive guard rather than silently succeeding.
    validate_plan(plan)?;

    if plan.init_loomweave() {
        initialise_project(&project_root, force)?;
    }

    if plan.claude_code() {
        install_claude_code(&project_root)?;
    }

    if plan.codex() {
        install_codex(codex_config_path)?;
    }

    if plan.skills() {
        install_claude_skills(&project_root)?;
    }

    if plan.codex_skills() {
        install_codex_skills(&project_root)?;
    }

    if plan.hooks() {
        install_hooks(&project_root)?;
    }

    if matches!(plan, InstallPlan::All) {
        install_integration_bindings(&project_root)?;
    }

    Ok(())
}

fn validate_plan(plan: InstallPlan) -> Result<()> {
    // `from_components` cannot produce a do-nothing plan, but a hand-built
    // `Components { skills: false, hooks: false }` still could, so keep a
    // defensive guard rather than silently succeeding.
    if !plan.init_loomweave()
        && !plan.claude_code()
        && !plan.codex()
        && !plan.skills()
        && !plan.codex_skills()
        && !plan.hooks()
    {
        bail!(
            "nothing to install: pass --claude-code, --codex, --skills, \
             --codex-skills, --hooks, --all, \
             or run bare `loomweave install` to do everything."
        );
    }
    Ok(())
}

fn initialise_project(project_root: &Path, force: bool) -> Result<()> {
    let loomweave_dir = project_root.join(".loomweave");
    let exists = loomweave_dir.exists();
    // `All` (including naked install) treats an existing .loomweave/ as
    // already-initialised and skips re-init, still applying the idempotent
    // components. A non-directory .loomweave is not a usable index, so refuse
    // rather than "succeed" with skills/hooks atop a project with no loomweave.db.
    // Component-only installs skip this block.
    if exists && !force {
        if !loomweave_dir.is_dir() {
            bail!(
                "found a non-directory at {}; expected an initialised .loomweave/ \
                 directory. Remove it (or pass --force) and re-run.",
                loomweave_dir.display()
            );
        }
        println!(
            "{} already initialised; skipping .loomweave/ init (pass --force to recreate).",
            loomweave_dir.display()
        );
        return Ok(());
    }

    if exists {
        // --force overwrite path.
        if !loomweave_dir.is_dir() {
            bail!(
                "--force can only overwrite an existing .loomweave/ directory; \
                 found non-directory at {}.",
                loomweave_dir.display()
            );
        }
        fs::remove_dir_all(&loomweave_dir)
            .with_context(|| format!("remove existing {}", loomweave_dir.display()))?;
    }

    fs::create_dir_all(&loomweave_dir)
        .with_context(|| format!("mkdir {}", loomweave_dir.display()))?;

    // Cleanup guard: if any post-mkdir step fails, remove .loomweave/ before
    // bubbling the error so the next install attempt isn't blocked by the
    // "already exists" check (clarion-ed5017139f).
    if let Err(err) = populate_after_mkdir(&loomweave_dir, project_root) {
        if let Err(cleanup_err) = fs::remove_dir_all(&loomweave_dir) {
            tracing::warn!(
                loomweave_dir = %loomweave_dir.display(),
                error = %cleanup_err,
                "install failed and cleanup of partial .loomweave/ also failed; \
                 manual rm -rf may be required"
            );
        }
        return Err(err);
    }

    tracing::info!(
        loomweave_dir = %loomweave_dir.display(),
        "loomweave install complete"
    );
    println!("Initialised {}", loomweave_dir.display());
    Ok(())
}

fn install_claude_code(project_root: &Path) -> Result<()> {
    let changed = crate::mcp_registration::install_mcp_entry(project_root)
        .context("install Claude Code MCP config")?;
    if changed {
        println!(
            "Installed Claude Code MCP config at {}/.mcp.json",
            project_root.display()
        );
    } else {
        println!("Claude Code MCP config already up to date");
    }
    Ok(())
}

fn install_codex(codex_config_path: Option<&Path>) -> Result<()> {
    let config_path = match codex_config_path {
        Some(path) => path.to_path_buf(),
        None => {
            crate::mcp_registration::codex_config_path().context("locate Codex MCP config path")?
        }
    };
    let changed = crate::mcp_registration::install_codex_mcp_entry(&config_path)
        .context("install Codex MCP config")?;
    if changed {
        println!("Installed Codex MCP config at {}", config_path.display());
    } else {
        println!("Codex MCP config already up to date");
    }
    Ok(())
}

fn install_claude_skills(project_root: &Path) -> Result<()> {
    let report = crate::skill_pack::install_claude_skill_pack(project_root)
        .context("install loomweave-workflow skill pack for Claude Code")?;
    if report.copied {
        println!(
            "Installed loomweave-workflow skill into {}/.claude/skills",
            project_root.display()
        );
    } else {
        println!("loomweave-workflow Claude Code skill already up to date");
    }
    Ok(())
}

fn install_codex_skills(project_root: &Path) -> Result<()> {
    let report = crate::skill_pack::install_codex_skill_pack(project_root)
        .context("install loomweave-workflow skill pack for Codex")?;
    if report.copied {
        println!(
            "Installed loomweave-workflow skill into {}/.agents/skills",
            project_root.display()
        );
    } else {
        println!("loomweave-workflow Codex skill already up to date");
    }
    Ok(())
}

fn install_hooks(project_root: &Path) -> Result<()> {
    let changed = crate::hooks_settings::install_session_start_hook(project_root)
        .context("merge SessionStart hook into .claude/settings.json")?;
    if changed {
        println!(
            "Added loomweave SessionStart hook to {}/.claude/settings.json",
            project_root.display()
        );
    } else {
        println!("loomweave SessionStart hook already present");
    }
    Ok(())
}

fn install_integration_bindings(project_root: &Path) -> Result<()> {
    let changed = crate::integration_bindings::install_bindings(project_root)
        .context("install local Loomweave/Filigree/Wardline integration bindings")?;
    if changed {
        println!("Installed local Loomweave/Filigree/Wardline integration bindings");
    } else {
        println!("Local Loomweave/Filigree/Wardline integration bindings already up to date");
    }
    Ok(())
}

fn populate_after_mkdir(loomweave_dir: &Path, project_root: &Path) -> Result<()> {
    let db_path = loomweave_dir.join("loomweave.db");
    initialise_db(&db_path).context("initialise loomweave.db")?;

    let config_path = loomweave_dir.join("config.json");
    fs::write(&config_path, CONFIG_JSON_STUB)
        .with_context(|| format!("write {}", config_path.display()))?;

    let gitignore_path = loomweave_dir.join(".gitignore");
    fs::write(&gitignore_path, GITIGNORE_CONTENTS)
        .with_context(|| format!("write {}", gitignore_path.display()))?;

    let yaml_path = project_root.join("loomweave.yaml");
    if yaml_path.exists() {
        tracing::debug!(
            path = %yaml_path.display(),
            "loomweave.yaml already exists; leaving untouched"
        );
    } else {
        fs::write(&yaml_path, LOOMWEAVE_YAML_STUB)
            .with_context(|| format!("write {}", yaml_path.display()))?;
    }
    Ok(())
}

fn initialise_db(path: &Path) -> Result<()> {
    let mut conn =
        Connection::open(path).with_context(|| format!("open database {}", path.display()))?;
    pragma::apply_write_pragmas(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    schema::apply_migrations(&mut conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{InstallComponent, InstallPlan};

    #[test]
    fn from_components_truth_table() {
        // Naked install: no flags -> everything (same as --all).
        let naked = InstallPlan::from_components(false, &[]);
        assert_eq!(naked, InstallPlan::All);
        assert!(naked.init_loomweave());
        assert!(naked.claude_code());
        assert!(naked.codex());
        assert!(naked.skills());
        assert!(naked.codex_skills());
        assert!(naked.hooks());

        // --skills: skills only, no init.
        let skills = InstallPlan::from_components(false, &[InstallComponent::Skills]);
        assert_eq!(
            skills,
            InstallPlan::Components {
                claude_code: false,
                codex: false,
                skills: true,
                codex_skills: false,
                hooks: false
            }
        );
        assert!(!skills.init_loomweave());
        assert!(!skills.claude_code());
        assert!(!skills.codex());
        assert!(skills.skills());
        assert!(!skills.codex_skills());
        assert!(!skills.hooks());

        // --hooks: hooks only, no init.
        let hooks = InstallPlan::from_components(false, &[InstallComponent::Hooks]);
        assert_eq!(
            hooks,
            InstallPlan::Components {
                claude_code: false,
                codex: false,
                skills: false,
                codex_skills: false,
                hooks: true
            }
        );
        assert!(!hooks.init_loomweave());
        assert!(!hooks.claude_code());
        assert!(!hooks.codex());
        assert!(!hooks.skills());
        assert!(!hooks.codex_skills());
        assert!(hooks.hooks());

        // --all: everything (component flags ignored).
        let all = InstallPlan::from_components(true, &[]);
        assert_eq!(all, InstallPlan::All);
        assert!(all.init_loomweave());
        assert!(all.claude_code());
        assert!(all.codex());
        assert!(all.skills());
        assert!(all.codex_skills());
        assert!(all.hooks());

        // Multiple component flags: selected components only, still no init.
        let both = InstallPlan::from_components(
            false,
            &[
                InstallComponent::ClaudeCode,
                InstallComponent::Codex,
                InstallComponent::Skills,
                InstallComponent::CodexSkills,
                InstallComponent::Hooks,
            ],
        );
        assert_eq!(
            both,
            InstallPlan::Components {
                claude_code: true,
                codex: true,
                skills: true,
                codex_skills: true,
                hooks: true
            }
        );
        assert!(!both.init_loomweave());
        assert!(both.claude_code());
        assert!(both.codex());
        assert!(both.skills());
        assert!(both.codex_skills());
        assert!(both.hooks());
    }

    #[test]
    fn from_components_never_yields_a_do_nothing_plan() {
        // The do-nothing {false,false,false} state that PR #21 guarded against
        // at run() entry is now unreachable through the only public constructor
        // (clarion-c6b8dc27f3): every flag combination resolves to a plan that
        // does at least one thing.
        let cases: &[&[InstallComponent]] = &[
            &[],
            &[InstallComponent::ClaudeCode],
            &[InstallComponent::Codex],
            &[InstallComponent::Skills],
            &[InstallComponent::CodexSkills],
            &[InstallComponent::Hooks],
            &[
                InstallComponent::ClaudeCode,
                InstallComponent::Codex,
                InstallComponent::Skills,
                InstallComponent::CodexSkills,
                InstallComponent::Hooks,
            ],
        ];
        for all in [false, true] {
            for components in cases {
                let plan = InstallPlan::from_components(all, components);
                assert!(
                    plan.init_loomweave()
                        || plan.claude_code()
                        || plan.codex()
                        || plan.skills()
                        || plan.codex_skills()
                        || plan.hooks(),
                    "from_components({all}, {components:?}) produced a do-nothing plan: {plan:?}"
                );
            }
        }
    }
}
