//! `clarion install` — initialise .clarion/ in the target directory.
//!
//! Creates:
//! - `.clarion/clarion.db`        (migrated)
//! - `.clarion/config.json`       (internal state stub)
//! - `.clarion/.gitignore`        (UQ-WP1-04 rules; ADR-005)
//! - `<path>/clarion.yaml`        (user-edited config stub at project root;
//!   see detailed-design.md §File layout)
//!
//! A bare `clarion install` refuses if `.clarion/` already exists unless
//! `--force` is passed. Under `--skills`/`--hooks`/`--all`, an existing
//! `.clarion/` is left in place (init is skipped) rather than treated as an
//! error, so the requested idempotent components can still be applied.

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

/// Which install components to perform. Resolved from CLI flags in
/// [`InstallComponents::from_flags`] per the flag semantics in the agent-
/// orientation plan: bare install = init only; `--skills`/`--hooks` are
/// independent and do NOT init; `--all` = init + skills + hooks.
#[derive(Debug, Clone, Copy)]
pub struct InstallComponents {
    pub init_clarion: bool,
    pub skills: bool,
    pub hooks: bool,
}

impl InstallComponents {
    #[must_use]
    pub fn from_flags(skills: bool, hooks: bool, all: bool) -> Self {
        if all {
            return Self {
                init_clarion: true,
                skills: true,
                hooks: true,
            };
        }
        let any_component = skills || hooks;
        Self {
            // Bare install (no component flags) keeps today's behavior: init.
            init_clarion: !any_component,
            skills,
            hooks,
        }
    }
}

/// Run the `install` subcommand.
///
/// # Errors
///
/// Returns an error if `.clarion/` already exists without `--force`, if the
/// target directory cannot be canonicalised, or if any filesystem or database
/// operation fails.
pub fn run(path: &Path, force: bool, components: InstallComponents) -> Result<()> {
    if !path.exists() {
        bail!(
            "target directory does not exist: {}. Create it first or pass a valid --path.",
            path.display()
        );
    }
    let project_root = path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise --path {}", path.display()))?;

    // The CLI flag parser cannot produce an all-false `components` (a bare
    // `clarion install` sets `init_clarion`), but the struct is representable,
    // so guard the do-nothing state defensively rather than silently succeeding.
    if !components.init_clarion && !components.skills && !components.hooks {
        bail!(
            "nothing to install: pass --skills, --hooks, or --all, \
             or run bare `clarion install` to initialise .clarion/."
        );
    }

    if components.init_clarion {
        let clarion_dir = project_root.join(".clarion");
        let exists = clarion_dir.exists();
        // A bare `clarion install` (no other components requested) refuses to
        // clobber an existing .clarion/. Under `--all` (other idempotent
        // components requested) an already-initialised project is not an error:
        // we keep the existing index and apply the remaining components.
        let bare_init = !components.skills && !components.hooks;
        if exists && !force {
            if bare_init {
                bail!(
                    ".clarion/ already exists at {}. Delete it or pass --force to overwrite it.",
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

    if components.skills {
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

    if components.hooks {
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
    use super::InstallComponents;

    #[test]
    fn from_flags_truth_table() {
        // Bare install: no component flags -> init only.
        let bare = InstallComponents::from_flags(false, false, false);
        assert!(bare.init_clarion);
        assert!(!bare.skills);
        assert!(!bare.hooks);

        // --skills: skills only, no init.
        let skills = InstallComponents::from_flags(true, false, false);
        assert!(!skills.init_clarion);
        assert!(skills.skills);
        assert!(!skills.hooks);

        // --hooks: hooks only, no init.
        let hooks = InstallComponents::from_flags(false, true, false);
        assert!(!hooks.init_clarion);
        assert!(!hooks.skills);
        assert!(hooks.hooks);

        // --all: everything (component flags ignored).
        let all = InstallComponents::from_flags(false, false, true);
        assert!(all.init_clarion);
        assert!(all.skills);
        assert!(all.hooks);

        // --skills --hooks: both components, still no init.
        let both = InstallComponents::from_flags(true, true, false);
        assert!(!both.init_clarion);
        assert!(both.skills);
        assert!(both.hooks);
    }
}
