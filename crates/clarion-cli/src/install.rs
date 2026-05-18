//! `clarion install` — initialise .clarion/ in the target directory.
//!
//! Creates:
//! - `.clarion/clarion.db`        (migrated)
//! - `.clarion/config.json`       (internal state stub)
//! - `.clarion/.gitignore`        (UQ-WP1-04 rules; ADR-005)
//! - `<path>/clarion.yaml`        (user-edited config stub at project root;
//!   see detailed-design.md §File layout)
//!
//! Refuses if `.clarion/` already exists unless `--force` is passed.

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

const CLARION_YAML_STUB: &str = "# clarion.yaml — user-edited config.\n\
# Do not delete this file: clarion serve reads MCP, LLM, and integration\n\
# settings from here when present.\n\
version: 1\n\
llm_policy:\n\
  enabled: false\n\
  provider: openrouter\n\
  allow_live_provider: false\n\
  openrouter:\n\
    endpoint_url: https://openrouter.ai/api/v1\n\
    api_key_env: OPENROUTER_API_KEY\n\
    attribution:\n\
      referer: https://github.com/qacona/clarion\n\
      title: Clarion\n\
  model_id: anthropic/claude-sonnet-4.6\n\
  session_token_ceiling: 1000000\n\
  max_inferred_edges_per_caller: 8\n\
  cache_max_age_days: 180\n\
integrations:\n\
  filigree:\n\
    enabled: false\n\
    base_url: http://127.0.0.1:8766\n\
    actor: clarion-mcp\n\
    token_env: FILIGREE_API_TOKEN\n\
    timeout_seconds: 5\n";

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

/// Run the `install` subcommand.
///
/// # Errors
///
/// Returns an error if `.clarion/` already exists without `--force`, if the
/// target directory cannot be canonicalised, or if any filesystem or database
/// operation fails.
pub fn run(path: &Path, force: bool) -> Result<()> {
    if !path.exists() {
        bail!(
            "target directory does not exist: {}. Create it first or pass a valid --path.",
            path.display()
        );
    }
    let project_root = path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise --path {}", path.display()))?;
    let clarion_dir = project_root.join(".clarion");
    if clarion_dir.exists() {
        if !force {
            bail!(
                ".clarion/ already exists at {}. Delete it or pass --force to overwrite it.",
                clarion_dir.display()
            );
        }
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

    fs::create_dir_all(&clarion_dir).with_context(|| format!("mkdir {}", clarion_dir.display()))?;

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
