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
        /// Overwrite an existing .clarion/ (not implemented in Sprint 1).
        #[arg(long)]
        force: bool,

        /// Directory to install into (default: current directory).
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },

    /// Run an analysis pass. Sprint 1: no plugins are loaded; run status is
    /// `skipped_no_plugins`. WP2 wires plugin spawning.
    Analyze {
        /// Path to analyse (default: current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
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
