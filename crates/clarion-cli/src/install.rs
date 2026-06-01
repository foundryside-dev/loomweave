//! `clarion install` — initialise .clarion/ in the target directory.
//!
//! Creates:
//! - `.clarion/clarion.db`        (migrated)
//! - `.clarion/config.json`       (internal state stub)
//! - `.clarion/.gitignore`        (UQ-WP1-04 rules; ADR-005)
//! - `<path>/clarion.yaml`        (user-edited config stub at project root;
//!   see detailed-design.md §File layout)
//!
//! A bare `clarion install` (no flags) does everything: init + skills + hooks.
//! If `.clarion/` already exists, init is skipped and the idempotent components
//! (skills, hooks) are still applied. Pass `--force` to wipe and reinitialise
//! `.clarion/`. `--skills` / `--hooks` / `--all` are still accepted for
//! explicit partial installs.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use clarion_storage::{pragma, schema};

const CONFIG_JSON_STUB: &str = r#"{
    "schema_version": 1,
    "last_run_id": null
}
"#;

// NOTE: Do not use `\` line-continuation here — Rust strips both the newline
// AND all leading whitespace on the continuation line, producing flat (and
// therefore broken) YAML. Use raw newlines + explicit indentation.
const CLARION_YAML_STUB: &str = "# clarion.yaml — user-edited config.
# Do not delete this file: clarion serve reads MCP, LLM, and integration
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
      referer: https://github.com/tachyon-beep/clarion
      title: Clarion
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
    actor: clarion-mcp
    token_env: FILIGREE_API_TOKEN
    timeout_seconds: 5
serve:
  http:
    enabled: false
    bind: 127.0.0.1:9111
";

const GITIGNORE_CONTENTS: &str = "\
# Clarion .gitignore — ADR-005 tracked-vs-excluded list.
# Tracked (committed): clarion.db, config.json, .gitignore itself.
# Excluded (ignored): WAL sidecars, shadow DB, per-run logs, tmp scratch.

# SQLite write-ahead files never belong in the repo.
*-wal
*-shm
*.db-wal
*.db-shm

# Shadow DB intermediate (ADR-011 --shadow-db).
*.shadow.db
*.db.new

# Scratch / temp space.
tmp/

