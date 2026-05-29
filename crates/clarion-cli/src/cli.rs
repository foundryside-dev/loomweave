use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "clarion", version, about = "Clarion code-archaeology tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialise .clarion/ and/or install agent-orientation assets.
    ///
    /// Bare `clarion install` initialises .clarion/ only (refuses if it
    /// already exists). `--skills` and `--hooks` install the orientation
    /// assets and do NOT initialise .clarion/. `--all` does init + skills +
    /// hooks.
    Install {
        /// Overwrite an existing .clarion/ directory.
        #[arg(long)]
        force: bool,

        /// Directory to install into (default: current directory).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Install the bundled clarion-workflow skill pack into
        /// .claude/skills/ and .agents/skills/.
        #[arg(long)]
        skills: bool,

        /// Merge a `SessionStart` hook into .claude/settings.json.
        #[arg(long)]
        hooks: bool,

        /// Do everything: .clarion/ init + --skills + --hooks.
        #[arg(long)]
        all: bool,
    },

    /// Run an analysis pass: walk the source tree, dispatch discovered plugins
    /// to extract entities/edges, and persist results to `.clarion/clarion.db`.
    /// Re-runs are idempotent (UPSERT on `entities.id`). If no plugins are on
    /// `$PATH`, exits 0 with a WARN and status `skipped_no_plugins` — see
    /// `docs/operator/getting-started.md` Troubleshooting.
    Analyze {
        /// Path to analyse (default: current directory).
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Path to clarion.yaml (default: project-root/clarion.yaml if present).
        #[arg(long)]
        config: Option<PathBuf>,

        /// Allow analysis of files containing unredacted secrets. Requires a
        /// confirmation step when detections are present.
        #[arg(long)]
        allow_unredacted_secrets: bool,

        /// Non-TTY confirmation token for --allow-unredacted-secrets.
        #[arg(long, value_name = "TOKEN", requires = "allow_unredacted_secrets")]
        confirm_allow_unredacted_secrets: Option<String>,
    },

    /// Run the MCP stdio server.
    Serve {
        /// Project directory containing .clarion/clarion.db.
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Path to clarion.yaml (default: project-root/clarion.yaml if present).
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Agent-lifecycle hook entrypoints. Always exit 0 (fail-soft) so a
    /// misbehaving hook never blocks session start.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },

    /// Local database maintenance.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
}

#[derive(Subcommand)]
pub enum DbCommand {
    /// Take a consistent, WAL-safe online backup of `.clarion/clarion.db`.
    ///
    /// Unlike `cp`, this captures outstanding WAL frames into a standalone
    /// single-file copy, so it is safe to run during a live `clarion analyze`.
    Backup {
        /// Destination file for the backup copy.
        output: PathBuf,

        /// Project directory containing .clarion/clarion.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Overwrite the output file if it already exists.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum HookCommand {
    /// Print a project snapshot and re-sync the skill pack on drift.
    SessionStart {
        /// Project directory containing .clarion/clarion.db.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}
