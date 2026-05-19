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
    /// Initialise .clarion/ in the current directory.
    Install {
        /// Overwrite an existing .clarion/ directory.
        #[arg(long)]
        force: bool,

        /// Directory to install into (default: current directory).
        #[arg(long, default_value = ".")]
        path: PathBuf,
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
}