# Per-run log directories (see detailed-design §File layout). The run dir
# metadata (config.yaml, stats.json, partial.json) is tracked; only the
# raw LLM request/response log is excluded.
logs/
runs/*/log.jsonl
";

/// What `clarion install` should do, resolved from the CLI flags.
///
/// Modeled as an enum rather than three independent bools so the derived and
/// illegal states the bool form allowed are unrepresentable: `init_clarion` is
/// no longer a peer field that can contradict an explicit component request,
/// and the do-nothing `{false, false, false}` state (which PR #21 had to guard
/// against at the `run()` entry) cannot be produced by [`InstallPlan::from_flags`]
/// at all (clarion-c6b8dc27f3). The three booleans are derived on demand via
/// [`init_clarion`](Self::init_clarion) / [`skills`](Self::skills) /
/// [`hooks`](Self::hooks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPlan {
    /// `--skills` and/or `--hooks` without `--all`: apply the named components
    /// and do NOT initialise `.clarion/`. `from_flags` only constructs this
    /// when at least one field is `true`.
    Components { skills: bool, hooks: bool },
    /// No flags or `--all`: initialise `.clarion/` + skills + hooks.
    All,
}

impl InstallPlan {
    /// Resolve the CLI flags into a plan. `--all` wins; otherwise any of
    /// `--skills`/`--hooks` selects [`Components`](Self::Components); no flag
    /// selects [`All`](Self::All) so that a naked `clarion install` does
    /// everything. Never yields a do-nothing plan.
    #[must_use]
    pub fn from_flags(skills: bool, hooks: bool, all: bool) -> Self {
        if all {
            Self::All
        } else if skills || hooks {
            Self::Components { skills, hooks }
        } else {
            Self::All
        }
    }

    /// Whether to initialise `.clarion/` (the index). True for `All`.
    #[must_use]
    pub fn init_clarion(self) -> bool {
        matches!(self, Self::All)
    }

    /// Whether to install the `clarion-workflow` skill pack.
    #[must_use]
    pub fn skills(self) -> bool {
        matches!(self, Self::All | Self::Components { skills: true, .. })
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
/// Returns an error if `.clarion/` already exists without `--force`, if the
/// target directory cannot be canonicalised, or if any filesystem or database
/// operation fails.
pub fn run(path: &Path, force: bool, plan: InstallPlan) -> Result<()> {
    if !path.exists() {
        bail!(
            "target directory does not exist: {}. Create it first or pass a valid --path.",
            path.display()
        );
    }
    let project_root = path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise --path {}", path.display()))?;

    // `from_flags` cannot produce a do-nothing plan, but a hand-built
    // `Components { skills: false, hooks: false }` still could, so keep a
    // defensive guard rather than silently succeeding.
    if !plan.init_clarion() && !plan.skills() && !plan.hooks() {
        bail!(
            "nothing to install: pass --skills, --hooks, --all, \
             or run bare `clarion install` to do everything."
        );
    }

    if plan.init_clarion() {
        let clarion_dir = project_root.join(".clarion");
        let exists = clarion_dir.exists();
        // `All` (including naked install) treats an existing .clarion/ as
        // already-initialised and skips re-init, still applying the idempotent
        // components. A non-directory .clarion is not a usable index, so refuse
        // rather than "succeed" with skills/hooks atop a project with no clarion.db.
        // `--skills`/`--hooks` alone are `Components` (init_clarion() == false)
        // and skip this entire block.
        if exists && !force {
            if !clarion_dir.is_dir() {
                bail!(
                    "found a non-directory at {}; expected an initialised .clarion/ \
                     directory. Remove it (or pass --force) and re-run.",
                    clarion_dir.display()
                );
            }
            println!(
                "{} already initialised; skipping .clarion/ init (pass --force to recreate).",
                clarion_dir.display()
            );
        } else {
            if exists {
                // --force overwrite path.
                if !clarion_dir.is_dir() {
                    bail!(
                        "--force can only overwrite an existing .clarion/ directory; \
                         found non-directory at {}.",
                        clarion_dir.display()
                    );
                }
                fs::remove_dir_all(&clarion_dir)
                    .with_context(|| format!("remove existing {}", clarion_dir.display()))?;
            }

            fs::create_dir_all(&clarion_dir)
                .with_context(|| format!("mkdir {}", clarion_dir.display()))?;

            // Cleanup guard: if any post-mkdir step fails, remove .clarion/ before
            // bubbling the error so the next install attempt isn't blocked by the
            // "already exists" check (clarion-ed5017139f).
            if let Err(err) = populate_after_mkdir(&clarion_dir, &project_root) {
                if let Err(cleanup_err) = fs::remove_dir_all(&clarion_dir) {
                    tracing::warn!(
                        clarion_dir = %clarion_dir.display(),
                        error = %cleanup_err,
                        "install failed and cleanup of partial .clarion/ also failed; \
                         manual rm -rf may be required"
                    );
                }
                return Err(err);
            }

            tracing::info!(
                clarion_dir = %clarion_dir.display(),
                "clarion install complete"
            );
            println!("Initialised {}", clarion_dir.display());
        }
    }

    if plan.skills() {
        let report = crate::skill_pack::install_skill_pack(&project_root)
            .context("install clarion-workflow skill pack")?;
        if report.copied {
            println!(
                "Installed clarion-workflow skill into {}/.claude/skills and {}/.agents/skills",
                project_root.display(),
                project_root.display()
            );
        } else {
            println!("clarion-workflow skill already up to date");
        }
    }

    if plan.hooks() {
        let changed = crate::hooks_settings::install_session_start_hook(&project_root)
            .context("merge SessionStart hook into .claude/settings.json")?;
        if changed {
            println!(
                "Added clarion SessionStart hook to {}/.claude/settings.json",
                project_root.display()
            );
        } else {
            println!("clarion SessionStart hook already present");
        }
    }

    Ok(())
}

fn populate_after_mkdir(clarion_dir: &Path, project_root: &Path) -> Result<()> {
    let db_path = clarion_dir.join("clarion.db");
    initialise_db(&db_path).context("initialise clarion.db")?;

    let config_path = clarion_dir.join("config.json");
    fs::write(&config_path, CONFIG_JSON_STUB)
        .with_context(|| format!("write {}", config_path.display()))?;

    let gitignore_path = clarion_dir.join(".gitignore");
    fs::write(&gitignore_path, GITIGNORE_CONTENTS)
        .with_context(|| format!("write {}", gitignore_path.display()))?;

    let yaml_path = project_root.join("clarion.yaml");
    if yaml_path.exists() {
        tracing::debug!(
            path = %yaml_path.display(),
            "clarion.yaml already exists; leaving untouched"
        );
    } else {
        fs::write(&yaml_path, CLARION_YAML_STUB)
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
    use super::InstallPlan;

    #[test]
    fn from_flags_truth_table() {
        // Naked install: no flags -> everything (same as --all).
        let naked = InstallPlan::from_flags(false, false, false);
        assert_eq!(naked, InstallPlan::All);
        assert!(naked.init_clarion());
        assert!(naked.skills());
        assert!(naked.hooks());

        // --skills: skills only, no init.
        let skills = InstallPlan::from_flags(true, false, false);
        assert_eq!(
            skills,
            InstallPlan::Components {
                skills: true,
                hooks: false
            }
        );
        assert!(!skills.init_clarion());
        assert!(skills.skills());
        assert!(!skills.hooks());

        // --hooks: hooks only, no init.
        let hooks = InstallPlan::from_flags(false, true, false);
        assert_eq!(
            hooks,
            InstallPlan::Components {
                skills: false,
                hooks: true
            }
        );
        assert!(!hooks.init_clarion());
        assert!(!hooks.skills());
        assert!(hooks.hooks());

        // --all: everything (component flags ignored).
        let all = InstallPlan::from_flags(false, false, true);
        assert_eq!(all, InstallPlan::All);
        assert!(all.init_clarion());
        assert!(all.skills());
        assert!(all.hooks());

        // --skills --hooks: both components, still no init.
        let both = InstallPlan::from_flags(true, true, false);
        assert_eq!(
            both,
            InstallPlan::Components {
                skills: true,
                hooks: true
            }
        );
        assert!(!both.init_clarion());
        assert!(both.skills());
        assert!(both.hooks());
    }

    #[test]
    fn from_flags_never_yields_a_do_nothing_plan() {
        // The do-nothing {false,false,false} state that PR #21 guarded against
        // at run() entry is now unreachable through the only public constructor
        // (clarion-c6b8dc27f3): every flag combination resolves to a plan that
        // does at least one thing.
        for skills in [false, true] {
            for hooks in [false, true] {
                for all in [false, true] {
                    let plan = InstallPlan::from_flags(skills, hooks, all);
                    assert!(
                        plan.init_clarion() || plan.skills() || plan.hooks(),
                        "from_flags({skills},{hooks},{all}) produced a do-nothing plan: {plan:?}"
                    );
                }
            }
        }
    }
}
